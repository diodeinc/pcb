#![cfg(not(target_os = "windows"))]

use pcb_test_utils::assert_snapshot;
use pcb_test_utils::sandbox::Sandbox;

#[test]
fn test_export_kicad_rejects_missing_file() {
    let output = Sandbox::new().snapshot_run(
        "pcb",
        ["export-kicad", "boards/Nonexistent.zen", "-o", "out"],
    );
    assert_snapshot!("export_kicad_missing_file", output);
}
