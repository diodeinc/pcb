#![allow(clippy::arc_with_non_send_sync)]

use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
};

use anyhow::anyhow;
use starlark::{codemap::ResolvedSpan, collections::SmallMap, values::FrozenHeap};
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

#[cfg(feature = "native")]
use rayon::prelude::*;

use tracing::{info_span, instrument};

use crate::lang::{assert::assert_globals, component::init_net_global};
use crate::lang::{
    builtin::builtin_globals,
    component::component_globals,
    type_info::{ParameterInfo, TypeInfo},
};
use crate::lang::{
    electrical_check::FrozenElectricalCheck,
    evaluator_ext::EvaluatorExt,
    file::file_globals,
    module::{FrozenModuleValue, ModulePath},
};
use crate::{convert::ModuleConverter, lang::context::FrozenPendingChild};
use crate::{Diagnostic, WithDiagnostics};

use super::{
    context::{ContextValue, FrozenContextValue},
    interface::interface_globals,
    module::{module_globals, ModuleLoader},
    spice_model::model_globals,
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
    /// Session keeps the frozen heap alive for the lifetime of this output.
    /// Also provides access to the module tree.
    session: EvalSession,
}

impl EvalOutput {
    /// Get the session (for creating a new EvalContext that shares state with this output).
    pub fn session(&self) -> &EvalSession {
        &self.session
    }

    /// Get the module tree from the session.
    pub fn module_tree(&self) -> BTreeMap<ModulePath, FrozenModuleValue> {
        self.session.clone_module_tree()
    }

    /// Convert to schematic with diagnostics
    pub fn to_schematic_with_diagnostics(&self) -> crate::WithDiagnostics<pcb_sch::Schematic> {
        let converter = ModuleConverter::new();
        converter.build(self.module_tree())
    }

    /// Convert to schematic (error if conversion fails)
    pub fn to_schematic(&self) -> anyhow::Result<pcb_sch::Schematic> {
        let result = self.to_schematic_with_diagnostics();
        match result.output {
            Some(schematic) if !result.diagnostics.has_errors() => Ok(schematic),
            Some(_) => {
                let errors: Vec<String> = result
                    .diagnostics
                    .diagnostics
                    .iter()
                    .map(|d| d.to_string())
                    .collect();
                Err(anyhow::anyhow!(
                    "Schematic conversion had errors:\n{}",
                    errors.join("\n")
                ))
            }
            None => {
                let errors: Vec<String> = result
                    .diagnostics
                    .diagnostics
                    .iter()
                    .map(|d| d.to_string())
                    .collect();
                Err(anyhow::anyhow!(
                    "Schematic conversion failed:\n{}",
                    errors.join("\n")
                ))
            }
        }
    }

    /// Collect all testbenches from all modules in the tree
    pub fn collect_testbenches(&self) -> Vec<crate::lang::test_bench::FrozenTestBenchValue> {
        let mut result = Vec::new();
        let module_tree = self.module_tree();

        // Iterate through all modules in the tree
        for module in module_tree.values() {
            // Get testbenches from this module
            for testbench in module.testbenches() {
                result.push(testbench.clone());
            }
        }

        result
    }

    /// Collect all electrical checks from all modules in the tree
    pub fn collect_electrical_checks(&self) -> Vec<(FrozenElectricalCheck, FrozenModuleValue)> {
        let mut result = Vec::new();
        let module_tree = self.module_tree();
        for module in module_tree.values() {
            for check in module.electrical_checks() {
                result.push((check.clone(), module.clone()));
            }
        }
        result
    }

    /// If the underlying resolver is a CoreLoadResolver, return a reference to it
    pub fn core_resolver(&self) -> Option<Arc<crate::CoreLoadResolver>> {
        let load_resolver = self.load_resolver.clone();
        (load_resolver as Arc<dyn std::any::Any + Send + Sync>)
            .downcast::<crate::CoreLoadResolver>()
            .ok()
    }
}

#[derive(Default)]
struct EvalSessionInner {
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

    /// Tree of all child modules indexed by fully qualified path.
    /// Keys are paths like "root", "root.child"
    /// Components are stored in each module's components field, not in this tree.
    module_tree: BTreeMap<ModulePath, FrozenModuleValue>,

    /// Diagnostics collected during evaluation across all contexts in this session.
    /// Uses BTreeMap for deterministic ordering regardless of parallel execution timing.
    /// Key: (path, span line, span column, body) for stable sorting.
    diagnostics: BTreeMap<DiagnosticKey, Vec<Diagnostic>>,
}

/// Key for ordering diagnostics deterministically: (path, line, column, body)
type DiagnosticKey = (String, Option<usize>, Option<usize>, String);

/// Handle to shared evaluation session state. Cheaply cloneable.
#[derive(Clone)]
pub struct EvalSession {
    inner: Arc<RwLock<EvalSessionInner>>,
    /// Shared frozen heap for the entire evaluation tree.
    /// Separate from inner because FrozenHeap contains RefCell which isn't Sync.
    frozen_heap: Arc<Mutex<FrozenHeap>>,
}

/// Configuration for creating an EvalContext. Send + Sync safe for passing across threads.
/// Use `EvalSession::create_context(config)` to create an EvalContext from this.
#[derive(Clone)]
pub struct EvalConfig {
    /// Documentation source for built-in Starlark symbols keyed by their name.
    /// Wrapped in Arc since it's the same for all contexts.
    pub(crate) builtin_docs: Arc<HashMap<String, String>>,

    /// Load resolver for resolving load() paths
    pub(crate) load_resolver: Arc<dyn crate::LoadResolver>,

    /// The fully qualified path of the module we are evaluating (e.g., "root", "root.child")
    pub(crate) module_path: ModulePath,

    /// Per-context load chain for cycle detection. Contains canonical paths of all files
    /// in the current load chain (ancestors). Thread-local to each evaluation path.
    pub(crate) load_chain: HashSet<PathBuf>,

    /// The absolute path to the module we are evaluating.
    pub(crate) source_path: Option<PathBuf>,

    /// The contents of the module we are evaluating.
    pub(crate) contents: Option<String>,

    /// When `true`, missing required io()/config() placeholders are treated as errors during
    /// evaluation. This is enabled when a module is instantiated via `ModuleLoader`.
    pub(crate) strict_io_config: bool,

    /// When `true`, process pending_children to build the full circuit hierarchy.
    /// False for library loads (introspection only), true for actual circuit builds.
    pub(crate) build_circuit: bool,

    /// When `true`, the surrounding LSP wishes to eagerly parse all files in the workspace.
    /// Defaults to `true` so that features work out-of-the-box.
    pub(crate) eager: bool,
}

impl EvalConfig {
    /// Create a new root EvalConfig with the given load resolver.
    pub fn new(load_resolver: Arc<dyn crate::LoadResolver>) -> Self {
        Self {
            builtin_docs: Arc::new(Self::build_builtin_docs()),
            load_resolver,
            module_path: ModulePath::root(),
            load_chain: HashSet::new(),
            source_path: None,
            contents: None,
            strict_io_config: false,
            build_circuit: false,
            eager: true,
        }
    }

    /// Build the builtin docs map from globals.
    fn build_builtin_docs() -> HashMap<String, String> {
        let globals = EvalContext::build_globals();
        let mut builtin_docs = HashMap::new();
        for (name, item) in globals.documentation().members {
            builtin_docs.insert(name.clone(), item.render_as_code(&name));
        }
        builtin_docs
    }

    /// Set the source path of the module we are evaluating.
    pub fn set_source_path(mut self, path: PathBuf) -> Self {
        self.source_path = Some(path);
        self
    }

    /// Provide the raw contents of the Starlark module.
    pub fn set_source_contents<S: Into<String>>(mut self, contents: S) -> Self {
        self.contents = Some(contents.into());
        self
    }

    /// Enable or disable strict IO/config placeholder checking.
    pub fn set_strict_io_config(mut self, enabled: bool) -> Self {
        self.strict_io_config = enabled;
        self
    }

    /// Enable or disable circuit building mode.
    pub fn set_build_circuit(mut self, enabled: bool) -> Self {
        self.build_circuit = enabled;
        self
    }

    /// Enable or disable eager workspace parsing.
    pub fn set_eager(mut self, eager: bool) -> Self {
        self.eager = eager;
        self
    }

    /// Create a child config for loading a module at the given path.
    /// Adds the current source to the load chain for cycle detection.
    pub fn child_for_load(&self, child_module_path: ModulePath, target_path: PathBuf) -> Self {
        let mut child_load_chain = self.load_chain.clone();
        if let Some(ref source) = self.source_path {
            child_load_chain.insert(source.clone());
        }

        Self {
            builtin_docs: self.builtin_docs.clone(),
            load_resolver: self.load_resolver.clone(),
            module_path: child_module_path,
            load_chain: child_load_chain,
            source_path: Some(target_path),
            contents: None,
            strict_io_config: false,
            build_circuit: false,
            eager: self.eager,
        }
    }

    /// Check if loading the given path would create a cycle.
    pub fn would_create_cycle(&self, path: &Path) -> bool {
        self.load_chain.contains(path)
    }

    /// Create a child config for a pending child module instantiation.
    /// Uses a fresh load chain since this is a new module instantiation, not a nested load.
    pub fn child_for_pending(&self, child_name: &str) -> Self {
        let mut child_module_path = self.module_path.clone();
        child_module_path.push(child_name);

        Self {
            builtin_docs: self.builtin_docs.clone(),
            load_resolver: self.load_resolver.clone(),
            module_path: child_module_path,
            load_chain: HashSet::new(),
            source_path: None,
            contents: None,
            strict_io_config: false,
            build_circuit: false,
            eager: self.eager,
        }
    }
}

impl Default for EvalSession {
    fn default() -> Self {
        Self {
            inner: Arc::new(RwLock::new(EvalSessionInner::default())),
            frozen_heap: Arc::new(Mutex::new(FrozenHeap::new())),
        }
    }
}

impl EvalSession {
    // --- Module tree ---

    fn insert_module(&self, path: ModulePath, module: FrozenModuleValue) {
        self.inner.write().unwrap().module_tree.insert(path, module);
    }

    fn clone_module_tree(&self) -> BTreeMap<ModulePath, FrozenModuleValue> {
        self.inner.read().unwrap().module_tree.clone()
    }

    // --- Diagnostics ---

    /// Insert a diagnostic into the ordered map for deterministic output.
    fn add_diagnostic(&self, diag: Diagnostic) {
        let key: DiagnosticKey = (
            diag.path.clone(),
            diag.span.as_ref().map(|s| s.begin.line),
            diag.span.as_ref().map(|s| s.begin.column),
            diag.body.clone(),
        );
        self.inner
            .write()
            .unwrap()
            .diagnostics
            .entry(key)
            .or_default()
            .push(diag);
    }

    fn clone_diagnostics(&self) -> Vec<Diagnostic> {
        self.inner
            .read()
            .unwrap()
            .diagnostics
            .values()
            .flatten()
            .cloned()
            .collect()
    }

    fn clear_diagnostics(&self) {
        self.inner.write().unwrap().diagnostics.clear();
    }

    // --- Load cache ---

    fn get_cached_module(&self, path: &Path) -> Option<EvalOutput> {
        self.inner.read().unwrap().load_cache.get(path).cloned()
    }

    fn cache_module(&self, path: PathBuf, module: EvalOutput) {
        self.inner.write().unwrap().load_cache.insert(path, module);
    }

    fn clear_load_cache(&self) {
        self.inner.write().unwrap().load_cache.clear();
    }

    fn clear_file_contents(&self, path: &Path) {
        self.inner.write().unwrap().file_contents.remove(path);
    }

    fn clear_symbol_maps(&self, path: &Path) {
        let mut inner = self.inner.write().unwrap();
        inner.symbol_index.remove(path);
        inner.symbol_params.remove(path);
        inner.symbol_meta.remove(path);
    }

    fn clear_module_dependencies(&self, path: &Path) {
        self.inner.write().unwrap().module_deps.remove(path);
    }

    // --- File contents ---

    fn get_file_contents(&self, path: &Path) -> Option<String> {
        self.inner.read().unwrap().file_contents.get(path).cloned()
    }

    fn set_file_contents(&self, path: PathBuf, contents: String) {
        self.inner
            .write()
            .unwrap()
            .file_contents
            .insert(path, contents);
    }

    // --- Module dependencies ---

    fn record_module_dependency(&self, from: &Path, to: &Path) {
        let mut inner = self.inner.write().unwrap();
        inner
            .module_deps
            .entry(from.to_path_buf())
            .or_default()
            .insert(to.to_path_buf());
    }

    fn module_dep_exists(&self, from: &Path, to: &Path) -> bool {
        self.inner
            .read()
            .unwrap()
            .module_deps
            .get(from)
            .map(|deps| deps.contains(to))
            .unwrap_or(false)
    }

    fn get_module_dependencies(&self, path: &Path) -> Option<HashSet<PathBuf>> {
        self.inner.read().unwrap().module_deps.get(path).cloned()
    }

    // --- Symbol metadata ---

    fn get_symbol_params(&self, file: &Path, symbol: &str) -> Option<Vec<String>> {
        self.inner
            .read()
            .unwrap()
            .symbol_params
            .get(file)
            .and_then(|m| m.get(symbol).cloned())
    }

    fn get_symbol_info(&self, file: &Path, symbol: &str) -> Option<crate::SymbolInfo> {
        self.inner
            .read()
            .unwrap()
            .symbol_meta
            .get(file)
            .and_then(|m| m.get(symbol).cloned())
    }

    fn get_symbols_for_file(&self, path: &Path) -> Option<HashMap<String, crate::SymbolInfo>> {
        self.inner.read().unwrap().symbol_meta.get(path).cloned()
    }

    fn get_symbol_index(&self, path: &Path) -> Option<HashMap<String, PathBuf>> {
        self.inner.read().unwrap().symbol_index.get(path).cloned()
    }

    fn update_symbol_maps(
        &self,
        path: PathBuf,
        symbol_index: HashMap<String, PathBuf>,
        symbol_params: HashMap<String, Vec<String>>,
        symbol_meta: HashMap<String, crate::SymbolInfo>,
    ) {
        let mut inner = self.inner.write().unwrap();
        inner.symbol_index.insert(path.clone(), symbol_index);
        inner.symbol_params.insert(path.clone(), symbol_params);
        inner.symbol_meta.insert(path, symbol_meta);
    }

    /// Add a reference to the shared frozen heap.
    fn add_frozen_heap_reference(&self, heap: &starlark::values::FrozenHeapRef) {
        self.frozen_heap.lock().unwrap().add_reference(heap);
    }

    /// Create an EvalContext from an EvalConfig.
    /// This is the primary way to create contexts for evaluation.
    pub fn create_context(&self, config: EvalConfig) -> EvalContext {
        EvalContext {
            module: starlark::environment::Module::new(),
            session: self.clone(),
            config,
            current_load_index: RefCell::new(0),
        }
    }
}

pub struct EvalContext {
    /// The starlark::environment::Module we are evaluating.
    pub module: starlark::environment::Module,

    /// The shared session state (module_tree, diagnostics, frozen_heap, etc.)
    session: EvalSession,

    /// Configuration for this evaluation context (Send + Sync safe).
    config: EvalConfig,

    /// Index to track which load statement we're currently processing (for span resolution)
    current_load_index: RefCell<usize>,
}

/// Helper to recursively convert JSON to heap values
fn json_value_to_heap_value<'v>(json: &serde_json::Value, heap: &'v Heap) -> Value<'v> {
    use starlark::values::dict::AllocDict;
    match json {
        serde_json::Value::Null => Value::new_none(),
        serde_json::Value::Bool(b) => Value::new_bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                heap.alloc(i as i32)
            } else if let Some(f) = n.as_f64() {
                heap.alloc(starlark::values::float::StarlarkFloat(f))
            } else {
                panic!("Invalid number")
            }
        }
        serde_json::Value::String(s) => heap.alloc_str(s).to_value(),
        serde_json::Value::Array(arr) => {
            let mut values = Vec::new();
            for item in arr {
                values.push(json_value_to_heap_value(item, heap));
            }
            heap.alloc(values)
        }
        serde_json::Value::Object(obj) => {
            let mut pairs = Vec::new();
            for (k, v) in obj {
                let val = json_value_to_heap_value(v, heap);
                pairs.push((heap.alloc_str(k).to_value(), val));
            }
            heap.alloc(AllocDict(pairs))
        }
    }
}

impl EvalContext {
    /// Create a new EvalContext with a fresh session.
    pub fn new(load_resolver: Arc<dyn crate::LoadResolver>) -> Self {
        let config = EvalConfig::new(load_resolver);
        EvalSession::default().create_context(config)
    }

    /// Create an EvalContext that shares an existing session.
    /// Useful for creating a context that can access module_tree from a previous evaluation.
    pub fn with_session(session: EvalSession, load_resolver: Arc<dyn crate::LoadResolver>) -> Self {
        let config = EvalConfig::new(load_resolver);
        session.create_context(config)
    }

    /// Create an EvalContext from an existing session and config.
    /// This is the preferred way to create contexts for parallel evaluation.
    pub fn from_session_and_config(session: EvalSession, config: EvalConfig) -> Self {
        session.create_context(config)
    }

    /// Get the current config (for creating child configs).
    pub fn config(&self) -> &EvalConfig {
        &self.config
    }

    /// Get the source path of the module we are evaluating.
    pub fn source_path(&self) -> Option<&PathBuf> {
        self.config.source_path.as_ref()
    }

    /// Get the module path (fully qualified path in the tree).
    pub fn module_path(&self) -> &ModulePath {
        &self.config.module_path
    }

    /// Get the load resolver.
    pub fn load_resolver(&self) -> &Arc<dyn crate::LoadResolver> {
        &self.config.load_resolver
    }

    /// Check if strict IO/config checking is enabled.
    pub fn strict_io_config(&self) -> bool {
        self.config.strict_io_config
    }

    /// Create a child config for loading a module.
    /// This can be passed across thread boundaries safely.
    pub fn child_config_for_load(
        &self,
        child_module_path: ModulePath,
        target_path: PathBuf,
    ) -> EvalConfig {
        self.config.child_for_load(child_module_path, target_path)
    }

    pub fn file_provider(&self) -> &dyn crate::FileProvider {
        self.config.load_resolver.file_provider()
    }

    /// Set the file provider for this context
    pub fn set_load_resolver(mut self, load_resolver: Arc<dyn crate::LoadResolver>) -> Self {
        self.config.load_resolver = load_resolver;
        self
    }

    /// Enable or disable strict IO/config placeholder checking for subsequent evaluations.
    pub fn set_strict_io_config(mut self, enabled: bool) -> Self {
        self.config.strict_io_config = enabled;
        self
    }

    fn freeze(&mut self) -> FrozenModule {
        let module = std::mem::take(&mut self.module);
        let frozen = module.freeze().expect("failed to freeze module");
        self.session.add_frozen_heap_reference(frozen.frozen_heap());
        frozen
    }

    /// Enable or disable eager workspace parsing.
    pub fn set_eager(mut self, eager: bool) -> Self {
        self.config.eager = eager;
        self
    }

    /// Create a new Context that shares caches with this one
    pub fn child_context(&self, name: Option<&str>) -> Self {
        let mut module_path = self.config.module_path.clone();
        if let Some(name) = name {
            module_path.push(name);
        }
        let child_config = EvalConfig {
            builtin_docs: self.config.builtin_docs.clone(),
            load_resolver: self.config.load_resolver.clone(),
            module_path,
            load_chain: self.config.load_chain.clone(),
            source_path: None,
            contents: None,
            strict_io_config: false,
            build_circuit: false,
            eager: self.config.eager,
        };
        self.session.create_context(child_config)
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
        .with(init_net_global)
        .with(module_globals)
        .with(interface_globals)
        .with(assert_globals)
        .with(file_globals)
        .with(model_globals)
        .with(test_bench_globals)
        .build()
    }

    /// Get a clone of the module tree from the session.
    pub fn module_tree(&self) -> BTreeMap<ModulePath, FrozenModuleValue> {
        self.session.clone_module_tree()
    }

    /// Record that `from` references `to` via a `Module()` call.
    pub(crate) fn record_module_dependency(&self, from: &Path, to: &Path) {
        self.session.record_module_dependency(from, to);
    }

    /// Get a cached frozen module if it exists
    pub fn get_cached_module(&self, path: &Path) -> Option<EvalOutput> {
        self.session.get_cached_module(path)
    }

    /// Cache a frozen module
    pub fn cache_module(&self, path: PathBuf, module: EvalOutput) {
        self.session.cache_module(path, module);
    }

    /// Check if there is a module dependency between two files
    pub fn module_dep_exists(&self, from: &Path, to: &Path) -> bool {
        self.session.module_dep_exists(from, to)
    }

    /// Return the cached parameter list for a global symbol if one is available.
    pub fn get_params_for_global_symbol(
        &self,
        current_file: &Path,
        symbol: &str,
    ) -> Option<Vec<String>> {
        self.session.get_symbol_params(current_file, symbol)
    }

    /// Return rich completion metadata for a symbol if available.
    pub fn get_symbol_info(&self, current_file: &Path, symbol: &str) -> Option<crate::SymbolInfo> {
        if let Some(info) = self.session.get_symbol_info(current_file, symbol) {
            return Some(info);
        }

        // Fallback: built-in global docs.
        if let Some(doc) = self.config.builtin_docs.get(symbol) {
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
        self.config.contents = Some(contents.into());
        self
    }

    /// Set the source path of the module we are evaluating.
    pub fn set_source_path(mut self, path: PathBuf) -> Self {
        self.config.source_path = Some(path);
        self
    }

    /// Set inputs from already frozen parent values.
    pub fn set_inputs_from_frozen_values(&mut self, parent_inputs: SmallMap<String, FrozenValue>) {
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
    }

    /// Set properties from already frozen parent values.
    pub fn set_properties_from_frozen_values(
        &mut self,
        parent_properties: SmallMap<String, FrozenValue>,
    ) {
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
    }

    /// Set parent component modifiers from already frozen parent values.
    pub fn set_parent_component_modifiers_from_frozen_values(
        &mut self,
        parent_modifiers: Vec<FrozenValue>,
    ) {
        let eval = Evaluator::new(&self.module);
        if self.module.extra_value().is_none() {
            let ctx_value = eval.heap().alloc_complex(ContextValue::from_context(self));
            self.module.set_extra_value(ctx_value);
        }
        let extra_value = self.module.extra_value().unwrap();
        let ctx_value = extra_value.downcast_ref::<ContextValue>().unwrap();

        let mut module = ctx_value.module_mut();
        let unfrozen_modifiers: Vec<_> = parent_modifiers
            .into_iter()
            .map(|fv| fv.to_value())
            .collect();
        module.set_parent_component_modifiers(unfrozen_modifiers);
    }

    /// Apply component modifiers to all children after module evaluation but before freezing.
    /// This ensures modifiers apply to all components regardless of declaration order.
    fn apply_component_modifiers(eval: &mut Evaluator) -> starlark::Result<()> {
        let Some(module) = eval.module_value() else {
            return Ok(());
        };

        let children = module.children().clone();
        let own_modifiers = module.component_modifiers().clone();
        let parent_modifiers = module.parent_component_modifiers().clone();
        let all_modifiers = module.collect_all_component_modifiers_as_values();
        drop(module);

        // Apply modifiers to direct children (bottom-up: own then parent)
        for child in &children {
            for modifier in own_modifiers.iter().chain(&parent_modifiers) {
                eval.eval_function(*modifier, &[*child], &[])?;
            }
        }

        // Update pending child modules with final modifier list
        if let Some(context) = eval.context_value() {
            for pending in context.pending_children_mut().iter_mut() {
                pending.component_modifiers = all_modifiers.clone();
            }
        }

        Ok(())
    }

    /// Convert JSON inputs directly to heap values and set them (for external APIs)
    pub fn set_json_inputs(&mut self, json_inputs: SmallMap<String, serde_json::Value>) {
        let eval = Evaluator::new(&self.module);
        if self.module.extra_value().is_none() {
            let ctx_value = eval.heap().alloc_complex(ContextValue::from_context(self));
            self.module.set_extra_value(ctx_value);
        }
        let extra_value = self.module.extra_value().unwrap();
        let ctx_value = extra_value.downcast_ref::<ContextValue>().unwrap();

        let mut module = ctx_value.module_mut();
        for (name, json) in json_inputs.iter() {
            let value = json_value_to_heap_value(json, eval.heap());
            module.add_input(name.clone(), value);
        }
    }

    /// Evaluate the configured module. All required fields must be provided
    /// beforehand via the corresponding setters. When a required field is
    /// missing this function returns a failed [`WithDiagnostics`].
    #[instrument(
        name = "eval",
        skip_all,
        fields(
            module = %self.config.module_path,
            file = self.config.source_path.as_ref().map(|p| p.file_name().and_then(|f| f.to_str()).unwrap_or("")).unwrap_or("")
        )
    )]
    pub fn eval(mut self) -> WithDiagnostics<EvalOutput> {
        // Make sure a source path is set.
        let source_path = match self.config.source_path {
            Some(ref path) => path,
            None => {
                return anyhow::anyhow!("source_path not set on Context before eval()").into();
            }
        };

        self.config.load_resolver.track_file(source_path);

        // Fetch contents: prefer explicit override, otherwise read from disk.
        let contents_owned = match &self.config.contents {
            Some(c) => c.clone(),
            None => match self.file_provider().read_file(source_path) {
                Ok(c) => {
                    // Cache the read contents for subsequent accesses.
                    self.config.contents = Some(c.clone());
                    c
                }
                Err(err) => {
                    return anyhow::anyhow!("Failed to read file: {}", err).into();
                }
            },
        };

        // Cache provided contents in `open_files` so that nested `load()` calls see the
        // latest buffer state rather than potentially stale on-disk contents.
        self.session
            .set_file_contents(source_path.clone(), contents_owned.clone());

        let ast_res = {
            let _span = info_span!("parse").entered();
            AstModule::parse(
                source_path.to_str().expect("path is not a string"),
                contents_owned.to_string(),
                &self.dialect(),
            )
        };

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
            let _span = info_span!("starlark_eval").entered();
            eval.eval_module(ast.clone(), &globals)
                .and_then(|_| Self::apply_component_modifiers(&mut eval))
        };

        // Collect print output after evaluation
        let print_output = print_handler.take_output();

        match eval_result {
            Ok(_) => {
                // Extract needed references before freezing (which moves self.module)
                let session_ref = self.session.clone();
                let load_resolver_ref = self.config.load_resolver.clone();

                let frozen_module = {
                    let _span = info_span!("freeze_module").entered();
                    self.freeze()
                };
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

                        // Get human-readable display of default value
                        let default_display =
                            param.default_value.as_ref().map(|v| v.to_value().to_repr());

                        ParameterInfo {
                            name: param.name.clone(),
                            type_info,
                            required: !param.optional,
                            default_value,
                            default_display,
                            help: param.help.clone(),
                        }
                    })
                    .collect();

                // Process pending children after parent is frozen
                // Only process children if building circuit (root with build_circuit=true, or child instantiation)
                let module_path = extra.module.path().clone();
                let is_root = module_path.segments.is_empty();

                // Add this module to the tree at its path
                if self.config.build_circuit || is_root {
                    session_ref.insert_module(module_path, extra.module.clone());
                    let process_children_span = info_span!("process_children", module = %extra.module.path().name(), count = extra.pending_children.len());
                    let _guard = process_children_span.enter();

                    // Extract Send-safe config for parallel child creation
                    let session = self.session.clone();
                    let base_config = self.config.clone();

                    #[cfg(feature = "native")]
                    {
                        extra.pending_children.par_iter().for_each(|pending| {
                            let child_config = base_config.child_for_pending(&pending.final_name);
                            session
                                .create_context(child_config)
                                .process_pending_child(pending.clone());
                        });
                    }

                    #[cfg(not(feature = "native"))]
                    {
                        for pending in extra.pending_children.iter() {
                            let child_config = base_config.child_for_pending(&pending.final_name);
                            session
                                .create_context(child_config)
                                .process_pending_child(pending.clone());
                        }
                    }
                }

                let output = EvalOutput {
                    ast,
                    star_module: frozen_module,
                    sch_module: extra.module.clone(),
                    signature,
                    print_output,
                    load_resolver: load_resolver_ref.clone(),
                    session: session_ref.clone(),
                };
                let mut ret = WithDiagnostics::success(output);

                // Emit collision warnings for nets that were renamed due to duplicates
                for (_id, net_info) in extra.module.introduced_nets() {
                    if let Some(original) = &net_info.original_name {
                        // Sorry!
                        if original == "NC" {
                            continue;
                        }
                        // Find the first frame with location info (iterating from most recent)
                        // Native Rust functions (like `io()`) have location: None, so we skip them
                        let frame_with_location = net_info
                            .call_stack
                            .frames
                            .iter()
                            .rev()
                            .find(|f| f.location.is_some());
                        let span = frame_with_location
                            .and_then(|f| f.location.as_ref())
                            .map(|loc| loc.resolve_span());
                        let path = frame_with_location
                            .and_then(|f| f.location.as_ref())
                            .map(|loc| loc.file.filename().to_string())
                            .unwrap_or_else(|| extra.module.source_path().to_string());
                        ret.diagnostics.push(crate::Diagnostic {
                            path,
                            span,
                            severity: EvalSeverity::Warning,
                            body: format!(
                                "Net '{}' was renamed to '{}' due to name collision",
                                original, net_info.final_name
                            ),
                            call_stack: Some(net_info.call_stack.clone()),
                            child: None,
                            source_error: None,
                            suppressed: false,
                        });
                    }
                }

                ret.diagnostics.extend(extra.diagnostics().clone());
                // Only include session diagnostics at root level to avoid duplication.
                // Child diagnostics are wrapped by process_pending_child and added to session,
                // so including them here would cause duplicates when children also include
                // session diagnostics in their return values.
                if is_root {
                    ret.diagnostics.extend(session_ref.clone_diagnostics());
                }
                ret
            }
            Err(err) => {
                let mut ret = WithDiagnostics::default();
                ret.diagnostics.push(err.into());
                ret
            }
        }
    }

    /// Get the file contents from the in-memory cache
    pub fn get_file_contents(&self, path: &Path) -> Option<String> {
        self.session.get_file_contents(path)
    }

    /// Set file contents in the in-memory cache
    pub fn set_file_contents(&self, path: PathBuf, contents: String) {
        self.session.set_file_contents(path, contents);
    }

    /// Remove file contents from the in-memory cache.
    pub fn clear_file_contents(&self, path: &Path) {
        self.session.clear_file_contents(path);
    }

    /// Get all symbols for a file
    pub fn get_symbols_for_file(&self, path: &Path) -> Option<HashMap<String, crate::SymbolInfo>> {
        self.session.get_symbols_for_file(path)
    }

    /// Get the symbol index for a file (symbol name -> target path)
    pub fn get_symbol_index(&self, path: &Path) -> Option<HashMap<String, PathBuf>> {
        self.session.get_symbol_index(path)
    }

    /// Get module dependencies for a file
    pub fn get_module_dependencies(&self, path: &Path) -> Option<HashSet<PathBuf>> {
        self.session.get_module_dependencies(path)
    }

    /// Clear module dependency tracking for a file.
    pub fn clear_module_dependencies(&self, path: &Path) {
        self.session.clear_module_dependencies(path);
    }

    /// Parse and analyze a file, updating the symbol index and metadata
    pub fn parse_and_analyze_file(
        &self,
        path: PathBuf,
        contents: String,
    ) -> WithDiagnostics<Option<AstModule>> {
        self.session.clear_diagnostics();
        self.session.clear_load_cache();
        self.session.clear_module_dependencies(&path);
        self.session.clear_symbol_maps(&path);

        // Update the in-memory file contents
        self.set_file_contents(path.clone(), contents.clone());

        // Evaluate the file
        let result = self
            .child_context(None)
            .set_source_path(path.clone())
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
            self.session
                .update_symbol_maps(path.clone(), symbol_index, symbol_params, symbol_meta);
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
        self.session
            .get_symbol_index(current_file)
            .and_then(|map| map.get(symbol).cloned())
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
        self.config.builtin_docs.get(symbol).cloned()
    }

    /// Check if eager workspace parsing is enabled
    pub fn is_eager(&self) -> bool {
        self.config.eager
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
        let source_path = self.config.source_path.as_ref()?;
        let contents = self.config.contents.as_ref()?;
        starlark::syntax::AstModule::parse(
            &source_path.to_string_lossy(),
            contents.clone(),
            &self.dialect(),
        )
        .ok()
    }

    /// Get the codemap for the current module being evaluated
    pub fn get_codemap(&self) -> Option<starlark::codemap::CodeMap> {
        if let (Some(source_path), Some(contents)) =
            (&self.config.source_path, &self.config.contents)
        {
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
        self.config.source_path.as_deref()
    }

    /// Get the load resolver if available
    pub fn get_load_resolver(&self) -> &Arc<dyn crate::LoadResolver> {
        &self.config.load_resolver
    }

    /// Append a diagnostic that was produced while this context was active.
    pub fn add_diagnostic<D: Into<Diagnostic>>(&self, diag: D) {
        self.session.add_diagnostic(diag.into());
    }

    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        self.session.clone_diagnostics()
    }

    #[instrument(name = "load", skip_all, fields(path = %path))]
    pub fn resolve_and_eval_module(
        &self,
        path: &str,
        span: Option<ResolvedSpan>,
    ) -> starlark::Result<EvalOutput> {
        log::debug!(
            "Trying to load path {path} with current path {:?}",
            self.config.source_path
        );
        let load_resolver = self.config.load_resolver.clone();
        let file_provider = load_resolver.file_provider();

        let module_path = self.config.source_path.clone();
        let Some(current_file) = module_path.as_ref() else {
            return Err(starlark::Error::new_other(anyhow::anyhow!(
                "Cannot resolve load path '{}' without a current file context",
                path
            )));
        };

        // Resolve the load path to an absolute path
        let mut resolve_context = load_resolver.resolve_context(path, current_file)?;
        let canonical_path = load_resolver.resolve(&mut resolve_context)?;

        // Check for cyclic imports using per-context load chain (thread-safe)
        if self.config.load_chain.contains(&canonical_path) {
            return Err(starlark::Error::new_other(anyhow!(
                "cyclic load detected while loading `{}`",
                canonical_path.display()
            )));
        }

        let source_path = self
            .config
            .source_path
            .clone()
            .unwrap_or_else(|| PathBuf::from("<unknown>"));

        // Fast path: if we've already loaded (and frozen) this module once
        // within the current evaluation context, simply return the cached
        // instance so that callers share the same definitions.
        if let Some(frozen) = self.get_cached_module(&canonical_path) {
            return Ok(frozen);
        }

        let span = span.or_else(|| self.resolve_load_span(path));

        if file_provider.is_directory(&canonical_path) {
            return Err(starlark::Error::new_other(anyhow::anyhow!(
                "Directory load syntax is no longer supported"
            )));
        }

        // Build child config for the nested load
        let name = canonical_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();

        let mut child_path = self.config.module_path.clone();
        child_path.push(&name);

        let child_config = self
            .config
            .child_for_load(child_path, canonical_path.clone());

        let result = self.session.create_context(child_config).eval();

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
                    suppressed: false,
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
                suppressed: false,
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
                suppressed: false,
            };
            Err(diagnostic.into())
        }
    }

    /// Process a pending child after the parent module has been frozen
    #[instrument(name = "instantiate", skip_all, fields(module = %pending.loader.name))]
    fn process_pending_child(mut self, pending: FrozenPendingChild) {
        self.config.strict_io_config = true;
        self.config.build_circuit = true;
        self.config.source_path = Some(PathBuf::from(&pending.loader.source_path));

        if let Some(props) = pending.properties {
            self.set_properties_from_frozen_values(props);
        }
        self.set_inputs_from_frozen_values(pending.inputs);
        self.set_parent_component_modifiers_from_frozen_values(pending.component_modifiers);

        // Save session reference before eval() consumes self
        let session = self.session.clone();
        let child_result = self.eval();

        // Add child's diagnostics to parent with proper context
        for child_diag in child_result.diagnostics.diagnostics.iter() {
            let (severity, message) = match child_diag.severity {
                EvalSeverity::Error => (
                    EvalSeverity::Error,
                    format!("Error instantiating `{}`", pending.loader.name),
                ),
                EvalSeverity::Warning => (
                    EvalSeverity::Warning,
                    format!("Warning from `{}`", pending.loader.name),
                ),
                other => (other, format!("Issue in `{}`", pending.loader.name)),
            };

            let diag_to_add = crate::Diagnostic {
                path: pending.call_site_path.clone(),
                span: Some(pending.call_site_span),
                severity,
                body: message,
                call_stack: Some(pending.call_stack.clone()),
                child: Some(Box::new(child_diag.clone())),
                source_error: None,
                suppressed: false,
            };

            session.add_diagnostic(diag_to_add);
        }

        // If child evaluation failed, return None (errors already propagated to diagnostics)
        let Some(output) = child_result.output else {
            return;
        };

        // Validate unused arguments
        let used_inputs: HashSet<String> = output
            .star_module
            .extra_value()
            .and_then(|extra| extra.downcast_ref::<FrozenContextValue>())
            .map(|fctx| {
                fctx.module
                    .signature()
                    .iter()
                    .map(|param| param.name.clone())
                    .collect()
            })
            .unwrap_or_default();

        let provided_set: HashSet<String> = pending.provided_names.into_iter().collect();
        let unused: Vec<String> = provided_set.difference(&used_inputs).cloned().collect();

        if !unused.is_empty() {
            let msg = format!(
                "Unknown argument(s) provided to module {}: {}",
                pending.loader.name,
                unused.join(", ")
            );

            let diag = crate::Diagnostic {
                path: pending.call_site_path.clone(),
                span: Some(pending.call_site_span),
                severity: EvalSeverity::Error,
                body: msg,
                call_stack: Some(pending.call_stack.clone()),
                child: None,
                source_error: None,
                suppressed: false,
            };
            session.add_diagnostic(diag);
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
