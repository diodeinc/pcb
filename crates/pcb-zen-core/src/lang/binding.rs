//! Binding diagnostics for top-level rebinds.

use std::collections::HashMap;
use std::path::Path;

use starlark::codemap::{CodeMap, Span};
use starlark::errors::EvalSeverity;
use starlark::syntax::AstModule;
use starlark_syntax::syntax::ast::{AssignP, AstAssignTarget, AstStmt, Stmt};

use crate::{Diagnostic, DiagnosticReference};

/// Diagnostic emitted when a name is rebound in module scope.
pub const BINDING_REBIND: &str = "binding.rebind";

struct BindingChecker<'a> {
    path: &'a Path,
    codemap: CodeMap,
    diagnostics: Vec<Diagnostic>,
}

#[derive(Clone, Default)]
struct ScopeState {
    /// Names that are bound on at least one path reaching the current point.
    maybe_bound: HashMap<String, Span>,
}

impl ScopeState {
    fn bind(&mut self, name: &str, span: Span) {
        self.maybe_bound.insert(name.to_owned(), span);
    }

    fn merge(lhs: Self, rhs: Self) -> Self {
        let mut maybe_bound = lhs.maybe_bound;
        maybe_bound.extend(rhs.maybe_bound);

        Self { maybe_bound }
    }
}

impl<'a> BindingChecker<'a> {
    fn new(path: &'a Path, contents: &str) -> Self {
        Self {
            path,
            codemap: CodeMap::new(path.to_string_lossy().to_string(), contents.to_owned()),
            diagnostics: Vec::new(),
        }
    }

    fn check(mut self, ast: &AstModule) -> Vec<Diagnostic> {
        self.visit_stmt(ast.statement(), ScopeState::default());
        self.diagnostics
    }

    fn bind_name(&mut self, state: &mut ScopeState, name: &str, span: Span) {
        // `_` is conventionally used as a discard target, so repeated uses should
        // not participate in same-scope rebinding diagnostics.
        if name == "_" {
            return;
        }

        if let Some(previous) = state.maybe_bound.get(name).copied() {
            let previous = self.codemap.file_span(previous).resolve_span();
            let message = format!("Rebinding '{name}' in the same scope");
            self.diagnostics.push(
                Diagnostic::categorized(
                    &self.path.to_string_lossy(),
                    &message,
                    BINDING_REBIND,
                    EvalSeverity::Warning,
                )
                .with_span(Some(self.codemap.file_span(span).resolve_span()))
                .with_related(DiagnosticReference {
                    path: self.path.to_string_lossy().to_string(),
                    span: previous,
                    message: "Previous binding is here".to_string(),
                }),
            );
        }

        state.bind(name, span);
    }

    fn visit_lvalue(&mut self, state: &mut ScopeState, target: &AstAssignTarget) {
        target.visit_lvalue(|ident| self.bind_name(state, ident.ident.as_str(), ident.span));
    }

    fn visit_stmt(&mut self, stmt: &AstStmt, mut state: ScopeState) -> ScopeState {
        match &**stmt {
            Stmt::Statements(stmts) => {
                for stmt in stmts {
                    state = self.visit_stmt(stmt, state);
                }
                state
            }
            Stmt::Break
            | Stmt::Continue
            | Stmt::Pass
            | Stmt::Return(None)
            | Stmt::Return(Some(_))
            | Stmt::Expression(_) => state,
            Stmt::If(_, then_branch) => {
                ScopeState::merge(self.visit_stmt(then_branch, state.clone()), state)
            }
            Stmt::IfElse(_, branches) => ScopeState::merge(
                self.visit_stmt(&branches.0, state.clone()),
                self.visit_stmt(&branches.1, state),
            ),
            Stmt::Assign(AssignP { lhs, ty: _, rhs: _ }) => {
                self.visit_lvalue(&mut state, lhs);
                state
            }
            Stmt::AssignModify(_, _, _) => state,
            Stmt::Def(_) => state,
            Stmt::For(_) => state,
            Stmt::Load(_) => state,
        }
    }
}

pub fn check_bindings(ast: &AstModule, path: &Path, contents: &str) -> Vec<Diagnostic> {
    BindingChecker::new(path, contents).check(ast)
}

#[cfg(test)]
mod tests {
    use super::{BINDING_REBIND, check_bindings};
    use std::fs;
    use std::path::{Path, PathBuf};

    use crate::lang::error::CategorizedDiagnostic;
    use starlark::syntax::{AstModule, Dialect};

    fn stdlib_files(root: &Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(root).expect("read_dir failed") {
            let entry = entry.expect("dir entry failed");
            let path = entry.path();
            if path.is_dir() {
                stdlib_files(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "zen") {
                out.push(path);
            }
        }
    }

    #[test]
    fn stdlib_has_no_rebinding_warnings() {
        let mut dialect = Dialect::Extended;
        dialect.enable_f_strings = true;

        let stdlib_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../stdlib")
            .canonicalize()
            .expect("canonicalize stdlib path");

        let mut files = Vec::new();
        stdlib_files(&stdlib_root, &mut files);
        files.sort();

        let mut offenders = Vec::new();
        for path in files {
            let contents = fs::read_to_string(&path).expect("read stdlib file");
            let ast = AstModule::parse(
                path.to_str().expect("path is not valid utf-8"),
                contents.clone(),
                &dialect,
            )
            .unwrap_or_else(|err| panic!("failed to parse {}: {err}", path.display()));

            let diagnostics = check_bindings(&ast, &path, &contents);
            for diagnostic in diagnostics {
                let Some(kind) = diagnostic
                    .source_error
                    .as_ref()
                    .and_then(|err| err.downcast_ref::<CategorizedDiagnostic>())
                    .map(|cat| cat.kind.as_str())
                else {
                    continue;
                };
                if kind == BINDING_REBIND {
                    offenders.push(format!(
                        "{}: {}",
                        path.strip_prefix(&stdlib_root).unwrap_or(&path).display(),
                        diagnostic.body
                    ));
                }
            }
        }

        assert!(
            offenders.is_empty(),
            "stdlib rebinding diagnostics found:\n{}",
            offenders.join("\n")
        );
    }
}
