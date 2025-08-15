use std::io::{self, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use comfy_table::presets::UTF8_FULL_CONDENSED;
use comfy_table::Table;
use pcb_sch::{generate_bom, BomEntry};
use pcb_ui::prelude::*;

use crate::build::evaluate_zen_file;

#[derive(ValueEnum, Debug, Clone, Default)]
pub enum BomFormat {
    #[default]
    Table,
    Json,
}

impl std::fmt::Display for BomFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BomFormat::Table => write!(f, "table"),
            BomFormat::Json => write!(f, "json"),
        }
    }
}

impl BomFormat {
    fn write<W: Write>(&self, entries: &[BomEntry], writer: W) -> Result<()> {
        match self {
            BomFormat::Json => write_bom_json(entries, writer),
            BomFormat::Table => write_bom_table(entries, writer),
        }
    }
}

#[derive(Args, Debug, Clone)]
#[command(about = "Generate Bill of Materials (BOM) from PCB projects")]
pub struct BomArgs {
    /// .zen file to process
    #[arg(value_name = "FILE", value_hint = clap::ValueHint::FilePath)]
    pub file: PathBuf,

    /// Output format
    #[arg(short, long, default_value_t = BomFormat::Table)]
    pub format: BomFormat,
}

pub fn execute(args: BomArgs) -> Result<()> {
    let file_name = args.file.file_name().unwrap().to_string_lossy();

    // Show spinner while processing
    let spinner = Spinner::builder(format!("{file_name}: Generating BOM")).start();

    // Evaluate the design
    let (eval_result, has_errors) = evaluate_zen_file(&args.file, false);

    if has_errors {
        spinner.error(format!("{file_name}: Build failed"));
        anyhow::bail!("Failed to build {} - cannot generate BOM", file_name);
    }

    let schematic = eval_result
        .output
        .ok_or_else(|| anyhow::anyhow!("No schematic generated from {}", file_name))?;

    // Generate BOM entries
    let bom_entries = generate_bom(&schematic);

    spinner.finish();

    // Write output to stdout
    args.format.write(&bom_entries, io::stdout().lock())?;

    Ok(())
}

pub fn write_bom_json<W: Write>(entries: &[BomEntry], writer: W) -> Result<()> {
    serde_json::to_writer_pretty(writer, entries).context("Failed to write JSON BOM")?;
    Ok(())
}

fn write_bom_table<W: Write>(entries: &[BomEntry], mut writer: W) -> Result<()> {
    let mut table = Table::new();
    table.load_preset(UTF8_FULL_CONDENSED);
    table.set_content_arrangement(comfy_table::ContentArrangement::DynamicFullWidth);

    // Set headers
    table.set_header(vec![
        "Designators",
        "MPN",
        "Manufacturer",
        "Package",
        "Value",
        "Description",
        "DNP",
    ]);

    // Add rows
    for entry in entries {
        table.add_row(vec![
            entry.designators.join(","),
            entry.mpn.as_deref().unwrap_or("").to_string(),
            entry.manufacturer.as_deref().unwrap_or("").to_string(),
            entry.package.as_deref().unwrap_or("").to_string(),
            entry.value.as_deref().unwrap_or("").to_string(),
            entry.description.as_deref().unwrap_or("").to_string(),
            if entry.dnp { "Yes" } else { "No" }.to_string(),
        ]);
    }

    writeln!(writer, "{table}")?;
    Ok(())
}
