//! `pcb gltf`: glTF export of a KiCad board through `pcb-gltf`. Takes the
//! arguments of `pcb step export`, as `kicad-cli pcb export glb` takes those
//! of `kicad-cli pcb export step`, but always exports outer copper, as
//! KiCad's VRML export does.

use anyhow::Result;
use clap::{Args, Subcommand};
use pcb_step::cli::{CopperArgs, ExportArgs, VisualArgs};

#[derive(Args)]
pub struct GltfArgs {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Export a board to a GLB file: body, drills, copper, models, silkscreen and solder mask
    Export {
        #[command(flatten)]
        args: ExportArgs,
        #[command(flatten)]
        visual: VisualArgs,
    },
}

pub fn execute(args: GltfArgs) -> Result<()> {
    let Commands::Export { args, visual } = args.command;
    args.run("glb", CopperArgs::OUTER, Some(&visual), pcb_gltf::export)
}
