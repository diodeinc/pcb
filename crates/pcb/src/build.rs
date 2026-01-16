use anyhow::Result;
use clap::Args;
use log::debug;
use pcb_sch::Schematic;
use pcb_ui::prelude::*;
use std::path::{Path, PathBuf};
use tracing::{info_span, instrument};

use crate::file_walker;

fn execute_electrical_check(
    check: &pcb_zen_core::lang::electrical_check::FrozenElectricalCheck,
    defining_module: &pcb_zen_core::lang::module::FrozenModuleValue,
) -> pcb_zen_core::Diagnostic {
    use starlark::environment::Module;
    use starlark::eval::Evaluator;
    use starlark::values::Heap;

    let heap = Heap::new();
    let module = Module::new();
    let mut eval = Evaluator::new(&module);
    let module_value = heap.alloc_simple(defining_module.clone());

    pcb_zen_core::lang::electrical_check::execute_electrical_check(&mut eval, check, module_value)
}

pub fn create_diagnostics_passes(
    suppress: &[String],
    promote: &[String],
) -> Vec<Box<dyn pcb_zen_core::DiagnosticsPass>> {
    let mut passes: Vec<Box<dyn pcb_zen_core::DiagnosticsPass>> = vec![
        Box::new(pcb_zen_core::FilterHiddenPass),
        Box::new(pcb_zen_core::SuppressPass::new(suppress.to_vec())),
        Box::new(pcb_zen_core::CommentSuppressPass::new()),
    ];

    // Add promote pass if patterns are specified (e.g., -W style)
    if !promote.is_empty() {
        passes.push(Box::new(pcb_zen_core::PromotePass::new(promote.to_vec())));
    }

    passes.push(Box::new(pcb_zen_core::AggregatePass));
    passes.push(Box::new(pcb_zen_core::SortPass));
    passes.push(Box::new(pcb_zen::diagnostics::RenderPass));

    passes
}

#[derive(Args, Debug, Default, Clone)]
#[command(about = "Build PCB projects from .zen files")]
pub struct BuildArgs {
    /// One or more .zen files or directories to build.
    /// When omitted, builds the current directory.
    #[arg(value_name = "PATHS", value_hint = clap::ValueHint::AnyPath)]
    pub paths: Vec<PathBuf>,

    /// Print JSON netlist to stdout (undocumented)
    #[arg(long = "netlist", hide = true)]
    pub netlist: bool,

    /// Print board config JSON to stdout (undocumented)
    #[arg(long = "board-config", hide = true)]
    pub board_config: bool,

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

    /// Promote diagnostics from advice to warning. Use 'style' for all style hints,
    /// or specific kinds like 'style.naming.io'. Useful for enforcing conventions in CI.
    /// Supports hierarchical matching (e.g., 'style' matches 'style.naming.io')
    #[arg(short = 'W', long = "warn", value_name = "KIND")]
    pub warn: Vec<String>,

    /// Require that pcb.toml and pcb.sum are up-to-date. Fails if auto-deps would
    /// add dependencies or if the lockfile would be modified. Recommended for CI.
    #[arg(long)]
    pub locked: bool,
}

/// Print success message with component count for a built schematic
pub fn print_build_success(file_name: &str, schematic: &Schematic) {
    let component_count = schematic
        .instances
        .values()
        .filter(|i| i.kind == pcb_sch::InstanceKind::Component)
        .count();
    eprintln!(
        "{} {} ({} components)",
        pcb_ui::icons::success(),
        file_name.with_style(Style::Green).bold(),
        component_count
    );
}

/// Evaluate a single Starlark file and print any diagnostics
/// Returns the evaluation result and whether there were any errors
#[instrument(name = "build_file", skip_all, fields(file = %zen_path.file_name().unwrap().to_string_lossy()))]
pub fn build(
    zen_path: &Path,
    offline: bool,
    passes: Vec<Box<dyn pcb_zen_core::DiagnosticsPass>>,
    deny_warnings: bool,
    has_errors: &mut bool,
    has_warnings: &mut bool,
    resolution_result: Option<pcb_zen::ResolutionResult>,
) -> Option<Schematic> {
    let file_name = zen_path.file_name().unwrap().to_string_lossy();

    debug!("Compiling Zener file: {}", zen_path.display());
    let spinner = Spinner::builder(format!("{file_name}: Building")).start();

    let eval_result = {
        let _span = info_span!("eval").entered();
        pcb_zen::eval(
            zen_path,
            pcb_zen::EvalConfig::with_resolution(resolution_result, offline),
        )
    };
    let mut diagnostics = eval_result.diagnostics;

    let output = if let Some(eval_output) = eval_result.output {
        let _span = info_span!("electrical_checks").entered();
        for (check, defining_module) in eval_output.collect_electrical_checks() {
            diagnostics
                .diagnostics
                .push(execute_electrical_check(check, defining_module));
        }
        Some(eval_output)
    } else {
        None
    };

    // Convert to schematic and merge diagnostics
    let schematic = output.and_then(|eval_output| {
        let _span = info_span!("to_schematic").entered();
        let schematic_result = eval_output.to_schematic_with_diagnostics();
        diagnostics
            .diagnostics
            .extend(schematic_result.diagnostics.diagnostics);
        schematic_result.output
    });

    if diagnostics.diagnostics.is_empty() && schematic.is_none() {
        spinner.set_message(format!("{file_name}: No output generated"));
    }
    spinner.finish();

    {
        let _span = info_span!("diagnostics_passes").entered();
        diagnostics.apply_passes(&passes);
    }

    // Check if build should fail due to errors OR denied warnings
    // Skip suppressed diagnostics when determining failure
    let has_unsuppressed_warnings = diagnostics
        .diagnostics
        .iter()
        .any(|d| !d.suppressed && matches!(d.severity, starlark::errors::EvalSeverity::Warning));
    let has_unsuppressed_errors = diagnostics
        .diagnostics
        .iter()
        .any(|d| !d.suppressed && matches!(d.severity, starlark::errors::EvalSeverity::Error));
    let should_fail = has_unsuppressed_errors || (deny_warnings && has_unsuppressed_warnings);

    if has_unsuppressed_warnings {
        *has_warnings = true;
    }

    if should_fail {
        *has_errors = true;
        eprintln!(
            "{} {}: Build failed",
            pcb_ui::icons::error(),
            file_name.with_style(Style::Red).bold()
        );
        return None;
    }

    schematic
}

pub fn execute(args: BuildArgs) -> Result<()> {
    let mut has_errors = false;

    // V2 workspace-first architecture: resolve dependencies before finding .zen files
    let (workspace_info, resolution_result) = crate::resolve::resolve_v2_if_needed(
        args.paths.first().map(|p| p.as_path()),
        args.offline,
        args.locked,
    )?;

    // Process .zen files using shared walker - always recursive for directories
    let zen_files = file_walker::collect_workspace_zen_files(&args.paths, &workspace_info)?;

    // Process each .zen file
    let deny_warnings = args.deny.contains(&"warnings".to_string());
    let mut has_warnings = false;
    for zen_path in &zen_files {
        let file_name = zen_path.file_name().unwrap().to_string_lossy();
        let Some(schematic) = build(
            zen_path,
            args.offline,
            create_diagnostics_passes(&args.suppress, &args.warn),
            deny_warnings,
            &mut has_errors,
            &mut has_warnings,
            resolution_result.clone(),
        ) else {
            continue;
        };

        if args.netlist {
            match schematic.to_json() {
                Ok(json) => println!("{json}"),
                Err(e) => {
                    eprintln!("Error serializing netlist to JSON: {e}");
                    has_errors = true;
                }
            }
        } else if args.board_config {
            match pcb_layout::utils::extract_board_config(&schematic) {
                Some(config) => {
                    if let Ok(json) = serde_json::to_string_pretty(&config) {
                        println!("{json}");
                    }
                }
                None => {
                    eprintln!("No board config found in {}", file_name);
                    std::process::exit(1);
                }
            }
        } else {
            print_build_success(&file_name, &schematic);
        }
    }

    if has_errors {
        anyhow::bail!("Build failed with errors");
    }

    Ok(())
}
