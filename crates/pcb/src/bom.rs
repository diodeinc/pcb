use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::build::create_diagnostics_passes;
use crate::release::extract_layout_path;
use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use comfy_table::presets::UTF8_FULL_CONDENSED;
use comfy_table::Table;
use pcb_sch::{parse_kicad_csv_bom, Bom};
use pcb_ui::prelude::*;
use starlark_syntax::slice_vec_ext::SliceExt;

/// Generate BOM with KiCad fallback if design BOM is empty
pub fn generate_bom_with_fallback(design_bom: Bom, layout_path: Option<&Path>) -> Result<Bom> {
    if design_bom.is_empty() {
        if let Some(layout_dir) = layout_path {
            let kicad_sch_path = layout_dir.join("layout.kicad_sch");

            if kicad_sch_path.exists() {
                let temp_csv = std::env::temp_dir().join(format!("bom_{}.csv", std::process::id()));

                pcb_kicad::KiCadCliBuilder::new()
                    .command("sch")
                    .subcommand("export")
                    .subcommand("bom")
                    .arg(kicad_sch_path.to_string_lossy().as_ref())
                    .arg("-o")
                    .arg(temp_csv.to_string_lossy().as_ref())
                    .arg("--fields")
                    .arg("Reference,Value,Footprint,Manufacturer,MPN,Description,${DNP}")
                    .arg("--labels")
                    .arg("Reference,Value,Footprint,Manufacturer,MPN,Description,DNP")
                    .run()
                    .context("Failed to extract BOM from KiCad schematic")?;

                let csv_content = std::fs::read_to_string(&temp_csv)
                    .context("Failed to read KiCad BOM export")?;
                let _ = std::fs::remove_file(&temp_csv);

                return parse_kicad_csv_bom(&csv_content)
                    .map_err(|e| anyhow::anyhow!("Failed to parse KiCad BOM: {}", e));
            }
        }
    }

    Ok(design_bom)
}

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

    let mut bom = generate_bom_with_fallback(schematic.bom(), layout_path.as_deref())?;

    // Apply BOM matching rules if provided
    if let Some(rules_path) = &args.rules {
        spinner.set_message(format!("{file_name}: Applying BOM rules"));
        let rules_content =
            std::fs::read_to_string(rules_path).context("Failed to read rules file")?;
        let rules: Vec<pcb_sch::BomMatchingRule> =
            serde_json::from_str(&rules_content).context("Failed to parse rules file")?;
        bom.apply_bom_rules(&rules);
    }

    // Filter out components marked as skip_bom
    bom = bom.filter_excluded();

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
        let (mpn, manufacturer) = entry
            .get("offers")
            .and_then(|o| o.as_array())
            .and_then(|arr| arr.first())
            .map(|offer| {
                (
                    offer["manufacturer_pn"].as_str().unwrap_or_default(),
                    offer["manufacturer"].as_str().unwrap_or_default(),
                )
            })
            .unwrap_or_else(|| {
                (
                    entry["mpn"].as_str().unwrap_or_default(),
                    entry["manufacturer"].as_str().unwrap_or_default(),
                )
            });

        // Use value as description until all the generics have proper descriptions
        let description = entry["description"].as_str().unwrap_or_default();

        table.add_row(vec![
            designators.as_str(),
            mpn,
            manufacturer,
            entry["package"].as_str().unwrap_or_default(),
            description,
            if entry.get("dnp").and_then(|v| v.as_bool()).unwrap_or(false) {
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
        "DNP",
    ]);

    writeln!(writer, "{table}")?;
    Ok(())
}
