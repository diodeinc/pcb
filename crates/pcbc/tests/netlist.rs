#![cfg(not(target_os = "windows"))]

use pcb_test_utils::assert_snapshot;
use pcb_test_utils::sandbox::Sandbox;

/// Helper to run netlist command and extract just position data for focused snapshot testing.
/// This avoids the noise of the full netlist JSON and focuses on position data verification.
fn snapshot_netlist_positions(sandbox: &mut Sandbox, program: &str, args: &[&str]) -> String {
    // Run the normal netlist command
    let full_output = sandbox.snapshot_run(program, args);

    // Parse JSON and extract position data if the build succeeded
    if full_output.contains("Exit Code: 0")
        && full_output.contains("--- STDOUT ---")
        && let Some(json_start) = full_output.find("{")
        && let Some(json_end) = full_output.rfind("}")
    {
        let json_str = &full_output[json_start..=json_end];
        if let Ok(netlist) = serde_json::from_str::<serde_json::Value>(json_str) {
            return extract_position_data(sandbox, &netlist);
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
                if let Some(symbol_pos) = instance_obj.get("symbol_positions")
                    && let Some(symbol_pos_obj) = symbol_pos.as_object()
                    && !symbol_pos_obj.is_empty()
                {
                    instance_positions["symbol_positions"] = symbol_pos.clone();
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
# pcb-version = "0.4"
# ```

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
# pcb:sch VCC_3V3.1 x=80.0000 y=180.0000 rot=0
# pcb:sch VCC_3V3.2 x=120.0000 y=180.0000 rot=0
# pcb:sch GND.1 x=80.0000 y=220.0000 rot=0
# pcb:sch GND.2 x=170.0000 y=220.0000 rot=0
# pcb:sch LED_ANODE x=125.0000 y=200.0000 rot=0
"#;

const HIERARCHICAL_BOARD_WITH_POSITIONS_ZEN: &str = r#"
load("@stdlib/interfaces.zen", "Gpio")

LedModule = Module("../modules/LedModule.zen")

vcc_3v3 = Power("VCC_3V3")
gnd = Ground("GND")

LedModule(name="LED1", led_color="green", VCC=vcc_3v3, GND=gnd, CTRL=Gpio("LED_CTRL"))
LedModule(name="LED2", led_color="red", VCC=vcc_3v3, GND=gnd, CTRL=Gpio("LED_CTRL2"))

# Position comments for hierarchical design
# pcb:sch LED1.R1 x=100.0000 y=100.0000 rot=0
# pcb:sch LED1.D1 x=150.0000 y=100.0000 rot=90
# pcb:sch LED2.R1 x=100.0000 y=200.0000 rot=0
# pcb:sch LED2.D1 x=150.0000 y=200.0000 rot=90
# pcb:sch VCC_3V3.1 x=50.0000 y=150.0000 rot=0
# pcb:sch VCC_3V3.2 x=200.0000 y=150.0000 rot=0
# pcb:sch GND.1 x=50.0000 y=250.0000 rot=0
# pcb:sch GND.2 x=200.0000 y=250.0000 rot=0
# pcb:sch LED_CTRL_LED_CTRL x=80.0000 y=120.0000 rot=0
# pcb:sch LED_CTRL2_LED_CTRL2 x=80.0000 y=220.0000 rot=0
"#;

const WORKSPACE_PCB_TOML: &str = r#"
[workspace]
pcb-version = "0.4"
"#;

const SIMPLE_BOARD_WITH_MIRROR_POSITIONS_ZEN: &str = r#"
# ```pcb
# [workspace]
# pcb-version = "0.4"
# ```

Resistor = Module("@stdlib/generics/Resistor.zen")
Led = Module("@stdlib/generics/Led.zen")

vcc = Power("VCC_3V3")
gnd = Ground("GND")
led_anode = Net("LED_ANODE")

Resistor(name="R1", value="330Ohm", package="0603", P1=vcc, P2=led_anode)
Led(name="D1", color="red", package="0603", A=led_anode, K=gnd)

# Position comments with optional mirror
# pcb:sch R1 x=100.0000 y=200.0000 rot=0 mirror=x
# pcb:sch D1 x=150.0000 y=200.0000 rot=90
# pcb:sch VCC_3V3.1 x=80.0000 y=180.0000 rot=0 mirror=y
# pcb:sch VCC_3V3.2 x=120.0000 y=180.0000 rot=0
# pcb:sch GND.1 x=80.0000 y=220.0000 rot=0
# pcb:sch GND.2 x=170.0000 y=220.0000 rot=0
# pcb:sch LED_ANODE x=125.0000 y=200.0000 rot=0
"#;

const LED_MODULE_ZEN: &str = r#"
load("@stdlib/interfaces.zen", "Gpio")

Resistor = Module("@stdlib/generics/Resistor.zen")
Led = Module("@stdlib/generics/Led.zen")

led_color = config(str, default="red")
r_value = config(str, default="330Ohm")
package = config(str, default="0603")

VCC = io(Power)
GND = io(Ground)
CTRL = io(Gpio)

led_anode = Net("LED_ANODE")

Resistor(name="R1", value=r_value, package=package, P1=VCC, P2=led_anode)
Led(name="D1", color=led_color, package=package, A=led_anode, K=CTRL)
"#;

#[test]
fn test_netlist_simple_board_with_positions() {
    let mut sandbox = Sandbox::new();
    sandbox.write("boards/SimpleBoard.zen", SIMPLE_BOARD_WITH_POSITIONS_ZEN);
    let output = snapshot_netlist_positions(
        &mut sandbox,
        "pcbc",
        &["build", "boards/SimpleBoard.zen", "--netlist"],
    );
    assert_snapshot!("netlist_simple_board_with_positions", output);
}

#[test]
fn test_netlist_hierarchical_board_with_positions() {
    let mut sandbox = Sandbox::new();
    sandbox
        .write("pcb.toml", WORKSPACE_PCB_TOML)
        .write("modules/LedModule.zen", LED_MODULE_ZEN)
        .write(
            "boards/HierarchicalBoard.zen",
            HIERARCHICAL_BOARD_WITH_POSITIONS_ZEN,
        )
        .sync();
    let output = snapshot_netlist_positions(
        &mut sandbox,
        "pcbc",
        &["build", "boards/HierarchicalBoard.zen", "--netlist"],
    );
    assert_snapshot!("netlist_hierarchical_board_with_positions", output);
}

const I2C_MODULE_ZEN: &str = r#"
load("@stdlib/interfaces.zen", "I2c")

Resistor = Module("@stdlib/generics/Resistor.zen")

VDD = io(Power)
GND = io(Ground)
I2C = io(I2c)

Resistor(name="R_SCL", value="10kohm", package="0402", P1=VDD, P2=I2C.SCL)
Resistor(name="R_SDA", value="10kohm", package="0402", P1=VDD, P2=I2C.SDA)

# Hierarchical labels on multi-field interface bus (I2c) - SCL and SDA fields.
# pcb:sch I2C_SCL.0 x=10.0000 y=20.0000 rot=0
# pcb:sch I2C_SDA.0 x=10.0000 y=30.0000 rot=0
"#;

const I2C_HIERARCHICAL_BOARD_ZEN: &str = r#"
load("@stdlib/interfaces.zen", "I2c")

I2cPullups = Module("../modules/I2cPullups.zen")

vdd = Power("VDD_3V3")
gnd = Ground("GND")
bus = I2c("I2C_BUS")

I2cPullups(name="PU1", VDD=vdd, GND=gnd, I2C=bus)
"#;

#[test]
fn test_netlist_interface_field_positions() {
    let mut sandbox = Sandbox::new();
    sandbox
        .write("pcb.toml", WORKSPACE_PCB_TOML)
        .write("modules/I2cPullups.zen", I2C_MODULE_ZEN)
        .write("boards/I2cBoard.zen", I2C_HIERARCHICAL_BOARD_ZEN)
        .sync();
    let output = snapshot_netlist_positions(
        &mut sandbox,
        "pcbc",
        &["build", "boards/I2cBoard.zen", "--netlist"],
    );
    assert_snapshot!("netlist_interface_field_positions", output);
}

#[test]
fn test_netlist_no_positions() {
    let board_zen = r#"
# ```pcb
# [workspace]
# pcb-version = "0.4"
# ```

Resistor = Module("@stdlib/generics/Resistor.zen")

vcc = Power("VCC")
gnd = Ground("GND")

Resistor(name="R1", value="1kOhm", package="0603", P1=vcc, P2=gnd)
"#;

    let mut sandbox = Sandbox::new();
    sandbox.write("boards/NoPositions.zen", board_zen);
    let output = snapshot_netlist_positions(
        &mut sandbox,
        "pcbc",
        &["build", "boards/NoPositions.zen", "--netlist"],
    );
    assert_snapshot!("netlist_no_positions", output);
}

#[test]
fn test_netlist_mixed_position_formats() {
    let board_zen = r#"
# ```pcb
# [workspace]
# pcb-version = "0.4"
# ```

Resistor = Module("@stdlib/generics/Resistor.zen")
Led = Module("@stdlib/generics/Led.zen")

vcc = Power("VCC")
gnd = Ground("GND")
sig = Net("SIGNAL")

Resistor(name="R1", value="1kOhm", package="0603", P1=vcc, P2=sig)
Led(name="D1", color="red", package="0603", A=sig, K=gnd)

# pcb:sch R1 x=100.0000 y=100.0000 rot=0
# pcb:sch VCC x=80.0000 y=80.0000 rot=0
# pcb:sch SIGNAL.1 x=125.0000 y=100.0000 rot=0
# pcb:sch SIGNAL.2 x=125.0000 y=150.0000 rot=0
"#;

    let mut sandbox = Sandbox::new();
    sandbox.write("boards/MixedPositions.zen", board_zen);
    let output = snapshot_netlist_positions(
        &mut sandbox,
        "pcbc",
        &["build", "boards/MixedPositions.zen", "--netlist"],
    );
    assert_snapshot!("netlist_mixed_position_formats", output);
}

#[test]
fn test_netlist_positions_with_mirror() {
    let mut sandbox = Sandbox::new();
    sandbox.write(
        "boards/SimpleBoardWithMirror.zen",
        SIMPLE_BOARD_WITH_MIRROR_POSITIONS_ZEN,
    );
    let output = snapshot_netlist_positions(
        &mut sandbox,
        "pcbc",
        &["build", "boards/SimpleBoardWithMirror.zen", "--netlist"],
    );
    assert_snapshot!("netlist_positions_with_mirror", output);
}

/// Helper to run netlist command and extract just the nets section for focused snapshot testing.
/// This shows net names, kinds, and other metadata without the noise of instances.
fn snapshot_netlist_nets(sandbox: &mut Sandbox, program: &str, args: &[&str]) -> String {
    let full_output = sandbox.snapshot_run(program, args);

    if full_output.contains("Exit Code: 0")
        && full_output.contains("--- STDOUT ---")
        && let Some(json_start) = full_output.find('{')
        && let Some(json_end) = full_output.rfind('}')
    {
        let json_str = &full_output[json_start..=json_end];
        if let Ok(netlist) = serde_json::from_str::<serde_json::Value>(json_str)
            && let Some(nets) = netlist.get("nets")
        {
            return serde_json::to_string_pretty(nets)
                .unwrap_or_else(|_| "Failed to serialize nets".to_string());
        }
    }

    full_output
}

const NOT_CONNECTED_MODULE_ZEN: &str = r#"
vcc = io(Power)

Component(
    name = "R1",
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0402_1005Metric.kicad_mod"),
    pin_defs = {"1": "1"},
    skip_bom = True,
    pins = {"1": vcc},
)
"#;

const NOT_CONNECTED_BOARD_ZEN: &str = r#"
# ```pcb
# [workspace]
# pcb-version = "0.4"
# ```

PowerConsumer = Module("PowerConsumer.zen")

# NotConnected satisfies the Power IO but remains an open net
nc = NotConnected()

PowerConsumer(name = "U1", vcc = nc)
"#;

const IO_RENAMED_CHILD_ZEN: &str = r#"
IN_GD = io("IN_GD", Net)
GND = io("GND", Net)

Component(
    name = "R1",
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0402_1005Metric.kicad_mod"),
    pin_defs = {"1": "1", "2": "2"},
    skip_bom = True,
    pins = {"1": IN_GD, "2": GND},
)

# pcb:sch IN_GD.0 x=-351.5200 y=-140.7000 rot=0
# pcb:sch GND.1 x=-100.0000 y=-50.0000 rot=0
"#;

const IO_RENAMED_PARENT_ZEN: &str = r#"
Child = Module("Child.zen")

VBUS_RAW = Net("VBUS_RAW")
GND = Net("GND")

Child(name = "U1", IN_GD = VBUS_RAW, GND = GND)

# pcb:sch U1.VBUS_RAW.0 x=-1498.6000 y=-25.4000 rot=0
# pcb:sch U1.GND.1 x=-1562.1000 y=12.7000 rot=0
"#;

/// Descendant net-symbol overrides (`<child>.<NET>.<idx>` comments in the parent)
/// must survive conversion even when the parent renames the net across the module
/// boundary (here `IN_GD=VBUS_RAW`); the override key uses the global net name.
#[test]
fn test_netlist_descendant_net_symbol_override_with_renamed_io() {
    let mut sandbox = Sandbox::new().with_workspace();
    sandbox
        .write("boards/Child.zen", IO_RENAMED_CHILD_ZEN)
        .write("boards/Parent.zen", IO_RENAMED_PARENT_ZEN);
    let output = sandbox.snapshot_run("pcbc", &["build", "boards/Parent.zen", "--netlist"]);

    let json_start = output.find('{').expect("netlist JSON in output");
    let json_end = output.rfind('}').expect("netlist JSON in output");
    let netlist: serde_json::Value =
        serde_json::from_str(&output[json_start..=json_end]).expect("netlist parses as JSON");

    let root_positions = netlist["instances"]
        .as_object()
        .unwrap()
        .iter()
        .find_map(|(path, inst)| path.ends_with(":<root>").then(|| &inst["symbol_positions"]))
        .expect("root instance present");

    // Renamed io: parent override uses the global net name, not the child's local name.
    assert_eq!(root_positions["sym:U1.VBUS_RAW#0"]["x"], -1498.6);
    // Same-named net keeps working.
    assert_eq!(root_positions["sym:U1.GND#1"]["x"], -1562.1);
}

#[test]
fn test_netlist_not_connected_open_intent() {
    let mut sandbox = Sandbox::new();
    sandbox
        .write("boards/PowerConsumer.zen", NOT_CONNECTED_MODULE_ZEN)
        .write("boards/NCBoard.zen", NOT_CONNECTED_BOARD_ZEN);
    let output = snapshot_netlist_nets(
        &mut sandbox,
        "pcbc",
        &["build", "boards/NCBoard.zen", "--netlist"],
    );
    assert_snapshot!("netlist_not_connected_open_intent", output);
}
