use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

pub(super) const IMPORT_EXTRACTION_REPORT_NAME: &str = ".kicad.import.extraction.json";

pub(super) fn build_import_report(
    paths: &super::ImportPaths,
    selection: &super::ImportSelection,
    validation: &super::ImportValidationRun,
    ir: super::ImportIr,
    materialized: &super::MaterializedBoard,
) -> super::ImportReport {
    let generated = super::GeneratedArtifacts {
        board_dir: super::rel_to_root(&paths.workspace_root, &materialized.board_dir),
        board_zen: super::rel_to_root(&paths.workspace_root, &materialized.board_zen),
        validation_diagnostics_json: super::rel_to_root(
            &paths.workspace_root,
            &materialized.validation_diagnostics_json,
        ),
        import_extraction_json: super::rel_to_root(
            &paths.workspace_root,
            &materialized.import_extraction_json,
        ),
        layout_dir: super::rel_to_root(&paths.workspace_root, &materialized.layout_dir),
        layout_kicad_pro: materialized
            .layout_kicad_pro
            .as_ref()
            .map(|path| super::rel_to_root(&paths.workspace_root, path)),
        layout_kicad_pcb: materialized
            .layout_kicad_pcb
            .as_ref()
            .map(|path| super::rel_to_root(&paths.workspace_root, path)),
        portable_kicad_project_zip: materialized
            .portable_kicad_project_zip
            .as_ref()
            .map(|path| super::rel_to_root(&paths.workspace_root, path)),
    };

    super::ImportReport {
        workspace_root: paths.workspace_root.clone(),
        kicad_project_root: paths.kicad_project_root.clone(),
        board_name: Some(selection.board_name.clone()),
        board_name_source: Some(selection.board_name_source),
        files: selection.files.clone(),
        extraction: Some(super::ImportExtractionReport {
            netlist_components: ir.components,
            netlist_nets: ir.nets,
            schematic_lib_symbol_ids: ir.schematic_lib_symbols.keys().cloned().collect(),
            schematic_power_symbol_decls: ir.schematic_power_symbol_decls,
            schematic_sheet_tree: ir.schematic_sheet_tree,
            hierarchy_plan: ir.hierarchy_plan,
            semantic: ir.semantic,
        }),
        validation: Some(validation.summary.clone()),
        generated: Some(generated),
    }
}

pub(super) fn write_import_extraction_report(
    out_path: &Path,
    payload: &super::ImportReport,
) -> Result<()> {
    fs::write(out_path, serde_json::to_string_pretty(payload)?)
        .with_context(|| format!("Failed to write {}", out_path.display()))?;
    Ok(())
}
