use std::path::Path;

use pcb_kicad_sch::analysis::{SchematicIssue, inspect_schematic};
use pcb_sch::Schematic;
use pcb_zen_core::Diagnostic;
use pcb_zen_core::lang::error::CategorizedDiagnostic;
use pcbc::kicad_schematic::{KicadProject, schematic_project_path};
use starlark::errors::EvalSeverity;

pub(crate) fn linked_schematic_diagnostics(
    schematic: &Schematic,
    source_path: &Path,
) -> Vec<Diagnostic> {
    let project_path = match schematic_project_path(schematic) {
        Ok(Some(path)) => path,
        Ok(None) => return Vec::new(),
        Err(error) => return vec![project_diagnostic(source_path, error)],
    };
    let project = match KicadProject::load(&project_path) {
        Ok(project) => project,
        Err(error) => return vec![project_diagnostic(source_path, error)],
    };
    let analysis = match inspect_schematic(&project.document, schematic) {
        Ok(inspection) => inspection.analysis,
        Err(error) => return vec![project_diagnostic(source_path, error)],
    };

    analysis
        .issues()
        .iter()
        .map(|issue| issue_diagnostic(source_path, issue))
        .collect()
}

fn project_diagnostic(source_path: &Path, error: anyhow::Error) -> Diagnostic {
    warning(
        source_path,
        "sch.project",
        format!("Failed to analyze linked KiCad schematic project: {error:#}"),
    )
}

fn issue_diagnostic(source_path: &Path, issue: &SchematicIssue) -> Diagnostic {
    let (kind, message) = match issue {
        SchematicIssue::MissingSymbol { slot } => (
            "sch.missing_symbol",
            format!(
                "KiCad schematic is missing component '{}' unit {}",
                slot.component_path(),
                slot.unit()
            ),
        ),
        SchematicIssue::DuplicateSymbol { slot, locations } => (
            "sch.duplicate_symbol",
            format!(
                "KiCad schematic contains {} placements for component '{}' unit {}",
                locations.len(),
                slot.component_path(),
                slot.unit()
            ),
        ),
        SchematicIssue::MismatchedSymbolId {
            slot,
            location,
            expected_symbol_id,
        } => (
            "sch.mismatched_symbol_id",
            format!(
                "KiCad symbol '{}' for component '{}' unit {} must use UUID '{}'",
                location.symbol_id,
                slot.component_path(),
                slot.unit(),
                expected_symbol_id
            ),
        ),
        SchematicIssue::UnexpectedSymbol { slot, locations } => (
            "sch.unexpected_symbol",
            format!(
                "KiCad schematic contains {} placement(s) for unknown component '{}' unit {}",
                locations.len(),
                slot.component_path(),
                slot.unit()
            ),
        ),
        SchematicIssue::UnboundSymbol { location } => (
            "sch.unbound_symbol",
            format!(
                "KiCad symbol '{}' on page '{}' is not bound to a Zener component",
                location.symbol_id, location.page_id
            ),
        ),
        SchematicIssue::DisconnectedNet {
            net_name,
            islands,
            missing_terminals,
        } => {
            let detail = if missing_terminals.is_empty() {
                format!("spans {} disconnected KiCad islands", islands.len())
            } else {
                format!(
                    "is missing {} expected terminal(s)",
                    missing_terminals.len()
                )
            };
            (
                "sch.disconnected_net",
                format!("KiCad net '{net_name}' is disconnected: {detail}"),
            )
        }
        SchematicIssue::MissingPort {
            net_name, ports, ..
        } => (
            "sch.missing_port",
            format!(
                "KiCad net '{net_name}' is connected but does not expose interface port(s) {}: add a hierarchical label on the top-level page",
                ports.join(", ")
            ),
        ),
        SchematicIssue::UnexpectedNet { net_name, .. } => (
            "sch.unexpected_net",
            format!("KiCad schematic contains unexpected net '{net_name}'"),
        ),
        SchematicIssue::UnexpectedConnection { terminals, .. } => (
            "sch.unexpected_connection",
            format!(
                "KiCad schematic contains {} unexpected terminal connection(s)",
                terminals.len()
            ),
        ),
        SchematicIssue::Shorted { net_names, .. } => (
            "sch.short",
            format!(
                "KiCad schematic shorts Zener nets {}",
                net_names.iter().cloned().collect::<Vec<_>>().join(", ")
            ),
        ),
    };
    warning(source_path, kind, message)
}

fn warning(source_path: &Path, kind: &str, message: String) -> Diagnostic {
    Diagnostic::categorized(
        &source_path.to_string_lossy(),
        &message,
        kind,
        EvalSeverity::Warning,
    )
}

pub(crate) fn has_unsuppressed_schematic_diagnostics(
    diagnostics: &pcb_zen_core::Diagnostics,
) -> bool {
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
