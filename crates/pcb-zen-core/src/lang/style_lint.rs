use std::collections::HashSet;

use starlark::syntax::{
    AstModule,
    ast::{
        ArgumentP, AssignTargetP, AstExpr, AstNoPayload, AstParameter, AstStmt, ClauseP, ExprP,
        LoadArgP, ParameterP, StmtP,
    },
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
    fn assignment_name_diagnostic(
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

    fn explicit_name_diagnostic(
        self,
        name: &str,
        span: starlark::codemap::ResolvedSpan,
        path: &std::path::Path,
    ) -> Option<Diagnostic> {
        match self {
            Self::Io => naming::check_uppercase_name(
                "io() name",
                naming::STYLE_NAMING_IO,
                name,
                Some(span),
                path,
            ),
            Self::Config => naming::check_snake_case_name(
                "config() name",
                naming::STYLE_NAMING_CONFIG,
                name,
                Some(span),
                path,
            ),
            Self::Net => naming::check_uppercase_name(
                "Net() name",
                naming::STYLE_NAMING_NET,
                name,
                Some(span),
                path,
            ),
            Self::Interface => naming::check_uppercase_name(
                "interface() name",
                naming::STYLE_NAMING_INTERFACE,
                name,
                Some(span),
                path,
            ),
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
    let top_level = top_level_stmts(ast.statement());
    let interface_names = interface_names(&top_level);
    let mut linter = StyleLinter {
        ast,
        path: std::path::Path::new(ast.codemap().filename()),
        interface_names,
        diagnostics: Vec::new(),
    };

    for stmt in &top_level {
        linter.lint_stmt(stmt);
    }

    linter.diagnostics
}

pub(crate) fn is_ast_style_diagnostic(diagnostic: &Diagnostic) -> bool {
    diagnostic
        .downcast_error_ref::<crate::lang::error::CategorizedDiagnostic>()
        .is_some_and(|categorized| categorized.kind.starts_with("style."))
}

struct StyleLinter<'a> {
    ast: &'a AstModule,
    path: &'a std::path::Path,
    interface_names: InterfaceNames,
    diagnostics: Vec<Diagnostic>,
}

impl StyleLinter<'_> {
    fn lint_stmt(&mut self, stmt: &AstStmt) {
        match &stmt.node {
            StmtP::Break | StmtP::Continue | StmtP::Pass | StmtP::Load(_) => {}
            StmtP::Return(expr) => {
                if let Some(expr) = expr {
                    self.lint_expr(expr);
                }
            }
            StmtP::Expression(expr) => self.lint_expr(expr),
            StmtP::Assign(assign) => {
                if let Some((assigned_name, lhs_span)) =
                    assigned_identifier(&assign.lhs, self.ast.codemap())
                {
                    self.lint_assignment_call(assigned_name, lhs_span, &assign.rhs);
                }
                self.lint_expr(&assign.rhs);
            }
            StmtP::AssignModify(_, _, expr) => self.lint_expr(expr),
            StmtP::Statements(stmts) => {
                for stmt in stmts {
                    self.lint_stmt(stmt);
                }
            }
            StmtP::If(condition, body) => {
                self.lint_expr(condition);
                self.lint_stmt(body);
            }
            StmtP::IfElse(condition, branches) => {
                self.lint_expr(condition);
                self.lint_stmt(&branches.0);
                self.lint_stmt(&branches.1);
            }
            StmtP::For(for_stmt) => {
                self.lint_expr(&for_stmt.over);
                self.lint_stmt(&for_stmt.body);
            }
            StmtP::Def(def) => {
                for param in &def.params {
                    self.lint_param(param);
                }
                self.lint_stmt(&def.body);
            }
        }
    }

    fn lint_param(&mut self, param: &AstParameter) {
        if let ParameterP::Normal(_, _, Some(default)) = &param.node {
            self.lint_expr(default);
        }
    }

    fn lint_expr(&mut self, expr: &AstExpr) {
        self.lint_explicit_name(expr);

        match &expr.node {
            ExprP::Identifier(_) | ExprP::Literal(_) => {}
            ExprP::Tuple(exprs) | ExprP::List(exprs) => {
                for expr in exprs {
                    self.lint_expr(expr);
                }
            }
            ExprP::Dot(expr, _)
            | ExprP::Not(expr)
            | ExprP::Minus(expr)
            | ExprP::Plus(expr)
            | ExprP::BitNot(expr) => self.lint_expr(expr),
            ExprP::Call(fun, args) => {
                self.lint_expr(fun);
                for arg in &args.args {
                    self.lint_expr(arg.node.expr());
                }
            }
            ExprP::Index(exprs) => {
                self.lint_expr(&exprs.0);
                self.lint_expr(&exprs.1);
            }
            ExprP::Index2(exprs) => {
                self.lint_expr(&exprs.0);
                self.lint_expr(&exprs.1);
                self.lint_expr(&exprs.2);
            }
            ExprP::Slice(expr, start, stop, step) => {
                self.lint_expr(expr);
                for part in [start.as_deref(), stop.as_deref(), step.as_deref()]
                    .into_iter()
                    .flatten()
                {
                    self.lint_expr(part);
                }
            }
            ExprP::Lambda(lambda) => self.lint_expr(&lambda.body),
            ExprP::Op(left, _, right) => {
                self.lint_expr(left);
                self.lint_expr(right);
            }
            ExprP::If(exprs) => {
                self.lint_expr(&exprs.0);
                self.lint_expr(&exprs.1);
                self.lint_expr(&exprs.2);
            }
            ExprP::Dict(entries) => {
                for (key, value) in entries {
                    self.lint_expr(key);
                    self.lint_expr(value);
                }
            }
            ExprP::ListComprehension(expr, for_clause, clauses) => {
                self.lint_expr(expr);
                self.lint_expr(&for_clause.over);
                for clause in clauses {
                    self.lint_clause(clause);
                }
            }
            ExprP::DictComprehension(entry, for_clause, clauses) => {
                self.lint_expr(&entry.0);
                self.lint_expr(&entry.1);
                self.lint_expr(&for_clause.over);
                for clause in clauses {
                    self.lint_clause(clause);
                }
            }
            ExprP::FString(fstring) => {
                for expr in &fstring.expressions {
                    self.lint_expr(expr);
                }
            }
        }
    }

    fn lint_clause(&mut self, clause: &ClauseP<AstNoPayload>) {
        match clause {
            ClauseP::For(for_clause) => self.lint_expr(&for_clause.over),
            ClauseP::If(expr) => self.lint_expr(expr),
        }
    }

    fn lint_assignment_call(
        &mut self,
        assigned_name: &str,
        lhs_span: starlark::codemap::ResolvedSpan,
        expr: &AstExpr,
    ) {
        let Some(call) = classify_direct_style_call(&expr.node, &self.interface_names) else {
            return;
        };

        if let Some(diagnostic) =
            call.assignment_name_diagnostic(assigned_name, lhs_span, self.path)
        {
            self.diagnostics.push(diagnostic);
        }

        if let Some(diagnostic) =
            redundant_name_diagnostic(self.ast, &expr.node, call, assigned_name, self.path)
        {
            self.diagnostics.push(diagnostic);
        }
    }

    fn lint_explicit_name(&mut self, expr: &AstExpr) {
        let Some(call) = classify_direct_style_call(&expr.node, &self.interface_names) else {
            return;
        };
        let Some(first_arg) = first_positional_arg(&expr.node) else {
            return;
        };
        let ExprP::Literal(starlark::syntax::ast::AstLiteral::String(explicit_name)) =
            &first_arg.node
        else {
            return;
        };

        let span = self.ast.codemap().file_span(first_arg.span).resolve_span();
        if let Some(diagnostic) =
            call.explicit_name_diagnostic(&explicit_name.node, span, self.path)
        {
            self.diagnostics.push(diagnostic);
        }
    }
}

fn interface_names(top_level: &[&AstStmt]) -> InterfaceNames {
    let mut names = HashSet::new();

    for stmt in top_level {
        match &stmt.node {
            StmtP::Load(load) if load.module.node.ends_with("interfaces.zen") => {
                for LoadArgP { local, .. } in &load.args {
                    names.insert(local.node.ident.to_owned());
                }
            }
            _ => {
                if let Some((name, rhs)) = assigned_identifier_expr(stmt)
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
    expr: &ExprP<AstNoPayload>,
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
    expr: &ExprP<AstNoPayload>,
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

fn first_positional_arg(expr: &ExprP<AstNoPayload>) -> Option<&AstExpr> {
    let ExprP::Call(_, args) = expr else {
        return None;
    };

    args.args.iter().find_map(|arg| match &arg.node {
        ArgumentP::Positional(expr) => Some(expr),
        _ => None,
    })
}

fn assigned_identifier_expr(stmt: &AstStmt) -> Option<(&str, &ExprP<AstNoPayload>)> {
    let StmtP::Assign(assign) = &stmt.node else {
        return None;
    };
    let AssignTargetP::Identifier(ident) = &assign.lhs.node else {
        return None;
    };
    Some((ident.node.ident.as_str(), &assign.rhs.node))
}

fn assigned_identifier<'a>(
    target: &'a starlark::syntax::ast::AstAssignTargetP<AstNoPayload>,
    codemap: &starlark::codemap::CodeMap,
) -> Option<(&'a str, starlark::codemap::ResolvedSpan)> {
    let AssignTargetP::Identifier(ident) = &target.node else {
        return None;
    };
    Some((
        ident.node.ident.as_str(),
        codemap.file_span(target.span).resolve_span(),
    ))
}

fn direct_called_function_name(expr: &ExprP<AstNoPayload>) -> Option<&str> {
    let ExprP::Call(fun, _) = expr else {
        return None;
    };

    match &fun.node {
        ExprP::Identifier(ident) => Some(ident.node.ident.as_str()),
        _ => None,
    }
}

fn is_direct_call(expr: &ExprP<AstNoPayload>, name: &str) -> bool {
    direct_called_function_name(expr) == Some(name)
}
