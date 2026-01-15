use anyhow::Result;
use assert_fs::prelude::*;
use assert_fs::TempDir;
use pcb_layout::process_layout;
use pcb_zen_core::Diagnostics;
use serial_test::serial;

mod helpers;
use helpers::*;

/// Test that FPID changes (footprint type changes) result in the footprint being replaced
/// with the new geometry.
///
/// This test verifies the lens-based sync's FPID change handling:
/// 1. Creates initial layout with R_0402 package
/// 2. Changes to R_0603 package (different footprint geometry)
/// 3. Verifies the footprint geometry changed (pads are further apart in 0603)
///
/// The lens layer preserves position/orientation/layer but:
/// - Loads the new footprint geometry from the library
/// - Unlocks the footprint (for potential adjustment)
/// - Resets field positions (new geometry may be different size)
#[cfg(not(target_os = "windows"))]
#[test]
#[serial]
fn test_fpid_change_replaces_footprint_geometry() -> Result<()> {
    // Create a temp directory and copy the test resources
    let temp = TempDir::new()?.into_persistent();
    let resource_path = get_resource_path("fpid_change");
    temp.copy_from(&resource_path, &["**/*", "!.pcb/cache/**/*"])?;

    // --- Step 1: Initial layout with 0402 package ---
    let zen_file = temp.path().join("Board.zen");
    assert!(zen_file.exists(), "Board.zen should exist");

    let (output, diagnostics) = pcb_zen::run(&zen_file, pcb_zen::EvalConfig::default()).unpack();
    if !diagnostics.is_empty() {
        eprintln!("Zen evaluation diagnostics (step 1):");
        for diag in diagnostics {
            eprintln!("  {:?}", diag);
        }
    }
    let schematic = output.expect("Zen evaluation should produce a schematic");

    let mut layout_diagnostics = Diagnostics::default();
    let result = process_layout(
        &schematic,
        &zen_file,
        false,
        false,
        false,
        &mut layout_diagnostics,
    )?
    .unwrap();
    assert!(
        result.pcb_file.exists(),
        "PCB file should exist after initial sync"
    );

    // Snapshot the log (contains changeset, oplog, and lens state)
    assert_log_snapshot!("fpid_change_step1.log", result.log_file);

    // Read and parse the initial snapshot
    let initial_snapshot: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&result.snapshot_file)?)?;

    // Verify initial footprint is 0402
    let initial_footprints = initial_snapshot["footprints"].as_array().unwrap();
    let initial_r1 = initial_footprints
        .iter()
        .find(|fp| fp["reference"].as_str() == Some("U1"))
        .expect("R1 footprint (U1) should exist");

    assert_eq!(
        initial_r1["footprint"].as_str(),
        Some("Resistor_SMD:R_0402_1005Metric"),
        "Initial footprint should be R_0402"
    );

    // Get initial pad positions
    let initial_pads = initial_r1["pads"].as_array().unwrap();
    let initial_pad1 = &initial_pads[0]["position"];
    let initial_pad2 = &initial_pads[1]["position"];
    let initial_pad_spacing =
        (initial_pad2["x"].as_i64().unwrap() - initial_pad1["x"].as_i64().unwrap()).abs();

    println!("Initial pad spacing (0402): {} nm", initial_pad_spacing);

    // --- Step 2: Change to 0603 package ---
    // Copy the 0603 version over the original Board.zen
    let board_0603_content = std::fs::read_to_string(temp.path().join("Board_0603.zen"))?;
    std::fs::write(&zen_file, board_0603_content)?;

    let (output2, diagnostics2) = pcb_zen::run(&zen_file, pcb_zen::EvalConfig::default()).unpack();
    if !diagnostics2.is_empty() {
        eprintln!("Zen evaluation diagnostics (step 2):");
        for diag in diagnostics2 {
            eprintln!("  {:?}", diag);
        }
    }
    let schematic2 = output2.expect("Second Zen evaluation should produce a schematic");

    let mut layout_diagnostics2 = Diagnostics::default();
    let result2 = process_layout(
        &schematic2,
        &zen_file,
        false,
        false,
        false,
        &mut layout_diagnostics2,
    )?
    .unwrap();
    assert!(
        result2.pcb_file.exists(),
        "PCB file should exist after FPID change sync"
    );

    // Snapshot the log (contains changeset, oplog, and lens state)
    assert_log_snapshot!("fpid_change_step2.log", result2.log_file);

    // Read and parse the updated snapshot
    let updated_snapshot: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&result2.snapshot_file)?)?;

    // Verify updated footprint is 0603
    let updated_footprints = updated_snapshot["footprints"].as_array().unwrap();
    let updated_r1 = updated_footprints
        .iter()
        .find(|fp| fp["reference"].as_str() == Some("U1"))
        .expect("R1 footprint (U1) should exist after FPID change");

    assert_eq!(
        updated_r1["footprint"].as_str(),
        Some("Resistor_SMD:R_0603_1608Metric"),
        "Updated footprint should be R_0603"
    );

    // Verify the geometry changed - pads should be further apart for 0603
    let updated_pads = updated_r1["pads"].as_array().unwrap();
    let updated_pad1 = &updated_pads[0]["position"];
    let updated_pad2 = &updated_pads[1]["position"];
    let updated_pad_spacing =
        (updated_pad2["x"].as_i64().unwrap() - updated_pad1["x"].as_i64().unwrap()).abs();

    println!("Updated pad spacing (0603): {} nm", updated_pad_spacing);

    // 0603 pads are further apart than 0402 (roughly 1.6mm vs 1.0mm center-to-center)
    assert!(
        updated_pad_spacing > initial_pad_spacing,
        "0603 pad spacing ({} nm) should be greater than 0402 ({} nm)",
        updated_pad_spacing,
        initial_pad_spacing
    );

    // Verify the footprint is unlocked after FPID change
    // Note: The lens layer sets locked=False when FPID changes
    assert_eq!(
        updated_r1["locked"].as_bool(),
        Some(false),
        "Footprint should be unlocked after FPID change"
    );

    Ok(())
}

/// Test that FPID change preserves the footprint position.
#[cfg(not(target_os = "windows"))]
#[test]
#[serial]
fn test_fpid_change_preserves_position() -> Result<()> {
    // Create a temp directory and copy the test resources
    let temp = TempDir::new()?.into_persistent();
    let resource_path = get_resource_path("fpid_change");
    temp.copy_from(&resource_path, &["**/*", "!.pcb/cache/**/*"])?;

    // --- Step 1: Initial layout with 0402 package ---
    let zen_file = temp.path().join("Board.zen");

    let (output, _) = pcb_zen::run(&zen_file, pcb_zen::EvalConfig::default()).unpack();
    let schematic = output.expect("Zen evaluation should produce a schematic");
    let mut layout_diagnostics = Diagnostics::default();
    let result = process_layout(
        &schematic,
        &zen_file,
        false,
        false,
        false,
        &mut layout_diagnostics,
    )?
    .unwrap();

    let initial_snapshot: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&result.snapshot_file)?)?;

    let initial_footprints = initial_snapshot["footprints"].as_array().unwrap();
    let initial_r1 = initial_footprints
        .iter()
        .find(|fp| fp["reference"].as_str() == Some("U1"))
        .unwrap();

    let initial_position = &initial_r1["position"];
    let initial_x = initial_position["x"].as_i64().unwrap();
    let initial_y = initial_position["y"].as_i64().unwrap();

    println!("Initial position: ({}, {})", initial_x, initial_y);

    // --- Step 2: Change to 0603 package ---
    let board_0603_content = std::fs::read_to_string(temp.path().join("Board_0603.zen"))?;
    std::fs::write(&zen_file, board_0603_content)?;

    let (output2, _) = pcb_zen::run(&zen_file, pcb_zen::EvalConfig::default()).unpack();
    let schematic2 = output2.expect("Second Zen evaluation should produce a schematic");
    let mut layout_diagnostics2 = Diagnostics::default();
    let result2 = process_layout(
        &schematic2,
        &zen_file,
        false,
        false,
        false,
        &mut layout_diagnostics2,
    )?
    .unwrap();

    let updated_snapshot: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&result2.snapshot_file)?)?;

    let updated_footprints = updated_snapshot["footprints"].as_array().unwrap();
    let updated_r1 = updated_footprints
        .iter()
        .find(|fp| fp["reference"].as_str() == Some("U1"))
        .unwrap();

    let updated_position = &updated_r1["position"];
    let updated_x = updated_position["x"].as_i64().unwrap();
    let updated_y = updated_position["y"].as_i64().unwrap();

    println!("Updated position: ({}, {})", updated_x, updated_y);

    // Position should be preserved after FPID change
    assert_eq!(
        initial_x, updated_x,
        "X position should be preserved after FPID change"
    );
    assert_eq!(
        initial_y, updated_y,
        "Y position should be preserved after FPID change"
    );

    Ok(())
}
