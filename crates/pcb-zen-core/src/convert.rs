use crate::lang::interface::FrozenInterfaceValue;
use crate::lang::module::{find_moved_span, ModulePath};
use crate::lang::r#enum::EnumValue;
use crate::lang::symbol::SymbolValue;
use crate::lang::type_info::TypeInfo;
use crate::moved::{collect_existing_paths, scoped_path, Remapper};
use crate::{Diagnostic, Diagnostics, WithDiagnostics};
use crate::{
    FrozenComponentValue, FrozenModuleValue, FrozenNetValue, FrozenSpiceModelValue, NetId,
};
use itertools::Itertools;
use pcb_sch::physical::PhysicalValue;
use pcb_sch::position::Position;
use pcb_sch::{AttributeValue, Instance, InstanceRef, ModuleRef, Net, NetKind, Schematic};
use serde::{Deserialize, Serialize};
use serde_json::{Map as JsonMap, Number as JsonNumber, Value as JsonValue};
use starlark::errors::EvalSeverity;
use starlark::values::list::ListRef;
use starlark::values::record::FrozenRecord;
use starlark::values::{dict::DictRef, FrozenValue, Value, ValueLike};
use std::collections::HashSet;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// Convert a [`FrozenModuleValue`] to a [`Schematic`].
pub(crate) struct ModuleConverter {
    schematic: Schematic,
    net_to_ports: HashMap<NetId, Vec<InstanceRef>>,
    net_to_name: HashMap<NetId, String>,
    net_to_properties: HashMap<NetId, HashMap<String, AttributeValue>>,
    // Mapping <ref to component instance> -> <spice model>
    comp_models: Vec<(InstanceRef, FrozenSpiceModelValue)>,
    // Mapping <module instance ref> -> <module value> for position processing
    module_instances: Vec<(InstanceRef, FrozenModuleValue)>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_value: Option<serde_json::Value>,
}

fn serialize_signature_value(value: FrozenValue) -> Option<JsonValue> {
    Some(serialize_value(value.to_value()))
}

fn serialize_value(value: Value) -> JsonValue {
    if let Some(list) = ListRef::from_value(value) {
        return JsonValue::Array(list.iter().map(serialize_value).collect());
    }

    if let Some(dict) = DictRef::from_value(value) {
        return JsonValue::Object(
            dict.iter()
                .map(|(key, val)| {
                    let key = key
                        .unpack_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| key.to_string());
                    (key, serialize_value(val))
                })
                .collect(),
        );
    }

    if let Some(net) = value.downcast_ref::<FrozenNetValue>() {
        return serialize_net(net);
    }

    if let Some(interface) = value.downcast_ref::<FrozenInterfaceValue>() {
        return serialize_interface(interface);
    }

    match value.to_json_value() {
        Ok(json) => json,
        Err(_) => {
            let mut unsupported = JsonMap::new();
            unsupported.insert(
                "Unsupported".to_string(),
                JsonValue::String(value.get_type().to_string()),
            );
            JsonValue::Object(unsupported)
        }
    }
}

fn serialize_net(net: &FrozenNetValue) -> JsonValue {
    let properties = JsonValue::Object(
        net.properties()
            .iter()
            .map(|(key, val)| (key.clone(), serialize_value(val.to_value())))
            .collect(),
    );

    wrap(
        "Net",
        JsonValue::Object(JsonMap::from_iter([
            (
                "id".to_string(),
                JsonValue::Number(JsonNumber::from(net.id())),
            ),
            ("name".to_string(), JsonValue::String(net.name().to_owned())),
            ("properties".to_string(), properties),
        ])),
    )
}

fn serialize_interface(interface: &FrozenInterfaceValue) -> JsonValue {
    let inner = JsonMap::from_iter([(
        "fields".to_string(),
        JsonValue::Object(
            interface
                .fields()
                .iter()
                .map(|(key, val)| (key.clone(), serialize_value(val.to_value())))
                .collect(),
        ),
    )]);

    wrap("Interface", JsonValue::Object(inner))
}

fn wrap(tag: &str, inner: JsonValue) -> JsonValue {
    JsonValue::Object(JsonMap::from_iter([(tag.to_string(), inner)]))
}

impl ModuleConverter {
    pub(crate) fn new() -> Self {
        Self {
            schematic: Schematic::new(),
            net_to_ports: HashMap::new(),
            net_to_name: HashMap::new(),
            net_to_properties: HashMap::new(),
            comp_models: Vec::new(),
            module_instances: Vec::new(),
        }
    }

    pub(crate) fn build(
        mut self,
        module_tree: BTreeMap<ModulePath, FrozenModuleValue>,
    ) -> crate::WithDiagnostics<Schematic> {
        let root_module = module_tree.get(&ModulePath::root()).unwrap();
        let root_instance_ref = InstanceRef::new(
            ModuleRef::new(root_module.source_path(), "<root>"),
            Vec::new(),
        );
        self.schematic.set_root_ref(root_instance_ref);

        for (path, module) in module_tree.iter() {
            let instance_ref = InstanceRef::new(
                ModuleRef::new(root_module.source_path(), root_module.path().name()),
                path.segments.clone(),
            );
            if let Err(err) = self.add_module_at(module, &instance_ref) {
                let mut diagnostics = Diagnostics::default();
                diagnostics.push(err.into());
                return WithDiagnostics {
                    output: None,
                    diagnostics,
                };
            }

            // Link child to parent module
            if let Some(parent_path) = path.parent() {
                let parent_ref = InstanceRef::new(
                    ModuleRef::new(root_module.source_path(), root_module.path().name()),
                    parent_path.segments.clone(),
                );
                if let Some(parent_inst) = self.schematic.instances.get_mut(&parent_ref) {
                    parent_inst.add_child(module.path().name(), instance_ref.clone());
                }
            }
        }

        // Propagate impedance from DiffPair interfaces to P/N nets (before creating Net objects)
        propagate_diffpair_impedance(&mut self.net_to_properties, &module_tree);

        // Create Net objects directly using the names recorded per-module.
        // Ensure global uniqueness and stable creation order by sorting names.
        let mut ids_and_names: Vec<(NetId, String)> = Vec::new();
        for net_id in self.net_to_name.keys() {
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
                    let mut diagnostics = Diagnostics::default();
                    diagnostics.push(Diagnostic::new(
                        format!("Duplicate net name: {name}"),
                        EvalSeverity::Error,
                        Path::new(root_module.source_path()),
                    ));
                    return WithDiagnostics {
                        output: None,
                        diagnostics,
                    };
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
        for (instance_ref, model) in &self.comp_models {
            assert!(self.schematic.instances.contains_key(instance_ref));
            let comp_inst: &mut Instance = self.schematic.instances.get_mut(instance_ref).unwrap();
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

        // Validate moved directives, collect warnings, and filter out problematic ones
        let (diagnostics, filtered_moved_paths) = self.validate_and_filter_moved_directives();

        self.schematic.moved_paths = filtered_moved_paths;
        self.post_process_all_positions();

        WithDiagnostics {
            output: Some(self.schematic),
            diagnostics,
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

        // Consolidate DNP handling for modules: check legacy properties
        // (modules don't have dnp field, they set it via properties)
        let legacy_keys = ["do_not_populate", "Do_not_populate", "DNP", "dnp"];
        let is_dnp = legacy_keys.iter().any(|&key| {
            module
                .properties()
                .get(key)
                .map(|val| {
                    // Try to interpret the value as a boolean
                    if let Some(s) = val.downcast_frozen_str() {
                        let s_str = s.to_string();
                        s_str.to_lowercase() == "true" || s_str == "1"
                    } else {
                        val.unpack_bool().unwrap_or_default()
                    }
                })
                .unwrap_or(false)
        });

        // Only emit DNP attribute when it's true (false is the default)
        if is_dnp {
            inst.add_attribute(crate::attrs::DNP.to_string(), AttributeValue::Boolean(true));
        }

        // Build the module signature
        let mut signature = ModuleSignature {
            parameters: Vec::new(),
        };

        // Process the module's signature
        for param in module.signature().iter() {
            let type_info = TypeInfo::from_value(param.type_value.to_value());
            // Add to signature
            signature.parameters.push(ParameterInfo {
                name: param.name.clone(),
                typ: type_info,
                optional: param.optional,
                has_default: param.default_value.is_some(),
                is_config: param.is_config,
                help: param.help.clone(),
                value: param.actual_value.and_then(serialize_signature_value),
                default_value: param.default_value.and_then(serialize_signature_value),
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

        for (net_id, net_info) in module.introduced_nets().iter() {
            let final_name = if module_path.is_empty() {
                net_info.final_name.clone()
            } else {
                format!("{module_path}.{}", net_info.final_name)
            };
            self.net_to_name.insert(*net_id, final_name);
        }

        // Add direct child components
        for component in module.components() {
            let child_ref = instance_ref.append(component.name().to_string());
            self.add_component_at(component, &child_ref)?;
            inst.add_child(component.name().to_string(), child_ref.clone());
        }

        // Add instance to schematic.
        self.schematic.add_instance(instance_ref.clone(), inst);

        // Record this module instance for position post-processing
        self.module_instances
            .push((instance_ref.clone(), module.clone()));

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

        if let Some(manufacturer) = component.manufacturer() {
            comp_inst.add_attribute(
                crate::attrs::MANUFACTURER,
                AttributeValue::String(manufacturer.to_owned()),
            );
        }

        if let Some(ctype) = component.ctype() {
            comp_inst.add_attribute(crate::attrs::TYPE, AttributeValue::String(ctype.to_owned()));
        }

        if let Some(datasheet) = component.datasheet() {
            comp_inst.add_attribute(
                crate::attrs::DATASHEET,
                AttributeValue::String(datasheet.to_owned()),
            );
        }

        if let Some(description) = component.description() {
            comp_inst.add_attribute(
                crate::attrs::DESCRIPTION,
                AttributeValue::String(description.to_owned()),
            );
        }

        // Add any properties defined directly on the component.
        for (key, val) in component.properties().iter() {
            let attr_value = to_attribute_value(*val)?;
            comp_inst.add_attribute(key.clone(), attr_value);
        }

        // Handle DNP, skip_bom, and skip_pos (legacy properties already consolidated in Component constructor)
        add_bool_attribute_if_true(&mut comp_inst, crate::attrs::DNP, component.dnp());
        add_bool_attribute_if_true(&mut comp_inst, crate::attrs::SKIP_BOM, component.skip_bom());
        add_bool_attribute_if_true(&mut comp_inst, crate::attrs::SKIP_POS, component.skip_pos());

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

    fn post_process_all_positions(&mut self) {
        let remapper = Remapper::from_path_map(self.schematic.moved_paths.clone());

        for (instance_ref, module) in &self.module_instances {
            let module_path = instance_ref.instance_path.join(".");
            for (key, pos) in module.positions().iter() {
                let scoped_key = scoped_path(&module_path, key);
                let remapped_key = remapper.remap(&scoped_key).unwrap_or(scoped_key.clone());
                let is_canonical = remapped_key == scoped_key;
                let final_key = remapped_key
                    .strip_prefix(&format!("{}.", module_path))
                    .unwrap_or(&remapped_key);

                let position = Position {
                    x: pos.x,
                    y: pos.y,
                    rotation: pos.rotation,
                };

                // Determine position type and convert to unified format using the remapped key
                let symbol_key = if self.is_instance_position(final_key, instance_ref).is_some() {
                    // Component position: component_name -> comp:component_name
                    Some(format!("comp:{}", final_key))
                } else {
                    self.find_net_symbol_key(final_key, module, instance_ref)
                };

                if let (Some(symbol_key), Some(instance)) =
                    (symbol_key, self.schematic.instances.get_mut(instance_ref))
                {
                    // Only insert if we don't have this symbol yet, or if this is canonical (new name)
                    if !instance.symbol_positions.contains_key(&symbol_key) || is_canonical {
                        instance.symbol_positions.insert(symbol_key, position);
                    }
                }
            }
        }
    }

    fn is_instance_position(&self, key: &str, instance_ref: &InstanceRef) -> Option<()> {
        // Strip @U suffix from the key if present (for multi-unit symbols)
        // e.g., "U1.OPEN_Q_6490CS@U1" -> "U1.OPEN_Q_6490CS"
        let key_without_unit = key.split('@').next().unwrap_or(key);

        // Traverse the instance hierarchy using the dot-separated key
        key_without_unit
            .split('.')
            .try_fold(instance_ref, |current_ref, part| {
                self.schematic
                    .instances
                    .get(current_ref)?
                    .children
                    .get(part)
            })
            .filter(|final_ref| self.schematic.instances.contains_key(final_ref))
            .map(|_| ())
    }

    fn find_net_symbol_key(
        &self,
        key: &str,
        module: &FrozenModuleValue,
        instance_ref: &InstanceRef,
    ) -> Option<String> {
        let (net_part, suffix) = key.rsplit_once('.').unwrap_or((key, "1"));

        // First try: public io() nets from signature - these need net ID lookup to get actual name
        for param in module.signature().iter().filter(|p| !p.is_config) {
            if let Some(default_net_name) = param.default_value.and_then(|v| {
                v.downcast_ref::<FrozenNetValue>()
                    .map(|n| n.name().to_string())
                    .or_else(|| {
                        v.downcast_ref::<FrozenInterfaceValue>()?
                            .fields()
                            .get("NET")?
                            .downcast_ref::<FrozenNetValue>()
                            .map(|n| n.name().to_string())
                    })
            }) {
                if default_net_name == net_part {
                    // Get the actual net name from the net ID
                    let net_id = if let Some(net_value) =
                        param.actual_value?.downcast_ref::<FrozenNetValue>()
                    {
                        net_value.id()
                    } else if let Some(net_value) = param
                        .actual_value?
                        .downcast_ref::<FrozenInterfaceValue>()?
                        .fields()
                        .get("NET")?
                        .downcast_ref::<FrozenNetValue>()
                    {
                        net_value.id()
                    } else {
                        continue;
                    };

                    // Look up actual net name and construct symbol key
                    if let Some(actual_net_name) = self.net_to_name.get(&net_id) {
                        return Some(format!("sym:{}#{}", actual_net_name, suffix));
                    }
                }
            }
        }

        // Second try: internal nets - construct symbol key directly from fq_name
        let fq_name = if instance_ref.instance_path.is_empty() {
            // Root module - net name is not prefixed
            net_part.to_string()
        } else {
            // Sub-module - prefix with module path
            format!("{}.{}", instance_ref.instance_path.join("."), net_part)
        };

        // Check if this internal net exists in our net mappings
        if self.net_to_name.values().any(|name| name == &fq_name) {
            Some(format!("sym:{}#{}", fq_name, suffix))
        } else {
            None
        }
    }

    fn validate_and_filter_moved_directives(&self) -> (Diagnostics, HashMap<String, String>) {
        let mut diagnostics = Diagnostics::default();
        let mut filtered = HashMap::new();
        let existing = collect_existing_paths(&self.schematic.instances, &self.schematic.nets);
        for (instance_ref, module) in &self.module_instances {
            let module_path = instance_ref.instance_path.join(".");
            for (old, (new, auto_generated)) in module.moved_directives().iter() {
                let old_scoped = scoped_path(&module_path, old);
                let new_scoped = scoped_path(&module_path, new);
                let source = Path::new(module.source_path());

                // Skip warnings for auto-generated directives
                if *auto_generated {
                    if existing.contains(&new_scoped) {
                        filtered.insert(old_scoped, new_scoped.clone());
                    }
                    continue;
                }

                if existing.contains(&old_scoped) {
                    let span = find_moved_span(module.source_path(), old, new, false);
                    let body = format!("moved() references path '{}' that still exists.", old);
                    let diagnostic = Diagnostic::new(body, EvalSeverity::Warning, source);
                    diagnostics.push(diagnostic.with_span(span));
                } else if !existing.contains(&new_scoped) {
                    let span = find_moved_span(module.source_path(), old, new, true);
                    let body = format!("moved() references path '{}' that doesn't exist.", new);
                    let diagnostic = Diagnostic::new(body, EvalSeverity::Warning, source);
                    diagnostics.push(diagnostic.with_span(span));
                } else {
                    filtered.insert(old_scoped, new_scoped.clone());
                }
            }
        }

        (diagnostics, filtered)
    }
}

/// Propagate impedance from DiffPair interfaces to P/N nets
fn propagate_diffpair_impedance(
    net_props: &mut HashMap<NetId, HashMap<String, AttributeValue>>,
    tree: &BTreeMap<ModulePath, FrozenModuleValue>,
) {
    for module in tree.values() {
        for param in module.signature().iter().filter(|p| !p.is_config) {
            if let Some(val) = param.actual_value {
                propagate_from_value(val.to_value(), net_props);
            }
        }
    }
}

/// Propagate impedance from DiffPair interfaces to their P/N nets
fn propagate_from_value(
    value: Value,
    net_props: &mut HashMap<NetId, HashMap<String, AttributeValue>>,
) {
    let Some(interface) = value.downcast_ref::<FrozenInterfaceValue>() else {
        return;
    };

    // Try to extract DiffPair impedance: interface must have impedance, P, and N fields
    let fields = interface.fields();
    if let (Some(impedance_val), Some(p), Some(n)) = (
        fields.get("impedance").filter(|v| !v.is_none()),
        fields
            .get("P")
            .and_then(|v| v.downcast_ref::<FrozenNetValue>()),
        fields
            .get("N")
            .and_then(|v| v.downcast_ref::<FrozenNetValue>()),
    ) {
        if let Ok(attr) = to_attribute_value(*impedance_val) {
            net_props
                .entry(p.id())
                .or_default()
                .insert("differential_impedance".to_string(), attr.clone());
            net_props
                .entry(n.id())
                .or_default()
                .insert("differential_impedance".to_string(), attr);
        }
    }

    // Recursively check all nested interface fields
    for field in fields.values() {
        propagate_from_value(field.to_value(), net_props);
    }
}

/// Helper to add a boolean attribute only if the value is true
fn add_bool_attribute_if_true(instance: &mut Instance, attr_name: &str, value: bool) {
    if value {
        instance.add_attribute(attr_name.to_string(), AttributeValue::Boolean(true));
    }
}

fn to_attribute_value(v: starlark::values::FrozenValue) -> anyhow::Result<AttributeValue> {
    // Handle scalars first
    if let Some(s) = v.downcast_frozen_str() {
        return Ok(AttributeValue::String(s.to_string()));
    } else if let Some(n) = v.unpack_i32() {
        return Ok(AttributeValue::Number(n as f64));
    } else if let Some(b) = v.unpack_bool() {
        return Ok(AttributeValue::Boolean(b));
    } else if let Some(&physical) = v.downcast_ref::<PhysicalValue>() {
        return Ok(AttributeValue::String(physical.to_string()));
    } else if let Some(enum_val) = v.downcast_ref::<EnumValue>() {
        return Ok(AttributeValue::String(enum_val.value().to_string()));
    }

    if v.downcast_ref::<FrozenRecord>().is_some() {
        match v.to_value().to_json_value() {
            Ok(json) => return Ok(AttributeValue::Json(json)),
            Err(_) => {
                // If JSON conversion fails, fall back to string
                return Ok(AttributeValue::String(v.to_string()));
            }
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
