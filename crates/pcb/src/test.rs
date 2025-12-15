use anyhow::Result;
use clap::{Args, ValueEnum};
use comfy_table::{presets::UTF8_FULL_CONDENSED, Cell, Color, Table};
use log::debug;
use pcb_ui::prelude::*;
use pcb_zen_core::ModulePath;
use serde::Serialize;
use std::path::{Path, PathBuf};

use crate::build::create_diagnostics_passes;
use crate::file_walker;

#[derive(Args, Debug, Default, Clone)]
#[command(about = "Run tests in .zen files")]
pub struct TestArgs {
    /// One or more .zen files or directories containing .zen files to test.
    /// When omitted, all .zen files in the current directory tree are tested.
    #[arg(value_name = "PATHS", value_hint = clap::ValueHint::AnyPath)]
    pub paths: Vec<PathBuf>,

    /// Disable network access (offline mode) - only use vendored dependencies
    #[arg(long = "offline")]
    pub offline: bool,

    /// Set lint level to deny (treat as error). Use 'warnings' for all warnings,
    /// or specific lint names like 'unstable-refs'
    #[arg(short = 'D', long = "deny", value_name = "LINT")]
    pub deny: Vec<String>,

    /// Suppress diagnostics by kind or severity. Use 'warnings' or 'errors' for all
    /// warnings/errors, or specific kinds like 'electrical.voltage_mismatch'.
    /// Supports hierarchical matching (e.g., 'electrical' matches 'electrical.voltage_mismatch')
    #[arg(short = 'S', long = "suppress", value_name = "KIND")]
    pub suppress: Vec<String>,

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

    // Evaluate the design (use eval() not run() to get EvalOutput and collect TestBenches)
    let eval_result = pcb_zen::eval(
        zen_path,
        pcb_zen::EvalConfig {
            offline,
            ..Default::default()
        },
    );

    let mut diagnostics = eval_result.diagnostics;

    // Execute deferred TestBench checks if evaluation succeeded
    if let Some(eval_output) = eval_result.output {
        let testbenches = eval_output.collect_testbenches();

        if !testbenches.is_empty() {
            debug!(
                "Found {} TestBench(es), executing deferred checks",
                testbenches.len()
            );

            // Execute checks for each TestBench
            for testbench in testbenches {
                let check_diagnostics = execute_testbench_checks(testbench, &eval_output);
                diagnostics.diagnostics.extend(check_diagnostics);
            }
        }
    }

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

/// Execute all deferred checks for a TestBench
fn execute_testbench_checks(
    testbench: &pcb_zen_core::lang::test_bench::FrozenTestBenchValue,
    eval_output: &pcb_zen_core::lang::eval::EvalOutput,
) -> Vec<pcb_zen_core::Diagnostic> {
    use pcb_zen_core::lang::eval::EvalContext;
    use pcb_zen_core::lang::test_bench::execute_deferred_check;
    use starlark::environment::Module;
    use starlark::eval::Evaluator;
    use starlark::values::{dict::AllocDict, Heap, ValueLike};

    let mut all_diagnostics = Vec::new();
    let mut total_checks = 0;
    let mut passed_checks = 0;

    // Create a minimal EvalContext with the module tree
    let eval_ctx = EvalContext::new(eval_output.load_resolver.clone())
        .set_source_path(std::path::PathBuf::from(testbench.source_path()));

    // Share the module tree
    *eval_ctx.module_tree.lock().unwrap() = eval_output.module_tree.clone();

    // Create a ContextValue and attach it to the module
    let heap = Heap::new();
    let module = Module::new();
    let ctx_value = pcb_zen_core::lang::context::ContextValue::from_context(&eval_ctx);
    module.set_extra_value(heap.alloc_complex(ctx_value));
    let mut eval = Evaluator::new(&module);

    for deferred_case in testbench.deferred_cases().iter() {
        // Look up evaluated module from tree by full path
        let module_path = ModulePath::from(deferred_case.case_final_name.clone());
        let case_module = eval_output.module_tree.get(&module_path).cloned();

        if let Some(module_value) = case_module {
            // Reconstruct inputs dict from deferred case params
            let inputs_dict = heap
                .alloc(AllocDict(
                    deferred_case
                        .params
                        .iter()
                        .map(|(k, v)| (heap.alloc_str(k).to_value(), v.to_value()))
                        .collect::<Vec<_>>(),
                ))
                .to_value();

            // Execute each check
            let ctx = pcb_zen_core::lang::test_bench::CheckContext {
                test_bench_name: testbench.name(),
                case_name: &deferred_case.case_name,
                source_path: testbench.source_path(),
                call_span: testbench.call_span(),
            };

            for check in &deferred_case.checks {
                total_checks += 1;
                // module_value is FrozenModuleValue (ModuleValueGen<FrozenValue>)
                // Allocate it to heap to get a Value
                let module_as_value = heap.alloc_complex(module_value.clone());
                let (passed, mut diagnostics) =
                    execute_deferred_check(&mut eval, check, module_as_value, inputs_dict, &ctx);

                if passed {
                    passed_checks += 1;
                }

                all_diagnostics.append(&mut diagnostics);
            }
        }
    }

    // Print summary for successful test benches
    if total_checks > 0 && passed_checks == total_checks {
        let case_word = if testbench.case_count() == 1 {
            "case"
        } else {
            "cases"
        };
        let check_word = if total_checks == 1 { "check" } else { "checks" };
        eprintln!(
            "{} {}: {} {} passed across {} {}",
            pcb_ui::icons::success().with_style(pcb_ui::Style::Green),
            testbench.name(),
            total_checks,
            check_word,
            testbench.case_count(),
            case_word
        );
    }

    all_diagnostics
}

pub fn execute(args: TestArgs) -> Result<()> {
    // Determine which .zen files to test - always recursive for directories
    let zen_paths = file_walker::collect_zen_files(&args.paths, false)?;

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
            create_diagnostics_passes(&args.suppress, &[]),
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
