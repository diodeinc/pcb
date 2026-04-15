use anyhow::{Result, bail};
use clap::Args;
use std::path::PathBuf;

#[derive(Args, Debug, Clone)]
#[command(about = "Export a .zen design as a standalone KiCad project")]
pub struct ExportKicadArgs {
    /// Path to .zen file
    #[arg(value_name = "FILE", value_hint = clap::ValueHint::FilePath)]
    pub file: PathBuf,

    /// Output path for the exported KiCad project directory
    #[arg(short = 'o', long = "output", value_name = "PATH", value_hint = clap::ValueHint::AnyPath)]
    pub output: PathBuf,

    /// Disable network access (offline mode) - only use vendored dependencies
    #[arg(long = "offline")]
    pub offline: bool,

    /// Require that pcb.toml is up-to-date and verify pcb.sum if it exists.
    #[arg(long)]
    pub locked: bool,
}

pub fn execute(args: ExportKicadArgs) -> Result<()> {
    crate::file_walker::require_zen_file(&args.file)?;
    bail!("pcb export-kicad is not yet implemented");
}
