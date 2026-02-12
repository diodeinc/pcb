#![cfg(not(target_os = "windows"))]

use pcb_test_utils::assert_snapshot;
use pcb_test_utils::sandbox::Sandbox;
use std::fs;

const LED_MODULE_ZEN: &str = r#"
load("@stdlib/interfaces.zen", "Gpio", "Ground", "Power")

Resistor = Module("@stdlib/generics/Resistor.zen")
Led = Module("@stdlib/generics/Led.zen")

led_color = config("led_color", str, default = "red")
r_value = config("r_value", str, default = "330Ohm")
package = config("package", str, default = "0603")

VCC = io("VCC", Power)
GND = io("GND", Ground)
CTRL = io("CTRL", Gpio)

led_anode = Net("LED_ANODE")

Resistor(name = "R1", value = r_value, package = package, P1 = VCC.NET, P2 = led_anode)
Led(name = "D1", color = led_color, package = package, A = led_anode, K = CTRL.NET)
"#;

const TEST_BOARD_ZEN: &str = r#"
load("@stdlib/interfaces.zen", "Gpio", "Ground", "Power")

LedModule = Module("../modules/LedModule.zen")
Resistor = Module("@stdlib/generics/Resistor.zen")
Capacitor = Module("@stdlib/generics/Capacitor.zen")
Crystal = Module("@stdlib/generics/Crystal.zen")

vcc_3v3 = Power("VCC_3V3")
gnd = Ground("GND")
led_ctrl = Gpio("LED_CTRL")
osc_xi = Gpio("OSC_XI")
osc_xo = Gpio("OSC_XO")

Capacitor(name = "C1", value = "100nF", package = "0402", P1 = vcc_3v3.NET, P2 = gnd.NET)
Capacitor(name = "C2", value = "10uF", package = "0805", P1 = vcc_3v3.NET, P2 = gnd.NET)

LedModule(name = "LED1", led_color = "green", VCC = vcc_3v3, GND = gnd, CTRL = led_ctrl)
LedModule(name = "LED2", led_color = "red", VCC = vcc_3v3, GND = gnd, CTRL = Gpio(NET = gnd.NET))

Crystal(name = "X1", frequency = "16MHz", load_capacitance = "18pF", package = "5032_2Pin", XIN = osc_xi.NET, XOUT = osc_xo.NET, GND = gnd.NET)

Capacitor(name = "C3", value = "22pF", package = "0402", P1 = osc_xi.NET, P2 = gnd.NET)
Capacitor(name = "C4", value = "22pF", package = "0402", P1 = osc_xo.NET, P2 = gnd.NET)

Resistor(name = "R1", value = "10kOhm", package = "0603", P1 = vcc_3v3.NET, P2 = led_ctrl.NET)
"#;

const SIMPLE_RESISTOR_BOARD_ZEN: &str = r#"
# ```pcb
# [workspace]
# pcb-version = "0.3"
# ```

load("@stdlib/interfaces.zen", "Power", "Ground")

Resistor = Module("@stdlib/generics/Resistor.zen")

vcc = Power("VCC")
gnd = Ground("GND")

Resistor(name = "R1", value = "1kOhm", package = "0603", P1 = vcc.NET, P2 = gnd.NET)
Resistor(name = "R2", value = "1kOhm", package = "0603", P1 = vcc.NET, P2 = gnd.NET)
Resistor(name = "R3", value = "4.7kOhm", package = "0402", P1 = vcc.NET, P2 = gnd.NET)
"#;

const CAPACITOR_BOARD_ZEN: &str = r#"
# ```pcb
# [workspace]
# pcb-version = "0.3"
# ```

load("@stdlib/interfaces.zen", "Power", "Ground")

Capacitor = Module("@stdlib/generics/Capacitor.zen")

vcc = Power("VCC")
gnd = Ground("GND")

Capacitor(name = "C1", value = "100nF", package = "0402", voltage = "16V", dielectric = "X7R", P1 = vcc.NET, P2 = gnd.NET)
Capacitor(name = "C2", value = "10uF", package = "0805", voltage = "25V", dielectric = "X5R", P1 = vcc.NET, P2 = gnd.NET)
Capacitor(name = "C3", value = "1uF", package = "0603", P1 = vcc.NET, P2 = gnd.NET)
"#;

const SKIP_BOM_BOARD_ZEN: &str = r#"
# ```pcb
# [workspace]
# pcb-version = "0.3"
# ```

P1 = Net("P1")
P2 = Net("P2")

# Normal resistor - should appear in BOM
Component(
    name = "normal",
    footprint = "Resistor_SMD:R_0603_1005Metric",
    pin_defs = {"1": "1", "2": "2"},
    pins = {"1": P1, "2": P2},
    mpn = "RC0603FR-071KL",
    manufacturer = "Yageo",
)

# Component with skip_bom kwarg - should NOT appear in BOM
Component(
    name = "skip_bom_kwarg",
    footprint = "Resistor_SMD:R_0603_1005Metric",
    pin_defs = {"1": "1", "2": "2"},
    pins = {"1": P1, "2": P2},
    skip_bom = True,
)

# Component with legacy Exclude_from_bom property - should NOT appear in BOM
Component(
    name = "exclude_from_bom_legacy",
    footprint = "Resistor_SMD:R_0603_1005Metric",
    pin_defs = {"1": "1", "2": "2"},
    pins = {"1": P1, "2": P2},
    properties = {"Exclude_from_bom": True},
)

# Normal resistor - should appear in BOM
Component(
    name = "normal2",
    footprint = "Resistor_SMD:R_0603_1005Metric",
    pin_defs = {"1": "1", "2": "2"},
    pins = {"1": P1, "2": P2},
    mpn = "RC0603FR-0710KL",
    manufacturer = "Yageo",
)
"#;

const DNP_BOARD_ZEN: &str = r#"
# ```pcb
# [workspace]
# pcb-version = "0.3"
# ```

P1 = Net("P1")
P2 = Net("P2")

# Normal component - should appear in BOM with dnp omitted (false)
Component(
    name = "normal",
    footprint = "Resistor_SMD:R_0603_1005Metric",
    pin_defs = {"1": "1", "2": "2"},
    pins = {"1": P1, "2": P2},
    mpn = "RC0603FR-071KL",
    manufacturer = "Yageo",
)

# DNP component (via legacy property) - should appear in BOM with dnp=true
Component(
    name = "dnp_legacy",
    footprint = "Resistor_SMD:R_0603_1005Metric",
    pin_defs = {"1": "1", "2": "2"},
    pins = {"1": P1, "2": P2},
    mpn = "RC0603FR-0710KL",
    manufacturer = "Yageo",
    properties = {"do_not_populate": True},
)

# DNP component (via dnp kwarg) - should appear in BOM with dnp=true
Component(
    name = "dnp_kwarg",
    footprint = "Resistor_SMD:R_0603_1005Metric",
    pin_defs = {"1": "1", "2": "2"},
    pins = {"1": P1, "2": P2},
    mpn = "RC0603FR-074K7L",
    manufacturer = "Yageo",
    dnp = True,
)

# Component with both DNP and skip_bom - should NOT appear in BOM (skip_bom wins)
Component(
    name = "dnp_and_skip_bom",
    footprint = "Resistor_SMD:R_0603_1005Metric",
    pin_defs = {"1": "1", "2": "2"},
    pins = {"1": P1, "2": P2},
    mpn = "RC0603FR-07100KL",
    manufacturer = "Yageo",
    dnp = True,
    skip_bom = True,
)
"#;

const MODULE_DNP_BOARD_ZEN: &str = r#"
# ```pcb
# [workspace]
# pcb-version = "0.3"
# ```

load("@stdlib/interfaces.zen", "Power", "Ground")

Resistor = Module("@stdlib/generics/Resistor.zen")
Capacitor = Module("@stdlib/generics/Capacitor.zen")

vcc = Power("VCC")
gnd = Ground("GND")

# Normal module - components should NOT be DNP
Resistor(name = "R1", value = "1kOhm", package = "0603", P1 = vcc.NET, P2 = gnd.NET)

# Module with dnp=True - all child components should be DNP
Resistor(name = "R2_DNP", value = "10kOhm", package = "0603", P1 = vcc.NET, P2 = gnd.NET, dnp = True)
Capacitor(name = "C1_DNP", value = "100nF", package = "0402", P1 = vcc.NET, P2 = gnd.NET, dnp = True)

# Normal module again - should NOT be DNP
Capacitor(name = "C2", value = "10uF", package = "0805", P1 = vcc.NET, P2 = gnd.NET)
"#;

const WORKSPACE_TOML: &str = r#"
[workspace]
pcb-version = "0.3"
members = ["boards/*", "modules/*"]
"#;

#[test]
fn test_bom_json_format() {
    let output = Sandbox::new()
        .write("pcb.toml", WORKSPACE_TOML)
        .write("modules/LedModule.zen", LED_MODULE_ZEN)
        .write("boards/TestBoard.zen", TEST_BOARD_ZEN)
        .snapshot_run("pcb", ["bom", "boards/TestBoard.zen", "-f", "json"]);
    assert_snapshot!("bom_json", output);
}

#[test]
fn test_bom_table_format() {
    let output = Sandbox::new()
        .write("pcb.toml", WORKSPACE_TOML)
        .write("modules/LedModule.zen", LED_MODULE_ZEN)
        .write("boards/TestBoard.zen", TEST_BOARD_ZEN)
        .snapshot_run("pcb", ["bom", "boards/TestBoard.zen", "-f", "table"]);
    assert_snapshot!("bom_table", output);
}

#[test]
fn test_bom_default_format() {
    let output = Sandbox::new()
        .write("pcb.toml", WORKSPACE_TOML)
        .write("modules/LedModule.zen", LED_MODULE_ZEN)
        .write("boards/TestBoard.zen", TEST_BOARD_ZEN)
        .snapshot_run("pcb", ["bom", "boards/TestBoard.zen"]);
    assert_snapshot!("bom_default", output);
}

#[test]
fn test_bom_simple_resistors() {
    let output = Sandbox::new()
        .write("pcb.toml", WORKSPACE_TOML)
        .write("boards/SimpleResistors.zen", SIMPLE_RESISTOR_BOARD_ZEN)
        .snapshot_run("pcb", ["bom", "boards/SimpleResistors.zen", "-f", "json"]);
    assert_snapshot!("bom_simple_resistors_json", output);
}

#[test]
fn test_bom_simple_resistors_table() {
    let output = Sandbox::new()
        .write("pcb.toml", WORKSPACE_TOML)
        .write("boards/SimpleResistors.zen", SIMPLE_RESISTOR_BOARD_ZEN)
        .snapshot_run("pcb", ["bom", "boards/SimpleResistors.zen", "-f", "table"]);
    assert_snapshot!("bom_simple_resistors_table", output);
}

#[test]
fn test_bom_capacitors_with_dielectric() {
    let output = Sandbox::new()
        .write("pcb.toml", WORKSPACE_TOML)
        .write("boards/Capacitors.zen", CAPACITOR_BOARD_ZEN)
        .snapshot_run("pcb", ["bom", "boards/Capacitors.zen", "-f", "json"]);
    assert_snapshot!("bom_capacitors_json", output);
}

#[test]
fn test_bom_capacitors_table() {
    let output = Sandbox::new()
        .write("pcb.toml", WORKSPACE_TOML)
        .write("boards/Capacitors.zen", CAPACITOR_BOARD_ZEN)
        .snapshot_run("pcb", ["bom", "boards/Capacitors.zen", "-f", "table"]);
    assert_snapshot!("bom_capacitors_table", output);
}

#[test]
fn test_bom_kicad_fallback_json() {
    // Test BOM fallback to kicad-cli when design has no components
    // Copy the kicad project files into the sandbox
    let workspace_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let test_dir = workspace_root.join("crates/pcb-sch/test/kicad-bom");

    let kicad_sch = fs::read_to_string(test_dir.join("layout.kicad_sch")).unwrap();
    let kicad_pcb = fs::read_to_string(test_dir.join("layout.kicad_pcb")).unwrap();
    let kicad_pro = fs::read_to_string(test_dir.join("layout.kicad_pro")).unwrap();

    let zen_file = r#"
load("@stdlib/properties.zen", "Layout")
Layout(name="kicad-bom", path="layout", bom_profile=None)
"#;

    let output = Sandbox::new()
        .write("pcb.toml", WORKSPACE_TOML)
        .write("kicad-bom.zen", zen_file)
        .write("layout/layout.kicad_sch", kicad_sch)
        .write("layout/layout.kicad_pcb", kicad_pcb)
        .write("layout/layout.kicad_pro", kicad_pro)
        .snapshot_run("pcb", ["bom", "kicad-bom.zen", "-f", "json"]);
    assert_snapshot!("bom_kicad_fallback_json", output);
}

#[test]
fn test_bom_skip_bom_filtering() {
    // Test that components with skip_bom are excluded from BOM output
    let output = Sandbox::new()
        .write("pcb.toml", WORKSPACE_TOML)
        .write("boards/SkipBom.zen", SKIP_BOM_BOARD_ZEN)
        .snapshot_run("pcb", ["bom", "boards/SkipBom.zen", "-f", "json"]);
    assert_snapshot!("bom_skip_bom_json", output);
}

#[test]
fn test_bom_skip_bom_filtering_table() {
    // Test skip_bom filtering in table format
    let output = Sandbox::new()
        .write("pcb.toml", WORKSPACE_TOML)
        .write("boards/SkipBom.zen", SKIP_BOM_BOARD_ZEN)
        .snapshot_run("pcb", ["bom", "boards/SkipBom.zen", "-f", "table"]);
    assert_snapshot!("bom_skip_bom_table", output);
}

#[test]
fn test_bom_dnp_components() {
    // Test that DNP components appear in BOM (dnp is for assembly, not procurement)
    let output = Sandbox::new()
        .write("pcb.toml", WORKSPACE_TOML)
        .write("boards/DnpBoard.zen", DNP_BOARD_ZEN)
        .snapshot_run("pcb", ["bom", "boards/DnpBoard.zen", "-f", "json"]);
    assert_snapshot!("bom_dnp_json", output);
}

#[test]
fn test_bom_module_dnp_propagation() {
    // Test that module-level dnp=True propagates to all child components
    let output = Sandbox::new()
        .write("pcb.toml", WORKSPACE_TOML)
        .write("boards/ModuleDnp.zen", MODULE_DNP_BOARD_ZEN)
        .snapshot_run("pcb", ["bom", "boards/ModuleDnp.zen", "-f", "json"]);
    assert_snapshot!("bom_module_dnp_json", output);
}
