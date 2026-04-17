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

const NONEXISTENT_REPO_BOARD_ZEN: &str = r#"
load("github.com/nonexistent/repo:main/interfaces.zen", "Gpio", "Ground", "Power")

vcc_3v3 = Power("VCC_3V3")
gnd = Ground("GND")
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
    properties = {"value": value, "type": "resistor"},
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

fn lock_dep_lines<'a>(pcb_sum: &'a str, module_path: &str) -> Vec<&'a str> {
    let prefix = format!("{module_path} ");
    pcb_sum
        .lines()
        .filter(|line| line.starts_with(&prefix))
        .collect()
}

const SIMPLE_WORKSPACE_PCB_TOML: &str = r#"
[workspace]
pcb-version = "0.3"
name = "simple_workspace"
members = ["boards"]

[dependencies]
"gitlab.com/kicad/libraries/kicad-symbols" = "9.0.3"
"gitlab.com/kicad/libraries/kicad-footprints" = "9.0.3"
"#;

const TEST_BOARD_PCB_TOML: &str = r#"
[board]
name = "TestBoard"
path = "TestBoard.zen"
description = "Main test board for validation"
"#;

const PCB_TOML_MIN: &str = r#"
[workspace]
pcb-version = "0.3"

[dependencies]
"gitlab.com/kicad/libraries/kicad-symbols" = "9.0.3"
"gitlab.com/kicad/libraries/kicad-footprints" = "9.0.3"
"#;

const WORKSPACE_NAMESPACE_PCB_TOML: &str = r#"
[workspace]
pcb-version = "0.3"
repository = "github.com/acme/workspace"
members = ["modules/*"]
"#;

const REMOTE_IO_MODULE_ZEN: &str = r#"
P1 = io(Net)
P2 = io(Net)
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
        .write("pcb.toml", PCB_TOML_MIN)
        .write("boards/SimpleBoard.zen", SIMPLE_BOARD_ZEN)
        .snapshot_run("pcb", ["build", "boards/SimpleBoard.zen"]);
    assert_snapshot!("simple_board", output);
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
        .snapshot_run("pcb", ["build", "boards/TestBoard.zen"]);
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
pcb-version = "0.3"

[dependencies]
"github.com/mycompany/components/SimpleResistor" = "1.0.0"
"#,
        )
        .write("board.zen", GIT_FIXTURE_BOARD_ZEN)
        .snapshot_run("pcb", ["build", "board.zen"]);
    assert_snapshot!("git_fixture", output);
}

#[test]
#[cfg(not(target_os = "windows"))]
fn test_offline_build_uses_selected_pseudo_version() {
    let mut sandbox = Sandbox::new();

    let mut fixture = sandbox.git_fixture("https://github.com/mycompany/components.git");
    fixture
        .write("SimpleResistor/pcb.toml", "[dependencies]\n")
        .write("SimpleResistor/SimpleResistor.zen", SIMPLE_RESISTOR_ZEN)
        .write("SimpleResistor/test.kicad_mod", TEST_KICAD_MOD)
        .commit("v1")
        .push_mirror();
    let rev1 = fixture.rev_parse_head();

    std::thread::sleep(std::time::Duration::from_secs(1));

    fixture
        .write(
            "SimpleResistor/SimpleResistor.zen",
            r#"
value = config(str, default = "10kOhm")

P1 = io(Net)
P2 = io(Net)

Component(
    name = "R",
    prefix = "R",
    footprint = File("test.kicad_mod"),
    pin_defs = {"P1": "1", "P2": "2"},
    pins = {"P1": P1, "P2": P2},
    properties = {"value": value, "type": "resistor", "revision": "v2"},
)
"#,
        )
        .commit("v2")
        .push_mirror();
    let rev2 = fixture.rev_parse_head();

    let online_output = sandbox
        .write(
            "pcb.toml",
            r#"[workspace]
pcb-version = "0.3"
members = ["boards/*"]
vendor = ["github.com/mycompany/components/**"]
"#,
        )
        .write(
            "boards/A/pcb.toml",
            format!(
                r#"[board]
name = "A"
path = "A.zen"

[dependencies]
"github.com/mycompany/components/SimpleResistor" = {{ branch = "main", rev = "{rev1}" }}
"#
            ),
        )
        .write("boards/A/A.zen", "x = 1\n")
        .write(
            "boards/B/pcb.toml",
            format!(
                r#"[board]
name = "B"
path = "B.zen"

[dependencies]
"github.com/mycompany/components/SimpleResistor" = {{ branch = "main", rev = "{rev2}" }}
"#
            ),
        )
        .write("boards/B/B.zen", GIT_FIXTURE_BOARD_ZEN)
        .snapshot_run("pcb", ["build", "boards/B/B.zen"]);

    let offline_output = sandbox.snapshot_run("pcb", ["build", "boards/B/B.zen", "--offline"]);

    let pcb_sum =
        std::fs::read_to_string(sandbox.default_cwd().join("pcb.sum")).unwrap_or_default();
    let dep_lines = lock_dep_lines(&pcb_sum, "github.com/mycompany/components/SimpleResistor");
    let dep_version = dep_lines[0]
        .split_whitespace()
        .nth(1)
        .expect("dependency content line must include version");

    let snapshot = sandbox
        .sanitize_output(&format!(
            "--- online ---\n{}\n--- offline ---\n{}\n--- pcb.sum dep lines ---\n{}\n",
            online_output,
            offline_output,
            dep_lines.join("\n")
        ))
        .replace(dep_version, "<SELECTED_PSEUDO_VERSION>")
        .replace(&rev1, "<REV1>")
        .replace(&rev2, "<REV2>");

    assert_snapshot!(
        "offline_build_reuses_selected_workspace_pseudo_version",
        snapshot
    );
}

#[test]
#[cfg(not(target_os = "windows"))]
fn test_pcb_help() {
    let output = Sandbox::new().snapshot_run("pcb", ["help"]);
    assert_snapshot!("help", output);
}

#[test]
#[cfg(not(target_os = "windows"))]
fn test_workspace_namespace_dependency_does_not_fallback_to_remote() {
    let mut sandbox = Sandbox::new();

    sandbox
        .git_fixture("https://github.com/acme/workspace.git")
        .write("modules/Missing/pcb.toml", "")
        .write("modules/Missing/Missing.zen", REMOTE_IO_MODULE_ZEN)
        .commit("Add remote-only missing package")
        .tag("modules/Missing/v1.0.0", false)
        .push_mirror();

    let result = sandbox
        .write("pcb.toml", WORKSPACE_NAMESPACE_PCB_TOML)
        .write(
            "modules/Board/pcb.toml",
            r#"
[board]
name = "Board"
path = "Board.zen"

[dependencies]
"github.com/acme/workspace/modules/Missing" = "1.0.0"
"#,
        )
        .write(
            "modules/Board/Board.zen",
            r#"
Missing = Module("github.com/acme/workspace/modules/Missing/Missing.zen")

Layout(name="Board", path="build/Board", bom_profile=None)

p1 = Net("P1")
p2 = Net("P2")

Missing(name="U1", P1=p1, P2=p2)
"#,
        )
        .run("pcb", ["build", "modules/Board/Board.zen"])
        .stderr_capture()
        .stdout_capture()
        .unchecked()
        .run()
        .expect("build command failed");

    assert!(
        !result.status.success(),
        "expected workspace-namespace build to fail"
    );

    let stderr = sandbox.sanitize_output(&String::from_utf8_lossy(&result.stderr));
    assert!(
        stderr.contains("is in this workspace, but no workspace member provides it."),
        "stderr should explain missing workspace member:\n{stderr}"
    );
    assert!(
        stderr.contains("Fix the dependency URL or remove it."),
        "stderr should explain how to fix the missing workspace member:\n{stderr}"
    );
    assert!(
        !stderr.contains("Failed to fetch github.com/acme/workspace/modules/Missing"),
        "stderr should not contain remote fetch failure:\n{stderr}"
    );
    assert!(
        !stderr.contains("git sparse checkout"),
        "stderr should not mention remote sparse checkout fallback:\n{stderr}"
    );
}

#[test]
#[cfg(not(target_os = "windows"))]
fn test_workspace_namespace_dependency_missing_manifest_gets_specific_hint() {
    let mut sandbox = Sandbox::new();

    let result = sandbox
        .write("pcb.toml", WORKSPACE_NAMESPACE_PCB_TOML)
        .write("modules/Missing/Missing.zen", REMOTE_IO_MODULE_ZEN)
        .write(
            "modules/Board/pcb.toml",
            r#"
[board]
name = "Board"
path = "Board.zen"

[dependencies]
"github.com/acme/workspace/modules/Missing" = "1.0.0"
"#,
        )
        .write(
            "modules/Board/Board.zen",
            r#"
Layout(name="Board", path="build/Board", bom_profile=None)
"#,
        )
        .run("pcb", ["build", "modules/Board/Board.zen"])
        .stderr_capture()
        .stdout_capture()
        .unchecked()
        .run()
        .expect("build command failed");

    assert!(
        !result.status.success(),
        "expected workspace-namespace build to fail"
    );

    let stderr = sandbox.sanitize_output(&String::from_utf8_lossy(&result.stderr));
    assert!(
        stderr.contains("Found directory 'modules/Missing' with no pcb.toml."),
        "stderr should explain the missing manifest case:\n{stderr}"
    );
    assert!(
        stderr.contains("Add pcb.toml there so the workspace can discover it."),
        "stderr should suggest adding pcb.toml:\n{stderr}"
    );
}

#[test]
#[cfg(not(target_os = "windows"))]
fn test_transitive_workspace_namespace_dependency_fails_before_remote_fallback() {
    let mut sandbox = Sandbox::new();

    sandbox
        .git_fixture("https://github.com/acme/workspace.git")
        .write("modules/Missing/pcb.toml", "")
        .write("modules/Missing/Missing.zen", REMOTE_IO_MODULE_ZEN)
        .commit("Add remote-only missing package")
        .tag("modules/Missing/v1.0.0", false)
        .push_mirror();

    sandbox
        .git_fixture("https://github.com/vendor/components.git")
        .write(
            "Thing/pcb.toml",
            r#"
[dependencies]
"github.com/acme/workspace/modules/Missing" = "1.0.0"
"#,
        )
        .write("Thing/Thing.zen", REMOTE_IO_MODULE_ZEN)
        .commit("Add external package with bad transitive workspace dep")
        .tag("Thing/v1.0.0", false)
        .push_mirror();

    let result = sandbox
        .write("pcb.toml", WORKSPACE_NAMESPACE_PCB_TOML)
        .write(
            "modules/Board/pcb.toml",
            r#"
[board]
name = "Board"
path = "Board.zen"

[dependencies]
"github.com/vendor/components/Thing" = "1.0.0"
"#,
        )
        .write(
            "modules/Board/Board.zen",
            r#"
Thing = Module("github.com/vendor/components/Thing/Thing.zen")

Layout(name="Board", path="build/Board", bom_profile=None)

p1 = Net("P1")
p2 = Net("P2")

Thing(name="U1", P1=p1, P2=p2)
"#,
        )
        .run("pcb", ["build", "modules/Board/Board.zen"])
        .stderr_capture()
        .stdout_capture()
        .unchecked()
        .run()
        .expect("build command failed");

    assert!(
        !result.status.success(),
        "expected transitive workspace-namespace build to fail"
    );

    let stderr = sandbox.sanitize_output(&String::from_utf8_lossy(&result.stderr));
    assert!(
        stderr.contains("Dependency 'github.com/acme/workspace/modules/Missing' in github.com/vendor/components/Thing@v1.0.0"),
        "stderr should identify the external package that introduced the bad dep:\n{stderr}"
    );
    assert!(
        stderr.contains("is in this workspace, but no workspace member provides it."),
        "stderr should explain missing workspace member:\n{stderr}"
    );
    assert!(
        !stderr.contains("Failed to fetch github.com/acme/workspace/modules/Missing"),
        "stderr should not contain remote fetch failure:\n{stderr}"
    );
}
