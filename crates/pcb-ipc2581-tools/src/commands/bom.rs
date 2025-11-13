use std::collections::HashMap;
use std::io::{self, Write};
use std::path::Path;

#[cfg(feature = "api")]
use anyhow::Context;
use anyhow::Result;
use pcb_sch::{Bom, BomEntry};

use crate::utils::file as file_utils;
use crate::OutputFormat;

/// Trim and truncate description to 100 chars max
fn trim_description(s: Option<String>) -> Option<String> {
    s.map(|s| {
        let trimmed = s.trim();
        if trimmed.len() > 100 {
            format!("{} ...", &trimmed[..96])
        } else {
            trimmed.to_string()
        }
    })
    .filter(|s| !s.is_empty())
}

/// Extracted characteristics data from IPC-2581 BOM items
#[derive(Debug, Default)]
pub struct CharacteristicsData {
    pub package: Option<String>,
    pub value: Option<String>,
    pub path: Option<String>,
    pub matcher: Option<String>,
    pub alternatives: Vec<pcb_sch::Alternative>,
    pub properties: std::collections::BTreeMap<String, String>,
    pub component_type: Option<String>,
    pub resistance: Option<String>,
    pub capacitance: Option<String>,
    pub voltage: Option<String>,
    pub dielectric: Option<String>,
    pub esr: Option<String>,
}

/// Extract characteristics from IPC-2581 Characteristics
/// Returns package, value, alternatives, and custom properties
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
                "path" => data.path = Some(val_str),
                "matcher" => data.matcher = Some(val_str),
                "alternatives" => {
                    if let Some(alternative) = parse_alternative_json(&val_str) {
                        data.alternatives.push(alternative);
                    }
                }
                // Generic component fields
                "type" => data.component_type = Some(val_str.to_lowercase()),
                "resistance" => data.resistance = Some(val_str),
                "capacitance" => data.capacitance = Some(val_str),
                "voltage" => data.voltage = Some(val_str),
                "dielectric" => data.dielectric = Some(val_str),
                "esr" => data.esr = Some(val_str),
                // Exclude well-known fields (MPN/Manufacturer come from AVL)
                // and instance-specific metadata
                "mpn"
                | "manufacturerpartnumber"
                | "partnumber"
                | "manufacturer"
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

/// Parse alternative part data from JSON string
/// Handles HTML-encoded JSON like: {&quot;mpn&quot;: &quot;...&quot;, &quot;manufacturer&quot;: &quot;...&quot;}
fn parse_alternative_json(json_str: &str) -> Option<pcb_sch::Alternative> {
    // Decode HTML entities using quick-xml
    let decoded = quick_xml::escape::unescape(json_str).ok()?.to_string();

    // Parse as JSON
    let parsed: serde_json::Value = serde_json::from_str(&decoded).ok()?;

    // Extract mpn and manufacturer
    let mpn = parsed.get("mpn")?.as_str()?.to_string();
    let manufacturer = parsed.get("manufacturer")?.as_str()?.to_string();

    Some(pcb_sch::Alternative { mpn, manufacturer })
}

/// Build GenericComponent from extracted characteristics
/// Reuses the same logic as detect_generic_component in pcb-sch
fn build_generic_component(data: &CharacteristicsData) -> Option<pcb_sch::GenericComponent> {
    match data.component_type.as_deref()? {
        "resistor" => {
            let resistance = data.resistance.as_ref()?.parse().ok()?;
            let voltage = data.voltage.as_ref().and_then(|v| v.parse().ok());
            Some(pcb_sch::GenericComponent::Resistor(pcb_sch::Resistor {
                resistance,
                voltage,
            }))
        }
        "capacitor" => {
            let capacitance = data.capacitance.as_ref()?.parse().ok()?;
            let dielectric = data.dielectric.as_ref().and_then(|d| d.parse().ok());
            let esr = data.esr.as_ref().and_then(|e| e.parse().ok());
            let voltage = data.voltage.as_ref().and_then(|v| v.parse().ok());
            Some(pcb_sch::GenericComponent::Capacitor(pcb_sch::Capacitor {
                capacitance,
                dielectric,
                esr,
                voltage,
            }))
        }
        _ => None,
    }
}

pub fn execute(file: &Path, format: OutputFormat, availability: bool) -> Result<()> {
    let content = file_utils::load_ipc_file(file)?;
    let ipc = ipc2581::Ipc2581::parse(&content)?;

    // Extract BOM from IPC-2581
    #[cfg_attr(not(feature = "api"), allow(unused_mut))]
    let mut bom = extract_bom_from_ipc(&ipc)?;

    #[cfg(feature = "api")]
    if availability {
        use pcb_ui::prelude::*;
        let file_name = file.file_name().unwrap_or_default().to_string_lossy();
        let spinner = Spinner::builder(format!("{file_name}: Fetching availability")).start();

        let token = pcb_diode_api::auth::get_valid_token()
            .context("Not authenticated. Run `pcb auth login` to authenticate.")?;

        if let Err(e) = pcb_diode_api::fetch_and_populate_availability(&token, &mut bom) {
            log::warn!("Failed to fetch availability data: {}", e);
        }

        spinner.finish();
    }

    let mut writer = io::stdout().lock();
    match format {
        OutputFormat::Json => {
            write!(writer, "{}", bom.ungrouped_json())?;
        }
        OutputFormat::Text => {
            bom.write_table(writer)?;
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
            let characteristics_data = item
                .characteristics
                .as_ref()
                .map(|chars| extract_characteristics(ipc, chars))
                .unwrap_or_default();

            let CharacteristicsData {
                package,
                value,
                path: component_path,
                matcher,
                alternatives: textual_alternatives,
                properties,
                ..
            } = &characteristics_data;

            // AVL provides canonical MPN and Manufacturer via Enterprise references
            let (mpn, manufacturer, avl_alternatives) =
                lookup_from_avl(ipc, item.oem_design_number_ref);

            // Merge alternatives: AVL takes precedence, then textual characteristics
            let mut alternatives = avl_alternatives;
            alternatives.extend(textual_alternatives.clone());

            // Use BomItem description attribute if present, otherwise fallback to value
            let description = item
                .description
                .map(|sym| ipc.resolve(sym).to_string())
                .or(value.clone());

            // Build generic component data if available
            let generic_data = build_generic_component(&characteristics_data);

            // Build entry
            let entry = BomEntry {
                mpn,
                alternatives,
                manufacturer,
                package: package.clone(),
                value: value.clone(),
                description: trim_description(description),
                generic_data,
                offers: Vec::new(),
                dnp: false, // Will be set per ref_des
                skip_bom: false,
                matcher: matcher.clone(),
                properties: properties.clone(),
            };

            // Process reference designators
            for ref_des in &item.ref_des_list {
                let designator = ipc.resolve(ref_des.name).to_string();
                if designator.is_empty() {
                    continue;
                }

                // Use Path from characteristics, or fallback to ipc::designator format
                let path = component_path
                    .clone()
                    .unwrap_or_else(|| format!("ipc::{}", designator));

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
                        matcher: None,
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
    let Some(avl) = ipc.avl() else {
        return (None, None, Vec::new());
    };

    let oem_design_number_str = ipc.resolve(oem_design_number_ref);
    let Some(avl_item) = avl
        .items
        .iter()
        .find(|item| ipc.resolve(item.oem_design_number) == oem_design_number_str)
    else {
        return (None, None, Vec::new());
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
    let alternatives = sorted_vmpn[1..]
        .iter()
        .filter_map(|vmpn| {
            let mpn = ipc.resolve(vmpn.mpns.first()?.name).to_string();
            let manufacturer = ipc
                .resolve_enterprise(vmpn.vendors.first()?.enterprise_ref)?
                .to_string();
            Some(pcb_sch::Alternative { mpn, manufacturer })
        })
        .collect();

    (primary_mpn, primary_manufacturer, alternatives)
}
