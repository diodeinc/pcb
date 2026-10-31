//! Resolver-backed call intelligence for Zener.
//!
//! A *callable* is what a call such as `Resistor(...)` or `Power(...)` invokes.
//! Its parameters come from the authoritative sources only:
//! - `Module()` aliases: the evaluated module's `io()`/`config()` signature
//!   (names, kinds, types, defaults, help); navigation ranges from the resolved
//!   module file's AST (the `P1` in `P1 = io(Net)` or the `"P1"` in `io("P1", …)`).
//! - Prelude/stdlib builtins (`Power`, `Net`, `Board`, …): the stdlib source
//!   that defines them (`X = builtin.net_type(…, field=…)`, `X = interface(…)`,
//!   or `def X(…)`).
//! - Local `def`s in the current file.
//!
//! Nothing is inferred from names, docs text or similarly named modules: an
//! unresolved callee yields no completion and no definition.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use starlark::codemap::{CodeMap, ResolvedPos, ResolvedSpan};
use starlark::syntax::ast::*;
use starlark::syntax::{AstModule, Dialect};
use starlark_syntax::syntax::module::AstModuleFields;

/// Calls that declare a module parameter.
const DECLARATION_CALLS: &[&str] = &["io", "input", "output", "config"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Decl {
    pub(crate) path: PathBuf,
    pub(crate) span: ResolvedSpan,
}

#[derive(Clone, Debug)]
pub(crate) struct Param {
    pub(crate) name: String,
    /// `io`, `config`, `implicit` (Module's own `name`/`properties`), `field` (builtin), `parameter` (def).
    pub(crate) kind: &'static str,
    pub(crate) type_name: Option<String>,
    pub(crate) required: bool,
    pub(crate) default: Option<String>,
    pub(crate) help: Option<String>,
    pub(crate) allowed: Option<Vec<String>>,
    pub(crate) decl: Option<Decl>,
}

#[derive(Clone, Debug)]
pub(crate) struct Callable {
    pub(crate) callee: String,
    /// `module`, `builtin` or `function`.
    pub(crate) kind: &'static str,
    /// The file that defines the callable (module file, stdlib file, current file).
    pub(crate) source: Option<PathBuf>,
    pub(crate) params: Vec<Param>,
}

impl Callable {
    pub(crate) fn param(&self, name: &str) -> Option<&Param> {
        self.params.iter().find(|p| p.name == name)
    }
}

/// Where the cursor is relative to the innermost enclosing call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Cursor {
    /// On a keyword key: `P1|=`.
    Key { name: String, span: ResolvedSpan },
    /// Where a keyword argument may start; `prefix` is the identifier typed so far.
    Slot {
        prefix: String,
        replace: Option<ResolvedSpan>,
    },
    /// Inside an argument's value; `keyword` names the argument when it is named.
    Value {
        keyword: Option<String>,
        positional: Option<usize>,
    },
    /// On the callee itself.
    Callee,
}

#[derive(Clone, Debug)]
pub(crate) struct CallSite {
    pub(crate) callee: String,
    /// Keyword arguments present in the call, except the one under the cursor.
    pub(crate) supplied: Vec<String>,
    pub(crate) cursor: Cursor,
}

pub(crate) fn dialect() -> Dialect {
    let mut d = Dialect::Extended;
    d.enable_f_strings = true;
    d
}

pub(crate) fn parse(path: &Path, contents: &str) -> Option<AstModule> {
    AstModule::parse(&path.to_string_lossy(), contents.to_string(), &dialect()).ok()
}

/// Parse the buffer as the user has it. While typing it often does not parse:
/// `Foo(` is retried with `)` at the cursor; `Foo(name = "x", P` with `=None)`
/// (or `=None` when the call is already closed, as auto-closing editors leave it),
/// because a bare positional after a keyword argument is a Starlark syntax error
/// (the typed prefix then reads as a keyword key, see `Cursor::Key`).
///
/// Each variant is also tried with the brackets still open at the end of the buffer
/// closed there, so an unfinished call elsewhere (e.g. the line being typed at the
/// end of the file) does not break navigation at the cursor.
#[cfg(test)]
pub(crate) fn parse_for_position(
    path: &Path,
    contents: &str,
    pos: ResolvedPos,
) -> Option<AstModule> {
    parse_at(path, contents, pos).map(|p| p.ast)
}

/// A buffer parsed for a cursor position. `pos` is the cursor in the parsed text;
/// spans from `ast` map back to the buffer with [`unshift_span`] and `shifts`.
pub(crate) struct ParsedAt {
    pub(crate) ast: AstModule,
    pub(crate) pos: ResolvedPos,
    pub(crate) shifts: Vec<(usize, usize)>,
}

/// Map a buffer position into text with `None` fills (inverse of [`unshift`]).
pub(crate) fn shift_forward(pos: ResolvedPos, shifts: &[(usize, usize)]) -> ResolvedPos {
    let mut column = pos.column;
    for &(_, at) in shifts.iter().filter(|(l, _)| *l == pos.line) {
        if column >= at {
            column += 4;
        }
    }
    ResolvedPos {
        line: pos.line,
        column,
    }
}

pub(crate) fn parse_at(path: &Path, contents: &str, pos: ResolvedPos) -> Option<ParsedAt> {
    let unshifted = |ast| {
        Some(ParsedAt {
            ast,
            pos,
            shifts: vec![],
        })
    };
    let offset = byte_offset(contents, pos)?;
    // `=None` alone covers a prefix typed inside an already-closed call (editors
    // auto-close `(`): `Foo(a = 1, P|)` → `Foo(a = 1, P=None)`.
    for insert in ["", ")", "=None)", "=None"] {
        let mut repaired = String::with_capacity(contents.len() + insert.len());
        repaired.push_str(&contents[..offset]);
        repaired.push_str(insert);
        repaired.push_str(&contents[offset..]);
        if let Some(ast) = parse(path, &repaired) {
            return unshifted(ast);
        }
        if let Some(ast) = close_at_end(path, &repaired) {
            return unshifted(ast);
        }
        // Fills shift columns after them; the cursor moves forward with them and
        // callers map spans back with `unshift_span`.
        let (filled, shifts) = fill_missing_values_except(&repaired, None);
        if filled != repaired
            && let Some(ast) = parse(path, &filled).or_else(|| close_at_end(path, &filled))
        {
            let pos = shift_forward(pos, &shifts);
            return Some(ParsedAt { ast, pos, shifts });
        }
    }
    None
}

/// `key = )` / `key = ,` (a keyword still waiting for its value, e.g. right after
/// accepting a completion) gets `None` at the closer. Spans before the insertion
/// are unchanged. Strings, comments and `==`/`<=`/`>=`/`!=` are left alone.
#[cfg(test)]
pub(crate) fn fill_missing_values(text: &str) -> String {
    fill_missing_values_except(text, None).0
}

/// As [`fill_missing_values`], skipping inserts at byte offsets in `protect` (`start..end`).
/// Also returns where `None` was inserted, as (line, column) in the *filled* text.
pub(crate) fn fill_missing_values_except(
    text: &str,
    protect: Option<(usize, usize)>,
) -> (String, Vec<(usize, usize)>) {
    let b = text.as_bytes();
    let mut inserts = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'#' => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            q @ (b'"' | b'\'') => {
                let triple = b.get(i + 1) == Some(&q) && b.get(i + 2) == Some(&q);
                i += if triple { 3 } else { 1 };
                while i < b.len() {
                    if b[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if triple && b[i] == q && b.get(i + 1) == Some(&q) && b.get(i + 2) == Some(&q) {
                        i += 3;
                        break;
                    }
                    if !triple && (b[i] == q || b[i] == b'\n') {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                continue;
            }
            b'=' if b.get(i + 1) != Some(&b'=')
                && !matches!(
                    i.checked_sub(1).map(|p| b[p]),
                    Some(b'=' | b'<' | b'>' | b'!')
                ) =>
            {
                let mut j = i + 1;
                while j < b.len() && matches!(b[j], b' ' | b'\t' | b'\n' | b'\r') {
                    j += 1;
                }
                if j < b.len() && matches!(b[j], b',' | b')' | b']' | b'}') {
                    inserts.push(j);
                }
            }
            _ => {}
        }
        i += 1;
    }
    let inserts: Vec<usize> = inserts
        .into_iter()
        .filter(|at| !protect.is_some_and(|(s, e)| (s..e).contains(at)))
        .collect();
    let mut out = text.to_string();
    for at in inserts.iter().rev() {
        out.insert_str(*at, "None");
    }
    let shifts = inserts
        .iter()
        .enumerate()
        .map(|(i, at)| {
            let filled_at = at + 4 * i;
            let before = &out[..filled_at];
            let line = before.matches('\n').count();
            (
                line,
                before[before.rfind('\n').map(|p| p + 1).unwrap_or(0)..]
                    .chars()
                    .count(),
            )
        })
        .collect();
    (out, shifts)
}

/// Map a position in filled text back to the original buffer.
pub(crate) fn unshift(pos: ResolvedPos, shifts: &[(usize, usize)]) -> ResolvedPos {
    let mut column = pos.column;
    let mut delta = 0;
    for &(line, at) in shifts.iter().filter(|(l, _)| *l == pos.line) {
        if pos.column >= at + 4 {
            delta += 4;
        } else if pos.column > at {
            column = at;
        }
        let _ = line;
    }
    ResolvedPos {
        line: pos.line,
        column: column - delta.min(column),
    }
}

pub(crate) fn unshift_span(span: ResolvedSpan, shifts: &[(usize, usize)]) -> ResolvedSpan {
    ResolvedSpan {
        begin: unshift(span.begin, shifts),
        end: unshift(span.end, shifts),
    }
}

/// Close what is left open at the end of the buffer. A dangling name after keyword
/// arguments (`Foo(a = 1, P`) only parses as a keyword (`P=None`), and a dangling
/// `x = ` needs a value, so those completions are tried before the closers.
fn close_at_end(path: &Path, text: &str) -> Option<AstModule> {
    ["", "=None", "None"].into_iter().find_map(|suffix| {
        let t = format!("{text}{suffix}");
        let closers = unclosed_brackets(&t);
        (!closers.is_empty() || !suffix.is_empty())
            .then(|| parse(path, &format!("{t}{closers}")))
            .flatten()
    })
}

/// Parse a whole buffer, closing brackets left open at its end if needed.
/// Parse a whole buffer leniently; also returns the `None` fills to map spans back
/// with [`unshift_span`] (empty unless values had to be filled).
pub(crate) fn parse_lenient(
    path: &Path,
    contents: &str,
) -> Option<(AstModule, Vec<(usize, usize)>)> {
    if let Some(ast) = parse(path, contents).or_else(|| close_at_end(path, contents)) {
        return Some((ast, vec![]));
    }
    let (filled, shifts) = fill_missing_values_except(contents, None);
    parse(path, &filled)
        .or_else(|| close_at_end(path, &filled))
        .map(|ast| (ast, shifts))
}

/// Closing brackets for those still open at the end of `text`, innermost first.
/// Strings (incl. triple-quoted) and `#` comments are skipped.
pub(crate) fn unclosed_brackets(text: &str) -> String {
    let b = text.as_bytes();
    let mut stack = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'#' => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            q @ (b'"' | b'\'') => {
                let triple = b.get(i + 1) == Some(&q) && b.get(i + 2) == Some(&q);
                i += if triple { 3 } else { 1 };
                while i < b.len() {
                    if b[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if triple && b[i] == q && b.get(i + 1) == Some(&q) && b.get(i + 2) == Some(&q) {
                        i += 2;
                        break;
                    }
                    if !triple && (b[i] == q || b[i] == b'\n') {
                        break;
                    }
                    i += 1;
                }
            }
            b'(' => stack.push(')'),
            b'[' => stack.push(']'),
            b'{' => stack.push('}'),
            b')' | b']' | b'}' => {
                stack.pop();
            }
            _ => {}
        }
        i += 1;
    }
    stack.iter().rev().collect()
}

/// True when the buffer has `=` after the cursor (skipping spaces), i.e. the
/// identifier under the cursor is a finished keyword key, not one being typed.
pub(crate) fn key_is_complete(contents: &str, pos: ResolvedPos) -> bool {
    byte_offset(contents, pos)
        .map(|o| {
            contents[o..]
                .trim_start_matches([' ', '\t'])
                .starts_with('=')
                && !contents[o..]
                    .trim_start_matches([' ', '\t'])
                    .starts_with("==")
        })
        .unwrap_or(false)
}

/// Byte offset of a (line, character) position; characters are Unicode scalar values.
pub(crate) fn byte_offset(contents: &str, pos: ResolvedPos) -> Option<usize> {
    let mut line_start = 0usize;
    for _ in 0..pos.line {
        line_start += contents[line_start..].find('\n')? + 1;
    }
    let line = &contents[line_start..];
    let line = &line[..line.find('\n').unwrap_or(line.len())];
    let within = line
        .char_indices()
        .nth(pos.column)
        .map(|(i, _)| i)
        .unwrap_or(line.len());
    Some(line_start + within)
}

/// `begin <= pos <= end` (inclusive end: a cursor right after a token is "on" it).
fn touches(span: ResolvedSpan, pos: ResolvedPos) -> bool {
    (span.begin.line, span.begin.column) <= (pos.line, pos.column)
        && (pos.line, pos.column) <= (span.end.line, span.end.column)
}

/// Strictly inside the call's parentheses: after the callee, before or at `)`.
fn call_site_of<P: AstPayload>(
    codemap: &CodeMap,
    target: &AstExprP<P>,
    args: &CallArgsP<P>,
    pos: ResolvedPos,
) -> Option<CallSite> {
    let ExprP::Identifier(ident) = &target.node else {
        return None;
    };
    let callee = ident.node.ident.clone();
    if touches(codemap.resolve_span(target.span), pos) {
        return Some(CallSite {
            callee,
            supplied: vec![],
            cursor: Cursor::Callee,
        });
    }
    let mut cursor = Cursor::Slot {
        prefix: String::new(),
        replace: None,
    };
    let mut at_index = None;
    for (index, arg) in args.args.iter().enumerate() {
        if !touches(codemap.resolve_span(arg.span), pos) {
            continue;
        }
        at_index = Some(index);
        cursor = match &arg.node {
            ArgumentP::Named(name, _) => {
                let key = codemap.resolve_span(name.span);
                if touches(key, pos) {
                    Cursor::Key {
                        name: name.node.clone(),
                        span: key,
                    }
                } else {
                    Cursor::Value {
                        keyword: Some(name.node.clone()),
                        positional: None,
                    }
                }
            }
            ArgumentP::Positional(expr) => match &expr.node {
                ExprP::Identifier(id) => {
                    let span = codemap.resolve_span(expr.span);
                    let typed = pos.column.saturating_sub(span.begin.column);
                    let prefix: String = id.node.ident.chars().take(typed).collect();
                    Cursor::Slot {
                        prefix,
                        replace: Some(span),
                    }
                }
                _ => Cursor::Value {
                    keyword: None,
                    positional: Some(
                        args.args[..index]
                            .iter()
                            .filter(|a| matches!(a.node, ArgumentP::Positional(_)))
                            .count(),
                    ),
                },
            },
            _ => Cursor::Value {
                keyword: None,
                positional: None,
            },
        };
        break;
    }
    let supplied = args
        .args
        .iter()
        .enumerate()
        .filter(|(i, _)| Some(*i) != at_index)
        .filter_map(|(_, a)| match &a.node {
            ArgumentP::Named(name, _) => Some(name.node.clone()),
            _ => None,
        })
        .collect();
    Some(CallSite {
        callee,
        supplied,
        cursor,
    })
}

fn innermost_call<P: AstPayload>(
    expr: &AstExprP<P>,
    codemap: &CodeMap,
    pos: ResolvedPos,
    found: &mut Option<CallSite>,
) {
    if !touches(codemap.resolve_span(expr.span), pos) {
        return;
    }
    if let ExprP::Call(target, args) = &expr.node
        && let Some(site) = call_site_of(codemap, target, args, pos)
    {
        *found = Some(site);
    }
    // Children are visited after the parent so the innermost call wins.
    expr.visit_expr(|child| innermost_call(child, codemap, pos, found));
}

/// The innermost call around `pos`, if any.
pub(crate) fn call_at(ast: &AstModule, pos: ResolvedPos) -> Option<CallSite> {
    let mut found = None;
    ast.statement()
        .visit_expr(|expr| innermost_call(expr, ast.codemap(), pos, &mut found));
    found
}

fn string_literal<P: AstPayload>(expr: &AstExprP<P>) -> Option<&AstString> {
    match &expr.node {
        ExprP::Literal(AstLiteral::String(s)) => Some(s),
        _ => None,
    }
}

fn callee_name<P: AstPayload>(expr: &AstExprP<P>) -> Option<&str> {
    match &expr.node {
        ExprP::Call(target, _) => match &target.node {
            ExprP::Identifier(id) => Some(id.node.ident.as_str()),
            ExprP::Dot(_, attr) => Some(attr.node.as_str()),
            _ => None,
        },
        _ => None,
    }
}

/// Every statement of a module, including those nested in `if`/`for` blocks
/// (not `def` bodies: those are not module-level declarations).
pub(crate) fn module_statements<'a, P: AstPayload>(
    stmt: &'a AstStmtP<P>,
    out: &mut Vec<&'a AstStmtP<P>>,
) {
    match &stmt.node {
        StmtP::Statements(xs) => xs.iter().for_each(|x| module_statements(x, out)),
        StmtP::If(_, body) => module_statements(body, out),
        StmtP::IfElse(_, bodies) => {
            module_statements(&bodies.0, out);
            module_statements(&bodies.1, out);
        }
        StmtP::For(f) => module_statements(&f.body, out),
        _ => out.push(stmt),
    }
}

/// Spans naming each `io()`/`config()` declaration in a module file: the
/// string in `io("P1", …)`, else the assigned identifier in `P1 = io(…)`.
pub(crate) fn declaration_spans(ast: &AstModule) -> HashMap<String, ResolvedSpan> {
    let codemap = ast.codemap();
    let mut out = HashMap::new();
    let mut stmts = Vec::new();
    module_statements(ast.statement(), &mut stmts);
    for stmt in stmts {
        let (lhs, call) = match &stmt.node {
            StmtP::Assign(a) => (Some(&a.lhs), &a.rhs),
            StmtP::Expression(e) => (None, e),
            _ => continue,
        };
        let ExprP::Call(target, args) = &call.node else {
            continue;
        };
        let ExprP::Identifier(id) = &target.node else {
            continue;
        };
        if !DECLARATION_CALLS.contains(&id.node.ident.as_str()) {
            continue;
        }
        let named = args.args.first().and_then(|a| match &a.node {
            ArgumentP::Positional(e) => string_literal(e),
            _ => None,
        });
        if let Some(s) = named {
            out.entry(s.node.clone())
                .or_insert(codemap.resolve_span(s.span));
        } else if let Some(lhs) = lhs
            && let AssignTargetP::Identifier(ident) = &lhs.node
        {
            out.entry(ident.node.ident.clone())
                .or_insert(codemap.resolve_span(ident.span));
        }
    }
    out
}

fn def_params<P: AstPayload>(codemap: &CodeMap, path: &Path, def: &DefP<P>) -> Vec<Param> {
    def.params
        .iter()
        .filter_map(|p| match &p.node {
            ParameterP::Normal(ident, ty, default) => Some(Param {
                name: ident.node.ident.clone(),
                kind: "parameter",
                type_name: ty.as_ref().map(|t| codemap.source_span(t.span).to_string()),
                required: default.is_none(),
                default: default
                    .as_ref()
                    .map(|d| codemap.source_span(d.span).to_string()),
                help: None,
                allowed: None,
                decl: Some(Decl {
                    path: path.to_path_buf(),
                    span: codemap.resolve_span(ident.span),
                }),
            }),
            _ => None,
        })
        .collect()
}

/// A `def name(...)` in `ast`.
pub(crate) fn local_def(ast: &AstModule, path: &Path, name: &str) -> Option<Callable> {
    let mut stmts = Vec::new();
    module_statements(ast.statement(), &mut stmts);
    stmts.into_iter().find_map(|s| match &s.node {
        StmtP::Def(def) if def.name.node.ident == name => Some(Callable {
            callee: name.to_string(),
            kind: "function",
            source: Some(path.to_path_buf()),
            params: def_params(ast.codemap(), path, def),
        }),
        _ => None,
    })
}

/// A builtin defined in stdlib source: `name = <factory>(…, key = field(T, default = …), …)` or `def name(…)`.
pub(crate) fn stdlib_callable(path: &Path, contents: &str, name: &str) -> Option<Callable> {
    let ast = parse(path, contents)?;
    if let Some(def) = local_def(&ast, path, name) {
        return Some(Callable {
            kind: "builtin",
            ..def
        });
    }
    let codemap = ast.codemap();
    let mut stmts = Vec::new();
    module_statements(ast.statement(), &mut stmts);
    for stmt in stmts {
        let StmtP::Assign(assign) = &stmt.node else {
            continue;
        };
        let AssignTargetP::Identifier(ident) = &assign.lhs.node else {
            continue;
        };
        if ident.node.ident != name {
            continue;
        }
        let ExprP::Call(_, args) = &assign.rhs.node else {
            return None;
        };
        let params = args
            .args
            .iter()
            .filter_map(|a| match &a.node {
                ArgumentP::Named(key, value) => {
                    let (type_name, default) = match &value.node {
                        ExprP::Call(f, field_args) if matches!(&f.node, ExprP::Identifier(i) if i.node.ident == "field") => {
                            let ty = field_args.args.iter().find_map(|x| match &x.node {
                                ArgumentP::Positional(e) => Some(codemap.source_span(e.span).to_string()),
                                _ => None,
                            });
                            let default = field_args.args.iter().find_map(|x| match &x.node {
                                ArgumentP::Named(k, e) if k.node == "default" => Some(codemap.source_span(e.span).to_string()),
                                _ => None,
                            });
                            (ty, default)
                        }
                        _ => (Some(codemap.source_span(value.span).to_string()), None),
                    };
                    Some(Param {
                        name: key.node.clone(),
                        kind: "field",
                        type_name,
                        required: false,
                        default,
                        help: None,
                        allowed: None,
                        decl: Some(Decl { path: path.to_path_buf(), span: codemap.resolve_span(key.span) }),
                    })
                }
                _ => None,
            })
            .collect();
        return Some(Callable {
            callee: name.to_string(),
            kind: "builtin",
            source: Some(path.to_path_buf()),
            params,
        });
    }
    None
}

// ---------------------------------------------------------------------------
// Presentation
// ---------------------------------------------------------------------------

fn display_path(path: &Path) -> String {
    path.file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
}

/// One-line summary: `io · Net · required`.
pub(crate) fn param_detail(p: &Param) -> String {
    let mut parts = vec![p.kind.to_string()];
    if let Some(t) = &p.type_name {
        parts.push(t.clone());
    }
    parts.push(if p.required {
        "required".to_string()
    } else {
        "optional".to_string()
    });
    parts.join(" · ")
}

/// Markdown card for a parameter, with its provenance.
pub(crate) fn param_markdown(c: &Callable, p: &Param) -> String {
    let mut md = format!(
        "**{}** — `{}` parameter of `{}`\n\n`{}`",
        p.name,
        p.kind,
        c.callee,
        param_detail(p)
    );
    if let Some(help) = &p.help {
        md.push_str(&format!("\n\n{help}"));
    }
    if let Some(d) = &p.default {
        md.push_str(&format!("\n\nDefault: `{d}`"));
    }
    if let Some(a) = &p.allowed {
        md.push_str(&format!(
            "\n\nAllowed: {}",
            a.iter()
                .map(|v| format!("`{v}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    match &p.decl {
        Some(d) => md.push_str(&format!(
            "\n\nDeclared in `{}` line {}",
            display_path(&d.path),
            d.span.begin.line + 1
        )),
        None => {
            if let Some(s) = &c.source {
                md.push_str(&format!("\n\nFrom `{}`", display_path(s)));
            }
        }
    }
    md
}

/// Module aliases bound in a file: `X = Module("spec")` → (X, spec).
pub(crate) fn module_aliases(ast: &AstModule) -> Vec<(String, String)> {
    let mut stmts = Vec::new();
    module_statements(ast.statement(), &mut stmts);
    stmts
        .into_iter()
        .filter_map(|s| match &s.node {
            StmtP::Assign(a) => match (&a.lhs.node, &a.rhs.node) {
                (AssignTargetP::Identifier(id), ExprP::Call(target, args))
                    if matches!(&target.node, ExprP::Identifier(t) if t.node.ident == "Module") =>
                {
                    args.args.first().and_then(|x| match &x.node {
                        ArgumentP::Positional(e) => string_literal(e).map(|s| (id.node.ident.clone(), s.node.clone())),
                        _ => None,
                    })
                }
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// Names a file binds at module level (assignments, defs, loads): these shadow prelude builtins.
pub(crate) fn bound_names(ast: &AstModule) -> HashSet<String> {
    let mut stmts = Vec::new();
    module_statements(ast.statement(), &mut stmts);
    let mut out = HashSet::new();
    for s in stmts {
        match &s.node {
            StmtP::Assign(a) => {
                if let AssignTargetP::Identifier(id) = &a.lhs.node {
                    out.insert(id.node.ident.clone());
                }
            }
            StmtP::Def(d) => {
                out.insert(d.name.node.ident.clone());
            }
            StmtP::Load(l) => {
                for arg in &l.args {
                    out.insert(arg.local.node.ident.clone());
                }
            }
            _ => {}
        }
    }
    out
}

/// Keyword-argument keys `key=` in calls to any of `callees` across a file.
pub(crate) fn keyword_uses(
    ast: &AstModule,
    callees: &HashSet<String>,
    key: &str,
) -> Vec<ResolvedSpan> {
    fn walk<P: AstPayload>(
        e: &AstExprP<P>,
        codemap: &CodeMap,
        callees: &HashSet<String>,
        key: &str,
        out: &mut Vec<ResolvedSpan>,
    ) {
        if let ExprP::Call(target, args) = &e.node
            && let ExprP::Identifier(id) = &target.node
            && callees.contains(&id.node.ident)
        {
            for a in &args.args {
                if let ArgumentP::Named(name, _) = &a.node
                    && name.node == key
                {
                    out.push(codemap.resolve_span(name.span));
                }
            }
        }
        e.visit_expr(|c| walk(c, codemap, callees, key, out));
    }
    let mut out = Vec::new();
    ast.statement()
        .visit_expr(|e| walk(e, ast.codemap(), callees, key, &mut out));
    out
}

/// One document symbol: (name, kind, detail, statement span, selection span).
pub(crate) struct Outline {
    pub(crate) name: String,
    pub(crate) kind: lsp_types::SymbolKind,
    pub(crate) detail: Option<String>,
    pub(crate) range: ResolvedSpan,
    pub(crate) selection: ResolvedSpan,
}

/// Module outline: `load()` imports, Module aliases, `io()`/`config()` declarations,
/// defs, other bindings, and instances (`Foo(name = "X", …)`).
pub(crate) fn outline(ast: &AstModule) -> Vec<Outline> {
    use lsp_types::SymbolKind as K;
    let codemap = ast.codemap();
    let mut stmts = Vec::new();
    module_statements(ast.statement(), &mut stmts);
    let mut out = Vec::new();
    for s in stmts {
        let range = codemap.resolve_span(s.span);
        match &s.node {
            StmtP::Load(l) => {
                for arg in &l.args {
                    out.push(Outline {
                        name: arg.local.node.ident.clone(),
                        kind: K::PACKAGE,
                        detail: Some(format!("load(\"{}\")", l.module.node)),
                        range,
                        selection: codemap.resolve_span(arg.local.span),
                    });
                }
            }
            StmtP::Def(d) => out.push(Outline {
                name: d.name.node.ident.clone(),
                kind: K::FUNCTION,
                detail: Some("def".to_string()),
                range,
                selection: codemap.resolve_span(d.name.span),
            }),
            StmtP::Assign(a) => {
                let AssignTargetP::Identifier(id) = &a.lhs.node else {
                    continue;
                };
                let callee = callee_name(&a.rhs);
                let (kind, detail) = match callee {
                    Some("Module") => {
                        (K::MODULE, Some(codemap.source_span(a.rhs.span).to_string()))
                    }
                    Some("io" | "input" | "output") => (K::FIELD, Some("io".to_string())),
                    Some("config") => (K::PROPERTY, Some("config".to_string())),
                    Some(c) => (K::VARIABLE, Some(format!("{c}(…)"))),
                    None => (K::VARIABLE, None),
                };
                out.push(Outline {
                    name: id.node.ident.clone(),
                    kind,
                    detail,
                    range,
                    selection: codemap.resolve_span(id.span),
                });
            }
            StmtP::Expression(e) => {
                let ExprP::Call(target, args) = &e.node else {
                    continue;
                };
                let ExprP::Identifier(callee) = &target.node else {
                    continue;
                };
                let name_arg = args.args.iter().find_map(|a| match &a.node {
                    ArgumentP::Named(k, v) if k.node == "name" => string_literal(v),
                    _ => None,
                });
                let first_str = args.args.first().and_then(|a| match &a.node {
                    ArgumentP::Positional(v) => string_literal(v),
                    _ => None,
                });
                if let Some(n) = name_arg {
                    out.push(Outline {
                        name: n.node.clone(),
                        kind: K::OBJECT,
                        detail: Some(callee.node.ident.clone()),
                        range,
                        selection: codemap.resolve_span(n.span),
                    });
                } else if DECLARATION_CALLS.contains(&callee.node.ident.as_str())
                    && let Some(n) = first_str
                {
                    out.push(Outline {
                        name: n.node.clone(),
                        kind: if callee.node.ident == "config" {
                            K::PROPERTY
                        } else {
                            K::FIELD
                        },
                        detail: Some(callee.node.ident.clone()),
                        range,
                        selection: codemap.resolve_span(n.span),
                    });
                }
            }
            _ => {}
        }
    }
    out
}

/// `load()` statements: (module spec, [(local name, exported name)]).
pub(crate) fn loads(ast: &AstModule) -> Vec<(String, Vec<(String, String)>)> {
    let mut stmts = Vec::new();
    module_statements(ast.statement(), &mut stmts);
    stmts
        .into_iter()
        .filter_map(|s| match &s.node {
            StmtP::Load(l) => Some((
                l.module.node.clone(),
                l.args
                    .iter()
                    .map(|a| (a.local.node.ident.clone(), a.their.node.clone()))
                    .collect(),
            )),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(line: usize, column: usize) -> ResolvedPos {
        ResolvedPos { line, column }
    }

    #[test]
    fn call_site_classifies_key_value_slot_and_prefix() {
        let src = "Foo(P, name = \"x\", P1 = a)\n";
        let ast = parse(Path::new("t.zen"), src).unwrap();
        let key = call_at(&ast, pos(0, 20)).unwrap();
        assert!(matches!(key.cursor, Cursor::Key { ref name, .. } if name == "P1"));
        let value = call_at(&ast, pos(0, 24)).unwrap();
        assert_eq!(
            value.cursor,
            Cursor::Value {
                keyword: Some("P1".into()),
                positional: None
            }
        );
        let slot = call_at(&ast, pos(0, 5)).unwrap();
        assert!(matches!(slot.cursor, Cursor::Slot { ref prefix, .. } if prefix == "P"));
        assert_eq!(slot.supplied, vec!["name".to_string(), "P1".to_string()]);
    }

    #[test]
    fn a_prefix_typed_after_keyword_arguments_is_completable() {
        let src = "Foo(name = \"x\", P";
        let ast = parse_for_position(Path::new("t.zen"), src, pos(0, 17)).unwrap();
        let site = call_at(&ast, pos(0, 17)).unwrap();
        // A bare positional after keyword arguments is a syntax error, so the `)`
        // repair fails and `=None)` yields a key still being typed; either cursor
        // kind completes the same way.
        assert!(
            matches!(site.cursor, Cursor::Slot { ref prefix, .. } if prefix == "P")
                || matches!(site.cursor, Cursor::Key { ref name, .. } if name == "P"),
            "{:?}",
            site.cursor
        );
        assert!(!key_is_complete(src, pos(0, 17)));
        assert!(key_is_complete("Foo(P1 = a)", pos(0, 6)));
    }

    #[test]
    fn unclosed_call_is_repaired_at_the_cursor() {
        let src = "x = 1\nFoo(";
        let ast = parse_for_position(Path::new("t.zen"), src, pos(1, 4)).unwrap();
        let site = call_at(&ast, pos(1, 4)).unwrap();
        assert_eq!(site.callee, "Foo");
        assert!(matches!(site.cursor, Cursor::Slot { .. }));
    }

    #[test]
    fn fills_report_shifts_that_map_spans_back() {
        let (filled, shifts) = fill_missing_values_except("F(a=, kk=1, P=2)", None);
        assert_eq!(filled, "F(a=None, kk=1, P=2)");
        assert_eq!(shifts, vec![(0, 4)]);
        assert_eq!(
            unshift(pos(0, 16), &shifts),
            pos(0, 12),
            "P moves back to its original column"
        );
        assert_eq!(unshift(pos(0, 2), &shifts), pos(0, 2));
        assert_eq!(
            shift_forward(pos(0, 12), &shifts),
            pos(0, 16),
            "and forward again"
        );
        let (protected, none) = fill_missing_values_except("F(a=, P=2)", Some((0, 9)));
        assert_eq!((protected.as_str(), none.len()), ("F(a=, P=2)", 0));
    }

    #[test]
    fn missing_keyword_values_are_filled_without_touching_comparisons_or_strings() {
        assert_eq!(
            fill_missing_values("Foo(a = 1, P1 = )"),
            "Foo(a = 1, P1 = None)"
        );
        assert_eq!(fill_missing_values("Foo(P1=,P2=)"), "Foo(P1=None,P2=None)");
        assert_eq!(
            fill_missing_values("x = a == b\ny = \"k = )\" # z = )"),
            "x = a == b\ny = \"k = )\" # z = )"
        );
    }

    #[test]
    fn unclosed_brackets_ignore_strings_and_comments() {
        assert_eq!(unclosed_brackets("Foo(a, [1, {"), "}])");
        assert_eq!(
            unclosed_brackets("Foo(\"(\", '[') # (\nBar(\"\"\"(\"\"\""),
            ")"
        );
        assert_eq!(unclosed_brackets("x = 1\n"), "");
    }

    #[test]
    fn declaration_spans_cover_both_io_forms() {
        let src = "P1 = io(Net)\nio(\"EN\", Net)\nvalue = config(int, default = 1)\n";
        let ast = parse(Path::new("m.zen"), src).unwrap();
        let spans = declaration_spans(&ast);
        assert_eq!(spans["P1"].begin, pos(0, 0));
        assert_eq!(spans["EN"].begin, pos(1, 3));
        assert_eq!(spans["value"].begin, pos(2, 0));
    }

    #[test]
    fn stdlib_fields_and_defs_become_parameters() {
        let src = "Power = builtin.net_type(\"Power\", symbol = Symbol, voltage = field(Voltage | None, default = None))\ndef Board(name, layers = 2):\n    pass\n";
        let p = Path::new("interfaces.zen");
        let power = stdlib_callable(p, src, "Power").unwrap();
        let v = power.param("voltage").unwrap();
        assert_eq!(
            (v.type_name.as_deref(), v.default.as_deref()),
            (Some("Voltage | None"), Some("None"))
        );
        let board = stdlib_callable(p, src, "Board").unwrap();
        assert!(board.param("name").unwrap().required && !board.param("layers").unwrap().required);
    }
}
