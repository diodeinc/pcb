use anyhow::{bail, Context, Result};
use clap::Args;
use colored::Colorize;
use pcb_zen::git;
use pcb_zen::workspace::{get_workspace_info, WorkspaceInfo, WorkspaceInfoExt};
use pcb_zen_core::DefaultFileProvider;
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
    let (workspace, zen_path, board_name) = resolve_preview_target(&args.file)?;

    let version = preview_version(&workspace.root);

    let zip_path = release::build_board_release(
        workspace,
        zen_path,
        board_name.clone(),
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

fn resolve_preview_target(path: &Path) -> Result<(WorkspaceInfo, PathBuf, String)> {
    let file_provider = DefaultFileProvider::new();
    file_walker::require_zen_file(path)?;
    let start_path = path.parent().unwrap_or(Path::new("."));
    let workspace = get_workspace_info(&file_provider, start_path)?;
    ensure_valid_workspace(&workspace)?;

    let board_path = path.canonicalize().context("Board file not found")?;
    let pkg_url = workspace
        .package_url_for_zen(&board_path)
        .ok_or_else(|| anyhow::anyhow!("File not found in workspace: {}", path.display()))?;
    let pkg = &workspace.packages[&pkg_url];
    if pkg.config.board.is_none() {
        bail!(
            "Not a board package: {}\n\nTo preview a board, the package's pcb.toml must have a [board] section.",
            path.display()
        );
    }

    let board_name = workspace
        .board_name_for_zen(&board_path)
        .unwrap_or_else(|| {
            board_path
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .to_string()
        });

    Ok((workspace, board_path, board_name))
}

fn ensure_valid_workspace(workspace: &WorkspaceInfo) -> Result<()> {
    if !workspace.errors.is_empty() {
        for err in &workspace.errors {
            eprintln!("{}", err.error);
        }
        bail!("Found {} invalid pcb.toml file(s)", workspace.errors.len());
    }
    Ok(())
}

fn preview_version(workspace_root: &Path) -> Option<String> {
    let short = git::rev_parse_short_head(workspace_root)?;

    match git::has_uncommitted_changes(workspace_root).ok() {
        Some(true) => Some(format!("{short}-dirty")),
        _ => Some(short),
    }
}
