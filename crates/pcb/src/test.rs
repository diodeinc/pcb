use anyhow::Result;
use clap::Args;
use log::debug;
use pcb_ui::prelude::*;
use std::path::{Path, PathBuf};

use crate::build::{collect_files, collect_files_recursive, create_diagnostics_passes};

#[derive(Args, Debug, Default, Clone)]
#[command(about = "Run tests in .zen files")]
pub struct TestArgs {
    /// One or more .zen files or directories containing .zen files (non-recursive) to test.
    /// When omitted, all .zen files in the current directory are tested.
    #[arg(value_name = "PATHS", value_hint = clap::ValueHint::AnyPath)]
    pub paths: Vec<PathBuf>,

    /// Recursively traverse directories to find .zen/.star files
    #[arg(short = 'r', long = "recursive", default_value_t = false)]
    pub recursive: bool,

    /// Disable network access (offline mode) - only use vendored dependencies
    #[arg(long = "offline")]
    pub offline: bool,

    /// Set lint level to deny (treat as error). Use 'warnings' for all warnings,
    /// or specific lint names like 'unstable-refs'
    #[arg(short = 'D', long = "deny", value_name = "LINT")]
    pub deny: Vec<String>,
}

/// Test a single Starlark file by evaluating it and running testbench() calls
/// Returns whether there were any errors
pub fn test(
    zen_path: &Path,
    offline: bool,
    passes: Vec<Box<dyn pcb_zen_core::DiagnosticsPass>>,
) -> bool {
    let file_name = zen_path.file_name().unwrap().to_string_lossy();

    // Show spinner while testing
    debug!("Testing Zener file: {}", zen_path.display());
    let spinner = Spinner::builder(format!("{file_name}: Testing")).start();

    // Evaluate the design in test mode
    let (_output, mut diagnostics) =
        pcb_zen::run(zen_path, offline, pcb_zen::EvalMode::Test).unpack();

    // Finish spinner before printing diagnostics
    spinner.finish();

    // Apply all passes including rendering
    diagnostics.apply_passes(&passes);

    // Check for errors
    if diagnostics.has_errors() {
        println!(
            "{} {}: Test failed",
            pcb_ui::icons::error(),
            file_name.with_style(Style::Red).bold()
        );
        return false;
    }
    true
}

pub fn execute(args: TestArgs) -> Result<()> {
    // Determine which .zen files to test
    let zen_paths = if args.recursive {
        collect_files_recursive(&args.paths)?
    } else {
        collect_files(&args.paths)?
    };

    if zen_paths.is_empty() {
        let cwd = std::env::current_dir()?;
        anyhow::bail!(
            "No .zen source files found in {}",
            cwd.canonicalize().unwrap_or(cwd).display()
        );
    }

    let mut passed = 0;
    let mut failed = 0;

    // Process each .zen file
    for zen_path in zen_paths {
        if test(
            &zen_path,
            args.offline,
            create_diagnostics_passes(&args.deny),
        ) {
            passed += 1;
        } else {
            failed += 1;
        }
    }

    // Print summary
    if failed > 0 {
        eprintln!(
            "\n{} {} passed, {} failed",
            pcb_ui::icons::error(),
            passed,
            failed
        );
        anyhow::bail!("Test run failed");
    } else {
        eprintln!("\n{} {} test(s) passed", pcb_ui::icons::success(), passed);
    }

    Ok(())
}
