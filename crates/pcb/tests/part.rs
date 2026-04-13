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

fn module_signature_params(netlist: &Value) -> &[Value] {
    netlist
        .get("instances")
        .and_then(Value::as_object)
        .and_then(|instances| {
            instances.values().find_map(|inst| {
                if inst.get("kind").and_then(Value::as_str) == Some("Module") {
                    inst.get("attributes")
                        .and_then(Value::as_object)
                        .and_then(|attrs| attrs.get("__signature"))
                        .and_then(|signature| signature.get("Json"))
                        .and_then(|json| json.get("parameters"))
                        .and_then(Value::as_array)
                } else {
                    None
                }
            })
        })
        .expect("expected module instance with __signature parameters")
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
    part = Part(
        mpn = "PART-123",
        manufacturer = "ACME",
        qualifications = ["Q1"],
    ),
    properties = {
        "alternatives": [Part(mpn = "ALT-1", manufacturer = "ALT-MFR-1")],
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

#[test]
fn netlist_signature_includes_io_direction_metadata() {
    let board = r#"
# ```pcb
# [workspace]
# pcb-version = "0.3"
# ```

Child = Module("Child.zen")

Child(name = "U1", VIN = Net("VIN"), VOUT = Net("VOUT"), BIDIR = Net("IO"))
"#;

    let child = r#"
VIN = io("VIN", Net, direction = "input")
VOUT = io("VOUT", Net, direction = "output")
BIDIR = io("BIDIR", Net)

Component(
    name = "U",
    footprint = "TEST:0402",
    pin_defs = {"IN": "1", "OUT": "2", "IO": "3"},
    pins = {"IN": VIN, "OUT": VOUT, "IO": BIDIR},
)
"#;

    let output = Sandbox::new()
        .write("board.zen", board)
        .write("Child.zen", child)
        .snapshot_run("pcb", ["build", "board.zen", "--netlist"]);

    let netlist = parse_netlist_json(&output);
    let params = module_signature_params(&netlist);

    let vin = params
        .iter()
        .find(|param| param["name"].as_str() == Some("VIN"))
        .expect("expected VIN parameter");
    assert_eq!(vin["direction"].as_str(), Some("input"));

    let vout = params
        .iter()
        .find(|param| param["name"].as_str() == Some("VOUT"))
        .expect("expected VOUT parameter");
    assert_eq!(vout["direction"].as_str(), Some("output"));

    let bidir = params
        .iter()
        .find(|param| param["name"].as_str() == Some("BIDIR"))
        .expect("expected BIDIR parameter");
    assert!(
        bidir.get("direction").is_none(),
        "expected no direction for BIDIR, got {bidir:?}"
    );
}

// --- Manifest-inherited parts (symbol_parts) tests ---

const MINIMAL_KICAD_SYM: &str = r#"(kicad_symbol_lib
  (version 20241209)
  (symbol "TestPart"
    (property "Reference" "U" (at 0 0 0) (effects (font (size 1.27 1.27))))
    (property "Value" "TestPart" (at 0 -2.54 0) (effects (font (size 1.27 1.27))))
    (property "Footprint" "TestPart" (at 0 0 0) (effects (font (size 1.27 1.27)) hide))
    (symbol "TestPart_0_1"
      (pin input line (at -5.08 0 0) (length 2.54) (name "P1" (effects (font (size 1.27 1.27)))) (number "1" (effects (font (size 1.27 1.27)))))
      (pin input line (at 5.08 0 180) (length 2.54) (name "P2" (effects (font (size 1.27 1.27)))) (number "2" (effects (font (size 1.27 1.27)))))
    )
  )
)"#;

const TEST_KICAD_MOD: &str = r#"(footprint "TestPart"
  (layer "F.Cu")
  (pad "1" smd rect (at -1 0) (size 1 1) (layers "F.Cu"))
  (pad "2" smd rect (at 1 0) (size 1 1) (layers "F.Cu"))
)"#;

fn manifest_component_attrs(parts_toml: &str, component_args: &str) -> Map<String, Value> {
    let mut sb = Sandbox::new();

    sb.git_fixture("https://github.com/testorg/components.git")
        .write("MyPart/pcb.toml", parts_toml)
        .write("MyPart/TestPart.kicad_sym", MINIMAL_KICAD_SYM)
        .write("MyPart/TestPart.kicad_mod", TEST_KICAD_MOD)
        .write(
            "MyPart/MyPart.zen",
            format!(
                r#"
P1 = io("P1", Net)
P2 = io("P2", Net)

Component(
    name = "U",
    symbol = Symbol(library = "TestPart.kicad_sym"),
    pins = {{"P1": P1, "P2": P2}},
{component_args}
)
"#
            ),
        )
        .commit("Add test component")
        .tag("MyPart/v1.0.0", false)
        .push_mirror();

    let output = sb
        .write(
            "pcb.toml",
            r#"
[workspace]
pcb-version = "0.3"

[dependencies]
"github.com/testorg/components/MyPart" = "1.0.0"
"#,
        )
        .write(
            "board.zen",
            r#"
MyPart = Module("github.com/testorg/components/MyPart/MyPart.zen")

MyPart(name = "U1", P1 = Net("A"), P2 = Net("B"))
"#,
        )
        .snapshot_run("pcb", ["build", "board.zen", "--netlist"]);

    let netlist = parse_netlist_json(&output);
    component_attrs(&netlist).clone()
}

#[test]
fn manifest_parts_single_part_inherited() {
    let attrs = manifest_component_attrs(
        r#"
parts = [
  { mpn = "TEST-MPN-001", symbol = "TestPart.kicad_sym", manufacturer = "TestCorp" },
]
"#,
        "",
    );

    assert_eq!(
        attrs["mpn"]["String"].as_str(),
        Some("TEST-MPN-001"),
        "part.mpn should be inherited from manifest"
    );
    assert_eq!(
        attrs["manufacturer"]["String"].as_str(),
        Some("TestCorp"),
        "part.manufacturer should be inherited from manifest"
    );
    assert_eq!(
        attrs["part"]["Json"]["mpn"].as_str(),
        Some("TEST-MPN-001"),
        "part JSON payload should be present"
    );
    assert!(
        attrs.get("alternatives").is_none(),
        "no alternatives when manifest has a single part"
    );
}

#[test]
fn manifest_parts_multiple_parts_with_alternatives() {
    let attrs = manifest_component_attrs(
        r#"
parts = [
  { mpn = "PRIMARY-001", symbol = "TestPart.kicad_sym", manufacturer = "PrimaryCorp", qualifications = ["Preferred"] },
  { mpn = "ALT-001", symbol = "TestPart.kicad_sym", manufacturer = "AltCorp1", qualifications = ["Approved"] },
  { mpn = "ALT-002", symbol = "TestPart.kicad_sym", manufacturer = "AltCorp2" },
]
"#,
        "",
    );

    assert_eq!(attrs["mpn"]["String"].as_str(), Some("PRIMARY-001"));
    assert_eq!(
        attrs["manufacturer"]["String"].as_str(),
        Some("PrimaryCorp")
    );
    assert_eq!(attrs["part"]["Json"]["mpn"].as_str(), Some("PRIMARY-001"));
    assert_eq!(
        attrs["part"]["Json"]["qualifications"],
        serde_json::json!(["Preferred"])
    );

    let alternatives = attrs["alternatives"]["Array"]
        .as_array()
        .expect("expected alternatives array from manifest");
    assert_eq!(alternatives.len(), 2);
    assert_eq!(alternatives[0]["Json"]["mpn"].as_str(), Some("ALT-001"));
    assert_eq!(
        alternatives[0]["Json"]["manufacturer"].as_str(),
        Some("AltCorp1")
    );
    assert_eq!(
        alternatives[0]["Json"]["qualifications"],
        serde_json::json!(["Approved"])
    );
    assert_eq!(alternatives[1]["Json"]["mpn"].as_str(), Some("ALT-002"));
    assert_eq!(
        alternatives[1]["Json"]["manufacturer"].as_str(),
        Some("AltCorp2")
    );
    assert_eq!(
        alternatives[1]["Json"]["qualifications"],
        serde_json::json!([])
    );
}

#[test]
fn manifest_parts_explicit_part_overrides_manifest() {
    let attrs = manifest_component_attrs(
        r#"
parts = [
  { mpn = "MANIFEST-001", symbol = "TestPart.kicad_sym", manufacturer = "ManifestCorp" },
]
"#,
        r#"    part = Part(
        mpn = "EXPLICIT-999",
        manufacturer = "ExplicitCorp",
    ),"#,
    );

    assert_eq!(
        attrs["mpn"]["String"].as_str(),
        Some("EXPLICIT-999"),
        "explicit part should override manifest"
    );
    assert_eq!(
        attrs["manufacturer"]["String"].as_str(),
        Some("ExplicitCorp"),
        "explicit manufacturer should override manifest"
    );
}

#[test]
fn manifest_parts_append_to_existing_alternatives_when_part_is_explicit() {
    let attrs = manifest_component_attrs(
        r#"
parts = [
  { mpn = "MANIFEST-001", symbol = "TestPart.kicad_sym", manufacturer = "ManifestCorp", qualifications = ["Preferred"] },
  { mpn = "MANIFEST-002", symbol = "TestPart.kicad_sym", manufacturer = "AltCorp" },
]
"#,
        r#"    part = Part(
        mpn = "EXPLICIT-999",
        manufacturer = "ExplicitCorp",
    ),
    properties = {
        "alternatives": [Part(mpn = "USER-ALT-1", manufacturer = "UserCorp")],
    },"#,
    );

    assert_eq!(attrs["mpn"]["String"].as_str(), Some("EXPLICIT-999"));
    assert_eq!(
        attrs["manufacturer"]["String"].as_str(),
        Some("ExplicitCorp")
    );
    assert_eq!(attrs["part"]["Json"]["mpn"].as_str(), Some("EXPLICIT-999"));

    let alternatives = attrs["alternatives"]["Array"]
        .as_array()
        .expect("expected alternatives array");
    assert_eq!(alternatives.len(), 3);
    assert_eq!(alternatives[0]["Json"]["mpn"].as_str(), Some("USER-ALT-1"));
    assert_eq!(
        alternatives[1]["Json"]["mpn"].as_str(),
        Some("MANIFEST-001")
    );
    assert_eq!(
        alternatives[2]["Json"]["mpn"].as_str(),
        Some("MANIFEST-002")
    );
    assert_eq!(
        alternatives[1]["Json"]["qualifications"],
        serde_json::json!(["Preferred"])
    );
}

#[test]
fn component_inherits_local_symbol_datasheet() {
    let output = Sandbox::new()
        .write(
            "components/TestPart/Part.kicad_sym",
            r#"(kicad_symbol_lib
  (version 20241209)
  (symbol "TestPart"
    (property "Reference" "U" (at 0 0 0) (effects (font (size 1.27 1.27))))
    (property "Value" "TestPart" (at 0 -2.54 0) (effects (font (size 1.27 1.27))))
    (property "Footprint" "Part" (at 0 0 0) (effects (font (size 1.27 1.27)) hide))
    (property "Datasheet" "docs/Part.pdf" (at 0 0 0) (effects (font (size 1.27 1.27)) hide))
    (symbol "TestPart_0_1"
      (pin input line (at -5.08 0 0) (length 2.54) (name "P1" (effects (font (size 1.27 1.27)))) (number "1" (effects (font (size 1.27 1.27)))))
      (pin input line (at 5.08 0 180) (length 2.54) (name "P2" (effects (font (size 1.27 1.27)))) (number "2" (effects (font (size 1.27 1.27)))))
    )
  )
)"#,
        )
        .write(
            "components/TestPart/Part.kicad_mod",
            r#"(footprint "Part"
  (layer "F.Cu")
  (pad "1" smd rect (at -1 0) (size 1 1) (layers "F.Cu"))
  (pad "2" smd rect (at 1 0) (size 1 1) (layers "F.Cu"))
)"#,
        )
        .write("components/TestPart/docs/Part.pdf", "%PDF-1.4\n%")
        .write(
            "components/TestPart/Part.zen",
            r#"
P1 = io("P1", Net)
P2 = io("P2", Net)

Component(
    name = "U",
    symbol = Symbol(library = "Part.kicad_sym"),
    pins = {"P1": P1, "P2": P2},
)
"#,
        )
        .write(
            "board.zen",
            r#"
Part = Module("components/TestPart/Part.zen")

Part(name = "U1", P1 = Net("A"), P2 = Net("B"))
"#,
        )
        .snapshot_run("pcb", ["build", "board.zen", "--netlist"]);

    let netlist = parse_netlist_json(&output);
    let attrs = component_attrs(&netlist);
    assert_eq!(
        attrs["datasheet"]["String"].as_str(),
        Some("package://workspace/components/TestPart/docs/Part.pdf")
    );
}

#[test]
fn component_drops_invalid_inherited_symbol_datasheet() {
    let output = Sandbox::new()
        .write(
            "components/TestPart/Part.kicad_sym",
            r#"(kicad_symbol_lib
  (version 20241209)
  (symbol "TestPart"
    (property "Reference" "U" (at 0 0 0) (effects (font (size 1.27 1.27))))
    (property "Value" "TestPart" (at 0 -2.54 0) (effects (font (size 1.27 1.27))))
    (property "Footprint" "Part" (at 0 0 0) (effects (font (size 1.27 1.27)) hide))
    (property "Datasheet" "missing/Part.pdf" (at 0 0 0) (effects (font (size 1.27 1.27)) hide))
    (symbol "TestPart_0_1"
      (pin input line (at -5.08 0 0) (length 2.54) (name "P1" (effects (font (size 1.27 1.27)))) (number "1" (effects (font (size 1.27 1.27)))))
      (pin input line (at 5.08 0 180) (length 2.54) (name "P2" (effects (font (size 1.27 1.27)))) (number "2" (effects (font (size 1.27 1.27)))))
    )
  )
)"#,
        )
        .write("components/TestPart/Part.kicad_mod", TEST_KICAD_MOD)
        .write(
            "components/TestPart/Part.zen",
            r#"
P1 = io("P1", Net)
P2 = io("P2", Net)

Component(
    name = "U",
    symbol = Symbol(library = "Part.kicad_sym"),
    pins = {"P1": P1, "P2": P2},
)
"#,
        )
        .write(
            "board.zen",
            r#"
Part = Module("components/TestPart/Part.zen")

Part(name = "U1", P1 = Net("A"), P2 = Net("B"))
"#,
        )
        .snapshot_run("pcb", ["build", "board.zen", "--netlist"]);

    let netlist = parse_netlist_json(&output);
    let attrs = component_attrs(&netlist);
    assert!(
        !attrs.contains_key("datasheet"),
        "expected invalid inherited datasheet to be dropped, attrs were: {attrs:#?}"
    );
}

#[test]
fn component_inherits_skip_bom_from_symbol_in_bom() {
    let sym_not_in_bom = r#"(kicad_symbol_lib
  (version 20241209)
  (symbol "TestPart" (in_bom no) (on_board yes)
    (property "Reference" "U" (at 0 0 0) (effects (font (size 1.27 1.27))))
    (property "Value" "TestPart" (at 0 -2.54 0) (effects (font (size 1.27 1.27))))
    (property "Footprint" "TestPart" (at 0 0 0) (effects (font (size 1.27 1.27)) hide))
    (symbol "TestPart_0_1"
      (pin input line (at -5.08 0 0) (length 2.54) (name "P1" (effects (font (size 1.27 1.27)))) (number "1" (effects (font (size 1.27 1.27)))))
      (pin input line (at 5.08 0 180) (length 2.54) (name "P2" (effects (font (size 1.27 1.27)))) (number "2" (effects (font (size 1.27 1.27)))))
    )
  )
)"#;

    // Symbol has in_bom=no → component should inherit skip_bom=true
    let output = Sandbox::new()
        .write("components/TestPart/TestPart.kicad_sym", sym_not_in_bom)
        .write("components/TestPart/TestPart.kicad_mod", TEST_KICAD_MOD)
        .write(
            "components/TestPart/TestPart.zen",
            r#"
P1 = io("P1", Net)
P2 = io("P2", Net)

Component(
    name = "U",
    symbol = Symbol(library = "TestPart.kicad_sym"),
    pins = {"P1": P1, "P2": P2},
)
"#,
        )
        .write(
            "board.zen",
            r#"
# ```pcb
# [workspace]
# pcb-version = "0.3"
# ```

TestPart = Module("components/TestPart/TestPart.zen")

TestPart(name = "U1", P1 = Net("A"), P2 = Net("B"))
"#,
        )
        .snapshot_run("pcb", ["build", "board.zen", "--netlist"]);

    let netlist = parse_netlist_json(&output);
    let attrs = component_attrs(&netlist);
    assert_eq!(
        attrs["skip_bom"]["Boolean"].as_bool(),
        Some(true),
        "symbol with in_bom=no should set skip_bom=true"
    );

    // Symbol has in_bom=yes → component should have skip_bom=false (default)
    let output2 = Sandbox::new()
        .write("components/TestPart/TestPart.kicad_sym", MINIMAL_KICAD_SYM)
        .write("components/TestPart/TestPart.kicad_mod", TEST_KICAD_MOD)
        .write(
            "components/TestPart/TestPart.zen",
            r#"
P1 = io("P1", Net)
P2 = io("P2", Net)

Component(
    name = "U",
    symbol = Symbol(library = "TestPart.kicad_sym"),
    pins = {"P1": P1, "P2": P2},
)
"#,
        )
        .write(
            "board.zen",
            r#"
# ```pcb
# [workspace]
# pcb-version = "0.3"
# ```

TestPart = Module("components/TestPart/TestPart.zen")

TestPart(name = "U1", P1 = Net("A"), P2 = Net("B"))
"#,
        )
        .snapshot_run("pcb", ["build", "board.zen", "--netlist"]);

    let netlist2 = parse_netlist_json(&output2);
    let attrs2 = component_attrs(&netlist2);
    assert!(
        attrs2.get("skip_bom").is_none() || attrs2["skip_bom"]["Boolean"].as_bool() == Some(false),
        "symbol with in_bom=yes should not set skip_bom"
    );
}
