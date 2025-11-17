use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use pcb_sch::{BomMatchingKey, BomMatchingRule, GenericComponent};

use crate::utils::file as file_utils;

/// Key for deduplicating VMPN entries by MPN and manufacturer
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
struct VmpnKey {
    mpn: String,
    manufacturer: String,
}

impl VmpnKey {
    fn new(mpn: String, manufacturer: String) -> Self {
        Self { mpn, manufacturer }
    }
}

/// Helper to create an AvlVmpn with minimal boilerplate
fn create_vmpn(
    interner: &mut ipc2581::Interner,
    mpn: &str,
    manufacturer: &str,
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
        enterprise_ref: interner.intern(manufacturer),
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
) -> (VmpnKey, ipc2581::types::AvlVmpn) {
    (
        VmpnKey::new(
            ipc.resolve(vmpn.mpns[0].name).to_string(),
            ipc.resolve(vmpn.vendors[0].enterprise_ref).to_string(),
        ),
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
    )
}

fn load_existing_avl(
    ipc: &ipc2581::Ipc2581,
    interner: &mut ipc2581::Interner,
) -> HashMap<String, HashMap<VmpnKey, ipc2581::types::AvlVmpn>> {
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
                    .map(|v| reintern_vmpn(ipc, v, interner))
                    .collect(),
            )
        })
        .collect()
}

pub fn execute(file: &Path, rules_file: &Path, output: Option<&Path>) -> Result<()> {
    let content = file_utils::load_ipc_file(file)?;
    let ipc = ipc2581::Ipc2581::parse(&content)?;
    let mut interner = ipc2581::Interner::new();

    let rules: Vec<BomMatchingRule> =
        serde_json::from_str(&std::fs::read_to_string(rules_file).context("Read rules file")?)
            .context("Parse rules JSON")?;

    let bom = ipc.bom().ok_or_else(|| anyhow::anyhow!("No BOM section"))?;
    let mut merged_items = load_existing_avl(&ipc, &mut interner);

    for item in &bom.items {
        let oem_design_number = ipc.resolve(item.oem_design_number_ref).to_string();
        // Get MPN from AVL (canonical source)
        let (mpn, _, _) = super::bom::lookup_from_avl(&ipc, item.oem_design_number_ref);

        for rule in &rules {
            let matched = match &rule.key {
                BomMatchingKey::Mpn(rule_mpn) => mpn.as_ref() == Some(rule_mpn),
                BomMatchingKey::Path(paths) => item.ref_des_list.iter().any(|ref_des| {
                    let designator = ipc.resolve(ref_des.name);
                    paths.iter().any(|path| path == designator)
                }),
                BomMatchingKey::Generic(generic_key) => extract_generic_component(&ipc, item)
                    .is_some_and(|(c, p)| {
                        p == generic_key.package && c.matches(&generic_key.component)
                    }),
            };

            if matched {
                let mpn_map = merged_items.entry(oem_design_number.clone()).or_default();

                for offer in &rule.offers {
                    let (mpn, mfr) = match (&offer.manufacturer_pn, &offer.manufacturer) {
                        (Some(m), Some(f)) => (m, f),
                        _ => anyhow::bail!(
                            "Offer missing MPN or manufacturer for OEM: {}",
                            oem_design_number
                        ),
                    };
                    mpn_map.insert(
                        VmpnKey::new(mpn.clone(), mfr.clone()),
                        create_vmpn(&mut interner, mpn, mfr, offer.rank, Some(true)),
                    );
                }
            }
        }
    }

    if merged_items.is_empty() {
        eprintln!("Warning: No BOM items found");
        return Ok(());
    }

    let avl_items: Vec<ipc2581::types::AvlItem> = merged_items
        .into_iter()
        .map(|(oem, mpn_map)| {
            let mut vmpn_list: Vec<_> = mpn_map.into_values().collect();
            vmpn_list.sort_by(|a, b| a.cmp_priority(b));
            for (i, vmpn) in vmpn_list.iter_mut().enumerate() {
                vmpn.chosen = Some(i == 0);
            }
            ipc2581::types::AvlItem {
                oem_design_number: interner.intern(&oem),
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

    let updated_xml = patch_or_add_avl_section(&content, &avl.to_xml(&interner))?;
    file_utils::save_ipc_file(output.unwrap_or(file), &updated_xml)?;

    eprintln!("✓ Added BOM alternatives to {:?}", output.unwrap_or(file));
    Ok(())
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
