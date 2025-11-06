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
    // For warn() and error(), convert the call stack into a diagnostic chain.
    // This shows the full trace of where the warning originated, not just the
    // warn() call itself.
    let call_stack = eval.call_stack();

    // Collect all frames with locations, skipping frames without locations (like lambda)
    let frames_with_loc: Vec<_> = call_stack
        .frames
        .iter()
        .filter_map(|frame| {
            frame
                .location
                .as_ref()
                .map(|loc| (frame.name.as_str(), loc))
        })
        .collect();

    // Skip the last frame (the warn/error call itself) since we want to show
    // where the warn/error was called FROM, not the warn/error function itself.
    // For example, if foo() calls bar() calls warn(), we want to show bar() and foo().
    let frames_to_use = if frames_with_loc.len() > 1 {
        &frames_with_loc[..frames_with_loc.len() - 1]
    } else {
        &frames_with_loc[..]
    };

    if frames_to_use.is_empty() {
        // Fall back if no frames with location
        let (path, span) = eval
            .call_stack_top_location()
            .map(|loc| (loc.file.filename().to_string(), Some(loc.resolve_span())))
            .unwrap_or_else(|| (eval.source_path().unwrap_or_default(), None));

        return Diagnostic {
            path,
            span,
            severity,
            body: msg.clone(),
            call_stack: None,
            child: None,
            source_error: make_source_error(&msg, kind),
            suppressed,
        };
    }

    // Build diagnostic chain from innermost to outermost.
    // Call stack is ordered [outermost, ..., innermost].
    // After skipping warn/error, the LAST frame is the innermost (direct caller).
    // We iterate in reverse so we build from innermost outward.
    let mut current: Option<Diagnostic> = None;
    let innermost_idx = frames_to_use.len() - 1;
    let source_error = make_source_error(&msg, kind);

    for (i, (frame_name, loc)) in frames_to_use.iter().enumerate().rev() {
        let is_innermost = i == innermost_idx;

        current = Some(Diagnostic {
            path: loc.file.filename().to_string(),
            span: Some(loc.resolve_span()),
            severity,
            body: if is_innermost {
                msg.clone()
            } else {
                format!("In `{}`", frame_name)
            },
            call_stack: None,
            child: if is_innermost {
                None
            } else {
                current.map(Box::new)
            },
            source_error: if is_innermost {
                source_error.clone()
            } else {
                None
            },
            suppressed,
        });
    }

    current.expect("frames_to_use is not empty, so current should be Some")
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
