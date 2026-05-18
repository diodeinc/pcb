use anyhow::{Result, bail};
use clap::Args;
use pcb_layout::{process_layout, utils as layout_utils};
use pcb_sch::Schematic;
use pcb_ui::prelude::*;
use serde::Serialize;
use std::path::{Path, PathBuf};

use crate::build::{build, create_diagnostics_passes};
use crate::config_input::{CONFIG_ARG_HELP, parse_config_overrides};
use crate::drc;

#[derive(Args, Debug, Default, Clone)]
#[command(about = "Generate PCB layout files from a .zen file")]
pub struct LayoutArgs {
    /// Path to .zen file
    #[arg(value_name = "FILE", value_hint = clap::ValueHint::FilePath)]
    pub file: PathBuf,

    #[arg(long = "config", value_name = "KEY=VALUE", help = CONFIG_ARG_HELP)]
    pub config: Vec<String>,

    /// Skip opening the layout file after generation
    #[arg(long)]
    pub no_open: bool,

    /// Disable network access (offline mode) - only use vendored dependencies
    #[arg(long = "offline")]
    pub offline: bool,

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

    /// Require that pcb.toml is up-to-date and verify pcb.sum if it exists.
    /// Does not write pcb.toml or pcb.sum. Recommended for CI.
    #[arg(long)]
    pub locked: bool,

    /// Resolve existing layout files without updating them
    #[arg(long = "no-sync", conflicts_with_all = ["temp", "check"])]
    pub no_sync: bool,

    /// Output format
    #[arg(short = 'f', long, value_enum, default_value_t = LayoutOutputFormat::Human)]
    pub format: LayoutOutputFormat,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum LayoutOutputFormat {
    /// Human-readable output
    #[default]
    Human,
    /// JSON output
    Json,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LayoutCommandResult {
    source_file: PathBuf,
    layout_dir: Option<PathBuf>,
    pcb_file: Option<PathBuf>,
}

pub fn execute(mut args: LayoutArgs) -> Result<()> {
    crate::file_walker::require_zen_file(&args.file)?;
    let config_inputs = parse_config_overrides(&args.config)?;
    let hide_progress = args.format == LayoutOutputFormat::Json;

    // --check implies --no-open
    if args.check {
        args.no_open = true;
    }

    // Default to locked mode in CI environments
    let locked = args.locked || std::env::var("CI").is_ok();

    // Resolve dependencies before building
    let resolution_result = crate::resolve::resolve(Some(&args.file), args.offline, locked)?;
    let model_dirs = resolution_result.kicad_model_dirs();

    let zen_path = &args.file;
    let file_name = zen_path.file_name().unwrap().to_string_lossy().to_string();

    let Some(schematic) = build(
        zen_path,
        config_inputs,
        create_diagnostics_passes(&args.suppress, &[]),
        false,
        &mut false.clone(),
        &mut false.clone(),
        resolution_result,
    ) else {
        anyhow::bail!("Build failed");
    };

    if args.no_sync {
        let result = resolve_existing_layout(zen_path, &schematic)?;
        print_layout_result(&result, args.format, zen_path, &file_name)?;

        if !args.no_open
            && let Some(pcb_file) = &result.pcb_file
        {
            pcb_kicad::open_pcbnew(pcb_file)?;
        }

        return Ok(());
    }

    // Process layout and collect diagnostics
    let spinner_msg = if args.check {
        format!("{file_name}: Checking layout")
    } else {
        format!("{file_name}: Generating layout")
    };
    let spinner = Spinner::builder(spinner_msg).hidden(hide_progress).start();
    let mut diagnostics = pcb_zen_core::Diagnostics::default();
    let result = process_layout(
        &schematic,
        &model_dirs,
        args.temp,
        args.check,
        &mut diagnostics,
    )?;
    spinner.finish();

    let Some(layout_result) = result else {
        drc::render_diagnostics(&mut diagnostics, &args.suppress);
        if diagnostics.error_count() > 0 {
            anyhow::bail!("Layout sync failed with errors");
        }

        print_layout_result(
            &LayoutCommandResult {
                source_file: zen_path.to_path_buf(),
                layout_dir: None,
                pcb_file: None,
            },
            args.format,
            zen_path,
            &file_name,
        )?;

        return Ok(());
    };
    let pcb_file = layout_result.pcb_file.clone();
    let display_pcb_file = layout_result.display_pcb_file().to_path_buf();

    print_layout_result(
        &LayoutCommandResult {
            source_file: zen_path.to_path_buf(),
            layout_dir: Some(layout_result.layout_dir.clone()),
            pcb_file: Some(display_pcb_file.clone()),
        },
        args.format,
        zen_path,
        &file_name,
    )?;

    // Run DRC in check mode.
    if args.check {
        let spinner = Spinner::builder(format!("{file_name}: Running DRC checks"))
            .hidden(hide_progress)
            .start();
        let drc_output = tempfile::NamedTempFile::new()?;
        let working_dir = pcb_file.parent();
        let report = pcb_kicad::run_drc(&pcb_file, false, working_dir, drc_output.path())?;
        report.add_to_diagnostics(&mut diagnostics, &display_pcb_file.to_string_lossy());
        spinner.finish();
    }

    // Render diagnostics
    drc::render_diagnostics(&mut diagnostics, &args.suppress);
    if diagnostics.error_count() > 0 {
        anyhow::bail!("DRC failed");
    }

    // Open the layout if not disabled (or if using temp)
    if !args.no_open || args.temp {
        pcb_kicad::open_pcbnew(&pcb_file)?;
    }

    Ok(())
}

fn resolve_existing_layout(zen_path: &Path, schematic: &Schematic) -> Result<LayoutCommandResult> {
    let Some(layout_dir) = layout_utils::resolve_layout_dir(schematic)? else {
        return Ok(LayoutCommandResult {
            source_file: zen_path.to_path_buf(),
            layout_dir: None,
            pcb_file: None,
        });
    };

    let kicad_files = layout_utils::require_kicad_files(&layout_dir)?;
    let pcb_file = kicad_files.kicad_pcb();
    if !pcb_file.exists() {
        bail!(
            "Layout file not found: {}. Run 'pcb layout {}' to generate it.",
            pcb_file.display(),
            zen_path.display()
        );
    }

    Ok(LayoutCommandResult {
        source_file: zen_path.to_path_buf(),
        layout_dir: Some(layout_dir),
        pcb_file: Some(pcb_file),
    })
}

fn print_layout_result(
    result: &LayoutCommandResult,
    format: LayoutOutputFormat,
    zen_path: &Path,
    file_name: &str,
) -> Result<()> {
    match format {
        LayoutOutputFormat::Json => println!("{}", serde_json::to_string_pretty(result)?),
        LayoutOutputFormat::Human => {
            if let Some(pcb_file) = &result.pcb_file {
                let relative_path = zen_path
                    .parent()
                    .and_then(|parent| pcb_file.strip_prefix(parent).ok())
                    .unwrap_or(pcb_file);
                println!(
                    "{} {} ({})",
                    pcb_ui::icons::success(),
                    file_name.with_style(Style::Green).bold(),
                    relative_path.display()
                );
            }
        }
    }
    Ok(())
}
