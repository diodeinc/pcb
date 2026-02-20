use anyhow::{Context, Result};
use pcb_zen::ast_utils::{SourceEdit, apply_edits, visit_string_literals};
use starlark::syntax::{AstModule, Dialect};
use starlark_syntax::syntax::ast::StmtP;
use starlark_syntax::syntax::module::AstModuleFields;
use std::path::Path;

use super::{Codemod, MigrateContext};

/// Convert all workspace-relative paths (//) in .zen files to file-relative paths
pub struct WorkspacePaths;

impl Codemod for WorkspacePaths {
    fn apply(
        &self,
        ctx: &MigrateContext,
        zen_file: &Path,
        content: &str,
    ) -> Result<Option<String>> {
        let mut dialect = Dialect::Extended;
        dialect.enable_f_strings = true;

        let ast = match AstModule::parse("<memory>", content.to_owned(), &dialect) {
            Ok(a) => a,
            Err(_) => return Ok(None),
        };

        let mut edits: Vec<SourceEdit> = Vec::new();

        // Visit all expressions
        ast.statement().visit_expr(|expr| {
            visit_string_literals(expr, &mut |s, lit_expr| {
                if s.starts_with("//")
                    && let Ok(relative) =
                        convert_workspace_to_file_relative(s, zen_file, &ctx.workspace_root)
                {
                    let span = ast.codemap().resolve_span(lit_expr.span);
                    edits.push((
                        span.begin.line,
                        span.begin.column,
                        span.end.line,
                        span.end.column,
                        format!("\"{}\"", relative),
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
            if module_path.starts_with("//")
                && let Ok(relative) =
                    convert_workspace_to_file_relative(module_path, zen_file, &ctx.workspace_root)
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

        if edits.is_empty() {
            return Ok(None);
        }

        let mut lines: Vec<String> = content.split('\n').map(|s| s.to_string()).collect();
        apply_edits(&mut lines, edits);
        Ok(Some(lines.join("\n")))
    }
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

        let boards_dir = workspace_root.join("boards");
        fs::create_dir(&boards_dir)?;
        let stdlib_dir = workspace_root.join("stdlib");
        fs::create_dir(&stdlib_dir)?;

        let main_zen = boards_dir.join("main.zen");
        fs::write(&main_zen, "")?;

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

        let ctx = MigrateContext {
            workspace_root: workspace_root.to_path_buf(),
            repository: "github.com/test/repo".to_string(),
            repo_subpath: None,
        };
        let codemod = WorkspacePaths;
        let result = codemod.apply(&ctx, &main_zen, content)?;
        assert!(result.is_some());

        let updated = result.unwrap();
        assert!(updated.contains("\"../stdlib/units.zen\""));
        assert!(updated.contains("\"../stdlib/helpers.zen\""));

        Ok(())
    }
}
