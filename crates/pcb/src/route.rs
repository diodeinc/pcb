//! PCB auto-routing command using DeepPCB cloud service

use anyhow::{Context, Result};
use clap::Args;
use colored::Colorize;
use pcb_diode_api::routing::{self, RoutingJob, RoutingStatus, StartRoutingRequest};
use pcb_kicad::PythonScriptBuilder;
use pcb_layout::utils;
use pcb_ui::prelude::*;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::NamedTempFile;

use crate::file_walker;

#[derive(Args, Debug, Clone)]
#[command(about = "Auto-route PCB using DeepPCB cloud service")]
pub struct RouteArgs {
    /// Path to .zen file
    #[arg(value_name = "FILE", value_hint = clap::ValueHint::FilePath)]
    pub file: PathBuf,

    /// Routing timeout in minutes (default: 20, max: 60)
    #[arg(long, short = 't', default_value = "20")]
    pub timeout: u32,

    /// Don't open KiCad after routing
    #[arg(long)]
    pub no_open: bool,

    /// Override project ID (default: derived from .zen file name)
    #[arg(long)]
    pub project_id: Option<String>,
}

pub fn execute(args: RouteArgs) -> Result<()> {
    file_walker::require_zen_file(&args.file)?;

    // Validate timeout
    if args.timeout > 60 {
        anyhow::bail!("Timeout cannot exceed 60 minutes");
    }

    // Resolve dependencies
    let resolution_result = crate::resolve::resolve(Some(&args.file), false, false)?;

    let zen_path = &args.file;
    let board_name = zen_path.file_stem().unwrap().to_string_lossy();

    // Evaluate the .zen file to find the layout path
    let (output, diagnostics) = pcb_zen::run(zen_path, resolution_result).unpack();

    if diagnostics.has_errors() {
        anyhow::bail!("Failed to evaluate {}: build errors", zen_path.display());
    }

    let schematic = output.context("No schematic output from evaluation")?;

    let layout_dir = utils::resolve_layout_dir(&schematic)?
        .context("No layout path defined in schematic. Add layout=\"path\" to your module.")?;

    // Discover KiCad project + board paths
    let kicad_files = utils::require_kicad_files(&layout_dir)?;
    let board_path = kicad_files.kicad_pcb();
    let project_path = kicad_files.kicad_pro;

    // Validate files exist
    if !board_path.exists() {
        anyhow::bail!(
            "No layout found at {}\n\nRun {} first to generate the board.",
            board_path.display(),
            "pcb layout".yellow()
        );
    }

    if !project_path.exists() {
        anyhow::bail!(
            "Missing project file: {}\n\nEnsure the layout was generated with KiCad 6+",
            project_path.display()
        );
    }

    // Derive project ID
    let project_id = args
        .project_id
        .clone()
        .unwrap_or_else(|| board_name.to_string());

    // Set up Ctrl+C handler
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    })
    .context("Failed to set Ctrl+C handler")?;

    // Start routing
    println!(
        "Starting routing for {}",
        board_path.file_name().unwrap().to_string_lossy().green()
    );

    let spinner = Spinner::builder("Uploading board...").start();

    let request = StartRoutingRequest {
        project_id: project_id.clone(),
        timeout: Some(args.timeout),
    };

    let job_id = match routing::start_routing(&board_path, &project_path, &request) {
        Ok(id) => {
            spinner.finish();
            id
        }
        Err(e) => {
            spinner.error("Failed to start routing");
            anyhow::bail!("{}", e);
        }
    };

    let short_job_id = job_id.split('-').next().unwrap_or(&job_id);
    println!(
        "Job {} started ({} min timeout, $0.50/min)",
        short_job_id.yellow(),
        args.timeout
    );
    println!();

    // Polling loop
    let mut last_revision: u32 = 0;
    let start_time = Instant::now();
    let mut last_status: Option<RoutingJob> = None;
    let mut consecutive_errors = 0;

    while running.load(Ordering::SeqCst) {
        match routing::get_routing_status(&job_id) {
            Ok(status) => {
                consecutive_errors = 0;

                // Apply new revision
                if let Some(ref stats) = status.stats
                    && stats.revision_number > last_revision
                    && status.status != RoutingStatus::Queued
                {
                    match download_and_apply_ses(&job_id, &board_path) {
                        Ok(()) => {
                            println!("{}", format_progress(&status, stats.revision_number));
                            last_revision = stats.revision_number;
                            if !args.no_open {
                                let _ = open::that(&board_path);
                            }
                        }
                        Err(e) => {
                            println!("{} Failed to apply: {}", "!".yellow(), e);
                        }
                    }
                }

                // Check termination
                if matches!(
                    status.status,
                    RoutingStatus::Complete | RoutingStatus::Error
                ) {
                    last_status = Some(status);
                    break;
                }

                if status.converged {
                    println!("{} Converged! Stopping...", "✓".green());
                    let _ = routing::stop_routing(&job_id);
                    last_status = Some(status);
                    break;
                }
            }
            Err(e) => {
                consecutive_errors += 1;
                if consecutive_errors >= 3 {
                    println!("{} Error: {}", "✗".red(), e);
                }
            }
        }

        // Poll every 3s with Ctrl+C responsiveness
        for _ in 0..30 {
            if !running.load(Ordering::SeqCst) {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    // Handle Ctrl+C
    if !running.load(Ordering::SeqCst) {
        println!();
        println!("Stopping routing job...");
        let _ = routing::stop_routing(&job_id);
        println!("{} Stopped. Best result applied to board.", "✓".green());
    }

    // Display final summary
    println!();
    if let Some(status) = last_status {
        display_summary(&status, start_time.elapsed(), &board_path);
    }

    Ok(())
}

fn format_progress(status: &RoutingJob, revision: u32) -> String {
    if let Some(ref stats) = status.stats {
        let sep = "·".dimmed();
        format!(
            "{:>3}  {:>2}/{:<2} nets {} {:>3}/{:<3} air {} {:>2} vias {} {:>6.1} mm",
            format!("#{}", revision).cyan().bold(),
            stats.nets_completed,
            stats.total_nets,
            sep,
            stats.air_wires_connected,
            stats.air_wires_total,
            sep,
            stats.vias,
            sep,
            stats.wire_length / 1000.0
        )
    } else {
        format!("{}", format!("#{}", revision).cyan().bold())
    }
}

fn display_summary(status: &RoutingJob, elapsed: Duration, board_path: &Path) {
    let cost = elapsed.as_secs_f64() / 60.0 * 0.5;

    if let Some(ref stats) = status.stats {
        println!("{}", "Routing complete".green().bold());
        println!(
            "  Nets:       {}/{}",
            stats.nets_completed, stats.total_nets
        );
        println!(
            "  Air wires:  {}/{}",
            stats.air_wires_connected, stats.air_wires_total
        );
        println!("  Vias:       {}", stats.vias);
        println!("  Wire:       {:.1} mm", stats.wire_length / 1000.0);
        println!("  Time:       {}", format_duration(elapsed));
        println!("  Cost:       ${:.2}", cost);
    }

    println!();
    println!(
        "Result saved to {}",
        board_path.display().to_string().cyan()
    );
}

fn download_and_apply_ses(job_id: &str, board_path: &Path) -> Result<()> {
    // Download SES
    let ses_bytes = routing::download_routing_result(job_id)?;

    // Write to temp file
    let mut temp_file = NamedTempFile::new()?;
    temp_file.write_all(&ses_bytes)?;
    let ses_path = temp_file.path();

    // Import SES into KiCad board
    let script = r#"
import pcbnew
import sys

brd_filename = sys.argv[1]
ses_filename = sys.argv[2]
brd = pcbnew.LoadBoard(brd_filename)
pcbnew.ImportSpecctraSES(brd, ses_filename)

filler = pcbnew.ZONE_FILLER(brd)
filler.Fill(brd.Zones())

pcbnew.SaveBoard(brd_filename, brd)
"#;

    PythonScriptBuilder::new(script)
        .arg(board_path.to_string_lossy())
        .arg(ses_path.to_string_lossy())
        .run()
        .context("Failed to import SES file")?;

    Ok(())
}

fn format_duration(duration: Duration) -> String {
    let total_secs = duration.as_secs();
    let mins = total_secs / 60;
    let secs = total_secs % 60;
    format!("{}:{:02}", mins, secs)
}
