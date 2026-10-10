use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::{Context, Result, bail};
use pcb_sch::{
    ATTR_SYMBOL_FORMAT_VERSION, AttributeValue, Instance, InstanceKind, InstanceRef, Schematic,
};

use crate::{Symbol, SymbolDefinition, SymbolSlotKey, canonical_component_path, symbol};

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

pub(crate) fn component_symbols(
    netlist: &Schematic,
) -> Result<Vec<(SymbolSlotKey, Option<SymbolDefinition>)>> {
    let mut slots = Vec::new();
    let mut definitions = HashMap::<&str, (SymbolDefinition, Vec<u32>)>::new();
    for (instance_ref, instance) in &netlist.instances {
        if instance.kind != InstanceKind::Component {
            continue;
        }
        let component_path = canonical_component_path(&instance_ref.instance_path)
            .context("component instance has no canonical path")?;
        let (definition, units): (_, &[u32]) =
            if let Some(raw) = raw_symbol_definition(netlist, "component", &instance.attributes)? {
                let (definition, units) = match definitions.entry(raw) {
                    std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        let definition = SymbolDefinition::from_kicad_symbol_sexpr(raw)
                            .context("failed to parse component symbol definition")?;
                        let units = symbol::ParsedSymbolDefinition::parse(&definition)?
                            .unit_indices()
                            .to_vec();
                        entry.insert((definition, units))
                    }
                };
                (Some(&*definition), units)
            } else {
                (None, &[1])
            };
        for &unit in units {
            let slot = SymbolSlotKey::new(component_path.clone(), unit)
                .context("component symbol slot has an empty path")?;
            slots.push((slot, definition.cloned()));
        }
    }
    slots.sort_by(|a, b| a.0.cmp(&b.0));
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
