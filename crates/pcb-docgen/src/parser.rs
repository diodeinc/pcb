//! Parser for extracting docstrings, functions, types, and constants from .zen files.
//!
//! Uses starlark-rust's AST parsing for accurate extraction instead of regex.
//! Module detection is done via `pcb build --netlist` in signature.rs.

use crate::types::*;
use starlark::docs::DocString;
use starlark::docs::DocStringKind;
use starlark::syntax::AstModule;
use starlark::syntax::Dialect;
use starlark_syntax::syntax::ast::AssignTargetP;
use starlark_syntax::syntax::ast::AstLiteral;
use starlark_syntax::syntax::ast::AstPayload;
use starlark_syntax::syntax::ast::AstStmtP;
use starlark_syntax::syntax::ast::ExprP;
use starlark_syntax::syntax::ast::ParameterP;
use starlark_syntax::syntax::ast::StmtP;

/// Parse a library file to extract its documentation using AST parsing.
pub fn parse_library(path: String, content: &str) -> anyhow::Result<LibraryDoc> {
    let mut dialect = Dialect::Extended;
    dialect.enable_f_strings = true;
    let ast = AstModule::parse("<memory>", content.to_owned(), &dialect)
        .map_err(|e| anyhow::anyhow!("Failed to parse {}: {}", path, e))?;

    let stmt = ast.statement();
    let (types, constants) = extract_types_and_constants(stmt);

    Ok(LibraryDoc {
        path,
        file_doc: extract_file_docstring_from_stmt(stmt),
        functions: extract_functions(stmt),
        types,
        constants,
    })
}

/// Extract the file-level docstring from source content using AST parsing.
pub fn extract_file_docstring(content: &str) -> Option<crate::types::DocString> {
    let mut dialect = Dialect::Extended;
    dialect.enable_f_strings = true;
    let ast = AstModule::parse("<memory>", content.to_owned(), &dialect).ok()?;
    extract_file_docstring_from_stmt(ast.statement())
}

/// Extract docstring from an expression statement if it's a string literal.
fn try_extract_docstring_from_expr(
    expr: &starlark_syntax::codemap::Spanned<ExprP<impl AstPayload>>,
) -> Option<crate::types::DocString> {
    if let ExprP::Literal(AstLiteral::String(s)) = &expr.node {
        let raw = s.node.as_str();
        if let Some(doc) = DocString::from_docstring(DocStringKind::Starlark, raw) {
            return Some(crate::types::DocString {
                summary: doc.summary,
                description: doc.details.unwrap_or_default(),
            });
        }
    }
    None
}

/// Extract the file-level docstring from an AST statement.
fn extract_file_docstring_from_stmt(
    stmt: &AstStmtP<impl AstPayload>,
) -> Option<crate::types::DocString> {
    match &stmt.node {
        // Multiple statements - find first non-load that's a docstring
        StmtP::Statements(stmts) => {
            for s in stmts {
                match &s.node {
                    StmtP::Load(_) => continue,
                    StmtP::Expression(expr) => return try_extract_docstring_from_expr(expr),
                    _ => return None,
                }
            }
            None
        }
        // Single expression statement (file contains only docstring)
        StmtP::Expression(expr) => try_extract_docstring_from_expr(expr),
        _ => None,
    }
}

/// Extract function definitions from the AST.
fn extract_functions(
    stmt: &AstStmtP<impl starlark_syntax::syntax::ast::AstPayload>,
) -> Vec<FunctionDoc> {
    let mut functions = Vec::new();

    if let StmtP::Statements(stmts) = &stmt.node {
        for s in stmts {
            if let StmtP::Def(def) = &s.node {
                let name = def.name.ident.clone();

                // Skip private functions
                if name.starts_with('_') {
                    continue;
                }

                // Build signature string
                let mut sig = format!("def {}(", name);
                let params: Vec<String> =
                    def.params.iter().map(|p| format_param(&p.node)).collect();
                sig.push_str(&params.join(", "));
                sig.push_str("):");

                // Extract docstring from function body
                let doc = peek_docstring(&def.body).and_then(|raw| {
                    DocString::from_docstring(DocStringKind::Starlark, raw).map(|d| {
                        crate::types::DocString {
                            summary: d.summary,
                            description: d.details.unwrap_or_default(),
                        }
                    })
                });

                functions.push(FunctionDoc {
                    name,
                    signature: sig,
                    doc,
                });
            }
        }
    }

    functions.sort_by(|a, b| a.name.cmp(&b.name));
    functions
}

/// Extract types (enums, interfaces, net_types) and constants from the AST.
fn extract_types_and_constants(
    stmt: &AstStmtP<impl starlark_syntax::syntax::ast::AstPayload>,
) -> (Vec<TypeDoc>, Vec<ConstDoc>) {
    let mut types = Vec::new();
    let mut constants = Vec::new();

    if let StmtP::Statements(stmts) = &stmt.node {
        for s in stmts {
            if let StmtP::Assign(assign) = &s.node {
                // Get the assigned name - only handle simple identifiers
                let name = match &assign.lhs.node {
                    AssignTargetP::Identifier(ident) => ident.ident.clone(),
                    _ => continue, // Skip tuple/index/dot assignments
                };

                // Check what's being assigned
                if let ExprP::Call(func, _args) = &assign.rhs.node {
                    let func_name = get_call_name(func);

                    match func_name.as_str() {
                        "enum" => types.push(TypeDoc {
                            name: name.clone(),
                            kind: "enum".to_string(),
                        }),
                        "interface" => types.push(TypeDoc {
                            name: name.clone(),
                            kind: "interface".to_string(),
                        }),
                        "record" => types.push(TypeDoc {
                            name: name.clone(),
                            kind: "record".to_string(),
                        }),
                        "builtin.net_type" => types.push(TypeDoc {
                            name: name.clone(),
                            kind: "net_type".to_string(),
                        }),
                        "builtin.physical_value" => types.push(TypeDoc {
                            name: name.clone(),
                            kind: "PhysicalValue".to_string(),
                        }),
                        _ => {
                            if is_constant_name(&name) {
                                constants.push(ConstDoc { name: name.clone() });
                            }
                        }
                    }
                } else if is_constant_name(&name) {
                    constants.push(ConstDoc { name: name.clone() });
                }
            }
        }
    }

    types.sort_by(|a, b| a.name.cmp(&b.name));
    constants.sort_by(|a, b| a.name.cmp(&b.name));

    (types, constants)
}

/// Get the name of a called function from its expression.
fn get_call_name<P: AstPayload>(expr: &starlark_syntax::codemap::Spanned<ExprP<P>>) -> String {
    match &expr.node {
        ExprP::Identifier(ident) => ident.ident.clone(),
        ExprP::Dot(base, attr) => {
            let base_name = get_call_name(base);
            format!("{}.{}", base_name, attr.node)
        }
        _ => String::new(),
    }
}

/// Format a parameter for the signature string.
fn format_param<P: AstPayload>(param: &ParameterP<P>) -> String {
    match param {
        ParameterP::Normal(ident, _, Some(_default)) => format!("{}=...", ident.ident),
        ParameterP::Normal(ident, _, None) => ident.ident.clone(),
        ParameterP::Args(ident, _) => format!("*{}", ident.ident),
        ParameterP::KwArgs(ident, _) => format!("**{}", ident.ident),
        ParameterP::NoArgs => "*".to_string(),
        ParameterP::Slash => "/".to_string(),
    }
}

/// Check if a name looks like a constant (ALL_CAPS with underscores).
fn is_constant_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().next().unwrap().is_uppercase()
        && name
            .chars()
            .all(|c| c.is_uppercase() || c.is_ascii_digit() || c == '_')
}

/// Peek into a statement to find a docstring.
fn peek_docstring(stmt: &AstStmtP<impl starlark_syntax::syntax::ast::AstPayload>) -> Option<&str> {
    match &stmt.node {
        StmtP::Statements(stmts) => stmts.first().and_then(peek_docstring),
        StmtP::Expression(expr) => {
            if let ExprP::Literal(AstLiteral::String(s)) = &expr.node {
                Some(s.node.as_str())
            } else {
                None
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_file_docstring() {
        let content = r#""""General utilities for Zen.""""#;
        let doc = extract_file_docstring(content);
        assert!(doc.is_some());
        assert_eq!(doc.unwrap().summary, "General utilities for Zen.");
    }

    #[test]
    fn test_extract_multiline_file_docstring() {
        let content = r#""""
Placeholder for an arbitrary block.

```zen
Block(name="POWER_REGULATOR")
```
""""#;
        let doc = extract_file_docstring(content);
        assert!(doc.is_some());
        let doc = doc.unwrap();
        assert_eq!(doc.summary, "Placeholder for an arbitrary block.");
        assert!(doc.description.contains("```zen"));
    }

    #[test]
    fn test_is_constant_name() {
        assert!(is_constant_name("FOO_BAR"));
        assert!(is_constant_name("E96"));
        assert!(!is_constant_name("FooBar"));
        assert!(!is_constant_name("foo_bar"));
    }
}
