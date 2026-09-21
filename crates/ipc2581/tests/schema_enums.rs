use ipc2581::{Ipc2581, LayerFunction, PadUse};

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
