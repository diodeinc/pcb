use super::*;
use anyhow::{Context, Result};
use pcb_zen_core::Diagnostics;
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

pub(super) fn materialize_board(
    paths: &ImportPaths,
    selection: &ImportSelection,
    validation: &ImportValidationRun,
    staged_root: &Path,
) -> Result<MaterializedBoard> {
    let board_dir = paths.workspace_root.clone();
    let board_zen = board_dir.join(format!("{}.zen", selection.board_name));
    let import_extraction_json = board_dir.join(report::IMPORT_EXTRACTION_REPORT_NAME);
    let selected = &selection.selected;
    let portable_kicad_project_zip = selected
        .kicad_pcb
        .is_some()
        .then(|| board_dir.join(format!("{}.kicad.archive.zip", selection.board_name)));

    let validation_diagnostics_json = write_validation_diagnostics(
        &board_dir,
        &paths.kicad_project_root,
        &validation.summary,
        &validation.diagnostics,
    )?;

    let layout_dir = paths.project_dir();
    let layout_kicad_pro = layout_dir.join(selected.layout_kicad_pro());
    copy_project_sources(staged_root, selected, &layout_kicad_pro)?;

    // The live schematic is the original hierarchy, not a reconstruction from symbol positions.
    let schematic_files = selection.portable.schematic_files_rel();
    for relative in &schematic_files {
        let destination = layout_dir.join(relative);
        fs::create_dir_all(destination.parent().context("Schematic has no parent")?)?;
        fs::copy(staged_root.join(relative), &destination)
            .with_context(|| format!("Failed to copy schematic {}", relative.display()))?;
    }
    make_sheet_file_ids_unique(&layout_dir, &selected.kicad_sch, &schematic_files)?;
    // Without a source board, forced imports retain eda/. Its matching board still needs
    // stackup extraction and identity prepatching, but retains its existing net names.
    let layout_kicad_pcb =
        Some(layout_kicad_pro.with_extension("kicad_pcb")).filter(|path| path.is_file());

    if let Some(output_zip) = &portable_kicad_project_zip {
        portable::write_portable_zip(&selection.portable, staged_root, output_zip)
            .context("Failed to write portable KiCad project archive")?;
    }

    Ok(MaterializedBoard {
        board_dir,
        board_zen,
        layout_dir,
        layout_kicad_pro,
        layout_kicad_pcb,
        portable_kicad_project_zip,
        validation_diagnostics_json,
        import_extraction_json,
    })
}

fn make_sheet_file_ids_unique(
    layout_dir: &Path,
    root_schematic: &Path,
    schematic_files: &[PathBuf],
) -> Result<()> {
    let root = pcb_kicad_sch::normalize_schematic_path(root_schematic);
    let mut files = schematic_files
        .iter()
        .map(|relative| pcb_kicad_sch::normalize_schematic_path(relative))
        .collect::<Vec<_>>();
    files.sort_by_key(|relative| *relative != root);

    let mut seen = std::collections::BTreeSet::new();
    for relative in files {
        let path = layout_dir.join(&relative);
        let source = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let tree = pcb_sexpr::parse(&source)
            .with_context(|| format!("Failed to parse {}", path.display()))?;
        let Some(id) = tree
            .as_list()
            .and_then(|items| pcb_sexpr::find_child_list(items, "uuid"))
            .and_then(|items| items.get(1))
        else {
            continue;
        };
        let Some(value) = id.as_str() else {
            continue;
        };
        let mut replacement = value.to_string();
        while !seen.insert(replacement.clone()) {
            let key = format!("{replacement}/{}", relative.display());
            replacement =
                uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, key.as_bytes()).to_string();
        }
        if replacement == value {
            continue;
        }
        let mut patches = pcb_sexpr::PatchSet::new();
        patches.replace_string(id.span, &replacement);
        let mut output = Vec::new();
        patches.write_to(&source, &mut output)?;
        fs::write(&path, output).with_context(|| format!("Failed to write {}", path.display()))?;
    }
    Ok(())
}

fn write_validation_diagnostics(
    board_dir: &Path,
    kicad_project_root: &Path,
    validation: &ImportValidation,
    diagnostics: &Diagnostics,
) -> Result<PathBuf> {
    #[derive(Serialize)]
    struct ImportValidationDiagnosticsFile<'a> {
        kicad_project_root: &'a Path,
        selected: &'a SelectedKicadFiles,
        diagnostics: &'a Diagnostics,
    }

    let out_path = board_dir.join(".kicad.validation.diagnostics.json");
    let payload = ImportValidationDiagnosticsFile {
        kicad_project_root,
        selected: &validation.selected,
        diagnostics,
    };

    fs::write(&out_path, serde_json::to_string_pretty(&payload)?)
        .with_context(|| format!("Failed to write {}", out_path.display()))?;
    Ok(out_path)
}

/// The source project and board become the generated project. Without a source project, a
/// retained one is kept and an empty one is created otherwise.
fn copy_project_sources(
    source_root: &Path,
    selected: &SelectedKicadFiles,
    layout_kicad_pro: &Path,
) -> Result<()> {
    let layout_dir = layout_kicad_pro
        .parent()
        .context("Project file has no parent")?;
    fs::create_dir_all(layout_dir)
        .with_context(|| format!("Failed to create layout directory {}", layout_dir.display()))?;

    if let Some(kicad_pcb) = &selected.kicad_pcb {
        let dst_pcb = layout_kicad_pro.with_extension("kicad_pcb");
        anyhow::ensure!(
            !dst_pcb.exists(),
            "Layout directory already contains a KiCad board (refusing to overwrite): {}",
            dst_pcb.display()
        );
        copy_file(&source_root.join(kicad_pcb), &dst_pcb)?;
    }

    match &selected.kicad_pro {
        Some(kicad_pro) => {
            let src_pro = source_root.join(kicad_pro);
            copy_file(&src_pro, layout_kicad_pro)?;
            copy_optional_kicad_dru(&src_pro, layout_kicad_pro)
        }
        None if !layout_kicad_pro.exists() => Ok(fs::write(layout_kicad_pro, "{}\n")?),
        None => Ok(()),
    }
}

fn copy_file(src: &Path, dst: &Path) -> Result<()> {
    fs::copy(src, dst)
        .with_context(|| format!("Failed to copy {} -> {}", src.display(), dst.display()))?;
    Ok(())
}

fn copy_optional_kicad_dru(src_pro: &Path, dst_pro: &Path) -> Result<()> {
    let src_dru = src_pro.with_extension("kicad_dru");
    if !src_dru.is_file() {
        return Ok(());
    }
    copy_file(&src_dru, &dst_pro.with_extension("kicad_dru"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selected_files() -> SelectedKicadFiles {
        SelectedKicadFiles {
            kicad_pro: Some(PathBuf::from("board.kicad_pro")),
            kicad_sch: PathBuf::from("board.kicad_sch"),
            kicad_pcb: Some(PathBuf::from("board.kicad_pcb")),
        }
    }

    fn setup_sources(with_dru: bool) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let src_root = dir.path().join("src");
        let board_dir = dir.path().join("out");
        fs::create_dir_all(&src_root).expect("mkdir src");

        fs::write(src_root.join("board.kicad_pro"), "(kicad_pro)").expect("write pro");
        fs::write(src_root.join("board.kicad_pcb"), "(kicad_pcb)").expect("write pcb");
        if with_dru {
            fs::write(src_root.join("board.kicad_dru"), "(kicad_dru)").expect("write dru");
        }

        (dir, src_root, board_dir)
    }

    #[test]
    fn copy_project_sources_copies_kicad_dru_when_present() {
        let (_dir, src_root, board_dir) = setup_sources(true);
        let dst_pro = board_dir.join("eda/board.kicad_pro");
        copy_project_sources(&src_root, &selected_files(), &dst_pro).expect("copy layout");

        let dst_dru = dst_pro.with_extension("kicad_dru");
        assert!(dst_dru.is_file());
        assert_eq!(
            fs::read_to_string(&dst_dru).expect("read dst dru"),
            "(kicad_dru)"
        );
    }

    #[test]
    fn copy_project_sources_skips_kicad_dru_when_missing() {
        let (_dir, src_root, board_dir) = setup_sources(false);
        let dst_pro = board_dir.join("eda/board.kicad_pro");
        copy_project_sources(&src_root, &selected_files(), &dst_pro).expect("copy layout");

        assert!(!dst_pro.with_extension("kicad_dru").exists());
    }
}
