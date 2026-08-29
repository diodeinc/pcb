//! Debug tests that process RouterInput JSON files and generate debug outputs.
//!
//! To use:
//! 1. Save RouterInput JSON files to `tests/debug_inputs/`
//! 2. Run `cargo test debug_` to run all debug tests
//! 3. Run `cargo test debug_dm0001` to run a specific test
//! 4. Check `tests/debug_outputs/<filename>/` for SVG outputs and timing.json
//!
//! Each JSON file will get its own output folder with:
//! - 01_input.svg - Obstacles and ports only
//! - 02_visibility_graph.svg - With visibility graph overlay
//! - 03_pathfinding.svg - Paths after A* search
//! - 04_improve_crossings.svg - Paths after crossing improvement
//! - 05a_nudge_x_unify.svg - After X-dimension unifying pass
//! - 05b_nudge_x_nudge.svg - After X-dimension nudging pass
//! - 05c_nudge_y_unify.svg - After Y-dimension unifying pass
//! - 05d_nudge_y_nudge.svg - After Y-dimension nudging pass
//! - 05e_nudge_merge.svg - After same-net path merging
//! - 06_final.svg - Final paths after nudging
//! - timing.json - Timing breakdown
//!
//! To add a new test, add its filename (without .json extension) to the
//! `debug_test!` invocations at the bottom of this file.

use std::path::Path;

/// Process a single JSON file and generate debug outputs.
fn process_debug_file(test_name: &str) {
    let _ = env_logger::builder().is_test(true).try_init();

    // Strip "debug_" prefix from test name to get the actual filename
    let filename = test_name.strip_prefix("debug_").unwrap_or(test_name);

    let input_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/debug_inputs");
    let output_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/debug_outputs");

    let input_path = input_dir.join(format!("{}.json", filename));

    if !input_path.exists() {
        println!("Skipping test: {} not found at {:?}", filename, input_path);
        println!("To generate debug outputs:");
        println!("  1. Use 'Save RouterInput JSON' button in sch app debug panel");
        println!("  2. Save JSON files to tests/debug_inputs/");
        return;
    }

    println!("Processing {}...", filename);

    match ortho_router::debug::process_debug_input(&input_path, &output_dir) {
        Ok(timing) => {
            println!(
                "OK ({:.2}ms total, {:.2}ms pathfinding)",
                timing.total_ms, timing.pathfinding_ms
            );
            println!(
                "  Breakdown: vis={:.2}ms, path={:.2}ms, cross={:.2}ms, nudge={:.2}ms",
                timing.visibility_graph_ms,
                timing.pathfinding_ms,
                timing.improve_crossings_ms,
                timing.nudging_ms
            );
            println!("Output written to {:?}", output_dir.join(filename));
        }
        Err(e) => {
            panic!("Failed to process {}: {}", filename, e);
        }
    }
}

/// Macro to generate a debug test for a specific input file.
///
/// Usage: `debug_test!(dm0001);` generates a test named `debug_dm0001`
/// that processes `tests/debug_inputs/dm0001.json`.
macro_rules! debug_test {
    ($name:ident) => {
        #[test]
        fn $name() {
            process_debug_file(stringify!($name));
        }
    };
}

// Generate individual tests for each input file.
// Add new entries here when you add new JSON files to tests/debug_inputs/
debug_test!(debug_dm0001);
debug_test!(debug_dm0001_buck);
debug_test!(debug_dm0001_phasedriverb);
debug_test!(debug_dm0001_stm32g431c8t6);
debug_test!(debug_dm0003);
debug_test!(debug_dm0003_cm5);
debug_test!(debug_dm0003_pcie_m2);
debug_test!(debug_dm0003_usb_3pi);
debug_test!(debug_dm0003_usb_pi);

/// Process all JSON files in tests/debug_inputs/ and write outputs to tests/debug_outputs/
/// This is useful for running all tests at once with summary output.
#[test]
fn debug_all() {
    let _ = env_logger::builder().is_test(true).try_init();

    let input_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/debug_inputs");
    let output_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/debug_outputs");

    // Skip if input directory doesn't exist or is empty
    if !input_dir.exists() {
        println!("No debug_inputs directory found, skipping");
        return;
    }

    let json_files: Vec<_> = std::fs::read_dir(&input_dir)
        .expect("Failed to read debug_inputs directory")
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .map(|ext| ext == "json")
                .unwrap_or(false)
        })
        .collect();

    if json_files.is_empty() {
        println!("No JSON files in debug_inputs/, skipping");
        println!("To generate debug outputs:");
        println!("  1. Use 'Save RouterInput JSON' button in sch app debug panel");
        println!("  2. Save JSON files to tests/debug_inputs/");
        println!("  3. Run `cargo test debug_`");
        return;
    }

    println!(
        "Processing {} JSON files from {:?}",
        json_files.len(),
        input_dir
    );

    let mut results = Vec::new();
    let mut errors = Vec::new();

    for entry in json_files {
        let path = entry.path();
        let filename = path.file_name().unwrap().to_string_lossy().to_string();

        print!("  Processing {}... ", filename);

        match ortho_router::debug::process_debug_input(&path, &output_dir) {
            Ok(timing) => {
                println!(
                    "OK ({:.2}ms total, {:.2}ms pathfinding)",
                    timing.total_ms, timing.pathfinding_ms
                );
                results.push((filename, timing));
            }
            Err(e) => {
                println!("ERROR: {}", e);
                errors.push((filename, e.to_string()));
            }
        }
    }

    // Summary
    println!("\n=== Summary ===");
    println!("Processed: {}", results.len());
    println!("Errors: {}", errors.len());

    if !results.is_empty() {
        println!("\nTiming breakdown:");
        for (filename, timing) in &results {
            println!(
                "  {}: total={:.2}ms (vis={:.2}ms, path={:.2}ms, cross={:.2}ms, nudge={:.2}ms)",
                filename,
                timing.total_ms,
                timing.visibility_graph_ms,
                timing.pathfinding_ms,
                timing.improve_crossings_ms,
                timing.nudging_ms
            );
        }
    }

    if !errors.is_empty() {
        println!("\nErrors:");
        for (filename, error) in &errors {
            println!("  {}: {}", filename, error);
        }
        panic!("Some files failed to process");
    }

    println!("\nOutputs written to {:?}", output_dir);
}
