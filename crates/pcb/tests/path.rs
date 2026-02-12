#![cfg(not(target_os = "windows"))]

use pcb_test_utils::assert_snapshot;
use pcb_test_utils::sandbox::Sandbox;

const SIMPLE_WORKSPACE_PCB_TOML: &str = r#"
[workspace]
pcb-version = "0.3"
members = ["boards/*"]
"#;

const LOCAL_PATH_TEST_BOARD_PCB_TOML: &str = r#"
[board]
name = "LocalPathTest" 
path = "LocalPathTest.zen"
"#;

#[test]
fn test_path_function_local_mixed() {
    let mut sb = Sandbox::new();

    // Create a board that directly uses Path() with mixed existing/non-existing files
    sb.write(
        "boards/LocalPathTest.zen",
        r#"
load("@stdlib/properties.zen", "Layout")
# Test various Path() scenarios locally
Layout(name="LocalPathTest", path="build/LocalPathTest", bom_profile=None)

existing_file = Path("existing.toml")
nonexistent_file = Path("missing.toml", allow_not_exist=True)
existing_dir = Path("config")
nonexistent_dir = Path("missing_dir", allow_not_exist=True)

print("Existing file:", existing_file)
print("Nonexistent file:", nonexistent_file)
print("Existing directory:", existing_dir)
print("Nonexistent directory:", nonexistent_dir)

# Simple board to test Path() functionality - just define some nets
vcc = Net("VCC") 
gnd = Net("GND")
"#,
    )
    .write("boards/existing.toml", "# This file exists")
    .write("boards/config/settings.json", r#"{"debug": true}"#) // Make config dir exist
    .write("pcb.toml", SIMPLE_WORKSPACE_PCB_TOML)
    .write("boards/pcb.toml", LOCAL_PATH_TEST_BOARD_PCB_TOML);

    // Build should succeed with mixed existing/non-existing paths
    assert_snapshot!(
        "path_local_mixed_build",
        sb.snapshot_run("pcb", ["build", "boards/LocalPathTest.zen"])
    );
}

#[test]
fn test_path_function_missing_without_allow() {
    let mut sb = Sandbox::new();

    // Create a board that references non-existent file WITHOUT allow_not_exist=true
    sb.write(
        "boards/FailingTest.zen",
        r#"
# This should fail - references non-existent file without allow_not_exist=true
missing_config = Path("this_file_does_not_exist.toml")

Component(
    name="R1",
    footprint="",
    pin_defs={"1": "P1", "2": "P2"},
    pins={"1": Net("VCC"), "2": Net("GND")}
)
"#,
    )
    .write("pcb.toml", SIMPLE_WORKSPACE_PCB_TOML);

    // Build should fail because file doesn't exist and allow_not_exist=false (default)
    assert_snapshot!(
        "path_missing_no_allow_build",
        sb.snapshot_run("pcb", ["build", "boards/FailingTest.zen"])
    );
}
