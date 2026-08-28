use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use pcb_ipc2581_tools::{LayoutTarget, commands};

use crate::layout::LayoutArgs;

#[derive(Args, Debug)]
#[command(about = "Run DFM checks for a .zen board")]
pub struct DfmArgs {
    /// Path to the board .zen file
    #[arg(value_name = "FILE", value_hint = clap::ValueHint::FilePath)]
    pub file: PathBuf,

    /// Built-in PDK name or fabrication PDK TOML path (built-ins: standard)
    #[arg(long)]
    pub pdk: PathBuf,

    /// Disable network access (offline mode) - only use vendored dependencies
    #[arg(long = "offline")]
    pub offline: bool,
}

pub fn execute(args: DfmArgs) -> Result<()> {
    let layout_args = LayoutArgs {
        file: args.file.clone(),
        no_open: true,
        offline: args.offline,
        ..Default::default()
    };
    let design = crate::layout::prepare_design(&layout_args)?;
    let layout = crate::layout::apply_prepared(&layout_args, design)?;
    let pcb_file = layout
        .pcb_file_abs
        .as_deref()
        .with_context(|| format!("{} does not declare a layout", args.file.display()))?;

    let temporary_dir = tempfile::tempdir().context("failed to create temporary DFM directory")?;
    let ipc_path = temporary_dir.path().join("ipc2581.xml");
    crate::release::export_ipc2581(pcb_file, &ipc_path)?;

    commands::dfm::execute_check(
        &ipc_path,
        &commands::dfm::CheckOptions {
            pdk: args.pdk,
            waivers: None,
            output: None,
            layout_target: LayoutTarget::Board,
        },
    )
}
