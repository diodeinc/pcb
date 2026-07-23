use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::utils::file as file_utils;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    path: String,
    manufacturer: String,
    mpn: String,
}

struct ResolvedSelection<'a> {
    selection: &'a Selection,
    oem_design_number: String,
}

#[derive(Debug, Default)]
struct EnterpriseRegistry {
    name_to_id: HashMap<String, String>,
    id_to_name: HashMap<String, String>,
    next_vendor_id: usize,
    new_enterprises: Vec<(String, String)>,
}

impl EnterpriseRegistry {
    fn from_ipc(ipc: &ipc2581::Ipc2581) -> Self {
        let Some(logistic) = ipc.logistic_header() else {
            return Self::default();
        };

        let mut name_to_id = HashMap::new();
        let mut id_to_name = HashMap::new();
        let mut max_vendor_id = 0;

        for enterprise in &logistic.enterprises {
            let id = ipc.resolve(enterprise.id);

            if let Some(num) = id.strip_prefix("VENDOR_").and_then(|s| s.parse().ok()) {
                max_vendor_id = max_vendor_id.max(num);
            }

            if let Some(name) = enterprise.name.map(|name| ipc.resolve(name))
                && !name.is_empty()
                && !matches!(name, "Manufacturer" | "NONE" | "N/A")
            {
                name_to_id
                    .entry(name.to_string())
                    .or_insert_with(|| id.to_string());
                id_to_name.insert(id.to_string(), name.to_string());
            }
        }

        Self {
            name_to_id,
            id_to_name,
            next_vendor_id: max_vendor_id + 1,
            new_enterprises: Vec::new(),
        }
    }

    fn get_or_create_enterprise_id(&mut self, manufacturer: &str) -> String {
        if let Some(id) = self.name_to_id.get(manufacturer) {
            return id.clone();
        }

        let id = format!("VENDOR_{}", self.next_vendor_id);
        self.next_vendor_id += 1;

        self.name_to_id.insert(manufacturer.to_string(), id.clone());
        self.id_to_name.insert(id.clone(), manufacturer.to_string());
        self.new_enterprises
            .push((id.clone(), manufacturer.to_string()));

        id
    }

    fn manufacturer_for_id(&self, id: &str) -> Option<&str> {
        self.id_to_name.get(id).map(String::as_str)
    }
}

fn load_selections(path: &Path) -> Result<Vec<Selection>> {
    let selections: Vec<Selection> =
        serde_json::from_str(&std::fs::read_to_string(path).context("Read selections file")?)
            .context("Parse selections JSON")?;
    let mut paths = HashSet::new();

    for selection in &selections {
        for (name, value) in [
            ("path", &selection.path),
            ("manufacturer", &selection.manufacturer),
            ("mpn", &selection.mpn),
        ] {
            if value.is_empty() {
                anyhow::bail!("Selection {name} must not be empty");
            }
            if value.trim() != value {
                anyhow::bail!("Selection {name} must not have leading or trailing whitespace");
            }
        }

        if !paths.insert(selection.path.as_str()) {
            anyhow::bail!("Duplicate selection path: {}", selection.path);
        }
    }

    Ok(selections)
}

fn resolve_selections<'a>(
    ipc: &ipc2581::Ipc2581,
    selections: &'a [Selection],
) -> Result<Vec<ResolvedSelection<'a>>> {
    let bom = ipc.bom().ok_or_else(|| anyhow::anyhow!("No BOM section"))?;

    selections
        .iter()
        .map(|selection| {
            let matches: Vec<_> = bom
                .items
                .iter()
                .filter(|item| {
                    item.characteristics
                        .as_ref()
                        .is_some_and(|characteristics| {
                            characteristics.textuals.iter().any(|textual| {
                                textual.name.is_some_and(|name| ipc.resolve(name) == "Path")
                                    && textual
                                        .value
                                        .is_some_and(|value| ipc.resolve(value) == selection.path)
                            })
                        })
                })
                .collect();

            match matches.as_slice() {
                [item] => Ok(ResolvedSelection {
                    selection,
                    oem_design_number: ipc.resolve(item.oem_design_number_ref).to_string(),
                }),
                [] => anyhow::bail!("Selection path not found: {}", selection.path),
                _ => anyhow::bail!("Selection path is ambiguous: {}", selection.path),
            }
        })
        .collect()
}

fn reintern_symbol(
    ipc: &ipc2581::Ipc2581,
    interner: &mut ipc2581::Interner,
    symbol: ipc2581::Symbol,
) -> ipc2581::Symbol {
    interner.intern(ipc.resolve(symbol))
}

fn reintern_vmpn(
    ipc: &ipc2581::Ipc2581,
    interner: &mut ipc2581::Interner,
    vmpn: &ipc2581::types::AvlVmpn,
) -> ipc2581::types::AvlVmpn {
    ipc2581::types::AvlVmpn {
        evpl_vendor: vmpn
            .evpl_vendor
            .map(|symbol| reintern_symbol(ipc, interner, symbol)),
        evpl_mpn: vmpn
            .evpl_mpn
            .map(|symbol| reintern_symbol(ipc, interner, symbol)),
        qualified: vmpn.qualified,
        chosen: vmpn.chosen,
        mpns: vmpn
            .mpns
            .iter()
            .map(|mpn| ipc2581::types::AvlMpn {
                name: reintern_symbol(ipc, interner, mpn.name),
                rank: mpn.rank,
                cost: mpn.cost,
                moisture_sensitivity: mpn.moisture_sensitivity,
                availability: mpn.availability,
                other: mpn
                    .other
                    .map(|symbol| reintern_symbol(ipc, interner, symbol)),
            })
            .collect(),
        vendors: vmpn
            .vendors
            .iter()
            .map(|vendor| ipc2581::types::AvlVendor {
                enterprise_ref: reintern_symbol(ipc, interner, vendor.enterprise_ref),
            })
            .collect(),
    }
}

fn reintern_avl(ipc: &ipc2581::Ipc2581, interner: &mut ipc2581::Interner) -> ipc2581::types::Avl {
    let Some(avl) = ipc.avl() else {
        return ipc2581::types::Avl {
            name: interner.intern("BOM_Selections"),
            header: Some(ipc2581::types::AvlHeader {
                title: interner.intern("BOM Selections"),
                source: interner.intern("pcb"),
                author: interner.intern("pcb"),
                datetime: interner
                    .intern(&chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S").to_string()),
                version: 1,
                comment: None,
                mod_ref: None,
            }),
            items: Vec::new(),
        };
    };

    ipc2581::types::Avl {
        name: reintern_symbol(ipc, interner, avl.name),
        header: avl.header.as_ref().map(|header| ipc2581::types::AvlHeader {
            title: reintern_symbol(ipc, interner, header.title),
            source: reintern_symbol(ipc, interner, header.source),
            author: reintern_symbol(ipc, interner, header.author),
            datetime: reintern_symbol(ipc, interner, header.datetime),
            version: header.version,
            comment: header
                .comment
                .map(|symbol| reintern_symbol(ipc, interner, symbol)),
            mod_ref: header
                .mod_ref
                .map(|symbol| reintern_symbol(ipc, interner, symbol)),
        }),
        items: avl
            .items
            .iter()
            .map(|item| ipc2581::types::AvlItem {
                oem_design_number: reintern_symbol(ipc, interner, item.oem_design_number),
                vmpn_list: item
                    .vmpn_list
                    .iter()
                    .map(|vmpn| reintern_vmpn(ipc, interner, vmpn))
                    .collect(),
                spec_refs: item
                    .spec_refs
                    .iter()
                    .map(|symbol| reintern_symbol(ipc, interner, *symbol))
                    .collect(),
            })
            .collect(),
    }
}

fn create_vmpn(
    interner: &mut ipc2581::Interner,
    mpn: &str,
    enterprise_id: &str,
) -> ipc2581::types::AvlVmpn {
    ipc2581::types::AvlVmpn {
        evpl_vendor: None,
        evpl_mpn: None,
        qualified: Some(true),
        chosen: Some(true),
        mpns: vec![ipc2581::types::AvlMpn {
            name: interner.intern(mpn),
            rank: None,
            cost: None,
            moisture_sensitivity: None,
            availability: None,
            other: None,
        }],
        vendors: vec![ipc2581::types::AvlVendor {
            enterprise_ref: interner.intern(enterprise_id),
        }],
    }
}

fn apply_selection(
    avl: &mut ipc2581::types::Avl,
    interner: &mut ipc2581::Interner,
    enterprise_registry: &mut EnterpriseRegistry,
    resolved: &ResolvedSelection<'_>,
) {
    let selection = resolved.selection;
    let enterprise_id = enterprise_registry.get_or_create_enterprise_id(&selection.manufacturer);

    let item = match avl
        .items
        .iter_mut()
        .find(|item| interner.resolve(item.oem_design_number) == resolved.oem_design_number)
    {
        Some(item) => item,
        None => {
            avl.items.push(ipc2581::types::AvlItem {
                oem_design_number: interner.intern(&resolved.oem_design_number),
                vmpn_list: Vec::new(),
                spec_refs: Vec::new(),
            });
            avl.items.last_mut().expect("AVL item was just appended")
        }
    };

    for vmpn in &mut item.vmpn_list {
        vmpn.chosen = None;
    }

    let existing = item.vmpn_list.iter_mut().find(|vmpn| {
        let has_mpn = vmpn
            .mpns
            .iter()
            .any(|mpn| interner.resolve(mpn.name) == selection.mpn);
        let has_manufacturer = vmpn.vendors.iter().any(|vendor| {
            enterprise_registry.manufacturer_for_id(interner.resolve(vendor.enterprise_ref))
                == Some(selection.manufacturer.as_str())
        });
        has_mpn && has_manufacturer
    });

    if let Some(vmpn) = existing {
        vmpn.qualified = Some(true);
        vmpn.chosen = Some(true);
    } else {
        item.vmpn_list
            .push(create_vmpn(interner, &selection.mpn, &enterprise_id));
    }
}

pub fn execute(file: &Path, selections_file: &Path, output: &Path) -> Result<()> {
    let content = file_utils::load_ipc_file(file)?;
    let ipc = ipc2581::Ipc2581::parse(&content)?;
    let selections = load_selections(selections_file)?;
    let resolved = resolve_selections(&ipc, &selections)?;

    let mut interner = ipc2581::Interner::new();
    let mut enterprise_registry = EnterpriseRegistry::from_ipc(&ipc);
    let mut avl = reintern_avl(&ipc, &mut interner);

    for selection in &resolved {
        apply_selection(&mut avl, &mut interner, &mut enterprise_registry, selection);
    }

    let doc = ipc2581::edit::Doc::parse(&content)?;
    let comment = format!("BOM selections updated ({} items)", resolved.len());
    let mut edits = crate::utils::history::file_revision_edits(&doc, &comment)?;
    if !enterprise_registry.new_enterprises.is_empty() {
        edits.extend(logistic_header_edit(
            &doc,
            &enterprise_registry.new_enterprises,
        ));
    }
    edits.push(avl_section_edit(&doc, avl.to_xml(&interner))?);

    let updated_xml = ipc2581::edit::apply(&content, edits)?;
    let updated_xml = crate::utils::format::reformat_xml(&updated_xml)?;
    file_utils::save_ipc_file(output, &updated_xml)?;

    eprintln!(
        "Updated {} BOM selection{} in {:?}",
        resolved.len(),
        if resolved.len() == 1 { "" } else { "s" },
        output
    );
    Ok(())
}

fn logistic_header_edit(
    doc: &ipc2581::edit::Doc,
    new_enterprises: &[(String, String)],
) -> Option<ipc2581::edit::Edit> {
    let root = doc.root().ok()?;
    let header = doc.child(root, "LogisticHeader")?;

    let mut writer = ipc2581::XmlWriter::new();
    for (id, name) in new_enterprises {
        writer.empty_element(
            "Enterprise",
            &[
                ("id", id.as_str()),
                ("name", name.as_str()),
                ("code", "NONE"),
            ],
        );
    }
    let enterprises_xml = writer.into_string();

    Some(match doc.child(header, "Person") {
        Some(person) => doc.insert_before(person, enterprises_xml),
        None => doc.append_inside(header, enterprises_xml),
    })
}

fn avl_section_edit(doc: &ipc2581::edit::Doc, new_avl_xml: String) -> Result<ipc2581::edit::Edit> {
    let root = doc.root()?;
    Ok(match doc.child(root, "Avl") {
        Some(avl) => doc.replace(avl, new_avl_xml),
        None => doc.append_inside(root, new_avl_xml),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patch_avl(original: &str, new_avl: &str) -> String {
        let doc = ipc2581::edit::Doc::parse(original).unwrap();
        let edit = avl_section_edit(&doc, new_avl.to_string()).unwrap();
        ipc2581::edit::apply(original, vec![edit]).unwrap()
    }

    #[test]
    fn test_patch_or_add_avl_when_missing() {
        let original = r#"<?xml version="1.0"?>
<IPC-2581>
  <Content/>
</IPC-2581>"#;

        let new_avl = "  <Avl name=\"Test\">\n  </Avl>\n";

        let result = patch_avl(original, new_avl);

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

        let result = patch_avl(original, new_avl);

        assert!(result.contains("<Avl name=\"New\">"));
        assert!(result.contains("OEMDesignNumber=\"NEW\""));
        assert!(!result.contains("OEMDesignNumber=\"OLD\""));
        assert!(result.contains("<Bom/>"));
    }
}
