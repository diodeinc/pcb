use anyhow::{Context, Result};
use clap::Args;
use inquire::Select;
use pcb_layout::{process_layout, LayoutError};
use pcb_ui::prelude::*;
use std::path::PathBuf;

use crate::build::collect_star_files;

#[derive(Args, Debug, Default, Clone)]
#[command(about = "Generate PCB layout files from .star files")]
pub struct LayoutArgs {
    #[arg(long, help = "Skip opening the layout file after generation")]
    pub no_open: bool,

    #[arg(
        short = 's',
        long,
        help = "Always prompt to choose a layout even when only one"
    )]
    pub select: bool,

    /// One or more .star files to process for layout generation.
    /// When omitted, all .star files in the current directory are processed.
    #[arg(value_name = "PATHS", value_hint = clap::ValueHint::AnyPath)]
    pub paths: Vec<PathBuf>,
}

pub fn execute(args: LayoutArgs) -> Result<()> {
    // Collect .star files to process
    let star_paths = collect_star_files(&args.paths)?;

    if star_paths.is_empty() {
        let cwd = std::env::current_dir()?;
        anyhow::bail!(
            "No .star source files found in {}",
            cwd.canonicalize().unwrap_or(cwd).display()
        );
    }

    let mut has_errors = false;
    let mut generated_layouts = Vec::new();

    // Process each .star file
    for star_path in star_paths {
        let file_name = star_path.file_name().unwrap().to_string_lossy();

        // Building stage
        let mut spinner = Spinner::builder(format!("{file_name}: Building")).start();

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

                if matches!(diag.severity, pcb_star::EvalSeverity::Error) {
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
                continue;
            }

            // Restart spinner for layout stage after diagnostics
            spinner = Spinner::builder(format!("{file_name}: Generating layout")).start();
        } else {
            // No diagnostics - just update the spinner message
            spinner.set_message(format!("{file_name}: Generating layout"));
        }

        // Check if the schematic has a layout
        if let Some(schematic) = &eval_result.output {
            match process_layout(schematic, &star_path) {
                Ok(layout_result) => {
                    spinner.finish();
                    // Print success with the layout path relative to the star file
                    let relative_path = star_path
                        .parent()
                        .and_then(|parent| layout_result.pcb_file.strip_prefix(parent).ok())
                        .unwrap_or(&layout_result.pcb_file);
                    println!(
                        "{} {} ({})",
                        pcb_ui::icons::success(),
                        file_name.with_style(Style::Green).bold(),
                        relative_path.display()
                    );
                    generated_layouts.push((star_path.clone(), layout_result.pcb_file.clone()));
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
        } else {
            spinner.finish();
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
