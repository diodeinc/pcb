use ipc2581::{FeatureShape, Ipc2581, PadstackPadDef, StandardPrimitive};

fn pad_defs(doc: &Ipc2581) -> &[PadstackPadDef] {
    &doc.ecad().unwrap().cad_data.steps[0].padstack_defs[0].pad_defs
}

fn fixture(pad_defs: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
  </Content>
  <Ecad>
    <CadHeader units="INCH"/>
    <CadData>
      <Step name="board">
        <PadStackDef name="L3_5842X089FS_TRACE">{pad_defs}</PadStackDef>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#
    )
}

/// Allegro leaves `Location` at the origin and puts the shape offset in `Xform`.
#[test]
fn parses_pad_def_xform_in_ecad_units() {
    let doc = Ipc2581::parse(&fixture(
        r#"<PadstackPadDef layerRef="L3" padUse="REGULAR">
             <Xform xOffset="-0.0354" rotation="90.000" mirror="true"/>
             <Location x="0.0" y="0.0"/>
             <StandardPrimitiveRef id="SHAPE_LP5842X089_FS_SHAPE"/>
           </PadstackPadDef>"#,
    ))
    .unwrap();
    let pad_def = &pad_defs(&doc)[0];

    let xform = pad_def.xform.expect("Xform should be retained");
    assert!((xform.x_offset + 0.0354 * 25.4).abs() < 1e-12);
    assert_eq!(xform.y_offset, 0.0);
    assert_eq!(xform.rotation, 90.0);
    assert!(xform.mirror);
    assert_eq!((pad_def.x, pad_def.y), (0.0, 0.0));
    let Some(FeatureShape::StandardPrimitiveRef(id)) = pad_def.feature else {
        panic!("unexpected feature: {:?}", pad_def.feature);
    };
    assert_eq!(doc.resolve(id), "SHAPE_LP5842X089_FS_SHAPE");
    assert_eq!(pad_def.standard_primitive_ref, Some(id));
    assert_eq!(pad_def.user_primitive_ref, None);
}

#[test]
fn parses_pad_def_inline_and_user_ref_shapes() {
    let doc = Ipc2581::parse(&fixture(
        r#"<PadstackPadDef layerRef="L1" padUse="REGULAR">
             <Location x="0.1" y="0.2"/>
             <Circle diameter="0.05"/>
           </PadstackPadDef>
           <PadstackPadDef layerRef="L2" padUse="ANTIPAD">
             <Location x="0" y="0"/>
             <UserPrimitiveRef id="custom"/>
           </PadstackPadDef>"#,
    ))
    .unwrap();
    let [inline, user] = pad_defs(&doc) else {
        panic!("expected two pad defs");
    };

    assert_eq!(inline.xform, None);
    assert!((inline.x - 2.54).abs() < 1e-12 && (inline.y - 5.08).abs() < 1e-12);
    let Some(FeatureShape::StandardPrimitive(StandardPrimitive::Circle(circle))) = &inline.feature
    else {
        panic!("unexpected feature: {:?}", inline.feature);
    };
    assert!((circle.shape.diameter - 1.27).abs() < 1e-12);
    assert_eq!(inline.standard_primitive_ref, None);
    assert_eq!(inline.user_primitive_ref, None);

    let Some(FeatureShape::UserPrimitiveRef(id)) = user.feature else {
        panic!("unexpected feature: {:?}", user.feature);
    };
    assert_eq!(doc.resolve(id), "custom");
    assert_eq!(user.user_primitive_ref, Some(id));
}
