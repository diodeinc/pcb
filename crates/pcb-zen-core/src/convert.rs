use crate::{FrozenComponentValue, FrozenModuleValue, FrozenNetValue, NetId};
use pcb_sch::Net;
use pcb_sch::NetKind;
use pcb_sch::{AttributeValue, Instance, InstanceRef, ModuleRef, Schematic};
use starlark::values::FrozenValue;
use starlark::values::ValueLike;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Convert a [`FrozenModuleValue`] to a [`Schematic`].
pub(crate) struct ModuleConverter {
    schematic: Schematic,
    net_to_ports: HashMap<NetId, Vec<InstanceRef>>,
    net_to_name: HashMap<NetId, String>,
    net_to_properties: HashMap<NetId, HashMap<String, FrozenValue>>,
    refdes_counters: HashMap<String, u32>,
}

// Information about a net used during name resolution.
#[derive(Clone)]
struct NetInfo {
    id: NetId,
    ports: Vec<InstanceRef>,
    base_name: String,
    // Shortest instance path expressed as individual segments. May be empty.
    path: Vec<String>,
}

/// Compute the number of leading path segments that all paths in `paths` share.
fn common_prefix_len(paths: &[&[String]]) -> usize {
    if paths.is_empty() {
        return 0;
    }
    let mut idx = 0;
    loop {
        if paths.iter().any(|p| p.len() <= idx) {
            break;
        }
        let seg = &paths[0][idx];
        if paths.iter().any(|p| &p[idx] != seg) {
            break;
        }
        idx += 1;
    }
    idx
}

impl ModuleConverter {
    pub(crate) fn new() -> Self {
        Self {
            schematic: Schematic::new(),
            net_to_ports: HashMap::new(),
            net_to_name: HashMap::new(),
            net_to_properties: HashMap::new(),
            refdes_counters: HashMap::new(),
        }
    }

    pub(crate) fn build(mut self, module: &FrozenModuleValue) -> anyhow::Result<Schematic> {
        let root_instance_ref = InstanceRef::new(
            ModuleRef::new(module.source_path(), module.name()),
            Vec::new(),
        );

        self.add_module_at(module, &root_instance_ref)?;
        self.schematic.set_root_ref(root_instance_ref);

        // First, collect metadata for every net so that we can perform a
        // global, deterministic name-assignment pass.
        let mut nets: Vec<NetInfo> = Vec::with_capacity(self.net_to_ports.len());

        for (net_id, ports) in self.net_to_ports.iter() {
            // Determine the base name (explicit first, otherwise derived).
            let explicit = self.net_to_name.get(net_id).cloned().unwrap_or_default();

            let base_name: String = if !explicit.trim().is_empty() {
                explicit
            } else {
                // Derive name from the shortest port path.
                let derived_path = ports
                    .iter()
                    .filter_map(|p| {
                        if p.instance_path.is_empty() {
                            None
                        } else {
                            Some(p.instance_path.join("."))
                        }
                    })
                    .min_by_key(|p| p.len());

                if let Some(path) = &derived_path {
                    path.to_string()
                } else {
                    format!("N{}", *net_id)
                }
            };

            // Also capture the path segments we may need for disambiguation.
            let shortest_path_segments: Vec<String> = ports
                .iter()
                .filter_map(|p| {
                    if p.instance_path.is_empty() {
                        None
                    } else {
                        Some(p.instance_path.clone())
                    }
                })
                .min_by_key(|p| p.len())
                .unwrap_or_default();

            nets.push(NetInfo {
                id: *net_id,
                ports: ports.clone(),
                base_name,
                path: shortest_path_segments,
            });
        }

        // Group nets by their base name so we can resolve conflicts inside each group.
        let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new(); // base_name -> indices into nets
        for (idx, info) in nets.iter().enumerate() {
            groups.entry(info.base_name.clone()).or_default().push(idx);
        }

        // This will hold the final, globally-unique names per net index.
        let mut final_names: Vec<String> = vec![String::new(); nets.len()];

        // Resolve each group.
        for (_base_name, indices) in groups.iter() {
            if indices.len() == 1 {
                // No conflict, keep the base name as is.
                let idx = indices[0];
                final_names[idx] = nets[idx].base_name.clone();
                continue;
            }

            // Collect tails after stripping the common prefix.
            let paths_ref: Vec<&[String]> =
                indices.iter().map(|&i| nets[i].path.as_slice()).collect();
            let cp_len = common_prefix_len(&paths_ref);

            // Precompute tails.
            let tails: Vec<Vec<String>> = indices
                .iter()
                .map(|&i| nets[i].path[cp_len..].to_vec())
                .collect();

            let max_tail_len = tails.iter().map(|t| t.len()).max().unwrap_or(0);

            let mut k = 1;
            let mut unique_found = false;
            let mut candidate_names: Vec<String> = vec![String::new(); indices.len()];

            while k <= max_tail_len {
                let mut seen: BTreeSet<String> = BTreeSet::new();
                let mut dup = false;

                for (pos, tail) in tails.iter().enumerate() {
                    let segs = if tail.is_empty() {
                        Vec::new()
                    } else {
                        let take = std::cmp::min(k, tail.len());
                        tail[..take].to_vec()
                    };

                    let prefix = if segs.is_empty() {
                        String::new()
                    } else {
                        segs.join(".")
                    };

                    let cand = if prefix.is_empty() {
                        nets[indices[pos]].base_name.clone()
                    } else {
                        format!("{}.{}", prefix, nets[indices[pos]].base_name)
                    };

                    if !seen.insert(cand.clone()) {
                        dup = true;
                    }

                    candidate_names[pos] = cand;
                }

                if !dup {
                    unique_found = true;
                    break;
                }

                k += 1;
            }

            if !unique_found {
                // Fallback: use full tail (may be empty) and then handle duplicates via suffixes.
                let mut name_counts: HashMap<String, usize> = HashMap::new();
                for (pos, tail) in tails.iter().enumerate() {
                    let prefix = if tail.is_empty() {
                        String::new()
                    } else {
                        tail.join(".")
                    };
                    let mut name = if prefix.is_empty() {
                        nets[indices[pos]].base_name.clone()
                    } else {
                        format!("{}.{}", prefix, nets[indices[pos]].base_name)
                    };

                    let counter = name_counts.entry(name.clone()).or_insert(0);
                    if *counter > 0 {
                        name = format!("{}_{}", name, *counter);
                    }
                    *counter += 1;

                    candidate_names[pos] = name;
                }
            }

            // Commit the chosen names for this group.
            for (idx, &net_idx) in indices.iter().enumerate() {
                final_names[net_idx] = candidate_names[idx].clone();
            }
        }

        // As a last guard, ensure global uniqueness (should already be true).
        let mut used_names: HashSet<String> = HashSet::new();
        for name in final_names.iter() {
            if !used_names.insert(name.clone()) {
                return Err(anyhow::anyhow!(
                    "Internal error: duplicate net name generated: {}",
                    name
                ));
            }
        }

        // Finally, create the Net objects in a stable order (base_name, then full name).
        let mut creation_order: Vec<usize> = (0..nets.len()).collect();
        creation_order.sort_by_key(|&i| final_names[i].clone());

        for idx in creation_order {
            let info = &nets[idx];
            let unique_name = final_names[idx].clone();

            // Determine net kind from properties.
            let net_kind = if let Some(props) = self.net_to_properties.get(&info.id) {
                if let Some(type_prop) = props.get("type") {
                    match type_prop.to_value().unpack_str() {
                        Some("ground") => NetKind::Ground,
                        Some("power") => NetKind::Power,
                        _ => NetKind::Normal,
                    }
                } else {
                    NetKind::Normal
                }
            } else {
                NetKind::Normal
            };

            let mut net = Net::new(net_kind, unique_name);
            for port in info.ports.iter() {
                net.add_port(port.clone());
            }

            // Add properties to the net.
            if let Some(props) = self.net_to_properties.get(&info.id) {
                for (key, value) in props.iter() {
                    net.add_property(key.clone(), to_attribute_value(*value)?);
                }
            }

            self.schematic.add_net(net);
        }

        Ok(self.schematic)
    }

    fn add_instance_at(
        &mut self,
        instance_ref: &InstanceRef,
        value: FrozenValue,
    ) -> anyhow::Result<()> {
        if let Some(module_value) = value.downcast_ref::<FrozenModuleValue>() {
            self.add_module_at(module_value, instance_ref)
        } else if let Some(component_value) = value.downcast_ref::<FrozenComponentValue>() {
            self.add_component_at(component_value, instance_ref)
        } else {
            Err(anyhow::anyhow!("Unexpected value in module: {}", value))
        }
    }

    fn value_name(&self, value: &FrozenValue) -> anyhow::Result<String> {
        if let Some(module_value) = value.downcast_ref::<FrozenModuleValue>() {
            Ok(module_value.name().to_string())
        } else if let Some(component_value) = value.downcast_ref::<FrozenComponentValue>() {
            Ok(component_value.name().to_string())
        } else {
            Err(anyhow::anyhow!("Unexpected value in module: {}", value))
        }
    }

    fn add_module_at(
        &mut self,
        module: &FrozenModuleValue,
        instance_ref: &InstanceRef,
    ) -> anyhow::Result<()> {
        // Create instance for this module type.
        let type_modref = ModuleRef::new(module.source_path(), "<root>");
        let mut inst = Instance::module(type_modref);

        // Add only this module's own properties to this instance.
        for (key, val) in module.properties().iter() {
            // HACK: If this is a layout_path attribute and we're not at the root,
            // prepend the module's directory path to the layout path
            if key == "layout_path" && !instance_ref.instance_path.is_empty() {
                if let Ok(AttributeValue::String(layout_path)) = to_attribute_value(*val) {
                    // Get the directory of the module's source file
                    let module_dir = Path::new(module.source_path())
                        .parent()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_default();

                    let full_layout_path = if module_dir.is_empty() {
                        layout_path
                    } else {
                        format!("{module_dir}/{layout_path}")
                    };

                    inst.add_attribute(key.clone(), AttributeValue::String(full_layout_path));
                } else {
                    // If it's not a string, just add it as-is
                    inst.add_attribute(key.clone(), to_attribute_value(*val)?);
                }
            } else {
                inst.add_attribute(key.clone(), to_attribute_value(*val)?);
            }
        }

        // Recurse into children, but don't pass any properties down.
        // Each module/component should only have its own properties.
        for child in module.children().iter() {
            let child_name = self.value_name(child)?;
            let child_inst_ref = instance_ref.append(child_name.clone());
            self.add_instance_at(&child_inst_ref, *child)?;
            inst.add_child(child_name.clone(), child_inst_ref.clone());
        }

        // Add instance to schematic.
        self.schematic.add_instance(instance_ref.clone(), inst);

        Ok(())
    }

    fn next_refdes(&mut self, prefix: &str) -> String {
        let counter = self.refdes_counters.entry(prefix.to_string()).or_insert(0);
        *counter += 1;
        format!("{}{}", prefix, *counter)
    }

    fn update_net(&mut self, net: &FrozenNetValue, instance_ref: &InstanceRef) {
        let entry = self.net_to_ports.entry(net.id()).or_default();
        entry.push(instance_ref.clone());
        self.net_to_name.insert(net.id(), net.name().to_string());

        // Store net properties if not already stored
        self.net_to_properties.entry(net.id()).or_insert_with(|| {
            let mut props_map = HashMap::new();
            for (key, value) in net.properties().iter() {
                props_map.insert(key.clone(), *value);
            }
            props_map
        });
    }

    fn add_component_at(
        &mut self,
        component: &FrozenComponentValue,
        instance_ref: &InstanceRef,
    ) -> anyhow::Result<()> {
        // Child is a component.
        let comp_type_ref = ModuleRef::new(component.source_path(), component.name());
        let mut comp_inst = Instance::component(comp_type_ref.clone());

        // Add component's built-in attributes.
        comp_inst.add_attribute(
            "footprint",
            AttributeValue::String(component.footprint().to_owned()),
        );

        comp_inst.add_attribute(
            "prefix",
            AttributeValue::String(component.prefix().to_owned()),
        );

        if let Some(mpn) = component.mpn() {
            comp_inst.add_attribute("mpn", AttributeValue::String(mpn.to_owned()));
        }

        if let Some(ctype) = component.ctype() {
            comp_inst.add_attribute("type", AttributeValue::String(ctype.to_owned()));
        }

        // Add any properties defined directly on the component.
        for (key, val) in component.properties().iter() {
            comp_inst.add_attribute(key.clone(), to_attribute_value(*val)?);
        }

        comp_inst.set_reference_designator(self.next_refdes(component.prefix()));

        // Add pin children.
        for (pin_name, pin_val) in component.pins().iter() {
            let pin_inst_ref = instance_ref.append(pin_name.clone());
            let mut pin_inst = Instance::port(comp_type_ref.clone());

            if let Some(pad_str) = pin_val.downcast_frozen_str() {
                pin_inst.add_attribute("pad", AttributeValue::String(pad_str.to_string()));
            }

            self.schematic.add_instance(pin_inst_ref.clone(), pin_inst);
            comp_inst.add_child(pin_name.clone(), pin_inst_ref.clone());

            // If the pin is connected, record it in net_map
            if let Some(net_val) = component.connections().get(pin_name) {
                let net = net_val
                    .downcast_ref::<FrozenNetValue>()
                    .ok_or(anyhow::anyhow!(
                        "Expected net value for pin '{}', found '{}'",
                        pin_name,
                        net_val
                    ))?;

                self.update_net(net, &pin_inst_ref);
            }
        }

        // Finish component instance.
        self.schematic.add_instance(instance_ref.clone(), comp_inst);

        Ok(())
    }
}

pub trait ToSchematic {
    fn to_schematic(&self) -> anyhow::Result<Schematic>;
}

fn to_attribute_value(v: starlark::values::FrozenValue) -> anyhow::Result<AttributeValue> {
    if let Some(s) = v.downcast_frozen_str() {
        Ok(AttributeValue::String(s.to_string()))
    } else if let Some(n) = v.unpack_i32() {
        Ok(AttributeValue::Number(n as f64))
    } else if let Some(b) = v.unpack_bool() {
        Ok(AttributeValue::Boolean(b))
    } else {
        // For now, convert other types to their string representation
        // This handles floats, lists, and other complex types
        Ok(AttributeValue::String(v.to_string()))
    }
}

impl ToSchematic for FrozenModuleValue {
    fn to_schematic(&self) -> anyhow::Result<Schematic> {
        let converter = ModuleConverter::new();
        converter.build(self)
    }
}
