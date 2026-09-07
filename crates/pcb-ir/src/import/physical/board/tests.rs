use super::*;
use crate::import::ipc2581::import_design;
use ipc2581::Ipc2581;

fn design(xml: &str) -> ImportedDesign {
    import_design(&Ipc2581::parse(xml).unwrap(), Resolution::default()).unwrap()
}

// Deliberately asymmetric geometry: mirroring/rotation mistakes cannot hide
// behind circular or origin-centered envelopes. The nested placements must
// disappear from the canonical board frame, but component placements must not.
fn fixture() -> &'static str {
    r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner"><FunctionMode mode="FABRICATION"/><StepRef name="panel"/></Content>
  <Ecad name="test"><CadHeader units="MILLIMETER"/><CadData>
    <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
    <Layer name="ROUT" layerFunction="ROUT" side="ALL" polarity="POSITIVE"><Span fromLayer="TOP" toLayer="TOP"/></Layer>
    <Stackup name="stack" overallThickness="1.6"><StackupGroup name="g">
      <StackupLayer layerOrGroupRef="TOP" thickness="0.035" sequence="0"/>
    </StackupGroup></Stackup>
    <Step name="board" type="BOARD"><Datum x="3" y="4"/>
      <Profile><Polygon><PolyBegin x="0" y="0"/><PolyStepSegment x="10" y="0"/><PolyStepSegment x="10" y="8"/><PolyStepSegment x="0" y="8"/><PolyStepSegment x="0" y="0"/></Polygon>
        <Cutout><PolyBegin x="6" y="5"/><PolyStepSegment x="8" y="5"/><PolyStepSegment x="8" y="7"/><PolyStepSegment x="6" y="7"/><PolyStepSegment x="6" y="5"/></Cutout>
      </Profile>
      <Package name="pkg" type="OTHER" pinOne="1" pinOneOrientation="OTHER">
        <Outline><Polygon><PolyBegin x="0" y="0"/><PolyStepSegment x="4" y="0"/><PolyStepSegment x="4" y="2"/><PolyStepSegment x="0" y="2"/><PolyStepSegment x="0" y="0"/></Polygon><LineDesc lineWidth="0.1" lineEnd="ROUND"/></Outline>
        <AssemblyDrawing><Outline><Polygon><PolyBegin x="0" y="0"/><PolyStepSegment x="2" y="0"/><PolyStepSegment x="2" y="1"/><PolyStepSegment x="0" y="1"/><PolyStepSegment x="0" y="0"/></Polygon><LineDesc lineWidth="0.1" lineEnd="ROUND"/></Outline></AssemblyDrawing>
      </Package>
      <Component refDes="U1" packageRef="pkg" part="p" layerRef="TOP" mountType="SMT"><Xform rotation="90" mirror="true"/><Location x="4" y="4"/></Component>
      <Component refDes="U2" packageRef="pkg" part="p" layerRef="TOP" mountType="SMT"><Location x="5" y="1"/></Component>
      <LayerFeature layerRef="TOP">
        <Set><Features><Location x="0" y="0"/><Contour><Polygon><PolyBegin x="0" y="0"/><PolyStepSegment x="5" y="0"/><PolyStepSegment x="5" y="4"/><PolyStepSegment x="0" y="4"/><PolyStepSegment x="0" y="0"/></Polygon></Contour></Features></Set>
        <Set polarity="NEGATIVE"><Features><Location x="0" y="0"/><Contour><Polygon><PolyBegin x="1" y="1"/><PolyStepSegment x="2" y="1"/><PolyStepSegment x="2" y="2"/><PolyStepSegment x="1" y="2"/><PolyStepSegment x="1" y="1"/></Polygon></Contour></Features></Set>
      </LayerFeature>
      <LayerFeature layerRef="ROUT"><Set><SlotCavity name="slot" platingStatus="NONPLATED" plusTol="0" minusTol="0"><Location x="8" y="2"/><Oval width="2" height="1"/></SlotCavity></Set></LayerFeature>
    </Step>
    <Step name="cell" type="PALLET"><StepRepeat stepRef="board" x="30" y="40" nx="2" ny="1" dx="20" dy="0" angle="90" mirror="true"/></Step>
    <Step name="panel" type="PALLET"><StepRepeat stepRef="cell" x="100" y="200" nx="1" ny="2" dx="0" dy="80" angle="270" mirror="true"/></Step>
  </CadData></Ecad>
</IPC-2581>"#
}

#[test]
fn board_local_profile_copper_rout_and_identity_survive_nested_placement() {
    let imported = design(fixture());
    let view = imported.physical_board(Resolution::default()).unwrap();
    assert_eq!(view.substrate.area(), 76.0);
    assert_eq!(view.profile_cutouts.area(), 4.0);
    assert!(!view.substrate.contains_point(Point::new(7.0, 6.0)));
    assert!(
        view.substrate.contains_point(Point::new(8.0, 2.0)),
        "not an implicit through-route subtraction"
    );
    assert_eq!(view.copper[0].image.area(), 19.0);
    assert!(!view.copper[0].image.contains_point(Point::new(1.5, 1.5)));
    assert_eq!(view.holes.len(), 1);
    assert_eq!(view.holes[0].at, Point::new(8.0, 2.0));
    assert!(view.holes[0].image.contains_point(Point::new(8.0, 2.0)));
    assert_eq!(view.removal_layers.len(), 1);
    assert_eq!(view.removal_layers[0].sources, [view.holes[0].id.0]);
    assert!(
        view.removal_layers[0]
            .image
            .contains_point(Point::new(8.0, 2.0))
    );
    assert_eq!(imported.resolve(view.holes[0].source_name.unwrap()), "slot");
    assert!(
        imported
            .feature_definition(view.holes[0].id.0.feature)
            .is_some()
    );
    assert_eq!(view.components.len(), 2);
    assert_ne!(view.components[0].component, view.components[1].component);
    let u1 = &view.components[0];
    assert_eq!(u1.designator.as_deref(), Some("U1"));
    assert_eq!(u1.envelopes.len(), 2);
    assert_eq!(
        u1.envelopes[0].kind,
        EnvelopeKind::PackageOutlineUnspecified
    );
    assert_eq!(u1.envelopes[1].kind, EnvelopeKind::AssemblyOutline);
    assert_eq!(u1.envelopes[0].image.area(), 8.0);
    assert_eq!(u1.envelopes[1].image.area(), 2.0);
    assert!(u1.envelopes[1].image.contains_point(Point::new(3.5, 3.0)));
    assert!(!u1.envelopes[1].image.contains_point(Point::new(2.5, 3.0)));
    assert!(
        view.diagnostics
            .contains(&BoardPhysicalDiagnostic::UnspecifiedPackageOutline(
                u1.component
            ))
    );
    let direct =
        design(&fixture().replace("<StepRef name=\"panel\"/>", "<StepRef name=\"board\"/>"));
    let direct = direct.physical_board(Resolution::default()).unwrap();
    assert!(
        view.substrate
            .difference(&direct.substrate)
            .unwrap()
            .is_empty()
    );
    assert!(
        view.copper[0]
            .image
            .difference(&direct.copper[0].image)
            .unwrap()
            .is_empty()
    );
    assert_eq!(view.components[0].component, direct.components[0].component);
}

#[test]
fn assembly_ir_envelopes_keep_nested_identity_and_outline_local_transform() {
    let imported = design(fixture());
    let assembly = imported
        .assembly_document(assembly::Scope::BoardArray)
        .unwrap();
    let envelopes = component_envelopes(&assembly, Resolution::default()).unwrap();
    assert_eq!(envelopes.len(), 8);
    let ids = envelopes
        .iter()
        .map(|component| component.component)
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(ids.len(), 8);
    for (component, occurrence) in envelopes.iter().zip(&assembly.occurrences) {
        let expected = occurrence
            .root_from_component
            .transform_point(Point::new(1.0, 0.5));
        assert!(component.envelopes[1].image.contains_point(expected));
        assert!((component.envelopes[1].image.area() - 2.0).abs() < 1e-6);
    }

    // Exercise the geometry-only API after import, including polygon-local
    // offsets in the rotated/mirrored/scaled frame (not world-space offsets).
    let mut assembly = imported.assembly_document(assembly::Scope::Board).unwrap();
    assembly.packages[0].views[0]
        .outline
        .as_mut()
        .unwrap()
        .transform = Some(assembly::Transform {
        x_offset: 1.0,
        y_offset: 2.0,
        rotation_degrees: 90.0,
        mirror: true,
        face_up: false,
        scale: 2.0,
    });
    let envelopes = component_envelopes(&assembly, Resolution::default()).unwrap();
    // (2,1) + offset -> (3,3), mirror/scale -> (-6,6), rotate -> (-6,-6),
    // U2 translation -> (-1,-5). U2 has no component rotation.
    assert!(
        envelopes[1].envelopes[0]
            .image
            .contains_point(Point::new(-1.0, -5.0))
    );
    assert_eq!(envelopes[1].envelopes[0].image.area(), 32.0);
    assert_eq!(envelopes[1].envelopes[1].image.area(), 2.0);
}

#[test]
fn generic_rout_artwork_and_clears_are_not_lost_to_hole_classification() {
    let xml = fixture().replace("<LayerFeature layerRef=\"ROUT\"><Set>", r#"<LayerFeature layerRef="ROUT">
      <Set><Features><Location x="0" y="0"/><Contour><Polygon><PolyBegin x="0" y="0"/><PolyStepSegment x="3" y="0"/><PolyStepSegment x="3" y="3"/><PolyStepSegment x="0" y="3"/><PolyStepSegment x="0" y="0"/></Polygon></Contour></Features></Set>
      <Set polarity="NEGATIVE"><Features><Location x="0" y="0"/><Contour><Polygon><PolyBegin x="1" y="1"/><PolyStepSegment x="2" y="1"/><PolyStepSegment x="2" y="2"/><PolyStepSegment x="1" y="2"/><PolyStepSegment x="1" y="1"/></Polygon></Contour></Features></Set><Set>"#);
    let view = design(&xml).physical_board(Resolution::default()).unwrap();
    assert_eq!(view.holes.len(), 1);
    assert_eq!(view.removal_layers[0].sources.len(), 3);
    assert!(
        view.removal_layers[0]
            .image
            .contains_point(Point::new(0.5, 0.5))
    );
    assert!(
        !view.removal_layers[0]
            .image
            .contains_point(Point::new(1.5, 1.5))
    );
    assert!(
        view.removal_layers[0]
            .image
            .contains_point(Point::new(8.0, 2.0))
    );
}

#[test]
fn inch_input_normalizes_every_physical_length_at_the_boundary() {
    let mm = design(fixture())
        .physical_board(Resolution::default())
        .unwrap();
    let inch = design(&fixture().replace("MILLIMETER", "INCH"))
        .physical_board(Resolution::default())
        .unwrap();
    // Boolean coordinate snapping introduces small absolute area error on
    // this 254 mm board. This bound is well below a 1 µm boundary displacement.
    assert!((inch.substrate.area() - mm.substrate.area() * 25.4 * 25.4).abs() < 1e-4);
    assert!((inch.components[0].envelopes[1].image.area() - 2.0 * 25.4 * 25.4).abs() < 1e-6);
    assert!((inch.holes[0].at.x - 8.0 * 25.4).abs() < 1e-9);
    assert!((inch.metadata.overall_thickness_mm.unwrap() - 1.6 * 25.4).abs() < 1e-9);
    assert!((inch.metadata.layers[0].thickness_mm.unwrap() - 0.035 * 25.4).abs() < 1e-9);
}

#[test]
fn metadata_reports_missing_ambiguous_invalid_and_conflicting_evidence() {
    let mut imported = design(fixture());
    let meta = imported.physical_board_metadata();
    assert_eq!(meta.overall_thickness_mm, Some(1.6));
    assert!(matches!(meta.layers[0].material, Association::Unresolved));
    let layer = meta.layers[0].layer_ref;
    assert!(
        meta.diagnostics
            .contains(&BoardPhysicalDiagnostic::MissingMaterial { layer })
    );
    imported.stackups[0].layers[0].thickness = Some(-1.0);
    imported.stackups[0].layers[0].spec_ref = Some(layer);
    imported.stackups[0].overall_thickness = None;
    let meta = imported.physical_board_metadata();
    assert_eq!(meta.layers[0].thickness_mm, None);
    assert!(
        meta.diagnostics
            .contains(&BoardPhysicalDiagnostic::InvalidThickness {
                layer: Some(layer),
                value: -1.0
            })
    );
    assert!(
        meta.diagnostics
            .contains(&BoardPhysicalDiagnostic::MissingThickness { layer: None })
    );
    assert!(
        meta.diagnostics
            .contains(&BoardPhysicalDiagnostic::UnresolvedSpec { layer, spec: layer })
    );
    let source = imported.stackups[0].clone();
    imported.stackups.push(source);
    assert!(matches!(
        imported.physical_board_metadata().stackup,
        Association::Ambiguous(_)
    ));
    imported.stackups.clear();
    assert_eq!(
        imported.physical_board_metadata().diagnostics,
        [BoardPhysicalDiagnostic::MissingStackup]
    );

    let xml = fixture().replace("<CadHeader units=\"MILLIMETER\"/>", r#"<CadHeader units="MILLIMETER"><Spec name="mat"><General type="MATERIAL"><Property text="FR4"/></General></Spec></CadHeader>"#)
        .replace("sequence=\"0\"/>", "sequence=\"0\"><SpecRef id=\"mat\"/></StackupLayer>");
    let mut imported = design(&xml);
    assert!(matches!(
        imported.physical_board_metadata().layers[0].material,
        Association::Resolved(_)
    ));
    // Canonical source evidence can conflict after programmatic edits.
    imported.stackups[0].layers[0].material = Some(layer);
    let meta = imported.physical_board_metadata();
    assert!(
        matches!(meta.layers[0].material, Association::Conflicting(_)),
        "{meta:?}"
    );

    let ambiguous = design(&xml.replace(
        "<Property text=\"FR4\"/>",
        "<Property text=\"FR4\"/><Property text=\"PTFE\"/>",
    ));
    assert!(matches!(
        ambiguous.physical_board_metadata().layers[0].material,
        Association::Ambiguous(_)
    ));
    let unresolved = design(&fixture().replace(
        "sequence=\"0\"/>",
        "sequence=\"0\"><SpecRef id=\"external\"/></StackupLayer>",
    ));
    assert!(
        unresolved
            .physical_board_metadata()
            .diagnostics
            .iter()
            .any(|diagnostic| matches!(diagnostic, BoardPhysicalDiagnostic::UnresolvedSpec { .. }))
    );
}

#[test]
fn missing_profile_and_multiple_board_definitions_are_not_silent() {
    let mut imported = design(fixture());
    let board = imported
        .geometry
        .layout
        .steps
        .iter_mut()
        .find(|step| step.kind == LayoutStepKind::Board)
        .unwrap();
    board.profiles = crate::geom::Span::EMPTY;
    let extra = board.clone();
    let view = imported.physical_board(Resolution::default()).unwrap();
    assert!(
        view.diagnostics
            .contains(&BoardPhysicalDiagnostic::MissingProfile)
    );
    imported.geometry.layout.steps.push(extra);
    assert!(
        imported
            .physical_board(Resolution::default())
            .unwrap_err()
            .to_string()
            .contains("exactly one board definition")
    );
}

#[test]
fn unresolved_stackup_keeps_board_and_hole_evidence_without_land_associations() {
    let baseline = design(fixture())
        .physical_board(Resolution::default())
        .unwrap();
    for ambiguous in [true, false] {
        let mut imported = design(fixture());
        if ambiguous {
            imported.stackups.push(imported.stackups[0].clone());
        } else {
            let layer = imported.stackups[0].layers[0].clone();
            imported.stackups[0].layers.push(layer);
        }
        let view = imported.physical_board(Resolution::default()).unwrap();
        assert!(view.metadata.diagnostics.iter().any(|diagnostic| matches!(
            diagnostic,
            BoardPhysicalDiagnostic::AmbiguousStackup
                | BoardPhysicalDiagnostic::InvalidStackupOrder(_)
        )));
        assert_eq!(view.substrate.area(), baseline.substrate.area());
        assert_eq!(view.holes.len(), baseline.holes.len());
        let hole = &view.holes[0];
        let original = &baseline.holes[0];
        assert_eq!(hole.id, original.id);
        assert_eq!(hole.source_name, original.source_name);
        assert_eq!(hole.span, original.span);
        assert_eq!(hole.plating, original.plating);
        assert_eq!(hole.image.area(), original.image.area());
        assert!(hole.lands.is_empty());
        assert!(matches!(hole.termination, Association::Unresolved));
        // The strict association API keeps its existing rejection contract.
        assert!(
            imported
                .physical_holes(ArtworkScope::Board, Resolution::default())
                .is_err()
        );
    }
}

#[test]
fn material_designators_preserve_identity_and_reconcile_bom_spec_evidence() {
    let xml = fixture().replace("sequence=\"0\"", "sequence=\"0\" matDes=\"laminate-42\"");
    let imported = design(&xml);
    let metadata = imported.physical_board_metadata();
    let layer = &metadata.layers[0];
    assert_eq!(imported.resolve(layer.mat_des.unwrap()), "laminate-42");
    assert_eq!(imported.stackups[0].layers[0].mat_des, layer.mat_des);
    assert!(matches!(layer.material, Association::Unresolved));
    assert!(metadata.diagnostics.iter().any(|diagnostic| matches!(
        diagnostic,
        BoardPhysicalDiagnostic::UnresolvedMaterialDesignator { .. }
    )));

    let bom = r#"<Bom name="materials"><BomItem OEMDesignNumberRef="laminate" quantity="1" category="MATERIAL"><MatDes name="laminate-42"/><SpecRef id="bom-material"/></BomItem></Bom>"#;
    let xml = xml.replace("<Ecad name=\"test\">", &format!("{bom}<Ecad name=\"test\">"))
        .replace("<CadHeader units=\"MILLIMETER\"/>", r#"<CadHeader units="MILLIMETER"><Spec name="bom-material"><General type="MATERIAL"><Property text="FR4"/></General></Spec><Spec name="layer-material"><General type="MATERIAL"><Property text="PTFE"/></General></Spec></CadHeader>"#);
    let imported = design(&xml);
    let metadata = imported.physical_board_metadata();
    assert_eq!(
        imported.resolve(*metadata.layers[0].material.resolved().unwrap()),
        "FR4"
    );
    assert!(metadata.diagnostics.is_empty());

    let mut conflict = design(&xml.replace(
        "matDes=\"laminate-42\"/>",
        "matDes=\"laminate-42\"><SpecRef id=\"layer-material\"/></StackupLayer>",
    ));
    let metadata = conflict.physical_board_metadata();
    assert!(matches!(
        metadata.layers[0].material,
        Association::Conflicting(_)
    ));
    assert_eq!(
        conflict.resolve(metadata.layers[0].mat_des.unwrap()),
        "laminate-42"
    );
    // Public imported evidence can carry a third declared material, even
    // though XML parsing normally derives this field from the layer SpecRef.
    conflict.stackups[0].layers[0].material = conflict.stackups[0].layers[0].mat_des;
    let metadata = conflict.physical_board_metadata();
    let Association::Conflicting(values) = &metadata.layers[0].material else {
        panic!("all three source values must remain conflicting");
    };
    assert_eq!(
        values
            .iter()
            .map(|value| conflict.resolve(*value))
            .collect::<Vec<_>>(),
        ["FR4", "PTFE", "laminate-42"]
    );
    // A designator name differing from material text is not itself a conflict.
    let consistent = design(&xml.replace(
        "matDes=\"laminate-42\"/>",
        "matDes=\"laminate-42\"><SpecRef id=\"bom-material\"/></StackupLayer>",
    ));
    assert!(matches!(
        consistent.physical_board_metadata().layers[0].material,
        Association::Resolved(_)
    ));

    // Identically named designators on another board or layer must neither
    // override this material nor create ambiguity. Unscoped BOMs still apply.
    for (header, layer) in [
        (
            r#"<BomHeader assembly="other" revision="1"><StepRef name="other-board"/></BomHeader>"#,
            "",
        ),
        ("", r#" layerRef="BOTTOM""#),
    ] {
        let foreign = format!(
            r#"<Bom name="foreign">{header}<BomItem OEMDesignNumberRef="other" quantity="1" category="MATERIAL"><MatDes name="laminate-42"{layer}/><SpecRef id="layer-material"/></BomItem></Bom>"#
        );
        let scoped = design(&xml.replace(
            "<Ecad name=\"test\">",
            &format!("{foreign}<Ecad name=\"test\">"),
        ));
        let metadata = scoped.physical_board_metadata();
        assert_eq!(
            scoped.resolve(*metadata.layers[0].material.resolved().unwrap()),
            "FR4"
        );
        assert!(metadata.diagnostics.is_empty());
    }
    let scoped = design(&xml.replace("<Bom name=\"materials\">", r#"<Bom name="materials"><BomHeader assembly="board" revision="1"><StepRef name="board"/></BomHeader>"#).replace("<MatDes name=\"laminate-42\"/>", "<MatDes name=\"laminate-42\" layerRef=\"TOP\"/>"));
    let metadata = scoped.physical_board_metadata();
    assert_eq!(
        scoped.resolve(*metadata.layers[0].material.resolved().unwrap()),
        "FR4"
    );
    assert!(metadata.diagnostics.is_empty());
}

#[test]
fn multiple_stackup_specs_preserve_provenance_and_reconcile_without_last_ref_wins() {
    let xml = fixture().replace("<CadHeader units=\"MILLIMETER\"/>", r#"<CadHeader units="MILLIMETER"><Spec name="a"><General type="MATERIAL"><Property text="FR4"/></General></Spec><Spec name="b"><General type="MATERIAL"><Property text="PTFE"/></General></Spec></CadHeader>"#);
    for refs in [["a", "missing"], ["missing", "a"], ["a", "b"], ["a", "a"]] {
        let children = refs
            .iter()
            .map(|reference| format!(r#"<SpecRef id="{reference}"/>"#))
            .collect::<String>();
        let imported = design(&xml.replace(
            "sequence=\"0\"/>",
            &format!("sequence=\"0\">{children}</StackupLayer>"),
        ));
        let source = &imported.stackups[0].layers[0];
        assert_eq!(
            source
                .spec_refs
                .iter()
                .map(|reference| imported.resolve(*reference))
                .collect::<Vec<_>>(),
            refs
        );
        assert!(source.spec_ref.is_none());
        assert!(source.material.is_none());
        assert!(source.dielectric_constant.is_none());
        assert!(source.loss_tangent.is_none());
        let metadata = imported.physical_board_metadata();
        let layer = &metadata.layers[0];
        assert_eq!(layer.spec_refs, source.spec_refs);
        assert!(layer.spec_ref.is_none());
        if refs.contains(&"b") {
            let Association::Conflicting(values) = &layer.material else {
                panic!("different resolved specs must conflict");
            };
            assert_eq!(
                values
                    .iter()
                    .map(|value| imported.resolve(*value))
                    .collect::<Vec<_>>(),
                ["FR4", "PTFE"]
            );
            assert!(
                metadata
                    .diagnostics
                    .contains(&BoardPhysicalDiagnostic::ConflictingMaterial {
                        layer: layer.layer_ref
                    })
            );
        } else {
            assert_eq!(imported.resolve(*layer.material.resolved().unwrap()), "FR4");
            let missing = metadata
                .diagnostics
                .iter()
                .filter_map(|diagnostic| match diagnostic {
                    BoardPhysicalDiagnostic::UnresolvedSpec { spec, .. } => {
                        Some(imported.resolve(*spec))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(
                missing,
                if refs.contains(&"missing") {
                    vec!["missing"]
                } else {
                    vec![]
                }
            );
        }
    }
}

#[test]
fn missing_stackup_does_not_infer_hole_land_links_from_layer_declarations() {
    let xml = fixture().replace("</Content>", r#"<DictionaryStandard units="MILLIMETER"><EntryStandard id="pad"><Circle diameter="3"/></EntryStandard></DictionaryStandard></Content>"#)
        .replace("<LayerFeature layerRef=\"TOP\">", r#"<LayerFeature layerRef="TOP"><Set><Pad padstackDefRef="P"><Location x="8" y="2"/><StandardPrimitiveRef id="pad"/></Pad></Set>"#);
    let mut imported = design(&xml);
    imported.stackups.clear();
    // Demonstrate the legacy API really has a declaration-order association
    // to suppress, rather than testing a board with no source lands.
    let strict = imported
        .physical_holes(ArtworkScope::Board, Resolution::default())
        .unwrap();
    assert!(!strict[0].lands.is_empty());
    let view = imported.physical_board(Resolution::default()).unwrap();
    assert!(
        view.metadata
            .diagnostics
            .contains(&BoardPhysicalDiagnostic::MissingStackup)
    );
    assert_eq!(view.holes[0].id, strict[0].id);
    assert_eq!(view.holes[0].span, strict[0].span);
    assert_eq!(view.holes[0].plating, strict[0].plating);
    assert_eq!(view.holes[0].image.area(), strict[0].image.area());
    assert!(view.holes[0].lands.is_empty());
    assert!(matches!(view.holes[0].termination, Association::Unresolved));
}
