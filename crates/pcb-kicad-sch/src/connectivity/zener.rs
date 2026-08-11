use std::collections::BTreeSet;

use anyhow::Result;
use pcb_sch::{AttributeValue, InstanceRef, Schematic};

use super::{
    ComponentIdentity, ComponentNode, ComponentOrigin, ConnectionGroup, ConnectionOrigin,
    ConnectivityGraph, Terminal,
};
use crate::{component_slots, root_interface};

pub(super) fn reduce(netlist: &Schematic) -> Result<ConnectivityGraph> {
    let mut components = component_slots::component_symbol_slots(netlist)?
        .into_iter()
        .map(|slot| ComponentNode {
            managed_slot: Some(slot),
            origin: ComponentOrigin::Zener,
        })
        .collect::<Vec<_>>();
    components.sort();

    let interface_ports = root_interface::ports_by_net(netlist)?;
    let mut nets = netlist.nets.values().collect::<Vec<_>>();
    nets.sort_by(|a, b| a.name.cmp(&b.name));
    let groups = nets
        .into_iter()
        .filter_map(|net| {
            let name = net.name.as_str();
            if net.kind == "NotConnected" || name.is_empty() {
                return None;
            }
            let mut terminals = net
                .ports
                .iter()
                .filter_map(|port| component_terminal(netlist, port))
                .collect::<BTreeSet<_>>();
            if let Some(ports) = interface_ports.get(name) {
                terminals.extend(
                    ports
                        .iter()
                        .map(|port| Terminal::InterfacePort { name: port.clone() }),
                );
            }
            Some(ConnectionGroup {
                names: BTreeSet::from([name.to_string()]),
                terminals,
                origins: BTreeSet::from([ConnectionOrigin::ZenerNet {
                    name: name.to_string(),
                }]),
            })
        })
        .collect();

    Ok(ConnectivityGraph { components, groups })
}

pub(super) fn not_connected_terminals(netlist: &Schematic) -> BTreeSet<Terminal> {
    netlist
        .nets
        .values()
        .filter(|net| net.kind == "NotConnected")
        .flat_map(|net| &net.ports)
        .filter_map(|port| component_terminal(netlist, port))
        .collect()
}

fn component_terminal(netlist: &Schematic, port: &InstanceRef) -> Option<Terminal> {
    let (component_ref, pin_name) = netlist.component_ref_and_pin_for_port(port)?;
    let component_path = crate::canonical_component_path(&component_ref.instance_path)?;
    let pin_numbers = pads_for_port(netlist, port)
        .into_iter()
        .filter(|number| !number.is_empty())
        .collect();
    Some(Terminal::ComponentPin {
        component: ComponentIdentity::ManagedPath(component_path),
        pin_name,
        pin_numbers,
    })
}

fn pads_for_port(netlist: &Schematic, port: &InstanceRef) -> Vec<String> {
    let Some(instance) = netlist.instances.get(port) else {
        return Vec::new();
    };
    let Some(AttributeValue::Array(values)) = instance.attributes.get("pads") else {
        return Vec::new();
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
