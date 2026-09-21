use ipc2581::{Ipc2581, SetFeature, StandardPrimitive};

fn fixture(entries: &str, sets: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <DictionaryStandard units="INCH">{entries}</DictionaryStandard>
  </Content>
  <Ecad>
    <CadHeader units="INCH"/>
    <CadData>
      <Step name="board"><LayerFeature layerRef="top">{sets}</LayerFeature></Step>
    </CadData>
  </Ecad>
</IPC-2581>"#
    )
}

fn pad(xform: &str) -> String {
    fixture(
        "",
        &format!(
            r#"<Set><Pad><Xform {xform}/><Location x="1" y="2"/><StandardPrimitiveRef id="s"/></Pad></Set>"#
        ),
    )
}

fn entry(primitive: &str) -> String {
    fixture(
        &format!(r#"<EntryStandard id="e">{primitive}</EntryStandard>"#),
        "",
    )
}

fn assert_rejected(xml: &str, needle: &str) {
    let error = Ipc2581::parse(xml).expect_err(xml).to_string();
    assert!(error.contains(needle), "{error}");
}

#[test]
fn malformed_transform_attributes_are_errors() {
    for xform in [
        r#"rotation="90,0""#,
        r#"rotation="90deg""#,
        r#"scale="x""#,
        r#"xOffset="1,5""#,
        r#"yOffset="""#,
        r#"mirror="TRUE""#,
    ] {
        assert_rejected(&pad(xform), "alue for");
    }

    let doc = Ipc2581::parse(&pad(r#"rotation=" 90 " xOffset="1""#)).unwrap();
    let SetFeature::Pad(pad) =
        &doc.ecad().unwrap().cad_data.steps[0].layer_features[0].sets[0].features[0]
    else {
        panic!("expected a pad");
    };
    let xform = pad.xform.unwrap();
    assert_eq!(
        (xform.rotation, xform.x_offset, xform.scale),
        (90.0, 25.4, 1.0)
    );
    assert!(!xform.mirror);
}

#[test]
fn non_finite_numbers_are_errors() {
    for value in ["NaN", "INF", "-INF", "inf", "1e400", "1e308"] {
        let xml = fixture(
            "",
            &format!(
                r#"<Set><Hole name="h" diameter="1" platingStatus="VIA" plusTol="0" minusTol="0" x="{value}" y="0"/></Set>"#
            ),
        );
        assert_rejected(&xml, "Value for x");
    }
}

#[test]
fn unknown_set_polarity_and_layer_function_are_errors() {
    assert_rejected(&fixture("", r#"<Set polarity="NEG"/>"#), "polarity");
    let layer = fixture("", "").replace(
        "<Step ",
        r#"<Layer name="top" layerFunction="COPPER"/><Step "#,
    );
    assert_rejected(&layer, "Unknown layerFunction: COPPER");
}

#[test]
fn counts_that_drive_loops_are_bounded() {
    let thermal = |spokes: &str| {
        entry(&format!(
            r#"<Thermal shape="ROUND" outerDiameter="2" innerDiameter="1" spokeCount="{spokes}" spokeStartAngle="0"/>"#
        ))
    };
    assert_rejected(&thermal("5"), "spokeCount");
    assert_rejected(&thermal("4000000000"), "spokeCount");
    assert_rejected(&thermal("-1"), "spokeCount");
    assert!(Ipc2581::parse(&thermal("0")).is_ok());

    let moire = |rings: &str| {
        entry(&format!(
            r#"<Moire diameter="2" ringWidth="0.1" ringGap="0" ringNumber="{rings}"/>"#
        ))
    };
    assert_rejected(&moire("4000000000"), "ringNumber");
    assert!(Ipc2581::parse(&moire("3")).is_ok());
}

#[test]
fn corner_flags_default_to_false_but_reject_malformed_values() {
    let rect = |corners: &str| {
        entry(&format!(
            r#"<RectRound width="2" height="1" radius="0.1" {corners}/>"#
        ))
    };
    assert_rejected(&rect(r#"upperRight="TRUE""#), "upperRight");
    assert_rejected(&rect(r#"lowerLeft="yes""#), "lowerLeft");

    let doc = Ipc2581::parse(&rect(r#"upperLeft="true""#)).unwrap();
    let StandardPrimitive::RectRound(rect) =
        &doc.content().dictionary_standard.entries[0].primitive
    else {
        panic!("expected a RectRound");
    };
    assert!(rect.shape.upper_left);
    assert!(!rect.shape.upper_right && !rect.shape.lower_left && !rect.shape.lower_right);
}

#[test]
fn percentage_tolerances_are_not_scaled_as_lengths() {
    let xml = fixture("", "").replace(
        "<Step ",
        r#"<Stackup name="s" overallThickness="0.063" tolPlus="10" tolMinus="5" tolPercent="true" whereMeasured="METAL" stackupStatus="PROPOSED">
             <StackupGroup name="g" thickness="0.063" tolPlus="0" tolMinus="0">
               <StackupLayer layerOrGroupRef="top" thickness="0.001" tolPlus="0.0001" tolMinus="0.0001" sequence="1.0"/>
             </StackupGroup>
           </Stackup><Step "#,
    );
    let doc = Ipc2581::parse(&xml).unwrap();
    let stackup = &doc.ecad().unwrap().cad_data.stackups[0];

    assert!(stackup.tol_percent);
    assert_eq!(
        (stackup.tol_plus, stackup.tol_minus),
        (Some(10.0), Some(5.0))
    );
    let layer = &stackup.layers[0];
    assert!(!layer.tol_percent);
    assert_eq!(layer.tol_plus, Some(0.0001 * 25.4));
    assert_eq!(layer.layer_number, Some(1));

    assert_rejected(
        &xml.replace(r#"sequence="1.0""#, r#"sequence="1.5""#),
        "sequence",
    );
}
