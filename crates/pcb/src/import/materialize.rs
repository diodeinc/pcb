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
) -> Result<MaterializedBoard> {
    let board_dir = paths
        .workspace_root
        .join("boards")
        .join(&selection.board_name);
    let board_zen = board_dir.join(format!("{}.zen", selection.board_name));
    let import_extraction_json = board_dir.join(".kicad.import.extraction.json");
    let portable_kicad_project_zip =
        board_dir.join(format!("{}.kicad.archive.zip", selection.board_name));

    let validation_diagnostics_json = write_validation_diagnostics(
        &board_dir,
        &paths.kicad_project_root,
        &validation.summary,
        &validation.diagnostics,
    )?;

    let (layout_dir, layout_kicad_pro, layout_kicad_pcb) = copy_layout_sources(
        &paths.kicad_project_root,
        &validation.summary.selected,
        &board_dir,
    )?;

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

fn copy_layout_sources(
    kicad_project_root: &Path,
    selected: &SelectedKicadFiles,
    board_dir: &Path,
) -> Result<(PathBuf, PathBuf, PathBuf)> {
    // This matches the default `pcb new --board` template: `layout_path = "layout"`.
    let layout_dir = board_dir.join("layout");
    fs::create_dir_all(&layout_dir)
        .with_context(|| format!("Failed to create layout directory {}", layout_dir.display()))?;

    let src_pro = kicad_project_root.join(&selected.kicad_pro);
    let src_pcb = kicad_project_root.join(&selected.kicad_pcb);

    let dst_pro = layout_dir.join(&selected.kicad_pro);
    let dst_pcb = layout_dir.join(&selected.kicad_pcb);

    if dst_pro.exists() || dst_pcb.exists() {
        anyhow::bail!(
            "Layout directory already contains KiCad files (refusing to overwrite): {}",
            layout_dir.display()
        );
    }

    for path in [&dst_pro, &dst_pcb] {
        let Some(parent) = path.parent() else {
            continue;
        };
        if parent.as_os_str().is_empty() {
            continue;
        }
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create output directory {}", parent.display()))?;
    }

    fs::copy(&src_pro, &dst_pro).with_context(|| {
        format!(
            "Failed to copy {} -> {}",
            src_pro.display(),
            dst_pro.display()
        )
    })?;
    fs::copy(&src_pcb, &dst_pcb).with_context(|| {
        format!(
            "Failed to copy {} -> {}",
            src_pcb.display(),
            dst_pcb.display()
        )
    })?;
    copy_optional_kicad_dru(&src_pro, &dst_pro)?;

    Ok((layout_dir, dst_pro, dst_pcb))
}

fn copy_optional_kicad_dru(src_pro: &Path, dst_pro: &Path) -> Result<()> {
    let src_dru = src_pro.with_extension("kicad_dru");
    if !src_dru.is_file() {
        return Ok(());
    }

    let dst_dru = dst_pro.with_extension("kicad_dru");
    fs::copy(&src_dru, &dst_dru).with_context(|| {
        format!(
            "Failed to copy {} -> {}",
            src_dru.display(),
            dst_dru.display()
        )
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selected_files() -> SelectedKicadFiles {
        SelectedKicadFiles {
            kicad_pro: PathBuf::from("board.kicad_pro"),
            kicad_sch: PathBuf::from("board.kicad_sch"),
            kicad_pcb: PathBuf::from("board.kicad_pcb"),
        }
    }

    fn setup_sources(with_dru: bool) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let src_root = dir.path().join("src");
        let board_dir = dir.path().join("out/boards/test");
        fs::create_dir_all(&src_root).expect("mkdir src");

        fs::write(src_root.join("board.kicad_pro"), "(kicad_pro)").expect("write pro");
        fs::write(src_root.join("board.kicad_pcb"), "(kicad_pcb)").expect("write pcb");
        if with_dru {
            fs::write(src_root.join("board.kicad_dru"), "(kicad_dru)").expect("write dru");
        }

        (dir, src_root, board_dir)
    }

    #[test]
    fn copy_layout_sources_copies_kicad_dru_when_present() {
        let (_dir, src_root, board_dir) = setup_sources(true);

        let (_layout_dir, dst_pro, _dst_pcb) =
            copy_layout_sources(&src_root, &selected_files(), &board_dir).expect("copy layout");

        let dst_dru = dst_pro.with_extension("kicad_dru");
        assert!(dst_dru.is_file());
        assert_eq!(
            fs::read_to_string(&dst_dru).expect("read dst dru"),
            "(kicad_dru)"
        );
    }

    #[test]
    fn copy_layout_sources_skips_kicad_dru_when_missing() {
        let (_dir, src_root, board_dir) = setup_sources(false);

        let (_layout_dir, dst_pro, _dst_pcb) =
            copy_layout_sources(&src_root, &selected_files(), &board_dir).expect("copy layout");

        assert!(!dst_pro.with_extension("kicad_dru").exists());
    }
}
