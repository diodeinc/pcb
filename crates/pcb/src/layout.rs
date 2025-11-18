use anyhow::{Context, Result};
use clap::Args;
use inquire::Select;
use pcb_layout::{process_layout, LayoutError};
use pcb_ui::prelude::*;
use std::path::{Path, PathBuf};

use crate::build::{build, create_diagnostics_passes};
use crate::drc;
use crate::file_walker;
use pcb_sch::Schematic;

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
    /// When omitted, all .zen files in the current directory tree are processed.
    #[arg(value_name = "PATHS", value_hint = clap::ValueHint::AnyPath)]
    pub paths: Vec<PathBuf>,

    /// Disable network access (offline mode) - only use vendored dependencies
    #[arg(long = "offline")]
    pub offline: bool,

    /// Apply board config (default: true)
    #[arg(
        long = "sync-board-config",
        action = clap::ArgAction::Set,
        default_value_t = true,
        value_parser = clap::builder::BoolishValueParser::new(),
        num_args = 0..=1,
        default_missing_value = "true"
    )]
    pub sync_board_config: bool,

    /// Generate layout in a temporary directory (fresh layout, opens KiCad)
    #[arg(long = "temp")]
    pub temp: bool,

    /// Check layout sync status and run DRC without modifying files
    #[arg(long = "check")]
    pub check: bool,

    /// Suppress specific diagnostic kinds (e.g., layout.drc.clearance, layout.sync.footprint_removed)
    #[arg(short = 'S', long = "suppress", value_name = "KIND")]
    pub suppress: Vec<String>,
}

pub fn execute(args: LayoutArgs) -> Result<()> {
    // Check mode implies no-open
    let no_open = args.no_open || args.check;

    // Collect .zen files to process - always recursive for directories
    let zen_paths = file_walker::collect_zen_files(&args.paths, false)?;

    if zen_paths.is_empty() {
        let cwd = std::env::current_dir()?;
        anyhow::bail!(
            "No .zen source files found in {}",
            cwd.canonicalize().unwrap_or(cwd).display()
        );
    }

    let mut has_errors = false;
    let mut has_warnings = false;
    let mut generated_layouts = Vec::new();

    // Process each .zen file
    for zen_path in zen_paths {
        let file_name = zen_path.file_name().unwrap().to_string_lossy().to_string();
        let Some(schematic) = build(
            &zen_path,
            args.offline,
            create_diagnostics_passes(&[]),
            false, // don't deny warnings for layout command
            &mut has_errors,
            &mut has_warnings,
        ) else {
            continue;
        };

        if args.check {
            // Check mode: Run combined layout sync + DRC checks
            let spinner = Spinner::builder(format!("{file_name}: Checking layout")).start();

            // Get layout directory from schematic
            let Some(layout_path) = pcb_layout::utils::extract_layout_path(&schematic) else {
                spinner.finish();
                println!(
                    "{} {} (no layout)",
                    pcb_ui::icons::warning(),
                    file_name.with_style(Style::Yellow).bold(),
                );
                continue;
            };

            let layout_dir = if layout_path.is_relative() {
                zen_path
                    .parent()
                    .unwrap_or(Path::new("."))
                    .join(&layout_path)
            } else {
                layout_path
            };

            let pcb_path = layout_dir.join("layout.kicad_pcb");

            match spinner.suspend(|| {
                run_combined_layout_checks(&zen_path, &pcb_path, &schematic, &args.suppress)
            }) {
                Ok((had_check_errors, _warnings)) => {
                    if had_check_errors {
                        has_errors = true;
                    }
                }
                Err(e) => {
                    eprintln!(
                        "{} {}: {e:#}",
                        pcb_ui::icons::error(),
                        file_name.with_style(Style::Red).bold()
                    );
                    has_errors = true;
                }
            }
        } else {
            // Normal mode: Generate/update layout
            let spinner = Spinner::builder(format!("{file_name}: Generating layout")).start();

            // Check if the schematic has a layout
            match process_layout(&schematic, &zen_path, args.sync_board_config, args.temp) {
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
                        file_name.clone().with_style(Style::Green).bold(),
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
                        "{} {}: {e:#}",
                        pcb_ui::icons::error(),
                        file_name.with_style(Style::Red).bold()
                    );
                    has_errors = true;
                }
            }
        }
    }

    if has_errors {
        std::process::exit(1);
    }

    // Only handle layout opening in normal mode
    if !args.check {
        if generated_layouts.is_empty() {
            println!("\nNo layouts found.");
            return Ok(());
        }

        // Open the selected layout if not disabled (or if using temp)
        if (!no_open || args.temp) && !generated_layouts.is_empty() {
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

/// Run layout sync check and DRC, return combined diagnostics
///
/// This is used by both `layout --check` and `release` commands
pub fn run_combined_layout_checks(
    zen_path: &Path,
    pcb_path: &Path,
    schematic: &Schematic,
    suppress_kinds: &[String],
) -> Result<(bool, usize)> {
    // Prepare paths
    let temp_dir = tempfile::tempdir()?;
    let netlist_path = temp_dir.path().join("netlist.json");

    // Write JSON netlist
    let json_content = schematic.to_json()?;
    std::fs::write(&netlist_path, json_content)?;

    // Get board config path if it exists
    let board_config_path = pcb_layout::utils::extract_board_config(schematic).and_then(|config| {
        let board_config_file = temp_dir.path().join("board_config.json");
        serde_json::to_string(&config).ok().and_then(|json| {
            std::fs::write(&board_config_file, json).ok()?;
            Some(board_config_file)
        })
    });

    // Run layout sync check
    let sync_diagnostics = crate::layout_check::run_layout_check(
        zen_path,
        pcb_path,
        &netlist_path,
        board_config_path.as_deref(),
        suppress_kinds,
    )?;

    // Run DRC checks if PCB exists
    let drc_diagnostics = if pcb_path.exists() {
        drc::run_drc(pcb_path, suppress_kinds)?
    } else {
        pcb_zen_core::Diagnostics {
            diagnostics: vec![],
        }
    };

    // Combine diagnostics
    let mut all_diags = sync_diagnostics.diagnostics;
    all_diags.extend(drc_diagnostics.diagnostics);
    let combined = pcb_zen_core::Diagnostics {
        diagnostics: all_diags,
    };

    // Print with unified summary
    print_combined_diagnostics(&combined)
}

/// Print combined diagnostics with unified summary table
fn print_combined_diagnostics(diagnostics: &pcb_zen_core::Diagnostics) -> Result<(bool, usize)> {
    use comfy_table::{presets, Attribute, Cell, ContentArrangement, Table};
    use pcb_zen_core::lang::error::CategorizedDiagnostic;
    use starlark::errors::EvalSeverity;
    use std::collections::HashMap;

    let mut category_counts: HashMap<String, (usize, usize, usize, usize)> = HashMap::new();
    let mut errors = 0;
    let mut warnings = 0;
    let mut suppressed_errors = 0;
    let mut suppressed_warnings = 0;

    // Print diagnostics and collect counts
    for diagnostic in &diagnostics.diagnostics {
        // Get full category with prefix (e.g., "layout.drc.clearance" or "layout.sync.footprint_added")
        let category = diagnostic
            .source_error
            .as_ref()
            .and_then(|e| e.downcast_ref::<CategorizedDiagnostic>())
            .map(|c| c.kind.as_str())
            .unwrap_or("other");

        let entry = category_counts
            .entry(category.to_string())
            .or_insert((0, 0, 0, 0));

        // Update counts
        match (diagnostic.severity, diagnostic.suppressed) {
            (EvalSeverity::Error, false) => {
                entry.0 += 1;
                errors += 1;
            }
            (EvalSeverity::Error, true) => {
                entry.1 += 1;
                suppressed_errors += 1;
            }
            (EvalSeverity::Warning, false) => {
                entry.2 += 1;
                warnings += 1;
            }
            (EvalSeverity::Warning, true) => {
                entry.3 += 1;
                suppressed_warnings += 1;
            }
            _ => {}
        }

        // Print diagnostic (skip suppressed)
        if !diagnostic.suppressed {
            if let Some((severity_str, severity_color)) = match diagnostic.severity {
                EvalSeverity::Error => Some(("Error", Style::Red)),
                EvalSeverity::Warning => Some(("Warning", Style::Yellow)),
                _ => None,
            } {
                let lines: Vec<&str> = diagnostic.body.lines().collect();
                if let Some(first_line) = lines.first() {
                    eprintln!(
                        "{}: {}",
                        severity_str.with_style(severity_color).bold(),
                        first_line
                    );
                    for line in lines.iter().skip(1) {
                        eprintln!("{}", line.dimmed());
                    }
                }
            }
        }
    }

    // Print summary table
    if !diagnostics.diagnostics.is_empty() {
        eprintln!();
        let mut table = Table::new();
        table
            .load_preset(presets::UTF8_BORDERS_ONLY)
            .set_content_arrangement(ContentArrangement::Dynamic);

        table.set_header(vec![
            Cell::new("Category").add_attribute(Attribute::Bold),
            Cell::new(format!(
                "{} {}",
                "Errors".red().bold(),
                "(excluded)".dimmed()
            )),
            Cell::new(format!(
                "{} {}",
                "Warnings".yellow().bold(),
                "(excluded)".dimmed()
            )),
        ]);

        let mut sorted_categories: Vec<_> = category_counts.iter().collect();
        sorted_categories.sort_by_key(|(k, _)| *k);

        for (category, (e, se, w, sw)) in sorted_categories {
            table.add_row(vec![
                Cell::new(category),
                Cell::new(format_count(*e, *se, |s| s.red())),
                Cell::new(format_count(*w, *sw, |s| s.yellow())),
            ]);
        }

        table.add_row(vec![
            Cell::new("Total").add_attribute(Attribute::Bold),
            Cell::new(format_count(errors, suppressed_errors, |s| s.red().bold())),
            Cell::new(format_count(warnings, suppressed_warnings, |s| {
                s.yellow().bold()
            })),
        ]);

        eprintln!("{}", table);
    }

    // Print error message if there were errors
    if errors > 0 {
        eprintln!(
            "\n{} Layout check failed with {} error(s)",
            pcb_ui::icons::error(),
            errors
        );
    }

    Ok((errors > 0, warnings))
}

/// Format count with optional excluded count in parentheses
fn format_count<F>(count: usize, excluded: usize, color_fn: F) -> String
where
    F: Fn(String) -> colored::ColoredString,
{
    match (count, excluded) {
        (0, 0) => "-".dimmed().to_string(),
        (0, e) => format!("({})", e).dimmed().to_string(),
        (c, 0) => color_fn(c.to_string()).to_string(),
        (c, e) => format!(
            "{} {}",
            color_fn(c.to_string()),
            format!("({})", e).dimmed()
        ),
    }
}
