//! Semantic root-interface extraction from an evaluated Zener netlist.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use pcb_sch::{AttributeValue, Schematic};
use serde_json::Value;

pub(crate) fn ports_by_net(netlist: &Schematic) -> Result<BTreeMap<String, BTreeSet<String>>> {
    let Some(root) = netlist
        .root_ref
        .as_ref()
        .and_then(|root| netlist.instances.get(root))
    else {
        return Ok(BTreeMap::new());
    };
    let Some(AttributeValue::Json(signature)) = root.attributes.get("__signature") else {
        return Ok(BTreeMap::new());
    };
    let Some(parameters) = signature.get("parameters").and_then(Value::as_array) else {
        bail!("root __signature.parameters must be an array");
    };

    let net_name_by_id = netlist
        .nets
        .values()
        .map(|net| (net.id as i64, net.name.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut ports = BTreeMap::new();
    for parameter in parameters {
        let is_config = parameter
            .get("is_config")
            .and_then(Value::as_bool)
            .context("root signature parameter is_config must be a boolean")?;
        if is_config {
            continue;
        }
        let io_name = parameter
            .get("name")
            .and_then(Value::as_str)
            .context("root signature parameter name must be a string")?;
        let value = parameter
            .get("value")
            .with_context(|| format!("root signature io {io_name} has no value"))?;
        collect_ports(
            value,
            parameter.get("default_value"),
            io_name,
            &net_name_by_id,
            &mut ports,
            true,
        )?;
    }
    Ok(ports)
}

fn collect_ports(
    value: &Value,
    default_value: Option<&Value>,
    io_name: &str,
    net_name_by_id: &BTreeMap<i64, String>,
    ports: &mut BTreeMap<String, BTreeSet<String>>,
    require_electrical_value: bool,
) -> Result<()> {
    if let Some(net) = value.get("Net").and_then(Value::as_object) {
        let Some(id) = net.get("id").and_then(Value::as_i64) else {
            bail!("root signature net {io_name} has no integer id");
        };
        // A default symbol represents the endpoint directly, so the external
        // interface port does not need a separate persisted endpoint.
        if default_value.and_then(net_symbol_value).is_some() {
            return Ok(());
        }
        let net_name = net_name_by_id
            .get(&id)
            .with_context(|| {
                format!("root signature net {io_name} references unknown net id {id}")
            })?
            .clone();
        ports
            .entry(net_name)
            .or_default()
            .insert(io_name.to_string());
        return Ok(());
    }

    let Some(fields) = value
        .get("Interface")
        .and_then(|value| value.get("fields"))
        .and_then(Value::as_object)
    else {
        if require_electrical_value {
            bail!("root signature io {io_name} is neither a Net nor an Interface");
        }
        return Ok(());
    };
    let defaults = default_value
        .and_then(|value| value.get("Interface"))
        .and_then(|value| value.get("fields"))
        .and_then(Value::as_object);
    for (field_name, field_value) in fields {
        let nested_name = if io_name.is_empty() {
            field_name.clone()
        } else {
            format!("{io_name}.{field_name}")
        };
        collect_ports(
            field_value,
            defaults.and_then(|defaults| defaults.get(field_name)),
            &nested_name,
            net_name_by_id,
            ports,
            false,
        )?;
    }
    Ok(())
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
        netlist.add_instance(root_ref, root);
        netlist.add_net(Net {
            kind: "Net".to_string(),
            id: 7,
            name: "CSI_A_CLK_P".to_string(),
            ports: Vec::new(),
            properties: Default::default(),
        });

        let ports = ports_by_net(&netlist).unwrap();

        assert_eq!(
            ports["CSI_A_CLK_P"],
            BTreeSet::from(["CSI_A.CLK.P".to_string()])
        );
    }
}
