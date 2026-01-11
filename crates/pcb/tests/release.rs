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

LedModule = Module("modules/LedModule.zen")
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
pcb-version = "0.3"
name = "test_workspace"
"#;

const BOARD_PCB_TOML: &str = r#"
[board]
name = "TestBoard"
path = "TestBoard.zen"
"#;

const TB0001_BOARD_PCB_TOML: &str = r#"
[board]
name = "TB0001"
path = "TB0001.zen"
"#;

const CASE_BOARD_PCB_TOML: &str = r#"
[board]
name = "CaseBoard"
path = "CaseBoard.zen"
"#;

const CASE_WORKSPACE_PCB_TOML: &str = r#"
[workspace]
pcb-version = "0.3"
name = "case_workspace"
"#;

const TB0002_BOARD_PCB_TOML: &str = r#"
[board]
name = "TB0002"
path = "TB0002.zen"
"#;

const BOARD_WITH_DESCRIPTION_PCB_TOML: &str = r#"
[board]
name = "DescBoard"
path = "DescBoard.zen"
description = "A test board with a description"
"#;

const SIMPLE_COMPONENT: &str = r#"
value = config("value", str, default = "10kOhm")

P1 = io("P1", Net)
P2 = io("P2", Net)

Component(
    name = "R",
    prefix = "R",
    footprint = File("test.kicad_mod"),
    pin_defs = {"P1": "1", "P2": "2"},
    pins = {"P1": P1, "P2": P2},
    properties = {
        "value": value,
        "type": "resistor",
        "datasheet": File("datasheet.txt"),
    }
)
"#;

const TEST_KICAD_MOD: &str = r#"(footprint "test"
  (layer "F.Cu")
  (pad "1" smd rect (at -1 0) (size 1 1) (layers "F.Cu"))
  (pad "2" smd rect (at 1 0) (size 1 1) (layers "F.Cu"))
)
"#;

const SIMPLE_BOARD_ZEN: &str = r#"
SimpleComponent = Module("modules/component.zen")
add_property("layout_path", "build/TestBoard")
vcc_3v3 = Net("VCC_3V3")
gnd = Net("GND")
SimpleComponent(name = "foo", P1 = vcc_3v3, P2 = gnd)
"#;

#[test]
fn test_pcb_release_source_only() {
    let mut sb = Sandbox::new().allow_network();
    sb.cwd("src")
        .write("pcb.toml", PCB_TOML)
        .write("boards/pcb.toml", BOARD_PCB_TOML)
        .write("boards/modules/LedModule.zen", LED_MODULE_ZEN)
        .write("boards/TestBoard.zen", TEST_BOARD_ZEN)
        .hash_globs(["*.kicad_mod", "**/diodeinc/stdlib/*.zen"])
        .ignore_globs(["layout/*", "**/vendor/**", "**/build/**"]);

    // Generate layout files first (releases require layout)
    sb.run("pcb", ["layout", "--no-open", "boards/TestBoard.zen"])
        .run()
        .expect("layout generation failed");

    // Run source-only release with JSON output
    let output = sb
        .cmd(
            cargo_bin!("pcb"),
            [
                "release",
                "--board",
                "TestBoard",
                "--source-only",
                "-f",
                "json",
            ],
        )
        .read()
        .expect("Failed to run pcb release command");

    // Parse JSON output to get staging directory
    let json: Value = serde_json::from_str(&output).expect("Failed to parse JSON output");
    let staging_dir = json["release"]["staging_directory"]
        .as_str()
        .expect("Missing staging_directory in JSON");

    // Snapshot the staging directory contents
    assert_snapshot!("release_basic", sb.snapshot_dir(staging_dir));

    // Snapshot the build from release contents
    let build_ouput = sb.snapshot_run(
        "pcb",
        [
            "build",
            "--offline",
            &format!("{staging_dir}/src/boards/TestBoard.zen"),
        ],
    );
    assert_snapshot!("release_basic_build", build_ouput);
}

#[test]
fn test_pcb_release_with_git() {
    let mut sb = Sandbox::new().allow_network();
    sb.cwd("src")
        .ignore_globs(["layout/*", "**/vendor/**", "**/build/**"])
        .hash_globs(["*.kicad_mod", "**/diodeinc/stdlib/*.zen"])
        .write(".gitignore", ".pcb")
        .write("pcb.toml", PCB_TOML)
        .write("boards/pcb.toml", TB0001_BOARD_PCB_TOML)
        .write("boards/modules/LedModule.zen", LED_MODULE_ZEN)
        .write("boards/TB0001.zen", TEST_BOARD_ZEN)
        .init_git()
        .commit("Initial commit");

    // Generate layout files first (releases require layout)
    sb.run("pcb", ["layout", "--no-open", "boards/TB0001.zen"])
        .run()
        .expect("layout generation failed");

    // Commit layout files and tag AFTER layout generation so the release picks up the tag
    sb.commit("Add layout files").tag("boards/v1.2.3"); // Package path-based tag (boards/ contains pcb.toml)

    let output = sb
        .cmd(
            cargo_bin!("pcb"),
            [
                "release",
                "--board",
                "TB0001",
                "--source-only",
                "-f",
                "json",
            ],
        )
        .read()
        .expect("Failed to run pcb release command");

    // Parse JSON output to get staging directory and verify git version
    let json: Value = serde_json::from_str(&output).expect("Failed to parse JSON output");
    let staging_dir = json["release"]["staging_directory"].as_str().unwrap();

    let metadata_file = File::open(format!("{staging_dir}/metadata.json")).unwrap();
    let metadata_json: Value = serde_json::from_reader(metadata_file).unwrap();
    let git_version = metadata_json["release"]["git_version"].as_str().unwrap();

    // Verify git version is detected properly (with v prefix for backward compat)
    assert_eq!(git_version, "v1.2.3");

    // Snapshot the staging directory contents
    assert_snapshot!("release_with_git", sb.snapshot_dir(staging_dir));

    // Snapshot the build from release contents
    let build_ouput = sb.snapshot_run(
        "pcb",
        [
            "build",
            "--offline",
            &format!("{staging_dir}/src/boards/TB0001.zen"),
        ],
    );
    assert_snapshot!("release_with_git_build", build_ouput);
}

#[test]
fn test_pcb_release_full() {
    let mut sb = Sandbox::new().allow_network();
    sb.cwd("src")
        .write("pcb.toml", PCB_TOML)
        .write("boards/pcb.toml", BOARD_PCB_TOML)
        .write("boards/modules/LedModule.zen", LED_MODULE_ZEN)
        .write("boards/TestBoard.zen", TEST_BOARD_ZEN)
        .hash_globs(["*.kicad_mod", "**/diodeinc/stdlib/*.zen"])
        .ignore_globs([
            "layout/*",
            "3d/*",
            "manufacturing/*.xml",
            "manufacturing/*.html",
            "**/vendor/**",
            "**/build/**",
        ]);

    // Generate layout files first (releases require layout and lockfile)
    sb.run("pcb", ["layout", "--no-open", "boards/TestBoard.zen"])
        .run()
        .expect("layout generation failed");

    // Run full release with JSON output (suppress test board DRC issues)
    let output = sb
        .cmd(
            cargo_bin!("pcb"),
            [
                "release",
                "--board",
                "TestBoard",
                "-f",
                "json",
                "-S",
                "layout.drc.invalid_outline",
                "-S",
                "warnings",
            ],
        )
        .read()
        .expect("Failed to run pcb release command");

    // Parse JSON output to get staging directory
    let json: Value = serde_json::from_str(&output).expect("Failed to parse JSON output");
    let staging_dir = json["release"]["staging_directory"]
        .as_str()
        .expect("Missing staging_directory in JSON");

    // Snapshot the staging directory contents
    assert_snapshot!("release_full", sb.snapshot_dir(staging_dir));

    // Snapshot the build from release contents
    let build_ouput = sb.snapshot_run(
        "pcb",
        [
            "build",
            "--offline",
            &format!("{staging_dir}/src/boards/TestBoard.zen"),
        ],
    );
    assert_snapshot!("release_full_build", build_ouput);
}

#[test]
fn test_pcb_release_case_insensitive_tag() {
    let board_zen = r#"
add_property("layout_path", "build/CaseBoard")

n1 = Net("N1")
n2 = Net("N2")
"#;

    // Board name is CaseBoard; now uses package path-based tags
    let mut sb = Sandbox::new().allow_network();
    sb.cwd("src")
        .ignore_globs(["layout/*", "**/vendor/**", "**/build/**"])
        .write(".gitignore", ".pcb")
        .write("pcb.toml", CASE_WORKSPACE_PCB_TOML)
        .write("boards/pcb.toml", CASE_BOARD_PCB_TOML)
        .write("boards/CaseBoard.zen", board_zen)
        .init_git()
        .commit("Initial commit");

    // Generate layout files first (releases require layout)
    sb.run("pcb", ["layout", "--no-open", "boards/CaseBoard.zen"])
        .run()
        .expect("layout generation failed");

    // Commit layout files and tag AFTER layout generation so the release picks up the tag
    sb.commit("Add layout files").tag("boards/v9.9.9"); // Package path-based tag

    let output = sb
        .cmd(
            cargo_bin!("pcb"),
            [
                "release",
                "--board",
                "CaseBoard",
                "--source-only",
                "-f",
                "json",
            ],
        )
        .read()
        .expect("Failed to run pcb release command");

    // Parse JSON output to get staging directory
    let json: Value = serde_json::from_str(&output).expect("Failed to parse JSON output");
    let staging_dir = json["release"]["staging_directory"].as_str().unwrap();

    // Snapshot the build from release contents
    let build_ouput = sb.snapshot_run(
        "pcb",
        [
            "build",
            "--offline",
            &format!("{staging_dir}/src/boards/CaseBoard.zen"),
        ],
    );
    assert_snapshot!("case_insensitive_tag_build", build_ouput);

    // Load and sanitize metadata.json for stable snapshot
    let metadata_path = format!("{staging_dir}/metadata.json");
    let metadata_file = File::open(&metadata_path).unwrap();
    let meta: Value = serde_json::from_reader(metadata_file).unwrap();

    // Ensure git tag was detected (with v prefix for backward compat)
    let git_version = meta["release"]["git_version"].as_str().unwrap();
    assert_eq!(git_version, "v9.9.9");
    assert_snapshot!("case_insensitive_tag", sb.snapshot_dir(staging_dir));
}

#[test]
fn test_pcb_release_with_file() {
    let mut sb = Sandbox::new().allow_network();
    const DATASHEET_CONTENTS: &str = "Simple component datasheet.";
    sb.cwd("src")
        .write("pcb.toml", PCB_TOML)
        .write("boards/pcb.toml", TB0002_BOARD_PCB_TOML)
        .write("boards/modules/component.zen", SIMPLE_COMPONENT)
        .write("boards/modules/test.kicad_mod", TEST_KICAD_MOD)
        .write("boards/modules/datasheet.txt", DATASHEET_CONTENTS)
        .write("boards/TB0002.zen", SIMPLE_BOARD_ZEN)
        .ignore_globs(["layout/*", "**/vendor/**", "**/build/**"]);

    // Generate layout files first (releases require layout)
    sb.run("pcb", ["layout", "--no-open", "boards/TB0002.zen"])
        .run()
        .expect("layout generation failed");

    // Run source-only release with JSON output
    let output = sb
        .cmd(
            cargo_bin!("pcb"),
            [
                "release",
                "--board",
                "TB0002",
                "--source-only",
                "-f",
                "json",
            ],
        )
        .read()
        .expect("Failed to run pcb release command");

    // Parse JSON output to get staging directory
    let json: Value = serde_json::from_str(&output).expect("Failed to parse JSON output");
    let staging_dir = json["release"]["staging_directory"]
        .as_str()
        .expect("Missing staging_directory in JSON");

    let datasheet_path = format!("{staging_dir}/src/boards/modules/datasheet.txt");
    let datasheet_contents = std::fs::read_to_string(&datasheet_path).unwrap();
    assert_eq!(datasheet_contents, DATASHEET_CONTENTS);

    // Snapshot the staging directory contents
    assert_snapshot!("release_with_file", sb.snapshot_dir(staging_dir));

    // Snapshot the build from release contents
    let build_ouput = sb.snapshot_run(
        "pcb",
        [
            "build",
            "--offline",
            &format!("{staging_dir}/src/boards/TB0002.zen"),
        ],
    );
    assert_snapshot!("release_with_file_build", build_ouput);
}

#[test]
fn test_pcb_release_with_description() {
    let mut sb = Sandbox::new().allow_network();
    sb.cwd("src")
        .write("pcb.toml", PCB_TOML)
        .write("boards/pcb.toml", BOARD_WITH_DESCRIPTION_PCB_TOML)
        .write("boards/modules/LedModule.zen", LED_MODULE_ZEN)
        .write("boards/DescBoard.zen", TEST_BOARD_ZEN)
        .hash_globs(["*.kicad_mod", "**/diodeinc/stdlib/*.zen"])
        .ignore_globs(["layout/*", "**/vendor/**", "**/build/**"]);

    // Generate layout files first (releases require layout)
    sb.run("pcb", ["layout", "--no-open", "boards/DescBoard.zen"])
        .run()
        .expect("layout generation failed");

    // Run source-only release with JSON output
    let output = sb
        .cmd(
            cargo_bin!("pcb"),
            [
                "release",
                "--board",
                "DescBoard",
                "--source-only",
                "-f",
                "json",
            ],
        )
        .read()
        .expect("Failed to run pcb release command");

    // Parse JSON output to get staging directory
    let json: Value = serde_json::from_str(&output).expect("Failed to parse JSON output");
    let staging_dir = json["release"]["staging_directory"]
        .as_str()
        .expect("Missing staging_directory in JSON");

    // Snapshot the staging directory contents including metadata.json with description
    assert_snapshot!("release_with_description", sb.snapshot_dir(staging_dir));
}
