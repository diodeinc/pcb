use std::collections::HashMap;
use std::io::{self, Write};
use std::path::Path;

#[cfg(feature = "api")]
use anyhow::Context;
use anyhow::Result;
use pcb_sch::{Bom, BomEntry};

use crate::accessors::{CharacteristicsData, IpcAccessor};
use crate::utils::file as file_utils;
use crate::OutputFormat;
use pcb_sch::Alternative;

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

/// Convert accessor Alternative to pcb_sch::Alternative
fn to_sch_alternative(alt: &Alternative) -> pcb_sch::Alternative {
    pcb_sch::Alternative {
        mpn: alt.mpn.clone(),
        manufacturer: alt.manufacturer.clone(),
    }
}

pub fn execute(file: &Path, format: OutputFormat, _offline: bool) -> Result<()> {
    let content = file_utils::load_ipc_file(file)?;
    let ipc = ipc2581::Ipc2581::parse(&content)?;
    let accessor = IpcAccessor::new(&ipc);

    // Extract BOM from IPC-2581
    #[cfg_attr(not(feature = "api"), allow(unused_mut))]
    let mut bom = extract_bom_from_ipc(&accessor)?;

    #[cfg(feature = "api")]
    if !_offline {
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
fn extract_bom_from_ipc(accessor: &IpcAccessor) -> Result<Bom> {
    let ipc = accessor.ipc();
    let mut entries = HashMap::new();
    let mut designators = HashMap::new();

    // Get BOM from IPC-2581
    if let Some(bom_section) = ipc.bom() {
        for item in &bom_section.items {
            // Skip items with DOCUMENT category (e.g., test points marked exclude_from_bom in KiCad)
            if matches!(item.category, Some(ipc2581::types::BomCategory::Document)) {
                continue;
            }

            // Extract characteristics from BomItem using accessor
            let characteristics_data = item
                .characteristics
                .as_ref()
                .map(|chars| accessor.extract_characteristics(chars))
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
            let avl_lookup = accessor.lookup_avl(item.oem_design_number_ref);

            // Merge alternatives: AVL takes precedence, then textual characteristics
            let mut alternatives: Vec<pcb_sch::Alternative> = avl_lookup
                .alternatives
                .iter()
                .map(to_sch_alternative)
                .collect();
            alternatives.extend(textual_alternatives.iter().map(to_sch_alternative));

            // Use BomItem description attribute if present, otherwise fallback to value
            let description = item
                .description
                .map(|sym| ipc.resolve(sym).to_string())
                .or(value.clone());

            // Build generic component data if available
            let generic_data = build_generic_component(&characteristics_data);

            // Build entry
            let entry = BomEntry {
                mpn: avl_lookup.primary_mpn,
                alternatives,
                manufacturer: avl_lookup.primary_manufacturer,
                package: package.clone(),
                value: value.clone(),
                description: trim_description(description),
                generic_data,
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
