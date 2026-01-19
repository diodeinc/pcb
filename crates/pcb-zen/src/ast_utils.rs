//! AST utilities for traversing Starlark expressions and .zen file manipulation

use ignore::DirEntry;
use starlark_syntax::syntax::ast::{ArgumentP, ExprP};

/// Filter function that skips vendor directories
pub fn skip_vendor(entry: &DirEntry) -> bool {
    if entry.file_type().is_some_and(|ft| ft.is_dir()) {
        if let Some(name) = entry.file_name().to_str() {
            if name == "vendor" {
                return false;
            }
        }
    }
    true
}

/// Recursively visit all string literals in an expression tree.
///
/// The callback `f` is called for each unquoted string literal found.
/// This handles nested expressions including Dot (member access), Index,
/// Call arguments, If expressions, List, Tuple, Dict, and FString templates.
///
/// For f-strings, the format template string is passed to the callback with `{}`
/// markers in place of interpolated expressions. This allows extracting static
/// portions of the string for analysis (e.g., asset path detection).
pub fn visit_string_literals<F>(expr: &starlark_syntax::syntax::ast::AstExpr, f: &mut F)
where
    F: FnMut(&str, &starlark_syntax::syntax::ast::AstExpr),
{
    // Check if this is a string literal
    if let ExprP::Literal(lit) = &expr.node {
        let s = lit.to_string();
        if s.len() > 2 && (s.starts_with('"') || s.starts_with('\'')) {
            f(&s[1..s.len() - 1], expr);
        }
    }

    // Recurse into subexpressions
    match &expr.node {
        ExprP::FString(fstring) => {
            // Extract the format template string (with {} markers for interpolations)
            // This allows detecting static asset paths even in f-strings
            let template = &fstring.format.node;
            f(template, expr);

            // Also recurse into the interpolated expressions
            for e in &fstring.expressions {
                visit_string_literals(e, f);
            }
        }
        ExprP::Call(_, args) => {
            for arg in &args.args {
                let arg_expr = match &arg.node {
                    ArgumentP::Positional(e) | ArgumentP::Named(_, e) => e,
                    _ => continue,
                };
                visit_string_literals(arg_expr, f);
            }
        }
        ExprP::Dot(inner, _) => {
            visit_string_literals(inner, f);
        }
        ExprP::Index(pair) => {
            visit_string_literals(&pair.0, f);
            visit_string_literals(&pair.1, f);
        }
        ExprP::Index2(triple) => {
            visit_string_literals(&triple.0, f);
            visit_string_literals(&triple.1, f);
            visit_string_literals(&triple.2, f);
        }
        ExprP::If(if_box) => {
            let (a, b, c) = &**if_box;
            visit_string_literals(a, f);
            visit_string_literals(b, f);
            visit_string_literals(c, f);
        }
        ExprP::List(exprs) | ExprP::Tuple(exprs) => {
            for e in exprs {
                visit_string_literals(e, f);
            }
        }
        ExprP::Dict(pairs) => {
            for (k, v) in pairs {
                visit_string_literals(k, f);
                visit_string_literals(v, f);
            }
        }
        ExprP::Op(left, _, right) => {
            // Handle binary operations like string concatenation ("a" + "b")
            visit_string_literals(left, f);
            visit_string_literals(right, f);
        }
        _ => {}
    }
}

/// An edit to apply to source code: (start_line, start_col, end_line, end_col, replacement)
pub type SourceEdit = (usize, usize, usize, usize, String);

/// Apply edits to source lines. Edits are sorted and applied in reverse order to preserve offsets.
pub fn apply_edits(lines: &mut Vec<String>, mut edits: Vec<SourceEdit>) {
    if edits.is_empty() {
        return;
    }

    edits.sort_by(|a, b| (a.0, a.1, a.2, a.3).cmp(&(b.0, b.1, b.2, b.3)));

    for (start_line, start_col, end_line, end_col, replacement) in edits.into_iter().rev() {
        if start_line == end_line {
            if start_line >= lines.len() {
                continue;
            }
            let line = &mut lines[start_line];
            if start_col > line.len() || end_col > line.len() || end_col < start_col {
                continue;
            }
            let (pre, rest) = line.split_at(start_col);
            let (_, post) = rest.split_at(end_col - start_col);
            let mut new_line = String::with_capacity(pre.len() + replacement.len() + post.len());
            new_line.push_str(pre);
            new_line.push_str(&replacement);
            new_line.push_str(post);
            *line = new_line;
        } else {
            if start_line >= lines.len() || end_line >= lines.len() {
                continue;
            }
            let first_prefix =
                lines[start_line][..start_col.min(lines[start_line].len())].to_string();
            let last_suffix = lines[end_line][end_col.min(lines[end_line].len())..].to_string();
            lines.splice(
                start_line..=end_line,
                vec![format!("{}{}{}", first_prefix, replacement, last_suffix)],
            );
        }
    }
}
