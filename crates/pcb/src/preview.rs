use anyhow::Result;
use clap::Args;
use colored::Colorize;
use pcb_zen::git;
use std::path::{Path, PathBuf};

use crate::file_walker;
use crate::release;

#[derive(Args, Debug)]
#[command(about = "Build and upload a preview release for a board")]
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

    let version = preview_version(&target.workspace.root);

    let zip_path = release::build_board_release(
        target.workspace,
        target.zen_path,
        target.board_name,
        args.suppress,
        version,
        args.exclude,
        true,
    )?;

    eprintln!("Uploading preview release to Diode...");
    let result = pcb_diode_api::upload_preview(&zip_path)?;

    eprintln!(
        "{} Preview uploaded: {}",
        "✓".green(),
        result.preview_url.cyan()
    );

    Ok(())
}

fn preview_version(workspace_root: &Path) -> Option<String> {
    let short = git::rev_parse_short_head(workspace_root)?;

    match git::has_uncommitted_changes(workspace_root).ok() {
        Some(true) => Some(format!("{short}-dirty")),
        _ => Some(short),
    }
}
