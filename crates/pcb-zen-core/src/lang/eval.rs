use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    ops::Deref,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use anyhow::anyhow;
use starlark::collections::SmallMap;
use starlark::typing::Interface;
use starlark::{
    any::ProvidesStaticType,
    environment::{GlobalsBuilder, LibraryExtension},
    errors::EvalMessage,
    eval::{Evaluator, FileLoader},
    syntax::{AstModule, Dialect},
    typing::TypeMap,
    values::{
        dict::{AllocDict, DictRef},
        Heap, Value, ValueLike,
    },
    PrintHandler,
};

use crate::lang::component::{build_component_factory_from_symbol, component_globals};
use crate::lang::file::file_globals;
use crate::lang::input::{InputMap, InputValue};
use crate::{file_extensions, lang::assert::assert_globals};
use crate::{Diagnostic, WithDiagnostics};

#[cfg(feature = "native")]
fn default_file_provider() -> Arc<dyn crate::FileProvider> {
    Arc::new(crate::DefaultFileProvider) as Arc<dyn crate::FileProvider>
}

#[cfg(not(feature = "native"))]
fn default_file_provider() -> Arc<dyn crate::FileProvider> {
    panic!(
        "No default file provider available in WASM mode. A custom FileProvider must be provided."
    )
}

use super::{
    context::{ContextValue, FrozenContextValue},
    interface::interface_globals,
    module::{module_globals, FrozenModuleValue, ModuleLoader},
};

/// A PrintHandler that collects all print output into a vector
struct CollectingPrintHandler {
    output: RefCell<Vec<String>>,
}

impl CollectingPrintHandler {
    fn new() -> Self {
        Self {
            output: RefCell::new(Vec::new()),
        }
    }

    fn take_output(&self) -> Vec<String> {
        self.output.borrow_mut().drain(..).collect()
    }
}

impl PrintHandler for CollectingPrintHandler {
    fn println(&self, text: &str) -> starlark::Result<()> {
        eprintln!("{text}");
        self.output.borrow_mut().push(text.to_string());
        Ok(())
    }
}

pub(crate) trait DeepCopyToHeap {
    fn deep_copy_to<'dst>(&self, dst: &'dst Heap) -> anyhow::Result<Value<'dst>>;
}

unsafe impl<'v> ProvidesStaticType<'v> for dyn DeepCopyToHeap + 'v {
    type StaticType = dyn DeepCopyToHeap;
}

pub(crate) fn copy_value<'dst>(v: Value<'_>, dst: &'dst Heap) -> anyhow::Result<Value<'dst>> {
    if v.is_none() {
        return Ok(Value::new_none().to_value());
    }

    if let Some(b) = v.unpack_bool() {
        return Ok(Value::new_bool(b).to_value());
    }

    if let Some(i) = v.unpack_i32() {
        return Ok(dst.alloc(i));
    }

    if let Some(s) = v.unpack_str() {
        return Ok(dst.alloc_str(s).to_value());
    }

    if let Some(m) = DictRef::from_value(v) {
        let mut new_map = Vec::new();
        for (k, v) in m.iter() {
            let new_key = copy_value(k, dst)?;
            let new_value = copy_value(v, dst)?;
            new_map.push((new_key, new_value));
        }
        return Ok(dst.alloc(AllocDict(new_map)));
    }

    if let Some(dc) = v.request_value::<&dyn DeepCopyToHeap>() {
        return dc.deep_copy_to(dst);
    }

    Err(anyhow!(
        "internal error: cannot extract value of `{}`",
        v.get_type()
    ))
}

pub struct EvalOutput {
    pub ast: AstModule,
    pub star_module: starlark::environment::FrozenModule,
    pub sch_module: FrozenModuleValue,
    /// Map of parameter names to their full parameter information
    pub signature: HashMap<String, crate::lang::type_info::ParameterInfo>,
    /// Print output collected during evaluation
    pub print_output: Vec<String>,
}

#[derive(Debug, Default)]
struct EvalContextState {
    /// In-memory contents of files that are currently open/edited. Keyed by canonical path.
    file_contents: HashMap<PathBuf, String>,

    /// Per-file mapping of `symbol → target path` for "go-to definition".
    symbol_index: HashMap<PathBuf, HashMap<String, PathBuf>>,

    /// Per-file mapping of `symbol → parameter list` harvested from ModuleLoader
    /// instances so that signature help can surface them without having to
    /// re-evaluate the module each time.
    symbol_params: HashMap<PathBuf, HashMap<String, Vec<String>>>,

    /// Per-file mapping of `symbol → metadata` (kind, docs, etc.)
    /// generated when a module is frozen so that completion items can
    /// surface rich information without additional parsing.
    symbol_meta: HashMap<PathBuf, HashMap<String, crate::SymbolInfo>>,

    /// Cache of previously loaded modules keyed by their canonical absolute path. This
    /// ensures that repeated `load()` calls for the same file return the *same* frozen
    /// module instance so that type identities remain consistent across the evaluation
    /// graph (e.g. record types defined in that module).
    load_cache: HashMap<PathBuf, starlark::environment::FrozenModule>,

    /// Map of `module.zen` → set of files referenced via `load()`. Used by the LSP to
    /// propagate diagnostics when a dependency changes.
    module_deps: HashMap<PathBuf, HashSet<PathBuf>>,

    /// Cache of type maps for each module.
    #[allow(dead_code)]
    type_cache: HashMap<PathBuf, TypeMap>,

    /// Per-file mapping of raw load path strings (as written in `load()` statements)
    /// to the `Interface` returned by the Starlark type-checker for the loaded
    /// module. This allows tooling to quickly look up the public types exported
    /// by dependencies without re-parsing them.
    #[allow(dead_code)]
    interface_map: HashMap<PathBuf, HashMap<String, Interface>>,

    /// Map of paths that we are currently loading to the source file that triggered the load.
    /// This is used to detect cyclic imports and to skip in-flight files when loading directories.
    load_in_progress: HashMap<PathBuf, PathBuf>,
}

/// RAII guard that automatically removes a path from the load_in_progress set when dropped.
struct LoadGuard {
    state: Arc<Mutex<EvalContextState>>,
    path: PathBuf,
}

impl LoadGuard {
    fn new(
        state: Arc<Mutex<EvalContextState>>,
        path: PathBuf,
        source: PathBuf,
    ) -> starlark::Result<Self> {
        {
            let mut state_guard = state.lock().unwrap();

            // Special handling for directories: allow multiple loads from different sources
            if path.is_dir() {
                // For directories, we don't need to check for cycles here because
                // load_directory_as_module will skip files that are already being loaded
                state_guard.load_in_progress.insert(path.clone(), source);
            } else {
                // For files, check if this would create a cycle
                if let Some(_existing_source) = state_guard.load_in_progress.get(&path) {
                    // It's a cycle if the file we're trying to load is already loading something
                    if state_guard.load_in_progress.values().any(|v| {
                        v.canonicalize().unwrap_or(v.clone())
                            == path.canonicalize().unwrap_or(path.clone())
                    }) {
                        return Err(starlark::Error::new_other(anyhow!(format!(
                            "cyclic load detected while loading `{}`",
                            path.display()
                        ))));
                    }
                }
                state_guard.load_in_progress.insert(path.clone(), source);
            }
        }
        Ok(Self { state, path })
    }
}

impl Drop for LoadGuard {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            state.load_in_progress.remove(&self.path);
        }
    }
}

pub struct EvalContext {
    /// The starlark::environment::Module we are evaluating.
    pub module: starlark::environment::Module,

    /// The shared state of the evaluation context (potentially shared with other contexts).
    state: Arc<Mutex<EvalContextState>>,

    /// Documentation source for built-in Starlark symbols keyed by their name.
    builtin_docs: HashMap<String, String>,

    /// When `true`, missing required io()/config() placeholders are treated as errors during
    /// evaluation.  This is enabled when a module is instantiated via `ModuleLoader`.
    pub(crate) strict_io_config: bool,

    /// When `true`, the surrounding LSP wishes to eagerly parse all files in the workspace.
    /// Defaults to `true` so that features work out-of-the-box. Clients can opt-out via CLI
    /// flag which toggles this value before the server starts.
    eager: bool,

    /// The absolute path to the module we are evaluating.
    pub(crate) source_path: Option<PathBuf>,

    /// The contents of the module we are evaluating.
    contents: Option<String>,

    /// The name of the module we are evaluating.
    pub(crate) name: Option<String>,

    /// The inputs of the module we are evaluating.
    pub(crate) inputs: Option<InputMap>,

    /// Optional map of custom properties to attach to the root `ModuleValue` before the
    /// module body is executed. Populated by `ModuleLoader` when the caller passes the
    /// `properties = {...}` keyword argument.
    properties: Option<starlark::collections::SmallMap<String, crate::lang::input::InputValue>>,

    /// Additional diagnostics from this evaluation context that will be merged with any other
    /// diagnostics attached to the ContextValue.
    diagnostics: RefCell<Vec<Diagnostic>>,

    /// File provider for accessing file system operations
    pub(crate) file_provider: Option<Arc<dyn crate::FileProvider>>,

    /// Load resolver for resolving load() paths
    pub(crate) load_resolver: Option<Arc<dyn crate::LoadResolver>>,
}

impl Default for EvalContext {
    fn default() -> Self {
        Self::new()
    }
}

impl EvalContext {
    pub fn new() -> Self {
        // Build a `Globals` instance so we can harvest the documentation for
        // all built-in symbols. We replicate the same extensions that
        // `build_globals` uses so that the docs are in sync with what the
        // evaluator will actually expose.
        let globals = Self::build_globals();

        // Convert the docs into a map keyed by symbol name
        let mut builtin_docs: HashMap<String, String> = HashMap::new();
        for (name, item) in globals.documentation().members {
            builtin_docs.insert(name.clone(), item.render_as_code(&name));
        }

        Self {
            module: starlark::environment::Module::new(),
            state: Arc::new(Mutex::new(EvalContextState::default())),
            builtin_docs,
            strict_io_config: false,
            eager: true,
            source_path: None,
            contents: None,
            name: None,
            inputs: None,
            properties: None,
            diagnostics: RefCell::new(Vec::new()),
            file_provider: None,
            load_resolver: None,
        }
    }

    /// Create a new EvalContext with a custom file provider
    pub fn with_file_provider(file_provider: Arc<dyn crate::FileProvider>) -> Self {
        let mut ctx = Self::new();
        ctx.file_provider = Some(file_provider);
        ctx
    }

    /// Set the file provider for this context
    pub fn set_file_provider(mut self, file_provider: Arc<dyn crate::FileProvider>) -> Self {
        self.file_provider = Some(file_provider);
        self
    }

    /// Set the load resolver for this context
    pub fn set_load_resolver(mut self, load_resolver: Arc<dyn crate::LoadResolver>) -> Self {
        self.load_resolver = Some(load_resolver);
        self
    }

    /// Enable or disable strict IO/config placeholder checking for subsequent evaluations.
    pub fn set_strict_io_config(mut self, enabled: bool) -> Self {
        self.strict_io_config = enabled;
        self
    }

    /// Enable or disable eager workspace parsing.
    pub fn set_eager(mut self, eager: bool) -> Self {
        self.eager = eager;
        self
    }

    /// Create a new Context that shares caches with this one
    pub fn child_context(&self) -> Self {
        Self {
            module: starlark::environment::Module::new(),
            state: self.state.clone(),
            builtin_docs: self.builtin_docs.clone(),
            strict_io_config: false,
            eager: true,
            source_path: None,
            contents: None,
            name: None,
            inputs: None,
            properties: None,
            diagnostics: RefCell::new(Vec::new()),
            file_provider: self.file_provider.clone(),
            load_resolver: self.load_resolver.clone(),
        }
    }

    fn dialect(&self) -> Dialect {
        let mut dialect = Dialect::Extended;
        dialect.enable_f_strings = true;
        dialect
    }

    /// Construct the `Globals` used when evaluating modules. Kept in one place so the
    /// configuration stays consistent between the main evaluator and nested `load()`s.
    fn build_globals() -> starlark::environment::Globals {
        GlobalsBuilder::extended_by(&[
            LibraryExtension::RecordType,
            LibraryExtension::EnumType,
            LibraryExtension::Typing,
            LibraryExtension::StructType,
            LibraryExtension::Print,
            LibraryExtension::Debug,
            LibraryExtension::Partial,
            LibraryExtension::Breakpoint,
            LibraryExtension::SetType,
        ])
        .with(component_globals)
        .with(module_globals)
        .with(interface_globals)
        .with(assert_globals)
        .with(file_globals)
        .build()
    }

    /// Record that `from` references `to` via a `Module()` call.
    pub(crate) fn record_module_dependency(&self, from: &Path, to: &Path) {
        if let Ok(mut state) = self.state.lock() {
            let entry = state.module_deps.entry(from.to_path_buf()).or_default();
            entry.insert(to.to_path_buf());
        }
    }

    /// Check if there is a module dependency between two files
    pub fn module_dep_exists(&self, from: &Path, to: &Path) -> bool {
        if let Ok(state) = self.state.lock() {
            if let Some(deps) = state.module_deps.get(from) {
                deps.contains(to)
            } else {
                false
            }
        } else {
            false
        }
    }

    /// Return the cached parameter list for a global symbol if one is available.
    pub fn get_params_for_global_symbol(
        &self,
        current_file: &Path,
        symbol: &str,
    ) -> Option<Vec<String>> {
        if let Ok(state) = self.state.lock() {
            if let Some(map) = state.symbol_params.get(current_file) {
                if let Some(list) = map.get(symbol) {
                    return Some(list.clone());
                }
            }
        }
        None
    }

    /// Return rich completion metadata for a symbol if available.
    pub fn get_symbol_info(&self, current_file: &Path, symbol: &str) -> Option<crate::SymbolInfo> {
        if let Ok(state) = self.state.lock() {
            if let Some(map) = state.symbol_meta.get(current_file) {
                if let Some(meta) = map.get(symbol) {
                    return Some(meta.clone());
                }
            }
        }

        // Fallback: built-in global docs.
        if let Some(doc) = self.builtin_docs.get(symbol) {
            return Some(crate::SymbolInfo {
                kind: crate::SymbolKind::Function,
                parameters: None,
                source_path: None,
                type_name: "function".to_string(),
                documentation: Some(doc.clone()),
            });
        }
        None
    }

    /// Build a synthetic module that exposes one `ModuleLoader` per `.zen` file
    /// found directly inside `dir`.  This is used to implement the shorthand
    /// `load("path/to/folder", "Foo", "Bar")`, which behaves as if the
    /// caller had written `Foo = Module("path/to/folder/Foo.zen")` for
    /// each requested symbol.
    ///
    /// Returns a tuple of (frozen_module, errors_by_symbol) where errors_by_symbol
    /// maps symbol names to the diagnostics that occurred while loading that symbol.
    fn load_directory_as_module(
        &self,
        dir: &std::path::Path,
        original_load_path: &str,
    ) -> starlark::Result<(
        starlark::environment::FrozenModule,
        HashMap<String, Vec<Diagnostic>>,
    )> {
        // Get or create a default file provider if none was set
        let file_provider = self
            .file_provider
            .clone()
            .unwrap_or_else(|| default_file_provider());

        // Ensure the directory exists.
        if !file_provider.is_directory(dir) {
            return Err(starlark::Error::new_other(anyhow::anyhow!(format!(
                "Directory {} does not exist or is not a directory",
                dir.display()
            ))));
        }

        // Gather all immediate *.zen entries.
        let dir_entries = file_provider.list_directory(dir).map_err(|e| {
            starlark::Error::new_other(anyhow::anyhow!(format!(
                "Failed to read directory {}: {e}",
                dir.display()
            )))
        })?;

        let mut entries: Vec<std::path::PathBuf> = dir_entries
            .into_iter()
            .filter(|p| {
                file_provider.exists(p)
                    && !file_provider.is_directory(p)
                    && file_extensions::is_starlark_file(p.extension())
            })
            .collect();

        // Deterministic order – keeps cache keys stable.
        entries.sort();

        // Prepare an environment to host the exported loaders.
        let env = starlark::environment::Module::new();
        let heap = env.heap();

        // Get the set of files currently being loaded
        let in_progress_files = self.state.lock().unwrap().load_in_progress.clone();

        // Collect errors keyed by symbol name
        let mut errors_by_symbol: HashMap<String, Vec<Diagnostic>> = HashMap::new();

        for star_path in entries {
            // Determine symbol name (file stem without extension).
            let symbol_name = star_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();

            // Construct the load path for this file within the directory
            // If the original load path was "@something/my/directory",
            // this becomes "@something/my/directory/file.zen"
            let file_load_path = format!(
                "{}/{}",
                original_load_path,
                star_path.file_name().unwrap().to_string_lossy()
            );

            // Use the load resolver to resolve this specific file's load path
            let resolved_star_path = if let Some(load_resolver) = &self.load_resolver {
                match load_resolver.resolve_path(
                    file_provider.as_ref(),
                    &file_load_path,
                    self.source_path.as_ref().unwrap_or(&PathBuf::from(".")),
                ) {
                    Ok(resolved) => resolved,
                    Err(e) => {
                        // Create a diagnostic for resolution errors
                        let diag = Diagnostic {
                            path: file_load_path.clone(),
                            span: None,
                            severity: starlark::analysis::EvalSeverity::Error,
                            body: format!("Failed to resolve load path '{file_load_path}': {e}"),
                            call_stack: None,
                            child: None,
                        };
                        errors_by_symbol
                            .entry(symbol_name.clone())
                            .or_default()
                            .push(diag);
                        continue;
                    }
                }
            } else {
                // No load resolver, use the path as-is
                star_path.clone()
            };

            // Canonicalize the resolved path for comparison
            let canonical_star_path = match file_provider.canonicalize(&resolved_star_path) {
                Ok(p) => p,
                Err(_) => resolved_star_path.clone(),
            };

            // Skip files that are currently being loaded (to avoid cycles)
            let mut should_skip = false;

            // Check if this file is being loaded (appears as a key)
            if in_progress_files.contains_key(&canonical_star_path) {
                should_skip = true;
            }

            // Check if this file triggered any ongoing load (appears as a value)
            if !should_skip {
                for (_, source_path) in in_progress_files.iter() {
                    let canonical_source = file_provider
                        .canonicalize(source_path)
                        .unwrap_or_else(|_| source_path.clone());
                    if canonical_source == canonical_star_path {
                        should_skip = true;
                        break;
                    }
                }
            }

            // Check if this is our own source path
            if !should_skip {
                if let Some(ref our_path) = self.source_path {
                    let canonical_our_path = file_provider
                        .canonicalize(our_path)
                        .unwrap_or_else(|_| our_path.clone());
                    if canonical_our_path == canonical_star_path {
                        should_skip = true;
                    }
                }
            }

            if should_skip {
                continue;
            }

            // Create a LoadGuard for this file before evaluating it
            // This ensures that if this file tries to load the same directory,
            // it will see itself in load_in_progress and skip itself
            let _guard = match LoadGuard::new(
                self.state.clone(),
                canonical_star_path.clone(),
                self.source_path
                    .clone()
                    .unwrap_or_else(|| PathBuf::from("<directory-load>")),
            ) {
                Ok(guard) => guard,
                Err(_) => {
                    // This shouldn't happen since we already checked above
                    continue;
                }
            };

            // Try to evaluate the module to check for errors
            let eval_result = self
                .child_context()
                .set_source_path(resolved_star_path.clone())
                .set_module_name(symbol_name.clone())
                .set_inputs(InputMap::new())
                .eval();

            // Collect any error diagnostics for this symbol
            let mut symbol_errors = Vec::new();
            for diag in &eval_result.diagnostics {
                if diag.is_error() {
                    symbol_errors.push(diag.clone());
                }
            }

            // If there were errors, store them keyed by symbol name
            if !symbol_errors.is_empty() {
                errors_by_symbol.insert(symbol_name.clone(), symbol_errors);
            }

            // If the module loaded successfully, create a loader for it
            if let Some(output) = eval_result.output {
                // Build a ModuleLoader with the frozen module
                let loader = ModuleLoader {
                    name: symbol_name.clone(),
                    source_path: resolved_star_path.to_string_lossy().to_string(),
                    params: {
                        let mut params = vec!["name".to_string(), "properties".to_string()];
                        if let Some(extra) = output
                            .star_module
                            .extra_value()
                            .and_then(|e| e.downcast_ref::<FrozenContextValue>())
                        {
                            for (param_name, _) in extra.used_inputs.iter() {
                                params.push(param_name.clone());
                            }
                        }
                        params.sort();
                        params.dedup();
                        params
                    },
                    param_types: {
                        let mut param_types = SmallMap::new();
                        if let Some(extra) = output
                            .star_module
                            .extra_value()
                            .and_then(|e| e.downcast_ref::<FrozenContextValue>())
                        {
                            for (param_name, param_type) in extra.used_inputs.iter() {
                                param_types.insert(param_name.clone(), param_type.to_string());
                            }
                        }
                        param_types
                    },
                    frozen_module: Some(output.star_module),
                };

                // Insert into the environment
                let loader_val = heap.alloc(loader);
                env.set(&symbol_name, loader_val);
            }
        }

        // Gather all immediate *.kicad_sym entries.
        let dir_entries = file_provider.list_directory(dir).map_err(|e| {
            starlark::Error::new_other(anyhow::anyhow!(format!(
                "Failed to read directory {}: {e}",
                dir.display()
            )))
        })?;

        let mut sym_entries: Vec<std::path::PathBuf> = dir_entries
            .into_iter()
            .filter(|p| {
                file_provider.exists(p)
                    && !file_provider.is_directory(p)
                    && file_extensions::is_kicad_symbol_file(p.extension())
            })
            .collect();

        sym_entries.sort();

        for sym_path in sym_entries {
            let symbol_name = sym_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();

            // Construct the load path for this .kicad_sym file within the directory
            let file_load_path = format!(
                "{}/{}",
                original_load_path,
                sym_path.file_name().unwrap().to_string_lossy()
            );

            // Use the load resolver to resolve this specific file's load path
            let resolved_sym_path = if let Some(load_resolver) = &self.load_resolver {
                match load_resolver.resolve_path(
                    file_provider.as_ref(),
                    &file_load_path,
                    self.source_path.as_ref().unwrap_or(&PathBuf::from(".")),
                ) {
                    Ok(resolved) => resolved,
                    Err(e) => {
                        // Create a diagnostic for resolution errors
                        let diag = Diagnostic {
                            path: file_load_path.clone(),
                            span: None,
                            severity: starlark::analysis::EvalSeverity::Error,
                            body: format!("Failed to resolve load path '{file_load_path}': {e}"),
                            call_stack: None,
                            child: None,
                        };
                        errors_by_symbol
                            .entry(symbol_name.clone())
                            .or_default()
                            .push(diag);
                        continue;
                    }
                }
            } else {
                // No load resolver, use the path as-is
                sym_path.clone()
            };

            match build_component_factory_from_symbol(
                &resolved_sym_path,
                None,
                Some(dir),
                file_provider.as_ref(),
            ) {
                Ok(factory) => {
                    let val = heap.alloc(factory);
                    env.set(&symbol_name, val);
                }
                Err(e) => {
                    // Create a diagnostic for component factory errors
                    let diag = Diagnostic {
                        path: file_load_path.clone(),
                        span: None,
                        severity: starlark::analysis::EvalSeverity::Error,
                        body: format!("Failed to load component from {file_load_path}: {e}"),
                        call_stack: None,
                        child: None,
                    };
                    errors_by_symbol
                        .entry(symbol_name.clone())
                        .or_default()
                        .push(diag);
                }
            }
        }

        // Freeze the environment and return both the module and errors
        let frozen = env.freeze().map_err(starlark::Error::from)?;
        Ok((frozen, errors_by_symbol))
    }

    /// Provide the raw contents of the Starlark module. When omitted, the contents
    /// will be read from `source_path` during [`Context::eval`].
    #[allow(dead_code)]
    pub fn set_source_contents<S: Into<String>>(mut self, contents: S) -> Self {
        self.contents = Some(contents.into());
        self
    }

    /// Set the source path of the module we are evaluating.
    pub fn set_source_path(mut self, path: PathBuf) -> Self {
        self.source_path = Some(path);
        self
    }

    /// Override the module name that is exposed to user code via `ContextValue`.
    /// When unset, callers should rely on their own default (e.g. "<root>").
    pub fn set_module_name<S: Into<String>>(mut self, name: S) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Specify the `InputMap` containing external inputs (`io()` / `config()`).
    /// If not provided, an empty map is assumed.
    pub fn set_inputs(mut self, inputs: InputMap) -> Self {
        self.inputs = Some(inputs);
        self
    }

    /// Specify a map of `name → value` pairs that should be attached as custom
    /// properties on the module value *before* the Starlark file is executed.
    pub fn set_properties(mut self, props: SmallMap<String, InputValue>) -> Self {
        self.properties = Some(props);
        self
    }

    /// Evaluate the configured module. All required fields must be provided
    /// beforehand via the corresponding setters. When a required field is
    /// missing this function returns a failed [`WithDiagnostics`].
    pub fn eval(mut self) -> WithDiagnostics<EvalOutput> {
        // Make sure a source path is set.
        let source_path = match self.source_path {
            Some(ref path) => path,
            None => {
                return WithDiagnostics::failure(vec![Diagnostic::from_error(
                    anyhow::anyhow!("source_path not set on Context before eval()").into(),
                )]);
            }
        };

        // Get or create a default file provider if none was set
        let file_provider = self
            .file_provider
            .clone()
            .unwrap_or_else(|| default_file_provider());

        // Fetch contents: prefer explicit override, otherwise read from disk.
        let contents_owned = match &self.contents {
            Some(c) => c.clone(),
            None => match file_provider.read_file(source_path) {
                Ok(c) => {
                    // Cache the read contents for subsequent accesses.
                    self.contents = Some(c.clone());
                    c
                }
                Err(err) => {
                    let diag = crate::Diagnostic::from_error(starlark::Error::new_other(
                        anyhow::anyhow!("Failed to read file: {}", err),
                    ));
                    return WithDiagnostics::failure(vec![diag]);
                }
            },
        };

        // Cache provided contents in `open_files` so that nested `load()` calls see the
        // latest buffer state rather than potentially stale on-disk contents.
        if let Ok(mut state) = self.state.lock() {
            state
                .file_contents
                .insert(source_path.clone(), contents_owned.clone());
        }

        let ast_res = AstModule::parse(
            source_path.to_str().expect("path is not a string"),
            contents_owned.to_string(),
            &self.dialect(),
        );

        let eval_res = match ast_res {
            Ok(ast) => WithDiagnostics::success(ast, Vec::new()),
            Err(err) => WithDiagnostics::failure(vec![crate::Diagnostic::from_eval_message(
                EvalMessage::from_error(source_path, &err),
            )]),
        };

        eval_res.flat_map(|ast| {
            // Create a print handler to collect output
            let print_handler = CollectingPrintHandler::new();

            let eval_result = {
                let mut eval = Evaluator::new(&self.module);
                eval.enable_static_typechecking(true);
                eval.set_loader(&self);
                eval.set_print_handler(&print_handler);

                // Attach a `ContextValue` so user code can access evaluation context.
                self.module
                    .set_extra_value(eval.heap().alloc_complex(ContextValue::from_context(&self)));

                // If the caller supplied custom properties via `set_properties()` attach them to the
                // freshly created `ModuleValue` *before* executing the module body so that user code
                // can observe/override them as needed.
                if let Some(props) = &self.properties {
                    if let Some(ctx_val) = eval
                        .module()
                        .extra_value()
                        .and_then(|e| e.downcast_ref::<ContextValue>())
                    {
                        for (key, iv) in props.iter() {
                            match iv.to_value(&mut eval, None) {
                                Ok(val) => ctx_val.add_property(key.clone(), val),
                                Err(e) => {
                                    return WithDiagnostics::failure(vec![
                                        crate::Diagnostic::from_error(e.into()),
                                    ]);
                                }
                            }
                        }
                    }
                }

                let globals = Self::build_globals();

                // We are only interested in whether evaluation succeeded, not in the
                // value of the final expression, so map the result to `()`.
                eval.eval_module(ast.clone(), &globals).map(|_| ())
            };

            // Collect print output after evaluation
            let print_output = print_handler.take_output();

            let result = match eval_result {
                Ok(_) => {
                    let frozen_module = self.module.freeze().expect("failed to freeze module");
                    let extra = frozen_module
                        .extra_value()
                        .expect("extra value should be set before freezing")
                        .downcast_ref::<FrozenContextValue>()
                        .expect("extra value should be a FrozenContextValue");
                    let mut diagnostics = extra.diagnostics().clone();
                    diagnostics.extend(self.diagnostics.borrow().clone());

                    // Create a heap for type introspection
                    let heap = Heap::new();

                    let signature = extra
                        .used_inputs
                        .iter()
                        .map(|(name, frozen_type_value)| {
                            use crate::lang::type_info::{ParameterInfo, TypeInfo};

                            // Convert frozen value to regular value for introspection
                            let type_value = frozen_type_value.to_value();
                            let type_info = TypeInfo::from_value(type_value, &heap);

                            let param_info = ParameterInfo {
                                name: name.clone(),
                                type_info,
                                required: true,      // For now, all are required
                                default_value: None, // We don't have default value info yet
                            };

                            (name.clone(), param_info)
                        })
                        .collect();

                    WithDiagnostics::success(
                        EvalOutput {
                            ast,
                            star_module: frozen_module,
                            sch_module: extra.module.clone(),
                            signature,
                            print_output,
                        },
                        diagnostics,
                    )
                }
                Err(err) => {
                    let mut diagnostics = vec![crate::Diagnostic::from_error(err)];
                    diagnostics.extend(self.diagnostics.borrow().clone());
                    WithDiagnostics::failure(diagnostics)
                }
            };

            result
        })
    }

    /// Introspect a module by evaluating it with empty inputs and non-strict IO config.
    /// Returns the used inputs and their types.
    pub fn introspect_module(
        &self,
        source_path: &Path,
        module_name: &str,
    ) -> WithDiagnostics<HashMap<String, String>> {
        self.child_context()
            .set_source_path(source_path.to_path_buf())
            .set_module_name(module_name.to_string())
            .set_inputs(InputMap::new()) // Empty inputs
            .set_strict_io_config(false) // Non-strict so we don't fail on missing inputs
            .eval()
            .map(|output| {
                output
                    .signature
                    .iter()
                    .map(|(k, v)| (k.clone(), format!("{:?}", v.type_info))) // Use Debug formatting for now
                    .collect()
            })
    }

    /// Introspect a module and return structured type information.
    /// This is a richer API that returns TypeInfo instead of just strings.
    pub fn introspect_module_typed(
        &self,
        source_path: &Path,
        module_name: &str,
    ) -> WithDiagnostics<Vec<crate::lang::type_info::ParameterInfo>> {
        // First evaluate the module with empty inputs to get the used inputs
        let eval_result = self
            .child_context()
            .set_source_path(source_path.to_path_buf())
            .set_module_name(module_name.to_string())
            .set_inputs(InputMap::new())
            .set_strict_io_config(false)
            .eval();

        match eval_result.output {
            Some(output) => {
                // Convert the signature HashMap to a Vec of ParameterInfo
                let parameters: Vec<_> = output.signature.into_values().collect();

                WithDiagnostics::success(parameters, eval_result.diagnostics)
            }
            None => {
                // If evaluation failed, return empty parameters with diagnostics
                WithDiagnostics::failure(eval_result.diagnostics)
            }
        }
    }

    /// Get the file contents from the in-memory cache
    pub fn get_file_contents(&self, path: &Path) -> Option<String> {
        if let Ok(state) = self.state.lock() {
            state.file_contents.get(path).cloned()
        } else {
            None
        }
    }

    /// Set file contents in the in-memory cache
    pub fn set_file_contents(&self, path: PathBuf, contents: String) {
        if let Ok(mut state) = self.state.lock() {
            state.file_contents.insert(path, contents);
        }
    }

    /// Get all symbols for a file
    pub fn get_symbols_for_file(&self, path: &Path) -> Option<HashMap<String, crate::SymbolInfo>> {
        if let Ok(state) = self.state.lock() {
            state.symbol_meta.get(path).cloned()
        } else {
            None
        }
    }

    /// Get the symbol index for a file (symbol name -> target path)
    pub fn get_symbol_index(&self, path: &Path) -> Option<HashMap<String, PathBuf>> {
        if let Ok(state) = self.state.lock() {
            state.symbol_index.get(path).cloned()
        } else {
            None
        }
    }

    /// Get module dependencies for a file
    pub fn get_module_dependencies(&self, path: &Path) -> Option<HashSet<PathBuf>> {
        if let Ok(state) = self.state.lock() {
            state.module_deps.get(path).cloned()
        } else {
            None
        }
    }

    /// Parse and analyze a file, updating the symbol index and metadata
    pub fn parse_and_analyze_file(
        &self,
        path: PathBuf,
        contents: String,
    ) -> WithDiagnostics<Option<AstModule>> {
        // Update the in-memory file contents
        self.set_file_contents(path.clone(), contents.clone());

        // Evaluate the file
        let result = self
            .child_context()
            .set_source_path(path.clone())
            .set_module_name("<root>")
            .set_source_contents(contents)
            .eval();

        // Extract symbol information
        if let Some(ref output) = result.output {
            let mut symbol_index: HashMap<String, PathBuf> = HashMap::new();
            let mut symbol_params: HashMap<String, Vec<String>> = HashMap::new();
            let mut symbol_meta: HashMap<String, crate::SymbolInfo> = HashMap::new();

            let names = output.star_module.names().collect::<Vec<_>>();

            for name_val in names {
                let name_str = name_val.as_str();

                if let Ok(Some(owned_val)) = output.star_module.get_option(name_str) {
                    let value = owned_val.value();

                    // ModuleLoader → .zen file
                    if let Some(loader) = value.downcast_ref::<ModuleLoader>() {
                        let mut p = PathBuf::from(&loader.source_path);
                        // If the path is relative, resolve it against the directory of
                        // the Starlark file we are currently parsing.
                        if p.is_relative() {
                            if let Some(parent) = path.parent() {
                                p = parent.join(&p);
                            }
                        }

                        // Get or create a default file provider if none was set
                        let file_provider = self
                            .file_provider
                            .clone()
                            .unwrap_or_else(|| default_file_provider());

                        if let Ok(canon) = file_provider.canonicalize(&p) {
                            p = canon;
                        }

                        // Record dependency for propagation.
                        self.record_module_dependency(path.as_path(), &p);

                        symbol_index.insert(name_str.to_string(), p.clone());

                        // Record parameter list for signature helpers.
                        if !loader.params.is_empty() {
                            symbol_params.insert(name_str.to_string(), loader.params.clone());
                        }

                        // Build SymbolInfo
                        let info = crate::SymbolInfo {
                            kind: crate::SymbolKind::Module,
                            parameters: Some(loader.params.clone()),
                            source_path: Some(p),
                            type_name: "ModuleLoader".to_string(),
                            documentation: None,
                        };
                        symbol_meta.insert(name_str.to_string(), info);
                    } else {
                        // Build SymbolInfo for other types
                        let typ = value.get_type();
                        let kind = match typ {
                            "NativeFunction" | "function" | "FrozenNativeFunction" => {
                                crate::SymbolKind::Function
                            }
                            "ComponentFactory" | "ComponentType" => crate::SymbolKind::Component,
                            "InterfaceFactory" => crate::SymbolKind::Interface,
                            "ModuleLoader" => crate::SymbolKind::Module,
                            _ => crate::SymbolKind::Variable,
                        };

                        let params = symbol_params.get(name_str).cloned();

                        let info = crate::SymbolInfo {
                            kind,
                            parameters: params,
                            source_path: None,
                            type_name: typ.to_string(),
                            documentation: None,
                        };
                        symbol_meta.insert(name_str.to_string(), info);
                    }
                }
            }

            // Store/update the maps for this file.
            if let Ok(mut state) = self.state.lock() {
                if !symbol_index.is_empty() {
                    state.symbol_index.insert(path.clone(), symbol_index);
                }

                if !symbol_params.is_empty() {
                    state.symbol_params.insert(path.clone(), symbol_params);
                }

                if !symbol_meta.is_empty() {
                    state.symbol_meta.insert(path.clone(), symbol_meta);
                }
            }
        }

        result.map(|output| Some(output.ast))
    }

    /// Get the frozen module for a file if it has been evaluated
    pub fn get_environment(&self, _path: &Path) -> Option<starlark::environment::FrozenModule> {
        // This would need to be implemented to track evaluated modules
        // For now, return None
        None
    }

    /// Get the URL for a global symbol (for go-to-definition)
    pub fn get_url_for_global_symbol(&self, current_file: &Path, symbol: &str) -> Option<PathBuf> {
        if let Ok(state) = self.state.lock() {
            if let Some(map) = state.symbol_index.get(current_file) {
                map.get(symbol).cloned()
            } else {
                None
            }
        } else {
            None
        }
    }

    /// Get hover information for a value
    pub fn get_hover_for_value(
        &self,
        current_file: &Path,
        symbol: &str,
    ) -> Option<crate::SymbolInfo> {
        self.get_symbol_info(current_file, symbol)
    }

    /// Get documentation for a builtin symbol
    pub fn get_builtin_docs(&self, symbol: &str) -> Option<String> {
        self.builtin_docs.get(symbol).cloned()
    }

    /// Check if eager workspace parsing is enabled
    pub fn is_eager(&self) -> bool {
        self.eager
    }

    /// Find all Starlark files in the given workspace roots
    #[cfg(feature = "native")]
    pub fn find_workspace_files(
        &self,
        workspace_roots: &[PathBuf],
    ) -> anyhow::Result<Vec<PathBuf>> {
        let mut files = Vec::new();

        // Get or create a default file provider if none was set
        let file_provider = self
            .file_provider
            .clone()
            .unwrap_or_else(|| default_file_provider());

        for root in workspace_roots {
            if !file_provider.exists(root) {
                continue;
            }

            for entry in walkdir::WalkDir::new(root)
                .into_iter()
                .filter_map(Result::ok)
            {
                if entry.file_type().is_file() {
                    let path = entry.into_path();
                    let ext = path.extension().and_then(|e| e.to_str());
                    let file_name = path.file_name().and_then(|e| e.to_str());
                    let is_starlark =
                        matches!((ext, file_name), (Some("star"), _) | (Some("zen"), _));
                    if is_starlark {
                        files.push(path);
                    }
                }
            }
        }
        Ok(files)
    }

    /// Find the span of a load statement that loads the given path
    pub fn find_load_span_for_path(&self, path: &str) -> Option<starlark::codemap::Span> {
        // We need access to the AST of the current module being evaluated
        // This is a bit tricky since we're in the middle of evaluation
        // For now, we'll try to parse the contents if available

        if let (Some(source_path), Some(contents)) = (&self.source_path, &self.contents) {
            // Try to parse the AST to find load statements
            if let Ok(ast) = AstModule::parse(
                &source_path.to_string_lossy(),
                contents.clone(),
                &self.dialect(),
            ) {
                // Get all load statements
                let loads = ast.loads();

                // Find a load statement that matches our path
                for load in loads {
                    if load.module_id == path {
                        return Some(load.span.span);
                    }
                }
            }
        }

        None
    }

    /// Get the codemap for the current module being evaluated
    pub fn get_codemap(&self) -> Option<starlark::codemap::CodeMap> {
        if let (Some(source_path), Some(contents)) = (&self.source_path, &self.contents) {
            Some(starlark::codemap::CodeMap::new(
                source_path.to_string_lossy().to_string(),
                contents.clone(),
            ))
        } else {
            None
        }
    }

    /// Get the source path of the current module being evaluated
    pub fn get_source_path(&self) -> Option<&Path> {
        self.source_path.as_deref()
    }

    /// Get the load resolver if available
    pub fn get_load_resolver(&self) -> Option<&Arc<dyn crate::LoadResolver>> {
        self.load_resolver.as_ref()
    }
}

// Add FileLoader implementation so that Starlark `load()` works when evaluating modules.
impl FileLoader for EvalContext {
    fn load(&self, path: &str) -> starlark::Result<starlark::environment::FrozenModule> {
        log::debug!(
            "Trying to load path {path} with current path {:?}",
            self.source_path
        );

        // Get or create default providers if none were set
        let file_provider = self
            .file_provider
            .clone()
            .unwrap_or_else(|| default_file_provider());

        let load_resolver = self
            .load_resolver
            .clone()
            .ok_or_else(|| starlark::Error::new_other(anyhow!("No LoadResolver provided")))?;

        let module_path = self.source_path.clone();

        // Resolve the load path to an absolute path
        let absolute_path = match module_path {
            Some(ref current_file) => load_resolver
                .resolve_path(file_provider.deref(), path, current_file)
                .map_err(starlark::Error::new_other)?,
            None => {
                return Err(starlark::Error::new_other(anyhow::anyhow!(
                    "Cannot resolve load path '{}' without a current file context",
                    path
                )));
            }
        };

        // Canonicalize the path for cache lookup
        let canonical_path = file_provider
            .canonicalize(&absolute_path)
            .unwrap_or(absolute_path.clone());

        // Create a LoadGuard to prevent cyclic imports
        let source_path = self
            .source_path
            .clone()
            .unwrap_or_else(|| PathBuf::from("<unknown>"));
        let _guard = LoadGuard::new(self.state.clone(), canonical_path.clone(), source_path)?;

        // Fast path: if we've already loaded (and frozen) this module once
        // within the current evaluation context, simply return the cached
        // instance so that callers share the same definitions.
        if let Some(frozen) = self.state.lock().unwrap().load_cache.get(&canonical_path) {
            return Ok(frozen.clone());
        }

        // Special-case: if the load path refers to a *directory* treat it as a
        // namespace that exports one `ModuleLoader` per `.zen` file found
        // inside that directory.
        if file_provider.is_directory(&canonical_path) {
            let (frozen, errors_by_symbol) =
                match self.load_directory_as_module(&canonical_path, path) {
                    Ok(result) => result,
                    Err(e) => {
                        // Find the load statement that triggered this error and attach proper span
                        if let Some(load_span) = self.find_load_span_for_path(path) {
                            if let Some(codemap) = self.get_codemap() {
                                let mut err = starlark::Error::new_other(anyhow::anyhow!(
                                    "Failed to load directory as module: {}",
                                    e
                                ));
                                err.set_span(load_span, &codemap);
                                return Err(err);
                            }
                        }
                        return Err(e);
                    }
                };

            // If there were errors loading any symbols, return the first one with proper span
            if !errors_by_symbol.is_empty() {
                // Find the first error to return - sort by symbol name for deterministic behavior
                let mut sorted_symbols: Vec<_> = errors_by_symbol.keys().collect();
                sorted_symbols.sort();

                // Find the first error and its associated symbol
                let mut error_symbol = "<unknown>";
                let first_error = sorted_symbols
                    .iter()
                    .filter_map(|&symbol| {
                        errors_by_symbol.get(symbol).and_then(|errors| {
                            if !errors.is_empty() {
                                error_symbol = symbol;
                                Some(errors.iter().next())
                            } else {
                                None
                            }
                        })
                    })
                    .flatten()
                    .next();

                if let Some(error) = first_error {
                    // Construct the full path including the file that caused the error
                    let error_path = if error_symbol != "<unknown>" {
                        // Check if the error is from a .zen file or .kicad_sym file
                        if error.path.ends_with(".kicad_sym") {
                            format!("{path}/{error_symbol}.kicad_sym")
                        } else {
                            format!("{path}/{error_symbol}.zen")
                        }
                    } else {
                        path.to_string()
                    };

                    // Try to find the load statement span to attach to the error
                    if let Some(load_span) = self.find_load_span_for_path(path) {
                        if let Some(codemap) = self.get_codemap() {
                            // Create a parent diagnostic that wraps the child error
                            let parent_diag = crate::Diagnostic {
                                path: self
                                    .source_path
                                    .as_ref()
                                    .map(|p| p.to_string_lossy().to_string())
                                    .unwrap_or_default(),
                                span: Some(codemap.file_span(load_span).resolve_span()),
                                severity: starlark::analysis::EvalSeverity::Error,
                                body: format!("Error loading module `{error_path}`"),
                                call_stack: None,
                                child: Some(Box::new(error.clone())),
                            };

                            // Wrap in DiagnosticError and pass through anyhow
                            let diag_err = crate::DiagnosticError(parent_diag);
                            let load_err = crate::LoadError {
                                message: format!("Error loading module `{error_path}`"),
                                diagnostic: diag_err,
                            };
                            let mut err = starlark::Error::new_other(anyhow::Error::new(load_err));
                            err.set_span(load_span, &codemap);
                            return Err(err);
                        }
                    }
                    // Fallback: return error without span
                    let parent_diag = crate::Diagnostic {
                        path: self
                            .source_path
                            .as_ref()
                            .map(|p| p.to_string_lossy().to_string())
                            .unwrap_or_default(),
                        span: None,
                        severity: starlark::analysis::EvalSeverity::Error,
                        body: format!("Error loading module `{error_path}`"),
                        call_stack: None,
                        child: Some(Box::new(error.clone())),
                    };
                    let diag_err = crate::DiagnosticError(parent_diag);
                    let load_err = crate::LoadError {
                        message: format!("Error loading module `{error_path}`"),
                        diagnostic: diag_err,
                    };
                    let err = starlark::Error::new_other(anyhow::Error::new(load_err));
                    return Err(err);
                }
            }

            self.state
                .lock()
                .unwrap()
                .load_cache
                .insert(canonical_path.clone(), frozen.clone());
            return Ok(frozen);
        }

        let result = self
            .child_context()
            .set_source_path(canonical_path.clone())
            .eval();

        // If there were any error diagnostics, return the first one
        if let Some(first_error) = result.diagnostics.iter().find(|d| d.is_error()) {
            // Try to attach the error to the load statement span
            if let Some(load_span) = self.find_load_span_for_path(path) {
                if let Some(codemap) = self.get_codemap() {
                    // Create a parent diagnostic that wraps the child error
                    let parent_diag = crate::Diagnostic {
                        path: self
                            .source_path
                            .as_ref()
                            .map(|p| p.to_string_lossy().to_string())
                            .unwrap_or_default(),
                        span: Some(codemap.file_span(load_span).resolve_span()),
                        severity: starlark::analysis::EvalSeverity::Error,
                        body: format!("Error loading module `{path}`"),
                        call_stack: None,
                        child: Some(Box::new(first_error.clone())),
                    };

                    // Wrap in DiagnosticError and pass through anyhow
                    let diag_err = crate::DiagnosticError(parent_diag);
                    let load_err = crate::LoadError {
                        message: format!("Error loading module `{path}`"),
                        diagnostic: diag_err,
                    };
                    let mut err = starlark::Error::new_other(anyhow::Error::new(load_err));
                    err.set_span(load_span, &codemap);
                    return Err(err);
                } else {
                    let diag = crate::Diagnostic {
                        path: self
                            .source_path
                            .as_ref()
                            .map(|p| p.to_string_lossy().to_string())
                            .unwrap_or_default(),
                        span: None,
                        severity: starlark::analysis::EvalSeverity::Error,
                        body: format!("Failed to load module `{path}`"),
                        call_stack: None,
                        child: None,
                    };
                    let diag_err = crate::DiagnosticError(diag);
                    let load_err = crate::LoadError {
                        message: format!("Failed to load module `{path}`"),
                        diagnostic: diag_err,
                    };
                    let err = starlark::Error::new_other(anyhow::Error::new(load_err));
                    return Err(err);
                }
            }
        }

        // Cache the result if successful
        if let Some(output) = result.output {
            let frozen = output.star_module;
            self.state
                .lock()
                .unwrap()
                .load_cache
                .insert(canonical_path, frozen.clone());
            Ok(frozen)
        } else {
            // No specific error diagnostic but evaluation failed
            if let Some(load_span) = self.find_load_span_for_path(path) {
                if let Some(codemap) = self.get_codemap() {
                    let diag = crate::Diagnostic {
                        path: self
                            .source_path
                            .as_ref()
                            .map(|p| p.to_string_lossy().to_string())
                            .unwrap_or_default(),
                        span: Some(codemap.file_span(load_span).resolve_span()),
                        severity: starlark::analysis::EvalSeverity::Error,
                        body: format!("Failed to load module `{path}`"),
                        call_stack: None,
                        child: None,
                    };
                    let diag_err = crate::DiagnosticError(diag);
                    let load_err = crate::LoadError {
                        message: format!("Failed to load module `{path}`"),
                        diagnostic: diag_err,
                    };
                    let mut err = starlark::Error::new_other(anyhow::Error::new(load_err));
                    err.set_span(load_span, &codemap);
                    return Err(err);
                }
            }
            let diag = crate::Diagnostic {
                path: self
                    .source_path
                    .as_ref()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default(),
                span: None,
                severity: starlark::analysis::EvalSeverity::Error,
                body: format!("Failed to load module `{path}`"),
                call_stack: None,
                child: None,
            };
            let diag_err = crate::DiagnosticError(diag);
            let load_err = crate::LoadError {
                message: format!("Failed to load module `{path}`"),
                diagnostic: diag_err,
            };
            let err = starlark::Error::new_other(anyhow::Error::new(load_err));
            Err(err)
        }
    }
}
