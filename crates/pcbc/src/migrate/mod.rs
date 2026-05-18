use anyhow::Result;
use clap::Args;
use std::path::PathBuf;

/// Arguments for the `migrate` command
#[derive(Args, Debug, Default, Clone)]
#[command(about = "Run available PCB project migrations")]
pub struct MigrateArgs {
    /// One or more paths to consider for migration.
    #[arg(value_name = "PATHS", value_hint = clap::ValueHint::AnyPath)]
    pub paths: Vec<PathBuf>,
}

/// Execute the `migrate` command
pub fn execute(_args: MigrateArgs) -> Result<()> {
    eprintln!("No migrations are currently available for this release.");
    Ok(())
}
