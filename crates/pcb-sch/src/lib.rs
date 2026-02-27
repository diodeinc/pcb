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

pub mod bom;
#[cfg(feature = "table")]
mod bom_table;
pub mod hierarchical_layout;
pub mod kicad_netlist;
pub mod natural_string;
pub mod physical;
pub mod position;

use std::collections::{BTreeMap, HashMap};
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

/// URI prefix for stable, machine-independent package references.
pub const PACKAGE_URI_PREFIX: &str = "package://";

/// Resolve a `package://` URI to an absolute filesystem path.
///
/// Uses longest-prefix matching against the provided package roots map
/// (package URL or package URL + version → absolute filesystem path).
pub fn resolve_package_uri(
    uri: &str,
    package_roots: &BTreeMap<String, PathBuf>,
) -> anyhow::Result<PathBuf> {
    let rest = uri
        .strip_prefix(PACKAGE_URI_PREFIX)
        .ok_or_else(|| anyhow::anyhow!("expected package:// URI, got: {uri}"))?;
    let (pkg_url, pkg_root) = package_roots
        .iter()
        .filter_map(|(coord, root)| {
            (rest.starts_with(coord)
                && (rest.len() == coord.len() || rest.as_bytes().get(coord.len()) == Some(&b'/')))
            .then_some((coord.as_str(), root.as_path()))
        })
        .max_by_key(|(coord, _)| coord.len())
        .ok_or_else(|| anyhow::anyhow!("unknown package in URI: {uri}"))?;

    let rel = rest[pkg_url.len()..].trim_start_matches('/');
    if rel.is_empty() {
        Ok(pkg_root.to_path_buf())
    } else {
        Ok(pkg_root.join(rel))
    }
}

/// Format an absolute path as a `package://` URI.
///
/// Uses longest-prefix matching against the provided package roots map
/// to find the owning package.
pub fn format_package_uri(abs: &Path, package_roots: &BTreeMap<String, PathBuf>) -> Option<String> {
    let (pkg_url, pkg_root) = package_roots
        .iter()
        .filter_map(|(coord, root)| {
            abs.starts_with(root)
                .then_some((coord.as_str(), root.as_path()))
        })
        .max_by_key(|(_, root)| root.as_os_str().len())?;
    let rel = abs.strip_prefix(pkg_root).ok()?;
    let rel_str = rel.to_str()?;
    if rel_str.is_empty() {
        Some(format!("{PACKAGE_URI_PREFIX}{pkg_url}"))
    } else {
        let rel_str = rel_str.replace('\\', "/");
        Some(format!("{PACKAGE_URI_PREFIX}{pkg_url}/{rel_str}"))
    }
}

mod refdes_alloc {
    use super::Symbol;

    #[derive(Debug, Clone)]
    pub(super) struct ParsedRefdes {
        pub(super) prefix: String,
        pub(super) number: u32,
    }

    const KNOWN_PREFIXES: &[&str] = &[
        "A", "C", "D", "F", "FB", "IC", "J", "K", "L", "LED", "MH", "P", "Q", "R", "RV", "SW",
        "TP", "U", "X", "Y",
    ];

    fn is_known_prefix(prefix: &str) -> bool {
        KNOWN_PREFIXES.contains(&prefix)
    }

    fn parse_refdes_like(s: &str) -> Option<ParsedRefdes> {
        // Uppercase letters + digits, no leading zeros (e.g. R1, IC10, LED12, R1000).
        if s.len() < 2 {
            return None;
        }

        let first_digit = s.find(|c: char| c.is_ascii_digit())?;
        let (prefix, digits) = s.split_at(first_digit);
        if prefix.is_empty() || digits.is_empty() {
            return None;
        }
        if !prefix.chars().all(|c| c.is_ascii_uppercase()) {
            return None;
        }
        if !digits.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        if digits.len() > 1 && digits.starts_with('0') {
            return None;
        }

        let number: u32 = digits.parse().ok()?;
        if number == 0 {
            return None;
        }

        Some(ParsedRefdes {
            prefix: prefix.to_owned(),
            number,
        })
    }

    pub(super) fn parse_existing(s: &str) -> Option<ParsedRefdes> {
        // Preserve user-set refdes for large boards too (e.g. R1000).
        parse_refdes_like(s)
    }

    fn parse_hint(s: &str) -> Option<ParsedRefdes> {
        // Hint format is intentionally strict: 1-3 uppercase letters + 1-3 digits.
        if !(2..=6).contains(&s.len()) {
            return None;
        }
        let first_digit = s.find(|c: char| c.is_ascii_digit())?;
        let (prefix, digits) = s.split_at(first_digit);
        if !(1..=3).contains(&prefix.len()) || !(1..=3).contains(&digits.len()) {
            return None;
        }
        if !is_known_prefix(prefix) {
            return None;
        }
        parse_refdes_like(s)
    }

    pub(super) fn extract_hint_number(
        instance_path: &[Symbol],
        component_prefix: &str,
    ) -> Option<u32> {
        // Hints are allowed in any non-leaf segment of the path.
        let (leaf, prefix_path) = instance_path.split_last()?;
        let _ = leaf;

        let mut matching = prefix_path
            .iter()
            .filter_map(|part| parse_hint(part))
            .filter(|hint| hint.prefix == component_prefix)
            .map(|hint| hint.number);

        let first = matching.next()?;
        // Multiple matching hints for one component is ambiguous: ignore all.
        matching.next().is_none().then_some(first)
    }
}

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Net {
    pub kind: String,
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
                        if let AttributeValue::String(s) = av
                            && let Ok(json_val) = serde_json::from_str::<serde_json::Value>(s)
                        {
                            let mpn = json_val.get("mpn")?.as_str()?.to_string();
                            let manufacturer = json_val.get("manufacturer")?.as_str()?.to_string();
                            return Some(Alternative { mpn, manufacturer });
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

    pub fn matcher(&self) -> Option<String> {
        self.string_attr(&["Matcher", "matcher"])
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

    /// Package roots for resolving package:// URIs.
    /// Maps package coordinate (`<url>` for workspace packages, `<url>@<version>` for resolved packages)
    /// to absolute filesystem path.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub package_roots: BTreeMap<String, PathBuf>,
}

impl Schematic {
    /// Create an empty schematic.
    pub fn new() -> Self {
        Self::default()
    }

    /// Serialize the schematic to canonical (deterministic) JSON string.
    /// Uses RFC 8785 canonical JSON format with sorted keys.
    pub fn to_json(&self) -> anyhow::Result<String> {
        let mut buf = Vec::new();
        let mut ser =
            serde_json::Serializer::with_formatter(&mut buf, canon_json::CanonicalFormatter::new());
        serde::Serialize::serialize(self, &mut ser)?;
        Ok(String::from_utf8(buf)?)
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

    /// Resolve the owning component instance for a port by longest-prefix match.
    ///
    /// This is robust to `InstanceRef` string roundtrips where dotted port names
    /// can be split into multiple `instance_path` segments.
    pub fn component_ref_for_port(&self, port_ref: &InstanceRef) -> Option<InstanceRef> {
        for prefix_len in (0..=port_ref.instance_path.len()).rev() {
            let candidate = InstanceRef {
                module: port_ref.module.clone(),
                instance_path: port_ref.instance_path[..prefix_len].to_vec(),
            };
            if self
                .instances
                .get(&candidate)
                .is_some_and(|inst| inst.kind == InstanceKind::Component)
            {
                return Some(candidate);
            }
        }
        None
    }

    /// Resolve the owning component and full port/pin name for a port reference.
    ///
    /// The returned pin name is the suffix after the component path, joined with
    /// `.` so dotted names survive string roundtrips.
    pub fn component_ref_and_pin_for_port(
        &self,
        port_ref: &InstanceRef,
    ) -> Option<(InstanceRef, Symbol)> {
        let comp_ref = self.component_ref_for_port(port_ref)?;
        let pin_path = &port_ref.instance_path[comp_ref.instance_path.len()..];
        if pin_path.is_empty() {
            return None;
        }
        Some((comp_ref, pin_path.join(".")))
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

    /// Resolve a `package://` URI to an absolute path using the schematic's package roots.
    pub fn resolve_package_uri(&self, uri: &str) -> anyhow::Result<PathBuf> {
        resolve_package_uri(uri, &self.package_roots)
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
        struct ComponentForRefdes<'a> {
            hier: String,
            inst_ref: InstanceRef,
            inst: &'a mut Instance,
        }

        // Collect components and cache their hierarchical name for deterministic ordering.
        // Use *natural* ordering so instance names like `R2` sort before `R10`.
        let mut components: Vec<ComponentForRefdes<'_>> = self
            .instances
            .iter_mut()
            .filter_map(|(inst_ref, inst)| {
                (inst.kind == InstanceKind::Component).then_some(ComponentForRefdes {
                    hier: inst_ref.instance_path.join("."),
                    inst_ref: inst_ref.clone(),
                    inst,
                })
            })
            .collect();

        components.sort_by(|a, b| natord::compare(&a.hier, &b.hier));

        // Opportunistic heuristic:
        // If any non-leaf segment of the hierarchical instance path looks like a valid refdes
        // (e.g. `foo.R22.part`) and matches the component's prefix, honor it when safe.
        //
        // Safety rules:
        // - Only accept 1-3 uppercase letters + 1-3 digits, no leading zeros.
        // - Only accept known prefixes (hard-coded, conservative list).
        // - If multiple components hint the same refdes, drop those hints and auto-assign.
        // - If a single component contains multiple matching hints, treat it as ambiguous.

        let prefixes: Vec<String> = components
            .iter()
            .map(|component| get_component_prefix(component.inst))
            .collect();

        let mut used_numbers_by_prefix: HashMap<String, std::collections::HashSet<u32>> =
            HashMap::new();

        // Preserve any pre-assigned reference designators on component instances, as long as they
        // look valid and match the component's prefix. Conflicts are dropped and reassigned.
        let fixed_numbers: Vec<Option<u32>> = components
            .iter()
            .enumerate()
            .map(|(i, component)| {
                let refdes = component.inst.reference_designator.as_deref()?;
                let parsed = refdes_alloc::parse_existing(refdes)?;
                (parsed.prefix == prefixes[i]).then_some(parsed.number)
            })
            .collect();

        let mut fixed_counts: HashMap<(String, u32), usize> = HashMap::new();
        for (i, number) in fixed_numbers.iter().enumerate() {
            let Some(number) = number else {
                continue;
            };
            *fixed_counts
                .entry((prefixes[i].clone(), *number))
                .or_insert(0) += 1;
        }

        let mut assigned_numbers: Vec<Option<u32>> = vec![None; components.len()];
        for (i, number) in fixed_numbers.into_iter().enumerate() {
            let Some(number) = number else { continue };
            let key = (prefixes[i].clone(), number);
            if fixed_counts.get(&key).copied().unwrap_or(0) != 1 {
                continue;
            }
            used_numbers_by_prefix
                .entry(prefixes[i].clone())
                .or_default()
                .insert(number);
            assigned_numbers[i] = Some(number);
        }

        // Opportunistically assign hints for components that didn't have a fixed refdes.
        let hint_numbers: Vec<Option<u32>> = components
            .iter()
            .enumerate()
            .map(|(i, component)| {
                assigned_numbers[i].is_none().then_some(())?;
                refdes_alloc::extract_hint_number(&component.inst_ref.instance_path, &prefixes[i])
            })
            .collect();

        let mut hint_counts: HashMap<(String, u32), usize> = HashMap::new();
        for (i, number) in hint_numbers.iter().enumerate() {
            let Some(number) = number else {
                continue;
            };
            let prefix = &prefixes[i];
            let is_reserved = used_numbers_by_prefix
                .get(prefix)
                .is_some_and(|used| used.contains(number));
            if is_reserved {
                continue;
            }
            *hint_counts.entry((prefix.clone(), *number)).or_insert(0) += 1;
        }

        for (i, number) in hint_numbers.into_iter().enumerate() {
            if assigned_numbers[i].is_some() {
                continue;
            }
            let Some(number) = number else {
                continue;
            };

            let prefix = prefixes[i].clone();
            let used = used_numbers_by_prefix.entry(prefix.clone()).or_default();
            if used.contains(&number) {
                continue;
            }

            let count = hint_counts
                .get(&(prefix.clone(), number))
                .copied()
                .unwrap_or(0);
            if count == 1 {
                used.insert(number);
                assigned_numbers[i] = Some(number);
            }
        }

        let mut ref_map: HashMap<InstanceRef, String> = HashMap::new();

        let mut next_number_by_prefix: HashMap<String, u32> = HashMap::new();

        for (i, component) in components.into_iter().enumerate() {
            let prefix = prefixes[i].clone();
            let number = assigned_numbers[i].unwrap_or_else(|| {
                let used = used_numbers_by_prefix.entry(prefix.clone()).or_default();
                let next = next_number_by_prefix.entry(prefix.clone()).or_insert(1);
                while used.contains(next) {
                    *next += 1;
                }
                let number = *next;
                used.insert(number);
                *next += 1;
                number
            });

            let refdes = format!("{prefix}{number}");
            component.inst.reference_designator = Some(refdes.clone());
            ref_map.insert(component.inst_ref, refdes);
        }

        ref_map
    }

    pub fn bom(&self) -> bom::Bom {
        bom::Bom::from_schematic(self)
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
    if let Some(AttributeValue::String(t)) = inst.attributes.get("type")
        && let Some(first) = t.chars().next()
    {
        return first.to_ascii_uppercase().to_string();
    }
    // Fallback to "U"
    "U".to_owned()
}

impl Net {
    /// Create a new net with the given kind and name.
    pub fn new(kind: String, name: impl Into<String>, id: u64) -> Self {
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
/// builder.add_net(Net::new("Ground".to_string(), "GND", 0));
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
    use std::collections::BTreeMap;
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
    fn package_uri_supports_workspace_and_versioned_coordinates() {
        let mut roots = BTreeMap::new();
        roots.insert("workspace".to_string(), PathBuf::from("/tmp/workspace"));
        roots.insert(
            "github.com/example/lib@1.2.3".to_string(),
            PathBuf::from("/tmp/lib"),
        );

        let workspace = resolve_package_uri("package://workspace/module/file.kicad_mod", &roots)
            .expect("workspace package URI should resolve");
        assert_eq!(
            workspace,
            PathBuf::from("/tmp/workspace/module/file.kicad_mod")
        );

        let ok = resolve_package_uri(
            "package://github.com/example/lib@1.2.3/file.kicad_mod",
            &roots,
        )
        .expect("versioned package URI should resolve");
        assert_eq!(ok, PathBuf::from("/tmp/lib/file.kicad_mod"));
    }

    #[test]
    fn component_ref_and_pin_for_port_handles_split_dotted_port_segments() {
        let module_ref = ModuleRef::from_path(Path::new("/tmp/test.zen"), "<root>");

        let comp_ref = InstanceRef::new(
            module_ref.clone(),
            vec!["USB_C".into(), "TVS".into(), "TVS".into()],
        );
        let mut component = Instance::component(module_ref.clone());
        component.reference_designator = Some("D1".to_owned());

        // Simulate a dotted port name after lossy string parsing: "NC.2" -> ["NC", "2"].
        let port_ref = InstanceRef::new(
            module_ref.clone(),
            vec![
                "USB_C".into(),
                "TVS".into(),
                "TVS".into(),
                "NC".into(),
                "2".into(),
            ],
        );
        let port = Instance::port(module_ref.clone());

        let mut schematic = Schematic::new();
        schematic.add_instance(comp_ref, component);
        schematic.add_instance(port_ref.clone(), port);

        let owner_ref = schematic
            .component_ref_for_port(&port_ref)
            .expect("owner component should be found");
        assert_eq!(
            owner_ref.instance_path,
            vec!["USB_C".to_string(), "TVS".to_string(), "TVS".to_string()]
        );

        let (owner_ref2, pin_name) = schematic
            .component_ref_and_pin_for_port(&port_ref)
            .expect("owner and pin name should resolve");
        assert_eq!(owner_ref2.instance_path, owner_ref.instance_path);
        assert_eq!(pin_name, "NC.2");
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

    #[test]
    fn assign_refdes_natural_hier_sort() {
        let mut schematic = Schematic::new();
        let mod_ref = ModuleRef::from_path(Path::new("/test.pmod"), "TestModule");

        // If instance names include numeric suffixes, we want natural ordering:
        // `r2` < `r10` (not lexicographic `r10` < `r2`).
        let r2_ref = InstanceRef::new(mod_ref.clone(), vec!["r2".into()]);
        let r2 = Instance::component(mod_ref.clone()).with_attribute("type", "res".to_string());
        schematic.add_instance(r2_ref.clone(), r2);

        let r10_ref = InstanceRef::new(mod_ref.clone(), vec!["r10".into()]);
        let r10 = Instance::component(mod_ref.clone()).with_attribute("type", "res".to_string());
        schematic.add_instance(r10_ref.clone(), r10);

        let ref_map = schematic.assign_reference_designators();

        assert_eq!(ref_map.get(&r2_ref), Some(&"R1".to_string()));
        assert_eq!(ref_map.get(&r10_ref), Some(&"R2".to_string()));
    }

    #[test]
    fn assign_refdes_uses_path_hint() {
        let mut schematic = Schematic::new();
        let mod_ref = ModuleRef::from_path(Path::new("/test.pmod"), "TestModule");

        // Hint is in a non-leaf segment: foo.R22.x
        let a_ref = InstanceRef::new(
            mod_ref.clone(),
            vec!["foo".into(), "R22".into(), "x".into()],
        );
        let a = Instance::component(mod_ref.clone()).with_attribute("type", "res".to_string());
        schematic.add_instance(a_ref.clone(), a);

        let b_ref = InstanceRef::new(mod_ref.clone(), vec!["foo".into(), "R5".into(), "y".into()]);
        let b = Instance::component(mod_ref.clone()).with_attribute("type", "res".to_string());
        schematic.add_instance(b_ref.clone(), b);

        // No hint; should fill the lowest available number (R1).
        let c_ref = InstanceRef::new(mod_ref.clone(), vec!["foo".into(), "z".into()]);
        let c = Instance::component(mod_ref.clone()).with_attribute("type", "res".to_string());
        schematic.add_instance(c_ref.clone(), c);

        let ref_map = schematic.assign_reference_designators();

        assert_eq!(ref_map.get(&a_ref), Some(&"R22".to_string()));
        assert_eq!(ref_map.get(&b_ref), Some(&"R5".to_string()));
        assert_eq!(ref_map.get(&c_ref), Some(&"R1".to_string()));
    }

    #[test]
    fn assign_refdes_ignores_leaf_hint() {
        let mut schematic = Schematic::new();
        let mod_ref = ModuleRef::from_path(Path::new("/test.pmod"), "TestModule");

        // Leaf segment "R22" must not be treated as a hint.
        let a_ref = InstanceRef::new(
            mod_ref.clone(),
            vec!["foo".into(), "x".into(), "R22".into()],
        );
        let a = Instance::component(mod_ref.clone()).with_attribute("type", "res".to_string());
        schematic.add_instance(a_ref.clone(), a);

        let ref_map = schematic.assign_reference_designators();
        assert_eq!(ref_map.get(&a_ref), Some(&"R1".to_string()));
    }

    #[test]
    fn assign_refdes_drops_hint_conflicts() {
        let mut schematic = Schematic::new();
        let mod_ref = ModuleRef::from_path(Path::new("/test.pmod"), "TestModule");

        let a_ref = InstanceRef::new(
            mod_ref.clone(),
            vec!["foo".into(), "R22".into(), "x".into()],
        );
        let a = Instance::component(mod_ref.clone()).with_attribute("type", "res".to_string());
        schematic.add_instance(a_ref.clone(), a);

        let b_ref = InstanceRef::new(
            mod_ref.clone(),
            vec!["bar".into(), "R22".into(), "y".into()],
        );
        let b = Instance::component(mod_ref.clone()).with_attribute("type", "res".to_string());
        schematic.add_instance(b_ref.clone(), b);

        let ref_map = schematic.assign_reference_designators();

        let a = ref_map.get(&a_ref).unwrap().clone();
        let b = ref_map.get(&b_ref).unwrap().clone();
        assert_ne!(a, "R22");
        assert_ne!(b, "R22");
        assert_ne!(a, b);
        assert!(a.starts_with('R'));
        assert!(b.starts_with('R'));
    }

    #[test]
    fn assign_refdes_requires_prefix_match() {
        let mut schematic = Schematic::new();
        let mod_ref = ModuleRef::from_path(Path::new("/test.pmod"), "TestModule");

        // Hint prefix is R, but component prefix is C (from type=cap), so ignore.
        let c_ref = InstanceRef::new(
            mod_ref.clone(),
            vec!["foo".into(), "R22".into(), "cap".into()],
        );
        let c = Instance::component(mod_ref.clone()).with_attribute("type", "cap".to_string());
        schematic.add_instance(c_ref.clone(), c);

        let ref_map = schematic.assign_reference_designators();
        assert_eq!(ref_map.get(&c_ref), Some(&"C1".to_string()));
    }

    #[test]
    fn assign_refdes_unknown_prefix_ignores_hint() {
        let mut schematic = Schematic::new();
        let mod_ref = ModuleRef::from_path(Path::new("/test.pmod"), "TestModule");

        // Unknown prefix "ZZ" should not accept hint ZZ99; allocator should assign ZZ1/ZZ2 instead.
        let a_ref = InstanceRef::new(
            mod_ref.clone(),
            vec!["foo".into(), "ZZ99".into(), "x".into()],
        );
        let a = Instance::component(mod_ref.clone()).with_attribute("prefix", "ZZ".to_string());
        schematic.add_instance(a_ref.clone(), a);

        let b_ref = InstanceRef::new(mod_ref.clone(), vec!["foo".into(), "y".into()]);
        let b = Instance::component(mod_ref.clone()).with_attribute("prefix", "ZZ".to_string());
        schematic.add_instance(b_ref.clone(), b);

        let ref_map = schematic.assign_reference_designators();
        let a = ref_map.get(&a_ref).unwrap();
        let b = ref_map.get(&b_ref).unwrap();
        assert_ne!(a, "ZZ99");
        assert_ne!(b, "ZZ99");
    }

    #[test]
    fn assign_refdes_preserves_existing() {
        let mut schematic = Schematic::new();
        let mod_ref = ModuleRef::from_path(Path::new("/test.pmod"), "TestModule");

        let a_ref = InstanceRef::new(mod_ref.clone(), vec!["a".into()]);
        let a = Instance::component(mod_ref.clone())
            .with_attribute("type", "res".to_string())
            .with_reference_designator("R22");
        schematic.add_instance(a_ref.clone(), a);

        let b_ref = InstanceRef::new(mod_ref.clone(), vec!["b".into()]);
        let b = Instance::component(mod_ref.clone()).with_attribute("type", "res".to_string());
        schematic.add_instance(b_ref.clone(), b);

        let ref_map = schematic.assign_reference_designators();
        assert_eq!(ref_map.get(&a_ref), Some(&"R22".to_string()));
        assert_eq!(ref_map.get(&b_ref), Some(&"R1".to_string()));
    }

    #[test]
    fn assign_refdes_drops_existing_conflicts() {
        let mut schematic = Schematic::new();
        let mod_ref = ModuleRef::from_path(Path::new("/test.pmod"), "TestModule");

        let a_ref = InstanceRef::new(mod_ref.clone(), vec!["a".into()]);
        let a = Instance::component(mod_ref.clone())
            .with_attribute("type", "res".to_string())
            .with_reference_designator("R22");
        schematic.add_instance(a_ref.clone(), a);

        let b_ref = InstanceRef::new(mod_ref.clone(), vec!["b".into()]);
        let b = Instance::component(mod_ref.clone())
            .with_attribute("type", "res".to_string())
            .with_reference_designator("R22");
        schematic.add_instance(b_ref.clone(), b);

        let ref_map = schematic.assign_reference_designators();
        let a = ref_map.get(&a_ref).unwrap().clone();
        let b = ref_map.get(&b_ref).unwrap().clone();
        assert_ne!(a, "R22");
        assert_ne!(b, "R22");
        assert_ne!(a, b);
    }
}
