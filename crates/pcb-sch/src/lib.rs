//! Simple netlist extraction utilities for Diode's schematic viewer.
//!
//! This crate re-exports a small subset of the Atopile evaluator output that is
//! required by the GUI schematic viewer and other downstream tooling.  It is
//! a *read-only* representation – the structures are serialisable using
//! `serde` so that they can be stored or transferred as JSON.
//!
//! The central structure is [`netlist::Schematic`], which stores two maps:
//!
//! * `instances` – all `Module`, `Component` and `Port` instances keyed by a
//!   stable [`netlist::InstanceRef`].
//! * `nets` – all electrical nets keyed by their deduplicated name.

mod bom;
pub mod hierarchical_layout;
pub mod kicad_netlist;
pub mod kicad_schematic;
pub mod physical;
pub mod position;

// Re-export BOM functionality
pub use bom::{
    parse_kicad_csv_bom, Alternative, Bom, BomEntry, BomMatchingKey, BomMatchingRule, Capacitor,
    Dielectric, GenericComponent, GenericMatchingKey, GroupedBomEntry, KiCadBomError, MatchedOffer,
    Offer, Resistor, UngroupedBomEntry,
};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::physical::PhysicalValue;
use crate::position::Position;

/// Helper type alias – we map the original Atopile `Symbol` to a plain
/// UTF-8 `String`.
pub type Symbol = String;

/// Attribute key that stores the path to the KiCad PCB layout associated with
/// a module or instance. Used with `AttributeValue::String`.
pub const ATTR_LAYOUT_PATH: &str = "layout_path";

/// Attribute key that stores a list of layout hint expressions (e.g. placement
/// constraints). Used with `AttributeValue::Array` where each element is an
/// `AttributeValue::String`.
pub const ATTR_LAYOUT_HINTS: &str = "layout_hints";

/// Reference to a *module definition* (type) together with the file it was
/// declared in.
///
/// This is **not** an *instance* – rather it identifies the *kind* (type) of a
/// module so that different instances referring to the same definition share a
/// single `ModuleRef`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ModuleRef {
    /// Absolute path to the source file that declares the root module.
    pub source_path: PathBuf,
    /// Name of the root module inside that file.
    pub module_name: Symbol,
}

impl ModuleRef {
    pub fn new<P: Into<PathBuf>, S: Into<Symbol>>(source_path: P, module_name: S) -> Self {
        Self {
            source_path: source_path.into(),
            module_name: module_name.into(),
        }
    }
    /// Convenience constructor from a `&Path`.
    pub fn from_path(path: &Path, module_name: &str) -> Self {
        Self {
            source_path: path.to_path_buf(),
            module_name: module_name.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq)]
#[serde(into = "String", try_from = "String")] // serialize and deserialize using string format
pub struct InstanceRef {
    /// Reference to the root module this instance belongs to.
    pub module: ModuleRef,
    /// Hierarchical path from the root module to this instance.
    pub instance_path: Vec<Symbol>,
}

impl InstanceRef {
    pub fn new(module: ModuleRef, instance_path: Vec<Symbol>) -> Self {
        Self {
            module,
            instance_path,
        }
    }

    pub fn append(&self, instance_path: Symbol) -> Self {
        let mut new_path = self.instance_path.clone();
        new_path.push(instance_path);

        Self {
            module: self.module.clone(),
            instance_path: new_path,
        }
    }
}

impl std::hash::Hash for InstanceRef {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Hash via Display representation for stable hashing
        self.to_string().hash(state);
    }
}

impl PartialEq for InstanceRef {
    fn eq(&self, other: &Self) -> bool {
        self.module.source_path == other.module.source_path
            && self.module.module_name == other.module.module_name
            && self.instance_path == other.instance_path
    }
}

impl std::fmt::Display for InstanceRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}",
            self.module.source_path.display(),
            self.module.module_name
        )?;
        for part in &self.instance_path {
            write!(f, ".{part}")?;
        }
        Ok(())
    }
}

impl From<InstanceRef> for String {
    fn from(i: InstanceRef) -> Self {
        i.to_string()
    }
}

impl std::str::FromStr for InstanceRef {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Parse format: "path/to/file.zen:module_name.instance.path"
        let (module_part, instance_path_str) = s
            .split_once(':')
            .ok_or_else(|| format!("Invalid InstanceRef format: missing ':' in '{}'", s))?;

        let parts: Vec<&str> = instance_path_str.split('.').collect();
        if parts.is_empty() {
            return Err(format!("Invalid InstanceRef: no module name in '{}'", s));
        }

        let module_name = parts[0];
        let instance_path: Vec<Symbol> = parts[1..].iter().map(|&p| p.into()).collect();

        let module_ref = ModuleRef::new(PathBuf::from(module_part), Symbol::from(module_name));
        Ok(InstanceRef::new(module_ref, instance_path))
    }
}

impl TryFrom<String> for InstanceRef {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

/// Discriminates the *kind* of an [`Instance`].
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum InstanceKind {
    Module,
    Component,
    Interface,
    Port,
    Pin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PhysicalUnit {
    Ohms,
    Volts,
    Amperes,
    Farads,
    Henries,
    Hertz,
    Seconds,
    Kelvin,
    Coulombs,
    Watts,
    Joules,
    Siemens,
    Webers,
}

impl PhysicalUnit {
    pub const fn suffix(&self) -> &'static str {
        match self {
            PhysicalUnit::Ohms => "", // This should be "Ohm", but keep as empty for backward compatibility
            PhysicalUnit::Volts => "V",
            PhysicalUnit::Amperes => "A",
            PhysicalUnit::Farads => "F",
            PhysicalUnit::Henries => "H",
            PhysicalUnit::Hertz => "Hz",
            PhysicalUnit::Seconds => "s",
            PhysicalUnit::Kelvin => "K",
            PhysicalUnit::Coulombs => "C",
            PhysicalUnit::Watts => "W",
            PhysicalUnit::Joules => "J",
            PhysicalUnit::Siemens => "S",
            PhysicalUnit::Webers => "Wb",
        }
    }

    pub const fn quantity(&self) -> &'static str {
        match self {
            PhysicalUnit::Ohms => "Resistance",
            PhysicalUnit::Volts => "Voltage",
            PhysicalUnit::Amperes => "Current",
            PhysicalUnit::Farads => "Capacitance",
            PhysicalUnit::Henries => "Inductance",
            PhysicalUnit::Hertz => "Frequency",
            PhysicalUnit::Seconds => "Time",
            PhysicalUnit::Kelvin => "Temperature",
            PhysicalUnit::Coulombs => "Charge",
            PhysicalUnit::Watts => "Power",
            PhysicalUnit::Joules => "Energy",
            PhysicalUnit::Siemens => "Conductance",
            PhysicalUnit::Webers => "Flux",
        }
    }
}

impl std::fmt::Display for PhysicalUnit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PhysicalUnit::Ohms => write!(f, "Ohm"),
            PhysicalUnit::Volts => write!(f, "Volt"),
            PhysicalUnit::Amperes => write!(f, "Ampere"),
            PhysicalUnit::Farads => write!(f, "Farad"),
            PhysicalUnit::Henries => write!(f, "Henry"),
            PhysicalUnit::Hertz => write!(f, "Hertz"),
            PhysicalUnit::Seconds => write!(f, "Second"),
            PhysicalUnit::Kelvin => write!(f, "Kelvin"),
            PhysicalUnit::Coulombs => write!(f, "Coulomb"),
            PhysicalUnit::Watts => write!(f, "Watt"),
            PhysicalUnit::Joules => write!(f, "Joule"),
            PhysicalUnit::Siemens => write!(f, "Siemens"),
            PhysicalUnit::Webers => write!(f, "Weber"),
        }
    }
}

impl std::str::FromStr for PhysicalUnit {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "" | "Ω" | "ohm" | "Ohm" | "ohms" | "Ohms" => Ok(PhysicalUnit::Ohms),
            "V" | "volt" | "Volt" | "volts" | "Volts" => Ok(PhysicalUnit::Volts),
            "A" | "ampere" | "Ampere" | "amperes" | "Amperes" => Ok(PhysicalUnit::Amperes),
            "F" | "farad" | "Farad" | "farads" | "Farads" => Ok(PhysicalUnit::Farads),
            "H" | "henry" | "Henry" | "henries" | "Henries" => Ok(PhysicalUnit::Henries),
            "Hz" | "hz" | "hertz" | "Hertz" => Ok(PhysicalUnit::Hertz),
            "s" | "second" | "Second" | "seconds" | "Seconds" => Ok(PhysicalUnit::Seconds),
            "K" | "kelvin" | "Kelvin" => Ok(PhysicalUnit::Kelvin),
            "C" | "coulomb" | "Coulomb" | "coulombs" | "Coulombs" => Ok(PhysicalUnit::Coulombs),
            "W" | "watt" | "Watt" | "watts" | "Watts" => Ok(PhysicalUnit::Watts),
            "J" | "joule" | "Joule" | "joules" | "Joules" => Ok(PhysicalUnit::Joules),
            "S" | "siemens" | "Siemens" => Ok(PhysicalUnit::Siemens),
            "Wb" | "weber" | "Weber" | "webers" | "Webers" => Ok(PhysicalUnit::Webers),
            _ => Err(format!("Unknown unit: '{}'", s)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")] // Match original casing in JSON (String, Number ...)
pub enum AttributeValue {
    String(String),
    Number(f64),
    Boolean(bool),
    Port(String),
    Array(Vec<AttributeValue>),
    Json(serde_json::Value),
}

impl AttributeValue {
    pub fn string(&self) -> Option<&str> {
        match self {
            AttributeValue::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn physical(&self) -> Option<PhysicalValue> {
        match self {
            AttributeValue::String(s) => s.parse().ok(),
            _ => None,
        }
    }
}

impl From<String> for AttributeValue {
    fn from(s: String) -> Self {
        AttributeValue::String(s)
    }
}

/// High-level semantic classification of a net.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum NetKind {
    /// Standard signal net.
    Normal,
    /// Dedicated ground return.
    Ground,
    /// Dedicated power rail.
    Power,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Net {
    pub kind: NetKind,
    pub id: u64,
    pub name: String,
    pub ports: Vec<InstanceRef>,
    pub properties: HashMap<Symbol, AttributeValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Instance {
    pub type_ref: ModuleRef,
    pub kind: InstanceKind,
    pub attributes: HashMap<Symbol, AttributeValue>,
    pub children: HashMap<Symbol, InstanceRef>,
    pub reference_designator: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub symbol_positions: HashMap<String, Position>,
}

impl Instance {
    pub fn new(type_ref: ModuleRef, kind: InstanceKind) -> Self {
        Self {
            type_ref,
            kind,
            attributes: HashMap::new(),
            children: HashMap::new(),
            reference_designator: None,
            symbol_positions: HashMap::new(),
        }
    }

    // Convenience constructors for common instance kinds --------------------
    pub fn module(type_ref: ModuleRef) -> Self {
        Self::new(type_ref, InstanceKind::Module)
    }

    pub fn component(type_ref: ModuleRef) -> Self {
        Self::new(type_ref, InstanceKind::Component)
    }

    pub fn interface(type_ref: ModuleRef) -> Self {
        Self::new(type_ref, InstanceKind::Interface)
    }

    pub fn port(type_ref: ModuleRef) -> Self {
        Self::new(type_ref, InstanceKind::Port)
    }

    pub fn pin(type_ref: ModuleRef) -> Self {
        Self::new(type_ref, InstanceKind::Pin)
    }

    // Fluent-style mutators --------------------------------------------------
    /// Add (or replace) an attribute and return a mutable reference for
    /// further chaining.
    pub fn add_attribute(
        &mut self,
        key: impl Into<Symbol>,
        value: impl Into<AttributeValue>,
    ) -> &mut Self {
        self.attributes.insert(key.into(), value.into());
        self
    }

    /// Builder-style attribute insertion that consumes `self` and returns it.
    pub fn with_attribute(
        mut self,
        key: impl Into<Symbol>,
        value: impl Into<AttributeValue>,
    ) -> Self {
        self.attributes.insert(key.into(), value.into());
        self
    }

    /// Add (or replace) a child reference and return a mutable reference for
    /// chaining.
    pub fn add_child(&mut self, name: impl Into<Symbol>, reference: InstanceRef) -> &mut Self {
        self.children.insert(name.into(), reference);
        self
    }

    /// Builder-style child insertion that consumes `self`.
    pub fn with_child(mut self, name: impl Into<Symbol>, reference: InstanceRef) -> Self {
        self.children.insert(name.into(), reference);
        self
    }

    /// Set the reference designator, returning a mutable reference for chaining.
    pub fn set_reference_designator(&mut self, designator: impl Into<String>) -> &mut Self {
        self.reference_designator = Some(designator.into());
        self
    }

    /// Builder-style reference designator insertion that consumes `self`.
    pub fn with_reference_designator(mut self, designator: impl Into<String>) -> Self {
        self.reference_designator = Some(designator.into());
        self
    }

    pub fn string_attr(&self, keys: &[&str]) -> Option<String> {
        keys.iter().find_map(|&key| {
            self.attributes.get(key).and_then(|attr| match attr {
                AttributeValue::String(s) => Some(s.clone()),
                _ => None,
            })
        })
    }

    pub fn boolean_attr(&self, keys: &[&str]) -> Option<bool> {
        keys.iter().find_map(|&key| {
            self.attributes.get(key).and_then(|attr| match attr {
                AttributeValue::Boolean(b) => Some(*b),
                _ => None,
            })
        })
    }

    pub fn string_list_attr(&self, keys: &[&str]) -> Vec<String> {
        keys.iter()
            .find_map(|&key| match self.attributes.get(key)? {
                AttributeValue::Array(arr) => Some(
                    arr.iter()
                        .filter_map(|av| match av {
                            AttributeValue::String(s) => Some(s.clone()),
                            _ => None,
                        })
                        .collect::<Vec<String>>(),
                ),
                _ => None,
            })
            .unwrap_or_default()
    }

    pub fn alternatives_attr(&self) -> Vec<crate::bom::Alternative> {
        use crate::bom::Alternative;

        let keys = ["alternatives", "__alternatives__"];
        keys.iter()
            .filter_map(|&key| self.attributes.get(key))
            .filter_map(|val| match val {
                AttributeValue::Array(arr) => Some(arr),
                _ => None,
            })
            .map(|arr| {
                let alternatives: Vec<Alternative> = arr
                    .iter()
                    .filter_map(|av| {
                        // Try to parse as JSON object
                        if let AttributeValue::Json(json_val) = av {
                            let mpn = json_val.get("mpn")?.as_str()?.to_string();
                            let manufacturer = json_val.get("manufacturer")?.as_str()?.to_string();
                            return Some(Alternative { mpn, manufacturer });
                        }
                        // Try to parse JSON string (from Starlark dict serialization)
                        if let AttributeValue::String(s) = av {
                            if let Ok(json_val) = serde_json::from_str::<serde_json::Value>(s) {
                                let mpn = json_val.get("mpn")?.as_str()?.to_string();
                                let manufacturer =
                                    json_val.get("manufacturer")?.as_str()?.to_string();
                                return Some(Alternative { mpn, manufacturer });
                            }
                        }
                        None
                    })
                    .collect();
                alternatives
            })
            .next()
            .unwrap_or_default()
    }

    pub fn physical_attr(&self, keys: &[&str]) -> Option<PhysicalValue> {
        keys.iter()
            .filter_map(|&key| self.attributes.get(key))
            .find_map(|attr| attr.physical())
    }

    pub fn component_type(&self) -> Option<String> {
        self.string_attr(&["Type", "type"])
            .map(|s| s.to_lowercase())
    }

    pub fn mpn(&self) -> Option<String> {
        self.string_attr(&["MPN", "Mpn", "mpn"])
    }

    pub fn manufacturer(&self) -> Option<String> {
        self.string_attr(&["Manufacturer", "manufacturer"])
    }

    pub fn description(&self) -> Option<String> {
        self.string_attr(&["Description", "description"])
    }

    pub fn package(&self) -> Option<String> {
        self.string_attr(&["Package", "package"])
    }

    pub fn value(&self) -> Option<String> {
        self.string_attr(&["Value", "value"])
    }

    pub fn dnp(&self) -> bool {
        // Check for the standardized boolean "dnp" attribute
        self.boolean_attr(&["dnp"]).unwrap_or(false)
    }

    pub fn skip_bom(&self) -> bool {
        // Check for the standardized boolean "skip_bom" attribute
        self.boolean_attr(&["skip_bom"]).unwrap_or(false)
    }

    pub fn skip_pos(&self) -> bool {
        // Check for the standardized boolean "skip_pos" attribute
        self.boolean_attr(&["skip_pos"]).unwrap_or(false)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
/// Complete schematic description (instances + nets).
pub struct Schematic {
    /// Every instance in the design, keyed by its fully-qualified reference.
    pub instances: HashMap<InstanceRef, Instance>,

    /// Electrical nets, keyed by their **unique** name.
    pub nets: HashMap<String, Net>,

    /// Root module reference.
    pub root_ref: Option<InstanceRef>,

    /// Symbol library - maps symbol paths to their s-expression content
    pub symbols: HashMap<String, String>,

    /// Path remapping rules for moved() directives (old_path -> new_path)
    pub moved_paths: HashMap<String, String>,
}

impl Schematic {
    /// Create an empty schematic.
    pub fn new() -> Self {
        Self::default()
    }

    /// Serialize the schematic to JSON string.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Insert (or replace) an instance.
    pub fn add_instance(&mut self, reference: InstanceRef, instance: Instance) -> &mut Self {
        self.instances.insert(reference, instance);
        self
    }

    /// Mutable access to an existing instance (if any).
    pub fn instance_mut(&mut self, reference: &InstanceRef) -> Option<&mut Instance> {
        self.instances.get_mut(reference)
    }

    /// Insert (or replace) a net.
    pub fn add_net(&mut self, net: Net) -> &mut Self {
        self.nets.insert(net.name.clone(), net);
        self
    }

    /// Mutable access to an existing net by name.
    pub fn net_mut(&mut self, name: &str) -> Option<&mut Net> {
        self.nets.get_mut(name)
    }

    /// Set the root module reference.
    pub fn set_root_ref(&mut self, root: InstanceRef) -> &mut Self {
        self.root_ref = Some(root);
        self
    }

    pub fn root(&self) -> Option<&Instance> {
        self.root_ref
            .as_ref()
            .map(|r| self.instances.get(r).unwrap())
    }

    /// Assign reference designators to all components in the schematic.
    ///
    /// This follows the same logic as KiCad netlist export:
    /// 1. Components are sorted by their hierarchical path
    /// 2. Reference designators are assigned using a prefix (derived from component attributes)
    ///    and an incrementing counter
    ///
    /// Returns a map from InstanceRef to the assigned reference designator.
    pub fn assign_reference_designators(&mut self) -> HashMap<InstanceRef, String> {
        // Collect all components
        let mut components: Vec<(&InstanceRef, &mut Instance)> = self
            .instances
            .iter_mut()
            .filter(|(_, inst)| inst.kind == InstanceKind::Component)
            .collect();

        // Sort by hierarchical name (dot-separated instance path) for deterministic ordering
        components.sort_by(|a, b| {
            let hier_a = a.0.instance_path.join(".");
            let hier_b = b.0.instance_path.join(".");
            hier_a.cmp(&hier_b)
        });

        // Track counters for each prefix
        let mut ref_counts: HashMap<String, u32> = HashMap::new();
        let mut ref_map: HashMap<InstanceRef, String> = HashMap::new();

        // Assign reference designators
        for (inst_ref, instance) in components {
            let prefix = get_component_prefix(instance);
            let counter = ref_counts.entry(prefix.clone()).or_default();
            *counter += 1;
            let refdes = format!("{}{}", prefix, *counter);

            // Store in the instance
            instance.reference_designator = Some(refdes.clone());

            // Store in the return map
            ref_map.insert(inst_ref.clone(), refdes);
        }

        ref_map
    }

    pub fn bom(&self) -> Bom {
        Bom::from_schematic(self)
    }
}

/// Extract a prefix string for a component.
///
/// Prefers explicit `prefix` attribute if present, otherwise derives from
/// component `type` attribute (e.g. `resistor` → `R`), with fallback to `U`.
pub fn get_component_prefix(inst: &Instance) -> String {
    // Prefer explicit `prefix` attribute if present
    if let Some(AttributeValue::String(s)) = inst.attributes.get("prefix") {
        return s.clone();
    }
    // Derive from component `type` attribute (e.g. `res` → `R`)
    if let Some(AttributeValue::String(t)) = inst.attributes.get("type") {
        if let Some(first) = t.chars().next() {
            return first.to_ascii_uppercase().to_string();
        }
    }
    // Fallback to "U"
    "U".to_owned()
}

impl Net {
    /// Create a new net with the given kind and name.
    pub fn new(kind: NetKind, name: impl Into<String>, id: u64) -> Self {
        Self {
            kind,
            id,
            name: name.into(),
            ports: Vec::new(),
            properties: HashMap::new(),
        }
    }

    /// Add a port (instance reference) to the net and return a mutable
    /// reference for chaining.
    pub fn add_port(&mut self, port: InstanceRef) -> &mut Self {
        self.ports.push(port);
        self
    }

    /// Builder-style port insertion that consumes `self`.
    pub fn with_port(mut self, port: InstanceRef) -> Self {
        self.ports.push(port);
        self
    }

    /// Add (or replace) a property and return a mutable reference for chaining.
    pub fn add_property(
        &mut self,
        key: impl Into<Symbol>,
        value: impl Into<AttributeValue>,
    ) -> &mut Self {
        self.properties.insert(key.into(), value.into());
        self
    }

    /// Builder-style property insertion that consumes `self`.
    pub fn with_property(
        mut self,
        key: impl Into<Symbol>,
        value: impl Into<AttributeValue>,
    ) -> Self {
        self.properties.insert(key.into(), value.into());
        self
    }
}

/// Fluent builder for constructing [`Schematic`] structures.
///
/// Example:
/// ```rust
/// use pcb_sch::*;
/// # use std::path::Path;
/// let root_mod = ModuleRef::from_path(Path::new("/project/root.pmod"), "Root");
/// let root_ref = InstanceRef::new(root_mod.clone(), Vec::new());
/// let mut builder = Schematic::builder();
/// builder.add_instance(root_ref.clone(), Instance::module(root_mod));
/// builder.add_net(Net::new(NetKind::Ground, "GND", 0));
/// let sch = builder.build();
/// ```
#[derive(Default)]
pub struct SchematicBuilder {
    schematic: Schematic,
}

impl SchematicBuilder {
    /// Create a fresh builder with an empty schematic.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert (or replace) an [`Instance`] record.
    pub fn add_instance(&mut self, reference: InstanceRef, instance: Instance) -> &mut Self {
        self.schematic.add_instance(reference, instance);
        self
    }

    /// Insert (or replace) a [`Net`].
    pub fn add_net(&mut self, net: Net) -> &mut Self {
        self.schematic.add_net(net);
        self
    }

    /// Finish building and return the [`Schematic`].
    pub fn build(self) -> Schematic {
        self.schematic
    }
}

impl From<SchematicBuilder> for Schematic {
    fn from(builder: SchematicBuilder) -> Self {
        builder.build()
    }
}

// Provide a convenient entry-point on the [`Schematic`] type itself.
impl Schematic {
    /// Start building a new schematic using the fluent [`SchematicBuilder`].
    pub fn builder() -> SchematicBuilder {
        SchematicBuilder::default()
    }
}

// Tests
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn instance_ref_display_roundtrip() {
        let mod_ref = ModuleRef::from_path(Path::new("/tmp/test.pmod"), "root");
        let inst = InstanceRef::new(mod_ref.clone(), vec!["child".into(), "pin".into()]);
        let disp = inst.to_string();
        assert_eq!(disp, "/tmp/test.pmod:root.child.pin");

        // Hash via string representation should be stable – test equality via roundtrip.
        let mut h1 = std::collections::hash_map::DefaultHasher::new();
        inst.hash(&mut h1);
        let mut h2 = std::collections::hash_map::DefaultHasher::new();
        disp.hash(&mut h2);
        assert_eq!(h1.finish(), h2.finish());
    }

    #[test]
    fn test_assign_reference_designators() {
        let mut schematic = Schematic::new();
        let mod_ref = ModuleRef::from_path(Path::new("/test.pmod"), "TestModule");

        // Add some components with different prefixes
        let r1_ref = InstanceRef::new(mod_ref.clone(), vec!["r1".into()]);
        let r1 = Instance::component(mod_ref.clone()).with_attribute("type", "res".to_string());
        schematic.add_instance(r1_ref.clone(), r1);

        let c1_ref = InstanceRef::new(mod_ref.clone(), vec!["c1".into()]);
        let c1 = Instance::component(mod_ref.clone()).with_attribute("type", "cap".to_string());
        schematic.add_instance(c1_ref.clone(), c1);

        let r2_ref = InstanceRef::new(mod_ref.clone(), vec!["r2".into()]);
        let r2 = Instance::component(mod_ref.clone()).with_attribute("type", "res".to_string());
        schematic.add_instance(r2_ref.clone(), r2);

        // Component with explicit prefix
        let u1_ref = InstanceRef::new(mod_ref.clone(), vec!["u1".into()]);
        let u1 = Instance::component(mod_ref.clone()).with_attribute("prefix", "IC".to_string());
        schematic.add_instance(u1_ref.clone(), u1);

        // Component with MPN
        let d1_ref = InstanceRef::new(mod_ref.clone(), vec!["d1".into()]);
        let d1 = Instance::component(mod_ref.clone()).with_attribute("mpn", "1N4148".to_string());
        schematic.add_instance(d1_ref.clone(), d1);

        // Component with no attributes (should get "U" prefix)
        let unknown_ref = InstanceRef::new(mod_ref.clone(), vec!["unknown".into()]);
        let unknown = Instance::component(mod_ref.clone());
        schematic.add_instance(unknown_ref.clone(), unknown);

        // Assign reference designators
        let ref_map = schematic.assign_reference_designators();

        // Check assignments
        assert_eq!(ref_map.get(&c1_ref), Some(&"C1".to_string()));
        assert_eq!(ref_map.get(&d1_ref), Some(&"U1".to_string())); // No type attribute, so falls back to "U"
        assert_eq!(ref_map.get(&r1_ref), Some(&"R1".to_string()));
        assert_eq!(ref_map.get(&r2_ref), Some(&"R2".to_string()));
        assert_eq!(ref_map.get(&u1_ref), Some(&"IC1".to_string()));
        assert_eq!(ref_map.get(&unknown_ref), Some(&"U2".to_string())); // Second component with "U" prefix

        // Verify the reference designators were also stored in the instances
        assert_eq!(
            schematic
                .instances
                .get(&c1_ref)
                .unwrap()
                .reference_designator,
            Some("C1".to_string())
        );
        assert_eq!(
            schematic
                .instances
                .get(&d1_ref)
                .unwrap()
                .reference_designator,
            Some("U1".to_string())
        );
        assert_eq!(
            schematic
                .instances
                .get(&r1_ref)
                .unwrap()
                .reference_designator,
            Some("R1".to_string())
        );
        assert_eq!(
            schematic
                .instances
                .get(&r2_ref)
                .unwrap()
                .reference_designator,
            Some("R2".to_string())
        );
        assert_eq!(
            schematic
                .instances
                .get(&u1_ref)
                .unwrap()
                .reference_designator,
            Some("IC1".to_string())
        );
        assert_eq!(
            schematic
                .instances
                .get(&unknown_ref)
                .unwrap()
                .reference_designator,
            Some("U2".to_string())
        );
    }
}
