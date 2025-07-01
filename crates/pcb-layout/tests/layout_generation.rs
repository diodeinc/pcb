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

                // Persist the temp directory so it doesn't get cleaned up
                let temp_path = temp.path().to_path_buf();
                println!("Test directory persisted at: {}", temp_path.display());

                // Find and evaluate the board star file
                let star_file = temp_path.join(format!("{}.star", $board_name));
                assert!(star_file.exists(), "{}.star should exist", $board_name);

                // Evaluate the Starlark file to generate a schematic
                let eval_result = pcb_star::run(&star_file);

                // Check for errors in evaluation
                if !eval_result.diagnostics.is_empty() {
                    eprintln!("Starlark evaluation diagnostics:");
                    for diag in &eval_result.diagnostics {
                        eprintln!("  {:?}", diag);
                    }
                }

                let schematic = eval_result
                    .output
                    .expect("Starlark evaluation should produce a schematic");

                // Process the layout
                let result = process_layout(&schematic, &star_file)?;

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
