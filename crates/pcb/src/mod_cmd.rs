use clap::{Args, Subcommand};

use crate::pcb_mod;

#[derive(Args, Debug)]
#[command(about = "Manage package dependency manifests")]
pub struct ModArgs {
    #[command(subcommand)]
    command: ModCommand,
}

#[derive(Subcommand, Debug)]
enum ModCommand {
    /// Add or update a direct dependency
    Add(pcb_mod::ModAddArgs),

    /// Print why a dependency is needed
    Why(pcb_mod::ModWhyArgs),

    /// Print the lane-aware dependency graph
    Graph(pcb_mod::ModGraphArgs),

    /// Print the frozen MVS v2 resolution table for a target
    Resolve(pcb_mod::ModResolveArgs),

    /// Download modules to the package cache
    Download(pcb_mod::ModDownloadArgs),

    /// Reconcile source imports and hydrate package dependency manifests
    Sync(pcb_mod::SyncArgs),
}

pub fn execute(args: ModArgs) -> anyhow::Result<()> {
    match args.command {
        ModCommand::Add(args) => pcb_mod::execute_mod_add(args),
        ModCommand::Why(args) => pcb_mod::execute_mod_why(args),
        ModCommand::Graph(args) => pcb_mod::execute_mod_graph(args),
        ModCommand::Resolve(args) => pcb_mod::execute_mod_resolve(args),
        ModCommand::Download(args) => pcb_mod::execute_mod_download(args),
        ModCommand::Sync(args) => pcb_mod::execute_sync(args),
    }
}
