//! Implicit net rename detection for PCB layout sync.
//!
//! This module implements Phase 2 of the layout sync pipeline: detecting net renames
//! that were made in the Zener source code without explicit `moved()` directives.
//!
//! # Problem
//!
//! When a user renames a net in Zener without adding a `moved("old", "new")` directive,
//! the layout file's zones and vias referencing the old net name become orphaned. This
//! module infers such renames with 100% confidence by analyzing port connectivity.
//!
//! # Algorithm
//!
//! A **port** is a stable identifier `(component_path, pad_name)` derived from the Zener
//! module hierarchy. Ports are stable across sync because component paths come from code
//! structure, not mutable properties like reference designators.
//!
//! The algorithm uses **signature-based matching**:
//!
//! 1. Build port→net mappings from both netlist and layout
//! 2. For each net, compute its **signature** = sorted list of common ports
//! 3. Build signature→net indexes for both sides (only if signature is unique)
//! 4. For each signature that maps uniquely to one net on each side:
//!    - If names differ and presence checks pass → infer rename
//!
//! # Properties
//!
//! - **No heuristics**: Only 100% confidence cases are handled
//! - **Rejects ambiguity**: Net merges, splits, and signature collisions are skipped
//! - **Conservative**: When in doubt, does nothing (false negatives OK, false positives not)
//!
//! # Pipeline Position
//!
//! ```text
//! Phase 1: Explicit moved() renames      (moved.rs)
//! Phase 2: Implicit net rename detection (this module)
//! Phase 3: Python layout sync            (update_layout_file.py)
//! ```

use log::info;
use pcb_sch::Schematic;
use pcb_sexpr::Sexpr;
use std::collections::{HashMap, HashSet};

/// A port is a stable identifier for a connection point: (component_path, pad_name)
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Port {
    pub component_path: String,
    pub pad_name: String,
}

/// A signature is a sorted list of ports that uniquely identifies a net's connectivity.
type Signature = Vec<Port>;

/// Result of implicit rename detection
#[derive(Debug, Default)]
pub struct ImplicitRenameResult {
    /// Inferred renames: layout_net_name -> netlist_net_name
    pub renames: HashMap<String, String>,
    /// Layout-only nets that could not be resolved (ambiguous or no match)
    pub orphaned_layout_nets: HashSet<String>,
}

/// Extract port-to-net mapping from the schematic/netlist.
fn build_netlist_port_to_net(schematic: &Schematic) -> HashMap<Port, String> {
    let mut port_to_net: HashMap<Port, String> = HashMap::new();

    for (net_name, net) in &schematic.nets {
        for port_ref in &net.ports {
            let instance_path = &port_ref.instance_path;
            if instance_path.len() < 2 {
                continue;
            }

            let component_path = instance_path[..instance_path.len() - 1].join(".");

            if let Some(port_instance) = schematic.instances.get(port_ref) {
                if let Some(pcb_sch::AttributeValue::Array(pads)) =
                    port_instance.attributes.get("pads")
                {
                    for pad in pads {
                        if let pcb_sch::AttributeValue::String(pad_name) = pad {
                            let port = Port {
                                component_path: component_path.clone(),
                                pad_name: pad_name.clone(),
                            };
                            port_to_net.insert(port, net_name.clone());
                        }
                    }
                }
            }
        }
    }

    port_to_net
}

/// Extract port-to-net mapping from the layout file.
fn build_layout_port_to_net(board: &Sexpr) -> HashMap<Port, String> {
    let mut port_to_net: HashMap<Port, String> = HashMap::new();

    let Some(items) = board.as_list() else {
        return port_to_net;
    };

    for item in items {
        let Some(list) = item.as_list() else { continue };
        if list.first().and_then(|s| s.as_sym()) != Some("footprint") {
            continue;
        }

        let Some((component_path, pad_nets)) = extract_footprint_ports(list) else {
            continue;
        };

        for (pad_name, net_name) in pad_nets {
            if net_name.is_empty() {
                continue;
            }
            let port = Port {
                component_path: component_path.clone(),
                pad_name,
            };
            port_to_net.insert(port, net_name.clone());
        }
    }

    port_to_net
}

/// Extract all net names declared at board level: `(net N "NAME")`
/// Skips empty net names (KiCad's "unconnected" net 0).
fn extract_layout_net_names(board: &Sexpr) -> HashSet<String> {
    let mut names = HashSet::new();

    let Some(items) = board.as_list() else {
        return names;
    };

    for item in items {
        let Some(list) = item.as_list() else { continue };
        if list.first().and_then(|s| s.as_sym()) == Some("net") {
            if let Some(name) = list.get(2).and_then(|s| s.as_str()) {
                if !name.is_empty() {
                    names.insert(name.to_string());
                }
            }
        }
    }

    names
}

/// Extract the Path property and pad->net mappings from a footprint.
fn extract_footprint_ports(footprint: &[Sexpr]) -> Option<(String, Vec<(String, String)>)> {
    let mut component_path: Option<String> = None;
    let mut pad_nets: Vec<(String, String)> = Vec::new();

    for item in footprint {
        let Some(list) = item.as_list() else { continue };
        let tag = list.first().and_then(|s| s.as_sym());

        match tag {
            Some("property") => {
                if list.get(1).and_then(|s| s.as_str()) == Some("Path") {
                    if let Some(value) = list.get(2).and_then(|s| s.as_str()) {
                        component_path = Some(value.to_string());
                    }
                }
            }
            Some("pad") => {
                if let Some(pad_name) = list.get(1).and_then(|s| s.as_str()) {
                    for pad_item in list.iter().skip(2) {
                        if let Some(pad_list) = pad_item.as_list() {
                            if pad_list.first().and_then(|s| s.as_sym()) == Some("net") {
                                if let Some(net_name) = pad_list.get(2).and_then(|s| s.as_str()) {
                                    pad_nets.push((pad_name.to_string(), net_name.to_string()));
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    component_path.map(|path| (path, pad_nets))
}

/// Build net→signature map, where signature = sorted common ports.
fn build_signatures(
    port_to_net: &HashMap<Port, String>,
    common_ports: &HashSet<Port>,
) -> HashMap<String, Signature> {
    let mut net_to_sig: HashMap<String, Signature> = HashMap::new();

    for (port, net) in port_to_net {
        if common_ports.contains(port) {
            net_to_sig
                .entry(net.clone())
                .or_default()
                .push(port.clone());
        }
    }

    // Sort signatures for deterministic comparison
    for sig in net_to_sig.values_mut() {
        sig.sort();
    }

    net_to_sig
}

/// Build signature→net index, but only for unique signatures.
/// Returns None for signatures that map to multiple nets.
fn build_unique_signature_index(
    net_to_sig: &HashMap<String, Signature>,
) -> HashMap<Signature, String> {
    let mut sig_to_net: HashMap<Signature, Option<String>> = HashMap::new();

    for (net, sig) in net_to_sig {
        if sig.is_empty() {
            continue;
        }
        sig_to_net
            .entry(sig.clone())
            .and_modify(|v| *v = None) // Collision: mark as ambiguous
            .or_insert_with(|| Some(net.clone()));
    }

    sig_to_net
        .into_iter()
        .filter_map(|(sig, net)| net.map(|n| (sig, n)))
        .collect()
}

/// Detect implicit net renames by comparing netlist and layout port assignments.
pub fn detect_implicit_renames(schematic: &Schematic, board: &Sexpr) -> ImplicitRenameResult {
    let mut result = ImplicitRenameResult::default();

    // Build port→net mappings
    let netlist_port_to_net = build_netlist_port_to_net(schematic);
    let layout_port_to_net = build_layout_port_to_net(board);

    // Compute common ports
    let netlist_ports: HashSet<Port> = netlist_port_to_net.keys().cloned().collect();
    let layout_ports: HashSet<Port> = layout_port_to_net.keys().cloned().collect();
    let common_ports: HashSet<Port> = netlist_ports.intersection(&layout_ports).cloned().collect();

    info!(
        "Implicit rename detection: {} netlist ports, {} layout ports, {} common",
        netlist_ports.len(),
        layout_ports.len(),
        common_ports.len()
    );

    // Build signatures
    let netlist_sigs = build_signatures(&netlist_port_to_net, &common_ports);
    let layout_sigs = build_signatures(&layout_port_to_net, &common_ports);

    // Build unique signature indexes
    let netlist_sig_to_net = build_unique_signature_index(&netlist_sigs);
    let layout_sig_to_net = build_unique_signature_index(&layout_sigs);

    // Net name sets for presence checks
    let netlist_nets: HashSet<&str> = schematic.nets.keys().map(|s| s.as_str()).collect();
    let layout_nets = extract_layout_net_names(board);

    // Find renames: signatures that exist uniquely on both sides with different names
    for (sig, netlist_net) in &netlist_sig_to_net {
        let Some(layout_net) = layout_sig_to_net.get(sig) else {
            continue;
        };

        // Same name: no rename needed
        if layout_net == netlist_net {
            continue;
        }

        // Check: old name (layout) not in netlist
        if netlist_nets.contains(layout_net.as_str()) {
            continue;
        }

        // Check: new name (netlist) not already in layout
        if layout_nets.contains(netlist_net) {
            continue;
        }

        info!(
            "Detected implicit net rename: \"{}\" -> \"{}\" ({} common ports)",
            layout_net,
            netlist_net,
            sig.len()
        );

        result
            .renames
            .insert(layout_net.clone(), netlist_net.clone());
    }

    // Identify orphaned layout-only nets
    for layout_net in &layout_nets {
        if !netlist_nets.contains(layout_net.as_str()) && !result.renames.contains_key(layout_net) {
            result.orphaned_layout_nets.insert(layout_net.clone());
        }
    }

    if !result.orphaned_layout_nets.is_empty() {
        info!(
            "Found {} orphaned layout-only nets that could not be resolved",
            result.orphaned_layout_nets.len()
        );
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use pcb_sch::{AttributeValue, Instance, InstanceKind, InstanceRef, ModuleRef, Net, NetKind};
    use pcb_sexpr::parse;
    use std::path::PathBuf;

    fn make_instance_ref(path: &[&str]) -> InstanceRef {
        InstanceRef {
            module: ModuleRef {
                source_path: PathBuf::from("/test.zen"),
                module_name: "<root>".to_string(),
            },
            instance_path: path.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn make_port_instance(pads: &[&str]) -> Instance {
        let mut inst = Instance::new(ModuleRef::new("/test.zen", "Port"), InstanceKind::Port);
        inst.attributes.insert(
            "pads".to_string(),
            AttributeValue::Array(
                pads.iter()
                    .map(|s| AttributeValue::String(s.to_string()))
                    .collect(),
            ),
        );
        inst
    }

    #[test]
    fn test_simple_rename_detection() {
        let mut schematic = Schematic::new();

        schematic
            .instances
            .insert(make_instance_ref(&["R1", "P1"]), make_port_instance(&["1"]));
        schematic
            .instances
            .insert(make_instance_ref(&["R1", "P2"]), make_port_instance(&["2"]));

        schematic.nets.insert(
            "NET_NEW".to_string(),
            Net {
                kind: NetKind::Normal,
                id: 1,
                name: "NET_NEW".to_string(),
                ports: vec![
                    make_instance_ref(&["R1", "P1"]),
                    make_instance_ref(&["R1", "P2"]),
                ],
                properties: HashMap::new(),
            },
        );

        let layout = parse(
            r#"(kicad_pcb
            (net 1 "NET_OLD")
            (footprint "R_0603"
                (property "Path" "R1")
                (pad "1" smd rect (net 1 "NET_OLD"))
                (pad "2" smd rect (net 1 "NET_OLD"))
            )
        )"#,
        )
        .unwrap();

        let result = detect_implicit_renames(&schematic, &layout);

        assert_eq!(result.renames.len(), 1);
        assert_eq!(result.renames.get("NET_OLD"), Some(&"NET_NEW".to_string()));
        assert!(result.orphaned_layout_nets.is_empty());
    }

    #[test]
    fn test_no_rename_when_names_match() {
        let mut schematic = Schematic::new();

        schematic
            .instances
            .insert(make_instance_ref(&["R1", "P1"]), make_port_instance(&["1"]));

        schematic.nets.insert(
            "VCC".to_string(),
            Net {
                kind: NetKind::Normal,
                id: 1,
                name: "VCC".to_string(),
                ports: vec![make_instance_ref(&["R1", "P1"])],
                properties: HashMap::new(),
            },
        );

        let layout = parse(
            r#"(kicad_pcb
            (net 1 "VCC")
            (footprint "R_0603"
                (property "Path" "R1")
                (pad "1" smd rect (net 1 "VCC"))
            )
        )"#,
        )
        .unwrap();

        let result = detect_implicit_renames(&schematic, &layout);

        assert!(result.renames.is_empty());
        assert!(result.orphaned_layout_nets.is_empty());
    }

    #[test]
    fn test_deleted_component_still_allows_rename() {
        let mut schematic = Schematic::new();

        schematic
            .instances
            .insert(make_instance_ref(&["R1", "P1"]), make_port_instance(&["1"]));

        schematic.nets.insert(
            "NET_NEW".to_string(),
            Net {
                kind: NetKind::Normal,
                id: 1,
                name: "NET_NEW".to_string(),
                ports: vec![make_instance_ref(&["R1", "P1"])],
                properties: HashMap::new(),
            },
        );

        let layout = parse(
            r#"(kicad_pcb
            (net 1 "NET_OLD")
            (footprint "R_0603"
                (property "Path" "R1")
                (pad "1" smd rect (net 1 "NET_OLD"))
            )
            (footprint "R_0603"
                (property "Path" "R2")
                (pad "1" smd rect (net 1 "NET_OLD"))
            )
        )"#,
        )
        .unwrap();

        let result = detect_implicit_renames(&schematic, &layout);

        assert_eq!(result.renames.len(), 1);
        assert_eq!(result.renames.get("NET_OLD"), Some(&"NET_NEW".to_string()));
        assert!(result.orphaned_layout_nets.is_empty());
    }

    #[test]
    fn test_skip_when_port_sets_differ() {
        let mut schematic = Schematic::new();

        schematic
            .instances
            .insert(make_instance_ref(&["R1", "P1"]), make_port_instance(&["1"]));
        schematic
            .instances
            .insert(make_instance_ref(&["R2", "P1"]), make_port_instance(&["1"]));

        schematic.nets.insert(
            "NET_NEW1".to_string(),
            Net {
                kind: NetKind::Normal,
                id: 1,
                name: "NET_NEW1".to_string(),
                ports: vec![make_instance_ref(&["R1", "P1"])],
                properties: HashMap::new(),
            },
        );
        schematic.nets.insert(
            "NET_NEW2".to_string(),
            Net {
                kind: NetKind::Normal,
                id: 2,
                name: "NET_NEW2".to_string(),
                ports: vec![make_instance_ref(&["R2", "P1"])],
                properties: HashMap::new(),
            },
        );

        let layout = parse(
            r#"(kicad_pcb
            (net 1 "NET_A")
            (footprint "R_0603"
                (property "Path" "R1")
                (pad "1" smd rect (net 1 "NET_A"))
            )
            (footprint "R_0603"
                (property "Path" "R2")
                (pad "1" smd rect (net 1 "NET_A"))
            )
        )"#,
        )
        .unwrap();

        let result = detect_implicit_renames(&schematic, &layout);

        assert!(result.renames.is_empty());
        assert!(result.orphaned_layout_nets.contains("NET_A"));
    }

    #[test]
    fn test_skip_when_old_name_still_in_netlist() {
        let mut schematic = Schematic::new();

        schematic
            .instances
            .insert(make_instance_ref(&["R1", "P1"]), make_port_instance(&["1"]));
        schematic
            .instances
            .insert(make_instance_ref(&["R2", "P1"]), make_port_instance(&["1"]));

        schematic.nets.insert(
            "NET_NEW".to_string(),
            Net {
                kind: NetKind::Normal,
                id: 1,
                name: "NET_NEW".to_string(),
                ports: vec![make_instance_ref(&["R1", "P1"])],
                properties: HashMap::new(),
            },
        );
        schematic.nets.insert(
            "NET_OLD".to_string(),
            Net {
                kind: NetKind::Normal,
                id: 2,
                name: "NET_OLD".to_string(),
                ports: vec![make_instance_ref(&["R2", "P1"])],
                properties: HashMap::new(),
            },
        );

        let layout = parse(
            r#"(kicad_pcb
            (net 1 "NET_OLD")
            (footprint "R_0603"
                (property "Path" "R1")
                (pad "1" smd rect (net 1 "NET_OLD"))
            )
            (footprint "R_0603"
                (property "Path" "R2")
                (pad "1" smd rect (net 1 "NET_OLD"))
            )
        )"#,
        )
        .unwrap();

        let result = detect_implicit_renames(&schematic, &layout);

        assert!(result.renames.is_empty());
    }

    #[test]
    fn test_skip_ambiguous_multiple_layout_nets() {
        let mut schematic = Schematic::new();

        schematic
            .instances
            .insert(make_instance_ref(&["R1", "P1"]), make_port_instance(&["1"]));
        schematic
            .instances
            .insert(make_instance_ref(&["R1", "P2"]), make_port_instance(&["2"]));

        schematic.nets.insert(
            "NET_NEW".to_string(),
            Net {
                kind: NetKind::Normal,
                id: 1,
                name: "NET_NEW".to_string(),
                ports: vec![
                    make_instance_ref(&["R1", "P1"]),
                    make_instance_ref(&["R1", "P2"]),
                ],
                properties: HashMap::new(),
            },
        );

        let layout = parse(
            r#"(kicad_pcb
            (net 1 "NET_A")
            (net 2 "NET_B")
            (footprint "R_0603"
                (property "Path" "R1")
                (pad "1" smd rect (net 1 "NET_A"))
                (pad "2" smd rect (net 2 "NET_B"))
            )
        )"#,
        )
        .unwrap();

        let result = detect_implicit_renames(&schematic, &layout);

        assert!(result.renames.is_empty());
        assert!(result.orphaned_layout_nets.contains("NET_A"));
        assert!(result.orphaned_layout_nets.contains("NET_B"));
    }
}
