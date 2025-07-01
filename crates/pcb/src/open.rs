use anyhow::{Context, Result};
use clap::Args;
use inquire::Select;
use pcb_layout::utils;
use std::path::{Path, PathBuf};

use crate::build::{collect_star_files, evaluate_star_file};

#[derive(Args, Debug)]
pub struct OpenArgs {
    /// One or more .star files to build/open. When omitted, behaves like before.
    #[arg(value_name = "STAR", value_hint = clap::ValueHint::FilePath)]
    paths: Vec<PathBuf>,
}

pub fn execute(args: OpenArgs) -> Result<()> {
    open_layout(args.paths)
}

fn open_layout(star_paths: Vec<PathBuf>) -> Result<()> {
    // Collect .star files to process
    let star_paths = collect_star_files(&star_paths)?;

    if star_paths.is_empty() {
        // Try to find a layout file in the current directory
        let cwd = std::env::current_dir()?;
        let layout_files: Vec<_> = std::fs::read_dir(&cwd)?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "kicad_pcb"))
            .collect();

        if layout_files.is_empty() {
            anyhow::bail!(
                "No .star source files or .kicad_pcb layout files found in {}",
                cwd.canonicalize().unwrap_or(cwd).display()
            );
        }

        // If there's only one layout file, open it
        if layout_files.len() == 1 {
            open::that(&layout_files[0])
                .with_context(|| format!("Failed to open file: {}", layout_files[0].display()))?;
            return Ok(());
        }

        // Multiple layout files, let user choose
        let selected = choose_layout_file(&layout_files)?;
        open::that(selected)
            .with_context(|| format!("Failed to open file: {}", selected.display()))?;
        return Ok(());
    }

    let mut available_layouts = Vec::new();

    // Process each .star file to find available layouts
    for star_path in star_paths {
        let file_name = star_path.file_name().unwrap().to_string_lossy();

        // Evaluate the star file
        let (eval_result, has_errors) = evaluate_star_file(&star_path);

        if has_errors {
            eprintln!("Skipping {file_name} due to build errors");
            continue;
        }

        // Check if the schematic has a layout
        if let Some(schematic) = &eval_result.output {
            if let Some(layout_path_attr) = utils::extract_layout_path(schematic) {
                // Convert relative path to absolute based on star file location
                let layout_dir = if layout_path_attr.is_relative() {
                    star_path
                        .parent()
                        .unwrap_or(Path::new("."))
                        .join(&layout_path_attr)
                } else {
                    layout_path_attr
                };

                let layout_path = utils::get_layout_paths(&layout_dir).pcb;
                if layout_path.exists() {
                    available_layouts.push((star_path.clone(), layout_path));
                }
            }
        }
    }

    if available_layouts.is_empty() {
        anyhow::bail!("No layout files found. Run 'pcb layout' to generate layouts first.");
    }

    // Open the selected layout
    let layout_to_open = if available_layouts.len() == 1 {
        // Only one layout - open it directly
        &available_layouts[0].1
    } else {
        // Multiple layouts - let user choose
        let selected_idx = choose_star_layout(&available_layouts)?;
        &available_layouts[selected_idx].1
    };

    open::that(layout_to_open)
        .with_context(|| format!("Failed to open file: {}", layout_to_open.display()))?;

    Ok(())
}

/// Let the user choose which layout to open from star file associations
fn choose_star_layout(layouts: &[(PathBuf, PathBuf)]) -> Result<usize> {
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

/// Let the user choose which layout file to open
fn choose_layout_file(layout_files: &[PathBuf]) -> Result<&PathBuf> {
    // Get current directory for making relative paths
    let cwd = std::env::current_dir()?;

    let options: Vec<String> = layout_files
        .iter()
        .map(|path| {
            // Try to make the path relative to current directory
            path.strip_prefix(&cwd)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| path.display().to_string())
        })
        .collect();

    let selection = Select::new("Select a layout to open:", options.clone())
        .prompt()
        .context("Failed to get user selection")?;

    // Find which file was selected
    let selected_index = options
        .iter()
        .position(|option| option == &selection)
        .ok_or_else(|| anyhow::anyhow!("Invalid selection"))?;

    Ok(&layout_files[selected_index])
}
