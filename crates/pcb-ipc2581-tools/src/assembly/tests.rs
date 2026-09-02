use std::io::Cursor;

use ipc2581::Ipc2581;
use pcb_ir::import::ipc2581::import_design;
use sha2::{Digest, Sha256};

use super::{build_report, report};
use crate::LayoutTarget;

const FIXTURE: &str = include_str!("testdata/report.xml");

fn report(target: LayoutTarget) -> report::AssemblyReport {
    report_xml(FIXTURE, target)
}

fn report_xml(xml: &str, target: LayoutTarget) -> report::AssemblyReport {
    ipc2581::validate(xml).expect("assembly report fixture conforms to IPC-2581C");
    let ipc = Ipc2581::parse(xml).unwrap();
    let imported = import_design(&ipc).unwrap();
    build_report(&imported, target).unwrap()
}

#[test]
fn reports_scoped_components_and_exact_physical_evidence() {
    let report = report(LayoutTarget::BoardArray);

    assert_eq!(report.schema_version, 3);
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
    assert!(
        report
            .boards
            .iter()
            .all(|board| board.transform[0] * board.transform[3]
                - board.transform[1] * board.transform[2]
                < 0.0)
    );

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

    assert_eq!(report.holes.len(), 6);
    assert_eq!(
        report
            .holes
            .iter()
            .filter(|hole| {
                hole.termination.status == report::AssociationStatus::ExactGeometric
                    && hole.termination.basis == Some(report::AssociationBasis::ExactGeometry)
            })
            .count(),
        2
    );
    assert_eq!(
        report
            .holes
            .iter()
            .filter(|hole| {
                hole.termination.status == report::AssociationStatus::Explicit
                    && hole.termination.basis == Some(report::AssociationBasis::SourceIdentity)
            })
            .count(),
        2
    );
    assert!(report.holes.iter().all(|hole| hole.source_name.is_some()));
    assert!(
        report
            .holes
            .iter()
            .filter(|hole| { hole.termination.status == report::AssociationStatus::ExactGeometric })
            .all(|hole| {
                hole.protection.status == report::ProtectionStatus::Explicit
                    && hole.protection.fill_material == Some(report::FillMaterial::NonConductive)
                    && hole
                        .protection
                        .methods
                        .contains(&report::ProtectionMethod::Filled)
            })
    );
    assert!(
        report
            .holes
            .iter()
            .filter(|hole| { hole.plating == report::HolePlating::ViaCapped })
            .all(|hole| hole
                .protection
                .methods
                .contains(&report::ProtectionMethod::Capped))
    );
    assert!(
        report
            .holes
            .iter()
            .filter(|hole| { hole.termination.status == report::AssociationStatus::Unresolved })
            .all(|hole| hole.protection.status == report::ProtectionStatus::Unknown)
    );
    let u1_termination = report
        .terminations
        .iter()
        .find(|termination| termination.component_id == u1.id)
        .unwrap();
    assert_eq!(u1_termination.hole_ids.len(), 2);
    assert_eq!(
        u1_termination.mask_openings,
        [report::MaskEvidence {
            layer: "MASK".to_owned(),
            side: report::Side::Top,
        }]
    );
}

#[test]
fn reports_ambiguous_overlap_without_selecting_a_termination() {
    let xml = FIXTURE.replace(
        "name=\"standalone-via\" diameter=\"0.2\" platingStatus=\"VIA\" plusTol=\"0\" minusTol=\"0\" x=\"8\" y=\"5\"",
        "name=\"standalone-via\" diameter=\"2.2\" platingStatus=\"VIA\" plusTol=\"0\" minusTol=\"0\" x=\"3\" y=\"2\"",
    );

    let report = report_xml(&xml, LayoutTarget::Board);
    let hole = report
        .holes
        .iter()
        .find(|hole| hole.finished_diameter_mm == Some(2.2))
        .unwrap();

    assert_eq!(
        hole.termination.status,
        report::AssociationStatus::Ambiguous
    );
    assert_eq!(
        hole.termination.basis,
        Some(report::AssociationBasis::ExactGeometry)
    );
    assert_eq!(hole.termination.termination_ids.len(), 2);
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == report::DiagnosticCode::AmbiguousHoleTermination
            && diagnostic.subject.id == hole.id
    }));
}

#[test]
fn geometric_association_requires_one_known_reachable_land() {
    for replacement in [
        "<Span fromLayer=\"TOP\"/>",
        "<Span fromLayer=\"BOTTOM\" toLayer=\"BOTTOM\"/>",
    ] {
        let xml = FIXTURE.replacen(
            "<Span fromLayer=\"TOP\" toLayer=\"BOTTOM\"/>",
            replacement,
            1,
        );
        let report = report_xml(&xml, LayoutTarget::Board);
        let via = report
            .holes
            .iter()
            .find(|hole| hole.source_name.as_deref() == Some("U1-via"))
            .unwrap();
        assert_eq!(
            via.termination.status,
            report::AssociationStatus::Unresolved
        );
        assert_eq!(via.termination.basis, None);
    }

    let u1_land = "<Set><Pad padstackDefRef=\"smt-padstack\"><Location x=\"2\" y=\"2\"/><StandardPrimitiveRef id=\"land\"/><PinRef componentRef=\"U1\" pin=\"1\"/></Pad></Set>";
    let xml = FIXTURE.replacen(u1_land, &format!("{u1_land}\n          {u1_land}"), 1);
    let report = report_xml(&xml, LayoutTarget::Board);
    let via = report
        .holes
        .iter()
        .find(|hole| hole.source_name.as_deref() == Some("U1-via"))
        .unwrap();
    assert_eq!(via.termination.status, report::AssociationStatus::Ambiguous);
    assert_eq!(via.termination.termination_ids.len(), 1);
}

#[test]
fn semantic_hole_ids_survive_source_reordering() {
    let capped = "          <Set geometry=\"via-padstack\" componentRef=\"U1\"><Hole name=\"U1-via-capped\" diameter=\"0.2\" platingStatus=\"VIA_CAPPED\" plusTol=\"0\" minusTol=\"0\" x=\"1.8\" y=\"2\"/></Set>";
    let filled = "          <Set geometry=\"via-padstack\"><Hole name=\"U1-via\" diameter=\"0.2\" platingStatus=\"VIA\" plusTol=\"0\" minusTol=\"0\" x=\"2.2\" y=\"2\"/></Set>";
    let reordered = FIXTURE.replace(
        &format!("{capped}\n{filled}"),
        &format!("{filled}\n{capped}"),
    );

    assert_ne!(reordered, FIXTURE);
    assert_eq!(
        report(LayoutTarget::Board),
        report_xml(&reordered, LayoutTarget::Board)
    );
}

#[test]
fn reports_unknown_and_conflicting_component_land_via_protection() {
    let unknown_xml = FIXTURE.replace(
        "name=\"standalone-via\" diameter=\"0.2\" platingStatus=\"VIA\" plusTol=\"0\" minusTol=\"0\" x=\"8\" y=\"5\"",
        "name=\"standalone-via\" diameter=\"0.2\" platingStatus=\"VIA\" plusTol=\"0\" minusTol=\"0\" x=\"2\" y=\"2.3\"",
    );
    let unknown = report_xml(&unknown_xml, LayoutTarget::Board);
    assert!(
        unknown
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.code == report::DiagnosticCode::UnknownViaProtection })
    );

    let plated_xml = unknown_xml.replace(
        "name=\"standalone-via\" diameter=\"0.2\" platingStatus=\"VIA\"",
        "name=\"standalone-via\" diameter=\"0.2\" platingStatus=\"PLATED\"",
    );
    let plated = report_xml(&plated_xml, LayoutTarget::Board);
    let plated_hole = plated
        .holes
        .iter()
        .find(|hole| hole.location_mm == report::Point { x: 2.0, y: 2.3 })
        .unwrap();
    assert_eq!(
        plated_hole.termination.status,
        report::AssociationStatus::ExactGeometric
    );
    assert!(!plated.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == report::DiagnosticCode::UnknownViaProtection
            && diagnostic.subject.id == plated_hole.id
    }));

    let conflicting_xml = FIXTURE.replace("NON-CONDUCTIVE EPOXY", "OPEN NON-CONDUCTIVE EPOXY");
    let conflicting = report_xml(&conflicting_xml, LayoutTarget::Board);
    assert!(conflicting.holes.iter().any(|hole| {
        hole.termination.status == report::AssociationStatus::ExactGeometric
            && hole.protection.status == report::ProtectionStatus::Conflicting
    }));
    assert!(
        conflicting.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == report::DiagnosticCode::ConflictingViaProtection
        })
    );
}

#[test]
fn preserves_distinct_explicit_protection_methods_and_fill_materials() {
    for (term, method, material) in [
        (
            "NON-CONDUCTIVE EPOXY",
            report::ProtectionMethod::Filled,
            Some(report::FillMaterial::NonConductive),
        ),
        (
            "CONDUCTIVE EPOXY",
            report::ProtectionMethod::Filled,
            Some(report::FillMaterial::Conductive),
        ),
        (
            "COPPER FILL",
            report::ProtectionMethod::Filled,
            Some(report::FillMaterial::Copper),
        ),
        (
            "NON-CONDUCTIVE PLUGGED EPOXY",
            report::ProtectionMethod::Plugged,
            Some(report::FillMaterial::NonConductive),
        ),
    ] {
        let xml = FIXTURE.replace("NON-CONDUCTIVE EPOXY", term);
        let report = report_xml(&xml, LayoutTarget::Board);
        assert!(
            report
                .holes
                .iter()
                .filter(|hole| {
                    hole.termination.status == report::AssociationStatus::ExactGeometric
                })
                .all(|hole| {
                    hole.protection.methods.contains(&method)
                        && hole.protection.fill_material == material
                })
        );
    }

    for (function, term, method) in [
        (
            "COATINGNONCOND",
            "TENTED COATING",
            report::ProtectionMethod::Tented,
        ),
        (
            "COATINGCOND",
            "CAPPED COATING",
            report::ProtectionMethod::Capped,
        ),
    ] {
        let xml = FIXTURE
            .replace(
                "layerFunction=\"HOLEFILL\"",
                &format!("layerFunction=\"{function}\""),
            )
            .replace("NON-CONDUCTIVE EPOXY", term);
        let report = report_xml(&xml, LayoutTarget::Board);
        assert!(
            report
                .holes
                .iter()
                .filter(|hole| {
                    hole.termination.status == report::AssociationStatus::ExactGeometric
                })
                .all(|hole| {
                    hole.protection.methods.contains(&method)
                        && hole.protection.fill_material.is_none()
                })
        );
    }

    let ordinary_coating = FIXTURE
        .replace(
            "layerFunction=\"HOLEFILL\"",
            "layerFunction=\"COATINGCOND\"",
        )
        .replace("NON-CONDUCTIVE EPOXY", "ENIG");
    let report = report_xml(&ordinary_coating, LayoutTarget::Board);
    let capped = report
        .holes
        .iter()
        .find(|hole| hole.plating == report::HolePlating::ViaCapped)
        .unwrap();
    assert_eq!(
        capped.protection.methods,
        [report::ProtectionMethod::Capped]
    );
    assert_eq!(capped.protection.fill_material, None);
    let uncapped = report
        .holes
        .iter()
        .find(|hole| hole.source_name.as_deref() == Some("U1-via"))
        .unwrap();
    assert_eq!(
        uncapped.protection.status,
        report::ProtectionStatus::Unknown
    );
}

#[test]
fn reports_explicit_open_source_terms_without_inferring_fill() {
    let xml = FIXTURE
        .replace(
            "    </CadHeader>",
            "      <Spec name=\"open-via\"><General type=\"MATERIAL\"><Property text=\"OPEN\"/></General></Spec>\n    </CadHeader>",
        )
        .replace(
            "<Set geometry=\"via-padstack\"><Hole name=\"standalone-via\" diameter=\"0.2\" platingStatus=\"VIA\" plusTol=\"0\" minusTol=\"0\" x=\"8\" y=\"5\"/></Set>",
            "<Set geometry=\"via-padstack\"><SpecRef id=\"open-via\"/><Hole name=\"standalone-via\" diameter=\"0.2\" platingStatus=\"VIA\" plusTol=\"0\" minusTol=\"0\" x=\"8\" y=\"5\"/></Set>",
        );

    let report = report_xml(&xml, LayoutTarget::Board);
    let hole = report
        .holes
        .iter()
        .find(|hole| hole.location_mm == report::Point { x: 8.0, y: 5.0 })
        .unwrap();

    assert_eq!(hole.protection.status, report::ProtectionStatus::Explicit);
    assert_eq!(hole.protection.methods, [report::ProtectionMethod::Open]);
    assert_eq!(hole.protection.fill_material, None);
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
    assert_eq!(report.holes.len(), 3);
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
        "047ec7cb0494a77bb7a1261584eb0cd97ab15743fb1c9d7bd4c1d6491d46739c",
        "schema v3 changed without an explicit version change"
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
