use starlark::environment::GlobalsBuilder;
use starlark::errors::EvalSeverity;
use starlark::eval::Evaluator;
use starlark::starlark_module;
use starlark::values::Value;

use crate::lang::evaluator_ext::EvaluatorExt;
use crate::Diagnostic;

/// Miscellaneous built-in Starlark helpers used by Diode.
///
/// Currently this exposes:
///  • error(msg): unconditionally raises a runtime error with the provided message.
///  • warn(msg): emits a warning diagnostic and continues execution.
///  • check(cond, msg): raises an error with `msg` when `cond` is false.
#[starlark_module]
pub(crate) fn assert_globals(builder: &mut GlobalsBuilder) {
    /// Raise a runtime error with the given message.
    fn error<'v>(#[starlark(require = pos)] msg: String) -> anyhow::Result<Value<'v>> {
        Err(anyhow::anyhow!(msg))
    }

    /// Emit a warning diagnostic with the given message and continue execution.
    fn warn<'v>(
        #[starlark(require = pos)] msg: String,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        // Get the call site location and span information
        let (path, span) = if let Some(location) = eval.call_stack_top_location() {
            let resolved_span = location.resolve_span();
            (location.file.filename().to_string(), Some(resolved_span))
        } else {
            // Fallback if no call stack location is available
            (eval.source_path().unwrap_or_default(), None)
        };

        let diagnostic = Diagnostic {
            path,
            span,
            severity: EvalSeverity::Warning,
            body: msg,
            call_stack: None,
            child: None,
            source_error: None,
        };

        eval.add_diagnostic(diagnostic);
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
