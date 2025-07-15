pub mod signature;

use lsp_server::ResponseError;
use lsp_types::{
    request::Request, Hover, HoverContents, MarkupContent, MarkupKind, ServerCapabilities,
    SignatureHelpOptions, Url, WorkDoneProgressOptions,
};
use pcb_starlark_lsp::server::{
    self, CompletionMeta, LspContext, LspEvalResult, LspUrl, Response, StringLiteralResult,
};
use pcb_zen_core::workspace::find_workspace_root;
use pcb_zen_core::{
    CoreLoadResolver, DefaultFileProvider, EvalContext, FileProvider, InputMap, LoadResolver,
};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use starlark::docs::DocModule;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use crate::load::DefaultRemoteFetcher;
use pcb_zen_core::convert::ToSchematic;

/// Wrapper around EvalContext that implements LspContext
pub struct LspEvalContext {
    inner: EvalContext,
    builtin_docs: HashMap<LspUrl, String>,
    file_provider: Arc<dyn FileProvider>,
}

/// Helper function to create a standard load resolver with remote and workspace support
fn create_standard_load_resolver(
    file_provider: Arc<dyn FileProvider>,
    workspace_root: &Path,
) -> Arc<CoreLoadResolver> {
    let remote_fetcher = Arc::new(DefaultRemoteFetcher);
    Arc::new(CoreLoadResolver::new(
        file_provider,
        remote_fetcher,
        Some(workspace_root.to_path_buf()),
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

        let file_provider = Arc::new(DefaultFileProvider);
        let inner = EvalContext::with_file_provider(file_provider.clone());

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

        lsp_types::Diagnostic {
            range,
            severity: Some(severity),
            code: None,
            code_description: None,
            source: Some("diode-star".to_string()),
            message: diag.body.clone(),
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
                // Determine workspace root from the file path
                let workspace_root = path
                    .parent()
                    .and_then(|parent| find_workspace_root(self.file_provider.as_ref(), parent))
                    .unwrap_or_else(|| path.parent().unwrap_or(path).to_path_buf());

                // Create a load resolver for this file
                let load_resolver =
                    create_standard_load_resolver(self.file_provider.clone(), &workspace_root);

                // Parse and analyze the file with the load resolver set
                let result = self
                    .inner
                    .child_context()
                    .set_load_resolver(load_resolver)
                    .parse_and_analyze_file(path.clone(), content.clone());

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
                // Determine workspace root from current file
                let workspace_root = current_path.parent().unwrap_or(current_path).to_path_buf();

                let load_resolver =
                    create_standard_load_resolver(self.file_provider.clone(), &workspace_root);
                let resolved =
                    load_resolver.resolve_path(self.file_provider.as_ref(), path, current_path)?;
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
                let workspace_root = current_path.parent().unwrap_or(current_path).to_path_buf();

                let load_resolver =
                    create_standard_load_resolver(self.file_provider.clone(), &workspace_root);
                if let Ok(resolved) =
                    load_resolver.resolve_path(self.file_provider.as_ref(), literal, current_path)
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
                let workspace_root = current_path.parent().unwrap_or(current_path).to_path_buf();

                let load_resolver =
                    create_standard_load_resolver(self.file_provider.clone(), &workspace_root);
                if let Ok(resolved) =
                    load_resolver.resolve_path(self.file_provider.as_ref(), load_path, current_path)
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
                            let workspace_root = path_buf
                                .parent()
                                .unwrap_or(path_buf.as_path())
                                .to_path_buf();

                            let ctx = EvalContext::new()
                                .set_file_provider(self.file_provider.clone())
                                .set_load_resolver(create_standard_load_resolver(
                                    self.file_provider.clone(),
                                    &workspace_root,
                                ));

                            let eval_result = if let Some(contents) = maybe_contents {
                                ctx.set_source_path(path_buf.clone())
                                    .set_module_name("<root>".to_string())
                                    .set_inputs(InputMap::new())
                                    .set_source_contents(contents)
                                    .eval()
                            } else {
                                ctx.set_source_path(path_buf.clone())
                                    .set_module_name("<root>".to_string())
                                    .set_inputs(InputMap::new())
                                    .eval()
                            };

                            eval_result.output.and_then(|fmv| {
                                match fmv.sch_module.to_schematic() {
                                    Ok(schematic) => {
                                        // Serialize to JSON
                                        serde_json::to_value(&schematic).ok()
                                    }
                                    Err(_) => None,
                                }
                            })
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

        None
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
