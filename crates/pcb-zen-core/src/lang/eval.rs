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
    values::{FrozenHeapName, Value, ValueLike},
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
    footprint::validate_footprints,
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
    module::module_globals,
    path::format_relative_path_as_package_uri,
    spice_model::model_globals,
    test_bench::test_bench_globals,
};

/// Stdlib symbols that are implicitly available in user `.zen` files without
/// an explicit `load()` statement. Each entry maps a stdlib module path to the
/// symbol names to inject.
pub const PRELUDE: &[(&str, &[&str])] = &[
    ("@stdlib/io.zen", &["io", "input", "output"]),
    (
        "@stdlib/interfaces.zen",
        &["Net", "Power", "Ground", "NotConnected"],
    ),
    ("@stdlib/properties.zen", &["Project", "Layout", "Part"]),
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
    /// Parsed source and independently owned, completed circuit.
    pub ast: Arc<AstModule>,
    root_path: ModulePath,
    modules: BTreeMap<ModulePath, FrozenModule>,
    /// Ordered list of parameter information
    pub signature: Vec<ParameterInfo>,
    /// Print output collected during evaluation
    pub print_output: Vec<String>,
    resolution: Arc<ResolutionResult>,
    active_root_package: Option<String>,
}

#[derive(Clone)]
struct CachedModule {
    module: FrozenModule,
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

/// Concurrent memoization for an immutable set of evaluation inputs.
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
}

impl EvalOutput {
    pub fn star_module(&self) -> &FrozenModule {
        &self.modules[&self.root_path]
    }

    pub fn sch_module(&self) -> &FrozenModuleValue {
        &frozen_context(self.star_module()).module
    }

    /// Get the resolution result.
    pub fn resolution(&self) -> &crate::resolution::ResolutionResult {
        &self.resolution
    }

    /// Fresh services for deferred checks, preserving the evaluated package scope.
    pub fn check_context(
        &self,
        file_provider: Arc<dyn FileProvider>,
        source_path: PathBuf,
    ) -> EvalContext {
        let mut config = EvalContextConfig::new(file_provider, self.resolution.clone());
        config.active_root_package = self.active_root_package.clone();
        EvalContext::from_caches_and_config(Arc::default(), config.set_source_path(source_path))
    }

    /// Borrow circuit values; their frozen heaps belong to this output.
    pub fn module_tree(&self) -> BTreeMap<ModulePath, &FrozenModuleValue> {
        self.modules
            .iter()
            .map(|(path, module)| (path.clone(), &frozen_context(module).module))
            .collect()
    }

    /// Validate the KiCad footprints referenced by components in the module
    /// tree. Decompresses and hashes embedded payloads, so this is expensive —
    /// callers that actually consume footprints (e.g. layout) opt in.
    pub fn validate_footprints(&self, file_provider: &dyn FileProvider) -> Vec<Diagnostic> {
        validate_footprints(&self.module_tree(), &self.resolution, file_provider)
    }

    /// Convert to schematic with diagnostics
    pub fn to_schematic_with_diagnostics(&self) -> crate::WithDiagnostics<pcb_sch::Schematic> {
        let converter = ModuleConverter::new();
        let module_tree = self.module_tree();
        let mut result = converter.build(module_tree);
        if let Some(ref mut schematic) = result.output {
            schematic.package_roots = self.resolution.package_roots();

            // Resolve project paths to stable, machine-independent package URIs.
            for inst in schematic.instances.values_mut() {
                if inst.kind != pcb_sch::InstanceKind::Module {
                    continue;
                }
                for key in [pcb_sch::ATTR_LAYOUT_PATH, pcb_sch::ATTR_SCHEMATIC_PATH] {
                    let value = inst
                        .attributes
                        .get(key)
                        .and_then(|value| value.string())
                        .map(str::to_owned);
                    if let Some(raw) = value
                        && !raw.starts_with(pcb_sch::PACKAGE_URI_PREFIX)
                    {
                        let source_dir = inst.type_ref.source_path.parent();
                        if let Some(uri) =
                            format_relative_path_as_package_uri(&raw, source_dir, &self.resolution)
                        {
                            inst.add_attribute(
                                key.to_string(),
                                pcb_sch::AttributeValue::String(uri),
                            );
                        }
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
    pub fn collect_testbenches(&self) -> Vec<&crate::lang::test_bench::FrozenTestBenchValue> {
        let mut result = Vec::new();
        let module_tree = self.module_tree();

        // Iterate through all modules in the tree
        for module in module_tree.values() {
            // Get testbenches from this module
            for testbench in module.testbenches() {
                result.push(testbench);
            }
        }

        result
    }

    /// Collect all electrical checks from all modules in the tree
    pub fn collect_electrical_checks(&self) -> Vec<(&FrozenElectricalCheck, &FrozenModuleValue)> {
        let mut result = Vec::new();
        let module_tree = self.module_tree();
        for module in module_tree.values() {
            for check in module.electrical_checks() {
                result.push((check, *module));
            }
        }
        result
    }
}

fn frozen_context(module: &FrozenModule) -> &FrozenContextValue {
    module
        .extra_value()
        .unwrap()
        .downcast_ref::<FrozenContextValue>()
        .unwrap()
}

/// Reusable caches for unchanged sources and resolution. Entries never own
/// evaluators, completed circuits, or a reference back to these caches.
#[derive(Default)]
pub struct EvalCaches {
    /// On-disk contents and parsed AST per module path, so repeated
    /// instantiations of the same module skip the disk read and reparse.
    pub(crate) source_cache: CacheMap<PathBuf, ParsedSource>,
    /// Loaded (frozen) modules. Frozen package resolution is package-local,
    /// so cached modules are keyed by the loaded file's package identity and
    /// resolved dependency map.
    load_cache: CacheMap<LoadCacheKey, CachedModule>,
    /// `Symbol(library = ...)` values keyed by resolved library path and
    /// symbol name.
    pub(crate) symbol_cache: CacheMap<SymbolCacheKey, crate::lang::symbol::SymbolValue>,
    /// Spice subcircuits keyed by resolved model path and subcircuit name.
    pub(crate) spice_cache: CacheMap<SpiceCacheKey, crate::lang::spice_model::CachedSpiceModel>,
}

/// Configuration for creating an EvalContext. Send + Sync safe for passing across threads.
/// Evaluation caches are supplied separately from per-root configuration.
#[derive(Clone)]
pub struct EvalContextConfig {
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
        Self {
            file_provider,
            resolution,
            module_path: ModulePath::root(),
            load_chain: HashSet::new(),
            source_path: None,
            active_root_package: None,
            contents: None,
            strict_io_config: false,
            inject_prelude: true,
        }
    }

    /// Set the source path of the module we are evaluating.
    pub fn set_source_path(mut self, path: PathBuf) -> Self {
        self.inject_prelude = self.inject_prelude
            && !path_starts_with_canonical(
                &path,
                &self.resolution.workspace_info.workspace_stdlib_dir(),
                self.file_provider.as_ref(),
            );
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
            file_provider: self.file_provider.clone(),
            resolution: self.resolution.clone(),
            module_path: child_module_path,
            load_chain: child_load_chain,
            source_path: None,
            active_root_package: self.active_root_package.clone(),
            contents: None,
            strict_io_config: false,
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
            file_provider: self.file_provider.clone(),
            resolution: self.resolution.clone(),
            module_path: child_module_path,
            load_chain: HashSet::new(),
            source_path: None,
            active_root_package: self.active_root_package.clone(),
            contents: None,
            strict_io_config: false,
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
        let mut full_url = if let LoadSpec::Stdlib { path } = context.latest_spec() {
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

        let resolved = crate::package_url::resolve_package_reference(&full_url, |candidate| {
            scope.resolve_package_url(candidate)
        });
        let resolved = match resolved {
            Some((matched_url, resolved)) => {
                let matched_url = matched_url.into_owned();
                full_url = matched_url;
                Some(resolved)
            }
            None => None,
        };
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

pub struct EvalContext {
    caches: Arc<EvalCaches>,

    /// Configuration for this evaluation context (Send + Sync safe).
    config: EvalContextConfig,

    /// Diagnostics collected during load() calls in this context.
    load_diagnostics: RefCell<Vec<Diagnostic>>,

    /// Instantiation values and the frozen parent that owns them.
    parent: Option<(FrozenModule, FrozenPendingChild)>,
    json_inputs: SmallMap<String, serde_json::Value>,
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
        Self::from_caches_and_config(Arc::default(), config)
    }

    /// Reuse input caches across evaluations of unchanged sources.
    pub fn from_caches_and_config(caches: Arc<EvalCaches>, config: EvalContextConfig) -> Self {
        Self {
            caches,
            config,
            load_diagnostics: RefCell::new(Vec::new()),
            parent: None,
            json_inputs: SmallMap::new(),
        }
    }

    /// Get the current config (for creating child configs).
    pub fn config(&self) -> &EvalContextConfig {
        &self.config
    }

    pub(crate) fn caches(&self) -> &EvalCaches {
        &self.caches
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
        FrozenHeapName::user(format!("{}:{source}", self.config.module_path))
    }

    /// Enable or disable stdlib prelude injection.
    pub fn set_inject_prelude(mut self, inject: bool) -> Self {
        self.config.inject_prelude = inject;
        self
    }

    fn dialect(&self) -> Dialect {
        let mut dialect = Dialect::Extended;
        dialect.enable_f_strings = true;
        dialect
    }

    /// Construct the `Globals` used when evaluating modules. Kept in one place so the
    /// configuration stays consistent between the main evaluator and nested `load()`s.
    /// Built once per process; `Globals` is cheaply cloneable and shared across threads.
    pub fn build_globals() -> starlark::environment::Globals {
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

    fn load_cache_scope(&self, path: &Path) -> Option<PackageScopeKey> {
        self.config
            .resolution
            .load_cache_scope_key_for_file(path, self.config.active_root_package.as_deref())
    }

    fn get_cached_module(&self, path: &Path) -> Option<CachedModule> {
        let key = (self.load_cache_scope(path), path.to_path_buf());
        self.caches.load_cache.get(&key)
    }

    fn cache_module(&self, path: PathBuf, module: CachedModule) {
        let key = (self.load_cache_scope(&path), path);
        self.caches.load_cache.insert(key, module);
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
            for (name, json) in self.json_inputs.iter() {
                module_value.add_input(name.clone(), heap.alloc(json));
            }
        }

        if let Some((parent, pending)) = &self.parent {
            // Every value transferred below belongs to the frozen parent.
            heap.add_reference(parent.frozen_heap());
            {
                let mut module_value = ctx_value.module_mut();
                for (name, value) in &pending.inputs {
                    module_value.add_input(name.clone(), value.to_value());
                }
                module_value.set_parent_component_modifiers(
                    pending
                        .component_modifiers
                        .iter()
                        .map(|v| v.to_value())
                        .collect(),
                );
            }
            if let Some(properties) = &pending.properties {
                for (name, value) in properties {
                    ctx_value.add_property(name.clone(), value.to_value());
                }
            }
        }
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

    /// Parse Starlark source with this context's dialect.
    fn parse_ast(&self, filename: &str, contents: String) -> starlark::Result<AstModule> {
        let _span = info_span!("parse").entered();
        AstModule::parse(filename, contents, &self.dialect())
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

        if let Some(source) = self.caches.source_cache.get(&source_path.to_path_buf()) {
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
        self.caches
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
    pub fn eval(self) -> WithDiagnostics<EvalOutput> {
        let config = self.config.clone();
        let caches = self.caches.clone();
        let mut result = self.eval_body();
        if let Some(output) = &mut result.output {
            let parent = output.star_module();
            let pending = &frozen_context(parent).pending_children;
            #[cfg(feature = "native")]
            let children = pending.par_iter();
            #[cfg(not(feature = "native"))]
            let children = pending.iter();
            let children: Vec<_> = children
                .map(|pending| {
                    Self::from_caches_and_config(
                        caches.clone(),
                        config.child_for_pending(&pending.final_name),
                    )
                    .process_pending_child(parent.clone(), pending.clone())
                })
                .collect();
            for child in children {
                result
                    .diagnostics
                    .diagnostics
                    .extend(child.diagnostics.diagnostics);
                if let Some(child) = child.output {
                    output.modules.extend(child.modules);
                }
            }
        }
        result
    }

    /// Evaluate and freeze one body, without instantiating its children.
    fn eval_body(mut self) -> WithDiagnostics<EvalOutput> {
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

                    let signature: Vec<ParameterInfo> = extra
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

                    let mut unknown_inputs: Vec<_> = self
                        .json_inputs
                        .keys()
                        .filter(|name| !signature.iter().any(|param| param.name == **name))
                        .cloned()
                        .collect();
                    unknown_inputs.sort();
                    if !unknown_inputs.is_empty() {
                        diagnostics.push(Diagnostic::new(
                            format!("Unknown root input(s): {}", unknown_inputs.join(", ")),
                            EvalSeverity::Error,
                            source_path,
                        ));
                    }

                    // Module's own diagnostics (from ContextValue)
                    diagnostics.extend(extra.diagnostics().iter().cloned());

                    if !diagnostics.iter().any(Diagnostic::is_error) {
                        diagnostics.extend(ast_style_lints(&ast));
                    }

                    let output = EvalOutput {
                        ast,
                        root_path: self.config.module_path.clone(),
                        modules: BTreeMap::from([(self.config.module_path.clone(), frozen_module)]),
                        signature,
                        print_output,
                        resolution: self.config.resolution.clone(),
                        active_root_package: self.config.active_root_package.clone(),
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
                Ok(module) => module,
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
    ) -> starlark::Result<FrozenModule> {
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
            return Ok(cached.module);
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

        let result = Self::from_caches_and_config(self.caches.clone(), child_config).eval_body();

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
            let module = output.star_module().clone();
            self.cache_module(
                canonical_path,
                CachedModule {
                    module: module.clone(),
                    warnings: warning_diagnostics,
                },
            );
            Ok(module)
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
    /// Returns the completed child circuit and its call-site diagnostics.
    #[instrument(name = "instantiate", skip_all, fields(module = %pending.loader.name))]
    fn process_pending_child(
        mut self,
        parent: FrozenModule,
        pending: FrozenPendingChild,
    ) -> WithDiagnostics<EvalOutput> {
        self.config.strict_io_config = true;
        self = self.set_source_path(PathBuf::from(&pending.loader.source_path));
        self.parent = Some((parent, pending.clone()));

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
            return WithDiagnostics {
                output: None,
                diagnostics: Diagnostics::from(result),
            };
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

        WithDiagnostics {
            output: Some(output),
            diagnostics: Diagnostics::from(result),
        }
    }
}

// Add FileLoader implementation so that Starlark `load()` works when evaluating modules.
impl FileLoader for EvalContext {
    fn load(&self, path: &str) -> starlark::Result<FrozenModule> {
        self.resolve_and_eval_module(path, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(
        Debug,
        starlark::any::ProvidesStaticType,
        starlark::values::NoSerialize,
        allocative::Allocative,
    )]
    struct HeapWitness {
        #[allocative(skip)]
        _owner: Arc<()>,
    }

    impl std::fmt::Display for HeapWitness {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "heap witness")
        }
    }

    starlark::starlark_simple_value!(HeapWitness);

    #[starlark::values::starlark_value(type = "heap_witness")]
    impl<'v> starlark::values::StarlarkValue<'v> for HeapWitness {}

    #[test]
    fn evaluations_release_caches_and_frozen_heaps_on_success_and_failure() {
        use crate::resolution::{FrozenPackage, FrozenPackageIdentity, FrozenResolutionMap};

        for (source, success) in [
            ("load('library.zen', 'witness')\n", true),
            (
                "load('library.zen', 'witness')\nfail('root failed')\n",
                false,
            ),
            (
                "load('library.zen', 'witness')\nChild = Module('child.zen')\nChild()\n",
                false,
            ),
            (
                "load('library.zen', 'witness')\nChild = Module('child.zen')\nChild(name='child', data='parent ' + 'payload')\n",
                true,
            ),
        ] {
            let provider = Arc::new(crate::InMemoryFileProvider::new(HashMap::from([
                ("/root.zen".to_string(), source.to_string()),
                ("/library.zen".to_string(), String::new()),
                ("/child.zen".to_string(), "data = config(str)\n".to_string()),
            ])));
            let resolution = ResolutionResult::frozen(
                ResolutionResult::empty().workspace_info,
                BTreeMap::from([(
                    "test".to_string(),
                    FrozenResolutionMap {
                        selected_remote: BTreeMap::new(),
                        packages: BTreeMap::from([(
                            PathBuf::from("/"),
                            FrozenPackage {
                                identity: FrozenPackageIdentity::Workspace("test".to_string()),
                                deps: BTreeMap::new(),
                                parts: Vec::new(),
                            },
                        )]),
                    },
                )]),
                HashMap::new(),
            );
            let mut context = EvalContext::new(provider, resolution)
                .set_inject_prelude(false)
                .set_source_path(PathBuf::from("/root.zen"));
            context.config.active_root_package = Some("test".to_string());
            let caches = Arc::downgrade(&context.caches);
            let witness = Arc::new(());
            let heap_witness = Arc::downgrade(&witness);
            let library = Module::with_temp_heap(|module| {
                module.set(
                    "witness",
                    module.heap().alloc(HeapWitness { _owner: witness }),
                );
                module.freeze().unwrap()
            });
            context.cache_module(
                PathBuf::from("/library.zen"),
                CachedModule {
                    module: library,
                    warnings: Vec::new(),
                },
            );
            let result = context.eval();
            assert!(caches.upgrade().is_none(), "evaluation retained its caches");
            assert_eq!(result.is_success(), success, "{:?}", result.diagnostics);
            if result.output.is_some() {
                assert!(
                    heap_witness.upgrade().is_some(),
                    "output lost its loaded heap"
                );
            }
            let child = result.output.as_ref().and_then(|output| {
                output
                    .modules
                    .get(&ModulePath::from("child".to_string()))
                    .cloned()
            });
            drop(result);
            if let Some(child) = child {
                assert!(
                    heap_witness.upgrade().is_some(),
                    "child lost its parent heap"
                );
                if success {
                    assert_eq!(
                        child.get("data").unwrap().value().unpack_str(),
                        Some("parent payload")
                    );
                }
            }
            assert!(
                heap_witness.upgrade().is_none(),
                "evaluation leaked its loaded heap"
            );
        }
    }

    #[test]
    #[cfg(all(unix, feature = "native"))]
    fn set_source_path_handles_symlinked_stdlib() -> anyhow::Result<()> {
        use std::{fs, os::unix::fs::symlink};

        let dir = tempfile::tempdir()?;
        let root = dir.path().canonicalize()?;
        let mut resolution = ResolutionResult::empty();
        resolution.workspace_info.root = root.clone();
        let stdlib_dir = resolution.workspace_info.workspace_stdlib_dir();
        fs::create_dir_all(&stdlib_dir)?;
        fs::write(stdlib_dir.join("interfaces.zen"), "")?;
        fs::write(root.join("board.zen"), "")?;
        let linked = root.join("linked");
        symlink(&root, &linked)?;
        let config = EvalContextConfig::new(
            Arc::new(crate::DefaultFileProvider::new()),
            Arc::new(resolution),
        );

        for path in [&root, &linked] {
            let stdlib_path = path.join(".pcb/stdlib/interfaces.zen");
            assert_eq!(
                stdlib_path.canonicalize()?,
                stdlib_dir.join("interfaces.zen")
            );
            assert!(!config.clone().set_source_path(stdlib_path).inject_prelude);
            let user_path = path.join("board.zen");
            assert!(
                config
                    .clone()
                    .set_source_path(user_path.clone())
                    .inject_prelude
            );
            assert!(
                !config
                    .clone()
                    .set_inject_prelude(false)
                    .set_source_path(user_path)
                    .inject_prelude
            );
        }
        Ok(())
    }
}
