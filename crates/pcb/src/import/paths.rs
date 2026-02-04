use super::*;
use anyhow::{Context, Result};
use pcb_zen_core::config::{find_workspace_root, PcbToml};
use pcb_zen_core::DefaultFileProvider;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

pub(super) fn resolve_paths(args: &ImportArgs) -> Result<ImportPaths> {
    let workspace_start = match &args.workspace {
        Some(p) => p.clone(),
        None => env::current_dir()?,
    };
    if !workspace_start.exists() {
        anyhow::bail!(
            "Workspace path does not exist: {}",
            workspace_start.display()
        );
    }
    let workspace_start = fs::canonicalize(&workspace_start).unwrap_or(workspace_start);
    let workspace_root = require_existing_workspace(&workspace_start)?;

    let (kicad_project_root, passed_kicad_pro) = normalize_kicad_project_path(&args.kicad_project)?;
    Ok(ImportPaths {
        workspace_root,
        kicad_project_root,
        passed_kicad_pro,
    })
}

fn normalize_kicad_project_path(path: &Path) -> Result<(PathBuf, Option<PathBuf>)> {
    let meta = fs::metadata(path)
        .with_context(|| format!("Failed to stat KiCad project path: {}", path.display()))?;

    if meta.is_dir() {
        return Ok((path.to_path_buf(), None));
    }

    if meta.is_file() && path.extension() == Some(OsStr::new("kicad_pro")) {
        let parent = path
            .parent()
            .context("A .kicad_pro path must have a parent directory")?;
        return Ok((parent.to_path_buf(), Some(path.to_path_buf())));
    }

    anyhow::bail!(
        "Expected --kicad-project to be a directory or a .kicad_pro file, got: {}",
        path.display()
    );
}

fn require_existing_workspace(start_path: &Path) -> Result<PathBuf> {
    let file_provider = DefaultFileProvider::new();
    let workspace_root =
        find_workspace_root(&file_provider, start_path).context("Not inside a pcb workspace")?;
    let pcb_toml = workspace_root.join("pcb.toml");
    if !pcb_toml.exists() {
        anyhow::bail!(
            "Not inside a pcb workspace (missing pcb.toml at {})",
            pcb_toml.display()
        );
    }
    let config = PcbToml::from_file(&file_provider, &pcb_toml)
        .with_context(|| format!("Failed to parse {}", pcb_toml.display()))?;
    if !config.is_workspace() {
        anyhow::bail!(
            "Not inside a pcb workspace (pcb.toml is not a workspace): {}",
            pcb_toml.display()
        );
    }
    Ok(workspace_root)
}
