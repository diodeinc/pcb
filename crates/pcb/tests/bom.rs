use pcb_test_utils::assert_snapshot;
use pcb_test_utils::sandbox::Sandbox;

const LED_MODULE_ZEN: &str = r#"
load("@stdlib:v0.2.2/interfaces.zen", "Gpio", "Ground", "Power")

Resistor = Module("@stdlib:v0.2.2/generics/Resistor.zen")
Led = Module("@stdlib:v0.2.2/generics/Led.zen")

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
load("@stdlib:v0.2.2/interfaces.zen", "Gpio", "Ground", "Power")

LedModule = Module("../modules/LedModule.zen")
Resistor = Module("@stdlib:v0.2.2/generics/Resistor.zen")
Capacitor = Module("@stdlib:v0.2.2/generics/Capacitor.zen")
Crystal = Module("@stdlib:v0.2.2/generics/Crystal.zen")

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
load("@stdlib:v0.2.2/interfaces.zen", "Power", "Ground")

Resistor = Module("@stdlib:v0.2.2/generics/Resistor.zen")

vcc = Power("VCC")
gnd = Ground("GND")

Resistor(name = "R1", value = "1kOhm", package = "0603", P1 = vcc.NET, P2 = gnd.NET)
Resistor(name = "R2", value = "1kOhm", package = "0603", P1 = vcc.NET, P2 = gnd.NET)
Resistor(name = "R3", value = "4.7kOhm", package = "0402", P1 = vcc.NET, P2 = gnd.NET)
"#;

const CAPACITOR_BOARD_ZEN: &str = r#"
load("@stdlib:v0.2.2/interfaces.zen", "Power", "Ground")

Capacitor = Module("@stdlib:v0.2.2/generics/Capacitor.zen")

vcc = Power("VCC")
gnd = Ground("GND")

Capacitor(name = "C1", value = "100nF", package = "0402", voltage = "16V", dielectric = "X7R", P1 = vcc.NET, P2 = gnd.NET)
Capacitor(name = "C2", value = "10uF", package = "0805", voltage = "25V", dielectric = "X5R", P1 = vcc.NET, P2 = gnd.NET)
Capacitor(name = "C3", value = "1uF", package = "0603", P1 = vcc.NET, P2 = gnd.NET)
"#;

#[cfg(not(target_os = "windows"))]
#[test]
fn test_bom_json_format() {
    let output = Sandbox::new()
        .seed_stdlib(&["v0.2.2"])
        .seed_kicad(&["9.0.0"])
        .write("modules/LedModule.zen", LED_MODULE_ZEN)
        .write("boards/TestBoard.zen", TEST_BOARD_ZEN)
        .snapshot_run("pcb", ["bom", "boards/TestBoard.zen", "-f", "json"]);
    assert_snapshot!("bom_json", output);
}

#[test]
#[cfg(not(target_os = "windows"))]
fn test_bom_table_format() {
    let output = Sandbox::new()
        .seed_stdlib(&["v0.2.2"])
        .seed_kicad(&["9.0.0"])
        .write("modules/LedModule.zen", LED_MODULE_ZEN)
        .write("boards/TestBoard.zen", TEST_BOARD_ZEN)
        .snapshot_run("pcb", ["bom", "boards/TestBoard.zen", "-f", "table"]);
    assert_snapshot!("bom_table", output);
}

#[test]
#[cfg(not(target_os = "windows"))]
fn test_bom_default_format() {
    let output = Sandbox::new()
        .seed_stdlib(&["v0.2.2"])
        .seed_kicad(&["9.0.0"])
        .write("modules/LedModule.zen", LED_MODULE_ZEN)
        .write("boards/TestBoard.zen", TEST_BOARD_ZEN)
        .snapshot_run("pcb", ["bom", "boards/TestBoard.zen"]);
    assert_snapshot!("bom_default", output);
}

#[test]
#[cfg(not(target_os = "windows"))]
fn test_bom_simple_resistors() {
    let output = Sandbox::new()
        .seed_stdlib(&["v0.2.2"])
        .seed_kicad(&["9.0.0"])
        .write("boards/SimpleResistors.zen", SIMPLE_RESISTOR_BOARD_ZEN)
        .snapshot_run("pcb", ["bom", "boards/SimpleResistors.zen", "-f", "json"]);
    assert_snapshot!("bom_simple_resistors_json", output);
}

#[test]
#[cfg(not(target_os = "windows"))]
fn test_bom_simple_resistors_table() {
    let output = Sandbox::new()
        .seed_stdlib(&["v0.2.2"])
        .seed_kicad(&["9.0.0"])
        .write("boards/SimpleResistors.zen", SIMPLE_RESISTOR_BOARD_ZEN)
        .snapshot_run("pcb", ["bom", "boards/SimpleResistors.zen", "-f", "table"]);
    assert_snapshot!("bom_simple_resistors_table", output);
}

#[test]
#[cfg(not(target_os = "windows"))]
fn test_bom_capacitors_with_dielectric() {
    let output = Sandbox::new()
        .seed_stdlib(&["v0.2.2"])
        .seed_kicad(&["9.0.0"])
        .write("boards/Capacitors.zen", CAPACITOR_BOARD_ZEN)
        .snapshot_run("pcb", ["bom", "boards/Capacitors.zen", "-f", "json"]);
    assert_snapshot!("bom_capacitors_json", output);
}

#[test]
#[cfg(not(target_os = "windows"))]
fn test_bom_capacitors_table() {
    let output = Sandbox::new()
        .seed_stdlib(&["v0.2.2"])
        .seed_kicad(&["9.0.0"])
        .write("boards/Capacitors.zen", CAPACITOR_BOARD_ZEN)
        .snapshot_run("pcb", ["bom", "boards/Capacitors.zen", "-f", "table"]);
    assert_snapshot!("bom_capacitors_table", output);
}
