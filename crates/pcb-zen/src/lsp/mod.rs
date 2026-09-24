pub mod signature;

use log::{debug, info};
use lsp_server::ResponseError;
use lsp_types::{
    Hover, HoverContents, MarkupContent, MarkupKind, ServerCapabilities, SignatureHelpOptions,
    WorkDoneProgressOptions, request::Request,
};
use pcb_sch::position::{Position, edit_position_comments, symbol_id_to_comment_key};
use pcb_starlark_lsp::server::{
    self, CompletionMeta, LspContext, LspEvalResult, LspUri, Response, StringLiteralResult,
};
use pcb_zen_core::config::find_workspace_root;
use pcb_zen_core::file_extensions::is_kicad_symbol_file;
use pcb_zen_core::lang::module::ModuleLoader;
use pcb_zen_core::lang::symbol::invalidate_symbol_library;
use pcb_zen_core::lang::type_info::ParameterInfo;
use pcb_zen_core::{
    DefaultFileProvider, EvalContext, EvalContextConfig, FileProvider, FileProviderError,
};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use serde_json::json;
use starlark::docs::DocModule;
use starlark::values::ValueLike;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use url::Url;

// JSON-RPC 2.0 error codes
const INVALID_PARAMS: i32 = -32602;
const INTERNAL_ERROR: i32 = -32603;
// LSP error code: the document was modified since the state the request was
// computed against. Clients should re-request with a fresh `baseHash`.
const CONTENT_MODIFIED: i32 = -32801;

/// Hex-encoded SHA-256 of the document text (exact UTF-8 bytes, no
/// normalization). Used to correlate position edits and evaluation results
/// with the document content they were computed from.
fn content_hash(content: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(content.as_bytes()))
}

/// Convert a byte offset into an LSP `Position` (line + UTF-16 character).
fn offset_to_lsp_position(content: &str, offset: usize) -> lsp_types::Position {
    let prefix = &content[..offset];
    let line = prefix.bytes().filter(|b| *b == b'\n').count() as u32;
    let line_start = prefix.rfind('\n').map(|i| i + 1).unwrap_or(0);
    let character = prefix[line_start..]
        .chars()
        .map(|c| c.len_utf16() as u32)
        .sum();
    lsp_types::Position { line, character }
}

/// Language-server state, containing only owned results of evaluations.
pub struct LspEvalContext {
    eager: bool,
    analysis: RwLock<HashMap<PathBuf, FileAnalysis>>,
    builtin_docs: HashMap<LspUri, String>,
    builtin_meta: HashMap<String, pcb_zen_core::SymbolInfo>,
    file_provider: Arc<dyn FileProvider>,
    offline: bool,
    resolution_cache: RwLock<HashMap<PathBuf, Arc<ResolutionResult>>>,
    workspace_root_cache: RwLock<HashMap<PathBuf, PathBuf>>,
    open_files: Arc<RwLock<HashMap<PathBuf, String>>>,
    netlist_subscriptions: Arc<RwLock<HashMap<PathBuf, HashMap<String, JsonValue>>>>,
    /// Per-file cache of the schematic extracted before dropping the evaluation.
    last_schematics: Arc<RwLock<HashMap<PathBuf, pcb_sch::Schematic>>>,
    custom_request_handler: Option<Arc<CustomRequestHandler>>,
    schematic_hydrator: Option<Arc<SchematicHydrator>>,
}

type CustomRequestHandler =
    dyn Fn(&str, &JsonValue) -> anyhow::Result<Option<JsonValue>> + Send + Sync;
type SchematicHydrator = dyn Fn(&Path, &mut pcb_sch::Schematic) + Send + Sync;

#[derive(Default)]
struct FileAnalysis {
    symbols: HashMap<String, pcb_zen_core::SymbolInfo>,
    dependencies: HashSet<PathBuf>,
}

struct OverlayFileProvider {
    base: Arc<dyn FileProvider>,
    open_files: Arc<RwLock<HashMap<PathBuf, String>>>,
}

impl OverlayFileProvider {
    /// Apply `f` to the open-file contents shadowing `path`, if any.
    fn overlay<T>(&self, path: &Path, f: impl FnOnce(&String) -> T) -> Option<T> {
        let open_files = self.open_files.read().unwrap();
        // Workspace discovery reads every manifest before anything is open;
        // an empty overlay must not cost a canonicalize per path.
        if open_files.is_empty() {
            return None;
        }
        open_files
            .get(path)
            .or_else(|| open_files.get(&self.base.canonicalize(path).ok()?))
            .map(f)
    }
}

impl FileProvider for OverlayFileProvider {
    fn read_file(&self, path: &Path) -> Result<String, FileProviderError> {
        self.overlay(path, String::clone)
            .map_or_else(|| self.base.read_file(path), Ok)
    }

    fn exists(&self, path: &Path) -> bool {
        self.overlay(path, |_| ()).is_some() || self.base.exists(path)
    }

    fn is_directory(&self, path: &Path) -> bool {
        self.base.is_directory(path)
    }

    fn is_symlink(&self, path: &Path) -> bool {
        self.base.is_symlink(path)
    }

    fn list_directory(&self, path: &Path) -> Result<Vec<PathBuf>, FileProviderError> {
        self.base.list_directory(path)
    }

    fn list_directory_entries(
        &self,
        path: &Path,
    ) -> Result<Vec<pcb_zen_core::DirEntry>, FileProviderError> {
        self.base.list_directory_entries(path)
    }

    fn canonicalize(&self, path: &Path) -> Result<PathBuf, FileProviderError> {
        self.base.canonicalize(path)
    }

    fn cache_dir(&self) -> std::path::PathBuf {
        self.base.cache_dir()
    }
}

/// Create a load resolver rooted at `workspace_root` with optional dependency resolution.
use pcb_zen_core::resolution::ResolutionResult;

impl Default for LspEvalContext {
    fn default() -> Self {
        // Build builtin documentation map
        let globals = starlark::environment::GlobalsBuilder::extended_by(&[
            starlark::environment::LibraryExtension::RecordType,
            starlark::environment::LibraryExtension::EnumType,
            starlark::environment::LibraryExtension::Typing,
            starlark::environment::LibraryExtension::StructType,
            starlark::environment::LibraryExtension::Print,
            starlark::environment::LibraryExtension::Debug,
            starlark::environment::LibraryExtension::Partial,
            starlark::environment::LibraryExtension::Breakpoint,
            starlark::environment::LibraryExtension::SetType,
            starlark::environment::LibraryExtension::Json,
        ])
        .build();

        let mut builtin_docs = HashMap::new();
        for (name, item) in globals.documentation().members {
            if let Ok(url) = Url::parse(&format!("starlark:/{name}.zen"))
                && let Ok(lsp_url) = LspUri::try_from(url)
            {
                builtin_docs.insert(lsp_url, item.render_as_code(&name));
            }
        }

        let base_provider = Arc::new(DefaultFileProvider::new());
        let open_files = Arc::new(RwLock::new(HashMap::new()));
        let file_provider: Arc<dyn FileProvider> = Arc::new(OverlayFileProvider {
            base: base_provider,
            open_files: open_files.clone(),
        });
        let builtin_meta = EvalContext::build_globals()
            .documentation()
            .members
            .into_iter()
            .map(|(name, item)| {
                let info = pcb_zen_core::SymbolInfo {
                    kind: pcb_zen_core::SymbolKind::Function,
                    parameters: None,
                    source_path: None,
                    type_name: "function".to_string(),
                    documentation: Some(item.render_as_code(&name)),
                };
                (name, info)
            })
            .collect();

        Self {
            eager: true,
            analysis: RwLock::new(HashMap::new()),
            builtin_docs,
            builtin_meta,
            file_provider,
            offline: false,
            resolution_cache: RwLock::new(HashMap::new()),
            workspace_root_cache: RwLock::new(HashMap::new()),
            open_files,
            netlist_subscriptions: Arc::new(RwLock::new(HashMap::new())),
            last_schematics: Arc::new(RwLock::new(HashMap::new())),
            custom_request_handler: None,
            schematic_hydrator: None,
        }
    }
}

impl LspEvalContext {
    pub fn set_offline(mut self, offline: bool) -> Self {
        self.offline = offline;
        self
    }

    fn diagnostic_target_uri(path: &str) -> Option<lsp_types::Uri> {
        if path.is_empty() {
            return None;
        }

        Url::from_file_path(path)
            .ok()
            .or_else(|| Url::parse(path).ok())
            .and_then(|url| server::url_to_uri(&url))
    }

    pub fn set_eager(mut self, eager: bool) -> Self {
        self.eager = eager;
        self
    }

    pub fn with_custom_request_handler<F>(mut self, handler: F) -> Self
    where
        F: Fn(&str, &JsonValue) -> anyhow::Result<Option<JsonValue>> + Send + Sync + 'static,
    {
        self.custom_request_handler = Some(Arc::new(handler));
        self
    }

    pub fn with_schematic_hydrator<F>(mut self, hydrator: F) -> Self
    where
        F: Fn(&Path, &mut pcb_sch::Schematic) + Send + Sync + 'static,
    {
        self.schematic_hydrator = Some(Arc::new(hydrator));
        self
    }

    fn hydrate_schematic(&self, source_path: &Path, schematic: &mut pcb_sch::Schematic) {
        if let Some(hydrator) = &self.schematic_hydrator {
            hydrator(source_path, schematic);
        }
    }

    fn open_file_contents(&self, path: &Path) -> Option<String> {
        if let Some(contents) = self.open_files.read().unwrap().get(path) {
            return Some(contents.clone());
        }

        if let Ok(canon) = self.file_provider.canonicalize(path)
            && let Some(contents) = self.open_files.read().unwrap().get(&canon)
        {
            return Some(contents.clone());
        }

        None
    }

    fn store_open_file(&self, path: &Path, contents: &str) {
        let mut open_files = self.open_files.write().unwrap();
        let owned = contents.to_string();
        open_files.insert(path.to_path_buf(), owned.clone());
        if let Ok(canon) = self.file_provider.canonicalize(path) {
            open_files.insert(canon, owned);
        }
    }

    fn remove_open_file(&self, path: &Path) {
        let mut open_files = self.open_files.write().unwrap();
        open_files.remove(path);
        if let Ok(canon) = self.file_provider.canonicalize(path) {
            open_files.remove(&canon);
        }
    }

    fn maybe_invalidate_symbol_library(&self, path: &Path) {
        if is_kicad_symbol_file(path.extension()) {
            invalidate_symbol_library(path, self.file_provider.as_ref());
        }
    }

    fn is_dependency_manifest(path: &Path) -> bool {
        matches!(
            path.file_name().and_then(|name| name.to_str()),
            Some("pcb.toml")
        )
    }

    fn maybe_invalidate_resolution_cache(&self, path: &Path) -> bool {
        if Self::is_dependency_manifest(path) {
            self.resolution_cache.write().unwrap().clear();
            self.workspace_root_cache.write().unwrap().clear();
            return true;
        }
        false
    }

    fn maybe_invalidate_on_saved_source(&self, path: &Path) {
        let is_source = matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("zen" | "star")
        );
        if is_source {
            self.resolution_cache.write().unwrap().clear();
        }
    }

    fn normalize_path(&self, path: &Path) -> PathBuf {
        self.file_provider
            .canonicalize(path)
            .unwrap_or_else(|_| path.to_path_buf())
    }

    fn set_netlist_subscription(&self, path: &Path, inputs: &HashMap<String, JsonValue>) {
        let key = self.normalize_path(path);
        self.netlist_subscriptions
            .write()
            .unwrap()
            .insert(key, inputs.clone());
    }

    fn get_netlist_inputs(&self, path: &Path) -> Option<HashMap<String, JsonValue>> {
        let key = self.normalize_path(path);
        self.netlist_subscriptions
            .read()
            .unwrap()
            .get(&key)
            .cloned()
    }

    fn set_last_schematic(&self, path: &Path, schematic: pcb_sch::Schematic) {
        let key = self.normalize_path(path);
        self.last_schematics.write().unwrap().insert(key, schematic);
    }

    fn get_last_schematic(&self, path: &Path) -> Option<pcb_sch::Schematic> {
        let key = self.normalize_path(path);
        self.last_schematics.read().unwrap().get(&key).cloned()
    }

    fn clear_last_schematic(&self, path: &Path) {
        let key = self.normalize_path(path);
        self.last_schematics.write().unwrap().remove(&key);
    }

    fn evaluate_with_inputs(
        &self,
        path_buf: &Path,
        inputs: &HashMap<String, JsonValue>,
    ) -> ZenerEvaluateResponse {
        let uri = LspUri::File(path_buf.to_path_buf());
        let maybe_contents = self.get_load_contents(&uri).ok().flatten();
        let evaluated_content_hash = maybe_contents.as_deref().map(content_hash);

        let config = self.config_for(path_buf);
        let mut ctx = EvalContext::from_caches_and_config(Default::default(), config);

        if let Some(contents) = maybe_contents {
            ctx = ctx.set_source_contents(contents);
        }

        if !inputs.is_empty() {
            let json_map = starlark::collections::SmallMap::from_iter(inputs.clone());
            ctx.set_json_inputs(json_map);
        }

        let eval_result = ctx.eval();

        let parameters = eval_result
            .output
            .as_ref()
            .map(|output| output.signature.clone());

        let schematic_result =
            eval_result.and_then(|output| output.to_schematic_with_diagnostics());

        self.evaluation_response(
            path_buf,
            parameters,
            schematic_result,
            evaluated_content_hash,
        )
    }

    fn evaluation_response(
        &self,
        path: &Path,
        parameters: Option<Vec<ParameterInfo>>,
        schematic_result: pcb_zen_core::WithDiagnostics<pcb_sch::Schematic>,
        content_hash: Option<String>,
    ) -> ZenerEvaluateResponse {
        let schematic_result =
            schematic_result.inspect_mut(|schematic| self.hydrate_schematic(path, schematic));
        ZenerEvaluateResponse {
            success: schematic_result.is_success(),
            parameters,
            schematic: schematic_result
                .output
                .as_ref()
                .filter(|_| schematic_result.is_success())
                .and_then(|schematic| serde_json::to_value(schematic).ok()),
            diagnostics: schematic_result
                .diagnostics
                .into_iter()
                .map(|d| diagnostic_to_info(&d))
                .collect(),
            content_hash,
        }
    }

    fn workspace_root_for(&self, file_path: &Path) -> PathBuf {
        let abs_path = self
            .file_provider
            .canonicalize(file_path)
            .unwrap_or_else(|_| file_path.to_path_buf());
        let start_dir = if self.file_provider.is_directory(&abs_path) {
            abs_path.clone()
        } else {
            abs_path.parent().unwrap_or(&abs_path).to_path_buf()
        };

        if let Some(root) = self.workspace_root_cache.read().unwrap().get(&start_dir) {
            return root.clone();
        }

        let workspace_root = find_workspace_root(self.file_provider.as_ref(), &abs_path)
            .expect("failed to find workspace root");
        let workspace_root = self
            .file_provider
            .canonicalize(&workspace_root)
            .unwrap_or(workspace_root);

        self.workspace_root_cache
            .write()
            .unwrap()
            .insert(start_dir, workspace_root.clone());

        workspace_root
    }

    /// Return the cached resolution for the workspace that owns `file_path`.
    fn resolution_for(&self, file_path: &Path) -> Arc<ResolutionResult> {
        let workspace_root = self.workspace_root_for(file_path);
        if let Some(cached) = self.resolution_cache.read().unwrap().get(&workspace_root) {
            return cached.clone();
        }

        let resolution = match crate::get_workspace_info(&self.file_provider, &workspace_root)
            .and_then(|ws| crate::resolve_workspace_dependencies(ws, &workspace_root, self.offline))
        {
            Ok(resolution) => Arc::new(resolution),
            Err(err) => {
                log::debug!(
                    "Failed to resolve dependencies for {}: {err:#}",
                    workspace_root.display()
                );
                return Arc::new(ResolutionResult::empty());
            }
        };
        self.resolution_cache
            .write()
            .unwrap()
            .insert(workspace_root, resolution.clone());
        resolution
    }

    /// Create a fresh EvalContextConfig rooted at the given file.
    ///
    /// All LSP operations share this path so navigation and evaluation use the
    /// same package-local resolution scope.
    fn config_for(&self, file_path: &Path) -> EvalContextConfig {
        EvalContextConfig::new(self.file_provider.clone(), self.resolution_for(file_path))
            .set_source_path(file_path.to_path_buf())
    }

    fn analyze_module(
        &self,
        path: &Path,
        module: &starlark::environment::FrozenModule,
        config: &EvalContextConfig,
    ) -> FileAnalysis {
        use pcb_zen_core::{SymbolInfo, SymbolKind};

        let mut analysis = FileAnalysis::default();
        for name in module.names() {
            let name = name.as_str();
            if let Ok(Some(owned)) = module.get_option(name) {
                let value = owned.value();
                let info = if let Some(loader) = value.downcast_ref::<ModuleLoader>() {
                    let mut target = PathBuf::from(&loader.source_path);
                    if target.is_relative()
                        && let Some(parent) = path.parent()
                    {
                        target = parent.join(target);
                    }
                    let target = self.normalize_path(&target);
                    analysis.dependencies.insert(target.clone());
                    SymbolInfo {
                        kind: SymbolKind::Module,
                        parameters: Some(loader.params.clone()),
                        source_path: Some(target),
                        type_name: "ModuleLoader".to_string(),
                        documentation: None,
                    }
                } else {
                    let typ = value.get_type();
                    SymbolInfo {
                        kind: match typ {
                            "NativeFunction" | "function" | "FrozenNativeFunction" => {
                                SymbolKind::Function
                            }
                            "ComponentFactory" | "ComponentType" => SymbolKind::Component,
                            "InterfaceFactory" => SymbolKind::Interface,
                            _ => SymbolKind::Variable,
                        },
                        parameters: None,
                        source_path: None,
                        type_name: typ.to_string(),
                        documentation: None,
                    }
                };
                analysis.symbols.insert(name.to_string(), info);
            }
        }

        // Prelude definitions are navigation targets even when not exported.
        for &(module_path, symbols) in config.prelude() {
            if let Ok(resolved) = config.resolve_path(module_path, path) {
                for &name in symbols {
                    let info = analysis.symbols.entry(name.to_string()).or_insert_with(|| {
                        self.builtin_meta.get(name).cloned().unwrap_or(SymbolInfo {
                            kind: SymbolKind::Variable,
                            parameters: None,
                            source_path: None,
                            type_name: name.to_string(),
                            documentation: None,
                        })
                    });
                    info.source_path.get_or_insert_with(|| resolved.clone());
                }
            }
        }
        analysis
    }

    /// Create LSP-specific diagnostic passes
    fn create_lsp_diagnostic_passes(
        &self,
        workspace_root: &Path,
    ) -> Vec<Box<dyn pcb_zen_core::DiagnosticsPass>> {
        vec![
            Box::new(pcb_zen_core::FilterHiddenPass),
            Box::new(pcb_zen_core::LspFilterPass::new(
                workspace_root.to_path_buf(),
            )),
            // Promote style diagnostics from Advice to Warning for LSP visibility
            Box::new(pcb_zen_core::StylePromotePass),
        ]
    }

    fn diagnostic_to_lsp(&self, diag: &pcb_zen_core::Diagnostic) -> lsp_types::Diagnostic {
        use lsp_types::{
            DiagnosticRelatedInformation, DiagnosticSeverity, Location, Position, Range,
        };

        let to_location = |path: &str, span: &starlark::codemap::ResolvedSpan| {
            Some(Location {
                uri: Self::diagnostic_target_uri(path)?,
                range: Range {
                    start: Position {
                        line: span.begin.line as u32,
                        character: span.begin.column as u32,
                    },
                    end: Position {
                        line: span.end.line as u32,
                        character: span.end.column as u32,
                    },
                },
            })
        };

        // Build relatedInformation from explicit related references and child diagnostics.
        let mut related: Vec<DiagnosticRelatedInformation> = Vec::new();

        // Convert primary span (if any).
        let (range, _add_related) = if let Some(span) = &diag.span {
            let range = Range {
                start: Position {
                    line: span.begin.line as u32,
                    character: span.begin.column as u32,
                },
                end: Position {
                    line: span.end.line as u32,
                    character: span.end.column as u32,
                },
            };
            (range, false)
        } else {
            // No primary span, use a dummy range
            let range = Range {
                start: Position {
                    line: 0,
                    character: 0,
                },
                end: Position {
                    line: 0,
                    character: 0,
                },
            };
            (range, true)
        };

        let mut current_opt: Option<&pcb_zen_core::Diagnostic> = Some(diag);
        while let Some(current) = current_opt {
            related.extend(current.related.iter().filter_map(|reference| {
                Some(DiagnosticRelatedInformation {
                    location: to_location(&reference.path, &reference.span)?,
                    message: reference.message.clone(),
                })
            }));

            if let Some(span) = &current.span
                && !current.path.is_empty()
                && !std::ptr::eq(current, diag)
                && let Some(location) = to_location(&current.path, span)
            {
                related.push(DiagnosticRelatedInformation {
                    location,
                    message: current.body.clone(),
                });
            }

            current_opt = current.child.as_deref();
        }

        let severity = match diag.severity {
            starlark::errors::EvalSeverity::Error => DiagnosticSeverity::ERROR,
            starlark::errors::EvalSeverity::Warning => DiagnosticSeverity::WARNING,
            starlark::errors::EvalSeverity::Advice => DiagnosticSeverity::HINT,
            starlark::errors::EvalSeverity::Disabled => DiagnosticSeverity::INFORMATION,
        };

        // Build a full-chain message: primary message followed by any child messages
        // prefixed with "Caused by:" on new lines for clarity in editors.
        let mut full_chain_lines: Vec<String> = Vec::new();
        {
            let mut current_opt: Option<&pcb_zen_core::Diagnostic> = Some(diag);
            let mut is_first = true;
            while let Some(current) = current_opt {
                if is_first {
                    full_chain_lines.push(current.body.clone());
                    is_first = false;
                } else {
                    full_chain_lines.push(format!("Caused by: {}", current.body));
                }
                current_opt = current.child.as_deref();
            }
        }
        let full_message = full_chain_lines.join("\n");

        lsp_types::Diagnostic {
            range,
            severity: Some(severity),
            code: None,
            code_description: None,
            source: Some("diode-star".to_string()),
            message: full_message,
            related_information: if related.is_empty() {
                None
            } else {
                Some(related)
            },
            tags: None,
            data: Self::diagnostic_target_uri(&diag.path).map(|uri| json!({ "targetUri": uri })),
        }
    }
}

impl LspContext for LspEvalContext {
    fn capabilities() -> ServerCapabilities {
        ServerCapabilities {
            signature_help_provider: Some(SignatureHelpOptions {
                trigger_characters: Some(vec!["(".to_string(), ",".to_string()]),
                retrigger_characters: Some(vec![",".to_string()]),
                work_done_progress_options: WorkDoneProgressOptions {
                    work_done_progress: None,
                },
            }),
            ..ServerCapabilities::default()
        }
    }

    fn did_change_file_contents(&self, uri: &LspUri, contents: &str) {
        if let LspUri::File(path) = uri {
            self.store_open_file(path, contents);
            self.maybe_invalidate_symbol_library(path);
            self.maybe_invalidate_resolution_cache(path);
        }
    }

    fn did_close_file(&self, uri: &LspUri) {
        if let LspUri::File(path) = uri {
            self.remove_open_file(path);
            let key = self.normalize_path(path);
            self.netlist_subscriptions.write().unwrap().remove(&key);
            self.clear_last_schematic(path);
            self.maybe_invalidate_symbol_library(path);
            self.maybe_invalidate_resolution_cache(path);
        }
    }

    fn did_save_file(&self, uri: &LspUri) {
        if let LspUri::File(path) = uri {
            self.maybe_invalidate_on_saved_source(path);
        }
    }

    fn watched_file_changed(&self, uri: &LspUri) -> bool {
        match uri {
            LspUri::File(path) => {
                let mut should_revalidate = false;

                if is_kicad_symbol_file(path.extension()) {
                    self.maybe_invalidate_symbol_library(path);
                    should_revalidate = true;
                }

                if self.maybe_invalidate_resolution_cache(path) {
                    should_revalidate = true;
                }

                should_revalidate
            }
            _ => false,
        }
    }

    fn on_save_diagnostics(&self, uri: &LspUri) -> Vec<lsp_types::Diagnostic> {
        let path = match uri {
            LspUri::File(p) => p,
            _ => return vec![],
        };

        // Use the owned schematic cached during parse_file_with_contents.
        let Some(schematic) = self.get_last_schematic(path) else {
            return vec![];
        };

        // Only run simulation if the schematic has sim setup
        let Some(root) = schematic.root() else {
            return vec![];
        };
        if !root.attributes.contains_key(pcb_zen_core::attrs::SIM_SETUP) {
            return vec![];
        }

        // Read the call-site span stored by set_sim_setup() during eval
        let sim_setup_range = root
            .attributes
            .get(pcb_zen_core::attrs::SIM_SETUP_SPAN)
            .and_then(|v| v.string())
            .and_then(parse_sim_setup_span)
            .unwrap_or_default();

        // Generate .cir content
        let mut buf = Vec::new();
        if let Err(e) = pcb_sim::gen_sim(&schematic, &mut buf) {
            return vec![lsp_types::Diagnostic {
                range: sim_setup_range,
                severity: Some(lsp_types::DiagnosticSeverity::ERROR),
                source: Some("ngspice".to_string()),
                message: format!("{e}"),
                ..Default::default()
            }];
        }

        // Check if ngspice is installed
        if pcb_sim::check_ngspice_installed().is_err() {
            return vec![lsp_types::Diagnostic {
                range: sim_setup_range,
                severity: Some(lsp_types::DiagnosticSeverity::INFORMATION),
                source: Some("ngspice".to_string()),
                message: "ngspice is not installed. Install it to enable simulation diagnostics."
                    .to_string(),
                ..Default::default()
            }];
        }

        // Write .cir next to the zen file so ngspice resolves relative paths correctly
        let zen_dir = path.parent().unwrap_or(std::path::Path::new("."));
        let mut tmp = match tempfile::Builder::new().suffix(".cir").tempfile_in(zen_dir) {
            Ok(t) => t,
            Err(_) => return vec![],
        };
        if std::io::Write::write_all(&mut tmp, &buf).is_err() {
            return vec![];
        }

        match pcb_sim::run_ngspice_captured(tmp.path(), zen_dir) {
            Ok(result) if !result.success => {
                vec![lsp_types::Diagnostic {
                    range: sim_setup_range,
                    severity: Some(lsp_types::DiagnosticSeverity::ERROR),
                    source: Some("ngspice".to_string()),
                    message: result.output,
                    ..Default::default()
                }]
            }
            Err(e) => {
                vec![lsp_types::Diagnostic {
                    range: sim_setup_range,
                    severity: Some(lsp_types::DiagnosticSeverity::ERROR),
                    source: Some("ngspice".to_string()),
                    message: format!("Simulation failed: {e}"),
                    ..Default::default()
                }]
            }
            _ => vec![],
        }
    }

    fn parse_file_with_contents(&self, uri: &LspUri, content: String) -> LspEvalResult<'_> {
        match uri {
            LspUri::File(path) => {
                let workspace_root = self.workspace_root_for(path);
                let config = self.config_for(path);
                let evaluated_content_hash = content_hash(&content);

                let result =
                    EvalContext::from_caches_and_config(Default::default(), config.clone())
                        .set_source_contents(content)
                        .eval();

                if let Some(output) = &result.output {
                    let analysis = self.analyze_module(path, output.star_module(), &config);
                    self.analysis
                        .write()
                        .unwrap()
                        .insert(path.clone(), analysis);
                } else {
                    // Keep dependency reachability through failed evaluations.
                    if let Some(analysis) = self.analysis.write().unwrap().get_mut(path) {
                        analysis.symbols.clear();
                    }
                }

                // Apply LSP-specific diagnostic passes
                let passes = self.create_lsp_diagnostic_passes(&workspace_root);
                let mut lsp_diagnostics = result.diagnostics.clone();
                lsp_diagnostics.apply_passes(&passes);
                let diagnostics = lsp_diagnostics
                    .iter()
                    .map(|d| self.diagnostic_to_lsp(d))
                    .collect();
                let ast = result.output.as_ref().map(|output| output.ast.clone());
                let parameters = result
                    .output
                    .as_ref()
                    .map(|output| output.signature.clone());

                let converted = result
                    .output
                    .map(|output| output.to_schematic_with_diagnostics())
                    .unwrap_or_default();
                let netlist_update = self.get_netlist_inputs(path).map(|inputs| {
                    let path = path.clone();
                    let uri = uri.clone();
                    let mut schematic_result = converted.clone();
                    schematic_result.diagnostics.extend(result.diagnostics);
                    Box::new(move || {
                        let response = if inputs.is_empty() {
                            self.evaluation_response(
                                &path,
                                parameters,
                                schematic_result,
                                Some(evaluated_content_hash),
                            )
                        } else {
                            self.evaluate_with_inputs(&path, &inputs)
                        };
                        serde_json::to_value(ZenerNetlistUpdateParams {
                            uri,
                            result: response,
                            inputs: (!inputs.is_empty()).then_some(inputs),
                        })
                        .expect("netlist update contains JSON-serializable data")
                    }) as Box<dyn FnOnce() -> JsonValue>
                });
                if let Ok(schematic) = converted.output_result() {
                    self.set_last_schematic(path, schematic);
                } else {
                    self.clear_last_schematic(path);
                }

                LspEvalResult {
                    diagnostics,
                    ast: ast.map(Arc::unwrap_or_clone),
                    netlist_update,
                }
            }
            _ => {
                // For non-file URLs, return empty result
                LspEvalResult {
                    diagnostics: vec![],
                    ast: None,
                    netlist_update: None,
                }
            }
        }
    }

    fn resolve_load(
        &self,
        path: &str,
        current_file: &LspUri,
        _workspace_root: Option<&Path>,
    ) -> Result<LspUri, String> {
        match current_file {
            LspUri::File(current_path) => {
                let config = self.config_for(current_path);
                let resolved = config
                    .resolve_path(path, current_path)
                    .map_err(|error| format!("{error:#}"))?;
                Ok(LspUri::File(resolved))
            }
            _ => Err("Cannot resolve load from non-file URI".to_owned()),
        }
    }

    fn render_as_load(
        &self,
        target: &LspUri,
        current_file: &LspUri,
        _workspace_root: Option<&Path>,
    ) -> Result<String, String> {
        match (target, current_file) {
            (LspUri::File(target_path), LspUri::File(current_path)) => {
                // Simple implementation: if in same directory, use relative path
                if let (Some(target_parent), Some(current_parent)) =
                    (target_path.parent(), current_path.parent())
                    && target_parent == current_parent
                    && let Some(file_name) = target_path.file_name()
                {
                    return Ok(format!("./{}", file_name.to_string_lossy()));
                }
                // Otherwise use absolute path
                Ok(target_path.to_string_lossy().to_string())
            }
            _ => Err("Can only render file URIs".to_owned()),
        }
    }

    fn resolve_string_literal(
        &self,
        literal: &str,
        current_file: &LspUri,
        _workspace_root: Option<&Path>,
    ) -> Result<Option<StringLiteralResult>, String> {
        match current_file {
            LspUri::File(current_path) => {
                // Try to resolve as a file path
                let config = self.config_for(current_path);
                if let Ok(resolved) = config.resolve_path(literal, current_path)
                    && resolved.exists()
                {
                    return Ok(Some(StringLiteralResult {
                        uri: LspUri::File(resolved),
                        location_finder: None,
                    }));
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    fn get_load_contents(&self, uri: &LspUri) -> Result<Option<String>, String> {
        match uri {
            LspUri::File(path) => {
                if let Some(contents) = self.open_file_contents(path) {
                    return Ok(Some(contents));
                }

                if self.file_provider.exists(path) {
                    self.file_provider
                        .read_file(path)
                        .map(Some)
                        .map_err(|error| format!("{error:#}"))
                } else {
                    Ok(None)
                }
            }
            LspUri::Starlark(_) => {
                // For starlark: URLs, check if we have builtin documentation
                Ok(self.builtin_docs.get(uri).cloned())
            }
            _ => Ok(None),
        }
    }

    fn get_environment(&self, _uri: &LspUri) -> DocModule {
        // Return empty doc module for now
        DocModule::default()
    }

    fn get_uri_for_global_symbol(
        &self,
        current_file: &LspUri,
        symbol: &str,
    ) -> Result<Option<LspUri>, String> {
        match current_file {
            LspUri::File(path) => {
                if let Some(target_path) = self
                    .analysis
                    .read()
                    .unwrap()
                    .get(path)
                    .and_then(|analysis| analysis.symbols.get(symbol))
                    .and_then(|info| info.source_path.clone())
                {
                    Ok(Some(LspUri::File(target_path)))
                } else {
                    // Check if it's a builtin
                    if let Ok(parsed_url) = Url::parse(&format!("starlark:/{symbol}.zen"))
                        && let Ok(lsp_url) = LspUri::try_from(parsed_url)
                        && self.builtin_docs.contains_key(&lsp_url)
                    {
                        return Ok(Some(lsp_url));
                    }
                    Ok(None)
                }
            }
            _ => Ok(None),
        }
    }

    fn get_completion_meta(&self, current_file: &LspUri, symbol: &str) -> Option<CompletionMeta> {
        match current_file {
            LspUri::File(path) => {
                // First check for symbol info from the file
                if let Some(info) = self
                    .analysis
                    .read()
                    .unwrap()
                    .get(path)
                    .and_then(|analysis| analysis.symbols.get(symbol))
                    .or_else(|| self.builtin_meta.get(symbol))
                {
                    return Some(CompletionMeta {
                        kind: None, // We could map SymbolKind to CompletionItemKind here
                        detail: Some(info.type_name.clone()),
                        documentation: info.documentation.clone(),
                    });
                }

                // Fallback to builtin docs
                if let Ok(parsed_url) = Url::parse(&format!("starlark:/{symbol}.zen"))
                    && let Ok(lsp_url) = LspUri::try_from(parsed_url)
                    && let Some(doc) = self.builtin_docs.get(&lsp_url)
                {
                    let first_line = doc.lines().next().unwrap_or("").to_string();
                    return Some(CompletionMeta {
                        kind: Some(lsp_types::CompletionItemKind::FUNCTION),
                        detail: Some(first_line),
                        documentation: Some(doc.clone()),
                    });
                }
                None
            }
            _ => None,
        }
    }

    fn warm_workspace(&self, workspace_roots: &[PathBuf]) {
        // Workspace discovery and resolution dominate the first evaluation. A
        // folder without a manifest is not a workspace; do not walk it. A
        // manifest that does not parse makes `workspace_root_for` panic; skip
        // it so the warm-up cannot take the server down at start-up.
        for root in workspace_roots {
            if self.file_provider.exists(&root.join("pcb.toml"))
                && find_workspace_root(self.file_provider.as_ref(), root).is_ok()
            {
                self.resolution_for(root);
            }
        }
    }

    fn is_eager(&self) -> bool {
        self.eager
    }

    fn workspace_files(
        &self,
        workspace_roots: &[std::path::PathBuf],
    ) -> Result<Vec<std::path::PathBuf>, String> {
        let mut files = Vec::new();
        let mut pending = workspace_roots.to_vec();
        while let Some(path) = pending.pop() {
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with('.'))
            {
                continue;
            }
            if self.file_provider.is_directory(&path) {
                if let Ok(entries) = self.file_provider.list_directory(&path) {
                    pending.extend(
                        entries
                            .into_iter()
                            .filter(|entry| !self.file_provider.is_symlink(entry)),
                    );
                }
            } else if self.file_provider.exists(&path)
                && matches!(
                    path.extension().and_then(|ext| ext.to_str()),
                    Some("zen" | "star")
                )
            {
                files.push(path);
            }
        }
        Ok(files)
    }

    fn has_module_dependency(&self, from: &Path, to: &Path) -> bool {
        self.analysis
            .read()
            .unwrap()
            .get(from)
            .is_some_and(|analysis| analysis.dependencies.contains(to))
    }

    fn get_custom_hover_for_load(
        &self,
        load_path: &str,
        _symbol_name: &str,
        current_file: &LspUri,
        _workspace_root: Option<&Path>,
    ) -> Result<Option<Hover>, String> {
        // Check if the load path is a directory
        match current_file {
            LspUri::File(current_path) => {
                let config = self.config_for(current_path);
                if let Ok(resolved) = config.resolve_path(load_path, current_path)
                    && resolved.is_dir()
                {
                    return Ok(Some(Hover {
                        contents: HoverContents::Markup(MarkupContent {
                            kind: MarkupKind::Markdown,
                            value: format!("Directory: `{}`", resolved.display()),
                        }),
                        range: None,
                    }));
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    fn handle_custom_request(
        &self,
        req: &server::Request,
        _initialize_params: &lsp_types::InitializeParams,
    ) -> Option<Response> {
        debug!("Received custom request: method={}", req.method);
        // Handle signature help requests
        if req.method == "textDocument/signatureHelp" {
            match serde_json::from_value::<lsp_types::SignatureHelpParams>(req.params.clone()) {
                Ok(params) => {
                    let uri: LspUri = match params
                        .text_document_position_params
                        .text_document
                        .uri
                        .try_into()
                    {
                        Ok(u) => u,
                        Err(e) => {
                            return Some(Response::new_err(
                                req.id.clone(),
                                0,
                                format!("Invalid URI: {e}"),
                            ));
                        }
                    };

                    // Fetch the contents of the file
                    let contents = match self.get_load_contents(&uri) {
                        Ok(Some(c)) => c,
                        _ => String::new(),
                    };

                    // Parse AST
                    let ast = match starlark::syntax::AstModule::parse(
                        uri.path().to_string_lossy().as_ref(),
                        contents,
                        &starlark::syntax::Dialect::Extended,
                    ) {
                        Ok(a) => a,
                        Err(_) => {
                            let empty = lsp_types::SignatureHelp {
                                signatures: vec![],
                                active_signature: None,
                                active_parameter: None,
                            };
                            return Some(Response::new_ok(req.id.clone(), empty));
                        }
                    };

                    // Compute signature help
                    let position = params.text_document_position_params.position;
                    let sig_help = crate::lsp::signature::signature_help(
                        &ast,
                        position.line,
                        position.character,
                        self,
                        &uri,
                    );

                    return Some(Response::new_ok(req.id.clone(), sig_help));
                }
                Err(e) => {
                    return Some(Response::new_err(
                        req.id.clone(),
                        0,
                        format!("Failed to parse params: {e}"),
                    ));
                }
            }
        }

        // Handle viewer/getState requests
        if req.method == ViewerGetStateRequest::METHOD {
            match serde_json::from_value::<ViewerGetStateParams>(req.params.clone()) {
                Ok(params) => {
                    let state_json: Option<JsonValue> = match &params.uri {
                        LspUri::File(path_buf) => {
                            // Try the cached schematic first (populated during
                            // parse_file_with_contents) so we can return the
                            // schematic without a redundant full evaluation.
                            if let Some(mut cached) = self.get_last_schematic(path_buf) {
                                self.hydrate_schematic(path_buf, &mut cached);
                                serde_json::to_value(&cached).ok()
                            } else {
                                // Fallback: evaluate in a fresh context.
                                let maybe_contents =
                                    self.get_load_contents(&params.uri).ok().flatten();
                                let config = self.config_for(path_buf);
                                let ctx =
                                    EvalContext::from_caches_and_config(Default::default(), config);

                                let eval_result = if let Some(contents) = maybe_contents {
                                    ctx.set_source_contents(contents).eval()
                                } else {
                                    ctx.eval()
                                };

                                eval_result
                                    .output
                                    .and_then(|fmv| fmv.to_schematic().ok())
                                    .map(|mut schematic| {
                                        self.hydrate_schematic(path_buf, &mut schematic);
                                        schematic
                                    })
                                    .and_then(|schematic| serde_json::to_value(&schematic).ok())
                            }
                        }
                        _ => None,
                    };

                    let response_payload = ViewerGetStateResponse { state: state_json };
                    return Some(Response::new_ok(req.id.clone(), response_payload));
                }
                Err(e) => {
                    return Some(Response::new_err(
                        req.id.clone(),
                        0,
                        format!("Failed to parse params: {e}"),
                    ));
                }
            }
        }

        // Handle zener/evaluate requests
        if req.method == ZenerEvaluateRequest::METHOD {
            match serde_json::from_value::<ZenerEvaluateParams>(req.params.clone()) {
                Ok(params) => {
                    let result = self.evaluate_module(params);
                    match result {
                        Ok(response) => {
                            return Some(Response::new_ok(req.id.clone(), response));
                        }
                        Err(e) => {
                            return Some(Response::new_err(
                                req.id.clone(),
                                0,
                                format!("Evaluation failed: {e}"),
                            ));
                        }
                    }
                }
                Err(e) => {
                    return Some(Response::new_err(
                        req.id.clone(),
                        0,
                        format!("Failed to parse params: {e}"),
                    ));
                }
            }
        }

        // Handle pcb/savePositions requests
        if req.method == "pcb/savePositions" {
            info!("Received pcb/savePositions request");
            match serde_json::from_value::<PcbSavePositionsParams>(req.params.clone()) {
                Ok(params) => {
                    let file_path = &params.file_path;
                    info!(
                        "Saving {} position updates and {} deletions to file: {}",
                        params.symbol_positions.len(),
                        params.deleted_symbol_ids.len(),
                        file_path
                    );

                    // Convert symbol positions to comment format
                    let mut flat_positions = BTreeMap::new();
                    for (symbol_id, position) in params.symbol_positions {
                        let Some(comment_name) = symbol_id_to_comment_key(&symbol_id) else {
                            return Some(Response::new_err(
                                req.id.clone(),
                                INVALID_PARAMS,
                                format!("Invalid symbol ID format: {symbol_id}"),
                            ));
                        };
                        flat_positions.insert(comment_name, position);
                    }

                    let mut deleted_comment_names = Vec::new();
                    for symbol_id in params.deleted_symbol_ids {
                        let Some(comment_name) = symbol_id_to_comment_key(&symbol_id) else {
                            return Some(Response::new_err(
                                req.id.clone(),
                                INVALID_PARAMS,
                                format!("Invalid symbol ID format: {symbol_id}"),
                            ));
                        };
                        deleted_comment_names.push(comment_name);
                    }

                    let result =
                        self.position_block_edit(file_path, &params.base_hash, |content| {
                            edit_position_comments(content, &flat_positions, &deleted_comment_names)
                        });
                    return Some(position_edit_result_to_response(req.id.clone(), result));
                }
                Err(e) => {
                    return Some(Response::new_err(
                        req.id.clone(),
                        INVALID_PARAMS,
                        format!("Invalid pcb/savePositions params: {e}"),
                    ));
                }
            }
        }

        if let Some(handler) = &self.custom_request_handler {
            match handler(&req.method, &req.params) {
                Ok(Some(result)) => {
                    return Some(Response::new_ok(req.id.clone(), result));
                }
                Ok(None) => {}
                Err(err) => {
                    return Some(Response::new_err(
                        req.id.clone(),
                        INTERNAL_ERROR,
                        err.to_string(),
                    ));
                }
            }
        }

        None
    }

    fn handle_custom_notification(
        &self,
        _notification: &lsp_server::Notification,
        _initialize_params: &lsp_types::InitializeParams,
    ) {
    }
}

impl LspEvalContext {
    /// Compute a position-block edit against the current authoritative content
    /// (open-file overlay, falling back to disk) without mutating either.
    /// `rewrite` maps the content to the position block's byte offset and its
    /// replacement text.
    fn position_block_edit(
        &self,
        file_path: &str,
        base_hash: &str,
        rewrite: impl FnOnce(&str) -> (usize, String),
    ) -> Result<PcbPositionEditResponse, ResponseError> {
        let uri = LspUri::File(PathBuf::from(file_path));
        let content = match self.get_load_contents(&uri) {
            Ok(Some(content)) => content,
            Ok(None) => {
                return Err(ResponseError {
                    code: INVALID_PARAMS,
                    message: format!("File not found: {file_path}"),
                    data: None,
                });
            }
            Err(e) => {
                return Err(ResponseError {
                    code: INTERNAL_ERROR,
                    message: format!("Failed to read file: {e}"),
                    data: None,
                });
            }
        };

        let actual_hash = content_hash(&content);
        if actual_hash != base_hash {
            return Err(ResponseError {
                code: CONTENT_MODIFIED,
                message: format!(
                    "Document content changed (expected {base_hash}, found {actual_hash})"
                ),
                data: None,
            });
        }

        let (block_start, new_block) = rewrite(&content);
        let result_hash = content_hash(&format!("{}{}", &content[..block_start], new_block));

        Ok(PcbPositionEditResponse {
            edit: lsp_types::TextEdit {
                range: lsp_types::Range {
                    start: offset_to_lsp_position(&content, block_start),
                    end: offset_to_lsp_position(&content, content.len()),
                },
                new_text: new_block,
            },
            base_hash: actual_hash,
            result_hash,
        })
    }

    fn evaluate_module(
        &self,
        params: ZenerEvaluateParams,
    ) -> anyhow::Result<ZenerEvaluateResponse> {
        let path_buf = match &params.uri {
            LspUri::File(path) => path,
            _ => return Err(anyhow::anyhow!("Only file URIs are supported")),
        };

        let response = self.evaluate_with_inputs(path_buf, &params.inputs);
        self.set_netlist_subscription(path_buf, &params.inputs);
        Ok(response)
    }
}

fn position_edit_result_to_response(
    id: lsp_server::RequestId,
    result: Result<PcbPositionEditResponse, ResponseError>,
) -> Response {
    match result {
        Ok(response) => Response::new_ok(id, response),
        Err(error) => Response {
            id,
            response_result: Err(error),
        },
    }
}

/// Parse a "begin_line:begin_col:end_line:end_col" span string into an LSP Range.
fn parse_sim_setup_span(s: &str) -> Option<lsp_types::Range> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 4 {
        return None;
    }
    Some(lsp_types::Range::new(
        lsp_types::Position::new(parts[0].parse().ok()?, parts[1].parse().ok()?),
        lsp_types::Position::new(parts[2].parse().ok()?, parts[3].parse().ok()?),
    ))
}

/// Convert a Diagnostic to DiagnosticInfo
fn diagnostic_to_info(diag: &pcb_zen_core::Diagnostic) -> DiagnosticInfo {
    let level = match diag.severity {
        starlark::errors::EvalSeverity::Error => "error",
        starlark::errors::EvalSeverity::Warning => "warning",
        starlark::errors::EvalSeverity::Advice => "info",
        starlark::errors::EvalSeverity::Disabled => "info",
    }
    .to_string();

    DiagnosticInfo {
        level,
        message: diag.body.clone(),
        file: Some(diag.path.clone()),
        line: diag.span.as_ref().map(|s| s.begin.line as u32),
        child: diag.child.as_ref().map(|c| Box::new(diagnostic_to_info(c))),
    }
}

// Custom LSP request (legacy-compatible) to fetch the viewer state – now used to return the netlist.
struct ViewerGetStateRequest;
impl lsp_types::request::Request for ViewerGetStateRequest {
    type Params = ViewerGetStateParams;
    type Result = ViewerGetStateResponse;
    const METHOD: &'static str = "viewer/getState";
}

#[derive(Debug, PartialEq, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ViewerGetStateParams {
    uri: LspUri,
}

#[derive(Debug, PartialEq, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ViewerGetStateResponse {
    state: Option<JsonValue>,
}

// Custom LSP request for zener/evaluate - evaluates a module with given inputs and returns a netlist
struct ZenerEvaluateRequest;
impl lsp_types::request::Request for ZenerEvaluateRequest {
    type Params = ZenerEvaluateParams;
    type Result = ZenerEvaluateResponse;
    const METHOD: &'static str = "zener/evaluate";
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ZenerEvaluateParams {
    uri: LspUri,
    inputs: HashMap<String, JsonValue>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ZenerEvaluateResponse {
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    parameters: Option<Vec<ParameterInfo>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    schematic: Option<JsonValue>,
    diagnostics: Vec<DiagnosticInfo>,
    /// Content hash of the document this result was evaluated from. Clients
    /// apply a result only when this matches their current buffer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    content_hash: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ZenerNetlistUpdateParams {
    uri: LspUri,
    result: ZenerEvaluateResponse,
    #[serde(skip_serializing_if = "Option::is_none")]
    inputs: Option<HashMap<String, JsonValue>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct DiagnosticInfo {
    level: String,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    child: Option<Box<DiagnosticInfo>>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct PcbSavePositionsParams {
    file_path: String,
    symbol_positions: BTreeMap<String, Position>,
    /// Symbol IDs whose position comments should be deleted in the same edit.
    deleted_symbol_ids: Vec<String>,
    /// Content hash of the document the positions were computed against.
    base_hash: String,
}

/// Response to `pcb/savePositions`: the edit is applied by the client to its
/// own buffer (and mirrored to disk by the client); the server touches neither.
/// `resultHash` is the content hash after applying the edit, which the client
/// can verify and use to correlate the subsequent evaluation.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct PcbPositionEditResponse {
    edit: lsp_types::TextEdit,
    base_hash: String,
    result_hash: String,
}

#[cfg(test)]
mod tests {
    use super::{LspContext, LspEvalContext, LspUri};
    use lsp_server::Request;
    use lsp_server::RequestId;
    use pcb_zen_core::DiagnosticReference;
    use serde_json::json;
    use starlark::analysis::EvalSeverity;
    use starlark::codemap::ResolvedSpan;
    use std::collections::HashMap;
    use std::fs;
    use std::path::Path;

    #[test]
    fn subscribed_validation_reuses_default_evaluation_and_preserves_custom_inputs()
    -> anyhow::Result<()> {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };

        let dir = tempfile::tempdir()?;
        let root = dir.path().canonicalize()?;
        fs::write(
            root.join("pcb.toml"),
            "[workspace]\npcb-version = \"0.4\"\n",
        )?;
        let path = root.join("board.zen");
        let uri = LspUri::File(path.clone());
        let source = "label = config(str, default = \"DEFAULT\")\nsignal = Net(label)\n";
        fs::write(&path, source)?;
        let hydrations = Arc::new(AtomicUsize::new(0));
        let counter = hydrations.clone();
        let ctx = LspEvalContext::default()
            .set_offline(true)
            .with_schematic_hydrator(move |_, _| {
                counter.fetch_add(1, Ordering::Relaxed);
            });
        ctx.did_change_file_contents(&uri, source);
        ctx.set_netlist_subscription(&path, &HashMap::new());

        let mut previous_id = None;
        for i in 0..2 {
            let result = ctx.parse_file_with_contents(&uri, source.to_string());
            assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
            assert_eq!(hydrations.load(Ordering::Relaxed), i);
            let update: super::ZenerNetlistUpdateParams =
                serde_json::from_value(result.netlist_update.unwrap()())?;
            assert_eq!(hydrations.load(Ordering::Relaxed), i + 1);
            assert!(update.result.success);
            assert!(update.inputs.is_none());
            assert_eq!(
                update.result.content_hash,
                Some(super::content_hash(source))
            );
            assert_eq!(update.result.parameters.unwrap()[0].name, "label");
            let schematic: pcb_sch::Schematic =
                serde_json::from_value(update.result.schematic.unwrap())?;
            let id = schematic.nets["DEFAULT"].id;
            // Net IDs are unique per evaluation: matching IDs prove both consumers
            // use the same evaluation, while a new edit must produce a fresh one.
            assert_eq!(
                ctx.get_last_schematic(&path).unwrap().nets["DEFAULT"].id,
                id
            );
            assert_ne!(previous_id, Some(id));
            previous_id = Some(id);
        }

        let inputs = HashMap::from([("label".to_string(), json!("CUSTOM"))]);
        ctx.set_netlist_subscription(&path, &inputs);
        // Save-only validation discards the update without hydrating it.
        drop(ctx.parse_file_with_contents(&uri, source.to_string()));
        assert_eq!(hydrations.load(Ordering::Relaxed), 2);
        let result = ctx.parse_file_with_contents(&uri, source.to_string());
        let update: super::ZenerNetlistUpdateParams =
            serde_json::from_value(result.netlist_update.unwrap()())?;
        assert!(result.diagnostics.is_empty());
        assert_eq!(update.inputs, Some(inputs));
        assert!(update.result.success);
        let schematic: pcb_sch::Schematic =
            serde_json::from_value(update.result.schematic.unwrap())?;
        assert!(schematic.nets.contains_key("CUSTOM"));
        assert!(!schematic.nets.contains_key("DEFAULT"));
        assert!(
            ctx.get_last_schematic(&path)
                .unwrap()
                .nets
                .contains_key("DEFAULT")
        );

        ctx.set_netlist_subscription(&path, &HashMap::new());
        let broken = "fail(\"broken edit\")\n";
        ctx.did_change_file_contents(&uri, broken);
        let result = ctx.parse_file_with_contents(&uri, broken.to_string());
        let update: super::ZenerNetlistUpdateParams =
            serde_json::from_value(result.netlist_update.unwrap()())?;
        assert!(!result.diagnostics.is_empty());
        assert!(!update.result.success);
        assert!(update.result.schematic.is_none());
        assert!(!update.result.diagnostics.is_empty());
        assert_eq!(
            update.result.content_hash,
            Some(super::content_hash(broken))
        );
        assert!(ctx.get_last_schematic(&path).is_none());

        ctx.did_close_file(&uri);
        assert!(
            ctx.parse_file_with_contents(&uri, source.to_string())
                .netlist_update
                .is_none()
        );
        Ok(())
    }

    #[test]
    #[cfg(unix)]
    fn workspace_discovery_follows_only_root_symlinks() -> anyhow::Result<()> {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir()?;
        let root = dir.path().join("workspace");
        let linked = dir.path().join("linked");
        let outside = dir.path().join("outside");
        fs::create_dir_all(root.join("sub"))?;
        fs::create_dir_all(root.join(".hidden"))?;
        fs::create_dir(&outside)?;
        fs::write(root.join("board.zen"), "")?;
        fs::write(root.join("sub/part.star"), "")?;
        fs::write(root.join(".hidden/hidden.zen"), "")?;
        fs::write(outside.join("excluded.zen"), "")?;
        symlink(&root, &linked)?;
        symlink(&outside, root.join("external"))?;
        symlink(outside.join("excluded.zen"), root.join("alias.zen"))?;

        let ctx = LspEvalContext::default();
        let mut files = ctx
            .workspace_files(std::slice::from_ref(&linked))
            .map_err(anyhow::Error::msg)?;
        files.sort();
        assert_eq!(
            files,
            vec![linked.join("board.zen"), linked.join("sub/part.star")]
        );
        Ok(())
    }

    #[test]
    fn analysis_omits_disabled_prelude_navigation() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path().canonicalize()?;
        fs::write(
            root.join("pcb.toml"),
            "[workspace]\npcb-version = \"0.4\"\n",
        )?;
        let board = root.join("board.zen");
        let source = "answer = 42\n";
        fs::write(&board, source)?;
        let ctx = LspEvalContext::default().set_offline(true);
        let stdlib = ctx
            .resolution_for(&board)
            .workspace_info
            .workspace_stdlib_dir();
        let stdlib_file = stdlib.join("navigation_probe.zen");
        fs::write(&stdlib_file, source)?;

        for (path, has_prelude) in [(board, true), (stdlib_file, false)] {
            let uri = LspUri::File(path.clone());
            let result = ctx.parse_file_with_contents(&uri, source.to_string());
            assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
            assert!(result.ast.is_some());
            assert_eq!(
                ctx.analysis.read().unwrap()[&path]
                    .symbols
                    .contains_key("Power"),
                has_prelude
            );
            assert_eq!(
                ctx.get_uri_for_global_symbol(&uri, "Power")
                    .unwrap()
                    .is_some(),
                has_prelude
            );
        }
        Ok(())
    }

    #[test]
    fn analysis_retains_dependencies_on_failure_and_replaces_them_on_success() -> anyhow::Result<()>
    {
        let dir = tempfile::tempdir()?;
        let root = dir.path().canonicalize()?;
        fs::write(
            root.join("pcb.toml"),
            "[workspace]\npcb-version = \"0.4\"\n",
        )?;
        let path = root.join("main.zen");
        let child = root.join("child.zen");
        fs::write(&child, "setting = config(int, default = 7)\n")?;
        let source = "Child = Module(\"./child.zen\")\n";
        fs::write(&path, source)?;
        let uri = LspUri::File(path.clone());
        let ctx = LspEvalContext::default().set_offline(true);

        let result = ctx.parse_file_with_contents(&uri, source.to_string());
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        assert!(ctx.has_module_dependency(&path, &child));
        assert_eq!(
            ctx.get_uri_for_global_symbol(&uri, "Child").unwrap(),
            Some(LspUri::File(child.clone()))
        );
        assert_eq!(
            ctx.analysis.read().unwrap()[&path].symbols["Child"].parameters,
            Some(vec![
                "name".to_string(),
                "properties".to_string(),
                "setting".to_string()
            ])
        );
        assert!(
            ctx.get_uri_for_global_symbol(&uri, "Power")
                .unwrap()
                .is_some()
        );

        let failed = ctx.parse_file_with_contents(&uri, "fail(\"broken\")\n".to_string());
        assert!(!failed.diagnostics.is_empty());
        assert!(ctx.has_module_dependency(&path, &child));
        assert!(
            ctx.get_uri_for_global_symbol(&uri, "Child")
                .unwrap()
                .is_none()
        );
        assert!(
            ctx.get_completion_meta(&uri, "Module")
                .unwrap()
                .documentation
                .is_some()
        );

        let replaced = ctx.parse_file_with_contents(&uri, "answer = 42\n".to_string());
        assert!(
            replaced.diagnostics.is_empty(),
            "{:?}",
            replaced.diagnostics
        );
        assert!(!ctx.has_module_dependency(&path, &child));
        assert_eq!(
            ctx.get_completion_meta(&uri, "answer")
                .unwrap()
                .detail
                .as_deref(),
            Some("int")
        );
        Ok(())
    }

    #[test]
    fn closing_document_removes_netlist_subscription() {
        let ctx = LspEvalContext::default();
        let path = std::env::temp_dir().join("subscribed.zen");
        let uri = LspUri::File(path.clone());

        ctx.set_netlist_subscription(&path, &HashMap::new());
        assert_eq!(ctx.get_netlist_inputs(&path), Some(HashMap::new()));

        ctx.did_close_file(&uri);

        assert_eq!(ctx.get_netlist_inputs(&path), None);
    }

    #[test]
    fn symbol_watch_recovers_failed_evaluation_without_reopening_document() -> anyhow::Result<()> {
        use lsp_server::{Connection, Message};
        use std::time::Duration;

        let dir = tempfile::tempdir()?;
        let root = dir.path().canonicalize()?;
        let source_path = root.join("main.zen");
        let symbol_path = root.join("Part.kicad_sym");
        let uri = url::Url::from_file_path(&source_path).unwrap().to_string();
        let symbol_uri = url::Url::from_file_path(&symbol_path).unwrap().to_string();
        let source = r#"Component(
    name = "U1", skip_bom = True,
    symbol = Symbol(library = "Part.kicad_sym", name = "Part"),
    pins = {"P": builtin.net_type("Net")("N")},
)
"#;
        let symbol = r#"(kicad_symbol_lib (version 20211014) (generator kicad_symbol_editor)
  (symbol "Part" (in_bom yes) (on_board yes)
    (property "Reference" "U" (at 0 0 0))
    (property "Footprint" "Part" (at 0 0 0))
    (symbol "Part_1_1"
      (pin passive line (at 0 0 0) (length 2.54)
        (name "P" (effects (font (size 1.27 1.27))))
        (number "1" (effects (font (size 1.27 1.27))))))))
"#;
        fs::write(
            root.join("pcb.toml"),
            "[workspace]\npcb-version = \"0.4\"\n",
        )?;
        fs::write(&source_path, source)?;
        fs::write(
            &symbol_path,
            symbol.replace("\"Footprint\" \"Part\"", "\"Footprint\" \"Part.kicad_mod\""),
        )?;
        fs::write(
            root.join("Part.kicad_mod"),
            r#"(footprint "Part"
  (version 20240108) (generator "pcbnew") (layer "F.Cu")
  (pad "1" smd rect (at 0 0) (size 1 1) (layers "F.Cu" "F.Paste" "F.Mask")))"#,
        )?;

        let (client, server) = Connection::memory();
        let thread = std::thread::spawn(move || {
            super::server::server_with_connection(
                server,
                LspEvalContext::default().set_offline(true),
            )
        });
        let send = |value| -> anyhow::Result<()> {
            client
                .sender
                .send(serde_json::from_value::<Message>(value)?)?;
            Ok(())
        };
        let receive =
            |field: &str, expected: serde_json::Value| -> anyhow::Result<serde_json::Value> {
                loop {
                    let message = client.receiver.recv_timeout(Duration::from_secs(10))?;
                    let value = serde_json::to_value(message)?;
                    if value[field] == expected {
                        return Ok(value);
                    }
                }
            };
        send(json!({"id": 1, "method": "initialize", "params": {
            "rootUri": url::Url::from_directory_path(&root).unwrap().as_str(),
            "capabilities": {"workspace": {"didChangeWatchedFiles": {"dynamicRegistration": true}}}
        }}))?;
        receive("id", json!(1))?;
        send(json!({"method": "initialized", "params": {}}))?;
        let registration = receive("method", json!("client/registerCapability"))?;
        let watchers = &registration["params"]["registrations"][0]["registerOptions"]["watchers"];
        // These watches must exist before the first evaluation, and include
        // creation/deletion so missing or recreated symbols can recover too.
        assert_eq!(
            watchers,
            &json!([
                {"globPattern": "**/pcb.toml", "kind": 7},
                {"globPattern": "**/*.kicad_sym", "kind": 7}
            ])
        );
        send(json!({"id": registration["id"], "result": null}))?;
        send(
            json!({"method": "textDocument/didOpen", "params": {"textDocument": {
                "uri": uri, "languageId": "starlark", "version": 1, "text": source
            }}}),
        )?;
        send(json!({"id": 2, "method": "zener/evaluate", "params": {"uri": uri, "inputs": {}}}))?;
        let failed = receive("id", json!(2))?;
        assert_eq!(failed["result"]["success"], false, "{failed}");
        assert!(
            failed["result"]["diagnostics"]
                .to_string()
                .contains("Part.kicad_mod.kicad_mod"),
            "{failed}"
        );

        // Only the dependency changes; the open .zen buffer and subscription
        // stay unchanged. Watch notifications must push fresh netlists.
        for (change_type, success) in [(2, true), (3, false), (1, true)] {
            if change_type == 3 {
                fs::remove_file(&symbol_path)?;
            } else {
                fs::write(&symbol_path, symbol)?;
            }
            send(
                json!({"method": "workspace/didChangeWatchedFiles", "params": {"changes": [
                    {"uri": symbol_uri, "type": change_type}
                ]}}),
            )?;
            let update = receive("method", json!("zener/netlistUpdated"))?;
            assert_eq!(update["params"]["uri"], uri);
            assert_eq!(update["params"]["result"]["success"], success, "{update}");
            if success {
                assert_eq!(update["params"]["result"]["diagnostics"], json!([]));
            }
        }

        send(json!({"id": 3, "method": "shutdown", "params": null}))?;
        receive("id", json!(3))?;
        send(json!({"method": "exit", "params": null}))?;
        thread.join().unwrap()?;
        Ok(())
    }

    #[test]
    fn lsp_uses_file_scope_for_navigation_and_evaluation() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path().canonicalize()?;
        let package_dir = root.join("pkg");
        fs::create_dir_all(&package_dir)?;
        let dep_path = package_dir.join("dep.zen");
        let main_path = package_dir.join("main.zen");

        let main_contents = "load(\"./dep.zen\", \"foo\")\n";
        fs::write(
            root.join("pcb.toml"),
            "[workspace]\npcb-version = \"0.4\"\n",
        )?;
        fs::write(package_dir.join("pcb.toml"), "")?;
        fs::write(&main_path, main_contents)?;
        fs::write(&dep_path, "def foo():\n    return 0\n")?;

        let ctx = LspEvalContext::default();
        let main_url = LspUri::File(main_path.clone());
        let dep_url = LspUri::File(dep_path.clone());
        let stdlib_path = root.join(".pcb/stdlib/interfaces.zen");
        let stdlib_url = LspUri::File(stdlib_path.clone());

        assert_eq!(
            ctx.resolve_load("./dep.zen", &main_url, None)
                .map_err(anyhow::Error::msg)?,
            dep_url
        );
        assert_eq!(
            ctx.resolve_load("@stdlib/interfaces.zen", &main_url, None)
                .map_err(anyhow::Error::msg)?,
            stdlib_url
        );

        let stdlib_result =
            ctx.parse_file_with_contents(&stdlib_url, fs::read_to_string(stdlib_path)?);
        assert!(
            stdlib_result.ast.is_some(),
            "expected a loaded stdlib file to use the same evaluable scope"
        );
        assert!(
            stdlib_result.diagnostics.is_empty(),
            "expected no stdlib diagnostics, got {:?}",
            stdlib_result.diagnostics
        );

        ctx.did_change_file_contents(&main_url, main_contents);
        ctx.did_change_file_contents(&dep_url, "def foo():\n    return 1\n");

        let result = ctx.parse_file_with_contents(&main_url, main_contents.to_string());
        assert!(
            result.diagnostics.is_empty(),
            "expected no diagnostics, got {:?}",
            result.diagnostics
        );

        ctx.did_change_file_contents(&dep_url, "def foo(:\n");
        let result = ctx.parse_file_with_contents(&main_url, main_contents.to_string());
        assert!(
            !result.diagnostics.is_empty(),
            "expected diagnostics when dependency buffer is invalid"
        );

        fs::write(&dep_path, "def foo(:\n")?;
        ctx.did_close_file(&dep_url);
        let result = ctx.parse_file_with_contents(&main_url, main_contents.to_string());
        assert!(
            !result.diagnostics.is_empty(),
            "expected diagnostics when dependency falls back to disk"
        );

        Ok(())
    }

    #[test]
    fn lsp_does_not_cache_failed_dependency_resolution() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let main_path = dir.path().join("main.zen");
        let manifest_path = dir.path().join("pcb.toml");

        fs::write(
            &manifest_path,
            r#"[workspace]
pcb-version = "0.4"

[dependencies]
"github.com/acme/dep" = { branch = "main" }
"#,
        )?;
        fs::write(&main_path, "x = 1\n")?;

        let ctx = LspEvalContext::default();
        let workspace_root = ctx.workspace_root_for(&main_path);
        let _ = ctx.resolution_for(&main_path);
        assert!(
            !ctx.resolution_cache
                .read()
                .unwrap()
                .contains_key(&workspace_root),
            "failed dependency resolution should not be cached"
        );

        fs::write(
            &manifest_path,
            r#"[workspace]
pcb-version = "0.4"
"#,
        )?;
        let _ = ctx.resolution_for(&main_path);
        assert!(
            ctx.resolution_cache
                .read()
                .unwrap()
                .contains_key(&workspace_root),
            "successful dependency resolution should be cached"
        );

        Ok(())
    }

    #[test]
    fn warm_workspace_resolves_only_valid_manifest_roots() -> anyhow::Result<()> {
        let workspace = tempfile::tempdir()?;
        let plain_folder = tempfile::tempdir()?;
        let malformed = tempfile::tempdir()?;
        let main_path = workspace.path().join("main.zen");
        fs::write(
            workspace.path().join("pcb.toml"),
            "[workspace]\npcb-version = \"0.4\"\n",
        )?;
        fs::write(&main_path, "x = 1\n")?;
        fs::write(malformed.path().join("pcb.toml"), "[workspace\n")?;

        let ctx = LspEvalContext::default();
        ctx.warm_workspace(&[
            workspace.path().to_path_buf(),
            plain_folder.path().to_path_buf(),
            malformed.path().to_path_buf(),
        ]);

        assert_eq!(
            ctx.resolution_cache
                .read()
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            vec![&ctx.workspace_root_for(&main_path)],
            "only the valid manifest root is resolved, under the key evaluation looks up"
        );

        Ok(())
    }

    #[test]
    fn lsp_invalidates_symbol_library_cache_on_edit() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path().canonicalize()?;
        let lib_path = root.join("lib.kicad_sym");
        let main_path = root.join("main.zen");

        let single_symbol = r#"(kicad_symbol_lib
    (symbol "OnlySymbol"
        (property "Reference" "U" (at 0 0 0))
        (symbol "OnlySymbol_0_1"
            (pin input line (at 0 0 0) (length 2.54)
                (name "IN" (effects (font (size 1.27 1.27))))
                (number "1" (effects (font (size 1.27 1.27))))
            )
        )
    )
)"#;

        let multi_symbol = r#"(kicad_symbol_lib
    (symbol "OnlySymbol"
        (property "Reference" "U" (at 0 0 0))
        (symbol "OnlySymbol_0_1"
            (pin input line (at 0 0 0) (length 2.54)
                (name "IN" (effects (font (size 1.27 1.27))))
                (number "1" (effects (font (size 1.27 1.27))))
            )
        )
    )
    (symbol "SecondSymbol"
        (property "Reference" "U" (at 0 0 0))
        (symbol "SecondSymbol_0_1"
            (pin input line (at 0 0 0) (length 2.54)
                (name "IN" (effects (font (size 1.27 1.27))))
                (number "1" (effects (font (size 1.27 1.27))))
            )
        )
    )
)"#;

        let main_contents = "sym = Symbol(\"lib.kicad_sym\")\n";

        fs::write(
            root.join("pcb.toml"),
            "[workspace]\npcb-version = \"0.4\"\n",
        )?;
        fs::write(&lib_path, single_symbol)?;
        fs::write(&main_path, main_contents)?;

        let ctx = LspEvalContext::default();
        let main_url = LspUri::File(main_path.clone());
        let lib_url = LspUri::File(lib_path.clone());

        ctx.did_change_file_contents(&main_url, main_contents);
        let result = ctx.parse_file_with_contents(&main_url, main_contents.to_string());
        assert!(
            result.diagnostics.is_empty(),
            "expected no diagnostics for single symbol library"
        );

        ctx.did_change_file_contents(&lib_url, multi_symbol);
        let result = ctx.parse_file_with_contents(&main_url, main_contents.to_string());
        assert!(
            !result.diagnostics.is_empty(),
            "expected diagnostics for multiple symbols in library"
        );

        ctx.did_close_file(&lib_url);
        let result = ctx.parse_file_with_contents(&main_url, main_contents.to_string());
        assert!(
            result.diagnostics.is_empty(),
            "expected diagnostics to clear after closing edited library"
        );

        Ok(())
    }

    /// Convert an LSP position (line + UTF-16 character) back to a byte
    /// offset, mirroring how a client would apply the returned edit.
    fn lsp_position_to_offset(content: &str, position: lsp_types::Position) -> usize {
        let mut offset = 0;
        for _ in 0..position.line {
            offset += content[offset..].find('\n').map(|i| i + 1).unwrap();
        }
        let mut character = position.character;
        for c in content[offset..].chars() {
            if character == 0 {
                break;
            }
            character -= c.len_utf16() as u32;
            offset += c.len_utf8();
        }
        offset
    }

    fn apply_text_edit(content: &str, edit: &lsp_types::TextEdit) -> String {
        let start = lsp_position_to_offset(content, edit.range.start);
        let end = lsp_position_to_offset(content, edit.range.end);
        format!("{}{}{}", &content[..start], edit.new_text, &content[end..])
    }

    fn save_positions_request(id: i32, path: &std::path::Path, base_hash: &str) -> Request {
        let params = json!({
            "filePath": path.to_str().unwrap(),
            "baseHash": base_hash,
            "symbolPositions": {
                "comp:R1": { "x": 12.5, "y": 30.0, "rotation": 90.0 }
            },
            "deletedSymbolIds": ["comp:C1"],
        });
        Request {
            id: RequestId::from(id),
            method: "pcb/savePositions".to_string(),
            params,
        }
    }

    #[test]
    fn save_positions_with_base_hash_returns_edit_without_writing() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let zen_path = dir.path().canonicalize()?.join("main.zen");
        let disk_contents = "x = 1\n";
        // Buffer diverges from disk: the edit must be computed from the buffer.
        // No trailing newline exercises combined edits without reparsing a
        // regenerated position block at a different byte offset.
        let buffer_contents =
            "x = 1\n\n# pcb:sch C1 x=1.0000 y=2.0000 rot=0\n# pcb:sch C2 x=3.0000 y=4.0000 rot=0";
        fs::write(&zen_path, disk_contents)?;

        let ctx = LspEvalContext::default();
        ctx.did_change_file_contents(&LspUri::File(zen_path.clone()), buffer_contents);

        let base_hash = super::content_hash(buffer_contents);
        let response = ctx
            .handle_custom_request(
                &save_positions_request(1, &zen_path, &base_hash),
                &lsp_types::InitializeParams::default(),
            )
            .expect("request should be handled");

        let result = response.response_result.expect("request should succeed");
        assert_eq!(result["baseHash"], json!(base_hash));

        let edit: lsp_types::TextEdit = serde_json::from_value(result["edit"].clone())?;
        let updated = apply_text_edit(buffer_contents, &edit);
        assert!(updated.contains("# pcb:sch R1 x=12.5000 y=30.0000 rot=90"));
        assert!(!updated.contains("# pcb:sch C1"));
        assert!(updated.contains("# pcb:sch C2 x=3.0000 y=4.0000 rot=0"));
        assert_eq!(result["resultHash"], json!(super::content_hash(&updated)));

        // Neither disk nor the overlay was touched.
        assert_eq!(fs::read_to_string(&zen_path)?, disk_contents);
        assert_eq!(
            ctx.open_file_contents(&zen_path).as_deref(),
            Some(buffer_contents)
        );

        Ok(())
    }

    #[test]
    fn save_positions_with_stale_base_hash_returns_content_modified() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let zen_path = dir.path().canonicalize()?.join("main.zen");
        fs::write(&zen_path, "x = 1\n")?;

        let ctx = LspEvalContext::default();
        let response = ctx
            .handle_custom_request(
                &save_positions_request(1, &zen_path, &super::content_hash("stale")),
                &lsp_types::InitializeParams::default(),
            )
            .expect("request should be handled");

        let error = response
            .response_result
            .expect_err("stale hash should be rejected");
        assert_eq!(error.code, super::CONTENT_MODIFIED);
        // The file was not modified.
        assert_eq!(fs::read_to_string(&zen_path)?, "x = 1\n");

        Ok(())
    }

    #[test]
    fn save_positions_without_deleted_symbol_ids_is_rejected() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let zen_path = dir.path().canonicalize()?.join("main.zen");
        let contents = "x = 1\n\n# pcb:sch C1 x=1.0000 y=2.0000 rot=0\n";
        fs::write(&zen_path, contents)?;

        let ctx = LspEvalContext::default();
        let mut request = save_positions_request(1, &zen_path, &super::content_hash(contents));
        request
            .params
            .as_object_mut()
            .unwrap()
            .remove("deletedSymbolIds");
        let response = ctx
            .handle_custom_request(&request, &lsp_types::InitializeParams::default())
            .expect("request should be handled");

        let error = response
            .response_result
            .expect_err("missing deletedSymbolIds should fail");
        assert_eq!(error.code, super::INVALID_PARAMS);
        assert_eq!(fs::read_to_string(&zen_path)?, contents);

        Ok(())
    }

    #[test]
    fn save_positions_without_base_hash_is_rejected() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let zen_path = dir.path().canonicalize()?.join("main.zen");
        let contents = "x = 1\n";
        fs::write(&zen_path, contents)?;

        let ctx = LspEvalContext::default();
        let mut request = save_positions_request(1, &zen_path, "unused");
        let params = request.params.as_object_mut().unwrap();
        params.remove("baseHash");
        let response = ctx
            .handle_custom_request(&request, &lsp_types::InitializeParams::default())
            .expect("request should be handled");

        let error = response
            .response_result
            .expect_err("missing baseHash should fail");
        assert_eq!(error.code, super::INVALID_PARAMS);
        assert_eq!(fs::read_to_string(&zen_path)?, contents);

        Ok(())
    }

    #[test]
    fn custom_request_handler_handles_method() {
        let ctx = LspEvalContext::default().with_custom_request_handler(|method, params| {
            if method != "pcb/resolveDatasheet" {
                return Ok(None);
            }

            Ok(Some(json!({
                "ok": true,
                "echo": params
            })))
        });

        let req = Request {
            id: RequestId::from(1),
            method: "pcb/resolveDatasheet".to_string(),
            params: json!({
                "datasheetUrl": "https://example.com/datasheet.pdf"
            }),
        };

        let response = ctx
            .handle_custom_request(&req, &lsp_types::InitializeParams::default())
            .expect("custom request should be handled");

        assert_eq!(
            response.response_result.expect("request should succeed"),
            json!({
                "ok": true,
                "echo": {
                    "datasheetUrl": "https://example.com/datasheet.pdf"
                }
            })
        );
    }

    #[test]
    fn diagnostic_target_uri_parses_file_paths_before_urls() {
        let path = if cfg!(windows) {
            r"C:\Users\project\child.zen"
        } else {
            "/tmp/child.zen"
        };

        let url = LspEvalContext::diagnostic_target_uri(path).expect("path should resolve");
        assert_eq!(url.scheme().map(|scheme| scheme.as_str()), Some("file"));
    }

    #[test]
    fn diagnostic_target_uri_still_accepts_non_file_urls() {
        let url = LspEvalContext::diagnostic_target_uri("starlark:stdlib/foo.zen")
            .expect("URL should resolve");
        assert_eq!(url.scheme().map(|scheme| scheme.as_str()), Some("starlark"));
    }

    #[test]
    fn diagnostic_target_uri_encodes_bracketed_paths() {
        let path = std::env::temp_dir().join("board[rev2].zen");
        let uri = LspEvalContext::diagnostic_target_uri(&path.to_string_lossy())
            .expect("path should resolve");

        assert!(uri.as_str().contains("board%5Brev2%5D.zen"));
    }

    #[test]
    fn diagnostic_conversion_skips_invalid_related_uri() {
        let diagnostic = pcb_zen_core::Diagnostic::new(
            "primary diagnostic",
            EvalSeverity::Error,
            Path::new("main.zen"),
        )
        .with_related(DiagnosticReference {
            path: String::new(),
            span: ResolvedSpan::default(),
            message: "invalid related location".to_owned(),
        });

        let lsp_diagnostic = LspEvalContext::default().diagnostic_to_lsp(&diagnostic);

        assert!(lsp_diagnostic.related_information.is_none());
    }
}
