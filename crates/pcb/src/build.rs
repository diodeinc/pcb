use anyhow::Result;
use clap::Args;
use log::debug;
use pcb_sch::Schematic;
use pcb_ui::prelude::*;
use std::path::{Path, PathBuf};

use crate::file_walker;

/// Create diagnostics passes for the given deny list
pub fn create_diagnostics_passes(deny: &[String]) -> Vec<Box<dyn pcb_zen_core::DiagnosticsPass>> {
    vec![
        Box::new(pcb_zen_core::FilterHiddenPass),
        Box::new(pcb_zen_core::PromoteDeniedPass::new(deny)),
        Box::new(pcb_zen_core::AggregatePass),
        Box::new(pcb_zen_core::SortPass),
        Box::new(pcb_zen::diagnostics::RenderPass),
    ]
}

#[derive(Args, Debug, Default, Clone)]
#[command(about = "Build PCB projects from .zen files")]
pub struct BuildArgs {
    /// One or more .zen files or directories containing .zen files to build.
    /// When omitted, all .zen files in the current directory tree are built.
    #[arg(value_name = "PATHS", value_hint = clap::ValueHint::AnyPath)]
    pub paths: Vec<PathBuf>,

    /// Print JSON netlist to stdout (undocumented)
    #[arg(long = "netlist", hide = true)]
    pub netlist: bool,

    /// Disable network access (offline mode) - only use vendored dependencies
    #[arg(long = "offline")]
    pub offline: bool,

    /// Set lint level to deny (treat as error). Use 'warnings' for all warnings,
    /// or specific lint names like 'unstable-refs'
    #[arg(short = 'D', long = "deny", value_name = "LINT")]
    pub deny: Vec<String>,
}

/// Evaluate a single Starlark file and print any diagnostics
/// Returns the evaluation result and whether there were any errors
pub fn build(
    zen_path: &Path,
    offline: bool,
    passes: Vec<Box<dyn pcb_zen_core::DiagnosticsPass>>,
    has_errors: &mut bool,
) -> Option<Schematic> {
    let file_name = zen_path.file_name().unwrap().to_string_lossy();

    // Show spinner while building
    debug!("Compiling Zener file: {}", zen_path.display());
    let spinner = Spinner::builder(format!("{file_name}: Building")).start();

    // Evaluate the design
    let eval_config = pcb_zen::EvalConfig {
        offline,
        ..Default::default()
    };
    let eval = pcb_zen::run(zen_path, eval_config);

    // Finish spinner before printing diagnostics
    if eval.is_empty() {
        spinner.set_message(format!("{file_name}: No output generated"));
    }
    spinner.finish();

    // Apply all passes including rendering
    let mut diagnostics = eval.diagnostics.clone();
    diagnostics.apply_passes(&passes);

    // Check for errors
    if diagnostics.has_errors() {
        *has_errors = true;
    }

    eval.output_result()
        .inspect_err(|_| {
            eprintln!(
                "{} {}: Build failed",
                pcb_ui::icons::error(),
                file_name.with_style(Style::Red).bold()
            );
        })
        .inspect_err(|diagnostics| {
            if diagnostics.has_errors() {
                *has_errors = true;
            }
        })
        .ok()
}

pub fn execute(args: BuildArgs) -> Result<()> {
    let mut has_errors = false;

    // Process .zen files using shared walker - always recursive for directories
    let zen_files = file_walker::collect_zen_files(&args.paths, false)?;

    if zen_files.is_empty() {
        let cwd = std::env::current_dir()?;
        anyhow::bail!(
            "No .zen source files found in {}",
            cwd.canonicalize().unwrap_or(cwd).display()
        );
    }

    // Process each .zen file
    for zen_path in &zen_files {
        let file_name = zen_path.file_name().unwrap().to_string_lossy();
        let Some(schematic) = build(
            zen_path,
            args.offline,
            create_diagnostics_passes(&args.deny),
            &mut has_errors,
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
        } else {
            // Print success with component count
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
    }

    if has_errors {
        anyhow::bail!("Build failed with errors");
    }

    Ok(())
}
