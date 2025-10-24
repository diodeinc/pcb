pub mod signature;

use log::{debug, info};
use lsp_server::ResponseError;
use lsp_types::{
    request::Request, Hover, HoverContents, MarkupContent, MarkupKind, ServerCapabilities,
    SignatureHelpOptions, Url, WorkDoneProgressOptions,
};
use pcb_sch::position::{
    parse_position_comments, replace_pcb_sch_comments, symbol_id_to_comment_key, NaturalString,
    Position,
};
use pcb_starlark_lsp::server::{
    self, CompletionMeta, LspContext, LspEvalResult, LspUrl, Response, StringLiteralResult,
};
use pcb_zen_core::config::find_workspace_root;
use pcb_zen_core::lang::type_info::ParameterInfo;
use pcb_zen_core::{
    CoreLoadResolver, DefaultFileProvider, EvalContext, FileProvider, LoadResolver,
};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use starlark::docs::DocModule;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;
use std::{
    fs::OpenOptions,
    io::{Seek, Write},
};

use crate::load::DefaultRemoteFetcher;

// JSON-RPC 2.0 error codes
const INVALID_PARAMS: i32 = -32602;
const INTERNAL_ERROR: i32 = -32603;

/// Wrapper around EvalContext that implements LspContext
pub struct LspEvalContext {
    inner: EvalContext,
    builtin_docs: HashMap<LspUrl, String>,
    file_provider: Arc<dyn FileProvider>,
}

/// Helper function to create a standard load resolver with remote and workspace support
fn create_standard_load_resolver(
    file_provider: Arc<dyn FileProvider>,
    file_path: &Path,
) -> Arc<CoreLoadResolver> {
    let workspace_root = find_workspace_root(file_provider.as_ref(), file_path);

    let remote_fetcher = Arc::new(DefaultRemoteFetcher::default());
    Arc::new(CoreLoadResolver::new(
        file_provider,
        remote_fetcher,
        workspace_root.to_path_buf(),
        true,
    ))
}

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
            if let Ok(url) = Url::parse(&format!("starlark:/{name}.zen")) {
                if let Ok(lsp_url) = LspUrl::try_from(url) {
                    builtin_docs.insert(lsp_url, item.render_as_code(&name));
                }
            }
        }

        let file_provider = Arc::new(DefaultFileProvider::new());
        let load_resolver =
            create_standard_load_resolver(file_provider.clone(), &std::env::temp_dir());
        let inner = EvalContext::new(load_resolver);

        Self {
            inner,
            builtin_docs,
            file_provider,
        }
    }
}

impl LspEvalContext {
    pub fn set_eager(mut self, eager: bool) -> Self {
        self.inner = self.inner.set_eager(eager);
        self
    }

    /// Create LSP-specific diagnostic passes
    fn create_lsp_diagnostic_passes(
        &self,
        current_file: &std::path::Path,
    ) -> Vec<Box<dyn pcb_zen_core::DiagnosticsPass>> {
        let file_provider = self.inner.file_provider();
        let workspace_root = find_workspace_root(file_provider, current_file);
        vec![
            Box::new(pcb_zen_core::FilterHiddenPass),
            Box::new(pcb_zen_core::LspFilterPass::new(workspace_root)),
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
            if let Some(span) = &child.span {
                if !child.path.is_empty() {
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

    fn parse_file_with_contents(&self, uri: &LspUrl, content: String) -> LspEvalResult {
        match uri {
            LspUrl::File(path) => {
                // Create a load resolver for this file
                let load_resolver =
                    create_standard_load_resolver(self.file_provider.clone(), uri.path());

                // Parse and analyze the file with the load resolver set
                let mut result = self
                    .inner
                    .child_context(None)
                    .set_load_resolver(load_resolver)
                    .parse_and_analyze_file(path.clone(), content.clone());

                // Apply LSP-specific diagnostic passes
                let passes = self.create_lsp_diagnostic_passes(path);
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
                let load_resolver =
                    create_standard_load_resolver(self.file_provider.clone(), current_path);
                let resolved = load_resolver.resolve_path(path, current_path)?;
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
                {
                    if target_parent == current_parent {
                        if let Some(file_name) = target_path.file_name() {
                            return Ok(format!("./{}", file_name.to_string_lossy()));
                        }
                    }
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
                let load_resolver =
                    create_standard_load_resolver(self.file_provider.clone(), current_path);
                if let Ok(resolved) = load_resolver
                    .resolve_context(literal, current_path)
                    .and_then(|mut c| load_resolver.resolve(&mut c))
                {
                    if resolved.exists() {
                        return Ok(Some(StringLiteralResult {
                            url: LspUrl::File(resolved),
                            location_finder: None,
                        }));
                    }
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    fn get_load_contents(&self, uri: &LspUrl) -> anyhow::Result<Option<String>> {
        match uri {
            LspUrl::File(path) => {
                // First check in-memory contents
                if let Some(contents) = self.inner.get_file_contents(path) {
                    return Ok(Some(contents));
                }
                // Then check file system
                if path.exists() {
                    Ok(Some(std::fs::read_to_string(path)?))
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
                    if let Ok(parsed_url) = Url::parse(&format!("starlark:/{symbol}.zen")) {
                        if let Ok(lsp_url) = LspUrl::try_from(parsed_url) {
                            if self.builtin_docs.contains_key(&lsp_url) {
                                return Ok(Some(lsp_url));
                            }
                        }
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
                if let Ok(parsed_url) = Url::parse(&format!("starlark:/{symbol}.zen")) {
                    if let Ok(lsp_url) = LspUrl::try_from(parsed_url) {
                        if let Some(doc) = self.builtin_docs.get(&lsp_url) {
                            let first_line = doc.lines().next().unwrap_or("").to_string();
                            return Some(CompletionMeta {
                                kind: Some(lsp_types::CompletionItemKind::FUNCTION),
                                detail: Some(first_line),
                                documentation: Some(doc.clone()),
                            });
                        }
                    }
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
                let load_resolver =
                    create_standard_load_resolver(self.file_provider.clone(), current_path);
                if let Ok(resolved) = load_resolver
                    .resolve_context(load_path, current_path)
                    .and_then(|mut c| load_resolver.resolve(&mut c))
                {
                    if resolved.is_dir() {
                        return Ok(Some(Hover {
                            contents: HoverContents::Markup(MarkupContent {
                                kind: MarkupKind::Markdown,
                                value: format!("Directory: `{}`", resolved.display()),
                            }),
                            range: None,
                        }));
                    }
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
                            let load_resolver =
                                create_standard_load_resolver(self.file_provider.clone(), path_buf);
                            let ctx = EvalContext::new(load_resolver);

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

                    // Read existing content
                    let content = match std::fs::read_to_string(file_path) {
                        Ok(c) => c,
                        Err(e) => {
                            return Some(Response {
                                id: req.id.clone(),
                                result: None,
                                error: Some(ResponseError {
                                    code: INTERNAL_ERROR,
                                    message: format!("Failed to read file: {e}"),
                                    data: None,
                                }),
                            });
                        }
                    };

                    // Parse existing positions and determine the block start
                    let (mut existing_positions, block_start) = parse_position_comments(&content);

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

                    // Remove if present (use NaturalString to ensure comparator matches)
                    let lookup_key = NaturalString::from(comment_key.as_str());
                    let _removed = existing_positions.remove(&lookup_key).is_some();

                    // Prepare new position comments (preserve formatting rules)
                    let mut position_comments = String::new();
                    if !existing_positions.is_empty() {
                        let content_before = &content[..block_start];
                        let needs_blank_line =
                            !content_before.is_empty() && !content_before.ends_with("\n\n");
                        if needs_blank_line {
                            if content_before.ends_with('\n') {
                                position_comments.push('\n');
                            } else {
                                position_comments.push_str("\n\n");
                            }
                        }

                        for (element_id, position) in &existing_positions {
                            let comment = format!(
                                "# pcb:sch {} x={:.4} y={:.4} rot={:.0}\n",
                                element_id, position.x, position.y, position.rotation
                            );
                            position_comments.push_str(&comment);
                        }
                    }

                    // Write back: truncate at block_start, then append new comments (if any)
                    let mut file = match OpenOptions::new().write(true).read(true).open(file_path) {
                        Ok(f) => f,
                        Err(e) => {
                            return Some(Response {
                                id: req.id.clone(),
                                result: None,
                                error: Some(ResponseError {
                                    code: INTERNAL_ERROR,
                                    message: format!("Failed to open file for write: {e}"),
                                    data: None,
                                }),
                            });
                        }
                    };
                    if let Err(e) = file.set_len(block_start as u64) {
                        return Some(Response {
                            id: req.id.clone(),
                            result: None,
                            error: Some(ResponseError {
                                code: INTERNAL_ERROR,
                                message: format!("Failed to truncate file: {e}"),
                                data: None,
                            }),
                        });
                    }
                    if let Err(e) = file.seek(std::io::SeekFrom::Start(block_start as u64)) {
                        return Some(Response {
                            id: req.id.clone(),
                            result: None,
                            error: Some(ResponseError {
                                code: INTERNAL_ERROR,
                                message: format!("Failed to seek file: {e}"),
                                data: None,
                            }),
                        });
                    }
                    if let Err(e) = file.write_all(position_comments.as_bytes()) {
                        return Some(Response {
                            id: req.id.clone(),
                            result: None,
                            error: Some(ResponseError {
                                code: INTERNAL_ERROR,
                                message: format!("Failed to write file: {e}"),
                                data: None,
                            }),
                        });
                    }

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

        None
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

        // Get contents from memory or disk
        let maybe_contents = self.get_load_contents(&params.uri).ok().flatten();

        // Create evaluation context
        let load_resolver = create_standard_load_resolver(self.file_provider.clone(), path_buf);
        let mut ctx = EvalContext::new(load_resolver)
            .set_source_path(path_buf.clone());

        ctx = if let Some(contents) = maybe_contents {
            ctx.set_source_contents(contents)
        } else {
            ctx
        };

        // Convert JSON inputs directly to heap values (no serialization!)
        if !params.inputs.is_empty() {
            let json_map = starlark::collections::SmallMap::from_iter(params.inputs);
            ctx.set_json_inputs(json_map);
        }

        // Evaluate the module
        let eval_result = ctx.eval();

        // Extract parameters from the result
        let parameters = eval_result
            .output
            .as_ref()
            .map(|output| output.signature.clone());

        // Generate schematic JSON if evaluation succeeded
        let schematic = eval_result
            .output
            .as_ref()
            .and_then(|output| output.to_schematic().ok())
            .and_then(|schematic| serde_json::to_value(&schematic).ok());

        // Convert diagnostics
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
