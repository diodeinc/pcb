//! `LspEvalContext` side of semantic call intelligence. Resolution of
//! callables and the LSP results built from them (completion, signature help,
//! keyword hover/definition, references, document symbols, parameter info).
//! See `parameter.rs` for the AST/model layer.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionTextEdit, DocumentSymbol, Documentation, Hover,
    HoverContents, Location, LocationLink, MarkupContent, MarkupKind, ParameterInformation,
    ParameterLabel, Position, Range, SignatureHelp, SignatureInformation, TextEdit,
};
use pcb_starlark_lsp::server::{self, LspContext, LspUri};
use pcb_zen_core::lang::context::FrozenContextValue;
use pcb_zen_core::lang::module::ModuleLoader;
use serde_json::{Value as JsonValue, json};
use starlark::codemap::{ResolvedPos, ResolvedSpan};
use starlark::values::ValueLike;
use url::Url;

use super::LspEvalContext;
use super::parameter::{self, Callable, Cursor, Decl, Param};

const MAX_WORKSPACE_FILES: usize = 5000;

pub(crate) fn lsp_range(span: ResolvedSpan) -> Range {
    Range {
        start: Position {
            line: span.begin.line as u32,
            character: span.begin.column as u32,
        },
        end: Position {
            line: span.end.line as u32,
            character: span.end.column as u32,
        },
    }
}

fn file_uri(path: &Path) -> Option<lsp_types::Uri> {
    server::url_to_uri(&Url::from_file_path(path).ok()?)
}

fn resolved(position: Position) -> ResolvedPos {
    ResolvedPos {
        line: position.line as usize,
        column: position.character as usize,
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    a == b
        || matches!((std::fs::canonicalize(a), std::fs::canonicalize(b)), (Ok(x), Ok(y)) if x == y)
}

impl LspEvalContext {
    /// Editor buffer if open, else the file on disk.
    pub(crate) fn current_contents(&self, path: &Path) -> Option<String> {
        self.open_file_contents(path)
            .or_else(|| self.file_provider.read_file(path).ok())
    }

    /// Callable for a `Module()` alias from its evaluated loader: the implicit
    /// `name`/`properties`, then every `io()`/`config()` with its declaration.
    pub(crate) fn module_callable(
        &self,
        callee: &str,
        loader: &ModuleLoader,
        target: &Path,
    ) -> Callable {
        let spans = self
            .current_contents(target)
            .and_then(|c| parameter::parse(target, &c))
            .map(|ast| parameter::declaration_spans(&ast))
            .unwrap_or_default();
        let mut params = vec![
            Param {
                name: "name".to_string(),
                kind: "implicit",
                type_name: Some("str".to_string()),
                required: true,
                default: None,
                help: Some("Instance name of this module in the design.".to_string()),
                allowed: None,
                decl: None,
            },
            Param {
                name: "properties".to_string(),
                kind: "implicit",
                type_name: Some("dict".to_string()),
                required: false,
                default: None,
                help: Some("Extra properties attached to this module instance.".to_string()),
                allowed: None,
                decl: None,
            },
        ];
        if let Some(extra) = loader
            .frozen_module
            .extra_value()
            .and_then(|e| e.downcast_ref::<FrozenContextValue>())
        {
            for p in extra.module().signature() {
                let span = spans.get(&p.name).copied().or(p.declaration_span);
                params.push(Param {
                    name: p.name.clone(),
                    kind: if p.is_config { "config" } else { "io" },
                    type_name: Some(p.type_value.to_value().to_str()),
                    // An io() is required unless optional=True (evaluation gives it a
                    // generated default net, which is not a user-facing default).
                    required: !p.optional && (!p.is_config || p.default_value.is_none()),
                    default: if p.is_config {
                        p.default_value.map(|v| v.to_value().to_repr())
                    } else {
                        None
                    },
                    help: p.help.clone(),
                    allowed: p
                        .allowed_values
                        .as_ref()
                        .map(|vs| vs.iter().map(|v| v.to_value().to_repr()).collect()),
                    decl: span.map(|span| Decl {
                        path: target.to_path_buf(),
                        span,
                    }),
                });
            }
        }
        Callable {
            callee: callee.to_string(),
            kind: "module",
            source: Some(target.to_path_buf()),
            params,
        }
    }

    /// The stdlib file that defines a prelude name for `path`, even after a failed evaluation.
    fn prelude_source(&self, path: &Path, name: &str) -> Option<PathBuf> {
        if let Some(p) = self
            .analysis
            .read()
            .unwrap()
            .get(path)
            .and_then(|a| a.symbols.get(name))
            .and_then(|i| i.source_path.clone())
        {
            return Some(p);
        }
        let config = self.config_for(path);
        config
            .prelude()
            .iter()
            .find(|(_, symbols)| symbols.contains(&name))
            .and_then(|(module_path, _)| config.resolve_path(module_path, path).ok())
    }

    /// What `name` invokes from `path` (with `ast` the current buffer).
    pub(crate) fn resolve_callable(
        &self,
        path: &Path,
        name: &str,
        ast: &starlark::syntax::AstModule,
    ) -> Option<Callable> {
        let cached = self
            .analysis
            .read()
            .unwrap()
            .get(path)
            .and_then(|a| a.callables.get(name))
            .cloned();
        if let Some(c) = cached
            && let Some(source) = c.source.as_deref()
            && self.alias_points_to(path, ast, name, source)
        {
            return Some(c);
        }
        if let Some(c) = parameter::local_def(ast, path, name) {
            return Some(c);
        }
        if parameter::bound_names(ast).contains(name) {
            return None; // a local binding we could not evaluate: never a guess
        }
        let source = self.prelude_source(path, name)?;
        let contents = self.current_contents(&source)?;
        parameter::stdlib_callable(&source, &contents, name)
    }

    /// Does the buffer (`ast`) still bind `name` to `Module(<spec resolving to source>)`,
    /// directly or through one `load()` re-export? Guards cached aliases against renames.
    fn alias_points_to(
        &self,
        path: &Path,
        ast: &starlark::syntax::AstModule,
        name: &str,
        source: &Path,
    ) -> bool {
        if let Some((_, spec)) = parameter::module_aliases(ast)
            .into_iter()
            .find(|(a, _)| a == name)
        {
            return self.resolves_to(&spec, path, source);
        }
        parameter::loads(ast).into_iter().any(|(module, args)| {
            args.iter().any(|(local, their)| {
                local == name
                    && matches!(self.resolve_load(&module, &LspUri::File(path.to_path_buf()), None), Ok(LspUri::File(lib))
                        if self.current_contents(&lib).and_then(|t| parameter::parse(&lib, &t)).is_some_and(|lib_ast| {
                            parameter::module_aliases(&lib_ast).iter().any(|(a, spec)| a == their && self.resolves_to(spec, &lib, source))
                        }))
            })
        })
    }

    fn site(
        &self,
        uri: &LspUri,
        position: Position,
    ) -> Option<(PathBuf, starlark::syntax::AstModule, parameter::CallSite)> {
        let LspUri::File(path) = uri else { return None };
        let contents = self.current_contents(path)?;
        let parsed = parameter::parse_at(path, &contents, resolved(position))?;
        let mut site = parameter::call_at(&parsed.ast, parsed.pos)?;
        // Spans handed back to the editor are in buffer coordinates.
        match &mut site.cursor {
            Cursor::Key { span, .. } => *span = parameter::unshift_span(*span, &parsed.shifts),
            Cursor::Slot {
                replace: Some(span),
                ..
            } => *span = parameter::unshift_span(*span, &parsed.shifts),
            _ => {}
        }
        Some((path.clone(), parsed.ast, site))
    }

    pub(crate) fn semantic_completion(
        &self,
        uri: &LspUri,
        position: Position,
    ) -> Option<Vec<CompletionItem>> {
        let (path, ast, site) = self.site(uri, position)?;
        let (prefix, replace) = match &site.cursor {
            Cursor::Slot { prefix, replace } => (prefix.clone(), *replace),
            // A key still being typed (no `=` after it in the buffer) completes like a slot.
            Cursor::Key { name, span }
                if !parameter::key_is_complete(
                    &self.current_contents(&path)?,
                    resolved(position),
                ) =>
            {
                let typed = (position.character as usize).saturating_sub(span.begin.column);
                (name.chars().take(typed).collect::<String>(), Some(*span))
            }
            _ => return None,
        };
        let prefix = &prefix;
        let callable = self.resolve_callable(&path, &site.callee, &ast)?;
        let items = callable
            .params
            .iter()
            .enumerate()
            .filter(|(_, p)| {
                !site.supplied.contains(&p.name) && p.name.starts_with(prefix.as_str())
            })
            .map(|(i, p)| {
                let insert = format!("{} = ", p.name);
                CompletionItem {
                    label: p.name.clone(),
                    kind: Some(CompletionItemKind::PROPERTY),
                    detail: Some(parameter::param_detail(p)),
                    documentation: Some(Documentation::MarkupContent(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: parameter::param_markdown(&callable, p),
                    })),
                    insert_text: Some(insert.clone()),
                    filter_text: Some(p.name.clone()),
                    sort_text: Some(format!("{}{:03}", if p.required { 0 } else { 1 }, i)),
                    text_edit: replace.map(|span| {
                        CompletionTextEdit::Edit(TextEdit {
                            range: lsp_range(span),
                            new_text: insert,
                        })
                    }),
                    ..Default::default()
                }
            })
            .collect();
        Some(items)
    }

    pub(crate) fn semantic_hover(&self, uri: &LspUri, position: Position) -> Option<Hover> {
        let (path, ast, site) = self.site(uri, position)?;
        let Cursor::Key { name, span } = &site.cursor else {
            return None;
        };
        let callable = self.resolve_callable(&path, &site.callee, &ast)?;
        let param = callable.param(name)?;
        Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: parameter::param_markdown(&callable, param),
            }),
            range: Some(lsp_range(*span)),
        })
    }

    /// `Some` for keyword keys (authoritative; empty when unresolved), `None` elsewhere.
    pub(crate) fn semantic_definition(
        &self,
        uri: &LspUri,
        position: Position,
    ) -> Option<Vec<LocationLink>> {
        let (path, ast, site) = self.site(uri, position)?;
        let Cursor::Key { name, span } = &site.cursor else {
            return None;
        };
        let link = self
            .resolve_callable(&path, &site.callee, &ast)
            .and_then(|c| c.param(name).and_then(|p| p.decl.clone()))
            .and_then(|decl| {
                Some(LocationLink {
                    origin_selection_range: Some(lsp_range(*span)),
                    target_uri: file_uri(&decl.path)?,
                    target_range: lsp_range(decl.span),
                    target_selection_range: lsp_range(decl.span),
                })
            });
        Some(link.into_iter().collect())
    }

    pub(crate) fn semantic_signature_help(
        &self,
        path: &Path,
        contents: &str,
        position: Position,
    ) -> Option<SignatureHelp> {
        let parsed = parameter::parse_at(path, contents, resolved(position))?;
        let ast = parsed.ast;
        let site = parameter::call_at(&ast, parsed.pos)?;
        if site.cursor == Cursor::Callee {
            return None;
        }
        let callable = self.resolve_callable(path, &site.callee, &ast)?;
        let names: Vec<&str> = callable.params.iter().map(|p| p.name.as_str()).collect();
        let index_of = |n: &str| names.iter().position(|x| *x == n).map(|i| i as u32);
        let active = match &site.cursor {
            Cursor::Key { name, .. }
            | Cursor::Value {
                keyword: Some(name),
                ..
            } => index_of(name),
            Cursor::Slot { prefix, .. } if !prefix.is_empty() => names
                .iter()
                .position(|x| x.starts_with(prefix.as_str()))
                .map(|i| i as u32),
            _ => None,
        };
        let parameters = callable
            .params
            .iter()
            .map(|p| ParameterInformation {
                label: ParameterLabel::Simple(p.name.clone()),
                documentation: Some(Documentation::MarkupContent(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: parameter::param_markdown(&callable, p),
                })),
            })
            .collect();
        let info = SignatureInformation {
            label: format!("{}({})", callable.callee, names.join(", ")),
            documentation: callable
                .source
                .as_ref()
                .map(|s| Documentation::String(format!("{} from {}", callable.kind, s.display()))),
            parameters: Some(parameters),
            active_parameter: active,
        };
        Some(SignatureHelp {
            signatures: vec![info],
            active_signature: Some(0),
            active_parameter: active,
        })
    }

    /// `.zen` files under the workspace root (hidden directories and build output skipped).
    fn workspace_zen_files(&self, root: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in entries.flatten() {
                let p = e.path();
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with('.') || name == "target" || name == "node_modules" {
                    continue;
                }
                match e.file_type() {
                    Ok(t) if t.is_dir() => stack.push(p),
                    Ok(t) if t.is_file() && name.ends_with(".zen") => {
                        out.push(p);
                        if out.len() >= MAX_WORKSPACE_FILES {
                            return out;
                        }
                    }
                    _ => {}
                }
            }
        }
        out.sort();
        out
    }

    fn resolves_to(&self, spec: &str, from: &Path, target: &Path) -> bool {
        matches!(self.resolve_load(spec, &LspUri::File(from.to_path_buf()), None), Ok(LspUri::File(p)) if same_file(&p, target))
    }

    /// References to the parameter under the cursor (a call keyword, or its `io()`/`config()` declaration).
    pub(crate) fn semantic_references(
        &self,
        uri: &LspUri,
        position: Position,
        include_declaration: bool,
    ) -> Vec<Location> {
        let LspUri::File(path) = uri else {
            return vec![];
        };
        let Some(contents) = self.current_contents(path) else {
            return vec![];
        };
        let Some(parsed) = parameter::parse_at(path, &contents, resolved(position)) else {
            return vec![];
        };
        let (ast, pos, shifts) = (parsed.ast, parsed.pos, parsed.shifts);

        // Identity: (file declaring the parameter, parameter name, builtin callee if any).
        let identity = match parameter::call_at(&ast, pos) {
            Some(site) if matches!(site.cursor, Cursor::Key { .. }) => {
                let Cursor::Key { name, .. } = &site.cursor else {
                    unreachable!()
                };
                self.resolve_callable(path, &site.callee, &ast)
                    .and_then(|c| {
                        let decl = c.param(name)?.decl.clone();
                        let by_name = (c.kind != "module").then(|| c.callee.clone());
                        Some((c.source.clone()?, name.clone(), by_name, decl, c.kind))
                    })
            }
            _ => parameter::declaration_spans(&ast)
                .into_iter()
                .find(|(_, span)| {
                    (span.begin.line, span.begin.column) <= (pos.line, pos.column)
                        && (pos.line, pos.column) <= (span.end.line, span.end.column)
                })
                .map(|(name, span)| {
                    (
                        path.clone(),
                        name,
                        None,
                        Some(Decl {
                            path: path.clone(),
                            span: parameter::unshift_span(span, &shifts),
                        }),
                        "module",
                    )
                }),
        };
        let Some((decl_file, param, builtin, decl, kind)) = identity else {
            return vec![];
        };
        // A local def's keywords only exist in its own file.
        let files = if kind == "function" {
            vec![path.clone()]
        } else {
            self.workspace_zen_files(&self.workspace_root_for(path))
        };

        let mut out = Vec::new();
        for file in files {
            let Some(text) = self.current_contents(&file) else {
                continue;
            };
            let Some((file_ast, shifts)) = parameter::parse_lenient(&file, &text) else {
                continue;
            };
            let mut callees: HashSet<String> = HashSet::new();
            match &builtin {
                Some(callee) if kind == "function" => {
                    callees.insert(callee.clone());
                }
                Some(callee) => {
                    if !parameter::bound_names(&file_ast).contains(callee) {
                        callees.insert(callee.clone());
                    }
                }
                None => {
                    for (alias, spec) in parameter::module_aliases(&file_ast) {
                        if self.resolves_to(&spec, &file, &decl_file) {
                            callees.insert(alias);
                        }
                    }
                    // One level of re-export: load("./lib.zen", "X") where lib.zen has X = Module(...).
                    for (module, args) in parameter::loads(&file_ast) {
                        let Ok(LspUri::File(lib)) =
                            self.resolve_load(&module, &LspUri::File(file.clone()), None)
                        else {
                            continue;
                        };
                        let Some(lib_ast) = self
                            .current_contents(&lib)
                            .and_then(|t| parameter::parse(&lib, &t))
                        else {
                            continue;
                        };
                        let exported = parameter::module_aliases(&lib_ast);
                        for (local, their) in args {
                            if exported.iter().any(|(a, spec)| {
                                *a == their && self.resolves_to(spec, &lib, &decl_file)
                            }) {
                                callees.insert(local);
                            }
                        }
                    }
                }
            }
            if callees.is_empty() {
                continue;
            }
            let Some(file_uri) = file_uri(&file) else {
                continue;
            };
            for span in parameter::keyword_uses(&file_ast, &callees, &param) {
                out.push(Location {
                    uri: file_uri.clone(),
                    range: lsp_range(parameter::unshift_span(span, &shifts)),
                });
            }
        }
        if include_declaration
            && let Some(d) = decl
            && let Some(u) = file_uri(&d.path)
        {
            out.push(Location {
                uri: u,
                range: lsp_range(d.span),
            });
        }
        out
    }

    #[allow(deprecated)] // DocumentSymbol::deprecated must be set
    pub(crate) fn semantic_document_symbols(&self, uri: &LspUri) -> Vec<DocumentSymbol> {
        let LspUri::File(path) = uri else {
            return vec![];
        };
        let Some((ast, shifts)) = self
            .current_contents(path)
            .and_then(|c| parameter::parse_lenient(path, &c))
        else {
            return vec![];
        };
        parameter::outline(&ast)
            .into_iter()
            .map(|o| DocumentSymbol {
                name: o.name,
                detail: o.detail,
                kind: o.kind,
                tags: None,
                deprecated: None,
                range: lsp_range(parameter::unshift_span(o.range, &shifts)),
                selection_range: lsp_range(parameter::unshift_span(o.selection, &shifts)),
                children: None,
            })
            .collect()
    }

    /// `zener/parameterInfo`: the resolved callable around a position, with provenance.
    pub(crate) fn semantic_parameter_info(&self, uri: &LspUri, position: Position) -> JsonValue {
        let Some((path, ast, site)) = self.site(uri, position) else {
            return JsonValue::Null;
        };
        let Some(c) = self.resolve_callable(&path, &site.callee, &ast) else {
            return JsonValue::Null;
        };
        let active = match &site.cursor {
            Cursor::Key { name, .. }
            | Cursor::Value {
                keyword: Some(name),
                ..
            } => Some(name.clone()),
            _ => None,
        };
        json!({
            "callee": c.callee,
            "kind": c.kind,
            "source": c.source.as_deref().and_then(file_uri).map(|u| u.to_string()),
            "active": active,
            "parameters": c.params.iter().map(|p| json!({
                "name": p.name, "kind": p.kind, "type": p.type_name, "required": p.required,
                "default": p.default, "help": p.help, "allowed": p.allowed,
                "location": p.decl.as_ref().and_then(|d| Some(json!({"uri": file_uri(&d.path)?.to_string(), "range": lsp_range(d.span)}))),
            })).collect::<Vec<_>>(),
        })
    }
}

impl LspEvalContext {
    /// Route the semantic requests; `None` for other methods.
    pub(crate) fn handle_semantic_request(
        &self,
        req: &server::Request,
    ) -> Option<server::Response> {
        use server::Response;
        let bad = |e: String| Response::new_err(req.id.clone(), -32602, e);
        match req.method.as_str() {
            "textDocument/references" => Some(
                match serde_json::from_value::<lsp_types::ReferenceParams>(req.params.clone()) {
                    Ok(p) => match LspUri::try_from(p.text_document_position.text_document.uri) {
                        Ok(uri) => Response::new_ok(
                            req.id.clone(),
                            self.semantic_references(
                                &uri,
                                p.text_document_position.position,
                                p.context.include_declaration,
                            ),
                        ),
                        Err(e) => bad(format!("Invalid URI: {e}")),
                    },
                    Err(e) => bad(format!("Invalid params: {e}")),
                },
            ),
            "textDocument/documentSymbol" => Some(
                match serde_json::from_value::<lsp_types::DocumentSymbolParams>(req.params.clone())
                {
                    Ok(p) => match LspUri::try_from(p.text_document.uri) {
                        Ok(uri) => Response::new_ok(
                            req.id.clone(),
                            lsp_types::DocumentSymbolResponse::Nested(
                                self.semantic_document_symbols(&uri),
                            ),
                        ),
                        Err(e) => bad(format!("Invalid URI: {e}")),
                    },
                    Err(e) => bad(format!("Invalid params: {e}")),
                },
            ),
            "zener/parameterInfo" => Some(
                match serde_json::from_value::<lsp_types::TextDocumentPositionParams>(
                    req.params.clone(),
                ) {
                    Ok(p) => match LspUri::try_from(p.text_document.uri) {
                        Ok(uri) => Response::new_ok(
                            req.id.clone(),
                            self.semantic_parameter_info(&uri, p.position),
                        ),
                        Err(e) => bad(format!("Invalid URI: {e}")),
                    },
                    Err(e) => bad(format!("Invalid params: {e}")),
                },
            ),
            _ => None,
        }
    }
}
