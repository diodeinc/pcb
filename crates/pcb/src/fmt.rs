use anyhow::Result;
use clap::Args;
use log::debug;
use pcb_ui::prelude::*;
use pcb_zen::file_extensions;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Args, Debug, Default, Clone)]
#[command(about = "Format .zen and .star files using buildifier")]
pub struct FmtArgs {
    /// One or more .zen/.star files or directories containing such files (non-recursive) to format.
    /// When omitted, all .zen/.star files in the current directory are formatted.
    #[arg(value_name = "PATHS", value_hint = clap::ValueHint::AnyPath)]
    pub paths: Vec<PathBuf>,

    /// Check if files are formatted correctly without modifying them.
    /// Exit with non-zero code if any file needs formatting.
    #[arg(long)]
    pub check: bool,

    /// Show diffs instead of writing files
    #[arg(long)]
    pub diff: bool,
}

/// Check if buildifier is available in the system PATH
fn check_buildifier_available() -> Result<()> {
    match Command::new("buildifier")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(status) if status.success() => Ok(()),
        _ => Err(anyhow::anyhow!(
            "buildifier is not installed or not available in PATH.\n\
            Please install buildifier to use the fmt command.\n\
            \n\
            Installation options:\n\
            - Go: go install github.com/bazelbuild/buildtools/buildifier@latest\n\
            - Homebrew: brew install buildifier\n\
            - Download from: https://github.com/bazelbuild/buildtools/releases"
        )),
    }
}

/// Format a single file using buildifier
fn format_file(file_path: &Path, args: &FmtArgs) -> Result<bool> {
    debug!("Formatting file: {}", file_path.display());

    let mut cmd = Command::new("buildifier");

    // Set the appropriate mode based on arguments
    if args.check {
        cmd.arg("--mode=check");
    } else if args.diff {
        cmd.arg("--mode=diff");
    } else {
        cmd.arg("--mode=fix");
    }

    // Add the file path
    cmd.arg(file_path);

    // Execute the command
    let output = cmd.output()?;

    // Handle different exit codes
    match output.status.code() {
        Some(0) => {
            // File was already formatted or successfully formatted
            if args.diff && !output.stdout.is_empty() {
                // Print diff output
                print!("{}", String::from_utf8_lossy(&output.stdout));
            }
            Ok(true)
        }
        Some(1) => {
            // File needs formatting (in check mode) or had formatting applied
            if args.check {
                // In check mode, this means file needs formatting
                Ok(false)
            } else {
                // In format mode, this should not happen if file was successfully formatted
                Ok(true)
            }
        }
        Some(4) => {
            // Exit code 4 means file needs reformatting (common in check mode)
            if args.check {
                Ok(false)
            } else if args.diff {
                // In diff mode, print the output and consider it successful
                if !output.stdout.is_empty() {
                    print!("{}", String::from_utf8_lossy(&output.stdout));
                }
                Ok(true)
            } else {
                // In format mode, this shouldn't happen if formatting was successful
                Ok(true)
            }
        }
        Some(2) => {
            // Syntax error or other buildifier error
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(anyhow::anyhow!(
                "Buildifier error for {}: {}",
                file_path.display(),
                stderr
            ))
        }
        Some(code) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(anyhow::anyhow!(
                "Buildifier exited with code {} for {}: {}",
                code,
                file_path.display(),
                stderr
            ))
        }
        None => Err(anyhow::anyhow!(
            "Buildifier was terminated by signal for {}",
            file_path.display()
        )),
    }
}

/// Collect .zen and .star files from the provided paths
pub fn collect_files(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut unique: HashSet<PathBuf> = HashSet::new();

    if !paths.is_empty() {
        // Collect files from the provided paths (non-recursive)
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
        // Fallback: find all Starlark files in the current directory (non-recursive)
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

pub fn execute(args: FmtArgs) -> Result<()> {
    // Check if buildifier is available
    check_buildifier_available()?;

    // Determine which files to format
    let starlark_paths = collect_files(&args.paths)?;

    if starlark_paths.is_empty() {
        let cwd = std::env::current_dir()?;
        anyhow::bail!(
            "No .zen or .star files found in {}",
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

        match format_file(&file_path, &args) {
            Ok(is_formatted) => {
                spinner.finish();

                if args.check {
                    if is_formatted {
                        println!(
                            "{} {}",
                            pcb_ui::icons::success(),
                            file_name.with_style(Style::Green).bold()
                        );
                    } else {
                        println!(
                            "{} {} (needs formatting)",
                            pcb_ui::icons::warning(),
                            file_name.with_style(Style::Yellow).bold()
                        );
                        all_formatted = false;
                        files_needing_format.push(file_path.clone());
                    }
                } else if args.diff {
                    println!(
                        "{} {}",
                        pcb_ui::icons::success(),
                        file_name.with_style(Style::Green).bold()
                    );
                } else {
                    println!(
                        "{} {}",
                        pcb_ui::icons::success(),
                        file_name.with_style(Style::Green).bold()
                    );
                }
            }
            Err(e) => {
                spinner.error(format!("{file_name}: Format failed"));
                eprintln!("Error: {}", e);
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
