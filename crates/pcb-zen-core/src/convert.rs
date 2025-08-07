use crate::lang::symbol::SymbolValue;
use crate::lang::type_info::TypeInfo;
use crate::{FrozenComponentValue, FrozenModuleValue, FrozenNetValue, InputValue, NetId};
use pcb_sch::Net;
use pcb_sch::NetKind;
use pcb_sch::{AttributeValue, Instance, InstanceRef, ModuleRef, Schematic};
use serde::{Deserialize, Serialize};
use starlark::values::FrozenValue;
use starlark::values::ValueLike;
use std::collections::HashMap;
use std::collections::HashSet;
// removed unused BTree imports after refactor
use std::path::Path;

/// Convert a [`FrozenModuleValue`] to a [`Schematic`].
pub(crate) struct ModuleConverter {
    schematic: Schematic,
    net_to_ports: HashMap<NetId, Vec<InstanceRef>>,
    net_to_name: HashMap<NetId, String>,
    net_to_properties: HashMap<NetId, HashMap<String, AttributeValue>>,
    refdes_counters: HashMap<String, u32>,
}

/// Module signature information to be serialized as JSON
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ModuleSignature {
    parameters: Vec<ParameterInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ParameterInfo {
    name: String,
    typ: TypeInfo,
    optional: bool,
    has_default: bool,
    is_config: bool, // true for config(), false for io()
    help: Option<String>,
    value: Option<InputValue>,
    default_value: Option<InputValue>,
}

// Name resolution is now deterministic at net creation time; legacy helpers removed.

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

        // Create Net objects directly using the names recorded per-module.
        // Ensure global uniqueness and stable creation order by sorting names.
        let mut ids_and_names: Vec<(NetId, String)> = Vec::new();
        for net_id in self.net_to_ports.keys() {
            let name = self
                .net_to_name
                .get(net_id)
                .filter(|s| !s.is_empty())
                .cloned()
                .unwrap_or_else(|| format!("N{net_id}"));
            ids_and_names.push((*net_id, name));
        }

        ids_and_names.sort_by(|a, b| a.1.cmp(&b.1));

        // Guard for uniqueness
        {
            let mut seen: HashSet<&str> = HashSet::new();
            for (_, name) in ids_and_names.iter() {
                if !seen.insert(name.as_str()) {
                    return Err(anyhow::anyhow!("Duplicate net name: {name}"));
                }
            }
        }

        for (net_id, unique_name) in ids_and_names {
            // Determine net kind from properties.
            let net_kind = if let Some(props) = self.net_to_properties.get(&net_id) {
                if let Some(type_prop) = props.get("type") {
                    match type_prop.string() {
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

            let mut net = Net::new(net_kind, unique_name, net_id);
            if let Some(ports) = self.net_to_ports.get(&net_id) {
                for port in ports.iter() {
                    net.add_port(port.clone());
                }
            }

            // Add properties to the net.
            if let Some(props) = self.net_to_properties.get(&net_id) {
                for (key, value) in props.iter() {
                    net.add_property(key.clone(), value.clone());
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
        let mut inst = Instance::module(type_modref.clone());

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

        // Build the module signature
        let mut signature = ModuleSignature {
            parameters: Vec::new(),
        };

        // Process the module's signature
        for param in module.signature().iter() {
            // Add to signature
            signature.parameters.push(ParameterInfo {
                name: param.name.clone(),
                typ: TypeInfo::from_value(param.type_value.to_value()),
                optional: param.optional,
                has_default: param.default_value.is_some(),
                is_config: param.is_config,
                help: param.help.clone(),
                value: param
                    .actual_value
                    .map(|v| InputValue::from_value(v.to_value())),
                default_value: param
                    .default_value
                    .map(|v| InputValue::from_value(v.to_value())),
            });
        }

        // Add the signature as a JSON attribute
        if !signature.parameters.is_empty() {
            let signature_json = serde_json::to_value(&signature).unwrap_or_default();
            inst.add_attribute("__signature", AttributeValue::Json(signature_json));
        }

        // Record final names for nets introduced by this module using the instance path.
        // For the root module, no prefix is added.
        let module_path = instance_ref.instance_path.join(".");

        for (net_id, local_name) in module.introduced_nets().iter() {
            let final_name = if module_path.is_empty() {
                local_name.clone()
            } else {
                format!("{module_path}.{local_name}")
            };
            self.net_to_name.insert(*net_id, final_name);
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
        // Honor explicit names on nets encountered during connections unless already set
        self.net_to_name.entry(net.id()).or_insert_with(|| {
            let local = net.name();
            let module_pref = if instance_ref.instance_path.len() >= 2 {
                let module_segments =
                    &instance_ref.instance_path[..instance_ref.instance_path.len() - 2];
                if module_segments.is_empty() {
                    None
                } else {
                    Some(module_segments.join("."))
                }
            } else {
                None
            };

            if local.is_empty() {
                if let Some(pref) = module_pref {
                    format!("{pref}.N{}", net.id())
                } else {
                    String::new()
                }
            } else if let Some(pref) = module_pref {
                format!("{pref}.{local}")
            } else {
                local.to_string()
            }
        });

        self.net_to_properties.entry(net.id()).or_insert_with(|| {
            let mut props_map = HashMap::new();

            // Convert regular properties to AttributeValue
            for (key, value) in net.properties().iter() {
                if let Ok(attr_value) = to_attribute_value(*value) {
                    props_map.insert(key.clone(), attr_value);
                }
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

        // Add symbol information if the component has a symbol
        let symbol_value = component.symbol();
        if !symbol_value.is_none() {
            if let Some(symbol) = symbol_value.downcast_ref::<SymbolValue>() {
                // Add symbol_name for backwards compatibility
                if let Some(name) = symbol.name() {
                    comp_inst.add_attribute(
                        "symbol_name".to_string(),
                        AttributeValue::String(name.to_string()),
                    );
                }

                // Add symbol_path for backwards compatibility
                if let Some(path) = symbol.source_path() {
                    comp_inst.add_attribute(
                        "symbol_path".to_string(),
                        AttributeValue::String(path.to_string()),
                    );
                }

                // Add the raw s-expression if available
                let raw_sexp = symbol.raw_sexp();
                if let Some(sexp_string) = raw_sexp {
                    // The raw_sexp is stored as a string value in the SymbolValue
                    comp_inst.add_attribute(
                        "__symbol_value".to_string(),
                        AttributeValue::String(sexp_string.to_string()),
                    );
                }
            }
        }

        comp_inst.set_reference_designator(self.next_refdes(component.prefix()));

        // Get the symbol from the component to access pin mappings
        let symbol = component.symbol();
        if let Some(symbol_value) = symbol.downcast_ref::<SymbolValue>() {
            // First, group pads by signal name
            let mut signal_to_pads: HashMap<String, Vec<String>> = HashMap::new();

            for (pad_number, signal_val) in symbol_value.pad_to_signal().iter() {
                signal_to_pads
                    .entry(signal_val.to_string())
                    .or_default()
                    .push(pad_number.clone());
            }

            // Now create one port per signal
            for (signal_name, pads) in signal_to_pads.iter() {
                // Create a unique instance reference using the signal name
                let pin_inst_ref = instance_ref.append(signal_name.to_string());
                let mut pin_inst = Instance::port(comp_type_ref.clone());

                pin_inst.add_attribute(
                    "pads",
                    AttributeValue::Array(
                        pads.iter()
                            .map(|p| AttributeValue::String(p.clone()))
                            .collect(),
                    ),
                );

                self.schematic.add_instance(pin_inst_ref.clone(), pin_inst);
                comp_inst.add_child(signal_name.clone(), pin_inst_ref.clone());

                // If this signal is connected, record it in net_map
                if let Some(net_val) = component.connections().get(signal_name) {
                    let net = net_val
                        .downcast_ref::<FrozenNetValue>()
                        .ok_or(anyhow::anyhow!(
                            "Expected net value for pin '{}', found '{}'",
                            signal_name,
                            net_val
                        ))?;

                    self.update_net(net, &pin_inst_ref);
                }
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
