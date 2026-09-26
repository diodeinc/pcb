//! Component-data predicates shared with the assembly report.

use crate::assembly::{component_diagnostic_message, report as assembly_report};

use super::super::design::Design;
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
                .filter(|placement| !component.terminated_placements.contains(placement))
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
            subject: Subject {
                role: "affected_component",
                kind: "component",
                name: Some(component.facts.id.clone()),
                reference_designator: component.facts.reference_designator.clone(),
                source: Some(SourceLocator {
                    step: Some(
                        design
                            .imported
                            .resolve(
                                design.imported.geometry.layout.steps[design.step as usize]
                                    .source_step_ref,
                            )
                            .to_owned(),
                    ),
                    layer: None,
                    set_index: Some(component.source_index),
                    feature_index: None,
                    instance_index: None,
                }),
                anchor: Some(component.anchor.into()),
                ..Subject::default()
            },
        });
    }
    Evaluation { checked, issues }
}
