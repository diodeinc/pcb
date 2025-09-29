#![allow(clippy::needless_lifetimes)]

use allocative::Allocative;
use starlark::{
    any::ProvidesStaticType,
    collections::SmallMap,
    environment::GlobalsBuilder,
    eval::{Arguments, Evaluator, ParametersSpec, ParametersSpecParam},
    starlark_complex_value, starlark_module, starlark_simple_value,
    values::{
        dict::{AllocDict, DictRef},
        starlark_value, Coerce, Freeze, Heap, NoSerialize, StarlarkValue, Trace, Value, ValueLike,
    },
};

use crate::{
    lang::{evaluator_ext::EvaluatorExt, physical::PhysicalValue, spice_model::SpiceModelValue},
    FrozenSpiceModelValue,
};

use super::net::NetType;
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

#[derive(Clone, Coerce, Trace, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub struct ComponentValueGen<V> {
    name: String,
    mpn: Option<String>,
    ctype: Option<String>,
    footprint: String,
    prefix: String,
    connections: SmallMap<String, V>,
    properties: SmallMap<String, V>,
    source_path: String,
    symbol: V,
    spice_model: Option<V>,
}

impl<V: std::fmt::Debug> std::fmt::Debug for ComponentValueGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = f.debug_struct("Component");
        debug.field("name", &self.name);

        if let Some(mpn) = &self.mpn {
            debug.field("mpn", mpn);
        }
        if let Some(ctype) = &self.ctype {
            debug.field("type", ctype);
        }

        debug.field("footprint", &self.footprint);
        debug.field("prefix", &self.prefix);

        // Sort connections for deterministic output
        if !self.connections.is_empty() {
            let mut conns: Vec<_> = self.connections.iter().collect();
            conns.sort_by_key(|(k, _)| k.as_str());
            let conns_map: std::collections::BTreeMap<_, _> =
                conns.into_iter().map(|(k, v)| (k.as_str(), v)).collect();
            debug.field("connections", &conns_map);
        }

        // Sort properties for deterministic output
        if !self.properties.is_empty() {
            let mut props: Vec<_> = self.properties.iter().collect();
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

starlark_complex_value!(pub ComponentValue);

#[starlark_value(type = "Component")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for ComponentValueGen<V>
where
    Self: ProvidesStaticType<'v>,
{
    fn get_attr(&self, attr: &str, heap: &'v Heap) -> Option<Value<'v>> {
        match attr {
            "name" => Some(heap.alloc_str(&self.name).to_value()),
            "prefix" => Some(heap.alloc_str(&self.prefix).to_value()),
            "mpn" => Some(
                self.mpn()
                    .map(|mpn| heap.alloc_str(mpn).to_value())
                    .or_else(|| self.properties().get("mpn").map(|t| t.to_value()))
                    .or_else(|| self.properties().get("Mpn").map(|t| t.to_value()))
                    .unwrap_or_else(Value::new_none),
            ),
            "type" => Some(
                self.ctype()
                    .map(|ctype| heap.alloc_str(ctype).to_value())
                    .or_else(|| self.properties().get("type").map(|t| t.to_value()))
                    .or_else(|| self.properties().get("Type").map(|t| t.to_value()))
                    .unwrap_or_else(Value::new_none),
            ),
            "properties" => {
                // Build the same properties dictionary as in the testbench components dict
                let mut component_attrs = std::collections::HashMap::new();

                // Add component properties (excluding internal ones)
                for (key, value) in &self.properties {
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
            "capacitance" => Some(
                self.properties
                    .get("__capacitance__")
                    .map(|v| v.to_value())
                    .unwrap_or_else(Value::new_none),
            ),
            "resistance" => Some(
                self.properties
                    .get("__resistance__")
                    .map(|v| v.to_value())
                    .unwrap_or_else(Value::new_none),
            ),
            _ => None,
        }
    }

    fn has_attr(&self, attr: &str, _heap: &'v Heap) -> bool {
        matches!(
            attr,
            "name"
                | "prefix"
                | "mpn"
                | "type"
                | "properties"
                | "pins"
                | "capacitance"
                | "resistance"
        )
    }

    fn dir_attr(&self) -> Vec<String> {
        vec![
            "name".to_string(),
            "prefix".to_string(),
            "mpn".to_string(),
            "type".to_string(),
            "properties".to_string(),
            "pins".to_string(),
            "capacitance".to_string(),
            "resistance".to_string(),
        ]
    }
}

impl<'v, V: ValueLike<'v>> std::fmt::Display for ComponentValueGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = self
            .mpn
            .as_deref()
            .unwrap_or(self.ctype.as_deref().unwrap_or("<unknown>"));
        writeln!(f, "Component({name})")?;

        if !self.properties.is_empty() {
            let mut props: Vec<_> = self.properties.iter().collect();
            props.sort_by(|(a, _), (b, _)| a.cmp(b));
            writeln!(f, "Properties:")?;
            for (key, value) in props {
                writeln!(f, "  {key}: {value:?}")?;
            }
        }
        Ok(())
    }
}

impl<'v, V: ValueLike<'v>> ComponentValueGen<V> {
    pub fn mpn(&self) -> Option<&str> {
        self.mpn.as_deref()
    }

    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// Optional component *type* as declared via the `type = "..."` field when
    /// the factory was defined.  Used by schematic viewers to pick an
    /// appropriate symbol when the MPN is not available.
    pub fn ctype(&self) -> Option<&str> {
        self.ctype.as_deref()
    }

    pub fn footprint(&self) -> &str {
        &self.footprint
    }

    pub fn properties(&self) -> &SmallMap<String, V> {
        &self.properties
    }

    pub fn connections(&self) -> &SmallMap<String, V> {
        &self.connections
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn source_path(&self) -> &str {
        &self.source_path
    }

    pub fn symbol(&self) -> &V {
        &self.symbol
    }

    pub fn spice_model(&self) -> Option<&V> {
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
        let param_spec = ParametersSpec::new_named_only(
            "Component",
            [
                ("name", ParametersSpecParam::<Value<'_>>::Required),
                ("footprint", ParametersSpecParam::<Value<'_>>::Required),
                ("pin_defs", ParametersSpecParam::<Value<'_>>::Optional),
                ("pins", ParametersSpecParam::<Value<'_>>::Required),
                (
                    "prefix",
                    ParametersSpecParam::<Value<'_>>::Defaulted(
                        eval.heap().alloc_str("U").to_value(),
                    ),
                ),
                ("symbol", ParametersSpecParam::<Value<'_>>::Optional),
                ("mpn", ParametersSpecParam::<Value<'_>>::Optional),
                ("type", ParametersSpecParam::<Value<'_>>::Optional),
                ("properties", ParametersSpecParam::<Value<'_>>::Optional),
                ("spice_model", ParametersSpecParam::<Value<'_>>::Optional),
            ],
        );

        let component_val = param_spec.parser(args, eval, |param_parser, eval_ctx| {
            let name_val: Value = param_parser.next()?;
            let name = name_val
                .unpack_str()
                .ok_or(ComponentError::NameNotString)?
                .to_owned();

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

            let prefix_val: Value = param_parser.next()?;
            let prefix = prefix_val
                .unpack_str()
                .ok_or(ComponentError::PrefixNotString)?
                .to_owned();

            // Optional fields
            let symbol_val: Option<Value> = param_parser.next_opt()?;
            let mpn: Option<Value> = param_parser.next_opt()?;
            let ctype: Option<Value> = param_parser.next_opt()?;
            let properties_val: Value = param_parser.next_opt()?.unwrap_or_default();
            let spice_model_val: Option<Value> = param_parser.next_opt()?;

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
                        }
                    } else {
                        // symbol is not a Symbol type, just use pin_defs
                        SymbolValue {
                            name: None,
                            pad_to_signal,
                            source_path: None,
                            raw_sexp: None,
                        }
                    }
                } else {
                    // No symbol provided, create minimal SymbolValue from pin_defs
                    SymbolValue {
                        name: None,
                        pad_to_signal,
                        source_path: None,
                        raw_sexp: None,
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
                        try_add_physical_property(
                            eval_ctx.heap(),
                            &mut properties_map,
                            &key_str,
                            &v_val,
                        );
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

            let component = eval_ctx.heap().alloc_complex(ComponentValue {
                name,
                mpn: mpn.and_then(|v| v.unpack_str().map(|s| s.to_owned())),
                ctype: ctype.and_then(|v| v.unpack_str().map(|s| s.to_owned())),
                footprint,
                prefix,
                connections,
                properties: properties_map,
                source_path: eval_ctx.source_path().unwrap_or_default(),
                symbol: eval_ctx.heap().alloc_complex(final_symbol),
                spice_model: spice_model_val,
            });

            Ok(component)
        })?;

        // Add to current module context if available
        if let Some(mut module) = eval.module_value_mut() {
            module.add_child(component_val);
        }

        Ok(component_val)
    }

    fn eval_type(&self) -> Option<starlark::typing::Ty> {
        Some(<ComponentType as StarlarkValue>::get_type_starlark_repr())
    }
}

fn try_add_physical_property<'a, 'b>(
    heap: &'a Heap,
    map: &mut SmallMap<String, Value<'a>>,
    key: &str,
    value: &Value<'b>,
) -> Option<PhysicalValue> {
    if let Some(val) = value.unpack_str() {
        if let Ok(physical) = val.parse::<PhysicalValue>() {
            let key = format!("__{}__", key.to_ascii_lowercase());
            map.insert(key, heap.alloc(physical));
        }
    }
    None
}

impl std::fmt::Display for ComponentType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<Component>")
    }
}

#[starlark_module]
pub fn component_globals(builder: &mut GlobalsBuilder) {
    const Component: ComponentType = ComponentType;
    const Net: NetType = NetType;
    const Symbol: SymbolType = SymbolType;
}
