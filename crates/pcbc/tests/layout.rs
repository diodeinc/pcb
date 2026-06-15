#![cfg(not(target_os = "windows"))]

use pcb_test_utils::sandbox::Sandbox;
use serde_json::Value;

const PCB_TOML: &str = r#"
[workspace]
pcb-version = "0.4"
"#;

const BOARD_ZEN: &str = r#"
SimpleComponent = Module("modules/component.zen")

Layout(name="TestBoard", path="build/TestBoard", bom_profile=None)

vcc_3v3 = Net("VCC_3V3")
gnd = Net("GND")

SimpleComponent(name = "foo", P1 = vcc_3v3, P2 = gnd)
"#;

const COMPONENT_ZEN: &str = r#"
P1 = io(Net)
P2 = io(Net)

Component(
    name = "R",
    prefix = "R",
    footprint = File("test.kicad_mod"),
    pin_defs = {"P1": "1", "P2": "2"},
    pins = {"P1": P1, "P2": P2},
    type = "resistor",
    properties = {"value": "10kOhm"},
)
"#;

const TEST_KICAD_MOD: &str = r#"(footprint "test"
  (layer "F.Cu")
  (pad "1" smd rect (at -1 0) (size 1 1) (layers "F.Cu"))
  (pad "2" smd rect (at 1 0) (size 1 1) (layers "F.Cu"))
)
"#;

const NO_LAYOUT_ZEN: &str = r#"
p1 = Net("P1")
"#;

#[test]
fn layout_json_output_is_parseable() {
    let mut sandbox = Sandbox::new();
    sandbox
        .write("pcb.toml", PCB_TOML)
        .write("board.zen", BOARD_ZEN)
        .write("modules/component.zen", COMPONENT_ZEN)
        .write("modules/test.kicad_mod", TEST_KICAD_MOD)
        .write("no-layout.zen", NO_LAYOUT_ZEN);

    let output = sandbox
        .run("pcbc", ["layout", "--no-open", "-f", "json", "board.zen"])
        .stderr_capture()
        .stdout_capture()
        .run()
        .expect("layout command failed");
    let json: Value = serde_json::from_slice(&output.stdout).expect("stdout should be valid JSON");
    assert_eq!(json["sourceFile"], "board.zen");
    assert!(
        json["pcbFile"]
            .as_str()
            .is_some_and(|path| path.ends_with("layout.kicad_pcb"))
    );

    let no_sync_output = sandbox
        .run(
            "pcbc",
            [
                "layout",
                "--no-sync",
                "--no-open",
                "-f",
                "json",
                "no-layout.zen",
            ],
        )
        .stderr_capture()
        .stdout_capture()
        .run()
        .expect("layout --no-sync command failed");
    let json: Value =
        serde_json::from_slice(&no_sync_output.stdout).expect("stdout should be valid JSON");
    assert_eq!(json["sourceFile"], "no-layout.zen");
    assert!(json["layoutDir"].is_null());
    assert!(json["pcbFile"].is_null());
}
