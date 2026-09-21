use super::*;

#[test]
fn rotated_bottom_silkscreen_matches_kicad_board_coordinates() {
    // Warden J3: the line must stay left of the footprint origin after placement.
    let ipc = Ipc2581::parse(
        r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="Owner"><FunctionMode mode="FABRICATION"/><StepRef name="board"/></Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="B.Silkscreen" layerFunction="SILKSCREEN" side="BOTTOM"/>
      <Step name="board" type="BOARD">
        <LayerFeature layerRef="B.Silkscreen">
          <Set geometryUsage="GRAPHIC">
            <Features>
              <Xform rotation="270" mirror="true"/>
              <Location x="171.456527" y="-116.7"/>
              <Line startX="-2.15" startY="2.8" endX="-3.25" endY="2.8">
                <LineDesc lineWidth="0.25" lineEnd="ROUND"/>
              </Line>
            </Features>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
    )
    .unwrap();
    let resolution = Resolution::default();
    let imported = import_design(&ipc, resolution).unwrap();
    let image = imported
        .composed_layer_image(
            imported.layer_id("B.Silkscreen").unwrap(),
            ArtworkScope::Board,
            resolution,
        )
        .unwrap();

    // Coordinates independently checked against KiCad's board and direct Gerber.
    assert!(image.contains_point(Point::new(168.656527, -114.55)));
    assert!(image.contains_point(Point::new(168.656527, -113.45)));
    assert!(!image.contains_point(Point::new(174.256527, -118.85)));
}

#[test]
fn ipc_offsets_and_geometry_rotate_before_mirroring() {
    for (rotation, mirror, expected) in [
        (0.0, false, Point::new(14.0, 26.0)),
        (90.0, false, Point::new(4.0, 24.0)),
        (180.0, false, Point::new(6.0, 14.0)),
        (270.0, false, Point::new(16.0, 16.0)),
        (0.0, true, Point::new(6.0, 26.0)),
        (90.0, true, Point::new(16.0, 24.0)),
        (180.0, true, Point::new(14.0, 14.0)),
        (270.0, true, Point::new(4.0, 16.0)),
    ] {
        let placement = ipc_placement(
            Point::new(10.0, 20.0),
            Some(Xform {
                rotation,
                mirror,
                scale: 2.0,
                x_offset: 0.5,
                y_offset: 1.0,
                ..Xform::default()
            }),
        );
        let actual = placement.transform.transform_point(Point::new(1.5, 2.0));
        assert!((actual.x - expected.x).abs() < 1e-9);
        assert!((actual.y - expected.y).abs() < 1e-9);
    }
}

#[test]
fn void_verification_inherits_accuracy_but_keeps_its_significance() {
    let metadata = CopperBalanceVoidMetadata {
        lattice_origin: Point::new(0.0, 0.0),
        lattice_pitch_mm: 3.0,
        radius_mm: 1.0,
        corner_radius_mm: 0.0001,
    };
    let outline = shapes::rounded_hexagon(metadata.radius_mm, metadata.corner_radius_mm, 0.0)
        .unwrap()
        .with_uncertainty(0.02);
    let mut doc = GeometryDocument::new();
    let path = doc.push_path(
        Paint::Fill {
            rule: FillRule::NonZero,
        },
        [outline],
    );
    let mut feature = GeometryFeature::new(FeatureKind::Primitive, GeometryPolarity::Dark);
    feature.paths = Span::single(path);

    let coarse = Resolution::new(10.0, crate::geom::GeometryAccuracy::micrometres(30));
    validate_copper_balance_void_shape(&doc, &feature, metadata, coarse).unwrap();
    let fine = coarse.with_accuracy(crate::geom::GeometryAccuracy::micrometres(10));
    let error = validate_copper_balance_void_shape(&doc, &feature, metadata, fine).unwrap_err();
    assert!(error.downcast_ref::<crate::geom::AccuracyError>().is_some());
}

#[test]
fn maps_all_ipc_line_properties_to_ir_patterns() {
    assert_eq!(map_line_pattern(None), LinePattern::Solid);
    assert_eq!(
        map_line_pattern(Some(LineProperty::Solid)),
        LinePattern::Solid
    );
    assert_eq!(
        map_line_pattern(Some(LineProperty::Dotted)),
        LinePattern::Dotted
    );
    assert_eq!(
        map_line_pattern(Some(LineProperty::Dashed)),
        LinePattern::Dashed
    );
    assert_eq!(
        map_line_pattern(Some(LineProperty::Center)),
        LinePattern::Center
    );
    assert_eq!(
        map_line_pattern(Some(LineProperty::Phantom)),
        LinePattern::Phantom
    );
    assert_eq!(
        map_line_pattern(Some(LineProperty::Erase)),
        LinePattern::Erase
    );
}

#[test]
fn preserves_inline_feature_line_property() {
    let ipc = Ipc2581::parse(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="Owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <LayerRef name="TOP"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SILKSCREEN" side="TOP"/>
      <Step name="board" type="BOARD">
        <LayerFeature layerRef="TOP">
          <Set>
            <Features>
              <Line startX="0" startY="0" endX="10" endY="0">
                <LineDesc lineWidth="0.1" lineEnd="ROUND" lineProperty="PHANTOM"/>
              </Line>
            </Features>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
    )
    .unwrap();

    let layer =
        extract_layer_for_view(&ipc, "TOP", ArtworkScope::Board, Resolution::default()).unwrap();
    let path = &layer.arena.paths[layer.features[0].paths.start as usize];

    assert_eq!(path.stroke().unwrap().pattern, LinePattern::Phantom);
}

/// One board with `features` on its TOP copper and `drills` on its drill layer.
fn shape_fixture(features: &str, drills: &str) -> Ipc2581 {
    Ipc2581::parse(&format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="Owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="CONDUCTOR" side="TOP" polarity="POSITIVE"/>
      <Layer name="BOTTOM" layerFunction="CONDUCTOR" side="BOTTOM" polarity="POSITIVE"/>
      <Layer name="DRILL" layerFunction="DRILL" side="ALL" polarity="POSITIVE">
        <Span fromLayer="TOP" toLayer="BOTTOM"/>
      </Layer>
      <Step name="board" type="BOARD">
        <LayerFeature layerRef="TOP"><Set>{features}</Set></LayerFeature>
        <LayerFeature layerRef="DRILL"><Set>{drills}</Set></LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#
    ))
    .unwrap()
}

/// The area TOP images as artwork, and how many of its objects are flashes.
fn top_artwork_area_and_flashes(ipc: &Ipc2581) -> (f64, usize) {
    let resolution = Resolution::default();
    let mut doc = extract_layer_for_view(ipc, "TOP", ArtworkScope::Board, resolution).unwrap();
    crate::dialects::ipc::process::normalize_for_positive_artwork(&mut doc, resolution).unwrap();
    let artwork = crate::dialects::ipc::lower_layer_to_artwork(
        &doc,
        0,
        crate::dialects::LayerRole::Copper,
        crate::dialects::Side::Top,
    );
    let flashes = artwork
        .objects
        .iter()
        .filter(|object| {
            matches!(
                object.geometry,
                crate::dialects::artwork::Geometry::Flash { .. }
            )
        })
        .count();
    let (images, _) =
        crate::dialects::artwork::compose_owner_regions(&artwork, |_| Some(()), resolution)
            .unwrap();
    (
        images[0].iter().map(|(_, image)| image.area()).sum(),
        flashes,
    )
}

#[test]
fn a_fiducial_flashes_only_the_circle_it_is() {
    let pi = std::f64::consts::PI;
    let fiducial = |shape: &str| {
        shape_fixture(
            &format!(r#"<GlobalFiducial><Location x="5" y="5"/>{shape}</GlobalFiducial>"#),
            "",
        )
    };

    let (area, flashes) = top_artwork_area_and_flashes(&fiducial(r#"<Circle diameter="2"/>"#));
    assert_eq!(flashes, 1);
    assert!(
        (area / pi - 1.0).abs() < 0.01,
        "a 2 mm disc, not {area} mm²"
    );

    let (area, flashes) =
        top_artwork_area_and_flashes(&fiducial(r#"<Xform scale="2"/><Circle diameter="2"/>"#));
    assert_eq!(flashes, 1);
    assert!(
        (area / (4.0 * pi) - 1.0).abs() < 0.01,
        "a 4 mm disc, not {area} mm²"
    );

    let (area, flashes) = top_artwork_area_and_flashes(&fiducial(
        r#"<Donut shape="ROUND" outerDiameter="2" innerDiameter="1"/>"#,
    ));
    assert_eq!(flashes, 0, "a ring is not a disc aperture");
    assert!(
        (area / (0.75 * pi) - 1.0).abs() < 0.01,
        "a ring, not {area} mm²"
    );
}

#[test]
fn a_fiducial_a_slot_cuts_images_cut() {
    let ipc = shape_fixture(
        r#"<GlobalFiducial><Location x="5" y="5"/><Circle diameter="2"/></GlobalFiducial>"#,
        r#"<SlotCavity name="S" platingStatus="NONPLATED" plusTol="0" minusTol="0">
             <Location x="5" y="5"/><RectCenter width="4" height="1"/>
           </SlotCavity>"#,
    );
    let (area, flashes) = top_artwork_area_and_flashes(&ipc);
    assert_eq!(flashes, 0);
    assert!(
        area < 0.5 * std::f64::consts::PI,
        "the slot's band is gone, not {area} mm²"
    );
}

#[test]
fn nc_refuses_a_square_hole_instead_of_drilling_it_round() {
    let hole = |shape: &str| {
        let ipc = shape_fixture(
            "",
            &format!(
                r#"<Hole name="H" diameter="1" platingStatus="NONPLATED" plusTol="0" minusTol="0" x="3" y="4"{shape}/>"#
            ),
        );
        let doc = extract_layer_for_view(&ipc, "DRILL", ArtworkScope::Board, Resolution::default())
            .unwrap();
        let mut nc = crate::dialects::nc::Document::new();
        crate::dialects::ipc::lower_to_nc(&doc, &mut nc).map(|()| nc.objects.len())
    };

    assert_eq!(hole(""), Ok(1));
    assert!(
        hole(r#" type="SQUARE""#)
            .unwrap_err()
            .contains("not a round hole")
    );
}

#[test]
fn nc_plunges_an_oval_slot_as_wide_as_it_is_long() {
    let slot = |oval: &str| {
        let ipc = shape_fixture(
            "",
            &format!(
                r#"<SlotCavity name="S" platingStatus="NONPLATED" plusTol="0" minusTol="0">
                     <Location x="3" y="4"/>{oval}
                   </SlotCavity>"#
            ),
        );
        let doc = extract_layer_for_view(&ipc, "DRILL", ArtworkScope::Board, Resolution::default())
            .unwrap();
        let mut nc = crate::dialects::nc::Document::new();
        crate::dialects::ipc::lower_to_nc(&doc, &mut nc).unwrap();
        nc.objects.remove(0).geometry
    };

    assert_eq!(
        slot(r#"<Oval width="1" height="1"/>"#),
        crate::dialects::nc::Geometry::Drill {
            at: Point::new(3.0, 4.0),
            diameter: 1.0,
        }
    );
    assert_eq!(
        slot(r#"<Xform rotation="90"/><Oval width="3" height="1"/>"#),
        crate::dialects::nc::Geometry::Slot {
            diameter: 1.0,
            start: Point::new(3.0, 3.0),
            end: Point::new(3.0, 5.0),
        }
    );
}

#[test]
fn carries_spec_refs_fiducials_and_vcut_intent() {
    let ipc = Ipc2581::parse(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="Owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="Panel"/>
    <LayerRef name="TOP"/>
    <LayerRef name="VCUT"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER">
      <Spec name="VCut_1">
        <V_Cut type="ANGLE">
          <Property value="90" unit="DEGREES"/>
        </V_Cut>
      </Spec>
    </CadHeader>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE">
        <SpecRef id="VCut_1"/>
      </Layer>
      <Layer name="VCUT" layerFunction="V_CUT" side="ALL" polarity="POSITIVE">
        <SpecRef id="VCut_1"/>
      </Layer>
      <Step name="Panel" type="PALLET">
        <LayerFeature layerRef="TOP">
          <Set>
            <SpecRef id="VCut_1"/>
            <GlobalFiducial>
              <Location x="1" y="2"/>
              <Circle diameter="1"/>
              <PinRef componentRef="U1" pin="1"/>
            </GlobalFiducial>
          </Set>
        </LayerFeature>
        <LayerFeature layerRef="VCUT">
          <Set>
            <SpecRef id="VCut_1"/>
            <Features>
              <Line startX="0" startY="5" endX="10" endY="5">
                <LineDesc lineWidth="0.1" lineEnd="ROUND"/>
              </Line>
            </Features>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
    )
    .unwrap();

    let top = extract_layer_for_view(
        &ipc,
        "TOP",
        ArtworkScope::ArrayFlattened,
        Resolution::default(),
    )
    .unwrap();
    assert_eq!(top.specs.len(), 1);
    assert_eq!(top.layers[0].spec_refs.count, 1);
    assert_eq!(top.feature_sets.len(), 1);
    assert_eq!(top.feature_sets[0].spec_refs.count, 1);
    assert_eq!(top.features[0].bucket, FeatureBucket::Fiducial);
    assert_eq!(top.features[0].intent.role, FeatureRole::Fiducial);
    assert_eq!(top.features[0].fiducial_kind, FiducialKind::Global);
    assert!(top.features[0].is_fiducial());
    assert_eq!(top.features[0].source_step_kind, LayoutStepKind::Panel);
    assert_eq!(
        top.features[0]
            .source_step_ref
            .map(|step| ipc.resolve(step)),
        Some("Panel")
    );
    assert_eq!(top.features[0].pin_refs.count, 1);
    assert_eq!(ipc.resolve(top.pin_refs[0].pin), "1");

    let vcut = extract_layer_for_view(
        &ipc,
        "VCUT",
        ArtworkScope::ArrayFlattened,
        Resolution::default(),
    )
    .unwrap();
    assert_eq!(vcut.layers[0].spec_refs.count, 1);
    assert_eq!(vcut.feature_sets[0].spec_refs.count, 1);
    assert_eq!(vcut.features[0].intent.domain, FeatureDomain::VCut);
    assert_eq!(vcut.features[0].intent.role, FeatureRole::ArraySeparation);
    assert!(vcut.features[0].is_vcut());
}

#[test]
fn lowers_moire_as_rings_and_crosshair() {
    let mut doc = GeometryDocument::new();

    push_moire_path(
        &mut doc,
        Affine2::identity(),
        &ipc2581::types::Moire {
            diameter: 8.0,
            ring_width: 0.5,
            ring_gap: 1.0,
            ring_number: 3,
            line_width: Some(0.2),
            line_length: Some(10.0),
            line_angle: Some(0.0),
        },
    );

    assert_eq!(doc.arena.paths.len(), 5);
    assert_eq!(doc.arena.paths[0].fill_rule(), Some(FillRule::EvenOdd));
    assert_eq!(doc.arena.paths[0].contours.count, 2);
    assert_eq!(doc.arena.paths[1].contours.count, 2);
    assert_eq!(doc.arena.paths[2].contours.count, 2);
    assert_eq!(doc.arena.paths[3].fill_rule(), Some(FillRule::NonZero));
    assert_eq!(doc.arena.paths[4].fill_rule(), Some(FillRule::NonZero));
    assert_eq!(doc.arena.paths[0].bbox.min, Point::new(-4.25, -4.25));
    assert_eq!(doc.arena.paths[0].bbox.max, Point::new(4.25, 4.25));
    assert_eq!(doc.arena.paths[1].bbox.min, Point::new(-3.25, -3.25));
    assert_eq!(doc.arena.paths[1].bbox.max, Point::new(3.25, 3.25));
}

#[test]
fn reads_standard_primitive_fill_properties() {
    let ipc = Ipc2581::parse(
        r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581"><Content roleRef="Owner"><FunctionMode mode="FABRICATION"/></Content></IPC-2581>"#,
    )
    .unwrap();
    let context = ExtractContext::new(ipc.interner(), ipc.content(), Resolution::default());
    let circle = ipc2581::types::StandardPrimitive::Circle(ipc2581::types::Styled {
        shape: ipc2581::types::Circle { diameter: 1.0 },
        fill_property: Some(FillProperty::Hollow),
        line_desc: None,
        line_desc_ref: None,
        fill_desc: None,
        fill_desc_ref: None,
    });
    let rect = ipc2581::types::StandardPrimitive::RectCenter(ipc2581::types::Styled {
        shape: ipc2581::types::RectCenter {
            size: ipc2581::types::Size {
                width: 1.0,
                height: 1.0,
            },
        },
        fill_property: Some(FillProperty::Void),
        line_desc: None,
        line_desc_ref: None,
        fill_desc: None,
        fill_desc_ref: None,
    });

    assert_eq!(
        primitive_fill_property(&context, &circle),
        Some(FillProperty::Hollow)
    );
    assert_eq!(
        primitive_fill_property(&context, &rect),
        Some(FillProperty::Void)
    );
}

#[test]
fn patterned_fills_are_painted_solid_with_a_warning() {
    let ipc = Ipc2581::parse(
        r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581"><Content roleRef="Owner"><FunctionMode mode="FABRICATION"/></Content></IPC-2581>"#,
    )
    .unwrap();
    let context = ExtractContext::new(ipc.interner(), ipc.content(), Resolution::default());
    for fill_property in [FillProperty::Hatch, FillProperty::Mesh] {
        let mut doc = GeometryDocument::new();
        let primitive = StandardPrimitive::Circle(ipc2581::types::Styled {
            shape: ipc2581::types::Circle { diameter: 1.0 },
            fill_property: Some(fill_property),
            line_desc: None,
            line_desc_ref: None,
            fill_desc: None,
            fill_desc_ref: None,
        });

        lower_standard_primitive(&context, &mut doc, &primitive, Affine2::identity()).unwrap();

        assert!(doc.arena.paths[0].is_filled());
        assert_eq!(doc.diagnostics.len(), 1, "{fill_property:?}");
    }
}

#[test]
fn zero_area_standard_primitive_emits_no_paths() {
    let ipc = Ipc2581::parse(
        r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581"><Content roleRef="Owner"><FunctionMode mode="FABRICATION"/></Content></IPC-2581>"#,
    )
    .unwrap();
    let context = ExtractContext::new(ipc.interner(), ipc.content(), Resolution::default());
    let mut doc = GeometryDocument::new();
    let primitive = ipc2581::types::StandardPrimitive::RectCenter(ipc2581::types::Styled {
        shape: ipc2581::types::RectCenter {
            size: ipc2581::types::Size {
                width: 0.0,
                height: 1.0,
            },
        },
        fill_property: None,
        line_desc: None,
        line_desc_ref: None,
        fill_desc: None,
        fill_desc_ref: None,
    });

    let void =
        lower_standard_primitive(&context, &mut doc, &primitive, Affine2::identity()).unwrap();

    assert!(!void);
    assert!(doc.arena.paths.is_empty());
    assert!(doc.arena.contours.is_empty());
    assert!(doc.arena.cmds.is_empty());
}

#[test]
fn a_curve_step_onto_its_own_center_lowers_as_a_straight_step() {
    let point = ipc2581::types::Point { x: 2.0, y: 1.0 };
    let cmds = poly_step_commands(
        Point::new(2.0, 1.0),
        &[PolyStep::Curve(ipc2581::types::PolyStepCurve {
            point,
            center: point,
            clockwise: false,
        })],
    );

    assert!(cmds.iter().all(|cmd| cmd.op != PathOp::ArcTo));
    assert_eq!(cmds.last().map(|cmd| cmd.p0), Some(Point::new(2.0, 1.0)));
}

#[test]
fn lowers_trace_poly_step_curves_as_arcs() {
    let mut doc = GeometryDocument::new();
    let trace = ipc2581::types::Trace {
        line_desc_ref: None,
        points: vec![
            ipc2581::types::ecad::TracePoint { x: 1.0, y: 0.0 },
            ipc2581::types::ecad::TracePoint { x: 0.0, y: 1.0 },
        ],
        steps: vec![PolyStep::Curve(ipc2581::types::PolyStepCurve {
            point: ipc2581::types::Point { x: 0.0, y: 1.0 },
            center: ipc2581::types::Point { x: 0.0, y: 0.0 },
            clockwise: false,
        })],
    };

    let feature = push_stroked_trace(
        &mut doc,
        StrokedFeatureStyle {
            net: None,
            polarity: GeometryPolarity::Dark,
            source: SourceRef::default(),
            width: 0.2,
            line_cap: LineCap::Round,
            line_pattern: LinePattern::Solid,
        },
        &trace,
    );

    assert_eq!(feature.paths.count, 1);
    assert_eq!(doc.arena.paths[0].bbox.min, Point::new(-0.1, -0.1));
    assert_eq!(doc.arena.paths[0].bbox.max, Point::new(1.1, 1.1));
    assert!(doc.arena.cmds.iter().any(|cmd| cmd.op == PathOp::ArcTo));
}

#[test]
fn lowers_feature_poly_step_curves_as_arcs() {
    let mut doc = GeometryDocument::new();
    let polyline = ipc2581::types::ecad::FeaturePolyline {
        begin: ipc2581::types::Point { x: 1.0, y: 0.0 },
        steps: vec![PolyStep::Curve(ipc2581::types::PolyStepCurve {
            point: ipc2581::types::Point { x: 0.0, y: 1.0 },
            center: ipc2581::types::Point { x: 0.0, y: 0.0 },
            clockwise: false,
        })],
        line_desc_ref: None,
        line_width: Some(0.2),
        line_end: Some(LineEnd::Round),
        line_property: None,
    };

    let ipc = Ipc2581::parse(
        r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581"><Content roleRef="Owner"><FunctionMode mode="FABRICATION"/></Content></IPC-2581>"#,
    )
    .unwrap();
    let feature = extract_feature_polyline(
        &ExtractContext::new(ipc.interner(), ipc.content(), Resolution::default()),
        None,
        GeometryPolarity::Dark,
        SourceRef::default(),
        &polyline,
        &mut doc,
    )
    .unwrap();

    assert_eq!(feature.paths.count, 1);
    assert_eq!(doc.arena.paths[0].bbox.min, Point::new(-0.1, -0.1));
    assert_eq!(doc.arena.paths[0].bbox.max, Point::new(1.1, 1.1));
    assert!(doc.arena.cmds.iter().any(|cmd| cmd.op == PathOp::ArcTo));
}

#[test]
fn lowers_hollow_user_circle_as_stroked_path() {
    let mut doc = GeometryDocument::new();
    let primitive = UserPrimitive::UserSpecial(ipc2581::types::UserSpecial {
        shapes: vec![ipc2581::types::UserShape {
            shape: UserShapeType::Circle(ipc2581::types::Circle { diameter: 1.4 }),
            line_desc: Some(ipc2581::types::LineDesc {
                line_width: 0.1,
                line_end: LineEnd::Round,
                line_property: None,
            }),
            line_desc_ref: None,
            fill_desc: Some(ipc2581::types::FillDesc {
                fill_property: FillProperty::Hollow,
                line_width: None,
                pitch1: None,
                pitch2: None,
                angle1: None,
                angle2: None,
                color: None,
            }),
            fill_desc_ref: None,
        }],
    });

    let ipc = Ipc2581::parse(
        r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581"><Content roleRef="Owner"><FunctionMode mode="FABRICATION"/></Content></IPC-2581>"#,
    )
    .unwrap();
    let context = ExtractContext::new(ipc.interner(), ipc.content(), Resolution::default());
    lower_user_primitive(&context, &mut doc, &primitive, Affine2::identity()).unwrap();

    assert_eq!(doc.arena.paths.len(), 1);
    assert!(doc.arena.paths[0].is_stroked());
    assert!(!doc.arena.paths[0].is_filled());
    assert_eq!(doc.arena.paths[0].stroke().unwrap().width, 0.1);
    assert_eq!(doc.arena.paths[0].bbox.min, Point::new(-0.75, -0.75));
    assert_eq!(doc.arena.paths[0].bbox.max, Point::new(0.75, 0.75));
    assert!(doc.arena.cmds.iter().any(|cmd| cmd.op == PathOp::ArcTo));
    assert!(!doc.arena.cmds.iter().any(|cmd| cmd.op == PathOp::CubicTo));
}

#[test]
fn strokes_without_a_line_description_are_reported_not_invented() {
    let ipc = Ipc2581::parse(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <DictionaryUser units="MILLIMETER">
      <EntryUser id="bare_line">
        <UserSpecial>
          <Line startX="0" startY="0" endX="4" endY="0"/>
        </UserSpecial>
      </EntryUser>
    </DictionaryUser>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP"/>
      <Step name="board" type="BOARD">
        <LayerFeature layerRef="TOP">
          <Set>
            <Features>
              <Location x="0" y="0"/>
              <UserPrimitiveRef id="bare_line"/>
            </Features>
          </Set>
          <Set>
            <Features>
              <Location x="0" y="0"/>
              <Line startX="0" startY="5" endX="4" endY="5">
                <LineDescRef id="absent"/>
              </Line>
            </Features>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
    )
    .unwrap();
    let resolution = Resolution::default();
    let imported = import_design(&ipc, resolution).unwrap();

    // Neither stroke becomes copper of a made-up width.
    assert!(
        imported
            .geometry
            .arena
            .paths
            .iter()
            .all(|path| !path.is_stroked()),
        "{:?}",
        imported.geometry.arena.paths
    );
    let messages = imported
        .geometry
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.as_str())
        .collect::<Vec<_>>();
    assert!(
        messages
            .iter()
            .any(|message| message.contains("it has no line description")),
        "{messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|message| message.contains("LineDesc 'absent' is missing")),
        "{messages:?}"
    );
}

#[test]
fn hollow_outline_width_scales_with_its_placement() {
    let ipc = Ipc2581::parse(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <DictionaryStandard units="MILLIMETER">
      <EntryStandard id="ring">
        <Circle diameter="2">
          <LineDesc lineWidth="0.1" lineEnd="ROUND"/>
          <FillDesc fillProperty="HOLLOW"/>
        </Circle>
      </EntryStandard>
    </DictionaryStandard>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP"/>
      <Step name="board" type="BOARD">
        <LayerFeature layerRef="TOP">
          <Set>
            <Pad>
              <Xform scale="2"/>
              <Location x="0" y="0"/>
              <StandardPrimitiveRef id="ring"/>
            </Pad>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
    )
    .unwrap();

    let doc = extract_layer(&ipc, "TOP", Resolution::default()).unwrap();
    let path = &doc.arena.paths[doc.features[0].paths.start as usize];
    assert!((path.stroke().unwrap().width - 0.2).abs() < 1e-12);
    assert!((path.bbox.width() - 4.2).abs() < 1e-9);
}

#[test]
fn lowers_user_special_lines_polylines_and_line_desc_refs() {
    let ipc = Ipc2581::parse(
        r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="Owner">
    <FunctionMode mode="FABRICATION"/>
    <DictionaryLineDesc units="MILLIMETER">
      <EntryLineDesc id="fine">
        <LineDesc lineWidth="0.15" lineEnd="NONE"/>
      </EntryLineDesc>
    </DictionaryLineDesc>
  </Content>
</IPC-2581>"#,
    )
    .unwrap();
    let entry = ipc.content().dictionary_line_desc.entries[0].clone();
    let context = ExtractContext::new(ipc.interner(), ipc.content(), Resolution::default());
    let mut doc = GeometryDocument::new();
    let primitive = UserPrimitive::UserSpecial(ipc2581::types::UserSpecial {
        shapes: vec![
            ipc2581::types::UserShape {
                shape: UserShapeType::Line(ipc2581::types::primitives::Line {
                    start: ipc2581::types::Point { x: 0.0, y: 0.0 },
                    end: ipc2581::types::Point { x: 1.0, y: 0.0 },
                }),
                line_desc: None,
                line_desc_ref: Some(entry.id),
                fill_desc: None,
                fill_desc_ref: None,
            },
            ipc2581::types::UserShape {
                shape: UserShapeType::Polyline(ipc2581::types::Polyline {
                    begin: ipc2581::types::Point { x: 1.0, y: 0.0 },
                    steps: vec![PolyStep::Curve(ipc2581::types::PolyStepCurve {
                        point: ipc2581::types::Point { x: 0.0, y: 1.0 },
                        center: ipc2581::types::Point { x: 0.0, y: 0.0 },
                        clockwise: false,
                    })],
                }),
                line_desc: None,
                line_desc_ref: Some(entry.id),
                fill_desc: None,
                fill_desc_ref: None,
            },
        ],
    });

    lower_user_primitive(&context, &mut doc, &primitive, Affine2::identity()).unwrap();

    assert_eq!(doc.arena.paths.len(), 2);
    assert!(doc.arena.paths.iter().all(|path| path.is_stroked()));
    assert!(
        doc.arena
            .paths
            .iter()
            .all(|path| path.stroke().unwrap().width == 0.15)
    );
    assert!(doc.arena.cmds.iter().any(|cmd| cmd.op == PathOp::ArcTo));
}

#[test]
fn extracts_inline_stroked_user_primitive_as_trace_feature() {
    let ipc = Ipc2581::parse(
        r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581"><Content roleRef="Owner"><FunctionMode mode="FABRICATION"/></Content></IPC-2581>"#,
    )
    .unwrap();
    let context = ExtractContext::new(ipc.interner(), ipc.content(), Resolution::default());
    let primitive = ipc2581::types::ecad::FeatureUserPrimitive {
        primitive: UserPrimitive::UserSpecial(ipc2581::types::UserSpecial {
            shapes: vec![ipc2581::types::UserShape {
                shape: UserShapeType::Line(ipc2581::types::primitives::Line {
                    start: ipc2581::types::Point { x: 0.0, y: 0.0 },
                    end: ipc2581::types::Point { x: 1.0, y: 0.0 },
                }),
                line_desc: Some(ipc2581::types::LineDesc {
                    line_width: 0.2,
                    line_end: LineEnd::Round,
                    line_property: None,
                }),
                line_desc_ref: None,
                fill_desc: None,
                fill_desc_ref: None,
            }],
        }),
        x: 10.0,
        y: 20.0,
    };
    let mut doc = GeometryDocument::new();

    let features = extract_inline_user_primitive(
        &context,
        None,
        GeometryPolarity::Dark,
        SourceRef::default(),
        &primitive,
        &mut doc,
    )
    .unwrap();

    assert_eq!(features.len(), 1);
    let feature = &features[0];
    assert_eq!(feature.bucket, FeatureBucket::Trace);
    assert_eq!(feature.paths.count, 1);
    assert!(doc.arena.paths[feature.paths.start as usize].is_stroked());
}

#[test]
fn contour_cutout_outside_its_outline_is_reported() {
    let mut doc = GeometryDocument::new();
    let mut contour = ipc2581::types::Contour {
        polygon: rect_polygon(0.0, 0.0, 10.0, 10.0),
        cutouts: vec![rect_polygon(2.0, 2.0, 4.0, 4.0)],
    };
    push_contour_path(&mut doc, &contour, Affine2::identity());
    assert!(doc.diagnostics.is_empty());

    contour.cutouts.push(rect_polygon(8.0, 8.0, 12.0, 9.0));
    push_contour_path(&mut doc, &contour, Affine2::identity());
    assert_eq!(doc.diagnostics.len(), 1);
}

#[test]
fn lowers_inline_user_contour_as_compound_path_at_feature_location() {
    let ipc = Ipc2581::parse(
        r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581"><Content roleRef="Owner"><FunctionMode mode="FABRICATION"/></Content></IPC-2581>"#,
    )
    .unwrap();
    let context = ExtractContext::new(ipc.interner(), ipc.content(), Resolution::default());
    let primitive = ipc2581::types::ecad::FeatureUserPrimitive {
        primitive: UserPrimitive::UserSpecial(ipc2581::types::UserSpecial {
            shapes: vec![ipc2581::types::UserShape {
                shape: UserShapeType::Contour(ipc2581::types::Contour {
                    polygon: rect_polygon(0.0, 0.0, 2.0, 2.0),
                    cutouts: vec![rect_polygon(0.5, 0.5, 1.5, 1.5)],
                }),
                line_desc: None,
                line_desc_ref: None,
                fill_desc: None,
                fill_desc_ref: None,
            }],
        }),
        x: 10.0,
        y: 20.0,
    };
    let mut doc = GeometryDocument::new();

    let features = extract_inline_user_primitive(
        &context,
        None,
        GeometryPolarity::Dark,
        SourceRef::default(),
        &primitive,
        &mut doc,
    )
    .unwrap();

    assert_eq!(features.len(), 1);
    let feature = &features[0];
    assert_eq!(feature.paths.count, 1);
    assert_eq!(doc.arena.paths[0].fill_rule(), Some(FillRule::EvenOdd));
    assert_eq!(doc.arena.paths[0].contours.count, 2);
    assert_eq!(doc.arena.paths[0].bbox.min, Point::new(10.0, 20.0));
    assert_eq!(doc.arena.paths[0].bbox.max, Point::new(12.0, 22.0));
    let image = ContourSet::from_painted_paths(&doc.arena, &doc.arena.paths, Resolution::default())
        .unwrap();
    assert!((image.area() - 3.0).abs() < 1e-9);
    assert!(!image.contains_point(Point::new(11.0, 21.0)));
}

#[test]
fn user_special_voids_clear_only_preceding_fills_in_their_own_scope() {
    let contour = |x0, y0, x1, y1, style: &str| {
        format!(
            "<Contour><Polygon><PolyBegin x='{x0}' y='{y0}'/>
             <PolyStepSegment x='{x1}' y='{y0}'/><PolyStepSegment x='{x1}' y='{y1}'/>
             <PolyStepSegment x='{x0}' y='{y1}'/><PolyStepSegment x='{x0}' y='{y0}'/>
             {style}</Polygon></Contour>"
        )
    };
    let nested = format!(
        "<UserSpecial>{}
             <Line startX='1.2' startY='3.6' endX='4.8' endY='3.6'>
               <LineDesc lineWidth='0.1' lineEnd='NONE'/>
             </Line>{}{}</UserSpecial>",
        contour(0.0, 0.0, 8.0, 6.0, ""),
        contour(1.0, 1.0, 5.0, 4.0, "<FillDescRef id='void'/>"),
        contour(2.0, 2.0, 3.0, 3.0, ""),
    );
    // Both direct nesting and dictionary references must establish a scope.
    for child in [&nested, "<UserPrimitiveRef id='nested'/>"] {
        let ipc = Ipc2581::parse(&format!(
            "<IPC-2581 revision='C' xmlns='http://webstds.ipc.org/2581'>
                 <Content roleRef='owner'><FunctionMode mode='FABRICATION'/>
                   <StepRef name='board'/><LayerRef name='TOP'/>
                   <DictionaryFillDesc units='MILLIMETER'>
                     <EntryFillDesc id='void'><FillDesc fillProperty='VOID'/></EntryFillDesc>
                   </DictionaryFillDesc>
                   <DictionaryUser units='MILLIMETER'><EntryUser id='nested'>{nested}</EntryUser></DictionaryUser>
                 </Content><Ecad><CadHeader units='MILLIMETER'/><CadData>
                 <Layer name='TOP' layerFunction='SIGNAL' side='TOP' polarity='POSITIVE'/>
                 <Step name='board' type='BOARD'><LayerFeature layerRef='TOP'><Set>
                   <Features><UserSpecial>{}</UserSpecial></Features>
                   <Features><UserSpecial>{}{child}</UserSpecial></Features>
                 </Set></LayerFeature></Step></CadData></Ecad></IPC-2581>",
            contour(1.1, 1.1, 1.4, 1.4, ""),
            contour(1.5, 1.1, 1.8, 1.4, ""),
        )).unwrap();
        let resolution = Resolution::default();
        let mut doc = extract_layer(&ipc, "TOP", resolution).unwrap();
        process::normalize_for_artwork(&mut doc, resolution).unwrap();
        let image = ContourSet::from_painted_paths(
            &doc.arena,
            doc.features
                .iter()
                .flat_map(|feature| feature.paths.slice(&doc.arena.paths))
                .filter(|path| path.is_filled()),
            resolution,
        )
        .unwrap();
        // Plate minus void, plus later island and two independent earlier squares.
        assert!(
            (image.area() - 37.18).abs() < 1e-6,
            "area: {}",
            image.area()
        );
        for point in [
            Point::new(1.2, 1.2),
            Point::new(1.6, 1.2),
            Point::new(2.5, 2.5),
        ] {
            assert!(image.contains_point(point));
        }
        assert!(!image.contains_point(Point::new(4.0, 2.0)));
        let strokes = doc
            .features
            .iter()
            .flat_map(|feature| feature.paths.slice(&doc.arena.paths))
            .filter(|path| path.is_stroked())
            .collect::<Vec<_>>();
        assert_eq!(strokes.len(), 1);
        assert!((strokes[0].bbox.width() - 3.7).abs() < 1e-9);
    }
}

#[test]
fn splits_mixed_inline_user_primitive_into_trace_and_fill_features() {
    let ipc = Ipc2581::parse(
        r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581"><Content roleRef="Owner"><FunctionMode mode="FABRICATION"/></Content></IPC-2581>"#,
    )
    .unwrap();
    let context = ExtractContext::new(ipc.interner(), ipc.content(), Resolution::default());
    let primitive = ipc2581::types::ecad::FeatureUserPrimitive {
        primitive: UserPrimitive::UserSpecial(ipc2581::types::UserSpecial {
            shapes: vec![
                ipc2581::types::UserShape {
                    shape: UserShapeType::Line(ipc2581::types::primitives::Line {
                        start: ipc2581::types::Point { x: 0.0, y: 0.0 },
                        end: ipc2581::types::Point { x: 2.0, y: 0.0 },
                    }),
                    line_desc: Some(ipc2581::types::LineDesc {
                        line_width: 0.2,
                        line_end: LineEnd::Round,
                        line_property: None,
                    }),
                    line_desc_ref: None,
                    fill_desc: None,
                    fill_desc_ref: None,
                },
                ipc2581::types::UserShape {
                    shape: UserShapeType::Contour(ipc2581::types::Contour {
                        polygon: rect_polygon(0.0, 1.0, 2.0, 3.0),
                        cutouts: Vec::new(),
                    }),
                    line_desc: None,
                    line_desc_ref: None,
                    fill_desc: None,
                    fill_desc_ref: None,
                },
            ],
        }),
        x: 10.0,
        y: 20.0,
    };
    let mut doc = GeometryDocument::new();

    let features = extract_inline_user_primitive(
        &context,
        None,
        GeometryPolarity::Dark,
        SourceRef::default(),
        &primitive,
        &mut doc,
    )
    .unwrap();

    assert_eq!(features.len(), 2);
    assert_eq!(features[0].bucket, FeatureBucket::Trace);
    assert_eq!(features[0].paths.count, 1);
    assert_eq!(features[1].bucket, FeatureBucket::Fill);
    assert_eq!(features[1].paths.count, 1);
    assert!(doc.arena.paths[features[0].paths.start as usize].is_stroked());
    assert!(doc.arena.paths[features[1].paths.start as usize].is_filled());
}

#[test]
fn lowers_butterfly_with_removed_quadrants() {
    let mut doc = GeometryDocument::new();

    push_butterfly_path(
        &mut doc,
        Affine2::identity(),
        ipc2581::types::ButterflyShape::Square,
        4.0,
    );
    push_butterfly_path(
        &mut doc,
        Affine2::identity(),
        ipc2581::types::ButterflyShape::Round,
        4.0,
    );

    assert_eq!(doc.arena.paths.len(), 2);
    assert_eq!(doc.arena.paths[0].contours.count, 2);
    assert_eq!(doc.arena.paths[1].contours.count, 2);
    assert!(doc.arena.cmds.iter().any(|cmd| cmd.op == PathOp::ArcTo));
}

fn rect_polygon(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> ipc2581::types::Polygon {
    ipc2581::types::Polygon {
        begin: ipc2581::types::Point { x: min_x, y: min_y },
        steps: vec![
            PolyStep::Segment(ipc2581::types::PolyStepSegment {
                point: ipc2581::types::Point { x: max_x, y: min_y },
            }),
            PolyStep::Segment(ipc2581::types::PolyStepSegment {
                point: ipc2581::types::Point { x: max_x, y: max_y },
            }),
            PolyStep::Segment(ipc2581::types::PolyStepSegment {
                point: ipc2581::types::Point { x: min_x, y: max_y },
            }),
            PolyStep::Segment(ipc2581::types::PolyStepSegment {
                point: ipc2581::types::Point { x: min_x, y: min_y },
            }),
        ],
    }
}

fn thermal(
    shape: ConcentricShape,
    spoke_count: u32,
    spoke_width: Option<f64>,
) -> ipc2581::types::Thermal {
    ipc2581::types::Thermal {
        shape,
        outer_diameter: 10.0,
        inner_diameter: 6.0,
        spoke_count,
        spoke_width,
        spoke_start_angle: Some(0.0),
    }
}

fn thermal_image(doc: &GeometryDocument) -> ContourSet {
    ContourSet::from_painted_paths(&doc.arena, &doc.arena.paths, Resolution::default()).unwrap()
}

#[test]
fn lowers_thermal_as_ring_interrupted_by_spoke_gaps() {
    let mut doc = GeometryDocument::new();
    push_thermal_path(
        &mut doc,
        Affine2::identity(),
        &thermal(ConcentricShape::Round, 4, Some(2.0)),
        Resolution::default(),
    )
    .unwrap();

    let image = thermal_image(&doc);
    let diagonal = 4.0 * std::f64::consts::FRAC_1_SQRT_2;
    assert!(image.contains_point(Point::new(diagonal, diagonal)));
    assert!(image.contains_point(Point::new(-diagonal, diagonal)));
    assert!(!image.contains_point(Point::new(4.0, 0.0)));
    assert!(!image.contains_point(Point::new(0.0, -4.0)));
    assert!(!image.contains_point(Point::new(0.0, 0.0)));
    assert_eq!(image.connected_components().len(), 4);
}

#[test]
fn spokeless_thermal_is_exactly_its_donut() {
    let mut thermal_doc = GeometryDocument::new();
    push_thermal_path(
        &mut thermal_doc,
        Affine2::identity(),
        &thermal(ConcentricShape::Round, 0, Some(2.0)),
        Resolution::default(),
    )
    .unwrap();
    let mut donut_doc = GeometryDocument::new();
    push_ring_path(
        &mut donut_doc,
        Affine2::identity(),
        ConcentricShape::Round,
        10.0,
        6.0,
    );

    assert_eq!(thermal_doc.arena.paths, donut_doc.arena.paths);
    assert_eq!(thermal_doc.arena.cmds, donut_doc.arena.cmds);
    assert_eq!(
        thermal_doc.arena.paths[0].fill_rule(),
        Some(FillRule::EvenOdd)
    );
    assert_eq!(thermal_doc.arena.paths[0].contours.count, 2);
}

#[test]
fn thermal_spokes_default_to_the_ring_width_at_45_degrees() {
    let mut doc = GeometryDocument::new();
    push_thermal_path(
        &mut doc,
        Affine2::identity(),
        &ipc2581::types::Thermal {
            spoke_start_angle: None,
            ..thermal(ConcentricShape::Round, 4, None)
        },
        Resolution::default(),
    )
    .unwrap();

    // Cuts of outer - inner = 4 mm on the diagonals leave the axes.
    let image = thermal_image(&doc);
    let diagonal = 4.0 * std::f64::consts::FRAC_1_SQRT_2;
    assert!(image.contains_point(Point::new(4.0, 0.0)));
    assert!(image.contains_point(Point::new(0.0, 4.0)));
    assert!(!image.contains_point(Point::new(diagonal, diagonal)));
    assert!(!image.contains_point(Point::new(diagonal - 1.2, diagonal + 1.2)));
    assert!(doc.diagnostics.is_empty());
}

#[test]
fn donut_and_thermal_rings_follow_their_shape() {
    for (shape, corner, flat) in [
        (ConcentricShape::Round, false, true),
        (ConcentricShape::Square, true, true),
        // Vertex down: the ring reaches 5 mm along Y but only 4.33 mm along X.
        (ConcentricShape::Hexagon, false, false),
    ] {
        let mut doc = GeometryDocument::new();
        push_ring_path(&mut doc, Affine2::identity(), shape, 10.0, 6.0);
        let image = thermal_image(&doc);

        assert_eq!(
            image.contains_point(Point::new(4.5, 4.5)),
            corner,
            "{shape:?}"
        );
        assert_eq!(
            image.contains_point(Point::new(4.8, 0.0)),
            flat,
            "{shape:?}"
        );
        assert!(image.contains_point(Point::new(0.0, -4.8)), "{shape:?}");
        assert!(!image.contains_point(Point::new(0.0, 0.0)), "{shape:?}");
    }

    let mut doc = GeometryDocument::new();
    push_thermal_path(
        &mut doc,
        Affine2::identity(),
        &thermal(ConcentricShape::Square, 4, Some(2.0)),
        Resolution::default(),
    )
    .unwrap();
    let image = thermal_image(&doc);
    assert!(image.contains_point(Point::new(4.5, 4.5)));
    assert!(!image.contains_point(Point::new(4.5, 0.0)));
}

#[test]
fn extracts_panel_and_repeated_layer_instances() {
    let ipc = ipc2581::Ipc2581::parse(panel_layer_fixture())
        .expect("synthetic panel fixture should parse");
    let doc =
        extract_layer(&ipc, "TOP", Resolution::default()).expect("panel layer should extract");
    let layer = &doc.layers[0];
    let features = layer.features.slice(&doc.features);

    let (_, root_step) = root_step(&doc).unwrap();
    assert_eq!(root_step.kind, LayoutStepKind::Panel);
    assert_eq!(features.len(), 3);
    assert_eq!(features[0].center, Point::new(40.0, 5.0));
    assert_eq!(features[1].center, Point::new(12.0, 23.0));
    assert_eq!(features[2].center, Point::new(27.0, 23.0));
    assert_eq!(features[0].source.set_index, 0);
    assert_eq!(features[1].source.set_index, 1);
    assert_eq!(features[2].source.set_index, 2);
    assert_eq!(layer.bbox.min, Point::new(11.5, 4.5));
    assert_eq!(layer.bbox.max, Point::new(40.5, 23.5));
    assert_eq!(board_step_count(&doc), 1);
    assert_eq!(panel_step_count(&doc), 1);
    assert_eq!(board_instance_count(&doc), 2);
    let simple_array = simple_board_array_layout(&doc).unwrap();
    assert_eq!(simple_array.columns, 2);
    assert_eq!(simple_array.rows, 1);
    assert_eq!(simple_array.board_step, 1);
    assert_eq!(simple_array.board_width, 10.0);
    assert_eq!(simple_array.board_height, 5.0);
    assert_eq!(board_bbox(&doc).unwrap().min, Point::new(0.0, 0.0));
    assert_eq!(board_bbox(&doc).unwrap().max, Point::new(10.0, 5.0));
    assert_eq!(panel_bbox(&doc).unwrap().min, Point::new(0.0, 0.0));
    assert_eq!(panel_bbox(&doc).unwrap().max, Point::new(100.0, 80.0));
    assert_eq!(doc.layout.instances[0].bbox.min, Point::new(10.0, 20.0));
    assert_eq!(doc.layout.instances[0].bbox.max, Point::new(20.0, 25.0));
    assert_eq!(doc.layout.instances[1].bbox.min, Point::new(25.0, 20.0));
    assert_eq!(doc.layout.instances[1].bbox.max, Point::new(35.0, 25.0));
    assert_eq!(doc.layout.steps.len(), 2);
    assert_eq!(doc.layout.repeats.len(), 1);
    assert_eq!(doc.layout.instances.len(), 2);
    assert_eq!(doc.layout.root_step, Some(0));
    assert_eq!(doc.layout.steps[0].kind, LayoutStepKind::Panel);
    assert_eq!(doc.layout.steps[1].kind, LayoutStepKind::Board);
    assert_eq!(doc.layout.repeats[0].instances.start, 0);
    assert_eq!(doc.layout.repeats[0].instances.count, 2);
    assert_eq!(doc.layout.instances[0].repeat_index_x, 0);
    assert_eq!(doc.layout.instances[1].repeat_index_x, 1);
    assert_eq!(doc.layout.instances[1].transform.m02, 25.0);
}

#[test]
fn imported_design_owns_strings_and_reuses_step_local_geometry() {
    let imported = {
        let ipc = ipc2581::Ipc2581::parse(panel_layer_fixture())
            .expect("synthetic panel fixture should parse");
        import_design(&ipc, Resolution::default()).expect("complete design should import")
    };

    let top = imported.layer_id("TOP").unwrap();
    assert_eq!(
        imported.resolve(imported.layer_definitions[top.0 as usize].name),
        "TOP"
    );
    assert_eq!(imported.step_layers.len(), 2);
    assert_eq!(
        imported
            .step_layers
            .iter()
            .map(|step_layer| {
                imported.geometry.layers[step_layer.document_layer as usize]
                    .features
                    .count
            })
            .sum::<u32>(),
        2,
        "the panel and board each retain one local feature definition"
    );

    let occurrences = imported
        .feature_occurrences(top, ArtworkScope::ArrayFlattened)
        .unwrap();
    assert_eq!(occurrences.len(), 3);
    assert_eq!(
        occurrences
            .iter()
            .map(|occurrence| occurrence.id.feature)
            .collect::<BTreeSet<_>>()
            .len(),
        2,
        "repeated boards create occurrences, not cloned definitions"
    );
    assert_eq!(
        occurrences
            .iter()
            .map(|occurrence| occurrence.id)
            .collect::<BTreeSet<_>>()
            .len(),
        3
    );
}

#[test]
fn imported_design_carries_global_bom_and_package_associations() {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="ASSEMBLY"/>
    <StepRef name="board-a"/>
    <StepRef name="board-b"/>
    <BomRef name="bom-a"/>
    <BomRef name="bom-b"/>
    <DictionaryLineDesc units="MILLIMETER">
      <EntryLineDesc id="line"><LineDesc lineWidth="0" lineEnd="ROUND"/></EntryLineDesc>
    </DictionaryLineDesc>
  </Content>
  <LogisticHeader>
    <Role id="owner" roleFunction="OWNER"/>
    <Enterprise id="maker" code="maker" name="Maker"/>
    <Person name="Engineer" enterpriseRef="maker" roleRef="owner"/>
  </LogisticHeader>
  <HistoryRecord number="1" origination="2026-01-01T00:00:00Z" software="test" lastChange="2026-01-01T00:00:00Z">
    <FileRevision fileRevisionId="1" comment="test">
      <SoftwarePackage name="test" vendor="test" revision="1"><Certification certificationStatus="SELFTEST"/></SoftwarePackage>
    </FileRevision>
  </HistoryRecord>
  <Bom name="bom-a">
    <BomHeader assembly="a" revision="1"><StepRef name="board-a"/></BomHeader>
    <BomItem OEMDesignNumberRef="part-a" quantity="1" category="ELECTRICAL">
      <RefDes name="U1" packageRef="pkg-a" populate="0" layerRef="TOP"/>
      <Characteristics category="ELECTRICAL"/>
    </BomItem>
  </Bom>
  <Bom name="bom-b">
    <BomHeader assembly="b" revision="1"><StepRef name="board-b"/></BomHeader>
    <BomItem OEMDesignNumberRef="part-b" quantity="1" category="ELECTRICAL">
      <RefDes name="U3" packageRef="pkg-b" layerRef="TOP"/>
      <Characteristics category="ELECTRICAL"/>
    </BomItem>
  </Bom>
  <Bom name="not-selected">
    <BomHeader assembly="other" revision="1"><StepRef name="board-a"/></BomHeader>
    <BomItem OEMDesignNumberRef="other" quantity="1" category="ELECTRICAL">
      <RefDes name="U4" packageRef="pkg-a" populate="1" layerRef="TOP"/>
      <Characteristics category="ELECTRICAL"/>
    </BomItem>
  </Bom>
  <Ecad name="design">
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Step name="board-a" type="BOARD">
        <Datum x="0" y="0"/>
        <Package name="pkg-a" type="OTHER" pinOneOrientation="OTHER"><Outline><Polygon><PolyBegin x="0" y="0"/><PolyStepSegment x="0" y="0"/></Polygon><LineDescRef id="line"/></Outline></Package>
        <Package name="shared-package" type="OTHER" pinOneOrientation="OTHER"><Outline><Polygon><PolyBegin x="0" y="0"/><PolyStepSegment x="0" y="0"/></Polygon><LineDescRef id="line"/></Outline></Package>
        <Component refDes="U1" packageRef="pkg-a" part="part-a" layerRef="TOP" mountType="SMT"><Location x="1" y="1"/></Component>
        <Component refDes="U4" packageRef="pkg-a" part="other" layerRef="TOP" mountType="SMT"><Location x="4" y="4"/></Component>
      </Step>
      <Step name="board-b" type="BOARD">
        <Datum x="0" y="0"/>
        <Package name="pkg-b" type="OTHER" pinOneOrientation="OTHER"><Outline><Polygon><PolyBegin x="0" y="0"/><PolyStepSegment x="0" y="0"/></Polygon><LineDescRef id="line"/></Outline></Package>
        <Component refDes="U3" packageRef="pkg-b" part="part-b" layerRef="TOP" mountType="SMT"><Location x="2" y="2"/></Component>
        <Component refDes="U1" packageRef="pkg-b" part="part-b-u1" layerRef="TOP" mountType="SMT"><Location x="2.5" y="2.5"/></Component>
        <Component packageRef="shared-package" part="shared-part" layerRef="TOP" mountType="SMT"><Location x="3" y="3"/></Component>
      </Step>
    </CadData>
  </Ecad>
  <Avl name="parts">
    <AvlHeader title="parts" source="test" author="test" datetime="2026-01-01T00:00:00Z" version="1"/>
    <AvlItem OEMDesignNumber="part-a"/>
  </Avl>
</IPC-2581>"#;
    ipc2581::validate(xml).expect("association fixture conforms to IPC-2581C");
    let ipc = Ipc2581::parse(xml).unwrap();

    let imported = import_design(&ipc, Resolution::default()).unwrap();
    assert_eq!(imported.boms.len(), 3);
    assert!(imported.logistic_header.is_some());
    assert!(imported.avl.is_some());
    assert_eq!(imported.packages.len(), 3);
    assert_eq!(imported.components.len(), 5);

    let assembly = imported
        .assembly_document(crate::dialects::assembly::Scope::BoardArray)
        .unwrap();
    assert_eq!(assembly.primary_bom, Some(0));
    assert_eq!(assembly.boms.len(), 3);
    assert_eq!(assembly.packages.len(), 3);
    assert_eq!(assembly.components.len(), 5);
    assert_eq!(assembly.occurrences.len(), 2);
    assert_eq!(assembly.avl.as_ref().unwrap().items.len(), 1);
    let assembly_a = assembly
        .components
        .iter()
        .find(|component| component.part == "part-a")
        .unwrap();
    assert_eq!(assembly_a.side, crate::dialects::assembly::Side::Top);
    assert_eq!(assembly_a.mount, crate::dialects::assembly::Mount::Smt);
    assert_eq!(
        assembly_a.population,
        crate::dialects::assembly::Population::DoNotPopulate
    );
    let reference = assembly.preferred_bom_reference(assembly_a).unwrap();
    assert_eq!(assembly.bom_designator(reference).name, "U1");
    assert_eq!(
        assembly.bom_item(reference).category,
        Some(crate::dialects::assembly::BomCategory::Electrical)
    );

    let a = imported
        .components
        .iter()
        .find(|component| imported.resolve(component.source.part) == "part-a")
        .unwrap();
    assert_eq!(a.population, PopulationState::DoNotPopulate);
    assert_eq!(a.bom_references.len(), 1);
    let a_package = imported.package_definition(a.package.unwrap()).unwrap();
    assert_eq!(imported.resolve(a_package.source.name), "pkg-a");
    assert_eq!(
        imported
            .bom_reference(a.bom_references[0])
            .unwrap()
            .populate,
        Some(false)
    );
    assert_eq!(
        imported.resolve(
            imported
                .bom_item(a.bom_references[0])
                .unwrap()
                .oem_design_number_ref
        ),
        "part-a"
    );

    let b = imported
        .components
        .iter()
        .find(|component| imported.resolve(component.source.part) == "part-b")
        .unwrap();
    assert_eq!(b.population, PopulationState::Unspecified);
    assert_eq!(b.bom_references.len(), 1);
    assert_eq!(
        imported
            .bom_reference(b.bom_references[0])
            .unwrap()
            .populate,
        None
    );

    let repeated_refdes = imported
        .components
        .iter()
        .find(|component| imported.resolve(component.source.part) == "part-b-u1")
        .unwrap();
    assert_eq!(repeated_refdes.population, PopulationState::Unspecified);
    assert!(repeated_refdes.bom_references.is_empty());

    let unselected = imported
        .components
        .iter()
        .find(|component| imported.resolve(component.source.part) == "other")
        .unwrap();
    assert_eq!(unselected.population, PopulationState::Populate);
    assert_eq!(unselected.bom_references.len(), 1);
    assert_eq!(
        imported.resolve(
            imported
                .bom_item(unselected.bom_references[0])
                .unwrap()
                .oem_design_number_ref
        ),
        "other"
    );

    let shared = imported
        .components
        .iter()
        .find(|component| imported.resolve(component.source.part) == "shared-part")
        .unwrap();
    let shared_package = imported
        .package_definition(shared.package.unwrap())
        .unwrap();
    assert_eq!(
        imported.resolve(shared_package.source.name),
        "shared-package"
    );
    assert_ne!(shared.step, shared_package.step);
}

#[test]
fn components_bind_to_the_package_of_their_own_step() {
    let package = r#"<Package name="R_0402" type="OTHER" pinOneOrientation="OTHER"><Outline><Polygon><PolyBegin x="0" y="0"/><PolyStepSegment x="0" y="0"/></Polygon><LineDesc lineWidth="0.1" lineEnd="ROUND"/></Outline></Package>
        <Component refDes="R1" packageRef="R_0402" part="r" layerRef="TOP" mountType="SMT"><Location x="1" y="1"/></Component>"#;
    let ipc = Ipc2581::parse(&format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner"><FunctionMode mode="ASSEMBLY"/><StepRef name="board-a"/><StepRef name="board-b"/></Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP"/>
      <Step name="board-a" type="BOARD">{package}</Step>
      <Step name="board-b" type="BOARD">{package}</Step>
    </CadData>
  </Ecad>
</IPC-2581>"#
    ))
    .unwrap();
    let imported = import_design(&ipc, Resolution::default()).unwrap();

    assert_eq!(imported.components.len(), 2);
    for component in &imported.components {
        let package = imported
            .package_definition(component.package.unwrap())
            .unwrap();
        assert_eq!(package.step, component.step);
    }
}

#[test]
fn board_scope_rejects_an_unreachable_board_definition() {
    let ipc = Ipc2581::parse(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner"><FunctionMode mode="FABRICATION"/><StepRef name="panel"/></Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP"/>
      <Step name="unrelated-board" type="BOARD"/>
      <Step name="panel" type="PALLET"/>
    </CadData>
  </Ecad>
</IPC-2581>"#,
    )
    .unwrap();
    let imported = import_design(&ipc, Resolution::default()).unwrap();
    let top = imported.layer_id("TOP").unwrap();

    let error = imported
        .materialize_layer(top, ArtworkScope::Board)
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("primary step 'panel' does not reference a board step"),
        "unexpected error: {error}"
    );
}

#[test]
fn import_reports_features_it_had_to_drop() {
    let ipc = Ipc2581::parse(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP"/>
      <Step name="board" type="BOARD">
        <LayerFeature layerRef="TOP">
          <Set><Features><Location x="0" y="0"/><StandardPrimitiveRef id="absent"/></Features></Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
    )
    .unwrap();
    let imported = import_design(&ipc, Resolution::default()).unwrap();

    // The layer's only feature is gone, so nothing else can say why.
    assert!(
        imported
            .geometry
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("'absent' is missing")),
        "diagnostics: {:?}",
        imported.geometry.diagnostics
    );
}

#[test]
fn non_finite_source_numbers_never_reach_the_arena() {
    let ipc = Ipc2581::parse(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner"><FunctionMode mode="FABRICATION"/><StepRef name="board"/></Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP"/>
      <Step name="board" type="BOARD">
        <LayerFeature layerRef="TOP">
          <Set>
            <Pad><Location x="1" y="1"/><Circle diameter="1"/></Pad>
            <Pad><Location x="5" y="1"/><Circle diameter="1"/></Pad>
            <Pad><Location x="9" y="1"/><Circle diameter="1"/></Pad>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
    )
    .unwrap();
    // Other producers hand the importer a typed model no parser vetted.
    let cad = &ipc.ecad().unwrap().cad_data;
    let mut step = cad.steps[0].clone();
    let [
        SetFeature::Pad(nan),
        SetFeature::Pad(_),
        SetFeature::Pad(infinite),
    ] = step.layer_features[0].sets[0].features.as_mut_slice()
    else {
        panic!("fixture pads");
    };
    nan.x = Some(f64::NAN);
    let Some(FeatureShape::StandardPrimitive(StandardPrimitive::Circle(circle))) =
        &mut infinite.feature
    else {
        panic!("fixture circle");
    };
    circle.shape.diameter = f64::INFINITY;

    let doc = extract_step_layer_local(
        &ipc,
        &step,
        &cad.layers,
        &cad.layers[0],
        "TOP",
        Resolution::default(),
    )
    .unwrap();

    assert_eq!(doc.features.len(), 1);
    assert_eq!(doc.diagnostics.len(), 2, "{:?}", doc.diagnostics);
    assert!(doc.arena.cmds.iter().all(|cmd| cmd.is_finite()));
    assert_eq!(doc.arena.paths.len(), 1);
    assert_eq!(doc.layers[0].bbox.min, Point::new(4.5, 0.5));
    assert_eq!(doc.layers[0].bbox.max, Point::new(5.5, 1.5));
}

#[test]
fn flattened_nested_panels_preserve_depth_first_paint_order() {
    let ipc = Ipc2581::parse(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="root"/>
    <DictionaryStandard units="MILLIMETER">
      <EntryStandard id="dot"><Circle diameter="2"/></EntryStandard>
    </DictionaryStandard>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <LayerFeature layerRef="TOP">
          <Set polarity="NEGATIVE"><Features><Location x="0" y="0"/><StandardPrimitiveRef id="dot"/></Features></Set>
        </LayerFeature>
      </Step>
      <Step name="cell" type="PALLET">
        <StepRepeat stepRef="board" x="10" y="0" nx="1" ny="1" dx="0" dy="0"/>
        <LayerFeature layerRef="TOP">
          <Set><Features><Location x="0" y="0"/><StandardPrimitiveRef id="dot"/></Features></Set>
        </LayerFeature>
      </Step>
      <Step name="root" type="PALLET">
        <StepRepeat stepRef="cell" x="0" y="0" nx="2" ny="1" dx="10" dy="0"/>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
    )
    .unwrap();
    let imported = import_design(&ipc, Resolution::default()).unwrap();
    let top = imported.layer_id("TOP").unwrap();
    let document = imported
        .materialize_layer(top, ArtworkScope::ArrayFlattened)
        .unwrap();

    assert_eq!(
        document
            .features
            .iter()
            .map(|feature| (feature.center.x, feature.polarity))
            .collect::<Vec<_>>(),
        vec![
            (0.0, GeometryPolarity::Dark),
            (10.0, GeometryPolarity::Clear),
            (10.0, GeometryPolarity::Dark),
            (20.0, GeometryPolarity::Clear),
        ]
    );
    let image = imported
        .composed_layer_image(top, ArtworkScope::ArrayFlattened, Resolution::default())
        .unwrap();
    assert!(image.contains_point(Point::new(10.0, 0.0)));
}

#[test]
fn component_occurrence_ids_survive_mirrored_board_repeats() {
    let ipc = Ipc2581::parse(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner"><FunctionMode mode="ASSEMBLY"/><StepRef name="panel"/></Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="COMPONENT_TOP" side="TOP"/>
      <Step name="board" type="BOARD">
        <Component refDes="U1" packageRef="pkg" part="part" layerRef="TOP" mountType="SMT">
          <Location x="1" y="2"/>
        </Component>
      </Step>
      <Step name="panel" type="PALLET">
        <StepRepeat stepRef="board" x="10" y="20" nx="2" ny="1" dx="20" dy="0" angle="90" mirror="true"/>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
    )
    .unwrap();
    let imported = import_design(&ipc, Resolution::default()).unwrap();

    let occurrences = imported
        .component_occurrences(ArtworkScope::ArrayFlattened)
        .unwrap();
    assert_eq!(occurrences.len(), 2);
    assert_ne!(occurrences[0].id, occurrences[1].id);
    assert_eq!(occurrences[0].id.component, occurrences[1].id.component);
    assert!((occurrences[0].root_from_component.m02 - 12.0).abs() < 1e-9);
    assert!((occurrences[1].root_from_component.m02 - 32.0).abs() < 1e-9);
    for occurrence in occurrences {
        assert!((occurrence.root_from_component.m12 - 21.0).abs() < 1e-9);
        let board_local = occurrence.board_from_component.unwrap();
        assert!((board_local.m02 - 1.0).abs() < 1e-9);
        assert!((board_local.m12 - 2.0).abs() < 1e-9);
    }
}

#[test]
fn extracts_layer_for_geometry_view_board_or_board_array() {
    let ipc = ipc2581::Ipc2581::parse(panel_layer_fixture())
        .expect("synthetic panel fixture should parse");

    let board = extract_layer_for_view(&ipc, "TOP", ArtworkScope::Board, Resolution::default())
        .expect("board layer should extract");
    let board_layer = &board.layers[0];
    let board_features = board_layer.features.slice(&board.features);

    assert_eq!(board_features.len(), 1);
    assert_eq!(board_features[0].center, Point::new(2.0, 3.0));
    assert_eq!(board.layout.steps.len(), 1);
    assert_eq!(board.layout.root_step, Some(0));
    assert_eq!(board.layout.steps[0].kind, LayoutStepKind::Board);
    assert!(board.layout.instances.is_empty());
    assert_eq!(
        profile_occurrences_for(&board, ProfileSet::BoardOutlines).len(),
        1
    );

    let panel = extract_layer_for_view(
        &ipc,
        "TOP",
        ArtworkScope::ArrayFlattened,
        Resolution::default(),
    )
    .expect("panel layer should extract");
    let panel_layer = &panel.layers[0];
    let panel_features = panel_layer.features.slice(&panel.features);

    assert_eq!(panel_features.len(), 3);
    assert_eq!(panel_features[0].center, Point::new(40.0, 5.0));
    assert_eq!(panel_features[1].center, Point::new(12.0, 23.0));
    assert_eq!(panel_features[2].center, Point::new(27.0, 23.0));
    assert_eq!(panel.layout.steps.len(), 2);
    assert_eq!(panel.layout.instances.len(), 2);
    assert_eq!(
        profile_occurrences_for(&panel, ProfileSet::FabricationOutlines).len(),
        3
    );
}

#[test]
fn step_only_panel_extraction_omits_repeat_graph_expansion() {
    let ipc = ipc2581::Ipc2581::parse(panel_layer_fixture())
        .expect("synthetic panel fixture should parse");
    let doc = extract_layer_for_view(&ipc, "TOP", ArtworkScope::ArrayLocal, Resolution::default())
        .expect("panel layer should extract");
    let layer = &doc.layers[0];
    let features = layer.features.slice(&doc.features);

    assert_eq!(features.len(), 1);
    assert_eq!(doc.layout.steps.len(), 1);
    assert!(doc.layout.repeats.is_empty());
    assert!(doc.layout.instances.is_empty());
    assert_eq!(board_instance_count(&doc), 0);
    assert_eq!(panel_step_count(&doc), 1);
}

#[test]
fn extract_layout_builds_sidecar_without_layer_features() {
    let ipc = ipc2581::Ipc2581::parse(panel_layer_fixture())
        .expect("synthetic panel fixture should parse");
    let doc = extract_layout(&ipc).expect("layout should extract");

    assert!(doc.layers.is_empty());
    assert!(doc.features.is_empty());
    assert_eq!(doc.layout.steps.len(), 2);
    assert_eq!(doc.layout.repeats.len(), 1);
    assert_eq!(doc.layout.instances.len(), 2);
    assert_eq!(panel_step_count(&doc), 1);
    assert_eq!(board_instance_count(&doc), 2);
}

#[test]
fn layout_expansion_bounds_large_repeats_and_skips_empty_repeats() {
    for (nx, ny) in [
        (u32::MAX, 1),
        (u32::MAX, u32::MAX),
        (0, u32::MAX),
        (u32::MAX, 0),
    ] {
        let xml =
            panel_layer_fixture().replace("nx=\"2\" ny=\"1\"", &format!("nx=\"{nx}\" ny=\"{ny}\""));
        let ipc = Ipc2581::parse(&xml).unwrap();
        let result = extract_layout(&ipc);
        if nx == 0 || ny == 0 {
            let layout = result.unwrap();
            assert_eq!(layout.layout.repeats.len(), 1);
            assert!(layout.layout.instances.is_empty());
        } else {
            assert!(result.unwrap_err().to_string().contains("limit"));
        }
    }
}

#[test]
fn step_repeat_places_the_child_datum() {
    // The panel of IPC-2581C 8.2.3.5: a 200 x 100 board whose datum is
    // (10, 10), turned 90 degrees onto (110, 20) and stepped by (120, 207).
    let ipc = Ipc2581::parse(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner"><FunctionMode mode="FABRICATION"/><StepRef name="panel"/></Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP"/>
      <Step name="board" type="BOARD">
        <Datum x="10" y="10"/>
        <Profile><Polygon>
          <PolyBegin x="0" y="0"/><PolyStepSegment x="200" y="0"/>
          <PolyStepSegment x="200" y="100"/><PolyStepSegment x="0" y="100"/>
        </Polygon></Profile>
      </Step>
      <Step name="panel" type="PALLET">
        <Datum x="0" y="0"/>
        <StepRepeat stepRef="board" x="110" y="20" nx="2" ny="2" dx="120" dy="207" angle="90" mirror="false"/>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
    )
    .unwrap();
    let layout = extract_layout(&ipc).unwrap().layout;

    let placed = |instance: usize, point: Point| {
        let placed = layout.instances[instance].transform.transform_point(point);
        (
            (placed.x * 1e9).round() / 1e9,
            (placed.y * 1e9).round() / 1e9,
        )
    };
    assert_eq!(placed(0, Point::new(10.0, 10.0)), (110.0, 20.0));
    // The four boards sit symmetrically on the 260 x 427 panel.
    assert_eq!(placed(0, Point::new(0.0, 0.0)), (120.0, 10.0));
    assert_eq!(placed(0, Point::new(200.0, 100.0)), (20.0, 210.0));
    assert_eq!(placed(3, Point::new(0.0, 0.0)), (240.0, 217.0));
    assert_eq!(placed(3, Point::new(200.0, 100.0)), (140.0, 417.0));
}

#[test]
fn nested_panel_layout_keeps_symbolic_parent_instances() {
    let ipc = ipc2581::Ipc2581::parse(nested_panel_fixture())
        .expect("synthetic nested panel fixture should parse");
    let doc = extract_layout(&ipc).expect("layout should extract");
    let fabrication_profiles = profile_occurrences_for(&doc, ProfileSet::FabricationOutlines);
    let layout_boundaries = profile_occurrences_for(&doc, ProfileSet::LayoutBoundaries);

    assert_eq!(doc.profiles.len(), 3);
    assert_eq!(fabrication_profiles.len(), 5);
    assert_eq!(layout_boundaries.len(), 7);
    assert_eq!(
        fabrication_profiles
            .iter()
            .filter(|profile| profile.role == ProfileOccurrenceRole::RootPanel)
            .count(),
        1
    );
    assert_eq!(
        fabrication_profiles
            .iter()
            .filter(|profile| profile.role == ProfileOccurrenceRole::BoardInstance)
            .count(),
        4
    );
    assert!(
        fabrication_profiles
            .iter()
            .all(|profile| profile.role != ProfileOccurrenceRole::PanelInstance)
    );
    assert_eq!(
        layout_boundaries
            .iter()
            .filter(|profile| profile.role == ProfileOccurrenceRole::PanelInstance)
            .count(),
        2
    );
    assert_eq!(doc.layout.steps.len(), 3);
    assert_eq!(doc.layout.repeats.len(), 3);
    assert_eq!(doc.layout.instances.len(), 6);
    assert_eq!(board_instance_count(&doc), 4);
    assert_eq!(doc.layout.repeats[0].instances.start, 0);
    assert_eq!(doc.layout.repeats[0].instances.count, 2);
    assert_eq!(doc.layout.repeats[1].instances.start, 2);
    assert_eq!(doc.layout.repeats[1].instances.count, 2);
    assert_eq!(doc.layout.repeats[2].instances.start, 4);
    assert_eq!(doc.layout.repeats[2].instances.count, 2);
    assert_eq!(doc.layout.instances[0].parent_instance, None);
    assert_eq!(doc.layout.instances[1].parent_instance, None);
    assert_eq!(doc.layout.instances[2].parent_instance, Some(0));
    assert_eq!(doc.layout.instances[3].parent_instance, Some(0));
    assert_eq!(doc.layout.instances[4].parent_instance, Some(1));
    assert_eq!(doc.layout.instances[5].parent_instance, Some(1));
}

#[test]
fn nested_panel_layer_extraction_materializes_descendant_board_features() {
    let ipc = ipc2581::Ipc2581::parse(nested_panel_fixture())
        .expect("synthetic nested panel fixture should parse");
    let doc = extract_layer_for_view(
        &ipc,
        "TOP",
        ArtworkScope::ArrayFlattened,
        Resolution::default(),
    )
    .expect("nested panel layer should extract");
    let layer = &doc.layers[0];
    let features = layer.features.slice(&doc.features);
    let centers = features
        .iter()
        .map(|feature| feature.center)
        .collect::<Vec<_>>();

    assert_eq!(
        centers,
        [
            Point::new(7.0, 8.0),
            Point::new(22.0, 8.0),
            Point::new(7.0, 28.0),
            Point::new(22.0, 28.0)
        ]
    );
    assert_eq!(board_instance_count(&doc), 4);
}

/// The reported fabrication-panel bug: a render of a nested panel has to
/// carry every descendant board's copper, not just the root step's own
/// support geometry.
#[test]
fn nested_panel_render_draws_every_descendant_board_instance() {
    let ipc = ipc2581::Ipc2581::parse(nested_panel_fixture())
        .expect("synthetic nested panel fixture should parse");
    let mut doc = extract_layer_for_view(
        &ipc,
        "TOP",
        ArtworkScope::ArrayFlattened,
        Resolution::default(),
    )
    .expect("nested panel layer should extract");
    crate::dialects::ipc::process::normalize_for_artwork(&mut doc, Resolution::default()).unwrap();

    let artwork = crate::dialects::ipc::lower_layer_to_artwork(
        &doc,
        0,
        crate::dialects::LayerRole::Copper,
        crate::dialects::Side::None,
    );
    let svg =
        crate::render::artwork_svg(&artwork, &crate::render::RenderOptions::default()).unwrap();

    // One drawn pad per board across both nested repeat levels.
    assert_eq!(svg.matches("<path d=").count(), 4, "{svg}");
}

#[test]
fn nested_panel_instance_bbox_includes_child_repeats_without_profile() {
    let ipc = ipc2581::Ipc2581::parse(nested_panel_without_subpanel_profile_fixture())
        .expect("synthetic nested panel fixture should parse");
    let doc = extract_layout(&ipc).expect("layout should extract");

    assert_eq!(doc.layout.instances[0].bbox.min, Point::new(5.0, 5.0));
    assert_eq!(doc.layout.instances[0].bbox.max, Point::new(30.0, 10.0));
    assert_eq!(doc.layout.instances[1].bbox.min, Point::new(5.0, 25.0));
    assert_eq!(doc.layout.instances[1].bbox.max, Point::new(30.0, 30.0));
    assert_eq!(doc.layout.repeats[0].bbox.min, Point::new(5.0, 5.0));
    assert_eq!(doc.layout.repeats[0].bbox.max, Point::new(30.0, 30.0));
}

#[test]
fn repeated_panel_traces_keep_distinct_source_sets_after_processing() {
    let ipc = ipc2581::Ipc2581::parse(panel_trace_fixture())
        .expect("synthetic panel fixture should parse");
    let imported = import_design(&ipc, Resolution::default()).expect("panel should import");
    let mut doc = imported
        .materialize_layer(
            imported.layer_id("TOP").unwrap(),
            ArtworkScope::ArrayFlattened,
        )
        .expect("panel layer should extract");
    crate::dialects::ipc::process::normalize_for_artwork(&mut doc, Resolution::default()).unwrap();

    let layer = &doc.layers[0];
    let traces = layer
        .features
        .slice(&doc.features)
        .iter()
        .filter(|feature| feature.bucket == FeatureBucket::Trace)
        .collect::<Vec<_>>();

    assert_eq!(traces.len(), 2);
    assert!(traces.iter().all(|feature| feature.paths.count > 0));
    assert_eq!(traces[0].source.set_index, 0);
    assert_eq!(traces[1].source.set_index, 1);
    assert_eq!(traces[0].source_instance, Some(0));
    assert_eq!(traces[1].source_instance, Some(1));
    let first = feature_occurrence_id(traces[0]).unwrap();
    let second = feature_occurrence_id(traces[1]).unwrap();
    assert_eq!(first.feature, second.feature);
    let definition = imported.feature_definition(first.feature).unwrap();
    assert_eq!(definition.source.set_index, 0);
    assert_eq!(
        definition.source.feature_index,
        traces[0].source.feature_index
    );
}

#[test]
fn extracts_step_profile_and_cutouts_as_physical_board_profiles() {
    let ipc =
        ipc2581::Ipc2581::parse(profile_fixture()).expect("synthetic profile fixture should parse");
    let doc =
        extract_layer(&ipc, "TOP", Resolution::default()).expect("profile outline should extract");

    assert_eq!(doc.profiles.len(), 1);
    assert_eq!(doc.profile_cutouts.len(), 1);
    assert_eq!(board_step_count(&doc), 1);
    assert_eq!(panel_step_count(&doc), 0);
    assert_eq!(board_instance_count(&doc), 0);
    assert_eq!(doc.layout.steps[0].profiles.start, 0);
    assert_eq!(doc.layout.steps[0].profiles.count, 1);
    assert_eq!(board_bbox(&doc).unwrap().min, Point::new(0.0, 0.0));
    assert_eq!(board_bbox(&doc).unwrap().max, Point::new(20.0, 10.0));
    assert_eq!(doc.profiles[0].bbox.min, Point::new(0.0, 0.0));
    assert_eq!(doc.profiles[0].bbox.max, Point::new(20.0, 10.0));
    assert!(doc.layers[0].bbox.is_empty());
    assert!(doc.arena.paths.iter().all(|path| path.paint == Paint::None));
    assert!(doc.arena.cmds.iter().any(|cmd| cmd.op == PathOp::ArcTo));
}

fn negative_plane_fixture(layer_features: &str, profile: bool) -> String {
    let profile = if profile {
        r#"<Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="20" y="0"/>
            <PolyStepSegment x="20" y="10"/>
            <PolyStepSegment x="0" y="10"/>
          </Polygon>
          <Cutout>
            <PolyBegin x="16" y="4"/>
            <PolyStepSegment x="18" y="4"/>
            <PolyStepSegment x="18" y="6"/>
            <PolyStepSegment x="16" y="6"/>
          </Cutout>
        </Profile>"#
    } else {
        ""
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="GND" layerFunction="PLANE" side="INTERNAL" polarity="NEGATIVE"/>
      <Step name="board" type="BOARD">
        {profile}
        {layer_features}
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#
    )
}

fn negative_plane_image(xml: &str) -> ContourSet {
    let ipc = Ipc2581::parse(xml).unwrap();
    let resolution = Resolution::default();
    let imported = import_design(&ipc, resolution).unwrap();
    imported
        .composed_layer_image(
            imported.layer_id("GND").unwrap(),
            ArtworkScope::Board,
            resolution,
        )
        .unwrap()
}

#[test]
fn negative_layer_clears_its_features_from_the_step_profile() {
    let image = negative_plane_image(&negative_plane_fixture(
        r#"<LayerFeature layerRef="GND">
          <Set>
            <Pad><Location x="5" y="5"/><Circle diameter="2"/></Pad>
          </Set>
        </LayerFeature>"#,
        true,
    ));

    // The plane fills the profile, minus its cutout and the antipad.
    assert!(image.contains_point(Point::new(10.0, 5.0)));
    assert!(image.contains_point(Point::new(0.5, 9.5)));
    assert!(!image.contains_point(Point::new(5.0, 5.0)));
    assert!(!image.contains_point(Point::new(17.0, 5.0)));
    assert!(!image.contains_point(Point::new(21.0, 5.0)));
    assert!((image.area() - (200.0 - 4.0 - std::f64::consts::PI)).abs() < 0.01);
}

#[test]
fn negative_layer_without_features_is_a_full_plane() {
    let image = negative_plane_image(&negative_plane_fixture("", true));
    assert!((image.area() - 196.0).abs() < 1e-9);
}

#[test]
fn set_polarity_is_absolute_on_a_negative_layer() {
    // Allegro writes anti-etch as NEGATIVE sets on NEGATIVE plane layers:
    // they remove copper like the antipads beside them. Only an explicit
    // POSITIVE set restores material inside a clearance.
    let image = negative_plane_image(&negative_plane_fixture(
        r#"<LayerFeature layerRef="GND">
          <Set>
            <Pad><Location x="5" y="5"/><Circle diameter="4"/></Pad>
          </Set>
          <Set polarity="NEGATIVE">
            <Pad><Location x="12" y="5"/><Circle diameter="2"/></Pad>
          </Set>
          <Set polarity="POSITIVE">
            <Pad><Location x="5" y="5"/><Circle diameter="1"/></Pad>
          </Set>
        </LayerFeature>"#,
        true,
    ));

    assert!(!image.contains_point(Point::new(12.0, 5.0)));
    assert!(image.contains_point(Point::new(5.0, 5.0)));
    assert!(!image.contains_point(Point::new(6.0, 5.0)));
}

#[test]
fn negative_layer_without_a_profile_reports_its_empty_image() {
    let ipc = Ipc2581::parse(&negative_plane_fixture(
        r#"<LayerFeature layerRef="GND">
          <Set>
            <Pad><Location x="5" y="5"/><Circle diameter="2"/></Pad>
          </Set>
        </LayerFeature>"#,
        false,
    ))
    .unwrap();
    let imported = import_design(&ipc, Resolution::default()).unwrap();

    assert!(
        imported
            .geometry
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("has no Profile to fill")),
        "diagnostics: {:?}",
        imported.geometry.diagnostics
    );
}

#[test]
fn chamfered_rect_respects_corner_flags() {
    let mut doc = GeometryDocument::new();

    push_filled_shape(
        &mut doc,
        Affine2::identity(),
        shapes::chamfered_rect(10.0, 6.0, 1.0, [true, false, false, false]),
    );

    let path = &doc.arena.paths[0];
    let contour = &doc.arena.contours[path.contours.start as usize];
    let cmds = contour.cmds.slice(&doc.arena.cmds);

    assert!(!cmds.iter().any(|cmd| cmd.p0 == Point::new(4.0, -3.0)));
    assert!(!cmds.iter().any(|cmd| cmd.p0 == Point::new(5.0, -2.0)));
    assert!(cmds.iter().any(|cmd| cmd.p0 == Point::new(5.0, 2.0)));
    assert!(cmds.iter().any(|cmd| cmd.p0 == Point::new(4.0, 3.0)));
    assert!(!cmds.iter().any(|cmd| cmd.p0 == Point::new(-4.0, 3.0)));
    assert!(!cmds.iter().any(|cmd| cmd.p0 == Point::new(-5.0, 2.0)));
    assert!(!cmds.iter().any(|cmd| cmd.p0 == Point::new(-5.0, -2.0)));
    assert!(!cmds.iter().any(|cmd| cmd.p0 == Point::new(-4.0, -3.0)));
}

#[test]
fn rounded_rect_preserves_arcs_when_transform_preserves_circles() {
    let mut doc = GeometryDocument::new();

    push_filled_shape(
        &mut doc,
        Affine2::identity(),
        shapes::rounded_rect(10.0, 6.0, 1.0, [true; 4]),
    );

    let path = &doc.arena.paths[0];
    let contour = &doc.arena.contours[path.contours.start as usize];
    let cmds = contour.cmds.slice(&doc.arena.cmds);

    assert_eq!(cmds.iter().filter(|cmd| cmd.op == PathOp::ArcTo).count(), 4);
    assert!(!cmds.iter().any(|cmd| cmd.op == PathOp::CubicTo));
}

#[test]
fn rounded_rect_uses_elliptical_arcs_when_transform_distorts_circles() {
    let mut doc = GeometryDocument::new();

    push_filled_shape(
        &mut doc,
        Affine2 {
            m00: 2.0,
            m01: 0.0,
            m02: 0.0,
            m10: 0.0,
            m11: 1.0,
            m12: 0.0,
        },
        shapes::rounded_rect(10.0, 6.0, 1.0, [true; 4]),
    );

    let path = &doc.arena.paths[0];
    let contour = &doc.arena.contours[path.contours.start as usize];
    let cmds = contour.cmds.slice(&doc.arena.cmds);

    assert_eq!(
        cmds.iter()
            .filter(|cmd| cmd.op == PathOp::EllipseTo)
            .count(),
        4
    );
    assert!(
        !cmds
            .iter()
            .any(|cmd| matches!(cmd.op, PathOp::ArcTo | PathOp::CubicTo))
    );
}

#[test]
fn slot_cavity_span_controls_target_layers() {
    let mut interner = ipc2581::Interner::new();
    let l1 = test_layer(&mut interner, "L1", LayerFunction::Signal, None);
    let l2 = test_layer(&mut interner, "L2", LayerFunction::Signal, None);
    let l3 = test_layer(&mut interner, "L3", LayerFunction::Signal, None);
    let route = test_layer(
        &mut interner,
        "ROUT",
        LayerFunction::Rout,
        Some(ipc2581::types::ecad::LayerSpan {
            from_layer: Some(l1.name),
            to_layer: Some(l2.name),
        }),
    );
    let layers = [l1.name, l2.name, l3.name, route.name];
    let layer_order = Some(layers.as_slice());
    let slot = test_slot(false);

    assert!(slot_applies_to_layer(&route, &l1, layer_order, &slot));
    assert!(slot_applies_to_layer(&route, &l2, layer_order, &slot));
    assert!(!slot_applies_to_layer(&route, &l3, layer_order, &slot));
    assert!(slot_applies_to_layer(&route, &route, layer_order, &slot));
}

#[test]
fn partial_depth_slot_cavity_does_not_default_to_through_board() {
    let mut interner = ipc2581::Interner::new();
    let l1 = test_layer(&mut interner, "L1", LayerFunction::Signal, None);
    let route = test_layer(&mut interner, "ROUT", LayerFunction::Rout, None);
    let layer_order = None;
    let slot = test_slot(true);

    assert!(!slot_applies_to_layer(&route, &l1, layer_order, &slot));
    assert!(slot_applies_to_layer(&route, &route, layer_order, &slot));
}

#[test]
fn unspanned_route_slot_stays_on_route_layer() {
    let mut interner = ipc2581::Interner::new();
    let l1 = test_layer(&mut interner, "L1", LayerFunction::Signal, None);
    let route = test_layer(&mut interner, "ROUT", LayerFunction::Rout, None);
    let layer_order = None;
    let slot = test_slot(false);

    assert!(!slot_applies_to_layer(&route, &l1, layer_order, &slot));
    assert!(slot_applies_to_layer(&route, &route, layer_order, &slot));
}

#[test]
fn rotated_slot_cavity_xform_orients_route_slot() {
    let ipc = Ipc2581::parse(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <LayerRef name="F.Cu_B.Cu_1"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="F.Cu_B.Cu_1" layerFunction="ROUT" side="ALL">
        <Span fromLayer="F.Cu" toLayer="B.Cu"/>
      </Layer>
      <Step name="board" type="BOARD">
        <LayerFeature layerRef="F.Cu_B.Cu_1">
          <Set>
            <SlotCavity name="SLOT1" platingStatus="PLATED" plusTol="0" minusTol="0">
              <Location x="10" y="20"/>
              <Xform rotation="90"/>
              <Oval width="1.70" height="0.60"/>
            </SlotCavity>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
    )
    .unwrap();

    let doc = extract_layer(&ipc, "F.Cu_B.Cu_1", Resolution::default()).unwrap();
    assert_eq!(doc.features.len(), 1);

    let slot = &doc.features[0];
    assert_eq!(slot.kind, FeatureKind::Slot);
    assert!(
        slot.bbox.height() > slot.bbox.width(),
        "expected rotated slot to be vertical, got bbox {:?}",
        slot.bbox
    );
    assert!((slot.bbox.width() - 0.60).abs() < 1e-6);
    assert!((slot.bbox.height() - 1.70).abs() < 1e-6);
}

#[test]
fn padstack_shape_offsets_do_not_reposition_pad_locations() {
    // KiCad exports a pad's final shape center in the Pad Location. The
    // PadstackPadDef offset describes the padstack but must not be applied
    // again when placing layer artwork.
    let ipc = Ipc2581::parse(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <LayerRef name="TOP"/>
    <DictionaryStandard units="MILLIMETER">
      <EntryStandard id="square">
        <RectCenter width="8" height="8"/>
      </EntryStandard>
    </DictionaryStandard>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <PadStackDef name="offset_pad">
          <PadstackPadDef layerRef="TOP" padUse="REGULAR">
            <Location x="-2.0" y="-2.0"/>
            <StandardPrimitiveRef id="square"/>
          </PadstackPadDef>
        </PadStackDef>
        <LayerFeature layerRef="TOP">
          <Set>
            <Pad padstackDefRef="offset_pad">
              <Location x="10" y="10"/>
              <StandardPrimitiveRef id="square"/>
            </Pad>
            <Pad padstackDefRef="offset_pad">
              <Xform rotation="270.0"/>
              <Location x="40" y="10"/>
              <StandardPrimitiveRef id="square"/>
            </Pad>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
    )
    .unwrap();

    let doc = extract_layer(&ipc, "TOP", Resolution::default()).unwrap();
    assert_eq!(doc.features.len(), 2);

    let unrotated = doc.features[0].bbox;
    assert!((unrotated.center().x - 10.0).abs() < 1e-9);
    assert!((unrotated.center().y - 10.0).abs() < 1e-9);

    let rotated = doc.features[1].bbox;
    assert!((rotated.center().x - 40.0).abs() < 1e-9);
    assert!((rotated.center().y - 10.0).abs() < 1e-9);
}

#[test]
fn padstack_xform_offsets_do_not_reposition_pad_locations() {
    // Allegro writes the shape offset as a PadstackPadDef Xform, but its
    // Pad Location is already pin origin + rotated offset. Coordinates are
    // L44 of the Allegro testcase5 fixture: pins at x = 2.94 and 6.064.
    let ipc = Ipc2581::parse(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <LayerRef name="TOP"/>
    <DictionaryStandard units="MILLIMETER">
      <EntryStandard id="land">
        <RectCenter width="1.45" height="4.4"/>
      </EntryStandard>
    </DictionaryStandard>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <PadStackDef name="LS145X440_SP">
          <PadstackPadDef layerRef="TOP" padUse="REGULAR">
            <Xform xOffset="0.0850"/>
            <Location x="0.0" y="0.0"/>
            <StandardPrimitiveRef id="land"/>
          </PadstackPadDef>
        </PadStackDef>
        <LayerFeature layerRef="TOP">
          <Set>
            <Pad padstackDefRef="LS145X440_SP">
              <Location x="3.0250" y="45.7500"/>
              <StandardPrimitiveRef id="land"/>
            </Pad>
            <Pad padstackDefRef="LS145X440_SP">
              <Xform rotation="180.000"/>
              <Location x="5.9790" y="45.7500"/>
            </Pad>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
    )
    .unwrap();

    let doc = extract_layer(&ipc, "TOP", Resolution::default()).unwrap();
    assert_eq!(doc.features.len(), 2);
    for (feature, x) in doc.features.iter().zip([3.025, 5.979]) {
        let center = feature.bbox.center();
        assert!((center.x - x).abs() < 1e-9, "{center:?}");
        assert!((center.y - 45.75).abs() < 1e-9, "{center:?}");
    }
}

#[test]
fn pad_draws_its_inline_shape_without_a_padstack() {
    let ipc = Ipc2581::parse(
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
        <LayerFeature layerRef="TOP">
          <Set net="GND">
            <Pad>
              <Location x="5" y="7"/>
              <Circle diameter="2"/>
            </Pad>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
    )
    .unwrap();

    let doc = extract_layer(&ipc, "TOP", Resolution::default()).unwrap();
    assert!(doc.diagnostics.is_empty(), "{:?}", doc.diagnostics);
    assert_eq!(doc.features.len(), 1);

    let pad = &doc.features[0];
    assert_eq!(pad.kind, FeatureKind::Padstack);
    assert_eq!(pad.padstack_ref, None);
    assert_eq!(pad.primitive_ref, None);
    assert_eq!(pad.bbox.min, Point::new(4.0, 6.0));
    assert_eq!(pad.bbox.max, Point::new(6.0, 8.0));
}

#[test]
fn pad_inline_shape_overrides_its_padstack_shape() {
    let ipc = Ipc2581::parse(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <LayerRef name="TOP"/>
    <DictionaryStandard units="MILLIMETER">
      <EntryStandard id="square">
        <RectCenter width="8" height="8"/>
      </EntryStandard>
    </DictionaryStandard>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <PadStackDef name="via">
          <PadstackHoleDef name="drill" diameter="0.3" platingStatus="VIA" plusTol="0" minusTol="0" x="0" y="0"/>
          <PadstackPadDef layerRef="TOP" padUse="REGULAR">
            <Location x="0" y="0"/>
            <StandardPrimitiveRef id="square"/>
          </PadstackPadDef>
        </PadStackDef>
        <LayerFeature layerRef="TOP">
          <Set>
            <Pad padstackDefRef="via">
              <Location x="10" y="10"/>
              <Oval width="3" height="1"/>
            </Pad>
            <Pad padstackDefRef="via">
              <Location x="30" y="10"/>
            </Pad>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
    )
    .unwrap();

    let doc = extract_layer(&ipc, "TOP", Resolution::default()).unwrap();
    assert_eq!(doc.features.len(), 2);

    // The padstack still refines what the pad is, not what it looks like.
    let inline = &doc.features[0];
    assert_eq!(inline.intent.role, FeatureRole::Via);
    assert_eq!(inline.primitive_ref, None);
    assert!((inline.bbox.width() - 3.0).abs() < 1e-9);
    assert!((inline.bbox.height() - 1.0).abs() < 1e-9);

    let from_padstack = &doc.features[1];
    assert!(from_padstack.primitive_ref.is_some());
    assert!((from_padstack.bbox.width() - 8.0).abs() < 1e-9);
}

#[test]
fn extracts_nonplated_padstack_artwork_on_soldermask_layers() {
    let ipc = Ipc2581::parse(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <LayerRef name="F.Mask"/>
    <DictionaryStandard units="MILLIMETER">
      <EntryStandard id="mask_opening">
        <Circle diameter="0.9906"/>
      </EntryStandard>
    </DictionaryStandard>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="F.Mask" layerFunction="SOLDERMASK" side="TOP" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <PadStackDef name="npth_mask">
          <PadstackHoleDef name="npth" diameter="0.9906" platingStatus="NONPLATED" plusTol="0" minusTol="0" x="0" y="0"/>
          <PadstackPadDef layerRef="F.Mask" padUse="REGULAR">
            <StandardPrimitiveRef id="mask_opening"/>
          </PadstackPadDef>
        </PadStackDef>
        <LayerFeature layerRef="F.Mask">
          <Set>
            <Pad padstackDefRef="npth_mask">
              <Location x="117.065" y="-133.14"/>
              <PinRef componentRef="J3" pin="NPTH0"/>
            </Pad>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
    )
    .unwrap();

    let doc = extract_layer(&ipc, "F.Mask", Resolution::default()).unwrap();

    assert_eq!(doc.features.len(), 1);
    let feature = &doc.features[0];
    assert_eq!(feature.bucket, FeatureBucket::Pth);
    assert_eq!(feature.intent.domain, FeatureDomain::Soldermask);
    assert_eq!(feature.intent.plating, PlatingKind::NonPlated);
    assert_eq!(feature.pin_refs.count, 1);
    assert!((feature.bbox.width() - 0.9906).abs() < 1e-6);
    assert!((feature.bbox.height() - 0.9906).abs() < 1e-6);
}

fn test_layer(
    interner: &mut ipc2581::Interner,
    name: &str,
    layer_function: LayerFunction,
    span: Option<ipc2581::types::ecad::LayerSpan>,
) -> Layer {
    Layer {
        name: interner.intern(name),
        layer_function,
        side: None,
        polarity: None,
        span,
        spec_refs: Vec::new(),
        profiles: Vec::new(),
    }
}

fn test_slot(z_axis_dim: bool) -> ipc2581::types::Slot {
    ipc2581::types::Slot {
        name: None,
        shape: SlotShape::Primitive(StandardPrimitive::Circle(ipc2581::types::Styled {
            shape: ipc2581::types::Circle { diameter: 1.0 },
            fill_property: None,
            line_desc: None,
            line_desc_ref: None,
            fill_desc: None,
            fill_desc_ref: None,
        })),
        plating_status: PlatingStatus::NonPlated,
        z_axis_dim,
        xform: None,
        x: 0.0,
        y: 0.0,
    }
}

fn panel_layer_fixture() -> &'static str {
    r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="panel"/>
    <LayerRef name="TOP"/>
    <DictionaryStandard units="MILLIMETER">
      <EntryStandard id="pad">
        <Circle diameter="1"/>
      </EntryStandard>
    </DictionaryStandard>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="10" y="0"/>
            <PolyStepSegment x="10" y="5"/>
            <PolyStepSegment x="0" y="5"/>
          </Polygon>
        </Profile>
        <PadStackDef name="padstack">
          <PadstackPadDef layerRef="TOP" padUse="REGULAR">
            <StandardPrimitiveRef id="pad"/>
          </PadstackPadDef>
        </PadStackDef>
        <LayerFeature layerRef="TOP">
          <Set>
            <Pad padstackDefRef="padstack">
              <Location x="2" y="3"/>
            </Pad>
          </Set>
        </LayerFeature>
      </Step>
      <Step name="panel" type="PALLET">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="100" y="0"/>
            <PolyStepSegment x="100" y="80"/>
            <PolyStepSegment x="0" y="80"/>
          </Polygon>
        </Profile>
        <PadStackDef name="panel_padstack">
          <PadstackPadDef layerRef="TOP" padUse="REGULAR">
            <StandardPrimitiveRef id="pad"/>
          </PadstackPadDef>
        </PadStackDef>
        <LayerFeature layerRef="TOP">
          <Set>
            <Pad padstackDefRef="panel_padstack">
              <Location x="40" y="5"/>
            </Pad>
          </Set>
        </LayerFeature>
        <StepRepeat stepRef="board" x="10" y="20" nx="2" ny="1" dx="15" dy="0"/>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#
}

fn panel_trace_fixture() -> &'static str {
    r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="panel"/>
    <LayerRef name="TOP"/>
    <DictionaryLineDesc units="MILLIMETER">
      <EntryLineDesc id="trace">
        <LineDesc lineWidth="1" lineEnd="ROUND"/>
      </EntryLineDesc>
    </DictionaryLineDesc>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <LayerFeature layerRef="TOP">
          <Set net="N1">
            <Polyline lineDescRef="trace">
              <PolyBegin x="0" y="0"/>
              <PolyStepSegment x="10" y="0"/>
            </Polyline>
          </Set>
        </LayerFeature>
      </Step>
      <Step name="panel" type="PALLET">
        <StepRepeat stepRef="board" x="0" y="0" nx="2" ny="1" dx="20" dy="0"/>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#
}

fn nested_panel_fixture() -> &'static str {
    r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="panel"/>
    <LayerRef name="TOP"/>
    <DictionaryStandard units="MILLIMETER">
      <EntryStandard id="pad"><Circle diameter="1"/></EntryStandard>
    </DictionaryStandard>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="10" y="0"/>
            <PolyStepSegment x="10" y="5"/>
            <PolyStepSegment x="0" y="5"/>
          </Polygon>
        </Profile>
        <PadStackDef name="padstack">
          <PadstackPadDef layerRef="TOP" padUse="REGULAR">
            <StandardPrimitiveRef id="pad"/>
          </PadstackPadDef>
        </PadStackDef>
        <LayerFeature layerRef="TOP">
          <Set>
            <Pad padstackDefRef="padstack">
              <Location x="2" y="3"/>
            </Pad>
          </Set>
        </LayerFeature>
      </Step>
      <Step name="subpanel" type="PALLET">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="30" y="0"/>
            <PolyStepSegment x="30" y="10"/>
            <PolyStepSegment x="0" y="10"/>
          </Polygon>
        </Profile>
        <StepRepeat stepRef="board" x="0" y="0" nx="2" ny="1" dx="15" dy="0"/>
      </Step>
      <Step name="panel" type="PALLET">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="40" y="0"/>
            <PolyStepSegment x="40" y="40"/>
            <PolyStepSegment x="0" y="40"/>
          </Polygon>
        </Profile>
        <StepRepeat stepRef="subpanel" x="5" y="5" nx="1" ny="2" dx="0" dy="20"/>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#
}

fn nested_panel_without_subpanel_profile_fixture() -> &'static str {
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
      <Step name="board" type="BOARD">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="10" y="0"/>
            <PolyStepSegment x="10" y="5"/>
            <PolyStepSegment x="0" y="5"/>
          </Polygon>
        </Profile>
      </Step>
      <Step name="subpanel" type="PALLET">
        <StepRepeat stepRef="board" x="0" y="0" nx="2" ny="1" dx="15" dy="0"/>
      </Step>
      <Step name="panel" type="PALLET">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="40" y="0"/>
            <PolyStepSegment x="40" y="40"/>
            <PolyStepSegment x="0" y="40"/>
          </Polygon>
        </Profile>
        <StepRepeat stepRef="subpanel" x="5" y="5" nx="1" ny="2" dx="0" dy="20"/>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#
}

fn profile_fixture() -> &'static str {
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
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="20" y="0"/>
            <PolyStepSegment x="20" y="10"/>
            <PolyStepSegment x="0" y="10"/>
          </Polygon>
          <Cutout>
            <PolyBegin x="6" y="5"/>
            <PolyStepCurve x="4" y="5" centerX="5" centerY="5" clockwise="false"/>
            <PolyStepCurve x="6" y="5" centerX="5" centerY="5" clockwise="false"/>
          </Cutout>
        </Profile>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#
}
