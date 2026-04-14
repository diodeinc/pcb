use std::collections::HashSet;

use starlark::syntax::{
    AstModule,
    ast::{ArgumentP, AssignTargetP, ExprP, LoadArgP, StmtP},
};
use starlark_syntax::syntax::{module::AstModuleFields, top_level_stmts::top_level_stmts};

use crate::{Diagnostic, lang::naming};

type InterfaceNames = HashSet<String>;

#[derive(Clone, Copy)]
enum DirectStyleCall {
    Io,
    Config,
    Net,
    Interface,
}

impl DirectStyleCall {
    fn naming_diagnostic(
        self,
        name: &str,
        span: starlark::codemap::ResolvedSpan,
        path: &std::path::Path,
    ) -> Option<Diagnostic> {
        match self {
            Self::Io => naming::check_io_naming(name, Some(span), path),
            Self::Config => naming::check_config_naming(name, Some(span), path),
            Self::Net | Self::Interface => None,
        }
    }

    fn redundant_name_label(self) -> &'static str {
        match self {
            Self::Io => "io()",
            Self::Config => "config()",
            Self::Net => "Net()",
            Self::Interface => "interface()",
        }
    }
}

pub(crate) fn ast_style_lints(ast: &AstModule) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let top_level = top_level_stmts(ast.statement());
    let interface_names = interface_names(&top_level);
    let path = std::path::Path::new(ast.codemap().filename());

    for stmt in &top_level {
        let StmtP::Assign(assign) = &stmt.node else {
            continue;
        };
        let AssignTargetP::Identifier(ident) = &assign.lhs.node else {
            continue;
        };
        let rhs = &assign.rhs.node;
        let Some(call) = classify_direct_style_call(rhs, &interface_names) else {
            continue;
        };
        let lhs_span = ast.codemap().file_span(assign.lhs.span).resolve_span();

        if let Some(diagnostic) = call.naming_diagnostic(ident.node.ident.as_str(), lhs_span, path)
        {
            diagnostics.push(diagnostic);
        }

        if let Some(diagnostic) =
            redundant_name_diagnostic(ast, rhs, call, ident.node.ident.as_str(), path)
        {
            diagnostics.push(diagnostic);
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

fn classify_direct_style_call(
    expr: &ExprP<starlark::syntax::ast::AstNoPayload>,
    interface_names: &InterfaceNames,
) -> Option<DirectStyleCall> {
    let target_name = direct_called_function_name(expr)?;
    match target_name {
        "io" | "input" | "output" => Some(DirectStyleCall::Io),
        "config" => Some(DirectStyleCall::Config),
        "Net" | "Power" | "Ground" | "NotConnected" | "Analog" | "Gpio" | "Pwm" => {
            Some(DirectStyleCall::Net)
        }
        name if interface_names.contains(name) => Some(DirectStyleCall::Interface),
        _ => None,
    }
}

fn redundant_name_diagnostic(
    ast: &AstModule,
    expr: &ExprP<starlark::syntax::ast::AstNoPayload>,
    call: DirectStyleCall,
    assigned_name: &str,
    path: &std::path::Path,
) -> Option<Diagnostic> {
    let first_arg = first_positional_arg(expr)?;
    let ExprP::Literal(starlark::syntax::ast::AstLiteral::String(explicit_name)) = &first_arg.node
    else {
        return None;
    };

    if explicit_name.node != assigned_name {
        return None;
    }

    let span = ast.codemap().file_span(first_arg.span).resolve_span();
    Some(naming::redundant_name_diagnostic(
        call.redundant_name_label(),
        &explicit_name.node,
        Some(span),
        path,
    ))
}

fn first_positional_arg(
    expr: &ExprP<starlark::syntax::ast::AstNoPayload>,
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
        .next()
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
