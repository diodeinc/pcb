#![cfg(not(target_os = "windows"))]

use std::fs::File;

use pcb_test_utils::assert_snapshot;
use pcb_test_utils::sandbox::Sandbox;
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

/// Helper to build args for source-only publish (excludes all manufacturing artifacts)
fn source_only_args(board_zen: &str) -> Vec<&str> {
    vec![
        "publish",
        board_zen,
        "--no-push",
        "--exclude",
        "drc",
        "--exclude",
        "bom",
        "--exclude",
        "gerbers",
        "--exclude",
        "cpl",
        "--exclude",
        "assembly",
        "--exclude",
        "odb",
        "--exclude",
        "ipc2581",
        "--exclude",
        "step",
        "--exclude",
        "vrml",
        "--exclude",
        "glb",
        "--exclude",
        "svg",
    ]
}

/// Find the staging directory for a board (uses git hash as version suffix)
fn find_staging_dir(sb: &Sandbox, board_name: &str) -> String {
    let releases_dir = sb.root_path().join("src/.pcb/releases");
    let staging_dir_name = std::fs::read_dir(&releases_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            name.starts_with(&format!("{}-", board_name)) && !name.ends_with(".zip")
        })
        .map(|e| e.file_name().to_string_lossy().to_string())
        .expect("Staging directory not found");
    format!(".pcb/releases/{}", staging_dir_name)
}

#[test]
fn test_publish_board_source_only() {
    let mut sb = Sandbox::new();
    sb.cwd("src")
        .write("pcb.toml", PCB_TOML)
        .write("boards/pcb.toml", BOARD_PCB_TOML)
        .write("boards/modules/LedModule.zen", LED_MODULE_ZEN)
        .write("boards/TestBoard.zen", TEST_BOARD_ZEN)
        .hash_globs(["*.kicad_mod", "**/diodeinc/stdlib/*.zen", "**/netlist.json"])
        .ignore_globs(["layout/*", "**/vendor/**", "**/build/**"])
        .init_git()
        .commit("Initial commit");

    // Build first to generate lockfile (required for release)
    sb.run("pcb", ["build", "boards/TestBoard.zen"])
        .run()
        .expect("build failed");

    // Run source-only publish (no layout needed)
    sb.run("pcb", source_only_args("boards/TestBoard.zen"))
        .run()
        .expect("Failed to run pcb publish command");

    let staging_dir = find_staging_dir(&sb, "TestBoard");
    assert_snapshot!("publish_source_only", sb.snapshot_dir(&staging_dir));
}

#[test]
fn test_publish_board_with_version() {
    let mut sb = Sandbox::new();
    sb.cwd("src")
        .ignore_globs(["layout/*", "**/vendor/**", "**/build/**"])
        .hash_globs(["*.kicad_mod", "**/diodeinc/stdlib/*.zen", "**/netlist.json"])
        .write(".gitignore", ".pcb")
        .write("pcb.toml", PCB_TOML)
        .write("boards/pcb.toml", TB0001_BOARD_PCB_TOML)
        .write("boards/modules/LedModule.zen", LED_MODULE_ZEN)
        .write("boards/TB0001.zen", TEST_BOARD_ZEN)
        .init_git()
        .commit("Initial commit");

    // Build first to generate lockfile (required for release)
    sb.run("pcb", ["build", "boards/TB0001.zen"])
        .run()
        .expect("build failed");

    // Commit lockfile and tag an existing version
    sb.commit("Add lockfile").tag("boards/v1.2.3");

    // Run source-only publish with bump (creates v1.3.0)
    let mut args = source_only_args("boards/TB0001.zen");
    args.push("--bump=minor");
    sb.run("pcb", &args)
        .run()
        .expect("Failed to run pcb publish command");

    // Staging directory uses version: .pcb/releases/{board_name}-v{version}
    let staging_dir = ".pcb/releases/TB0001-v1.3.0";

    // Check metadata for git version
    let metadata_file = File::open(
        sb.root_path()
            .join("src")
            .join(staging_dir)
            .join("metadata.json"),
    )
    .unwrap();
    let metadata_json: Value = serde_json::from_reader(metadata_file).unwrap();
    let git_version = metadata_json["release"]["git_version"].as_str().unwrap();
    assert_eq!(git_version, "v1.3.0");

    assert_snapshot!("publish_with_version", sb.snapshot_dir(staging_dir));
}

#[test]
fn test_publish_board_full() {
    let mut sb = Sandbox::new();
    sb.cwd("src")
        .write("pcb.toml", PCB_TOML)
        .write("boards/pcb.toml", BOARD_PCB_TOML)
        .write("boards/modules/LedModule.zen", LED_MODULE_ZEN)
        .write("boards/TestBoard.zen", TEST_BOARD_ZEN)
        .hash_globs(["*.kicad_mod", "**/diodeinc/stdlib/*.zen", "**/netlist.json"])
        .ignore_globs([
            "layout/*",
            "3d/*",
            "manufacturing/*.xml",
            "manufacturing/*.html",
            "**/vendor/**",
            "**/build/**",
            "**/drc.json",
        ])
        .init_git()
        .commit("Initial commit");

    // Generate layout files first (full releases require layout and lockfile)
    sb.run("pcb", ["layout", "--no-open", "boards/TestBoard.zen"])
        .run()
        .expect("layout generation failed");

    // Run full publish (with all artifacts, suppress test board DRC issues)
    sb.run(
        "pcb",
        [
            "publish",
            "boards/TestBoard.zen",
            "-S",
            "layout.drc.invalid_outline",
            "-S",
            "warnings",
            "--no-push",
        ],
    )
    .run()
    .expect("Failed to run pcb publish command");

    let staging_dir = find_staging_dir(&sb, "TestBoard");
    assert_snapshot!("publish_full", sb.snapshot_dir(&staging_dir));
}

#[test]
fn test_publish_board_with_file() {
    let mut sb = Sandbox::new();
    const DATASHEET_CONTENTS: &str = "Simple component datasheet.";
    sb.cwd("src")
        .write("pcb.toml", PCB_TOML)
        .write("boards/pcb.toml", TB0002_BOARD_PCB_TOML)
        .write("boards/modules/component.zen", SIMPLE_COMPONENT)
        .write("boards/modules/test.kicad_mod", TEST_KICAD_MOD)
        .write("boards/modules/datasheet.txt", DATASHEET_CONTENTS)
        .write("boards/TB0002.zen", SIMPLE_BOARD_ZEN)
        .ignore_globs(["layout/*", "**/vendor/**", "**/build/**"])
        .init_git()
        .commit("Initial commit");

    // Build first to generate lockfile (required for release)
    sb.run("pcb", ["build", "boards/TB0002.zen"])
        .run()
        .expect("build failed");

    // Run source-only publish
    sb.run("pcb", source_only_args("boards/TB0002.zen"))
        .run()
        .expect("Failed to run pcb publish command");

    let staging_dir = find_staging_dir(&sb, "TB0002");

    let datasheet_path = sb
        .root_path()
        .join("src")
        .join(&staging_dir)
        .join("src/boards/modules/datasheet.txt");
    let datasheet_contents = std::fs::read_to_string(&datasheet_path).unwrap();
    assert_eq!(datasheet_contents, DATASHEET_CONTENTS);

    assert_snapshot!("publish_with_file", sb.snapshot_dir(&staging_dir));
}

#[test]
fn test_publish_board_with_description() {
    let mut sb = Sandbox::new();
    sb.cwd("src")
        .write("pcb.toml", PCB_TOML)
        .write("boards/pcb.toml", BOARD_WITH_DESCRIPTION_PCB_TOML)
        .write("boards/modules/LedModule.zen", LED_MODULE_ZEN)
        .write("boards/DescBoard.zen", TEST_BOARD_ZEN)
        .hash_globs(["*.kicad_mod", "**/diodeinc/stdlib/*.zen"])
        .ignore_globs(["layout/*", "**/vendor/**", "**/build/**"])
        .init_git()
        .commit("Initial commit");

    // Build first to generate lockfile (required for release)
    sb.run("pcb", ["build", "boards/DescBoard.zen"])
        .run()
        .expect("build failed");

    // Run source-only publish
    sb.run("pcb", source_only_args("boards/DescBoard.zen"))
        .run()
        .expect("Failed to run pcb publish command");

    let staging_dir = find_staging_dir(&sb, "DescBoard");
    assert_snapshot!("publish_with_description", sb.snapshot_dir(&staging_dir));
}
