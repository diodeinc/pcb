use super::*;
use crate::accessors::IpcAccessor;
use crate::manufacturing::build_manufacturing_package;
use pcb_ir::dialects::ipc::{
    FeatureBucket, FeatureDomain, FeatureIntent, FeatureKind, FeatureOperation, FeatureRole,
    FeatureSpan, FiducialKind, LayoutStepKind, PlatingKind, View,
};
use pcb_ir::geom::Point;

#[test]
fn parses_board_margin_css_shorthand() {
    let cases = [
        (&[1.0][..], BoardMarginMm::all(1.0)),
        (
            &[1.0, 2.0][..],
            BoardMarginMm {
                top: 1.0,
                right: 2.0,
                bottom: 1.0,
                left: 2.0,
            },
        ),
        (
            &[1.0, 2.0, 3.0][..],
            BoardMarginMm {
                top: 1.0,
                right: 2.0,
                bottom: 3.0,
                left: 2.0,
            },
        ),
        (
            &[1.0, 2.0, 3.0, 4.0][..],
            BoardMarginMm {
                top: 1.0,
                right: 2.0,
                bottom: 3.0,
                left: 4.0,
            },
        ),
    ];

    for (values, expected) in cases {
        assert_eq!(BoardMarginMm::from_css_shorthand(values).unwrap(), expected);
    }
    assert!(BoardMarginMm::from_css_shorthand(&[]).is_err());
    assert!(BoardMarginMm::from_css_shorthand(&[1.0, 2.0, 3.0, 4.0, 5.0]).is_err());
}

#[test]
fn creates_rounded_panel_step_from_board_bbox() {
    let xml = create_board_array_xml(
        board_fixture_mm(),
        &BoardArrayCreateOptions {
            columns: 6,
            rows: 6,
            board_margin_mm: board_margin(5.0, 5.0),
            edge_rail_mm: BoardMarginMm::all(5.0),
        },
    )
    .unwrap();

    assert!(xml.contains(r#"<StepRef name="array"/>"#));
    assert!(xml.contains(r#"<StepRef name="board_cell"/>"#));
    assert!(xml.contains(r#"<StepRef name="board"/>"#));
    assert!(xml.contains(r#"<LayerRef name="V-Score"/>"#));
    assert!(xml.contains(
        r#"<Layer name="V-Score" layerFunction="V_CUT" side="NONE" polarity="POSITIVE"/>"#
    ));
    assert!(xml.contains(r#"<Step name="array" type="PALLET">"#));
    assert!(xml.contains(
        r#"<NonstandardAttribute name="diode.panelize.schema_version" type="INTEGER" value="1"/>"#
    ));
    assert!(xml.contains(
        r#"<NonstandardAttribute name="diode.panelize.mode" type="STRING" value="manual"/>"#
    ));
    assert!(xml.contains(
        r#"<NonstandardAttribute name="diode.panelize.columns" type="INTEGER" value="6"/>"#
    ));
    assert!(xml.contains(
        r#"<NonstandardAttribute name="diode.panelize.rows" type="INTEGER" value="6"/>"#
    ));
    assert!(xml.contains(
        r#"<NonstandardAttribute name="diode.panelize.board_margin_top_mm" type="DOUBLE" value="2.5"/>"#
    ));
    assert!(xml.contains(
        r#"<NonstandardAttribute name="diode.panelize.edge_rail_left_mm" type="DOUBLE" value="5"/>"#
    ));
    assert!(xml.contains(r#"<Step name="board_cell" type="PALLET">"#));
    assert!(xml.contains(
        r#"<StepRepeat stepRef="board_cell" x="5" y="5" nx="6" ny="6" dx="15" dy="15" angle="0.00" mirror="false"/>"#
    ));
    assert!(xml.contains(
        r#"<StepRepeat stepRef="board" x="4.5" y="5.5" nx="1" ny="1" dx="0" dy="0" angle="0.00" mirror="false"/>"#
    ));
    assert!(xml.contains(r#"<LayerFeature layerRef="V-Score">"#));
    assert!(xml.contains(r#"<Spec name="Board_Array_VCut">"#));
    assert!(xml.contains(r#"<SpecRef id="Board_Array_VCut"/>"#));
    assert!(
        xml.contains(r#"<PolyStepCurve x="3" y="100" centerX="3" centerY="97" clockwise="true"/>"#)
    );
    assert!(xml.contains(r#"<Line startX="7.5" startY="0" endX="7.5" endY="100">"#));
    assert!(xml.contains(r#"<Line startX="0" startY="7.5" endX="100" endY="7.5">"#));

    let ipc = Ipc2581::parse(&xml).unwrap();
    let layout = geometry::extract_layout(&ipc).unwrap();
    let (_, panel_step) = pcb_ir::dialects::ipc::root_panel_step(&layout).unwrap();
    assert_point_close(panel_step.bbox.min, Point::new(0.0, 0.0));
    assert_point_close(panel_step.bbox.max, Point::new(100.0, 100.0));
    assert_eq!(pcb_ir::dialects::ipc::board_step_count(&layout), 1);
    assert_eq!(pcb_ir::dialects::ipc::board_instance_count(&layout), 36);

    let first_instance = layout
        .layout
        .instances
        .iter()
        .find(|instance| {
            layout.layout.steps[instance.child_step as usize].kind == LayoutStepKind::Board
        })
        .unwrap();
    assert_point_close(first_instance.bbox.min, Point::new(7.5, 7.5));
    assert_point_close(first_instance.bbox.max, Point::new(17.5, 17.5));

    let vcut = geometry::extract_layer_for_view(&ipc, "V-Score", View::ArrayFlattened).unwrap();
    assert!(vcut.features.len() > 24);
    assert!(
        vcut.features
            .iter()
            .all(|feature| feature.intent.domain == FeatureDomain::VCut)
    );
    assert_eq!(geometry::board_array_vscore_lines(&ipc).unwrap().len(), 24);
}

#[test]
fn board_array_creation_adds_history_record() {
    let xml = create_board_array_xml(
        board_fixture_mm(),
        &BoardArrayCreateOptions {
            columns: 6,
            rows: 6,
            board_margin_mm: board_margin(5.0, 5.0),
            edge_rail_mm: BoardMarginMm::all(5.0),
        },
    )
    .unwrap();

    let ipc = Ipc2581::parse(&xml).unwrap();
    assert!(ipc.history_record().is_some());
    assert!(xml.contains(r#"<HistoryRecord number="1""#));
    assert!(xml.contains("Created board array"));
}

#[test]
fn auto_create_projects_board_to_a7_array() {
    let xml = create_auto_board_array_xml(board_fixture_mm()).unwrap();

    assert!(xml.contains(
        r#"<StepRepeat stepRef="board_cell" x="12.5" y="7" nx="4" ny="3" dx="20" dy="20" angle="0.00" mirror="false"/>"#
    ));
    assert!(xml.contains(
        r#"<NonstandardAttribute name="diode.panelize.mode" type="STRING" value="auto"/>"#
    ));
    assert!(xml.contains(
        r#"<NonstandardAttribute name="diode.panelize.sheet" type="STRING" value="A7"/>"#
    ));
    assert!(xml.contains(
        r#"<NonstandardAttribute name="diode.panelize.sheet_width_mm" type="DOUBLE" value="105"/>"#
    ));
    assert!(xml.contains(
        r#"<NonstandardAttribute name="diode.panelize.sheet_height_mm" type="DOUBLE" value="74"/>"#
    ));
    assert!(xml.contains(
        r#"<NonstandardAttribute name="diode.panelize.edge_rail_left_mm" type="DOUBLE" value="12.5"/>"#
    ));
    assert!(xml.contains(
        r#"<NonstandardAttribute name="diode.panelize.edge_rail_top_mm" type="DOUBLE" value="7"/>"#
    ));

    let ipc = Ipc2581::parse(&xml).unwrap();
    let layout = geometry::extract_layout(&ipc).unwrap();
    let (_, panel_step) = pcb_ir::dialects::ipc::root_panel_step(&layout).unwrap();
    assert_point_close(panel_step.bbox.min, Point::new(0.0, 0.0));
    assert_point_close(panel_step.bbox.max, Point::new(105.0, 74.0));
    assert_eq!(pcb_ir::dialects::ipc::board_instance_count(&layout), 12);
}

#[test]
fn auto_create_projects_board_to_requested_a5_array() {
    let xml = create_auto_board_array_xml_with_sheet(board_fixture_mm(), Some(AutoSheetSize::A5))
        .unwrap();

    assert!(xml.contains(
        r#"<StepRepeat stepRef="board_cell" x="5" y="14" nx="10" ny="6" dx="20" dy="20" angle="0.00" mirror="false"/>"#
    ));
    assert!(xml.contains(
        r#"<NonstandardAttribute name="diode.panelize.mode" type="STRING" value="auto_sheet"/>"#
    ));
    assert!(xml.contains(
        r#"<NonstandardAttribute name="diode.panelize.sheet" type="STRING" value="A5"/>"#
    ));
    assert!(xml.contains(
        r#"<NonstandardAttribute name="diode.panelize.sheet_width_mm" type="DOUBLE" value="210"/>"#
    ));
    assert!(xml.contains(
        r#"<NonstandardAttribute name="diode.panelize.sheet_height_mm" type="DOUBLE" value="148"/>"#
    ));

    let ipc = Ipc2581::parse(&xml).unwrap();
    let layout = geometry::extract_layout(&ipc).unwrap();
    let (_, panel_step) = pcb_ir::dialects::ipc::root_panel_step(&layout).unwrap();
    assert_point_close(panel_step.bbox.min, Point::new(0.0, 0.0));
    assert_point_close(panel_step.bbox.max, Point::new(210.0, 148.0));
    assert_eq!(pcb_ir::dialects::ipc::board_instance_count(&layout), 60);
}

#[test]
fn auto_create_derives_board_margin_from_courtyard_overhang() {
    let input = board_fixture_with_courtyard_overhang_mm();
    let ipc = Ipc2581::parse(input).unwrap();
    let board = primary_board_layout(&ipc).unwrap();
    let margin = auto_board_margin(&ipc, board.bbox).unwrap();

    assert_eq!(
        margin,
        BoardMarginMm {
            top: 7.0,
            right: 8.0,
            bottom: 6.0,
            left: 7.0,
        }
    );

    let xml = create_auto_board_array_xml(input).unwrap();
    assert!(xml.contains(
        r#"<StepRepeat stepRef="board_cell" x="12" y="6.5" nx="2" ny="4" dx="25" dy="23" angle="0.00" mirror="false"/>"#
    ));
    assert!(xml.contains(
        r#"<StepRepeat stepRef="board" x="7" y="6" nx="1" ny="1" dx="0" dy="0" angle="0.00" mirror="false"/>"#
    ));
}

#[test]
fn auto_create_allows_large_computed_board_margins() {
    let input = board_fixture_with_large_courtyard_overhang_mm();
    let ipc = Ipc2581::parse(input).unwrap();
    let board = primary_board_layout(&ipc).unwrap();
    let margin = auto_board_margin(&ipc, board.bbox).unwrap();

    assert_eq!(
        margin,
        BoardMarginMm {
            top: 5.0,
            right: 26.0,
            bottom: 5.0,
            left: 5.0,
        }
    );

    let xml = create_auto_board_array_xml(input).unwrap();
    let ipc = Ipc2581::parse(&xml).unwrap();
    let layout = geometry::extract_layout(&ipc).unwrap();
    assert_eq!(pcb_ir::dialects::ipc::board_instance_count(&layout), 6);
}

#[test]
fn auto_create_allows_large_leftover_edge_rails() {
    let xml = create_auto_board_array_xml(&board_fixture_with_mask_bbox_mm(124.0, 110.0)).unwrap();

    assert!(xml.contains(
        r#"<StepRepeat stepRef="board_cell" x="38" y="14" nx="1" ny="1" dx="134" dy="120" angle="0.00" mirror="false"/>"#
    ));

    let ipc = Ipc2581::parse(&xml).unwrap();
    let layout = geometry::extract_layout(&ipc).unwrap();
    let (_, panel_step) = pcb_ir::dialects::ipc::root_panel_step(&layout).unwrap();
    assert_point_close(panel_step.bbox.min, Point::new(0.0, 0.0));
    assert_point_close(panel_step.bbox.max, Point::new(210.0, 148.0));
    assert_eq!(pcb_ir::dialects::ipc::board_instance_count(&layout), 1);
}

#[test]
fn auto_create_falls_back_to_minimum_single_board_panel_when_a4_does_not_fit() {
    let xml = create_auto_board_array_xml(&board_fixture_with_mask_bbox_mm(278.0, 10.0)).unwrap();

    assert!(xml.contains(
        r#"<StepRepeat stepRef="board_cell" x="5" y="5" nx="1" ny="1" dx="288" dy="20" angle="0.00" mirror="false"/>"#
    ));
    assert!(xml.contains(
        r#"<StepRepeat stepRef="board" x="5" y="5" nx="1" ny="1" dx="0" dy="0" angle="0.00" mirror="false"/>"#
    ));
    assert!(xml.contains(
        r#"<NonstandardAttribute name="diode.panelize.mode" type="STRING" value="auto_minimum_panel"/>"#
    ));

    let ipc = Ipc2581::parse(&xml).unwrap();
    let layout = geometry::extract_layout(&ipc).unwrap();
    let (_, panel_step) = pcb_ir::dialects::ipc::root_panel_step(&layout).unwrap();
    assert_point_close(panel_step.bbox.min, Point::new(0.0, 0.0));
    assert_point_close(panel_step.bbox.max, Point::new(298.0, 30.0));
    assert_eq!(pcb_ir::dialects::ipc::board_instance_count(&layout), 1);
}

#[test]
fn auto_create_requested_sheet_still_errors_when_sheet_does_not_fit() {
    let error = create_auto_board_array_xml_with_sheet(
        &board_fixture_with_mask_bbox_mm(278.0, 278.0),
        Some(AutoSheetSize::A4),
    )
    .unwrap_err();

    assert!(error.to_string().contains("cannot fit in A4"));
}

#[test]
fn creates_board_array_with_asymmetric_edge_rails() {
    let xml = create_board_array_xml(
        board_fixture_mm(),
        &BoardArrayCreateOptions {
            columns: 6,
            rows: 6,
            board_margin_mm: board_margin(5.0, 5.0),
            edge_rail_mm: BoardMarginMm {
                top: 8.0,
                right: 6.0,
                bottom: 5.0,
                left: 7.0,
            },
        },
    )
    .unwrap();

    assert!(xml.contains(
        r#"<StepRepeat stepRef="board_cell" x="7" y="5" nx="6" ny="6" dx="15" dy="15" angle="0.00" mirror="false"/>"#
    ));

    let ipc = Ipc2581::parse(&xml).unwrap();
    let layout = geometry::extract_layout(&ipc).unwrap();
    let (_, panel_step) = pcb_ir::dialects::ipc::root_panel_step(&layout).unwrap();
    assert_point_close(panel_step.bbox.max, Point::new(103.0, 103.0));
}

#[test]
fn created_board_array_vcuts_flow_to_svg_and_gerber() {
    let xml = create_board_array_xml(
        board_fixture_mm(),
        &BoardArrayCreateOptions {
            columns: 6,
            rows: 6,
            board_margin_mm: board_margin(5.0, 5.0),
            edge_rail_mm: BoardMarginMm::all(5.0),
        },
    )
    .unwrap();
    let ipc = Ipc2581::parse(&xml).unwrap();
    let accessor = IpcAccessor::new(&ipc);

    let svg = crate::board_array::render_board_array_overview_svg(&accessor)
        .unwrap()
        .unwrap();
    assert!(svg.matches("vcut-guide").count() > 24);
    assert!(svg.contains("stroke='#dc2626'"));
    assert!(svg.contains("stroke-width='0.12'"));
    assert!(svg.contains("stroke-linecap='round'"));
    assert!(!svg.contains("stroke-dasharray"));
    assert!(!svg.contains("class='score-guide'"));
    let viewbox = svg_viewbox(&svg);
    assert!(viewbox.0 + viewbox.2 > 100.0);
    assert!(viewbox.1 + viewbox.3 > 100.0);
    assert_eq!(geometry::board_array_vscore_lines(&ipc).unwrap().len(), 24);

    let package = build_manufacturing_package(&ipc, View::ArrayFlattened).unwrap();

    let vcut = package
        .files
        .iter()
        .find(|file| file.filename == "V_Cut.gbr")
        .unwrap();
    assert!(vcut.contents.contains("%TF.FileFunction,Vcut,Top/Bot*%"));
    assert!(vcut.contents.contains("%TF.Part,Array*%"));
    assert!(vcut.contents.contains("%TA.AperFunction,Other,Vcut*%"));
    assert!(!vcut.contents.contains("G36*"));
    assert!(vcut.contents.matches("D01*").count() > 24);

    let board_package = build_manufacturing_package(&ipc, View::Board).unwrap();
    assert!(
        board_package
            .files
            .iter()
            .all(|file| file.filename != "V_Cut.gbr")
    );
}

#[test]
fn created_board_array_profile_gerber_derives_vscore_reliefs() {
    let xml = create_board_array_xml(
        rounded_corner_board_fixture_mm(),
        &BoardArrayCreateOptions {
            columns: 6,
            rows: 6,
            board_margin_mm: board_margin(5.0, 5.0),
            edge_rail_mm: BoardMarginMm::all(5.0),
        },
    )
    .unwrap();

    assert!(!xml.contains("<SlotCavity"));

    let ipc = Ipc2581::parse(&xml).unwrap();
    let package = build_manufacturing_package(&ipc, View::ArrayFlattened).unwrap();
    let vcut = package
        .files
        .iter()
        .find(|file| file.filename == "V_Cut.gbr")
        .unwrap();
    assert!(!vcut.contents.contains("G36*"));
    assert!(
        package
            .files
            .iter()
            .all(|file| file.filename != "Edge_Cuts.gm1")
    );
    let profile = package
        .files
        .iter()
        .find(|file| file.filename == "Board_Array_Profile.gm1")
        .unwrap();
    assert!(profile.contents.contains("%TF.FileFunction,Profile,NP*%"));
    assert!(profile.contents.contains("%TF.Part,Array*%"));
    assert!(profile.contents.contains("%TA.AperFunction,Profile*%"));
    assert!(profile.contents.contains("%ADD10C,0.05*%"));
    assert!(!profile.contents.contains("%ADD11C,1*%"));
    assert!(!profile.contents.contains("G36*"));
    assert!(
        profile.contents.matches("D01*").count()
            > geometry::board_array_vscore_lines(&ipc).unwrap().len(),
        "routed reliefs should emit closed contour strokes, not only the V-cut guide lines"
    );
    gerberx2::GerberX2::parse(&profile.contents).unwrap();
}

#[test]
fn board_array_creation_drops_source_board_outline_layer_features() {
    let xml = create_board_array_xml(
        board_fixture_with_edge_cuts_layer_mm(),
        &BoardArrayCreateOptions {
            columns: 2,
            rows: 2,
            board_margin_mm: board_margin(5.0, 5.0),
            edge_rail_mm: BoardMarginMm::all(5.0),
        },
    )
    .unwrap();

    assert!(xml.contains(r#"<LayerFeature layerRef="TOP">"#));
    assert!(!xml.contains(r#"<LayerRef name="Edge.Cuts""#));
    assert!(!xml.contains(r#"<Layer name="Edge.Cuts""#));
    assert!(!xml.contains(r#"<LayerFeature layerRef="Edge.Cuts">"#));

    let ipc = Ipc2581::parse(&xml).unwrap();
    let package = build_manufacturing_package(&ipc, View::ArrayFlattened).unwrap();
    assert!(
        package
            .files
            .iter()
            .all(|file| file.filename != "Edge_Cuts.gm1")
    );
    assert!(
        package
            .files
            .iter()
            .any(|file| file.filename == "Board_Array_Profile.gm1")
    );
}

#[test]
fn board_array_creation_preserves_board_target_geometry() {
    let input = board_fixture_with_top_line_mm();
    let before_ipc = Ipc2581::parse(input).unwrap();
    let before = geometry::extract_layer_for_view(&before_ipc, "TOP", View::Board).unwrap();

    let xml = create_board_array_xml(
        input,
        &BoardArrayCreateOptions {
            columns: 6,
            rows: 6,
            board_margin_mm: board_margin(5.0, 5.0),
            edge_rail_mm: BoardMarginMm::all(5.0),
        },
    )
    .unwrap();
    let after_ipc = Ipc2581::parse(&xml).unwrap();
    let after = geometry::extract_layer_for_view(&after_ipc, "TOP", View::Board).unwrap();

    assert_eq!(before.features.len(), after.features.len());
    assert_eq!(before.arena.paths.len(), after.arena.paths.len());
    assert_eq!(before.arena.contours.len(), after.arena.contours.len());
    assert_eq!(before.arena.cmds, after.arena.cmds);

    for (before_feature, after_feature) in before.features.iter().zip(&after.features) {
        assert_eq!(before_feature.kind, after_feature.kind);
        assert_eq!(before_feature.bucket, after_feature.bucket);
        assert_eq!(before_feature.polarity, after_feature.polarity);
        assert_intent_eq(
            &before_ipc,
            &after_ipc,
            &before_feature.intent,
            &after_feature.intent,
        );
        assert_eq!(before_feature.fiducial_kind, after_feature.fiducial_kind);
        assert_eq!(before_feature.bbox, after_feature.bbox);
        assert_eq!(before_feature.paths.count, after_feature.paths.count);
    }
}

#[test]
fn generated_array_geometry_writes_fiducials_and_nonplated_holes() {
    let input = board_fixture_with_mask_mm();
    let ipc = Ipc2581::parse(input).unwrap();
    let options = BoardArrayCreateOptions {
        columns: 6,
        rows: 6,
        board_margin_mm: board_margin(5.0, 5.0),
        edge_rail_mm: BoardMarginMm::all(5.0),
    };
    let mut spec = build_board_array_spec(
        &ipc,
        &options,
        BoardArrayValidationMode::Manual,
        BoardArrayPanelizationMetadata {
            mode: BoardArrayPanelizationMode::Manual,
            sheet: None,
            sheet_target_mm: None,
        },
    )
    .unwrap();

    spec.generated_geometry.add_layer_feature(
        GeneratedFeatureScope::Array,
        "TOP",
        Polarity::Positive,
        round_fiducial_features(IpcFiducialKind::Global, [(12.5, 12.5)], 1.0),
    );
    spec.generated_geometry.add_layer_feature(
        GeneratedFeatureScope::Array,
        "F.Mask",
        Polarity::Positive,
        round_fiducial_features(IpcFiducialKind::Global, [(12.5, 12.5)], 2.0),
    );
    spec.generated_geometry.add_layer(GeneratedLayer::new(
        "Array_Drill",
        LayerFunction::Drill,
        Some(Side::All),
        Some(Polarity::Positive),
    ));
    spec.generated_geometry.add_layer_feature(
        GeneratedFeatureScope::Array,
        "Array_Drill",
        Polarity::Positive,
        round_nonplated_hole_features([(20.0, 20.0)], 2.0),
    );
    spec.content_layer_refs = content_layer_refs(
        &ipc,
        &spec.generated_geometry,
        &spec.board_outline_layer_names,
    );

    let xml = write_board_array_xml(input, &spec).unwrap();

    assert!(xml.contains(r#"<LayerRef name="F.Mask"/>"#));
    assert!(xml.contains(r#"<LayerRef name="Array_Drill"/>"#));
    assert!(xml.contains(
        r#"<Layer name="Array_Drill" layerFunction="DRILL" side="ALL" polarity="POSITIVE"/>"#
    ));
    assert_eq!(xml.matches("<GlobalFiducial>").count(), 2);
    assert!(xml.contains(r#"<Circle diameter="1"/>"#));
    assert!(xml.contains(r#"<Circle diameter="2"/>"#));
    assert!(xml.contains(r#"diameter="2" platingStatus="NONPLATED""#));
    assert!(xml.contains(r#"x="20" y="20""#));

    let parsed = Ipc2581::parse(&xml).unwrap();
    let top = geometry::extract_layer_for_view(&parsed, "TOP", View::ArrayFlattened).unwrap();
    assert!(top.features.iter().any(|feature| {
        feature.intent.role == FeatureRole::Fiducial
            && feature.fiducial_kind == FiducialKind::Global
    }));

    let drill =
        geometry::extract_layer_for_view(&parsed, "Array_Drill", View::ArrayFlattened).unwrap();
    assert_eq!(drill.features.len(), 1);
    assert_eq!(drill.features[0].kind, FeatureKind::Hole);
    assert_eq!(drill.features[0].bucket, FeatureBucket::Cutout);
    assert_eq!(drill.features[0].intent.domain, FeatureDomain::Drill);
    assert_eq!(drill.features[0].intent.role, FeatureRole::Hole);
    assert_eq!(drill.features[0].intent.operation, FeatureOperation::Drill);
    assert_eq!(drill.features[0].intent.plating, PlatingKind::NonPlated);

    let package = build_manufacturing_package(&parsed, View::ArrayFlattened).unwrap();
    let top = package
        .files
        .iter()
        .find(|file| file.filename == "F_Cu.gtl")
        .unwrap();
    let mask = package
        .files
        .iter()
        .find(|file| file.filename == "F_Mask.gts")
        .unwrap();
    let drill = package
        .files
        .iter()
        .find(|file| file.filename == "NPTH.drl")
        .unwrap();

    assert!(
        top.contents
            .contains("%TA.AperFunction,FiducialPad,Global*%")
    );
    assert!(
        mask.contents
            .contains("%TA.AperFunction,FiducialPad,Global*%")
    );
    assert!(drill.contents.contains("; #@! TF.FileFunction,NonPlated"));
    assert!(
        drill
            .contents
            .contains("; #@! TA.AperFunction,NonPlated,NPTH,ComponentDrill")
    );
    assert!(drill.contents.contains("X20.0Y20.0"));
    assert!(!top.contents.contains("%TA.AperFunction,Other,Drill*%"));
    assert!(!mask.contents.contains("%TA.AperFunction,Other,Drill*%"));
}

#[test]
fn board_array_creation_adds_default_tooling_at_single_column_min_width() {
    let input = board_fixture_with_mask_bbox_mm(28.0, 40.0);
    let xml = create_board_array_xml(
        &input,
        &BoardArrayCreateOptions {
            columns: 1,
            rows: 1,
            board_margin_mm: board_margin(5.0, 0.0),
            edge_rail_mm: edge_rail(18.5, 15.0),
        },
    )
    .unwrap();

    let ipc = Ipc2581::parse(&xml).unwrap();
    let step = array_step(&ipc);
    let top_fiducials = fiducials_on_layer(&ipc, step, "TOP");
    let mask_fiducials = fiducials_on_layer(&ipc, step, "F.Mask");
    let tooling_holes = holes_on_layer(&ipc, step, TOOLING_HOLE_LAYER_BASE_NAME);
    let corner_holes = holes_with_diameter(&tooling_holes, CORNER_TOOLING_HOLE_DIAMETER_MM);
    let rail_holes = holes_with_diameter(&tooling_holes, TOOLING_HOLE_DIAMETER_MM);

    assert_eq!(top_fiducials.len(), 4);
    assert_eq!(mask_fiducials.len(), 4);
    assert_eq!(corner_holes.len(), 4);
    assert_eq!(rail_holes.len(), 4);
    assert!(
        top_fiducials
            .iter()
            .all(|fiducial| close(fiducial_diameter(fiducial), 1.0))
    );
    assert!(
        mask_fiducials
            .iter()
            .all(|fiducial| close(fiducial_diameter(fiducial), 2.0))
    );
    assert!(
        tooling_holes
            .iter()
            .all(|hole| hole.plating_status == PlatingStatus::NonPlated)
    );
    assert_corner_holes(&corner_holes, 70.0, 70.0);
    assert_points_close(
        fiducial_points(&top_fiducials),
        vec![(29.0, 66.15), (41.0, 66.15), (33.0, 3.85), (37.0, 3.85)],
    );
    assert_points_close(
        hole_points(&rail_holes),
        vec![(23.5, 67.5), (46.5, 67.5), (27.5, 2.5), (42.5, 2.5)],
    );
}

#[test]
fn board_array_creation_adds_default_tooling_at_multi_column_min_width() {
    let input = board_fixture_with_mask_bbox_mm(12.0, 40.0);
    let xml = create_board_array_xml(
        &input,
        &BoardArrayCreateOptions {
            columns: 2,
            rows: 1,
            board_margin_mm: board_margin(5.0, 0.0),
            edge_rail_mm: edge_rail(18.0, 15.0),
        },
    )
    .unwrap();

    let ipc = Ipc2581::parse(&xml).unwrap();
    let step = array_step(&ipc);
    let top_fiducials = fiducials_on_layer(&ipc, step, "TOP");
    let mask_fiducials = fiducials_on_layer(&ipc, step, "F.Mask");
    let tooling_holes = holes_on_layer(&ipc, step, TOOLING_HOLE_LAYER_BASE_NAME);
    let corner_holes = holes_with_diameter(&tooling_holes, CORNER_TOOLING_HOLE_DIAMETER_MM);
    let rail_holes = holes_with_diameter(&tooling_holes, TOOLING_HOLE_DIAMETER_MM);

    assert_eq!(top_fiducials.len(), 4);
    assert_eq!(mask_fiducials.len(), 4);
    assert_eq!(corner_holes.len(), 4);
    assert_eq!(rail_holes.len(), 4);
    assert_corner_holes(&corner_holes, 70.0, 70.0);
    assert_points_close(
        fiducial_points(&top_fiducials),
        vec![(28.5, 66.15), (41.5, 66.15), (32.5, 3.85), (37.5, 3.85)],
    );
    assert_points_close(
        hole_points(&rail_holes),
        vec![(23.0, 67.5), (47.0, 67.5), (27.0, 2.5), (43.0, 2.5)],
    );
}

#[test]
fn board_array_creation_places_array_tooling_on_left_right_for_landscape_arrays() {
    let input = board_fixture_with_mask_bbox_mm(40.0, 28.0);
    let xml = create_board_array_xml(
        &input,
        &BoardArrayCreateOptions {
            columns: 1,
            rows: 1,
            board_margin_mm: board_margin(5.0, 0.0),
            edge_rail_mm: edge_rail(15.0, 21.0),
        },
    )
    .unwrap();

    let ipc = Ipc2581::parse(&xml).unwrap();
    let step = array_step(&ipc);
    let top_fiducials = fiducials_on_layer(&ipc, step, "TOP");
    let tooling_holes = holes_on_layer(&ipc, step, TOOLING_HOLE_LAYER_BASE_NAME);
    let corner_holes = holes_with_diameter(&tooling_holes, CORNER_TOOLING_HOLE_DIAMETER_MM);
    let rail_holes = holes_with_diameter(&tooling_holes, TOOLING_HOLE_DIAMETER_MM);

    assert_eq!(top_fiducials.len(), 4);
    assert_eq!(corner_holes.len(), 4);
    assert_eq!(rail_holes.len(), 4);
    assert_corner_holes(&corner_holes, 75.0, 70.0);
    assert_points_close(
        fiducial_points(&top_fiducials),
        vec![(3.85, 41.0), (3.85, 29.0), (71.15, 37.0), (71.15, 33.0)],
    );
    assert_points_close(
        hole_points(&rail_holes),
        vec![(2.5, 46.5), (2.5, 23.5), (72.5, 42.5), (72.5, 27.5)],
    );
}

#[test]
fn board_array_creation_skips_default_tooling_when_board_width_is_too_small() {
    let input = board_fixture_with_mask_bbox_mm(11.99, 40.0);
    let xml = create_board_array_xml(
        &input,
        &BoardArrayCreateOptions {
            columns: 2,
            rows: 1,
            board_margin_mm: board_margin(5.0, 0.0),
            edge_rail_mm: edge_rail(18.5, 20.0),
        },
    )
    .unwrap();

    let ipc = Ipc2581::parse(&xml).unwrap();
    let ecad = ipc.ecad().unwrap();
    assert!(
        ecad.cad_data
            .layers
            .iter()
            .any(|layer| ipc.resolve(layer.name) == TOOLING_HOLE_LAYER_BASE_NAME)
    );

    let step = array_step(&ipc);
    let tooling_holes = holes_on_layer(&ipc, step, TOOLING_HOLE_LAYER_BASE_NAME);
    let fiducial_count = step
        .layer_features
        .iter()
        .flat_map(|layer_feature| &layer_feature.sets)
        .flat_map(|set| set.fiducials())
        .count();
    let hole_count = step
        .layer_features
        .iter()
        .flat_map(|layer_feature| &layer_feature.sets)
        .flat_map(|set| set.holes())
        .count();

    assert_eq!(fiducial_count, 0);
    assert_eq!(hole_count, 4);
    assert!(
        tooling_holes
            .iter()
            .all(|hole| close(hole.diameter, CORNER_TOOLING_HOLE_DIAMETER_MM))
    );
    assert_corner_holes(&tooling_holes, 70.98, 80.0);
}

#[test]
fn board_array_creation_adds_board_cell_fiducials_on_top_bottom_margins() {
    let input = board_fixture_with_mask_bbox_mm(40.0, 30.0);
    let xml = create_board_array_xml(
        &input,
        &BoardArrayCreateOptions {
            columns: 2,
            rows: 1,
            board_margin_mm: BoardMarginMm {
                top: 5.0,
                right: 0.0,
                bottom: 5.0,
                left: 0.0,
            },
            edge_rail_mm: BoardMarginMm::all(15.0),
        },
    )
    .unwrap();

    let ipc = Ipc2581::parse(&xml).unwrap();
    let cell = board_cell_step(&ipc);
    let top_fiducials = fiducials_on_layer(&ipc, cell, "TOP");
    let mask_fiducials = fiducials_on_layer(&ipc, cell, "F.Mask");

    assert_eq!(top_fiducials.len(), 4);
    assert_eq!(mask_fiducials.len(), 4);
    assert!(
        top_fiducials
            .iter()
            .all(|fiducial| fiducial.kind == IpcFiducialKind::Local)
    );
    assert!(
        top_fiducials
            .iter()
            .all(|fiducial| close(fiducial_diameter(fiducial), 1.0))
    );
    assert!(
        mask_fiducials
            .iter()
            .all(|fiducial| close(fiducial_diameter(fiducial), 2.0))
    );
    assert_points_close(
        fiducial_points(&top_fiducials),
        vec![(3.0, 38.0), (37.0, 38.0), (7.0, 2.0), (33.0, 2.0)],
    );

    let top = geometry::extract_layer_for_view(&ipc, "TOP", View::ArrayFlattened).unwrap();
    assert_eq!(
        top.features
            .iter()
            .filter(|feature| feature.fiducial_kind == FiducialKind::Local)
            .count(),
        8
    );

    let package = build_manufacturing_package(&ipc, View::ArrayFlattened).unwrap();
    let top = package
        .files
        .iter()
        .find(|file| file.filename == "F_Cu.gtl")
        .unwrap();
    assert!(
        top.contents
            .contains("%TA.AperFunction,FiducialPad,Local*%")
    );
}

#[test]
fn board_array_creation_adds_board_cell_fiducials_on_left_right_margins() {
    let input = board_fixture_with_mask_bbox_mm(30.0, 40.0);
    let xml = create_board_array_xml(
        &input,
        &BoardArrayCreateOptions {
            columns: 1,
            rows: 2,
            board_margin_mm: BoardMarginMm {
                top: 0.0,
                right: 5.0,
                bottom: 0.0,
                left: 5.0,
            },
            edge_rail_mm: BoardMarginMm::all(15.0),
        },
    )
    .unwrap();

    let ipc = Ipc2581::parse(&xml).unwrap();
    let top_fiducials = fiducials_on_layer(&ipc, board_cell_step(&ipc), "TOP");

    assert_eq!(top_fiducials.len(), 4);
    assert_points_close(
        fiducial_points(&top_fiducials),
        vec![(2.0, 37.0), (2.0, 3.0), (38.0, 33.0), (38.0, 7.0)],
    );
}

#[test]
fn board_array_creation_adds_board_cell_fiducials_when_single_board_array_is_eligible() {
    let input = board_fixture_with_mask_bbox_mm(40.0, 30.0);
    let xml = create_board_array_xml(
        &input,
        &BoardArrayCreateOptions {
            columns: 1,
            rows: 1,
            board_margin_mm: BoardMarginMm {
                top: 5.0,
                right: 5.0,
                bottom: 5.0,
                left: 5.0,
            },
            edge_rail_mm: BoardMarginMm::all(15.0),
        },
    )
    .unwrap();

    let ipc = Ipc2581::parse(&xml).unwrap();
    let top_fiducials = fiducials_on_layer(&ipc, board_cell_step(&ipc), "TOP");
    let mask_fiducials = fiducials_on_layer(&ipc, board_cell_step(&ipc), "F.Mask");

    assert_eq!(top_fiducials.len(), 4);
    assert_eq!(mask_fiducials.len(), 4);
    assert_points_close(
        fiducial_points(&top_fiducials),
        vec![(8.0, 38.0), (42.0, 38.0), (12.0, 2.0), (38.0, 2.0)],
    );
    assert_eq!(fiducials_on_layer(&ipc, array_step(&ipc), "TOP").len(), 4);
}

#[test]
fn board_array_creation_skips_board_cell_fiducials_without_eligible_margin() {
    let input = board_fixture_with_mask_bbox_mm(40.0, 35.0);
    let xml = create_board_array_xml(
        &input,
        &BoardArrayCreateOptions {
            columns: 2,
            rows: 1,
            board_margin_mm: BoardMarginMm {
                top: 4.99,
                right: 0.0,
                bottom: 4.99,
                left: 0.0,
            },
            edge_rail_mm: BoardMarginMm::all(15.0),
        },
    )
    .unwrap();

    let ipc = Ipc2581::parse(&xml).unwrap();
    assert!(fiducials_on_layer(&ipc, board_cell_step(&ipc), "TOP").is_empty());
    assert!(fiducials_on_layer(&ipc, board_cell_step(&ipc), "F.Mask").is_empty());
}

#[test]
fn board_array_creation_skips_board_cell_fiducials_without_eligible_span() {
    let input = board_fixture_with_mask_bbox_mm(16.99, 16.99);
    let xml = create_board_array_xml(
        &input,
        &BoardArrayCreateOptions {
            columns: 2,
            rows: 1,
            board_margin_mm: BoardMarginMm {
                top: 5.0,
                right: 5.0,
                bottom: 5.0,
                left: 5.0,
            },
            edge_rail_mm: BoardMarginMm::all(30.0),
        },
    )
    .unwrap();

    let ipc = Ipc2581::parse(&xml).unwrap();
    assert!(fiducials_on_layer(&ipc, board_cell_step(&ipc), "TOP").is_empty());
    assert!(fiducials_on_layer(&ipc, board_cell_step(&ipc), "F.Mask").is_empty());
}

#[test]
fn writes_generated_board_array_values_in_cad_header_units() {
    let xml = create_board_array_xml(
        board_fixture_inch(),
        &BoardArrayCreateOptions {
            columns: 1,
            rows: 1,
            board_margin_mm: board_margin(0.0, 0.0),
            edge_rail_mm: BoardMarginMm::all(25.4),
        },
    )
    .unwrap();

    assert!(xml.contains(r#"<PolyStepSegment x="0" y="2.88189"/>"#));
    assert!(xml.contains(
        r#"<PolyStepCurve x="0.11811" y="3" centerX="0.11811" centerY="2.88189" clockwise="true"/>"#
    ));
    assert!(xml.contains(
        r#"<StepRepeat stepRef="board_cell" x="1" y="1" nx="1" ny="1" dx="1" dy="1" angle="0.00" mirror="false"/>"#
    ));
    assert!(xml.contains(
        r#"<StepRepeat stepRef="board" x="0" y="0" nx="1" ny="1" dx="0" dy="0" angle="0.00" mirror="false"/>"#
    ));
}

#[test]
fn rejects_primary_panel_step() {
    let error = create_board_array_xml(
        panel_fixture(),
        &BoardArrayCreateOptions {
            columns: 1,
            rows: 1,
            board_margin_mm: board_margin(0.0, 0.0),
            edge_rail_mm: BoardMarginMm::all(5.0),
        },
    )
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("primary IPC-2581 step is already a board array")
    );
}

#[test]
fn validates_simple_api_ranges() {
    let error = create_board_array_xml(
        board_fixture_mm(),
        &BoardArrayCreateOptions {
            columns: 11,
            rows: 1,
            board_margin_mm: board_margin(0.0, 0.0),
            edge_rail_mm: BoardMarginMm::all(5.0),
        },
    )
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("columns must be between 1 and 10")
    );
}

#[test]
fn rejects_small_clearance_and_edge_rail() {
    let horizontal_gap_error = create_board_array_xml(
        board_fixture_mm(),
        &BoardArrayCreateOptions {
            columns: 2,
            rows: 1,
            board_margin_mm: board_margin(4.99, 0.0),
            edge_rail_mm: BoardMarginMm::all(5.0),
        },
    )
    .unwrap_err();
    assert!(
        horizontal_gap_error
            .to_string()
            .contains("horizontal board clearance must be 0 mm or at least 5 mm")
    );

    let vertical_gap_error = create_board_array_xml(
        board_fixture_mm(),
        &BoardArrayCreateOptions {
            columns: 1,
            rows: 2,
            board_margin_mm: board_margin(0.0, 4.99),
            edge_rail_mm: BoardMarginMm::all(5.0),
        },
    )
    .unwrap_err();
    assert!(
        vertical_gap_error
            .to_string()
            .contains("vertical board clearance must be 0 mm or at least 5 mm")
    );

    let rail_error = create_board_array_xml(
        board_fixture_mm(),
        &BoardArrayCreateOptions {
            columns: 1,
            rows: 1,
            board_margin_mm: board_margin(0.0, 0.0),
            edge_rail_mm: BoardMarginMm::all(0.0),
        },
    )
    .unwrap_err();
    assert!(
        rail_error
            .to_string()
            .contains("edge rail top must be between 5 and 30 mm; got 0 mm")
    );
}

#[test]
fn rejects_more_than_25_vcut_lines_per_axis() {
    let x_error = vcut_lines(VcutLineSpec {
        columns: 13,
        rows: 1,
        board_width_mm: 10.0,
        board_height_mm: 10.0,
        margin_x_mm: 5.0,
        margin_y_mm: 5.0,
        pitch_x_mm: 15.0,
        pitch_y_mm: 15.0,
        array_width_mm: 210.0,
        array_height_mm: 25.0,
    })
    .unwrap_err();
    assert!(
        x_error
            .to_string()
            .contains("X-axis V-cut line count must be at most 25; got 26")
    );

    let y_error = vcut_lines(VcutLineSpec {
        columns: 1,
        rows: 13,
        board_width_mm: 10.0,
        board_height_mm: 10.0,
        margin_x_mm: 5.0,
        margin_y_mm: 5.0,
        pitch_x_mm: 15.0,
        pitch_y_mm: 15.0,
        array_width_mm: 25.0,
        array_height_mm: 210.0,
    })
    .unwrap_err();
    assert!(
        y_error
            .to_string()
            .contains("Y-axis V-cut line count must be at most 25; got 26")
    );
}

#[test]
fn rejects_array_dimensions_outside_limits() {
    let narrow_error = create_board_array_xml(
        board_fixture_mm(),
        &BoardArrayCreateOptions {
            columns: 3,
            rows: 2,
            board_margin_mm: board_margin(5.0, 5.0),
            edge_rail_mm: BoardMarginMm::all(5.0),
        },
    )
    .unwrap_err();
    assert!(
        narrow_error
            .to_string()
            .contains("array width must be at least 70 mm; got 55 mm")
    );

    let short_error = create_board_array_xml(
        board_fixture_mm(),
        &BoardArrayCreateOptions {
            columns: 4,
            rows: 2,
            board_margin_mm: board_margin(5.0, 5.0),
            edge_rail_mm: BoardMarginMm::all(5.0),
        },
    )
    .unwrap_err();
    assert!(
        short_error
            .to_string()
            .contains("array height must be at least 70 mm; got 40 mm")
    );

    let wide_error = create_board_array_xml(
        large_board_fixture_mm(),
        &BoardArrayCreateOptions {
            columns: 6,
            rows: 1,
            board_margin_mm: board_margin(5.0, 5.0),
            edge_rail_mm: BoardMarginMm::all(5.0),
        },
    )
    .unwrap_err();
    assert!(
        wide_error
            .to_string()
            .contains("array width must be at most 297 mm; got 400 mm")
    );

    let tall_error = create_board_array_xml(
        large_board_fixture_mm(),
        &BoardArrayCreateOptions {
            columns: 1,
            rows: 6,
            board_margin_mm: board_margin(5.0, 5.0),
            edge_rail_mm: BoardMarginMm::all(5.0),
        },
    )
    .unwrap_err();
    assert!(
        tall_error
            .to_string()
            .contains("array height must be at most 297 mm; got 400 mm")
    );
}

fn board_margin(horizontal_gap_mm: f64, vertical_gap_mm: f64) -> BoardMarginMm {
    BoardMarginMm {
        top: vertical_gap_mm / 2.0,
        right: horizontal_gap_mm / 2.0,
        bottom: vertical_gap_mm / 2.0,
        left: horizontal_gap_mm / 2.0,
    }
}

fn edge_rail(horizontal_mm: f64, vertical_mm: f64) -> BoardMarginMm {
    BoardMarginMm {
        top: vertical_mm,
        right: horizontal_mm,
        bottom: vertical_mm,
        left: horizontal_mm,
    }
}

fn svg_viewbox(svg: &str) -> (f64, f64, f64, f64) {
    let value = svg
        .split("viewBox='")
        .nth(1)
        .and_then(|rest| rest.split('\'').next())
        .expect("SVG should have a viewBox");
    let values = value
        .split_whitespace()
        .map(|part| part.parse::<f64>().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(values.len(), 4);
    (values[0], values[1], values[2], values[3])
}

fn assert_point_close(actual: Point, expected: Point) {
    assert!(
        (actual.x - expected.x).abs() < 1e-9 && (actual.y - expected.y).abs() < 1e-9,
        "expected {expected:?}, got {actual:?}"
    );
}

fn assert_intent_eq(
    before_ipc: &Ipc2581,
    after_ipc: &Ipc2581,
    before: &FeatureIntent<ipc2581::Symbol>,
    after: &FeatureIntent<ipc2581::Symbol>,
) {
    assert_eq!(before.domain, after.domain);
    assert_eq!(before.role, after.role);
    assert_eq!(before.operation, after.operation);
    assert_eq!(before.material, after.material);
    assert_eq!(before.plating, after.plating);
    assert_eq!(before.side, after.side);
    assert_eq!(
        resolved_feature_span(before_ipc, before.span),
        resolved_feature_span(after_ipc, after.span)
    );
}

fn resolved_feature_span(ipc: &Ipc2581, span: FeatureSpan<ipc2581::Symbol>) -> String {
    match span {
        FeatureSpan::Unknown => "Unknown".to_string(),
        FeatureSpan::ThroughBoard => "ThroughBoard".to_string(),
        FeatureSpan::Layer(layer) => format!("Layer({})", ipc.resolve(layer)),
        FeatureSpan::FromTo { from, to } => format!(
            "FromTo({},{})",
            from.map(|layer| ipc.resolve(layer)).unwrap_or(""),
            to.map(|layer| ipc.resolve(layer)).unwrap_or("")
        ),
    }
}

fn close(actual: f64, expected: f64) -> bool {
    (actual - expected).abs() < 1e-9
}

fn array_step(ipc: &Ipc2581) -> &ipc2581::types::ecad::Step {
    ipc.ecad()
        .unwrap()
        .cad_data
        .steps
        .iter()
        .find(|step| ipc.resolve(step.name) == "array")
        .unwrap()
}

fn board_cell_step(ipc: &Ipc2581) -> &ipc2581::types::ecad::Step {
    ipc.ecad()
        .unwrap()
        .cad_data
        .steps
        .iter()
        .find(|step| ipc.resolve(step.name) == "board_cell")
        .unwrap()
}

fn fiducials_on_layer<'a>(
    ipc: &'a Ipc2581,
    step: &'a ipc2581::types::ecad::Step,
    layer_name: &str,
) -> Vec<&'a Fiducial> {
    step.layer_features
        .iter()
        .filter(|layer_feature| ipc.resolve(layer_feature.layer_ref) == layer_name)
        .flat_map(|layer_feature| &layer_feature.sets)
        .flat_map(|set| set.fiducials())
        .collect()
}

fn holes_on_layer<'a>(
    ipc: &'a Ipc2581,
    step: &'a ipc2581::types::ecad::Step,
    layer_name: &str,
) -> Vec<&'a Hole> {
    step.layer_features
        .iter()
        .filter(|layer_feature| ipc.resolve(layer_feature.layer_ref) == layer_name)
        .flat_map(|layer_feature| &layer_feature.sets)
        .flat_map(|set| set.holes())
        .collect()
}

fn fiducial_diameter(fiducial: &Fiducial) -> f64 {
    match &fiducial.shape {
        FiducialShape::Primitive(StandardPrimitive::Circle(circle)) => circle.shape.diameter,
        _ => panic!("expected round fiducial"),
    }
}

fn fiducial_points(fiducials: &[&Fiducial]) -> Vec<(f64, f64)> {
    fiducials
        .iter()
        .map(|fiducial| (fiducial.location.x, fiducial.location.y))
        .collect()
}

fn hole_points(holes: &[&Hole]) -> Vec<(f64, f64)> {
    holes.iter().map(|hole| (hole.x, hole.y)).collect()
}

fn holes_with_diameter<'a>(holes: &[&'a Hole], diameter_mm: f64) -> Vec<&'a Hole> {
    holes
        .iter()
        .copied()
        .filter(|hole| close(hole.diameter, diameter_mm))
        .collect()
}

fn assert_corner_holes(holes: &[&Hole], array_width_mm: f64, array_height_mm: f64) {
    let inset = ARRAY_CORNER_TOOLING_HOLE_INSET_MM;
    assert_points_close(
        hole_points(holes),
        vec![
            (inset, inset),
            (array_width_mm - inset, inset),
            (array_width_mm - inset, array_height_mm - inset),
            (inset, array_height_mm - inset),
        ],
    );
}

fn assert_points_close(actual: Vec<(f64, f64)>, expected: Vec<(f64, f64)>) {
    let actual = sorted_points(actual);
    let expected = sorted_points(expected);
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(&expected) {
        assert!(
            close(actual.0, expected.0) && close(actual.1, expected.1),
            "expected {expected:?}, got {actual:?}"
        );
    }
}

fn sorted_points(mut points: Vec<(f64, f64)>) -> Vec<(f64, f64)> {
    points.sort_by(|left, right| {
        left.1
            .total_cmp(&right.1)
            .then_with(|| left.0.total_cmp(&right.0))
    });
    points
}

fn board_fixture_mm() -> &'static str {
    r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
<FunctionMode mode="FABRICATION"/>
<StepRef name="board"/>
<LayerRef name="TOP"/>
  </Content>
  <Ecad>
<CadHeader units="MILLIMETER"/>
<CadData>
  <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
  <Step name="board" type="BOARD">
    <Datum x="0" y="0"/>
    <Profile>
      <Polygon>
        <PolyBegin x="-2" y="-3"/>
        <PolyStepSegment x="8" y="-3"/>
        <PolyStepSegment x="8" y="7"/>
        <PolyStepSegment x="-2" y="7"/>
        <PolyStepSegment x="-2" y="-3"/>
      </Polygon>
    </Profile>
  </Step>
</CadData>
  </Ecad>
</IPC-2581>"#
}

fn rounded_corner_board_fixture_mm() -> &'static str {
    r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
<FunctionMode mode="FABRICATION"/>
<StepRef name="board"/>
<LayerRef name="TOP"/>
  </Content>
  <Ecad>
<CadHeader units="MILLIMETER"/>
<CadData>
  <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
  <Step name="board" type="BOARD">
    <Datum x="0" y="0"/>
    <Profile>
      <Polygon>
        <PolyBegin x="0" y="0"/>
        <PolyStepSegment x="10" y="0"/>
        <PolyStepSegment x="10" y="10"/>
        <PolyStepSegment x="4" y="10"/>
        <PolyStepCurve x="0" y="6" centerX="4" centerY="6" clockwise="false"/>
        <PolyStepSegment x="0" y="0"/>
      </Polygon>
    </Profile>
  </Step>
</CadData>
  </Ecad>
</IPC-2581>"#
}

fn board_fixture_with_mask_bbox_mm(width_mm: f64, height_mm: f64) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
<FunctionMode mode="FABRICATION"/>
<StepRef name="board"/>
<LayerRef name="TOP"/>
  </Content>
  <Ecad>
<CadHeader units="MILLIMETER"/>
<CadData>
  <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
  <Layer name="F.Mask" layerFunction="SOLDERMASK" side="TOP" polarity="POSITIVE"/>
  <Step name="board" type="BOARD">
    <Datum x="0" y="0"/>
    <Profile>
      <Polygon>
        <PolyBegin x="0" y="0"/>
        <PolyStepSegment x="{width_mm}" y="0"/>
        <PolyStepSegment x="{width_mm}" y="{height_mm}"/>
        <PolyStepSegment x="0" y="{height_mm}"/>
        <PolyStepSegment x="0" y="0"/>
      </Polygon>
    </Profile>
  </Step>
</CadData>
  </Ecad>
</IPC-2581>"#
    )
}

fn board_fixture_with_courtyard_overhang_mm() -> &'static str {
    r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
<FunctionMode mode="FABRICATION"/>
<StepRef name="board"/>
<LayerRef name="TOP"/>
<LayerRef name="F.Courtyard"/>
  </Content>
  <Ecad>
<CadHeader units="MILLIMETER"/>
<CadData>
  <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
  <Layer name="F.Courtyard" layerFunction="COURTYARD" side="TOP" polarity="POSITIVE"/>
  <Step name="board" type="BOARD">
    <Datum x="0" y="0"/>
    <Profile>
      <Polygon>
        <PolyBegin x="0" y="0"/>
        <PolyStepSegment x="10" y="0"/>
        <PolyStepSegment x="10" y="10"/>
        <PolyStepSegment x="0" y="10"/>
        <PolyStepSegment x="0" y="0"/>
      </Polygon>
    </Profile>
    <LayerFeature layerRef="F.Courtyard">
      <Set polarity="POSITIVE">
        <Features>
          <Polygon>
            <PolyBegin x="-2" y="-1"/>
            <PolyStepSegment x="13" y="-1"/>
            <PolyStepSegment x="13" y="12"/>
            <PolyStepSegment x="-2" y="12"/>
            <PolyStepSegment x="-2" y="-1"/>
          </Polygon>
        </Features>
      </Set>
    </LayerFeature>
  </Step>
</CadData>
  </Ecad>
</IPC-2581>"#
}

fn board_fixture_with_large_courtyard_overhang_mm() -> &'static str {
    r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
<FunctionMode mode="FABRICATION"/>
<StepRef name="board"/>
<LayerRef name="TOP"/>
<LayerRef name="F.Courtyard"/>
  </Content>
  <Ecad>
<CadHeader units="MILLIMETER"/>
<CadData>
  <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
  <Layer name="F.Courtyard" layerFunction="COURTYARD" side="TOP" polarity="POSITIVE"/>
  <Step name="board" type="BOARD">
    <Datum x="0" y="0"/>
    <Profile>
      <Polygon>
        <PolyBegin x="0" y="0"/>
        <PolyStepSegment x="10" y="0"/>
        <PolyStepSegment x="10" y="10"/>
        <PolyStepSegment x="0" y="10"/>
        <PolyStepSegment x="0" y="0"/>
      </Polygon>
    </Profile>
    <LayerFeature layerRef="F.Courtyard">
      <Set polarity="POSITIVE">
        <Features>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="31" y="0"/>
            <PolyStepSegment x="31" y="10"/>
            <PolyStepSegment x="0" y="10"/>
            <PolyStepSegment x="0" y="0"/>
          </Polygon>
        </Features>
      </Set>
    </LayerFeature>
  </Step>
</CadData>
  </Ecad>
</IPC-2581>"#
}

fn board_fixture_with_mask_mm() -> &'static str {
    r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
<FunctionMode mode="FABRICATION"/>
<StepRef name="board"/>
<LayerRef name="TOP"/>
  </Content>
  <Ecad>
<CadHeader units="MILLIMETER"/>
<CadData>
  <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
  <Layer name="F.Mask" layerFunction="SOLDERMASK" side="TOP" polarity="POSITIVE"/>
  <Step name="board" type="BOARD">
    <Datum x="0" y="0"/>
    <Profile>
      <Polygon>
        <PolyBegin x="-2" y="-3"/>
        <PolyStepSegment x="8" y="-3"/>
        <PolyStepSegment x="8" y="7"/>
        <PolyStepSegment x="-2" y="7"/>
        <PolyStepSegment x="-2" y="-3"/>
      </Polygon>
    </Profile>
  </Step>
</CadData>
  </Ecad>
</IPC-2581>"#
}

fn board_fixture_with_top_line_mm() -> &'static str {
    r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
<FunctionMode mode="FABRICATION"/>
<StepRef name="board"/>
<LayerRef name="TOP"/>
  </Content>
  <Ecad>
<CadHeader units="MILLIMETER"/>
<CadData>
  <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
  <Step name="board" type="BOARD">
    <Datum x="0" y="0"/>
    <Profile>
      <Polygon>
        <PolyBegin x="-2" y="-3"/>
        <PolyStepSegment x="8" y="-3"/>
        <PolyStepSegment x="8" y="7"/>
        <PolyStepSegment x="-2" y="7"/>
        <PolyStepSegment x="-2" y="-3"/>
      </Polygon>
    </Profile>
    <LayerFeature layerRef="TOP">
      <Set polarity="POSITIVE">
        <Features>
          <Line startX="0" startY="0" endX="5" endY="0">
            <LineDesc lineWidth="0.2" lineEnd="ROUND"/>
          </Line>
        </Features>
      </Set>
    </LayerFeature>
  </Step>
</CadData>
  </Ecad>
</IPC-2581>"#
}

fn board_fixture_with_edge_cuts_layer_mm() -> &'static str {
    r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
<FunctionMode mode="FABRICATION"/>
<StepRef name="board"/>
<LayerRef name="TOP"/>
<LayerRef name="Edge.Cuts"/>
  </Content>
  <Ecad>
<CadHeader units="MILLIMETER"/>
<CadData>
  <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
  <Layer name="Edge.Cuts" layerFunction="BOARD_OUTLINE" side="ALL" polarity="POSITIVE"/>
  <Step name="board" type="BOARD">
    <Datum x="0" y="0"/>
    <Profile>
      <Polygon>
        <PolyBegin x="0" y="0"/>
        <PolyStepSegment x="40" y="0"/>
        <PolyStepSegment x="40" y="40"/>
        <PolyStepSegment x="0" y="40"/>
        <PolyStepSegment x="0" y="0"/>
      </Polygon>
    </Profile>
    <LayerFeature layerRef="TOP">
      <Set polarity="POSITIVE">
        <Features>
          <Line startX="1" startY="1" endX="5" endY="1">
            <LineDesc lineWidth="0.2" lineEnd="ROUND"/>
          </Line>
        </Features>
      </Set>
    </LayerFeature>
    <LayerFeature layerRef="Edge.Cuts">
      <Set polarity="POSITIVE">
        <Features>
          <Line startX="0" startY="0" endX="40" endY="0">
            <LineDesc lineWidth="0.05" lineEnd="ROUND"/>
          </Line>
        </Features>
      </Set>
    </LayerFeature>
  </Step>
</CadData>
  </Ecad>
</IPC-2581>"#
}

fn board_fixture_inch() -> &'static str {
    r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
<FunctionMode mode="FABRICATION"/>
<StepRef name="board"/>
<LayerRef name="TOP"/>
  </Content>
  <Ecad>
<CadHeader units="INCH"/>
<CadData>
  <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
  <Step name="board" type="BOARD">
    <Datum x="0" y="0"/>
    <Profile>
      <Polygon>
        <PolyBegin x="0" y="0"/>
        <PolyStepSegment x="1" y="0"/>
        <PolyStepSegment x="1" y="1"/>
        <PolyStepSegment x="0" y="1"/>
        <PolyStepSegment x="0" y="0"/>
      </Polygon>
    </Profile>
  </Step>
</CadData>
  </Ecad>
</IPC-2581>"#
}

fn large_board_fixture_mm() -> &'static str {
    r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
<FunctionMode mode="FABRICATION"/>
<StepRef name="board"/>
<LayerRef name="TOP"/>
  </Content>
  <Ecad>
<CadHeader units="MILLIMETER"/>
<CadData>
  <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
  <Step name="board" type="BOARD">
    <Datum x="0" y="0"/>
    <Profile>
      <Polygon>
        <PolyBegin x="0" y="0"/>
        <PolyStepSegment x="60" y="0"/>
        <PolyStepSegment x="60" y="60"/>
        <PolyStepSegment x="0" y="60"/>
        <PolyStepSegment x="0" y="0"/>
      </Polygon>
    </Profile>
  </Step>
</CadData>
  </Ecad>
</IPC-2581>"#
}

fn panel_fixture() -> &'static str {
    r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
<FunctionMode mode="FABRICATION"/>
<StepRef name="panel"/>
<LayerRef name="TOP"/>
  </Content>
  <Ecad>
<CadHeader units="MILLIMETER"/>
<CadData>
  <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
  <Step name="panel" type="PALLET">
    <Datum x="0" y="0"/>
    <Profile>
      <Polygon>
        <PolyBegin x="0" y="0"/>
        <PolyStepSegment x="10" y="0"/>
        <PolyStepSegment x="10" y="10"/>
        <PolyStepSegment x="0" y="10"/>
        <PolyStepSegment x="0" y="0"/>
      </Polygon>
    </Profile>
  </Step>
</CadData>
  </Ecad>
</IPC-2581>"#
}
