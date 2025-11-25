//! AST utilities for traversing Starlark expressions and .zen file manipulation

use anyhow::Result;
use ignore::WalkBuilder;
use starlark_syntax::syntax::ast::{ArgumentP, ExprP};
use std::path::{Path, PathBuf};

/// Recursively visit all string literals in an expression tree.
///
/// The callback `f` is called for each unquoted string literal found.
/// This handles nested expressions including Dot (member access), Index,
/// Call arguments, If expressions, List, Tuple, and Dict.
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
        _ => {}
    }
}

/// Collect all .zen files in a directory, respecting .gitignore
pub fn collect_zen_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();

    let walker = WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_exclude(true)
        .build();

    for entry in walker.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_file() && path.extension() == Some(std::ffi::OsStr::new("zen")) {
            files.push(path.to_path_buf());
        }
    }

    Ok(files)
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
