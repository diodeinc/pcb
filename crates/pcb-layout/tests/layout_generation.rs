use anyhow::Result;
use assert_fs::TempDir;
use assert_fs::prelude::*;
use pcb_layout::process_layout;
use pcb_zen_core::DefaultFileProvider;
use pcb_zen_core::Diagnostics;
use serial_test::serial;

mod helpers;
use helpers::*;

macro_rules! layout_test {
    ($name:expr, $board_name:expr) => {
        layout_test!($name, $board_name, false);
    };
    ($name:expr, $board_name:expr, $snapshot_kicad_pro:expr) => {
        paste::paste! {
            #[cfg(not(target_os = "windows"))]
            #[test]
            #[serial]
            fn [<test_layout_generation_with_ $name:snake>]() -> Result<()> {
                // Create a temp directory and copy the test resources
                let temp = TempDir::new()?.into_persistent();
                let resource_path = get_resource_path($name);
                temp.copy_from(&resource_path, &["**/*", "!.pcb/cache/**/*"])?;

                // Find and evaluate the board zen file
                let zen_file = temp.path().join(format!("{}.zen", $board_name));
                assert!(zen_file.exists(), "{}.zen should exist", $board_name);

                let mut workspace_info = pcb_zen::get_workspace_info(&DefaultFileProvider::new(), temp.path())?;
                let res = pcb_zen::resolve_dependencies(&mut workspace_info, false, false)?;
                let model_dirs = res.kicad_model_dirs();

                // Evaluate the Zen file to generate a schematic
                let (output, diagnostics) = pcb_zen::run(&zen_file, res).unpack();

                // Check for errors in evaluation
                if !diagnostics.is_empty() {
                    eprintln!("Zen evaluation diagnostics:");
                    for diag in diagnostics {
                        eprintln!("  {:?}", diag);
                    }
                }

                let schematic = output.expect("Zen evaluation should produce a schematic");

                // Process the layout
                let mut diagnostics = Diagnostics::default();
                let result = process_layout(&schematic, &model_dirs, false, false, &mut diagnostics)?.unwrap();

                // Verify the layout was created
                assert!(result.pcb_file.exists(), "PCB file should exist");
                assert!(result.netlist_file.exists(), "Netlist file should exist");
                assert!(result.snapshot_file.exists(), "Snapshot file should exist");
                assert!(result.log_file.exists(), "Log file should exist");

                // Check the layout snapshot matches
                assert_file_snapshot!(
                    format!("{}.layout.json", $name),
                    result.snapshot_file
                );

                // Check the log file snapshot (normalized for timing and paths)
                // Contains: lens state (OLD/NEW), changeset, oplog, and debug info
                assert_log_snapshot!(
                    format!("{}.log", $name),
                    result.log_file
                );

                // Snapshot netclass_patterns from .kicad_pro if requested
                if $snapshot_kicad_pro {
                    let kicad_pro_path = result.pcb_file.with_extension("kicad_pro");
                    assert!(kicad_pro_path.exists(), "kicad_pro file should exist");
                    assert_netclass_patterns_snapshot!(
                        format!("{}.netclass_patterns.json", $name),
                        kicad_pro_path
                    );
                }

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

layout_test!("dnp", "MyBoard");

layout_test!("zones", "Board");

layout_test!("tracks", "Board");

layout_test!("graphics", "Board");

layout_test!("complex", "Board");

layout_test!("netclass_assignment", "netclass", true);

layout_test!("not_connected", "Board");

layout_test!("not_connected_single_pin_multi_pad", "Board");
