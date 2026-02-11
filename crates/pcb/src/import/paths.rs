use super::*;
use anyhow::{Context, Result};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

pub(super) fn resolve_paths(args: &ImportArgs) -> Result<ImportPaths> {
    let kicad_pro_abs = require_kicad_pro_file(&args.kicad_pro)?;
    let kicad_project_root = kicad_pro_abs
        .parent()
        .context("A .kicad_pro path must have a parent directory")?
        .to_path_buf();

    let workspace_root = ensure_workspace_root(&args.output_dir)?;
    Ok(ImportPaths {
        workspace_root,
        kicad_project_root,
        kicad_pro_abs,
    })
}

fn require_kicad_pro_file(path: &Path) -> Result<PathBuf> {
    let meta = fs::metadata(path)
        .with_context(|| format!("Failed to stat KiCad project file: {}", path.display()))?;
    if !meta.is_file() || path.extension() != Some(OsStr::new("kicad_pro")) {
        anyhow::bail!("Expected a .kicad_pro file, got: {}", path.display());
    }
    Ok(fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()))
}

fn ensure_workspace_root(path: &Path) -> Result<PathBuf> {
    if path.exists() && !path.is_dir() {
        anyhow::bail!("Output directory is not a directory: {}", path.display());
    }
    let created = !path.exists();
    if created {
        fs::create_dir_all(path)
            .with_context(|| format!("Failed to create output directory: {}", path.display()))?;
    }
    let workspace_root = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());

    let pcb_toml = workspace_root.join("pcb.toml");
    if pcb_toml.exists() {
        let file_provider = pcb_zen_core::DefaultFileProvider::new();
        let config = pcb_zen_core::config::PcbToml::from_file(&file_provider, &pcb_toml)
            .with_context(|| format!("Failed to parse {}", pcb_toml.display()))?;
        if !config.is_workspace() {
            anyhow::bail!(
                "Output directory contains a pcb.toml but it is not a workspace: {}",
                pcb_toml.display()
            );
        }
        return Ok(workspace_root);
    }

    let mut entries = fs::read_dir(&workspace_root)
        .with_context(|| {
            format!(
                "Failed to read output directory: {}",
                workspace_root.display()
            )
        })?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name() != OsStr::new(".DS_Store"));
    if entries.next().is_some() {
        anyhow::bail!(
            "Output directory is not an empty workspace (missing pcb.toml): {}",
            workspace_root.display()
        );
    }

    if let Err(e) = crate::new::init_workspace(&workspace_root, "") {
        if created {
            let _ = fs::remove_dir_all(&workspace_root);
        }
        return Err(e);
    }
    Ok(workspace_root)
}
