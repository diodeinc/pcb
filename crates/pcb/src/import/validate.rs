use super::*;
use anyhow::{Context, Result};
use pcb_zen_core::Diagnostics;

pub(super) fn validate(
    paths: &ImportPaths,
    selection: &ImportSelection,
    args: &ImportArgs,
) -> Result<ImportValidationRun> {
    let kicad_pro_abs = paths.kicad_project_root.join(&selection.selected.kicad_pro);
    let kicad_sch_abs = paths.kicad_project_root.join(&selection.selected.kicad_sch);
    let kicad_pcb_abs = paths.kicad_project_root.join(&selection.selected.kicad_pcb);

    if !kicad_pro_abs.exists() {
        anyhow::bail!(
            "Selected KiCad project file does not exist: {}",
            kicad_pro_abs.display()
        );
    }

    let mut diagnostics = Diagnostics::default();

    // ERC (schematic)
    let erc_report = pcb_kicad::run_erc_report(&kicad_sch_abs, Some(&paths.kicad_project_root))
        .context("KiCad ERC failed")?;
    erc_report.add_to_diagnostics(&mut diagnostics, &kicad_sch_abs.to_string_lossy());
    let (erc_errors, erc_warnings) = count_erc(&erc_report);

    // DRC + schematic parity (layout)
    let drc_report =
        pcb_kicad::run_drc_report(&kicad_pcb_abs, true, Some(&paths.kicad_project_root))
            .context("KiCad DRC failed")?;
    drc_report.add_to_diagnostics(&mut diagnostics, &kicad_pcb_abs.to_string_lossy());
    drc_report
        .add_unconnected_items_to_diagnostics(&mut diagnostics, &kicad_pcb_abs.to_string_lossy());
    drc_report
        .add_schematic_parity_to_diagnostics(&mut diagnostics, &kicad_pcb_abs.to_string_lossy());

    let (drc_errors, drc_warnings) = drc_report.violation_counts();
    let schematic_parity_violations = drc_report.schematic_parity.len();
    let (schematic_parity_tolerated, schematic_parity_blocking) =
        classify_schematic_parity(&drc_report.schematic_parity);
    let schematic_parity_ok = schematic_parity_blocking == 0;

    let summary = ImportValidation {
        selected: selection.selected.clone(),
        schematic_parity_ok,
        schematic_parity_violations,
        schematic_parity_tolerated,
        schematic_parity_blocking,
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
    crate::drc::render_diagnostics(&mut diagnostics_for_render, &[]);

    if !summary.schematic_parity_ok {
        anyhow::bail!(
            "KiCad schematic/layout parity check failed: schematic and PCB appear out of sync"
        );
    }

    let error_count = diagnostics_for_render.error_count();
    if error_count > 0 {
        if args.force {
            eprintln!(
                "Warning: KiCad ERC/DRC reported {error_count} errors; continuing due to --force."
            );
        } else if !crate::tty::is_interactive() || std::env::var("CI").is_ok() {
            anyhow::bail!(
                "KiCad ERC/DRC reported {error_count} errors. Fix them, or re-run in an interactive terminal to confirm continuing."
            );
        } else {
            let continue_anyway = inquire::Confirm::new(&format!(
                "KiCad ERC/DRC reported {error_count} errors. Continue anyway?"
            ))
            .with_default(false)
            .prompt()
            .context("Failed to read confirmation")?;

            if !continue_anyway {
                anyhow::bail!("Aborted due to KiCad ERC/DRC errors");
            }
        }
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

fn classify_schematic_parity(parity: &[pcb_kicad::drc::DrcViolation]) -> (usize, usize) {
    // Import uses KiCad's parity check as a guardrail against having a split
    // schematic/layout source of truth. Some parity issues are tolerable for
    // import, notably extra footprints that are not represented in the schematic.
    //
    // We can exclude these from the Zener world later by only generating
    // components that exist in the schematic/netlist.
    let tolerated = parity
        .iter()
        .filter(|v| v.violation_type == "extra_footprint")
        .count();
    let blocking = parity.len().saturating_sub(tolerated);
    (tolerated, blocking)
}
