use pcb_test_utils::assert_snapshot;
use pcb_test_utils::sandbox::Sandbox;

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
#[cfg(not(target_os = "windows"))]
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
