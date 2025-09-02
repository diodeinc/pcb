use crate::graph::{CircuitGraph, PortId, PortPath};
use starlark::collections::SmallMap;
use starlark::values::{tuple::TupleRef, Heap, Value};

impl CircuitGraph {
    /// Resolve a label (net name or port tuple) to a PortId
    pub fn resolve_label_to_port<'v>(
        &self,
        label: Value<'v>,
        _heap: &'v Heap,
    ) -> starlark::Result<PortId> {
        // Check if it's a string (net name)
        if let Some(net_name) = label.unpack_str() {
            // Find the factor for this net name
            let factor_id = self.factor_id(net_name).ok_or_else(|| {
                starlark::Error::new_other(anyhow::anyhow!("Net '{}' not found", net_name))
            })?;

            // Get all ports connected to this net
            let ports = self.factor_ports(factor_id);
            if ports.is_empty() {
                return Err(starlark::Error::new_other(anyhow::anyhow!(
                    "Net '{}' has no connected ports",
                    net_name
                )));
            }

            // Net should only be used if there's exactly 1 port
            if ports.len() > 1 {
                return Err(starlark::Error::new_other(anyhow::anyhow!(
                    "Net '{}' has {} ports - use a specific port tuple (component, pin) instead",
                    net_name,
                    ports.len()
                )));
            }

            Ok(ports[0])
        }
        // Check if it's a tuple (component, pin)
        else if let Some(tuple_ref) = TupleRef::from_value(label) {
            if tuple_ref.len() != 2 {
                return Err(starlark::Error::new_other(anyhow::anyhow!(
                    "Port tuple must have exactly 2 elements: (component, pin)"
                )));
            }

            let component_str = tuple_ref.content()[0].unpack_str().ok_or_else(|| {
                starlark::Error::new_other(anyhow::anyhow!("Component path must be a string"))
            })?;

            let pin_str = tuple_ref.content()[1].unpack_str().ok_or_else(|| {
                starlark::Error::new_other(anyhow::anyhow!("Pin name must be a string"))
            })?;

            let port_path = PortPath::new(component_str, pin_str);

            self.port_id(&port_path).ok_or_else(|| {
                starlark::Error::new_other(anyhow::anyhow!("Port '{}' not found", port_path))
            })
        } else {
            Err(starlark::Error::new_other(anyhow::anyhow!(
                "Label must be a string (net name) or tuple (component, pin)"
            )))
        }
    }

    /// Create a PathValue object from a path of PortIds
    pub fn create_path_value<'v>(
        &self,
        port_path: &[PortId],
        components: &SmallMap<String, Value<'v>>,
        heap: &'v Heap,
    ) -> starlark::Result<Value<'v>> {
        use crate::graph::starlark::PathValueGen;

        let mut ports = Vec::new();
        let mut path_components = Vec::new();
        let mut nets = Vec::new();

        for &port_id in port_path {
            // Get port path directly - no string parsing needed!
            let port_path_obj = self.port_path(port_id).ok_or_else(|| {
                starlark::Error::new_other(anyhow::anyhow!("Port {:?} not found in graph", port_id))
            })?;

            let component_path = port_path_obj.component.to_string();
            let pin_name = &port_path_obj.pin;

            // Add port tuple
            let port_tuple =
                heap.alloc((heap.alloc_str(&component_path), heap.alloc_str(pin_name)));
            ports.push(port_tuple);

            // Find and add component object
            let component_value = components.get(&component_path).ok_or_else(|| {
                starlark::Error::new_other(anyhow::anyhow!(
                    "Component '{}' not found",
                    component_path
                ))
            })?;
            path_components.push(*component_value);

            // Find net connected to this port
            let [factor1, factor2] = self.port_factors(port_id);

            // Find the net factor (not component factor)
            let net_factor =
                if matches!(self.factor_type(factor1), crate::graph::FactorType::Net(_)) {
                    factor1
                } else if matches!(self.factor_type(factor2), crate::graph::FactorType::Net(_)) {
                    factor2
                } else {
                    return Err(starlark::Error::new_other(anyhow::anyhow!(
                        "Port '{}' has no net factor",
                        port_path_obj
                    )));
                };

            let net_name = if let crate::graph::FactorType::Net(name) = self.factor_type(net_factor)
            {
                name
            } else {
                return Err(starlark::Error::new_other(anyhow::anyhow!(
                    "Expected net factor but got component factor"
                )));
            };

            // Store the net name as a string
            nets.push(heap.alloc_str(net_name).to_value());
        }

        // Create PathValue object
        let path_value = PathValueGen {
            ports,
            components: path_components,
            nets,
        };

        Ok(heap.alloc_complex(path_value))
    }
}
