#![allow(clippy::needless_lifetimes)]

use std::path::Path;
use std::sync::{Arc, OnceLock, RwLock};

use allocative::Allocative;
use once_cell::sync::Lazy;
use starlark::{
    any::ProvidesStaticType,
    collections::SmallMap,
    eval::{Arguments, Evaluator, ParametersSpec, ParametersSpecParam},
    starlark_simple_value,
    typing::{Ty, TyStarlarkValue, TyUser, TyUserParams},
    values::{
        Freeze, Heap, NoSerialize, StarlarkValue, Trace, Value, list::ListRef, starlark_value,
        tuple::TupleRef, typing::TypeInstanceId,
    },
};
use tracing::instrument;

use std::collections::{HashMap, HashSet};

use crate::EvalContext;
use crate::lang::evaluator_ext::EvaluatorExt;

use anyhow::anyhow;
use pcb_eda::kicad::symbol_library::KicadSymbolLibrary;

/// Global cache for parsed symbol libraries.
/// The `KicadSymbolLibrary` handles its own internal caching of resolved symbols.
static SYMBOL_LIBRARY_CACHE: Lazy<RwLock<HashMap<String, Arc<KicadSymbolLibrary>>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));

pub fn invalidate_symbol_library(path: &Path, file_provider: &dyn crate::FileProvider) {
    let canonical_path = file_provider
        .canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf());
    let canonical_key = canonical_path.to_string_lossy().into_owned();
    let raw_key = path.to_string_lossy().into_owned();

    if let Ok(mut cache) = SYMBOL_LIBRARY_CACHE.write() {
        cache.remove(&canonical_key);
        if raw_key != canonical_key {
            cache.remove(&raw_key);
        }
    }
}

/// Symbol represents a schematic symbol definition with pins
#[derive(Clone, Debug, Trace, Allocative, Freeze, PartialEq)]
pub struct SymbolPinAlternate {
    pub name: String,
    pub electrical_type: Option<String>,
    pub graphical_style: Option<String>,
}

#[derive(Clone, Debug, Trace, Allocative, Freeze, PartialEq)]
pub struct SymbolPin {
    pub name: String,
    pub number: String,
    pub electrical_type: Option<String>,
    pub graphical_style: Option<String>,
    pub hidden: bool,
    pub alternates: Vec<SymbolPinAlternate>,
}

#[derive(Clone, Trace, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub struct SymbolValue {
    pub name: Option<String>,
    pub pad_to_signal: SmallMap<String, String>, // pad name -> signal name
    pub pins: Vec<SymbolPin>, // Full pin metadata preserved from the source symbol
    pub source_uri: Option<String>, // Stable package URI for the symbol library when available
    pub raw_sexp: Option<String>, // Raw s-expression of the symbol (if loaded from file, otherwise None)
    pub properties: SmallMap<String, String>, // Properties from the symbol definition
    pub in_bom: bool,             // KiCad in_bom flag (inverse of skip_bom)
}

impl std::fmt::Debug for SymbolValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = f.debug_struct("Symbol");
        debug.field("name", &self.name);
        if let Some(source_uri) = &self.source_uri {
            debug.field("source_uri", source_uri);
        }

        // Sort pins for deterministic output
        if !self.pad_to_signal.is_empty() {
            let mut pins: Vec<_> = self.pad_to_signal.iter().collect();
            pins.sort_by_key(|(k, _)| k.as_str());
            let pins_map: std::collections::BTreeMap<_, _> =
                pins.into_iter().map(|(k, v)| (k.as_str(), v)).collect();
            debug.field("pins", &pins_map);
        }

        // Sort properties for deterministic output
        if !self.properties.is_empty() {
            let mut props: Vec<_> = self.properties.iter().collect();
            props.sort_by_key(|(k, _)| k.as_str());
            let props_map: std::collections::BTreeMap<_, _> = props
                .into_iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            debug.field("properties", &props_map);
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
    fn get_attr(&self, attr: &str, heap: &'v Heap) -> Option<Value<'v>> {
        match attr {
            "properties" => {
                let props_vec: Vec<(Value<'v>, Value<'v>)> = self
                    .properties
                    .iter()
                    .map(|(key, value)| {
                        (
                            heap.alloc_str(key).to_value(),
                            heap.alloc_str(value).to_value(),
                        )
                    })
                    .collect();
                Some(heap.alloc(starlark::values::dict::AllocDict(props_vec)))
            }
            _ => None,
        }
    }

    fn has_attr(&self, attr: &str, _heap: &'v Heap) -> bool {
        matches!(attr, "properties")
    }

    fn dir_attr(&self) -> Vec<String> {
        vec!["properties".to_string()]
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
        pins.sort_by_key(|(key, _)| *key);

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
    #[instrument(name = "symbol", skip(definition, eval_ctx), fields(name = name.as_deref().unwrap_or("<anon>"), library = library.as_deref().unwrap_or("<none>")))]
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

            let pins = symbol_pins_from_pad_map(&pad_to_signal);

            Ok(SymbolValue {
                name: Some(name),
                pad_to_signal,
                pins,
                source_uri: None,
                raw_sexp: None,
                properties: SmallMap::new(),
                in_bom: true,
            })
        }
        // Case 2: Load from library
        else if let Some(library_path) = library {
            let current_file = eval_ctx
                .source_path()
                .ok_or_else(|| starlark::Error::new_other(anyhow!("No source path available")))?;

            let resolved_path = resolve_symbol_library_path(
                &library_path,
                eval_ctx,
                std::path::Path::new(&current_file),
            )?;

            let file_provider = eval_ctx.file_provider();

            let (_symbol_name, symbol, source_path) = if file_provider.is_directory(&resolved_path)
            {
                load_split_library_symbol(&resolved_path, name, file_provider)?
            } else {
                // Get or load the library (lazy - only scans for symbol names, doesn't parse them)
                let library = get_or_load_library(&resolved_path, file_provider)?;

                // Determine which symbol to use
                let symbol_name = if let Some(name) = name {
                    // Verify the symbol exists
                    if !library.has_symbol(&name) {
                        let available: Vec<_> = library.symbol_names();
                        return Err(starlark::Error::new_other(anyhow!(
                            "Symbol '{}' not found in library '{}'. Available symbols: {}",
                            name,
                            resolved_path.display(),
                            available.join(", ")
                        )));
                    }
                    name
                } else {
                    // No specific name provided, need exactly one symbol in library
                    let names = library.symbol_names();
                    if names.len() == 1 {
                        names[0].to_string()
                    } else if names.is_empty() {
                        return Err(starlark::Error::new_other(anyhow!(
                            "No symbols found in library '{}'",
                            resolved_path.display()
                        )));
                    } else {
                        return Err(starlark::Error::new_other(anyhow!(
                            "Library '{}' contains {} symbols. Please specify which one with the 'name' parameter. Available symbols: {}",
                            resolved_path.display(),
                            names.len(),
                            names.join(", ")
                        )));
                    }
                };

                // Now get the specific symbol (this does the actual parsing + extends resolution)
                let symbol = library
                    .get_symbol_lazy_as_eda(&symbol_name)
                    .map_err(|e| {
                        starlark::Error::new_other(anyhow!(
                            "Failed to parse symbol '{}': {}",
                            symbol_name,
                            e
                        ))
                    })?
                    .ok_or_else(|| {
                        starlark::Error::new_other(anyhow!(
                            "Symbol '{}' not found in library",
                            symbol_name
                        ))
                    })?;
                (symbol_name, symbol, resolved_path.clone())
            };

            let source_uri = eval_ctx
                .resolution()
                .format_package_uri(&source_path)
                .ok_or_else(|| {
                    starlark::Error::new_other(anyhow!(
                        "Symbol library '{}' must resolve inside a workspace or dependency package",
                        source_path.display()
                    ))
                })?;

            let sexpr = symbol.raw_sexp.as_ref().map(|s| {
                pcb_sexpr::formatter::format_tree(s, pcb_sexpr::formatter::FormatMode::Normal)
            });

            let mut properties = SmallMap::new();
            for (key, value) in &symbol.properties {
                properties.insert(key.clone(), value.clone());
            }

            Ok(SymbolValue::from_eda_symbol(
                &symbol,
                Some(source_uri),
                sexpr,
                properties,
            ))
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

    pub fn pins(&self) -> &[SymbolPin] {
        &self.pins
    }

    pub fn source_uri(&self) -> Option<&str> {
        self.source_uri.as_deref()
    }

    pub fn raw_sexp(&self) -> Option<&str> {
        self.raw_sexp.as_deref()
    }

    pub fn signal_names(&self) -> impl Iterator<Item = &str> {
        self.pad_to_signal.values().map(|v| v.as_str())
    }

    pub fn properties(&self) -> &SmallMap<String, String> {
        &self.properties
    }

    fn from_eda_symbol(
        symbol: &pcb_eda::Symbol,
        source_uri: Option<String>,
        raw_sexp: Option<String>,
        properties: SmallMap<String, String>,
    ) -> Self {
        let mut pad_to_signal: SmallMap<String, String> = SmallMap::new();
        let pins = symbol
            .pins
            .iter()
            .map(|pin| {
                pad_to_signal.insert(pin.number.clone(), pin.signal_name().to_owned());
                SymbolPin {
                    name: pin.name.clone(),
                    number: pin.number.clone(),
                    electrical_type: pin.electrical_type.clone(),
                    graphical_style: pin.graphical_style.clone(),
                    hidden: pin.hidden,
                    alternates: pin
                        .alternates
                        .iter()
                        .map(|alternate| SymbolPinAlternate {
                            name: alternate.name.clone(),
                            electrical_type: alternate.electrical_type.clone(),
                            graphical_style: alternate.graphical_style.clone(),
                        })
                        .collect(),
                }
            })
            .collect();

        Self {
            name: Some(symbol.name.clone()),
            pad_to_signal,
            pins,
            source_uri,
            raw_sexp,
            properties,
            in_bom: symbol.in_bom,
        }
    }
}

pub(crate) fn symbol_pins_from_pad_map(pad_to_signal: &SmallMap<String, String>) -> Vec<SymbolPin> {
    pad_to_signal
        .iter()
        .map(|(pad, signal)| SymbolPin {
            name: signal.clone(),
            number: pad.clone(),
            electrical_type: None,
            graphical_style: None,
            hidden: false,
            alternates: Vec::new(),
        })
        .collect()
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

impl SymbolType {
    /// Return a stable TypeInstanceId for Symbol across all evaluations
    fn type_instance_id() -> TypeInstanceId {
        static SYMBOL_TYPE_ID: OnceLock<TypeInstanceId> = OnceLock::new();
        *SYMBOL_TYPE_ID.get_or_init(TypeInstanceId::r#gen)
    }
}

#[starlark_value(type = "Symbol")]
impl<'v> StarlarkValue<'v> for SymbolType
where
    Self: ProvidesStaticType<'v>,
{
    fn invoke(
        &self,
        _me: Value<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        let param_spec = ParametersSpec::new_parts(
            "Symbol",
            // One optional positional parameter
            [("library_spec", ParametersSpecParam::<Value<'_>>::Optional)],
            // Named parameters
            [
                ("name", ParametersSpecParam::<Value<'_>>::Optional),
                ("definition", ParametersSpecParam::<Value<'_>>::Optional),
                ("library", ParametersSpecParam::<Value<'_>>::Optional),
            ],
            false,
            std::iter::empty::<(&str, ParametersSpecParam<_>)>(),
            false,
        );

        let (library_spec_val, name_val, definition_val, library_val) =
            param_spec.parser(args, eval, |param_parser, _eval_ctx| {
                let library_spec_val: Option<Value> = param_parser.next_opt()?;
                let name_val: Option<String> = param_parser
                    .next_opt()?
                    .and_then(|v: Value<'v>| v.unpack_str().map(|s| s.to_owned()));
                let definition_val: Option<Value> = param_parser.next_opt()?;
                let library_val: Option<String> = param_parser
                    .next_opt()?
                    .and_then(|v: Value<'v>| v.unpack_str().map(|s| s.to_owned()));

                Ok((library_spec_val, name_val, definition_val, library_val))
            })?;

        // Check if we have a positional argument in the format "library:name"
        let (resolved_library, resolved_name) = if let Some(spec_val) = library_spec_val {
            if let Some(spec_str) = spec_val.unpack_str() {
                // Check if it contains a colon
                if let Some(colon_pos) = spec_str.rfind(':') {
                    // Split into library and name
                    let lib_part = &spec_str[..colon_pos];
                    let name_part = &spec_str[colon_pos + 1..];

                    // Make sure we don't have conflicting parameters
                    if library_val.is_some() || name_val.is_some() {
                        return Err(starlark::Error::new_other(anyhow!(
                            "Cannot specify both positional 'library:name' argument and named 'library' or 'name' parameters"
                        )));
                    }

                    (Some(lib_part.to_owned()), Some(name_part.to_owned()))
                } else {
                    // No colon, treat as library path only
                    if library_val.is_some() {
                        return Err(starlark::Error::new_other(anyhow!(
                            "Cannot specify both positional library argument and named 'library' parameter"
                        )));
                    }
                    // Use positional as library, keep name from named parameter (if any)
                    (Some(spec_str.to_owned()), name_val)
                }
            } else {
                return Err(starlark::Error::new_other(anyhow!(
                    "Positional argument must be a string"
                )));
            }
        } else {
            (library_val, name_val)
        };

        Ok(eval.heap().alloc_complex(SymbolValue::from_args(
            resolved_name,
            definition_val,
            resolved_library,
            eval.eval_context().unwrap(),
        )?))
    }

    fn eval_type(&self) -> Option<Ty> {
        let id = SymbolType::type_instance_id();
        let ty = Ty::custom(
            TyUser::new(
                "Symbol".to_string(),
                TyStarlarkValue::new::<SymbolValue>(),
                id,
                TyUserParams::default(),
            )
            .ok()?,
        );
        Some(ty)
    }
}

/// Get a library from cache, or load it lazily if not cached.
///
/// This only scans the file for symbol names and byte ranges - it does NOT
/// parse any symbols. Individual symbols are parsed on-demand via `get_symbol_lazy`.
#[instrument(name = "load_library", skip(file_provider), fields(path = %path.display()))]
fn get_or_load_library(
    path: &std::path::Path,
    file_provider: &dyn crate::FileProvider,
) -> starlark::Result<Arc<KicadSymbolLibrary>> {
    let cache_key = file_provider
        .canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned();

    // Check cache first (read lock)
    {
        let cache = SYMBOL_LIBRARY_CACHE
            .read()
            .map_err(|e| starlark::Error::new_other(anyhow!("Failed to lock cache: {}", e)))?;
        if let Some(library) = cache.get(&cache_key) {
            return Ok(Arc::clone(library));
        }
    }

    // Not in cache - read and scan the file (lazy, no full parsing)
    let contents = file_provider.read_file(path).map_err(|e| {
        starlark::Error::new_other(anyhow!(
            "Failed to read symbol library '{}': {}",
            path.display(),
            e
        ))
    })?;

    let library = KicadSymbolLibrary::from_string_lazy(contents).map_err(|e| {
        starlark::Error::new_other(anyhow!(
            "Failed to parse symbol library {}: {}",
            path.display(),
            e
        ))
    })?;

    let library = Arc::new(library);

    // Store in cache (write lock)
    {
        let mut cache = SYMBOL_LIBRARY_CACHE
            .write()
            .map_err(|e| starlark::Error::new_other(anyhow!("Failed to lock cache: {}", e)))?;
        cache.insert(cache_key, Arc::clone(&library));
    }

    Ok(library)
}

fn resolve_symbol_library_path(
    library_path: &str,
    eval_ctx: &EvalContext,
    current_file: &std::path::Path,
) -> starlark::Result<std::path::PathBuf> {
    match eval_ctx
        .get_config()
        .resolve_path(library_path, current_file)
    {
        Ok(path) => Ok(path),
        Err(err) => {
            let Some(stem) = library_path.strip_suffix(".kicad_sym") else {
                return Err(starlark::Error::new_other(anyhow!(
                    "Failed to resolve library path: {}",
                    err
                )));
            };

            let fallback_path = format!("{stem}.kicad_symdir");
            eval_ctx
                .get_config()
                .resolve_path(&fallback_path, current_file)
                .map_err(|_| {
                    starlark::Error::new_other(anyhow!("Failed to resolve library path: {}", err))
                })
        }
    }
}

fn split_library_symbol_files(
    dir: &std::path::Path,
    file_provider: &dyn crate::FileProvider,
) -> starlark::Result<Vec<(String, std::path::PathBuf)>> {
    let mut entries = file_provider.list_directory(dir).map_err(|e| {
        starlark::Error::new_other(anyhow!(
            "Failed to list symbol library '{}': {}",
            dir.display(),
            e
        ))
    })?;
    entries.sort();

    Ok(entries
        .into_iter()
        .filter(|entry| entry.extension().and_then(|ext| ext.to_str()) == Some("kicad_sym"))
        .filter_map(|entry| {
            let stem = entry.file_stem()?.to_str()?.to_string();
            Some((stem, entry))
        })
        .collect())
}

fn collect_split_library_sources(
    dir: &std::path::Path,
    symbol_name: &str,
    file_provider: &dyn crate::FileProvider,
    seen: &mut HashSet<String>,
    sources: &mut Vec<String>,
) -> starlark::Result<()> {
    if !seen.insert(symbol_name.to_string()) {
        return Ok(());
    }

    let symbol_path = dir.join(format!("{symbol_name}.kicad_sym"));
    let contents = file_provider.read_file(&symbol_path).map_err(|e| {
        starlark::Error::new_other(anyhow!(
            "Failed to read symbol library '{}': {}",
            symbol_path.display(),
            e
        ))
    })?;

    let library = KicadSymbolLibrary::from_string_lazy(contents.clone()).map_err(|e| {
        starlark::Error::new_other(anyhow!(
            "Failed to parse symbol library {}: {}",
            symbol_path.display(),
            e
        ))
    })?;
    let symbol = library
        .get_symbol_lazy(symbol_name)
        .map_err(|e| {
            starlark::Error::new_other(anyhow!("Failed to parse symbol '{}': {}", symbol_name, e))
        })?
        .ok_or_else(|| {
            starlark::Error::new_other(anyhow!(
                "Symbol '{}' not found in library '{}'",
                symbol_name,
                symbol_path.display()
            ))
        })?;

    if let Some(parent_name) = symbol.extends() {
        collect_split_library_sources(dir, parent_name, file_provider, seen, sources)?;
    }

    sources.push(contents);
    Ok(())
}

fn load_split_library_symbol(
    dir: &std::path::Path,
    requested_name: Option<String>,
    file_provider: &dyn crate::FileProvider,
) -> starlark::Result<(String, pcb_eda::Symbol, std::path::PathBuf)> {
    let symbol_files = split_library_symbol_files(dir, file_provider)?;
    let available: Vec<String> = symbol_files.iter().map(|(name, _)| name.clone()).collect();

    let symbol_name = if let Some(name) = requested_name {
        if available.iter().any(|candidate| candidate == &name) {
            name
        } else {
            return Err(starlark::Error::new_other(anyhow!(
                "Symbol '{}' not found in library '{}'. Available symbols: {}",
                name,
                dir.display(),
                available.join(", ")
            )));
        }
    } else if available.len() == 1 {
        available[0].clone()
    } else if available.is_empty() {
        return Err(starlark::Error::new_other(anyhow!(
            "No symbols found in library '{}'",
            dir.display()
        )));
    } else {
        return Err(starlark::Error::new_other(anyhow!(
            "Library '{}' contains {} symbols. Please specify which one with the 'name' parameter. Available symbols: {}",
            dir.display(),
            available.len(),
            available.join(", ")
        )));
    };

    let mut sources = Vec::new();
    let mut seen = HashSet::new();
    collect_split_library_sources(dir, &symbol_name, file_provider, &mut seen, &mut sources)?;

    let library = KicadSymbolLibrary::from_sources(sources).map_err(|e| {
        starlark::Error::new_other(anyhow!(
            "Failed to parse symbol library {}: {}",
            dir.display(),
            e
        ))
    })?;
    let symbol = library
        .get_symbol_lazy_as_eda(&symbol_name)
        .map_err(|e| {
            starlark::Error::new_other(anyhow!("Failed to parse symbol '{}': {}", symbol_name, e))
        })?
        .ok_or_else(|| {
            starlark::Error::new_other(anyhow!(
                "Symbol '{}' not found in library '{}'",
                symbol_name,
                dir.display()
            ))
        })?;

    Ok((
        symbol_name.clone(),
        symbol,
        dir.join(format!("{symbol_name}.kicad_sym")),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_pin_metadata() {
        let symbol = pcb_eda::Symbol::from_string(
            r#"(kicad_symbol_lib
  (version 20211014)
  (generator "test")
  (symbol "AlternatePinDemo"
    (property "Reference" "U")
    (symbol "AlternatePinDemo_0_1"
      (pin bidirectional line
        (at 1.27 2.54 180)
        (length 2.54)
        (name "PIO1")
        (number "1")
        (alternate "GPIO1" bidirectional line)
        (alternate "nRESET" input inverted)
      )
    )
  )
)"#,
            "kicad_sym",
        )
        .expect("symbol should parse");

        let mut properties = SmallMap::new();
        properties.insert("Reference".to_string(), "U".to_string());

        let symbol_value = SymbolValue::from_eda_symbol(
            &symbol,
            Some("package://demo/AlternatePinDemo.kicad_sym".to_string()),
            Some("(symbol \"AlternatePinDemo\")".to_string()),
            properties,
        );

        assert_eq!(
            symbol_value.pad_to_signal().get("1").map(String::as_str),
            Some("PIO1")
        );
        assert_eq!(symbol_value.pins().len(), 1);

        let pin = &symbol_value.pins()[0];
        assert_eq!(pin.name, "PIO1");
        assert_eq!(pin.number, "1");
        assert_eq!(pin.electrical_type.as_deref(), Some("bidirectional"));
        assert_eq!(pin.graphical_style.as_deref(), Some("line"));
        assert_eq!(pin.alternates.len(), 2);
        assert_eq!(pin.alternates[0].name, "GPIO1");
        assert_eq!(
            pin.alternates[0].electrical_type.as_deref(),
            Some("bidirectional")
        );
        assert_eq!(pin.alternates[1].name, "nRESET");
        assert_eq!(pin.alternates[1].electrical_type.as_deref(), Some("input"));
        assert_eq!(
            pin.alternates[1].graphical_style.as_deref(),
            Some("inverted")
        );
    }
}
