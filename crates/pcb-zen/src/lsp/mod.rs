pub mod signature;

use log::{debug, info};
use lsp_server::ResponseError;
use lsp_types::{
    Hover, HoverContents, MarkupContent, MarkupKind, ServerCapabilities, SignatureHelpOptions, Url,
    WorkDoneProgressOptions, request::Request,
};
use pcb_sch::position::{
    Position, remove_positions, replace_pcb_sch_comments, symbol_id_to_comment_key,
};
use pcb_starlark_lsp::server::{
    self, CompletionMeta, LspContext, LspEvalResult, LspUrl, Response, StringLiteralResult,
};
use pcb_zen_core::config::find_workspace_root;
use pcb_zen_core::file_extensions::is_kicad_symbol_file;
use pcb_zen_core::lang::symbol::invalidate_symbol_library;
use pcb_zen_core::lang::type_info::ParameterInfo;
use pcb_zen_core::{
    DefaultFileProvider, EvalContext, EvalContextConfig, FileProvider, FileProviderError,
};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use starlark::docs::DocModule;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

// JSON-RPC 2.0 error codes
const INVALID_PARAMS: i32 = -32602;
const INTERNAL_ERROR: i32 = -32603;

/// Wrapper around EvalContext that implements LspContext
pub struct LspEvalContext {
    inner: EvalContext,
    builtin_docs: HashMap<LspUrl, String>,
    file_provider: Arc<dyn FileProvider>,
    resolution_cache: RwLock<HashMap<PathBuf, Arc<ResolutionResult>>>,
    workspace_root_cache: RwLock<HashMap<PathBuf, PathBuf>>,
    open_files: Arc<RwLock<HashMap<PathBuf, String>>>,
    netlist_subscriptions: Arc<RwLock<HashMap<PathBuf, HashMap<String, JsonValue>>>>,
    suppress_netlist_updates: Arc<RwLock<HashSet<PathBuf>>>,
    custom_request_handler: Option<Arc<CustomRequestHandler>>,
}

type CustomRequestHandler =
    dyn Fn(&str, &JsonValue) -> anyhow::Result<Option<JsonValue>> + Send + Sync;

struct OverlayFileProvider {
    base: Arc<dyn FileProvider>,
    open_files: Arc<RwLock<HashMap<PathBuf, String>>>,
}

impl OverlayFileProvider {
    fn lookup(&self, path: &Path) -> Option<String> {
        if let Some(contents) = self.open_files.read().unwrap().get(path) {
            return Some(contents.clone());
        }

        if let Ok(canon) = self.base.canonicalize(path)
            && let Some(contents) = self.open_files.read().unwrap().get(&canon)
        {
            return Some(contents.clone());
        }

        None
    }

    fn has_overlay(&self, path: &Path) -> bool {
        if self.open_files.read().unwrap().contains_key(path) {
            return true;
        }

        self.base
            .canonicalize(path)
            .ok()
            .map(|canon| self.open_files.read().unwrap().contains_key(&canon))
            .unwrap_or(false)
    }
}

impl FileProvider for OverlayFileProvider {
    fn read_file(&self, path: &Path) -> Result<String, FileProviderError> {
        if let Some(contents) = self.lookup(path) {
            return Ok(contents);
        }

        self.base.read_file(path)
    }

    fn exists(&self, path: &Path) -> bool {
        self.has_overlay(path) || self.base.exists(path)
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
                && let Ok(lsp_url) = LspUrl::try_from(url)
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
        let resolution = crate::get_workspace_info(&file_provider, &std::env::temp_dir())
            .and_then(|mut ws| crate::resolve_dependencies(&mut ws, false, false))
            .unwrap_or_else(|_| ResolutionResult::empty());
        let inner = EvalContext::new(file_provider.clone(), resolution);

        Self {
            inner,
            builtin_docs,
            file_provider,
            resolution_cache: RwLock::new(HashMap::new()),
            workspace_root_cache: RwLock::new(HashMap::new()),
            open_files,
            netlist_subscriptions: Arc::new(RwLock::new(HashMap::new())),
            suppress_netlist_updates: Arc::new(RwLock::new(HashSet::new())),
            custom_request_handler: None,
        }
    }
}

impl LspEvalContext {
    pub fn set_eager(mut self, eager: bool) -> Self {
        self.inner = self.inner.set_eager(eager);
        self
    }

    pub fn with_custom_request_handler<F>(mut self, handler: F) -> Self
    where
        F: Fn(&str, &JsonValue) -> anyhow::Result<Option<JsonValue>> + Send + Sync + 'static,
    {
        self.custom_request_handler = Some(Arc::new(handler));
        self
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
            Some("pcb.toml" | "pcb.sum")
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
            .insert(key.clone(), inputs.clone());
        self.suppress_netlist_updates.write().unwrap().remove(&key);
    }

    fn get_netlist_inputs(&self, path: &Path) -> Option<HashMap<String, JsonValue>> {
        let key = self.normalize_path(path);
        self.netlist_subscriptions
            .read()
            .unwrap()
            .get(&key)
            .cloned()
    }

    fn suppress_next_netlist_update(&self, path: &Path) {
        let key = self.normalize_path(path);
        self.suppress_netlist_updates.write().unwrap().insert(key);
    }

    fn should_suppress_netlist_update(&self, path: &Path) -> bool {
        let key = self.normalize_path(path);
        self.suppress_netlist_updates.write().unwrap().remove(&key)
    }

    fn evaluate_with_inputs(
        &self,
        path_buf: &Path,
        inputs: &HashMap<String, JsonValue>,
    ) -> anyhow::Result<ZenerEvaluateResponse> {
        let uri = LspUrl::File(path_buf.to_path_buf());
        let maybe_contents = self.get_load_contents(&uri).ok().flatten();

        let config = self.config_for(path_buf);
        let mut ctx = EvalContext::from_session_and_config(Default::default(), config)
            .set_source_path(path_buf.to_path_buf());

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

        let schematic = eval_result
            .output
            .as_ref()
            .and_then(|output| output.to_schematic().ok())
            .and_then(|schematic| serde_json::to_value(&schematic).ok());

        let diagnostics = eval_result
            .diagnostics
            .into_iter()
            .map(|d| diagnostic_to_info(&d))
            .collect();

        Ok(ZenerEvaluateResponse {
            success: eval_result.output.is_some(),
            parameters,
            schematic,
            diagnostics,
        })
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

    /// Return the cached, canonicalized resolution for the workspace that owns `file_path`.
    fn resolution_for(&self, file_path: &Path) -> Arc<ResolutionResult> {
        let workspace_root = self.workspace_root_for(file_path);
        if let Some(cached) = self.resolution_cache.read().unwrap().get(&workspace_root) {
            return cached.clone();
        }

        let mut resolution = crate::get_workspace_info(&self.file_provider, &workspace_root)
            .and_then(|mut ws| crate::resolve_dependencies(&mut ws, false, false))
            .unwrap_or_else(|_| ResolutionResult::empty());
        resolution.canonicalize_keys(&*self.file_provider);
        let resolution = Arc::new(resolution);
        self.resolution_cache
            .write()
            .unwrap()
            .insert(workspace_root, resolution.clone());
        resolution
    }

    /// Create a fresh EvalContextConfig for the given file.
    fn config_for(&self, file_path: &Path) -> EvalContextConfig {
        EvalContextConfig::new(self.file_provider.clone(), self.resolution_for(file_path))
            .set_eager(self.inner.is_eager())
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

        // Build relatedInformation from each child diagnostic message that carries a span + valid path.
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

        // Add child diagnostics as related information
        let mut current = &diag.child;
        while let Some(child) = current {
            if let Some(span) = &child.span
                && !child.path.is_empty()
            {
                let child_range = Range {
                    start: Position {
                        line: span.begin.line as u32,
                        character: span.begin.column as u32,
                    },
                    end: Position {
                        line: span.end.line as u32,
                        character: span.end.column as u32,
                    },
                };

                related.push(DiagnosticRelatedInformation {
                    location: Location {
                        uri: lsp_types::Url::from_file_path(&child.path).unwrap_or_else(|_| {
                            lsp_types::Url::parse(&format!("file://{}", child.path)).unwrap()
                        }),
                        range: child_range,
                    },
                    message: child.body.clone(),
                });
            }
            current = &child.child;
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
            data: None,
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

    fn did_change_file_contents(&self, uri: &LspUrl, contents: &str) {
        if let LspUrl::File(path) = uri {
            self.store_open_file(path, contents);
            self.inner
                .set_file_contents(path.to_path_buf(), contents.to_string());
            self.maybe_invalidate_symbol_library(path);
            self.maybe_invalidate_resolution_cache(path);
        }
    }

    fn did_close_file(&self, uri: &LspUrl) {
        if let LspUrl::File(path) = uri {
            self.remove_open_file(path);
            self.inner.clear_file_contents(path);
            if let Ok(canon) = self.file_provider.canonicalize(path) {
                self.inner.clear_file_contents(&canon);
            }
            self.maybe_invalidate_symbol_library(path);
            self.maybe_invalidate_resolution_cache(path);
        }
    }

    fn did_save_file(&self, uri: &LspUrl) {
        if let LspUrl::File(path) = uri {
            self.maybe_invalidate_on_saved_source(path);
        }
    }

    fn watched_file_changed(&self, uri: &LspUrl) -> bool {
        match uri {
            LspUrl::File(path) => {
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

    fn netlist_update(&self, uri: &LspUrl) -> anyhow::Result<Option<JsonValue>> {
        let path = match uri {
            LspUrl::File(path) => path,
            _ => return Ok(None),
        };

        if self.should_suppress_netlist_update(path) {
            return Ok(None);
        }

        let Some(inputs) = self.get_netlist_inputs(path) else {
            return Ok(None);
        };

        let response = self.evaluate_with_inputs(path, &inputs)?;
        let params = ZenerNetlistUpdateParams {
            uri: uri.clone(),
            result: response,
            inputs: if inputs.is_empty() {
                None
            } else {
                Some(inputs)
            },
        };
        Ok(Some(serde_json::to_value(params)?))
    }

    fn parse_file_with_contents(&self, uri: &LspUrl, content: String) -> LspEvalResult {
        match uri {
            LspUrl::File(path) => {
                let workspace_root = self.workspace_root_for(path);
                let config = self.config_for(path);

                // Parse and analyze the file with the right resolution
                let ctx =
                    EvalContext::from_session_and_config(self.inner.session().clone(), config);
                let mut result = ctx.parse_and_analyze_file(path.clone(), content);

                // Apply LSP-specific diagnostic passes
                let passes = self.create_lsp_diagnostic_passes(&workspace_root);
                result.diagnostics.apply_passes(&passes);

                // Convert diagnostics to LSP format
                let diagnostics = result
                    .diagnostics
                    .iter()
                    .map(|d| self.diagnostic_to_lsp(d))
                    .collect();

                LspEvalResult {
                    diagnostics,
                    ast: result.output.flatten(),
                }
            }
            _ => {
                // For non-file URLs, return empty result
                LspEvalResult {
                    diagnostics: vec![],
                    ast: None,
                }
            }
        }
    }

    fn resolve_load(
        &self,
        path: &str,
        current_file: &LspUrl,
        _workspace_root: Option<&Path>,
    ) -> anyhow::Result<LspUrl> {
        // Use the load resolver from the inner context
        match current_file {
            LspUrl::File(current_path) => {
                let config = self.config_for(current_path);
                let resolved = config.resolve_path(path, current_path)?;
                Ok(LspUrl::File(resolved))
            }
            _ => Err(anyhow::anyhow!("Cannot resolve load from non-file URL")),
        }
    }

    fn render_as_load(
        &self,
        target: &LspUrl,
        current_file: &LspUrl,
        _workspace_root: Option<&Path>,
    ) -> anyhow::Result<String> {
        match (target, current_file) {
            (LspUrl::File(target_path), LspUrl::File(current_path)) => {
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
            _ => Err(anyhow::anyhow!("Can only render file URLs")),
        }
    }

    fn resolve_string_literal(
        &self,
        literal: &str,
        current_file: &LspUrl,
        _workspace_root: Option<&Path>,
    ) -> anyhow::Result<Option<StringLiteralResult>> {
        match current_file {
            LspUrl::File(current_path) => {
                // Try to resolve as a file path
                let config = self.config_for(current_path);
                if let Ok(resolved) = config.resolve_path(literal, current_path)
                    && resolved.exists()
                {
                    return Ok(Some(StringLiteralResult {
                        url: LspUrl::File(resolved),
                        location_finder: None,
                    }));
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    fn get_load_contents(&self, uri: &LspUrl) -> anyhow::Result<Option<String>> {
        match uri {
            LspUrl::File(path) => {
                if let Some(contents) = self.open_file_contents(path) {
                    return Ok(Some(contents));
                }

                if self.file_provider.exists(path) {
                    Ok(Some(self.file_provider.read_file(path)?))
                } else {
                    Ok(None)
                }
            }
            LspUrl::Starlark(_) => {
                // For starlark: URLs, check if we have builtin documentation
                Ok(self.builtin_docs.get(uri).cloned())
            }
            _ => Ok(None),
        }
    }

    fn get_environment(&self, _uri: &LspUrl) -> DocModule {
        // Return empty doc module for now
        DocModule::default()
    }

    fn get_url_for_global_symbol(
        &self,
        current_file: &LspUrl,
        symbol: &str,
    ) -> anyhow::Result<Option<LspUrl>> {
        match current_file {
            LspUrl::File(path) => {
                if let Some(target_path) = self.inner.get_url_for_global_symbol(path, symbol) {
                    Ok(Some(LspUrl::File(target_path)))
                } else {
                    // Check if it's a builtin
                    if let Ok(parsed_url) = Url::parse(&format!("starlark:/{symbol}.zen"))
                        && let Ok(lsp_url) = LspUrl::try_from(parsed_url)
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

    fn get_completion_meta(&self, current_file: &LspUrl, symbol: &str) -> Option<CompletionMeta> {
        match current_file {
            LspUrl::File(path) => {
                // First check for symbol info from the file
                if let Some(info) = self.inner.get_symbol_info(path, symbol) {
                    return Some(CompletionMeta {
                        kind: None, // We could map SymbolKind to CompletionItemKind here
                        detail: Some(info.type_name),
                        documentation: info.documentation,
                    });
                }

                // Fallback to builtin docs
                if let Ok(parsed_url) = Url::parse(&format!("starlark:/{symbol}.zen"))
                    && let Ok(lsp_url) = LspUrl::try_from(parsed_url)
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

    fn is_eager(&self) -> bool {
        self.inner.is_eager()
    }

    fn workspace_files(
        &self,
        workspace_roots: &[std::path::PathBuf],
    ) -> anyhow::Result<Vec<std::path::PathBuf>> {
        self.inner.find_workspace_files(workspace_roots)
    }

    fn has_module_dependency(&self, from: &Path, to: &Path) -> bool {
        self.inner.module_dep_exists(from, to)
    }

    fn get_custom_hover_for_load(
        &self,
        load_path: &str,
        _symbol_name: &str,
        current_file: &LspUrl,
        _workspace_root: Option<&Path>,
    ) -> anyhow::Result<Option<Hover>> {
        // Check if the load path is a directory
        match current_file {
            LspUrl::File(current_path) => {
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
                    let uri: LspUrl = match params
                        .text_document_position_params
                        .text_document
                        .uri
                        .try_into()
                    {
                        Ok(u) => u,
                        Err(e) => {
                            return Some(Response {
                                id: req.id.clone(),
                                result: None,
                                error: Some(ResponseError {
                                    code: 0,
                                    message: format!("Invalid URI: {e}"),
                                    data: None,
                                }),
                            });
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
                            return Some(Response {
                                id: req.id.clone(),
                                result: Some(serde_json::to_value(empty).unwrap()),
                                error: None,
                            });
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

                    return Some(Response {
                        id: req.id.clone(),
                        result: Some(serde_json::to_value(sig_help).unwrap()),
                        error: None,
                    });
                }
                Err(e) => {
                    return Some(Response {
                        id: req.id.clone(),
                        result: None,
                        error: Some(ResponseError {
                            code: 0,
                            message: format!("Failed to parse params: {e}"),
                            data: None,
                        }),
                    });
                }
            }
        }

        // Handle viewer/getState requests
        if req.method == ViewerGetStateRequest::METHOD {
            match serde_json::from_value::<ViewerGetStateParams>(req.params.clone()) {
                Ok(params) => {
                    let state_json: Option<JsonValue> = match &params.uri {
                        LspUrl::File(path_buf) => {
                            // Get contents from memory or disk
                            let maybe_contents = self.get_load_contents(&params.uri).ok().flatten();

                            // Evaluate the module
                            let config = self.config_for(path_buf);
                            let ctx =
                                EvalContext::from_session_and_config(Default::default(), config);

                            let eval_result = if let Some(contents) = maybe_contents {
                                ctx.set_source_path(path_buf.clone())
                                    .set_source_contents(contents)
                                    .eval()
                            } else {
                                ctx.set_source_path(path_buf.clone()).eval()
                            };

                            eval_result
                                .output
                                .and_then(|fmv| fmv.to_schematic().ok())
                                .and_then(|schematic| serde_json::to_value(&schematic).ok())
                        }
                        _ => None,
                    };

                    let response_payload = ViewerGetStateResponse { state: state_json };
                    return Some(Response {
                        id: req.id.clone(),
                        result: Some(serde_json::to_value(response_payload).unwrap()),
                        error: None,
                    });
                }
                Err(e) => {
                    return Some(Response {
                        id: req.id.clone(),
                        result: None,
                        error: Some(ResponseError {
                            code: 0,
                            message: format!("Failed to parse params: {e}"),
                            data: None,
                        }),
                    });
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
                            return Some(Response {
                                id: req.id.clone(),
                                result: Some(serde_json::to_value(response).unwrap()),
                                error: None,
                            });
                        }
                        Err(e) => {
                            return Some(Response {
                                id: req.id.clone(),
                                result: None,
                                error: Some(ResponseError {
                                    code: 0,
                                    message: format!("Evaluation failed: {e}"),
                                    data: None,
                                }),
                            });
                        }
                    }
                }
                Err(e) => {
                    return Some(Response {
                        id: req.id.clone(),
                        result: None,
                        error: Some(ResponseError {
                            code: 0,
                            message: format!("Failed to parse params: {e}"),
                            data: None,
                        }),
                    });
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
                        "Saving {} symbol positions to file: {}",
                        params.symbol_positions.len(),
                        file_path
                    );

                    // Convert symbol positions to comment format
                    let mut flat_positions = BTreeMap::new();
                    for (symbol_id, position) in params.symbol_positions {
                        let Some(comment_name) = symbol_id_to_comment_key(&symbol_id) else {
                            return Some(Response {
                                id: req.id.clone(),
                                result: None,
                                error: Some(ResponseError {
                                    code: INVALID_PARAMS,
                                    message: format!("Invalid symbol ID format: {symbol_id}"),
                                    data: None,
                                }),
                            });
                        };
                        flat_positions.insert(comment_name, position);
                    }

                    match replace_pcb_sch_comments(file_path, &flat_positions) {
                        Ok(()) => {
                            self.suppress_next_netlist_update(Path::new(file_path));
                            info!("Successfully wrote positions to file");
                            return Some(Response {
                                id: req.id.clone(),
                                result: Some(serde_json::Value::Null), // null indicates success
                                error: None,
                            });
                        }
                        Err(e) => {
                            return Some(Response {
                                id: req.id.clone(),
                                result: None,
                                error: Some(ResponseError {
                                    code: INTERNAL_ERROR,
                                    message: format!("Failed to update file: {e}"),
                                    data: None,
                                }),
                            });
                        }
                    }
                }
                Err(e) => {
                    return Some(Response {
                        id: req.id.clone(),
                        result: None,
                        error: Some(ResponseError {
                            code: INVALID_PARAMS,
                            message: format!("Invalid pcb/savePositions params: {e}"),
                            data: None,
                        }),
                    });
                }
            }
        }

        // Handle pcb/removePosition requests
        if req.method == "pcb/removePosition" {
            match serde_json::from_value::<PcbRemovePositionParams>(req.params.clone()) {
                Ok(params) => {
                    let file_path = &params.file_path;

                    // Translate symbol_id to comment key used in pcb:sch lines
                    let Some(comment_key) = symbol_id_to_comment_key(&params.symbol_id) else {
                        return Some(Response {
                            id: req.id.clone(),
                            result: None,
                            error: Some(ResponseError {
                                code: INVALID_PARAMS,
                                message: format!("Invalid symbol ID format: {}", params.symbol_id),
                                data: None,
                            }),
                        });
                    };

                    // Use the shared removal logic from pcb-sch
                    if let Err(e) = remove_positions(file_path, &[comment_key]) {
                        return Some(Response {
                            id: req.id.clone(),
                            result: None,
                            error: Some(ResponseError {
                                code: INTERNAL_ERROR,
                                message: format!("Failed to update file: {e}"),
                                data: None,
                            }),
                        });
                    }

                    self.suppress_next_netlist_update(Path::new(file_path));
                    return Some(Response {
                        id: req.id.clone(),
                        result: Some(serde_json::Value::Null),
                        error: None,
                    });
                }
                Err(e) => {
                    return Some(Response {
                        id: req.id.clone(),
                        result: None,
                        error: Some(ResponseError {
                            code: INVALID_PARAMS,
                            message: format!("Invalid pcb/removePosition params: {e}"),
                            data: None,
                        }),
                    });
                }
            }
        }

        if let Some(handler) = &self.custom_request_handler {
            match handler(&req.method, &req.params) {
                Ok(Some(result)) => {
                    return Some(Response {
                        id: req.id.clone(),
                        result: Some(result),
                        error: None,
                    });
                }
                Ok(None) => {}
                Err(err) => {
                    return Some(Response {
                        id: req.id.clone(),
                        result: None,
                        error: Some(ResponseError {
                            code: INTERNAL_ERROR,
                            message: err.to_string(),
                            data: None,
                        }),
                    });
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
    fn evaluate_module(
        &self,
        params: ZenerEvaluateParams,
    ) -> anyhow::Result<ZenerEvaluateResponse> {
        let path_buf = match &params.uri {
            LspUrl::File(path) => path,
            _ => return Err(anyhow::anyhow!("Only file URIs are supported")),
        };

        let response = self.evaluate_with_inputs(path_buf, &params.inputs)?;
        self.set_netlist_subscription(path_buf, &params.inputs);
        Ok(response)
    }
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
    uri: LspUrl,
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
    uri: LspUrl,
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
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ZenerNetlistUpdateParams {
    uri: LspUrl,
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
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct PcbRemovePositionParams {
    file_path: String,
    /// Symbol ID in the same format used by pcb/savePositions keys
    /// (e.g. "comp:R1" or "sym:NET#1")
    symbol_id: String,
}

#[cfg(test)]
mod tests {
    use super::{LspContext, LspEvalContext, LspUrl};
    use lsp_server::Request;
    use lsp_server::RequestId;
    use serde_json::json;
    use std::fs;

    #[test]
    fn lsp_loads_open_dependency_contents() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let dep_path = dir.path().join("dep.zen");
        let main_path = dir.path().join("main.zen");

        let main_contents = "load(\"./dep.zen\", \"foo\")\n";
        fs::write(&main_path, main_contents)?;
        fs::write(&dep_path, "def foo(:\n")?;

        let ctx = LspEvalContext::default();
        let main_url = LspUrl::File(main_path.clone());
        let dep_url = LspUrl::File(dep_path.clone());

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

        ctx.did_close_file(&dep_url);
        let result = ctx.parse_file_with_contents(&main_url, main_contents.to_string());
        assert!(
            !result.diagnostics.is_empty(),
            "expected diagnostics when dependency falls back to disk"
        );

        Ok(())
    }

    #[test]
    fn lsp_invalidates_symbol_library_cache_on_edit() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let lib_path = dir.path().join("lib.kicad_sym");
        let main_path = dir.path().join("main.zen");

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

        fs::write(&lib_path, single_symbol)?;
        fs::write(&main_path, main_contents)?;

        let ctx = LspEvalContext::default();
        let main_url = LspUrl::File(main_path.clone());
        let lib_url = LspUrl::File(lib_path.clone());

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

        assert!(response.error.is_none());
        assert_eq!(
            response.result,
            Some(json!({
                "ok": true,
                "echo": {
                    "datasheetUrl": "https://example.com/datasheet.pdf"
                }
            }))
        );
    }
}
