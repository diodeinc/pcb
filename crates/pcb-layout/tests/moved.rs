use anyhow::Result;
use assert_fs::TempDir;
use assert_fs::prelude::*;
use pcb_layout::process_layout;
use pcb_zen_core::{DefaultFileProvider, Diagnostics};
use serial_test::serial;

mod helpers;
use helpers::*;

/// Test that moved() renames are applied correctly and preserve position.
///
/// This test verifies the Rust-side moved() preprocessing:
/// 1. Creates initial layout with module "OldModule"
/// 2. Renames to "NewModule" with moved("OldModule", "NewModule")
/// 3. Verifies the path is renamed in the PCB file
/// 4. Verifies position is preserved
#[cfg(not(target_os = "windows"))]
#[test]
#[serial]
fn test_moved_renames_path_and_preserves_position() -> Result<()> {
    let temp = TempDir::new()?.into_persistent();
    let resource_path = get_resource_path("moved");
    temp.copy_from(&resource_path, &["**/*", "!.pcb/cache/**/*"])?;

    // --- Step 1: Initial layout with OldModule ---
    let zen_file = temp.path().join("Board.zen");
    assert!(zen_file.exists(), "Board.zen should exist");

    let mut workspace_info = pcb_zen::get_workspace_info(&DefaultFileProvider::new(), temp.path())?;
    let res = pcb_zen::resolve_dependencies(&mut workspace_info, false, false)?;

    let (output, diagnostics) = pcb_zen::run(&zen_file, res.clone()).unpack();
    if !diagnostics.is_empty() {
        eprintln!("Zen evaluation diagnostics (step 1):");
        for diag in diagnostics {
            eprintln!("  {:?}", diag);
        }
    }
    let schematic = output.expect("Zen evaluation should produce a schematic");

    let mut layout_diagnostics = Diagnostics::default();
    let result = process_layout(&schematic, false, false, &mut layout_diagnostics)?.unwrap();
    assert!(
        result.pcb_file.exists(),
        "PCB file should exist after initial sync"
    );

    // Read initial snapshot and get position
    let initial_snapshot: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&result.snapshot_file)?)?;

    let initial_footprints = initial_snapshot["footprints"].as_array().unwrap();
    assert_eq!(initial_footprints.len(), 1, "Should have 1 footprint");
    let initial_fp = &initial_footprints[0];

    let initial_position = &initial_fp["position"];
    let initial_x = initial_position["x"].as_i64().unwrap();
    let initial_y = initial_position["y"].as_i64().unwrap();
    let initial_uuid = initial_fp["uuid"].as_str().unwrap();

    println!("Initial UUID: {}", initial_uuid);
    println!("Initial position: ({}, {})", initial_x, initial_y);

    // Snapshot step 1 log (contains changeset, oplog, and lens state)
    assert_log_snapshot!("moved_step1.log", result.log_file);

    // --- Step 2: Rename to NewModule with moved() ---
    let board_renamed_content = std::fs::read_to_string(temp.path().join("Board_renamed.zen"))?;
    std::fs::write(&zen_file, board_renamed_content)?;

    let (output2, diagnostics2) = pcb_zen::run(&zen_file, res).unpack();
    if !diagnostics2.is_empty() {
        eprintln!("Zen evaluation diagnostics (step 2):");
        for diag in diagnostics2 {
            eprintln!("  {:?}", diag);
        }
    }
    let schematic2 = output2.expect("Second Zen evaluation should produce a schematic");

    // Verify moved_paths is set
    assert!(
        schematic2.moved_paths.contains_key("OldModule"),
        "Schematic should have moved_paths for OldModule"
    );

    let mut layout_diagnostics2 = Diagnostics::default();
    let result2 = process_layout(&schematic2, false, false, &mut layout_diagnostics2)?.unwrap();
    assert!(
        result2.pcb_file.exists(),
        "PCB file should exist after rename sync"
    );

    // Read updated snapshot
    let updated_snapshot: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&result2.snapshot_file)?)?;

    let updated_footprints = updated_snapshot["footprints"].as_array().unwrap();
    assert_eq!(updated_footprints.len(), 1, "Should still have 1 footprint");
    let updated_fp = &updated_footprints[0];

    let updated_position = &updated_fp["position"];
    let updated_x = updated_position["x"].as_i64().unwrap();
    let updated_y = updated_position["y"].as_i64().unwrap();
    let updated_uuid = updated_fp["uuid"].as_str().unwrap();

    println!("Updated UUID: {}", updated_uuid);
    println!("Updated position: ({}, {})", updated_x, updated_y);

    // Verify position preserved
    assert_eq!(
        initial_x, updated_x,
        "X position should be preserved after rename"
    );
    assert_eq!(
        initial_y, updated_y,
        "Y position should be preserved after rename"
    );

    // Snapshot step 2 log (contains changeset, oplog, and lens state)
    assert_log_snapshot!("moved_step2.log", result2.log_file);

    Ok(())
}
