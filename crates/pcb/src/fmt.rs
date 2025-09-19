use anyhow::Result;
use clap::Args;
use ignore::WalkBuilder;
use log::debug;
use pcb_fmt::RuffFormatter;
use pcb_ui::prelude::*;
use pcb_zen::file_extensions;
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

/// Process .zen files using ignore crate for efficient traversal
fn process_files(
    formatter: &RuffFormatter,
    paths: &[PathBuf],
    args: &FmtArgs,
) -> Result<(bool, Vec<PathBuf>)> {
    let mut all_formatted = true;
    let mut files_needing_format = Vec::new();

    // Determine root paths to walk
    let walk_paths = if paths.is_empty() {
        vec![std::env::current_dir()?]
    } else {
        paths.to_vec()
    };

    let mut found_files = false;

    for root in walk_paths {
        let mut builder = WalkBuilder::new(&root);

        // Configure the walker
        builder
            .hidden(!args.hidden)
            .git_ignore(true)
            .git_exclude(true)
            .git_global(true)
            // Explicitly add vendor/ to ignored patterns
            .add_custom_ignore_filename(".pcbignore")
            .filter_entry(|entry| {
                // Skip vendor directories
                if entry.file_type().is_some_and(|ft| ft.is_dir()) {
                    if let Some(name) = entry.file_name().to_str() {
                        if name == "vendor" {
                            return false;
                        }
                    }
                }
                true
            });

        for result in builder.build() {
            let entry = result?;
            let path = entry.path();

            // Only process .zen files
            if path.is_file() && file_extensions::is_starlark_file(path.extension()) {
                found_files = true;
                let file_name = path.file_name().unwrap().to_string_lossy();

                // Show spinner while processing
                let spinner = if args.check {
                    Spinner::builder(format!("{file_name}: Checking format")).start()
                } else if args.diff {
                    Spinner::builder(format!("{file_name}: Checking diff")).start()
                } else {
                    Spinner::builder(format!("{file_name}: Formatting")).start()
                };

                match format_file(formatter, path, args) {
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
                                files_needing_format.push(path.to_path_buf());
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
        }
    }

    if !found_files {
        let root_display = if paths.is_empty() {
            let cwd = std::env::current_dir()?;
            cwd.canonicalize().unwrap_or(cwd).display().to_string()
        } else {
            paths
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        };
        anyhow::bail!("No .zen files found in {}", root_display);
    }

    Ok((all_formatted, files_needing_format))
}

pub fn execute(args: FmtArgs) -> Result<()> {
    // Create a ruff formatter instance
    let formatter = RuffFormatter::default();

    // Print version info in debug mode
    debug!("Using ruff formatter");

    // Process files with streaming approach
    let (all_formatted, files_needing_format) = process_files(&formatter, &args.paths, &args)?;

    // Handle check mode results
    if args.check && !all_formatted {
        eprintln!("\n{} files need formatting.", files_needing_format.len());
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
