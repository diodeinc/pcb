//! `pcb gltf`: glTF export of a KiCad board through `pcb-gltf`. Takes the
//! same arguments as `pcb step export`, as `kicad-cli pcb export glb` takes
//! those of `kicad-cli pcb export step`.

use anyhow::Result;
use clap::{Args, Subcommand};
use pcb_step::cli::ExportArgs;

#[derive(Args)]
pub struct GltfArgs {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Export a board to a GLB file: body, drills, models, silkscreen and solder mask
    Export(ExportArgs),
}

pub fn execute(args: GltfArgs) -> Result<()> {
    let Commands::Export(args) = args.command;
    args.run("glb", pcb_gltf::export)
}
