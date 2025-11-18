use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use pcb_sch::{BomMatchingKey, BomMatchingRule, GenericComponent};

use crate::utils::file as file_utils;

/// Type alias for BOM item alternatives map: OEM Design Number -> (MPN, Manufacturer) -> VMPN
type AlternativesMap = HashMap<String, HashMap<VmpnKey, ipc2581::types::AvlVmpn>>;

/// Tracks manufacturer name → Enterprise ID mapping
#[derive(Debug, Default)]
struct EnterpriseRegistry {
    /// Map from manufacturer name to Enterprise ID
    name_to_id: HashMap<String, String>,
    /// Next available vendor ID number
    next_vendor_id: usize,
    /// New enterprises that need to be added to LogisticHeader
    new_enterprises: Vec<(String, String)>, // (id, name)
}

impl EnterpriseRegistry {
    /// Create registry from existing LogisticHeader
    fn from_ipc(ipc: &ipc2581::Ipc2581) -> Self {
        let Some(logistic) = ipc.logistic_header() else {
            return Self::default();
        };

        let mut name_to_id = HashMap::new();
        let mut max_vendor_id = 0;

        for enterprise in &logistic.enterprises {
            let id = ipc.resolve(enterprise.id);

            // Track the highest VENDOR_N number
            if let Some(num) = id.strip_prefix("VENDOR_").and_then(|s| s.parse().ok()) {
                max_vendor_id = max_vendor_id.max(num);
            }

            // Map manufacturer name → Enterprise ID (skip placeholders)
            if let Some(name) = enterprise.name.map(|n| ipc.resolve(n)) {
                if !name.is_empty() && !matches!(name, "Manufacturer" | "NONE" | "N/A") {
                    name_to_id.insert(name.to_string(), id.to_string());
                }
            }
        }

        Self {
            name_to_id,
            next_vendor_id: max_vendor_id + 1,
            new_enterprises: Vec::new(),
        }
    }

    /// Get or create Enterprise ID for a manufacturer name
    fn get_or_create_enterprise_id(&mut self, manufacturer_name: &str) -> String {
        if let Some(id) = self.name_to_id.get(manufacturer_name) {
            return id.clone();
        }

        // Create new Enterprise ID
        let new_id = format!("VENDOR_{}", self.next_vendor_id);
        self.next_vendor_id += 1;

        self.name_to_id
            .insert(manufacturer_name.to_string(), new_id.clone());
        self.new_enterprises
            .push((new_id.clone(), manufacturer_name.to_string()));

        new_id
    }
}

/// Key for deduplicating VMPN entries by MPN and manufacturer name
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
struct VmpnKey {
    mpn: String,
    manufacturer: String,
}

/// Helper to create an AvlVmpn with minimal boilerplate
fn create_vmpn(
    interner: &mut ipc2581::Interner,
    mpn: &str,
    enterprise_id: &str,
    rank: Option<u32>,
    qualified: Option<bool>,
) -> ipc2581::types::AvlVmpn {
    let mpn_entry = ipc2581::types::AvlMpn {
        name: interner.intern(mpn),
        rank,
        cost: None,
        moisture_sensitivity: None,
        availability: None,
        other: None,
    };

    let vendor = ipc2581::types::AvlVendor {
        enterprise_ref: interner.intern(enterprise_id), // Use Enterprise ID, not name!
    };

    ipc2581::types::AvlVmpn {
        evpl_vendor: None,
        evpl_mpn: None,
        qualified,
        chosen: None,
        mpns: vec![mpn_entry],
        vendors: vec![vendor],
    }
}

fn extract_generic_component(
    ipc: &ipc2581::Ipc2581,
    item: &ipc2581::types::BomItem,
) -> Option<(GenericComponent, String)> {
    let chars = item.characteristics.as_ref()?;
    let mut fields: HashMap<String, String> = chars
        .textuals
        .iter()
        .filter_map(|t| {
            Some((
                ipc.resolve(t.name?).to_lowercase(),
                ipc.resolve(t.value?).to_string(),
            ))
        })
        .collect();

    let package = fields.remove("package")?;
    let value = fields
        .remove("capacitance")
        .or_else(|| fields.remove("resistance"))
        .or_else(|| fields.remove("value"))?;

    match fields.get("type")?.as_str() {
        "capacitor" => Some((
            GenericComponent::Capacitor(pcb_sch::Capacitor {
                capacitance: value.parse().ok()?,
                dielectric: None,
                esr: None,
                voltage: None,
            }),
            package,
        )),
        "resistor" => Some((
            GenericComponent::Resistor(pcb_sch::Resistor {
                resistance: value.parse().ok()?,
                voltage: None,
            }),
            package,
        )),
        t => {
            eprintln!(
                "Unsupported type '{}' for {}",
                t,
                ipc.resolve(item.oem_design_number_ref)
            );
            None
        }
    }
}

fn reintern_vmpn(
    ipc: &ipc2581::Ipc2581,
    vmpn: &ipc2581::types::AvlVmpn,
    interner: &mut ipc2581::Interner,
) -> Option<(VmpnKey, ipc2581::types::AvlVmpn)> {
    let mpn = ipc.resolve(vmpn.mpns[0].name).to_string();
    let manufacturer = ipc
        .resolve_enterprise(vmpn.vendors[0].enterprise_ref)?
        .to_string();

    Some((
        VmpnKey { mpn, manufacturer },
        ipc2581::types::AvlVmpn {
            evpl_vendor: vmpn.evpl_vendor.map(|s| interner.intern(ipc.resolve(s))),
            evpl_mpn: vmpn.evpl_mpn.map(|s| interner.intern(ipc.resolve(s))),
            qualified: vmpn.qualified,
            chosen: vmpn.chosen,
            mpns: vmpn
                .mpns
                .iter()
                .map(|m| ipc2581::types::AvlMpn {
                    name: interner.intern(ipc.resolve(m.name)),
                    rank: m.rank,
                    cost: m.cost,
                    moisture_sensitivity: m.moisture_sensitivity,
                    availability: m.availability,
                    other: m.other.map(|s| interner.intern(ipc.resolve(s))),
                })
                .collect(),
            vendors: vmpn
                .vendors
                .iter()
                .map(|v| ipc2581::types::AvlVendor {
                    enterprise_ref: interner.intern(ipc.resolve(v.enterprise_ref)),
                })
                .collect(),
        },
    ))
}

/// Check if a BOM item matches a rule's key
fn matches_rule_key(
    ipc: &ipc2581::Ipc2581,
    item: &ipc2581::types::BomItem,
    key: &BomMatchingKey,
    mpn: Option<&String>,
) -> bool {
    match key {
        BomMatchingKey::Mpn(rule_mpn) => mpn == Some(rule_mpn),
        BomMatchingKey::Path(paths) => item.ref_des_list.iter().any(|ref_des| {
            let designator = ipc.resolve(ref_des.name);
            paths.iter().any(|path| path == designator)
        }),
        BomMatchingKey::Generic(generic_key) => extract_generic_component(ipc, item)
            .is_some_and(|(c, p)| p == generic_key.package && c.matches(&generic_key.component)),
    }
}

fn load_existing_avl(ipc: &ipc2581::Ipc2581, interner: &mut ipc2581::Interner) -> AlternativesMap {
    let Some(avl) = ipc.avl() else {
        return HashMap::new();
    };

    avl.items
        .iter()
        .map(|item| {
            (
                ipc.resolve(item.oem_design_number).to_string(),
                item.vmpn_list
                    .iter()
                    .filter(|v| !v.mpns.is_empty() && !v.vendors.is_empty())
                    .filter_map(|v| reintern_vmpn(ipc, v, interner))
                    .collect(),
            )
        })
        .collect()
}

/// Convert alternatives map to AVL items with sorting and chosen flag
fn create_avl_items(
    alternatives: AlternativesMap,
    interner: &mut ipc2581::Interner,
) -> Vec<ipc2581::types::AvlItem> {
    alternatives
        .into_iter()
        .map(|(oem, mpn_map)| {
            let mut vmpn_list: Vec<_> = mpn_map.into_values().collect();
            vmpn_list.sort_by(|a, b| a.cmp_priority(b));

            // Mark first (highest priority) as chosen
            if let Some(first) = vmpn_list.first_mut() {
                first.chosen = Some(true);
            }

            ipc2581::types::AvlItem {
                oem_design_number: interner.intern(&oem),
                vmpn_list,
                spec_refs: vec![],
            }
        })
        .collect()
}

pub fn execute(file: &Path, rules_file: &Path, output: Option<&Path>) -> Result<()> {
    let content = file_utils::load_ipc_file(file)?;
    let ipc = ipc2581::Ipc2581::parse(&content)?;
    let mut interner = ipc2581::Interner::new();
    let mut enterprise_registry = EnterpriseRegistry::from_ipc(&ipc);

    let rules: Vec<BomMatchingRule> =
        serde_json::from_str(&std::fs::read_to_string(rules_file).context("Read rules file")?)
            .context("Parse rules JSON")?;

    let bom = ipc.bom().ok_or_else(|| anyhow::anyhow!("No BOM section"))?;
    let mut merged_items = load_existing_avl(&ipc, &mut interner);

    for item in &bom.items {
        let oem_design_number = ipc.resolve(item.oem_design_number_ref).to_string();
        // Get MPN from AVL (canonical source)
        let accessor = crate::accessors::IpcAccessor::new(&ipc);
        let avl_lookup = accessor.lookup_avl(item.oem_design_number_ref);
        let mpn = avl_lookup.primary_mpn;

        for rule in &rules {
            if !matches_rule_key(&ipc, item, &rule.key, mpn.as_ref()) {
                continue;
            }

            let mpn_map = merged_items.entry(oem_design_number.clone()).or_default();

            for offer in &rule.offers {
                let Some(mpn) = &offer.manufacturer_pn else {
                    anyhow::bail!("Offer missing MPN for OEM: {}", oem_design_number);
                };
                let Some(mfr) = &offer.manufacturer else {
                    anyhow::bail!("Offer missing manufacturer for OEM: {}", oem_design_number);
                };

                let enterprise_id = enterprise_registry.get_or_create_enterprise_id(mfr);

                mpn_map.insert(
                    VmpnKey {
                        mpn: mpn.clone(),
                        manufacturer: mfr.clone(),
                    },
                    create_vmpn(&mut interner, mpn, &enterprise_id, offer.rank, Some(true)),
                );
            }
        }
    }

    if merged_items.is_empty() {
        eprintln!("Warning: No BOM items found");
        return Ok(());
    }

    let avl_items = create_avl_items(merged_items, &mut interner);

    let num_items = avl_items.len();
    let num_alternatives: usize = avl_items.iter().map(|i| i.vmpn_list.len()).sum();

    eprintln!(
        "Created AVL entries for {} BOM items with {} total alternatives",
        num_items, num_alternatives
    );

    let avl = ipc2581::types::Avl {
        name: interner.intern("BOM_Alternatives"),
        header: Some(ipc2581::types::AvlHeader {
            title: interner.intern("BOM Alternatives"),
            source: interner.intern("pcb"),
            author: interner.intern("BOM Add Tool"),
            datetime: interner.intern(&chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S").to_string()),
            version: 1,
            comment: None,
            mod_ref: None,
        }),
        items: avl_items,
    };

    // Patch LogisticHeader first to add new Enterprise entries
    let mut updated_xml = content;
    if !enterprise_registry.new_enterprises.is_empty() {
        updated_xml = patch_logistic_header(&updated_xml, &enterprise_registry.new_enterprises)?;
        eprintln!(
            "Added {} new Enterprise entries to LogisticHeader",
            enterprise_registry.new_enterprises.len()
        );
    }

    // Then patch AVL section
    updated_xml = patch_or_add_avl_section(&updated_xml, &avl.to_xml(&interner))?;

    // Append FileRevision to HistoryRecord per IPC-2581C spec
    let comment = format!(
        "BOM alternatives added ({} items, {} total alternatives)",
        num_items, num_alternatives
    );
    updated_xml = crate::utils::history::append_file_revision(&updated_xml, &comment)?;

    // Reformat XML with proper indentation
    updated_xml = crate::utils::format::reformat_xml(&updated_xml)?;

    file_utils::save_ipc_file(output.unwrap_or(file), &updated_xml)?;

    eprintln!("✓ Added BOM alternatives to {:?}", output.unwrap_or(file));
    Ok(())
}

/// Patch LogisticHeader to add new Enterprise entries (before Person or closing tag)
fn patch_logistic_header(
    original_xml: &str,
    new_enterprises: &[(String, String)],
) -> Result<String> {
    use quick_xml::{
        events::{BytesStart, Event},
        Reader, Writer,
    };
    use std::io::Cursor;

    let mut reader = Reader::from_str(original_xml);
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    let mut buf = Vec::new();
    let (mut in_header, mut inserted) = (false, false);

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Eof => break,
            Event::Start(ref e) if e.name().as_ref() == b"LogisticHeader" => {
                in_header = true;
                writer.write_event(Event::Start(e.to_owned()))?;
            }
            Event::Empty(ref e) | Event::Start(ref e)
                if in_header && !inserted && e.name().as_ref() == b"Person" =>
            {
                for (id, name) in new_enterprises {
                    let mut elem = BytesStart::new("Enterprise");
                    elem.push_attribute(("id", id.as_str()));
                    elem.push_attribute(("name", name.as_str()));
                    elem.push_attribute(("code", "NONE"));
                    writer.write_event(Event::Empty(elem))?;
                }
                inserted = true;
                writer.write_event(Event::Empty(e.to_owned()).into_owned())?;
            }
            Event::End(ref e) if e.name().as_ref() == b"LogisticHeader" => {
                if !inserted {
                    for (id, name) in new_enterprises {
                        let mut elem = BytesStart::new("Enterprise");
                        elem.push_attribute(("id", id.as_str()));
                        elem.push_attribute(("name", name.as_str()));
                        elem.push_attribute(("code", "NONE"));
                        writer.write_event(Event::Empty(elem))?;
                    }
                }
                writer.write_event(Event::End(e.to_owned()))?;
            }
            e => writer.write_event(e)?,
        }
        buf.clear();
    }

    Ok(String::from_utf8(writer.into_inner().into_inner())?)
}

/// Patch AVL section in XML using quick-xml
fn patch_or_add_avl_section(original_xml: &str, new_avl_xml: &str) -> Result<String> {
    use quick_xml::{events::Event, Reader, Writer};
    use std::io::{Cursor, Write};

    let mut reader = Reader::from_str(original_xml);
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    let mut buf = Vec::new();
    let (mut skip_depth, mut avl_added) = (0, false);

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Eof => break,
            Event::Start(e) if e.name().as_ref() == b"Avl" && skip_depth == 0 => {
                skip_depth = 1;
                avl_added = true;
                writer.get_mut().write_all(new_avl_xml.as_bytes())?;
            }
            Event::Start(_) if skip_depth > 0 => skip_depth += 1,
            Event::End(e) if skip_depth > 0 => {
                skip_depth -= 1;
                if skip_depth == 0 && e.name().as_ref() != b"Avl" {
                    writer.write_event(Event::End(e))?;
                }
            }
            Event::End(e) if e.name().as_ref() == b"IPC-2581" => {
                if !avl_added {
                    writer.get_mut().write_all(new_avl_xml.as_bytes())?;
                }
                writer.write_event(Event::End(e))?;
            }
            Event::Empty(e) if e.name().as_ref() == b"Avl" => {
                avl_added = true;
                writer.get_mut().write_all(new_avl_xml.as_bytes())?;
            }
            e if skip_depth == 0 => writer.write_event(e)?,
            _ => {}
        }
        buf.clear();
    }
    Ok(String::from_utf8(writer.into_inner().into_inner())?)
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
