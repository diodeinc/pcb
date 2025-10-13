use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use anyhow::anyhow;
use starlark::{codemap::ResolvedSpan, collections::SmallMap};
use starlark::{environment::FrozenModule, typing::Interface};
use starlark::{
    environment::{GlobalsBuilder, LibraryExtension},
    errors::{EvalMessage, EvalSeverity},
    eval::{Evaluator, FileLoader},
    syntax::{AstModule, Dialect},
    typing::TypeMap,
    values::{FrozenValue, Heap, Value, ValueLike},
    PrintHandler,
};

use crate::lang::assert::assert_globals;
use crate::lang::file::file_globals;
use crate::lang::spice_model::model_globals;
use crate::lang::{
    builtin::builtin_globals,
    component::component_globals,
    physical::*,
    type_info::{ParameterInfo, TypeInfo},
};
use crate::{Diagnostic, WithDiagnostics};

use super::{
    context::{ContextValue, FrozenContextValue},
    interface::interface_globals,
    module::{module_globals, FrozenModuleValue, ModuleLoader},
    test_bench::test_bench_globals,
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

#[derive(Clone)]
pub struct EvalOutput {
    pub ast: AstModule,
    pub star_module: FrozenModule,
    pub sch_module: FrozenModuleValue,
    /// Ordered list of parameter information
    pub signature: Vec<ParameterInfo>,
    /// Print output collected during evaluation
    pub print_output: Vec<String>,
    /// Load resolver used for this evaluation, when available
    pub load_resolver: Arc<dyn crate::LoadResolver>,
}

impl EvalOutput {
    /// If the underlying resolver is a CoreLoadResolver, return a reference to it
    pub fn core_resolver(&self) -> Option<Arc<crate::CoreLoadResolver>> {
        let load_resolver = self.load_resolver.clone();
        (load_resolver as Arc<dyn std::any::Any + Send + Sync>)
            .downcast::<crate::CoreLoadResolver>()
            .ok()
    }
}

#[derive(Default)]
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
    load_cache: HashMap<PathBuf, EvalOutput>,

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

    /// Additional diagnostics from this evaluation context that will be merged with any other
    /// diagnostics attached to the ContextValue.
    diagnostics: RefCell<Vec<Diagnostic>>,

    /// Load resolver for resolving load() paths
    pub(crate) load_resolver: Arc<dyn crate::LoadResolver>,

    /// Index to track which load statement we're currently processing (for span resolution)
    current_load_index: RefCell<usize>,
}

// impl Default for EvalContext {
//     fn default() -> Self {
//         Self::new()
//     }
// }

/// Helper to recursively convert JSON to heap values
fn json_value_to_heap_value<'v>(
    json: &serde_json::Value,
    heap: &'v Heap,
) -> anyhow::Result<Value<'v>> {
    use starlark::values::dict::AllocDict;
    match json {
        serde_json::Value::Null => Ok(Value::new_none()),
        serde_json::Value::Bool(b) => Ok(Value::new_bool(*b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(heap.alloc(i as i32))
            } else if let Some(f) = n.as_f64() {
                Ok(heap.alloc(starlark::values::float::StarlarkFloat(f)))
            } else {
                Err(anyhow::anyhow!("Invalid number"))
            }
        }
        serde_json::Value::String(s) => Ok(heap.alloc_str(s).to_value()),
        serde_json::Value::Array(arr) => {
            let mut values = Vec::new();
            for item in arr {
                values.push(json_value_to_heap_value(item, heap)?);
            }
            Ok(heap.alloc(values))
        }
        serde_json::Value::Object(obj) => {
            let mut pairs = Vec::new();
            for (k, v) in obj {
                let val = json_value_to_heap_value(v, heap)?;
                pairs.push((heap.alloc_str(k).to_value(), val));
            }
            Ok(heap.alloc(AllocDict(pairs)))
        }
    }
}

impl EvalContext {
    pub fn new(load_resolver: Arc<dyn crate::LoadResolver>) -> Self {
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
            diagnostics: RefCell::new(Vec::new()),
            load_resolver,
            current_load_index: RefCell::new(0),
        }
    }

    pub fn file_provider(&self) -> &dyn crate::FileProvider {
        self.load_resolver.file_provider()
    }

    /// Set the file provider for this context
    pub fn set_load_resolver(mut self, load_resolver: Arc<dyn crate::LoadResolver>) -> Self {
        self.load_resolver = load_resolver;
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
            diagnostics: RefCell::new(Vec::new()),
            load_resolver: self.load_resolver.clone(),
            current_load_index: RefCell::new(0),
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
            LibraryExtension::Typing,
            LibraryExtension::StructType,
            LibraryExtension::Print,
            LibraryExtension::Debug,
            LibraryExtension::Partial,
            LibraryExtension::Breakpoint,
            LibraryExtension::SetType,
            LibraryExtension::Json,
        ])
        .with(builtin_globals)
        .with(component_globals)
        .with(module_globals)
        .with(interface_globals)
        .with(assert_globals)
        .with(file_globals)
        .with(model_globals)
        .with(test_bench_globals)
        .build()
    }

    /// Record that `from` references `to` via a `Module()` call.
    pub(crate) fn record_module_dependency(&self, from: &Path, to: &Path) {
        if let Ok(mut state) = self.state.lock() {
            let entry = state.module_deps.entry(from.to_path_buf()).or_default();
            entry.insert(to.to_path_buf());
        }
    }

    /// Get a cached frozen module if it exists
    pub fn get_cached_module(&self, path: &Path) -> Option<EvalOutput> {
        if let Ok(state) = self.state.lock() {
            state.load_cache.get(path).cloned()
        } else {
            None
        }
    }

    /// Cache a frozen module
    pub fn cache_module(&self, path: PathBuf, module: EvalOutput) {
        if let Ok(mut state) = self.state.lock() {
            state.load_cache.insert(path, module);
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

    /// Set inputs from already frozen parent values.
    pub fn set_inputs_from_frozen_values(
        &mut self,
        parent_inputs: SmallMap<String, FrozenValue>,
    ) -> starlark::Result<()> {
        let eval = Evaluator::new(&self.module);
        if self.module.extra_value().is_none() {
            let ctx_value = eval.heap().alloc_complex(ContextValue::from_context(self));
            self.module.set_extra_value(ctx_value);
        }
        let extra_value = self.module.extra_value().unwrap();
        let ctx_value = extra_value.downcast_ref::<ContextValue>().unwrap();

        let mut module = ctx_value.module_mut();
        for (name, value) in parent_inputs.into_iter() {
            module.add_input(name, value.to_value());
        }

        Ok(())
    }

    /// Set properties from already frozen parent values.
    pub fn set_properties_from_frozen_values(
        &mut self,
        parent_properties: SmallMap<String, FrozenValue>,
    ) -> starlark::Result<()> {
        let eval = Evaluator::new(&self.module);
        if self.module.extra_value().is_none() {
            let ctx_value = eval.heap().alloc_complex(ContextValue::from_context(self));
            self.module.set_extra_value(ctx_value);
        }
        let extra_value = self.module.extra_value().unwrap();
        let ctx_value = extra_value.downcast_ref::<ContextValue>().unwrap();

        for (name, value) in parent_properties.into_iter() {
            ctx_value.add_property(name, value.to_value());
        }

        Ok(())
    }

    /// Convert JSON inputs directly to heap values and set them (for external APIs)
    pub fn set_json_inputs(
        &mut self,
        json_inputs: SmallMap<String, serde_json::Value>,
    ) -> anyhow::Result<()> {
        let eval = Evaluator::new(&self.module);
        if self.module.extra_value().is_none() {
            let ctx_value = eval.heap().alloc_complex(ContextValue::from_context(self));
            self.module.set_extra_value(ctx_value);
        }
        let extra_value = self.module.extra_value().unwrap();
        let ctx_value = extra_value.downcast_ref::<ContextValue>().unwrap();

        let mut module = ctx_value.module_mut();
        for (name, json) in json_inputs.iter() {
            let value = json_value_to_heap_value(json, eval.heap())?;
            module.add_input(name.clone(), value);
        }

        Ok(())
    }

    /// Evaluate the configured module. All required fields must be provided
    /// beforehand via the corresponding setters. When a required field is
    /// missing this function returns a failed [`WithDiagnostics`].
    pub fn eval(mut self) -> WithDiagnostics<EvalOutput> {
        // Make sure a source path is set.
        let source_path = match self.source_path {
            Some(ref path) => path,
            None => {
                return anyhow::anyhow!("source_path not set on Context before eval()").into();
            }
        };

        self.load_resolver.track_file(source_path);

        // Fetch contents: prefer explicit override, otherwise read from disk.
        let contents_owned = match &self.contents {
            Some(c) => c.clone(),
            None => match self.file_provider().read_file(source_path) {
                Ok(c) => {
                    // Cache the read contents for subsequent accesses.
                    self.contents = Some(c.clone());
                    c
                }
                Err(err) => {
                    return anyhow::anyhow!("Failed to read file: {}", err).into();
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

        let ast = match ast_res {
            Ok(ast) => ast,
            Err(err) => return EvalMessage::from_error(source_path, &err).into(),
        };

        // Create a print handler to collect output
        let print_handler = CollectingPrintHandler::new();

        let eval_result = {
            let mut eval = Evaluator::new(&self.module);
            eval.enable_static_typechecking(true);
            eval.set_loader(&self);
            eval.set_print_handler(&print_handler);

            // Attach a `ContextValue` so user code can access evaluation context.
            // Only create one if it doesn't already exist (copy_and_set_inputs/properties may have created it)
            if self.module.extra_value().is_none() {
                self.module
                    .set_extra_value(eval.heap().alloc_complex(ContextValue::from_context(&self)));
            }

            let globals = Self::build_globals();

            // We are only interested in whether evaluation succeeded, not in the
            // value of the final expression, so map the result to `()`.
            eval.eval_module(ast.clone(), &globals).map(|_| ())
        };

        // Collect print output after evaluation
        let print_output = print_handler.take_output();

        match eval_result {
            Ok(_) => {
                self.hijack_builtins();
                let frozen_module = self.module.freeze().expect("failed to freeze module");
                let extra = frozen_module
                    .extra_value()
                    .expect("extra value should be set before freezing")
                    .downcast_ref::<FrozenContextValue>()
                    .expect("extra value should be a FrozenContextValue");
                let signature = extra
                    .module
                    .signature()
                    .iter()
                    .map(|param| {
                        // Convert frozen value to regular value for introspection
                        let type_value = param.type_value.to_value();
                        let type_info = TypeInfo::from_value(type_value);

                        // Convert default value to JSON using Starlark's native serialization
                        let default_value = param
                            .default_value
                            .as_ref()
                            .and_then(|v| v.to_value().to_json_value().ok());

                        ParameterInfo {
                            name: param.name.clone(),
                            type_info,
                            required: !param.optional,
                            default_value,
                            help: param.help.clone(),
                        }
                    })
                    .collect();

                let output = EvalOutput {
                    ast,
                    star_module: frozen_module,
                    sch_module: extra.module.clone(),
                    signature,
                    print_output,
                    load_resolver: self.load_resolver.clone(),
                };
                let mut ret = WithDiagnostics::success(output);
                ret.diagnostics.extend(extra.diagnostics().clone());
                ret.diagnostics.extend(self.diagnostics.borrow().clone());
                ret
            }
            Err(err) => {
                let mut ret = WithDiagnostics::default();
                ret.diagnostics.extend(self.diagnostics.borrow().clone());
                ret.diagnostics.push(err.into());
                ret
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

                        if let Ok(canon) = self.file_provider().canonicalize(&p) {
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
    pub fn get_environment(&self, _path: &Path) -> Option<FrozenModule> {
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

        for root in workspace_roots {
            if !self.file_provider().exists(root) {
                continue;
            }

            // Skip hidden directories and files (those whose name starts with ".").
            // Using filter_entry ensures we don't descend into hidden directories.
            let iter = walkdir::WalkDir::new(root).into_iter().filter_entry(|e| {
                if let Some(name) = e.file_name().to_str() {
                    // Keep entries whose immediate name does not start with a dot
                    return !name.starts_with('.');
                }
                true
            });

            for entry in iter.filter_map(Result::ok) {
                if entry.file_type().is_file() {
                    let path = entry.into_path();
                    let ext = path.extension().and_then(|e| e.to_str());
                    let file_name = path.file_name().and_then(|e| e.to_str());
                    // Also skip files whose own name starts with a dot to be safe
                    let is_hidden = file_name.map(|n| n.starts_with('.')).unwrap_or(false);
                    if is_hidden {
                        continue;
                    }
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

    /// Parse the current module's AST, returning None if parsing fails
    fn parse_current_ast(&self) -> Option<starlark::syntax::AstModule> {
        let source_path = self.source_path.as_ref()?;
        let contents = self.contents.as_ref()?;
        starlark::syntax::AstModule::parse(
            &source_path.to_string_lossy(),
            contents.clone(),
            &self.dialect(),
        )
        .ok()
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

    pub fn resolve_load_span(&self, path: &str) -> Option<ResolvedSpan> {
        let codemap = self.get_codemap()?;
        let ast = self.parse_current_ast()?;
        let span = ast
            .loads()
            .into_iter()
            .find(|load| load.module_id == path)
            .map(|load| load.span.span)?;
        Some(codemap.file_span(span).resolve_span())
    }

    fn increment_load_index(&self) {
        let mut index = self.current_load_index.borrow_mut();
        *index += 1;
    }

    /// Get the source path of the current module being evaluated
    pub fn get_source_path(&self) -> Option<&Path> {
        self.source_path.as_deref()
    }

    /// Get the load resolver if available
    pub fn get_load_resolver(&self) -> &Arc<dyn crate::LoadResolver> {
        &self.load_resolver
    }

    /// Append a diagnostic that was produced while this context was active.
    pub fn add_diagnostic<D: Into<Diagnostic>>(&self, diag: D) {
        self.diagnostics.borrow_mut().push(diag.into());
    }

    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        self.diagnostics.borrow().clone()
    }

    fn hijack_builtins(&mut self) {
        let Some(source_path) = self.get_source_path() else {
            return;
        };
        if source_path.file_name().and_then(|name| name.to_str()) != Some("units.zen") {
            return;
        }

        use pcb_sch::PhysicalUnit;
        let heap = self.module.heap();

        if self.module.get("Voltage").is_some() {
            self.module.set(
                "Voltage",
                heap.alloc_simple(PhysicalValueType::new(PhysicalUnit::Volts.into())),
            );
        }
        if self.module.get("Current").is_some() {
            self.module.set(
                "Current",
                heap.alloc_simple(PhysicalValueType::new(PhysicalUnit::Amperes.into())),
            );
        }
        if self.module.get("Resistance").is_some() {
            self.module.set(
                "Resistance",
                heap.alloc_simple(PhysicalValueType::new(PhysicalUnit::Ohms.into())),
            );
        }
        if self.module.get("Capacitance").is_some() {
            self.module.set(
                "Capacitance",
                heap.alloc_simple(PhysicalValueType::new(PhysicalUnit::Farads.into())),
            );
        }
        if self.module.get("Inductance").is_some() {
            self.module.set(
                "Inductance",
                heap.alloc_simple(PhysicalValueType::new(PhysicalUnit::Henries.into())),
            );
        }
        if self.module.get("Frequency").is_some() {
            self.module.set(
                "Frequency",
                heap.alloc_simple(PhysicalValueType::new(PhysicalUnit::Hertz.into())),
            );
        }
        if self.module.get("Time").is_some() {
            self.module.set(
                "Time",
                heap.alloc_simple(PhysicalValueType::new(PhysicalUnit::Seconds.into())),
            );
        }
        if self.module.get("Temperature").is_some() {
            self.module.set(
                "Temperature",
                heap.alloc_simple(PhysicalValueType::new(PhysicalUnit::Kelvin.into())),
            );
        }
        if self.module.get("Charge").is_some() {
            self.module.set(
                "Charge",
                heap.alloc_simple(PhysicalValueType::new(PhysicalUnit::Coulombs.into())),
            );
        }
        if self.module.get("Power").is_some() {
            self.module.set(
                "Power",
                heap.alloc_simple(PhysicalValueType::new(PhysicalUnit::Watts.into())),
            );
        }
        if self.module.get("Energy").is_some() {
            self.module.set(
                "Energy",
                heap.alloc_simple(PhysicalValueType::new(PhysicalUnit::Joules.into())),
            );
        }
        if self.module.get("Conductance").is_some() {
            self.module.set(
                "Conductance",
                heap.alloc_simple(PhysicalValueType::new(PhysicalUnit::Siemens.into())),
            );
        }
        if self.module.get("MagneticFlux").is_some() {
            self.module.set(
                "MagneticFlux",
                heap.alloc_simple(PhysicalValueType::new(PhysicalUnit::Webers.into())),
            );
        }
    }

    pub fn resolve_and_eval_module(
        &self,
        path: &str,
        span: Option<ResolvedSpan>,
    ) -> starlark::Result<EvalOutput> {
        log::debug!(
            "Trying to load path {path} with current path {:?}",
            self.source_path
        );
        let load_resolver = self.load_resolver.clone();
        let file_provider = load_resolver.file_provider();

        let module_path = self.source_path.clone();
        let Some(current_file) = module_path.as_ref() else {
            return Err(starlark::Error::new_other(anyhow::anyhow!(
                "Cannot resolve load path '{}' without a current file context",
                path
            )));
        };

        // Resolve the load path to an absolute path
        let mut resolve_context = load_resolver.resolve_context(path, current_file)?;
        let canonical_path = load_resolver.resolve(&mut resolve_context)?;

        // Create a LoadGuard to prevent cyclic imports
        let source_path = self
            .source_path
            .clone()
            .unwrap_or_else(|| PathBuf::from("<unknown>"));
        let _guard = LoadGuard::new(
            self.state.clone(),
            canonical_path.clone(),
            source_path.clone(),
        )?;

        // Fast path: if we've already loaded (and frozen) this module once
        // within the current evaluation context, simply return the cached
        // instance so that callers share the same definitions.
        if let Some(frozen) = self.get_cached_module(&canonical_path) {
            return Ok(frozen);
        }

        let span = span.or_else(|| self.resolve_load_span(path));
        if let Some(warning_diag) = crate::warnings::check_and_create_unstable_ref_warning(
            load_resolver.as_ref(),
            current_file,
            &resolve_context,
            span,
        ) {
            self.add_diagnostic(warning_diag);
        }

        if file_provider.is_directory(&canonical_path) {
            return Err(starlark::Error::new_other(anyhow::anyhow!(
                "Directory load syntax is no longer supported"
            )));
        }

        let name = canonical_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let result = self
            .child_context()
            .set_source_path(canonical_path.clone())
            .set_module_name(name)
            .eval();

        result.diagnostics.iter().for_each(|diag| {
            if matches!(diag.severity, EvalSeverity::Warning) {
                self.add_diagnostic(crate::Diagnostic {
                    path: source_path.to_string_lossy().to_string(),
                    span,
                    severity: diag.severity,
                    body: format!("Warning from `{path}`"),
                    call_stack: None,
                    child: Some(Box::new(diag.clone())),
                    source_error: None,
                });
            }
        });

        // If there were any error diagnostics, return the first one
        if let Some(first_error) = result.diagnostics.iter().find(|d| d.is_error()) {
            let diagnostic = crate::Diagnostic {
                path: source_path.to_string_lossy().to_string(),
                span,
                severity: starlark::analysis::EvalSeverity::Error,
                body: format!("Error loading module `{path}`"),
                call_stack: None,
                child: Some(Box::new(first_error.clone())),
                source_error: None,
            };
            return Err(diagnostic.into());
        }

        // Cache the result if successful
        if let Some(output) = result.output {
            self.cache_module(canonical_path, output.clone());
            Ok(output)
        } else {
            // No specific error diagnostic but evaluation failed
            let diagnostic = crate::Diagnostic {
                path: source_path.to_string_lossy().to_string(),
                span,
                severity: starlark::analysis::EvalSeverity::Error,
                body: format!("Failed to load module `{path}`"),
                call_stack: None,
                child: None,
                source_error: None,
            };
            Err(diagnostic.into())
        }
    }
}

// Add FileLoader implementation so that Starlark `load()` works when evaluating modules.
impl FileLoader for EvalContext {
    fn load(&self, path: &str) -> starlark::Result<FrozenModule> {
        self.increment_load_index();
        let eval_output = self.resolve_and_eval_module(path, None)?;
        Ok(eval_output.star_module)
    }
}
