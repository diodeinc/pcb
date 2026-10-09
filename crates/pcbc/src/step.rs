//! `pcb step`: STEP export of a KiCad board through `pcb-step`. Models
//! come from the board's embedded files; nothing is looked up on disk.

use anyhow::Result;
use clap::{Args, Subcommand};
use pcb_step::cli::ExportArgs;

#[derive(Args)]
pub struct StepArgs {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Export a board to a STEP assembly: body, drills, models, silkscreen and solder mask
    Export(ExportArgs),
}

pub fn execute(args: StepArgs) -> Result<()> {
    let Commands::Export(args) = args.command;
    args.run("step", |board, options, sink, report| {
        Ok(pcb_step::export(board, options, sink, report)?)
    })
}
