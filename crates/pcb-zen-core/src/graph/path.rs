use crate::graph::{CircuitGraph, FactorId, PortId, PortPath};
use starlark::collections::SmallMap;
use starlark::values::{tuple::TupleRef, Heap, Value};
use std::collections::HashSet;

impl CircuitGraph {
    /// Resolve a label (net name or port tuple) to a PortId
    pub fn resolve_label_to_port<'v>(
        &self,
        label: Value<'v>,
        _heap: &'v Heap,
    ) -> starlark::Result<PortId> {
        // Check if it's a string (net name) - assume it's referring to external net
        if let Some(net_name) = label.unpack_str() {
            // First try to find the external port for this net
            let external_port_path = PortPath::new("<external>", net_name);
            if let Some(external_port_id) = self.port_id(&external_port_path) {
                return Ok(external_port_id);
            }

            // If no external port exists, this net name is not valid for pathfinding
            Err(starlark::Error::new_other(anyhow::anyhow!(
                "Net '{}' is not a public (io) net - use a specific port tuple (component, pin) instead",
                net_name
            )))
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

    /// Create a PathValue object from a path of PortIds and factors
    pub fn create_path_value<'v>(
        &self,
        port_path: &[PortId],
        factors: &[FactorId],
        components: &SmallMap<String, Value<'v>>,
        heap: &'v Heap,
    ) -> starlark::Result<Value<'v>> {
        use crate::graph::starlark::PathValueGen;

        let mut ports = Vec::new();
        let mut path_components = Vec::new();
        let mut path_nets = Vec::new();

        // Build ports list
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
        }

        // Build components list from factors (deduplicated)
        let mut seen_components = HashSet::new();
        for &factor_id in factors {
            if let crate::graph::FactorType::Component(comp_path) = self.factor_type(factor_id) {
                if !seen_components.contains(comp_path) {
                    let component_value = components.get(comp_path).ok_or_else(|| {
                        starlark::Error::new_other(anyhow::anyhow!(
                            "Component '{}' not found",
                            comp_path
                        ))
                    })?;
                    path_components.push(*component_value);
                    seen_components.insert(comp_path.clone());
                }
            }
        }

        // Build nets list from factors (only net factors)
        for &factor_id in factors {
            if let crate::graph::FactorType::Net(net_name) = self.factor_type(factor_id) {
                path_nets.push(heap.alloc_str(net_name).to_value());
            }
        }

        // Create PathValue object
        let path_value = PathValueGen {
            ports,
            components: path_components,
            nets: path_nets,
        };

        Ok(heap.alloc_complex(path_value))
    }
}
