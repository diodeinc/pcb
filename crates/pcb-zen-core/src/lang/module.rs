#![allow(clippy::needless_lifetimes)]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use crate::lang::component::FrozenComponentValue;
use crate::lang::electrical_check::FrozenElectricalCheck;
use crate::lang::r#enum::{EnumType, EnumValue};
use crate::lang::test_bench::FrozenTestBenchValue;
use allocative::Allocative;
use serde::Serialize;
use starlark::environment::FrozenModule;
use starlark::values::record::{FrozenRecordType, RecordType};
use starlark::values::typing::{TypeCompiled, TypeType};
use starlark::values::{Heap, UnpackValue, ValueLifetimeless};
use starlark::{
    any::ProvidesStaticType,
    collections::SmallMap,
    environment::GlobalsBuilder,
    eval::{Arguments, Evaluator},
    starlark_complex_value, starlark_module, starlark_simple_value,
    values::{
        float::StarlarkFloat, list::ListRef, starlark_value, Coerce, Freeze, NoSerialize,
        StarlarkValue, Trace, Value, ValueLike,
    },
};

use crate::graph::starlark::ModuleGraphValueGen;
use crate::lang::context::{ContextValue, PendingChild};
use crate::lang::evaluator_ext::EvaluatorExt;
use crate::lang::interface::{
    FrozenInterfaceFactory, FrozenInterfaceValue, InterfaceFactory, InterfaceValue,
};
use crate::lang::validation::validate_identifier_name;
use regex::Regex;
use starlark::codemap::{CodeMap, Pos, Span};
use starlark::values::dict::{AllocDict, DictRef};

use starlark::values::record::{FrozenRecord, Record};
use std::fs;

/// Helper macro for frozen module downcasting to reduce repetition
#[macro_export]
macro_rules! downcast_frozen_module {
    ($module:expr) => {
        $module
            .to_value()
            .downcast_ref::<FrozenModuleValue>()
            .ok_or_else(|| {
                starlark::Error::new_other(anyhow::anyhow!(
                    "Module methods only work on frozen modules"
                ))
            })?
    };
}

use super::net::{generate_net_id, FrozenNetType, FrozenNetValue, NetId, NetType, NetValue};
use crate::lang::context::FrozenContextValue;
use starlark::errors::EvalMessage;

/// Info about a net introduced by this module
#[derive(Clone, Debug, Trace, Allocative, Freeze)]
pub struct IntroducedNet {
    pub final_name: String,
    /// Original name before deduplication (only set if collision occurred)
    pub original_name: Option<String>,
    /// Call stack at the time the net was registered (for diagnostic context)
    #[freeze(identity)]
    #[allocative(skip)]
    pub call_stack: starlark::eval::CallStack,
}

#[derive(Clone, PartialEq, Eq, Hash, Allocative, Freeze, Serialize)]
pub struct ModulePath {
    pub segments: Vec<String>,
}

impl ModulePath {
    pub fn name(&self) -> String {
        self.segments
            .last()
            .cloned()
            .unwrap_or("<root>".to_string())
    }

    pub fn root() -> Self {
        ModulePath { segments: vec![] }
    }

    pub fn is_root(&self) -> bool {
        self.segments.is_empty()
    }

    pub fn push<S: Into<String>>(&mut self, segment: S) {
        self.segments.push(segment.into());
    }

    pub fn parent(&self) -> Option<ModulePath> {
        if self.segments.is_empty() {
            None
        } else {
            Some(ModulePath {
                segments: self.segments[..self.segments.len() - 1].to_vec(),
            })
        }
    }

    /// Check if this path starts with the given base path
    pub fn starts_with(&self, base: &ModulePath) -> bool {
        self.segments.starts_with(&base.segments)
    }

    /// Strip the base path prefix, returning the relative path
    pub fn strip_prefix(&self, base: &ModulePath) -> Option<ModulePath> {
        if self.starts_with(base) {
            Some(ModulePath {
                segments: self.segments[base.segments.len()..].to_vec(),
            })
        } else {
            None
        }
    }

    /// Convert to a relative string path (relative to base)
    pub fn to_rel_string(&self, base: &ModulePath) -> Option<String> {
        self.strip_prefix(base).map(|p| p.segments.join("."))
    }
}

impl std::fmt::Display for ModulePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.segments.join("."))
    }
}

impl std::fmt::Debug for ModulePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.segments.is_empty() {
            f.write_str("<root>")
        } else {
            write!(f, "{}", self.segments.join("."))
        }
    }
}

impl From<&str> for ModulePath {
    fn from(s: &str) -> Self {
        ModulePath {
            segments: s.split('.').map(|s| s.to_string()).collect(),
        }
    }
}

impl From<String> for ModulePath {
    fn from(s: String) -> Self {
        ModulePath::from(s.as_str())
    }
}

impl PartialOrd for ModulePath {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ModulePath {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.segments.cmp(&other.segments)
    }
}

/// Position data from pcb:sch comments  
#[derive(Clone, Debug, ProvidesStaticType, NoSerialize, Allocative)]
pub struct Position {
    pub x: f64,
    pub y: f64,
    pub rotation: f64,
}

impl std::fmt::Display for Position {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Position({:.1}, {:.1}, {:.0})",
            self.x, self.y, self.rotation
        )
    }
}

impl starlark::values::Freeze for Position {
    type Frozen = Position;
    fn freeze(
        self,
        _: &starlark::values::Freezer,
    ) -> Result<Self::Frozen, starlark::values::FreezeError> {
        Ok(self)
    }
}

starlark_simple_value!(Position);
#[starlark_value(type = "Position")]
impl<'v> StarlarkValue<'v> for Position {}

pub type PositionMap = SmallMap<String, Position>;

/// Parse position data from pcb:sch comments in file content  
pub fn parse_positions(content: &str) -> PositionMap {
    pcb_sch::position::parse_position_comments(content)
        .0
        .into_iter()
        .map(|(k, v)| {
            (
                k.to_string(),
                Position {
                    x: v.x,
                    y: v.y,
                    rotation: v.rotation,
                },
            )
        })
        .collect()
}
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Error)]
#[error("Input '{name}' is required but was not provided and no default value was given")]
pub struct MissingInputError {
    name: String,
}

impl From<MissingInputError> for starlark::Error {
    fn from(err: MissingInputError) -> Self {
        starlark::Error::new_other(err)
    }
}

/// Metadata for a module parameter (from io() or config() calls)
#[derive(Clone, Debug, Trace, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub struct ParameterMetadataGen<V: ValueLifetimeless> {
    /// Parameter name
    pub name: String,
    /// Type value (e.g., Net, str, int, etc.)
    pub type_value: V,
    /// Whether the parameter is optional
    pub optional: bool,
    /// Default value if provided
    pub default_value: Option<V>,
    /// Whether this is a config parameter (vs io parameter)
    pub is_config: bool,
    /// Help text describing the parameter
    pub help: Option<String>,
    /// The actual value returned by io() or config()
    pub actual_value: Option<V>,
}

// Manual because no instance for Option<V>
unsafe impl<From: Coerce<To> + ValueLifetimeless, To: ValueLifetimeless>
    Coerce<ParameterMetadataGen<To>> for ParameterMetadataGen<From>
{
}

starlark_complex_value!(pub ParameterMetadata);

#[starlark_value(type = "ParameterMetadata")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for ParameterMetadataGen<V> where
    Self: ProvidesStaticType<'v>
{
}

impl<'v, V: ValueLike<'v>> std::fmt::Display for ParameterMetadataGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ParameterMetadata({})", self.name)
    }
}

impl<'v, V: ValueLike<'v>> ParameterMetadataGen<V> {
    pub fn new(
        name: String,
        type_value: V,
        optional: bool,
        default_value: Option<V>,
        is_config: bool,
        help: Option<String>,
    ) -> Self {
        Self {
            name,
            type_value,
            optional,
            default_value,
            is_config,
            help,
            actual_value: None,
        }
    }
}

#[derive(Clone, Coerce, Trace, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub struct ModuleValueGen<V: ValueLifetimeless> {
    path: ModulePath,
    source_path: String,
    inputs: SmallMap<String, V>,
    properties: SmallMap<String, V>,
    signature: Vec<ParameterMetadataGen<V>>,
    /// Nets that are introduced (created) by this module. Map of `net id → net info`.
    introduced_nets: SmallMap<NetId, IntroducedNet>,
    /// Local name → net id, to enforce uniqueness of names within a module.
    net_name_to_id: SmallMap<String, NetId>,
    /// Parsed position data from pcb:sch comments in this module's source file
    positions: PositionMap,
    /// Path movement directives from moved() calls. Map of `old path → (new path, auto_generated)`.
    moved_directives: SmallMap<String, (String, bool)>,
    /// Local values (components, electrical checks, testbenches). Child modules are in module_tree.
    children: Vec<V>,
    /// Component modifier functions registered via builtin.add_component_modifier().
    /// These are called in order on every component created in THIS module only (not children).
    /// Applied BEFORE parent modifiers to maintain bottom-up execution order:
    /// child's own → parent's → grandparent's, etc.
    component_modifiers: Vec<V>,
    /// Component modifier functions inherited from parent and ancestor modules.
    /// Collected during module instantiation by combining parent.component_modifiers() +
    /// parent.parent_component_modifiers(), creating the full ancestor chain.
    /// Applied AFTER the module's own modifiers.
    parent_component_modifiers: Vec<V>,
}

starlark_complex_value!(pub ModuleValue);

#[starlark_value(type = "Module")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for ModuleValueGen<V>
where
    Self: ProvidesStaticType<'v>,
{
    fn get_attr(&self, attr: &str, heap: &'v Heap) -> Option<Value<'v>> {
        let module_value = heap.alloc_complex(self.clone()).to_value();

        match attr {
            "nets" => {
                let callable = NetsCallableGen {
                    module: module_value,
                };
                Some(heap.alloc_complex(callable))
            }
            "components" => {
                let callable = ComponentsCallableGen {
                    module: module_value,
                };
                Some(heap.alloc_complex(callable))
            }
            "graph" => {
                let callable = GraphCallableGen {
                    module: module_value,
                };
                Some(heap.alloc_complex(callable))
            }
            _ => None,
        }
    }

    fn has_attr(&self, attr: &str, _heap: &'v Heap) -> bool {
        matches!(attr, "nets" | "components" | "graph")
    }

    fn dir_attr(&self) -> Vec<String> {
        vec![
            "nets".to_string(),
            "components".to_string(),
            "graph".to_string(),
        ]
    }
}

impl<'v, V: ValueLike<'v>> std::fmt::Display for ModuleValueGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Module({})", self.path)
    }
}

impl<'v, V: ValueLike<'v>> std::fmt::Debug for ModuleValueGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = f.debug_struct("Module");
        debug.field("path", &self.path);
        debug.field("source", &self.source_path);

        // Hide inputs from Debug output (internal implementation detail)
        // Inputs are copied values from parent - showing them creates snapshot noise

        // Sort properties for deterministic output
        if !self.properties.is_empty() {
            let mut props: Vec<_> = self.properties.iter().collect();
            props.sort_by_key(|(k, _)| k.as_str());
            let props_map: BTreeMap<_, _> = props
                .into_iter()
                .map(|(k, v)| (k.as_str(), format!("{v:?}")))
                .collect();
            debug.field("properties", &props_map);
        }

        // Print children (components, electrical checks, testbenches)
        if !self.children.is_empty() {
            debug.field("children", &self.children);
        }

        debug.finish()
    }
}

impl<'v, V: ValueLike<'v>> ModuleValueGen<V> {
    pub(crate) fn add_property(&mut self, name: String, value: V) {
        self.properties.insert(name, value);
    }

    pub(crate) fn add_input(&mut self, name: String, value: V) {
        self.inputs.insert(name, value);
    }

    pub fn new(path: ModulePath, source_path: &Path, positions: PositionMap) -> Self {
        let source_path = source_path.to_string_lossy().into_owned();
        ModuleValueGen {
            path,
            source_path,
            inputs: SmallMap::new(),
            properties: SmallMap::new(),
            signature: Vec::new(),
            introduced_nets: SmallMap::new(),
            net_name_to_id: SmallMap::new(),
            positions,
            moved_directives: SmallMap::new(),
            children: Vec::new(),
            component_modifiers: Vec::new(),
            parent_component_modifiers: Vec::new(),
        }
    }

    pub fn source_path(&self) -> &str {
        &self.source_path
    }

    pub fn positions(&self) -> &PositionMap {
        &self.positions
    }

    pub fn path(&self) -> &ModulePath {
        &self.path
    }

    pub fn inputs(&self) -> &SmallMap<String, V> {
        &self.inputs
    }

    /// Return a reference to the custom property map attached to this Module.
    pub fn properties(&self) -> &SmallMap<String, V> {
        &self.properties
    }

    /// Get the component modifiers registered for this module.
    pub fn component_modifiers(&self) -> &Vec<V> {
        &self.component_modifiers
    }

    /// Add a component modifier function to this module.
    pub fn add_component_modifier(&mut self, modifier_fn: V) {
        self.component_modifiers.push(modifier_fn);
    }

    /// Get the component modifiers inherited from parent modules.
    pub fn parent_component_modifiers(&self) -> &Vec<V> {
        &self.parent_component_modifiers
    }

    /// Set the component modifiers inherited from parent modules.
    pub fn set_parent_component_modifiers(&mut self, modifiers: Vec<V>) {
        self.parent_component_modifiers = modifiers;
    }

    /// Collect all component modifiers (own + inherited) as unfrozen Values.
    /// This is used when passing modifiers to child modules via PendingChild.
    pub fn collect_all_component_modifiers_as_values(&self) -> Vec<Value<'v>> {
        let mut result = Vec::new();
        for modifier in self.component_modifiers.iter() {
            result.push(modifier.to_value());
        }
        for modifier in self.parent_component_modifiers.iter() {
            result.push(modifier.to_value());
        }
        result
    }

    /// Add a parameter to the module's signature with full metadata.
    #[allow(clippy::too_many_arguments)]
    pub fn add_parameter_metadata(
        &mut self,
        name: String,
        type_value: V,
        optional: bool,
        default_value: Option<V>,
        is_config: bool,
        help: Option<String>,
        actual_value: Option<V>,
    ) {
        // Check if this parameter already exists
        if !self.signature.iter().any(|p| p.name == name) {
            let mut param = ParameterMetadataGen::new(
                name,
                type_value,
                optional,
                default_value,
                is_config,
                help,
            );
            param.actual_value = actual_value;
            self.signature.push(param);
        }
    }

    /// Get the module's signature.
    pub fn signature(&self) -> &Vec<ParameterMetadataGen<V>> {
        &self.signature
    }

    /// Add a child value (component, electrical check, or testbench) to this module
    pub fn add_child(&mut self, child: V) {
        self.children.push(child);
    }

    /// Get all children (components, electrical checks, testbenches) in this module
    pub fn children(&self) -> &Vec<V> {
        &self.children
    }

    /// Get all electrical checks registered in this module (requires downcasting)
    pub fn electrical_checks<'a>(&'a self) -> impl Iterator<Item = &'a FrozenElectricalCheck> + 'a
    where
        V: 'a,
        'v: 'a,
    {
        self.children
            .iter()
            .filter_map(move |child| child.downcast_ref::<FrozenElectricalCheck>())
    }

    /// Get all components created in this module (requires downcasting)
    pub fn components<'a>(&'a self) -> impl Iterator<Item = &'a FrozenComponentValue> + 'a
    where
        V: 'a,
        'v: 'a,
    {
        self.children
            .iter()
            .filter_map(move |child| child.downcast_ref::<FrozenComponentValue>())
    }

    /// Get all testbenches created in this module (requires downcasting)
    pub fn testbenches<'a>(&'a self) -> impl Iterator<Item = &'a FrozenTestBenchValue> + 'a
    where
        V: 'a,
        'v: 'a,
    {
        self.children
            .iter()
            .filter_map(move |child| child.downcast_ref::<FrozenTestBenchValue>())
    }

    /// Record that this module introduced a net with `id` and `local_name`.
    /// If another net with the same local name already exists in this module,
    /// generate a unique variant by appending a numeric suffix (e.g. `_2`, `_3`, ...).
    pub fn register_net(
        &mut self,
        id: NetId,
        local_name: String,
        call_stack: starlark::eval::CallStack,
    ) -> anyhow::Result<String> {
        // If this id was already registered, keep the first assignment (idempotent)
        if let Some(info) = self.introduced_nets.get(&id) {
            return Ok(info.final_name.clone());
        }

        // If the provided name is empty/whitespace, fall back to a stable placeholder.
        let base_name = if local_name.trim().is_empty() {
            format!("N{id}")
        } else {
            local_name
        };

        // Choose a unique name within this module.
        let unique_name = if let Some(existing_id) = self.net_name_to_id.get(&base_name) {
            if *existing_id == id {
                base_name.clone()
            } else {
                // Find the next available suffix
                let mut counter: u32 = 2;
                let mut candidate = format!("{base_name}_{counter}");
                while let Some(other_id) = self.net_name_to_id.get(&candidate) {
                    if *other_id == id {
                        break;
                    }
                    counter += 1;
                    candidate = format!("{base_name}_{counter}");
                }
                candidate
            }
        } else {
            base_name.clone()
        };

        // Track whether a collision occurred (name was changed)
        let had_collision = unique_name != base_name;

        self.net_name_to_id.insert(unique_name.clone(), id);
        self.introduced_nets.insert(
            id,
            IntroducedNet {
                final_name: unique_name.clone(),
                original_name: if had_collision { Some(base_name) } else { None },
                call_stack,
            },
        );
        Ok(unique_name)
    }

    /// Return the map of nets introduced by this module.
    pub fn introduced_nets(&self) -> &starlark::collections::SmallMap<NetId, IntroducedNet> {
        &self.introduced_nets
    }

    /// Add a moved directive to this module.
    pub fn add_moved_directive(
        &mut self,
        old_path: String,
        new_path: String,
        auto_generated: bool,
    ) {
        self.moved_directives
            .insert(old_path, (new_path, auto_generated));
    }

    /// Return the map of moved directives for this module.
    pub fn moved_directives(&self) -> &starlark::collections::SmallMap<String, (String, bool)> {
        &self.moved_directives
    }

    /// Extract all net names from a value recursively.
    /// This handles Net types directly and recursively extracts nets from Interface types.
    pub fn extract_nets_from_value(value: starlark::values::Value<'_>) -> HashSet<String> {
        use crate::lang::interface::{FrozenInterfaceValue, InterfaceValue};
        use crate::lang::net::{FrozenNetValue, NetValue};

        let mut nets = HashSet::new();

        // Check if it's a Net
        if let Some(net) = value.downcast_ref::<NetValue>() {
            nets.insert(net.name().to_string());
        } else if let Some(net) = value.downcast_ref::<FrozenNetValue>() {
            nets.insert(net.name().to_string());
        }
        // Check if it's an Interface
        else if let Some(iface) = value.downcast_ref::<InterfaceValue>() {
            // Recursively extract nets from all interface fields
            for (_field_name, field_value) in iface.fields().iter() {
                nets.extend(Self::extract_nets_from_value(*field_value));
            }
        } else if let Some(iface) = value.downcast_ref::<FrozenInterfaceValue>() {
            // Recursively extract nets from all interface fields
            for (_field_name, field_value) in iface.fields().iter() {
                nets.extend(Self::extract_nets_from_value(field_value.to_value()));
            }
        }

        nets
    }

    /// Remove a previously registered net from this module. Intended for
    /// cases where a `Net()` value was used as a template (e.g., inside
    /// `interface(...)`) and should not count as an introduced net for the
    /// enclosing module.
    pub fn unregister_net(&mut self, id: NetId) {
        // Find the name associated with this id (if any)
        let mut name_to_remove: Option<String> = None;
        for (nid, info) in self.introduced_nets.iter() {
            if *nid == id {
                name_to_remove = Some(info.final_name.clone());
                break;
            }
        }

        if let Some(name) = name_to_remove {
            // Rebuild introduced_nets without the given id
            let mut rebuilt_nets = starlark::collections::SmallMap::new();
            for (nid, info) in self.introduced_nets.iter() {
                if *nid != id {
                    rebuilt_nets.insert(*nid, info.clone());
                }
            }
            self.introduced_nets = rebuilt_nets;

            // Rebuild net_name_to_id without the given name
            let mut rebuilt_lookup = starlark::collections::SmallMap::new();
            for (k, v) in self.net_name_to_id.iter() {
                if k != &name {
                    rebuilt_lookup.insert(k.clone(), *v);
                }
            }
            self.net_name_to_id = rebuilt_lookup;
        }
    }
}

#[derive(Clone, Debug, Trace, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
pub struct ModuleLoader {
    pub name: String,
    pub source_path: String,
    /// List of placeholder names (io()/config()) declared by the module.  Populated lazily
    /// when the loader is constructed by evaluating the target file once with an empty
    /// input map so that signature help can surface them later without re-parsing.
    pub params: Vec<String>,

    /// Map of parameter names to their type information (e.g., "param_name" -> "Net")
    /// Extracted from diagnostics during the introspection pass.
    pub param_types: SmallMap<String, String>,

    #[freeze(identity)]
    pub frozen_module: Option<FrozenModule>,
}
starlark_simple_value!(ModuleLoader);

impl std::fmt::Display for ModuleLoader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<ModuleLoader {}>", self.name)
    }
}

#[starlark_value(type = "ModuleLoader")]
impl<'v> StarlarkValue<'v> for ModuleLoader
where
    Self: ProvidesStaticType<'v>,
{
    fn invoke(
        &self,
        _me: Value<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        let heap = eval.heap();
        // Only allow named arguments
        let positions_iter = args.positions(heap)?;
        if positions_iter.count() > 0 {
            return Err(starlark::Error::new_other(anyhow::anyhow!(
                "ModuleLoader only supports named arguments"
            )));
        }

        // Collect parent values temporarily
        let mut parent_values: SmallMap<String, Value<'v>> = SmallMap::new();
        let mut provided_names: HashSet<String> = HashSet::new();
        let mut override_name: Option<String> = None;
        // Optional map of properties passed via `properties = {...}`.
        let mut properties_override: Option<SmallMap<String, Value<'v>>> = None;

        for (arg_name, value) in args.names_map()? {
            if arg_name.as_str() == "name" {
                // Ensure `name` is a string.
                let name_str = value
                    .unpack_str()
                    .ok_or_else(|| {
                        starlark::Error::new_other(anyhow::anyhow!(
                            "name parameter must be a string"
                        ))
                    })?
                    .to_string();

                // Validate the module name
                validate_identifier_name(&name_str, "Module name")?;

                override_name = Some(name_str);
                // Do *not* add `name` to the input map.
                continue;
            }

            if arg_name.as_str() == "properties" {
                // Expect a dict {str: any}
                let dict = DictRef::from_value(value).ok_or_else(|| {
                    starlark::Error::new_other(anyhow::anyhow!(
                        "properties parameter must be a dict"
                    ))
                })?;

                let mut map = SmallMap::new();
                for (k, v) in dict.iter() {
                    let key_str = k.unpack_str().ok_or_else(|| {
                        starlark::Error::new_other(anyhow::anyhow!("property keys must be strings"))
                    })?;
                    map.insert(key_str.to_string(), v);
                }

                properties_override = Some(map);
                // Do *not* treat `properties` as an input placeholder.
                continue;
            }

            if arg_name.as_str() == "dnp" {
                properties_override
                    .get_or_insert_with(SmallMap::new)
                    .insert("dnp".to_string(), value.to_value());
                continue;
            }

            provided_names.insert(arg_name.as_str().to_string());
            // Store parent value temporarily (will copy to child heap before eval)
            parent_values.insert(arg_name.as_str().to_string(), value.to_value());
        }
        // `name` is required when instantiating a module via its loader.  If the
        // caller omitted it, emit a *soft* diagnostic (non-fatal) and fall back
        // to the loaderʼs default name so evaluation can continue.
        let final_name = if let Some(n) = override_name {
            n
        } else {
            if let Some(call_site) = eval.call_stack_top_location() {
                let msg = format!(
                    "Missing required argument `name` when instantiating module {}",
                    self.name
                );
                let mut diag = EvalMessage::from_any_error(Path::new(call_site.filename()), &msg);
                diag.span = Some(call_site.resolve_span());
                eval.add_diagnostic(diag);
            } else {
                let msg = format!(
                    "Missing required argument `name` when instantiating module {}",
                    self.name
                );
                eval.add_diagnostic(EvalMessage::from_any_error(
                    Path::new(&self.source_path),
                    &msg,
                ));
            }

            // Use the file-stem derived name from the loader as a fallback.
            self.name.clone()
        };

        let context = eval
            .module()
            .extra_value()
            .and_then(|extra| extra.downcast_ref::<ContextValue>())
            .ok_or_else(|| {
                starlark::Error::new_other(anyhow::anyhow!(
                    "unexpected context - ContextValue not found",
                ))
            })?;

        let call_site = eval
            .call_stack_top_location()
            .expect("Module instantiation requires a call site");

        let call_site_path = call_site.filename().to_string();
        let call_site_span = call_site.resolve_span();
        let call_stack = eval.call_stack().clone();

        let provided_names: Vec<String> = provided_names.into_iter().collect();

        // Inherit dnp from parent module (parent dnp=True dominates child)
        let parent_module = context.module();
        let parent_dnp = parent_module
            .properties()
            .get("dnp")
            .and_then(|v| v.unpack_bool())
            .unwrap_or(false);

        let child_dnp = properties_override
            .as_ref()
            .and_then(|m| m.get("dnp"))
            .and_then(|v| v.unpack_bool())
            .unwrap_or(false);

        if parent_dnp || child_dnp {
            properties_override
                .get_or_insert_with(SmallMap::new)
                .insert("dnp".to_string(), eval.heap().alloc(true).to_value());
        }

        // Collect parent modifiers (parent's own + parent's ancestors)
        let combined_modifiers = parent_module.collect_all_component_modifiers_as_values();
        drop(parent_module);

        context.enqueue_child(PendingChild {
            loader: self.clone(),
            final_name,
            inputs: parent_values,
            properties: properties_override,
            component_modifiers: combined_modifiers,
            provided_names,
            call_site_path,
            call_site_span,
            call_stack,
        });

        // Return `None` – in line with other factory functions like Component.
        Ok(Value::new_none())
    }

    fn eval_type(&self) -> Option<starlark::typing::Ty> {
        Some(<ModuleLoader as StarlarkValue>::get_type_starlark_repr())
    }

    // Expose exports from the target module as attributes on the loader so users can refer to
    // them via the familiar dot-notation (e.g. `Sub.Component`).  We lazily evaluate the target
    // file with an *empty* input map – mirroring the lightweight introspection pass in
    // `Module()` – and then deep-copy the requested symbol into the current heap so that it
    // lives alongside the callerʼs values.
    fn get_attr(&self, attr: &str, _heap: &'v Heap) -> Option<Value<'v>> {
        // Fast-path: ignore private/internal names.
        if attr.starts_with("__") {
            return None;
        }

        if let Some(frozen_module) = &self.frozen_module {
            return frozen_module.get_option(attr).ok().flatten().map(|owned| {
                // SAFETY: we know the frozen module is alive because we added a reference to it
                let fv = unsafe { owned.unchecked_frozen_value() };
                fv.to_value()
            });
        }

        None
    }
}

// Helper: given a Starlark `typ` value build a sensible default instance of that type.
fn default_for_type<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    typ: Value<'v>,
) -> anyhow::Result<Value<'v>> {
    let heap = eval.heap();

    if let Some(enum_type) = typ.downcast_ref::<EnumType>() {
        if let Ok(first_variant) = enum_type.at(heap.alloc(0), heap) {
            return Ok(first_variant.to_value());
        } else {
            return Err(anyhow::anyhow!(
                "EnumType provided to config/io() has no variants"
            ));
        }
    }

    // Our EnumType is a simple value (no separate Frozen version)
    // It's already handled above, so this block is no longer needed

    if typ.downcast_ref::<RecordType>().is_some()
        || typ.downcast_ref::<FrozenRecordType>().is_some()
    {
        return Err(anyhow::anyhow!(
            "Record dependencies require a default value"
        ));
    }

    // Check if it's a TypeType (like str, int, float constructors)
    if TypeType::unpack_value_opt(typ).is_some() {
        // Use the string representation to determine the type
        let type_str = typ.to_string();
        match type_str.as_str() {
            "str" => return Ok(heap.alloc("").to_value()),
            "int" => return Ok(heap.alloc(0i32).to_value()),
            "float" => return Ok(heap.alloc(StarlarkFloat(0.0)).to_value()),
            _ => {
                // Fall through to try calling it as a constructor
            }
        }
    }

    // Try to call it as a constructor with no arguments
    if typ
        .check_callable_with([], [], None, None, &starlark::typing::Ty::any())
        .is_ok()
    {
        return typ
            .invoke(&starlark::eval::Arguments::default(), eval)
            .map_err(|e| anyhow::anyhow!(e.to_string()));
    }

    // Handle special types by their runtime type
    let default = match typ.get_type() {
        "NetType" => heap
            .alloc(NetValue::new(
                generate_net_id(),
                String::new(),
                SmallMap::new(),
            ))
            .to_value(),
        "InterfaceFactory" => typ
            .invoke(&starlark::eval::Arguments::default(), eval)
            .map_err(|e| anyhow::anyhow!(e.to_string()))?,
        other => {
            return Err(anyhow::anyhow!(
                "config/io() only accepts Net, Interface, Enum, Record, str, int, or float types, got {other}"
            ));
        }
    };
    Ok(default)
}

pub(crate) fn find_moved_span(
    source_path: &str,
    target_old_path: &str,
    target_new_path: &str,
    highlight_new_path: bool,
) -> Option<starlark::codemap::ResolvedSpan> {
    if let Ok(content) = fs::read_to_string(source_path) {
        // Flexible regex to match moved("old", "new") calls across multiple lines
        let re = Regex::new(r#"(?s)moved\s*\(\s*"([^"]+)"\s*,\s*"([^"]+)"\s*\)"#).unwrap();

        for captures in re.captures_iter(&content) {
            let old_path = captures.get(1).unwrap().as_str();
            let new_path = captures.get(2).unwrap().as_str();

            // Check if this is the specific moved() call we're looking for
            if old_path == target_old_path && new_path == target_new_path {
                // Choose which argument to highlight based on the flag
                let target_match = if highlight_new_path {
                    captures.get(2).unwrap() // Second argument (new path)
                } else {
                    captures.get(1).unwrap() // First argument (old path)
                };
                let target_start = target_match.start() - 1; // Include opening quote
                let target_end = target_match.end() + 1; // Include closing quote

                let codemap = CodeMap::new(source_path.to_string(), content);
                let start = Pos::new(target_start as u32);
                let end = Pos::new(target_end as u32);
                let span = Span::new(start, end);
                return Some(codemap.file_span(span).resolve_span());
            }
        }
    }

    None
}

// Helper: validate that `value` matches the requested `typ` value.
fn validate_type<'v>(
    name: &str,
    value: Value<'v>,
    typ: Value<'v>,
    heap: &'v Heap,
) -> anyhow::Result<()> {
    if (typ.downcast_ref::<RecordType>().is_some()
        || typ.downcast_ref::<FrozenRecordType>().is_some())
        && (value.downcast_ref::<Record>().is_some()
            || value.downcast_ref::<FrozenRecord>().is_some())
    {
        return Ok(());
    }

    if typ.downcast_ref::<EnumType>().is_some() && value.downcast_ref::<EnumValue>().is_some() {
        return Ok(());
    }

    // NetType validation with asymmetric conversion rules
    if let Some(expected_type_name) = typ
        .downcast_ref::<NetType>()
        .map(|nt| &nt.type_name)
        .or_else(|| {
            typ.downcast_ref::<FrozenNetType>()
                .map(|fnt| &fnt.type_name)
        })
    {
        let actual_type_name = value
            .downcast_ref::<NetValue>()
            .map(|nv| nv.net_type_name())
            .or_else(|| {
                value
                    .downcast_ref::<FrozenNetValue>()
                    .map(|fnv| fnv.net_type_name())
            });

        if let Some(actual_type_name) = actual_type_name {
            // Only allow exact type match - conversion will be handled by try_net_conversion
            if expected_type_name == actual_type_name {
                return Ok(());
            }

            // Type mismatch - fail validation
            // Note: If expected is "Net" and actual is different (e.g., "Power"),
            // this will fail here and try_net_conversion will handle the conversion
            anyhow::bail!(
                "Input '{name}' has wrong net type: expected {expected_type_name}, got {actual_type_name}"
            );
        }
    }

    // InterfaceFactory validation
    if (typ.downcast_ref::<InterfaceFactory>().is_some()
        || typ.downcast_ref::<FrozenInterfaceFactory>().is_some())
        && (value.downcast_ref::<InterfaceValue>().is_some()
            || value.downcast_ref::<FrozenInterfaceValue>().is_some())
    {
        return Ok(());
    }

    if TypeType::unpack_value_opt(typ).is_some() {
        let tc = TypeCompiled::new(typ, heap)?;
        if tc.matches(value) {
            return Ok(());
        }

        let rendered_value = format!("{value}").replace("FrozenValue(", "Value(");

        anyhow::bail!(
            "Input '{name}' (type) has wrong type for this placeholder: expected {typ}, got {rendered_value}"
        );
    }

    let simple_type = typ.get_type();

    match simple_type {
        "str" | "string" | "String" => {
            if value.unpack_str().is_some() {
                return Ok(());
            }
        }
        "int" | "Int" => {
            if value.unpack_i32().is_some() {
                return Ok(());
            }
        }
        "float" | "Float" => {
            if value.downcast_ref::<StarlarkFloat>().is_some() {
                return Ok(());
            }
        }
        _ => {}
    }

    let rendered_value = format!("{value}").replace("FrozenValue(", "Value(");

    anyhow::bail!(
        "Input '{name}' has wrong type for this placeholder: expected {typ}, got {rendered_value}"
    );
}

// Add helper function to attempt converting a value to an enum variant when
// `typ` is an EnumType / FrozenEnumType and the provided `value` is not yet an
// `EnumValue`.  Returns `Ok(Some(converted))` if the conversion succeeds,
// `Ok(None)` if `typ` is not an enum type, and `Err(..)` if the conversion was
// attempted but failed.
fn try_enum_conversion<'v>(
    value: Value<'v>,
    typ: Value<'v>,
    eval: &mut Evaluator<'v, '_, '_>,
) -> anyhow::Result<Option<Value<'v>>> {
    // Only applicable for EnumType values.
    if typ.downcast_ref::<EnumType>().is_none() {
        return Ok(None);
    }

    // If the value is already an EnumValue, bail early – the caller should have
    // succeeded the type check in that case.
    if value.downcast_ref::<EnumValue>().is_some() {
        return Ok(None);
    }

    // Attempt to call the enum factory with the provided `value` as a single
    // positional argument.  This supports common call patterns like passing the
    // variant label as a string (e.g. "NORTH") or the numeric variant index.
    // Return Ok(None) on failure so other conversion strategies can be tried.
    match eval.eval_function(typ, &[value], &[]) {
        Ok(converted) => Ok(Some(converted)),
        Err(_) => Ok(None), // Can't convert - let caller try other strategies
    }
}

// Helper function to attempt net type conversion when passing a typed net to
// a parameter expecting a different net type (e.g., Power -> Net).
// Returns `Ok(Some(converted))` if conversion succeeds, `Ok(None)` if not applicable.
fn try_net_conversion<'v>(
    value: Value<'v>,
    expected_typ: Value<'v>,
    eval: &mut Evaluator<'v, '_, '_>,
) -> anyhow::Result<Option<Value<'v>>> {
    // Check if expected type is a NetType
    let expected_type_name = expected_typ
        .downcast_ref::<NetType>()
        .map(|nt| &nt.type_name)
        .or_else(|| {
            expected_typ
                .downcast_ref::<FrozenNetType>()
                .map(|fnt| &fnt.type_name)
        });

    let Some(expected_type_name) = expected_type_name else {
        return Ok(None); // Expected type is not a NetType
    };

    // Check if value is a NetValue
    if let Some(nv) = value.downcast_ref::<NetValue>() {
        let actual_type_name = nv.net_type_name();
        // Only convert if expected type is "Net" and actual type is different
        if expected_type_name == "Net" && actual_type_name != "Net" {
            // Use with_net_type helper to cast the net type without creating a new instance
            return Ok(Some(nv.with_net_type("Net", eval.heap())));
        }
    } else if let Some(fnv) = value.downcast_ref::<FrozenNetValue>() {
        let actual_type_name = fnv.net_type_name();
        // Only convert if expected type is "Net" and actual type is different
        if expected_type_name == "Net" && actual_type_name != "Net" {
            // Use with_net_type helper for frozen nets too
            return Ok(Some(fnv.with_net_type("Net", eval.heap())));
        }
    }

    Ok(None) // No conversion needed or value is not a NetValue
}

fn validate_or_convert<'v>(
    name: &str,
    value: Value<'v>,
    typ: Value<'v>,
    convert: Option<Value<'v>>,
    eval: &mut Evaluator<'v, '_, '_>,
) -> anyhow::Result<Value<'v>> {
    // First, try a direct type match.
    if validate_type(name, value, typ, eval.heap()).is_ok() {
        return Ok(value);
    }

    // 1. Try automatic conversions for values

    // 1a. Try net type conversion (e.g., Power -> Net)
    if let Some(converted) = try_net_conversion(value, typ, eval)? {
        validate_type(name, converted, typ, eval.heap())?;
        return Ok(converted);
    }

    // 1b. If expected type is enum and value is string, auto-convert (enum was downgraded)
    if let Some(converted) = try_enum_conversion(value, typ, eval)? {
        validate_type(name, converted, typ, eval.heap())?;
        return Ok(converted);
    }

    // 2. If a custom converter is provided, use it for other conversions
    if let Some(conv_fn) = convert {
        log::debug!("Converting {name} from {value} to {typ}");
        let converted = eval
            .eval_function(conv_fn, &[value], &[])
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;

        log::debug!("Converted {name} to {converted}");

        // Ensure the converted value now matches the expected type.
        validate_type(name, converted, typ, eval.heap())?;
        log::debug!("Converted {name} to {converted} and validated");
        return Ok(converted);
    }

    // 3. Try automatic int to float conversion (no custom converter)
    let type_str = typ.to_string();
    if type_str == "float" || type_str == "Float" {
        if let Some(i) = value.unpack_i32() {
            let float_val = eval.heap().alloc(StarlarkFloat(i as f64));
            if validate_type(name, float_val, typ, eval.heap()).is_ok() {
                return Ok(float_val);
            }
        }
    }

    // 4. None of the conversion paths worked – propagate the original validation error
    validate_type(name, value, typ, eval.heap())?;
    unreachable!();
}

/// Generate default value for io() parameters, optionally registering nets
fn io_generated_default<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    typ: Value<'v>,
    name: &str,
    for_metadata_only: bool,
) -> starlark::Result<Value<'v>> {
    let heap = eval.heap();

    match typ.get_type() {
        "NetType" => {
            let instance_name = heap.alloc_str(name).to_value();
            if for_metadata_only {
                // Pass __register=false for metadata-only defaults
                let kwargs = vec![("__register", heap.alloc(false))];
                eval.eval_function(typ, &[instance_name], &kwargs)
            } else {
                // Normal instantiation - no need to pass __register (defaults to true)
                eval.eval_function(typ, &[instance_name], &[])
            }
        }
        "InterfaceFactory" => {
            // Use internal instantiation path with explicit registration control
            use crate::lang::interface::{instantiate_interface, InstancePrefix};
            instantiate_interface(
                typ,
                &InstancePrefix::from_root(name),
                !for_metadata_only, // should_register
                heap,
                eval,
            )
        }
        _ => default_for_type(eval, typ).map_err(starlark::Error::from),
    }
}

/// Run check functions on a value
fn run_checks<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    checks: Option<Value<'v>>,
    value: Value<'v>,
) -> starlark::Result<()> {
    if let Some(checks_value) = checks {
        if let Some(checks_list) = ListRef::from_value(checks_value) {
            // It's a list - iterate through all check functions
            for check_fn in checks_list.iter() {
                eval.eval_function(check_fn, &[value], &[])?;
            }
        } else {
            // It's a single function - call it directly
            eval.eval_function(checks_value, &[value], &[])?;
        }
    }
    Ok(())
}

/// Callable wrapper for nets() method on modules
#[derive(Clone, Debug, Coerce, Trace, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub struct NetsCallableGen<V: ValueLifetimeless> {
    module: V,
}

starlark_complex_value!(pub NetsCallable);

impl<V: ValueLifetimeless> std::fmt::Display for NetsCallableGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Module.nets")
    }
}

#[starlark_value(type = "function")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for NetsCallableGen<V>
where
    Self: ProvidesStaticType<'v>,
{
    fn invoke(
        &self,
        _me: Value<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        // Nets method takes no arguments
        if args.len()? != 0 {
            return Err(starlark::Error::new_other(anyhow::anyhow!(
                "nets() takes no arguments"
            )));
        }

        let heap = eval.heap();

        // Get the module and collect its components
        let module_ref = downcast_frozen_module!(self.module);
        let components = eval.collect_components(module_ref.path());

        // Build reverse mapping: net_name -> list of (comp_path, pin_name) tuples
        let mut net_to_ports: HashMap<String, Vec<Value<'v>>> = HashMap::new();

        for (comp_path, component_val) in components.iter() {
            if let Some(component) = component_val.downcast_ref::<FrozenComponentValue>() {
                for (pin_name, net_val) in component.connections().iter() {
                    if let Some(net) = net_val.downcast_ref::<FrozenNetValue>() {
                        let port_tuple = heap.alloc((
                            heap.alloc_str(comp_path.to_string().as_str()),
                            heap.alloc_str(pin_name),
                        ));
                        net_to_ports
                            .entry(net.name().to_string())
                            .or_default()
                            .push(port_tuple.to_value());
                    }
                }
            }
        }

        // Convert to starlark dict format
        let nets_dict: Vec<_> = net_to_ports
            .into_iter()
            .map(|(net_name, port_tuples)| {
                (
                    heap.alloc_str(&net_name).to_value(),
                    heap.alloc(port_tuples),
                )
            })
            .collect();

        Ok(heap.alloc(AllocDict(nets_dict)))
    }
}

/// Callable wrapper for components() method on modules
#[derive(Clone, Debug, Coerce, Trace, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub struct ComponentsCallableGen<V: ValueLifetimeless> {
    module: V,
}

/// Callable wrapper for graph() method on modules
#[derive(Clone, Debug, Coerce, Trace, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub struct GraphCallableGen<V: ValueLifetimeless> {
    module: V,
}

starlark_complex_value!(pub ComponentsCallable);
starlark_complex_value!(pub GraphCallable);

impl<V: ValueLifetimeless> std::fmt::Display for ComponentsCallableGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Module.components")
    }
}

#[starlark_value(type = "function")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for ComponentsCallableGen<V>
where
    Self: ProvidesStaticType<'v>,
{
    fn invoke(
        &self,
        _me: Value<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        // Components method takes no arguments
        if args.len()? != 0 {
            return Err(starlark::Error::new_other(anyhow::anyhow!(
                "components() takes no arguments"
            )));
        }

        let heap = eval.heap();

        // Get the module and collect its components
        let module_ref = downcast_frozen_module!(self.module);
        let base_path = module_ref.path();
        let components = eval.collect_components(base_path);

        Ok(heap.alloc(AllocDict(
            components
                .iter()
                .map(|(path, comp_val)| {
                    let key = path.to_rel_string(base_path).unwrap_or_default();
                    (heap.alloc_str(&key).to_value(), comp_val.to_value())
                })
                .collect::<Vec<_>>(),
        )))
    }
}

impl<V: ValueLifetimeless> std::fmt::Display for GraphCallableGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Module.graph")
    }
}

#[starlark_value(type = "function")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for GraphCallableGen<V>
where
    Self: ProvidesStaticType<'v>,
{
    fn invoke(
        &self,
        _me: Value<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        // Graph method takes no arguments
        if args.len()? != 0 {
            return Err(starlark::Error::new_other(anyhow::anyhow!(
                "graph() takes no arguments"
            )));
        }

        let heap = eval.heap();

        // Get the module and collect its components
        let module_ref = downcast_frozen_module!(self.module);
        let components = eval.collect_components(module_ref.path());

        // Collect all component connections to build nets using proper types
        use crate::graph::PortPath;
        let mut net_to_ports: HashMap<String, Vec<PortPath>> = HashMap::new();
        let mut component_pins: HashMap<ModulePath, Vec<String>> = HashMap::new();

        for (comp_path, component_val) in components.iter() {
            if let Some(component) = component_val.downcast_ref::<FrozenComponentValue>() {
                let mut pins = Vec::new();
                for (pin_name, net_val) in component.connections().iter() {
                    if let Some(net) = net_val.downcast_ref::<crate::FrozenNetValue>() {
                        let port_path = PortPath::new(comp_path.clone(), pin_name.clone());
                        pins.push(pin_name.clone());

                        net_to_ports
                            .entry(net.name().to_string())
                            .or_default()
                            .push(port_path);
                    }
                }
                if !pins.is_empty() {
                    component_pins.insert(comp_path.clone(), pins);
                }
            }
        }

        // Collect public nets from module signature (io() parameters)
        let mut public_nets = HashSet::new();
        for param in module_ref.signature().iter() {
            if !param.is_config {
                // This is an io() parameter - extract nets from it
                if let Some(actual_value) = &param.actual_value {
                    public_nets.extend(ModuleValueGen::<V>::extract_nets_from_value(
                        actual_value.to_value(),
                    ));
                }
            }
        }

        // Build the CircuitGraph directly from the collected data
        let graph = crate::graph::CircuitGraph::new(net_to_ports, component_pins, public_nets)
            .map_err(|e| {
                starlark::Error::new_other(anyhow::anyhow!("Failed to create circuit graph: {}", e))
            })?;

        // Create and return ModuleGraph object
        let module_graph = ModuleGraphValueGen {
            module: self.module.to_value(),
            graph: Arc::new(graph),
        };

        Ok(heap.alloc_complex(module_graph))
    }
}

/// ModuleType is used for type annotations (like ComponentType)
#[derive(Debug, Trace, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub struct ModuleType;

starlark_simple_value!(ModuleType);

#[starlark_value(type = "Module")]
impl<'v> StarlarkValue<'v> for ModuleType
where
    Self: ProvidesStaticType<'v>,
{
    fn invoke(
        &self,
        _me: Value<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        // Extract path parameter - Module only takes one positional argument
        let path = args.positional1(eval.heap())?;

        let path = path
            .unpack_str()
            .ok_or_else(|| {
                starlark::Error::new_other(anyhow::anyhow!("Module path must be a string"))
            })?
            .to_string();

        // Get the parent context from the evaluator's ContextValue if available
        let parent_context = eval.eval_context().expect("expected eval context");
        let span = eval.call_stack_top_location().unwrap().resolve_span();
        let output = parent_context.resolve_and_eval_module(&path, Some(span))?;
        let mut params: Vec<String> = vec!["name".to_string(), "properties".to_string()];
        let mut param_types: SmallMap<String, String> = SmallMap::new();

        if let Some(extra) = output
            .star_module
            .extra_value()
            .and_then(|e| e.downcast_ref::<FrozenContextValue>())
        {
            // Get the signature from the module
            for param in extra.module.signature().iter() {
                params.push(param.name.clone());
                param_types.insert(param.name.clone(), param.type_value.to_string());
            }
        }
        let loader = ModuleLoader {
            name: output.sch_module.path.name().clone(),
            source_path: output.sch_module.source_path.clone(),
            params,
            param_types,
            frozen_module: Some(output.star_module),
        };

        // Retain the child heap so the cached values remain valid for the lifetime of the
        // parent module.
        if let Some(frozen_mod) = &loader.frozen_module {
            eval.frozen_heap().add_reference(frozen_mod.frozen_heap());
        }

        Ok(eval.heap().alloc(loader))
    }

    fn eval_type(&self) -> Option<starlark::typing::Ty> {
        Some(<FrozenModuleValue as StarlarkValue>::get_type_starlark_repr())
    }
}

impl std::fmt::Display for ModuleType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<Module>")
    }
}

#[starlark_module]
pub fn module_globals(builder: &mut GlobalsBuilder) {
    const Module: ModuleType = ModuleType;

    /// Declare a net/interface dependency on this module.
    fn io<'v>(
        #[starlark(require = pos)] name: String,
        #[starlark(require = pos)] typ: Value<'v>,
        checks: Option<Value<'v>>, // list of check functions to run on the value
        #[starlark(require = named)] default: Option<Value<'v>>, // explicit default provided by caller
        #[starlark(require = named)] optional: Option<bool>, // if true, the placeholder is not required
        #[starlark(require = named)] help: Option<String>,   // help text describing the parameter
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        let is_optional = optional.unwrap_or(false);

        // Helper to compute default value
        let compute_default = |eval: &mut Evaluator<'v, '_, '_>,
                               for_metadata_only: bool|
         -> starlark::Result<Value<'v>> {
            if let Some(explicit_default) = default {
                validate_type(name.as_str(), explicit_default, typ, eval.heap())?;
                Ok(explicit_default)
            } else if matches!(typ.get_type(), "NetType" | "InterfaceFactory") {
                io_generated_default(eval, typ, &name, for_metadata_only)
            } else {
                Ok(default_for_type(eval, typ)?)
            }
        };

        // Compute the actual value and metadata default
        let (result_value, default_for_metadata) =
            if let Some(provided) = eval.request_input(&name)? {
                // Value provided by parent - validate/convert it
                let value = validate_or_convert(&name, provided, typ, None, eval)?;
                // Generate default for metadata only (with unique name to avoid duplicates)
                let metadata_default = compute_default(eval, true)?;
                (value, Some(metadata_default))
            } else if is_optional {
                // Optional parameter with no provided value
                let default_val = compute_default(eval, false)?;
                if matches!(typ.get_type(), "NetType" | "InterfaceFactory") {
                    // Use generated net/interface as actual value
                    (default_val, Some(default_val))
                } else {
                    // Other types: return None but record default for metadata
                    (Value::new_none(), Some(default_val))
                }
            } else {
                // Required parameter with no provided value
                let strict = eval
                    .context_value()
                    .map(|ctx| ctx.strict_io_config())
                    .unwrap_or(false);

                if strict {
                    if let Some(ctx) = eval.context_value() {
                        ctx.add_missing_input(name.clone());
                    }
                    return Err(MissingInputError { name: name.clone() }.into());
                }

                // Non-strict mode: use computed default
                let default_val = compute_default(eval, false)?;
                (default_val, Some(default_val))
            };

        // Run checks
        run_checks(eval, checks, result_value)?;

        // Record metadata
        if let Some(ctx) = eval.context_value() {
            let mut module = ctx.module_mut();
            module.add_parameter_metadata(
                name.clone(),
                typ,
                is_optional,
                default_for_metadata,
                false, // is_config
                help,
                Some(result_value),
            );
        }

        Ok(result_value)
    }

    /// Declare a configuration value requirement. Works analogously to `io()` but typically
    /// used for primitive types coming from user configuration.
    fn config<'v>(
        #[starlark(require = pos)] name: String,
        #[starlark(require = pos)] typ: Value<'v>,
        #[starlark(require = named)] default: Option<Value<'v>>,
        #[starlark(require = named)] convert: Option<Value<'v>>,
        #[starlark(require = named)] optional: Option<bool>,
        #[starlark(require = named)] help: Option<String>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let is_optional = optional.unwrap_or(false);

        // Compute the actual value
        let result_value = if let Some(provided) = eval.request_input(&name)? {
            // Value provided - validate/convert it
            validate_or_convert(&name, provided, typ, convert, eval)?
        } else if is_optional {
            // Optional parameter with no provided value
            if let Some(default_val) = default {
                validate_or_convert(&name, default_val, typ, convert, eval)?
            } else {
                Value::new_none()
            }
        } else {
            // Required parameter with no provided value
            let strict = eval
                .context_value()
                .map(|ctx| ctx.strict_io_config())
                .unwrap_or(false);

            if strict && default.is_none() {
                if let Some(ctx) = eval.context_value() {
                    ctx.add_missing_input(name.clone());
                }
                return Err(anyhow::Error::new(MissingInputError { name: name.clone() }));
            }

            // Use default or generate one
            if let Some(default_val) = default {
                validate_or_convert(&name, default_val, typ, convert, eval)?
            } else {
                let gen_value = default_for_type(eval, typ)?;
                validate_or_convert(&name, gen_value, typ, convert, eval)?
            }
        };

        // Record metadata
        if let Some(ctx) = eval.context_value() {
            let mut module = ctx.module_mut();
            module.add_parameter_metadata(
                name.clone(),
                typ,
                is_optional,
                default,
                true, // is_config
                help,
                Some(result_value),
            );
        }

        Ok(result_value)
    }

    fn add_property<'v>(
        #[starlark(require = pos)] name: String,
        #[starlark(require = pos)] value: Value<'v>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        eval.add_property(&name, value);

        Ok(Value::new_none())
    }

    /// Record a path movement directive for refactoring support.
    fn moved<'v>(
        #[starlark(require = pos)] old_path: String,
        #[starlark(require = pos)] new_path: String,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        if let Some(ctx) = eval.context_value() {
            ctx.add_moved_directive(old_path, new_path, false);
        }
        Ok(Value::new_none())
    }
}
