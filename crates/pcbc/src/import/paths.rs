use super::*;
use anyhow::{Context, Result};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

pub(super) fn resolve_paths(args: &ImportArgs) -> Result<ImportPaths> {
    let kicad_input_abs = require_kicad_input_file(&find_kicad_project(&args.kicad_input)?)?;
    let kicad_project_root = kicad_input_abs
        .parent()
        .context("A KiCad input path must have a parent directory")?
        .to_path_buf();

    let (workspace_root, manifest_board_zen) = ensure_board_repo_root(&args.output_dir)?;
    Ok(ImportPaths {
        workspace_root,
        kicad_project_root,
        kicad_input_abs,
        manifest_board_zen,
    })
}

/// The input itself, or the one `.kicad_pro` in a directory, such as an extracted project
/// archive. Hidden directories (KiCad's `.history`, macOS `__MACOSX` metadata) and KiCad's
/// `*-backups` directories hold older copies of the project and are skipped.
fn find_kicad_project(input: &Path) -> Result<PathBuf> {
    if !input.is_dir() {
        return Ok(input.to_path_buf());
    }
    fn walk(dir: &Path, found: &mut Vec<PathBuf>) -> Result<()> {
        for entry in
            fs::read_dir(dir).with_context(|| format!("Failed to read {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            // Symlinks are not followed, so a link cannot loop or leave the directory.
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                if !(name.starts_with('.') || name == "__MACOSX" || name.ends_with("-backups")) {
                    walk(&path, found)?;
                }
            } else if file_type.is_file() && path.extension() == Some(OsStr::new("kicad_pro")) {
                found.push(path);
            }
        }
        Ok(())
    }
    let mut found = Vec::new();
    walk(input, &mut found)?;
    found.sort();
    match found.as_slice() {
        [project] => Ok(project.clone()),
        [] => anyhow::bail!(
            "No KiCad project (.kicad_pro) found in {}; pass a .kicad_sch to import a schematic",
            input.display()
        ),
        projects => anyhow::bail!(
            "Found {} KiCad projects in {}; pass the one to import:\n{}",
            projects.len(),
            input.display(),
            projects
                .iter()
                .map(|path| format!("  {}", path.display()))
                .collect::<Vec<_>>()
                .join("\n")
        ),
    }
}

fn require_kicad_input_file(path: &Path) -> Result<PathBuf> {
    let meta = fs::metadata(path)
        .with_context(|| format!("Failed to stat KiCad input file: {}", path.display()))?;
    let extension = path.extension();
    if matches!(extension, Some(ext) if ext == "pro" || ext == "sch") {
        anyhow::bail!(
            "{} is a KiCad 5 legacy file; open and save it in KiCad 9 or newer to convert it",
            path.display()
        );
    }
    if !meta.is_file()
        || !matches!(
            extension,
            Some(ext) if ext == OsStr::new("kicad_sch") || ext == OsStr::new("kicad_pro")
        )
    {
        anyhow::bail!(
            "Expected a .kicad_sch or .kicad_pro file, got: {}",
            path.display()
        );
    }
    Ok(fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()))
}

/// The board repository root, and the `[board] path` of a repository that already exists.
fn ensure_board_repo_root(path: &Path) -> Result<(PathBuf, Option<PathBuf>)> {
    if path.exists() && !path.is_dir() {
        anyhow::bail!("Output directory is not a directory: {}", path.display());
    }
    if !path.exists() {
        fs::create_dir_all(path)
            .with_context(|| format!("Failed to create output directory: {}", path.display()))?;
    }
    let workspace_root = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());

    let pcb_toml = workspace_root.join("pcb.toml");
    if pcb_toml.exists() {
        let config = read_pcb_toml(&pcb_toml)?;
        if !config.is_workspace() {
            anyhow::bail!(
                "Output directory contains a pcb.toml but it is not a workspace: {}",
                pcb_toml.display()
            );
        }
        if !config.is_board() {
            anyhow::bail!(
                "Output directory contains a pcb.toml but it is not a board repository: {}",
                pcb_toml.display()
            );
        }
        // Generated sources refer to modules, components, and the KiCad project from the root.
        let board_zen = config
            .board
            .and_then(|board| board.path)
            .map(|board_path| {
                let relative = Path::new(&board_path);
                anyhow::ensure!(
                    relative.components().count() == 1
                        && relative.extension() == Some(OsStr::new("zen")),
                    "{} sets [board] path = {board_path:?}; import writes the board as a .zen file at the repository root",
                    pcb_toml.display()
                );
                Ok(workspace_root.join(relative))
            })
            .transpose()?;
        return Ok((workspace_root, board_zen));
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
            "Output directory is not an empty board repository (missing pcb.toml): {}",
            workspace_root.display()
        );
    }

    Ok((workspace_root, None))
}

fn read_pcb_toml(pcb_toml: &Path) -> Result<pcb_zen_core::config::PcbToml> {
    let file_provider = pcb_zen_core::DefaultFileProvider::new();
    pcb_zen_core::config::PcbToml::from_file(&file_provider, pcb_toml)
        .with_context(|| format!("Failed to parse {}", pcb_toml.display()))
}
