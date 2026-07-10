#![cfg(not(target_os = "windows"))]

use std::fs::File;

use pcb_test_utils::assert_snapshot;
use pcb_test_utils::sandbox::Sandbox;
use serde_json::Value;

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

vcc_3v3 = Power("VCC_3V3")
gnd = Ground("GND")
led_ctrl = Gpio("LED_CTRL")

Capacitor(name = "C1", value = "100nF", package = "0402", P1 = vcc_3v3, P2 = gnd)
Capacitor(name = "C2", value = "10uF", package = "0805", P1 = vcc_3v3, P2 = gnd)

LedModule(name = "LED1", led_color = "green", VCC = vcc_3v3, GND = gnd, CTRL = led_ctrl)

Resistor(name = "R1", value = "10kOhm", package = "0603", P1 = vcc_3v3, P2 = led_ctrl)
"#;

const PCB_TOML: &str = r#"
[workspace]
pcb-version = "0.4"
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
    datasheet = File("datasheet.txt"),
    properties = {
        "value": value,
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
Layout(name="TestBoard", path="build/TestBoard", bom_profile=None)
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

fn setup_board_publish_infer_workspace(sb: &mut Sandbox) {
    sb.cwd("src")
        .write(".gitignore", ".pcb\n")
        .write("pcb.toml", PCB_TOML)
        .write("boards/pcb.toml", BOARD_PCB_TOML)
        .write("boards/modules/LedModule.zen", LED_MODULE_ZEN)
        .write("boards/TestBoard.zen", TEST_BOARD_ZEN)
        .hash_globs(["*.kicad_mod", "**/diodeinc/stdlib/*.zen", "**/netlist.json"])
        .ignore_globs(["layout/*", "**/vendor/**", "**/build/**"])
        .init_git()
        .commit("chore: initial workspace")
        .sync();

    sb.run("pcbc", ["build", "boards/TestBoard.zen"])
        .run()
        .expect("build failed");
    sb.commit("chore: hydrate manifests");
}

fn publish_board_infer(sb: &mut Sandbox) {
    let mut args = source_only_args("boards/TestBoard.zen");
    args.push("--bump=infer");
    sb.run("pcbc", &args).run().expect("publish failed");
}

fn assert_publish_git_version(sb: &Sandbox, version: &str) {
    let staging_dir = format!(".pcb/releases/TestBoard-{version}");
    let metadata_file = File::open(
        sb.root_path()
            .join("src")
            .join(&staging_dir)
            .join("metadata.json"),
    )
    .unwrap();
    let metadata_json: Value = serde_json::from_reader(metadata_file).unwrap();
    assert_eq!(
        metadata_json["release"]["git_version"].as_str(),
        Some(version)
    );
}

#[test]
fn test_publish_board_infer_first_release() {
    let mut sb = Sandbox::new();
    setup_board_publish_infer_workspace(&mut sb);

    publish_board_infer(&mut sb);

    assert_publish_git_version(&sb, "v0.1.0");
}

#[test]
fn test_publish_board_infer_pre_1_0_feat_is_patch() {
    let mut sb = Sandbox::new();
    setup_board_publish_infer_workspace(&mut sb);
    sb.tag("boards/v0.1.0")
        .write(
            "boards/TestBoard.zen",
            format!("{TEST_BOARD_ZEN}\n# add connector\n"),
        )
        .commit("feat: add connector");

    publish_board_infer(&mut sb);

    assert_publish_git_version(&sb, "v0.1.1");
}

#[test]
fn test_publish_board_infer_pre_1_0_breaking_is_minor() {
    let mut sb = Sandbox::new();
    setup_board_publish_infer_workspace(&mut sb);
    sb.tag("boards/v0.1.0")
        .write(
            "boards/TestBoard.zen",
            format!("{TEST_BOARD_ZEN}\n# reroute board\n"),
        )
        .commit("feat!: reroute board");

    publish_board_infer(&mut sb);

    assert_publish_git_version(&sb, "v0.2.0");
}

#[test]
fn test_publish_board_infer_post_1_0_feat_is_minor() {
    let mut sb = Sandbox::new();
    setup_board_publish_infer_workspace(&mut sb);
    sb.tag("boards/v1.0.0")
        .write(
            "boards/TestBoard.zen",
            format!("{TEST_BOARD_ZEN}\n# add connector\n"),
        )
        .commit("feat: add connector");

    publish_board_infer(&mut sb);

    assert_publish_git_version(&sb, "v1.1.0");
}

#[test]
fn test_publish_board_infer_post_1_0_breaking_is_major() {
    let mut sb = Sandbox::new();
    setup_board_publish_infer_workspace(&mut sb);
    sb.tag("boards/v1.0.0")
        .write(
            "boards/TestBoard.zen",
            format!("{TEST_BOARD_ZEN}\n# reroute board\n"),
        )
        .commit("feat!: reroute board");

    publish_board_infer(&mut sb);

    assert_publish_git_version(&sb, "v2.0.0");
}

#[test]
fn test_publish_board_infer_scopes_commit_history_to_board_path() {
    let mut sb = Sandbox::new();
    setup_board_publish_infer_workspace(&mut sb);
    sb.tag("boards/v1.0.0")
        .write("README.md", "outside board path\n")
        .commit("feat!: outside breaking change");

    publish_board_infer(&mut sb);

    assert_publish_git_version(&sb, "v1.0.1");
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
        .commit("Initial commit")
        .sync();

    // Build after hydrating dependency manifests (required for release)
    sb.run("pcbc", ["build", "boards/TestBoard.zen"])
        .run()
        .expect("build failed");

    // Run source-only publish (no layout needed)
    sb.run("pcbc", source_only_args("boards/TestBoard.zen"))
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
        .commit("Initial commit")
        .sync();

    // Build after hydrating dependency manifests (required for release)
    sb.run("pcbc", ["build", "boards/TB0001.zen"])
        .run()
        .expect("build failed");

    // Commit the hydrated manifests and tag an existing version
    sb.commit("Hydrate manifests").tag("boards/v1.2.3");

    // Run source-only publish with bump (creates v1.3.0)
    let mut args = source_only_args("boards/TB0001.zen");
    args.push("--bump=minor");
    sb.run("pcbc", &args)
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
        .commit("Initial commit")
        .sync();

    // Generate layout files first (full releases require layout)
    sb.run("pcbc", ["layout", "--no-open", "boards/TestBoard.zen"])
        .run()
        .expect("layout generation failed");

    // Run full publish (with all artifacts, suppress test board DRC issues)
    sb.run(
        "pcbc",
        [
            "publish",
            "boards/TestBoard.zen",
            "-S",
            "layout",
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
fn test_publish_metadata_includes_bom_strict() {
    let mut sb = Sandbox::new();
    sb.cwd("src")
        .write(
            "pcb.toml",
            r#"
[workspace]
pcb-version = "0.4"

[workspace.bom]
strict = true
"#,
        )
        .write("boards/pcb.toml", BOARD_PCB_TOML)
        .write("boards/modules/component.zen", SIMPLE_COMPONENT)
        .write("boards/modules/test.kicad_mod", TEST_KICAD_MOD)
        .write(
            "boards/modules/datasheet.txt",
            "Simple component datasheet.",
        )
        .write("boards/TestBoard.zen", SIMPLE_BOARD_ZEN)
        .ignore_globs(["layout/*", "**/vendor/**", "**/build/**"])
        .init_git()
        .commit("Initial commit")
        .sync();

    sb.run("pcbc", source_only_args("boards/TestBoard.zen"))
        .run()
        .expect("Failed to run pcb publish command");

    let staging_dir = find_staging_dir(&sb, "TestBoard");
    let metadata_file = File::open(
        sb.root_path()
            .join("src")
            .join(&staging_dir)
            .join("metadata.json"),
    )
    .unwrap();
    let metadata_json: Value = serde_json::from_reader(metadata_file).unwrap();
    assert_eq!(metadata_json["release"]["bom"]["strict"], true);
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
        .commit("Initial commit")
        .sync();

    // Build after hydrating dependency manifests (required for release)
    sb.run("pcbc", ["build", "boards/TB0002.zen"])
        .run()
        .expect("build failed");

    // Run source-only publish
    sb.run("pcbc", source_only_args("boards/TB0002.zen"))
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
        .commit("Initial commit")
        .sync();

    // Build after hydrating dependency manifests (required for release)
    sb.run("pcbc", ["build", "boards/DescBoard.zen"])
        .run()
        .expect("build failed");

    // Run source-only publish
    sb.run("pcbc", source_only_args("boards/DescBoard.zen"))
        .run()
        .expect("Failed to run pcb publish command");

    let staging_dir = find_staging_dir(&sb, "DescBoard");
    assert_snapshot!("publish_with_description", sb.snapshot_dir(&staging_dir));
}

#[test]
fn test_publish_board_vendors_remote_deps_for_validation() {
    let mut sb = Sandbox::new();

    // A remote component that the published board depends on.
    sb.git_fixture("https://github.com/mycompany/components.git")
        .write("Resistor/pcb.toml", "[dependencies]\n")
        .write("Resistor/Resistor.zen", SIMPLE_COMPONENT)
        .write("Resistor/test.kicad_mod", TEST_KICAD_MOD)
        .write("Resistor/datasheet.txt", "Simple component datasheet.")
        .commit("Add remote component")
        .tag("Resistor/v1.0.0", false)
        .push_mirror();

    sb.cwd("src")
        .write("pcb.toml", "[workspace]\npcb-version = \"0.4\"\n")
        .write(
            "boards/pcb.toml",
            r#"[board]
name = "TestBoard"

[dependencies]
"github.com/mycompany/components/Resistor" = "1.0.0"
"#,
        )
        .write(
            "boards/TestBoard.zen",
            r#"
Resistor = Module("github.com/mycompany/components/Resistor/Resistor.zen")
Layout(name="TestBoard", path="build/TestBoard", bom_profile=None)
Resistor(name = "foo", P1 = Net("VCC_3V3"), P2 = Net("GND"))
"#,
        )
        .init_git()
        .commit("Initial commit")
        .sync();

    sb.run("pcbc", source_only_args("boards/TestBoard.zen"))
        .run()
        .expect("publish should succeed");

    // The hydrated remote dependency is vendored into the source bundle so it
    // validates offline without network or a populated package cache.
    let staging_dir = find_staging_dir(&sb, "TestBoard");
    let vendored_remote = sb
        .root_path()
        .join("src")
        .join(&staging_dir)
        .join("src/vendor/github.com/mycompany/components/Resistor/1.0.0/pcb.toml");

    assert!(
        vendored_remote.exists(),
        "publish should stage the board's remote dependency for offline validation"
    );
}

/// Test that `pcb publish` works when run from the board directory with a relative .zen path.
/// Regression test: previously, `pcb publish DM0002.zen` from `boards/DM0002/` would fail
/// because workspace discovery broke on the empty parent path.
#[test]
fn test_publish_board_from_board_dir() {
    let mut sb = Sandbox::new();
    sb.cwd("src")
        .write("pcb.toml", PCB_TOML)
        .write("boards/pcb.toml", BOARD_PCB_TOML)
        .write("boards/modules/LedModule.zen", LED_MODULE_ZEN)
        .write("boards/TestBoard.zen", TEST_BOARD_ZEN)
        .hash_globs(["*.kicad_mod", "**/diodeinc/stdlib/*.zen", "**/netlist.json"])
        .ignore_globs(["layout/*", "**/vendor/**", "**/build/**"])
        .init_git()
        .commit("Initial commit")
        .sync();

    // Build after hydrating dependency manifests
    sb.run("pcbc", ["build", "boards/TestBoard.zen"])
        .run()
        .expect("build failed");

    // Run publish from the board directory with a relative path
    sb.cwd("src/boards")
        .run("pcbc", source_only_args("TestBoard.zen"))
        .run()
        .expect("publish from board dir with relative path should work");
}
