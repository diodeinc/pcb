use anyhow::Result;
use clap::Args;
use log::debug;
use pcb_sch::Schematic;
use pcb_ui::prelude::*;
use pcb_zen::file_extensions;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

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
    /// One or more .zen files or directories containing .zen files (non-recursive) to build.
    /// When omitted, all .zen files in the current directory are built.
    #[arg(value_name = "PATHS", value_hint = clap::ValueHint::AnyPath)]
    pub paths: Vec<PathBuf>,

    /// Print JSON netlist to stdout (undocumented)
    #[arg(long = "netlist", hide = true)]
    pub netlist: bool,

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
    let eval = pcb_zen::run(zen_path, offline, pcb_zen::EvalMode::Build);

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
    // Determine which .zen files to compile
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

    let mut has_errors = false;

    // Process each .zen file
    for zen_path in zen_paths {
        let file_name = zen_path.file_name().unwrap().to_string_lossy();
        let Some(schematic) = build(
            &zen_path,
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

/// Collect .zen files from the provided paths
pub fn collect_files(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut unique: HashSet<PathBuf> = HashSet::new();

    if !paths.is_empty() {
        // Collect .zen files from the provided paths (non-recursive)
        for user_path in paths {
            // Resolve path relative to current directory if not absolute
            let resolved = if user_path.is_absolute() {
                user_path.clone()
            } else {
                std::env::current_dir()?.join(user_path)
            };

            if resolved.is_file() {
                if file_extensions::is_starlark_file(resolved.extension()) {
                    unique.insert(resolved);
                }
            } else if resolved.is_dir() {
                // Iterate over files in the directory (non-recursive)
                for entry in fs::read_dir(resolved)?.flatten() {
                    let path = entry.path();
                    if path.is_file() && file_extensions::is_starlark_file(path.extension()) {
                        unique.insert(path);
                    }
                }
            }
        }
    } else {
        // Fallback: find all `.zen` files in the current directory (non-recursive)
        let cwd = std::env::current_dir()?;
        for entry in fs::read_dir(cwd)?.flatten() {
            let path = entry.path();
            if path.is_file() && file_extensions::is_starlark_file(path.extension()) {
                unique.insert(path);
            }
        }
    }

    // Convert to vec and keep deterministic ordering
    let mut paths_vec: Vec<_> = unique.into_iter().collect();
    paths_vec.sort();
    Ok(paths_vec)
}

/// Recursively collect Starlark source files (.zen/.star) from the provided paths.
/// Mirrors `collect_files` semantics but with recursive directory traversal.
pub fn collect_files_recursive(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut unique: HashSet<PathBuf> = HashSet::new();

    if !paths.is_empty() {
        for user_path in paths {
            let resolved = if user_path.is_absolute() {
                user_path.clone()
            } else {
                std::env::current_dir()?.join(user_path)
            };

            if resolved.is_file() {
                if file_extensions::is_starlark_file(resolved.extension()) {
                    unique.insert(resolved);
                }
            } else if resolved.is_dir() {
                visit_dir_recursive(&resolved, &mut unique)?;
            }
        }
    } else {
        let cwd = std::env::current_dir()?;
        visit_dir_recursive(&cwd, &mut unique)?;
    }

    let mut paths_vec: Vec<_> = unique.into_iter().collect();
    paths_vec.sort();
    Ok(paths_vec)
}

fn visit_dir_recursive(dir: &Path, out: &mut HashSet<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir)?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            visit_dir_recursive(&path, out)?;
        } else if path.is_file() && file_extensions::is_starlark_file(path.extension()) {
            out.insert(path);
        }
    }
    Ok(())
}
