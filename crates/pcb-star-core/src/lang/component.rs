#![allow(clippy::needless_lifetimes)]

use allocative::Allocative;
use starlark::{
    any::ProvidesStaticType,
    collections::SmallMap,
    environment::GlobalsBuilder,
    eval::{Arguments, Evaluator, ParametersSpec, ParametersSpecParam},
    starlark_complex_value, starlark_module, starlark_simple_value,
    values::{
        dict::DictRef, starlark_value, Coerce, Freeze, FreezeResult, Heap, NoSerialize,
        StarlarkValue, Trace, Value, ValueLike,
    },
};

use crate::lang::evaluator_ext::EvaluatorExt;

use super::net::NetType;

use anyhow::anyhow;
use pcb_eda::Symbol as EdaSymbol;

#[derive(Clone, Coerce, Trace, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub struct ComponentValueGen<V> {
    name: String,
    mpn: Option<String>,
    ctype: Option<String>,
    footprint: String,
    prefix: String,
    pins: SmallMap<String, V>,
    connections: SmallMap<String, V>,
    properties: SmallMap<String, V>,
    source_path: String,
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

        // Sort pins for deterministic output
        if !self.pins.is_empty() {
            let mut pins: Vec<_> = self.pins.iter().collect();
            pins.sort_by_key(|(k, _)| k.as_str());
            let pins_map: std::collections::BTreeMap<_, _> =
                pins.into_iter().map(|(k, v)| (k.as_str(), v)).collect();
            debug.field("pins", &pins_map);
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
        if !self.properties.is_empty() {
            let mut props: Vec<_> = self.properties.iter().collect();
            props.sort_by_key(|(k, _)| k.as_str());
            let props_map: std::collections::BTreeMap<_, _> =
                props.into_iter().map(|(k, v)| (k.as_str(), v)).collect();
            debug.field("properties", &props_map);
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
    fn get_attr(&self, attr: &str, _heap: &'v Heap) -> Option<Value<'v>> {
        self.pins.get(attr).map(|v| v.to_value())
    }
}

impl<'v, V: ValueLike<'v>> std::fmt::Display for ComponentValueGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = self
            .mpn
            .as_deref()
            .unwrap_or(self.ctype.as_deref().unwrap_or("<unknown>"));
        writeln!(f, "Component({name})")?;

        let mut pins: Vec<_> = self.pins.iter().collect();
        pins.sort_by(|(a, _), (b, _)| a.cmp(b));

        for (pin_name, pin_value) in pins {
            let pad_str = pin_value.to_value().unpack_str().unwrap_or("<pad>");
            writeln!(f, "  {pin_name} ({pad_str})")?;
        }

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

    pub fn pins(&self) -> &SmallMap<String, V> {
        &self.pins
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
}

/// ComponentFactory is a value that represents a factory for a component.
#[derive(Debug, Trace, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub struct ComponentType;

starlark_simple_value!(ComponentType);

#[starlark_value(type = "ComponentFactory")]
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
                ("pin_defs", ParametersSpecParam::<Value<'_>>::Required),
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
            ],
        );

        let component_val = param_spec.parser(args, eval, |param_parser, eval_ctx| {
            let name_val: Value = param_parser.next()?;
            let name = name_val
                .unpack_str()
                .ok_or_else(|| starlark::Error::new_other(anyhow!("`name` must be a string")))?
                .to_owned();

            // --- Footprint -------------------------------------------------
            let footprint_val: Value = param_parser.next()?;
            let mut footprint = footprint_val
                .unpack_str()
                .ok_or_else(|| starlark::Error::new_other(anyhow!("`footprint` must be a string")))?
                .to_owned();

            // If the footprint looks like a KiCad module file, make the path absolute
            if footprint.ends_with(".kicad_mod") {
                let candidate = std::path::PathBuf::from(&footprint);
                if !candidate.is_absolute() {
                    let current_path = eval_ctx.source_path().unwrap_or_default();

                    let current_dir =
                        std::path::Path::new(&current_path)
                            .parent()
                            .ok_or_else(|| {
                                starlark::Error::new_other(anyhow!(
                                    "could not determine parent directory of current file"
                                ))
                            })?;

                    footprint = current_dir.join(candidate).to_string_lossy().into_owned();
                }
            }

            // --- Pins ------------------------------------------------------
            let pin_defs_val: Value = param_parser.next()?;
            let dict_ref = DictRef::from_value(pin_defs_val).ok_or_else(|| {
                starlark::Error::new_other(anyhow!("`pin_defs` must be a dict of name -> pad"))
            })?;
            let mut pins_str_map: SmallMap<String, String> = SmallMap::new();
            for (k_val, v_val) in dict_ref.iter() {
                let name = k_val
                    .unpack_str()
                    .ok_or_else(|| {
                        starlark::Error::new_other(anyhow!("pin name must be a string"))
                    })?
                    .to_owned();
                let pad = v_val
                    .unpack_str()
                    .ok_or_else(|| starlark::Error::new_other(anyhow!("pad must be a string")))?
                    .to_owned();
                pins_str_map.insert(name, pad);
            }

            // --- Connections ---------------------------------------------
            let pins_val: Value = param_parser.next()?;
            let conn_dict = DictRef::from_value(pins_val).ok_or_else(|| {
                starlark::Error::new_other(anyhow!(
                    "`pins` must be a dict mapping pin names to Net"
                ))
            })?;

            let mut connections: SmallMap<String, Value<'v>> = SmallMap::new();
            for (k_val, v_val) in conn_dict.iter() {
                let pin_name = k_val
                    .unpack_str()
                    .ok_or_else(|| {
                        starlark::Error::new_other(anyhow!("pin names must be strings"))
                    })?
                    .to_owned();
                if !pins_str_map.contains_key(&pin_name) {
                    return Err(starlark::Error::new_other(anyhow!(format!(
                        "Unknown pin name '{}'",
                        pin_name
                    ))));
                }

                if v_val.get_type() != "Net" {
                    return Err(starlark::Error::new_other(anyhow!(format!(
                        "Pin '{}' must be connected to a Net, got {}",
                        pin_name,
                        v_val.get_type()
                    ))));
                }

                connections.insert(pin_name, v_val);
            }

            // Detect missing pins in connections
            let mut missing_pins: Vec<String> = pins_str_map
                .keys()
                .filter(|n| !connections.contains_key(*n))
                .cloned()
                .collect();
            missing_pins.sort();
            if !missing_pins.is_empty() {
                return Err(starlark::Error::new_other(anyhow!(format!(
                    "Unconnected pin(s): {}",
                    missing_pins.join(", ")
                ))));
            }

            let prefix_val: Value = param_parser.next()?;
            let prefix = prefix_val
                .unpack_str()
                .ok_or_else(|| starlark::Error::new_other(anyhow!("`prefix` must be a string")))?
                .to_owned();

            // Optional fields
            let symbol_val: Option<Value> = param_parser.next_opt()?;
            let mpn: Option<Value> = param_parser.next_opt()?;
            let ctype: Option<Value> = param_parser.next_opt()?;
            let properties_val: Value = param_parser.next_opt()?.unwrap_or_default();

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

            properties_map.insert(
                "symbol_path".to_string(),
                symbol_val.map(|v| v.to_value()).unwrap_or_default(),
            );

            // ------------------- Build pins SmallMap<String, Value> -----------
            let mut pins_val_map: SmallMap<String, Value<'v>> = SmallMap::new();
            for (name, pad) in pins_str_map.iter() {
                pins_val_map.insert(name.clone(), eval_ctx.heap().alloc_str(pad).to_value());
            }

            let component = eval_ctx.heap().alloc_complex(ComponentValue {
                name,
                mpn: mpn.and_then(|v| v.unpack_str().map(|s| s.to_owned())),
                ctype: ctype.and_then(|v| v.unpack_str().map(|s| s.to_owned())),
                footprint,
                prefix,
                pins: pins_val_map,
                connections,
                properties: properties_map,
                source_path: eval_ctx.source_path().unwrap_or_default(),
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

impl std::fmt::Display for ComponentType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<Component>")
    }
}

#[derive(Clone, Debug, Trace, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub struct ComponentFactoryValue {
    mpn: Option<String>,
    ctype: Option<String>,
    footprint: String,
    prefix: String,
    pins: SmallMap<String, String>,
    default_properties: SmallMap<String, String>,
}

starlark_simple_value!(ComponentFactoryValue);

impl ComponentFactoryValue {
    /// Get the MPN (Manufacturer Part Number) if available
    pub fn mpn(&self) -> Option<&str> {
        self.mpn.as_deref()
    }

    /// Get the component type/manufacturer if available
    pub fn ctype(&self) -> Option<&str> {
        self.ctype.as_deref()
    }

    /// Get the footprint
    pub fn footprint(&self) -> &str {
        &self.footprint
    }

    /// Get the reference designator prefix
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// Get the pins map
    pub fn pins(&self) -> &SmallMap<String, String> {
        &self.pins
    }

    /// Get the default properties
    pub fn default_properties(&self) -> &SmallMap<String, String> {
        &self.default_properties
    }
}

impl std::fmt::Display for ComponentFactoryValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut ds = f.debug_struct("ComponentFactory");

        if let Some(mpn) = &self.mpn {
            ds.field("mpn", mpn);
        }
        if let Some(ctype) = &self.ctype {
            ds.field("type", ctype);
        }

        ds.field("footprint", &self.footprint)
            .field("prefix", &self.prefix)
            .field("pins", &self.pins);

        if !self.default_properties.is_empty() {
            ds.field("default_properties", &self.default_properties);
        }

        ds.finish()
    }
}

#[starlark_value(type = "ComponentFactoryValue")]
impl<'v> StarlarkValue<'v> for ComponentFactoryValue
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
            "<Component>",
            [
                ("name", ParametersSpecParam::<Value<'_>>::Required),
                ("pins", ParametersSpecParam::<Value<'_>>::Required),
                ("footprint", ParametersSpecParam::<Value<'_>>::Optional),
                ("prefix", ParametersSpecParam::<Value<'_>>::Optional),
                ("mpn", ParametersSpecParam::<Value<'_>>::Optional),
                ("type", ParametersSpecParam::<Value<'_>>::Optional),
                ("properties", ParametersSpecParam::<Value<'_>>::Optional),
            ],
        );

        let component_val = param_spec.parser(args, eval, |param_parser, eval_ctx| {
            let name_val: Value = param_parser.next()?;
            let name = name_val
                .unpack_str()
                .ok_or_else(|| starlark::Error::new_other(anyhow!("`name` must be a string")))?
                .to_owned();

            let pins_dict_val: Value = param_parser.next()?;
            let dict_ref = DictRef::from_value(pins_dict_val).ok_or_else(|| {
                starlark::Error::new_other(anyhow!(
                    "`pins` must be a dict mapping pin names to nets"
                ))
            })?;

            // Build connections map and validate pin names
            let mut connections: SmallMap<String, Value<'v>> = SmallMap::new();
            for (k_val, v_val) in dict_ref.iter() {
                let pin_name = k_val
                    .unpack_str()
                    .ok_or_else(|| {
                        starlark::Error::new_other(anyhow!("pin names must be strings"))
                    })?
                    .to_owned();
                if !self.pins.contains_key(&pin_name) {
                    return Err(starlark::Error::new_other(anyhow!(format!(
                        "Unknown pin name '{}'",
                        pin_name
                    ))));
                }

                if v_val.get_type() != "Net" {
                    return Err(starlark::Error::new_other(anyhow!(format!(
                        "Pin '{}' must be connected to a Net, got {}",
                        pin_name,
                        v_val.get_type()
                    ))));
                }

                connections.insert(pin_name, v_val);
            }

            // Detect any pins missing from the provided dict.
            let mut missing_pins: Vec<String> = self
                .pins
                .keys()
                .filter(|n| !connections.contains_key(*n))
                .cloned()
                .collect();
            missing_pins.sort();
            if !missing_pins.is_empty() {
                return Err(starlark::Error::new_other(anyhow!(format!(
                    "Unconnected pin(s): {}",
                    missing_pins.join(", ")
                ))));
            }

            let footprint_val: Option<Value> = param_parser.next_opt()?;
            let mut final_footprint = if let Some(fp_val) = footprint_val {
                let s = fp_val.unpack_str().ok_or_else(|| {
                    starlark::Error::new_other(anyhow!("`footprint` must be a string"))
                })?;
                s.to_owned()
            } else {
                self.footprint.clone()
            };

            // If the footprint looks like a KiCad module file, make the path absolute
            if final_footprint.ends_with(".kicad_mod") {
                let candidate = std::path::PathBuf::from(&final_footprint);
                if !candidate.is_absolute() {
                    let current_path = eval_ctx.source_path().unwrap_or_default();
                    if let Some(current_dir) = std::path::Path::new(&current_path).parent() {
                        final_footprint =
                            current_dir.join(candidate).to_string_lossy().into_owned();
                    }
                }
            }

            let prefix_val: Option<Value> = param_parser.next_opt()?;
            let final_prefix = if let Some(p_val) = prefix_val {
                p_val
                    .unpack_str()
                    .ok_or_else(|| {
                        starlark::Error::new_other(anyhow!("`prefix` must be a string"))
                    })?
                    .to_owned()
            } else {
                self.prefix.clone()
            };

            let mpn_val: Option<Value> = param_parser.next_opt()?;
            let final_mpn = if let Some(m_val) = mpn_val {
                Some(
                    m_val
                        .unpack_str()
                        .ok_or_else(|| {
                            starlark::Error::new_other(anyhow!("`mpn` must be a string"))
                        })?
                        .to_owned(),
                )
            } else {
                self.mpn.clone()
            };

            let ctype_val: Option<Value> = param_parser.next_opt()?;
            let final_ctype = if let Some(c_val) = ctype_val {
                Some(
                    c_val
                        .unpack_str()
                        .ok_or_else(|| {
                            starlark::Error::new_other(anyhow!("`type` must be a string"))
                        })?
                        .to_owned(),
                )
            } else {
                self.ctype.clone()
            };

            let properties_val: Value = param_parser.next_opt()?.unwrap_or_default();
            let mut properties_map: SmallMap<String, Value<'v>> = SmallMap::new();

            // Start with default_properties from factory.
            for (k, v) in self.default_properties.iter() {
                properties_map.insert(k.clone(), eval_ctx.heap().alloc_str(v).to_value());
            }

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
                        "'properties' must be a dict"
                    )));
                }
            }

            // ------------------- Build pins SmallMap<String, Value> -----------
            let mut pins_val_map: SmallMap<String, Value<'v>> = SmallMap::new();
            for (name, pad) in self.pins.iter() {
                pins_val_map.insert(name.clone(), eval_ctx.heap().alloc_str(pad).to_value());
            }

            let component = eval_ctx.heap().alloc_complex(ComponentValue {
                name,
                mpn: final_mpn,
                ctype: final_ctype,
                footprint: final_footprint,
                prefix: final_prefix,
                pins: pins_val_map,
                connections,
                properties: properties_map,
                source_path: eval_ctx.source_path().unwrap_or_default(),
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
        Some(<ComponentFactoryValue as StarlarkValue>::get_type_starlark_repr())
    }
}

pub(crate) fn build_component_factory_from_symbol(
    symbol_path: &std::path::Path,
    footprint_override: Option<&str>,
    base_dir: Option<&std::path::Path>,
    file_provider: &dyn crate::FileProvider,
) -> anyhow::Result<ComponentFactoryValue> {
    // Read symbol file contents using FileProvider
    let contents = file_provider.read_file(symbol_path).map_err(|e| {
        anyhow!(format!(
            "Failed to read symbol file '{}': {e}",
            symbol_path.display()
        ))
    })?;

    // Parse symbol file
    let symbol: EdaSymbol = EdaSymbol::from_string(&contents, "kicad_sym").map_err(|e| {
        anyhow!(format!(
            "Failed to parse symbol file '{}': {e}",
            symbol_path.display()
        ))
    })?;

    // Build pins map
    let mut pins_map: SmallMap<String, String> = SmallMap::new();
    for pin in &symbol.pins {
        pins_map.insert(pin.name.clone(), pin.number.clone());
    }

    // Determine footprint (override takes precedence over symbol default)
    let mut final_footprint = footprint_override
        .map(|s| s.to_owned())
        .unwrap_or_else(|| symbol.footprint.clone());

    // If the footprint looks like a KiCad module file, make the path absolute
    if final_footprint.ends_with(".kicad_mod") {
        let candidate = std::path::PathBuf::from(&final_footprint);
        if !candidate.is_absolute() {
            if let Some(dir) = base_dir {
                final_footprint = dir.join(candidate).to_string_lossy().into_owned();
            }
        }
    }

    // Default properties from symbol
    let mut default_properties: SmallMap<String, String> = SmallMap::new();
    for (k, v) in symbol.properties.iter() {
        default_properties.insert(k.clone(), v.clone());
    }

    // Always record the *absolute* path to the source symbol file so that downstream tooling
    // (e.g. schematic viewers, netlisters) can trace components back to their definition.
    // Use the canonicalised path when available, otherwise fall back to the provided path.
    let abs_symbol_path = match file_provider.canonicalize(symbol_path) {
        Ok(p) => p,
        Err(_) => symbol_path.to_path_buf(),
    };

    default_properties.insert(
        "symbol_path".to_owned(),
        abs_symbol_path.to_string_lossy().into_owned(),
    );

    Ok(ComponentFactoryValue {
        mpn: symbol.mpn.clone(),
        ctype: symbol.manufacturer.clone(),
        footprint: final_footprint,
        prefix: "U".to_owned(),
        pins: pins_map,
        default_properties,
    })
}

#[starlark_module]
pub fn component_globals(builder: &mut GlobalsBuilder) {
    const Component: ComponentType = ComponentType;
    const Net: NetType = NetType;

    fn load_component<'v>(
        #[starlark(require = pos)] symbol_path: String,
        #[starlark(require = named)] footprint: Option<String>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        // Resolve symbol_path relative to current file directory
        let resolved_path = {
            let candidate = std::path::PathBuf::from(&symbol_path);
            if candidate.is_absolute() {
                candidate
            } else {
                let current_path = eval
                    .context_value()
                    .ok_or_else(|| anyhow!("unexpected context - ContextValue not found"))?
                    .source_path();

                let current_dir =
                    std::path::Path::new(&current_path)
                        .parent()
                        .ok_or_else(|| {
                            anyhow!("could not determine parent directory of current file")
                        })?;

                current_dir.join(candidate)
            }
        };

        // Determine the directory of the current source file for resolving relative paths
        let base_dir_opt: Option<std::path::PathBuf> = eval.context_value().and_then(|cv| {
            let src_path = cv.source_path();
            std::path::Path::new(&src_path)
                .parent()
                .map(|p| p.to_path_buf())
        });

        let file_provider = eval
            .file_provider()
            .ok_or_else(|| anyhow!("No file provider available"))?;

        // Build ComponentFactoryValue via helper
        let factory = build_component_factory_from_symbol(
            &resolved_path,
            footprint.as_deref(),
            base_dir_opt.as_deref(),
            file_provider.as_ref(),
        )?;

        Ok(eval.heap().alloc(factory))
    }
}
