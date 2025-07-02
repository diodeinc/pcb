use anyhow::Result;
use lsp_types::{ParameterInformation, ParameterLabel, SignatureHelp, SignatureInformation};
// MarkupContent etc not used currently but left for future expansion. Removed to suppress warnings.
use starlark::codemap::{CodeMap, ResolvedPos, ResolvedSpan};
use starlark::syntax::ast::*;
use starlark::syntax::AstModule;
use starlark_syntax::syntax::module::AstModuleFields;

use pcb_starlark_lsp::server::{LspContext, LspUrl};

use std::collections::HashMap;

/// Helper function to render a signature from a function name and parameters
pub fn render_signature(name: &str, params: &[String]) -> String {
    format!("{}({})", name, params.join(", "))
}

// Represents an invocation of a function call at the cursor position.
#[derive(Debug)]
pub(crate) struct Call {
    #[allow(dead_code)]
    pub(crate) function_span: ResolvedSpan,
    pub(crate) function_name: String,
    pub(crate) current_argument: CallArgument,
}

#[derive(Debug)]
pub(crate) enum CallArgument {
    /// The i-th positional argument.
    Positional(usize),
    /// Named argument `name = ...`.
    Named(String),
    /// The cursor is inside the parens but before any argument.
    None,
}

/// Given an [`AstModule`] and a cursor position (0-based line/character) return any function
/// invocations that enclose that position.
pub(crate) fn calls_at_position(ast: &AstModule, line: u32, character: u32) -> Result<Vec<Call>> {
    let mut out = Vec::new();
    let pos = ResolvedPos {
        line: line as usize,
        column: character as usize,
    };
    ast.statement()
        .visit_expr(|expr| visit_expr_recursive(expr, ast.codemap(), pos, &mut out));
    Ok(out)
}

fn visit_expr_recursive<P: AstPayload>(
    expr: &AstExprP<P>,
    codemap: &CodeMap,
    pos: ResolvedPos,
    out: &mut Vec<Call>,
) {
    // Only consider nodes that actually enclose the position.
    if !codemap.resolve_span(expr.span).contains(pos) {
        return;
    }

    match &expr.node {
        ExprP::Call(target, args) => {
            if let ExprP::Identifier(ident) = &target.node {
                // Determine which argument the cursor is currently inside.
                let current_arg = args
                    .args
                    .iter()
                    .enumerate()
                    .find_map(|(idx, arg)| {
                        let span = codemap.resolve_span(arg.span);
                        if span.contains(pos) {
                            match &arg.node {
                                ArgumentP::Positional(_) => Some(CallArgument::Positional(idx)),
                                ArgumentP::Named(name, _) => {
                                    Some(CallArgument::Named(name.node.clone()))
                                }
                                _ => None,
                            }
                        } else {
                            None
                        }
                    })
                    .unwrap_or({
                        if args.args.is_empty() {
                            CallArgument::None
                        } else {
                            CallArgument::Positional(args.args.len())
                        }
                    });

                out.push(Call {
                    function_span: codemap.resolve_span(ident.span),
                    function_name: ident.node.ident.clone(),
                    current_argument: current_arg,
                });
            }
            // Recurse into the call target and the arguments.
            visit_expr_recursive(target, codemap, pos, out);
            for arg in &args.args {
                match &arg.node {
                    ArgumentP::Positional(expr) => visit_expr_recursive(expr, codemap, pos, out),
                    ArgumentP::Named(_, v) => visit_expr_recursive(v, codemap, pos, out),
                    ArgumentP::Args(v) => visit_expr_recursive(v, codemap, pos, out),
                    ArgumentP::KwArgs(v) => visit_expr_recursive(v, codemap, pos, out),
                }
            }
        }
        _ => {
            // Recurse to children expressions.
            expr.visit_expr(|e| visit_expr_recursive(e, codemap, pos, out));
        }
    }
}

/// Helper to extract the parameter names from a `def` statement node.
fn param_names<P: AstPayload>(params: &[starlark::codemap::Spanned<ParameterP<P>>]) -> Vec<String> {
    params
        .iter()
        .filter_map(|param| param.split().0.map(|ident| ident.node.ident.clone()))
        .collect()
}

/// Search the AST for the first `def` with the given `name`.
pub(crate) fn find_def_params<P: AstPayload>(
    stmt: &AstStmtP<P>,
    name: &str,
) -> Option<Vec<String>> {
    match &stmt.node {
        StmtP::Def(def) if def.name.ident == name => Some(param_names(&def.params)),
        StmtP::Statements(ss) => ss.iter().find_map(|s| find_def_params(s, name)),
        StmtP::If(_, body) => find_def_params(body, name),
        StmtP::For(f) => find_def_params(&f.body, name),
        _ => None,
    }
}

/// Inspect all `load()` statements in `ast` and attempt to resolve the
/// parameters of each imported symbol. Returns two maps:
/// 1. `alias → Vec<param names>` so that signature helpers can surface them.
/// 2. `alias → LspUrl` pointing at the originating module for go-to-definition.
pub fn load_symbols_info<T: LspContext>(
    ast: &AstModule,
    ctx: &T,
    current_uri: &LspUrl,
) -> (HashMap<String, Vec<String>>, HashMap<String, LspUrl>) {
    use starlark::syntax::ast::{LoadArgP, StmtP};

    let mut param_map: HashMap<String, Vec<String>> = HashMap::new();
    let mut url_map: HashMap<String, LspUrl> = HashMap::new();

    ast.statement().visit_stmt(|stmt| {
        if let StmtP::Load(load) = &stmt.node {
            let module_path = &load.module.node;

            if let Ok(target_url) = ctx.resolve_load(module_path, current_uri, None) {
                // Note: store URL for *all* aliases, even if we fail to parse later; this
                // still enables go-to-definition to jump to the file.
                for LoadArgP { local, .. } in &load.args {
                    url_map.insert(local.node.ident.clone(), target_url.clone());
                }

                // For file URLs, try to parse the target module
                if let LspUrl::File(path) = &target_url {
                    if let Ok(contents) = std::fs::read_to_string(path) {
                        // Parse target module using same dialect semantics.
                        let mut dialect = starlark::syntax::Dialect::Extended;
                        dialect.enable_f_strings = true;
                        if let Ok(target_ast) = starlark::syntax::AstModule::parse(
                            path.to_string_lossy().as_ref(),
                            contents,
                            &dialect,
                        ) {
                            for LoadArgP { local, their, .. } in &load.args {
                                if let Some(params) =
                                    find_def_params(target_ast.statement(), &their.node)
                                {
                                    if !params.is_empty() {
                                        param_map.insert(local.node.ident.clone(), params);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    });

    (param_map, url_map)
}

/// Produce an LSP [`SignatureHelp`] value for the given AST and cursor position.
///
/// The implementation first searches for a `def` statement *within the current file*.
/// If no matching definition is found, it consults the surrounding [`Context`] to
/// resolve the symbol – this allows it to discover functions that were imported via
/// `load()` as well as built-in Starlark globals.
pub fn signature_help<T: LspContext>(
    ast: &AstModule,
    line: u32,
    character: u32,
    ctx: &T,
    current_uri: &LspUrl,
) -> SignatureHelp {
    let calls = calls_at_position(ast, line, character).unwrap_or_default();
    if calls.is_empty() {
        return SignatureHelp {
            signatures: vec![],
            active_signature: None,
            active_parameter: None,
        };
    }

    // Pick the innermost call (last in vector).
    let call = calls.last().unwrap();

    // Attempt to find parameter list for function.
    let mut params = find_def_params(ast.statement(), &call.function_name).unwrap_or_default();

    // ------------------------------------------------------------------
    // Fast path: if the Context already knows the parameter list (e.g. for a
    // ModuleLoader value) use that first.
    // ------------------------------------------------------------------
    // Note: We removed the get_signature call since it's not part of LspContext trait

    // ------------------------------------------------------------------
    // First try Context symbol index / built-ins.
    // ------------------------------------------------------------------
    if params.is_empty() {
        // Resolve the target URL for the symbol (if any).
        if let Ok(Some(target_url)) =
            ctx.get_url_for_global_symbol(current_uri, &call.function_name)
        {
            match &target_url {
                // User-defined Starlark file – parse its AST and look for `def`.
                LspUrl::File(path) => {
                    if let Ok(contents) = std::fs::read_to_string(path) {
                        // Parse using the same dialect settings as the main evaluator
                        let mut dialect = starlark::syntax::Dialect::Extended;
                        dialect.enable_f_strings = true;
                        if let Ok(target_ast) = starlark::syntax::AstModule::parse(
                            &path.to_string_lossy(),
                            contents,
                            &dialect,
                        ) {
                            params = find_def_params(target_ast.statement(), &call.function_name)
                                .unwrap_or_default();
                        }
                    }
                }
                // Built-in global – check if we have documentation
                LspUrl::Starlark(_) => {
                    if let Some(meta) = ctx.get_completion_meta(current_uri, &call.function_name) {
                        if let Some(doc) = meta.documentation {
                            params = parse_params_from_builtin_doc(&call.function_name, &doc);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // ------------------------------------------------------------------
    // Second fallback: scan `load()` statements in the current file to map
    // an alias to the originating module path, then parse that module to
    // discover the function definition.
    // ------------------------------------------------------------------
    if params.is_empty() {
        let (load_param_map, _url_map) = load_symbols_info(ast, ctx, current_uri);
        if let Some(p) = load_param_map.get(&call.function_name) {
            params = p.clone();
        }
    }

    let label = format!("{}({})", call.function_name, params.join(", "));

    let parameters: Vec<ParameterInformation> = params
        .iter()
        .map(|p| ParameterInformation {
            label: ParameterLabel::Simple(p.clone()),
            documentation: None,
        })
        .collect();

    let active_parameter = match &call.current_argument {
        CallArgument::Positional(i) => Some(*i as u32),
        CallArgument::Named(name) => params.iter().position(|p| p == name).map(|idx| idx as u32),
        CallArgument::None => None,
    };

    let sig_info = SignatureInformation {
        label,
        documentation: None,
        parameters: Some(parameters),
        active_parameter,
    };

    SignatureHelp {
        signatures: vec![sig_info],
        active_signature: Some(0),
        active_parameter,
    }
}

/// Attempt to extract the parameter names from the rendered documentation stub for a
/// built-in Starlark global. These docs are produced via `render_as_code()` and look
/// roughly like:
///
/// ```starlark
/// def len(x) -> int
/// ```
fn parse_params_from_builtin_doc(_name: &str, doc: &str) -> Vec<String> {
    // Find the first line that starts with "def" – this should contain the signature.
    for line in doc.lines() {
        let line = line.trim_start();
        if line.starts_with("def ") {
            // Strip leading "def" and function name upto the first opening paren.
            if let Some(start_idx) = line.find('(') {
                if let Some(end_idx) = line[start_idx + 1..].find(')') {
                    let params_str = &line[start_idx + 1..start_idx + 1 + end_idx];
                    return params_str
                        .split(',')
                        .map(|s| s.trim())
                        .filter(|s| !s.is_empty())
                        .map(|s| {
                            // Remove any leading * / ** as well as type annotations or defaults.
                            let s = s.trim_start_matches('*');
                            s.split(':')
                                .next()
                                .unwrap_or("")
                                .split('=')
                                .next()
                                .unwrap_or("")
                                .trim()
                                .to_string()
                        })
                        .collect();
                }
            }
        }
    }
    Vec::new()
}
