use std::{
    fmt::Display,
    ops::{Deref, DerefMut},
};

use serde::ser::SerializeStruct;
use starlark::{
    codemap::ResolvedSpan,
    errors::{EvalMessage, EvalSeverity},
    eval::CallStack,
};

/// A wrapper error type that carries a Diagnostic through the starlark error chain.
/// This allows us to preserve the full diagnostic information when errors cross
/// module boundaries during load() operations.
#[derive(Debug, Clone)]
pub struct DiagnosticError(pub Diagnostic);

impl Display for DiagnosticError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Just display the inner diagnostic
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for DiagnosticError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        None
    }
}

/// Wrapper error that has DiagnosticError as its source, allowing it to be
/// discovered through the error chain.
#[derive(Debug)]
pub struct LoadError {
    pub message: String,
    pub diagnostic: DiagnosticError,
}

impl Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for LoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.diagnostic)
    }
}

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub path: String,
    pub span: Option<ResolvedSpan>,
    pub severity: EvalSeverity,
    pub body: String,
    pub call_stack: Option<CallStack>,

    /// Optional child diagnostic representing a nested error that occurred in a
    /// downstream (e.g. loaded) module.  When present, this allows callers to
    /// reconstruct a chain of diagnostics across module/evaluation boundaries
    /// without needing to rely on parsing rendered strings.
    pub child: Option<Box<Diagnostic>>,
}

impl From<starlark::Error> for Diagnostic {
    fn from(err: starlark::Error) -> Self {
        // Check the source chain of the error kind
        if let Some(source) = err.kind().source() {
            let mut current: Option<&(dyn std::error::Error + 'static)> = Some(source);
            while let Some(src) = current {
                // Check if this source is our DiagnosticError
                if let Some(diag_err) = src.downcast_ref::<DiagnosticError>() {
                    return diag_err.0.clone();
                }
                current = src.source();
            }
        }

        // No hidden diagnostic found - create one from the starlark error
        Self {
            path: err
                .span()
                .map(|span| span.file.filename().to_string())
                .unwrap_or_default(),
            span: err.span().map(|span| span.resolve_span()),
            severity: EvalSeverity::Error,
            body: err.kind().to_string(),
            call_stack: Some(err.call_stack().clone()),
            child: None,
        }
    }
}

impl From<EvalMessage> for Diagnostic {
    fn from(msg: EvalMessage) -> Self {
        Self {
            path: msg.path,
            span: msg.span,
            severity: msg.severity,
            body: msg.description,
            call_stack: None,
            child: None,
        }
    }
}

impl From<anyhow::Error> for Diagnostic {
    fn from(err: anyhow::Error) -> Self {
        Self::from(starlark::Error::from(err))
    }
}

impl serde::Serialize for Diagnostic {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("Diagnostic", 6)?;
        state.serialize_field("path", &self.path)?;
        state.serialize_field("span", &self.span.map(|span| span.to_string()))?;
        state.serialize_field("severity", &self.severity)?;
        state.serialize_field("body", &self.body)?;
        state.serialize_field(
            "call_stack",
            &self.call_stack.as_ref().map(|stack| stack.to_string()),
        )?;
        state.serialize_field("child", &self.child)?;
        state.end()
    }
}

impl Diagnostic {
    pub fn with_child(self, child: Diagnostic) -> Self {
        Self {
            child: Some(Box::new(child)),
            ..self
        }
    }

    /// Return `true` if the diagnostic severity is `Error`.
    pub fn is_error(&self) -> bool {
        matches!(self.severity, EvalSeverity::Error)
    }
}

impl Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Format: "Error: path:line:col-line:col message"
        write!(f, "{}: ", self.severity)?;

        if !self.path.is_empty() {
            write!(f, "{}", self.path)?;
            if let Some(span) = &self.span {
                write!(f, ":{span}")?;
            }
            write!(f, " ")?;
        }

        write!(f, "{}", self.body)?;

        let mut current = &self.child;
        while let Some(diag) = current {
            write!(f, "\n{}: ", diag.severity)?;

            if !diag.path.is_empty() {
                write!(f, "{}", diag.path)?;
                if let Some(span) = &diag.span {
                    write!(f, ":{span}")?;
                }
                write!(f, " ")?;
            }

            write!(f, "{}", diag.body)?;
            current = &diag.child;
        }
        Ok(())
    }
}

impl std::error::Error for Diagnostic {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        // We don't have a source error, as Diagnostic is our root error type
        None
    }
}

#[derive(Debug, Clone)]
pub struct WithDiagnostics<T> {
    pub diagnostics: Diagnostics,
    pub output: Option<T>,
}

#[derive(Debug, Clone, Default)]
pub struct Diagnostics {
    pub diagnostics: Vec<Diagnostic>,
}

impl Deref for Diagnostics {
    type Target = Vec<Diagnostic>;
    fn deref(&self) -> &Self::Target {
        &self.diagnostics
    }
}

impl DerefMut for Diagnostics {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.diagnostics
    }
}

impl IntoIterator for Diagnostics {
    type Item = Diagnostic;
    type IntoIter = std::vec::IntoIter<Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        self.diagnostics.into_iter()
    }
}

impl<T: Display> Display for WithDiagnostics<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(output) = &self.output {
            write!(f, "{output}")?;
        }
        for diagnostic in self.diagnostics.deref() {
            write!(f, "{diagnostic}")?;
        }
        Ok(())
    }
}

impl<T> Default for WithDiagnostics<T> {
    fn default() -> Self {
        Self {
            diagnostics: Diagnostics::default(),
            output: None,
        }
    }
}

impl<T> WithDiagnostics<T> {
    pub fn success(output: T) -> Self {
        Self {
            diagnostics: Diagnostics::default(),
            output: Some(output),
        }
    }

    pub fn push(&mut self, diag: Diagnostic) {
        self.diagnostics.push(diag);
    }

    pub fn extend<I: IntoIterator<Item = Diagnostic>>(&mut self, diagnostics: I) {
        self.diagnostics.extend(diagnostics);
    }

    /// Return `true` if evaluation produced an output **and** did not emit
    /// any error-level diagnostics.
    pub fn is_success(&self) -> bool {
        self.output.is_some() && !self.diagnostics.has_errors()
    }

    pub fn map<U>(mut self, f: impl FnOnce(T) -> U) -> WithDiagnostics<U> {
        if let Some(output) = self.output.take() {
            return WithDiagnostics {
                diagnostics: self.diagnostics,
                output: Some(f(output)),
            };
        }
        WithDiagnostics {
            diagnostics: self.diagnostics,
            output: None,
        }
    }

    pub fn try_map<U, D: Into<Diagnostic>>(
        mut self,
        f: impl FnOnce(T) -> Result<U, D>,
    ) -> WithDiagnostics<U> {
        if let Some(output) = self.output.take() {
            match f(output) {
                Ok(output) => {
                    return WithDiagnostics {
                        diagnostics: self.diagnostics,
                        output: Some(output),
                    }
                }
                Err(diag) => self.diagnostics.push(diag.into()),
            }
        }
        WithDiagnostics {
            diagnostics: self.diagnostics,
            output: None,
        }
    }

    pub fn inspect_mut<O, F: FnOnce(&mut T) -> O>(mut self, f: F) -> Self {
        if let Some(output) = self.output.as_mut() {
            f(output);
        }
        self
    }

    pub fn output_result(self) -> Result<T, Diagnostics> {
        self.into()
    }

    pub fn unpack(self) -> (Option<T>, Diagnostics) {
        (self.output, self.diagnostics)
    }

    pub fn is_empty(&self) -> bool {
        self.output.is_none() && self.diagnostics.is_empty()
    }
}

impl Diagnostics {
    pub fn errors(&self) -> Vec<Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|diag| matches!(diag.severity, EvalSeverity::Error))
            .cloned()
            .collect()
    }

    pub fn warnings(&self) -> Vec<Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|diag| matches!(diag.severity, EvalSeverity::Warning))
            .cloned()
            .collect()
    }

    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(|diag| diag.is_error())
    }
}

impl From<Vec<Diagnostic>> for Diagnostics {
    fn from(diagnostics: Vec<Diagnostic>) -> Self {
        Diagnostics { diagnostics }
    }
}

impl<T> From<WithDiagnostics<T>> for Result<T, Diagnostics> {
    fn from(mut eval: WithDiagnostics<T>) -> Self {
        if eval.is_success() {
            Ok(eval.output.take().unwrap())
        } else {
            Err(eval.diagnostics)
        }
    }
}

impl<T, D: Into<Diagnostic>> From<D> for WithDiagnostics<T> {
    fn from(diagnostic: D) -> Self {
        WithDiagnostics {
            diagnostics: vec![diagnostic.into()].into(),
            output: None,
        }
    }
}
