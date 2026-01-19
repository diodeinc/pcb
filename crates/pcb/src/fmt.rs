use anyhow::Result;
use clap::Args;
use log::debug;
use pcb_fmt::RuffFormatter;
use pcb_ui::prelude::*;
use std::path::{Path, PathBuf};

use crate::file_walker;

#[derive(Args, Debug, Default, Clone)]
#[command(about = "Format .zen files")]
pub struct FmtArgs {
    /// .zen file or directory to format. Defaults to current directory.
    #[arg(value_name = "PATH", value_hint = clap::ValueHint::AnyPath)]
    pub path: Option<PathBuf>,

    /// Check if files are formatted correctly without modifying them.
    /// Exit with non-zero code if any file needs formatting.
    #[arg(long)]
    pub check: bool,

    /// Show diffs instead of writing files
    #[arg(long)]
    pub diff: bool,
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

/// Process .zen files using shared file walker
fn process_files(
    formatter: &RuffFormatter,
    paths: &[PathBuf],
    args: &FmtArgs,
) -> Result<(bool, Vec<PathBuf>)> {
    let mut all_formatted = true;
    let mut files_needing_format = Vec::new();

    let zen_files = file_walker::collect_zen_files(paths)?;

    if zen_files.is_empty() {
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

    for path in &zen_files {
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
                        files_needing_format.push(path.clone());
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

    Ok((all_formatted, files_needing_format))
}

pub fn execute(args: FmtArgs) -> Result<()> {
    // Create a ruff formatter instance
    let formatter = RuffFormatter::default();

    // Print version info in debug mode
    debug!("Using ruff formatter");

    // Process files with streaming approach
    let paths: Vec<PathBuf> = args.path.clone().into_iter().collect();
    let (all_formatted, files_needing_format) = process_files(&formatter, &paths, &args)?;

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
