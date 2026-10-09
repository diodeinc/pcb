use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use pcb_layout::utils::discover_kicad_files;
use pcb_zen_core::DefaultFileProvider;
use pcb_zen_core::workspace::get_workspace_info;
use starlark::syntax::ast::{ArgumentP, AstLiteral, ExprP, StmtP};
use starlark::syntax::{AstModule, Dialect};
use starlark_syntax::syntax::top_level_stmts::top_level_stmts;

/// Rename each board's KiCad project after the board, as `pcb apply` names
/// new ones. Returns the renamed project files.
pub(super) fn migrate_kicad_projects(root: &Path) -> Result<Vec<(PathBuf, PathBuf)>> {
    let workspace = get_workspace_info(&DefaultFileProvider::new(), root)?;
    let mut renamed = Vec::new();
    for board in workspace.boards().values() {
        let zen = board.absolute_zen_path(&workspace.root);
        let Some((path, name)) = declared_project(&zen)? else {
            continue;
        };
        let directory = zen.parent().context("board file has no parent")?.join(path);
        let Some(files) = discover_kicad_files(&directory)? else {
            continue;
        };
        let next = files.rename(&name)?;
        if next.kicad_pro != files.kicad_pro {
            renamed.push((files.kicad_pro, next.kicad_pro));
        }
    }
    Ok(renamed)
}

/// The literal `path` and `name` of the board file's top-level `Board()`,
/// `Project()` or `Layout()` call; none when the board file is missing.
fn declared_project(zen: &Path) -> Result<Option<(String, String)>> {
    let source = match fs::read_to_string(zen) {
        Ok(source) => source,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("Failed to read {}", zen.display()));
        }
    };
    let mut dialect = Dialect::Extended;
    dialect.enable_f_strings = true;
    let ast = AstModule::parse(&zen.display().to_string(), source, &dialect)
        .map_err(|error| anyhow::anyhow!("{error}"))
        .with_context(|| format!("Failed to parse {}", zen.display()))?;
    Ok(top_level_stmts(ast.statement())
        .into_iter()
        .find_map(|statement| {
            let expr = match &statement.node {
                StmtP::Expression(expr) => expr,
                StmtP::Assign(assign) => &assign.rhs,
                _ => return None,
            };
            let ExprP::Call(function, args) = &expr.node else {
                return None;
            };
            let ExprP::Identifier(function) = &function.node else {
                return None;
            };
            if !matches!(function.node.ident.as_str(), "Board" | "Project" | "Layout") {
                return None;
            }
            let literal = |keys: &[&str]| {
                args.args.iter().find_map(|arg| match &arg.node {
                    ArgumentP::Named(key, value) if keys.contains(&key.node.as_str()) => {
                        match &value.node {
                            ExprP::Literal(AstLiteral::String(value)) => Some(value.node.clone()),
                            _ => None,
                        }
                    }
                    _ => None,
                })
            };
            Some((literal(&["path", "layout_path"])?, literal(&["name"])?))
        }))
}
