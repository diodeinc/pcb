//! Component-data predicates shared with the assembly report.

use crate::assembly::{component_diagnostic_message, report as assembly_report};

use super::super::design::{AssemblyComponent, Design};
use super::super::pdk::AssemblyDiagnostic;
use super::super::report::{SourceLocator, Subject};

pub(super) struct Evaluation {
    pub checked: usize,
    pub issues: Vec<Issue>,
}

pub(super) struct Issue {
    pub placements: Option<Vec<u32>>,
    pub message: String,
    pub subject: Subject,
}

pub(super) fn evaluate(diagnostic: AssemblyDiagnostic, design: &Design) -> Evaluation {
    let code = match diagnostic {
        AssemblyDiagnostic::MissingPopulation => assembly_report::DiagnosticCode::MissingPopulation,
        AssemblyDiagnostic::ConflictingPopulation => {
            assembly_report::DiagnosticCode::ConflictingPopulation
        }
        AssemblyDiagnostic::MissingReferenceDesignator => {
            assembly_report::DiagnosticCode::MissingReferenceDesignator
        }
        AssemblyDiagnostic::MissingPackage => assembly_report::DiagnosticCode::MissingPackage,
        AssemblyDiagnostic::MissingPhysicalTerminations => {
            assembly_report::DiagnosticCode::MissingPhysicalTerminations
        }
        AssemblyDiagnostic::NonstandardBottomRotation => {
            return nonstandard_bottom_rotation(design);
        }
    };
    let mut issues = Vec::new();
    let mut checked = 0;
    for component in design
        .components
        .iter()
        .filter(|component| component.facts.included)
    {
        checked += design.placements.len();
        let placements = if diagnostic == AssemblyDiagnostic::MissingPhysicalTerminations {
            let missing = (0..design.placements.len() as u32)
                .filter(|placement| {
                    component
                        .terminated_placements
                        .binary_search(placement)
                        .is_err()
                })
                .collect::<Vec<_>>();
            if missing.is_empty() {
                continue;
            }
            (missing.len() != design.placements.len()).then_some(missing)
        } else {
            None
        };
        let mut facts = component.facts.clone();
        facts.has_terminations = false;
        let Some(message) = component_diagnostic_message(&facts, code) else {
            continue;
        };
        issues.push(Issue {
            placements,
            message,
            subject: subject(component, design),
        });
    }
    Evaluation { checked, issues }
}

/// One warning per part whose rotation the import corrected for a known
/// exporter defect: the outputs are right, but the source file is not.
fn nonstandard_bottom_rotation(design: &Design) -> Evaluation {
    let defect = design.imported.flipped_rotation_defect.as_ref();
    // Every populated part is placed, DOCUMENT ones included (see `cpl`).
    let populated = design
        .components
        .iter()
        .filter(|component| component.facts.population == assembly_report::Population::Populate)
        .collect::<Vec<_>>();
    let issues = populated
        .iter()
        .filter_map(|component| {
            let (defect, source) = defect.zip(component.corrected_source_rotation)?;
            let label = component
                .facts
                .reference_designator
                .as_deref()
                .unwrap_or(component.facts.id.as_str());
            Some(Issue {
                placements: None,
                message: format!(
                    "{} wrote bottom-side component '{label}' with rotation {source}° in a non-standard form; pcb corrected it. Re-export with KiCad 10.0.5 or later.",
                    defect.exporter
                ),
                subject: subject(component, design),
            })
        })
        .collect();
    Evaluation {
        checked: populated.len() * design.placements.len(),
        issues,
    }
}

fn subject(component: &AssemblyComponent, design: &Design) -> Subject {
    Subject {
        role: "affected_component",
        kind: "component",
        name: Some(component.facts.id.clone()),
        reference_designator: component.facts.reference_designator.clone(),
        source: Some(SourceLocator {
            step: Some(
                design
                    .imported
                    .resolve(
                        design.imported.geometry.layout.steps[design.step as usize].source_step_ref,
                    )
                    .to_owned(),
            ),
            layer: None,
            set_index: None,
            feature_index: None,
            instance_index: None,
        }),
        anchor: Some(component.anchor.into()),
        ..Subject::default()
    }
}
