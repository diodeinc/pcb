use anyhow::{Context, Result};
use ignore::WalkBuilder;
use starlark::syntax::{AstModule, Dialect};
use starlark_syntax::syntax::ast::{ArgumentP, ExprP, StmtP};
use starlark_syntax::syntax::module::AstModuleFields;
use std::path::{Path, PathBuf};

/// Convert all workspace-relative paths in .zen files to file-relative paths
pub fn convert_workspace_paths(workspace_root: &Path) -> Result<()> {
    let zen_files = collect_zen_files(workspace_root)?;

    if zen_files.is_empty() {
        eprintln!("  No .zen files found");
        return Ok(());
    }

    let mut converted_count = 0;

    for zen_file in &zen_files {
        let content = std::fs::read_to_string(zen_file)
            .with_context(|| format!("Failed to read {}", zen_file.display()))?;

        if let Some(updated) = convert_file(zen_file, &content, workspace_root)? {
            std::fs::write(zen_file, updated)
                .with_context(|| format!("Failed to write {}", zen_file.display()))?;
            eprintln!("  ✓ {}", zen_file.display());
            converted_count += 1;
        }
    }

    if converted_count == 0 {
        eprintln!("  No workspace-relative paths found");
    } else {
        eprintln!("  Converted {} file(s)", converted_count);
    }

    Ok(())
}

/// Collect all .zen files in workspace
fn collect_zen_files(workspace_root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();

    let walker = WalkBuilder::new(workspace_root)
        .hidden(true) // Ignore hidden files and directories
        .git_ignore(true) // Respect .gitignore
        .git_exclude(true) // Respect .git/info/exclude
        .build();

    for entry in walker.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_file() && path.extension() == Some(std::ffi::OsStr::new("zen")) {
            files.push(path.to_path_buf());
        }
    }

    Ok(files)
}

/// Convert workspace-relative paths in a single file
fn convert_file(zen_file: &Path, content: &str, workspace_root: &Path) -> Result<Option<String>> {
    let mut dialect = Dialect::Extended;
    dialect.enable_f_strings = true;

    let ast = match AstModule::parse("<memory>", content.to_owned(), &dialect) {
        Ok(a) => a,
        Err(_) => return Ok(None), // Skip unparseable files
    };

    let mut lines: Vec<String> = content.split('\n').map(|s| s.to_string()).collect();
    let mut edits: Vec<(usize, usize, usize, usize, String)> = Vec::new();

    // Helper to recursively check all expressions for workspace paths
    fn check_expr_for_workspace_paths(
        expr: &starlark_syntax::syntax::ast::AstExpr,
        edits: &mut Vec<(usize, usize, usize, usize, String)>,
        zen_file: &Path,
        workspace_root: &Path,
        ast: &AstModule,
    ) {
        // Check if this expression is a string literal with workspace path
        if let ExprP::Literal(lit) = &expr.node {
            let s = lit.to_string();
            if (s.starts_with('"') || s.starts_with('\'')) && s.len() > 2 {
                let unquoted = &s[1..s.len() - 1];
                if unquoted.starts_with("//") {
                    if let Ok(relative) =
                        convert_workspace_to_file_relative(unquoted, zen_file, workspace_root)
                    {
                        let span = ast.codemap().resolve_span(expr.span);
                        edits.push((
                            span.begin.line,
                            span.begin.column,
                            span.end.line,
                            span.end.column,
                            format!("\"{}\"", relative),
                        ));
                    }
                }
            }
        }

        // Recurse into subexpressions based on type
        match &expr.node {
            ExprP::Call(_name, args) => {
                for arg in &args.args {
                    let arg_expr = match &arg.node {
                        ArgumentP::Positional(e) => e,
                        ArgumentP::Named(_, e) => e,
                        _ => continue,
                    };
                    check_expr_for_workspace_paths(arg_expr, edits, zen_file, workspace_root, ast);
                }
            }
            ExprP::If(if_box) => {
                let (cond, then_expr, else_expr) = &**if_box;
                check_expr_for_workspace_paths(cond, edits, zen_file, workspace_root, ast);
                check_expr_for_workspace_paths(then_expr, edits, zen_file, workspace_root, ast);
                check_expr_for_workspace_paths(else_expr, edits, zen_file, workspace_root, ast);
            }
            ExprP::List(exprs) | ExprP::Tuple(exprs) => {
                for e in exprs {
                    check_expr_for_workspace_paths(e, edits, zen_file, workspace_root, ast);
                }
            }
            ExprP::Dict(pairs) => {
                for (k, v) in pairs {
                    check_expr_for_workspace_paths(k, edits, zen_file, workspace_root, ast);
                    check_expr_for_workspace_paths(v, edits, zen_file, workspace_root, ast);
                }
            }
            _ => {}
        }
    }

    // Visit all expressions
    ast.statement().visit_expr(|expr| {
        check_expr_for_workspace_paths(expr, &mut edits, zen_file, workspace_root, &ast);
    });

    // Check load() statements
    for stmt in starlark_syntax::syntax::top_level_stmts::top_level_stmts(ast.statement()) {
        let StmtP::Load(load) = &stmt.node else {
            continue;
        };

        let module_path: &str = &load.module.node;

        // Convert workspace-relative paths
        if module_path.starts_with("//") {
            if let Ok(relative) =
                convert_workspace_to_file_relative(module_path, zen_file, workspace_root)
            {
                let span = ast.codemap().resolve_span(load.module.span);
                edits.push((
                    span.begin.line,
                    span.begin.column,
                    span.end.line,
                    span.end.column,
                    format!("\"{}\"", relative),
                ));
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
            let first_prefix =
                lines[start_line][..start_col.min(lines[start_line].len())].to_string();
            let last_suffix = lines[end_line][end_col.min(lines[end_line].len())..].to_string();
            lines.splice(
                start_line..=end_line,
                vec![format!("{}{}{}", first_prefix, replacement, last_suffix)],
            );
        }
    }

    Ok(Some(lines.join("\n")))
}

/// Convert workspace-relative path to file-relative path
fn convert_workspace_to_file_relative(
    workspace_path: &str,
    zen_file: &Path,
    workspace_root: &Path,
) -> Result<String> {
    // Strip "//" prefix
    let rel_to_workspace = workspace_path
        .strip_prefix("//")
        .context("Path doesn't start with //")?;

    // Build absolute path
    let abs_target = workspace_root.join(rel_to_workspace);

    // Make relative to zen_file's directory
    let zen_dir = zen_file
        .parent()
        .context("Zen file has no parent directory")?;

    let relative =
        pathdiff::diff_paths(&abs_target, zen_dir).context("Cannot compute relative path")?;

    // Normalize to forward slashes, add "./" prefix if needed
    let relative_str = relative.to_string_lossy().replace('\\', "/");

    if relative_str.starts_with("..") || relative_str.starts_with('/') {
        Ok(relative_str)
    } else {
        Ok(format!("./{}", relative_str))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_convert_workspace_to_file_relative() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let workspace_root = temp.path();

        // Create structure:
        // workspace/
        //   boards/
        //     main.zen
        //   stdlib/
        //     units.zen

        let boards_dir = workspace_root.join("boards");
        fs::create_dir(&boards_dir)?;
        let stdlib_dir = workspace_root.join("stdlib");
        fs::create_dir(&stdlib_dir)?;

        let main_zen = boards_dir.join("main.zen");
        fs::write(&main_zen, "")?;

        // //stdlib/units.zen from boards/main.zen should become ../stdlib/units.zen
        let result =
            convert_workspace_to_file_relative("//stdlib/units.zen", &main_zen, workspace_root)?;
        assert_eq!(result, "../stdlib/units.zen");

        Ok(())
    }

    #[test]
    fn test_convert_file_with_workspace_paths() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let workspace_root = temp.path();

        let boards_dir = workspace_root.join("boards");
        fs::create_dir(&boards_dir)?;
        let stdlib_dir = workspace_root.join("stdlib");
        fs::create_dir(&stdlib_dir)?;

        let main_zen = boards_dir.join("main.zen");
        let content = r#"load("//stdlib/units.zen", "Voltage")
MyModule = Module("//stdlib/helpers.zen")
"#;
        fs::write(&main_zen, content)?;

        let result = convert_file(&main_zen, content, workspace_root)?;
        assert!(result.is_some());

        let updated = result.unwrap();
        assert!(updated.contains("\"../stdlib/units.zen\""));
        assert!(updated.contains("\"../stdlib/helpers.zen\""));

        Ok(())
    }
}
