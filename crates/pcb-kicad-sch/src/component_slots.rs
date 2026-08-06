use anyhow::{Context, Result, bail};
use pcb_sch::{AttributeValue, Instance, InstanceKind, Schematic};

use crate::{SymbolDefinition, SymbolSlotKey, canonical_component_path, symbol};

const SYMBOL_VALUE_ATTR: &str = "__symbol_value";
const SYMBOL_PATH_ATTR: &str = "symbol_path";

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
    Ok(slots)
}

fn component_unit_indices(netlist: &Schematic, instance: &Instance) -> Result<Vec<u32>> {
    match component_symbol_definition(netlist, instance)? {
        Some(definition) => symbol::unit_indices(&definition),
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

fn attribute_string<'a>(instance: &'a Instance, key: &str) -> Result<Option<&'a str>> {
    match instance.attributes.get(key) {
        None => Ok(None),
        Some(AttributeValue::String(value)) => Ok(Some(value)),
        Some(_) => bail!("component attribute {key} must be a string"),
    }
}
