use ipc2581::Ipc2581;
use pcb_ir::import::ipc2581::import_design;

use super::{build_report, report};
use crate::LayoutTarget;

const FIXTURE: &str = include_str!("testdata/report.xml");

fn report(target: LayoutTarget) -> report::AssemblyReport {
    ipc2581::validate(FIXTURE).expect("assembly report fixture conforms to IPC-2581C");
    let ipc = Ipc2581::parse(FIXTURE).unwrap();
    let imported = import_design(&ipc).unwrap();
    build_report(&imported, target).unwrap()
}

#[test]
fn reports_scoped_components_and_exact_physical_evidence() {
    let report = report(LayoutTarget::BoardArray);

    assert_eq!(report.schema_version, 1);
    assert_eq!(report.scope.kind, report::ScopeKind::BoardArray);
    assert_eq!(report.scope.root_step.as_deref(), Some("panel"));
    assert_eq!(report.source.creation_software.as_deref(), Some("KiCad"));
    assert_eq!(report.summary.board_occurrences, 2);
    assert_eq!(report.summary.packages, 3);
    assert_eq!(report.summary.components.total, 8);
    assert_eq!(report.summary.components.included, 6);
    assert_eq!(report.summary.components.excluded, 2);
    assert_eq!(report.summary.components.included_populated, 4);
    assert_eq!(report.summary.components.included_population_unresolved, 2);
    assert_eq!(report.summary.terminations.total, 6);
    assert_eq!(
        report.summary.terminations.on_included_populated_components,
        4
    );
    assert_eq!(
        report
            .summary
            .terminations
            .surface_on_included_populated_components,
        2
    );
    assert_eq!(
        report
            .summary
            .terminations
            .through_on_included_populated_components,
        2
    );
    assert_eq!(report.summary.paste.islands, 2);
    assert_eq!(report.summary.paste.exactly_linked_to_termination, 2);
    assert_eq!(report.summary.paste.on_included_populated_components, 2);
    assert_eq!(
        report
            .summary
            .paste
            .exactly_linked_on_included_populated_components,
        2
    );

    assert_eq!(report.readiness, report::Readiness::Incomplete);
    assert_eq!(report.diagnostics.len(), 2);
    assert!(report.diagnostics.iter().all(|diagnostic| {
        diagnostic.code == report::DiagnosticCode::MissingPopulation
            && diagnostic.subject.reference_designator.as_deref() == Some("U2")
    }));
    assert!(
        report
            .components
            .iter()
            .filter(|component| {
                component.reference_designator.as_deref() == Some("LOGO")
                    && component.assembly_status == report::AssemblyStatus::Excluded
                    && component.exclusion_reason
                        == Some(report::ExclusionReason::DocumentBomCategory)
            })
            .count()
            == 2
    );

    let u1 = report
        .components
        .iter()
        .find(|component| component.reference_designator.as_deref() == Some("U1"))
        .unwrap();
    assert_eq!(u1.termination_ids.len(), 1);
    assert_eq!(
        u1.bom.as_ref().unwrap().approved_parts[0].manufacturer_part_numbers,
        ["U1-MPN"]
    );
    let through = report
        .terminations
        .iter()
        .find(|termination| termination.pin_type == report::PinType::Through)
        .unwrap();
    assert_eq!(through.lands.len(), 2);
    assert!(through.paste_islands.is_empty());
}

#[test]
fn board_scope_is_one_canonical_board() {
    let report = report(LayoutTarget::Board);

    assert_eq!(report.scope.kind, report::ScopeKind::Board);
    assert_eq!(report.scope.root_step.as_deref(), Some("board"));
    assert_eq!(report.boards.len(), 1);
    assert_eq!(report.components.len(), 4);
    assert_eq!(report.terminations.len(), 3);
    assert_eq!(report.summary.paste.islands, 1);
    assert_eq!(report.diagnostics.len(), 1);
}

#[test]
fn serialization_is_deterministic() {
    let first = serde_json::to_string_pretty(&report(LayoutTarget::BoardArray)).unwrap();
    let second = serde_json::to_string_pretty(&report(LayoutTarget::BoardArray)).unwrap();

    assert_eq!(first, second);
    assert_eq!(
        first,
        include_str!("testdata/report-v1.json").trim_end(),
        "schema v1 changed without an explicit version change"
    );
}

#[test]
fn rejects_non_finite_report_numbers() {
    let ipc = Ipc2581::parse(FIXTURE).unwrap();
    let mut imported = import_design(&ipc).unwrap();
    imported.packages[0].source.height = Some(f64::NAN);

    let error = build_report(&imported, LayoutTarget::BoardArray).unwrap_err();

    assert_eq!(
        error.to_string(),
        "assembly report contains a non-finite number"
    );
}
