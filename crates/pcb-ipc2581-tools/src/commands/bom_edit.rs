use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;
use uuid::Uuid;

use crate::utils::file as file_utils;

const MANUFACTURER_ENTERPRISE_PREFIX: &str = "diode-mfr-";
const OFFER_DEFINITION_SOURCE: &str = "urn:diode:ipc2581:offer:v1";
const SUPPLIER_ENTERPRISE_REF: &str = "SelectedSupplierEnterpriseRef";
const SUPPLIER_PART_NUMBER: &str = "SelectedSupplierPartNumber";
const PART_IDENTITY_DEFINITION_SOURCE: &str = "urn:diode:ipc2581:part-identity:v1";
const EXTERNAL_PART_IDENTIFIER: &str = "ExternalPartIdentifier";
const MANUFACTURER_PART_NUMBER_ALIAS: &str = "ManufacturerPartNumberAlias";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Selection {
    path: String,
    refdes: Option<String>,
    manufacturer_id: Uuid,
    manufacturer: String,
    mpn: String,
    distributor: Option<String>,
    distributor_part_id: Option<String>,
    external_part_library: Option<String>,
    external_part_library_mpn: Option<String>,
    external_part_identifier: Option<String>,
    #[serde(default)]
    mpn_aliases: Vec<String>,
}

impl Selection {
    fn has_same_metadata(&self, other: &Self) -> bool {
        self.manufacturer_id == other.manufacturer_id
            && self.manufacturer == other.manufacturer
            && self.mpn == other.mpn
            && self.distributor == other.distributor
            && self.distributor_part_id == other.distributor_part_id
            && self.external_part_library == other.external_part_library
            && self.external_part_library_mpn == other.external_part_library_mpn
            && self.external_part_identifier == other.external_part_identifier
            && self.mpn_aliases == other.mpn_aliases
    }
}

#[derive(Debug)]
struct ResolvedSelection<'a> {
    selection: &'a Selection,
    oem_design_number: String,
    used_refdes_fallback: bool,
}

#[derive(Debug)]
struct BomHydration {
    oem_design_number: String,
    supplier: Option<(String, String)>,
    external_part_identifier: Option<String>,
    mpn_aliases: Vec<String>,
}

#[derive(Debug, Default)]
struct EnterpriseRegistry {
    name_to_id: HashMap<String, String>,
    id_to_name: HashMap<String, Option<String>>,
    requested_manufacturer_names: HashMap<String, String>,
    next_vendor_id: usize,
    new_enterprises: Vec<(String, String)>,
    renamed_enterprises: Vec<(String, String)>,
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
            let name = enterprise.name.map(|name| ipc.resolve(name).to_string());

            if let Some(num) = id.strip_prefix("VENDOR_").and_then(|s| s.parse().ok()) {
                max_vendor_id = max_vendor_id.max(num);
            }

            if let Some(name) = name.as_deref()
                && !name.is_empty()
                && !matches!(name, "Manufacturer" | "NONE" | "N/A")
                && !id.starts_with(MANUFACTURER_ENTERPRISE_PREFIX)
            {
                name_to_id
                    .entry(name.to_string())
                    .or_insert_with(|| id.to_string());
            }
            id_to_name.insert(id.to_string(), name);
        }

        Self {
            name_to_id,
            id_to_name,
            requested_manufacturer_names: HashMap::new(),
            next_vendor_id: max_vendor_id + 1,
            new_enterprises: Vec::new(),
            renamed_enterprises: Vec::new(),
        }
    }

    fn get_or_create_enterprise_id(&mut self, name: &str) -> String {
        if let Some(id) = self.name_to_id.get(name) {
            return id.clone();
        }

        let id = format!("VENDOR_{}", self.next_vendor_id);
        self.next_vendor_id += 1;

        self.name_to_id.insert(name.to_string(), id.clone());
        self.id_to_name.insert(id.clone(), Some(name.to_string()));
        self.new_enterprises.push((id.clone(), name.to_string()));

        id
    }

    fn get_or_create_manufacturer_id(
        &mut self,
        manufacturer_id: Uuid,
        manufacturer: &str,
    ) -> Result<String> {
        let enterprise_id = format!("{MANUFACTURER_ENTERPRISE_PREFIX}{manufacturer_id}");

        if let Some(previous) = self
            .requested_manufacturer_names
            .insert(enterprise_id.clone(), manufacturer.to_string())
            && previous != manufacturer
        {
            anyhow::bail!(
                "Manufacturer ID {manufacturer_id} has conflicting names: {previous:?} and {manufacturer:?}"
            );
        }

        if let Some(existing_name) = self.id_to_name.get(&enterprise_id) {
            if existing_name.as_deref() != Some(manufacturer) {
                self.id_to_name
                    .insert(enterprise_id.clone(), Some(manufacturer.to_string()));
                self.renamed_enterprises
                    .push((enterprise_id.clone(), manufacturer.to_string()));
            }
            return Ok(enterprise_id);
        }

        self.id_to_name
            .insert(enterprise_id.clone(), Some(manufacturer.to_string()));
        self.new_enterprises
            .push((enterprise_id.clone(), manufacturer.to_string()));
        Ok(enterprise_id)
    }
}

fn load_selections(path: &Path) -> Result<Vec<Selection>> {
    parse_selections(&std::fs::read_to_string(path).context("Read selections file")?)
}

fn parse_selections(content: &str) -> Result<Vec<Selection>> {
    let mut selections: Vec<Selection> =
        serde_json::from_str(content).context("Parse selections JSON")?;
    let mut paths = HashSet::new();

    for selection in &mut selections {
        if selection.distributor.is_some() != selection.distributor_part_id.is_some() {
            anyhow::bail!("Selection distributor and distributorPartId must be supplied together");
        }
        if selection.external_part_library.is_some()
            != selection.external_part_library_mpn.is_some()
        {
            anyhow::bail!(
                "Selection externalPartLibrary and externalPartLibraryMpn must be supplied together"
            );
        }
        if selection.external_part_identifier.is_some() && selection.external_part_library.is_none()
        {
            anyhow::bail!("Selection externalPartIdentifier requires externalPartLibrary");
        }
        for (name, value) in [
            ("path", Some(selection.path.as_str())),
            ("refdes", selection.refdes.as_deref()),
            ("manufacturer", Some(selection.manufacturer.as_str())),
            ("mpn", Some(selection.mpn.as_str())),
            ("distributor", selection.distributor.as_deref()),
            (
                "distributorPartId",
                selection.distributor_part_id.as_deref(),
            ),
            (
                "externalPartLibrary",
                selection.external_part_library.as_deref(),
            ),
            (
                "externalPartLibraryMpn",
                selection.external_part_library_mpn.as_deref(),
            ),
            (
                "externalPartIdentifier",
                selection.external_part_identifier.as_deref(),
            ),
        ] {
            let Some(value) = value else { continue };
            if value.is_empty() {
                anyhow::bail!("Selection {name} must not be empty");
            }
            if value.trim() != value {
                anyhow::bail!("Selection {name} must not have leading or trailing whitespace");
            }
        }

        let mut aliases = HashSet::new();
        for (index, alias) in selection.mpn_aliases.iter().enumerate() {
            if alias.is_empty() {
                anyhow::bail!("Selection mpnAliases[{index}] must not be empty");
            }
            if alias.trim() != alias {
                anyhow::bail!(
                    "Selection mpnAliases[{index}] must not have leading or trailing whitespace"
                );
            }
            if alias.eq_ignore_ascii_case(&selection.mpn) {
                anyhow::bail!("Selection mpnAliases[{index}] must not duplicate the canonical mpn");
            }
            if !aliases.insert(alias.to_ascii_lowercase()) {
                anyhow::bail!("Selection mpnAliases must be unique case-insensitively");
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

    let resolved = selections
        .iter()
        .map(|selection| {
            let path_matches: Vec<_> = bom
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

            match path_matches.as_slice() {
                [item] => Ok(ResolvedSelection {
                    selection,
                    oem_design_number: ipc.resolve(item.oem_design_number_ref).to_string(),
                    used_refdes_fallback: false,
                }),
                [] => {
                    let Some(refdes) = selection.refdes.as_deref() else {
                        anyhow::bail!("Selection path not found: {}", selection.path);
                    };
                    let refdes_matches: Vec<_> = bom
                        .items
                        .iter()
                        .filter(|item| {
                            item.reference_designators()
                                .any(|item_refdes| ipc.resolve(item_refdes.name) == refdes)
                        })
                        .collect();

                    match refdes_matches.as_slice() {
                        [item] => Ok(ResolvedSelection {
                            selection,
                            oem_design_number: ipc.resolve(item.oem_design_number_ref).to_string(),
                            used_refdes_fallback: true,
                        }),
                        [] => anyhow::bail!(
                            "Selection path not found and fallback RefDes not found: {} ({})",
                            selection.path,
                            refdes
                        ),
                        _ => anyhow::bail!("Selection fallback RefDes is ambiguous: {refdes}"),
                    }
                }
                _ => anyhow::bail!("Selection path is ambiguous: {}", selection.path),
            }
        })
        .collect::<Result<Vec<_>>>()?;

    let mut index_by_oem: HashMap<String, usize> = HashMap::new();
    let mut deduplicated: Vec<ResolvedSelection<'_>> = Vec::new();
    for resolved_selection in resolved {
        let selection = resolved_selection.selection;
        if let Some(&index) = index_by_oem.get(&resolved_selection.oem_design_number) {
            let previous = deduplicated[index].selection;
            if !previous.has_same_metadata(selection) {
                anyhow::bail!(
                    "Selections for paths {} and {} resolve to OEM {} but have differing metadata",
                    previous.path,
                    selection.path,
                    resolved_selection.oem_design_number
                );
            }
            deduplicated[index].used_refdes_fallback |= resolved_selection.used_refdes_fallback;
        } else {
            index_by_oem.insert(
                resolved_selection.oem_design_number.clone(),
                deduplicated.len(),
            );
            deduplicated.push(resolved_selection);
        }
    }

    Ok(deduplicated)
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
    rank: Option<u32>,
    qualified: Option<bool>,
    chosen: Option<bool>,
) -> ipc2581::types::AvlVmpn {
    ipc2581::types::AvlVmpn {
        evpl_vendor: None,
        evpl_mpn: None,
        qualified,
        chosen,
        mpns: vec![ipc2581::types::AvlMpn {
            name: interner.intern(mpn),
            rank,
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
) -> Result<BomHydration> {
    let selection = resolved.selection;
    let manufacturer_enterprise_id = enterprise_registry
        .get_or_create_manufacturer_id(selection.manufacturer_id, &selection.manufacturer)?;
    let supplier = selection
        .distributor
        .as_deref()
        .zip(selection.distributor_part_id.as_deref())
        .map(|(distributor, part_number)| {
            (
                enterprise_registry.get_or_create_enterprise_id(distributor),
                part_number.to_string(),
            )
        });

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

    let existing = item.vmpn_list.iter().position(|vmpn| {
        let has_mpn = vmpn
            .mpns
            .iter()
            .any(|mpn| interner.resolve(mpn.name) == selection.mpn);
        let has_manufacturer = vmpn
            .vendors
            .iter()
            .any(|vendor| interner.resolve(vendor.enterprise_ref) == manufacturer_enterprise_id);
        has_mpn && has_manufacturer
    });

    let selected_index = if let Some(index) = existing {
        index
    } else {
        item.vmpn_list.push(create_vmpn(
            interner,
            &selection.mpn,
            &manufacturer_enterprise_id,
            None,
            Some(true),
            Some(true),
        ));
        item.vmpn_list.len() - 1
    };
    let selected_vmpn = &mut item.vmpn_list[selected_index];
    selected_vmpn.qualified = Some(true);
    selected_vmpn.chosen = Some(true);
    selected_vmpn.evpl_vendor = selection
        .external_part_library
        .as_deref()
        .map(|value| interner.intern(value));
    selected_vmpn.evpl_mpn = selection
        .external_part_library_mpn
        .as_deref()
        .map(|value| interner.intern(value));

    Ok(BomHydration {
        oem_design_number: resolved.oem_design_number.clone(),
        supplier,
        external_part_identifier: selection.external_part_identifier.clone(),
        mpn_aliases: selection.mpn_aliases.clone(),
    })
}

fn write_avl(
    content: &str,
    avl: ipc2581::types::Avl,
    interner: &ipc2581::Interner,
    enterprise_registry: &EnterpriseRegistry,
    bom_hydrations: &[BomHydration],
    output: &Path,
    history_comment: &str,
) -> Result<()> {
    let doc = ipc2581::edit::Doc::parse(content)?;
    let mut edits = crate::utils::history::file_revision_edits(&doc, history_comment)?;
    edits.extend(enterprise_edits(&doc, enterprise_registry)?);
    edits.extend(bom_hydration_edits(&doc, bom_hydrations)?);
    edits.push(avl_section_edit(&doc, avl.to_xml(interner))?);

    let updated_xml = ipc2581::edit::apply(content, edits)?;
    let updated_xml = crate::utils::format::reformat_xml(&updated_xml)?;
    file_utils::save_ipc_file(output, &updated_xml)
}

pub fn execute_selections(file: &Path, selections_file: &Path, output: &Path) -> Result<()> {
    let content = file_utils::load_ipc_file(file)?;
    let ipc = ipc2581::Ipc2581::parse(&content)?;
    let selections = load_selections(selections_file)?;
    if selections.is_empty() {
        file_utils::save_ipc_file(output, &content)?;
        eprintln!("Updated 0 BOM selections in {:?}", output);
        return Ok(());
    }

    let resolved = resolve_selections(&ipc, &selections)?;
    let fallback_count = resolved
        .iter()
        .filter(|selection| selection.used_refdes_fallback)
        .count();
    if fallback_count > 0 {
        eprintln!(
            "Warning: resolved {} BOM selection{} by RefDes because Path was unavailable",
            fallback_count,
            if fallback_count == 1 { "" } else { "s" }
        );
    }

    let mut interner = ipc2581::Interner::new();
    let mut enterprise_registry = EnterpriseRegistry::from_ipc(&ipc);
    let mut avl = reintern_avl(&ipc, &mut interner);
    let mut bom_hydrations = Vec::with_capacity(resolved.len());

    for selection in &resolved {
        bom_hydrations.push(apply_selection(
            &mut avl,
            &mut interner,
            &mut enterprise_registry,
            selection,
        )?);
    }

    let comment = format!("BOM selections updated ({} items)", resolved.len());
    write_avl(
        &content,
        avl,
        &interner,
        &enterprise_registry,
        &bom_hydrations,
        output,
        &comment,
    )?;

    eprintln!(
        "Updated {} BOM selection{} in {:?}",
        resolved.len(),
        if resolved.len() == 1 { "" } else { "s" },
        output
    );
    Ok(())
}

fn unique_node_by_attr(
    doc: &ipc2581::edit::Doc,
    element: &str,
    attribute: &str,
    value: &str,
) -> Result<ipc2581::edit::Node> {
    let mut matches = doc
        .find_all(element)
        .into_iter()
        .filter(|&node| doc.attr(node, attribute) == Some(value));
    let node = matches
        .next()
        .ok_or_else(|| anyhow::anyhow!("{element} {attribute}={value} not found"))?;
    if matches.next().is_some() {
        anyhow::bail!("{element} {attribute}={value} is duplicated");
    }
    Ok(node)
}

fn enterprise_edits(
    doc: &ipc2581::edit::Doc,
    enterprise_registry: &EnterpriseRegistry,
) -> Result<Vec<ipc2581::edit::Edit>> {
    let mut edits = Vec::new();

    for (id, name) in &enterprise_registry.renamed_enterprises {
        let enterprise = unique_node_by_attr(doc, "Enterprise", "id", id)?;
        let attributes: Vec<_> = doc
            .attrs(enterprise)
            .filter(|(attribute, _)| *attribute != "name")
            .map(|(attribute, value)| (attribute.to_string(), value.to_string()))
            .chain(std::iter::once(("name".to_string(), name.clone())))
            .collect();
        let mut writer = ipc2581::XmlWriter::new();
        writer.empty_element_with("Enterprise", attributes);
        edits.push(doc.replace(enterprise, writer.into_string()));
    }

    if enterprise_registry.new_enterprises.is_empty() {
        return Ok(edits);
    }

    let root = doc.root()?;
    let header = doc
        .child(root, "LogisticHeader")
        .ok_or_else(|| anyhow::anyhow!("IPC-2581 has no LogisticHeader for new enterprises"))?;

    let mut writer = ipc2581::XmlWriter::new();
    for (id, name) in &enterprise_registry.new_enterprises {
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

    edits.push(match doc.child(header, "Person") {
        Some(person) => doc.insert_before(person, enterprises_xml),
        None => doc.append_inside(header, enterprises_xml),
    });
    Ok(edits)
}

fn bom_hydration_edits(
    doc: &ipc2581::edit::Doc,
    bom_hydrations: &[BomHydration],
) -> Result<Vec<ipc2581::edit::Edit>> {
    let mut edits = Vec::new();

    for hydration in bom_hydrations {
        let item = unique_node_by_attr(
            doc,
            "BomItem",
            "OEMDesignNumberRef",
            &hydration.oem_design_number,
        )?;
        let characteristics = doc.child(item, "Characteristics").ok_or_else(|| {
            anyhow::anyhow!(
                "BOM item {} has no Characteristics section",
                hydration.oem_design_number
            )
        })?;

        for textual in doc
            .children(characteristics)
            .into_iter()
            .filter(|&child| doc.name(child) == "Textual")
        {
            let is_diode_offer = doc.attr(textual, "definitionSource")
                == Some(OFFER_DEFINITION_SOURCE)
                && matches!(
                    doc.attr(textual, "textualCharacteristicName"),
                    Some(SUPPLIER_ENTERPRISE_REF | SUPPLIER_PART_NUMBER)
                );
            let is_diode_part_identity = doc.attr(textual, "definitionSource")
                == Some(PART_IDENTITY_DEFINITION_SOURCE)
                && matches!(
                    doc.attr(textual, "textualCharacteristicName"),
                    Some(EXTERNAL_PART_IDENTIFIER | MANUFACTURER_PART_NUMBER_ALIAS)
                );
            if is_diode_offer || is_diode_part_identity {
                edits.push(doc.delete(textual));
            }
        }

        if let Some((enterprise_id, part_number)) = &hydration.supplier {
            let mut writer = ipc2581::XmlWriter::new();
            writer.empty_element(
                "Textual",
                &[
                    ("definitionSource", OFFER_DEFINITION_SOURCE),
                    ("textualCharacteristicName", SUPPLIER_ENTERPRISE_REF),
                    ("textualCharacteristicValue", enterprise_id.as_str()),
                ],
            );
            writer.empty_element(
                "Textual",
                &[
                    ("definitionSource", OFFER_DEFINITION_SOURCE),
                    ("textualCharacteristicName", SUPPLIER_PART_NUMBER),
                    ("textualCharacteristicValue", part_number.as_str()),
                ],
            );
            edits.push(doc.append_inside(characteristics, writer.into_string()));
        }

        if hydration.external_part_identifier.is_some() || !hydration.mpn_aliases.is_empty() {
            let mut writer = ipc2581::XmlWriter::new();
            if let Some(identifier) = &hydration.external_part_identifier {
                writer.empty_element(
                    "Textual",
                    &[
                        ("definitionSource", PART_IDENTITY_DEFINITION_SOURCE),
                        ("textualCharacteristicName", EXTERNAL_PART_IDENTIFIER),
                        ("textualCharacteristicValue", identifier.as_str()),
                    ],
                );
            }
            for alias in &hydration.mpn_aliases {
                writer.empty_element(
                    "Textual",
                    &[
                        ("definitionSource", PART_IDENTITY_DEFINITION_SOURCE),
                        ("textualCharacteristicName", MANUFACTURER_PART_NUMBER_ALIAS),
                        ("textualCharacteristicValue", alias.as_str()),
                    ],
                );
            }
            edits.push(doc.append_inside(characteristics, writer.into_string()));
        }
    }

    Ok(edits)
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

    fn parse_selection_test_ipc(extra_item: &str) -> ipc2581::Ipc2581 {
        ipc2581::Ipc2581::parse(&format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="Owner">
    <FunctionMode mode="ASSEMBLY"/>
  </Content>
  <Bom name="TestBOM">
    <BomHeader assembly="Test Design" revision="1.0"/>
    <BomItem OEMDesignNumberRef="PATH_PART" quantity="1" category="ELECTRICAL">
      <RefDes name="R1" packageRef="R0402" populate="true" layerRef="F.Cu"/>
      <Characteristics category="ELECTRICAL">
        <Textual definitionSource="pcb" textualCharacteristicName="Path" textualCharacteristicValue="Power.R1"/>
        <Textual definitionSource="pcb" textualCharacteristicName="Path" textualCharacteristicValue="Alternate.R1"/>
      </Characteristics>
    </BomItem>
    <BomItem OEMDesignNumberRef="GROUPED_PART" quantity="2" category="ELECTRICAL">
      <RefDes name="C1" packageRef="C0201" populate="true" layerRef="F.Cu"/>
      <RefDes name="C2" packageRef="C0201" populate="true" layerRef="B.Cu"/>
      <Characteristics category="ELECTRICAL"/>
    </BomItem>
    {extra_item}
  </Bom>
</IPC-2581>"#
        ))
        .unwrap()
    }

    fn selection(path: &str, refdes: Option<&str>) -> Selection {
        Selection {
            path: path.to_string(),
            refdes: refdes.map(str::to_string),
            manufacturer_id: Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap(),
            manufacturer: "Example Manufacturer".to_string(),
            mpn: "EXAMPLE-MPN".to_string(),
            distributor: Some("Example Distributor".to_string()),
            distributor_part_id: Some("EXAMPLE-SKU".to_string()),
            external_part_library: None,
            external_part_library_mpn: None,
            external_part_identifier: None,
            mpn_aliases: Vec::new(),
        }
    }

    fn patch_avl(original: &str, new_avl: &str) -> String {
        let doc = ipc2581::edit::Doc::parse(original).unwrap();
        let edit = avl_section_edit(&doc, new_avl.to_string()).unwrap();
        ipc2581::edit::apply(original, vec![edit]).unwrap()
    }

    fn schema_valid_hydration_ipc() -> &'static str {
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="ASSEMBLY"/>
    <BomRef name="bom"/>
    <AvlRef name="avl"/>
  </Content>
  <LogisticHeader>
    <Role id="owner" roleFunction="OWNER"/>
    <Enterprise id="owner-enterprise" name="Owner" code="OWNER"/>
    <Enterprise id="LEGACY_MFR" name="New &amp; &lt;Manufacturer&gt;" code="LEGACY"/>
    <Enterprise id="diode-mfr-550e8400-e29b-41d4-a716-446655440000" name="Old Manufacturer Name" code="KEEP" address1="Preserve this address" phone="555-0100"/>
    <Enterprise id="VENDOR_7" name="Digi-Key &amp; Partners" code="SUPPLIER" email="orders@example.com"/>
    <Person name="designer" enterpriseRef="owner-enterprise" roleRef="owner"/>
  </LogisticHeader>
  <HistoryRecord number="1" origination="2026-08-25T00:00:00Z" software="test" lastChange="2026-08-25T00:00:00Z">
    <FileRevision fileRevisionId="1" comment="fixture">
      <SoftwarePackage name="test" vendor="test" revision="1">
        <Certification certificationStatus="SELFTEST"/>
      </SoftwarePackage>
    </FileRevision>
  </HistoryRecord>
  <Bom name="bom">
    <BomHeader assembly="test" revision="1"/>
    <BomItem OEMDesignNumberRef="EXISTING_PART" quantity="1" category="ELECTRICAL">
      <Characteristics category="ELECTRICAL">
        <Textual definitionSource="pcb" textualCharacteristicName="Path" textualCharacteristicValue="Power.R1"/>
        <Textual definitionSource="other" textualCharacteristicName="SelectedSupplierPartNumber" textualCharacteristicValue="preserve-me"/>
        <Textual definitionSource="urn:diode:ipc2581:offer:v1" textualCharacteristicName="SelectedSupplierEnterpriseRef" textualCharacteristicValue="OLD_VENDOR"/>
        <Textual definitionSource="urn:diode:ipc2581:offer:v1" textualCharacteristicName="SelectedSupplierEnterpriseRef" textualCharacteristicValue="DUPLICATE_VENDOR"/>
        <Textual definitionSource="urn:diode:ipc2581:offer:v1" textualCharacteristicName="SelectedSupplierPartNumber" textualCharacteristicValue="OLD-SKU"/>
        <Textual definitionSource="urn:diode:ipc2581:offer:v1" textualCharacteristicName="SelectedSupplierPartNumber" textualCharacteristicValue="DUPLICATE-SKU"/>
        <Textual definitionSource="urn:diode:ipc2581:part-identity:v1" textualCharacteristicName="ExternalPartIdentifier" textualCharacteristicValue="old:identifier"/>
        <Textual definitionSource="urn:diode:ipc2581:part-identity:v1" textualCharacteristicName="ExternalPartIdentifier" textualCharacteristicValue="duplicate:identifier"/>
        <Textual definitionSource="urn:diode:ipc2581:part-identity:v1" textualCharacteristicName="ManufacturerPartNumberAlias" textualCharacteristicValue="OLD-ALIAS"/>
        <Textual definitionSource="urn:diode:ipc2581:part-identity:v1" textualCharacteristicName="FutureIdentityField" textualCharacteristicValue="preserve-identity-extension"/>
      </Characteristics>
    </BomItem>
    <BomItem OEMDesignNumberRef="NEW_PART" quantity="1" category="ELECTRICAL">
      <Characteristics category="ELECTRICAL">
        <Textual definitionSource="pcb" textualCharacteristicName="Path" textualCharacteristicValue="Power.C1"/>
      </Characteristics>
    </BomItem>
  </Bom>
  <Ecad name="assembly">
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Step name="board">
        <Datum x="0" y="0"/>
      </Step>
    </CadData>
  </Ecad>
  <Avl name="avl">
    <AvlHeader title="Test" source="test" author="test" datetime="2026-08-25T00:00:00Z" version="1"/>
    <AvlItem OEMDesignNumber="EXISTING_PART">
      <AvlVmpn evplVendor="Legacy Library" evplMpn="LEGACY-EXTERNAL-MPN" qualified="true" chosen="true">
        <AvlMpn name="LEGACY-MPN" rank="7" other="preserve-me"/>
        <AvlVendor enterpriseRef="LEGACY_MFR"/>
      </AvlVmpn>
      <AvlVmpn evplVendor="Old Library" evplMpn="OLD-EXTERNAL-MPN" qualified="true">
        <AvlMpn name="MPN&lt;&amp;&quot;"/>
        <AvlVendor enterpriseRef="diode-mfr-550e8400-e29b-41d4-a716-446655440000"/>
      </AvlVmpn>
    </AvlItem>
  </Avl>
</IPC-2581>"#
    }

    fn assert_hydrated_output(xml: &str) {
        ipc2581::Ipc2581::validate(xml).unwrap();
        let doc = ipc2581::edit::Doc::parse(xml).unwrap();

        let existing_manufacturer = unique_node_by_attr(
            &doc,
            "Enterprise",
            "id",
            "diode-mfr-550e8400-e29b-41d4-a716-446655440000",
        )
        .unwrap();
        assert_eq!(
            doc.attr(existing_manufacturer, "name"),
            Some("New & <Manufacturer>")
        );
        assert_eq!(doc.attr(existing_manufacturer, "code"), Some("KEEP"));
        assert_eq!(
            doc.attr(existing_manufacturer, "address1"),
            Some("Preserve this address")
        );
        assert_eq!(doc.attr(existing_manufacturer, "phone"), Some("555-0100"));
        for id in ["diode-mfr-aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee", "VENDOR_8"] {
            let enterprise = unique_node_by_attr(&doc, "Enterprise", "id", id).unwrap();
            assert_eq!(doc.attr(enterprise, "code"), Some("NONE"));
        }

        for id in [
            "diode-mfr-550e8400-e29b-41d4-a716-446655440000",
            "diode-mfr-aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee",
            "VENDOR_7",
            "VENDOR_8",
        ] {
            assert_eq!(xml.matches(&format!("id=\"{id}\"")).count(), 1);
        }
        let diode_textuals = doc
            .find_all("Textual")
            .into_iter()
            .filter(|&node| doc.attr(node, "definitionSource") == Some(OFFER_DEFINITION_SOURCE))
            .collect::<Vec<_>>();
        assert_eq!(diode_textuals.len(), 4);
        for value in ["VENDOR_7", "SKU<&\"", "VENDOR_8", "NEW-SKU"] {
            assert_eq!(
                diode_textuals
                    .iter()
                    .filter(|&&node| doc.attr(node, "textualCharacteristicValue") == Some(value))
                    .count(),
                1
            );
        }
        assert!(xml.contains("definitionSource=\"other\""));
        assert!(xml.contains("LEGACY-MPN"));
        assert!(xml.contains("rank=\"7\""));
        assert!(xml.contains("other=\"preserve-me\""));
        assert!(xml.contains("evplVendor=\"Legacy Library\""));
        assert!(xml.contains("evplMpn=\"LEGACY-EXTERNAL-MPN\""));
        assert!(xml.contains("enterpriseRef=\"diode-mfr-550e8400-e29b-41d4-a716-446655440000\""));

        let selected_vmpn = doc
            .find_all("AvlVmpn")
            .into_iter()
            .find(|&vmpn| {
                doc.child(vmpn, "AvlMpn")
                    .is_some_and(|mpn| doc.attr(mpn, "name") == Some("MPN<&\""))
            })
            .unwrap();
        assert_eq!(doc.attr(selected_vmpn, "evplVendor"), Some("Cofactr & Co"));
        assert_eq!(doc.attr(selected_vmpn, "evplMpn"), Some("COFACTR-MPN<&\""));
        assert_eq!(doc.attr(selected_vmpn, "chosen"), Some("true"));
        let existing_avl_item =
            unique_node_by_attr(&doc, "AvlItem", "OEMDesignNumber", "EXISTING_PART").unwrap();
        assert_eq!(
            doc.children(existing_avl_item)
                .into_iter()
                .filter(|&node| {
                    doc.name(node) == "AvlVmpn" && doc.attr(node, "chosen") == Some("true")
                })
                .count(),
            1
        );
        assert!(doc.find_all("AvlMpn").into_iter().all(|node| {
            !matches!(
                doc.attr(node, "name"),
                Some("MPN-ALIAS<&\"" | "Second-Alias")
            )
        }));

        let identity_textuals = doc
            .find_all("Textual")
            .into_iter()
            .filter(|&node| {
                doc.attr(node, "definitionSource") == Some(PART_IDENTITY_DEFINITION_SOURCE)
            })
            .collect::<Vec<_>>();
        for (name, value) in [
            (EXTERNAL_PART_IDENTIFIER, "cofactr:CPID<&\""),
            (MANUFACTURER_PART_NUMBER_ALIAS, "MPN-ALIAS<&\""),
            (MANUFACTURER_PART_NUMBER_ALIAS, "Second-Alias"),
            ("FutureIdentityField", "preserve-identity-extension"),
        ] {
            assert_eq!(
                identity_textuals
                    .iter()
                    .filter(|&&node| {
                        doc.attr(node, "textualCharacteristicName") == Some(name)
                            && doc.attr(node, "textualCharacteristicValue") == Some(value)
                    })
                    .count(),
                1
            );
        }
        assert_eq!(identity_textuals.len(), 4);
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

    #[test]
    fn selection_path_remains_authoritative() {
        let ipc = parse_selection_test_ipc("");
        let selections = [selection("Power.R1", Some("C1"))];

        let resolved = resolve_selections(&ipc, &selections).unwrap();

        assert_eq!(resolved[0].oem_design_number, "PATH_PART");
        assert!(!resolved[0].used_refdes_fallback);
    }

    #[test]
    fn selection_falls_back_to_refdes_for_grouped_bom_item() {
        let ipc = parse_selection_test_ipc("");
        let selections = [selection("kicad::C2", Some("C2"))];

        let resolved = resolve_selections(&ipc, &selections).unwrap();

        assert_eq!(resolved[0].oem_design_number, "GROUPED_PART");
        assert!(resolved[0].used_refdes_fallback);
    }

    #[test]
    fn selection_rejects_ambiguous_refdes_fallback() {
        let ipc = parse_selection_test_ipc(
            r#"<BomItem OEMDesignNumberRef="OTHER_PART" quantity="1" category="ELECTRICAL">
      <RefDes name="C2" packageRef="C0201" populate="true" layerRef="F.Cu"/>
      <Characteristics category="ELECTRICAL"/>
    </BomItem>"#,
        );
        let selections = [selection("kicad::C2", Some("C2"))];

        let error = resolve_selections(&ipc, &selections).unwrap_err();

        assert_eq!(
            error.to_string(),
            "Selection fallback RefDes is ambiguous: C2"
        );
    }

    #[test]
    fn selection_schema_validates_identity_metadata() {
        let selections = parse_selections(
            r#"[{"path":"Power.R1","manufacturerId":"550E8400E29B41D4A716446655440000","manufacturer":"Example","mpn":"MPN","externalPartLibrary":"Cofactr","externalPartLibraryMpn":"LIBRARY-MPN","externalPartIdentifier":"cofactr:123","mpnAliases":["MPN-ALIAS","Second-Alias"]}]"#,
        )
        .unwrap();

        assert_eq!(
            selections[0].manufacturer_id.to_string(),
            "550e8400-e29b-41d4-a716-446655440000"
        );
        assert_eq!(selections[0].mpn_aliases, ["MPN-ALIAS", "Second-Alias"]);

        for invalid in [
            r#"[{"path":"Power.R1","manufacturerId":"not-a-uuid","manufacturer":"Example","mpn":"MPN"}]"#,
            r#"[{"path":"Power.R1","manufacturerId":"550e8400-e29b-41d4-a716-446655440000","manufacturer":"Example","mpn":"MPN","distributor":"Digi-Key"}]"#,
            r#"[{"path":"Power.R1","manufacturerId":"550e8400-e29b-41d4-a716-446655440000","manufacturer":"Example","mpn":"MPN","externalPartLibrary":"Cofactr"}]"#,
            r#"[{"path":"Power.R1","manufacturerId":"550e8400-e29b-41d4-a716-446655440000","manufacturer":"Example","mpn":"MPN","externalPartLibraryMpn":"LIBRARY-MPN"}]"#,
            r#"[{"path":"Power.R1","manufacturerId":"550e8400-e29b-41d4-a716-446655440000","manufacturer":"Example","mpn":"MPN","externalPartIdentifier":"cofactr:123"}]"#,
            r#"[{"path":"Power.R1","manufacturerId":"550e8400-e29b-41d4-a716-446655440000","manufacturer":"Example","mpn":"MPN","externalPartLibrary":"","externalPartLibraryMpn":"LIBRARY-MPN"}]"#,
            r#"[{"path":"Power.R1","manufacturerId":"550e8400-e29b-41d4-a716-446655440000","manufacturer":"Example","mpn":"MPN","externalPartLibrary":" Cofactr","externalPartLibraryMpn":"LIBRARY-MPN"}]"#,
            r#"[{"path":"Power.R1","manufacturerId":"550e8400-e29b-41d4-a716-446655440000","manufacturer":"Example","mpn":"MPN","mpnAliases":[""]}]"#,
            r#"[{"path":"Power.R1","manufacturerId":"550e8400-e29b-41d4-a716-446655440000","manufacturer":"Example","mpn":"MPN","mpnAliases":[" ALIAS"]}]"#,
            r#"[{"path":"Power.R1","manufacturerId":"550e8400-e29b-41d4-a716-446655440000","manufacturer":"Example","mpn":"MPN","mpnAliases":["ALIAS","alias"]}]"#,
            r#"[{"path":"Power.R1","manufacturerId":"550e8400-e29b-41d4-a716-446655440000","manufacturer":"Example","mpn":"MPN","mpnAliases":["mpn"]}]"#,
            r#"[{"path":"Power.R1","manufacturerId":"550e8400-e29b-41d4-a716-446655440000","manufacturer":"Example","mpn":"MPN","unknownIdentity":"value"}]"#,
        ] {
            assert!(parse_selections(invalid).is_err(), "accepted {invalid}");
        }
    }

    #[test]
    fn selections_deduplicate_or_reject_after_resolving_to_one_oem_item() {
        let ipc = parse_selection_test_ipc("");
        let mut selections = [
            selection("Power.R1", Some("R1")),
            selection("Alternate.R1", Some("R1")),
        ];

        let resolved = resolve_selections(&ipc, &selections).unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].oem_design_number, "PATH_PART");

        selections[1].mpn = "DIFFERENT-MPN".to_string();
        assert!(resolve_selections(&ipc, &selections).is_err());
        selections[1].mpn = selections[0].mpn.clone();
        selections[1].external_part_identifier = Some("different:identity".to_string());
        assert!(resolve_selections(&ipc, &selections).is_err());
    }

    #[test]
    fn hydration_preserves_provenance_and_is_idempotent_and_schema_valid() {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("input.xml");
        let selections = temp.path().join("selections.json");
        let first_output = temp.path().join("first.xml");
        let second_output = temp.path().join("second.xml");
        let cleared_output = temp.path().join("cleared.xml");

        let input_xml = schema_valid_hydration_ipc();
        ipc2581::Ipc2581::validate(input_xml).unwrap();
        std::fs::write(&input, input_xml).unwrap();
        std::fs::write(
            &selections,
            r#"[
  {"path":"Power.R1","manufacturerId":"550E8400E29B41D4A716446655440000","manufacturer":"New & <Manufacturer>","mpn":"MPN<&\"","distributor":"Digi-Key & Partners","distributorPartId":"SKU<&\"","externalPartLibrary":"Cofactr & Co","externalPartLibraryMpn":"COFACTR-MPN<&\"","externalPartIdentifier":"cofactr:CPID<&\"","mpnAliases":["MPN-ALIAS<&\"","Second-Alias"]},
  {"path":"Power.C1","manufacturerId":"AAAAAAAA-BBBB-4CCC-8DDD-EEEEEEEEEEEE","manufacturer":"New Manufacturer","mpn":"NEW-MPN","distributor":"New Distributor","distributorPartId":"NEW-SKU"}
]"#,
        )
        .unwrap();

        execute_selections(&input, &selections, &first_output).unwrap();
        let first_xml = std::fs::read_to_string(&first_output).unwrap();
        assert_hydrated_output(&first_xml);
        assert!(first_xml.contains("New &amp; &lt;Manufacturer&gt;"));
        assert!(first_xml.contains("MPN&lt;&amp;&quot;"));
        assert!(first_xml.contains("SKU&lt;&amp;&quot;"));

        execute_selections(&first_output, &selections, &second_output).unwrap();
        let second_xml = std::fs::read_to_string(&second_output).unwrap();
        assert_hydrated_output(&second_xml);

        std::fs::write(
            &selections,
            r#"[{"path":"Power.R1","manufacturerId":"550E8400E29B41D4A716446655440000","manufacturer":"New & <Manufacturer>","mpn":"MPN<&\"","distributor":"Digi-Key & Partners","distributorPartId":"SKU<&\""}]"#,
        )
        .unwrap();
        execute_selections(&second_output, &selections, &cleared_output).unwrap();
        let cleared_xml = std::fs::read_to_string(&cleared_output).unwrap();
        ipc2581::Ipc2581::validate(&cleared_xml).unwrap();
        let cleared_doc = ipc2581::edit::Doc::parse(&cleared_xml).unwrap();
        let selected_vmpn = cleared_doc
            .find_all("AvlVmpn")
            .into_iter()
            .find(|&vmpn| {
                cleared_doc
                    .child(vmpn, "AvlMpn")
                    .is_some_and(|mpn| cleared_doc.attr(mpn, "name") == Some("MPN<&\""))
            })
            .unwrap();
        assert_eq!(cleared_doc.attr(selected_vmpn, "evplVendor"), None);
        assert_eq!(cleared_doc.attr(selected_vmpn, "evplMpn"), None);
        let identity_textuals = cleared_doc
            .find_all("Textual")
            .into_iter()
            .filter(|&node| {
                cleared_doc.attr(node, "definitionSource") == Some(PART_IDENTITY_DEFINITION_SOURCE)
            })
            .collect::<Vec<_>>();
        assert_eq!(identity_textuals.len(), 1);
        assert_eq!(
            cleared_doc.attr(identity_textuals[0], "textualCharacteristicName"),
            Some("FutureIdentityField")
        );
    }
}
