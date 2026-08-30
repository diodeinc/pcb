use ipc2581::{GeometryUsage, Ipc2581, Ipc2581Error};

fn fixture(sets: &str) -> String {
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
