use ipc2581::{Ipc2581, Ipc2581Error, LayerFunction, PadUse};

const XSD: &str = include_str!("../IPC-2581C.xsd");

/// The enumeration values of a named simple type in the vendored schema.
fn schema_values(simple_type: &str) -> Vec<&'static str> {
    let start = XSD
        .find(&format!("<xsd:simpleType name=\"{simple_type}\">"))
        .unwrap_or_else(|| panic!("{simple_type} is in the schema"));
    let body = &XSD[start..start + XSD[start..].find("</xsd:simpleType>").unwrap()];
    body.split("<xsd:enumeration value=\"")
        .skip(1)
        .map(|rest| &rest[..rest.find('"').unwrap()])
        .collect()
}

fn fixture(layers: &str, pad_defs: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner"><FunctionMode mode="FABRICATION"/></Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>{layers}<Step name="board"><PadStackDef name="P">{pad_defs}</PadStackDef></Step></CadData>
  </Ecad>
</IPC-2581>"#
    )
}

#[test]
fn every_schema_layer_function_round_trips() {
    let values = schema_values("layerFunctionType");
    assert!(values.contains(&"BOARDFAB") && values.contains(&"PIN"));
    let layers = values
        .iter()
        .map(|value| format!(r#"<Layer name="{value}" layerFunction="{value}"/>"#))
        .collect::<String>();

    let doc = Ipc2581::parse(&fixture(&layers, "")).unwrap();
    for (layer, value) in doc.ecad().unwrap().cad_data.layers.iter().zip(values) {
        assert_eq!(layer.layer_function.as_str(), value);
        assert_eq!(
            layer.layer_function == LayerFunction::Other,
            value == "OTHER"
        );
    }
}

#[test]
fn every_schema_pad_use_parses() {
    let values = schema_values("padUseType");
    let pad_defs = values
        .iter()
        .map(|value| {
            format!(
                r#"<PadstackPadDef layerRef="L" padUse="{value}"><Location x="0" y="0"/><StandardPrimitiveRef id="s"/></PadstackPadDef>"#
            )
        })
        .collect::<String>();

    let doc = Ipc2581::parse(&fixture("", &pad_defs)).unwrap();
    let pad_uses = doc.ecad().unwrap().cad_data.steps[0].padstack_defs[0]
        .pad_defs
        .iter()
        .map(|pad_def| pad_def.pad_use)
        .collect::<Vec<_>>();
    assert_eq!(
        pad_uses,
        [
            PadUse::Regular,
            PadUse::Antipad,
            PadUse::Thermal,
            PadUse::Other
        ]
    );
}

/// Every token of `simple_type` parses and is written back unchanged.
fn assert_round_trips<T>(
    simple_type: &str,
    from_ipc: fn(&str) -> Result<T, Ipc2581Error>,
    as_str: impl Fn(T) -> &'static str,
) {
    // `b1` (lead-free solder) has no FinishType variant; it reads as Other.
    for value in schema_values(simple_type)
        .into_iter()
        .filter(|value| *value != "b1")
    {
        let parsed = from_ipc(value).unwrap_or_else(|error| panic!("{simple_type}: {error}"));
        assert_eq!(as_str(parsed), value, "{simple_type}");
    }
    assert!(from_ipc("not a token").is_err(), "{simple_type}");
}

#[test]
fn enum_tokens_match_the_schema() {
    use ipc2581::*;

    assert_round_trips("modeType", Mode::from_ipc, Mode::as_str);
    assert_round_trips("unitsType", Units::from_ipc, Units::as_str);
    assert_round_trips("lineEndType", LineEnd::from_ipc, LineEnd::as_str);
    assert_round_trips(
        "linePropertyType",
        LineProperty::from_ipc,
        LineProperty::as_str,
    );
    assert_round_trips(
        "fillPropertyType",
        FillProperty::from_ipc,
        FillProperty::as_str,
    );
    assert_round_trips(
        "butterflyShapeType",
        ButterflyShape::from_ipc,
        ButterflyShape::as_str,
    );
    assert_round_trips(
        "donutShapeType",
        ConcentricShape::from_ipc,
        ConcentricShape::as_str,
    );
    assert_round_trips(
        "thermalShapeType",
        ConcentricShape::from_ipc,
        ConcentricShape::as_str,
    );
    assert_round_trips(
        "surfaceFinishType",
        FinishType::from_ipc,
        FinishType::as_str,
    );
    assert_round_trips(
        "productCriteriaType",
        ProductCriteria::from_ipc,
        ProductCriteria::as_str,
    );
    assert_round_trips("stepType", StepType::from_ipc, StepType::as_str);
    assert_round_trips("mountType", MountType::from_ipc, MountType::as_str);
    assert_round_trips(
        "layerFunctionType",
        LayerFunction::from_ipc,
        LayerFunction::as_str,
    );
    assert_round_trips("sideType", Side::from_ipc, Side::as_str);
    assert_round_trips("polarityType", Polarity::from_ipc, Polarity::as_str);
    assert_round_trips(
        "whereMeasuredType",
        WhereMeasured::from_ipc,
        WhereMeasured::as_str,
    );
    assert_round_trips(
        "platingStatusType",
        PlatingStatus::from_ipc,
        PlatingStatus::as_str,
    );
    assert_round_trips("padUseType", PadUse::from_ipc, PadUse::as_str);
    assert_round_trips(
        "cadPinType",
        PackagePinType::from_ipc,
        PackagePinType::as_str,
    );
    assert_round_trips(
        "pinElectricalType",
        PackagePinElectricalType::from_ipc,
        PackagePinElectricalType::as_str,
    );
    assert_round_trips(
        "pinMountType",
        PackagePinMountType::from_ipc,
        PackagePinMountType::as_str,
    );
    assert_round_trips(
        "pinPolarityType",
        PackagePinPolarity::from_ipc,
        PackagePinPolarity::as_str,
    );
    assert_round_trips(
        "bomCategoryType",
        BomCategory::from_ipc,
        BomCategory::as_str,
    );
    assert_round_trips(
        "geometryUsageType",
        GeometryUsage::from_ipc,
        GeometryUsage::as_str,
    );
    assert_round_trips("holeShapeType", HoleShape::from_ipc, HoleShape::as_str);
    assert_round_trips("floorLifeType", MoistureSensitivity::from_ipc, |level| {
        level.as_str()
    });
}
