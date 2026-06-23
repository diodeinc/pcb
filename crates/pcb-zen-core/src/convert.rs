use crate::lang::r#enum::EnumValue;
use crate::lang::interface::FrozenInterfaceValue;
use crate::lang::io_direction::IoDirection;
use crate::lang::module::{ModulePath, find_moved_span};
use crate::lang::net::net_kind_requires_name;
use crate::lang::part::PartValue;
use crate::lang::symbol::SymbolValue;
use crate::lang::type_info::TypeInfo;
use crate::moved::{
    Remapper, collect_existing_paths, is_valid_moved_depth, path_depth, scoped_path,
};
use crate::{Diagnostic, Diagnostics, WithDiagnostics};
use crate::{
    FrozenComponentValue, FrozenModuleValue, FrozenNetValue, FrozenSpiceModelValue, NetId,
};
use itertools::Itertools;
use pcb_sch::physical::PhysicalValue;
use pcb_sch::position::{MirrorAxis, Position};
use pcb_sch::{AttributeValue, Instance, InstanceKind, InstanceRef, ModuleRef, Net, Schematic};
use serde::{Deserialize, Serialize};
use serde_json::{Map as JsonMap, Number as JsonNumber, Value as JsonValue};
use starlark::values::list::ListRef;
use starlark::values::{FrozenValue, Value, ValueLike, dict::DictRef};
use starlark::{codemap::ResolvedSpan, errors::EvalSeverity};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;
use tracing::info_span;

#[derive(Default)]
struct NetInfo {
    /// Canonical scoped name for this net, if already determined.
    name: Option<String>,
    /// Ports attached to this net.
    ports: Vec<InstanceRef>,
    /// Aggregated properties for this net.
    properties: HashMap<String, AttributeValue>,
    /// Starlark net kind, if observed during conversion.
    kind: Option<String>,
}

fn net_info_requires_name(kind: Option<&str>) -> bool {
    kind.is_none_or(net_kind_requires_name)
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
    direction: Option<IoDirection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_value: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    allowed_values: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    allowed_display: Option<Vec<String>>,
}

fn serialize_signature_value(value: FrozenValue) -> Option<JsonValue> {
    Some(serialize_value(value.to_value()))
}

fn serialize_signature_values(values: Option<&Vec<FrozenValue>>) -> Option<Vec<JsonValue>> {
    values.map(|values| {
        values
            .iter()
            .filter_map(|value| serialize_signature_value(*value))
            .collect()
    })
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

    if let Some(enum_value) = value.downcast_ref::<EnumValue>() {
        return JsonValue::String(enum_value.value().to_string());
    }

    if let Some(&physical) = value.downcast_ref::<PhysicalValue>() {
        return JsonValue::String(physical.to_string());
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

/// Walk a signature parameter's default value (Net or Interface, possibly nested) and return the
/// path of field names leading to a net whose name equals `target_net_name`. Returns `Some(vec![])`
/// when the value itself is a matching net.
fn find_net_field_path(value: FrozenValue, target_net_name: &str) -> Option<Vec<String>> {
    if let Some(net) = value.downcast_ref::<FrozenNetValue>() {
        return (net.name() == target_net_name).then(Vec::new);
    }
    let interface = value.downcast_ref::<FrozenInterfaceValue>()?;
    for (field_name, field_value) in interface.fields().iter() {
        if let Some(mut path) = find_net_field_path(*field_value, target_net_name) {
            path.insert(0, field_name.clone());
            return Some(path);
        }
    }
    None
}

/// Resolve a field path (as produced by [`find_net_field_path`]) against a value, returning the
/// net id at the end of the path. The terminal value must be a [`FrozenNetValue`].
fn resolve_net_id_along_path(value: FrozenValue, path: &[String]) -> Option<NetId> {
    let Some((head, tail)) = path.split_first() else {
        return value.downcast_ref::<FrozenNetValue>().map(|n| n.id());
    };
    let interface = value.downcast_ref::<FrozenInterfaceValue>()?;
    let next_value = *interface.fields().get(head)?;
    resolve_net_id_along_path(next_value, tail)
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
        for (net_id, net_info) in &self.net_to_info {
            if net_info.kind.as_deref() == Some("NotConnected") && net_info.ports.is_empty() {
                continue;
            }

            if net_info_requires_name(net_info.kind.as_deref()) && net_info.name.is_none() {
                let mut diagnostics = Diagnostics::default();
                diagnostics.push(Diagnostic::new(
                    "Net is unnamed",
                    EvalSeverity::Error,
                    Path::new(root_module.source_path()),
                ));
                return WithDiagnostics {
                    output: None,
                    diagnostics,
                };
            }
            let net_kind = net_info.kind.clone().unwrap_or_else(|| "Net".to_string());
            let net_name = net_info.name.clone().unwrap_or_else(|| {
                debug_assert_eq!(net_kind, "NotConnected");
                String::new()
            });

            let mut net = Net {
                kind: net_kind,
                id: *net_id,
                name: net_name,
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
                let name = net_info.name.clone().unwrap_or_else(|| {
                    debug_assert_eq!(net_info.kind.as_deref(), Some("NotConnected"));
                    String::new()
                });
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

        // These diagnostics are purely schematic/netlist semantics (not layout-specific),
        // so emit them during schematic conversion rather than in layout sync.
        self.diagnose_missing_bom_part_components(&mut diagnostics);
        self.diagnose_unused_module_io(&module_tree, &mut diagnostics);
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
                    let (comp_ref, pin_name) =
                        self.schematic.component_ref_and_pin_for_port(port_ref)?;

                    let refdes = self
                        .schematic
                        .instances
                        .get(&comp_ref)
                        .and_then(|inst| inst.reference_designator.clone())
                        .unwrap_or_else(|| comp_ref.instance_path.join("."));

                    Some((refdes, pin_name))
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

    fn diagnose_missing_bom_part_components(&self, diagnostics: &mut Diagnostics) {
        for instance in self.schematic.instances.values() {
            if instance.kind != InstanceKind::Component
                || instance.dnp()
                || instance.skip_bom()
                || instance.part().is_some()
            {
                continue;
            }

            let name = instance
                .reference_designator
                .as_deref()
                .unwrap_or(instance.type_ref.module_name.as_ref());
            let (kind, body) = match instance.string_attr(&[crate::attrs::BOM_MPN]) {
                Some(_) => (
                    "bom.underspecified",
                    format!(
                        "Component '{name}' is included in the BOM but is missing manufacturer information. Specify `part=Part(...)`."
                    ),
                ),
                None if Self::is_house_bom_match_eligible(instance) => continue,
                None => (
                    "bom.unspecified",
                    format!(
                        "Component '{name}' is included in the BOM but is missing part information. Specify `part=Part(...)`."
                    ),
                ),
            };
            diagnostics.push(Diagnostic::categorized(
                &instance.type_ref.source_path.to_string_lossy(),
                &body,
                kind,
                EvalSeverity::Error,
            ));
        }
    }

    fn is_house_bom_match_eligible(instance: &Instance) -> bool {
        match instance.component_type().as_deref() {
            Some(
                "resistor" | "capacitor" | "led" | "ferrite_bead" | "inductor" | "rectifier"
                | "zener" | "tvs" | "crystal",
            ) => true,
            Some("connector") => instance
                .string_attr(&["connector_type", "Connector_type"])
                .is_some_and(|connector_type| {
                    connector_type.eq_ignore_ascii_case("pin header")
                        || connector_type.eq_ignore_ascii_case("terminal block")
                }),
            _ => false,
        }
    }

    fn diagnose_unused_module_io(
        &self,
        module_tree: &BTreeMap<ModulePath, FrozenModuleValue>,
        diagnostics: &mut Diagnostics,
    ) {
        for (path, module) in module_tree {
            for param in module.signature().iter().filter(|p| !p.is_config) {
                let Some(actual_value) = param.actual_value else {
                    continue;
                };

                let net_ids = collect_net_ids_from_value(actual_value.to_value());
                if net_ids.is_empty() {
                    continue;
                }

                let used_in_module = net_ids.iter().any(|net_id| {
                    self.net_to_info.get(net_id).is_some_and(|info| {
                        info.ports
                            .iter()
                            .any(|port| port.instance_path.starts_with(&path.segments))
                    })
                });

                if used_in_module {
                    continue;
                }

                let body = if path.is_root() {
                    format!("io() '{}' is not connected to any ports", param.name)
                } else {
                    format!(
                        "io() '{}' in module '{}' is not connected to any ports",
                        param.name, path
                    )
                };

                let diagnostic_path = param
                    .declaration_call_stack
                    .frames
                    .iter()
                    .rev()
                    .find_map(|frame| frame.location.as_ref())
                    .map(|loc| loc.file.filename().to_string())
                    .unwrap_or_else(|| module.source_path().to_string());

                diagnostics.push(
                    Diagnostic::categorized(
                        &diagnostic_path,
                        &body,
                        "module.io.unused",
                        EvalSeverity::Warning,
                    )
                    .with_span(param.declaration_span)
                    .with_call_stack(Some(param.declaration_call_stack.clone())),
                );
            }
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
            inst.add_attribute(key.clone(), to_attribute_value(*val)?);
        }

        // Consolidate DNP handling for modules. `Module(..., dnp=True)` is stored
        // as a module property by the loader.
        let is_dnp = ["dnp"].iter().any(|&key| {
            module
                .properties()
                .get(key)
                .map(|val| {
                    // Try to interpret the value as a boolean
                    if let Some(s) = val.to_value().unpack_str() {
                        s.eq_ignore_ascii_case("true") || s == "1"
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
                direction: param.direction,
                value: param.actual_value.and_then(serialize_signature_value),
                default_value: param.default_value.and_then(serialize_signature_value),
                allowed_values: serialize_signature_values(param.allowed_values.as_ref()),
                allowed_display: param.allowed_display(),
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
            let local_name = introduced_net.name.as_str();
            if !local_name.is_empty() {
                let scoped_name = if module_path.is_empty() {
                    local_name.to_string()
                } else {
                    format!("{module_path}.{local_name}")
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
            }

            let info = self.net_info_mut(*net_id);
            info.kind.get_or_insert_with(|| introduced_net.kind.clone());
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
        net_info
            .kind
            .get_or_insert_with(|| net.net_kind_name().to_string());

        // For unnamed NotConnected nets, use a stable port-derived name when possible.
        if net_info.kind.as_deref() == Some("NotConnected")
            && net.original_name_opt().is_none()
            && net_info.ports.len() == 1
        {
            net_info.name = stable_single_port_not_connected_scoped_name(instance_ref);
        }

        // Honor explicit names on nets encountered during connections unless already set.
        if net_info.name.is_none() {
            let local = net.name();
            if !local.is_empty() {
                let module_len = instance_ref.instance_path.len().saturating_sub(2);
                let module_segments = &instance_ref.instance_path[..module_len];
                net_info.name = Some(if module_segments.is_empty() {
                    local.to_string()
                } else {
                    format!("{}.{local}", module_segments.join("."))
                });
            }
        }

        // Convert regular properties to AttributeValue if not already present.
        for (key, value) in net.properties().iter() {
            if !net_info.properties.contains_key(key)
                && let Ok(attr_value) = to_attribute_value(*value)
            {
                net_info.properties.insert(key.clone(), attr_value);
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

        if component.part().is_none()
            && let Some(mpn) = component.bom_mpn()
        {
            comp_inst.add_attribute(
                crate::attrs::BOM_MPN,
                AttributeValue::String(mpn.to_owned()),
            );
        }

        if let Some(part) = component.part() {
            comp_inst.add_attribute(
                crate::attrs::PART,
                AttributeValue::Json(part.to_json_value()),
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
            // Preserve typed part metadata emitted from `Component(part=...)`.
            // Legacy `properties["part"]` must not overwrite the structured JSON payload.
            if key == crate::attrs::PART {
                continue;
            }
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
        if !symbol_value.is_none()
            && let Some(symbol) = symbol_value.downcast_ref::<SymbolValue>()
        {
            comp_inst.internal_connectivity = symbol.internal_connectivity().clone();

            // Add symbol_name for backwards compatibility
            if let Some(name) = symbol.name() {
                comp_inst.add_attribute(
                    crate::attrs::SYMBOL_NAME.to_string(),
                    AttributeValue::String(name.to_string()),
                );
            }

            if let Some(path) = symbol.source_uri() {
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
                    mirror: pos
                        .mirror
                        .as_deref()
                        .and_then(MirrorAxis::from_comment_value),
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

        if let Some(symbol_key) =
            self.find_descendant_override_net_symbol_key(net_part, suffix, instance_ref)
        {
            return Some(symbol_key);
        }

        // First try: public io() nets from signature - these need net ID lookup to get actual name.
        // Walks the parameter's default value (Net or Interface, possibly nested) looking for a
        // net whose name matches `net_part`, then resolves the same field path on the actual
        // value to get the bound net's id.
        for param in module.signature().iter().filter(|p| !p.is_config) {
            let Some(default_value) = param.default_value else {
                continue;
            };
            let Some(field_path) = find_net_field_path(default_value, net_part) else {
                continue;
            };
            let Some(actual_value) = param.actual_value else {
                continue;
            };
            let Some(net_id) = resolve_net_id_along_path(actual_value, &field_path) else {
                continue;
            };
            if let Some(actual_net_name) = self
                .net_to_info
                .get(&net_id)
                .and_then(|info| info.name.clone())
            {
                return Some(format!("sym:{}#{}", actual_net_name, suffix));
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

    fn find_descendant_override_net_symbol_key(
        &self,
        net_part: &str,
        suffix: &str,
        instance_ref: &InstanceRef,
    ) -> Option<String> {
        let segments: Vec<&str> = net_part.split('.').collect();
        if segments.len() < 2 {
            return None;
        }

        for split_idx in 1..segments.len() {
            // The prefix must resolve to a descendant module instance.
            segments[..split_idx]
                .iter()
                .try_fold(instance_ref, |current_ref, part| {
                    let child_ref = self
                        .schematic
                        .instances
                        .get(current_ref)?
                        .children
                        .get(*part)?;
                    let child_instance = self.schematic.instances.get(child_ref)?;
                    (child_instance.kind == InstanceKind::Module).then_some(child_ref)
                })?;

            // The remainder must name an actual net. Override keys are
            // `<module path>.<global net name>.<index>` because converted net-symbol
            // keys use global net names (e.g. a descendant's local `IN_GD.0` comment
            // converts to `sym:VBUS_RAW#0` when the parent connects `IN_GD=VBUS_RAW`).
            let rest = segments[split_idx..].join(".");
            if self
                .net_to_info
                .values()
                .any(|info| info.name.as_deref() == Some(rest.as_str()))
            {
                return Some(format!("sym:{}#{}", net_part, suffix));
            }
        }

        None
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
                let mut push_moved_warning =
                    |body: String, kind: &str, span: Option<ResolvedSpan>| {
                        diagnostics.push(
                            Diagnostic::categorized(
                                &source.to_string_lossy(),
                                &body,
                                kind,
                                EvalSeverity::Warning,
                            )
                            .with_span(span),
                        );
                    };

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
                    push_moved_warning(body, "moved.invalid_depth", span);
                    continue;
                }

                if existing.contains(&old_scoped) {
                    let span = find_moved_span(module.source_path(), old, new, false);
                    let body = format!("moved() references path '{}' that still exists.", old);
                    push_moved_warning(body, "moved.old_path_exists", span);
                } else if !existing.contains(&new_scoped) {
                    let span = find_moved_span(module.source_path(), old, new, true);
                    let body = format!("moved() references path '{}' that doesn't exist.", new);
                    push_moved_warning(body, "moved.new_path_missing", span);
                } else {
                    filtered.insert(old_scoped, new_scoped.clone());
                }
            }
        }

        (diagnostics, filtered)
    }
}

fn stable_single_port_not_connected_scoped_name(port: &InstanceRef) -> Option<String> {
    if port.instance_path.is_empty() {
        return None;
    }

    let path = &port.instance_path;
    let (module_prefix, local_name) = if path.len() >= 2 {
        let module_prefix = path[..path.len() - 2].join(".");
        let comp = sanitize_nc_fragment(&path[path.len() - 2]);
        let pin = sanitize_nc_fragment(&path[path.len() - 1]);
        (module_prefix, format!("NC_{comp}_{pin}"))
    } else {
        let leaf = sanitize_nc_fragment(path.last().unwrap());
        (String::new(), format!("NC_{leaf}"))
    };

    Some(if module_prefix.is_empty() {
        local_name
    } else {
        format!("{module_prefix}.{local_name}")
    })
}

fn sanitize_nc_fragment(s: &str) -> String {
    // Be conservative: preserve most ASCII, but eliminate characters that break identifier
    // semantics for our own hierarchical scoping ('.') or internal suffixing ('@').
    s.chars()
        .map(|c| {
            if !c.is_ascii() || c.is_whitespace() || c == '.' || c == '@' {
                '_'
            } else {
                c
            }
        })
        .collect()
}

fn collect_net_ids_from_value(value: Value) -> HashSet<NetId> {
    let mut net_ids = HashSet::new();
    collect_net_ids_into(value, &mut net_ids);
    net_ids
}

fn collect_net_ids_into(value: Value, net_ids: &mut HashSet<NetId>) {
    if let Some(net) = value.downcast_ref::<FrozenNetValue>() {
        net_ids.insert(net.id());
        return;
    }

    if let Some(interface) = value.downcast_ref::<FrozenInterfaceValue>() {
        for field in interface.fields().values() {
            collect_net_ids_into(field.to_value(), net_ids);
        }
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
    ) && let Ok(attr) = to_attribute_value(*impedance_val)
    {
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
    if let Some(s) = v.to_value().unpack_str() {
        return Ok(AttributeValue::String(s.to_string()));
    } else if let Some(n) = v.unpack_i32() {
        return Ok(AttributeValue::Number(n as f64));
    } else if let Some(b) = v.unpack_bool() {
        return Ok(AttributeValue::Boolean(b));
    } else if let Some(&physical) = v.downcast_ref::<PhysicalValue>() {
        return Ok(AttributeValue::String(physical.to_string()));
    } else if let Some(enum_val) = v.downcast_ref::<EnumValue>() {
        return Ok(AttributeValue::String(enum_val.value().to_string()));
    } else if let Some(part) = v.downcast_ref::<PartValue>() {
        return Ok(AttributeValue::Json(part.to_json_value()));
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
            } else if let Some(part) = item.downcast_ref::<PartValue>() {
                AttributeValue::Json(part.to_json_value())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn inst_ref(path: &[&str]) -> InstanceRef {
        InstanceRef {
            module: ModuleRef::new("/test.zen", "<root>"),
            instance_path: path.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    #[test]
    fn stable_single_port_not_connected_scoped_name_root_two_segments() {
        assert_eq!(
            stable_single_port_not_connected_scoped_name(&inst_ref(&["R1", "P2"])).as_deref(),
            Some("NC_R1_P2")
        );
    }

    #[test]
    fn stable_single_port_not_connected_scoped_name_with_module_prefix() {
        assert_eq!(
            stable_single_port_not_connected_scoped_name(&inst_ref(&["Power", "DcDc", "U1", "SW"]))
                .as_deref(),
            Some("Power.DcDc.NC_U1_SW")
        );
    }

    #[test]
    fn stable_single_port_not_connected_scoped_name_one_segment() {
        assert_eq!(
            stable_single_port_not_connected_scoped_name(&inst_ref(&["SW"])).as_deref(),
            Some("NC_SW")
        );
    }

    #[test]
    fn stable_single_port_not_connected_scoped_name_empty_path_returns_none() {
        assert_eq!(
            stable_single_port_not_connected_scoped_name(&inst_ref(&[])),
            None
        );
    }

    #[test]
    fn stable_single_port_not_connected_scoped_name_sanitizes_fragments() {
        assert_eq!(
            stable_single_port_not_connected_scoped_name(&inst_ref(&[
                "Top",
                "U1@A.B",
                "PF0 OSC_IN"
            ]))
            .as_deref(),
            Some("Top.NC_U1_A_B_PF0_OSC_IN")
        );
    }
}
