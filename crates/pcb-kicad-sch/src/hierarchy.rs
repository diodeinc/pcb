use std::collections::BTreeMap;

use pcb_sch::{AttributeValue, Schematic};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RootIoLabel {
    pub io_name: String,
}

pub(crate) fn root_io_labels_by_net(netlist: &Schematic) -> BTreeMap<String, RootIoLabel> {
    let Some(root) = netlist
        .root_ref
        .as_ref()
        .and_then(|root| netlist.instances.get(root))
    else {
        return BTreeMap::new();
    };
    let Some(AttributeValue::Json(signature)) = root.attributes.get("__signature") else {
        return BTreeMap::new();
    };
    let Some(parameters) = signature.get("parameters").and_then(Value::as_array) else {
        return BTreeMap::new();
    };

    let net_name_by_id = netlist
        .nets
        .values()
        .map(|net| (net.id as i64, net.name.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut labels = BTreeMap::new();
    for parameter in parameters {
        if parameter.get("is_config").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let Some(value) = parameter.get("value") else {
            continue;
        };
        let io_name = parameter
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        collect_labels(
            value,
            parameter.get("default_value"),
            io_name,
            &net_name_by_id,
            &mut labels,
        );
    }
    labels
}

fn collect_labels(
    value: &Value,
    default_value: Option<&Value>,
    io_name: &str,
    net_name_by_id: &BTreeMap<i64, String>,
    labels: &mut BTreeMap<String, RootIoLabel>,
) {
    if let Some(net) = value.get("Net").and_then(Value::as_object) {
        let Some(id) = net.get("id").and_then(Value::as_i64) else {
            return;
        };
        // A default symbol represents the endpoint directly, so it does not
        // require a hierarchical label in the persisted schematic.
        if default_value.and_then(net_symbol_value).is_some() {
            return;
        }
        let net_name = net_name_by_id
            .get(&id)
            .cloned()
            .unwrap_or_else(|| id.to_string());
        labels.insert(
            net_name.clone(),
            RootIoLabel {
                io_name: io_name.to_string(),
            },
        );
        return;
    }

    let Some(fields) = value
        .get("Interface")
        .and_then(|value| value.get("fields"))
        .and_then(Value::as_object)
    else {
        return;
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
        collect_labels(
            field_value,
            defaults.and_then(|defaults| defaults.get(field_name)),
            &nested_name,
            net_name_by_id,
            labels,
        );
    }
}

fn net_symbol_value(value: &Value) -> Option<&str> {
    let value = value.get("Net")?.get("properties")?.get("__symbol_value")?;
    value
        .as_str()
        .or_else(|| value.get("String").and_then(Value::as_str))
}
