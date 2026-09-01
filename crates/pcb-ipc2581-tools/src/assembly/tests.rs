use std::io::Cursor;

use ipc2581::Ipc2581;
use pcb_ir::import::ipc2581::import_design;
use sha2::{Digest, Sha256};

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

    assert_eq!(report.schema_version, 2);
    assert_eq!(report.scope.kind, report::ScopeKind::BoardArray);
    assert_eq!(report.scope.root_step.as_deref(), Some("panel"));
    assert_eq!(report.scope.profile_ids.len(), 1);
    assert_bounds(report.scope.bounds_mm.unwrap(), 0.0, 0.0, 40.0, 35.0);
    assert_eq!(report.scope.area_mm2, Some(1_400.0));
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

    let panel_profile = report
        .profiles
        .iter()
        .find(|profile| profile.source_step == "panel")
        .unwrap();
    assert_eq!(
        report.scope.profile_ids.as_slice(),
        std::slice::from_ref(&panel_profile.id)
    );
    assert_bounds(panel_profile.bounds_mm, 0.0, 0.0, 40.0, 35.0);
    assert_eq!(panel_profile.area_mm2, 1_400.0);
    assert!(panel_profile.cutouts.is_empty());

    let board_profile = report
        .profiles
        .iter()
        .find(|profile| profile.source_step == "board")
        .unwrap();
    assert_bounds(board_profile.bounds_mm, 0.0, 0.0, 10.0, 8.0);
    assert_near(board_profile.area_mm2, 80.0 - std::f64::consts::PI * 0.25);
    assert_eq!(board_profile.cutouts.len(), 1);
    assert!(report.boards.iter().all(|board| {
        board.profile_ids.as_slice() == std::slice::from_ref(&board_profile.id)
            && board.area_mm2 == Some(board_profile.area_mm2)
            && board.bounds_mm == Some(board_profile.bounds_mm)
    }));

    let package = report
        .packages
        .iter()
        .find(|package| package.name == "pkg-smt")
        .unwrap();
    assert_eq!(package.pin_one.as_deref(), Some("1"));
    assert_eq!(package.pin_one_orientation.as_deref(), Some("OTHER"));
    assert_eq!(package.negative_body_extension_mm, Some(0.1));
    assert_eq!(package.comment.as_deref(), Some("SMT body"));
    assert_eq!(
        package.pickup_point_mm,
        Some(report::Point { x: 0.1, y: 0.2 })
    );
    let primary = package
        .views
        .iter()
        .find(|view| view.kind == report::PackageViewKind::Primary)
        .unwrap();
    let outline = primary.outline.as_ref().unwrap();
    assert_eq!(
        outline.shape.status,
        report::PackageGeometryStatus::Complete
    );
    assert!(!outline.shape.paths.is_empty());
    assert_eq!(outline.transform.unwrap().x_offset_mm, 0.1);
    assert_eq!(outline.transform.unwrap().rotation_degrees, 5.0);
    assert_eq!(
        package
            .views
            .iter()
            .map(|view| view.kind)
            .collect::<Vec<_>>(),
        [
            report::PackageViewKind::Primary,
            report::PackageViewKind::Topside,
            report::PackageViewKind::OtherSide,
        ]
    );

    let land_pattern = primary.land_pattern.as_ref().unwrap();
    assert_eq!(land_pattern.pads.len(), 7);
    assert!(matches!(
        land_pattern.pads[0].graphic,
        Some(report::PackageGraphic::Shape(ref shape))
            if shape.status == report::PackageGeometryStatus::Complete
                && !shape.paths.is_empty()
    ));
    assert_eq!(land_pattern.targets.len(), 1);
    assert_eq!(
        land_pattern.targets[0].shape.references[0].kind,
        report::PackageGeometryReferenceKind::StandardPrimitive
    );
    let hollow = pad_shape_with_reference(package, "hollow-inline");
    assert_eq!(hollow.status, report::PackageGeometryStatus::Complete);
    assert!(matches!(
        hollow.paths[0].paint,
        report::PathPaint::Stroke { width_mm, .. } if width_mm == 0.05
    ));
    let hollow_ref = pad_shape_with_reference(package, "hollow-ref");
    assert_eq!(hollow_ref.status, report::PackageGeometryStatus::Complete);
    assert!(hollow_ref.references.iter().any(|reference| {
        reference.kind == report::PackageGeometryReferenceKind::LineDescription
            && reference.id == "fine"
    }));
    assert!(matches!(
        hollow_ref.paths[0].paint,
        report::PathPaint::Stroke { width_mm, .. } if width_mm == 0.05
    ));
    let clear = pad_shape_with_reference(package, "void-ref");
    assert_eq!(clear.status, report::PackageGeometryStatus::Complete);
    assert_eq!(clear.polarity, report::GeometryPolarity::Clear);
    assert!(matches!(
        clear.paths[0].paint,
        report::PathPaint::Fill { .. }
    ));
    assert!(clear.references.iter().any(|reference| {
        reference.kind == report::PackageGeometryReferenceKind::FillDescription
            && reference.id == "void"
    }));
    let hatch = pad_shape_with_reference(package, "hatch-ref");
    assert_eq!(hatch.status, report::PackageGeometryStatus::Partial);
    assert!(
        hatch
            .paths
            .iter()
            .all(|path| path.paint == report::PathPaint::None)
    );
    let mesh = pad_shape_with_reference(package, "mesh-ref");
    assert_eq!(mesh.status, report::PackageGeometryStatus::Partial);
    assert!(
        mesh.paths
            .iter()
            .all(|path| path.paint == report::PathPaint::None)
    );
    let custom = pad_shape_with_reference(package, "custom");
    assert_eq!(custom.status, report::PackageGeometryStatus::Unsupported);
    assert!(custom.paths.is_empty());

    let silkscreen = primary.silkscreen.as_ref().unwrap();
    assert_eq!(silkscreen.outlines.len(), 1);
    assert!(!silkscreen.outlines[0].shape.paths.is_empty());
    assert_eq!(silkscreen.markings.len(), 1);
    assert!(matches!(
        silkscreen.markings[0].graphic,
        report::PackageGraphic::Shape(ref shape) if !shape.paths.is_empty()
    ));

    let drawing = primary.assembly_drawing.as_ref().unwrap();
    assert!(!drawing.outline.as_ref().unwrap().shape.paths.is_empty());
    assert!(matches!(
        drawing.markings[0].graphic,
        report::PackageGraphic::Text(ref text) if text.text == "pkg-smt"
    ));
    assert_eq!(package.pins.len(), 1);
    assert_eq!(
        package.pins[0].shape.references[0].kind,
        report::PackageGeometryReferenceKind::StandardPrimitive
    );
    assert!(!package.pins[0].shape.paths.is_empty());

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
    assert_bounds(report.scope.bounds_mm.unwrap(), 0.0, 0.0, 10.0, 8.0);
    assert_near(
        report.scope.area_mm2.unwrap(),
        80.0 - std::f64::consts::PI * 0.25,
    );
    assert_eq!(report.boards.len(), 1);
    assert_eq!(report.components.len(), 4);
    assert_eq!(report.terminations.len(), 3);
    assert_eq!(report.summary.paste.islands, 1);
    assert_eq!(report.diagnostics.len(), 1);
}

#[test]
fn reports_missing_style_references_without_guessing() {
    let ipc = Ipc2581::parse(FIXTURE).unwrap();
    let mut imported = import_design(&ipc).unwrap();
    let void = imported
        .content
        .dictionary_fill_desc
        .entries
        .iter()
        .find(|entry| imported.resolve(entry.id) == "void")
        .unwrap()
        .id;
    imported
        .content
        .dictionary_fill_desc
        .entries
        .retain(|entry| entry.id != void);

    let report = build_report(&imported, LayoutTarget::BoardArray).unwrap();
    let package = report
        .packages
        .iter()
        .find(|package| package.name == "pkg-smt")
        .unwrap();
    let shape = pad_shape_with_reference(package, "void-ref");

    assert_eq!(shape.status, report::PackageGeometryStatus::Unresolved);
    assert!(
        shape
            .paths
            .iter()
            .all(|path| path.paint == report::PathPaint::None)
    );
}

#[test]
fn panel_without_root_profile_does_not_invent_an_envelope() {
    let mut xml = FIXTURE.to_owned();
    let panel = xml.find("      <Step name=\"panel\"").unwrap();
    let profile_start = panel + xml[panel..].find("        <Profile>").unwrap();
    let profile_end = profile_start
        + xml[profile_start..].find("        </Profile>").unwrap()
        + "        </Profile>".len();
    xml.replace_range(profile_start..profile_end, "");
    ipc2581::validate(&xml).expect("panel without a Profile conforms to IPC-2581C");
    let ipc = Ipc2581::parse(&xml).unwrap();
    let imported = import_design(&ipc).unwrap();

    let report = build_report(&imported, LayoutTarget::BoardArray).unwrap();

    assert!(report.scope.profile_ids.is_empty());
    assert_eq!(report.scope.bounds_mm, None);
    assert_eq!(report.scope.area_mm2, None);
    assert_eq!(
        report
            .profiles
            .iter()
            .map(|profile| profile.source_step.as_str())
            .collect::<Vec<_>>(),
        ["board"]
    );
    assert!(
        report
            .boards
            .iter()
            .all(|board| !board.profile_ids.is_empty())
    );
}

#[test]
fn serialization_is_deterministic() {
    let first = serde_json::to_string_pretty(&report(LayoutTarget::BoardArray)).unwrap();
    let second = serde_json::to_string_pretty(&report(LayoutTarget::BoardArray)).unwrap();

    assert_eq!(first, second);
    assert_eq!(
        hex::encode(Sha256::digest(first.as_bytes())),
        "b93670440008eee052acf83e6895ad1d1dda4c467132cfcff9ccd9ff30f787d9",
        "schema v2 changed without an explicit version change"
    );
}

fn assert_bounds(bounds: report::Bounds, min_x: f64, min_y: f64, max_x: f64, max_y: f64) {
    assert_eq!(bounds.min, report::Point { x: min_x, y: min_y });
    assert_eq!(bounds.max, report::Point { x: max_x, y: max_y });
    assert_eq!(bounds.width, max_x - min_x);
    assert_eq!(bounds.height, max_y - min_y);
}

fn assert_near(actual: f64, expected: f64) {
    assert!((actual - expected).abs() <= 1e-12, "{actual} != {expected}");
}

fn pad_shape_with_reference<'a>(
    package: &'a report::Package,
    reference: &str,
) -> &'a report::PackageShape {
    package
        .views
        .iter()
        .filter_map(|view| view.land_pattern.as_ref())
        .flat_map(|land_pattern| &land_pattern.pads)
        .filter_map(|pad| match pad.graphic.as_ref() {
            Some(report::PackageGraphic::Shape(shape)) => Some(shape),
            _ => None,
        })
        .find(|shape| shape.references.iter().any(|source| source.id == reference))
        .unwrap()
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

#[test]
fn dm0002_excludes_document_objects_from_assembly_work() {
    let compressed = include_bytes!("../../../ipc2581/tests/data/DM0002-IPC-2518.xml.zst");
    let xml = zstd::decode_all(Cursor::new(compressed)).unwrap();
    let ipc = Ipc2581::parse(std::str::from_utf8(&xml).unwrap()).unwrap();
    let imported = import_design(&ipc).unwrap();

    let report = build_report(&imported, LayoutTarget::BoardArray).unwrap();

    assert_eq!(report.summary.components.total, 59);
    assert_eq!(report.summary.components.included, 53);
    assert_eq!(report.summary.components.excluded, 6);
    assert_eq!(report.summary.components.included_populated, 53);
    assert!(
        report
            .components
            .iter()
            .filter(|component| component.assembly_status == report::AssemblyStatus::Included)
            .all(|component| component.side == report::Side::Top)
    );
}
