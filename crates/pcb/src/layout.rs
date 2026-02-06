use anyhow::Result;
use clap::Args;
use pcb_layout::process_layout;
use pcb_ui::prelude::*;
use std::path::PathBuf;

use crate::build::{build, create_diagnostics_passes};
use crate::drc;

#[derive(Args, Debug, Default, Clone)]
#[command(about = "Generate PCB layout files from a .zen file")]
pub struct LayoutArgs {
    /// Path to .zen file
    #[arg(value_name = "FILE", value_hint = clap::ValueHint::FilePath)]
    pub file: PathBuf,

    /// Skip opening the layout file after generation
    #[arg(long)]
    pub no_open: bool,

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
    crate::file_walker::require_zen_file(&args.file)?;

    // --check implies --no-open
    if args.check {
        args.no_open = true;
    }

    // Default to locked mode in CI environments
    let locked = args.locked || std::env::var("CI").is_ok();

    // Resolve dependencies before building
    let (_workspace_info, resolution_result) =
        crate::resolve::resolve(args.file.parent(), args.offline, locked)?;

    let zen_path = &args.file;
    let file_name = zen_path.file_name().unwrap().to_string_lossy().to_string();

    let Some(schematic) = build(
        zen_path,
        create_diagnostics_passes(&args.suppress, &[]),
        false,
        &mut false.clone(),
        &mut false.clone(),
        resolution_result,
    ) else {
        anyhow::bail!("Build failed");
    };

    // Process layout and collect diagnostics
    let spinner_msg = if args.check {
        format!("{file_name}: Checking layout")
    } else {
        format!("{file_name}: Generating layout")
    };
    let spinner = Spinner::builder(spinner_msg).start();
    let mut diagnostics = pcb_zen_core::Diagnostics::default();
    let result = process_layout(
        &schematic,
        zen_path,
        args.sync_board_config,
        args.temp,
        args.check, // dry_run
        &mut diagnostics,
    )?;
    spinner.finish();

    let Some(layout_result) = result else {
        drc::render_diagnostics(&mut diagnostics, &args.suppress);
        if diagnostics.error_count() > 0 {
            anyhow::bail!("Layout sync failed with errors");
        }

        return Ok(());
    };
    let pcb_file = layout_result.pcb_file;

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

    // Run DRC in check mode
    if args.check {
        let spinner = Spinner::builder(format!("{file_name}: Running DRC checks")).start();
        pcb_kicad::run_drc(&pcb_file, &mut diagnostics)?;
        spinner.finish();
    }

    // Render diagnostics
    drc::render_diagnostics(&mut diagnostics, &args.suppress);
    if diagnostics.error_count() > 0 {
        anyhow::bail!("DRC failed");
    }

    // Open the layout if not disabled (or if using temp)
    if !args.no_open || args.temp {
        open::that(&pcb_file)?;
    }

    Ok(())
}
