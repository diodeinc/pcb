use anyhow::bail;
use clap::{Args, Subcommand};

use crate::add;

#[derive(Args, Debug)]
#[command(about = "Manage package dependency manifests")]
pub struct ModArgs {
    #[command(subcommand)]
    command: ModCommand,
}

#[derive(Subcommand, Debug)]
enum ModCommand {
    /// Add or update a direct dependency
    Add(ModAddArgs),

    /// Reconcile source imports and hydrate package dependency manifests
    Tidy(add::TidyArgs),
}

#[derive(Args, Debug)]
pub struct ModAddArgs {
    /// Dependency to add or update
    #[arg(value_name = "DEPENDENCY")]
    dependency: String,
}

pub fn execute(args: ModArgs) -> anyhow::Result<()> {
    match args.command {
        ModCommand::Add(_args) => bail!("`pcb mod add` is not implemented yet."),
        ModCommand::Tidy(args) => add::execute_tidy(args),
    }
}
