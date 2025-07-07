#![allow(clippy::needless_lifetimes)]

use std::collections::HashMap;
use std::sync::Mutex;

use allocative::Allocative;
use once_cell::sync::Lazy;
use starlark::{
    any::ProvidesStaticType,
    collections::SmallMap,
    eval::{Arguments, Evaluator, ParametersSpec, ParametersSpecParam},
    starlark_complex_value, starlark_simple_value,
    values::{
        list::ListRef, starlark_value, tuple::TupleRef, Coerce, Freeze, FreezeResult, Heap,
        NoSerialize, StarlarkValue, Trace, Value, ValueLike,
    },
};

use crate::lang::eval::{copy_value, DeepCopyToHeap};
use crate::lang::evaluator_ext::EvaluatorExt;

use anyhow::anyhow;
use pcb_eda::{Symbol as EdaSymbol, SymbolLibrary};

/// Cache for parsed symbol libraries to avoid re-parsing the same file multiple times
static SYMBOL_LIBRARY_CACHE: Lazy<Mutex<HashMap<String, Vec<EdaSymbol>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Symbol represents a schematic symbol definition with pins
#[derive(Clone, Coerce, Trace, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub struct SymbolValueGen<V> {
    name: String,
    pins: SmallMap<String, V>,   // pad name -> signal name
    source_path: Option<String>, // Absolute path to the symbol library (if loaded from file)
}

impl<V: std::fmt::Debug> std::fmt::Debug for SymbolValueGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = f.debug_struct("Symbol");
        debug.field("name", &self.name);

        // Sort pins for deterministic output
        if !self.pins.is_empty() {
            let mut pins: Vec<_> = self.pins.iter().collect();
            pins.sort_by_key(|(k, _)| k.as_str());
            let pins_map: std::collections::BTreeMap<_, _> =
                pins.into_iter().map(|(k, v)| (k.as_str(), v)).collect();
            debug.field("pins", &pins_map);
        }

        debug.finish()
    }
}

starlark_complex_value!(pub SymbolValue);

#[starlark_value(type = "Symbol")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for SymbolValueGen<V>
where
    Self: ProvidesStaticType<'v>,
{
    fn provide(&'v self, demand: &mut starlark::values::Demand<'_, 'v>) {
        demand.provide_value::<&dyn DeepCopyToHeap>(self);
    }
}

impl<'v, V: ValueLike<'v>> std::fmt::Display for SymbolValueGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Symbol {{ name: \"{}\", pins: {{", self.name)?;

        let mut pins: Vec<_> = self.pins.iter().collect();
        pins.sort_by(|(a, _), (b, _)| a.cmp(b));

        let mut first = true;
        for (pad_name, signal_value) in pins {
            if !first {
                write!(f, ",")?;
            }
            first = false;
            let signal_str = signal_value.to_value().unpack_str().unwrap_or("<signal>");
            write!(f, " \"{pad_name}\": \"{signal_str}\"")?;
        }
        write!(f, " }} }}")?;
        Ok(())
    }
}

impl<'v, V: ValueLike<'v>> SymbolValueGen<V> {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn pins(&self) -> &SmallMap<String, V> {
        &self.pins
    }

    pub fn source_path(&self) -> Option<&str> {
        self.source_path.as_deref()
    }
}

impl<'v, V: ValueLike<'v>> DeepCopyToHeap for SymbolValueGen<V> {
    fn deep_copy_to<'dst>(&self, dst: &'dst Heap) -> anyhow::Result<Value<'dst>> {
        let pins = self
            .pins
            .iter()
            .map(|(k, v)| {
                let copied_value = copy_value(v.to_value(), dst)?;
                Ok((k.clone(), copied_value))
            })
            .collect::<Result<SmallMap<String, Value<'dst>>, anyhow::Error>>()?;

        Ok(dst.alloc(SymbolValue {
            name: self.name.clone(),
            pins,
            source_path: self.source_path.clone(),
        }))
    }
}

/// SymbolType is a factory for creating Symbol values
#[derive(Debug, Trace, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub struct SymbolType;

starlark_simple_value!(SymbolType);

impl std::fmt::Display for SymbolType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<Symbol>")
    }
}

#[starlark_value(type = "Symbol")]
impl<'v> StarlarkValue<'v> for SymbolType
where
    Self: ProvidesStaticType<'v>,
{
    fn provide(&'v self, demand: &mut starlark::values::Demand<'_, 'v>) {
        demand.provide_value::<&dyn DeepCopyToHeap>(self);
    }

    fn invoke(
        &self,
        _me: Value<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        let param_spec = ParametersSpec::new_named_only(
            "Symbol",
            [
                ("name", ParametersSpecParam::<Value<'_>>::Optional),
                ("definition", ParametersSpecParam::<Value<'_>>::Optional),
                ("library", ParametersSpecParam::<Value<'_>>::Optional),
            ],
        );

        let symbol_val = param_spec.parser(args, eval, |param_parser, eval_ctx| {
            let name_val: Option<Value> = param_parser.next_opt()?;
            let definition_val: Option<Value> = param_parser.next_opt()?;
            let library_val: Option<Value> = param_parser.next_opt()?;

            // Case 1: Explicit definition provided
            if let Some(def_val) = definition_val {
                let name = name_val
                    .and_then(|v| v.unpack_str())
                    .map(|s| s.to_owned())
                    .unwrap_or_else(|| "Symbol".to_owned());

                let def_list = ListRef::from_value(def_val).ok_or_else(|| {
                    starlark::Error::new_other(anyhow!(
                        "`definition` must be a list of (signal_name, [pad_names]) tuples"
                    ))
                })?;

                let mut pins: SmallMap<String, Value<'v>> = SmallMap::new();

                for item in def_list.iter() {
                    let tuple = TupleRef::from_value(item).ok_or_else(|| {
                        starlark::Error::new_other(anyhow!(
                            "Each definition item must be a tuple of (signal_name, [pad_names])"
                        ))
                    })?;

                    let tuple_items: Vec<_> = tuple.iter().collect();
                    if tuple_items.len() != 2 {
                        return Err(starlark::Error::new_other(anyhow!(
                            "Each definition tuple must have exactly 2 elements: (signal_name, [pad_names])"
                        )));
                    }

                    let signal_name = tuple_items[0].unpack_str().ok_or_else(|| {
                        starlark::Error::new_other(anyhow!("Signal name must be a string"))
                    })?;

                    let pad_list = ListRef::from_value(tuple_items[1]).ok_or_else(|| {
                        starlark::Error::new_other(anyhow!("Pad names must be a list"))
                    })?;

                    if pad_list.is_empty() {
                        return Err(starlark::Error::new_other(anyhow!(
                            "Pad list for signal '{}' cannot be empty", signal_name
                        )));
                    }

                    // For each pad in the list, create a mapping from pad to signal
                    for pad_val in pad_list.iter() {
                        let pad_name = pad_val.unpack_str().ok_or_else(|| {
                            starlark::Error::new_other(anyhow!("Pad name must be a string"))
                        })?;

                        // Check for duplicate pad assignments
                        if pins.contains_key(pad_name) {
                            return Err(starlark::Error::new_other(anyhow!(
                                "Pad '{}' is already assigned to signal '{}'", 
                                pad_name,
                                pins.get(pad_name).unwrap().to_value().unpack_str().unwrap_or("<unknown>")
                            )));
                        }

                        // Map: pad_name -> signal_name (note: this is inverted from the comment in the struct)
                        pins.insert(pad_name.to_owned(), eval_ctx.heap().alloc_str(signal_name).to_value());
                    }
                }

                let symbol = eval_ctx.heap().alloc_complex(SymbolValue {
                    name,
                    pins,
                    source_path: None,  // No source path for manually defined symbols
                });

                Ok(symbol)
            }
            // Case 2: Load from library
            else if let Some(lib_val) = library_val {
                let library_path = lib_val
                    .unpack_str()
                    .ok_or_else(|| starlark::Error::new_other(anyhow!("`library` must be a string path")))?;

                let load_resolver = eval_ctx
                    .load_resolver()
                    .ok_or_else(|| starlark::Error::new_other(anyhow!("No load resolver available")))?;

                let current_file = eval_ctx
                    .source_path()
                    .ok_or_else(|| starlark::Error::new_other(anyhow!("No source path available")))?;

                let resolved_path = load_resolver
                    .resolve_path(eval_ctx.file_provider().unwrap().as_ref(), library_path, std::path::Path::new(&current_file))
                    .map_err(|e| starlark::Error::new_other(anyhow!("Failed to resolve library path: {}", e)))?;

                let file_provider = eval_ctx
                    .file_provider()
                    .ok_or_else(|| starlark::Error::new_other(anyhow!("No file provider available")))?;

                // Parse all symbols from the library (with caching)
                let symbols = load_symbols_from_library(&resolved_path, file_provider.as_ref())?;

                // Determine which symbol to use
                let selected_symbol = if symbols.len() == 1 {
                    // Only one symbol, use it
                    &symbols[0]
                } else if symbols.is_empty() {
                    return Err(starlark::Error::new_other(anyhow!(
                        "No symbols found in library '{}'", 
                        resolved_path.display()
                    )));
                } else {
                    // Multiple symbols, need name parameter
                    let symbol_name = name_val
                        .and_then(|v| v.unpack_str())
                        .ok_or_else(|| {
                            starlark::Error::new_other(anyhow!(
                                "Library '{}' contains {} symbols. Please specify which one with the 'name' parameter. Available symbols: {}",
                                resolved_path.display(),
                                symbols.len(),
                                symbols.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", ")
                            ))
                        })?;

                    symbols.iter()
                        .find(|s| s.name == symbol_name)
                        .ok_or_else(|| {
                            starlark::Error::new_other(anyhow!(
                                "Symbol '{}' not found in library '{}'. Available symbols: {}",
                                symbol_name,
                                resolved_path.display(),
                                symbols.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", ")
                            ))
                        })?
                };

                // Convert EdaSymbol pins to our Symbol format
                // Map pad number -> signal name (which is the pin name from the symbol)
                let mut pins: SmallMap<String, Value<'v>> = SmallMap::new();
                for pin in &selected_symbol.pins {
                    // If pin name is ~, use the pin number instead
                    let signal_name = if pin.name == "~" {
                        &pin.number
                    } else {
                        &pin.name
                    };
                    pins.insert(pin.number.clone(), eval_ctx.heap().alloc_str(signal_name).to_value());
                }

                // Get the absolute path using file provider
                let absolute_path = file_provider.canonicalize(&resolved_path)
                    .unwrap_or(resolved_path.clone())
                    .to_string_lossy()
                    .into_owned();

                let symbol = eval_ctx.heap().alloc_complex(SymbolValue {
                    name: selected_symbol.name.clone(),
                    pins,
                    source_path: Some(absolute_path),
                });

                Ok(symbol)
            }
            else {
                Err(starlark::Error::new_other(anyhow!(
                    "Symbol requires either 'definition' or 'library' parameter"
                )))
            }
        })?;

        Ok(symbol_val)
    }

    fn eval_type(&self) -> Option<starlark::typing::Ty> {
        Some(<SymbolType as StarlarkValue>::get_type_starlark_repr())
    }
}

/// Parse all symbols from a KiCad symbol library with caching
pub fn load_symbols_from_library(
    path: &std::path::Path,
    file_provider: &dyn crate::FileProvider,
) -> starlark::Result<Vec<EdaSymbol>> {
    // Get the canonical path for cache key
    let cache_key = file_provider
        .canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned();

    // Check cache first
    {
        let cache = SYMBOL_LIBRARY_CACHE
            .lock()
            .map_err(|e| starlark::Error::new_other(anyhow!("Failed to lock cache: {}", e)))?;
        if let Some(symbols) = cache.get(&cache_key) {
            return Ok(symbols.clone());
        }
    }

    // Not in cache, read and parse the file
    let contents = file_provider.read_file(path).map_err(|e| {
        starlark::Error::new_other(anyhow!(
            "Failed to read symbol library '{}': {}",
            path.display(),
            e
        ))
    })?;

    let library_symbols = SymbolLibrary::from_string(&contents, "kicad_sym")
        .map_err(|e| starlark::Error::new_other(anyhow!("Failed to parse symbol library: {}", e)))?
        .symbols()
        .to_vec();

    // Store in cache
    {
        let mut cache = SYMBOL_LIBRARY_CACHE
            .lock()
            .map_err(|e| starlark::Error::new_other(anyhow!("Failed to lock cache: {}", e)))?;
        cache.insert(cache_key, library_symbols.clone());
    }

    Ok(library_symbols)
}

impl DeepCopyToHeap for SymbolType {
    fn deep_copy_to<'dst>(&self, dst: &'dst Heap) -> anyhow::Result<Value<'dst>> {
        Ok(dst.alloc(SymbolType))
    }
}
