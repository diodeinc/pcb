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
        &selection.board_name,
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
    board_name: &str,
) -> Result<(PathBuf, PathBuf, PathBuf)> {
    // This matches the default `pcb new --board` template: `layout_path = "layout/<board_name>"`.
    let layout_dir = board_dir.join("layout").join(board_name);
    fs::create_dir_all(&layout_dir)
        .with_context(|| format!("Failed to create layout directory {}", layout_dir.display()))?;

    let src_pro = kicad_project_root.join(&selected.kicad_pro);
    let src_pcb = kicad_project_root.join(&selected.kicad_pcb);

    let dst_pro = layout_dir.join("layout.kicad_pro");
    let dst_pcb = layout_dir.join("layout.kicad_pcb");

    if dst_pro.exists() || dst_pcb.exists() {
        anyhow::bail!(
            "Layout directory already contains KiCad files (refusing to overwrite): {}",
            layout_dir.display()
        );
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

    Ok((layout_dir, dst_pro, dst_pcb))
}
