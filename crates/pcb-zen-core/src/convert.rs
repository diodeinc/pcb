use crate::lang::interface::FrozenInterfaceValue;
use crate::lang::module::{find_moved_span, ModulePath};
use crate::lang::r#enum::EnumValue;
use crate::lang::symbol::SymbolValue;
use crate::lang::type_info::TypeInfo;
use crate::moved::{
    collect_existing_paths, is_valid_moved_depth, path_depth, scoped_path, Remapper,
};
use crate::{Diagnostic, Diagnostics, WithDiagnostics};
use crate::{
    FrozenComponentValue, FrozenModuleValue, FrozenNetValue, FrozenSpiceModelValue, NetId,
};
use itertools::Itertools;
use pcb_sch::physical::PhysicalValue;
use pcb_sch::position::Position;
use pcb_sch::{AttributeValue, Instance, InstanceRef, ModuleRef, Net, Schematic};
use serde::{Deserialize, Serialize};
use serde_json::{Map as JsonMap, Number as JsonNumber, Value as JsonValue};
use starlark::errors::EvalSeverity;
use starlark::values::list::ListRef;
use starlark::values::record::FrozenRecord;
use starlark::values::{dict::DictRef, FrozenValue, Value, ValueLike};
use std::collections::{BTreeMap, HashMap};
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use tracing::info_span;

#[derive(Default)]
struct NetInfo {
    /// Canonical scoped name for this net, if already determined.
    name: Option<String>,
    /// Ports attached to this net.
    ports: Vec<InstanceRef>,
    /// Aggregated properties for this net.
    properties: HashMap<String, AttributeValue>,
    /// Starlark net type name (e.g. "Net", "Power", "Ground", "NotConnected").
    original_type_name: String,
}

/// Convert a [`FrozenModuleValue`] to a [`Schematic`].
pub(crate) struct ModuleConverter {
    schematic: Schematic,
    net_to_info: HashMap<NetId, NetInfo>,
    // Mapping <ref to component instance> -> <spice model>
    comp_models: Vec<(InstanceRef, FrozenSpiceModelValue)>,
    // Mapping <module instance ref> -> <module value> for position processing
    module_instances: Vec<(InstanceRef, FrozenModuleValue)>,
    // Net name aliases: when a net appears in multiple modules' introduced_nets,
    // the child's scoped name maps to the parent's canonical name.
    // Format: scoped_child_name -> canonical_name
    net_name_aliases: HashMap<String, String>,
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
            net_to_info: HashMap::new(),
            comp_models: Vec::new(),
            module_instances: Vec::new(),
            net_name_aliases: HashMap::new(),
        }
    }

    fn net_info_mut(&mut self, id: NetId) -> &mut NetInfo {
        self.net_to_info.entry(id).or_default()
    }

    pub(crate) fn build(
        mut self,
        module_tree: BTreeMap<ModulePath, FrozenModuleValue>,
    ) -> crate::WithDiagnostics<Schematic> {
        let _span = info_span!("schematic_convert", modules = module_tree.len()).entered();
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
        propagate_diffpair_impedance(&mut self.net_to_info, &module_tree);

        // Create Net objects directly using the accumulated NetInfo.
        // Ensure global uniqueness and stable creation order by sorting names.
        let mut ids_and_names: Vec<(NetId, String, String)> = Vec::new();
        for (net_id, net_info) in &self.net_to_info {
            let name = net_info
                .name
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| format!("N{net_id}"));
            ids_and_names.push((*net_id, name, net_info.original_type_name.clone()));
        }

        ids_and_names.sort_by(|a, b| a.1.cmp(&b.1));

        // Guard for uniqueness (skip NotConnected nets - they're allowed to have duplicate names)
        {
            let mut seen: HashSet<&str> = HashSet::new();
            for (_, name, net_type) in ids_and_names.iter() {
                // Skip NotConnected nets from duplicate check
                if net_type == "NotConnected" {
                    continue;
                }
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

        for (net_id, net_info) in &self.net_to_info {
            // Use the recorded type_name as the kind string if present, otherwise default.
            let net_kind = if net_info.original_type_name.is_empty() {
                "Net".to_string()
            } else {
                net_info.original_type_name.clone()
            };

            let mut net = Net {
                kind: net_kind,
                id: *net_id,
                name: net_info.name.clone().unwrap_or_default(),
                ports: Vec::new(),
                properties: HashMap::new(),
            };

            for port in &net_info.ports {
                net.add_port(port.clone());
            }

            // Add properties to the net.
            for (key, value) in &net_info.properties {
                net.add_property(key.clone(), value.clone());
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
                let net_info = self
                    .net_to_info
                    .get(&net_id)
                    .expect("NetInfo must exist for model net");
                let name = net_info
                    .name
                    .clone()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| format!("N{net_id}"));
                net_names.push(AttributeValue::String(name));
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
        let (mut diagnostics, mut filtered_moved_paths) =
            self.validate_and_filter_moved_directives();

        // These warnings are purely schematic/netlist semantics (not layout-specific),
        // so emit them during schematic conversion rather than in layout sync.
        self.diagnose_not_connected_multi_port(root_module.source_path(), &mut diagnostics);

        // Merge net name aliases (from nets appearing in multiple modules' introduced_nets)
        // These map the child's scoped name to the parent's canonical name.
        for (scoped_name, canonical_name) in &self.net_name_aliases {
            filtered_moved_paths
                .entry(scoped_name.clone())
                .or_insert_with(|| canonical_name.clone());
        }

        self.schematic.moved_paths = filtered_moved_paths;
        self.post_process_all_positions();

        WithDiagnostics {
            output: Some(self.schematic),
            diagnostics,
        }
    }

    fn diagnose_not_connected_multi_port(
        &self,
        root_source_path: &str,
        diagnostics: &mut Diagnostics,
    ) {
        for net in self.schematic.nets.values() {
            if net.kind != "NotConnected" {
                continue;
            }

            // Unique logical ports are keyed by (refdes, pin_name).
            let ports: BTreeSet<(String, String)> = net
                .ports
                .iter()
                .filter_map(|port_ref| {
                    let (pin_name, comp_path) = port_ref.instance_path.split_last()?;
                    let comp_ref = InstanceRef {
                        module: port_ref.module.clone(),
                        instance_path: comp_path.to_vec(),
                    };

                    let refdes = self
                        .schematic
                        .instances
                        .get(&comp_ref)
                        .and_then(|inst| inst.reference_designator.clone())
                        .unwrap_or_else(|| comp_ref.instance_path.join("."));

                    Some((refdes, pin_name.clone()))
                })
                .collect();

            if ports.len() <= 1 {
                continue;
            }

            let rendered = ports
                .iter()
                .take(8)
                .map(|(refdes, pin)| format!("{refdes}.{pin}"))
                .join(", ");
            let suffix = if ports.len() <= 8 {
                String::new()
            } else {
                format!(", … (+{} more)", ports.len() - 8)
            };

            let body = format!(
                "NotConnected net connects to {} ports: {rendered}{suffix}. NotConnected nets \
                 should connect to at most one port.",
                ports.len()
            );

            diagnostics.push(Diagnostic::categorized(
                root_source_path,
                &body,
                "net.notconnected.multi_port",
                EvalSeverity::Warning,
            ));
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

        for (net_id, introduced_net) in module.introduced_nets().iter() {
            let scoped_name = if module_path.is_empty() {
                introduced_net.final_name.clone()
            } else {
                format!("{module_path}.{}", introduced_net.final_name)
            };

            // If this net already has a name (from a parent module), don't overwrite.
            // Instead, record the scoped name as an alias pointing to the canonical name.
            if let Some(canonical_name) = self
                .net_to_info
                .get(net_id)
                .and_then(|info| info.name.clone())
            {
                if scoped_name != canonical_name {
                    self.net_name_aliases.insert(scoped_name, canonical_name);
                }
            } else {
                let info = self.net_info_mut(*net_id);
                info.name = Some(scoped_name);
            }

            // Set original_type_name from introduced_nets if not already set.
            // Since parent modules are processed before children (BTreeMap ordering),
            // the creating module's original type is captured first.
            let info = self.net_info_mut(*net_id);
            if info.original_type_name.is_empty() {
                info.original_type_name = introduced_net.net_type.clone();
            }
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
        let net_info = self.net_info_mut(net.id());
        net_info.ports.push(instance_ref.clone());
        // Honor explicit names on nets encountered during connections unless already set.
        if net_info.name.is_none() {
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

            let computed_name = if local.is_empty() {
                if let Some(pref) = &module_pref {
                    format!("{pref}.N{}", net.id())
                } else {
                    String::new()
                }
            } else if let Some(pref) = &module_pref {
                format!("{pref}.{local}")
            } else {
                local.to_string()
            };

            net_info.name = Some(computed_name);
        }

        // Convert regular properties to AttributeValue if not already present.
        for (key, value) in net.properties().iter() {
            if !net_info.properties.contains_key(key) {
                if let Ok(attr_value) = to_attribute_value(*value) {
                    net_info.properties.insert(key.clone(), attr_value);
                }
            }
        }
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
                    if let Some(actual_net_name) = self
                        .net_to_info
                        .get(&net_id)
                        .and_then(|info| info.name.clone())
                    {
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
        if self
            .net_to_info
            .values()
            .any(|info| info.name.as_deref() == Some(&fq_name))
        {
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

                // Skip validation for auto-generated directives
                if *auto_generated {
                    if existing.contains(&new_scoped) {
                        filtered.insert(old_scoped, new_scoped.clone());
                    }
                    continue;
                }

                // Depth constraint: min(depth(old), depth(new)) == 1
                // At least one path must be a direct child (depth 1, no dots)
                if !is_valid_moved_depth(old, new) {
                    let span = find_moved_span(module.source_path(), old, new, false);
                    let body = format!(
                        "moved(\"{}\", \"{}\"): at least one path must be a direct child \
                         (no dots; depth 1), but got depths {} and {}",
                        old,
                        new,
                        path_depth(old),
                        path_depth(new)
                    );
                    let diagnostic = Diagnostic::new(body, EvalSeverity::Warning, source);
                    diagnostics.push(diagnostic.with_span(span));
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
    net_info: &mut HashMap<NetId, NetInfo>,
    tree: &BTreeMap<ModulePath, FrozenModuleValue>,
) {
    for module in tree.values() {
        for param in module.signature().iter().filter(|p| !p.is_config) {
            if let Some(val) = param.actual_value {
                propagate_from_value(val.to_value(), net_info);
            }
        }
    }
}

/// Propagate impedance from DiffPair interfaces to their P/N nets
fn propagate_from_value(value: Value, net_info: &mut HashMap<NetId, NetInfo>) {
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
            net_info
                .entry(p.id())
                .or_default()
                .properties
                .insert("differential_impedance".to_string(), attr.clone());
            net_info
                .entry(n.id())
                .or_default()
                .properties
                .insert("differential_impedance".to_string(), attr);
        }
    }

    // Recursively check all nested interface fields
    for field in fields.values() {
        propagate_from_value(field.to_value(), net_info);
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
