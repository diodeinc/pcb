use crate::lang::symbol::SymbolValue;
use crate::lang::type_info::TypeInfo;
use crate::{
    FrozenComponentValue, FrozenModuleValue, FrozenNetValue, FrozenSpiceModelValue, InputValue,
    NetId,
};
use itertools::Itertools;
use pcb_sch::Net;
use pcb_sch::NetKind;
use pcb_sch::{
    AttributeValue, Instance, InstanceRef, ModuleRef, PhysicalUnit, PhysicalValue, Schematic,
};
use serde::{Deserialize, Serialize};
use starlark::values::float::StarlarkFloat;
use starlark::values::list::ListRef;
use starlark::values::record::FrozenRecord;
use starlark::values::FrozenValue;
use starlark::values::ValueLike;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Convert a [`FrozenModuleValue`] to a [`Schematic`].
pub(crate) struct ModuleConverter {
    schematic: Schematic,
    net_to_ports: HashMap<NetId, Vec<InstanceRef>>,
    net_to_name: HashMap<NetId, String>,
    net_to_properties: HashMap<NetId, HashMap<String, AttributeValue>>,
    // Mapping <ref to component instance> -> <spice model>
    comp_models: Vec<(InstanceRef, FrozenSpiceModelValue)>,
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
            comp_models: Vec::new(),
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
                if let Some(type_prop) = props.get(crate::attrs::TYPE) {
                    match type_prop.string() {
                        Some(crate::attrs::net::kind::GROUND) => NetKind::Ground,
                        Some(crate::attrs::net::kind::POWER) => NetKind::Power,
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

        // Finalize the component models now that we have finalized the net names
        for (instance_ref, model) in self.comp_models {
            assert!(self.schematic.instances.contains_key(&instance_ref));
            let comp_inst: &mut Instance = self.schematic.instances.get_mut(&instance_ref).unwrap();
            comp_inst.add_attribute(crate::attrs::MODEL_DEF, model.definition.clone());
            comp_inst.add_attribute(crate::attrs::MODEL_NAME, model.name.clone());
            let mut net_names = Vec::new();
            for net in model.nets() {
                let net_id = net.downcast_ref::<FrozenNetValue>().unwrap().id();
                assert!(self.net_to_name.contains_key(&net_id));
                net_names.push(AttributeValue::String(
                    self.net_to_name.get(&net_id).unwrap().to_string(),
                ));
            }
            comp_inst.add_attribute(crate::attrs::MODEL_NETS, AttributeValue::Array(net_names));
            let arg_str = model
                .args()
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .join(" ");
            comp_inst.add_attribute(crate::attrs::MODEL_ARGS, AttributeValue::String(arg_str));
        }

        self.schematic.assign_reference_designators();

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
            if key == crate::attrs::LAYOUT_PATH && !instance_ref.instance_path.is_empty() {
                if let Ok(AttributeValue::String(layout_path)) = to_attribute_value(*val) {
                    // Get the directory of the module's source file
                    let module_dir = Path::new(module.source_path())
                        .parent()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_default();

                    let full_layout_path =
                        if module_dir.is_empty() || PathBuf::from(&layout_path).is_absolute() {
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
            inst.add_attribute(
                crate::attrs::SIGNATURE,
                AttributeValue::Json(signature_json),
            );
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
            crate::attrs::FOOTPRINT,
            AttributeValue::String(component.footprint().to_owned()),
        );

        comp_inst.add_attribute(
            crate::attrs::PREFIX,
            AttributeValue::String(component.prefix().to_owned()),
        );

        if let Some(mpn) = component.mpn() {
            comp_inst.add_attribute(crate::attrs::MPN, AttributeValue::String(mpn.to_owned()));
        }

        if let Some(ctype) = component.ctype() {
            comp_inst.add_attribute(crate::attrs::TYPE, AttributeValue::String(ctype.to_owned()));
        }

        // Add any properties defined directly on the component.
        for (key, val) in component.properties().iter() {
            let attr_value = to_attribute_value(*val)?;
            comp_inst.add_attribute(key.clone(), attr_value);
        }

        if let Some(model_val) = component.spice_model() {
            let model =
                model_val
                    .downcast_ref::<FrozenSpiceModelValue>()
                    .ok_or(anyhow::anyhow!(
                        "Expected spice model for component {}",
                        component.name()
                    ))?;
            self.comp_models.push((instance_ref.clone(), model.clone()));
        }

        // Add symbol information if the component has a symbol
        let symbol_value = component.symbol();
        if !symbol_value.is_none() {
            if let Some(symbol) = symbol_value.downcast_ref::<SymbolValue>() {
                // Add symbol_name for backwards compatibility
                if let Some(name) = symbol.name() {
                    comp_inst.add_attribute(
                        crate::attrs::SYMBOL_NAME.to_string(),
                        AttributeValue::String(name.to_string()),
                    );
                }

                // Add symbol_path for backwards compatibility
                if let Some(path) = symbol.source_path() {
                    comp_inst.add_attribute(
                        crate::attrs::SYMBOL_PATH.to_string(),
                        AttributeValue::String(path.to_string()),
                    );
                }

                // Add the raw s-expression if available
                let raw_sexp = symbol.raw_sexp();
                if let Some(sexp_string) = raw_sexp {
                    // The raw_sexp is stored as a string value in the SymbolValue
                    comp_inst.add_attribute(
                        crate::attrs::SYMBOL_VALUE.to_string(),
                        AttributeValue::String(sexp_string.to_string()),
                    );
                }
            }
        }

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
                    crate::attrs::PADS,
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
                            "Expected net value for pin '{}' , found '{}'",
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
    // Handle scalars first
    if let Some(s) = v.downcast_frozen_str() {
        return Ok(AttributeValue::String(s.to_string()));
    } else if let Some(n) = v.unpack_i32() {
        return Ok(AttributeValue::Number(n as f64));
    } else if let Some(b) = v.unpack_bool() {
        return Ok(AttributeValue::Boolean(b));
    }

    // Handle unit records (Resistance, Capacitance, Voltage, etc.)
    if let Some(record) = v.downcast_ref::<FrozenRecord>() {
        let mut record_value = None;
        let mut record_tolerance = None;
        let mut record_unit = None;

        // Extract fields from the record
        for (field_name, field_value) in record.iter() {
            let field_name_str = field_name.to_string();
            if field_name_str == crate::attrs::record_fields::VALUE {
                // Try f64 first, fall back to i32 converted to f64
                if let Some(f) = field_value.downcast_ref::<StarlarkFloat>() {
                    record_value = Some(f.0);
                }
            } else if field_name_str == crate::attrs::record_fields::TOLERANCE {
                if let Some(f) = field_value.downcast_ref::<StarlarkFloat>() {
                    record_tolerance = Some(f.0);
                }
            } else if field_name_str == crate::attrs::record_fields::UNIT {
                // Unit is an enum like Ohms("Ohms"), parse to PhysicalUnit
                let unit_str = field_value.to_string();
                // Extract content within parentheses and remove quotes
                let unit_name = if let Some(start) = unit_str.find('(') {
                    if let Some(end) = unit_str.find(')') {
                        let inner = &unit_str[start + 1..end];
                        inner.trim_matches('"').to_string()
                    } else {
                        unit_str.trim_matches('"').to_string()
                    }
                } else {
                    unit_str.trim_matches('"').to_string()
                };

                record_unit = match unit_name.as_str() {
                    "Ohms" | "Ohm" => Some(PhysicalUnit::Ohms),
                    "V" | "Volts" => Some(PhysicalUnit::Volts),
                    "A" | "Amperes" => Some(PhysicalUnit::Amperes),
                    "F" | "Farads" => Some(PhysicalUnit::Farads),
                    "H" | "Henries" => Some(PhysicalUnit::Henries),
                    "Hz" | "Hertz" => Some(PhysicalUnit::Hertz),
                    "s" | "Seconds" => Some(PhysicalUnit::Seconds),
                    "K" | "Kelvin" => Some(PhysicalUnit::Kelvin),
                    _ => None, // Unknown unit, will fall back to string conversion
                };
            } else {
                // Ignore other fields like __str__
            }
        }

        // If we have all required fields, this is a unit record
        if let (Some(value), Some(tolerance), Some(unit)) =
            (record_value, record_tolerance, record_unit)
        {
            return Ok(AttributeValue::Physical(PhysicalValue::new(
                value, tolerance, unit,
            )));
        }
    }

    // Handle lists (no nested list support)
    if let Some(list) = ListRef::from_value(v.to_value()) {
        let mut elements = Vec::with_capacity(list.len());
        for item in list.iter() {
            let attr = if let Some(s) = item.unpack_str() {
                AttributeValue::String(s.to_string())
            } else if let Some(n) = item.unpack_i32() {
                AttributeValue::Number(n as f64)
            } else if let Some(b) = item.unpack_bool() {
                AttributeValue::Boolean(b)
            } else {
                // Any nested lists or other types get stringified
                AttributeValue::String(item.to_string())
            };
            elements.push(attr);
        }
        return Ok(AttributeValue::Array(elements));
    }

    // Any other type – fall back to string representation
    Ok(AttributeValue::String(v.to_string()))
}

impl ToSchematic for FrozenModuleValue {
    fn to_schematic(&self) -> anyhow::Result<Schematic> {
        let converter = ModuleConverter::new();
        converter.build(self)
    }
}
