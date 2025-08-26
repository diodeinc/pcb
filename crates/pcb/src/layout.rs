use anyhow::{Context, Result};
use clap::Args;
use inquire::Select;
use pcb_layout::{process_layout, LayoutError};
use pcb_ui::prelude::*;
use std::path::PathBuf;

use crate::build::{build, collect_files, collect_files_recursive, create_diagnostics_passes};

#[derive(Args, Debug, Default, Clone)]
#[command(about = "Generate PCB layout files from .zen files")]
pub struct LayoutArgs {
    #[arg(long, help = "Skip opening the layout file after generation")]
    pub no_open: bool,

    #[arg(
        short = 's',
        long,
        help = "Always prompt to choose a layout even when only one"
    )]
    pub select: bool,

    /// One or more .zen files to process for layout generation.
    /// When omitted, all .zen files in the current directory are processed.
    #[arg(value_name = "PATHS", value_hint = clap::ValueHint::AnyPath)]
    pub paths: Vec<PathBuf>,

    /// Recursively traverse directories to find .zen/.star files
    #[arg(short = 'r', long = "recursive", default_value_t = false)]
    pub recursive: bool,

    /// Disable network access (offline mode) - only use vendored dependencies
    #[arg(long = "offline")]
    pub offline: bool,
}

pub fn execute(args: LayoutArgs) -> Result<()> {
    // Collect .zen files to process
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
    let mut generated_layouts = Vec::new();

    // Process each .zen file
    for zen_path in zen_paths {
        let file_name = zen_path.file_name().unwrap().to_string_lossy();
        let Some(schematic) = build(
            &zen_path,
            args.offline,
            create_diagnostics_passes(&[]),
            &mut has_errors,
        ) else {
            continue;
        };

        // Layout stage
        let spinner = Spinner::builder(format!("{file_name}: Generating layout")).start();

        // Check if the schematic has a layout
        match process_layout(&schematic, &zen_path) {
            Ok(layout_result) => {
                spinner.finish();
                // Print success with the layout path relative to the star file
                let relative_path = zen_path
                    .parent()
                    .and_then(|parent| layout_result.pcb_file.strip_prefix(parent).ok())
                    .unwrap_or(&layout_result.pcb_file);
                println!(
                    "{} {} ({})",
                    pcb_ui::icons::success(),
                    file_name.with_style(Style::Green).bold(),
                    relative_path.display()
                );
                generated_layouts.push((zen_path.clone(), layout_result.pcb_file.clone()));
            }
            Err(LayoutError::NoLayoutPath) => {
                spinner.finish();
                // Show warning for files without layout
                println!(
                    "{} {} (no layout)",
                    pcb_ui::icons::warning(),
                    file_name.with_style(Style::Yellow).bold(),
                );
                continue;
            }
            Err(e) => {
                // Finish the spinner first to avoid visual overlap
                spinner.finish();
                // Now print the error message
                println!(
                    "{} {}: Layout generation failed",
                    pcb_ui::icons::error(),
                    file_name.with_style(Style::Red).bold()
                );
                eprintln!("  Error: {e}");
                has_errors = true;
            }
        }
    }

    if has_errors {
        anyhow::bail!("Layout generation failed with errors");
    }

    if generated_layouts.is_empty() {
        println!("\nNo layouts found.");
        return Ok(());
    }

    // Open the selected layout if not disabled
    if !args.no_open && !generated_layouts.is_empty() {
        let layout_to_open = if generated_layouts.len() == 1 && !args.select {
            // Only one layout and not forcing selection - open it directly
            &generated_layouts[0].1
        } else {
            // Multiple layouts or forced selection - let user choose
            let selected_idx = choose_layout(&generated_layouts)?;
            &generated_layouts[selected_idx].1
        };

        open::that(layout_to_open)?;
    }

    Ok(())
}

/// Let the user choose which layout to open
fn choose_layout(layouts: &[(PathBuf, PathBuf)]) -> Result<usize> {
    // Get current directory for making relative paths
    let cwd = std::env::current_dir()?;

    let options: Vec<String> = layouts
        .iter()
        .map(|(star_file, _)| {
            // Try to make the path relative to current directory
            star_file
                .strip_prefix(&cwd)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| star_file.display().to_string())
        })
        .collect();

    let selection = Select::new("Select a layout to open:", options.clone())
        .prompt()
        .context("Failed to get user selection")?;

    // Find which index was selected
    options
        .iter()
        .position(|option| option == &selection)
        .ok_or_else(|| anyhow::anyhow!("Invalid selection"))
}
