use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::{Context, Result, bail};
use pcb_sch::{
    ATTR_SYMBOL_FORMAT_VERSION, AttributeValue, Instance, InstanceKind, InstanceRef, Schematic,
};
use pcb_sexpr::Sexpr;

use crate::{
    SchDocument, SchItem, Symbol, SymbolDefinition, SymbolSlotKey, canonical_component_path,
    connectivity::kicad::page_instances, symbol,
};

pub(crate) const SYMBOL_VALUE_ATTR: &str = "__symbol_value";
pub(crate) const SYMBOL_PATH_ATTR: &str = "symbol_path";
const KICAD_10_SYMBOL_LIB_VERSION: i32 = 20251024;

pub(crate) fn validate_symbol_library_versions(netlist: &Schematic) -> Result<()> {
    for (instance_ref, instance) in &netlist.instances {
        if instance.kind != InstanceKind::Component {
            continue;
        }
        let component_path = canonical_component_path(&instance_ref.instance_path)
            .context("component instance has no canonical path")?;
        validate_symbol_library_version(
            &format!("component '{component_path}'"),
            &instance.attributes,
        )?;
    }
    for net in netlist.nets.values() {
        validate_symbol_library_version(&format!("net '{}'", net.name), &net.properties)?;
    }
    Ok(())
}

pub(crate) fn validate_symbol_library_version(
    owner: &str,
    attributes: &HashMap<String, AttributeValue>,
) -> Result<()> {
    let has_symbol_value = string_attribute(owner, attributes, SYMBOL_VALUE_ATTR)?.is_some();
    let has_symbol_path = string_attribute(owner, attributes, SYMBOL_PATH_ATTR)?.is_some();
    if !has_symbol_value && !has_symbol_path {
        return Ok(());
    }
    let version = match attributes.get(ATTR_SYMBOL_FORMAT_VERSION) {
        Some(AttributeValue::Number(version))
            if version.fract() == 0.0
                && *version >= i32::MIN as f64
                && *version <= i32::MAX as f64 =>
        {
            *version as i32
        }
        Some(_) => bail!("{owner} has an invalid KiCad symbol-library format version"),
        None => bail!(
            "{owner} does not declare its KiCad symbol-library format version; pcb apply supports only KiCad 10+ symbols"
        ),
    };
    if version < KICAD_10_SYMBOL_LIB_VERSION {
        bail!(
            "{owner} uses KiCad symbol-library format version {version}; pcb apply supports KiCad 10+ symbols (format version {KICAD_10_SYMBOL_LIB_VERSION} or newer)"
        );
    }
    Ok(())
}

pub(crate) fn component_symbol_slots(netlist: &Schematic) -> Result<Vec<SymbolSlotKey>> {
    let mut slots = Vec::new();
    let mut units_by_definition = HashMap::new();
    for (instance_ref, instance) in &netlist.instances {
        if instance.kind != InstanceKind::Component {
            continue;
        }
        let component_path = canonical_component_path(&instance_ref.instance_path)
            .context("component instance has no canonical path")?;
        for &unit in component_unit_indices(netlist, instance, &mut units_by_definition)? {
            let slot = SymbolSlotKey::new(component_path.clone(), unit)
                .context("component symbol slot has an empty path")?;
            slots.push(slot);
        }
    }
    slots.sort();
    Ok(slots)
}

pub(crate) fn component_instances(netlist: &Schematic) -> Result<BTreeMap<String, &Instance>> {
    let mut result = BTreeMap::new();
    for (instance_ref, instance) in &netlist.instances {
        if instance.kind != InstanceKind::Component {
            continue;
        }
        let path = canonical_component_path(&instance_ref.instance_path)
            .context("component instance has no canonical path")?;
        if result.insert(path.clone(), instance).is_some() {
            bail!("netlist contains duplicate component path '{path}'");
        }
    }
    Ok(result)
}

/// Keep native annotations consistent with the refreshed Reference fields.
/// KiCad looks up instances by sheet UUID path, even after a project rename;
/// annotations outside this document's hierarchy belong to other instances.
pub(crate) fn sync_symbol_instance_references(
    document: &mut SchDocument,
    slots: &BTreeSet<SymbolSlotKey>,
) -> Result<()> {
    let mut paths_by_page = BTreeMap::<String, BTreeSet<String>>::new();
    for instance in page_instances(document)? {
        paths_by_page
            .entry(instance.page.id.clone())
            .or_default()
            .insert(format!("/{}", instance.id));
    }
    for page in &mut document.pages {
        let Some(paths) = paths_by_page.get(&page.id) else {
            continue;
        };
        for item in &mut page.items {
            let SchItem::Symbol(symbol) = item else {
                continue;
            };
            let Some(slot) = symbol
                .field_value("Path")
                .and_then(|path| SymbolSlotKey::new(path, symbol.unit))
                .filter(|slot| slots.contains(slot))
            else {
                continue;
            };
            let reference = symbol
                .reference()
                .with_context(|| format!("managed symbol '{slot}' has no Reference field"))?
                .to_string();
            for instances in &mut symbol.unsupported {
                let Some(projects) = instances
                    .as_list_mut()
                    .filter(|items| items.first().and_then(Sexpr::as_sym) == Some("instances"))
                else {
                    continue;
                };
                for project in projects {
                    let Some(annotations) = project
                        .as_list_mut()
                        .filter(|items| items.first().and_then(Sexpr::as_sym) == Some("project"))
                    else {
                        continue;
                    };
                    for annotation in annotations {
                        let Some(fields) = annotation.as_list_mut().filter(|items| {
                            items.first().and_then(Sexpr::as_sym) == Some("path")
                                && items
                                    .get(1)
                                    .and_then(Sexpr::as_str)
                                    .is_some_and(|path| paths.contains(path))
                        }) else {
                            continue;
                        };
                        for field in fields {
                            if let Some(value) = field
                                .as_list_mut()
                                .filter(|items| {
                                    items.first().and_then(Sexpr::as_sym) == Some("reference")
                                })
                                .and_then(|items| items.get_mut(1))
                                && value.as_str() != Some(&reference)
                            {
                                *value = Sexpr::string(&reference);
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NetlistDerivedSymbolProperties {
    dnp: bool,
    in_bom: bool,
    on_board: bool,
    in_pos_files: bool,
}

impl NetlistDerivedSymbolProperties {
    fn from_instance(instance: &Instance) -> Self {
        Self {
            dnp: instance.dnp(),
            in_bom: !instance.skip_bom(),
            // Excluding placement output does not remove the component from the board.
            on_board: true,
            in_pos_files: !instance.skip_pos(),
        }
    }

    fn from_symbol(symbol: &Symbol) -> Self {
        Self {
            dnp: symbol.dnp,
            in_bom: symbol.in_bom,
            on_board: symbol.on_board,
            in_pos_files: symbol.in_pos_files,
        }
    }
}

pub(crate) fn sync_netlist_derived_symbol_properties(
    symbol: &mut Symbol,
    instance: &Instance,
) -> bool {
    let properties = NetlistDerivedSymbolProperties::from_instance(instance);
    let changed = NetlistDerivedSymbolProperties::from_symbol(symbol) != properties;
    symbol.dnp = properties.dnp;
    symbol.in_bom = properties.in_bom;
    symbol.on_board = properties.on_board;
    symbol.in_pos_files = properties.in_pos_files;
    changed
}

pub(crate) fn port_pad_numbers(netlist: &Schematic, port: &InstanceRef) -> BTreeSet<String> {
    let Some(instance) = netlist.instances.get(port) else {
        return BTreeSet::new();
    };
    let Some(AttributeValue::Array(values)) = instance.attributes.get("pads") else {
        return BTreeSet::new();
    };
    values
        .iter()
        .filter_map(|value| match value {
            AttributeValue::String(value) | AttributeValue::Port(value) => Some(value.clone()),
            AttributeValue::Number(value) => Some(value.to_string()),
            _ => None,
        })
        .collect()
}

fn component_unit_indices<'a, 'cache>(
    netlist: &'a Schematic,
    instance: &'a Instance,
    units_by_definition: &'cache mut HashMap<&'a str, Vec<u32>>,
) -> Result<&'cache [u32]> {
    let Some(raw) = raw_symbol_definition(netlist, "component", &instance.attributes)? else {
        return Ok(&[1]);
    };
    // Borrow text from the immutable netlist; retain only units for this invocation.
    let units = match units_by_definition.entry(raw) {
        std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
        std::collections::hash_map::Entry::Vacant(entry) => {
            let definition = SymbolDefinition::from_kicad_symbol_sexpr(raw)
                .context("failed to parse component symbol definition")?;
            entry.insert(
                symbol::ParsedSymbolDefinition::parse(&definition)?
                    .unit_indices()
                    .to_vec(),
            )
        }
    };
    Ok(units)
}

pub(crate) fn component_symbol_definition(
    netlist: &Schematic,
    instance: &Instance,
) -> Result<Option<SymbolDefinition>> {
    symbol_definition(netlist, "component", &instance.attributes)
}

pub(crate) fn symbol_definition(
    netlist: &Schematic,
    owner: &str,
    attributes: &HashMap<String, AttributeValue>,
) -> Result<Option<SymbolDefinition>> {
    raw_symbol_definition(netlist, owner, attributes)?
        .map(|raw| {
            SymbolDefinition::from_kicad_symbol_sexpr(raw)
                .with_context(|| format!("failed to parse {owner} symbol definition"))
        })
        .transpose()
}

fn raw_symbol_definition<'a>(
    netlist: &'a Schematic,
    owner: &str,
    attributes: &'a HashMap<String, AttributeValue>,
) -> Result<Option<&'a str>> {
    let raw = if let Some(raw) = string_attribute(owner, attributes, SYMBOL_VALUE_ATTR)? {
        Some(raw)
    } else if let Some(path) = string_attribute(owner, attributes, SYMBOL_PATH_ATTR)? {
        Some(
            netlist
                .symbols
                .get(path)
                .with_context(|| format!("symbol_path {path} is absent from netlist symbols"))?
                .as_str(),
        )
    } else {
        None
    };
    Ok(raw)
}

pub(crate) fn attribute_string<'a>(instance: &'a Instance, key: &str) -> Result<Option<&'a str>> {
    string_attribute("component", &instance.attributes, key)
}

fn string_attribute<'a>(
    owner: &str,
    attributes: &'a HashMap<String, AttributeValue>,
    key: &str,
) -> Result<Option<&'a str>> {
    match attributes.get(key) {
        None => Ok(None),
        Some(AttributeValue::String(value)) => Ok(Some(value)),
        Some(_) => bail!("{owner} attribute {key} must be a string"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pcb_sch::ModuleRef;

    const MULTI: &str = r#"(symbol "Multi" (symbol "Multi_3_1") (symbol "Multi_0_1") (symbol "Multi_1_1") (symbol "Multi_3_2"))"#;
    const OTHER: &str = r#"(symbol "Other" (symbol "Other_2_1"))"#;

    fn component(key: &str, value: AttributeValue) -> Instance {
        let mut instance =
            Instance::new(ModuleRef::new("test.zen", "Test"), InstanceKind::Component);
        instance.attributes.insert(key.into(), value);
        instance
    }

    #[test]
    fn repeated_raw_definitions_share_units_and_slots_are_sorted() -> Result<()> {
        let mut netlist = Schematic::new();
        netlist.symbols.insert("lib:Multi".into(), MULTI.into());
        netlist.symbols.insert("alias:Multi".into(), MULTI.into());
        for (path, instance) in [
            (
                "Z",
                component(SYMBOL_VALUE_ATTR, AttributeValue::String(MULTI.into())),
            ),
            (
                "B",
                component(SYMBOL_PATH_ATTR, AttributeValue::String("lib:Multi".into())),
            ),
            (
                "C",
                component(
                    SYMBOL_PATH_ATTR,
                    AttributeValue::String("alias:Multi".into()),
                ),
            ),
            (
                "A",
                component(SYMBOL_VALUE_ATTR, AttributeValue::String(OTHER.into())),
            ),
            (
                "D",
                Instance::new(ModuleRef::new("test.zen", "Test"), InstanceKind::Component),
            ),
        ] {
            netlist.instances.insert(
                InstanceRef::new(instance.type_ref.clone(), vec![path.into()]),
                instance,
            );
        }
        let mut cache = HashMap::new();
        for instance in netlist.instances.values() {
            component_unit_indices(&netlist, instance, &mut cache)?;
        }
        assert_eq!(cache.len(), 2);
        assert_eq!(cache[MULTI], [1, 3]);
        assert_eq!(cache[OTHER], [2]);
        let expected = [
            ("A", 2),
            ("B", 1),
            ("B", 3),
            ("C", 1),
            ("C", 3),
            ("D", 1),
            ("Z", 1),
            ("Z", 3),
        ]
        .into_iter()
        .map(|(path, unit)| SymbolSlotKey::new(path, unit).unwrap())
        .collect::<Vec<_>>();
        assert_eq!(component_symbol_slots(&netlist)?, expected);
        Ok(())
    }

    #[test]
    fn cache_preserves_attribute_validation_precedence_and_parse_errors() -> Result<()> {
        let netlist = Schematic::new();
        let mut cache = HashMap::new();
        let mut inline = component(SYMBOL_VALUE_ATTR, AttributeValue::String(MULTI.into()));
        inline
            .attributes
            .insert(SYMBOL_PATH_ATTR.into(), AttributeValue::Number(1.0));
        assert_eq!(
            component_unit_indices(&netlist, &inline, &mut cache)?,
            [1, 3]
        );
        for key in [SYMBOL_VALUE_ATTR, SYMBOL_PATH_ATTR] {
            let bad = component(key, AttributeValue::Number(1.0));
            assert_eq!(
                component_unit_indices(&netlist, &bad, &mut HashMap::new())
                    .unwrap_err()
                    .to_string(),
                format!("component attribute {key} must be a string")
            );
        }
        let missing = component(SYMBOL_PATH_ATTR, AttributeValue::String("missing".into()));
        assert_eq!(
            component_unit_indices(&netlist, &missing, &mut HashMap::new())
                .unwrap_err()
                .to_string(),
            "symbol_path missing is absent from netlist symbols"
        );
        let malformed = component(SYMBOL_VALUE_ATTR, AttributeValue::String("(".into()));
        assert_eq!(
            component_unit_indices(&netlist, &malformed, &mut HashMap::new())
                .unwrap_err()
                .to_string(),
            "failed to parse component symbol definition"
        );
        Ok(())
    }
}
