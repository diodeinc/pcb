use anyhow::Result;
use clap::Args;
use std::path::PathBuf;

#[derive(Args, Debug)]
#[command(about = "Embed a STEP model into a KiCad footprint")]
pub struct EmbedStepArgs {
    /// Path to the .kicad_mod footprint to modify
    #[arg(value_name = "FOOTPRINT", value_hint = clap::ValueHint::FilePath)]
    pub footprint: PathBuf,

    /// Path to the .step or .stp model to embed
    #[arg(value_name = "STEP", value_hint = clap::ValueHint::FilePath)]
    pub step: PathBuf,
}

pub fn execute(args: EmbedStepArgs) -> Result<()> {
    pcb_kicad::footprint::embed_step_into_footprint_file(&args.footprint, &args.step, false)
}
