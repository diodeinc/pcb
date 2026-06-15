//! Legacy dependency update command.

use anyhow::Result;
use clap::Args;
use std::path::PathBuf;

#[derive(Args, Debug)]
#[command(about = "Update dependencies to latest compatible versions")]
pub struct UpdateArgs {
    /// Path to workspace or package.
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Legacy filter retained for CLI compatibility.
    #[arg(long, short = 'p', hide = true)]
    pub packages: Vec<String>,
}

pub fn execute(_args: UpdateArgs) -> Result<()> {
    anyhow::bail!(
        "`pcb update` is no longer supported. Use `pcb add -u` from the package directory instead."
    )
}
