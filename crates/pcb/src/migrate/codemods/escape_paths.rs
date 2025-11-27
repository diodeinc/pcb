use anyhow::{Context, Result};
use pcb_zen::ast_utils::{apply_edits, visit_string_literals, SourceEdit};
use starlark::syntax::{AstModule, Dialect};
use starlark_syntax::syntax::ast::StmtP;
use starlark_syntax::syntax::module::AstModuleFields;
use std::path::{Path, PathBuf};

use super::{Codemod, MigrateContext};

/// Convert cross-package relative paths to URLs in .zen files
pub struct EscapePaths;

impl Codemod for EscapePaths {
    fn apply(
        &self,
        ctx: &MigrateContext,
        zen_file: &Path,
        content: &str,
    ) -> Result<Option<String>> {
        let package_root = find_package_root(zen_file, &ctx.workspace_root);
        let repo_subpath = ctx.repo_subpath.as_ref().map(|p| p.to_string_lossy());

        convert_file(
            zen_file,
            content,
            &package_root,
            &ctx.workspace_root,
            &ctx.repository,
            repo_subpath.as_deref(),
        )
    }
}

/// Find the package root (nearest pcb.toml) for a .zen file
fn find_package_root(zen_file: &Path, workspace_root: &Path) -> PathBuf {
    let mut current = zen_file.parent().unwrap_or(workspace_root);
    while current != workspace_root && current.starts_with(workspace_root) {
        if current.join("pcb.toml").exists() {
            return current.to_path_buf();
        }
        current = match current.parent() {
            Some(p) => p,
            None => break,
        };
    }
    workspace_root.to_path_buf()
}

/// Check if a resolved path escapes the package boundary
fn escapes_package(resolved_path: &Path, package_root: &Path) -> bool {
    !resolved_path.starts_with(package_root)
}

/// Try to convert a path to a URL, returns None if no conversion needed
fn try_convert_path(
    path_str: &str,
    zen_dir: &Path,
    package_root: &Path,
    workspace_root: &Path,
    repository: &str,
    workspace_path: Option<&str>,
) -> Option<String> {
    // Skip non-.zen files, aliases, and already-converted URLs
    if !path_str.ends_with(".zen")
        || path_str.starts_with('@')
        || path_str.starts_with("github.com/")
        || path_str.starts_with("gitlab.com/")
    {
        return None;
    }

    // Resolve the path
    let resolved = if let Some(rest) = path_str.strip_prefix("//") {
        // Workspace-relative path: //common/foo.zen -> workspace_root/common/foo.zen
        workspace_root.join(rest)
    } else {
        // Relative path: ../foo.zen -> resolved relative to zen_dir
        zen_dir.join(path_str)
    };

    let resolved = resolved
        .canonicalize()
        .unwrap_or_else(|_| normalize_path(&resolved));

    // Only convert if it escapes the package and stays within workspace
    if !escapes_package(&resolved, package_root) || !resolved.starts_with(workspace_root) {
        return None;
    }

    build_url(&resolved, workspace_root, repository, workspace_path)
}

/// Build URL from resolved path
fn build_url(
    resolved_path: &Path,
    workspace_root: &Path,
    repository: &str,
    workspace_path: Option<&str>,
) -> Option<String> {
    let rel_to_workspace = resolved_path.strip_prefix(workspace_root).ok()?;
    let rel_str = rel_to_workspace.to_string_lossy().replace('\\', "/");

    Some(match workspace_path {
        Some(ws_path) => format!("{}/{}/{}", repository, ws_path, rel_str),
        None => format!("{}/{}", repository, rel_str),
    })
}

/// Convert cross-package paths in a single file
fn convert_file(
    zen_file: &Path,
    content: &str,
    package_root: &Path,
    workspace_root: &Path,
    repository: &str,
    workspace_path: Option<&str>,
) -> Result<Option<String>> {
    let mut dialect = Dialect::Extended;
    dialect.enable_f_strings = true;

    let ast = match AstModule::parse("<memory>", content.to_owned(), &dialect) {
        Ok(a) => a,
        Err(_) => return Ok(None),
    };

    let zen_dir = zen_file.parent().context("Zen file has no parent")?;
    let mut edits: Vec<SourceEdit> = Vec::new();

    // Visit all expressions
    ast.statement().visit_expr(|expr| {
        visit_string_literals(expr, &mut |s, lit_expr| {
            if let Some(url) = try_convert_path(
                s,
                zen_dir,
                package_root,
                workspace_root,
                repository,
                workspace_path,
            ) {
                let span = ast.codemap().resolve_span(lit_expr.span);
                edits.push((
                    span.begin.line,
                    span.begin.column,
                    span.end.line,
                    span.end.column,
                    format!("\"{}\"", url),
                ));
            }
        });
    });

    // Check load() statements
    for stmt in starlark_syntax::syntax::top_level_stmts::top_level_stmts(ast.statement()) {
        let StmtP::Load(load) = &stmt.node else {
            continue;
        };

        let module_path: &str = &load.module.node;
        if let Some(url) = try_convert_path(
            module_path,
            zen_dir,
            package_root,
            workspace_root,
            repository,
            workspace_path,
        ) {
            let span = ast.codemap().resolve_span(load.module.span);
            edits.push((
                span.begin.line,
                span.begin.column,
                span.end.line,
                span.end.column,
                format!("\"{}\"", url),
            ));
        }
    }

    if edits.is_empty() {
        return Ok(None);
    }

    let mut lines: Vec<String> = content.split('\n').map(|s| s.to_string()).collect();
    apply_edits(&mut lines, edits);
    Ok(Some(lines.join("\n")))
}

/// Normalize a path by resolving . and .. components without requiring the path to exist
fn normalize_path(path: &Path) -> PathBuf {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                components.pop();
            }
            std::path::Component::CurDir => {}
            c => components.push(c),
        }
    }
    components.iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_find_package_root() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let workspace_root = temp.path();

        // Create structure:
        // workspace/
        //   pcb.toml (root)
        //   reference/
        //     foo/
        //       pcb.toml (package root)
        //       test/
        //         test.zen

        fs::create_dir_all(workspace_root.join("reference/foo/test"))?;
        fs::write(workspace_root.join("pcb.toml"), "")?;
        fs::write(workspace_root.join("reference/foo/pcb.toml"), "")?;

        let zen_file = workspace_root.join("reference/foo/test/test.zen");
        fs::write(&zen_file, "")?;

        let package_root = find_package_root(&zen_file, workspace_root);
        assert_eq!(package_root, workspace_root.join("reference/foo"));

        Ok(())
    }

    #[test]
    fn test_escapes_package() {
        let package_root = PathBuf::from("/workspace/reference/foo");

        // Within package
        assert!(!escapes_package(
            &PathBuf::from("/workspace/reference/foo/bar.zen"),
            &package_root
        ));

        // Escapes package
        assert!(escapes_package(
            &PathBuf::from("/workspace/components/bar/bar.zen"),
            &package_root
        ));
    }

    #[test]
    fn test_build_url() {
        let workspace_root = PathBuf::from("/workspace");
        let resolved = PathBuf::from("/workspace/components/LED/LED.zen");

        // Without workspace path
        let url = build_url(
            &resolved,
            &workspace_root,
            "github.com/diodeinc/registry",
            None,
        );
        assert_eq!(
            url,
            Some("github.com/diodeinc/registry/components/LED/LED.zen".to_string())
        );

        // With workspace path
        let url = build_url(
            &resolved,
            &workspace_root,
            "github.com/company/monorepo",
            Some("hardware"),
        );
        assert_eq!(
            url,
            Some("github.com/company/monorepo/hardware/components/LED/LED.zen".to_string())
        );
    }

    #[test]
    fn test_try_convert_path() {
        let workspace_root = PathBuf::from("/workspace");
        let package_root = PathBuf::from("/workspace/reference/foo");
        let zen_dir = PathBuf::from("/workspace/reference/foo/src");

        // Relative path escaping package -> converts
        // Note: can't test fully without real filesystem, but we can test the logic

        // Already a URL -> no conversion
        assert!(try_convert_path(
            "github.com/diodeinc/registry/foo.zen",
            &zen_dir,
            &package_root,
            &workspace_root,
            "github.com/test",
            None
        )
        .is_none());

        // Alias -> no conversion
        assert!(try_convert_path(
            "@stdlib/interfaces.zen",
            &zen_dir,
            &package_root,
            &workspace_root,
            "github.com/test",
            None
        )
        .is_none());

        // Not a .zen file -> no conversion
        assert!(try_convert_path(
            "//common/foo.txt",
            &zen_dir,
            &package_root,
            &workspace_root,
            "github.com/test",
            None
        )
        .is_none());
    }
}
