mod common;
use common::TestProject;

use pcb_sch::{AttributeValue, InstanceKind, bom::Alternative};

#[test]
fn part_and_alternatives_serialize_in_pcb_zen_layer() {
    let env = TestProject::new();

    env.add_file(
        "test.zen",
        r#"
P1 = Net()
P2 = Net()

primary = Part(
    mpn = "RC0603FR-0710KL",
    manufacturer = "Yageo",
    qualifications = ["AEC-Q200"],
)
alt = Part(
    mpn = "ERJ-3EKF1001V",
    manufacturer = "Panasonic",
)

Component(
    name = "R1",
    footprint = "Resistor_SMD:R_0603_1005Metric",
    pin_defs = {"1": "1", "2": "2"},
    pins = {"1": P1, "2": P2},
    part = primary,
    properties = {"alternatives": [alt]},
)
"#,
    );

    let eval_result = env.eval("test.zen");
    assert!(
        eval_result.is_success(),
        "eval failed: {:?}",
        eval_result.diagnostics
    );
    let eval_output = eval_result.output.expect("expected EvalOutput");

    let sch_result = eval_output.to_schematic_with_diagnostics();
    assert!(
        !sch_result.diagnostics.has_errors(),
        "schematic conversion failed: {:?}",
        sch_result.diagnostics
    );
    let schematic = sch_result.output.expect("expected schematic output");
    let component = schematic
        .instances
        .values()
        .find(|inst| inst.kind == InstanceKind::Component)
        .expect("expected component instance");

    assert_eq!(component.mpn().as_deref(), Some("RC0603FR-0710KL"));
    assert_eq!(component.manufacturer().as_deref(), Some("Yageo"));

    let part_json = match component.attributes.get("part") {
        Some(AttributeValue::Json(v)) => v,
        other => panic!("expected `part` JSON attribute, got: {:?}", other),
    };
    assert_eq!(
        part_json.get("mpn").and_then(|v| v.as_str()),
        Some("RC0603FR-0710KL")
    );
    assert_eq!(
        part_json.get("manufacturer").and_then(|v| v.as_str()),
        Some("Yageo")
    );
    assert_eq!(
        part_json.get("qualifications"),
        Some(&serde_json::json!(["AEC-Q200"]))
    );

    match component.attributes.get("alternatives") {
        Some(AttributeValue::Array(arr)) => {
            assert_eq!(arr.len(), 1);
            match &arr[0] {
                AttributeValue::Json(v) => {
                    assert_eq!(v.get("mpn").and_then(|x| x.as_str()), Some("ERJ-3EKF1001V"));
                    assert_eq!(
                        v.get("manufacturer").and_then(|x| x.as_str()),
                        Some("Panasonic")
                    );
                    assert_eq!(v.get("qualifications"), Some(&serde_json::json!([])));
                }
                other => panic!("expected JSON alternative entry, got {:?}", other),
            }
        }
        other => panic!("expected `alternatives` array attribute, got {:?}", other),
    }

    assert_eq!(
        component.alternatives_attr(),
        vec![Alternative {
            mpn: "ERJ-3EKF1001V".to_string(),
            manufacturer: "Panasonic".to_string(),
        }]
    );
}

#[test]
fn modifiers_can_mutate_part_and_alternatives_in_pcb_zen_layer() {
    let env = TestProject::new();

    env.add_file(
        "test.zen",
        r#"
P1 = Net()
P2 = Net()

def mutate(component):
    if component.name == "R1":
        component.part = Part(
            mpn = "PART-MOD",
            manufacturer = "MFR-MOD",
            qualifications = ["Preferred"],
        )
        component.alternatives = [Part(mpn = "ALT-1", manufacturer = "ALT-MFR-1")]
        component.alternatives.append(
            Part(mpn = "ALT-2", manufacturer = "ALT-MFR-2")
        )

builtin.add_component_modifier(mutate)

Component(
    name = "R1",
    footprint = "Resistor_SMD:R_0603_1005Metric",
    pin_defs = {"1": "1", "2": "2"},
    pins = {"1": P1, "2": P2},
)
"#,
    );

    let eval_result = env.eval("test.zen");
    assert!(
        eval_result.is_success(),
        "eval failed: {:?}",
        eval_result.diagnostics
    );
    let eval_output = eval_result.output.expect("expected EvalOutput");

    let sch_result = eval_output.to_schematic_with_diagnostics();
    assert!(
        !sch_result.diagnostics.has_errors(),
        "schematic conversion failed: {:?}",
        sch_result.diagnostics
    );
    let schematic = sch_result.output.expect("expected schematic output");
    let component = schematic
        .instances
        .values()
        .find(|inst| inst.kind == InstanceKind::Component)
        .expect("expected component instance");

    assert_eq!(component.mpn().as_deref(), Some("PART-MOD"));
    assert_eq!(component.manufacturer().as_deref(), Some("MFR-MOD"));

    let part_json = match component.attributes.get("part") {
        Some(AttributeValue::Json(v)) => v,
        other => panic!("expected `part` JSON attribute, got: {:?}", other),
    };
    assert_eq!(
        part_json.get("mpn").and_then(|v| v.as_str()),
        Some("PART-MOD")
    );
    assert_eq!(
        part_json.get("manufacturer").and_then(|v| v.as_str()),
        Some("MFR-MOD")
    );
    assert_eq!(
        part_json.get("qualifications"),
        Some(&serde_json::json!(["Preferred"]))
    );

    match component.attributes.get("alternatives") {
        Some(AttributeValue::Array(arr)) => {
            assert_eq!(arr.len(), 2);
            let mpns: Vec<_> = arr
                .iter()
                .map(|v| match v {
                    AttributeValue::Json(json) => json
                        .get("mpn")
                        .and_then(|x| x.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    other => panic!("expected JSON alternative entry, got {:?}", other),
                })
                .collect();
            assert_eq!(mpns, vec!["ALT-1".to_string(), "ALT-2".to_string()]);
        }
        other => panic!("expected `alternatives` array attribute, got {:?}", other),
    }
}

#[test]
fn kicad_netlist_remains_compatible_and_adds_part_property() {
    let env = TestProject::new();

    env.add_file(
        "test.zen",
        r#"
P1 = Net()
P2 = Net()

Component(
    name = "R1",
    footprint = "Resistor_SMD:R_0603_1005Metric",
    pin_defs = {"1": "1", "2": "2"},
    pins = {"1": P1, "2": P2},
    part = Part(
        mpn = "PART-123",
        manufacturer = "ACME",
        qualifications = ["Q1"],
    ),
)
"#,
    );

    let result = env.eval_netlist("test.zen");
    assert!(
        result.is_success(),
        "netlist eval failed: {:?}",
        result.diagnostics
    );
    let netlist = result.output.expect("expected netlist output");

    assert!(netlist.contains("(comp (ref \"U1\")"));
    assert!(netlist.contains("(value \"PART-123\")"));
    assert!(netlist.contains("(property (name \"manufacturer\") (value \"ACME\"))"));
    assert!(netlist.contains("(property (name \"part\") (value "));
}
