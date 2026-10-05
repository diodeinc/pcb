use super::*;
use anstream::eprintln;
use anyhow::{Context, Result};
use pcb_zen_core::Diagnostics;

pub(super) fn validate(
    paths: &ImportPaths,
    selection: &ImportSelection,
    staged_root: &Path,
) -> Result<ImportValidationRun> {
    // KiCad CLI can update project-local preference files while running ERC/DRC. Validate the
    // retained source snapshot so import never changes its source repository.
    let validation_sch = staged_root.join(&selection.selected.kicad_sch);
    let source_sch = paths.kicad_project_root.join(&selection.selected.kicad_sch);
    let mut diagnostics = Diagnostics::default();

    // ERC is the only source validation available for standalone schematics.
    let erc_report = pcb_kicad::run_erc_report(&validation_sch, Some(staged_root))
        .context("KiCad ERC failed")?;
    erc_report.add_to_diagnostics(&mut diagnostics, &source_sch.to_string_lossy());
    let (erc_errors, erc_warnings) = count_erc(&erc_report);

    let mut schematic_parity_violations = 0;
    let mut drc_errors = 0;
    let mut drc_warnings = 0;

    if selection.portable.source_kind == ImportSourceKind::Project {
        let kicad_pro = selection
            .selected
            .kicad_pro
            .as_ref()
            .context("Project import is missing a selected .kicad_pro file")?;
        let kicad_pcb = selection
            .selected
            .kicad_pcb
            .as_ref()
            .context("Project import is missing a selected .kicad_pcb file")?;
        let validation_pro = staged_root.join(kicad_pro);
        let validation_pcb = staged_root.join(kicad_pcb);
        let source_pcb = paths.kicad_project_root.join(kicad_pcb);
        if !validation_pro.exists() {
            anyhow::bail!(
                "Selected KiCad project file does not exist: {}",
                paths.kicad_project_root.join(kicad_pro).display()
            );
        }

        let drc_output = tempfile::NamedTempFile::new()
            .context("Failed to create temporary file for DRC output")?;
        let drc_report =
            pcb_kicad::run_drc(&validation_pcb, true, Some(staged_root), drc_output.path())
                .context("KiCad DRC failed")?;
        drc_report.add_to_diagnostics(&mut diagnostics, &source_pcb.to_string_lossy());
        drc_report
            .add_unconnected_items_to_diagnostics(&mut diagnostics, &source_pcb.to_string_lossy());
        drc_report
            .add_schematic_parity_to_diagnostics(&mut diagnostics, &source_pcb.to_string_lossy());

        (drc_errors, drc_warnings) = drc_report.violation_counts();
        schematic_parity_violations = drc_report.schematic_parity.len();
    }

    let summary = ImportValidation {
        selected: selection.selected.clone(),
        schematic_parity_ok: schematic_parity_violations == 0,
        schematic_parity_violations,
        erc_errors,
        erc_warnings,
        drc_errors,
        drc_warnings,
    };

    // Persist a copy of the raw diagnostics (before render filters mutate suppression state).
    let diagnostics_for_file = Diagnostics {
        diagnostics: diagnostics.diagnostics.clone(),
    };
    // Render diagnostics for the user (this is intentionally noisy and useful).
    let mut diagnostics_for_render = diagnostics;
    crate::drc::render_diagnostics(&mut diagnostics_for_render, &[], true);

    if schematic_parity_violations > 0 {
        eprintln!(
            "Warning: KiCad reported {schematic_parity_violations} schematic/PCB parity mismatches; these do not block import. Connectivity follows the schematic; existing PCB placement and routing are retained."
        );
    }
    let error_count = diagnostics_for_render.error_count();
    if error_count > 0 {
        eprintln!(
            "Warning: KiCad ERC/DRC reported {error_count} errors; these do not block import."
        );
    }

    Ok(ImportValidationRun {
        summary,
        diagnostics: diagnostics_for_file,
    })
}

fn count_erc(report: &pcb_kicad::erc::ErcReport) -> (usize, usize) {
    let mut errors = 0;
    let mut warnings = 0;
    for sheet in &report.sheets {
        for v in &sheet.violations {
            match v.severity.as_str() {
                "error" => errors += 1,
                "warning" => warnings += 1,
                _ => {}
            }
        }
    }
    (errors, warnings)
}
