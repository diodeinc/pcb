use pcb_ir::geom::{GeometryAccuracy, Resolution};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use pcb_ipc2581_tools::{LayoutTarget, commands};

use crate::layout::LayoutArgs;

// Findings are calibrated at 10 µm, independently of the CLI export budget.
pub(crate) const DFM_RESOLUTION: Resolution = Resolution {
    tolerance_mm: pcb_ir::geom::tol::REGION_MM,
    accuracy: GeometryAccuracy::micrometres(10),
};

#[derive(Args, Debug)]
#[command(about = "Run DFM checks for a .zen board")]
pub struct DfmArgs {
    /// Path to a .zen, .kicad_pcb, or .kicad_pro file. Checks the saved layout as-is.
    #[arg(value_name = "FILE", value_hint = clap::ValueHint::FilePath)]
    pub file: PathBuf,

    /// Built-in PDK name or fabrication PDK TOML path
    #[arg(long, default_value = "standard")]
    pub pdk: PathBuf,

    /// Output self-contained JSON report path. Omit to write to stdout.
    #[arg(short, long, value_hint = clap::ValueHint::FilePath)]
    pub output: Option<PathBuf>,

    /// Open the report in dfm.diode.computer after writing it
    #[arg(long)]
    pub open: bool,

    /// Disable network access (offline mode) - only use vendored dependencies
    #[arg(long = "offline")]
    pub offline: bool,
}

pub fn execute(args: DfmArgs) -> Result<()> {
    let temporary_output = if args.open && args.output.is_none() {
        Some(tempfile::tempdir().context("failed to create temporary DFM report directory")?)
    } else {
        None
    };
    let output = args.output.clone().or_else(|| {
        temporary_output.as_ref().map(|directory| {
            let filename = args
                .file
                .with_extension("dfm.json")
                .file_name()
                .unwrap_or_default()
                .to_owned();
            directory.path().join(filename)
        })
    });
    let options = commands::dfm::CheckOptions {
        pdk: args.pdk.clone(),
        waivers: None,
        output,
        layout_target: LayoutTarget::Board,
    };
    commands::dfm::validate_output(&args.file, &options)?;
    let dfm_result = match export_layout(&args) {
        Ok((_temporary_dir, ipc_path)) => {
            match commands::dfm::execute_check(&ipc_path, &options, DFM_RESOLUTION)? {
                commands::dfm::CheckOutcome::Passed => Ok(()),
                commands::dfm::CheckOutcome::Failed(error) => Err(error),
            }
        }
        Err(error) => {
            commands::dfm::write_error_report(&args.file, &options, &error)
                .with_context(|| format!("DFM check was incomplete: {error:#}"))?;
            Err(error)
        }
    };

    if !args.open {
        return dfm_result;
    }

    if let Err(open_error) = crate::open::open_dfm_report(
        options
            .output
            .as_deref()
            .expect("--open always selects a report file"),
    ) {
        if let Some(directory) = temporary_output {
            let _ = directory.keep();
            anstream::eprintln!(
                "DFM report kept at {}",
                options.output.as_deref().unwrap().display()
            );
        }
        if dfm_result.is_ok() {
            return Err(open_error);
        }
        anstream::eprintln!("Warning: {open_error:#}");
    }
    dfm_result
}

fn export_layout(args: &DfmArgs) -> Result<(tempfile::TempDir, PathBuf)> {
    match args.file.extension().and_then(|ext| ext.to_str()) {
        Some("kicad_pcb") => return export_ipc(&args.file),
        Some("kicad_pro") => {
            anyhow::ensure!(
                args.file.is_file(),
                "Project file not found: {}",
                args.file.display()
            );
            return export_ipc(&args.file.with_extension("kicad_pcb"));
        }
        _ => {}
    }
    let layout_args = LayoutArgs {
        file: args.file.clone(),
        no_open: true,
        offline: args.offline,
        // Only evaluate the source to locate its saved layout.
        skip_bom_hydration: true,
        no_sync: true,
        ..Default::default()
    };
    let design = crate::layout::prepare_design(&layout_args)?;
    let layout = crate::layout::resolve_existing_layout(&args.file, &design.schematic)
        .with_context(|| {
            format!(
                "Could not find the existing layout. Run 'pcb layout {}' to generate it.",
                args.file.display()
            )
        })?;
    let pcb_file = layout
        .pcb_file_abs
        .as_deref()
        .with_context(|| format!("{} does not declare a layout", args.file.display()))?;
    export_ipc(pcb_file)
}

/// Export a board file to a temporary IPC-2581 document for checking.
fn export_ipc(pcb_file: &std::path::Path) -> Result<(tempfile::TempDir, PathBuf)> {
    anyhow::ensure!(
        pcb_file.is_file(),
        "Layout file not found: {}",
        pcb_file.display()
    );
    let temporary_dir = tempfile::tempdir().context("failed to create temporary DFM directory")?;
    let ipc_path = temporary_dir.path().join("ipc2581.xml");
    crate::release::export_ipc2581(pcb_file, &ipc_path)?;
    Ok((temporary_dir, ipc_path))
}
