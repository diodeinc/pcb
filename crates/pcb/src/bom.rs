use std::io::{self, Write};
use std::path::PathBuf;

use crate::build::create_diagnostics_passes;
use crate::release::extract_layout_path;
use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use comfy_table::presets::UTF8_FULL_CONDENSED;
use comfy_table::Table;
use pcb_sch::{generate_bom_with_fallback, Bom};
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

    /// JSON file containing BOM matching rules
    #[arg(short = 'r', long = "rules", value_hint = clap::ValueHint::FilePath)]
    pub rules: Option<PathBuf>,
}

pub fn execute(args: BomArgs) -> Result<()> {
    let file_name = args.file.file_name().unwrap().to_string_lossy();

    // Show spinner while processing
    let spinner = Spinner::builder(format!("{file_name}: Building")).start();

    // Evaluate the design
    let eval_result = pcb_zen::eval(&args.file, pcb_zen::EvalConfig::default());
    let layout_path = extract_layout_path(&args.file, &eval_result).ok();
    let eval_output = eval_result.output_result().map_err(|mut diagnostics| {
        // Apply passes and render diagnostics if there are errors
        diagnostics.apply_passes(&create_diagnostics_passes(&[]));
        anyhow::anyhow!("Failed to build {} - cannot generate BOM", file_name)
    })?;

    // Generate BOM entries with KiCad fallback
    spinner.set_message(format!("{file_name}: Generating BOM"));
    let schematic = eval_output
        .to_schematic()
        .context("Failed to convert to schematic")?;
    let mut bom = generate_bom_with_fallback(schematic.bom(), layout_path.as_deref())
        .map_err(|e| anyhow::anyhow!("Failed to generate BOM: {}", e))?;

    // Apply BOM matching rules if provided
    if let Some(rules_path) = &args.rules {
        spinner.set_message(format!("{file_name}: Applying BOM rules"));
        let rules_content =
            std::fs::read_to_string(rules_path).context("Failed to read rules file")?;
        let rules: Vec<pcb_sch::BomMatchingRule> =
            serde_json::from_str(&rules_content).context("Failed to parse rules file")?;
        bom.apply_bom_rules(&rules);
    }

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
        // Use first offer info if available, otherwise use base component info
        let (mpn, manufacturer, distributor) = entry
            .get("offers")
            .and_then(|o| o.as_array())
            .and_then(|arr| arr.first())
            .map(|offer| {
                (
                    offer["manufacturer_pn"].as_str().unwrap_or_default(),
                    offer["manufacturer"].as_str().unwrap_or_default(),
                    offer["distributor"].as_str().unwrap_or_default(),
                )
            })
            .unwrap_or_else(|| {
                (
                    entry["mpn"].as_str().unwrap_or_default(),
                    entry["manufacturer"].as_str().unwrap_or_default(),
                    "",
                )
            });

        // Use value as description until all the generics have proper descriptions
        let description = entry["value"].as_str().unwrap_or_default();

        table.add_row(vec![
            designators.as_str(),
            mpn,
            manufacturer,
            entry["package"].as_str().unwrap_or_default(),
            description,
            distributor,
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
        "Description",
        "Distributor",
        "DNP",
    ]);

    writeln!(writer, "{table}")?;
    Ok(())
}
