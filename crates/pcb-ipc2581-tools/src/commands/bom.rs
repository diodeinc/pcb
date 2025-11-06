use std::collections::HashMap;
use std::io::{self, Write};
use std::path::Path;

use anyhow::Result;
use comfy_table::presets::UTF8_FULL_CONDENSED;
use comfy_table::Table;
use pcb_sch::{Bom, BomEntry};
use starlark_syntax::slice_vec_ext::SliceExt;

use crate::utils::file as file_utils;
use crate::OutputFormat;

/// Extracted characteristics data from IPC-2581 BOM items
#[derive(Debug, Default)]
pub struct CharacteristicsData {
    pub package: Option<String>,
    pub value: Option<String>,
    pub properties: std::collections::BTreeMap<String, String>,
}

/// Extract characteristics from IPC-2581 Characteristics
/// Returns package, value, and custom properties
/// Note: MPN and Manufacturer must come from AVL/Enterprise (canonical IPC-2581 way)
pub fn extract_characteristics(
    ipc: &ipc2581::Ipc2581,
    chars: &ipc2581::types::Characteristics,
) -> CharacteristicsData {
    let mut data = CharacteristicsData::default();

    for textual in &chars.textuals {
        if let (Some(name), Some(val)) = (textual.name, textual.value) {
            let name_str = ipc.resolve(name).to_string();
            let name_lower = name_str.to_lowercase();
            let val_str = ipc.resolve(val).to_string();

            match name_lower.as_str() {
                "package" | "footprint" => data.package = Some(val_str),
                "value" => data.value = Some(val_str),
                // Exclude well-known fields and instance-specific metadata
                "mpn"
                | "manufacturerpartnumber"
                | "partnumber"
                | "manufacturer"
                | "path"
                | "prefix"
                | "symbol_name"
                | "symbol_path" => {}
                _ => {
                    data.properties.insert(name_str, val_str);
                }
            }
        }
    }

    data
}

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
            // Skip items with DOCUMENT category (e.g., test points marked exclude_from_bom in KiCad)
            if matches!(item.category, Some(ipc2581::types::BomCategory::Document)) {
                continue;
            }

            // Extract characteristics from BomItem
            let CharacteristicsData {
                package,
                value,
                properties,
            } = item
                .characteristics
                .as_ref()
                .map(|chars| extract_characteristics(ipc, chars))
                .unwrap_or_default();

            // AVL provides canonical MPN and Manufacturer via Enterprise references
            let (mpn, manufacturer, avl_alternatives) =
                lookup_from_avl(ipc, item.oem_design_number_ref);

            // Use BomItem description attribute if present, otherwise fallback to value
            let description = item
                .description
                .map(|sym| ipc.resolve(sym).to_string())
                .or(value.clone());

            // Build entry
            let entry = BomEntry {
                mpn,
                alternatives: avl_alternatives,
                manufacturer,
                package,
                value,
                description,
                generic_data: None,
                offers: Vec::new(),
                dnp: false, // Check ref des for populate flag
                skip_bom: false,
                properties,
            };

            // Process reference designators
            for ref_des in &item.ref_des_list {
                let designator = ipc.resolve(ref_des.name).to_string();

                // Skip empty designators (invalid/placeholder entries)
                if designator.is_empty() {
                    continue;
                }

                // Use Path property if available, otherwise use ipc::designator format
                let path = entry
                    .properties
                    .get("Path")
                    .cloned()
                    .unwrap_or_else(|| format!("ipc::{}", designator));

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

                    // Skip empty designators (invalid/placeholder entries)
                    if ref_des.is_empty() {
                        continue;
                    }

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
                        skip_bom: false,
                        properties: std::collections::BTreeMap::new(),
                    };

                    entries.insert(path.clone(), entry);
                    designators.insert(path, ref_des);
                }
            }
        }
    }

    Ok(Bom::new(entries, designators))
}

/// Look up MPN, manufacturer, and alternatives from AVL section
/// Returns (primary_mpn, primary_manufacturer, alternatives)
/// Per IPC-2581 spec: rank=1 or chosen=true is primary, rest are alternatives
pub fn lookup_from_avl(
    ipc: &ipc2581::Ipc2581,
    oem_design_number_ref: ipc2581::Symbol,
) -> (Option<String>, Option<String>, Vec<pcb_sch::Alternative>) {
    // Check if AVL section exists
    let avl = match ipc.avl() {
        Some(avl) => avl,
        None => return (None, None, Vec::new()),
    };

    // Get the OEM design number string to match against
    let oem_design_number_str = ipc.resolve(oem_design_number_ref);

    // Find matching AvlItem
    let avl_item = match avl
        .items
        .iter()
        .find(|item| ipc.resolve(item.oem_design_number) == oem_design_number_str)
    {
        Some(item) => item,
        None => return (None, None, Vec::new()),
    };

    if avl_item.vmpn_list.is_empty() {
        return (None, None, Vec::new());
    }

    // Sort by priority: chosen → rank (ascending) → unranked
    let mut sorted_vmpn: Vec<_> = avl_item.vmpn_list.iter().collect();
    sorted_vmpn.sort_by(|a, b| a.cmp_priority(b));

    // First entry is primary
    let primary = sorted_vmpn[0];
    let primary_mpn = primary
        .mpns
        .first()
        .map(|m| ipc.resolve(m.name).to_string());
    let primary_manufacturer = primary.vendors.first().and_then(|v| {
        ipc.resolve_enterprise(v.enterprise_ref)
            .map(|s| s.to_string())
    });

    // Rest are alternatives
    let alternatives: Vec<pcb_sch::Alternative> = sorted_vmpn[1..]
        .iter()
        .filter_map(|vmpn| {
            let mpn = vmpn.mpns.first()?.name;
            let manufacturer_ref = vmpn.vendors.first()?.enterprise_ref;
            let manufacturer = ipc.resolve_enterprise(manufacturer_ref)?;

            Some(pcb_sch::Alternative {
                mpn: ipc.resolve(mpn).to_string(),
                manufacturer: manufacturer.to_string(),
            })
        })
        .collect();

    (primary_mpn, primary_manufacturer, alternatives)
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
            .and_then(|arr| {
                arr.iter()
                    .find(|offer| offer["distributor"].as_str() != Some("__AVL__"))
            })
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

        // Use value as description if available, otherwise use description field
        let description = entry["value"]
            .as_str()
            .or_else(|| entry["description"].as_str())
            .unwrap_or_default();

        // Get alternatives if present
        let alternatives_str = entry
            .get("alternatives")
            .and_then(|a| a.as_array())
            .map(|arr| {
                if arr.is_empty() {
                    String::new()
                } else {
                    arr.iter()
                        .filter_map(|alt| alt["mpn"].as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            })
            .unwrap_or_default();

        table.add_row(vec![
            designators.as_str(),
            mpn,
            manufacturer,
            entry["package"].as_str().unwrap_or_default(),
            description,
            alternatives_str.as_str(),
            if entry["dnp"].as_bool().unwrap_or(false) {
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
        "Alternatives",
        "DNP",
    ]);

    writeln!(writer, "{table}")?;
    Ok(())
}
