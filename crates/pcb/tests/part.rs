#![cfg(not(target_os = "windows"))]

use pcb_test_utils::sandbox::Sandbox;
use serde_json::{Map, Value};

fn parse_netlist_json(output: &str) -> Value {
    assert!(
        output.contains("Exit Code: 0"),
        "build failed, output was:\n{}",
        output
    );
    let json_start = output
        .find('{')
        .expect("expected JSON object start in snapshot output");
    let json_end = output
        .rfind('}')
        .expect("expected JSON object end in snapshot output");
    let json_str = &output[json_start..=json_end];
    serde_json::from_str::<Value>(json_str).expect("expected valid netlist JSON")
}

fn component_attrs(netlist: &Value) -> &Map<String, Value> {
    netlist
        .get("instances")
        .and_then(Value::as_object)
        .and_then(|instances| {
            instances.values().find_map(|inst| {
                if inst.get("kind").and_then(Value::as_str) == Some("Component") {
                    inst.get("attributes").and_then(Value::as_object)
                } else {
                    None
                }
            })
        })
        .expect("expected component instance with attributes")
}

#[test]
fn netlist_includes_part_and_alternatives_json() {
    let board = r#"
# ```pcb
# [workspace]
# pcb-version = "0.3"
# ```

P1 = Net("P1")
P2 = Net("P2")

Component(
    name = "R1",
    footprint = "Resistor_SMD:R_0603_1005Metric",
    pin_defs = {"1": "1", "2": "2"},
    pins = {"1": P1, "2": P2},
    part = builtin.Part(
        mpn = "PART-123",
        manufacturer = "ACME",
        qualifications = ["Q1"],
    ),
    properties = {
        "alternatives": [builtin.Part(mpn = "ALT-1", manufacturer = "ALT-MFR-1")],
    },
)
"#;

    let output = Sandbox::new()
        .write("boards/PartBoard.zen", board)
        .snapshot_run("pcb", ["build", "boards/PartBoard.zen", "--netlist"]);

    let netlist = parse_netlist_json(&output);
    let attrs = component_attrs(&netlist);

    assert_eq!(
        attrs["mpn"]["String"].as_str(),
        Some("PART-123"),
        "expected scalar mpn"
    );
    assert_eq!(
        attrs["manufacturer"]["String"].as_str(),
        Some("ACME"),
        "expected scalar manufacturer"
    );
    assert_eq!(
        attrs["part"]["Json"]["mpn"].as_str(),
        Some("PART-123"),
        "expected part.mpn JSON payload"
    );
    assert_eq!(
        attrs["part"]["Json"]["manufacturer"].as_str(),
        Some("ACME"),
        "expected part.manufacturer JSON payload"
    );
    assert_eq!(
        attrs["part"]["Json"]["qualifications"],
        serde_json::json!(["Q1"]),
        "expected part.qualifications JSON payload"
    );

    let alternatives = attrs["alternatives"]["Array"]
        .as_array()
        .expect("expected alternatives array");
    assert_eq!(alternatives.len(), 1);
    assert_eq!(
        alternatives[0]["Json"]["mpn"].as_str(),
        Some("ALT-1"),
        "expected alternatives[0].mpn JSON payload"
    );
}

#[test]
fn netlist_reflects_modifier_mutations_for_part_and_alternatives() {
    let board = r#"
# ```pcb
# [workspace]
# pcb-version = "0.3"
# ```

P1 = Net("P1")
P2 = Net("P2")

def mutate(component):
    if component.name == "R1":
        component.part = builtin.Part(
            mpn = "PART-MOD",
            manufacturer = "MFR-MOD",
            qualifications = ["Preferred"],
        )
        component.alternatives = [builtin.Part(mpn = "ALT-1", manufacturer = "ALT-MFR-1")]
        component.alternatives.append(
            builtin.Part(mpn = "ALT-2", manufacturer = "ALT-MFR-2")
        )

builtin.add_component_modifier(mutate)

Component(
    name = "R1",
    footprint = "Resistor_SMD:R_0603_1005Metric",
    pin_defs = {"1": "1", "2": "2"},
    pins = {"1": P1, "2": P2},
)
"#;

    let output = Sandbox::new()
        .write("boards/PartModifierBoard.zen", board)
        .snapshot_run(
            "pcb",
            ["build", "boards/PartModifierBoard.zen", "--netlist"],
        );

    let netlist = parse_netlist_json(&output);
    let attrs = component_attrs(&netlist);

    assert_eq!(attrs["mpn"]["String"].as_str(), Some("PART-MOD"));
    assert_eq!(attrs["manufacturer"]["String"].as_str(), Some("MFR-MOD"));
    assert_eq!(attrs["part"]["Json"]["mpn"].as_str(), Some("PART-MOD"));
    assert_eq!(
        attrs["part"]["Json"]["qualifications"],
        serde_json::json!(["Preferred"])
    );

    let alternatives = attrs["alternatives"]["Array"]
        .as_array()
        .expect("expected alternatives array");
    assert_eq!(alternatives.len(), 2);
    assert_eq!(alternatives[0]["Json"]["mpn"].as_str(), Some("ALT-1"));
    assert_eq!(alternatives[1]["Json"]["mpn"].as_str(), Some("ALT-2"));
}
