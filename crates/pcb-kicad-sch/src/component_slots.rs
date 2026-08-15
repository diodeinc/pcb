use std::collections::{BTreeSet, HashMap};

use anyhow::{Context, Result, bail};
use pcb_sch::{
    ATTR_SYMBOL_FORMAT_VERSION, AttributeValue, Instance, InstanceKind, InstanceRef, Schematic,
};

use crate::{SymbolDefinition, SymbolSlotKey, canonical_component_path, symbol};

const SYMBOL_VALUE_ATTR: &str = "__symbol_value";
const SYMBOL_PATH_ATTR: &str = "symbol_path";
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

fn validate_symbol_library_version(
    owner: &str,
    attributes: &HashMap<String, AttributeValue>,
) -> Result<()> {
    let has_symbol_value =
        validate_optional_string_attribute(owner, attributes, SYMBOL_VALUE_ATTR)?;
    let has_symbol_path = validate_optional_string_attribute(owner, attributes, SYMBOL_PATH_ATTR)?;
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

fn validate_optional_string_attribute(
    owner: &str,
    attributes: &HashMap<String, AttributeValue>,
    key: &str,
) -> Result<bool> {
    match attributes.get(key) {
        None => Ok(false),
        Some(AttributeValue::String(_)) => Ok(true),
        Some(_) => bail!("{owner} attribute {key} must be a string"),
    }
}

pub(crate) fn component_symbol_slots(netlist: &Schematic) -> Result<Vec<SymbolSlotKey>> {
    let mut slots = Vec::new();
    for (instance_ref, instance) in &netlist.instances {
        if instance.kind != InstanceKind::Component {
            continue;
        }
        let component_path = canonical_component_path(&instance_ref.instance_path)
            .context("component instance has no canonical path")?;
        for unit in component_unit_indices(netlist, instance)? {
            let slot = SymbolSlotKey::new(component_path.clone(), unit)
                .context("component symbol slot has an empty path")?;
            slots.push(slot);
        }
    }
    slots.sort();
    Ok(slots)
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

fn component_unit_indices(netlist: &Schematic, instance: &Instance) -> Result<Vec<u32>> {
    match component_symbol_definition(netlist, instance)? {
        Some(definition) => Ok(symbol::ParsedSymbolDefinition::parse(&definition)?
            .unit_indices()
            .to_vec()),
        None => Ok(vec![1]),
    }
}

pub(crate) fn component_symbol_definition(
    netlist: &Schematic,
    instance: &Instance,
) -> Result<Option<SymbolDefinition>> {
    let raw = if let Some(raw) = attribute_string(instance, SYMBOL_VALUE_ATTR)? {
        Some(raw.to_string())
    } else if let Some(path) = attribute_string(instance, SYMBOL_PATH_ATTR)? {
        Some(
            netlist
                .symbols
                .get(path)
                .with_context(|| format!("symbol_path {path} is absent from netlist symbols"))?
                .clone(),
        )
    } else {
        None
    };
    raw.map(|raw| {
        SymbolDefinition::from_kicad_symbol_sexpr(&raw)
            .context("failed to parse component symbol definition")
    })
    .transpose()
}

pub(crate) fn attribute_string<'a>(instance: &'a Instance, key: &str) -> Result<Option<&'a str>> {
    match instance.attributes.get(key) {
        None => Ok(None),
        Some(AttributeValue::String(value)) => Ok(Some(value)),
        Some(_) => bail!("component attribute {key} must be a string"),
    }
}
