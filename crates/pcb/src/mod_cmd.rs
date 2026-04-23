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
    Add(add::ModAddArgs),

    /// Print why a dependency is needed
    Why(add::ModWhyArgs),

    /// Print the lane-aware dependency graph
    Graph(add::ModGraphArgs),

    /// Reconcile source imports and hydrate package dependency manifests
    Tidy(add::TidyArgs),
}

pub fn execute(args: ModArgs) -> anyhow::Result<()> {
    match args.command {
        ModCommand::Add(args) => add::execute_mod_add(args),
        ModCommand::Why(args) => add::execute_mod_why(args),
        ModCommand::Graph(args) => add::execute_mod_graph(args),
        ModCommand::Tidy(args) => add::execute_tidy(args),
    }
}
