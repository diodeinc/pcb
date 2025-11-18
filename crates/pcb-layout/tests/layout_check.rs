use anyhow::Result;
use assert_fs::prelude::*;
use assert_fs::TempDir;
use pcb_kicad::PythonScriptBuilder;
use pcb_layout::process_layout;
use pcb_layout::sync_check::SyncReport;
use serial_test::serial;
use std::path::Path;

#[macro_use]
mod helpers;
use helpers::*;

#[cfg(not(target_os = "windows"))]
#[test]
#[serial]
fn test_layout_check_diff() -> Result<()> {
    // 1. Setup "simple" project
    let temp = TempDir::new()?.into_persistent();
    let resource_path = get_resource_path("simple");
    temp.copy_from(&resource_path, &["**/*", "!.pcb/cache/**/*"])?;

    let zen_file = temp.path().join("MyBoard.zen");

    // 2. Run initial layout generation (establish baseline)
    let (output, _) = pcb_zen::run(&zen_file, pcb_zen::EvalConfig::default()).unpack();
    let schematic = output.expect("schematic");
    let result = process_layout(&schematic, &zen_file, false, false)?;
    assert!(result.pcb_file.exists());

    // 3. Modify the schematic to introduce a change
    // We'll remove the "BMI270" module instance to simulate a removal.
    let mut modified_schematic = schematic.clone();

    // Find keys to remove.
    // We need to use to_string() because InstanceRef doesn't have ends_with.
    let keys_to_remove: Vec<pcb_sch::InstanceRef> = modified_schematic
        .instances
        .keys()
        .filter(|k| k.to_string().contains(".BMI270"))
        .cloned()
        .collect();

    assert!(!keys_to_remove.is_empty(), "Should find BMI270 instance");

    for key in keys_to_remove {
        modified_schematic.instances.remove(&key);
    }

    // Write modified netlist
    let modified_netlist_path = temp.path().join("modified_netlist.json");
    let json_content = modified_schematic.to_json()?;
    std::fs::write(&modified_netlist_path, json_content)?;

    // 4. Run check script
    let changes_path = temp.path().join("changes.json");
    let pcb_path = result.pcb_file;

    // Accessing the script from source
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let script_path = Path::new(manifest_dir).join("src/scripts/update_layout_file.py");
    let script_content = std::fs::read_to_string(script_path)?;

    let removal_log_path = temp.path().join("removal.log");
    let removal_log_file = std::fs::File::create(&removal_log_path)?;

    let script_builder = PythonScriptBuilder::new(&script_content)
        .arg("--check")
        .arg("--changes-output")
        .arg(changes_path.to_str().unwrap())
        .arg("-j")
        .arg(modified_netlist_path.to_str().unwrap())
        .arg("-o")
        .arg(pcb_path.to_str().unwrap())
        .log_file(removal_log_file);

    script_builder.run()?;

    // 5. Verify removal changes
    let report = SyncReport::from_json_file(&changes_path)?;
    assert!(report.change_count > 0, "Should detect removals");

    // Snapshot removals
    let mut changes_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&changes_path)?)?;
    sanitize_report_json(&mut changes_json);
    std::fs::write(&changes_path, serde_json::to_string_pretty(&changes_json)?)?;
    assert_file_snapshot!("layout_check_removal.json", changes_path);

    // 6. Scenario: Metadata Change
    // We use the original schematic but modify a value
    let mut metadata_schematic = schematic.clone();
    // Find a resistor or capacitor to change value
    for (_, inst) in metadata_schematic.instances.iter_mut() {
        if inst.value().is_some() {
            inst.add_attribute("value", "100M".to_string()); // Change value to something distinctive
            break;
        }
    }

    let metadata_netlist_path = temp.path().join("metadata_netlist.json");
    std::fs::write(&metadata_netlist_path, metadata_schematic.to_json()?)?;

    let metadata_changes_path = temp.path().join("metadata_changes.json");
    let metadata_log_path = temp.path().join("metadata.log");
    let metadata_log_file = std::fs::File::create(&metadata_log_path)?;

    let script_builder = PythonScriptBuilder::new(&script_content)
        .arg("--check")
        .arg("--changes-output")
        .arg(metadata_changes_path.to_str().unwrap())
        .arg("-j")
        .arg(metadata_netlist_path.to_str().unwrap())
        .arg("-o")
        .arg(pcb_path.to_str().unwrap())
        .log_file(metadata_log_file); // Check against original board

    script_builder.run()?;

    let report = SyncReport::from_json_file(&metadata_changes_path)?;
    assert!(report.change_count > 0, "Should detect metadata changes");

    let mut changes_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&metadata_changes_path)?)?;
    sanitize_report_json(&mut changes_json);
    std::fs::write(
        &metadata_changes_path,
        serde_json::to_string_pretty(&changes_json)?,
    )?;
    assert_file_snapshot!("layout_check_metadata.json", metadata_changes_path);

    // 7. Scenario: Addition (Component missing from board)
    // We need a board that is missing components.
    // We can use the 'modified_netlist' (from removal scenario) to UPDATE the board.
    // Then we check against the ORIGINAL netlist.

    // Update board to remove components
    let update_snapshot_path = temp.path().join("update_snapshot.json");
    let update_script_builder = PythonScriptBuilder::new(&script_content)
        // No --check means update
        .arg("-j")
        .arg(modified_netlist_path.to_str().unwrap())
        .arg("-o")
        .arg(pcb_path.to_str().unwrap())
        .arg("-s")
        .arg(update_snapshot_path.to_str().unwrap());

    update_script_builder.run()?;

    // Now check against original netlist (which has the components)
    // The board now lacks them, so they should be detected as "Added" (missing from board)

    // We need original netlist file path. process_layout generated it but in a temp dir inside LayoutPaths which is gone.
    // But we have `schematic`.
    let original_netlist_path = temp.path().join("original_netlist.json");
    std::fs::write(&original_netlist_path, schematic.to_json()?)?;

    let addition_changes_path = temp.path().join("addition_changes.json");
    let addition_log_path = temp.path().join("addition.log");
    let addition_log_file = std::fs::File::create(&addition_log_path)?;

    let script_builder = PythonScriptBuilder::new(&script_content)
        .arg("--check")
        .arg("--changes-output")
        .arg(addition_changes_path.to_str().unwrap())
        .arg("-j")
        .arg(original_netlist_path.to_str().unwrap())
        .arg("-o")
        .arg(pcb_path.to_str().unwrap())
        .log_file(addition_log_file);

    script_builder.run()?;

    let report = SyncReport::from_json_file(&addition_changes_path)?;
    assert!(report.change_count > 0, "Should detect additions");

    let mut changes_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&addition_changes_path)?)?;
    sanitize_report_json(&mut changes_json);
    std::fs::write(
        &addition_changes_path,
        serde_json::to_string_pretty(&changes_json)?,
    )?;
    assert_file_snapshot!("layout_check_addition.json", addition_changes_path);

    Ok(())
}

fn sanitize_report_json(json: &mut serde_json::Value) {
    if let Some(obj) = json.as_object_mut() {
        obj.remove("timestamp");
        obj.remove("source");
        obj.remove("netlist_source");
    }
}
