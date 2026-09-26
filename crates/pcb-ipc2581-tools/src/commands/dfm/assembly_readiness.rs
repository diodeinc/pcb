//! Standard-PDK rules backed by the assembly report's completeness diagnostics.

use anyhow::Result;
use pcb_ir::geom::Resolution;
use pcb_ir::import::ipc2581::ImportedDesign;

use super::checks::AdditionalResults;
use super::report::{Finding, Location, Measurement, RuleResult, Severity, Subject};
use crate::LayoutTarget;
use crate::assembly::{self, report as assembly_report};

struct DiagnosticRule {
    id: &'static str,
    title: &'static str,
}

const RULES: &[DiagnosticRule] = &[
    DiagnosticRule {
        id: "assembly.missing_population",
        title: "Components have explicit population states",
    },
    DiagnosticRule {
        id: "assembly.conflicting_population",
        title: "Components have consistent population states",
    },
    DiagnosticRule {
        id: "assembly.missing_reference_designator",
        title: "Components have reference designators",
    },
    DiagnosticRule {
        id: "assembly.missing_package",
        title: "Populated components have resolved packages",
    },
    DiagnosticRule {
        id: "assembly.missing_physical_terminations",
        title: "Populated solder-mounted components have physical terminations",
    },
];

fn rule_for(code: assembly_report::DiagnosticCode) -> Option<&'static DiagnosticRule> {
    match code {
        assembly_report::DiagnosticCode::MissingPopulation => Some(&RULES[0]),
        assembly_report::DiagnosticCode::ConflictingPopulation => Some(&RULES[1]),
        assembly_report::DiagnosticCode::MissingReferenceDesignator => Some(&RULES[2]),
        assembly_report::DiagnosticCode::MissingPackage => Some(&RULES[3]),
        assembly_report::DiagnosticCode::MissingPhysicalTerminations => Some(&RULES[4]),
        assembly_report::DiagnosticCode::AmbiguousHoleTermination
        | assembly_report::DiagnosticCode::ConflictingHoleTermination
        | assembly_report::DiagnosticCode::ConflictingViaProtection
        | assembly_report::DiagnosticCode::UnknownViaProtection => None,
    }
}

pub(super) fn check(
    imported: &ImportedDesign,
    target: LayoutTarget,
    resolution: Resolution,
) -> Result<AdditionalResults> {
    let readiness = assembly::component_diagnostics_only(imported, target, resolution)?;
    let mut results = AdditionalResults {
        rules: RULES
            .iter()
            .map(|rule| {
                RuleResult::assembly_diagnostic(
                    rule.id.to_owned(),
                    rule.title,
                    "component",
                    readiness.included,
                )
            })
            .collect(),
        findings: Vec::new(),
    };

    for diagnostic in readiness
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == assembly_report::DiagnosticSeverity::Error)
    {
        let rule = rule_for(diagnostic.code)
            .expect("every incomplete assembly diagnostic has a standard DFM rule");
        results.findings.push(Finding {
            id: String::new(),
            rule_id: rule.id.to_owned(),
            severity: Severity::Error,
            waived: false,
            waiver_reason: None,
            title: rule.title.to_owned(),
            message: diagnostic.message.clone(),
            measurement: Measurement::maximum_count(1, 0),
            location: Location::default(),
            layers: Vec::new(),
            subjects: vec![Subject {
                role: "affected_component",
                kind: "component",
                name: Some(diagnostic.subject.id.clone()),
                reference_designator: diagnostic.subject.reference_designator.clone(),
                ..Subject::default()
            }],
            evidence: Vec::new(),
            sites: Vec::new(),
            frame: 0,
        });
    }

    Ok(results)
}
