use anyhow::Result;
use clap::Args;
use log::debug;
use pcb_fmt::RuffFormatter;
use pcb_ui::prelude::*;
use pcb_zen::file_extensions;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Args, Debug, Default, Clone)]
#[command(about = "Format .zen files")]
pub struct FmtArgs {
    /// One or more .zen files or directories containing .zen files to format.
    /// When omitted, all .zen files in the current directory tree are formatted.
    #[arg(value_name = "PATHS", value_hint = clap::ValueHint::AnyPath)]
    pub paths: Vec<PathBuf>,

    /// Check if files are formatted correctly without modifying them.
    /// Exit with non-zero code if any file needs formatting.
    #[arg(long)]
    pub check: bool,

    /// Show diffs instead of writing files
    #[arg(long)]
    pub diff: bool,

    /// Include hidden files and directories
    #[arg(long)]
    pub hidden: bool,
}

/// Format a single file using ruff formatter
fn format_file(formatter: &RuffFormatter, file_path: &Path, args: &FmtArgs) -> Result<bool> {
    debug!("Formatting file: {}", file_path.display());

    if args.check {
        formatter.check_file(file_path)
    } else if args.diff {
        let diff = formatter.diff_file(file_path)?;
        if !diff.is_empty() {
            print!("{diff}");
        }
        Ok(true)
    } else {
        formatter.format_file(file_path)?;
        Ok(true)
    }
}

/// Recursively collect .zen files from a directory
fn collect_files_recursive(dir: &Path, files: &mut HashSet<PathBuf>, hidden: bool) -> Result<()> {
    for entry in fs::read_dir(dir)?.flatten() {
        let path = entry.path();
        if !hidden
            && path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .starts_with('.')
        {
            continue;
        }
        if path.is_file() && file_extensions::is_starlark_file(path.extension()) {
            files.insert(path);
        } else if path.is_dir() {
            // Recursively traverse subdirectories
            collect_files_recursive(&path, files, hidden)?;
        }
    }
    Ok(())
}

/// Collect .zen files from the provided paths
pub fn collect_files(paths: &[PathBuf], hidden: bool) -> Result<Vec<PathBuf>> {
    let mut unique: HashSet<PathBuf> = HashSet::new();

    if !paths.is_empty() {
        // Collect files from the provided paths (recursive for directories)
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
                // Recursively collect files from the directory
                collect_files_recursive(&resolved, &mut unique, hidden)?;
            }
        }
    } else {
        // Fallback: find all Starlark files in the current directory tree (recursive)
        let cwd = std::env::current_dir()?;
        collect_files_recursive(&cwd, &mut unique, hidden)?;
    }

    // Convert to vec and keep deterministic ordering
    let mut paths_vec: Vec<_> = unique.into_iter().collect();
    paths_vec.sort();
    Ok(paths_vec)
}

pub fn execute(args: FmtArgs) -> Result<()> {
    // Create a ruff formatter instance
    let formatter = RuffFormatter::default();

    // Print version info in debug mode
    debug!("Using ruff formatter");

    // Determine which files to format
    let starlark_paths = collect_files(&args.paths, args.hidden)?;

    if starlark_paths.is_empty() {
        let cwd = std::env::current_dir()?;
        anyhow::bail!(
            "No .zen files found in {}",
            cwd.canonicalize().unwrap_or(cwd).display()
        );
    }

    let mut all_formatted = true;
    let mut files_needing_format = Vec::new();

    // Process each file
    for file_path in starlark_paths {
        let file_name = file_path.file_name().unwrap().to_string_lossy();

        // Show spinner while processing
        let spinner = if args.check {
            Spinner::builder(format!("{file_name}: Checking format")).start()
        } else if args.diff {
            Spinner::builder(format!("{file_name}: Checking diff")).start()
        } else {
            Spinner::builder(format!("{file_name}: Formatting")).start()
        };

        match format_file(&formatter, &file_path, &args) {
            Ok(is_formatted) => {
                spinner.finish();

                if args.check {
                    if is_formatted {
                        println!(
                            "{} {} (needs formatting)",
                            pcb_ui::icons::warning(),
                            file_name.with_style(Style::Yellow).bold()
                        );
                        all_formatted = false;
                        files_needing_format.push(file_path.clone());
                    } else {
                        println!(
                            "{} {}",
                            pcb_ui::icons::success(),
                            file_name.with_style(Style::Green).bold()
                        );
                    }
                } else {
                    // For both diff mode and regular format mode, show success
                    println!(
                        "{} {}",
                        pcb_ui::icons::success(),
                        file_name.with_style(Style::Green).bold()
                    );
                }
            }
            Err(e) => {
                spinner.error(format!("{file_name}: Format failed"));
                eprintln!("Error: {e}");
                all_formatted = false;
            }
        }
    }

    // Handle check mode results
    if args.check && !all_formatted {
        eprintln!("\n{} files need formatting:", files_needing_format.len());
        for file in &files_needing_format {
            eprintln!("  {}", file.display());
        }
        eprintln!(
            "\nRun 'pcb fmt {}' to format these files.",
            files_needing_format
                .iter()
                .map(|p| p.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" ")
        );

        anyhow::bail!("Some files are not formatted correctly");
    }

    Ok(())
}
