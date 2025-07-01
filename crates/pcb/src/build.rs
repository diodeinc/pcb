use anyhow::Result;
use clap::Args;
use log::debug;
use pcb_star::EvalSeverity;
use pcb_ui::prelude::*;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Args, Debug, Default, Clone)]
#[command(about = "Build PCB projects from .star files")]
pub struct BuildArgs {
    /// One or more .star files or directories containing .star files (non-recursive) to build.
    /// When omitted, all .star files in the current directory are built.
    #[arg(value_name = "PATHS", value_hint = clap::ValueHint::AnyPath)]
    pub paths: Vec<PathBuf>,
}

/// Evaluate a single Starlark file and print any diagnostics
/// Returns the evaluation result and whether there were any errors
pub fn evaluate_star_file(path: &Path) -> (pcb_star::WithDiagnostics<pcb_sch::Schematic>, bool) {
    debug!("Compiling Starlark file: {}", path.display());

    // Evaluate the design
    let eval_result = pcb_star::run(path);
    let mut has_errors = false;

    // Print diagnostics
    for diag in eval_result.diagnostics.iter() {
        pcb_star::render_diagnostic(diag);
        eprintln!();

        if matches!(diag.severity, EvalSeverity::Error) {
            has_errors = true;
        }
    }

    (eval_result, has_errors)
}

pub fn execute(args: BuildArgs) -> Result<()> {
    // Determine which .star files to compile
    let star_paths = collect_star_files(&args.paths)?;

    if star_paths.is_empty() {
        let cwd = std::env::current_dir()?;
        anyhow::bail!(
            "No .star source files found in {}",
            cwd.canonicalize().unwrap_or(cwd).display()
        );
    }

    let mut has_errors = false;

    // Process each .star file
    for star_path in star_paths {
        let file_name = star_path.file_name().unwrap().to_string_lossy();

        // Show spinner while building
        let spinner = Spinner::builder(format!("{file_name}: Building")).start();

        // Evaluate the design
        let eval_result = pcb_star::run(&star_path);

        // Check if we have diagnostics to print
        if !eval_result.diagnostics.is_empty() {
            // Finish spinner before printing diagnostics
            spinner.finish();

            // Now print diagnostics
            let mut file_has_errors = false;
            for diag in eval_result.diagnostics.iter() {
                pcb_star::render_diagnostic(diag);
                eprintln!();

                if matches!(diag.severity, EvalSeverity::Error) {
                    file_has_errors = true;
                }
            }

            if file_has_errors {
                println!(
                    "{} {}: Build failed",
                    pcb_ui::icons::error(),
                    file_name.with_style(Style::Red).bold()
                );
                has_errors = true;
            }
        } else if let Some(schematic) = &eval_result.output {
            spinner.finish();

            // Print success with component count
            let component_count = schematic
                .instances
                .values()
                .filter(|i| i.kind == pcb_sch::InstanceKind::Component)
                .count();
            println!(
                "{} {} ({} components)",
                pcb_ui::icons::success(),
                file_name.with_style(Style::Green).bold(),
                component_count
            );
        } else {
            spinner.error(format!("{file_name}: No output generated"));
            has_errors = true;
        }
    }

    if has_errors {
        anyhow::bail!("Build failed with errors");
    }

    Ok(())
}

/// Collect .star files from the provided paths
pub fn collect_star_files(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut unique: HashSet<PathBuf> = HashSet::new();

    if !paths.is_empty() {
        // Collect .star files from the provided paths (non-recursive)
        for user_path in paths {
            // Resolve path relative to current directory if not absolute
            let resolved = if user_path.is_absolute() {
                user_path.clone()
            } else {
                std::env::current_dir()?.join(user_path)
            };

            if resolved.is_file() {
                if resolved
                    .extension()
                    .map(|ext| ext == "star")
                    .unwrap_or(false)
                {
                    unique.insert(resolved);
                }
            } else if resolved.is_dir() {
                // Iterate over files in the directory (non-recursive)
                for entry in fs::read_dir(resolved)?.flatten() {
                    let path = entry.path();
                    if path.is_file() && path.extension().map(|ext| ext == "star").unwrap_or(false)
                    {
                        unique.insert(path);
                    }
                }
            }
        }
    } else {
        // Fallback: find all `.star` files in the current directory (non-recursive)
        let cwd = std::env::current_dir()?;
        for entry in fs::read_dir(cwd)?.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().map(|ext| ext == "star").unwrap_or(false) {
                unique.insert(path);
            }
        }
    }

    // Convert to vec and keep deterministic ordering
    let mut paths_vec: Vec<_> = unique.into_iter().collect();
    paths_vec.sort();
    Ok(paths_vec)
}
