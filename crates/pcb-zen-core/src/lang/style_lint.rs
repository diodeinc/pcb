use std::collections::HashSet;

use starlark::syntax::{
    AstModule,
    ast::{ArgumentP, AssignTargetP, ExprP, LoadArgP, StmtP},
};
use starlark_syntax::syntax::{module::AstModuleFields, top_level_stmts::top_level_stmts};

use crate::{Diagnostic, lang::naming};

type InterfaceNames = HashSet<String>;

pub(crate) fn ast_style_lints(ast: &AstModule) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let top_level = top_level_stmts(ast.statement());
    let interface_names = interface_names(&top_level);

    for stmt in &top_level {
        let StmtP::Assign(assign) = &stmt.node else {
            continue;
        };
        let AssignTargetP::Identifier(ident) = &assign.lhs.node else {
            continue;
        };
        let rhs = &assign.rhs.node;

        if let Some(first_arg) = first_positional_arg(rhs)
            && let ExprP::Literal(starlark::syntax::ast::AstLiteral::String(explicit_name)) =
                &first_arg.node
            && explicit_name.node == ident.node.ident
            && let Some(callable) = redundant_name_call_kind(rhs, &interface_names)
        {
            let span = ast.codemap().file_span(first_arg.span).resolve_span();
            diagnostics.push(naming::redundant_name_diagnostic(
                callable,
                &explicit_name.node,
                Some(span),
                std::path::Path::new(ast.codemap().filename()),
            ));
        }
    }

    diagnostics
}

pub(crate) fn is_ast_style_diagnostic(diagnostic: &Diagnostic) -> bool {
    diagnostic
        .downcast_error_ref::<crate::lang::error::CategorizedDiagnostic>()
        .is_some_and(|categorized| categorized.kind.starts_with("style."))
}

fn interface_names(
    top_level: &[&starlark::codemap::Spanned<StmtP<starlark::syntax::ast::AstNoPayload>>],
) -> InterfaceNames {
    let mut names = HashSet::new();

    for stmt in top_level {
        match &stmt.node {
            StmtP::Load(load) if load.module.node.ends_with("interfaces.zen") => {
                for LoadArgP { local, .. } in &load.args {
                    names.insert(local.node.ident.to_owned());
                }
            }
            _ => {
                if let Some((name, rhs)) = assigned_identifier(stmt)
                    && is_direct_call(rhs, "interface")
                {
                    names.insert(name.to_owned());
                }
            }
        }
    }

    names
}

fn redundant_name_call_kind(
    expr: &ExprP<starlark::syntax::ast::AstNoPayload>,
    interface_names: &InterfaceNames,
) -> Option<&'static str> {
    let target_name = direct_called_function_name(expr)?;
    match target_name {
        "io" | "input" | "output" => Some("io()"),
        "config" => Some("config()"),
        "Net" | "Power" | "Ground" | "NotConnected" => Some("Net()"),
        name if interface_names.contains(name) => Some("interface()"),
        _ => None,
    }
}

fn first_positional_arg(
    expr: &ExprP<starlark::syntax::ast::AstNoPayload>,
) -> Option<&starlark::syntax::ast::AstExpr> {
    nth_positional_arg(expr, 0)
}

fn nth_positional_arg(
    expr: &ExprP<starlark::syntax::ast::AstNoPayload>,
    index: usize,
) -> Option<&starlark::syntax::ast::AstExpr> {
    let ExprP::Call(_, args) = expr else {
        return None;
    };

    args.args
        .iter()
        .filter_map(|arg| match &arg.node {
            ArgumentP::Positional(expr) => Some(expr),
            _ => None,
        })
        .nth(index)
}

fn assigned_identifier(
    stmt: &starlark::codemap::Spanned<StmtP<starlark::syntax::ast::AstNoPayload>>,
) -> Option<(&str, &ExprP<starlark::syntax::ast::AstNoPayload>)> {
    let StmtP::Assign(assign) = &stmt.node else {
        return None;
    };
    let AssignTargetP::Identifier(ident) = &assign.lhs.node else {
        return None;
    };
    Some((ident.node.ident.as_str(), &assign.rhs.node))
}

fn direct_called_function_name(expr: &ExprP<starlark::syntax::ast::AstNoPayload>) -> Option<&str> {
    let ExprP::Call(fun, _) = expr else {
        return None;
    };

    match &fun.node {
        ExprP::Identifier(ident) => Some(ident.node.ident.as_str()),
        _ => None,
    }
}

fn is_direct_call(expr: &ExprP<starlark::syntax::ast::AstNoPayload>, name: &str) -> bool {
    direct_called_function_name(expr) == Some(name)
}
