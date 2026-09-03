//! Semantic root-interface extraction from an evaluated Zener netlist.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use pcb_sch::{AttributeValue, InstanceRef, Schematic};
use serde_json::Value;

/// Net names keyed by netlist id, shared by every signature walker.
pub(crate) fn net_names_by_id(netlist: &Schematic) -> Result<BTreeMap<u64, &str>> {
    let mut names = BTreeMap::new();
    for net in netlist.nets.values() {
        if let Some(previous) = names.insert(net.id, net.name.as_str()) {
            bail!(
                "nets '{previous}' and '{}' have the same id {}",
                net.name,
                net.id
            );
        }
    }
    Ok(names)
}

/// A signature-net callback: dotted io path, resolved net name, the leaf
/// value, and its matching default value.
pub(crate) type SignatureNetVisitor<'a> =
    dyn FnMut(&str, &str, &Value, Option<&Value>) -> Result<()> + 'a;

/// Walk one module signature's electrical structure, calling `visit` for
/// every `Net` leaf with its dotted io path, resolved net name, leaf value,
/// and matching default value. With `strict`, a top-level io that is neither
/// a `Net` nor an `Interface` is an error; nested non-electrical fields are
/// always skipped.
pub(crate) fn visit_signature_nets(
    signature: &Value,
    owner: &str,
    strict: bool,
    net_names: &BTreeMap<u64, &str>,
    visit: &mut SignatureNetVisitor,
) -> Result<()> {
    let Some(parameters) = signature.get("parameters").and_then(Value::as_array) else {
        bail!("{owner} __signature.parameters must be an array");
    };
    for parameter in parameters {
        let is_config = parameter
            .get("is_config")
            .and_then(Value::as_bool)
            .with_context(|| format!("{owner} signature parameter is_config must be a boolean"))?;
        if is_config {
            continue;
        }
        let io_name = parameter
            .get("name")
            .and_then(Value::as_str)
            .with_context(|| format!("{owner} signature parameter name must be a string"))?;
        let value = parameter
            .get("value")
            .with_context(|| format!("{owner} signature io {io_name} has no value"))?;
        visit_signature_value(
            value,
            parameter.get("default_value"),
            io_name,
            owner,
            strict,
            net_names,
            visit,
        )?;
    }
    Ok(())
}

fn visit_signature_value(
    value: &Value,
    default_value: Option<&Value>,
    io_path: &str,
    owner: &str,
    strict: bool,
    net_names: &BTreeMap<u64, &str>,
    visit: &mut SignatureNetVisitor,
) -> Result<()> {
    if let Some(net) = value.get("Net").and_then(Value::as_object) {
        if net.get("kind").and_then(Value::as_str) == Some("NotConnected") {
            return Ok(());
        }
        let id = net
            .get("id")
            .and_then(Value::as_u64)
            .with_context(|| format!("{owner} signature net {io_path} has no integer id"))?;
        let net_name = net_names.get(&id).with_context(|| {
            format!("{owner} signature net {io_path} references unknown net id {id}")
        })?;
        return visit(io_path, net_name, value, default_value);
    }

    let Some(fields) = value
        .get("Interface")
        .and_then(|value| value.get("fields"))
        .and_then(Value::as_object)
    else {
        if strict {
            bail!("{owner} signature io {io_path} is neither a Net nor an Interface");
        }
        return Ok(());
    };
    let defaults = default_value
        .and_then(|value| value.get("Interface"))
        .and_then(|value| value.get("fields"))
        .and_then(Value::as_object);
    for (field_name, field_value) in fields {
        let nested_path = if io_path.is_empty() {
            field_name.clone()
        } else {
            format!("{io_path}.{field_name}")
        };
        visit_signature_value(
            field_value,
            defaults.and_then(|defaults| defaults.get(field_name)),
            &nested_path,
            owner,
            false,
            net_names,
            visit,
        )?;
    }
    Ok(())
}

pub(crate) fn ports_by_net(
    netlist: &Schematic,
    module_ref: &InstanceRef,
) -> Result<BTreeMap<String, BTreeSet<String>>> {
    let module = netlist
        .instances
        .get(module_ref)
        .with_context(|| format!("module instance '{module_ref}' is absent from the netlist"))?;
    let Some(AttributeValue::Json(signature)) = module.attributes.get("__signature") else {
        return Ok(BTreeMap::new());
    };
    let net_names = net_names_by_id(netlist)?;
    let mut ports = BTreeMap::<String, BTreeSet<String>>::new();
    visit_signature_nets(
        signature,
        &format!("module '{module_ref}'"),
        true,
        &net_names,
        &mut |io_path, net_name, _value, default_value| {
            // A default symbol represents the endpoint directly, so the
            // external interface port does not need a persisted endpoint.
            if default_value.and_then(net_symbol_value).is_some() {
                return Ok(());
            }
            ports
                .entry(net_name.to_string())
                .or_default()
                .insert(io_path.to_string());
            Ok(())
        },
    )?;
    Ok(ports)
}

/// Root interface ports whose source default declares a net symbol. These
/// may be represented by that symbol or by a hierarchical label.
pub(crate) fn symbol_ports_by_net(
    netlist: &Schematic,
    module_ref: &InstanceRef,
) -> Result<BTreeMap<String, BTreeSet<String>>> {
    let module = netlist
        .instances
        .get(module_ref)
        .with_context(|| format!("module instance '{module_ref}' is absent from the netlist"))?;
    let Some(AttributeValue::Json(signature)) = module.attributes.get("__signature") else {
        return Ok(BTreeMap::new());
    };
    let net_names = net_names_by_id(netlist)?;
    let mut ports = BTreeMap::<String, BTreeSet<String>>::new();
    visit_signature_nets(
        signature,
        &format!("module '{module_ref}'"),
        true,
        &net_names,
        &mut |io_path, net_name, _value, default_value| {
            if default_value.and_then(net_symbol_value).is_some() {
                ports
                    .entry(net_name.to_string())
                    .or_default()
                    .insert(io_path.to_string());
            }
            Ok(())
        },
    )?;
    Ok(ports)
}

fn net_symbol_value(value: &Value) -> Option<&str> {
    let value = value.get("Net")?.get("properties")?.get("__symbol_value")?;
    value
        .as_str()
        .or_else(|| value.get("String").and_then(Value::as_str))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use pcb_sch::{Instance, InstanceRef, ModuleRef, Net};

    use super::*;

    #[test]
    fn ignores_non_electrical_fields_inside_root_interfaces() {
        let module = ModuleRef::from_path(Path::new("/tmp/root.zen"), "root");
        let root_ref = InstanceRef::new(module.clone(), Vec::new());
        let mut root = Instance::module(module);
        root.attributes.insert(
            "__signature".to_string(),
            AttributeValue::Json(serde_json::json!({
                "parameters": [{
                    "name": "CSI_A",
                    "is_config": false,
                    "value": {
                        "Interface": {
                            "fields": {
                                "CLK": {
                                    "Interface": {
                                        "fields": {
                                            "P": {
                                                "Net": {
                                                    "id": 7,
                                                    "name": "CSI_A_CLK_P",
                                                    "properties": {}
                                                }
                                            },
                                            "impedance": {
                                                "PhysicalValue": {
                                                    "nominal": "90",
                                                    "unit": "ohm"
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }]
            })),
        );
        let mut netlist = Schematic::new();
        netlist.root_ref = Some(root_ref.clone());
        netlist.add_instance(root_ref.clone(), root);
        netlist.add_net(Net {
            kind: "Net".to_string(),
            id: 7,
            name: "CSI_A_CLK_P".to_string(),
            ports: Vec::new(),
            properties: Default::default(),
        });

        let ports = ports_by_net(&netlist, &root_ref).unwrap();

        assert_eq!(
            ports["CSI_A_CLK_P"],
            BTreeSet::from(["CSI_A.CLK.P".to_string()])
        );
    }
}
