use std::io::{self, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use comfy_table::presets::UTF8_FULL_CONDENSED;
use comfy_table::Table;
use pcb_sch::{generate_bom_entries, group_bom_entries, AggregatedBomEntry, BomEntry};
use pcb_ui::prelude::*;
use std::collections::BTreeMap;

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
    fn write_ungrouped<W: Write>(
        &self,
        entries: &BTreeMap<String, BomEntry>,
        writer: W,
    ) -> Result<()> {
        match self {
            BomFormat::Json => write_bom_json(entries, writer),
            BomFormat::Table => panic!("Use write_grouped for table format"),
        }
    }

    fn write_grouped<W: Write>(&self, entries: &[AggregatedBomEntry], writer: W) -> Result<()> {
        match self {
            BomFormat::Json => panic!("Use write_ungrouped for JSON format"),
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

    let mut schematic = eval_result
        .output
        .ok_or_else(|| anyhow::anyhow!("No schematic generated from {}", file_name))?;

    // Generate BOM entries
    let ungrouped_entries = generate_bom_entries(&mut schematic);

    spinner.finish();

    // Write output to stdout
    match args.format {
        BomFormat::Json => args
            .format
            .write_ungrouped(&ungrouped_entries, io::stdout().lock())?,
        BomFormat::Table => {
            let grouped_entries = group_bom_entries(ungrouped_entries);
            args.format
                .write_grouped(&grouped_entries, io::stdout().lock())?;
        }
    }

    Ok(())
}

pub fn write_bom_json<W: Write>(entries: &BTreeMap<String, BomEntry>, writer: W) -> Result<()> {
    // Output a list of BOM entries sorted by path
    let list: Vec<&BomEntry> = entries.values().collect();
    serde_json::to_writer_pretty(writer, &list).context("Failed to write JSON BOM")?;
    Ok(())
}

fn write_bom_table<W: Write>(entries: &[AggregatedBomEntry], mut writer: W) -> Result<()> {
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
            entry
                .designators
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(","),
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
