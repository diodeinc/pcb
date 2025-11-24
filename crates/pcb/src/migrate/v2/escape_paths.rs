use anyhow::{Context, Result};
use ignore::WalkBuilder;
use starlark::syntax::{AstModule, Dialect};
use starlark_syntax::syntax::ast::{ArgumentP, ExprP, StmtP};
use starlark_syntax::syntax::module::AstModuleFields;
use std::path::{Path, PathBuf};

/// Convert cross-package relative paths to URLs in .zen files
pub fn convert_escape_paths(
    workspace_root: &Path,
    repository: &str,
    workspace_path: Option<&str>,
) -> Result<()> {
    let zen_files = collect_zen_files(workspace_root)?;

    if zen_files.is_empty() {
        eprintln!("  No .zen files found");
        return Ok(());
    }

    let mut converted_count = 0;

    for zen_file in &zen_files {
        let content = std::fs::read_to_string(zen_file)
            .with_context(|| format!("Failed to read {}", zen_file.display()))?;

        let package_root = find_package_root(zen_file, workspace_root);

        if let Some(updated) =
            convert_file(zen_file, &content, &package_root, workspace_root, repository, workspace_path)?
        {
            std::fs::write(zen_file, updated)
                .with_context(|| format!("Failed to write {}", zen_file.display()))?;
            eprintln!("  ✓ {}", zen_file.display());
            converted_count += 1;
        }
    }

    if converted_count == 0 {
        eprintln!("  No cross-package paths found");
    } else {
        eprintln!("  Converted {} file(s)", converted_count);
    }

    Ok(())
}

/// Collect all .zen files in workspace
fn collect_zen_files(workspace_root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();

    let walker = WalkBuilder::new(workspace_root)
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

/// Check if a string looks like a relative path to a .zen file
fn is_relative_zen_path(s: &str) -> bool {
    // Must end with .zen
    if !s.ends_with(".zen") {
        return false;
    }
    // Must be a relative path (not an alias, not an absolute URL)
    !s.starts_with('@') && !s.contains("://") && !s.starts_with("//")
        && (s.starts_with("./") || s.starts_with("../") || !s.contains('/') || s.contains('/'))
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
    let mut lines: Vec<String> = content.split('\n').map(|s| s.to_string()).collect();
    let mut edits: Vec<(usize, usize, usize, usize, String)> = Vec::new();

    fn check_expr(
        expr: &starlark_syntax::syntax::ast::AstExpr,
        edits: &mut Vec<(usize, usize, usize, usize, String)>,
        zen_dir: &Path,
        package_root: &Path,
        workspace_root: &Path,
        repository: &str,
        workspace_path: Option<&str>,
        ast: &AstModule,
    ) {
        if let ExprP::Literal(lit) = &expr.node {
            let s = lit.to_string();
            if (s.starts_with('"') || s.starts_with('\'')) && s.len() > 2 {
                let unquoted = &s[1..s.len() - 1];
                if is_relative_zen_path(unquoted) {
                    // Resolve relative path
                    let resolved = zen_dir.join(unquoted);
                    let resolved = match resolved.canonicalize() {
                        Ok(p) => p,
                        Err(_) => {
                            // File doesn't exist, try to normalize without canonicalize
                            normalize_path(&resolved)
                        }
                    };

                    if escapes_package(&resolved, package_root) && resolved.starts_with(workspace_root) {
                        if let Some(url) = build_url(&resolved, workspace_root, repository, workspace_path) {
                            let span = ast.codemap().resolve_span(expr.span);
                            edits.push((
                                span.begin.line,
                                span.begin.column,
                                span.end.line,
                                span.end.column,
                                format!("\"{}\"", url),
                            ));
                        }
                    }
                }
            }
        }

        match &expr.node {
            ExprP::Call(_name, args) => {
                for arg in &args.args {
                    let arg_expr = match &arg.node {
                        ArgumentP::Positional(e) => e,
                        ArgumentP::Named(_, e) => e,
                        _ => continue,
                    };
                    check_expr(arg_expr, edits, zen_dir, package_root, workspace_root, repository, workspace_path, ast);
                }
            }
            ExprP::If(if_box) => {
                let (cond, then_expr, else_expr) = &**if_box;
                check_expr(cond, edits, zen_dir, package_root, workspace_root, repository, workspace_path, ast);
                check_expr(then_expr, edits, zen_dir, package_root, workspace_root, repository, workspace_path, ast);
                check_expr(else_expr, edits, zen_dir, package_root, workspace_root, repository, workspace_path, ast);
            }
            ExprP::List(exprs) | ExprP::Tuple(exprs) => {
                for e in exprs {
                    check_expr(e, edits, zen_dir, package_root, workspace_root, repository, workspace_path, ast);
                }
            }
            ExprP::Dict(pairs) => {
                for (k, v) in pairs {
                    check_expr(k, edits, zen_dir, package_root, workspace_root, repository, workspace_path, ast);
                    check_expr(v, edits, zen_dir, package_root, workspace_root, repository, workspace_path, ast);
                }
            }
            _ => {}
        }
    }

    // Visit all expressions
    ast.statement().visit_expr(|expr| {
        check_expr(expr, &mut edits, zen_dir, package_root, workspace_root, repository, workspace_path, &ast);
    });

    // Check load() statements
    for stmt in starlark_syntax::syntax::top_level_stmts::top_level_stmts(ast.statement()) {
        let StmtP::Load(load) = &stmt.node else {
            continue;
        };

        let module_path: &str = &load.module.node;

        if is_relative_zen_path(module_path) {
            let resolved = zen_dir.join(module_path);
            let resolved = match resolved.canonicalize() {
                Ok(p) => p,
                Err(_) => normalize_path(&resolved),
            };

            if escapes_package(&resolved, package_root) && resolved.starts_with(workspace_root) {
                if let Some(url) = build_url(&resolved, workspace_root, repository, workspace_path) {
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
        }
    }

    if edits.is_empty() {
        return Ok(None);
    }

    // Apply edits in reverse order to preserve offsets
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
            let first_prefix = lines[start_line][..start_col.min(lines[start_line].len())].to_string();
            let last_suffix = lines[end_line][end_col.min(lines[end_line].len())..].to_string();
            lines.splice(
                start_line..=end_line,
                vec![format!("{}{}{}", first_prefix, replacement, last_suffix)],
            );
        }
    }

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
        let url = build_url(&resolved, &workspace_root, "github.com/diodeinc/registry", None);
        assert_eq!(url, Some("github.com/diodeinc/registry/components/LED/LED.zen".to_string()));

        // With workspace path
        let url = build_url(&resolved, &workspace_root, "github.com/company/monorepo", Some("hardware"));
        assert_eq!(url, Some("github.com/company/monorepo/hardware/components/LED/LED.zen".to_string()));
    }

    #[test]
    fn test_is_relative_zen_path() {
        assert!(is_relative_zen_path("../../components/LED/LED.zen"));
        assert!(is_relative_zen_path("./module.zen"));
        assert!(is_relative_zen_path("Module.zen"));

        // Not relative paths
        assert!(!is_relative_zen_path("@stdlib/interfaces.zen"));
        assert!(!is_relative_zen_path("github.com/diodeinc/stdlib/interfaces.zen"));
        assert!(!is_relative_zen_path("//stdlib/interfaces.zen"));
        assert!(!is_relative_zen_path("not_a_zen_file.txt"));
    }
}
