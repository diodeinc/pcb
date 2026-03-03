use std::sync::Arc;

use pcb_sch::{AttributeValue, InstanceKind, bom::Alternative};
use pcb_zen_core::{DiagnosticsPass, EvalContext, SortPass};

mod common;
use common::InMemoryFileProvider;

fn eval(
    files: std::collections::HashMap<String, String>,
    main: &str,
) -> pcb_zen_core::WithDiagnostics<pcb_zen_core::EvalOutput> {
    let file_provider: Arc<dyn pcb_zen_core::FileProvider> =
        Arc::new(InMemoryFileProvider::new(files));
    let resolution = pcb_zen_core::resolution::ResolutionResult::empty();
    let ctx =
        EvalContext::new(file_provider, resolution).set_source_path(std::path::PathBuf::from(main));
    ctx.eval()
}

#[test]
#[cfg(not(target_os = "windows"))]
fn part_serializes_to_schematic_attributes() {
    let mut files = std::collections::HashMap::new();
    files.insert(
        "test.zen".to_string(),
        r#"
P1 = Net("P1")
P2 = Net("P2")

primary = builtin.Part(
    mpn = "RC0603FR-0710KL",
    manufacturer = "Yageo",
    qualifications = ["AEC-Q200"],
)
alt = builtin.Part(
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
"#
        .to_string(),
    );

    let eval_result = eval(files, "test.zen");
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
#[cfg(not(target_os = "windows"))]
fn typed_part_is_not_overwritten_by_legacy_part_property() {
    let mut files = std::collections::HashMap::new();
    files.insert(
        "test.zen".to_string(),
        r#"
P1 = Net("P1")
P2 = Net("P2")

primary = builtin.Part(
    mpn = "PART-TYPED",
    manufacturer = "MFR-TYPED",
    qualifications = ["Qualified"],
)

Component(
    name = "R1",
    footprint = "Resistor_SMD:R_0603_1005Metric",
    pin_defs = {"1": "1", "2": "2"},
    pins = {"1": P1, "2": P2},
    part = primary,
    properties = {
        "part": "legacy-string-value",
        "tag": "ok",
    },
)
"#
        .to_string(),
    );

    let eval_result = eval(files, "test.zen");
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

    let part_json = match component.attributes.get("part") {
        Some(AttributeValue::Json(v)) => v,
        other => panic!("expected typed `part` JSON attribute, got: {:?}", other),
    };
    assert_eq!(
        part_json.get("mpn").and_then(|v| v.as_str()),
        Some("PART-TYPED")
    );
    assert_eq!(
        part_json.get("manufacturer").and_then(|v| v.as_str()),
        Some("MFR-TYPED")
    );
    assert_eq!(
        part_json.get("qualifications"),
        Some(&serde_json::json!(["Qualified"]))
    );
    match component.attributes.get("tag") {
        Some(AttributeValue::String(v)) => assert_eq!(v, "ok"),
        other => panic!("expected `tag` string attribute, got: {:?}", other),
    }
}

#[test]
#[cfg(not(target_os = "windows"))]
fn part_overrides_explicit_mpn_and_manufacturer_without_warning() {
    let mut files = std::collections::HashMap::new();
    files.insert(
        "test.zen".to_string(),
        r#"
P1 = Net("P1")
P2 = Net("P2")

preferred = builtin.Part(
    mpn = "PART-A",
    manufacturer = "MFR-A",
    qualifications = ["Preferred"],
)

Component(
    name = "R1",
    footprint = "Resistor_SMD:R_0603_1005Metric",
    pin_defs = {"1": "1", "2": "2"},
    pins = {"1": P1, "2": P2},
    part = preferred,
    mpn = "PART-B",
    manufacturer = "MFR-B",
)
"#
        .to_string(),
    );

    let result = eval(files, "test.zen");
    assert!(result.is_success(), "eval failed: {:?}", result.diagnostics);

    let mut diagnostics = result.diagnostics;
    SortPass.apply(&mut diagnostics);
    let warnings = diagnostics.warnings();
    assert!(
        warnings.iter().all(|w| {
            !w.body.contains("overrides `part.mpn`")
                && !w.body.contains("overrides `part.manufacturer`")
        }),
        "unexpected part conflict warning(s): {:?}",
        warnings
    );

    let eval_output = result.output.expect("expected EvalOutput");
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

    assert_eq!(component.mpn().as_deref(), Some("PART-A"));
    assert_eq!(component.manufacturer().as_deref(), Some("MFR-A"));

    let part_json = match component.attributes.get("part") {
        Some(AttributeValue::Json(v)) => v,
        other => panic!("expected `part` JSON attribute, got: {:?}", other),
    };
    assert_eq!(
        part_json.get("mpn").and_then(|v| v.as_str()),
        Some("PART-A")
    );
    assert_eq!(
        part_json.get("manufacturer").and_then(|v| v.as_str()),
        Some("MFR-A")
    );
}

#[test]
#[cfg(not(target_os = "windows"))]
fn kicad_netlist_includes_part_property() {
    let mut files = std::collections::HashMap::new();
    files.insert(
        "test.zen".to_string(),
        r#"
P1 = Net("P1")
P2 = Net("P2")

preferred = builtin.Part(
    mpn = "PART-123",
    manufacturer = "ACME",
    qualifications = ["Q1"],
)

Component(
    name = "R1",
    footprint = "Resistor_SMD:R_0603_1005Metric",
    pin_defs = {"1": "1", "2": "2"},
    pins = {"1": P1, "2": P2},
    part = preferred,
)
"#
        .to_string(),
    );

    let eval_result = eval(files, "test.zen");
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
    let netlist = pcb_sch::kicad_netlist::to_kicad_netlist(&schematic);

    // Primary fields still serialize.
    assert!(netlist.contains("(comp (ref \"U1\")"));
    assert!(netlist.contains("(value \"PART-123\")"));
    assert!(netlist.contains("(property (name \"manufacturer\") (value \"ACME\"))"));

    // Typed part metadata is serialized as a property.
    assert!(netlist.contains("(property (name \"part\") (value "));
}

#[test]
#[cfg(not(target_os = "windows"))]
fn modifier_can_mutate_part_and_alternatives() {
    let mut files = std::collections::HashMap::new();
    files.insert(
        "test.zen".to_string(),
        r#"
P1 = Net("P1")
P2 = Net("P2")

def mutate(component):
    if component.name == "R1":
        component.part = builtin.Part(
            mpn = "PART-MOD",
            manufacturer = "MFR-MOD",
            qualifications = ["Preferred"],
        )
        component.alternatives = [
            builtin.Part(mpn = "ALT-1", manufacturer = "ALT-MFR-1"),
        ]

builtin.add_component_modifier(mutate)

Component(
    name = "R1",
    footprint = "Resistor_SMD:R_0603_1005Metric",
    pin_defs = {"1": "1", "2": "2"},
    pins = {"1": P1, "2": P2},
)
"#
        .to_string(),
    );

    let eval_result = eval(files, "test.zen");
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
            assert_eq!(arr.len(), 1);
            match &arr[0] {
                AttributeValue::Json(v) => {
                    assert_eq!(v.get("mpn").and_then(|x| x.as_str()), Some("ALT-1"));
                    assert_eq!(
                        v.get("manufacturer").and_then(|x| x.as_str()),
                        Some("ALT-MFR-1")
                    );
                }
                other => panic!("expected JSON alternative entry, got {:?}", other),
            }
        }
        other => panic!("expected `alternatives` array attribute, got {:?}", other),
    }
}

#[test]
#[cfg(not(target_os = "windows"))]
fn modifier_scalar_updates_keep_part_synced() {
    let mut files = std::collections::HashMap::new();
    files.insert(
        "test.zen".to_string(),
        r#"
P1 = Net("P1")
P2 = Net("P2")

def mutate(component):
    if component.name == "R1":
        component.mpn = "PART-UPDATED"
        component.manufacturer = "MFR-UPDATED"

builtin.add_component_modifier(mutate)

Component(
    name = "R1",
    footprint = "Resistor_SMD:R_0603_1005Metric",
    pin_defs = {"1": "1", "2": "2"},
    pins = {"1": P1, "2": P2},
    part = builtin.Part(
        mpn = "PART-BASE",
        manufacturer = "MFR-BASE",
        qualifications = ["CarryForward"],
    ),
)
"#
        .to_string(),
    );

    let eval_result = eval(files, "test.zen");
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

    assert_eq!(component.mpn().as_deref(), Some("PART-UPDATED"));
    assert_eq!(component.manufacturer().as_deref(), Some("MFR-UPDATED"));

    let part_json = match component.attributes.get("part") {
        Some(AttributeValue::Json(v)) => v,
        other => panic!("expected `part` JSON attribute, got: {:?}", other),
    };
    assert_eq!(
        part_json.get("mpn").and_then(|v| v.as_str()),
        Some("PART-UPDATED")
    );
    assert_eq!(
        part_json.get("manufacturer").and_then(|v| v.as_str()),
        Some("MFR-UPDATED")
    );
    assert_eq!(
        part_json.get("qualifications"),
        Some(&serde_json::json!(["CarryForward"]))
    );
}

#[test]
#[cfg(not(target_os = "windows"))]
fn modifier_can_append_to_alternatives_list() {
    let mut files = std::collections::HashMap::new();
    files.insert(
        "test.zen".to_string(),
        r#"
P1 = Net("P1")
P2 = Net("P2")

def mutate(component):
    if hasattr(component, "alternatives"):
        component.alternatives.append(
            builtin.Part(mpn = "ALT-2", manufacturer = "ALT-MFR-2")
        )

builtin.add_component_modifier(mutate)

Component(
    name = "R1",
    footprint = "Resistor_SMD:R_0603_1005Metric",
    pin_defs = {"1": "1", "2": "2"},
    pins = {"1": P1, "2": P2},
    properties = {
        "alternatives": [builtin.Part(mpn = "ALT-1", manufacturer = "ALT-MFR-1")],
    },
)
"#
        .to_string(),
    );

    let eval_result = eval(files, "test.zen");
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
