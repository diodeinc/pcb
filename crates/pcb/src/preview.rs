use anyhow::Result;
use clap::Args;
use std::path::PathBuf;

use crate::file_walker;
use crate::publish;
use crate::release;

#[derive(Args, Debug)]
#[command(about = "Build and upload a preview bundle for a board, including dirty worktrees")]
pub struct PreviewArgs {
    /// Path to a .zen file
    #[arg(value_name = "FILE", value_hint = clap::ValueHint::FilePath)]
    pub file: PathBuf,

    /// Suppress diagnostics by kind or severity
    #[arg(short = 'S', long = "suppress", value_name = "KIND")]
    pub suppress: Vec<String>,

    /// Exclude specific manufacturing artifacts from the release (can be specified multiple times)
    #[arg(long, value_enum)]
    pub exclude: Vec<release::ArtifactType>,
}

pub fn execute(args: PreviewArgs) -> Result<()> {
    let target = file_walker::resolve_board_target(&args.file, "preview")?;
    publish::run_board_preview(target, args.suppress, args.exclude, false, false)
}
