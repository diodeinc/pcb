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

    // Note: process_layout emits kicad_pro + kicad_pcb + kicad_prl, but not kicad_sch.
    // KiCad creates the schematic file itself the first time eeschema opens the project.
    // See crates/pcb-layout/tests/layout_generation.rs for the same assertion set.
    let exported = sandbox.default_cwd().join("exported");
    assert!(
        exported.join("layout.kicad_pro").exists(),
        "expected layout.kicad_pro under {}",
        exported.display()
    );
    assert!(
        exported.join("layout.kicad_pcb").exists(),
        "expected layout.kicad_pcb under {}",
        exported.display()
    );
}

// Verifies `--offline` and `--locked` are wired through to dependency resolution.
// The Sandbox blocks network egress and pre-seeds kicad-symbols/footprints, so a
// hermetic export run with both flags should still succeed.
#[test]
fn test_export_kicad_offline_locked_writes_project_directory() {
    let mut sandbox = Sandbox::new();
    let output = sandbox
        .write("pcb.toml", PCB_TOML_MIN)
        .write("boards/SimpleBoard.zen", SIMPLE_BOARD_ZEN)
        .snapshot_run(
            "pcb",
            [
                "export-kicad",
                "boards/SimpleBoard.zen",
                "-o",
                "exported",
                "--offline",
                "--locked",
            ],
        );
    assert_snapshot!("export_kicad_offline_locked", output);

    let exported = sandbox.default_cwd().join("exported");
    assert!(exported.join("layout.kicad_pro").exists());
    assert!(exported.join("layout.kicad_pcb").exists());
}
