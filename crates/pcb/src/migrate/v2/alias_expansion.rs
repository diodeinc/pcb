use anyhow::{Context, Result};
use pcb_zen::ast_utils::{apply_edits, collect_zen_files, visit_string_literals, SourceEdit};
use starlark::syntax::{AstModule, Dialect};
use starlark_syntax::syntax::ast::StmtP;
use starlark_syntax::syntax::module::AstModuleFields;
use std::path::Path;

const ALIAS_MAPPINGS: &[(&str, &str)] = &[("@registry", "github.com/diodeinc/registry")];

/// Expand hardcoded aliases in .zen files (e.g., @registry -> github.com/diodeinc/registry)
pub fn expand_aliases(workspace_root: &Path) -> Result<()> {
    let zen_files = collect_zen_files(workspace_root)?;

    if zen_files.is_empty() {
        eprintln!("  No .zen files found");
        return Ok(());
    }

    let mut converted_count = 0;

    for zen_file in &zen_files {
        let content = std::fs::read_to_string(zen_file)
            .with_context(|| format!("Failed to read {}", zen_file.display()))?;

        if let Some(updated) = convert_file(&content)? {
            std::fs::write(zen_file, &updated)
                .with_context(|| format!("Failed to write {}", zen_file.display()))?;
            eprintln!("  ✓ {}", zen_file.display());
            converted_count += 1;
        }
    }

    if converted_count == 0 {
        eprintln!("  No aliases to expand");
    } else {
        eprintln!("  Converted {} file(s)", converted_count);
    }

    Ok(())
}

/// Try to expand an alias, returns None if no expansion needed
fn try_expand_alias(path_str: &str) -> Option<String> {
    for (alias, expansion) in ALIAS_MAPPINGS {
        if let Some(rest) = path_str.strip_prefix(alias) {
            if rest.is_empty() || rest.starts_with('/') {
                return Some(format!("{}{}", expansion, rest));
            }
        }
    }
    None
}

/// Convert aliases in a single file
fn convert_file(content: &str) -> Result<Option<String>> {
    let mut dialect = Dialect::Extended;
    dialect.enable_f_strings = true;

    let ast = match AstModule::parse("<memory>", content.to_owned(), &dialect) {
        Ok(a) => a,
        Err(_) => return Ok(None),
    };

    let mut edits: Vec<SourceEdit> = Vec::new();

    ast.statement().visit_expr(|expr| {
        visit_string_literals(expr, &mut |s, lit_expr| {
            if let Some(expanded) = try_expand_alias(s) {
                let span = ast.codemap().resolve_span(lit_expr.span);
                edits.push((
                    span.begin.line,
                    span.begin.column,
                    span.end.line,
                    span.end.column,
                    format!("\"{}\"", expanded),
                ));
            }
        });
    });

    for stmt in starlark_syntax::syntax::top_level_stmts::top_level_stmts(ast.statement()) {
        let StmtP::Load(load) = &stmt.node else {
            continue;
        };

        let module_path: &str = &load.module.node;
        if let Some(expanded) = try_expand_alias(module_path) {
            let span = ast.codemap().resolve_span(load.module.span);
            edits.push((
                span.begin.line,
                span.begin.column,
                span.end.line,
                span.end.column,
                format!("\"{}\"", expanded),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_try_expand_alias() {
        assert_eq!(
            try_expand_alias("@registry/components/LED.zen"),
            Some("github.com/diodeinc/registry/components/LED.zen".to_string())
        );

        assert_eq!(
            try_expand_alias("@registry"),
            Some("github.com/diodeinc/registry".to_string())
        );

        assert_eq!(try_expand_alias("@stdlib/units.zen"), None);
        assert_eq!(try_expand_alias("./local.zen"), None);
        assert_eq!(try_expand_alias("@registryother/foo.zen"), None);
    }

    #[test]
    fn test_convert_file_with_registry_alias() -> Result<()> {
        let content = r#"load("@registry/components/LED.zen", "LED")
MyModule = Module("@registry/modules/Power.zen")
"#;

        let result = convert_file(content)?;
        assert!(result.is_some());

        let updated = result.unwrap();
        assert!(updated.contains("\"github.com/diodeinc/registry/components/LED.zen\""));
        assert!(updated.contains("\"github.com/diodeinc/registry/modules/Power.zen\""));

        Ok(())
    }

    #[test]
    fn test_convert_file_no_aliases() -> Result<()> {
        let content = r#"load("@stdlib/units.zen", "Voltage")
MyModule = Module("./local.zen")
"#;

        let result = convert_file(content)?;
        assert!(result.is_none());

        Ok(())
    }
}
