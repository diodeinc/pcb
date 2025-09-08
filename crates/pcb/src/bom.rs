use std::io::{self, Write};
use std::path::PathBuf;

use crate::build::create_diagnostics_passes;
use anyhow::Result;
use clap::{Args, ValueEnum};
use comfy_table::presets::UTF8_FULL_CONDENSED;
use comfy_table::Table;
use pcb_sch::Bom;
use pcb_ui::prelude::*;
use starlark_syntax::slice_vec_ext::SliceExt;

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
    let spinner = Spinner::builder(format!("{file_name}: Building")).start();

    // Evaluate the design
    let schematic = pcb_zen::run(&args.file, pcb_zen::EvalConfig::default())
        .output_result()
        .map_err(|mut diagnostics| {
            // Apply passes and render diagnostics if there are errors
            diagnostics.apply_passes(&create_diagnostics_passes(&[]));
            anyhow::anyhow!("Failed to build {} - cannot generate BOM", file_name)
        })?;

    // Generate BOM entries
    spinner.set_message(format!("{file_name}: Generating BOM"));
    let bom = schematic.bom();
    spinner.finish();

    let mut writer = io::stdout().lock();
    match args.format {
        BomFormat::Json => write!(writer, "{}", bom.ungrouped_json())?,
        BomFormat::Table => write_bom_table(&bom, writer)?,
    };

    Ok(())
}

fn write_bom_table<W: Write>(bom: &Bom, mut writer: W) -> io::Result<()> {
    let mut table = Table::new();
    table.load_preset(UTF8_FULL_CONDENSED);
    table.set_content_arrangement(comfy_table::ContentArrangement::DynamicFullWidth);

    let json: serde_json::Value = serde_json::from_str(&bom.grouped_json()).unwrap();
    for entry in json.as_array().unwrap() {
        let designators = entry["designators"]
            .as_array()
            .unwrap()
            .map(|d| d.as_str().unwrap())
            .join(",");
        table.add_row(vec![
            designators.as_str(),
            entry["mpn"].as_str().unwrap_or_default(),
            entry["manufacturer"].as_str().unwrap_or_default(),
            entry["package"].as_str().unwrap_or_default(),
            entry["value"].as_str().unwrap_or_default(),
            entry["description"].as_str().unwrap_or_default(),
            if entry["dnp"].as_bool().unwrap() {
                "Yes"
            } else {
                "No"
            },
        ]);
    }

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

    writeln!(writer, "{table}")?;
    Ok(())
}
