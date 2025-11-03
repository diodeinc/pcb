use std::sync::Arc;

use starlark::environment::GlobalsBuilder;
use starlark::errors::EvalSeverity;
use starlark::eval::Evaluator;
use starlark::starlark_module;
use starlark::values::Value;

use crate::lang::error::CategorizedDiagnostic;
use crate::lang::evaluator_ext::EvaluatorExt;
use crate::Diagnostic;

/// Helper to create source_error from kind string
fn make_source_error(msg: &str, kind: Option<String>) -> Option<Arc<anyhow::Error>> {
    kind.and_then(|k| {
        CategorizedDiagnostic::new(msg.to_string(), k)
            .ok()
            .map(|c| Arc::new(anyhow::Error::new(c)))
    })
}

/// Helper to create a diagnostic with kind
fn make_diagnostic(
    eval: &Evaluator,
    msg: String,
    severity: EvalSeverity,
    suppressed: bool,
    kind: Option<String>,
) -> Diagnostic {
    let (path, span) = eval
        .call_stack_top_location()
        .map(|loc| (loc.file.filename().to_string(), Some(loc.resolve_span())))
        .unwrap_or_else(|| (eval.source_path().unwrap_or_default(), None));

    Diagnostic {
        path,
        span,
        severity,
        body: msg.clone(),
        call_stack: None,
        child: None,
        source_error: make_source_error(&msg, kind),
        suppressed,
    }
}

#[starlark_module]
pub(crate) fn assert_globals(builder: &mut GlobalsBuilder) {
    fn error<'v>(
        #[starlark(require = pos)] msg: String,
        suppress: Option<bool>,
        kind: Option<String>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        if suppress.unwrap_or(false) {
            eval.add_diagnostic(make_diagnostic(eval, msg, EvalSeverity::Error, true, kind));
            Ok(Value::new_none())
        } else {
            Err(anyhow::anyhow!(msg))
        }
    }

    fn warn<'v>(
        #[starlark(require = pos)] msg: String,
        suppress: Option<bool>,
        kind: Option<String>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let suppressed = suppress.unwrap_or(false);
        eval.add_diagnostic(make_diagnostic(
            eval,
            msg,
            EvalSeverity::Warning,
            suppressed,
            kind,
        ));
        Ok(Value::new_none())
    }

    /// Check that a condition holds. If `cond` is false, raise an error with `msg`.
    fn check<'v>(
        #[starlark(require = pos)] cond: bool,
        #[starlark(require = pos)] msg: String,
    ) -> anyhow::Result<Value<'v>> {
        if cond {
            Ok(Value::new_none())
        } else {
            Err(anyhow::anyhow!(msg))
        }
    }
}
