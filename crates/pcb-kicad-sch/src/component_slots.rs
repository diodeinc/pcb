use anyhow::{Context, Result};
use pcb_sch::{AttributeValue, Instance, InstanceKind, Schematic};

use crate::{SymbolDefinition, SymbolSlotKey, canonical_component_path, symbol};

const SYMBOL_VALUE_ATTR: &str = "__symbol_value";
const SYMBOL_PATH_ATTR: &str = "symbol_path";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ComponentSymbolSlot {
    pub component_path: String,
    pub unit: u32,
}

pub(crate) fn component_symbol_slots(netlist: &Schematic) -> Vec<ComponentSymbolSlot> {
    netlist
        .instances
        .iter()
        .filter(|(_, instance)| instance.kind == InstanceKind::Component)
        .flat_map(|(instance_ref, instance)| {
            let Some(component_path) = canonical_component_path(&instance_ref.instance_path) else {
                return Vec::new();
            };
            component_unit_indices(netlist, instance)
                .into_iter()
                .filter_map(|unit| {
                    let slot = SymbolSlotKey::new(component_path.clone(), unit)?;
                    Some(ComponentSymbolSlot {
                        component_path: slot.component_path().to_string(),
                        unit: slot.unit(),
                    })
                })
                .collect()
        })
        .collect()
}

fn component_unit_indices(netlist: &Schematic, instance: &Instance) -> Vec<u32> {
    component_symbol_definition(netlist, instance)
        .ok()
        .flatten()
        .map(|definition| symbol::unit_indices(&definition))
        .unwrap_or_else(|| vec![1])
}

pub(crate) fn component_symbol_definition(
    netlist: &Schematic,
    instance: &Instance,
) -> Result<Option<SymbolDefinition>> {
    let raw = attribute_string(instance, SYMBOL_VALUE_ATTR).or_else(|| {
        let path = attribute_string(instance, SYMBOL_PATH_ATTR)?;
        netlist.symbols.get(&path).cloned()
    });
    raw.map(|raw| {
        SymbolDefinition::from_kicad_symbol_sexpr(&raw)
            .context("failed to parse component symbol definition")
    })
    .transpose()
}

fn attribute_string(instance: &Instance, key: &str) -> Option<String> {
    match instance.attributes.get(key)? {
        AttributeValue::String(value) | AttributeValue::Port(value) => Some(value.clone()),
        AttributeValue::Number(value) => Some(value.to_string()),
        AttributeValue::Boolean(value) => Some(value.to_string()),
        AttributeValue::Json(value) => value
            .as_str()
            .map(str::to_string)
            .or_else(|| value.get("String")?.as_str().map(str::to_string)),
        AttributeValue::Array(_) => None,
    }
}
