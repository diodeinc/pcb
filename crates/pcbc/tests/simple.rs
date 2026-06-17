use pcb_test_utils::assert_snapshot;
use pcb_test_utils::sandbox::Sandbox;

const LED_MODULE_ZEN: &str = r#"
load("@stdlib/interfaces.zen", "Gpio")

Resistor = Module("@stdlib/generics/Resistor.zen")
Led = Module("@stdlib/generics/Led.zen")

led_color = config(str, default = "red")
r_value = config(str, default = "330Ohm")
package = config(str, default = "0603")

VCC = io(Power)
GND = io(Ground)
CTRL = io(Gpio)

led_anode = Net("LED_ANODE")

Resistor(name = "R1", value = r_value, package = package, P1 = VCC, P2 = led_anode)
Led(name = "D1", color = led_color, package = package, A = led_anode, K = CTRL)
"#;

const TEST_BOARD_ZEN: &str = r#"
load("@stdlib/interfaces.zen", "Gpio")

Layout(name="TestBoard", path="build/TestBoard", bom_profile=None)

LedModule = Module("modules/LedModule.zen")
Resistor = Module("@stdlib/generics/Resistor.zen")
Capacitor = Module("@stdlib/generics/Capacitor.zen")
Crystal = Module("@stdlib/generics/Crystal.zen")

vcc_3v3 = Power("VCC_3V3")
gnd = Ground("GND")
led_ctrl = Gpio("LED_CTRL")
osc_xi = Gpio("OSC_XI")
osc_xo = Gpio("OSC_XO")

Capacitor(name = "C1", value = "100nF", package = "0402", P1 = vcc_3v3, P2 = gnd)
Capacitor(name = "C2", value = "10uF", package = "0805", P1 = vcc_3v3, P2 = gnd)

LedModule(name = "LED1", led_color = "green", VCC = vcc_3v3, GND = gnd, CTRL = led_ctrl)
LedModule(name = "LED2", led_color = "red", VCC = vcc_3v3, GND = gnd, CTRL = Gpio(gnd))

Crystal(name = "X1", frequency = "16MHz", load_capacitance = "18pF", package = "5032_2Pin", XIN = osc_xi, XOUT = osc_xo)

Capacitor(name = "C3", value = "22pF", package = "0402", P1 = osc_xi, P2 = gnd)
Capacitor(name = "C4", value = "22pF", package = "0402", P1 = osc_xo, P2 = gnd)

Resistor(name = "R1", value = "10kOhm", package = "0603", P1 = vcc_3v3, P2 = led_ctrl)
"#;

const SIMPLE_BOARD_ZEN: &str = r#"
load("@stdlib/interfaces.zen", "Gpio")

vcc_3v3 = Power("VCC_3V3")
gnd = Ground("GND")
test_signal = Gpio("TEST_SIGNAL")
internal_net = Net("INTERNAL")
"#;

const SIMPLE_RESISTOR_ZEN: &str = r#"
value = config(str, default = "10kOhm")

P1 = io(Net)
P2 = io(Net)

Component(
    name = "R",
    prefix = "R",
    footprint = File("test.kicad_mod"),
    pin_defs = {"P1": "1", "P2": "2"},
    pins = {"P1": P1, "P2": P2},
    type = "resistor",
    properties = {"value": value},
)
"#;

const GIT_FIXTURE_BOARD_ZEN: &str = r#"
SimpleResistor = Module("github.com/mycompany/components/SimpleResistor/SimpleResistor.zen")

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
pcb-version = "0.4"
name = "simple_workspace"
"#;

const TEST_BOARD_PCB_TOML: &str = r#"
[board]
name = "TestBoard"
path = "TestBoard.zen"
description = "Main test board for validation"
"#;

const PCB_TOML_MIN: &str = r#"
[workspace]
pcb-version = "0.4"
"#;

#[test]
#[cfg(not(target_os = "windows"))]
fn test_pcb_build_simple_board() {
    let output = Sandbox::new()
        .write("pcb.toml", PCB_TOML_MIN)
        .write("boards/SimpleBoard.zen", SIMPLE_BOARD_ZEN)
        .sync()
        .snapshot_run("pcbc", ["build", "boards/SimpleBoard.zen"]);
    assert_snapshot!("simple_board", output);
}

#[test]
#[cfg(not(target_os = "windows"))]
fn test_pcb_build_multiple_explicit_files() {
    let output = Sandbox::new()
        .write("pcb.toml", PCB_TOML_MIN)
        .write("boards/A.zen", SIMPLE_BOARD_ZEN)
        .write("boards/B.zen", SIMPLE_BOARD_ZEN)
        .sync()
        .snapshot_run("pcbc", ["build", "boards/A.zen", "boards/B.zen"]);
    assert_snapshot!("multiple_explicit_files", output);
}

#[test]
#[cfg(not(target_os = "windows"))]
fn test_pcb_build_simple_workspace() {
    let output = Sandbox::new()
        .write("pcb.toml", SIMPLE_WORKSPACE_PCB_TOML)
        .write("boards/pcb.toml", TEST_BOARD_PCB_TOML)
        .write("boards/modules/LedModule.zen", LED_MODULE_ZEN)
        .write("boards/TestBoard.zen", TEST_BOARD_ZEN)
        .hash_globs(["*.kicad_mod", "**/diodeinc/stdlib/*.zen"])
        .sync()
        .snapshot_run("pcbc", ["build", "boards/TestBoard.zen"]);
    assert_snapshot!("simple_workspace_build", output);
}

#[test]
#[cfg(not(target_os = "windows"))]
fn test_pcb_build_with_git_fixture() {
    let mut sandbox = Sandbox::new();

    // Create a fake git repository with a simple component
    sandbox
        .git_fixture("https://github.com/mycompany/components.git")
        .write(
            "SimpleResistor/pcb.toml",
            r#"
[dependencies]
"#,
        )
        .write("SimpleResistor/SimpleResistor.zen", SIMPLE_RESISTOR_ZEN)
        .write("SimpleResistor/test.kicad_mod", TEST_KICAD_MOD)
        .commit("Add simple resistor component")
        .tag("SimpleResistor/v1.0.0", false)
        .push_mirror();

    // Create a board that uses the component from the fake git repository
    let output = sandbox
        .write(
            "pcb.toml",
            r#"
[workspace]
pcb-version = "0.4"

[dependencies]
"github.com/mycompany/components/SimpleResistor" = "1.0.0"
"#,
        )
        .write("board.zen", GIT_FIXTURE_BOARD_ZEN)
        .sync()
        .snapshot_run("pcbc", ["build", "board.zen"]);
    assert_snapshot!("git_fixture", output);
}

#[test]
#[cfg(not(target_os = "windows"))]
fn test_pcb_build_does_not_prune_existing_vendor_entries() {
    let mut sandbox = Sandbox::new();

    sandbox
        .git_fixture("https://github.com/mycompany/components.git")
        .write("SimpleResistor/pcb.toml", "[dependencies]\n")
        .write("SimpleResistor/SimpleResistor.zen", SIMPLE_RESISTOR_ZEN)
        .write("SimpleResistor/test.kicad_mod", TEST_KICAD_MOD)
        .commit("Add simple resistor component")
        .tag("SimpleResistor/v1.0.0", false)
        .push_mirror();

    sandbox
        .write(
            "pcb.toml",
            r#"
[workspace]
pcb-version = "0.4"
vendor = ["github.com/mycompany/components/**"]

[dependencies]
"github.com/mycompany/components/SimpleResistor" = "1.0.0"
"#,
        )
        .write("board.zen", GIT_FIXTURE_BOARD_ZEN)
        .write(
            "vendor/github.com/other/package/9.0.3/marker.txt",
            "keep me",
        );

    let manifest_path = sandbox.default_cwd().join("pcb.toml");
    let manifest_before = std::fs::read_to_string(&manifest_path).unwrap();
    let vendor_marker = sandbox
        .default_cwd()
        .join("vendor/github.com/other/package/9.0.3/marker.txt");

    let output = sandbox
        .run("pcbc", ["build", "board.zen"])
        .stderr_capture()
        .stdout_capture()
        .unchecked()
        .run()
        .unwrap();

    assert!(
        output.status.success(),
        "build failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&manifest_path).unwrap(),
        manifest_before,
        "build must not rewrite pcb.toml"
    );
    assert!(
        vendor_marker.exists(),
        "build must not prune unrelated existing vendor entries"
    );
}

#[test]
#[cfg(not(target_os = "windows"))]
fn test_offline_build_reuses_vendored_pseudo_version() {
    let mut sandbox = Sandbox::new();

    // Component repo where the board pins the dependency to an exact commit.
    let mut fixture = sandbox.git_fixture("https://github.com/mycompany/components.git");
    fixture
        .write("SimpleResistor/pcb.toml", "[dependencies]\n")
        .write("SimpleResistor/SimpleResistor.zen", SIMPLE_RESISTOR_ZEN)
        .write("SimpleResistor/test.kicad_mod", TEST_KICAD_MOD)
        .commit("add SimpleResistor")
        .push_mirror();
    let rev = fixture.rev_parse_head();

    sandbox
        .write(
            "pcb.toml",
            r#"[workspace]
pcb-version = "0.4"
vendor = ["github.com/mycompany/components/**"]
"#,
        )
        .write(
            "boards/B/pcb.toml",
            format!(
                r#"[board]
name = "B"
path = "B.zen"

[dependencies]
"github.com/mycompany/components/SimpleResistor" = {{ branch = "main", rev = "{rev}" }}
"#
            ),
        )
        .write("boards/B/B.zen", GIT_FIXTURE_BOARD_ZEN);

    // `pcb sync` resolves the branch+rev to a pseudo-version, pins it in the
    // hydrated manifest, and vendors that exact version.
    sandbox.sync();

    let board_manifest =
        std::fs::read_to_string(sandbox.default_cwd().join("boards/B/pcb.toml")).unwrap();
    let pseudo_version = board_manifest
        .split_once("SimpleResistor\" = \"")
        .and_then(|(_, rest)| rest.split('"').next())
        .expect("hydrated manifest should pin SimpleResistor to a pseudo-version")
        .to_string();
    assert!(
        pseudo_version.ends_with(&rev),
        "pseudo-version {pseudo_version} should embed the resolved rev {rev}"
    );
    assert!(
        sandbox
            .default_cwd()
            .join(format!(
                "vendor/github.com/mycompany/components/SimpleResistor/{pseudo_version}"
            ))
            .exists(),
        "sync should vendor the selected pseudo-version"
    );

    // The offline build must reuse the vendored pseudo-version without network access.
    let online_output = sandbox.snapshot_run("pcbc", ["build", "boards/B/B.zen"]);
    let offline_output = sandbox.snapshot_run("pcbc", ["build", "boards/B/B.zen", "--offline"]);

    let snapshot = sandbox
        .sanitize_output(&format!(
            "--- online ---\n{online_output}\n--- offline ---\n{offline_output}\n--- hydrated dep ---\n{pseudo_version}\n"
        ))
        .replace(&pseudo_version, "<SELECTED_PSEUDO_VERSION>")
        .replace(&rev, "<REV>");

    assert_snapshot!("offline_build_reuses_vendored_pseudo_version", snapshot);
}

#[test]
#[cfg(not(target_os = "windows"))]
fn test_pcb_help() {
    let output = Sandbox::new().snapshot_run("pcbc", ["help"]);
    assert_snapshot!("help", output);
}
