use anyhow::{Context, Result};
use clap::Args;
use inquire::Select;
use pcb_layout::{process_layout, LayoutError};
use pcb_ui::prelude::*;
use std::path::PathBuf;

use crate::build::{build, create_diagnostics_passes};
use crate::drc;
use crate::file_walker;

#[derive(Args, Debug, Default, Clone)]
#[command(about = "Generate PCB layout files from .zen files")]
pub struct LayoutArgs {
    #[arg(long, help = "Skip opening the layout file after generation")]
    pub no_open: bool,

    #[arg(
        short = 's',
        long,
        help = "Always prompt to choose a layout even when only one"
    )]
    pub select: bool,

    /// One or more .zen files to process for layout generation.
    /// When omitted, all .zen files in the current directory tree are processed.
    #[arg(value_name = "PATHS", value_hint = clap::ValueHint::AnyPath)]
    pub paths: Vec<PathBuf>,

    /// Disable network access (offline mode) - only use vendored dependencies
    #[arg(long = "offline")]
    pub offline: bool,

    /// Apply board config (default: true)
    #[arg(
        long = "sync-board-config",
        action = clap::ArgAction::Set,
        default_value_t = true,
        value_parser = clap::builder::BoolishValueParser::new(),
        num_args = 0..=1,
        default_missing_value = "true"
    )]
    pub sync_board_config: bool,

    /// Generate layout in a temporary directory (fresh layout, opens KiCad)
    #[arg(long = "temp")]
    pub temp: bool,

    /// Run KiCad DRC checks after layout generation
    #[arg(long = "check")]
    pub check: bool,

    /// Suppress diagnostics by kind or severity. Use 'warnings' or 'errors' for all
    /// warnings/errors, or specific kinds like 'layout.drc.clearance'.
    /// Supports hierarchical matching (e.g., 'layout.drc' matches 'layout.drc.clearance')
    #[arg(short = 'S', long = "suppress", value_name = "KIND")]
    pub suppress: Vec<String>,

    /// Require that pcb.toml and pcb.sum are up-to-date. Fails if auto-deps would
    /// add dependencies or if the lockfile would be modified. Recommended for CI.
    #[arg(long)]
    pub locked: bool,
}

pub fn execute(mut args: LayoutArgs) -> Result<()> {
    // --check implies --no-open
    if args.check {
        args.no_open = true;
    }

    // Default to locked mode in CI environments
    let locked = args.locked || std::env::var("CI").is_ok();

    // V2 workspace-first architecture: resolve dependencies before finding .zen files
    let (_workspace_info, resolution_result) = crate::resolve::resolve_v2_if_needed(
        args.paths.first().map(|p| p.as_path()),
        args.offline,
        locked,
    )?;

    // Collect .zen files to process - always recursive for directories
    let zen_paths = file_walker::collect_zen_files(&args.paths, false)?;

    if zen_paths.is_empty() {
        let cwd = std::env::current_dir()?;
        anyhow::bail!(
            "No .zen source files found in {}",
            cwd.canonicalize().unwrap_or(cwd).display()
        );
    }

    let mut has_errors = false;
    let mut has_warnings = false;
    let mut generated_layouts = Vec::new();

    // Process each .zen file
    for zen_path in zen_paths {
        let file_name = zen_path.file_name().unwrap().to_string_lossy().to_string();
        let Some(schematic) = build(
            &zen_path,
            args.offline,
            create_diagnostics_passes(&args.suppress, &[]),
            false, // don't deny warnings for layout command
            &mut has_errors,
            &mut has_warnings,
            resolution_result.clone(),
        ) else {
            continue;
        };

        // Get pcb_file and sync_diagnostics based on mode
        let spinner_msg = if args.check {
            format!("{file_name}: Checking layout")
        } else {
            format!("{file_name}: Generating layout")
        };
        let spinner = Spinner::builder(spinner_msg).start();

        // Both modes use process_layout, dry_run controls whether to modify the board
        let result = process_layout(
            &schematic,
            &zen_path,
            args.sync_board_config,
            args.temp,
            args.check, // dry_run
        )
        .map(|r| (r.pcb_file, r.sync_diagnostics));

        let (pcb_file, sync_diagnostics) = match result {
            Ok(r) => r,
            Err(LayoutError::NoLayoutPath) => {
                spinner.finish();
                println!(
                    "{} {} (no layout)",
                    pcb_ui::icons::warning(),
                    file_name.with_style(Style::Yellow).bold(),
                );
                continue;
            }
            Err(LayoutError::NoLayoutFile(_)) => {
                spinner.finish();
                println!(
                    "{} {} (no layout file)",
                    pcb_ui::icons::warning(),
                    file_name.with_style(Style::Yellow).bold(),
                );
                continue;
            }
            Err(e) => {
                spinner.finish();
                println!(
                    "{} {}: {e:#}",
                    pcb_ui::icons::error(),
                    file_name.with_style(Style::Red).bold()
                );
                has_errors = true;
                continue;
            }
        };

        spinner.finish();
        let relative_path = zen_path
            .parent()
            .and_then(|parent| pcb_file.strip_prefix(parent).ok())
            .unwrap_or(&pcb_file);
        println!(
            "{} {} ({})",
            pcb_ui::icons::success(),
            file_name.clone().with_style(Style::Green).bold(),
            relative_path.display()
        );

        // Run DRC checks in --check mode
        if args.check {
            let drc_spinner = Spinner::builder(format!("{file_name}: Running DRC checks")).start();
            let result = drc_spinner
                .suspend(|| drc::run_and_print_drc(&pcb_file, &args.suppress, &sync_diagnostics));

            match result {
                Ok((had_errors, _warnings)) => {
                    drc_spinner.finish();
                    if had_errors {
                        has_errors = true;
                    }
                }
                Err(e) => {
                    drc_spinner.finish();
                    eprintln!(
                        "{} {}: {e:#}",
                        pcb_ui::icons::error(),
                        file_name.with_style(Style::Red).bold()
                    );
                    has_errors = true;
                }
            }
        }

        generated_layouts.push((zen_path.clone(), pcb_file));
    }

    if has_errors {
        std::process::exit(1);
    }

    if generated_layouts.is_empty() {
        println!("\nNo layouts found.");
        return Ok(());
    }

    // Open the selected layout if not disabled (or if using temp)
    if (!args.no_open || args.temp) && !generated_layouts.is_empty() {
        let layout_to_open = if generated_layouts.len() == 1 && !args.select {
            // Only one layout and not forcing selection - open it directly
            &generated_layouts[0].1
        } else {
            // Multiple layouts or forced selection - let user choose
            let selected_idx = choose_layout(&generated_layouts)?;
            &generated_layouts[selected_idx].1
        };

        open::that(layout_to_open)?;
    }

    Ok(())
}

/// Let the user choose which layout to open
fn choose_layout(layouts: &[(PathBuf, PathBuf)]) -> Result<usize> {
    // Get current directory for making relative paths
    let cwd = std::env::current_dir()?;

    let options: Vec<String> = layouts
        .iter()
        .map(|(star_file, _)| {
            // Try to make the path relative to current directory
            star_file
                .strip_prefix(&cwd)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| star_file.display().to_string())
        })
        .collect();

    let selection = Select::new("Select a layout to open:", options.clone())
        .prompt()
        .context("Failed to get user selection")?;

    // Find which index was selected
    options
        .iter()
        .position(|option| option == &selection)
        .ok_or_else(|| anyhow::anyhow!("Invalid selection"))
}
