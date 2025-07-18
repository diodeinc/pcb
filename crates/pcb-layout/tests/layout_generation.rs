use anyhow::Result;
use assert_fs::prelude::*;
use assert_fs::TempDir;
use pcb_layout::process_layout;
use serial_test::serial;

mod helpers;
use helpers::*;

macro_rules! layout_test {
    ($name:expr, $board_name:expr) => {
        paste::paste! {
            #[test]
            #[serial]
            fn [<test_layout_generation_with_ $name:snake>]() -> Result<()> {
                // Create a temp directory and copy the test resources
                let temp = TempDir::new()?.into_persistent();
                let resource_path = get_resource_path($name);
                temp.copy_from(&resource_path, &["**/*"])?;

                // Find and evaluate the board zen file
                let zen_file = temp.path().join(format!("{}.zen", $board_name));
                assert!(zen_file.exists(), "{}.zen should exist", $board_name);

                // Evaluate the Zen file to generate a schematic
                let eval_result = pcb_zen::run(&zen_file);

                // Check for errors in evaluation
                if !eval_result.diagnostics.is_empty() {
                    eprintln!("Zen evaluation diagnostics:");
                    for diag in &eval_result.diagnostics {
                        eprintln!("  {:?}", diag);
                    }
                }

                let schematic = eval_result
                    .output
                    .expect("Zen evaluation should produce a schematic");

                // Process the layout
                let result = process_layout(&schematic, &zen_file)?;

                // Verify the layout was created
                assert!(result.pcb_file.exists(), "PCB file should exist");
                assert!(result.netlist_file.exists(), "Netlist file should exist");
                assert!(result.snapshot_file.exists(), "Snapshot file should exist");
                assert!(result.log_file.exists(), "Log file should exist");

                // Print the log file contents
                let log_contents = std::fs::read_to_string(&result.log_file)?;
                println!("Layout log file contents:");
                println!("========================");
                println!("{}", log_contents);
                println!("========================");

                // Check the snapshot matches
                assert_file_snapshot!(
                    format!("{}.layout.json", $name),
                    result.snapshot_file
                );

                Ok(())
            }
        }
    };
}

// Schematic: A couple BMI270 modules in Starlark.
layout_test!("simple", "MyBoard");

layout_test!("module_layout", "Main");
layout_test!("component_side_sync", "Board");

layout_test!("multi_pads", "MultiPads");
