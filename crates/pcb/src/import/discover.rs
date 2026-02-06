use super::*;
use anyhow::{Context, Result};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

pub(super) fn discover_and_select(
    paths: &ImportPaths,
    args: &ImportArgs,
) -> Result<ImportSelection> {
    let mut files = discover_kicad_files(&paths.kicad_project_root)?;

    // If the user provided a .kicad_pro file, ensure it is included in discovery even if the
    // project root contains other nested projects.
    if let Some(kicad_pro_path) = paths.passed_kicad_pro.as_ref() {
        let canonical_root = fs::canonicalize(&paths.kicad_project_root)
            .unwrap_or_else(|_| paths.kicad_project_root.clone());
        let canonical_pro =
            fs::canonicalize(kicad_pro_path).unwrap_or_else(|_| kicad_pro_path.clone());
        if let Ok(rel) = canonical_pro.strip_prefix(&canonical_root) {
            let rel = rel.to_path_buf();
            if !files.kicad_pro.contains(&rel) {
                files.kicad_pro.push(rel);
            }
        }
    }

    sort_discovered_files(&mut files);

    // For directory projects, require a single top-level .kicad_pro to avoid ambiguous source-of-truth.
    if args.kicad_project.is_dir() && files.kicad_pro.len() != 1 {
        anyhow::bail!(
            "Expected exactly one .kicad_pro under {}, found {}.\nFound:\n  - {}",
            paths.kicad_project_root.display(),
            files.kicad_pro.len(),
            files
                .kicad_pro
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join("\n  - ")
        );
    }

    let (board_name, board_name_source) = infer_board_name(&args.kicad_project, &files);
    let Some(board_name) = board_name else {
        anyhow::bail!("Failed to determine KiCad project name (expected a .kicad_pro file)");
    };
    let Some(board_name_source) = board_name_source else {
        anyhow::bail!("Failed to determine KiCad project name source");
    };

    let selected = select_kicad_files(
        &paths.kicad_project_root,
        &args.kicad_project,
        &files,
        Some(&board_name),
    )?;

    Ok(ImportSelection {
        board_name,
        board_name_source,
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

fn infer_board_name(
    kicad_project_arg: &Path,
    files: &KicadDiscoveredFiles,
) -> (Option<String>, Option<BoardNameSource>) {
    // If the user gave a specific project file, use it directly.
    if kicad_project_arg.is_file() && kicad_project_arg.extension() == Some(OsStr::new("kicad_pro"))
    {
        if let Some(stem) = kicad_project_arg.file_stem().and_then(|s| s.to_str()) {
            return (
                Some(stem.to_string()),
                Some(BoardNameSource::KicadProArgument),
            );
        }
    }

    // For directory projects, we require exactly one .kicad_pro, so this is definitive.
    if files.kicad_pro.len() == 1 {
        if let Some(stem) = files.kicad_pro[0].file_stem().and_then(|s| s.to_str()) {
            return (
                Some(stem.to_string()),
                Some(BoardNameSource::SingleKicadProFound),
            );
        }
    }

    (None, None)
}

fn select_kicad_files(
    kicad_project_root: &Path,
    kicad_project_arg: &Path,
    files: &KicadDiscoveredFiles,
    preferred_stem: Option<&str>,
) -> Result<SelectedKicadFiles> {
    if files.kicad_pro.is_empty() {
        anyhow::bail!(
            "No .kicad_pro files found under {}",
            kicad_project_root.display()
        );
    }

    let kicad_pro = select_kicad_pro(kicad_project_root, kicad_project_arg, &files.kicad_pro)?;
    let kicad_sch = select_by_stem("kicad_sch", &files.kicad_sch, preferred_stem)?;
    let kicad_pcb = select_by_stem("kicad_pcb", &files.kicad_pcb, preferred_stem)?;

    Ok(SelectedKicadFiles {
        kicad_pro,
        kicad_sch,
        kicad_pcb,
    })
}

fn select_kicad_pro(
    kicad_project_root: &Path,
    kicad_project_arg: &Path,
    discovered: &[PathBuf],
) -> Result<PathBuf> {
    if kicad_project_arg.is_file() && kicad_project_arg.extension() == Some(OsStr::new("kicad_pro"))
    {
        let canonical_root = fs::canonicalize(kicad_project_root)
            .unwrap_or_else(|_| kicad_project_root.to_path_buf());
        let canonical_pro =
            fs::canonicalize(kicad_project_arg).unwrap_or_else(|_| kicad_project_arg.to_path_buf());
        let rel = canonical_pro
            .strip_prefix(&canonical_root)
            .with_context(|| {
                format!(
                    "Provided .kicad_pro is not under the project root {}: {}",
                    canonical_root.display(),
                    canonical_pro.display()
                )
            })?;
        return Ok(rel.to_path_buf());
    }

    if discovered.len() == 1 {
        return Ok(discovered[0].clone());
    }

    anyhow::bail!(
        "Multiple .kicad_pro files found; pass a specific .kicad_pro via --kicad-project.\nFound:\n  - {}",
        discovered
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("\n  - ")
    );
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

        let files = discover_kicad_files(&root)?;
        assert!(!files.kicad_pro.is_empty());
        assert!(!files.kicad_pcb.is_empty());
        assert!(!files.kicad_sch.is_empty());

        let (name, src) = infer_board_name(&root, &files);
        assert_eq!(name.as_deref(), Some("layout"));
        assert!(matches!(src, Some(BoardNameSource::SingleKicadProFound)));

        Ok(())
    }
}
