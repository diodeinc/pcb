use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::build::create_diagnostics_passes;
use crate::release::discover_layout_from_output;
use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use pcb_layout::utils;
use pcb_sch::bom::{Bom, parse_kicad_csv_bom};
use pcb_ui::prelude::*;

/// Generate BOM with KiCad fallback if design BOM is empty
pub fn generate_bom_with_fallback(design_bom: Bom, layout_path: Option<&Path>) -> Result<Bom> {
    if design_bom.is_empty()
        && let Some(layout_dir) = layout_path
    {
        let Some(kicad_files) = utils::discover_kicad_files(layout_dir)? else {
            return Ok(design_bom);
        };
        let kicad_sch_path = kicad_files.kicad_sch();

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

            let csv_content =
                std::fs::read_to_string(&temp_csv).context("Failed to read KiCad BOM export")?;
            let _ = std::fs::remove_file(&temp_csv);

            return parse_kicad_csv_bom(&csv_content)
                .map_err(|e| anyhow::anyhow!("Failed to parse KiCad BOM: {}", e));
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

    /// Disable network access (offline mode) - only use vendored dependencies
    #[arg(long = "offline")]
    pub offline: bool,

    /// Require that pcb.toml and pcb.sum are up-to-date. Fails if auto-deps would
    /// add dependencies or if the lockfile would be modified. Recommended for CI.
    #[arg(long)]
    pub locked: bool,
}

pub fn execute(args: BomArgs) -> Result<()> {
    crate::file_walker::require_zen_file(&args.file)?;

    // Resolve dependencies before evaluation
    let resolution_result = crate::resolve::resolve(args.file.parent(), args.offline, args.locked)?;

    let file_name = args.file.file_name().unwrap().to_string_lossy();

    // Show spinner while processing
    let spinner = Spinner::builder(format!("{file_name}: Building")).start();

    // Evaluate the design
    let eval_result = pcb_zen::eval(&args.file, resolution_result);
    let layout_path = eval_result
        .output
        .as_ref()
        .and_then(|output| discover_layout_from_output(output).transpose())
        .transpose()?
        .map(|d| d.layout_dir);
    let eval_output = eval_result.output_result().map_err(|mut diagnostics| {
        // Apply passes and render diagnostics if there are errors
        diagnostics.apply_passes(&create_diagnostics_passes(&[], &[]));
        anyhow::anyhow!("Failed to build {} - cannot generate BOM", file_name)
    })?;

    // Generate BOM entries with KiCad fallback
    spinner.set_message(format!("{file_name}: Generating BOM"));
    let schematic = eval_output
        .to_schematic()
        .context("Failed to convert to schematic")?;

    let mut bom = generate_bom_with_fallback(schematic.bom(), layout_path.as_deref())?;

    // Filter out components marked as skip_bom
    bom = bom.filter_excluded();

    #[cfg(feature = "api")]
    if !args.offline {
        match pcb_diode_api::auth::get_valid_token() {
            Ok(token) => {
                spinner.set_message(format!("{file_name}: Fetching availability"));
                if let Err(e) = pcb_diode_api::fetch_and_populate_availability(&token, &mut bom) {
                    log::warn!("Failed to fetch availability data: {}", e);
                }
            }
            Err(_) => {
                log::debug!("Not authenticated, skipping availability fetch");
            }
        }
    }

    spinner.finish();

    let mut writer = io::stdout().lock();
    match args.format {
        BomFormat::Json => write!(writer, "{}", bom.ungrouped_json())?,
        BomFormat::Table => bom.write_table(writer)?,
    };

    Ok(())
}
