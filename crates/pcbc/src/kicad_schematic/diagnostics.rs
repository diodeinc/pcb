//! Linked-schematic analysis surfaced as suppressible build diagnostics.

use std::path::Path;

use pcb_kicad_sch::analysis::inspect_schematic;
use pcb_sch::Schematic;
use pcb_zen_core::Diagnostic;
use pcb_zen_core::lang::error::CategorizedDiagnostic;
use starlark::errors::EvalSeverity;

use super::{KicadProject, schematic_project_path};

/// Analyze the design's linked KiCad project, if any, and report every
/// discrepancy as a `sch.*` warning attributed to the source file.
pub fn linked_schematic_diagnostics(schematic: &Schematic, source_path: &Path) -> Vec<Diagnostic> {
    let failed = |error: anyhow::Error| {
        vec![warning(
            source_path,
            "sch.project",
            format!("Failed to analyze linked KiCad schematic project: {error:#}"),
        )]
    };
    let project_path = match schematic_project_path(schematic) {
        Ok(Some(path)) => path,
        Ok(None) => return Vec::new(),
        Err(error) => return failed(error),
    };
    let project = match KicadProject::load(&project_path) {
        Ok(project) => project,
        Err(error) => return failed(error),
    };
    let analysis = match inspect_schematic(&project.document, schematic) {
        Ok(inspection) => inspection.analysis,
        Err(error) => return failed(error),
    };
    analysis
        .issues()
        .iter()
        .map(|issue| {
            warning(
                source_path,
                &format!("sch.{}", issue.kind()),
                format!("Linked KiCad schematic: {}", issue.summary()),
            )
        })
        .collect()
}

fn warning(source_path: &Path, kind: &str, message: String) -> Diagnostic {
    Diagnostic::categorized(
        &source_path.to_string_lossy(),
        &message,
        kind,
        EvalSeverity::Warning,
    )
}

/// Whether any unsuppressed `sch.*` diagnostic is present.
pub fn has_unsuppressed_schematic_diagnostics(diagnostics: &pcb_zen_core::Diagnostics) -> bool {
    diagnostics.diagnostics.iter().any(|diagnostic| {
        !diagnostic.suppressed
            && diagnostic
                .innermost()
                .downcast_error_ref::<CategorizedDiagnostic>()
                .is_some_and(|categorized| {
                    categorized.kind == "sch" || categorized.kind.starts_with("sch.")
                })
    })
}
