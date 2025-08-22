#![cfg(not(target_os = "windows"))]

use std::fs::File;

use pcb_test_utils::assert_snapshot;
use pcb_test_utils::sandbox::{cargo_bin, Sandbox};
use serde_json::Value;

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

add_property("layout_path", "build/TestBoard")

LedModule = Module("../modules/LedModule.zen")
Resistor = Module("@stdlib/generics/Resistor.zen")
Capacitor = Module("@stdlib/generics/Capacitor.zen")

vcc_3v3 = Power("VCC_3V3")
gnd = Ground("GND")
led_ctrl = Gpio("LED_CTRL")

Capacitor(name = "C1", value = "100nF", package = "0402", P1 = vcc_3v3.NET, P2 = gnd.NET)
Capacitor(name = "C2", value = "10uF", package = "0805", P1 = vcc_3v3.NET, P2 = gnd.NET)

LedModule(name = "LED1", led_color = "green", VCC = vcc_3v3, GND = gnd, CTRL = led_ctrl)

Resistor(name = "R1", value = "10kOhm", package = "0603", P1 = vcc_3v3.NET, P2 = led_ctrl.NET)
"#;

const PCB_TOML: &str = r#"
[workspace]
name = "test_workspace"

[packages]
stdlib = "@github/diodeinc/stdlib:v0.2.4"
"#;

#[test]
fn test_pcb_release_source_only() {
    let mut sb = Sandbox::new();
    sb.cwd("src")
        .seed_stdlib(&["v0.2.4"])
        .seed_kicad(&["9.0.0"])
        .write("pcb.toml", PCB_TOML)
        .write("modules/LedModule.zen", LED_MODULE_ZEN)
        .write("boards/TestBoard.zen", TEST_BOARD_ZEN)
        .hash_globs(&["*.kicad_mod", "**/diodeinc/stdlib/*.zen"])
        .ignore_globs(&["layout/*"]);

    // Run source-only release with JSON output
    let output = sb
        .cmd(
            cargo_bin!("pcb"),
            [
                "release",
                "boards/TestBoard.zen",
                "--source-only",
                "-f",
                "json",
            ],
        )
        .read()
        .expect("Failed to run pcb release command");

    // Parse JSON output to get staging directory
    let json: Value = serde_json::from_str(&output).expect("Failed to parse JSON output");
    let staging_dir = json["staging_directory"]
        .as_str()
        .expect("Missing staging_directory in JSON");

    // Snapshot the staging directory contents
    assert_snapshot!("release_basic", sb.snapshot_dir(staging_dir));
}

#[test]
fn test_pcb_release_with_git() {
    let mut sb = Sandbox::new();
    let output = sb
        .cwd("src")
        .ignore_globs(&["layout/*"])
        .hash_globs(&["*.kicad_mod", "**/diodeinc/stdlib/*.zen"])
        .seed_stdlib(&["v0.2.4"])
        .seed_kicad(&["9.0.0"])
        .write(".gitignore", ".pcb")
        .write("pcb.toml", PCB_TOML)
        .write("modules/LedModule.zen", LED_MODULE_ZEN)
        .write("boards/TB0001.zen", TEST_BOARD_ZEN)
        .init_git()
        .commit("Initial commit")
        .tag("TB0001/v1.2.3")
        .cmd(
            cargo_bin!("pcb"),
            [
                "release",
                "boards/TB0001.zen",
                "--source-only",
                "-f",
                "json",
            ],
        )
        .read()
        .expect("Failed to run pcb release command");

    // Parse JSON output to get staging directory and verify git version
    let json: Value = serde_json::from_str(&output).expect("Failed to parse JSON output");
    let staging_dir = json["staging_directory"].as_str().unwrap();

    let metadata_file = File::open(format!("{staging_dir}/metadata.json")).unwrap();
    let metadata_json: Value = serde_json::from_reader(metadata_file).unwrap();
    let git_version = metadata_json["release"]["git_version"].as_str().unwrap();

    // Verify git version is detected properly
    assert_eq!(git_version, "v1.2.3");

    // Snapshot the staging directory contents
    assert_snapshot!("release_with_git", sb.snapshot_dir(staging_dir));
}

#[test]
fn test_pcb_release_case_insensitive_tag() {
    let board_zen = r#"
add_property("layout_path", "build/CaseBoard")

n1 = Net("N1")
n2 = Net("N2")
"#;

    // Board name is CaseBoard; tag uses upper-case prefix to test case-insensitivity
    let mut sb = Sandbox::new();
    let output = sb
        .cwd("src")
        .ignore_globs(&["layout/*"])
        .write("boards/CaseBoard.zen", board_zen)
        .init_git()
        .commit("Initial commit")
        .tag("CASEBOARD/v9.9.9")
        .cmd(
            cargo_bin!("pcb"),
            [
                "release",
                "boards/CaseBoard.zen",
                "--source-only",
                "-f",
                "json",
            ],
        )
        .read()
        .expect("Failed to run pcb release command");

    // Parse JSON output to get staging directory
    let json: Value = serde_json::from_str(&output).expect("Failed to parse JSON output");
    let staging_dir = json["staging_directory"].as_str().unwrap();

    // Load and sanitize metadata.json for stable snapshot
    let metadata_path = format!("{staging_dir}/metadata.json");
    let metadata_file = File::open(&metadata_path).unwrap();
    let meta: Value = serde_json::from_reader(metadata_file).unwrap();

    // Ensure git tag was detected
    let git_version = meta["release"]["git_version"].as_str().unwrap();
    assert_eq!(git_version, "v9.9.9");
    assert_snapshot!("case_insensitive_tag", sb.snapshot_dir(staging_dir));
}
