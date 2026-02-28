#![allow(clippy::arc_with_non_send_sync)]

use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
};

use anyhow::anyhow;
use starlark::{
    PrintHandler,
    environment::{GlobalsBuilder, LibraryExtension},
    errors::{EvalMessage, EvalSeverity},
    eval::{Evaluator, FileLoader},
    syntax::{AstModule, Dialect},
    typing::TypeMap,
    values::{FrozenValue, Heap, Value, ValueLike},
};
use starlark::{codemap::ResolvedSpan, collections::SmallMap, values::FrozenHeap};
use starlark::{environment::FrozenModule, typing::Interface};

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
use crate::load_spec::LoadSpec;
use crate::resolution::ResolutionResult;
use crate::{Diagnostic, Diagnostics, WithDiagnostics};
use crate::{FileProvider, ResolveContext, config};
use crate::{convert::ModuleConverter, lang::context::FrozenPendingChild};

use super::{
    context::{ContextValue, FrozenContextValue},
    interface::interface_globals,
    module::{ModuleLoader, module_globals},
    path::format_relative_path_as_package_uri,
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
    /// Eval config (file provider, path specs, etc.)
    pub config: EvalContextConfig,
    /// Session keeps the frozen heap alive for the lifetime of this output.
    session: EvalSession,
    /// Snapshot of the module tree taken at evaluation time so that later
    /// `to_schematic()` calls return only the modules from this evaluation,
    /// not modules accumulated from other files evaluated in the same session.
    module_tree_snapshot: BTreeMap<ModulePath, FrozenModuleValue>,
}

/// Output of `parse_and_analyze_file`, preserving both parsed AST and full eval output.
#[derive(Clone)]
pub struct ParseAndAnalyzeOutput {
    pub ast: AstModule,
    pub eval_output: EvalOutput,
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

    /// Get the module tree snapshot taken at evaluation time.
    pub fn module_tree(&self) -> &BTreeMap<ModulePath, FrozenModuleValue> {
        &self.module_tree_snapshot
    }

    /// Convert to schematic with diagnostics
    pub fn to_schematic_with_diagnostics(&self) -> crate::WithDiagnostics<pcb_sch::Schematic> {
        let converter = ModuleConverter::new();
        let mut result = converter.build(self.module_tree());
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
#[allow(clippy::type_complexity)]
pub struct EvalSession {
    /// Dedicated file contents cache - frequently accessed during preload scanning.
    file_contents_cache: Arc<RwLock<HashMap<PathBuf, String>>>,
    /// Dedicated load cache - frequently accessed during parallel module evaluation.
    load_cache: Arc<RwLock<HashMap<PathBuf, EvalOutput>>>,
    /// Per-file mapping of `symbol → target path` for "go-to definition".
    symbol_index: Arc<RwLock<HashMap<PathBuf, HashMap<String, PathBuf>>>>,
    /// Per-file mapping of `symbol → parameter list` for signature help.
    symbol_params: Arc<RwLock<HashMap<PathBuf, HashMap<String, Vec<String>>>>>,
    /// Per-file mapping of `symbol → metadata` (kind, docs, etc.)
    symbol_meta: Arc<RwLock<HashMap<PathBuf, HashMap<String, crate::SymbolInfo>>>>,
    /// Map of `module.zen` → set of files referenced via `load()`.
    module_deps: Arc<RwLock<HashMap<PathBuf, HashSet<PathBuf>>>>,
    /// Cache of type maps for each module.
    #[allow(dead_code)]
    type_cache: Arc<RwLock<HashMap<PathBuf, TypeMap>>>,
    /// Per-file interface map for load path → Interface.
    #[allow(dead_code)]
    interface_map: Arc<RwLock<HashMap<PathBuf, HashMap<String, Interface>>>>,
    /// Tree of all child modules indexed by fully qualified path.
    module_tree: Arc<RwLock<BTreeMap<ModulePath, FrozenModuleValue>>>,
    /// Shared frozen heap for the entire evaluation tree.
    frozen_heap: Arc<Mutex<FrozenHeap>>,
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

    /// Maps resolved paths to their original LoadSpecs.
    /// Shared across all contexts so that nested loads see parent tracking.
    pub(crate) path_to_spec: Arc<RwLock<HashMap<PathBuf, LoadSpec>>>,

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

impl EvalContextConfig {
    /// Create a new root EvalContextConfig.
    ///
    /// The resolution's `package_resolutions` keys should already be
    /// canonicalized (see [`EvalContext::new`] which handles this).
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
            path_to_spec: Arc::new(RwLock::new(HashMap::new())),
            module_path: ModulePath::root(),
            load_chain: HashSet::new(),
            source_path: None,
            contents: None,
            strict_io_config: false,
            build_circuit: false,
            eager: true,
        }
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
            file_provider: self.file_provider.clone(),
            resolution: self.resolution.clone(),
            path_to_spec: self.path_to_spec.clone(),
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
            file_provider: self.file_provider.clone(),
            resolution: self.resolution.clone(),
            path_to_spec: self.path_to_spec.clone(),
            module_path: child_module_path,
            load_chain: HashSet::new(),
            source_path: None,
            contents: None,
            strict_io_config: false,
            build_circuit: false,
            eager: self.eager,
        }
    }

    pub(crate) fn file_provider(&self) -> &dyn FileProvider {
        &*self.file_provider
    }

    fn insert_load_spec(&self, resolved_path: PathBuf, spec: LoadSpec) {
        self.path_to_spec
            .write()
            .unwrap()
            .insert(resolved_path, spec);
    }

    /// Manually track a file. Useful for entrypoints.
    pub fn track_file(&self, path: &Path) {
        let canonical_path = self.file_provider.canonicalize(path).unwrap();
        if self.get_load_spec(&canonical_path).is_some() {
            return;
        }
        let load_spec = LoadSpec::local_path(&canonical_path);
        self.insert_load_spec(canonical_path, load_spec);
    }

    /// Get the LoadSpec for a specific resolved file path.
    pub fn get_load_spec(&self, path: &Path) -> Option<LoadSpec> {
        self.path_to_spec.read().unwrap().get(path).cloned()
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
        self.track_file(&current_file);
        let mut context =
            ResolveContext::new(self.file_provider(), current_file, load_spec.clone());
        self.resolve(&mut context)
    }

    /// Find the package root for a given file by walking up directories.
    /// Expects a canonical (absolute, normalized) path.
    fn find_package_root_for_file(&self, file: &Path) -> anyhow::Result<PathBuf> {
        let mut current = file.parent();
        while let Some(dir) = current {
            if self.resolution.package_resolutions.contains_key(dir) {
                return Ok(dir.to_path_buf());
            }
            let pcb_toml = dir.join("pcb.toml");
            if self.file_provider.exists(&pcb_toml) {
                return Ok(dir.to_path_buf());
            }
            current = dir.parent();
        }
        anyhow::bail!(
            "Internal error: current file not in any package: {}",
            file.display()
        )
    }

    fn resolved_map_for_package_root(
        &self,
        package_root: &Path,
    ) -> anyhow::Result<&BTreeMap<String, PathBuf>> {
        self.resolution
            .package_resolutions
            .get(package_root)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Dependency map not loaded for package '{}'",
                    package_root.display()
                )
            })
    }

    /// Expand alias using the resolution map.
    fn expand_alias(&self, context: &ResolveContext, alias: &str) -> Result<String, anyhow::Error> {
        let package_root = self.find_package_root_for_file(&context.current_file)?;
        let resolved_map = self.resolved_map_for_package_root(&package_root)?;

        for url in resolved_map.keys() {
            if let Some(last_segment) = url.rsplit('/').next()
                && last_segment == alias
            {
                return Ok(url.clone());
            }
        }

        for (kicad_alias, base_url, _) in config::KICAD_ASSETS {
            if *kicad_alias == alias {
                return Ok(base_url.to_string());
            }
        }

        anyhow::bail!("Unknown alias '@{}'", alias)
    }

    /// Remote resolution: longest prefix match against package's declared deps.
    fn try_resolve_workspace(
        &self,
        context: &ResolveContext,
        package_root: &Path,
    ) -> Result<PathBuf, anyhow::Error> {
        let spec = context.latest_spec();
        let full_url = spec
            .to_full_url()
            .expect("try_resolve_workspace called with non-URL spec");

        let resolved_map = self.resolved_map_for_package_root(package_root)?;

        let best_match = resolved_map.iter().rev().find(|(dep_url, _)| {
            full_url.starts_with(dep_url.as_str())
                && (full_url.len() == dep_url.len()
                    || full_url.as_bytes().get(dep_url.len()) == Some(&b'/'))
        });

        let Some((matched_dep, root_path)) = best_match else {
            anyhow::bail!(
                "No declared dependency or asset matches '{}'\n  \
                Add a dependency to [dependencies] or an asset to [assets] in pcb.toml that covers this path",
                full_url
            );
        };

        let relative_path = full_url
            .strip_prefix(matched_dep.as_str())
            .and_then(|s| s.strip_prefix('/'))
            .unwrap_or("");

        let full_path = if relative_path.is_empty() {
            root_path.clone()
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

        self.insert_load_spec(full_path.clone(), spec.clone());
        Ok(full_path)
    }

    /// URL resolution: translate canonical URL to cache path using resolution map.
    fn resolve_url(&self, context: &mut ResolveContext) -> Result<PathBuf, anyhow::Error> {
        let package_root = self.find_package_root_for_file(&context.current_file)?;
        self.try_resolve_workspace(context, &package_root)
    }

    /// Find the package URL for a given canonical package root path by scanning
    /// workspace members and resolution maps.
    // TODO: if this becomes a bottleneck, pre-build a reverse map (PathBuf -> URL) at init time.
    fn find_url_for_package_root(&self, canonical_root: &Path) -> Option<String> {
        let ws = &self.resolution.workspace_info;
        for (url, member) in &ws.packages {
            let dir = member.dir(&ws.root);
            let canon = self.file_provider.canonicalize(&dir).unwrap_or(dir);
            if canon == canonical_root {
                return Some(url.clone());
            }
        }
        for resolved_map in self.resolution.package_resolutions.values() {
            for (dep_url, dep_path) in resolved_map {
                if dep_path == canonical_root {
                    return Some(dep_url.clone());
                }
            }
        }
        None
    }

    /// Compute the canonical URL for a file being evaluated.
    ///
    /// For files loaded via URL (remote deps), reconstructs from their tracked LoadSpec.
    /// For local files (entry points, intra-package loads), uses the reverse map to find
    /// the package URL and appends the file's relative path within that package.
    fn file_url(&self, file_path: &Path) -> anyhow::Result<String> {
        // Fast path: check if the file was loaded via a URL-based LoadSpec
        if let Some(spec) = self.get_load_spec(file_path)
            && let Some(url) = spec.to_full_url()
        {
            return Ok(url);
        }

        // Slow path: scan workspace members and resolution maps to find the URL
        let pkg_root = self.find_package_root_for_file(file_path)?;
        let canonical_root = self.file_provider.canonicalize(&pkg_root)?;

        let pkg_url = self
            .find_url_for_package_root(&canonical_root)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Cannot determine package URL for '{}' (package root: '{}')",
                    file_path.display(),
                    pkg_root.display()
                )
            })?;

        let rel = file_path
            .strip_prefix(&canonical_root)
            .or_else(|_| file_path.strip_prefix(&pkg_root))
            .unwrap_or(Path::new(""));

        if rel.as_os_str().is_empty() {
            Ok(pkg_url.clone())
        } else {
            Ok(format!("{}/{}", pkg_url, rel.display()))
        }
    }

    /// Relative path resolution: resolve relative to current file with boundary enforcement.
    fn resolve_relative(&self, context: &mut ResolveContext) -> Result<PathBuf, anyhow::Error> {
        let LoadSpec::Path { path, .. } = context.latest_spec() else {
            unreachable!("resolve_relative called on non-Path spec");
        };
        let path = path.clone();

        let package_root = self.find_package_root_for_file(&context.current_file)?;

        let current_dir = context
            .current_file
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Current file has no parent directory"))?;

        let resolved_path = current_dir.join(&path);

        let canonical_resolved = context.file_provider.canonicalize(&resolved_path)?;
        let canonical_root = context.file_provider.canonicalize(&package_root)?;

        if !canonical_resolved.starts_with(&canonical_root) {
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

    /// Resolve a load path. Supports aliases, URLs, and relative paths.
    pub(crate) fn resolve(&self, context: &mut ResolveContext) -> Result<PathBuf, anyhow::Error> {
        // Expand aliases
        if let LoadSpec::Package { package, path, .. } = context.latest_spec() {
            let expanded_url = self.expand_alias(context, package)?;
            let full_url = if path.as_os_str().is_empty() {
                expanded_url
            } else {
                format!("{}/{}", expanded_url, path.display())
            };

            let expanded_spec = LoadSpec::parse(&full_url)
                .ok_or_else(|| anyhow::anyhow!("Failed to parse expanded alias: {}", full_url))?;
            context.push_spec(expanded_spec)?;
        }

        let resolved_path = match context.latest_spec() {
            LoadSpec::Github { .. } | LoadSpec::Gitlab { .. } => self.resolve_url(context)?,
            LoadSpec::Path { .. } => self.resolve_relative(context)?,
            LoadSpec::Package { .. } => unreachable!("Package checked above"),
            LoadSpec::PackageUri { .. } => unreachable!("PackageUri resolved in resolve_context"),
        };

        if !context.file_provider.exists(&resolved_path)
            && !context.original_spec().allow_not_exist()
        {
            return Err(anyhow::anyhow!(
                "File not found: {}",
                resolved_path.display()
            ));
        }

        if context.file_provider.exists(&resolved_path) {
            crate::validate_path_case(context.file_provider, &resolved_path)?;
        }

        Ok(resolved_path)
    }
}

impl Default for EvalSession {
    fn default() -> Self {
        Self {
            file_contents_cache: Arc::new(RwLock::new(HashMap::new())),
            load_cache: Arc::new(RwLock::new(HashMap::new())),
            symbol_index: Arc::new(RwLock::new(HashMap::new())),
            symbol_params: Arc::new(RwLock::new(HashMap::new())),
            symbol_meta: Arc::new(RwLock::new(HashMap::new())),
            module_deps: Arc::new(RwLock::new(HashMap::new())),
            type_cache: Arc::new(RwLock::new(HashMap::new())),
            interface_map: Arc::new(RwLock::new(HashMap::new())),
            module_tree: Arc::new(RwLock::new(BTreeMap::new())),
            frozen_heap: Arc::new(Mutex::new(FrozenHeap::new())),
        }
    }
}

impl EvalSession {
    // --- Module tree ---

    fn insert_module(&self, path: ModulePath, module: FrozenModuleValue) {
        self.module_tree.write().unwrap().insert(path, module);
    }

    fn clone_module_tree(&self) -> BTreeMap<ModulePath, FrozenModuleValue> {
        self.module_tree.read().unwrap().clone()
    }

    fn clear_module_tree(&self) {
        self.module_tree.write().unwrap().clear();
    }

    // --- Load cache ---

    fn get_cached_module(&self, path: &Path) -> Option<EvalOutput> {
        self.load_cache.read().unwrap().get(path).cloned()
    }

    fn cache_module(&self, path: PathBuf, module: EvalOutput) {
        self.load_cache.write().unwrap().insert(path, module);
    }

    fn clear_load_cache(&self) {
        self.load_cache.write().unwrap().clear();
    }

    fn clear_file_contents(&self, path: &Path) {
        self.file_contents_cache.write().unwrap().remove(path);
    }

    fn clear_symbol_maps(&self, path: &Path) {
        self.symbol_index.write().unwrap().remove(path);
        self.symbol_params.write().unwrap().remove(path);
        self.symbol_meta.write().unwrap().remove(path);
    }

    fn clear_module_dependencies(&self, path: &Path) {
        self.module_deps.write().unwrap().remove(path);
    }

    // --- File contents ---

    fn get_file_contents(&self, path: &Path) -> Option<String> {
        self.file_contents_cache.read().unwrap().get(path).cloned()
    }

    fn set_file_contents(&self, path: PathBuf, contents: String) {
        self.file_contents_cache
            .write()
            .unwrap()
            .insert(path, contents);
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
        self.symbol_params
            .read()
            .unwrap()
            .get(file)
            .and_then(|m| m.get(symbol).cloned())
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
        symbol_params: HashMap<String, Vec<String>>,
        symbol_meta: HashMap<String, crate::SymbolInfo>,
    ) {
        if !symbol_index.is_empty() {
            self.symbol_index
                .write()
                .unwrap()
                .insert(path.clone(), symbol_index);
        }
        if !symbol_params.is_empty() {
            self.symbol_params
                .write()
                .unwrap()
                .insert(path.clone(), symbol_params);
        }
        if !symbol_meta.is_empty() {
            self.symbol_meta.write().unwrap().insert(path, symbol_meta);
        }
    }

    /// Add a reference to the shared frozen heap.
    fn add_frozen_heap_reference(&self, heap: &starlark::values::FrozenHeapRef) {
        self.frozen_heap.lock().unwrap().add_reference(heap);
    }

    /// Create an EvalContext from an EvalContextConfig.
    /// This is the primary way to create contexts for evaluation.
    pub fn create_context(&self, config: EvalContextConfig) -> EvalContext {
        EvalContext {
            module: starlark::environment::Module::new(),
            session: self.clone(),
            config,
            current_load_index: RefCell::new(0),
            load_diagnostics: RefCell::new(Vec::new()),
        }
    }
}

pub struct EvalContext {
    /// The starlark::environment::Module we are evaluating.
    pub module: starlark::environment::Module,

    /// The shared session state (module_tree, frozen_heap, etc.)
    session: EvalSession,

    /// Configuration for this evaluation context (Send + Sync safe).
    config: EvalContextConfig,

    /// Index to track which load statement we're currently processing (for span resolution)
    current_load_index: RefCell<usize>,

    /// Diagnostics collected during load() calls in this context.
    load_diagnostics: RefCell<Vec<Diagnostic>>,
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
    ///
    /// Canonicalizes `package_resolutions` keys so that path lookups during
    /// evaluation match the canonicalized file paths used elsewhere.
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
        let child_config = EvalContextConfig {
            builtin_docs: self.config.builtin_docs.clone(),
            file_provider: self.config.file_provider.clone(),
            resolution: self.config.resolution.clone(),
            path_to_spec: self.config.path_to_spec.clone(),
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

        self.config.track_file(source_path);

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

        // Collect load diagnostics - this becomes our accumulator for all diagnostics
        let mut diagnostics = self.take_load_diagnostics();

        match eval_result {
            Ok(_) => {
                // Extract needed references before freezing (which moves self.module)
                let session_ref = self.session.clone();
                let config_ref = self.config.clone();

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
                let module_path = extra.module.path().clone();
                let is_root = module_path.segments.is_empty();

                if self.config.build_circuit || is_root {
                    session_ref.insert_module(module_path, extra.module.clone());
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
                            let child_config = base_config.child_for_pending(&pending.final_name);
                            diagnostics.extend(
                                session
                                    .create_context(child_config)
                                    .process_pending_child(pending.clone()),
                            );
                        }
                    }
                }

                let output = EvalOutput {
                    ast,
                    star_module: frozen_module,
                    sch_module: extra.module.clone(),
                    signature,
                    print_output,
                    config: config_ref.clone(),
                    module_tree_snapshot: session_ref.clone_module_tree(),
                    session: session_ref.clone(),
                };

                // Module's own diagnostics (from ContextValue)
                diagnostics.extend(extra.diagnostics().iter().cloned());

                // Emit warnings for nets renamed due to collisions or unnamed nets
                // Skip warnings for NotConnected nets (they're expected to have no name or duplicate names)
                for (_id, net_info) in extra.module.introduced_nets() {
                    // Skip all warnings for NotConnected nets
                    if net_info.net_type == "NotConnected" {
                        continue;
                    }

                    let location = net_info
                        .call_stack
                        .frames
                        .iter()
                        .rev()
                        .find_map(|f| f.location.as_ref());
                    let span = location.map(|loc| loc.resolve_span());
                    let path = location
                        .map(|loc| loc.file.filename().to_string())
                        .unwrap_or_else(|| extra.module.source_path().to_string());

                    if let Some(original) = &net_info.original_name {
                        if original == "NC" {
                            continue;
                        }
                        diagnostics.push(crate::Diagnostic {
                            path: path.clone(),
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
                    } else if net_info.auto_named {
                        diagnostics.push(crate::Diagnostic {
                            path,
                            span,
                            severity: EvalSeverity::Warning,
                            body: format!(
                                "Net had no explicit name; assigned '{}'",
                                net_info.final_name
                            ),
                            call_stack: Some(net_info.call_stack.clone()),
                            child: None,
                            source_error: None,
                            suppressed: false,
                        });
                    }
                }

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
    ) -> WithDiagnostics<ParseAndAnalyzeOutput> {
        self.session.clear_load_cache();
        self.session.clear_module_tree();
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
            // Replace dependency edges only when evaluation succeeds.
            // On failed evaluations, keep the previous dependency graph so
            // cross-file invalidation can still reach this module.
            self.session.clear_module_dependencies(&path);
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

        result.map(|output| ParseAndAnalyzeOutput {
            ast: output.ast.clone(),
            eval_output: output,
        })
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

        // Fast path: if we've already loaded (and frozen) this module once
        // within the current evaluation context, simply return the cached
        // instance so that callers share the same definitions.
        if let Some(frozen) = self.get_cached_module(&canonical_path) {
            return Ok(frozen);
        }

        let span = span.or_else(|| self.resolve_load_span(path));

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
        for diag in result.diagnostics.iter() {
            if matches!(diag.severity, EvalSeverity::Warning) {
                self.add_load_diagnostic(crate::Diagnostic {
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

    /// Process a pending child after the parent module has been frozen.
    /// Returns diagnostics collected during child evaluation.
    #[instrument(name = "instantiate", skip_all, fields(module = %pending.loader.name))]
    fn process_pending_child(mut self, pending: FrozenPendingChild) -> Vec<Diagnostic> {
        self.config.strict_io_config = true;
        self.config.build_circuit = true;
        self.config.source_path = Some(PathBuf::from(&pending.loader.source_path));

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
                suppressed: false,
            });
        }

        result
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
