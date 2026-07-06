//! Persisted wire block (`# pcb:wire`) flow: zen source -> compiled schematic.

mod common;

use pcb_sch::wire::WireEndpoint;

fn eval_to_schematic(zen: &str) -> pcb_sch::Schematic {
    let result = common::eval_zen(vec![("test.zen".to_string(), zen.to_string())]);
    assert!(result.is_success(), "eval failed: {:?}", result.diagnostics);
    let eval_output = result.output.expect("expected EvalOutput on success");
    eval_output
        .to_schematic_with_diagnostics()
        .output
        .expect("expected schematic")
}

const TEST_ZEN: &str = r#"
gnd = Net("GND")
vcc = Net("VCC")

Component(
    name = "R1",
    prefix = "R",
    footprint = "TEST:0402",
    pin_defs = {"P1": "1", "P2": "2"},
    pins = {"P1": vcc, "P2": gnd},
)

Component(
    name = "R2",
    prefix = "R",
    footprint = "TEST:0402",
    pin_defs = {"P1": "1", "P2": "2"},
    pins = {"P1": vcc, "P2": gnd},
)

# pcb:wire-meta v1 nethash=0123456789abcdef
# pcb:wire v1 GND R1@2 R2@2 100.0000,50.0000;200.0000,50.0000
# pcb:wire v2 VCC R1@1 R2@1 future-format
# pcb:sch R1 x=100.0000 y=50.0000 rot=0
# pcb:sch R2 x=200.0000 y=50.0000 rot=0
"#;

#[test]
#[cfg(not(target_os = "windows"))]
fn wire_block_attaches_to_root_module_instance() {
    let schematic = eval_to_schematic(TEST_ZEN);

    let root = schematic.root().expect("root instance");
    let block = root
        .wire_block
        .as_ref()
        .expect("wire block parsed from source comments");

    assert_eq!(block.nethash, "0123456789abcdef");
    assert_eq!(block.wires.len(), 1, "only the v1 wire parses");
    let wire = &block.wires[0];
    assert_eq!(wire.net, "GND");
    assert_eq!(
        wire.ep_a,
        WireEndpoint::ComponentPin {
            key: "R1".to_string(),
            pin: "2".to_string()
        }
    );
    assert_eq!(wire.points_01mm, vec![(100.0, 50.0), (200.0, 50.0)]);

    // The unknown-version line is preserved, never parsed (P1 tolerance).
    assert_eq!(
        block.preserved_lines,
        vec!["# pcb:wire v2 VCC R1@1 R2@1 future-format".to_string()]
    );

    // Positions still parse alongside the wire block.
    assert_eq!(root.symbol_positions.len(), 2);
    assert!(root.symbol_positions.contains_key("comp:R1"));
    assert!(root.symbol_positions.contains_key("comp:R2"));
}

#[test]
#[cfg(not(target_os = "windows"))]
fn instance_without_wires_has_no_block() {
    let schematic = eval_to_schematic("\ngnd = Net(\"GND\")\n");
    let root = schematic.root().expect("root instance");
    assert!(root.wire_block.is_none());
}

#[test]
#[cfg(not(target_os = "windows"))]
fn schematic_nethash_tracks_net_partition() {
    let make = |net_name: &str| {
        let zen = TEST_ZEN.replace("\"GND\"", &format!("\"{net_name}\""));
        eval_to_schematic(&zen)
    };

    let base = pcb_sch::wire::compute_schematic_nethash(&make("GND"));
    let same = pcb_sch::wire::compute_schematic_nethash(&make("GND"));
    let renamed = pcb_sch::wire::compute_schematic_nethash(&make("AGND"));

    assert_eq!(base.len(), 16);
    assert_eq!(base, same, "nethash must be deterministic across compiles");
    assert_ne!(base, renamed, "net rename must change the nethash");
}
