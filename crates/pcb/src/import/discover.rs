use super::*;
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

pub(super) fn discover_and_select(
    paths: &ImportPaths,
    _args: &ImportArgs,
) -> Result<ImportSelection> {
    let mut files = discover_kicad_files(&paths.kicad_project_root)?;

    // Ensure the explicitly provided .kicad_pro is included in discovery even if the
    // project root contains other nested projects.
    let canonical_root = fs::canonicalize(&paths.kicad_project_root)
        .unwrap_or_else(|_| paths.kicad_project_root.clone());
    let canonical_pro =
        fs::canonicalize(&paths.kicad_pro_abs).unwrap_or_else(|_| paths.kicad_pro_abs.clone());
    let rel_pro = canonical_pro
        .strip_prefix(&canonical_root)
        .with_context(|| {
            format!(
                "Provided .kicad_pro is not under the project root {}: {}",
                canonical_root.display(),
                canonical_pro.display()
            )
        })?
        .to_path_buf();
    if !files.kicad_pro.contains(&rel_pro) {
        files.kicad_pro.push(rel_pro.clone());
    }

    sort_discovered_files(&mut files);

    let board_name = rel_pro
        .file_stem()
        .and_then(|s| s.to_str())
        .context("Failed to determine KiCad project name (invalid .kicad_pro filename)")?
        .to_string();

    let selected = SelectedKicadFiles {
        kicad_pro: rel_pro.clone(),
        kicad_sch: select_by_stem("kicad_sch", &files.kicad_sch, Some(&board_name))?,
        kicad_pcb: select_by_stem("kicad_pcb", &files.kicad_pcb, Some(&board_name))?,
    };

    Ok(ImportSelection {
        board_name,
        board_name_source: BoardNameSource::KicadProArgument,
        files,
        selected,
    })
}

fn discover_kicad_files(root: &Path) -> Result<KicadDiscoveredFiles> {
    let canonical_root = fs::canonicalize(root).with_context(|| {
        format!(
            "Failed to canonicalize KiCad project root: {}",
            root.display()
        )
    })?;

    let mut out = KicadDiscoveredFiles::default();

    for entry in WalkDir::new(&canonical_root)
        .follow_links(false)
        .into_iter()
    {
        let entry = entry.with_context(|| {
            format!(
                "Failed while traversing KiCad project directory: {}",
                canonical_root.display()
            )
        })?;

        if !entry.file_type().is_file() {
            continue;
        }

        let abs_path = entry.path();
        let rel = abs_path
            .strip_prefix(&canonical_root)
            .unwrap_or(abs_path)
            .to_path_buf();

        let Some(kind) = classify_kicad_file(&rel) else {
            continue;
        };

        match kind {
            KicadFileKind::KicadPro => out.kicad_pro.push(rel),
            KicadFileKind::KicadSch => out.kicad_sch.push(rel),
            KicadFileKind::KicadPcb => out.kicad_pcb.push(rel),
            KicadFileKind::KicadSym => out.kicad_sym.push(rel),
            KicadFileKind::KicadMod => out.kicad_mod.push(rel),
            KicadFileKind::KicadPrl => out.kicad_prl.push(rel),
            KicadFileKind::KicadDru => out.kicad_dru.push(rel),
            KicadFileKind::FpLibTable => out.fp_lib_table.push(rel),
            KicadFileKind::SymLibTable => out.sym_lib_table.push(rel),
        }
    }

    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KicadFileKind {
    KicadPro,
    KicadSch,
    KicadPcb,
    KicadSym,
    KicadMod,
    KicadPrl,
    KicadDru,
    FpLibTable,
    SymLibTable,
}

fn classify_kicad_file(rel_path: &Path) -> Option<KicadFileKind> {
    let file_name = rel_path.file_name().and_then(|s| s.to_str()).unwrap_or("");

    if file_name == "fp-lib-table" {
        return Some(KicadFileKind::FpLibTable);
    }
    if file_name == "sym-lib-table" {
        return Some(KicadFileKind::SymLibTable);
    }

    match rel_path.extension().and_then(|s| s.to_str()) {
        Some("kicad_pro") => Some(KicadFileKind::KicadPro),
        Some("kicad_sch") => Some(KicadFileKind::KicadSch),
        Some("kicad_pcb") => Some(KicadFileKind::KicadPcb),
        Some("kicad_sym") => Some(KicadFileKind::KicadSym),
        Some("kicad_mod") => Some(KicadFileKind::KicadMod),
        Some("kicad_prl") => Some(KicadFileKind::KicadPrl),
        Some("kicad_dru") => Some(KicadFileKind::KicadDru),
        _ => None,
    }
}

fn sort_discovered_files(files: &mut KicadDiscoveredFiles) {
    files.kicad_pro.sort();
    files.kicad_sch.sort();
    files.kicad_pcb.sort();
    files.kicad_sym.sort();
    files.kicad_mod.sort();
    files.kicad_prl.sort();
    files.kicad_dru.sort();
    files.fp_lib_table.sort();
    files.sym_lib_table.sort();
}

fn select_by_stem(
    kind: &str,
    discovered: &[PathBuf],
    preferred_stem: Option<&str>,
) -> Result<PathBuf> {
    if discovered.is_empty() {
        anyhow::bail!("No .{kind} files found in KiCad project");
    }

    if let Some(stem) = preferred_stem {
        let matches: Vec<_> = discovered
            .iter()
            .filter(|p| p.file_stem().and_then(|s| s.to_str()) == Some(stem))
            .cloned()
            .collect();
        if matches.len() == 1 {
            return Ok(matches[0].clone());
        }
    }

    if discovered.len() == 1 {
        return Ok(discovered[0].clone());
    }

    anyhow::bail!(
        "Multiple .{kind} files found; unable to select one unambiguously.\nFound:\n  - {}",
        discovered
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("\n  - ")
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn discovers_kicad_files_and_infers_name() -> Result<()> {
        // Use an existing KiCad project fixture already in the repo.
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../pcb-sch/test/kicad-bom");
        let pro = root.join("layout.kicad_pro");

        let files = discover_kicad_files(&root)?;
        assert!(!files.kicad_pro.is_empty());
        assert!(!files.kicad_pcb.is_empty());
        assert!(!files.kicad_sch.is_empty());

        let paths = ImportPaths {
            workspace_root: root.clone(),
            kicad_project_root: root.clone(),
            kicad_pro_abs: pro,
        };
        let args = ImportArgs {
            kicad_pro: paths.kicad_pro_abs.clone(),
            output_dir: root.clone(),
            force: true,
        };
        let selection = discover_and_select(&paths, &args)?;
        assert_eq!(selection.board_name.as_str(), "layout");
        assert!(matches!(
            selection.board_name_source,
            BoardNameSource::KicadProArgument
        ));

        Ok(())
    }
}
