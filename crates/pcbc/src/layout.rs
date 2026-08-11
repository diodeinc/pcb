use anyhow::{Result, bail};
use clap::Args;
use pcb_layout::{LayoutOptions, process_layout, utils as layout_utils};
use pcb_sch::Schematic;
use pcb_ui::prelude::*;
use serde::Serialize;
use std::path::{Path, PathBuf};

use crate::build::{BuildEvalState, create_diagnostics_passes};
use crate::config_input::{CONFIG_ARG_HELP, parse_config_overrides};
use crate::drc;

#[derive(Args, Debug, Default, Clone)]
#[command(about = "Generate PCB layout files from a .zen file")]
pub struct LayoutArgs {
    /// Path to .zen file or diode:// sandbox URI
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

    /// Run KiCad DRC checks after layout generation
    #[arg(long = "check")]
    pub check: bool,

    /// Suppress diagnostics by kind or severity. Use 'warnings' or 'errors' for all
    /// warnings/errors, or specific kinds like 'layout.drc.clearance'.
    /// Supports hierarchical matching (e.g., 'layout.drc' matches 'layout.drc.clearance')
    #[arg(short = 'S', long = "suppress", value_name = "KIND")]
    pub suppress: Vec<String>,

    /// Resolve existing layout files without updating them
    #[arg(long = "no-sync", conflicts_with = "check")]
    pub no_sync: bool,

    /// Reload managed footprint definitions from source; preserve placement and routing
    #[arg(long, conflicts_with_all = ["check", "no_sync"])]
    pub sync_footprints: bool,

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

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum LayoutAction {
    Created,
    Updated,
    Checked,
    Unchanged,
}

impl LayoutAction {
    fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Updated => "updated",
            Self::Checked => "checked",
            Self::Unchanged => "unchanged",
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LayoutCommandResult {
    pub(crate) source_file: PathBuf,
    pub(crate) layout_dir: Option<PathBuf>,
    pub(crate) pcb_file: Option<PathBuf>,
    pub(crate) action: Option<LayoutAction>,
}

pub(crate) struct PreparedDesign {
    pub(crate) zen_path: PathBuf,
    pub(crate) file_name: String,
    pub(crate) schematic: Schematic,
    pub(crate) eval_output: Option<pcb_zen_core::EvalOutput>,
}

pub fn execute(mut args: LayoutArgs) -> Result<()> {
    if let Some(uri) = crate::sandbox_uri::parse_sandbox_file_arg(&args.file)? {
        crate::sandbox_uri::require_remote_zen_file(&uri)?;
        return crate::remote_sandbox::execute_layout(uri, args);
    }

    // --check implies --no-open
    if args.check {
        args.no_open = true;
    }

    let design = prepare_design(&args)?;
    let result = apply_prepared(&args, design)?;
    print_layout_result(&result, args.format)?;
    if !args.no_open
        && let Some(pcb_file) = &result.pcb_file
    {
        pcb_kicad::open_pcbnew(pcb_file)?;
    }
    Ok(())
}

pub(crate) fn prepare_design(args: &LayoutArgs) -> Result<PreparedDesign> {
    prepare_design_with_schematic_analysis(args, true)
}

pub(crate) fn prepare_design_for_apply(args: &LayoutArgs) -> Result<PreparedDesign> {
    prepare_design_with_schematic_analysis(args, false)
}

fn prepare_design_with_schematic_analysis(
    args: &LayoutArgs,
    analyze_linked_schematic: bool,
) -> Result<PreparedDesign> {
    crate::file_walker::require_zen_file(&args.file)?;
    let config_inputs = parse_config_overrides(&args.config)?;

    // Resolve dependencies before building
    let resolution_result = crate::resolve::resolve(Some(&args.file), args.offline)?;

    let zen_path = &args.file;
    let file_name = zen_path.file_name().unwrap().to_string_lossy().to_string();

    let mut build_state = BuildEvalState::new(resolution_result);
    if !analyze_linked_schematic {
        build_state = build_state.without_linked_schematic_analysis();
    }
    let build_result = build_state.build(
        zen_path,
        config_inputs,
        create_diagnostics_passes(&args.suppress, &[]),
        false,
        &mut false.clone(),
        &mut false.clone(),
    );
    let Some(schematic) = build_result.schematic else {
        anyhow::bail!("Build failed");
    };

    Ok(PreparedDesign {
        zen_path: zen_path.to_path_buf(),
        file_name,
        schematic,
        eval_output: build_result.eval_output,
    })
}

pub(crate) fn apply_prepared(
    args: &LayoutArgs,
    design: PreparedDesign,
) -> Result<LayoutCommandResult> {
    let hide_progress = args.format == LayoutOutputFormat::Json;
    let PreparedDesign {
        zen_path,
        file_name,
        schematic,
        eval_output,
    } = design;

    if args.no_sync {
        return resolve_existing_layout(&zen_path, &schematic);
    }

    // Layout consumes the footprints, so validate their contents (including
    // embedded payloads) before generating the board.
    if let Some(eval_output) = &eval_output {
        let mut footprint_diagnostics = pcb_zen_core::Diagnostics::default();
        footprint_diagnostics
            .diagnostics
            .extend(eval_output.validate_footprints());
        if !footprint_diagnostics.diagnostics.is_empty() {
            drc::render_diagnostics(&mut footprint_diagnostics, &args.suppress);
            if footprint_diagnostics.error_count() > 0 {
                anyhow::bail!("Invalid footprints");
            }
        }
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
        LayoutOptions {
            check: args.check,
            sync_footprints: args.sync_footprints,
        },
        &mut diagnostics,
    )?;
    spinner.finish();

    let Some(layout_result) = result else {
        drc::render_diagnostics(&mut diagnostics, &args.suppress);
        if diagnostics.error_count() > 0 {
            anyhow::bail!("Layout sync failed with errors");
        }

        return Ok(LayoutCommandResult {
            source_file: zen_path,
            layout_dir: None,
            pcb_file: None,
            action: None,
        });
    };
    let pcb_file = layout_result.pcb_file.clone();
    let display_pcb_file = layout_result.display_pcb_file().to_path_buf();

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

    Ok(LayoutCommandResult {
        source_file: zen_path,
        layout_dir: Some(layout_result.layout_dir),
        pcb_file: Some(display_pcb_file),
        action: Some(if args.check {
            LayoutAction::Checked
        } else if layout_result.created {
            LayoutAction::Created
        } else {
            LayoutAction::Updated
        }),
    })
}

fn resolve_existing_layout(zen_path: &Path, schematic: &Schematic) -> Result<LayoutCommandResult> {
    let Some(layout_dir) = layout_utils::resolve_layout_dir(schematic)? else {
        return Ok(LayoutCommandResult {
            source_file: zen_path.to_path_buf(),
            layout_dir: None,
            pcb_file: None,
            action: None,
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
        action: Some(LayoutAction::Unchanged),
    })
}

pub(crate) fn print_layout_result(
    result: &LayoutCommandResult,
    format: LayoutOutputFormat,
) -> Result<()> {
    match format {
        LayoutOutputFormat::Json => println!("{}", serde_json::to_string_pretty(result)?),
        LayoutOutputFormat::Human => {
            if let (Some(pcb_file), Some(action)) = (&result.pcb_file, result.action) {
                let file_name = result
                    .source_file
                    .file_name()
                    .unwrap_or(result.source_file.as_os_str())
                    .to_string_lossy();
                println!(
                    "{} {} layout {} ({})",
                    pcb_ui::icons::success(),
                    file_name.with_style(Style::Green).bold(),
                    action.as_str(),
                    pcb_file.display()
                );
            }
        }
    }
    Ok(())
}
