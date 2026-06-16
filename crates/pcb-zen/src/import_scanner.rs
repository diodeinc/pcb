use std::collections::BTreeSet;
use std::path::PathBuf;

use pcb_zen_core::load_spec::LoadSpec;
use starlark::syntax::{AstModule, Dialect};
use starlark_syntax::syntax::ast::StmtP;
use starlark_syntax::syntax::top_level_stmts::top_level_stmts;

use crate::ast_utils::visit_string_literals;

#[derive(Debug, Default)]
pub struct CollectedImports {
    pub urls: BTreeSet<String>,
    pub relative_paths: Vec<PathBuf>,
}

pub fn extract_imports(content: &str) -> Option<CollectedImports> {
    let mut dialect = Dialect::Extended;
    dialect.enable_f_strings = true;

    let ast = AstModule::parse("<memory>", content.to_owned(), &dialect).ok()?;
    let mut result = CollectedImports::default();

    ast.statement().visit_expr(|expr| {
        visit_string_literals(expr, &mut |s, _| {
            extract_from_literal(s, &mut result);
        });
    });

    for stmt in top_level_stmts(ast.statement()) {
        if let StmtP::Load(load) = &stmt.node {
            extract_from_load(&load.module.node, &mut result);
        }
    }

    Some(result)
}

fn extract_from_load(s: &str, result: &mut CollectedImports) {
    if let Some(spec) = LoadSpec::parse(s) {
        collect_spec(s, spec, result);
    }
}

/// Extract dependency-looking aliases, URLs, or relative paths from arbitrary string literals.
///
/// Unlike `load()` arguments, ordinary strings may be pin names, docstrings, or labels, so keep
/// this intentionally narrower than the full `LoadSpec` grammar.
fn extract_from_literal(s: &str, result: &mut CollectedImports) {
    if !s.starts_with('@') && !is_explicit_path(s) && !is_dependency_url(s) {
        return;
    }

    if let Some(spec) = LoadSpec::parse(s) {
        collect_spec(s, spec, result);
    }
}

fn collect_spec(s: &str, spec: LoadSpec, result: &mut CollectedImports) {
    match spec {
        LoadSpec::Stdlib { .. } | LoadSpec::PackageUri { .. } | LoadSpec::Package { .. } => {}
        LoadSpec::Url { .. } => {
            result.urls.insert(s.to_string());
        }
        LoadSpec::Path { path, .. } => {
            result.relative_paths.push(path);
        }
    }
}

fn is_explicit_path(s: &str) -> bool {
    s.starts_with("./") || s.starts_with("../") || s.starts_with('/')
}

fn is_dependency_url(s: &str) -> bool {
    let Some(last_segment) = s.rsplit('/').next() else {
        return false;
    };
    last_segment.contains('.')
        && [
            ".zen",
            ".kicad_sym",
            ".kicad_mod",
            ".pretty",
            ".step",
            ".wrl",
        ]
        .iter()
        .any(|suffix| last_segment.ends_with(suffix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_from_literal() {
        let mut result = CollectedImports::default();

        extract_from_literal(
            "@footprints/Resistor_SMD.pretty/R_0603.kicad_mod",
            &mut result,
        );
        assert!(result.urls.is_empty());

        result = CollectedImports::default();
        extract_from_literal("@stdlib/units.zen", &mut result);
        assert!(result.urls.is_empty());

        result = CollectedImports::default();
        extract_from_literal("github.com/diodeinc/stdlib/units.zen", &mut result);
        assert!(result.urls.contains("github.com/diodeinc/stdlib/units.zen"));

        result = CollectedImports::default();
        extract_from_literal("@footprints/{}.pretty/{}.kicad_mod", &mut result);
        assert!(result.urls.is_empty());

        result = CollectedImports::default();
        extract_from_literal(
            "github.com/example/components/Resistor/Resistor.zen",
            &mut result,
        );
        assert!(
            result
                .urls
                .contains("github.com/example/components/Resistor/Resistor.zen")
        );

        result = CollectedImports::default();
        extract_from_literal("../../other-pkg/foo.zen", &mut result);
        assert_eq!(result.relative_paths.len(), 1);
        assert_eq!(
            result.relative_paths[0],
            PathBuf::from("../../other-pkg/foo.zen")
        );

        result = CollectedImports::default();
        extract_from_literal("VCC", &mut result);
        extract_from_literal(
            "\nBX-DS1.27-3PTP\n\n3-position SMD DIP switch, 1.27mm pitch.\n",
            &mut result,
        );
        extract_from_literal("GPIO_VREF(1.8v/3.3v_Input)", &mut result);
        assert!(result.urls.is_empty());
        assert!(result.relative_paths.is_empty());
    }

    #[test]
    fn test_extract_from_load_uses_full_load_spec_grammar() {
        let mut result = CollectedImports::default();

        extract_from_load(
            "github.com/example/components/Resistor/Resistor.zen",
            &mut result,
        );
        assert!(
            result
                .urls
                .contains("github.com/example/components/Resistor/Resistor.zen")
        );

        result = CollectedImports::default();
        extract_from_load("relative_module.zen", &mut result);
        assert_eq!(
            result.relative_paths,
            vec![PathBuf::from("relative_module.zen")]
        );
    }
}
