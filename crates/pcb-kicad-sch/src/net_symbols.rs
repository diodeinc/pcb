use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::{Result, bail};
use pcb_sch::{ATTR_SYMBOL_FORMAT_VERSION, AttributeValue, InstanceKind, Schematic};
use serde_json::{Map, Value};

use crate::{
    Point, Rotation, Symbol, SymbolDefinition,
    component_slots::{self, SYMBOL_PATH_ATTR, SYMBOL_VALUE_ATTR, validate_symbol_library_version},
    connectivity::named_connected_nets,
    symbol,
};

/// A Zener net symbol is a creation preference, not reconciliation identity.
#[derive(Clone)]
pub(crate) struct NetSymbolSpec {
    pub definition: SymbolDefinition,
    pub unit: u32,
    pub pin_offset: Point,
}

pub(crate) fn specs(netlist: &Schematic) -> Result<BTreeMap<String, NetSymbolSpec>> {
    definitions_by_net(netlist)?
        .into_iter()
        .map(|(net_name, definition)| {
            let owner = format!("net '{net_name}'");
            let parsed = symbol::ParsedSymbolDefinition::parse(&definition)?;
            if parsed.power_scope().is_none() {
                bail!(
                    "{owner} symbol '{}' is not a KiCad power symbol",
                    definition.lib_id
                );
            }
            let units = parsed.power_input_units();
            let [unit] = units.as_slice() else {
                bail!(
                    "{owner} symbol '{}' must have exactly one unit containing a power-input pin",
                    definition.lib_id
                );
            };
            let unplaced = Symbol {
                id: String::new(),
                lib_id: definition.lib_id.clone(),
                unit: *unit,
                body_style: 1,
                at: Point::default(),
                rotation: Rotation::default(),
                mirror: None,
                fields_autoplaced: false,
                fields: BTreeMap::new(),
                pins: Vec::new(),
                unsupported: Vec::new(),
            };
            let connection_pins = parsed
                .placed_pins(&unplaced)?
                .into_iter()
                .filter(|pin| !pin.hidden && pin.is_power_input())
                .collect::<Vec<_>>();
            let [pin] = connection_pins.as_slice() else {
                bail!(
                    "{owner} symbol '{}' must have exactly one visible power-input pin",
                    definition.lib_id
                );
            };
            Ok((
                net_name,
                NetSymbolSpec {
                    definition,
                    unit: *unit,
                    pin_offset: pin.point,
                },
            ))
        })
        .collect()
}

fn definitions_by_net(netlist: &Schematic) -> Result<BTreeMap<String, SymbolDefinition>> {
    let mut definitions = BTreeMap::new();
    for net in named_connected_nets(netlist) {
        let owner = format!("net '{}'", net.name);
        insert_definition(
            &mut definitions,
            &net.name,
            definition_from_attributes(netlist, &owner, &net.properties)?,
        )?;
    }

    // A realized net's own properties are authoritative. Signatures retain
    // the same metadata for some io() nets whose deduplicated net properties
    // are empty, so use signatures only as an unambiguous fallback.
    let mut signature_definitions = BTreeMap::new();
    let authoritative_names = definitions.keys().cloned().collect::<BTreeSet<_>>();
    let net_names = crate::root_interface::net_names_by_id(netlist)?;
    for (instance_ref, instance) in &netlist.instances {
        if instance.kind != InstanceKind::Module {
            continue;
        }
        let Some(AttributeValue::Json(signature)) = instance.attributes.get("__signature") else {
            continue;
        };
        crate::root_interface::visit_signature_nets(
            signature,
            &format!("module '{instance_ref}'"),
            false,
            &net_names,
            &mut |io_path, net_name, value, default_value| {
                if authoritative_names.contains(net_name) {
                    return Ok(());
                }
                let properties =
                    symbol_properties(value).or_else(|| default_value.and_then(symbol_properties));
                if let Some(properties) = properties {
                    let owner = format!("module '{instance_ref}' signature io '{io_path}'");
                    let attributes = json_symbol_attributes(properties);
                    insert_definition(
                        &mut signature_definitions,
                        net_name,
                        definition_from_attributes(netlist, &owner, &attributes)?,
                    )?;
                }
                Ok(())
            },
        )?;
    }
    for (net_name, definition) in signature_definitions {
        definitions.entry(net_name).or_insert(definition);
    }
    Ok(definitions)
}

fn symbol_properties(value: &Value) -> Option<&Map<String, Value>> {
    let properties = value.get("Net")?.get("properties")?.as_object()?;
    properties
        .contains_key(SYMBOL_VALUE_ATTR)
        .then_some(properties)
        .or_else(|| {
            properties
                .contains_key(SYMBOL_PATH_ATTR)
                .then_some(properties)
        })
}

fn json_symbol_attributes(properties: &Map<String, Value>) -> HashMap<String, AttributeValue> {
    [
        SYMBOL_VALUE_ATTR,
        SYMBOL_PATH_ATTR,
        ATTR_SYMBOL_FORMAT_VERSION,
    ]
    .into_iter()
    .filter_map(|key| {
        properties.get(key).map(|value| {
            let value = match value {
                Value::String(value) => AttributeValue::String(value.clone()),
                Value::Number(value) => value
                    .as_f64()
                    .map(AttributeValue::Number)
                    .unwrap_or_else(|| AttributeValue::Json(value.clone().into())),
                _ => AttributeValue::Json(value.clone()),
            };
            (key.to_string(), value)
        })
    })
    .collect()
}

fn definition_from_attributes(
    netlist: &Schematic,
    owner: &str,
    attributes: &HashMap<String, AttributeValue>,
) -> Result<Option<SymbolDefinition>> {
    validate_symbol_library_version(owner, attributes)?;
    component_slots::symbol_definition(netlist, owner, attributes)
}

fn insert_definition(
    definitions: &mut BTreeMap<String, SymbolDefinition>,
    net_name: &str,
    definition: Option<SymbolDefinition>,
) -> Result<()> {
    let Some(definition) = definition else {
        return Ok(());
    };
    match definitions.get(net_name) {
        Some(existing) if existing != &definition => bail!(
            "net '{net_name}' has conflicting KiCad net symbol definitions '{}' and '{}'",
            existing.lib_id,
            definition.lib_id
        ),
        Some(_) => {}
        None => {
            definitions.insert(net_name.to_string(), definition);
        }
    }
    Ok(())
}
