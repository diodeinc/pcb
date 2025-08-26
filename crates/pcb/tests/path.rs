#![cfg(not(target_os = "windows"))]

use pcb_test_utils::assert_snapshot;
use pcb_test_utils::sandbox::{cargo_bin, Sandbox};
use serde_json::Value;

const SIMPLE_WORKSPACE_PCB_TOML: &str = r#"
[workspace]
name = "simple_workspace"
"#;

const PATH_MODULE_ZEN: &str = r#"
config_path = Path("module_config.toml", allow_not_exist = True) # this file doesn't exist
data_path = Path("module_data.json")
add_property("layout_path", Path("build/Module", allow_not_exist = True))
Component(
    name="D1",
    footprint = Path("test.kicad_mod"),
    pin_defs={"1": "A", "2": "K"},
    pins={"1": Net("P1"), "2": Net("P2")}
)
"#;

const BOARD_ZEN: &str = r#"
PathTestModule = Module("@github/testcompany/pathtest:v1.0.0/PathModule.zen")
PathTestModule(name="D1")
"#;

const TEST_KICAD_MOD: &str = r#"(footprint "test"
  (layer "F.Cu")
  (pad "1" smd rect (at -1 0) (size 1 1) (layers "F.Cu"))
  (pad "2" smd rect (at 1 0) (size 1 1) (layers "F.Cu"))
)
"#;

#[test]
fn test_path_function_vendor() {
    let mut sb = Sandbox::new();

    // Create a fake Git repository with a module that uses Path() function
    sb.git_fixture("https://github.com/testcompany/pathtest.git")
        .write("PathModule.zen", PATH_MODULE_ZEN)
        .write("module_data.json", r#"{"test": true}"#)
        .write("test.kicad_mod", TEST_KICAD_MOD)
        .commit("Add path test module with existing files")
        .tag("v1.0.0", false)
        .push_mirror();

    // Create the main board that depends on the Git module
    sb.cwd("src")
        .write("boards/PathTest.zen", BOARD_ZEN)
        .write("pcb.toml", SIMPLE_WORKSPACE_PCB_TOML);

    // Test that vendor command works - should vendor the module and existing files
    assert_snapshot!(
        "path_function_vendor",
        sb.snapshot_run("pcb", ["vendor", "boards/PathTest.zen"])
    );

    // Verify vendor directory contains the module and both config files
    assert_snapshot!("path_function_vendor_dir", sb.snapshot_dir("vendor"));
}

#[test]
fn test_path_function_vendor_directory() {
    let mut sb = Sandbox::new();

    // Create a Git repository with a module that references a directory using Path()
    sb.git_fixture("https://github.com/testcompany/pathtest.git")
        .write(
            "PathModule.zen",
            r#"
config_dir = Path("config")  # Reference to directory
Component(
    name="D1",
    footprint = Path("test.kicad_mod"),
    pin_defs={"1": "A", "2": "K"},
    pins={"1": Net("P1"), "2": Net("P2")}
)
"#,
        )
        .write("config/app.toml", "name = \"test-app\"")
        .write("config/schema.json", r#"{"type": "object"}"#)
        .write("config/deep/readme.txt", "Configuration files")
        .write("test.kicad_mod", TEST_KICAD_MOD)
        .commit("Add module with directory reference")
        .tag("v1.0.0", false)
        .push_mirror();

    // Create board that uses the module
    sb.cwd("src")
        .write(
            "boards/DirectoryTest.zen",
            r#"
PathTestModule = Module("@github/testcompany/pathtest:v1.0.0/PathModule.zen")
PathTestModule(name="D1")
"#,
        )
        .write("pcb.toml", SIMPLE_WORKSPACE_PCB_TOML);

    // Vendor should include the entire directory and its contents
    sb.run("pcb", ["vendor", "boards/DirectoryTest.zen"])
        .run()
        .unwrap();
    let module_vendor_dir = sb
        .root_path()
        .join("src/vendor/github.com/testcompany/pathtest/v1.0.0");
    assert!(module_vendor_dir.join("config/app.toml").exists());
    assert!(module_vendor_dir.join("config/schema.json").exists());
    assert!(module_vendor_dir.join("config/deep/readme.txt").exists());
    assert_snapshot!("path_directory_vendor_dir", sb.snapshot_dir("vendor"));
}

#[test]
fn test_path_function_local_mixed() {
    let mut sb = Sandbox::new();

    // Create a board that directly uses Path() with mixed existing/non-existing files
    sb.write(
        "boards/LocalPathTest.zen",
        r#"
# Test various Path() scenarios locally
add_property("layout_path", Path("build/LocalPathTest", allow_not_exist=True))

existing_file = Path("existing.toml")
nonexistent_file = Path("missing.toml", allow_not_exist=True)
existing_dir = Path("../config")
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
    .write("config/settings.json", r#"{"debug": true}"#) // Make config dir exist
    .write("pcb.toml", SIMPLE_WORKSPACE_PCB_TOML);

    // Build should succeed with mixed existing/non-existing paths
    assert_snapshot!(
        "path_local_mixed_build",
        sb.snapshot_run("pcb", ["build", "boards/LocalPathTest.zen"])
    );

    // Test release functionality - run source-only release with JSON output
    let output = sb
        .hash_globs(["*.kicad_mod"])
        .ignore_globs(["layout/*"])
        .cmd(
            cargo_bin!("pcb"),
            [
                "release",
                "boards/LocalPathTest.zen",
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
    assert_snapshot!("path_local_mixed_release", sb.snapshot_dir(staging_dir));
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
