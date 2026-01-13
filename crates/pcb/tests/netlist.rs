#![cfg(not(target_os = "windows"))]

use pcb_test_utils::assert_snapshot;
use pcb_test_utils::sandbox::Sandbox;

/// Helper to run netlist command and extract just position data for focused snapshot testing.
/// This avoids the noise of the full netlist JSON and focuses on position data verification.
fn snapshot_netlist_positions(sandbox: &mut Sandbox, program: &str, args: &[&str]) -> String {
    // Run the normal netlist command
    let full_output = sandbox.snapshot_run(program, args);

    // Parse JSON and extract position data if the build succeeded
    if full_output.contains("Exit Code: 0") && full_output.contains("--- STDOUT ---") {
        if let Some(json_start) = full_output.find("{") {
            if let Some(json_end) = full_output.rfind("}") {
                let json_str = &full_output[json_start..=json_end];
                if let Ok(netlist) = serde_json::from_str::<serde_json::Value>(json_str) {
                    return extract_position_data(sandbox, &netlist);
                }
            }
        }
    }

    // If parsing failed or build failed, return the original output
    full_output
}

fn extract_position_data(sandbox: &Sandbox, netlist: &serde_json::Value) -> String {
    use serde_json::json;

    let mut position_data = json!({});

    if let Some(instances) = netlist.get("instances").and_then(|i| i.as_object()) {
        for (instance_path, instance) in instances {
            if let Some(instance_obj) = instance.as_object() {
                let mut instance_positions = json!({});

                // Extract symbol_positions if present
                if let Some(symbol_pos) = instance_obj.get("symbol_positions") {
                    if let Some(symbol_pos_obj) = symbol_pos.as_object() {
                        if !symbol_pos_obj.is_empty() {
                            instance_positions["symbol_positions"] = symbol_pos.clone();
                        }
                    }
                }

                // Only include instances that have position data
                if !instance_positions.as_object().unwrap().is_empty() {
                    let sanitized_path = sandbox.sanitize_output(instance_path);
                    position_data[sanitized_path] = instance_positions;
                }
            }
        }
    }

    if position_data.as_object().unwrap().is_empty() {
        "No position data found in netlist".to_string()
    } else {
        serde_json::to_string_pretty(&position_data)
            .unwrap_or_else(|_| "Failed to serialize position data".to_string())
    }
}

const SIMPLE_BOARD_WITH_POSITIONS_ZEN: &str = r#"
# ```pcb
# [workspace]
# pcb-version = "0.3"
# ```

load("@stdlib/interfaces.zen", "Power", "Ground")

Resistor = Module("@stdlib/generics/Resistor.zen")
Led = Module("@stdlib/generics/Led.zen")

vcc = Power("VCC_3V3")
gnd = Ground("GND")
led_anode = Net("LED_ANODE")

Resistor(name="R1", value="330Ohm", package="0603", P1=vcc, P2=led_anode)
Led(name="D1", color="red", package="0603", A=led_anode, K=gnd)

# Position comments that should be parsed and included in netlist
# pcb:sch R1 x=100.0000 y=200.0000 rot=0
# pcb:sch D1 x=150.0000 y=200.0000 rot=90
# pcb:sch VCC_3V3_VCC.1 x=80.0000 y=180.0000 rot=0
# pcb:sch VCC_3V3_VCC.2 x=120.0000 y=180.0000 rot=0
# pcb:sch GND_GND.1 x=80.0000 y=220.0000 rot=0
# pcb:sch GND_GND.2 x=170.0000 y=220.0000 rot=0
# pcb:sch LED_ANODE x=125.0000 y=200.0000 rot=0
"#;

const HIERARCHICAL_BOARD_WITH_POSITIONS_ZEN: &str = r#"
load("@stdlib/interfaces.zen", "Power", "Ground", "Gpio")

LedModule = Module("../modules/LedModule.zen")

vcc_3v3 = Power("VCC_3V3")
gnd = Ground("GND")

LedModule(name="LED1", led_color="green", VCC=vcc_3v3, GND=gnd, CTRL=Gpio(NET=Net("LED_CTRL")))
LedModule(name="LED2", led_color="red", VCC=vcc_3v3, GND=gnd, CTRL=Gpio(NET=Net("LED_CTRL2")))

# Position comments for hierarchical design
# pcb:sch LED1.R1 x=100.0000 y=100.0000 rot=0
# pcb:sch LED1.D1 x=150.0000 y=100.0000 rot=90
# pcb:sch LED2.R1 x=100.0000 y=200.0000 rot=0
# pcb:sch LED2.D1 x=150.0000 y=200.0000 rot=90
# pcb:sch VCC_3V3_VCC.1 x=50.0000 y=150.0000 rot=0
# pcb:sch VCC_3V3_VCC.2 x=200.0000 y=150.0000 rot=0
# pcb:sch GND_GND.1 x=50.0000 y=250.0000 rot=0
# pcb:sch GND_GND.2 x=200.0000 y=250.0000 rot=0
# pcb:sch LED_CTRL_LED_CTRL x=80.0000 y=120.0000 rot=0
# pcb:sch LED_CTRL2_LED_CTRL2 x=80.0000 y=220.0000 rot=0
"#;

const WORKSPACE_PCB_TOML: &str = r#"
[workspace]
pcb-version = "0.3"
members = ["boards/*", "modules/*"]
"#;

const LED_MODULE_ZEN: &str = r#"
load("@stdlib/interfaces.zen", "Gpio", "Ground", "Power")

Resistor = Module("@stdlib/generics/Resistor.zen")
Led = Module("@stdlib/generics/Led.zen")

led_color = config("led_color", str, default="red")
r_value = config("r_value", str, default="330Ohm")
package = config("package", str, default="0603")

VCC = io("VCC", Power)
GND = io("GND", Ground)
CTRL = io("CTRL", Gpio)

led_anode = Net("LED_ANODE")

Resistor(name="R1", value=r_value, package=package, P1=VCC, P2=led_anode)
Led(name="D1", color=led_color, package=package, A=led_anode, K=CTRL)
"#;

#[test]
fn test_netlist_simple_board_with_positions() {
    let mut sandbox = Sandbox::new().allow_network();
    sandbox.write("boards/SimpleBoard.zen", SIMPLE_BOARD_WITH_POSITIONS_ZEN);
    let output = snapshot_netlist_positions(
        &mut sandbox,
        "pcb",
        &["build", "boards/SimpleBoard.zen", "--netlist"],
    );
    assert_snapshot!("netlist_simple_board_with_positions", output);
}

#[test]
fn test_netlist_hierarchical_board_with_positions() {
    let mut sandbox = Sandbox::new().allow_network();
    sandbox
        .write("pcb.toml", WORKSPACE_PCB_TOML)
        .write("modules/LedModule.zen", LED_MODULE_ZEN)
        .write(
            "boards/HierarchicalBoard.zen",
            HIERARCHICAL_BOARD_WITH_POSITIONS_ZEN,
        );
    let output = snapshot_netlist_positions(
        &mut sandbox,
        "pcb",
        &["build", "boards/HierarchicalBoard.zen", "--netlist"],
    );
    assert_snapshot!("netlist_hierarchical_board_with_positions", output);
}

#[test]
fn test_netlist_no_positions() {
    let board_zen = r#"
# ```pcb
# [workspace]
# pcb-version = "0.3"
# ```

load("@stdlib/interfaces.zen", "Power", "Ground")

Resistor = Module("@stdlib/generics/Resistor.zen")

vcc = Power("VCC")
gnd = Ground("GND")

Resistor(name="R1", value="1kOhm", package="0603", P1=vcc, P2=gnd)
"#;

    let mut sandbox = Sandbox::new().allow_network();
    sandbox.write("boards/NoPositions.zen", board_zen);
    let output = snapshot_netlist_positions(
        &mut sandbox,
        "pcb",
        &["build", "boards/NoPositions.zen", "--netlist"],
    );
    assert_snapshot!("netlist_no_positions", output);
}

#[test]
fn test_netlist_mixed_position_formats() {
    let board_zen = r#"
# ```pcb
# [workspace]
# pcb-version = "0.3"
# ```

load("@stdlib/interfaces.zen", "Power", "Ground")

Resistor = Module("@stdlib/generics/Resistor.zen")
Led = Module("@stdlib/generics/Led.zen")

vcc = Power("VCC")
gnd = Ground("GND")
sig = Net("SIGNAL")

Resistor(name="R1", value="1kOhm", package="0603", P1=vcc, P2=sig)
Led(name="D1", color="red", package="0603", A=sig, K=gnd)

# pcb:sch R1 x=100.0000 y=100.0000 rot=0
# pcb:sch VCC_VCC x=80.0000 y=80.0000 rot=0
# pcb:sch SIGNAL.1 x=125.0000 y=100.0000 rot=0
# pcb:sch SIGNAL.2 x=125.0000 y=150.0000 rot=0
"#;

    let mut sandbox = Sandbox::new().allow_network();
    sandbox.write("boards/MixedPositions.zen", board_zen);
    let output = snapshot_netlist_positions(
        &mut sandbox,
        "pcb",
        &["build", "boards/MixedPositions.zen", "--netlist"],
    );
    assert_snapshot!("netlist_mixed_position_formats", output);
}
