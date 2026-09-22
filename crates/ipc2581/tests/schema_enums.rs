use ipc2581::Ipc2581Error;

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

/// Every token of `simple_type` parses and is written back unchanged.
fn assert_round_trips<T>(
    simple_type: &str,
    from_ipc: fn(&str) -> Result<T, Ipc2581Error>,
    as_str: fn(T) -> &'static str,
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

    macro_rules! round_trips {
        ($($simple_type:literal => $ty:ty),+ $(,)?) => {
            $(assert_round_trips($simple_type, <$ty>::from_ipc, <$ty>::as_str);)+
        };
    }
    round_trips! {
        "modeType" => Mode,
        "unitsType" => Units,
        "lineEndType" => LineEnd,
        "linePropertyType" => LineProperty,
        "fillPropertyType" => FillProperty,
        "butterflyShapeType" => ButterflyShape,
        "donutShapeType" => ConcentricShape,
        "thermalShapeType" => ConcentricShape,
        "surfaceFinishType" => FinishType,
        "productCriteriaType" => ProductCriteria,
        "stepType" => StepType,
        "mountType" => MountType,
        "layerFunctionType" => LayerFunction,
        "sideType" => Side,
        "polarityType" => Polarity,
        "whereMeasuredType" => WhereMeasured,
        "platingStatusType" => PlatingStatus,
        "padUseType" => PadUse,
        "cadPinType" => PackagePinType,
        "pinElectricalType" => PackagePinElectricalType,
        "pinMountType" => PackagePinMountType,
        "pinPolarityType" => PackagePinPolarity,
        "bomCategoryType" => BomCategory,
        "geometryUsageType" => GeometryUsage,
        "holeShapeType" => HoleShape,
        "floorLifeType" => MoistureSensitivity,
    }
}
