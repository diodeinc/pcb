#![allow(clippy::needless_lifetimes)]

use allocative::Allocative;
use starlark::collections::SmallMap;
use starlark::eval::Evaluator;
use starlark::values::{starlark_value, ValueLike};
use starlark::{
    any::ProvidesStaticType,
    starlark_complex_value,
    values::{Coerce, Freeze, NoSerialize, StarlarkValue, Trace, Value, ValueLifetimeless},
};

use crate::Diagnostic;

#[derive(Clone, Coerce, Trace, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub struct ElectricalCheckGen<V: ValueLifetimeless> {
    pub name: String,
    pub inputs: SmallMap<String, V>,
    pub check_func: V,
    pub severity: String,
    pub source_path: String,
    #[freeze(identity)]
    #[allocative(skip)]
    pub call_span: Option<starlark::codemap::ResolvedSpan>,
}

starlark_complex_value!(pub ElectricalCheck);

#[starlark_value(type = "ElectricalCheck")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for ElectricalCheckGen<V> where
    Self: ProvidesStaticType<'v>
{
}

impl<'v, V: ValueLike<'v>> std::fmt::Display for ElectricalCheckGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ElectricalCheck({})", self.name)
    }
}

impl<'v, V: ValueLike<'v>> std::fmt::Debug for ElectricalCheckGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ElectricalCheck")
            .field("name", &self.name)
            .field("inputs", &self.inputs.len())
            .finish()
    }
}

/// Execute a single electrical check and create diagnostic
pub fn execute_electrical_check<'v, V: ValueLike<'v>>(
    eval: &mut Evaluator<'v, '_, '_>,
    check: &ElectricalCheckGen<V>,
    module_value: Value<'v>,
) -> Diagnostic {
    use starlark::errors::EvalSeverity;

    let kwargs: Vec<_> = check
        .inputs
        .iter()
        .map(|(k, v)| (k.as_str(), v.to_value()))
        .collect();

    let result = eval.eval_function(check.check_func.to_value(), &[module_value], &kwargs);
    let passed = result.is_ok();

    let failure_severity = match check.severity.as_str() {
        "warning" => EvalSeverity::Warning,
        "advice" => EvalSeverity::Advice,
        _ => EvalSeverity::Error,
    };

    Diagnostic {
        path: check.source_path.clone(),
        span: check.call_span,
        severity: if passed {
            EvalSeverity::Advice
        } else {
            failure_severity
        },
        body: format!(
            "Electrical check '{}' {}",
            check.name,
            if passed { "passed" } else { "failed" }
        ),
        call_stack: None,
        child: result.err().map(|e| Box::new(Diagnostic::from(e))),
        source_error: None,
        suppressed: false,
    }
}
