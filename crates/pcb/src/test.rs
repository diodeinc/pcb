use anyhow::Result;
use clap::{Args, ValueEnum};
use comfy_table::{presets::UTF8_FULL_CONDENSED, Cell, Color, Table};
use log::debug;
use pcb_ui::prelude::*;
use serde::Serialize;
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

    /// Output format for test results
    #[arg(short = 'f', long = "format", value_enum, default_value_t = OutputFormat::Table)]
    pub format: OutputFormat,
}

#[derive(ValueEnum, Clone, Debug, Default)]
pub enum OutputFormat {
    Tap,
    Json,
    #[default]
    Table,
}

#[derive(Serialize, Clone)]
pub struct TestResult {
    pub test_bench_name: String,
    pub case_name: Option<String>,
    pub check_name: String,
    pub file_path: String,
    pub status: String, // "pass" or "fail"
}

#[derive(Serialize)]
pub struct JsonTestOutput {
    pub results: Vec<TestResult>,
    pub summary: TestSummary,
}

#[derive(Serialize)]
pub struct TestSummary {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
}

/// Test a single Starlark file by evaluating it and running testbench() calls
/// Returns structured test results including both successes and failures
pub fn test(
    zen_path: &Path,
    offline: bool,
    passes: Vec<Box<dyn pcb_zen_core::DiagnosticsPass>>,
) -> (Vec<pcb_zen_core::lang::error::BenchTestResult>, bool) {
    let file_name = zen_path.file_name().unwrap().to_string_lossy();

    // Show spinner while testing
    debug!("Testing Zener file: {}", zen_path.display());
    let spinner = Spinner::builder(format!("{file_name}: Testing")).start();

    // Evaluate the design in test mode
    let (_output, mut diagnostics) =
        pcb_zen::run(zen_path, offline, pcb_zen::EvalMode::Test).unpack();

    // Finish spinner before printing diagnostics
    spinner.finish();

    // Collect structured test results before applying passes
    let test_results: Vec<pcb_zen_core::lang::error::BenchTestResult> = diagnostics
        .diagnostics
        .iter()
        .filter_map(|diag| diag.downcast_error_ref::<pcb_zen_core::lang::error::BenchTestResult>())
        .cloned()
        .collect();

    // Apply all passes including rendering
    diagnostics.apply_passes(&passes);

    // Determine if there were any diagnostics errors (non-test failures)
    let had_errors = diagnostics.has_errors();

    (test_results, had_errors)
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

    let mut all_test_results: Vec<pcb_zen_core::lang::error::BenchTestResult> = Vec::new();
    let mut has_errors = false;

    // Process each .zen file
    for zen_path in zen_paths {
        let (results, had_errors_file) = test(
            &zen_path,
            args.offline,
            create_diagnostics_passes(&args.deny),
        );
        all_test_results.extend(results);
        if had_errors_file {
            has_errors = true;
        }
    }

    // Convert to output format
    let all_results: Vec<TestResult> = all_test_results
        .iter()
        .map(|result| TestResult {
            test_bench_name: result.test_bench_name.clone(),
            case_name: result.case_name.clone(),
            check_name: result.check_name.clone(),
            file_path: result.file_path.clone(),
            status: if result.passed { "pass" } else { "fail" }.to_string(),
        })
        .collect();

    // Output structured results to stdout
    match args.format {
        OutputFormat::Tap => output_tap(&all_results),
        OutputFormat::Json => output_json(&all_results)?,
        OutputFormat::Table => output_table(&all_results),
    }

    // Exit with error if there were failures
    let has_failures = all_test_results.iter().any(|r| !r.passed);
    if has_failures || has_errors {
        anyhow::bail!("Test run failed");
    }

    Ok(())
}

fn output_tap(results: &[TestResult]) {
    println!("TAP version 13");
    println!("1..{}", results.len());

    for (i, result) in results.iter().enumerate() {
        let test_num = i + 1;
        let status = if result.status == "pass" {
            "ok"
        } else {
            "not ok"
        };

        let case_suffix = result
            .case_name
            .as_ref()
            .map(|name| format!(" case '{name}'"))
            .unwrap_or_default();

        println!(
            "{} {} TestBench '{}'{} check '{}'",
            status, test_num, result.test_bench_name, case_suffix, result.check_name
        );
    }
}

fn output_table(results: &[TestResult]) {
    if results.is_empty() {
        return;
    }

    let mut table = Table::new();
    table.load_preset(UTF8_FULL_CONDENSED);

    // Set header
    table.set_header(vec![
        Cell::new("Status")
            .fg(Color::Blue)
            .add_attribute(comfy_table::Attribute::Bold),
        Cell::new("TestBench")
            .fg(Color::Blue)
            .add_attribute(comfy_table::Attribute::Bold),
        Cell::new("Case")
            .fg(Color::Blue)
            .add_attribute(comfy_table::Attribute::Bold),
        Cell::new("Check")
            .fg(Color::Blue)
            .add_attribute(comfy_table::Attribute::Bold),
    ]);

    // Add rows for each result
    for result in results {
        let status_cell = if result.status == "pass" {
            Cell::new("✓ PASS")
                .fg(Color::Green)
                .add_attribute(comfy_table::Attribute::Bold)
        } else {
            Cell::new("✗ FAIL")
                .fg(Color::Red)
                .add_attribute(comfy_table::Attribute::Bold)
        };

        let case_name = result.case_name.as_deref().unwrap_or("-");

        table.add_row(vec![
            status_cell,
            Cell::new(&result.test_bench_name),
            Cell::new(case_name),
            Cell::new(&result.check_name),
        ]);
    }

    println!("{table}");

    // Print summary
    let passed = results.iter().filter(|r| r.status == "pass").count();
    let failed = results.iter().filter(|r| r.status == "fail").count();

    println!();
    if failed > 0 {
        println!(
            "{} {} passed, {} failed",
            pcb_ui::icons::error().with_style(Style::Red),
            passed,
            failed
        );
    } else if passed > 0 {
        println!(
            "{} All {} tests passed",
            pcb_ui::icons::success().with_style(Style::Green),
            passed
        );
    }
}

fn output_json(results: &[TestResult]) -> Result<()> {
    let passed = results.iter().filter(|r| r.status == "pass").count();
    let failed = results.iter().filter(|r| r.status == "fail").count();

    let output = JsonTestOutput {
        results: results.to_vec(),
        summary: TestSummary {
            total: results.len(),
            passed,
            failed,
        },
    };

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
