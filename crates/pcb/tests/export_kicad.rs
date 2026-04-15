#![cfg(not(target_os = "windows"))]

use pcb_test_utils::assert_snapshot;
use pcb_test_utils::sandbox::Sandbox;

const PCB_TOML_MIN: &str = r#"
[workspace]
pcb-version = "0.3"

[dependencies]
"gitlab.com/kicad/libraries/kicad-symbols" = "9.0.3"
"gitlab.com/kicad/libraries/kicad-footprints" = "9.0.3"
"#;

const SIMPLE_BOARD_ZEN: &str = r#"
Resistor = Module("@stdlib/generics/Resistor.zen")
Led = Module("@stdlib/generics/Led.zen")

vcc = Power("VCC")
gnd = Ground("GND")
led_anode = Net("LED_ANODE")

Resistor(name = "R1", value = "1kOhm", package = "0402", P1 = vcc.NET, P2 = led_anode)
Led(name = "D1", color = "red", package = "0402", A = led_anode, K = gnd.NET)
"#;

#[test]
fn test_export_kicad_rejects_missing_file() {
    let output = Sandbox::new().snapshot_run(
        "pcb",
        ["export-kicad", "boards/Nonexistent.zen", "-o", "out"],
    );
    assert_snapshot!("export_kicad_missing_file", output);
}

#[test]
#[ignore = "red: enabled once export-kicad is implemented (issue #682)"]
fn test_export_kicad_writes_project_directory() {
    let mut sandbox = Sandbox::new();
    let output = sandbox
        .write("pcb.toml", PCB_TOML_MIN)
        .write("boards/SimpleBoard.zen", SIMPLE_BOARD_ZEN)
        .snapshot_run(
            "pcb",
            ["export-kicad", "boards/SimpleBoard.zen", "-o", "exported"],
        );
    assert_snapshot!("export_kicad_simple_board", output);

    let exported = sandbox.default_cwd().join("exported");
    assert!(
        exported.join("SimpleBoard.kicad_pro").exists(),
        "expected SimpleBoard.kicad_pro under {}",
        exported.display()
    );
    assert!(
        exported.join("SimpleBoard.kicad_pcb").exists(),
        "expected SimpleBoard.kicad_pcb under {}",
        exported.display()
    );
    assert!(
        exported.join("SimpleBoard.kicad_sch").exists(),
        "expected SimpleBoard.kicad_sch under {}",
        exported.display()
    );
}
