#![allow(clippy::arc_with_non_send_sync)]

use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

use anyhow::anyhow;
use pcb_sch::physical::PhysicalValue;
use starlark::environment::FrozenModule;
use starlark::{
    PrintHandler,
    environment::{GlobalsBuilder, LibraryExtension, Module},
    errors::{EvalMessage, EvalSeverity},
    eval::{Evaluator, FileLoader},
    syntax::{AstModule, Dialect},
    values::{FrozenHeapName, FrozenValue, Heap, Value, ValueLike},
};
use starlark::{codemap::ResolvedSpan, collections::SmallMap};
use starlark_syntax::syntax::{
    ast::{LoadArgP, StmtP},
    module::AstModuleFields,
    top_level_stmts::top_level_stmts,
};

#[cfg(feature = "native")]
use rayon::prelude::*;

use tracing::{info_span, instrument};

use crate::lang::assert::assert_globals;
use crate::lang::{
    binding,
    builtin::builtin_globals,
    component::component_globals,
    r#enum::EnumValue,
    style_lint::{ast_style_lints, is_ast_style_diagnostic},
    type_info::{ParameterInfo, TypeInfo},
};
use crate::lang::{
    electrical_check::FrozenElectricalCheck,
    evaluator_ext::EvaluatorExt,
    file::file_globals,
    footprint::{FootprintCacheKey, footprint_cache_key, validate_footprints},
    module::{FrozenModuleValue, ModulePath},
};
use crate::load_spec::LoadSpec;
use crate::resolution::{PackageScopeKey, PackageUrlResolution, ResolutionResult};
use crate::{Diagnostic, Diagnostics, WithDiagnostics};
use crate::{FileProvider, ResolveContext};
use crate::{convert::ModuleConverter, lang::context::FrozenPendingChild};

pub use super::evaluator_ext::EvalContextRef;

use super::{
    context::{ContextValue, FrozenContextValue},
    interface::interface_globals,
    module::{ModuleLoader, module_globals},
    path::format_relative_path_as_package_uri,
    spice_model::model_globals,
    test_bench::test_bench_globals,
};

/// Stdlib symbols that are implicitly available in user `.zen` files without
/// an explicit `load()` statement. Each entry maps a stdlib module path to the
/// symbol names to inject.
const PRELUDE: &[(&str, &[&str])] = &[
    ("@stdlib/io.zen", &["io", "input", "output"]),
    (
        "@stdlib/interfaces.zen",
        &["Net", "Power", "Ground", "NotConnected"],
    ),
    ("@stdlib/properties.zen", &["Layout", "Part"]),
    ("@stdlib/board_config.zen", &["Board"]),
];

fn canonicalize_for_compare(path: &Path, file_provider: &dyn FileProvider) -> PathBuf {
    file_provider
        .canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
}

fn path_starts_with_canonical(
    path: &Path,
    prefix: &Path,
    file_provider: &dyn FileProvider,
) -> bool {
    canonicalize_for_compare(path, file_provider)
        .starts_with(canonicalize_for_compare(prefix, file_provider))
}

fn is_stdlib_source_path(path: &Path, config: &EvalContextConfig) -> bool {
    let file_provider = config.file_provider.as_ref();
    path_starts_with_canonical(
        path,
        &config.resolution.workspace_info.workspace_stdlib_dir(),
        file_provider,
    ) || path_starts_with_canonical(
        path,
        &config.resolution.workspace_info.root.join("stdlib"),
        file_provider,
    )
}

fn explicit_prelude_load_diagnostics(
    ast: &AstModule,
    config: &EvalContextConfig,
) -> Vec<Diagnostic> {
    if !config.inject_prelude {
        return Vec::new();
    }

    let Some(source_path) = config.source_path.as_deref() else {
        return Vec::new();
    };

    if is_stdlib_source_path(source_path, config) {
        return Vec::new();
    }

    let file_provider = config.file_provider.as_ref();
    let prelude_modules: Vec<_> = PRELUDE
        .iter()
        .filter_map(|(module_path, symbols)| {
            config
                .resolve_path(module_path, source_path)
                .ok()
                .map(|path| (canonicalize_for_compare(&path, file_provider), *symbols))
        })
        .collect();

    let mut diagnostics = Vec::new();
    for stmt in top_level_stmts(ast.statement()) {
        let StmtP::Load(load) = &stmt.node else {
            continue;
        };

        let Ok(load_path) = config.resolve_path(&load.module.node, source_path) else {
            continue;
        };
        let load_path = canonicalize_for_compare(&load_path, file_provider);

        let Some((_, prelude_symbols)) = prelude_modules
            .iter()
            .find(|(prelude_path, _)| prelude_path == &load_path)
        else {
            continue;
        };

        let explicitly_loaded: Vec<&str> = load
            .args
            .iter()
            .filter_map(|LoadArgP { their, .. }| {
                prelude_symbols
                    .contains(&their.node.as_str())
                    .then_some(their.node.as_str())
            })
            .collect();

        if explicitly_loaded.is_empty() {
            continue;
        }

        let names = match explicitly_loaded.as_slice() {
            [name] => format!("`{name}` is"),
            names => format!(
                "{} are",
                names
                    .iter()
                    .map(|name| format!("`{name}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
        let message = format!(
            "{names} available from the @stdlib prelude; remove from this explicit `load()`"
        );
        diagnostics.push(
            Diagnostic::categorized(
                &source_path.to_string_lossy(),
                &message,
                "stdlib.prelude_load",
                EvalSeverity::Warning,
            )
            .with_span(Some(ast.codemap().file_span(stmt.span).resolve_span())),
        );
    }

    diagnostics
}

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

fn serialize_parameter_value(value: Value<'_>) -> Option<serde_json::Value> {
    if let Some(enum_value) = value.downcast_ref::<EnumValue>() {
        return Some(serde_json::Value::String(enum_value.value().to_string()));
    }

    if let Some(&physical) = value.downcast_ref::<PhysicalValue>() {
        return Some(serde_json::Value::String(physical.to_string()));
    }

    value.to_json_value().ok()
}

#[derive(Clone)]
pub struct EvalOutput {
    /// Parsed AST of the module. Wrapped in `Arc` so that cloning an
    /// `EvalOutput` (e.g. on every load-cache hit) does not deep-copy the AST.
    pub ast: Arc<AstModule>,
    pub star_module: FrozenModule,
    pub sch_module: FrozenModuleValue,
    /// Ordered list of parameter information
    pub signature: Vec<ParameterInfo>,
    /// Print output collected during evaluation
    pub print_output: Vec<String>,
    /// Eval config (file provider, path specs, etc.)
    pub config: EvalContextConfig,
    /// Session owns the frozen module tree for this output.
    session: EvalSession,
}

#[derive(Clone)]
struct CachedModule {
    output: EvalOutput,
    warnings: Vec<Diagnostic>,
}

type LoadCacheKey = (Option<PackageScopeKey>, PathBuf);

/// Key for the session-level `Symbol(library = ...)` cache: the resolved
/// library path plus the requested symbol name.
pub(crate) type SymbolCacheKey = (PathBuf, Option<String>);

/// Key for the session-level spice subcircuit cache: the resolved model path
/// plus the subcircuit name.
pub(crate) type SpiceCacheKey = (PathBuf, String);

/// A source file's on-disk contents together with its parsed AST — the single
/// in-memory record for a module's source during an evaluation session.
#[derive(Clone)]
pub(crate) struct ParsedSource {
    pub(crate) contents: String,
    pub(crate) ast: Arc<AstModule>,
}

/// Concurrent map for session-scoped caches of values derived from files.
/// All [`EvalSession`] caches share this shape and lifecycle: cleared together
/// by [`EvalSession::clear_load_cache`], with per-file invalidation via
/// [`EvalContext::invalidate_file`].
pub(crate) struct CacheMap<K, V>(RwLock<HashMap<K, V>>);

impl<K, V> Default for CacheMap<K, V> {
    fn default() -> Self {
        Self(RwLock::new(HashMap::new()))
    }
}

impl<K: Eq + std::hash::Hash, V: Clone> CacheMap<K, V> {
    pub(crate) fn get(&self, key: &K) -> Option<V> {
        self.0.read().unwrap().get(key).cloned()
    }

    pub(crate) fn insert(&self, key: K, value: V) {
        self.0.write().unwrap().insert(key, value);
    }

    pub(crate) fn remove(&self, key: &K) {
        self.0.write().unwrap().remove(key);
    }

    pub(crate) fn clear(&self) {
        self.0.write().unwrap().clear();
    }

    pub(crate) fn retain(&self, f: impl FnMut(&K, &mut V) -> bool) {
        self.0.write().unwrap().retain(f);
    }
}

impl EvalOutput {
    /// Get the session (for creating a new EvalContext that shares state with this output).
    pub fn session(&self) -> &EvalSession {
        &self.session
    }

    /// Get the resolution result.
    pub fn resolution(&self) -> &crate::resolution::ResolutionResult {
        &self.config.resolution
    }

    /// Get the module tree from the session.
    pub fn module_tree(&self) -> BTreeMap<ModulePath, FrozenModuleValue> {
        self.session.clone_module_tree()
    }

    /// Validate the KiCad footprints referenced by components in the module
    /// tree. Decompresses and hashes embedded payloads, so this is expensive —
    /// callers that actually consume footprints (e.g. layout) opt in.
    pub fn validate_footprints(&self) -> Vec<Diagnostic> {
        validate_footprints(&self.module_tree(), &self.config, &self.session)
    }

    /// Convert to schematic with diagnostics
    pub fn to_schematic_with_diagnostics(&self) -> crate::WithDiagnostics<pcb_sch::Schematic> {
        let converter = ModuleConverter::new();
        let module_tree = self.module_tree();
        let mut result = converter.build(module_tree);
        if let Some(ref mut schematic) = result.output {
            schematic.package_roots = self.config.resolution.package_roots();

            // Resolve any non-package:// layout_path attributes to stable URIs
            for inst in schematic.instances.values_mut() {
                if inst.kind != pcb_sch::InstanceKind::Module {
                    continue;
                }
                let layout_val = inst
                    .attributes
                    .get(pcb_sch::ATTR_LAYOUT_PATH)
                    .and_then(|v| v.string())
                    .map(|s| s.to_owned());
                if let Some(raw) = layout_val
                    && !raw.starts_with(pcb_sch::PACKAGE_URI_PREFIX)
                {
                    let source_dir = inst.type_ref.source_path.parent();
                    if let Some(uri) = format_relative_path_as_package_uri(
                        &raw,
                        source_dir,
                        &self.config.resolution,
                    ) {
                        inst.add_attribute(
                            pcb_sch::ATTR_LAYOUT_PATH.to_string(),
                            pcb_sch::AttributeValue::String(uri),
                        );
                    }
                }
            }
        }
        result
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
}

/// Handle to shared evaluation session state. Cheaply cloneable.
/// Each cache has its own lock to minimize contention during parallel preloading.
#[derive(Clone)]
pub struct EvalSession {
    /// On-disk contents and parsed AST per module path, so repeated
    /// instantiations of the same module skip the disk read and reparse.
    pub(crate) source_cache: Arc<CacheMap<PathBuf, ParsedSource>>,
    /// Loaded (frozen) modules. Frozen package resolution is package-local,
    /// so cached modules are keyed by the loaded file's package identity and
    /// resolved dependency map.
    load_cache: Arc<CacheMap<LoadCacheKey, CachedModule>>,
    /// Diagnostics from validating footprint files.
    pub(crate) footprint_cache: Arc<CacheMap<FootprintCacheKey, Vec<Diagnostic>>>,
    /// `Symbol(library = ...)` values keyed by resolved library path and
    /// symbol name.
    pub(crate) symbol_cache: Arc<CacheMap<SymbolCacheKey, crate::lang::symbol::SymbolValue>>,
    /// Spice subcircuits keyed by resolved model path and subcircuit name.
    pub(crate) spice_cache:
        Arc<CacheMap<SpiceCacheKey, crate::lang::spice_model::CachedSpiceModel>>,
    /// Per-file mapping of `symbol → target path` for "go-to definition".
    symbol_index: Arc<RwLock<HashMap<PathBuf, HashMap<String, PathBuf>>>>,
    /// Per-file mapping of `symbol → metadata` (kind, docs, etc.)
    symbol_meta: Arc<RwLock<HashMap<PathBuf, HashMap<String, crate::SymbolInfo>>>>,
    /// Map of `module.zen` → set of files referenced via `load()`.
    module_deps: Arc<RwLock<HashMap<PathBuf, HashSet<PathBuf>>>>,
    /// Tree of all frozen child modules indexed by fully qualified path.
    module_tree: Arc<RwLock<BTreeMap<ModulePath, FrozenModule>>>,
}

/// Configuration for creating an EvalContext. Send + Sync safe for passing across threads.
/// Use `EvalSession::create_context(config)` to create an EvalContext from this.
#[derive(Clone)]
pub struct EvalContextConfig {
    /// Documentation source for built-in Starlark symbols keyed by their name.
    /// Wrapped in Arc since it's the same for all contexts.
    pub(crate) builtin_docs: Arc<HashMap<String, String>>,

    /// File provider for reading files and checking existence.
    pub(crate) file_provider: Arc<dyn FileProvider>,

    /// Resolution result from dependency resolution.
    pub(crate) resolution: Arc<ResolutionResult>,

    /// The fully qualified path of the module we are evaluating (e.g., "root", "root.child")
    pub(crate) module_path: ModulePath,

    /// Per-context load chain for cycle detection. Contains canonical paths of all files
    /// in the current load chain (ancestors). Thread-local to each evaluation path.
    pub(crate) load_chain: HashSet<PathBuf>,

    /// The absolute path to the module we are evaluating.
    pub(crate) source_path: Option<PathBuf>,

    /// Active root package for this frozen package-local eval tree.
    pub(crate) active_root_package: Option<String>,

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

    /// When `true`, inject stdlib prelude symbols (Power, Ground) before evaluation.
    /// Defaults to `true`. Set to `false` for stdlib modules (circular dep avoidance)
    /// and test harnesses that don't need the prelude.
    pub(crate) inject_prelude: bool,
}

impl EvalContextConfig {
    /// Create a new root EvalContextConfig.
    ///
    /// The resolution's package roots should already be canonicalized (see
    /// [`EvalContext::new`] which handles this).
    pub fn new(file_provider: Arc<dyn FileProvider>, resolution: Arc<ResolutionResult>) -> Self {
        use std::sync::OnceLock;
        static BUILTIN_DOCS: OnceLock<Arc<HashMap<String, String>>> = OnceLock::new();
        let builtin_docs = BUILTIN_DOCS
            .get_or_init(|| {
                let globals = EvalContext::build_globals();
                let mut docs = HashMap::new();
                for (name, item) in globals.documentation().members {
                    docs.insert(name.clone(), item.render_as_code(&name));
                }
                Arc::new(docs)
            })
            .clone();

        Self {
            builtin_docs,
            file_provider,
            resolution,
            module_path: ModulePath::root(),
            load_chain: HashSet::new(),
            source_path: None,
            active_root_package: None,
            contents: None,
            strict_io_config: false,
            build_circuit: false,
            eager: true,
            inject_prelude: true,
        }
    }

    /// Set the source path of the module we are evaluating.
    pub fn set_source_path(mut self, path: PathBuf) -> Self {
        let stdlib_dir = self.resolution.workspace_info.workspace_stdlib_dir();
        self.inject_prelude = self.inject_prelude && !path.starts_with(&stdlib_dir);
        if self.active_root_package.is_none() {
            let canonical_path = self
                .file_provider
                .canonicalize(&path)
                .unwrap_or_else(|_| path.clone());
            self.active_root_package = self
                .resolution
                .frozen_root_for_file(&canonical_path)
                .map(|(package_url, _)| package_url.to_string());
        }
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

    /// Enable or disable stdlib prelude injection.
    pub fn set_inject_prelude(mut self, inject: bool) -> Self {
        self.inject_prelude = inject;
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
            file_provider: self.file_provider.clone(),
            resolution: self.resolution.clone(),
            module_path: child_module_path,
            load_chain: child_load_chain,
            source_path: None,
            active_root_package: self.active_root_package.clone(),
            contents: None,
            strict_io_config: false,
            build_circuit: false,
            eager: self.eager,
            inject_prelude: self.inject_prelude,
        }
        .set_source_path(target_path)
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
            file_provider: self.file_provider.clone(),
            resolution: self.resolution.clone(),
            module_path: child_module_path,
            load_chain: HashSet::new(),
            source_path: None,
            active_root_package: self.active_root_package.clone(),
            contents: None,
            strict_io_config: false,
            build_circuit: false,
            eager: self.eager,
            inject_prelude: self.inject_prelude,
        }
    }

    pub(crate) fn file_provider(&self) -> &dyn FileProvider {
        &*self.file_provider
    }

    fn package_scope_for_file(
        &self,
        path: &Path,
    ) -> Option<crate::resolution::ResolvedPackageScope<'_>> {
        self.resolution
            .package_scope_for_file(path, self.active_root_package.as_deref())
    }

    /// Convenience method to resolve a load path string directly.
    pub fn resolve_path(&self, path: &str, current_file: &Path) -> Result<PathBuf, anyhow::Error> {
        let load_spec = LoadSpec::parse(path)
            .ok_or_else(|| anyhow::anyhow!("Invalid load path spec: {}", path))?;
        self.resolve_spec(&load_spec, current_file)
    }

    /// Convenience method to resolve a LoadSpec directly.
    /// The `current_file` is canonicalized before entering the resolution pipeline
    /// so that all internal code can assume canonical paths.
    pub fn resolve_spec(
        &self,
        load_spec: &LoadSpec,
        current_file: &Path,
    ) -> Result<PathBuf, anyhow::Error> {
        if let LoadSpec::PackageUri { uri, .. } = load_spec {
            let abs = self.resolution.resolve_package_uri(uri)?;
            return self.resolve_spec(&LoadSpec::local_path(abs), current_file);
        }

        let current_file = self.file_provider.canonicalize(current_file)?;
        let mut context =
            ResolveContext::new(self.file_provider(), current_file, load_spec.clone());
        self.resolve(&mut context)
    }

    fn current_package_scope(
        &self,
        file: &Path,
    ) -> anyhow::Result<crate::resolution::ResolvedPackageScope<'_>> {
        self.package_scope_for_file(file).ok_or_else(|| {
            anyhow::anyhow!(
                "Internal error: current file not in any package: {}",
                file.display()
            )
        })
    }

    /// Expand alias using the resolution map.
    fn expand_alias(&self, context: &ResolveContext, alias: &str) -> Result<String, anyhow::Error> {
        let scope = self.current_package_scope(&context.current_file)?;
        if let Some(url) = scope.expand_alias(alias) {
            return Ok(url.to_string());
        }

        anyhow::bail!("Unknown alias '@{}'", alias)
    }

    /// Remote resolution: longest prefix match against package's declared deps.
    fn try_resolve_workspace(
        &self,
        context: &ResolveContext,
        scope: &crate::resolution::ResolvedPackageScope<'_>,
    ) -> Result<PathBuf, anyhow::Error> {
        let full_url = if let LoadSpec::Stdlib { path } = context.latest_spec() {
            let stdlib_root = self.resolution.workspace_info.workspace_stdlib_dir();
            return Ok(if path.as_os_str().is_empty() {
                stdlib_root
            } else {
                stdlib_root.join(path)
            });
        } else {
            context
                .latest_spec()
                .to_full_url()
                .expect("try_resolve_workspace called with non-URL spec")
        };

        let resolved = scope.resolve_package_url(&full_url);
        let is_declared_dependency = matches!(
            resolved.as_ref(),
            Some(PackageUrlResolution::Dependency { .. })
        );
        if let Some(target_package_url) = self
            .resolution
            .workspace_info
            .package_url_for_url(&full_url)
            && scope.package_url() != Some(target_package_url)
            && !is_declared_dependency
        {
            anyhow::bail!(
                "No declared dependency matches '{}' required by '{}'\n  \
                Run `pcb sync` to update [dependencies] in pcb.toml",
                target_package_url,
                full_url
            );
        }

        let (matched_dep, root_path) = match resolved {
            Some(PackageUrlResolution::OwnPackage) => anyhow::bail!(
                "{} uses package URL '{}' that points into its own package '{}'; use a relative path instead",
                context.current_file.display(),
                full_url,
                scope.display()
            ),
            Some(PackageUrlResolution::Dependency { dep_url, root }) => (dep_url, root),
            None => anyhow::bail!(
                "No declared dependency matches '{}'\n  \
                Add a dependency to [dependencies] in pcb.toml that covers this path",
                full_url
            ),
        };

        let relative_path = full_url
            .strip_prefix(matched_dep)
            .and_then(|s| s.strip_prefix('/'))
            .unwrap_or("");

        let full_path = if relative_path.is_empty() {
            root_path.to_path_buf()
        } else {
            root_path.join(relative_path)
        };

        if !self.file_provider.exists(&full_path) {
            anyhow::bail!(
                "File not found: {} (resolved to: {}, dep root: {})",
                relative_path,
                full_path.display(),
                root_path.display()
            );
        }

        Ok(full_path)
    }

    /// URL resolution: translate canonical URL to cache path using resolution map.
    fn resolve_url(&self, context: &mut ResolveContext) -> Result<PathBuf, anyhow::Error> {
        let scope = self.current_package_scope(&context.current_file)?;
        self.try_resolve_workspace(context, &scope)
    }

    /// Compute the canonical URL for a file being evaluated: the owning
    /// package's URL plus the file's relative path within that package.
    fn file_url(&self, file_path: &Path) -> anyhow::Result<String> {
        self.resolution
            .package_url_for_file(
                file_path,
                self.active_root_package.as_deref(),
                self.file_provider(),
            )
            .ok_or_else(|| {
                anyhow::anyhow!("Cannot determine package URL for '{}'", file_path.display())
            })
    }

    /// Relative path resolution: resolve relative to current file with boundary enforcement.
    fn resolve_relative(&self, context: &mut ResolveContext) -> Result<PathBuf, anyhow::Error> {
        let LoadSpec::Path { path, .. } = context.latest_spec() else {
            unreachable!("resolve_relative called on non-Path spec");
        };
        let path = path.clone();

        let scope = self.current_package_scope(&context.current_file)?;
        let package_root = scope.root().to_path_buf();

        let current_dir = context
            .current_file
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Current file has no parent directory"))?;

        let resolved_path = current_dir.join(&path);

        let canonical_resolved = context.file_provider.canonicalize(&resolved_path)?;
        let canonical_root = context.file_provider.canonicalize(&package_root)?;

        // The load crosses a package boundary when the target is owned by a
        // different package than the current file — judged first by the frozen
        // package scopes, then by the (possibly finer-grained) workspace
        // package map. `target_root` is always Some for targets inside the
        // current package root: `current_package_scope` above proved the
        // active scope map contains `canonical_root`, so the ancestor walk in
        // `package_scope_for_file` finds at least that entry.
        let target_root = self
            .package_scope_for_file(&canonical_resolved)
            .map(|target_scope| {
                context
                    .file_provider
                    .canonicalize(target_scope.root())
                    .unwrap_or_else(|_| target_scope.root().to_path_buf())
            });
        let mut crosses_package_boundary = target_root.as_deref() != Some(&canonical_root);
        if !crosses_package_boundary
            && let (Some(current_url), Some(target_url)) = (
                self.resolution
                    .workspace_package_url_for_path(self.file_provider(), &context.current_file),
                self.resolution
                    .workspace_package_url_for_path(self.file_provider(), &canonical_resolved),
            )
        {
            crosses_package_boundary = current_url != target_url;
        }

        if crosses_package_boundary {
            // Escaped package boundary — resolve via URL arithmetic
            let current_url = self.file_url(&context.current_file)?;
            let current_dir_url = current_url
                .rsplit_once('/')
                .map(|(dir, _)| dir)
                .unwrap_or(&current_url);
            let target_url = crate::normalize_url_path(&format!(
                "{}/{}",
                current_dir_url,
                path.to_string_lossy().replace('\\', "/")
            ))?;

            let new_spec = LoadSpec::Package {
                package: target_url,
                path: PathBuf::new(),
            };
            context.push_spec(new_spec)?;
            return self.resolve_url(context);
        }

        crate::validate_path_case_with_canonical(&path, &canonical_resolved)?;

        Ok(canonical_resolved)
    }

    fn finish_resolve(
        &self,
        context: &ResolveContext,
        resolved_path: PathBuf,
    ) -> Result<PathBuf, anyhow::Error> {
        if context.file_provider.exists(&resolved_path) {
            crate::validate_path_case(context.file_provider, &resolved_path)?;
        } else if !context.original_spec().allow_not_exist() {
            return Err(anyhow::anyhow!(
                "File not found: {}",
                resolved_path.display()
            ));
        }

        Ok(resolved_path)
    }

    /// Resolve a load path. Supports aliases, URLs, and relative paths.
    pub(crate) fn resolve(&self, context: &mut ResolveContext) -> Result<PathBuf, anyhow::Error> {
        // Expand aliases
        if let LoadSpec::Package { package, path, .. } = context.latest_spec() {
            let expanded_url = self.expand_alias(context, package)?;
            let expanded_spec = LoadSpec::Package {
                package: expanded_url,
                path: path.clone(),
            };
            if &expanded_spec != context.latest_spec() {
                context.push_spec(expanded_spec)?;
            }
        }

        let resolved_path = match context.latest_spec() {
            LoadSpec::Path { .. } => self.resolve_relative(context)?,
            _ => self.resolve_url(context)?,
        };

        self.finish_resolve(context, resolved_path)
    }
}

impl Default for EvalSession {
    fn default() -> Self {
        Self {
            source_cache: Arc::default(),
            load_cache: Arc::default(),
            footprint_cache: Arc::default(),
            symbol_cache: Arc::default(),
            spice_cache: Arc::default(),
            symbol_index: Arc::new(RwLock::new(HashMap::new())),
            symbol_meta: Arc::new(RwLock::new(HashMap::new())),
            module_deps: Arc::new(RwLock::new(HashMap::new())),
            module_tree: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }
}

impl EvalSession {
    /// Reset per-root evaluation state while preserving reusable caches such as
    /// loaded modules and canonicalized file contents.
    ///
    /// Callers should consume any previous root's `EvalOutput` before resetting,
    /// since schematic conversion reads the shared module tree from the session.
    pub fn prepare_for_root_eval(&self) {
        self.clear_module_tree();
    }

    // --- Module tree ---

    fn insert_module(&self, path: ModulePath, module: FrozenModule) {
        self.module_tree.write().unwrap().insert(path, module);
    }

    fn clone_module_tree(&self) -> BTreeMap<ModulePath, FrozenModuleValue> {
        self.module_tree
            .read()
            .unwrap()
            .iter()
            .map(|(path, module)| {
                let module_value = module
                    .extra_value()
                    .and_then(|extra| extra.downcast_ref::<FrozenContextValue>())
                    .expect("module_tree entry missing FrozenContextValue")
                    .module
                    .clone();
                (path.clone(), module_value)
            })
            .collect()
    }

    fn clear_module_tree(&self) {
        self.module_tree.write().unwrap().clear();
    }

    // --- Derived-data caches ---

    /// Drop everything derived from source files: loaded modules, parsed
    /// sources, and footprint/symbol/spice values.
    pub fn clear_load_cache(&self) {
        self.load_cache.clear();
        self.source_cache.clear();
        self.footprint_cache.clear();
        self.symbol_cache.clear();
        self.spice_cache.clear();
    }

    /// Drop all cached state derived from `path`: parsed source, footprint
    /// diagnostics, and symbol/spice values sourced from it. A symbol library
    /// entry is also dropped when `path` sits inside its split-library
    /// directory.
    fn invalidate_file(&self, path: &Path, footprint_key: Option<FootprintCacheKey>) {
        self.source_cache.remove(&path.to_path_buf());
        if let Some(key) = footprint_key {
            self.footprint_cache.remove(&key);
        }
        self.symbol_cache.retain(|(lib_path, _), _| {
            lib_path != path && Some(lib_path.as_path()) != path.parent()
        });
        self.spice_cache
            .retain(|(model_path, _), _| model_path != path);
    }

    fn clear_symbol_maps(&self, path: &Path) {
        self.symbol_index.write().unwrap().remove(path);
        self.symbol_meta.write().unwrap().remove(path);
    }

    fn clear_module_dependencies(&self, path: &Path) {
        self.module_deps.write().unwrap().remove(path);
    }

    // --- Module dependencies ---

    fn record_module_dependency(&self, from: &Path, to: &Path) {
        self.module_deps
            .write()
            .unwrap()
            .entry(from.to_path_buf())
            .or_default()
            .insert(to.to_path_buf());
    }

    fn module_dep_exists(&self, from: &Path, to: &Path) -> bool {
        self.module_deps
            .read()
            .unwrap()
            .get(from)
            .map(|deps| deps.contains(to))
            .unwrap_or(false)
    }

    fn get_module_dependencies(&self, path: &Path) -> Option<HashSet<PathBuf>> {
        self.module_deps.read().unwrap().get(path).cloned()
    }

    // --- Symbol metadata ---

    fn get_symbol_params(&self, file: &Path, symbol: &str) -> Option<Vec<String>> {
        self.get_symbol_info(file, symbol)?
            .parameters
            .filter(|params| !params.is_empty())
    }

    fn get_symbol_info(&self, file: &Path, symbol: &str) -> Option<crate::SymbolInfo> {
        self.symbol_meta
            .read()
            .unwrap()
            .get(file)
            .and_then(|m| m.get(symbol).cloned())
    }

    fn get_symbols_for_file(&self, path: &Path) -> Option<HashMap<String, crate::SymbolInfo>> {
        self.symbol_meta.read().unwrap().get(path).cloned()
    }

    fn get_symbol_index(&self, path: &Path) -> Option<HashMap<String, PathBuf>> {
        self.symbol_index.read().unwrap().get(path).cloned()
    }

    fn update_symbol_maps(
        &self,
        path: PathBuf,
        symbol_index: HashMap<String, PathBuf>,
        symbol_meta: HashMap<String, crate::SymbolInfo>,
    ) {
        if !symbol_index.is_empty() {
            self.symbol_index
                .write()
                .unwrap()
                .insert(path.clone(), symbol_index);
        }
        if !symbol_meta.is_empty() {
            self.symbol_meta.write().unwrap().insert(path, symbol_meta);
        }
    }

    /// Create an EvalContext from an EvalContextConfig.
    /// This is the primary way to create contexts for evaluation.
    pub fn create_context(&self, config: EvalContextConfig) -> EvalContext {
        EvalContext {
            session: self.clone(),
            config,
            load_diagnostics: RefCell::new(Vec::new()),
            pending_inputs: SmallMap::new(),
            pending_properties: SmallMap::new(),
            pending_parent_component_modifiers: Vec::new(),
            json_inputs: SmallMap::new(),
        }
    }
}

pub struct EvalContext {
    /// The shared session state (module tree, load cache, symbol maps, etc.)
    session: EvalSession,

    /// Configuration for this evaluation context (Send + Sync safe).
    config: EvalContextConfig,

    /// Diagnostics collected during load() calls in this context.
    load_diagnostics: RefCell<Vec<Diagnostic>>,

    /// Values to seed into the active module once its branded heap exists.
    pending_inputs: SmallMap<String, FrozenValue>,
    pending_properties: SmallMap<String, FrozenValue>,
    pending_parent_component_modifiers: Vec<FrozenValue>,
    json_inputs: SmallMap<String, serde_json::Value>,
}

/// Helper to recursively convert JSON to heap values
fn json_value_to_heap_value<'v>(json: &serde_json::Value, heap: Heap<'v>) -> Value<'v> {
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
    ///
    /// Canonicalizes package roots so that path lookups during evaluation match
    /// the canonicalized file paths used elsewhere.
    pub fn new(file_provider: Arc<dyn FileProvider>, resolution: ResolutionResult) -> Self {
        let mut resolution = resolution;
        resolution.canonicalize_keys(&*file_provider);
        let config = EvalContextConfig::new(file_provider, Arc::new(resolution));
        EvalSession::default().create_context(config)
    }

    /// Create an EvalContext from an existing session and config.
    pub fn from_session_and_config(session: EvalSession, config: EvalContextConfig) -> Self {
        session.create_context(config)
    }

    /// Get the current config (for creating child configs).
    pub fn config(&self) -> &EvalContextConfig {
        &self.config
    }

    /// Get the session.
    pub fn session(&self) -> &EvalSession {
        &self.session
    }

    /// Get the source path of the module we are evaluating.
    pub fn source_path(&self) -> Option<&PathBuf> {
        self.config.source_path.as_ref()
    }

    /// Get the module path (fully qualified path in the tree).
    pub fn module_path(&self) -> &ModulePath {
        &self.config.module_path
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
    ) -> EvalContextConfig {
        self.config.child_for_load(child_module_path, target_path)
    }

    pub fn file_provider(&self) -> &dyn FileProvider {
        self.config.file_provider()
    }

    pub fn resolution(&self) -> &ResolutionResult {
        &self.config.resolution
    }

    /// Enable or disable strict IO/config placeholder checking for subsequent evaluations.
    pub fn set_strict_io_config(mut self, enabled: bool) -> Self {
        self.config.strict_io_config = enabled;
        self
    }

    fn frozen_heap_name(&self) -> FrozenHeapName {
        let source = self
            .config
            .source_path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "<unknown>".to_string());
        FrozenHeapName::User(Box::new(format!("{}:{source}", self.config.module_path)))
    }

    /// Enable or disable eager workspace parsing.
    pub fn set_eager(mut self, eager: bool) -> Self {
        self.config.eager = eager;
        self
    }

    /// Enable or disable stdlib prelude injection.
    pub fn set_inject_prelude(mut self, inject: bool) -> Self {
        self.config.inject_prelude = inject;
        self
    }

    /// Create a new Context that shares caches with this one
    pub fn child_context(&self, name: Option<&str>) -> Self {
        let mut module_path = self.config.module_path.clone();
        if let Some(name) = name {
            module_path.push(name);
        }
        let child_config = EvalContextConfig {
            builtin_docs: self.config.builtin_docs.clone(),
            file_provider: self.config.file_provider.clone(),
            resolution: self.config.resolution.clone(),
            module_path,
            load_chain: self.config.load_chain.clone(),
            source_path: None,
            active_root_package: self.config.active_root_package.clone(),
            contents: None,
            strict_io_config: false,
            build_circuit: false,
            eager: self.config.eager,
            inject_prelude: self.config.inject_prelude,
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
    /// Built once per process; `Globals` is cheaply cloneable and shared across threads.
    fn build_globals() -> starlark::environment::Globals {
        static GLOBALS: std::sync::OnceLock<starlark::environment::Globals> =
            std::sync::OnceLock::new();
        GLOBALS
            .get_or_init(|| {
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
            })
            .clone()
    }

    /// Get a clone of the module tree from the session.
    pub fn module_tree(&self) -> BTreeMap<ModulePath, FrozenModuleValue> {
        self.session.clone_module_tree()
    }

    /// Record that `from` references `to` via a `Module()` call.
    pub(crate) fn record_module_dependency(&self, from: &Path, to: &Path) {
        self.session.record_module_dependency(from, to);
    }

    fn load_cache_scope(&self, path: &Path) -> Option<PackageScopeKey> {
        self.config
            .resolution
            .load_cache_scope_key_for_file(path, self.config.active_root_package.as_deref())
    }

    fn get_cached_module(&self, path: &Path) -> Option<CachedModule> {
        let key = (self.load_cache_scope(path), path.to_path_buf());
        self.session.load_cache.get(&key)
    }

    fn cache_module(&self, path: PathBuf, module: CachedModule) {
        let key = (self.load_cache_scope(&path), path);
        self.session.load_cache.insert(key, module);
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
        self.config = self.config.set_source_path(path);
        self
    }

    fn initialize_context_value<'v>(&self, module: &Module<'v>) {
        let heap = module.heap();
        let ctx_value = heap.alloc_complex(ContextValue::from_context(self));
        module.set_extra_value(ctx_value);
        let ctx_value = ctx_value
            .downcast_ref::<ContextValue>()
            .expect("extra value should be a ContextValue");

        {
            let mut module_value = ctx_value.module_mut();
            for (name, value) in self.pending_inputs.iter() {
                module_value.add_input(name.clone(), value.to_value());
            }
            for (name, json) in self.json_inputs.iter() {
                module_value.add_input(name.clone(), json_value_to_heap_value(json, heap));
            }
            let parent_modifiers = self
                .pending_parent_component_modifiers
                .iter()
                .map(|value| value.to_value())
                .collect();
            module_value.set_parent_component_modifiers(parent_modifiers);
        }

        for (name, value) in self.pending_properties.iter() {
            ctx_value.add_property(name.clone(), value.to_value());
        }
    }

    /// Set inputs from already frozen parent values.
    pub fn set_inputs_from_frozen_values(&mut self, parent_inputs: SmallMap<String, FrozenValue>) {
        self.pending_inputs.extend(parent_inputs);
    }

    /// Set properties from already frozen parent values.
    pub fn set_properties_from_frozen_values(
        &mut self,
        parent_properties: SmallMap<String, FrozenValue>,
    ) {
        self.pending_properties.extend(parent_properties);
    }

    /// Set parent component modifiers from already frozen parent values.
    pub fn set_parent_component_modifiers_from_frozen_values(
        &mut self,
        parent_modifiers: Vec<FrozenValue>,
    ) {
        self.pending_parent_component_modifiers = parent_modifiers;
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
        self.json_inputs.extend(json_inputs);
    }

    /// Parse Starlark source with this context's dialect, using the recursive
    /// descent parser (same AST as the default LALRPOP parser, roughly half
    /// the cost).
    fn parse_ast(&self, filename: &str, contents: String) -> starlark::Result<AstModule> {
        let _span = info_span!("parse").entered();
        AstModule::parse_with(
            filename,
            contents,
            &self.dialect(),
            starlark::syntax::ParserKind::Rd,
        )
    }

    /// Contents + AST for the module being evaluated. Explicit contents (e.g.
    /// an editor buffer) parse fresh; otherwise the file is read and parsed
    /// through the session cache, so repeated instantiations of a module do
    /// neither more than once.
    fn parsed_source(&self) -> Result<ParsedSource, Box<WithDiagnostics<EvalOutput>>> {
        let source_path = self
            .config
            .source_path
            .as_deref()
            .expect("source_path is set before eval");
        let parse = |contents: String| {
            self.parse_ast(
                source_path.to_str().expect("path is not a string"),
                contents,
            )
            .map(Arc::new)
            .map_err(|err| Box::new(EvalMessage::from_error(source_path, &err).into()))
        };

        if let Some(contents) = &self.config.contents {
            let contents = contents.clone();
            let ast = parse(contents.clone())?;
            return Ok(ParsedSource { contents, ast });
        }

        if let Some(source) = self.session.source_cache.get(&source_path.to_path_buf()) {
            return Ok(source);
        }

        let contents = self
            .file_provider()
            .read_file(source_path)
            .map_err(|err| Box::new(anyhow::anyhow!("Failed to read file: {err}").into()))?;
        let source = ParsedSource {
            ast: parse(contents.clone())?,
            contents,
        };
        self.session
            .source_cache
            .insert(source_path.to_path_buf(), source.clone());
        Ok(source)
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
        if self.config.source_path.is_none() {
            return anyhow::anyhow!("source_path not set on Context before eval()").into();
        }

        let ParsedSource { contents, ast } = match self.parsed_source() {
            Ok(source) => source,
            Err(failure) => return *failure,
        };
        // Later span lookups (e.g. `resolve_load_span`) read `config.contents`.
        self.config.contents = Some(contents.clone());
        let source_path = self.config.source_path.as_ref().unwrap();

        for diagnostic in binding::check_bindings(&ast, source_path, &contents) {
            self.add_load_diagnostic(diagnostic);
        }
        for diagnostic in explicit_prelude_load_diagnostics(&ast, &self.config) {
            self.add_load_diagnostic(diagnostic);
        }

        Module::with_temp_heap(|module| {
            // Make prelude symbols available before user code runs.
            self.inject_prelude(&module);

            // Attach a `ContextValue` so user code can access evaluation context,
            // then seed any inputs/properties that were collected before the
            // branded Starlark heap existed.
            self.initialize_context_value(&module);

            // Create a print handler to collect output
            let print_handler = CollectingPrintHandler::new();

            let eval_result = {
                let mut eval_context_ref = EvalContextRef::new(&self);
                let mut eval = Evaluator::new(&module);
                eval.enable_static_typechecking(true);
                eval.set_loader(&self);
                eval.set_print_handler(&print_handler);
                eval.extra_mut = Some(&mut eval_context_ref);

                let globals = Self::build_globals();

                // We are only interested in whether evaluation succeeded, not in the
                // value of the final expression, so map the result to `()`.
                let _span = info_span!("starlark_eval").entered();
                eval.eval_module(AstModule::clone(&ast), &globals)
                    .and_then(|_| Self::apply_component_modifiers(&mut eval))
            };

            // Collect print output after evaluation
            let print_output = print_handler.take_output();

            // Collect load diagnostics - this becomes our accumulator for all diagnostics
            let mut diagnostics = self.take_load_diagnostics();

            match eval_result {
                Ok(_) => {
                    let frozen_module = {
                        let _span = info_span!("freeze_module").entered();
                        module
                            .freeze_named(self.frozen_heap_name())
                            .expect("failed to freeze module")
                    };
                    let extra = frozen_module
                        .extra_value()
                        .expect("extra value should be set before freezing")
                        .downcast_ref::<FrozenContextValue>()
                        .expect("extra value should be a FrozenContextValue");

                    for (_id, net_info) in extra.module.introduced_nets() {
                        if net_info.kind != "NotConnected" && net_info.name.is_pending_inference() {
                            diagnostics.push(anyhow!("Net is unnamed").into());
                            return WithDiagnostics {
                                output: None,
                                diagnostics: Diagnostics::from(diagnostics),
                            };
                        }
                    }

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
                                .and_then(|v| serialize_parameter_value(v.to_value()));

                            // Get human-readable display of default value
                            let default_display = param.default_display();
                            let allowed_values = param.allowed_values.as_ref().map(|values| {
                                values
                                    .iter()
                                    .filter_map(|value| serialize_parameter_value(value.to_value()))
                                    .collect()
                            });
                            let allowed_display = param.allowed_display();

                            ParameterInfo {
                                name: param.name.clone(),
                                type_info,
                                required: !param.optional,
                                default_value,
                                default_display,
                                allowed_values,
                                allowed_display,
                                help: param.help.clone(),
                                direction: param.direction,
                            }
                        })
                        .collect();
                    // Process pending children after parent is frozen
                    let module_path = extra.module.path().clone();
                    let is_root = module_path.segments.is_empty();

                    if self.config.build_circuit || is_root {
                        self.session
                            .insert_module(module_path, frozen_module.clone());
                        let process_children_span = info_span!("process_children", module = %extra.module.path().name(), count = extra.pending_children.len());
                        let _guard = process_children_span.enter();

                        let session = self.session.clone();
                        let base_config = self.config.clone();

                        #[cfg(feature = "native")]
                        {
                            // Collect into Vec to preserve deterministic ordering
                            let child_diag_vecs: Vec<Vec<Diagnostic>> = extra
                                .pending_children
                                .par_iter()
                                .map(|pending| {
                                    let child_config =
                                        base_config.child_for_pending(&pending.final_name);
                                    session
                                        .create_context(child_config)
                                        .process_pending_child(pending.clone())
                                })
                                .collect();
                            for child_diags in child_diag_vecs {
                                diagnostics.extend(child_diags);
                            }
                        }

                        #[cfg(not(feature = "native"))]
                        {
                            for pending in extra.pending_children.iter() {
                                let child_config =
                                    base_config.child_for_pending(&pending.final_name);
                                diagnostics.extend(
                                    session
                                        .create_context(child_config)
                                        .process_pending_child(pending.clone()),
                                );
                            }
                        }
                    }

                    // Module's own diagnostics (from ContextValue)
                    diagnostics.extend(extra.diagnostics().iter().cloned());

                    if !diagnostics.iter().any(Diagnostic::is_error) {
                        diagnostics.extend(ast_style_lints(&ast));
                    }

                    let output = EvalOutput {
                        ast,
                        star_module: frozen_module,
                        sch_module: extra.module.clone(),
                        signature,
                        print_output,
                        config: self.config.clone(),
                        session: self.session.clone(),
                    };

                    WithDiagnostics {
                        output: Some(output),
                        diagnostics: Diagnostics::from(diagnostics),
                    }
                }
                Err(err) => {
                    diagnostics.push(err.into());
                    WithDiagnostics {
                        output: None,
                        diagnostics: Diagnostics::from(diagnostics),
                    }
                }
            }
        })
    }

    /// Drop cached state derived from `path` (parsed source, footprint
    /// diagnostics, symbol/spice values). Call when a file changes on disk or
    /// in an editor buffer. The path is canonicalized to match cache keys.
    pub fn invalidate_file(&self, path: &Path) {
        let path = self
            .file_provider()
            .canonicalize(path)
            .unwrap_or_else(|_| path.to_path_buf());
        let footprint_key = (path.extension().and_then(|ext| ext.to_str()) == Some("kicad_mod"))
            .then(|| footprint_cache_key(&path, &self.config));
        self.session.invalidate_file(&path, footprint_key);
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
    ) -> WithDiagnostics<EvalOutput> {
        self.session.clear_load_cache();
        self.session.prepare_for_root_eval();
        self.session.clear_symbol_maps(&path);

        // Evaluate the file
        let result = self
            .child_context(None)
            .set_source_path(path.clone())
            .set_source_contents(contents)
            .eval();

        // Extract symbol information
        if let Some(ref output) = result.output {
            // Replace dependency edges only when evaluation succeeds.
            // On failed evaluations, keep the previous dependency graph so
            // cross-file invalidation can still reach this module.
            self.session.clear_module_dependencies(&path);
            let mut symbol_index: HashMap<String, PathBuf> = HashMap::new();
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
                        if p.is_relative()
                            && let Some(parent) = path.parent()
                        {
                            p = parent.join(&p);
                        }

                        if let Ok(canon) = self.file_provider().canonicalize(&p) {
                            p = canon;
                        }

                        // Record dependency for propagation.
                        self.record_module_dependency(path.as_path(), &p);

                        symbol_index.insert(name_str.to_string(), p.clone());

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

                        let info = crate::SymbolInfo {
                            kind,
                            parameters: None,
                            source_path: None,
                            type_name: typ.to_string(),
                            documentation: None,
                        };
                        symbol_meta.insert(name_str.to_string(), info);
                    }
                }
            }

            // Add prelude symbols to the index so cmd-click works for
            // implicit prelude symbols regardless of what else the file exports.
            if self.config.inject_prelude {
                for &(module_path, symbols) in PRELUDE {
                    if let Ok(resolved) = self.config.resolve_path(module_path, &path) {
                        for &name in symbols {
                            symbol_index
                                .entry(name.to_string())
                                .or_insert(resolved.clone());
                        }
                    }
                }
            }

            // Store/update the maps for this file.
            self.session
                .update_symbol_maps(path.clone(), symbol_index, symbol_meta);
        }

        result
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
        self.parse_ast(&source_path.to_string_lossy(), contents.clone())
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

    /// Get the source path of the current module being evaluated
    pub fn get_source_path(&self) -> Option<&Path> {
        self.config.source_path.as_deref()
    }

    /// Get the eval config
    pub fn get_config(&self) -> &EvalContextConfig {
        &self.config
    }

    /// Append a diagnostic to this context's local collection.
    fn add_load_diagnostic(&self, diag: Diagnostic) {
        self.load_diagnostics.borrow_mut().push(diag);
    }

    /// Take all collected load diagnostics, leaving the collection empty.
    fn take_load_diagnostics(&self) -> Vec<Diagnostic> {
        std::mem::take(&mut *self.load_diagnostics.borrow_mut())
    }

    /// Inject prelude symbols into the module scope before evaluation.
    /// Controlled by `config.inject_prelude`.
    fn inject_prelude<'v>(&self, module: &Module<'v>) {
        if !self.config.inject_prelude {
            return;
        }

        for &(module_path, symbols) in PRELUDE {
            let frozen_module = match self.resolve_and_eval_module(module_path, None) {
                Ok(output) => output.star_module,
                Err(err) => {
                    let mut diagnostic = crate::Diagnostic::new(
                        format!("Failed to load prelude module `{module_path}`"),
                        EvalSeverity::Error,
                        self.config
                            .source_path
                            .as_deref()
                            .unwrap_or_else(|| Path::new("")),
                    )
                    .with_source_error(Some(anyhow::anyhow!(err.to_string())));

                    let child = crate::Diagnostic::from(err);
                    if !child.body.is_empty() || !child.path.is_empty() {
                        diagnostic = diagnostic.with_child(Some(child.boxed()));
                    }

                    self.add_load_diagnostic(diagnostic);
                    continue;
                }
            };

            for &name in symbols {
                if let Ok(owned) = frozen_module.get(name) {
                    module.set(name, module.heap().access_owned_frozen_value(&owned));
                }
            }
        }
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
        let load_config = &self.config;

        let module_path = self.config.source_path.clone();
        let Some(current_file) = module_path.as_ref() else {
            return Err(starlark::Error::new_other(anyhow::anyhow!(
                "Cannot resolve load path '{}' without a current file context",
                path
            )));
        };

        // Resolve the load path to an absolute path
        let canonical_path = load_config.resolve_path(path, current_file)?;

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
        // Resolving the load span requires re-parsing the current file, so only
        // do it when a diagnostic actually needs to point at the load statement.
        let load_span = |span: Option<ResolvedSpan>| span.or_else(|| self.resolve_load_span(path));

        // Fast path: if we've already loaded (and frozen) this module once
        // within the current evaluation context, simply return the cached
        // instance so that callers share the same definitions.
        if let Some(cached) = self.get_cached_module(&canonical_path) {
            if !cached.warnings.is_empty() {
                let span = load_span(span);
                self.add_cached_load_warnings(path, &source_path, span, &cached.warnings);
            }
            return Ok(cached.output);
        }

        if load_config.file_provider.is_directory(&canonical_path) {
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

        // Collect warnings - child body is included in DiagnosticKey for uniqueness
        let warning_diagnostics: Vec<Diagnostic> = result
            .diagnostics
            .iter()
            .filter(|diag| matches!(diag.severity, EvalSeverity::Warning))
            .cloned()
            .collect();
        let needs_span = !warning_diagnostics.is_empty()
            || result.output.is_none()
            || result.diagnostics.iter().any(|d| d.is_error());
        let span = if needs_span { load_span(span) } else { span };
        if !warning_diagnostics.is_empty() {
            self.add_cached_load_warnings(path, &source_path, span, &warning_diagnostics);
        }

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
                related: Vec::new(),
                suppressed: false,
            };
            return Err(diagnostic.into());
        }

        // Cache the result if successful
        if let Some(output) = result.output {
            self.cache_module(
                canonical_path,
                CachedModule {
                    output: output.clone(),
                    warnings: warning_diagnostics,
                },
            );
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
                related: Vec::new(),
                suppressed: false,
            };
            Err(diagnostic.into())
        }
    }

    fn add_cached_load_warnings(
        &self,
        path: &str,
        source_path: &Path,
        span: Option<ResolvedSpan>,
        warnings: &[Diagnostic],
    ) {
        for diag in warnings {
            self.add_load_diagnostic(crate::Diagnostic {
                path: source_path.to_string_lossy().to_string(),
                span,
                severity: diag.severity,
                body: format!("Warning from `{path}`"),
                call_stack: None,
                child: Some(Box::new(diag.clone())),
                source_error: None,
                related: Vec::new(),
                suppressed: false,
            });
        }
    }

    /// Process a pending child after the parent module has been frozen.
    /// Returns diagnostics collected during child evaluation.
    #[instrument(name = "instantiate", skip_all, fields(module = %pending.loader.name))]
    fn process_pending_child(mut self, pending: FrozenPendingChild) -> Vec<Diagnostic> {
        self.config.strict_io_config = true;
        self.config.build_circuit = true;
        self = self.set_source_path(PathBuf::from(&pending.loader.source_path));

        if let Some(props) = pending.properties {
            self.set_properties_from_frozen_values(props);
        }
        self.set_inputs_from_frozen_values(pending.inputs.clone());
        self.set_parent_component_modifiers_from_frozen_values(pending.component_modifiers);

        let child_result = self.eval();

        // Wrap child diagnostics with call site context.
        // Child body is included in DiagnosticKey for uniqueness.
        let mut result: Vec<Diagnostic> = child_result
            .diagnostics
            .iter()
            .map(|child_diag| {
                if is_ast_style_diagnostic(child_diag) {
                    return child_diag.clone();
                }

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

                crate::Diagnostic {
                    path: pending.call_site_path.clone(),
                    span: Some(pending.call_site_span),
                    severity,
                    body: message,
                    call_stack: Some(pending.call_stack.clone()),
                    child: Some(Box::new(child_diag.clone())),
                    source_error: None,
                    related: Vec::new(),
                    suppressed: false,
                }
            })
            .collect();

        // If child evaluation failed, return collected diagnostics
        let Some(output) = child_result.output else {
            return result;
        };

        // Validate unused arguments
        let used_inputs: HashSet<String> = output
            .signature
            .iter()
            .map(|param| param.name.clone())
            .collect();

        let provided_set: HashSet<String> = pending.provided_names.into_iter().collect();
        let unused: Vec<String> = provided_set.difference(&used_inputs).cloned().collect();

        if !unused.is_empty() {
            result.push(crate::Diagnostic {
                path: pending.call_site_path.clone(),
                span: Some(pending.call_site_span),
                severity: EvalSeverity::Error,
                body: format!(
                    "Unknown argument(s) provided to module {}: {}",
                    pending.loader.name,
                    unused.join(", ")
                ),
                call_stack: Some(pending.call_stack.clone()),
                child: None,
                source_error: None,
                related: Vec::new(),
                suppressed: false,
            });
        }

        result
    }
}

// Add FileLoader implementation so that Starlark `load()` works when evaluating modules.
impl FileLoader for EvalContext {
    fn load(&self, path: &str) -> starlark::Result<FrozenModule> {
        let eval_output = self.resolve_and_eval_module(path, None)?;
        Ok(eval_output.star_module)
    }
}

#[cfg(test)]
mod tests {
    use std::{path::Path, sync::Arc};

    use crate::{InMemoryFileProvider, resolution::ResolutionResult};

    use super::*;

    #[test]
    fn invalidate_file_invalidates_canonicalized_footprint_cache_key() {
        let context = EvalContext::new(
            Arc::new(InMemoryFileProvider::empty()),
            ResolutionResult::empty(),
        );
        let cached_path = Path::new("/dir/bad.kicad_mod");
        let invalidation_path = Path::new("/dir/../dir/bad.kicad_mod");
        let key = footprint_cache_key(cached_path, context.config());

        context
            .session
            .footprint_cache
            .insert(key.clone(), Vec::new());
        assert!(context.session.footprint_cache.get(&key).is_some());

        context.invalidate_file(invalidation_path);

        assert!(context.session.footprint_cache.get(&key).is_none());
    }
}
