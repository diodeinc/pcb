use starlark::environment::GlobalsBuilder;
use starlark::starlark_module;
use starlark::values::Value;

/// Miscellaneous built-in Starlark helpers used by Diode.
///
/// Currently this exposes:
///  • error(msg): unconditionally raises a runtime error with the provided message.
///  • check(cond, msg): raises an error with `msg` when `cond` is false.
#[starlark_module]
pub(crate) fn assert_globals(builder: &mut GlobalsBuilder) {
    /// Raise a runtime error with the given message.
    fn error<'v>(#[starlark(require = pos)] msg: String) -> anyhow::Result<Value<'v>> {
        Err(anyhow::anyhow!(msg))
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
