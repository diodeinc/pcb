use anyhow::Result;
use pcb_zen::ast_utils::{SourceEdit, apply_edits, visit_string_literals};
use starlark::syntax::{AstModule, Dialect};
use starlark_syntax::syntax::ast::StmtP;
use starlark_syntax::syntax::module::AstModuleFields;
use std::path::{Path, PathBuf};

pub mod alias_expansion;
pub mod escape_paths;
pub mod local_url_paths;
pub mod manifest_v2;
pub mod path_correction;
pub mod workspace_paths;

/// Context passed to all codemods during migration
#[derive(Debug, Clone)]
pub struct MigrateContext {
    /// Absolute path to workspace root on disk
    pub workspace_root: PathBuf,
    /// Repository URL (e.g., "github.com/user/repo")
    pub repository: String,
    /// Subpath within the git repo if workspace is not at repo root (e.g., "hardware")
    pub repo_subpath: Option<PathBuf>,
}

pub trait Codemod {
    fn apply(&self, ctx: &MigrateContext, path: &Path, content: &str) -> Result<Option<String>>;
}

fn rewrite_strings(
    content: &str,
    mut rewrite: impl FnMut(&str) -> Option<String>,
) -> Option<String> {
    let mut dialect = Dialect::Extended;
    dialect.enable_f_strings = true;

    let ast = AstModule::parse("<memory>", content.to_owned(), &dialect).ok()?;
    let mut edits: Vec<SourceEdit> = Vec::new();

    ast.statement().visit_expr(|expr| {
        visit_string_literals(expr, &mut |s, lit_expr| {
            if let Some(updated) = rewrite(s) {
                let span = ast.codemap().resolve_span(lit_expr.span);
                edits.push((
                    span.begin.line,
                    span.begin.column,
                    span.end.line,
                    span.end.column,
                    format!("\"{}\"", updated),
                ));
            }
        });
    });

    for stmt in starlark_syntax::syntax::top_level_stmts::top_level_stmts(ast.statement()) {
        let StmtP::Load(load) = &stmt.node else {
            continue;
        };

        if let Some(updated) = rewrite(&load.module.node) {
            let span = ast.codemap().resolve_span(load.module.span);
            edits.push((
                span.begin.line,
                span.begin.column,
                span.end.line,
                span.end.column,
                format!("\"{}\"", updated),
            ));
        }
    }

    if edits.is_empty() {
        return None;
    }

    let mut lines: Vec<String> = content.split('\n').map(|s| s.to_string()).collect();
    apply_edits(&mut lines, edits);
    Some(lines.join("\n"))
}
