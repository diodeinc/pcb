use std::collections::HashMap;
use std::io::{self, Write};
use std::path::Path;

use anyhow::Result;
use comfy_table::presets::UTF8_FULL_CONDENSED;
use comfy_table::Table;
use pcb_sch::{Bom, BomEntry};

use crate::utils::file as file_utils;
use crate::OutputFormat;

pub fn execute(file: &Path, format: OutputFormat) -> Result<()> {
    let content = file_utils::load_ipc_file(file)?;
    let ipc = ipc2581::Ipc2581::parse(&content)?;

    // Extract BOM from IPC-2581
    let bom = extract_bom_from_ipc(&ipc)?;

    let mut writer = io::stdout().lock();
    match format {
        OutputFormat::Json => {
            write!(writer, "{}", bom.ungrouped_json())?;
        }
        OutputFormat::Text => {
            write_bom_table(&bom, writer)?;
        }
    };

    Ok(())
}

/// Extract BOM data from IPC-2581 and convert to pcb_sch::Bom format
fn extract_bom_from_ipc(ipc: &ipc2581::Ipc2581) -> Result<Bom> {
    let mut entries = HashMap::new();
    let mut designators = HashMap::new();

    // Get BOM from IPC-2581
    if let Some(bom_section) = ipc.bom() {
        for item in &bom_section.items {
            // Get OEM design number as the key
            let oem_design_number = ipc.resolve(item.oem_design_number_ref).to_string();

            // Extract MPN and other data from characteristics
            let (mpn, manufacturer, package, value) = if let Some(chars) = &item.characteristics {
                let mut mpn = None;
                let mut manufacturer = None;
                let mut package = None;
                let mut value = None;

                for textual in &chars.textuals {
                    if let Some(name) = textual.name {
                        let name_str = ipc.resolve(name);
                        if let Some(val) = textual.value {
                            let val_str = ipc.resolve(val).to_string();
                            // Case-insensitive matching for common field names
                            let name_lower = name_str.to_lowercase();
                            match name_lower.as_str() {
                                "mpn" | "manufacturerpartnumber" | "partnumber" => {
                                    mpn = Some(val_str.clone());
                                }
                                "manufacturer" => manufacturer = Some(val_str.clone()),
                                "package" | "footprint" => package = Some(val_str.clone()),
                                "value" => value = Some(val_str.clone()),
                                _ => {}
                            }
                        }
                    }
                }

                (mpn, manufacturer, package, value)
            } else {
                (None, None, None, None)
            };

            // Build entry
            let entry = BomEntry {
                mpn,
                alternatives: Vec::new(),
                manufacturer,
                package,
                value,
                description: Some(oem_design_number),
                generic_data: None,
                offers: Vec::new(),
                dnp: false, // Check ref des for populate flag
            };

            // Process reference designators
            for ref_des in &item.ref_des_list {
                let designator = ipc.resolve(ref_des.name).to_string();
                let path = format!("ipc::{}", designator);

                // Check DNP status
                let mut entry_with_dnp = entry.clone();
                entry_with_dnp.dnp = !ref_des.populate;

                entries.insert(path.clone(), entry_with_dnp);
                designators.insert(path, designator);
            }
        }
    }

    // If BOM section is empty or missing, try to extract from ECAD components
    if entries.is_empty() {
        if let Some(ecad) = ipc.ecad() {
            if let Some(step) = ecad.cad_data.steps.first() {
                for component in &step.components {
                    let ref_des = ipc.resolve(component.ref_des).to_string();
                    let path = format!("ipc::{}", ref_des);

                    let package = Some(ipc.resolve(component.package_ref).to_string());

                    let entry = BomEntry {
                        mpn: component.part.map(|s| ipc.resolve(s).to_string()),
                        alternatives: Vec::new(),
                        manufacturer: None,
                        package,
                        value: None,
                        description: None,
                        generic_data: None,
                        offers: Vec::new(),
                        dnp: false,
                    };

                    entries.insert(path.clone(), entry);
                    designators.insert(path, ref_des);
                }
            }
        }
    }

    Ok(Bom::new(entries, designators))
}

/// Write BOM table in the same format as pcb bom (using grouped JSON)
fn write_bom_table<W: Write>(bom: &Bom, mut writer: W) -> io::Result<()> {
    let mut table = Table::new();
    table.load_preset(UTF8_FULL_CONDENSED);
    table.set_content_arrangement(comfy_table::ContentArrangement::DynamicFullWidth);

    // Parse grouped JSON to get the table data
    let json: serde_json::Value = serde_json::from_str(&bom.grouped_json()).unwrap();
    for entry in json.as_array().unwrap() {
        let designators = entry["designators"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|d| d.as_str())
            .collect::<Vec<_>>()
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

        // Use value as description if available, otherwise use description field
        let description = entry["value"]
            .as_str()
            .or_else(|| entry["description"].as_str())
            .unwrap_or_default();

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
