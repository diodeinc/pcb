#![allow(clippy::needless_lifetimes)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use allocative::Allocative;
use once_cell::sync::Lazy;
use starlark::{
    any::ProvidesStaticType,
    collections::SmallMap,
    eval::{Arguments, Evaluator, ParametersSpec, ParametersSpecParam},
    starlark_simple_value,
    values::{
        list::ListRef, starlark_value, tuple::TupleRef, Freeze, FreezeResult, Heap, NoSerialize,
        StarlarkValue, Trace, Value,
    },
};

use crate::lang::eval::DeepCopyToHeap;
use crate::lang::evaluator_ext::EvaluatorExt;
use crate::EvalContext;

use anyhow::anyhow;
use pcb_eda::kicad::symbol_library::KicadSymbolLibrary;
use pcb_eda::Symbol as EdaSymbol;

/// Cache for parsed symbol libraries with lazy extends resolution
#[derive(Clone)]
struct CachedLibrary {
    /// The unresolved library for lazy loading
    kicad_library: Arc<KicadSymbolLibrary>,
    /// Cache of already resolved symbols
    resolved_symbols: Arc<Mutex<HashMap<String, EdaSymbol>>>,
}

/// Global cache for symbol libraries
static SYMBOL_LIBRARY_CACHE: Lazy<Mutex<HashMap<String, CachedLibrary>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Symbol represents a schematic symbol definition with pins
#[derive(Clone, Trace, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub struct SymbolValue {
    pub name: Option<String>,
    pub pad_to_signal: SmallMap<String, String>, // pad name -> signal name
    pub source_path: Option<String>, // Absolute path to the symbol library (if loaded from file)
    pub raw_sexp: Option<String>, // Raw s-expression of the symbol (if loaded from file, otherwise None)
}

impl std::fmt::Debug for SymbolValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = f.debug_struct("Symbol");
        debug.field("name", &self.name);

        // Sort pins for deterministic output
        if !self.pad_to_signal.is_empty() {
            let mut pins: Vec<_> = self.pad_to_signal.iter().collect();
            pins.sort_by_key(|(k, _)| k.as_str());
            let pins_map: std::collections::BTreeMap<_, _> =
                pins.into_iter().map(|(k, v)| (k.as_str(), v)).collect();
            debug.field("pins", &pins_map);
        }

        debug.finish()
    }
}

starlark_simple_value!(SymbolValue);

#[starlark_value(type = "Symbol")]
impl<'v> StarlarkValue<'v> for SymbolValue
where
    Self: ProvidesStaticType<'v>,
{
    fn provide(&'v self, demand: &mut starlark::values::Demand<'_, 'v>) {
        demand.provide_value::<&dyn DeepCopyToHeap>(self);
    }
}

impl std::fmt::Display for SymbolValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Symbol {{ name: \"{}\", pins: {{",
            self.name.as_deref().unwrap_or("<unknown>")
        )?;

        let mut pins: Vec<_> = self.pad_to_signal.iter().collect();
        pins.sort_by(|(a, _), (b, _)| a.cmp(b));

        let mut first = true;
        for (pad_name, signal_value) in pins {
            if !first {
                write!(f, ",")?;
            }
            first = false;
            write!(f, " \"{pad_name}\": \"{signal_value}\"")?;
        }
        write!(f, " }} }}")?;
        Ok(())
    }
}

impl<'v> SymbolValue {
    pub fn from_args(
        name: Option<String>,
        definition: Option<Value<'v>>,
        library: Option<String>,
        eval_ctx: &EvalContext,
    ) -> Result<SymbolValue, starlark::Error> {
        // Case 1: Explicit definition
        if let Some(def_val) = definition {
            let name = name
                .map(|s| s.to_owned())
                .unwrap_or_else(|| "Symbol".to_owned());

            let def_list = ListRef::from_value(def_val).ok_or_else(|| {
                starlark::Error::new_other(anyhow!(
                    "`definition` must be a list of (signal_name, [pad_names]) tuples"
                ))
            })?;

            let mut pad_to_signal: SmallMap<String, String> = SmallMap::new();

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
                        "Pad list for signal '{}' cannot be empty",
                        signal_name
                    )));
                }

                // For each pad in the list, create a mapping from pad to signal
                for pad_val in pad_list.iter() {
                    let pad_name = pad_val.unpack_str().ok_or_else(|| {
                        starlark::Error::new_other(anyhow!("Pad name must be a string"))
                    })?;

                    // Check for duplicate pad assignments
                    if pad_to_signal.contains_key(pad_name) {
                        return Err(starlark::Error::new_other(anyhow!(
                            "Pad '{}' is already assigned to signal '{}'",
                            pad_name,
                            pad_to_signal
                                .get(pad_name)
                                .unwrap_or(&"<unknown>".to_string())
                        )));
                    }

                    pad_to_signal.insert(pad_name.to_owned(), signal_name.to_owned());
                }
            }

            Ok(SymbolValue {
                name: Some(name),
                pad_to_signal,
                source_path: None,
                raw_sexp: None,
            })
        }
        // Case 2: Load from library
        else if let Some(library_path) = library {
            let load_resolver = eval_ctx
                .get_load_resolver()
                .ok_or_else(|| starlark::Error::new_other(anyhow!("No load resolver available")))?;

            let current_file = eval_ctx
                .source_path
                .as_ref()
                .ok_or_else(|| starlark::Error::new_other(anyhow!("No source path available")))?;

            let resolved_path = load_resolver
                .resolve_path(
                    eval_ctx.file_provider.as_ref().unwrap().as_ref(),
                    &library_path,
                    std::path::Path::new(&current_file),
                )
                .map_err(|e| {
                    starlark::Error::new_other(anyhow!("Failed to resolve library path: {}", e))
                })?;

            let file_provider = eval_ctx
                .file_provider
                .as_ref()
                .ok_or_else(|| starlark::Error::new_other(anyhow!("No file provider available")))?;

            // If we have a specific symbol name, use lazy loading
            let selected_symbol = if let Some(name) = name {
                // Load only the specific symbol we need
                match load_symbol_from_library(&resolved_path, &name, file_provider.as_ref())? {
                    Some(symbol) => symbol,
                    None => {
                        // If not found, we need to load all symbols to provide a helpful error
                        let symbols =
                            load_symbols_from_library(&resolved_path, file_provider.as_ref())?;
                        return Err(starlark::Error::new_other(anyhow!(
                            "Symbol '{}' not found in library '{}'. Available symbols: {}",
                            name,
                            resolved_path.display(),
                            symbols
                                .iter()
                                .map(|s| s.name.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )));
                    }
                }
            } else {
                // No specific name provided, need to check if library has exactly one symbol
                let symbols = load_symbols_from_library(&resolved_path, file_provider.as_ref())?;

                if symbols.len() == 1 {
                    // Only one symbol, use it
                    symbols.into_iter().next().unwrap()
                } else if symbols.is_empty() {
                    return Err(starlark::Error::new_other(anyhow!(
                        "No symbols found in library '{}'",
                        resolved_path.display()
                    )));
                } else {
                    // Multiple symbols, need name parameter
                    return Err(starlark::Error::new_other(anyhow!(
                            "Library '{}' contains {} symbols. Please specify which one with the 'name' parameter. Available symbols: {}",
                            resolved_path.display(),
                            symbols.len(),
                            symbols.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", ")
                        )));
                }
            };

            // Convert EdaSymbol pins to our Symbol format
            // Map pad number -> signal name (which is the pin name from the symbol)
            let mut pad_to_signal: SmallMap<String, String> = SmallMap::new();
            for pin in &selected_symbol.pins {
                // If pin name is ~, use the pin number instead
                let signal_name = if pin.name == "~" {
                    &pin.number
                } else {
                    &pin.name
                };
                pad_to_signal.insert(pin.number.clone(), signal_name.to_owned());
            }

            // Get the absolute path using file provider
            let absolute_path = file_provider
                .canonicalize(&resolved_path)
                .unwrap_or(resolved_path.clone())
                .to_string_lossy()
                .into_owned();

            // Store the raw s-expression if available
            let sexpr = selected_symbol
                .raw_sexp()
                .map(|s| pcb_sexpr::format_sexpr(s, 0));

            Ok(SymbolValue {
                name: Some(selected_symbol.name.clone()),
                pad_to_signal,
                source_path: Some(absolute_path),
                raw_sexp: sexpr,
            })
        } else {
            Err(starlark::Error::new_other(anyhow!(
                "Symbol requires either 'definition' or 'library' parameter"
            )))
        }
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub fn pad_to_signal(&self) -> &SmallMap<String, String> {
        &self.pad_to_signal
    }

    pub fn source_path(&self) -> Option<&str> {
        self.source_path.as_deref()
    }

    pub fn raw_sexp(&self) -> Option<&str> {
        self.raw_sexp.as_deref()
    }

    pub fn signal_names(&self) -> impl Iterator<Item = &str> {
        self.pad_to_signal.values().map(|v| v.as_str())
    }
}

impl DeepCopyToHeap for SymbolValue {
    fn deep_copy_to<'dst>(&self, dst: &'dst Heap) -> anyhow::Result<Value<'dst>> {
        Ok(dst.alloc(self.clone()))
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

        let (name_val, definition_val, library_val) =
            param_spec.parser(args, eval, |param_parser, _eval_ctx| {
                let name_val: Option<String> = param_parser
                    .next_opt()?
                    .and_then(|v: Value<'v>| v.unpack_str().map(|s| s.to_owned()));

                let definition_val: Option<Value> = param_parser.next_opt()?;

                let library_val: Option<String> = param_parser
                    .next_opt()?
                    .and_then(|v: Value<'v>| v.unpack_str().map(|s| s.to_owned()));

                Ok((name_val, definition_val, library_val))
            })?;

        Ok(eval.heap().alloc_complex(SymbolValue::from_args(
            name_val,
            definition_val,
            library_val,
            eval.eval_context().unwrap(),
        )?))
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
        if let Some(cached_lib) = cache.get(&cache_key) {
            // Library is cached, get all symbols lazily resolved
            let kicad_lib = &cached_lib.kicad_library;
            let mut result = Vec::new();

            // Get all symbol names and resolve them lazily
            for symbol_name in kicad_lib.symbol_names() {
                // Check if already resolved
                let mut resolved_cache = cached_lib.resolved_symbols.lock().map_err(|e| {
                    starlark::Error::new_other(anyhow!("Failed to lock resolved cache: {}", e))
                })?;

                if let Some(resolved) = resolved_cache.get(symbol_name) {
                    result.push(resolved.clone());
                } else {
                    // Resolve the symbol lazily
                    if let Some(resolved_kicad) =
                        kicad_lib.get_symbol_lazy(symbol_name).map_err(|e| {
                            starlark::Error::new_other(anyhow!(
                                "Failed to resolve symbol '{}': {}",
                                symbol_name,
                                e
                            ))
                        })?
                    {
                        let eda_symbol: EdaSymbol = resolved_kicad.into();
                        resolved_cache.insert(symbol_name.to_string(), eda_symbol.clone());
                        result.push(eda_symbol);
                    }
                }
            }

            return Ok(result);
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

    // Parse library without resolving extends
    let kicad_library = KicadSymbolLibrary::from_string_lazy(&contents).map_err(|e| {
        starlark::Error::new_other(anyhow!("Failed to parse symbol library: {}", e))
    })?;

    // Get all symbols and resolve them eagerly for now (to maintain compatibility)
    let mut resolved_symbols = HashMap::new();
    let mut result = Vec::new();

    for symbol_name in kicad_library.symbol_names() {
        if let Some(resolved_kicad) = kicad_library.get_symbol_lazy(symbol_name).map_err(|e| {
            starlark::Error::new_other(anyhow!("Failed to resolve symbol '{}': {}", symbol_name, e))
        })? {
            let eda_symbol: EdaSymbol = resolved_kicad.into();
            resolved_symbols.insert(symbol_name.to_string(), eda_symbol.clone());
            result.push(eda_symbol);
        }
    }

    // Store in cache
    {
        let mut cache = SYMBOL_LIBRARY_CACHE
            .lock()
            .map_err(|e| starlark::Error::new_other(anyhow!("Failed to lock cache: {}", e)))?;
        cache.insert(
            cache_key,
            CachedLibrary {
                kicad_library: Arc::new(kicad_library),
                resolved_symbols: Arc::new(Mutex::new(resolved_symbols)),
            },
        );
    }

    Ok(result)
}

/// Load a specific symbol from a library with lazy resolution
pub fn load_symbol_from_library(
    path: &std::path::Path,
    symbol_name: &str,
    file_provider: &dyn crate::FileProvider,
) -> starlark::Result<Option<EdaSymbol>> {
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
        if let Some(cached_lib) = cache.get(&cache_key) {
            // Check if already resolved
            {
                let resolved_cache = cached_lib.resolved_symbols.lock().map_err(|e| {
                    starlark::Error::new_other(anyhow!("Failed to lock resolved cache: {}", e))
                })?;

                if let Some(resolved) = resolved_cache.get(symbol_name) {
                    return Ok(Some(resolved.clone()));
                }
            }

            // Not resolved yet, resolve it now
            let kicad_lib = &cached_lib.kicad_library;
            if let Some(resolved_kicad) = kicad_lib.get_symbol_lazy(symbol_name).map_err(|e| {
                starlark::Error::new_other(anyhow!(
                    "Failed to resolve symbol '{}': {}",
                    symbol_name,
                    e
                ))
            })? {
                let eda_symbol: EdaSymbol = resolved_kicad.into();

                // Cache the resolved symbol
                let mut resolved_cache = cached_lib.resolved_symbols.lock().map_err(|e| {
                    starlark::Error::new_other(anyhow!("Failed to lock resolved cache: {}", e))
                })?;
                resolved_cache.insert(symbol_name.to_string(), eda_symbol.clone());

                return Ok(Some(eda_symbol));
            }

            return Ok(None);
        }
    }

    // Not in cache, need to load the library first
    load_symbols_from_library(path, file_provider)?;

    // Now try again
    load_symbol_from_library(path, symbol_name, file_provider)
}

impl DeepCopyToHeap for SymbolType {
    fn deep_copy_to<'dst>(&self, dst: &'dst Heap) -> anyhow::Result<Value<'dst>> {
        Ok(dst.alloc(SymbolType))
    }
}
