#![allow(clippy::needless_lifetimes)]

use allocative::Allocative;
use pcb_sch::physical::PhysicalValue;
use starlark::{
    any::ProvidesStaticType,
    collections::SmallMap,
    environment::GlobalsBuilder,
    eval::{Arguments, Evaluator, ParametersSpec, ParametersSpecParam},
    starlark_module, starlark_simple_value,
    values::{
        dict::{AllocDict, DictRef},
        starlark_value, Coerce, Freeze, FrozenValue, Heap, NoSerialize, StarlarkValue, Trace,
        Value, ValueLike,
    },
};
use std::cell::RefCell;
use tracing::info_span;

use crate::{
    lang::{evaluator_ext::EvaluatorExt, spice_model::SpiceModelValue},
    FrozenSpiceModelValue,
};

use super::symbol::{SymbolType, SymbolValue};
use super::validation::validate_identifier_name;

use anyhow::anyhow;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ComponentError {
    #[error("`name` must be a string")]
    NameNotString,
    #[error("`footprint` must be a string")]
    FootprintNotString,
    #[error("could not determine parent directory of current file")]
    ParentDirectoryNotFound,
    #[error("`pins` must be a dict mapping pin names to Net")]
    PinsNotDict,
    #[error("`prefix` must be a string")]
    PrefixNotString,
    #[error("`designator` must be a string")]
    DesignatorNotString,
    #[error("`designator` must be a valid reference designator like 'R1' or 'IC12' (got '{designator}')")]
    InvalidDesignator { designator: String },
    #[error("`pin_defs` must be a dict of name -> pad")]
    PinDefsNotDict,
    #[error("pin name must be a string")]
    PinNameNotString,
    #[error("pad must be a string")]
    PadNotString,
    #[error("Failed to downcast Symbol value")]
    SymbolDowncastFailed,
    #[error("no pin '{pin_name}' in symbol")]
    PinNotInSymbol { pin_name: String },
    #[error("no pad '{pad}' in symbol pin {pin_name}")]
    PadNotInSymbolPin { pad: String, pin_name: String },
    #[error("pin names must be strings")]
    PinNamesNotStrings,
    #[error("pin '{pin_name}' referenced but not defined in `pin_defs`")]
    PinNotInPinDefs { pin_name: String },
    #[error("pin '{pin_name}' defined in `pin_defs` but not connected")]
    PinDefinedButNotConnected { pin_name: String },
}

impl From<ComponentError> for starlark::Error {
    fn from(err: ComponentError) -> Self {
        starlark::Error::new_other(err)
    }
}

// Mutable data stored in ComponentValue (wrapped in RefCell)
#[derive(Clone, Debug, Trace, ProvidesStaticType, Allocative)]
pub struct ComponentData<'v> {
    pub(crate) mpn: Option<String>,
    pub(crate) manufacturer: Option<String>,
    pub(crate) dnp: bool,
    pub(crate) skip_bom: bool,
    pub(crate) skip_pos: bool,
    pub(crate) properties: SmallMap<String, Value<'v>>,
}

// Frozen data stored in FrozenComponentValue (no RefCell needed)
#[derive(Clone, Debug, ProvidesStaticType, Allocative)]
pub struct FrozenComponentData {
    pub(crate) mpn: Option<String>,
    pub(crate) manufacturer: Option<String>,
    pub(crate) dnp: bool,
    pub(crate) skip_bom: bool,
    pub(crate) skip_pos: bool,
    pub(crate) properties: SmallMap<String, FrozenValue>,
}

unsafe impl<'v> Coerce<ComponentData<'v>> for FrozenComponentData {}

// Generic component wrapper - T is either RefCell<ComponentData<'v>> or FrozenComponentData
#[derive(Clone, Trace, ProvidesStaticType, NoSerialize, Allocative)]
#[repr(C)]
pub struct ComponentGen<V, T> {
    name: String,
    ctype: Option<String>,
    footprint: String,
    prefix: String,
    designator: Option<String>,
    #[allocative(skip)]
    designator_span: Option<starlark::codemap::ResolvedSpan>,
    connections: SmallMap<String, V>,
    data: T,
    source_path: String,
    symbol: V,
    spice_model: Option<V>,
    datasheet: Option<String>,
    description: Option<String>,
}

// Type aliases for mutable and frozen versions
pub type ComponentValue<'v> = ComponentGen<Value<'v>, RefCell<ComponentData<'v>>>;
pub type FrozenComponentValue = ComponentGen<FrozenValue, FrozenComponentData>;

// Implement Coerce for ComponentGen
unsafe impl<'v> Coerce<ComponentValue<'v>> for FrozenComponentValue {}

// Freeze implementation
impl<'v> Freeze for ComponentValue<'v> {
    type Frozen = FrozenComponentValue;

    fn freeze(
        self,
        freezer: &starlark::values::Freezer,
    ) -> starlark::values::FreezeResult<Self::Frozen> {
        let data = self.data.into_inner();
        Ok(FrozenComponentValue {
            name: self.name,
            ctype: self.ctype,
            footprint: self.footprint,
            prefix: self.prefix,
            designator: self.designator,
            designator_span: self.designator_span,
            connections: self.connections.freeze(freezer)?,
            data: FrozenComponentData {
                mpn: data.mpn,
                manufacturer: data.manufacturer,
                dnp: data.dnp,
                skip_bom: data.skip_bom,
                skip_pos: data.skip_pos,
                properties: {
                    let mut frozen_props = SmallMap::new();
                    for (k, v) in data.properties.into_iter() {
                        frozen_props.insert(k, v.freeze(freezer)?);
                    }
                    frozen_props
                },
            },
            source_path: self.source_path,
            symbol: self.symbol.freeze(freezer)?,
            spice_model: match self.spice_model {
                Some(s) => Some(s.freeze(freezer)?),
                None => None,
            },
            datasheet: self.datasheet,
            description: self.description,
        })
    }
}

impl std::fmt::Debug for ComponentValue<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = f.debug_struct("Component");
        debug.field("name", &self.name);

        let data = self.data.borrow();
        if let Some(mpn) = &data.mpn {
            debug.field("mpn", mpn);
        }
        if let Some(manufacturer) = &data.manufacturer {
            debug.field("manufacturer", manufacturer);
        }
        if let Some(ctype) = &self.ctype {
            debug.field("type", ctype);
        }

        debug.field("footprint", &self.footprint);
        debug.field("prefix", &self.prefix);
        if let Some(designator) = &self.designator {
            debug.field("designator", designator);
        }

        // Sort connections for deterministic output
        if !self.connections.is_empty() {
            let mut conns: Vec<_> = self.connections.iter().collect();
            conns.sort_by_key(|(k, _)| k.as_str());
            let conns_map: std::collections::BTreeMap<_, _> =
                conns.into_iter().map(|(k, v)| (k.as_str(), v)).collect();
            debug.field("connections", &conns_map);
        }

        // Sort properties for deterministic output
        if !data.properties.is_empty() {
            let mut props: Vec<_> = data.properties.iter().collect();
            props.sort_by_key(|(k, _)| k.as_str());
            let props_map: std::collections::BTreeMap<_, _> =
                props.into_iter().map(|(k, v)| (k.as_str(), v)).collect();
            debug.field("properties", &props_map);
        }

        // Show symbol field
        debug.field("symbol", &self.symbol);

        // Show spice_model if present
        if let Some(spice_model) = &self.spice_model {
            debug.field("spice_model", spice_model);
        }

        debug.finish()
    }
}

impl std::fmt::Debug for FrozenComponentValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = f.debug_struct("Component");
        debug.field("name", &self.name);

        if let Some(mpn) = &self.data.mpn {
            debug.field("mpn", mpn);
        }
        if let Some(manufacturer) = &self.data.manufacturer {
            debug.field("manufacturer", manufacturer);
        }
        if let Some(ctype) = &self.ctype {
            debug.field("type", ctype);
        }

        debug.field("footprint", &self.footprint);
        debug.field("prefix", &self.prefix);
        if let Some(designator) = &self.designator {
            debug.field("designator", designator);
        }

        // Sort connections for deterministic output
        if !self.connections.is_empty() {
            let mut conns: Vec<_> = self.connections.iter().collect();
            conns.sort_by_key(|(k, _)| k.as_str());
            let conns_map: std::collections::BTreeMap<_, _> =
                conns.into_iter().map(|(k, v)| (k.as_str(), v)).collect();
            debug.field("connections", &conns_map);
        }

        // Sort properties for deterministic output
        if !self.data.properties.is_empty() {
            let mut props: Vec<_> = self.data.properties.iter().collect();
            props.sort_by_key(|(k, _)| k.as_str());
            let props_map: std::collections::BTreeMap<_, _> =
                props.into_iter().map(|(k, v)| (k.as_str(), v)).collect();
            debug.field("properties", &props_map);
        }

        // Show symbol field
        debug.field("symbol", &self.symbol);

        // Show spice_model if present
        if let Some(spice_model) = &self.spice_model {
            debug.field("spice_model", spice_model);
        }

        debug.finish()
    }
}

fn capitalize_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}

fn validate_reference_designator(designator: &str) -> Result<(), ComponentError> {
    let trimmed = designator.trim();
    if trimmed.is_empty() || trimmed != designator {
        return Err(ComponentError::InvalidDesignator {
            designator: designator.to_owned(),
        });
    }

    let mut chars = designator.chars().peekable();
    let mut saw_letter = false;
    while let Some(&c) = chars.peek() {
        if c.is_ascii_uppercase() {
            saw_letter = true;
            chars.next();
        } else {
            break;
        }
    }
    if !saw_letter {
        return Err(ComponentError::InvalidDesignator {
            designator: designator.to_owned(),
        });
    }

    let mut saw_digit = false;
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            saw_digit = true;
            chars.next();
        } else {
            break;
        }
    }
    if !saw_digit || chars.peek().is_some() {
        return Err(ComponentError::InvalidDesignator {
            designator: designator.to_owned(),
        });
    }

    Ok(())
}

fn designator_prefix(designator: &str) -> Option<String> {
    let prefix: String = designator
        .chars()
        .take_while(|c| c.is_ascii_uppercase())
        .collect();
    if prefix.is_empty() {
        None
    } else {
        Some(prefix)
    }
}

/// Helper to consolidate boolean properties from kwargs and legacy property names.
/// Handles both boolean values and string representations ("true", "1", etc.)
fn consolidate_bool_property<'v>(
    kwarg_val: Option<Value<'v>>,
    properties_map: &SmallMap<String, Value<'v>>,
    legacy_keys: &[&str],
) -> Option<bool> {
    kwarg_val.and_then(|v| v.unpack_bool()).or_else(|| {
        legacy_keys.iter().find_map(|&key| {
            properties_map.get(key).and_then(|v| {
                // Try boolean first, then check if it's a string "true"/"false" or "1"/"0"
                v.unpack_bool().or_else(|| {
                    v.unpack_str().and_then(|s| match s {
                        "true" | "1" => Some(true),
                        "false" | "0" => Some(false),
                        _ => {
                            let lower = s.to_lowercase();
                            if lower == "true" {
                                Some(true)
                            } else if lower == "false" {
                                Some(false)
                            } else {
                                None
                            }
                        }
                    })
                })
            })
        })
    })
}

// StarlarkValue implementation for mutable ComponentValue
#[starlark_value(type = "Component")]
impl<'v> StarlarkValue<'v> for ComponentValue<'v> {
    fn get_attr(&self, attr: &str, heap: &'v Heap) -> Option<Value<'v>> {
        let data = self.data.borrow();
        match attr {
            "name" => Some(heap.alloc_str(&self.name).to_value()),
            "prefix" => Some(heap.alloc_str(&self.prefix).to_value()),
            "designator" => Some(
                self.designator
                    .as_ref()
                    .map(|d| heap.alloc_str(d).to_value())
                    .unwrap_or_else(Value::new_none),
            ),
            "mpn" => Some(
                data.mpn
                    .as_ref()
                    .map(|mpn| heap.alloc_str(mpn).to_value())
                    .unwrap_or_else(Value::new_none),
            ),
            "manufacturer" => Some(
                data.manufacturer
                    .as_ref()
                    .map(|m| heap.alloc_str(m).to_value())
                    .unwrap_or_else(Value::new_none),
            ),
            "dnp" => Some(heap.alloc(data.dnp).to_value()),
            "skip_bom" => Some(heap.alloc(data.skip_bom).to_value()),
            "skip_pos" => Some(heap.alloc(data.skip_pos).to_value()),
            "type" => Some(
                self.ctype
                    .as_ref()
                    .map(|ctype| heap.alloc_str(ctype).to_value())
                    .unwrap_or_else(Value::new_none),
            ),
            "properties" => {
                // Build the same properties dictionary as in the testbench components dict
                let mut component_attrs = std::collections::HashMap::new();

                // Add component properties (excluding internal ones)
                for (key, value) in data.properties.iter() {
                    if matches!(key.as_str(), "footprint" | "symbol_path" | "symbol_name")
                        || key.starts_with("__")
                    {
                        continue;
                    }
                    component_attrs.insert(key.clone(), value.to_value());
                }

                // Convert HashMap to Starlark dictionary
                let attrs_vec: Vec<(Value<'v>, Value<'v>)> = component_attrs
                    .into_iter()
                    .map(|(key, value)| (heap.alloc_str(&key).to_value(), value))
                    .collect();

                Some(heap.alloc(AllocDict(attrs_vec)))
            }
            "pins" => {
                // Convert connections SmallMap to Starlark dictionary
                let connections_vec: Vec<(Value<'v>, Value<'v>)> = self
                    .connections
                    .iter()
                    .map(|(pin, net)| (heap.alloc_str(pin).to_value(), net.to_value()))
                    .collect();
                Some(heap.alloc(AllocDict(connections_vec)))
            }
            // Fallback: check properties map
            _ => {
                // We have to check both the original and capitalized keys
                // because config_properties does automatic case conversion
                // TODO: drop this when config_properties no longer does case conversion
                let keys = [attr.to_string(), capitalize_first(attr)];
                keys.iter()
                    .find_map(|key| data.properties.get(key))
                    .map(|v| {
                        // For capacitance/resistance, attempt to convert string to PhysicalValue
                        let is_special = matches!(
                            attr,
                            "capacitance" | "Capacitance" | "resistance" | "Resistance"
                        );
                        if is_special {
                            if let Some(s) = v.unpack_str() {
                                if let Ok(pv) = s.parse::<PhysicalValue>() {
                                    return heap.alloc(pv);
                                }
                            }
                        }
                        v.to_value()
                    })
            }
        }
    }

    fn set_attr(&self, attr: &str, value: Value<'v>) -> starlark::Result<()> {
        let mut data = self.data.borrow_mut();
        match attr {
            "mpn" => {
                data.mpn = value.unpack_str().map(|s| s.to_owned());
                Ok(())
            }
            "manufacturer" => {
                data.manufacturer = value.unpack_str().map(|s| s.to_owned());
                Ok(())
            }
            "dnp" => {
                data.dnp = value.unpack_bool().unwrap_or(false);
                Ok(())
            }
            "skip_bom" => {
                data.skip_bom = value.unpack_bool().unwrap_or(false);
                Ok(())
            }
            "skip_pos" => {
                data.skip_pos = value.unpack_bool().unwrap_or(false);
                Ok(())
            }
            // Fallback: set in properties map (always allowed)
            _ => {
                data.properties.insert(attr.to_string(), value);
                Ok(())
            }
        }
    }

    fn has_attr(&self, attr: &str, _heap: &'v Heap) -> bool {
        if matches!(
            attr,
            "name"
                | "prefix"
                | "designator"
                | "mpn"
                | "manufacturer"
                | "dnp"
                | "skip_bom"
                | "skip_pos"
                | "type"
                | "properties"
                | "pins"
        ) {
            return true;
        }
        let data = self.data.borrow();
        data.properties.contains_key(attr) || data.properties.contains_key(&capitalize_first(attr))
    }

    fn dir_attr(&self) -> Vec<String> {
        let mut attrs = vec![
            "name".to_string(),
            "prefix".to_string(),
            "designator".to_string(),
            "mpn".to_string(),
            "manufacturer".to_string(),
            "dnp".to_string(),
            "skip_bom".to_string(),
            "skip_pos".to_string(),
            "type".to_string(),
            "properties".to_string(),
            "pins".to_string(),
        ];
        let data = self.data.borrow();
        for key in data.properties.keys() {
            if !key.starts_with("__") {
                attrs.push(key.clone());
            }
        }
        attrs
    }
}

// StarlarkValue implementation for frozen FrozenComponentValue
#[starlark_value(type = "Component")]
impl<'v> StarlarkValue<'v> for FrozenComponentValue {
    type Canonical = FrozenComponentValue;

    fn get_attr(&self, attr: &str, heap: &'v Heap) -> Option<Value<'v>> {
        match attr {
            "name" => Some(heap.alloc_str(&self.name).to_value()),
            "prefix" => Some(heap.alloc_str(&self.prefix).to_value()),
            "designator" => Some(
                self.designator
                    .as_ref()
                    .map(|d| heap.alloc_str(d).to_value())
                    .unwrap_or_else(Value::new_none),
            ),
            "mpn" => Some(
                self.data
                    .mpn
                    .as_ref()
                    .map(|mpn| heap.alloc_str(mpn).to_value())
                    .unwrap_or_else(Value::new_none),
            ),
            "manufacturer" => Some(
                self.data
                    .manufacturer
                    .as_ref()
                    .map(|m| heap.alloc_str(m).to_value())
                    .unwrap_or_else(Value::new_none),
            ),
            "dnp" => Some(heap.alloc(self.data.dnp).to_value()),
            "skip_bom" => Some(heap.alloc(self.data.skip_bom).to_value()),
            "skip_pos" => Some(heap.alloc(self.data.skip_pos).to_value()),
            "type" => Some(
                self.ctype
                    .as_ref()
                    .map(|ctype| heap.alloc_str(ctype).to_value())
                    .unwrap_or_else(Value::new_none),
            ),
            "properties" => {
                // Build the same properties dictionary as in the testbench components dict
                let mut component_attrs = std::collections::HashMap::new();

                // Add component properties (excluding internal ones)
                for (key, value) in self.data.properties.iter() {
                    if matches!(key.as_str(), "footprint" | "symbol_path" | "symbol_name")
                        || key.starts_with("__")
                    {
                        continue;
                    }
                    component_attrs.insert(key.clone(), value.to_value());
                }

                // Convert HashMap to Starlark dictionary
                let attrs_vec: Vec<(Value<'v>, Value<'v>)> = component_attrs
                    .into_iter()
                    .map(|(key, value)| (heap.alloc_str(&key).to_value(), value))
                    .collect();

                Some(heap.alloc(AllocDict(attrs_vec)))
            }
            "pins" => {
                // Convert connections SmallMap to Starlark dictionary
                let connections_vec: Vec<(Value<'v>, Value<'v>)> = self
                    .connections
                    .iter()
                    .map(|(pin, net)| (heap.alloc_str(pin).to_value(), net.to_value()))
                    .collect();
                Some(heap.alloc(AllocDict(connections_vec)))
            }
            _ => {
                // We have to check both the original and capitalized keys
                // because config_properties does automatic case conversion
                // TODO: drop this when config_properties no longer does case conversion
                let keys = [attr.to_string(), capitalize_first(attr)];
                keys.iter()
                    .find_map(|key| self.data.properties.get(key))
                    .map(|v| {
                        // For capacitance/resistance, attempt to convert string to PhysicalValue
                        let is_special = matches!(
                            attr,
                            "capacitance" | "Capacitance" | "resistance" | "Resistance"
                        );
                        if is_special {
                            if let Some(s) = v.to_value().unpack_str() {
                                if let Ok(pv) = s.parse::<PhysicalValue>() {
                                    return heap.alloc(pv);
                                }
                            }
                        }
                        v.to_value()
                    })
            }
        }
    }

    fn has_attr(&self, attr: &str, _heap: &'v Heap) -> bool {
        if matches!(
            attr,
            "name"
                | "prefix"
                | "designator"
                | "mpn"
                | "manufacturer"
                | "dnp"
                | "skip_bom"
                | "skip_pos"
                | "type"
                | "properties"
                | "pins"
        ) {
            return true;
        }
        self.data.properties.contains_key(attr)
            || self.data.properties.contains_key(&capitalize_first(attr))
    }

    fn dir_attr(&self) -> Vec<String> {
        let mut attrs = vec![
            "name".to_string(),
            "prefix".to_string(),
            "designator".to_string(),
            "mpn".to_string(),
            "manufacturer".to_string(),
            "dnp".to_string(),
            "skip_bom".to_string(),
            "skip_pos".to_string(),
            "type".to_string(),
            "properties".to_string(),
            "pins".to_string(),
        ];
        for key in self.data.properties.keys() {
            if !key.starts_with("__") {
                attrs.push(key.clone());
            }
        }
        attrs
    }
}

impl std::fmt::Display for ComponentValue<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let data = self.data.borrow();
        let name = data
            .mpn
            .as_deref()
            .unwrap_or(self.ctype.as_deref().unwrap_or("<unknown>"));
        writeln!(f, "Component({name})")?;

        if !data.properties.is_empty() {
            let mut props: Vec<_> = data.properties.iter().collect();
            props.sort_by(|(a, _), (b, _)| a.cmp(b));
            writeln!(f, "Properties:")?;
            for (key, value) in props {
                writeln!(f, "  {key}: {value:?}")?;
            }
        }
        Ok(())
    }
}

impl std::fmt::Display for FrozenComponentValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = self
            .data
            .mpn
            .as_deref()
            .unwrap_or(self.ctype.as_deref().unwrap_or("<unknown>"));
        writeln!(f, "Component({name})")?;

        if !self.data.properties.is_empty() {
            let mut props: Vec<_> = self.data.properties.iter().collect();
            props.sort_by(|(a, _), (b, _)| a.cmp(b));
            writeln!(f, "Properties:")?;
            for (key, value) in props {
                writeln!(f, "  {key}: {value:?}")?;
            }
        }
        Ok(())
    }
}

// Accessor methods for ComponentValue
impl<'v> ComponentValue<'v> {
    pub fn mpn(&self) -> Option<String> {
        self.data.borrow().mpn.clone()
    }

    pub fn manufacturer(&self) -> Option<String> {
        self.data.borrow().manufacturer.clone()
    }

    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    pub fn designator(&self) -> Option<&str> {
        self.designator.as_deref()
    }

    /// Optional component *type* as declared via the `type = "..."` field when
    /// the factory was defined.  Used by schematic viewers to pick an
    /// appropriate symbol when the MPN is not available.
    pub fn ctype(&self) -> Option<&str> {
        self.ctype.as_deref()
    }

    pub fn dnp(&self) -> bool {
        self.data.borrow().dnp
    }

    pub fn skip_bom(&self) -> bool {
        self.data.borrow().skip_bom
    }

    pub fn skip_pos(&self) -> bool {
        self.data.borrow().skip_pos
    }

    pub fn datasheet(&self) -> Option<&str> {
        self.datasheet.as_deref()
    }

    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    pub fn footprint(&self) -> &str {
        &self.footprint
    }

    pub fn properties(&self) -> SmallMap<String, Value<'v>> {
        self.data.borrow().properties.clone()
    }

    pub fn connections(&self) -> &SmallMap<String, Value<'v>> {
        &self.connections
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn source_path(&self) -> &str {
        &self.source_path
    }

    pub fn symbol(&self) -> &Value<'v> {
        &self.symbol
    }

    pub fn spice_model(&self) -> Option<&Value<'v>> {
        self.spice_model.as_ref()
    }
}

// Accessor methods for FrozenComponentValue
impl FrozenComponentValue {
    pub fn mpn(&self) -> Option<&str> {
        self.data.mpn.as_deref()
    }

    pub fn manufacturer(&self) -> Option<&str> {
        self.data.manufacturer.as_deref()
    }

    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    pub fn designator(&self) -> Option<&str> {
        self.designator.as_deref()
    }

    pub fn designator_span(&self) -> Option<&starlark::codemap::ResolvedSpan> {
        self.designator_span.as_ref()
    }

    /// Optional component *type* as declared via the `type = "..."` field when
    /// the factory was defined.  Used by schematic viewers to pick an
    /// appropriate symbol when the MPN is not available.
    pub fn ctype(&self) -> Option<&str> {
        self.ctype.as_deref()
    }

    pub fn dnp(&self) -> bool {
        self.data.dnp
    }

    pub fn skip_bom(&self) -> bool {
        self.data.skip_bom
    }

    pub fn skip_pos(&self) -> bool {
        self.data.skip_pos
    }

    pub fn datasheet(&self) -> Option<&str> {
        self.datasheet.as_deref()
    }

    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    pub fn footprint(&self) -> &str {
        &self.footprint
    }

    pub fn properties(&self) -> &SmallMap<String, FrozenValue> {
        &self.data.properties
    }

    pub fn connections(&self) -> &SmallMap<String, FrozenValue> {
        &self.connections
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn source_path(&self) -> &str {
        &self.source_path
    }

    pub fn symbol(&self) -> &FrozenValue {
        &self.symbol
    }

    pub fn spice_model(&self) -> Option<&FrozenValue> {
        self.spice_model.as_ref()
    }
}

/// ComponentFactory is a value that represents a factory for a component.
#[derive(Debug, Trace, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub struct ComponentType;

starlark_simple_value!(ComponentType);

#[starlark_value(type = "Component")]
impl<'v> StarlarkValue<'v> for ComponentType
where
    Self: ProvidesStaticType<'v>,
{
    fn invoke(
        &self,
        _me: Value<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        // Check if parent module has dnp=True in properties
        let module_has_dnp = eval
            .module_value()
            .and_then(|m| m.properties().get("dnp")?.unpack_bool())
            .unwrap_or(false);

        let param_spec = ParametersSpec::new_named_only(
            "Component",
            [
                ("name", ParametersSpecParam::<Value<'_>>::Required),
                ("footprint", ParametersSpecParam::<Value<'_>>::Required),
                ("pin_defs", ParametersSpecParam::<Value<'_>>::Optional),
                ("pins", ParametersSpecParam::<Value<'_>>::Required),
                ("prefix", ParametersSpecParam::<Value<'_>>::Optional),
                ("designator", ParametersSpecParam::<Value<'_>>::Optional),
                ("symbol", ParametersSpecParam::<Value<'_>>::Optional),
                ("mpn", ParametersSpecParam::<Value<'_>>::Optional),
                ("manufacturer", ParametersSpecParam::<Value<'_>>::Optional),
                ("type", ParametersSpecParam::<Value<'_>>::Optional),
                ("properties", ParametersSpecParam::<Value<'_>>::Optional),
                ("spice_model", ParametersSpecParam::<Value<'_>>::Optional),
                ("dnp", ParametersSpecParam::<Value<'_>>::Optional),
                ("skip_bom", ParametersSpecParam::<Value<'_>>::Optional),
                ("skip_pos", ParametersSpecParam::<Value<'_>>::Optional),
                ("datasheet", ParametersSpecParam::<Value<'_>>::Optional),
                ("description", ParametersSpecParam::<Value<'_>>::Optional),
            ],
        );

        let component_val = param_spec.parser(args, eval, |param_parser, eval_ctx| {
            let name_val: Value = param_parser.next()?;
            let name = name_val
                .unpack_str()
                .ok_or(ComponentError::NameNotString)?
                .to_owned();

            let _span = info_span!("component", name = %name).entered();

            // Validate the component name
            validate_identifier_name(&name, "Component name")?;

            let footprint_val: Value = param_parser.next()?;
            let mut footprint = footprint_val
                .unpack_str()
                .ok_or(ComponentError::FootprintNotString)?
                .to_owned();

            // If the footprint looks like a KiCad module file, make the path absolute
            if footprint.ends_with(".kicad_mod") {
                let candidate = std::path::PathBuf::from(&footprint);
                if !candidate.is_absolute() {
                    let current_path = eval_ctx.source_path().unwrap_or_default();

                    let current_dir = std::path::Path::new(&current_path)
                        .parent()
                        .ok_or(ComponentError::ParentDirectoryNotFound)?;

                    footprint = current_dir.join(candidate).to_string_lossy().into_owned();
                }
            }

            let pin_defs_val: Option<Value> = param_parser.next_opt()?;

            let pins_val: Value = param_parser.next()?;
            let conn_dict = DictRef::from_value(pins_val).ok_or(ComponentError::PinsNotDict)?;

            let prefix_val: Option<Value> = param_parser.next_opt()?;
            let prefix = prefix_val.and_then(|v| v.unpack_str().map(|s| s.to_owned()));

            let designator_val: Option<Value> = param_parser.next_opt()?;
            let designator = designator_val
                .filter(|v| !v.is_none())
                .map(|v| -> Result<String, ComponentError> {
                    let s = v
                        .unpack_str()
                        .ok_or(ComponentError::DesignatorNotString)?
                        .to_owned();
                    validate_reference_designator(&s)?;
                    Ok(s)
                })
                .transpose()?;

            // Optional fields
            let symbol_val: Option<Value> = param_parser.next_opt()?;
            let mpn: Option<Value> = param_parser.next_opt()?;
            let manufacturer: Option<Value> = param_parser.next_opt()?;
            let ctype: Option<Value> = param_parser.next_opt()?;
            let properties_val: Value = param_parser.next_opt()?.unwrap_or_default();
            let spice_model_val: Option<Value> = param_parser.next_opt()?;
            let dnp_val: Option<Value> = param_parser.next_opt()?;
            let skip_bom_val: Option<Value> = param_parser.next_opt()?;
            let skip_pos_val: Option<Value> = param_parser.next_opt()?;
            let datasheet_val: Option<Value> = param_parser.next_opt()?;
            let description_val: Option<Value> = param_parser.next_opt()?;

            // Get a SymbolValue from the pin_defs or symbol_val
            let final_symbol: SymbolValue = if let Some(pin_defs) = pin_defs_val {
                // Old way: pin_defs provided as a dict
                let dict_ref = DictRef::from_value(pin_defs).ok_or_else(|| {
                    starlark::Error::new_other(anyhow!("`pin_defs` must be a dict of name -> pad"))
                })?;

                let mut pad_to_signal: SmallMap<String, String> = SmallMap::new();
                for (k_val, v_val) in dict_ref.iter() {
                    let pin_name = k_val
                        .unpack_str()
                        .ok_or_else(|| {
                            starlark::Error::new_other(anyhow!("pin name must be a string"))
                        })?
                        .to_owned();
                    let pad_name = v_val
                        .unpack_str()
                        .ok_or_else(|| starlark::Error::new_other(anyhow!("pad must be a string")))?
                        .to_owned();
                    pad_to_signal.insert(pad_name, pin_name);
                }

                // Check if symbol is also provided - if so, merge the information
                if let Some(symbol) = &symbol_val {
                    if symbol.get_type() == "Symbol" {
                        // Extract the Symbol value
                        let symbol_value =
                            symbol.downcast_ref::<SymbolValue>().ok_or_else(|| {
                                starlark::Error::new_other(anyhow!(
                                    "Failed to downcast Symbol value"
                                ))
                            })?;

                        // Create a new symbol that combines the symbol's metadata with pin_defs overrides
                        SymbolValue {
                            name: symbol_value.name.clone(),
                            pad_to_signal, // Use pin mappings from pin_defs
                            source_path: symbol_value.source_path.clone(),
                            raw_sexp: symbol_value.raw_sexp.clone(),
                            properties: symbol_value.properties.clone(),
                        }
                    } else {
                        // symbol is not a Symbol type, just use pin_defs
                        SymbolValue {
                            name: None,
                            pad_to_signal,
                            source_path: None,
                            raw_sexp: None,
                            properties: SmallMap::new(),
                        }
                    }
                } else {
                    // No symbol provided, create minimal SymbolValue from pin_defs
                    SymbolValue {
                        name: None,
                        pad_to_signal,
                        source_path: None,
                        raw_sexp: None,
                        properties: SmallMap::new(),
                    }
                }
            } else if let Some(symbol) = &symbol_val {
                // New way: symbol provided as a Symbol value
                if symbol.get_type() == "Symbol" {
                    // Extract pins from the Symbol value
                    let symbol_value = symbol.downcast_ref::<SymbolValue>().ok_or_else(|| {
                        starlark::Error::new_other(anyhow!("Failed to downcast Symbol value"))
                    })?;

                    // Return the existing symbol
                    symbol_value.clone()
                } else {
                    return Err(starlark::Error::new_other(anyhow!(
                        "Use Symbol(library = \"...\") to load a symbol from a library."
                    )));
                }
            } else {
                return Err(starlark::Error::new_other(anyhow!(
                    "Either `pin_defs` or a Symbol value for `symbol` must be provided"
                )));
            };

            // Now handle connections after we have pins_str_map
            let mut connections: SmallMap<String, Value<'v>> = SmallMap::new();
            for (k_val, v_val) in conn_dict.iter() {
                let signal_name = k_val
                    .unpack_str()
                    .ok_or_else(|| {
                        starlark::Error::new_other(anyhow!("pin names must be strings"))
                    })?
                    .to_owned();

                if !final_symbol.signal_names().any(|n| n == signal_name) {
                    return Err(starlark::Error::new_other(anyhow!(format!(
                        "Unknown pin name '{}' (expected one of: {})",
                        signal_name,
                        final_symbol.signal_names().collect::<Vec<_>>().join(", ")
                    ))));
                }

                if v_val.get_type() != "Net" {
                    return Err(starlark::Error::new_other(anyhow!(format!(
                        "Pin '{}' must be connected to a Net, got {}",
                        signal_name,
                        v_val.get_type()
                    ))));
                }

                connections.insert(signal_name, v_val);
            }

            // Detect missing pins in connections
            let mut missing_pins: Vec<&str> = final_symbol
                .signal_names()
                .filter(|n| !connections.contains_key(*n))
                .collect();

            missing_pins.sort();
            if !missing_pins.is_empty() {
                return Err(starlark::Error::new_other(anyhow!(format!(
                    "Unconnected pin(s): {}",
                    missing_pins.join(", ")
                ))));
            }

            // Properties map
            let mut properties_map: SmallMap<String, Value<'v>> = SmallMap::new();
            if !properties_val.is_none() {
                if let Some(dict_ref) = DictRef::from_value(properties_val) {
                    for (k_val, v_val) in dict_ref.iter() {
                        let key_str = k_val
                            .unpack_str()
                            .map(|s| s.to_owned())
                            .unwrap_or_else(|| k_val.to_string());
                        properties_map.insert(key_str, v_val);
                    }
                } else {
                    return Err(starlark::Error::new_other(anyhow!(
                        "`properties` must be a dict when provided"
                    )));
                }
            }

            // Store the symbol path in properties if the symbol has one
            if let Some(path) = final_symbol.source_path() {
                properties_map.insert(
                    "symbol_path".to_string(),
                    eval_ctx.heap().alloc_str(path).to_value(),
                );
            }

            if let Some(name) = final_symbol.name() {
                properties_map.insert(
                    "symbol_name".to_string(),
                    eval_ctx.heap().alloc_str(name).to_value(),
                );
            }

            if let Some(ref sm) = spice_model_val {
                if sm.downcast_ref::<SpiceModelValue>().is_none()
                    && sm.downcast_ref::<FrozenSpiceModelValue>().is_none()
                {
                    return Err(starlark::Error::new_other(anyhow!(format!(
                        "`spice_model` must be a SpiceModel, got {}",
                        sm.get_type()
                    ))));
                }
            }

            // If mpn is not explicitly provided, try to get it from properties, then symbol properties
            let final_mpn = mpn
                .and_then(|v| v.unpack_str().map(|s| s.to_owned()))
                .or_else(|| {
                    properties_map
                        .get("mpn")
                        .and_then(|v| v.unpack_str().map(|s| s.to_owned()))
                })
                .or_else(|| {
                    properties_map
                        .get("Mpn")
                        .and_then(|v| v.unpack_str().map(|s| s.to_owned()))
                })
                .or_else(|| {
                    final_symbol
                        .properties()
                        .get("Manufacturer_Part_Number")
                        .map(|s| s.to_owned())
                });

            // If manufacturer is not explicitly provided, try to get it from properties, then symbol properties
            let final_manufacturer = manufacturer
                .and_then(|v| v.unpack_str().map(|s| s.to_owned()))
                .or_else(|| {
                    properties_map
                        .get("manufacturer")
                        .and_then(|v| v.unpack_str().map(|s| s.to_owned()))
                })
                .or_else(|| {
                    final_symbol
                        .properties()
                        .get("Manufacturer_Name")
                        .map(|s| s.to_owned())
                });

            // Warn if manufacturer is set but mpn is missing
            if final_manufacturer.is_some() && final_mpn.is_none() {
                if let Some(call_site) = eval_ctx.call_stack_top_location() {
                    use crate::lang::error::CategorizedDiagnostic;
                    use crate::Diagnostic;
                    use starlark::errors::EvalSeverity;

                    let body = "MPN must be specified if manufacturer is specified";
                    let kind = "bom.incomplete_manufacturer";

                    let source_error =
                        CategorizedDiagnostic::new(body.to_string(), kind.to_string())
                            .ok()
                            .map(|c| std::sync::Arc::new(anyhow::Error::new(c)));

                    let diag = Diagnostic {
                        path: call_site.filename().to_string(),
                        span: Some(call_site.resolve_span()),
                        severity: EvalSeverity::Warning,
                        body: body.to_string(),
                        call_stack: None,
                        child: None,
                        source_error,
                        suppressed: false,
                    };
                    eval_ctx.add_diagnostic(diag);
                }
            }

            // If datasheet is not explicitly provided, try to get it from properties, then symbol properties
            // Skip empty strings and "~" (KiCad's placeholder for no datasheet) - prefer None over empty
            let final_datasheet = datasheet_val
                .and_then(|v| v.unpack_str().map(|s| s.to_owned()))
                .or_else(|| {
                    properties_map
                        .get("datasheet")
                        .and_then(|v| v.unpack_str().map(|s| s.to_owned()))
                })
                .or_else(|| {
                    final_symbol
                        .properties()
                        .get("Datasheet")
                        .filter(|s| !s.is_empty() && s.as_str() != "~")
                        .map(|s| s.to_owned())
                });

            // If description is not explicitly provided, try to get it from properties, then symbol properties
            // Skip empty strings - prefer None over empty
            let final_description = description_val
                .and_then(|v| v.unpack_str().map(|s| s.to_owned()))
                .or_else(|| {
                    properties_map
                        .get("description")
                        .and_then(|v| v.unpack_str().map(|s| s.to_owned()))
                })
                .or_else(|| {
                    properties_map
                        .get("Description")
                        .and_then(|v| v.unpack_str().map(|s| s.to_owned()))
                })
                .or_else(|| {
                    properties_map
                        .get("value")
                        .and_then(|v| v.unpack_str().map(|s| s.to_owned()))
                })
                .or_else(|| {
                    properties_map
                        .get("Value")
                        .and_then(|v| v.unpack_str().map(|s| s.to_owned()))
                })
                .or_else(|| {
                    final_symbol
                        .properties()
                        .get("Description")
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_owned())
                });

            // Consolidate DNP: module dnp (highest priority), then kwarg, then component properties
            let final_dnp = if module_has_dnp {
                Some(true)
            } else {
                consolidate_bool_property(
                    dnp_val,
                    &properties_map,
                    &["do_not_populate", "Do_not_populate", "DNP", "dnp"],
                )
            };

            // Consolidate skip_bom: check kwarg, then legacy properties
            let final_skip_bom = consolidate_bool_property(
                skip_bom_val,
                &properties_map,
                &["Exclude_from_bom", "exclude_from_bom"],
            );

            // Consolidate skip_pos: check kwarg, then legacy properties
            let final_skip_pos = consolidate_bool_property(
                skip_pos_val,
                &properties_map,
                &["Exclude_from_pos_files", "exclude_from_pos_files"],
            );

            // If prefix is not explicitly provided, try to get it from the symbol's Reference property
            let final_prefix = prefix
                .or_else(|| designator.as_ref().and_then(|d| designator_prefix(d)))
                .or_else(|| {
                    final_symbol
                        .properties()
                        .get("Reference")
                        .map(|s| s.to_owned())
                })
                .unwrap_or_else(|| "U".to_owned());

            // Consolidate ctype: check kwarg, then legacy properties (type, Type)
            let final_ctype = ctype
                .and_then(|v| v.unpack_str().map(|s| s.to_owned()))
                .or_else(|| {
                    properties_map
                        .get("type")
                        .and_then(|v| v.unpack_str().map(|s| s.to_owned()))
                })
                .or_else(|| {
                    properties_map
                        .get("Type")
                        .and_then(|v| v.unpack_str().map(|s| s.to_owned()))
                });

            // Remove typed fields from properties map to avoid duplication
            properties_map.shift_remove("mpn");
            properties_map.shift_remove("Mpn");
            properties_map.shift_remove("manufacturer");
            properties_map.shift_remove("datasheet");
            properties_map.shift_remove("description");
            properties_map.shift_remove("type");
            properties_map.shift_remove("Type");
            // Remove DNP legacy keys
            properties_map.shift_remove("do_not_populate");
            properties_map.shift_remove("Do_not_populate");
            properties_map.shift_remove("DNP");
            properties_map.shift_remove("dnp");
            // Remove skip_bom legacy keys
            properties_map.shift_remove("Exclude_from_bom");
            properties_map.shift_remove("exclude_from_bom");
            // Remove skip_pos legacy keys
            properties_map.shift_remove("Exclude_from_pos_files");
            properties_map.shift_remove("exclude_from_pos_files");

            let component = eval_ctx.heap().alloc_complex(ComponentValue {
                name,
                ctype: final_ctype,
                footprint,
                prefix: final_prefix,
                designator: designator.clone(),
                designator_span: designator.as_ref().and_then(|_| {
                    eval_ctx
                        .call_stack_top_location()
                        .map(|site| site.resolve_span())
                }),
                connections,
                data: RefCell::new(ComponentData {
                    mpn: final_mpn,
                    manufacturer: final_manufacturer,
                    dnp: final_dnp.unwrap_or(false),
                    skip_bom: final_skip_bom.unwrap_or(false),
                    skip_pos: final_skip_pos.unwrap_or(false),
                    properties: properties_map,
                }),
                source_path: eval_ctx.source_path().unwrap_or_default(),
                symbol: eval_ctx.heap().alloc_complex(final_symbol),
                spice_model: spice_model_val,
                datasheet: final_datasheet,
                description: final_description,
            });

            Ok(component)
        })?;

        // Add to current module context if available
        // Note: Component modifiers are applied later, after module evaluation but before freezing
        if let Some(context) = eval.context_value() {
            let comp_name = component_val
                .downcast_ref::<ComponentValue>()
                .map(|c| c.name());
            let call_site = eval.call_stack_top_location();
            context.add_child(comp_name, component_val, call_site.as_ref());
        }

        Ok(Value::new_none())
    }

    fn eval_type(&self) -> Option<starlark::typing::Ty> {
        Some(<ComponentType as StarlarkValue>::get_type_starlark_repr())
    }
}

impl std::fmt::Display for ComponentType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<Component>")
    }
}

/// Initialize Net with fields before adding to globals
pub fn init_net_global(builder: &mut GlobalsBuilder) {
    let net_type = super::net::make_default_net_type(builder.frozen_heap());
    builder.set("Net", builder.frozen_heap().alloc(net_type));
}

#[starlark_module]
pub fn component_globals(builder: &mut GlobalsBuilder) {
    const Component: ComponentType = ComponentType;
    const Symbol: SymbolType = SymbolType;
}
