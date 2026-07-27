use super::*;
use anyhow::Context;
use pcb_diode_api::{RegistrySearchClient, normalize_mpn_for_lookup};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub(super) const MPN_PROPERTY_KEYS: &[&str] = &[
    "mpn",
    "manufacturer_part_number",
    "manufacturer part number",
    "mfr part number",
    "manufacturer_pn",
    "part number",
    // Common KiCad/SnapEDA field aliases.
    "mp",
    "snapeda_pn",
];

pub(super) const MANUFACTURER_PROPERTY_KEYS: &[&str] = &[
    "manufacturer",
    "manufacturer_name",
    "manufacturer name",
    "mfr",
    "mfr_name",
    "mfg",
];

pub(super) fn explicit_mpn(component: &ImportComponentData) -> Option<&str> {
    component
        .best_properties()
        .and_then(|properties| find_property_ci(properties, MPN_PROPERTY_KEYS))
}

pub(super) fn explicit_manufacturer(component: &ImportComponentData) -> Option<&str> {
    component
        .best_properties()
        .and_then(|properties| find_property_ci(properties, MANUFACTURER_PROPERTY_KEYS))
}

/// Folding key for one manufacturer name: the trimmed, ASCII-lowercased form. Callers that group
/// manufacturers must key on this and keep an original spelling separately for output; it is never
/// itself a display value.
pub(super) fn manufacturer_key(manufacturer: &str) -> String {
    manufacturer.trim().to_ascii_lowercase()
}

/// Compare two manufacturer names for identity. The reuse gate, the manufacturer enrichment, and
/// the staged-board backstop all decide substitution on this rule, so they must not diverge.
pub(super) fn manufacturers_match(left: &str, right: &str) -> bool {
    manufacturer_key(left) == manufacturer_key(right)
}

fn is_property_alias(key: &str, aliases: &[&str]) -> bool {
    aliases.iter().any(|alias| key.eq_ignore_ascii_case(alias))
}

fn has_property_alias(properties: &BTreeMap<String, String>, aliases: &[&str]) -> bool {
    properties.keys().any(|key| is_property_alias(key, aliases))
}

/// Merge inherited embedded-symbol properties into each KiCad symbol instance. KiCad stores many
/// sourcing fields on the library symbol and writes only instance overrides beside the schematic
/// symbol. Instance values always take precedence, including when they use a different field alias.
pub(super) fn inherit_embedded_symbol_properties(
    components: &mut BTreeMap<KiCadUuidPathKey, ImportComponentData>,
    schematic_lib_symbols: &BTreeMap<KiCadLibId, String>,
) {
    #[derive(Clone)]
    struct InheritedProperties {
        properties: BTreeMap<String, String>,
        mpn: Option<String>,
        manufacturer: Option<String>,
    }

    let mut by_lib_id = BTreeMap::new();
    for (lib_id, symbol_text) in schematic_lib_symbols {
        let library_text =
            pcb_eda::kicad::symbol_library::wrap_symbol_as_library(symbol_text, "pcb import");
        let parsed = match pcb_eda::SymbolLibrary::from_string(&library_text, "kicad_sym") {
            Ok(parsed) => parsed,
            Err(error) => {
                log::debug!("Could not read inherited properties from {lib_id}: {error:#}");
                continue;
            }
        };
        let Some(symbol) = parsed.first_symbol() else {
            continue;
        };
        by_lib_id.insert(
            lib_id.clone(),
            InheritedProperties {
                properties: symbol
                    .properties
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
                mpn: symbol.mpn.clone(),
                manufacturer: symbol.manufacturer.clone(),
            },
        );
    }

    for component in components.values_mut() {
        let Some(schematic) = component.schematic.as_mut() else {
            continue;
        };
        let inherited = schematic.units.values().find_map(|unit| {
            unit.lib_name
                .as_deref()
                .map(|name| KiCadLibId::from(name.to_string()))
                .and_then(|id| by_lib_id.get(&id))
                .or_else(|| unit.lib_id.as_ref().and_then(|id| by_lib_id.get(id)))
        });
        let Some(inherited) = inherited.cloned() else {
            continue;
        };

        for unit in schematic.units.values_mut() {
            let instance_has_mpn = has_property_alias(&unit.properties, MPN_PROPERTY_KEYS);
            let instance_has_manufacturer =
                has_property_alias(&unit.properties, MANUFACTURER_PROPERTY_KEYS);
            for (key, value) in &inherited.properties {
                if (instance_has_mpn && is_property_alias(key, MPN_PROPERTY_KEYS))
                    || (instance_has_manufacturer
                        && is_property_alias(key, MANUFACTURER_PROPERTY_KEYS))
                {
                    continue;
                }
                unit.properties
                    .entry(key.clone())
                    .or_insert_with(|| value.clone());
            }
            if !instance_has_mpn
                && find_property_ci(&unit.properties, MPN_PROPERTY_KEYS).is_none()
                && let Some(mpn) = &inherited.mpn
                && !mpn.trim().is_empty()
            {
                unit.properties.insert("MPN".to_string(), mpn.clone());
            }
            if !instance_has_manufacturer
                && find_property_ci(&unit.properties, MANUFACTURER_PROPERTY_KEYS).is_none()
                && let Some(manufacturer) = &inherited.manufacturer
                && !manufacturer.trim().is_empty()
            {
                unit.properties
                    .insert("Manufacturer".to_string(), manufacturer.clone());
            }
        }
    }
}

pub(super) fn lookup_cached_registry_mpns(
    components: &BTreeMap<KiCadUuidPathKey, ImportComponentData>,
    workspace_root: &Path,
) -> ImportRegistryMpnLookup {
    let mut source_mpn_by_normalized: BTreeMap<String, String> = BTreeMap::new();
    for component in components.values() {
        let Some(mpn) = explicit_mpn(component) else {
            continue;
        };
        let normalized = normalize_mpn_for_lookup(mpn);
        if normalized.is_empty() {
            continue;
        }
        source_mpn_by_normalized
            .entry(normalized)
            .and_modify(|current| {
                if mpn < current.as_str() {
                    *current = mpn.to_string();
                }
            })
            .or_insert_with(|| mpn.to_string());
    }

    let queried_mpns = source_mpn_by_normalized.values().cloned().collect();
    if source_mpn_by_normalized.is_empty() {
        return ImportRegistryMpnLookup {
            queried_mpns,
            ..ImportRegistryMpnLookup::default()
        };
    }

    let scope =
        match pcb_diode_api::registry::download::cached_registry_search_scope(Some(workspace_root))
        {
            Ok(Some(scope)) => scope,
            Ok(None) => {
                return ImportRegistryMpnLookup {
                    cached_index_available: false,
                    queried_mpns,
                    ..ImportRegistryMpnLookup::default()
                };
            }
            Err(error) => {
                return ImportRegistryMpnLookup {
                    cached_index_available: false,
                    queried_mpns,
                    lookup_error: Some(format!(
                        "Failed to locate cached registry indexes: {error:#}"
                    )),
                    ..ImportRegistryMpnLookup::default()
                };
            }
        };
    let client = match RegistrySearchClient::open_cached_scope(&scope)
        .context("Failed to open cached Diode registry indexes for MPN lookup")
    {
        Ok(client) => client,
        Err(error) => {
            return ImportRegistryMpnLookup {
                cached_index_available: true,
                queried_mpns,
                lookup_error: Some(format!("{error:#}")),
                ..ImportRegistryMpnLookup::default()
            };
        }
    };

    let mut candidates_by_normalized =
        match client.find_component_candidates_by_mpns(source_mpn_by_normalized.keys()) {
            Ok(candidates) => candidates,
            Err(error) => {
                return ImportRegistryMpnLookup {
                    cached_index_available: true,
                    queried_mpns,
                    lookup_error: Some(format!("Failed exact registry MPN lookup: {error:#}")),
                    ..ImportRegistryMpnLookup::default()
                };
            }
        };
    let mut candidates_by_mpn = BTreeMap::new();
    for (normalized, source_mpn) in &source_mpn_by_normalized {
        let mut seen = BTreeSet::new();
        let candidates = candidates_by_normalized
            .remove(normalized)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|candidate| {
                let key = (
                    candidate.registry.id.clone(),
                    candidate.module_id,
                    candidate.symbol_id,
                );
                seen.insert(key).then_some(ImportRegistryMpnCandidate {
                    registry_id: candidate.registry.id,
                    registry_mpn: candidate.mpn,
                    manufacturer: candidate.manufacturer,
                    footprint: candidate.footprint,
                    module_url: candidate.module_url,
                    module_version: candidate.module_version,
                    entrypoints: candidate.entrypoints,
                    symbol_preferred: candidate.symbol_preferred,
                    module_preferred: candidate.module_preferred,
                })
            })
            .collect::<Vec<_>>();
        if !candidates.is_empty() {
            candidates_by_mpn.insert(source_mpn.clone(), candidates);
        }
    }

    ImportRegistryMpnLookup {
        cached_index_available: true,
        queried_mpns,
        candidates_by_mpn,
        lookup_error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn component_with_properties(properties: BTreeMap<String, String>) -> ImportComponentData {
        ImportComponentData {
            netlist: ImportNetlistComponent {
                refdes: KiCadRefDes::from("U1".to_string()),
                value: None,
                footprint: None,
                sheetpath_names: None,
                unit_pcb_paths: Vec::new(),
            },
            schematic: Some(ImportSchematicComponent {
                units: BTreeMap::from([(
                    KiCadUuidPathKey {
                        sheetpath_tstamps: "/".to_string(),
                        symbol_uuid: "u1".to_string(),
                    },
                    ImportSchematicUnit {
                        lib_name: None,
                        lib_id: None,
                        unit: Some(1),
                        at: None,
                        mirror: None,
                        in_bom: None,
                        on_board: None,
                        dnp: None,
                        exclude_from_sim: None,
                        instance_path: None,
                        properties,
                        pins: None,
                    },
                )]),
            }),
            layout: None,
        }
    }

    #[test]
    fn reads_common_explicit_mpn_aliases() {
        for key in ["MPN", "Manufacturer_Part_Number", "MP", "SnapEDA_PN"] {
            let component = component_with_properties(BTreeMap::from([(
                key.to_string(),
                " SAM-M10Q-00B ".to_string(),
            )]));
            assert_eq!(explicit_mpn(&component), Some("SAM-M10Q-00B"));
        }
    }

    #[test]
    fn inherits_sourcing_from_embedded_symbol_without_overriding_instance() {
        let mut component = component_with_properties(BTreeMap::from([(
            "MP".to_string(),
            "INSTANCE-MPN".to_string(),
        )]));
        component
            .schematic
            .as_mut()
            .unwrap()
            .units
            .values_mut()
            .next()
            .unwrap()
            .lib_name = Some("Imported:Device".to_string());
        let mut components = BTreeMap::from([(
            KiCadUuidPathKey {
                sheetpath_tstamps: "/".to_string(),
                symbol_uuid: "u1".to_string(),
            },
            component,
        )]);
        let symbols = BTreeMap::from([(
            KiCadLibId::from("Imported:Device".to_string()),
            r#"(symbol "Imported:Device"
                (property "Reference" "U" (at 0 0 0) (effects (font (size 1.27 1.27))))
                (property "Value" "Device" (at 0 0 0) (effects (font (size 1.27 1.27))))
                (property "Footprint" "Package:Device" (at 0 0 0) (effects (font (size 1.27 1.27)) hide))
                (property "Datasheet" "" (at 0 0 0) (effects (font (size 1.27 1.27)) hide))
                (property "Manufacturer_Part_Number" "LIBRARY-MPN" (at 0 0 0) (effects (font (size 1.27 1.27)) hide))
                (property "Manufacturer" "Acme" (at 0 0 0) (effects (font (size 1.27 1.27)) hide))
            )"#
            .to_string(),
        )]);

        inherit_embedded_symbol_properties(&mut components, &symbols);
        let component = components.values().next().unwrap();
        assert_eq!(explicit_mpn(component), Some("INSTANCE-MPN"));
        assert_eq!(explicit_manufacturer(component), Some("Acme"));

        let mut blank_override =
            component_with_properties(BTreeMap::from([("MP".to_string(), String::new())]));
        blank_override
            .schematic
            .as_mut()
            .unwrap()
            .units
            .values_mut()
            .next()
            .unwrap()
            .lib_name = Some("Imported:Device".to_string());
        let mut blank_components = BTreeMap::from([(
            KiCadUuidPathKey {
                sheetpath_tstamps: "/".to_string(),
                symbol_uuid: "blank".to_string(),
            },
            blank_override,
        )]);
        inherit_embedded_symbol_properties(&mut blank_components, &symbols);
        assert_eq!(
            explicit_mpn(blank_components.values().next().unwrap()),
            None
        );
    }

    #[test]
    fn manufacturer_identity_ignores_surrounding_space_and_ascii_case() {
        assert_eq!(manufacturer_key(" Acme "), "acme");
        assert!(manufacturers_match(" Acme ", "acme"));
        assert!(manufacturers_match("ACME", "Acme"));
        // Folding must not collapse distinct names.
        assert_ne!(manufacturer_key("Acme"), manufacturer_key("Acme Inc"));
        assert!(!manufacturers_match("Acme", "Acme Inc"));
    }

    #[test]
    fn does_not_treat_value_as_an_explicit_mpn() {
        let mut component = component_with_properties(BTreeMap::from([(
            "Value".to_string(),
            "SAM-M10Q-00B".to_string(),
        )]));
        component.netlist.value = Some("SAM-M10Q-00B".to_string());
        assert_eq!(explicit_mpn(&component), None);
    }
}
