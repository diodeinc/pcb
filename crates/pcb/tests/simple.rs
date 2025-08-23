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

add_property("layout_path", "build/TestBoard")

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

const SIMPLE_BOARD_ZEN: &str = r#"
load("@stdlib:v0.2.4/interfaces.zen", "Gpio", "Ground", "Power")

vcc_3v3 = Power("VCC_3V3")
gnd = Ground("GND")
test_signal = Gpio("TEST_SIGNAL")
internal_net = Net("INTERNAL")
"#;

const NONEXISTENT_REPO_BOARD_ZEN: &str = r#"
load("@github/nonexistent/repo:main/interfaces.zen", "Gpio", "Ground", "Power")

vcc_3v3 = Power("VCC_3V3")
gnd = Ground("GND")
vcc_3v3.NET = Net("VCC_3V3")
"#;

const SIMPLE_RESISTOR_ZEN: &str = r#"
value = config("value", str, default = "10kOhm")

P1 = io("P1", Net)
P2 = io("P2", Net)

Component(
    name = "R",
    prefix = "R",
    footprint = File("test.kicad_mod"),
    pin_defs = {"P1": "1", "P2": "2"},
    pins = {"P1": P1, "P2": P2},
    properties = {"value": value, "type": "resistor"},
)
"#;

const GIT_FIXTURE_BOARD_ZEN: &str = r#"
SimpleResistor = Module("@github/mycompany/components:v1.0.0/SimpleResistor.zen")

vcc = Net("VCC")
gnd = Net("GND")
SimpleResistor(name = "R1", value = "1kOhm", P1 = vcc, P2 = gnd)
SimpleResistor(name = "R2", value = "4.7kOhm", P1 = Net("SIGNAL"), P2 = gnd)
"#;

const TEST_KICAD_MOD: &str = r#"(footprint "test"
  (layer "F.Cu")
  (pad "1" smd rect (at -1 0) (size 1 1) (layers "F.Cu"))
  (pad "2" smd rect (at 1 0) (size 1 1) (layers "F.Cu"))
)
"#;

const SIMPLE_WORKSPACE_PCB_TOML: &str = r#"
[workspace]
name = "simple_workspace"
"#;

#[test]
#[cfg(not(target_os = "windows"))]
fn test_pcb_build_should_fail_without_fixture() {
    let output = Sandbox::new()
        .write("boards/TestBoard.zen", NONEXISTENT_REPO_BOARD_ZEN)
        .snapshot_run("pcb", ["build", "boards/TestBoard.zen"]);
    assert_snapshot!("no_fixture", output);
}

#[test]
#[cfg(not(target_os = "windows"))]
fn test_pcb_build_simple_board() {
    let output = Sandbox::new()
        .seed_stdlib(&["v0.2.4"])
        .seed_kicad(&["9.0.0"])
        .write("boards/SimpleBoard.zen", SIMPLE_BOARD_ZEN)
        .snapshot_run("pcb", ["build", "boards/SimpleBoard.zen"]);
    assert_snapshot!("simple_board", output);
}

#[test]
#[cfg(not(target_os = "windows"))]
fn test_pcb_build_simple_workspace() {
    let output = Sandbox::new()
        .seed_stdlib(&["v0.2.2"])
        .seed_kicad(&["9.0.0"])
        .write("modules/LedModule.zen", LED_MODULE_ZEN)
        .write("boards/TestBoard.zen", TEST_BOARD_ZEN)
        .snapshot_run("pcb", ["build", "boards/TestBoard.zen"]);
    assert_snapshot!("simple_workspace_build", output);
}

#[test]
#[cfg(not(target_os = "windows"))]
#[ignore = "slow test - run with 'cargo test -- --ignored' or 'cargo test -- --include-ignored'"]
fn test_pcb_release_simple_workspace() {
    let mut sb = Sandbox::new();
    sb.seed_stdlib(&["v0.2.2"])
        .seed_kicad(&["9.0.0"])
        .write("modules/LedModule.zen", LED_MODULE_ZEN)
        .write("boards/TestBoard.zen", TEST_BOARD_ZEN);

    // Test BOM first
    assert_snapshot!(
        "simple_workspace_bom",
        sb.snapshot_run("pcb", ["bom", "boards/TestBoard.zen", "-f", "json"])
    );

    // Test release
    assert_snapshot!(
        "simple_workspace_release",
        sb.snapshot_run("pcb", ["release", "boards/TestBoard.zen", "-f", "json"])
    );
}

#[test]
#[cfg(not(target_os = "windows"))]
fn test_pcb_vendor_simple_workspace() {
    let mut sb = Sandbox::new();
    sb.seed_stdlib(&["v0.2.2"])
        .seed_kicad(&["9.0.0"])
        .write("modules/LedModule.zen", LED_MODULE_ZEN)
        .write("boards/TestBoard.zen", TEST_BOARD_ZEN)
        .write("pcb.toml", SIMPLE_WORKSPACE_PCB_TOML)
        .hash_globs(&["*.kicad_mod", "**/diodeinc/stdlib/*.zen"]);
    assert_snapshot!(
        "simple_workspace_vendor",
        sb.snapshot_run("pcb", ["vendor", "boards/TestBoard.zen"])
    );
    assert_snapshot!("simple_workspace_vendor_dir", sb.snapshot_dir("vendor"));
}

#[test]
#[cfg(not(target_os = "windows"))]
fn test_pcb_build_with_git_fixture() {
    let mut sandbox = Sandbox::new();

    // Create a fake git repository with a simple component
    sandbox
        .git_fixture("https://github.com/mycompany/components.git")
        .write("SimpleResistor.zen", SIMPLE_RESISTOR_ZEN)
        .write("test.kicad_mod", TEST_KICAD_MOD)
        .commit("Add simple resistor component")
        .tag("v1.0.0", false)
        .push_mirror();

    // Create a board that uses the component from the fake git repository
    let output = sandbox
        .write("board.zen", GIT_FIXTURE_BOARD_ZEN)
        .snapshot_run("pcb", ["build", "board.zen"]);
    assert_snapshot!("git_fixture", output);
}

#[test]
#[cfg(not(target_os = "windows"))]
fn test_pcb_build_workspace_relative_paths_local_alias() {
    let output = Sandbox::new()
        .write("dep-ws/pcb.toml", SIMPLE_WORKSPACE_PCB_TOML)
        .write(
            "dep-ws/foo.zen",
            r#"
SimpleResistor = Module("//SimpleResistor.zen")
SimpleResistor(name = "R1", P1 = Net("VCC"), P2 = Net("GND"))"#,
        )
        .write("dep-ws/SimpleResistor.zen", SIMPLE_RESISTOR_ZEN)
        .write("dep-ws/test.kicad_mod", TEST_KICAD_MOD)
        .write(
            "build-ws/pcb.toml",
            r#"
[workspace]
name = "build-ws"
[packages]
dep-ws = "../dep-ws""#,
        )
        .write(
            "build-ws/foo.zen",
            r#"Module("@dep-ws/foo.zen")(name = "FOO")"#,
        )
        .snapshot_run("pcb", ["build", "build-ws/foo.zen"]);
    assert_snapshot!("workspace_relative_paths_local_alias", output);
}

#[test]
#[cfg(not(target_os = "windows"))]
fn test_pcb_help() {
    let output = Sandbox::new().snapshot_run("pcb", ["help"]);
    assert_snapshot!("help", output);
}
