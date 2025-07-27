use anyhow::{Context, Result};
use clap::Args;
use comfy_table::presets::UTF8_FULL_CONDENSED;
use comfy_table::Table;
use pcb_sch::{generate_bom, write_bom_json, write_bom_html, BomProfile, BomEntry};
use pcb_sch::profile::OutputConfig;
use pcb_ui::prelude::*;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::PathBuf;

use crate::build::evaluate_zen_file;

#[derive(Args, Debug, Clone)]
#[command(about = "Generate Bill of Materials (BOM) from PCB projects")]
pub struct BomArgs {
    /// .zen file to process
    #[arg(value_name = "FILE", value_hint = clap::ValueHint::FilePath)]
    pub file: PathBuf,

    /// BOM profile YAML file
    #[arg(short, long, value_hint = clap::ValueHint::FilePath)]
    pub profile: PathBuf,
}

pub fn execute(args: BomArgs) -> Result<()> {
    let file_name = args.file.file_name().unwrap().to_string_lossy();
    
    // Load and validate profile
    let profile = BomProfile::load(&args.profile)
        .with_context(|| format!("Failed to load profile from {}", args.profile.display()))?;

    // Show spinner while processing
    let spinner = Spinner::builder(format!("{file_name}: Generating BOM")).start();

    // Evaluate the design
    let (eval_result, has_errors) = evaluate_zen_file(&args.file);

    if has_errors {
        spinner.error(format!("{file_name}: Build failed"));
        anyhow::bail!("Failed to build {} - cannot generate BOM", file_name);
    }

    let schematic = eval_result.output
        .ok_or_else(|| anyhow::anyhow!("No schematic generated from {}", file_name))?;

    // Generate BOM entries
    let mut bom_entries = generate_bom(&schematic);

    // Apply profile rules
    profile.apply_rules(&mut bom_entries)
        .context("Failed to apply profile rules")?;

    // Always group entries (rules may have modified fields)
    bom_entries = profile.group_entries(bom_entries);

    spinner.finish();

    // Process every output in the profile
    for (output_name, output_config) in &profile.outputs {
        // Filter entries based on output configuration
        let filtered_entries = profile.filter_entries(bom_entries.clone(), output_name)
            .context("Failed to filter entries")?;

        // Determine output destination
        let design_name = args.file.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("design");
        let file_path = profile.substitute_file_path(&output_config.file, design_name);
        
        let dest = if file_path.is_empty() {
            OutputDest::Stdout
        } else {
            OutputDest::File(PathBuf::from(file_path))
        };

        // Write output
        write_output(&filtered_entries, output_config, dest.clone(), &profile)
            .with_context(|| format!("Failed to write output '{}'", output_name))?;

        // Show success message (only for file outputs)
        if let OutputDest::File(path) = &dest {
            eprintln!(
                "{} BOM '{}' with {} entries written to {}",
                pcb_ui::icons::success(),
                output_name,
                filtered_entries.len(),
                path.display()
            );
        }
    }

    Ok(())
}

#[derive(Clone)]
enum OutputDest {
    Stdout,
    File(PathBuf),
}

impl OutputDest {
    fn writer(&self) -> Result<Box<dyn Write>> {
        Ok(match self {
            Self::Stdout => Box::new(io::stdout().lock()),
            Self::File(path) => {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)
                        .with_context(|| format!("Failed to create directory {}", parent.display()))?;
                }
                Box::new(File::create(path)
                    .with_context(|| format!("Failed to create file {}", path.display()))?)
            }
        })
    }
}

fn write_output(
    entries: &[BomEntry],
    output_config: &OutputConfig,
    dest: OutputDest,
    profile: &BomProfile,
) -> Result<()> {
    let mut writer = dest.writer()?;

    match output_config.format.as_str() {
        "csv" => write_csv(entries, &mut writer, output_config, profile)?,
        "json" => write_bom_json(entries, &mut writer)
            .context("Failed to write JSON")?,
        "html" => write_bom_html(entries, &mut writer)
            .context("Failed to write HTML")?,
        "table" => write_table(entries, &mut writer, output_config, profile)?,
        _ => anyhow::bail!("Unsupported format: {}", output_config.format),
    }

    Ok(())
}

fn write_csv<W: Write>(
    entries: &[BomEntry],
    writer: &mut W,
    output_config: &OutputConfig,
    profile: &BomProfile,
) -> Result<()> {
    // Get columns (all columns if none specified)
    let columns = if output_config.columns.is_empty() {
        BomProfile::all_columns()
            .into_iter()
            .map(|(header, field)| (header.to_string(), field.to_string()))
            .collect::<Vec<_>>()
    } else {
        output_config.columns.iter()
            .map(|(header, field)| (header.clone(), field.clone()))
            .collect()
    };

    // Write CSV header
    let headers: Vec<String> = columns.iter().map(|(header, _)| header.clone()).collect();
    writeln!(writer, "{}", headers.join(","))?;

    // Write each entry
    for entry in entries {
        let values: Vec<String> = columns
            .iter()
            .map(|(_, field_name)| {
                let value = profile.get_field_value(entry, field_name);
                escape_csv_field(&value)
            })
            .collect();

        writeln!(writer, "{}", values.join(","))?;
    }

    Ok(())
}

fn write_table<W: Write>(
    entries: &[BomEntry],
    writer: &mut W,
    output_config: &OutputConfig,
    profile: &BomProfile,
) -> Result<()> {
    let mut table = Table::new();
    table.load_preset(UTF8_FULL_CONDENSED);

    // Get columns (all columns if none specified)
    let columns = if output_config.columns.is_empty() {
        BomProfile::all_columns()
            .into_iter()
            .map(|(header, field)| (header.to_string(), field.to_string()))
            .collect::<Vec<_>>()
    } else {
        output_config.columns.iter()
            .map(|(header, field)| (header.clone(), field.clone()))
            .collect()
    };

    // Set headers
    let headers: Vec<String> = columns.iter().map(|(header, _)| header.clone()).collect();
    table.set_header(headers);

    // Add rows
    for entry in entries {
        let row: Vec<String> = columns
            .iter()
            .map(|(_, field_name)| profile.get_field_value(entry, field_name))
            .collect();
        table.add_row(row);
    }

    writeln!(writer, "{}", table)?;
    Ok(())
}

/// Escape CSV field by quoting if it contains commas, quotes, or newlines
fn escape_csv_field(field: &str) -> String {
    if field.contains(',') || field.contains('"') || field.contains('\n') {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}
