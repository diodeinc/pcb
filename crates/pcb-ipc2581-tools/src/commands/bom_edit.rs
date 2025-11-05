use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use pcb_sch::{BomMatchingKey, BomMatchingRule, GenericComponent};

use crate::utils::file as file_utils;
use crate::OutputFormat;

/// Extract generic component data from BOM item characteristics
fn extract_generic_component(
    ipc: &ipc2581::Ipc2581,
    item: &ipc2581::types::BomItem,
) -> Option<(GenericComponent, String)> {
    let chars = item.characteristics.as_ref()?;

    // Extract type, package, and value
    let mut component_type = None;
    let mut package = None;
    let mut value = None;

    for textual in &chars.textuals {
        if let Some(name) = textual.name {
            let name_str = ipc.resolve(name).to_lowercase();
            if let Some(val) = textual.value {
                let val_str = ipc.resolve(val);
                match name_str.as_str() {
                    "type" => component_type = Some(val_str.to_string()),
                    "package" => package = Some(val_str.to_string()),
                    "capacitance" | "resistance" | "value" => value = Some(val_str.to_string()),
                    _ => {}
                }
            }
        }
    }

    let package = package?;
    let value_str = value?;

    // Parse based on component type
    match component_type.as_deref() {
        Some("capacitor") => {
            let capacitance = value_str.parse().ok()?;
            Some((
                GenericComponent::Capacitor(pcb_sch::Capacitor {
                    capacitance,
                    dielectric: None,
                    esr: None,
                    voltage: None,
                }),
                package,
            ))
        }
        Some("resistor") => {
            let resistance = value_str.parse().ok()?;
            Some((
                GenericComponent::Resistor(pcb_sch::Resistor {
                    resistance,
                    voltage: None,
                }),
                package,
            ))
        }
        _ => None,
    }
}

/// Load existing AVL entries from IPC-2581 file
/// Returns: OEMDesignNumber -> (MPN, Manufacturer) -> AvlVmpn
fn load_existing_avl(
    ipc: &ipc2581::Ipc2581,
    new_interner: &mut ipc2581::Interner,
) -> HashMap<String, HashMap<(String, String), ipc2581::types::AvlVmpn>> {
    let mut existing = HashMap::new();

    if let Some(avl) = ipc.avl() {
        for item in &avl.items {
            let oem_number = ipc.resolve(item.oem_design_number).to_string();
            let mut mpn_map = HashMap::new();

            for vmpn in &item.vmpn_list {
                if let (Some(mpn_entry), Some(vendor)) = (vmpn.mpns.first(), vmpn.vendors.first()) {
                    let mpn = ipc.resolve(mpn_entry.name).to_string();
                    let manufacturer = ipc.resolve(vendor.enterprise_ref).to_string();

                    // Re-intern into new interner for consistent symbol space
                    let new_mpn = ipc2581::types::AvlMpn {
                        name: new_interner.intern(&mpn),
                        rank: mpn_entry.rank,
                        cost: mpn_entry.cost,
                        moisture_sensitivity: mpn_entry.moisture_sensitivity,
                        availability: mpn_entry.availability,
                        other: mpn_entry.other.map(|s| new_interner.intern(ipc.resolve(s))),
                    };

                    let new_vendor = ipc2581::types::AvlVendor {
                        enterprise_ref: new_interner.intern(&manufacturer),
                    };

                    let new_vmpn = ipc2581::types::AvlVmpn {
                        evpl_vendor: vmpn
                            .evpl_vendor
                            .map(|s| new_interner.intern(ipc.resolve(s))),
                        evpl_mpn: vmpn.evpl_mpn.map(|s| new_interner.intern(ipc.resolve(s))),
                        qualified: vmpn.qualified,
                        chosen: vmpn.chosen,
                        mpns: vec![new_mpn],
                        vendors: vec![new_vendor],
                    };

                    mpn_map.insert((mpn, manufacturer), new_vmpn);
                }
            }

            existing.insert(oem_number, mpn_map);
        }
    }

    existing
}

pub fn execute(
    file: &Path,
    rules_file: &Path,
    output: Option<&Path>,
    _format: OutputFormat,
) -> Result<()> {
    // Load the IPC-2581 file
    let content = file_utils::load_ipc_file(file)?;
    let ipc = ipc2581::Ipc2581::parse(&content)?;

    // Create a new interner for new data
    let mut new_interner = ipc2581::Interner::new();

    // Load rules from JSON file
    let rules_content = std::fs::read_to_string(rules_file)
        .with_context(|| format!("Failed to read rules file: {:?}", rules_file))?;
    let rules: Vec<BomMatchingRule> =
        serde_json::from_str(&rules_content).with_context(|| "Failed to parse rules JSON")?;

    // Check if BOM section exists
    let bom = ipc
        .bom()
        .ok_or_else(|| anyhow::anyhow!("IPC-2581 file has no BOM section"))?;

    // Load existing AVL entries: OEMDesignNumber -> (MPN, Manufacturer) -> AvlVmpn
    let mut merged_items: HashMap<String, HashMap<(String, String), ipc2581::types::AvlVmpn>> =
        load_existing_avl(&ipc, &mut new_interner);

    // Match BOM items against rules
    for item in &bom.items {
        let oem_design_number = ipc.resolve(item.oem_design_number_ref).to_string();

        // Try to extract MPN from characteristics for matching
        let mpn = item.characteristics.as_ref().and_then(|chars| {
            chars.textuals.iter().find_map(|textual| {
                textual.name.and_then(|name| {
                    let name_str = ipc.resolve(name).to_lowercase();
                    if matches!(
                        name_str.as_str(),
                        "mpn" | "manufacturerpartnumber" | "partnumber"
                    ) {
                        textual.value.map(|v| ipc.resolve(v).to_string())
                    } else {
                        None
                    }
                })
            })
        });

        // Match against rules
        for rule in &rules {
            let matched = match &rule.key {
                BomMatchingKey::Mpn(rule_mpn) => mpn.as_ref() == Some(rule_mpn),
                BomMatchingKey::Path(paths) => item.ref_des_list.iter().any(|ref_des| {
                    let designator = ipc.resolve(ref_des.name);
                    paths.iter().any(|path| path.ends_with(designator))
                }),
                BomMatchingKey::Generic(generic_key) => {
                    // Extract generic component data and match
                    if let Some((component, pkg)) = extract_generic_component(&ipc, item) {
                        pkg == generic_key.package && component.matches(&generic_key.component)
                    } else {
                        false
                    }
                }
            };

            if matched {
                let mpn_map = merged_items.entry(oem_design_number.clone()).or_default();

                // Process each offer - new rules override existing AVL
                for offer in &rule.offers {
                    let mpn_str = offer.manufacturer_pn.as_ref().ok_or_else(|| {
                        anyhow::anyhow!(
                            "Offer missing manufacturer_pn for OEM design number: {}",
                            oem_design_number
                        )
                    })?;
                    let manufacturer_str = offer.manufacturer.as_ref().ok_or_else(|| {
                        anyhow::anyhow!(
                            "Offer missing manufacturer for OEM design number: {}",
                            oem_design_number
                        )
                    })?;

                    let key = (mpn_str.clone(), manufacturer_str.clone());

                    // Create or replace entry (new rules override existing)
                    let mpn = ipc2581::types::AvlMpn {
                        name: new_interner.intern(mpn_str),
                        rank: offer.rank, // Use rank from offer (may be None)
                        cost: None,
                        moisture_sensitivity: None,
                        availability: None,
                        other: None,
                    };

                    let vendor = ipc2581::types::AvlVendor {
                        enterprise_ref: new_interner.intern(manufacturer_str),
                    };

                    let vmpn = ipc2581::types::AvlVmpn {
                        evpl_vendor: None,
                        evpl_mpn: None,
                        qualified: Some(true),
                        chosen: None, // Set after sorting
                        mpns: vec![mpn],
                        vendors: vec![vendor],
                    };

                    mpn_map.insert(key, vmpn);
                }
            }
        }
    }

    if merged_items.is_empty() {
        eprintln!("Warning: No BOM items found");
        return Ok(());
    }

    // Sort and set chosen flag for each OEMDesignNumber
    let avl_items: Vec<ipc2581::types::AvlItem> = merged_items
        .into_iter()
        .map(|(oem_design_number, mpn_map)| {
            let mut vmpn_list: Vec<_> = mpn_map.into_values().collect();

            // Sort by priority: chosen → rank (ascending) → unranked
            vmpn_list.sort_by(|a, b| a.cmp_priority(b));

            // Set chosen flag on best (first) entry
            for (i, vmpn) in vmpn_list.iter_mut().enumerate() {
                vmpn.chosen = Some(i == 0);
            }

            ipc2581::types::AvlItem {
                oem_design_number: new_interner.intern(&oem_design_number),
                vmpn_list,
                spec_refs: vec![],
            }
        })
        .collect();

    eprintln!(
        "Created AVL entries for {} BOM items with {} total alternatives",
        avl_items.len(),
        avl_items.iter().map(|i| i.vmpn_list.len()).sum::<usize>()
    );

    let avl_header = ipc2581::types::AvlHeader {
        title: new_interner.intern("BOM Alternatives"),
        source: new_interner.intern("pcb-ipc2581"),
        author: new_interner.intern("BOM Add Tool"),
        datetime: new_interner.intern(&chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S").to_string()),
        version: 1,
        comment: None,
        mod_ref: None,
    };

    let avl = ipc2581::types::Avl {
        name: new_interner.intern("BOM_Alternatives"),
        header: Some(avl_header),
        items: avl_items,
    };

    // Generate AVL XML
    let avl_xml = avl.to_xml(&new_interner);

    // Patch the XML
    let updated_xml = patch_or_add_avl_section(&content, &avl_xml)?;

    // Write to output file
    let output_path = output.unwrap_or(file);
    file_utils::save_ipc_file(output_path, &updated_xml)?;

    eprintln!("✓ Added BOM alternatives to {:?}", output_path);

    Ok(())
}

/// Patch the AVL section in the XML, or add it if it doesn't exist
fn patch_or_add_avl_section(original_xml: &str, new_avl_xml: &str) -> Result<String> {
    // Find if AVL section exists
    if let Some(avl_start) = original_xml.find("<Avl ") {
        // Find the end of the AVL section
        let search_from = avl_start;
        if let Some(avl_end_tag_start) = original_xml[search_from..].find("</Avl>") {
            let avl_end = search_from + avl_end_tag_start + "</Avl>".len();

            // Replace existing AVL section
            let mut result = String::new();
            result.push_str(&original_xml[..avl_start]);
            result.push_str(new_avl_xml);
            result.push_str(&original_xml[avl_end..]);

            return Ok(result);
        }
    }

    // AVL doesn't exist, insert it before </IPC-2581>
    if let Some(ipc_end) = original_xml.rfind("</IPC-2581>") {
        let mut result = String::new();
        result.push_str(&original_xml[..ipc_end]);
        result.push_str(new_avl_xml);
        result.push_str(&original_xml[ipc_end..]);

        return Ok(result);
    }

    anyhow::bail!("Could not find </IPC-2581> closing tag in XML");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_patch_or_add_avl_when_missing() {
        let original = r#"<?xml version="1.0"?>
<IPC-2581>
  <Content/>
</IPC-2581>"#;

        let new_avl = "  <Avl name=\"Test\">\n  </Avl>\n";

        let result = patch_or_add_avl_section(original, new_avl).unwrap();

        assert!(result.contains("<Avl name=\"Test\">"));
        assert!(result.contains("</Avl>"));
        assert!(result.contains("</IPC-2581>"));
    }

    #[test]
    fn test_patch_or_add_avl_when_exists() {
        let original = r#"<?xml version="1.0"?>
<IPC-2581>
  <Content/>
  <Avl name="Old">
    <AvlItem OEMDesignNumber="OLD"/>
  </Avl>
  <Bom/>
</IPC-2581>"#;

        let new_avl = "  <Avl name=\"New\">\n    <AvlItem OEMDesignNumber=\"NEW\"/>\n  </Avl>\n";

        let result = patch_or_add_avl_section(original, new_avl).unwrap();

        assert!(result.contains("<Avl name=\"New\">"));
        assert!(result.contains("OEMDesignNumber=\"NEW\""));
        assert!(!result.contains("OEMDesignNumber=\"OLD\""));
        assert!(result.contains("<Bom/>"));
    }
}
