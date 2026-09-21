use ipc2581::{
    FeatureShape, GeometryUsage, Ipc2581, Ipc2581Error, LineDescGroup, SetFeature, SlotShape,
    StandardPrimitive, UserPrimitive, UserShapeType,
};

fn fixture(sets: &str) -> String {
    fixture_with_dictionary("", sets)
}

fn fixture_with_dictionary(entries: &str, sets: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <DictionaryUser units="MILLIMETER">{entries}</DictionaryUser>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Step name="board">
        <LayerFeature layerRef="top">{sets}</LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#
    )
}

#[test]
fn retains_component_ref_and_all_geometry_usage_variants() {
    let values = [
        ("THIEVING", GeometryUsage::Thieving),
        ("THERMAL_RELIEF", GeometryUsage::ThermalRelief),
        ("TEXT", GeometryUsage::Text),
        ("TEARDROP", GeometryUsage::Teardrop),
        ("GRAPHIC", GeometryUsage::Graphic),
        ("NONE", GeometryUsage::None),
    ];
    let sets = values
        .iter()
        .enumerate()
        .map(|(index, (value, _))| {
            format!(
                r#"<Set componentRef="U{}" geometryUsage="{}"/>"#,
                index + 1,
                value
            )
        })
        .collect::<String>();

    let doc = Ipc2581::parse(&fixture(&sets)).expect("fixture should parse");
    let parsed_sets = &doc.ecad().unwrap().cad_data.steps[0].layer_features[0].sets;
    assert_eq!(parsed_sets.len(), values.len());
    for (index, (set, (_, expected))) in parsed_sets.iter().zip(values).enumerate() {
        assert_eq!(
            doc.resolve(set.component_ref.unwrap()),
            format!("U{}", index + 1)
        );
        assert_eq!(set.geometry_usage, Some(expected));
    }
}

#[test]
fn rejects_unknown_geometry_usage() {
    let error = Ipc2581::parse(&fixture(r#"<Set geometryUsage="DECORATIVE"/>"#))
        .expect_err("unknown geometryUsage should fail parsing");

    assert!(
        matches!(error, Ipc2581Error::InvalidAttribute(ref message) if message == "Unknown geometryUsage: DECORATIVE"),
        "unexpected error: {error}"
    );
}

fn set_features(doc: &Ipc2581) -> &[SetFeature] {
    let layer_feature = &doc.ecad().unwrap().cad_data.steps[0].layer_features[0];
    layer_feature.sets[0]
        .features
        .slice(&layer_feature.features)
}

fn shape_types(primitive: &UserPrimitive) -> Vec<&UserShapeType> {
    let UserPrimitive::UserSpecial(special) = primitive;
    special.shapes.iter().map(|shape| &shape.shape).collect()
}

#[test]
fn features_accepts_every_member_of_the_feature_group() {
    let doc = Ipc2581::parse(&fixture(
        r#"<Set>
             <Features><Location x="1" y="2"/><Circle diameter="0.5"/></Features>
             <Features><Donut shape="ROUND" outerDiameter="2" innerDiameter="1"/></Features>
             <Features>
               <Outline>
                 <Polygon><PolyBegin x="0" y="0"/><PolyStepSegment x="1" y="0"/><PolyStepSegment x="0" y="0"/></Polygon>
                 <LineDesc lineEnd="ROUND" lineWidth="0"/>
               </Outline>
             </Features>
             <Features>
               <Text textString="REV A" fontSize="10"><BoundingBox lowerLeftX="0" lowerLeftY="0" upperRightX="5" upperRightY="1"/></Text>
             </Features>
           </Set>"#,
    ))
    .unwrap();

    let shapes = set_features(&doc)
        .iter()
        .map(|feature| {
            let SetFeature::UserPrimitive(feature) = feature else {
                panic!("unexpected feature: {feature:?}");
            };
            ((feature.x, feature.y), shape_types(&feature.primitive))
        })
        .collect::<Vec<_>>();
    assert!(matches!(
        shapes[0],
        ((1.0, 2.0), ref shapes) if matches!(shapes[..], [UserShapeType::Circle(circle)] if circle.diameter == 0.5)
    ));
    assert!(matches!(
        shapes[1].1[..],
        [UserShapeType::StandardPrimitive(StandardPrimitive::Donut(
            _
        ))]
    ));
    assert!(matches!(shapes[2].1[..], [UserShapeType::Outline(_)]));
    assert!(matches!(shapes[3].1[..], [UserShapeType::Text(_)]));
}

#[test]
fn user_special_retains_every_member_of_the_feature_group() {
    // KiCad writes outline-font glyphs as zero-width Outlines.
    let doc = Ipc2581::parse(&fixture_with_dictionary(
        r#"<EntryUser id="glyphs">
             <UserSpecial>
               <Outline>
                 <Polygon><PolyBegin x="0" y="0"/><PolyStepSegment x="1" y="0"/><PolyStepSegment x="0" y="0"/></Polygon>
                 <LineDesc lineEnd="ROUND" lineWidth="0"/>
               </Outline>
               <Hexagon length="2"><FillDesc fillProperty="VOID"/></Hexagon>
               <StandardPrimitiveRef id="land"/>
               <RectCenter width="2" height="1"/>
             </UserSpecial>
           </EntryUser>
           <EntryUser id="bare">
             <Polyline><PolyBegin x="0" y="0"/><PolyStepSegment x="1" y="0"/><LineDescRef id="thin"/></Polyline>
           </EntryUser>"#,
        "",
    ))
    .unwrap();
    let [glyphs, bare] = &doc.content().dictionary_user.entries[..] else {
        panic!("expected two entries");
    };

    let UserPrimitive::UserSpecial(special) = &glyphs.primitive;
    assert!(matches!(
        shape_types(&glyphs.primitive)[..],
        [
            UserShapeType::Outline(_),
            UserShapeType::StandardPrimitive(StandardPrimitive::Hexagon(_)),
            UserShapeType::StandardPrimitiveRef(_),
            UserShapeType::RectCenter(_),
        ]
    ));
    assert_eq!(
        special.shapes[1].fill_desc.map(|fill| fill.fill_property),
        Some(ipc2581::FillProperty::Void)
    );
    assert!(matches!(
        shape_types(&bare.primitive)[..],
        [UserShapeType::Polyline(_)]
    ));
}

#[test]
fn rejects_elements_outside_the_feature_group() {
    for xml in [
        fixture(r#"<Set><Features><Location x="0" y="0"/><Blob/></Features></Set>"#),
        fixture_with_dictionary(
            r#"<EntryUser id="u"><UserSpecial><Blob/></UserSpecial></EntryUser>"#,
            "",
        ),
        fixture(r#"<Set><Pad><Location x="0" y="0"/><StandardPrimitiveRef/></Pad></Set>"#),
    ] {
        assert!(Ipc2581::parse(&xml).is_err(), "{xml}");
    }
}

#[test]
fn slot_cavity_and_pad_take_any_inline_standard_primitive() {
    let slot = |shape: &str| {
        fixture(&format!(
            r#"<Set><SlotCavity name="S" platingStatus="PLATED" plusTol="0" minusTol="0">
                 <Location x="1" y="2"/>{shape}
               </SlotCavity></Set>"#
        ))
    };
    for shape in [
        r#"<RectCham width="2" height="1" chamfer="0.1"/>"#,
        r#"<Donut shape="ROUND" outerDiameter="2" innerDiameter="1"/>"#,
        r#"<Contour><Polygon><PolyBegin x="0" y="0"/><PolyStepSegment x="1" y="0"/><PolyStepSegment x="0" y="0"/></Polygon></Contour>"#,
    ] {
        let doc = Ipc2581::parse(&slot(shape)).unwrap();
        let SetFeature::Slot(slot) = &set_features(&doc)[0] else {
            panic!("expected a slot");
        };
        assert!(matches!(slot.shape, SlotShape::Primitive(_)), "{shape}");
    }
    let error = Ipc2581::parse(&slot(r#"<UserPrimitiveRef id="custom"/>"#)).unwrap_err();
    assert!(error.to_string().contains("Unsupported UserPrimitiveRef"));

    let doc = Ipc2581::parse(&fixture(
        r#"<Set><Pad><Location x="1" y="2"/><Circle diameter="0.5"/></Pad></Set>"#,
    ))
    .unwrap();
    let SetFeature::Pad(pad) = &set_features(&doc)[0] else {
        panic!("expected a pad");
    };
    let Some(FeatureShape::StandardPrimitive(primitive)) = &pad.feature else {
        panic!("expected an inline shape: {:?}", pad.feature);
    };
    assert!(matches!(**primitive, StandardPrimitive::Circle(_)));
}

#[test]
fn a_stroke_carries_its_reference_or_its_inline_line_desc() {
    let doc = Ipc2581::parse(&fixture(
        r#"<Set>
             <Features><Line startX="0" startY="0" endX="1" endY="0"><LineDescRef id="thin"/></Line></Features>
             <Features><Line startX="0" startY="0" endX="1" endY="0"><LineDesc lineEnd="SQUARE" lineWidth="0.2"/></Line></Features>
             <Features><Line startX="0" startY="0" endX="1" endY="0"/></Features>
             <Polyline lineDescRef="thin"><PolyBegin x="0" y="0"/><PolyStepSegment x="1" y="0"/><LineDesc lineEnd="ROUND" lineWidth="0.3"/></Polyline>
             <Polyline><PolyBegin x="0" y="0"/><PolyStepSegment x="1" y="0"/><LineDesc lineEnd="ROUND" lineWidth="0.3"/></Polyline>
           </Set>"#,
    ))
    .unwrap();
    let line_descs = set_features(&doc)
        .iter()
        .map(|feature| match feature {
            SetFeature::Stroke(stroke) => stroke.line_desc,
            feature => panic!("expected a stroke: {feature:?}"),
        })
        .collect::<Vec<_>>();

    let [
        Some(LineDescGroup::Ref(by_ref)),
        Some(LineDescGroup::Inline(inline)),
        None,
        Some(LineDescGroup::Ref(by_attribute)),
        Some(LineDescGroup::Inline(set_polyline)),
    ] = line_descs[..]
    else {
        panic!("unexpected line descriptions: {line_descs:?}");
    };
    assert_eq!(doc.resolve(by_ref), "thin");
    assert_eq!(doc.resolve(by_attribute), "thin");
    assert_eq!(
        (inline.line_width, inline.line_end),
        (0.2, ipc2581::LineEnd::Square)
    );
    assert_eq!(set_polyline.line_width, 0.3);
}
