#![allow(clippy::needless_lifetimes)]

use crate::lang::evaluator_ext::EvaluatorExt;
use crate::lang::input::InputMap;
use crate::lang::module::{FrozenModuleValue, ModuleLoader};
use crate::Diagnostic;
use allocative::Allocative;
use starlark::environment::GlobalsBuilder;
use starlark::errors::EvalSeverity;
use starlark::{
    any::ProvidesStaticType,
    collections::SmallMap,
    eval::Evaluator,
    starlark_complex_value, starlark_module,
    values::{
        list::ListRef, starlark_value, Coerce, Freeze, FreezeResult, NoSerialize, StarlarkValue,
        Trace, Value, ValueLifetimeless, ValueLike,
    },
};

/// TestBench value that can evaluate modules without requiring inputs
#[derive(Clone, Coerce, Trace, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub struct TestBenchValueGen<V: ValueLifetimeless> {
    /// Name of this TestBench instance
    name: String,
    /// The module loader that was used
    #[freeze(identity)]
    module_loader: ModuleLoader,
    /// The evaluated module (None if evaluation failed)
    #[freeze(identity)]
    evaluated_module: Option<FrozenModuleValue>,
    /// Additional properties that might be stored
    properties: SmallMap<String, V>,
    /// Results from running check functions
    check_results: Vec<V>,
}

starlark_complex_value!(pub TestBenchValue);

#[starlark_value(type = "TestBench")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for TestBenchValueGen<V> where
    Self: ProvidesStaticType<'v>
{
}

impl<'v, V: ValueLike<'v>> std::fmt::Display for TestBenchValueGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TestBench({})", self.name)
    }
}

impl<'v, V: ValueLike<'v>> std::fmt::Debug for TestBenchValueGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = f.debug_struct("TestBench");
        debug.field("name", &self.name);
        debug.field("module", &self.module_loader.name);
        debug.field("evaluated", &self.evaluated_module.is_some());
        debug.finish()
    }
}

impl<'v, V: ValueLike<'v>> TestBenchValueGen<V> {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn module_loader(&self) -> &ModuleLoader {
        &self.module_loader
    }

    pub fn evaluated_module(&self) -> Option<&FrozenModuleValue> {
        self.evaluated_module.as_ref()
    }

    pub fn check_results(&self) -> &Vec<V> {
        &self.check_results
    }
}

/// Extension to ModuleLoader for TestBench evaluation
impl ModuleLoader {
    /// Evaluate this module for a TestBench with non-strict input requirements
    pub fn evaluate_for_testbench<'v>(
        &self,
        testbench_name: String,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Option<FrozenModuleValue>> {
        // Create a child context with strict_io_config = false
        let ctx = eval
            .eval_context()
            .expect("expected eval context")
            .child_context()
            .set_strict_io_config(false); // KEY: Allow missing required inputs

        let ctx = ctx
            .set_source_path(std::path::PathBuf::from(&self.source_path))
            .set_module_name(testbench_name)
            .set_inputs(InputMap::new()); // Empty inputs

        let (output, diagnostics) = ctx.eval().unpack();

        // Get the parent context for diagnostic propagation
        let parent_context = eval
            .module()
            .extra_value()
            .and_then(|extra| extra.downcast_ref::<crate::lang::context::ContextValue>())
            .ok_or_else(|| anyhow::anyhow!("unexpected context - ContextValue not found"))?;

        let call_site = eval.call_stack_top_location();

        // Propagate diagnostics from the testbench module
        for child in diagnostics.into_iter() {
            let diag_to_add = if let Some(cs) = &call_site {
                // Wrap diagnostics with call-site context
                let (severity, message) = match child.severity {
                    EvalSeverity::Error => (
                        EvalSeverity::Error,
                        format!("Error in TestBench module `{}`", self.name),
                    ),
                    EvalSeverity::Warning => (
                        EvalSeverity::Warning,
                        format!("Warning from TestBench module `{}`", self.name),
                    ),
                    other => (other, format!("Issue in TestBench module `{}`", self.name)),
                };

                Diagnostic {
                    path: cs.filename().to_string(),
                    span: Some(cs.resolve_span()),
                    severity,
                    body: message,
                    call_stack: Some(eval.call_stack().clone()),
                    child: Some(Box::new(child)),
                    source_error: None,
                }
            } else {
                child
            };

            // Propagate the diagnostic upwards
            parent_context.add_diagnostic(diag_to_add);
        }

        match output {
            Some(output) => {
                // Add a reference to the dependent module's frozen heap so it stays alive
                eval.frozen_heap()
                    .add_reference(output.star_module.frozen_heap());

                // NOTE: We do NOT add the evaluated module as a child to the parent context
                // This is a key difference from normal module invocation

                Ok(Some(output.sch_module))
            }
            None => {
                // Module evaluation failed, but we still return Ok with None
                // The diagnostics have already been propagated
                Ok(None)
            }
        }
    }
}

/// Execute a single check function and handle the result
fn execute_check<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    check_func: Value<'v>,
    args: &[Value<'v>],
    testbench_name: &str,
) -> anyhow::Result<(Value<'v>, bool)> {
    match eval.eval_function(check_func, args, &[]) {
        Ok(result) => Ok((result, false)), // Success, no failure
        Err(e) => {
            let check_func_str = check_func.to_string();
            let check_name = check_func_str.rsplit('.').next().unwrap_or("check");
            let ctx = eval.context_value().unwrap();
            let testbench_location = eval.call_stack_top_location().unwrap();

            // Extract clean error message
            let error_string = e.to_string();
            let error_msg = error_string
                .lines()
                .find(|line| line.starts_with("error: "))
                .and_then(|line| line.strip_prefix("error: "))
                .unwrap_or("check failed");

            // Child diagnostic for the specific check location
            let child = e.span().map(|span| {
                Box::new(Diagnostic {
                    path: span.file.filename().to_string(),
                    span: Some(span.resolve_span()),
                    severity: EvalSeverity::Error,
                    body: error_msg.to_string(),
                    call_stack: None,
                    child: None,
                    source_error: None,
                })
            });

            // Parent diagnostic for TestBench context
            ctx.add_diagnostic(Diagnostic {
                path: testbench_location.filename().to_string(),
                span: Some(testbench_location.resolve_span()),
                severity: EvalSeverity::Error,
                body: format!(
                    "TestBench '{}' check '{}' failed",
                    testbench_name, check_name
                ),
                call_stack: Some(eval.call_stack().clone()),
                child,
                source_error: None,
            });

            Ok((eval.heap().alloc(false).to_value(), true)) // Failure
        }
    }
}

#[starlark_module]
pub fn testbench_globals(builder: &mut GlobalsBuilder) {
    /// Create a TestBench that can evaluate modules without required inputs
    fn TestBench<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] module: Value<'v>,
        #[starlark(require = named)] checks: Option<Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        // Extract ModuleLoader from the module parameter
        let loader = module.downcast_ref::<ModuleLoader>().ok_or_else(|| {
            anyhow::anyhow!("'module' parameter must be a ModuleLoader (created with Module())")
        })?;

        // Evaluate the module with non-strict mode
        let evaluated_module = loader.evaluate_for_testbench(name.clone(), eval)?;

        // Execute check functions if provided
        let mut check_count = 0;
        let mut failed_count = 0;

        if let (Some(checks_value), Some(ref module)) = (checks, &evaluated_module) {
            let checks_list = ListRef::from_value(checks_value)
                .ok_or_else(|| anyhow::anyhow!("'checks' parameter must be a list of functions"))?;

            // Use frozen_heap to allocate the FrozenModuleValue, like in module.rs
            let module_value = eval.frozen_heap().alloc(module.clone()).to_value();
            let args = [module_value];

            for check_func in checks_list.iter() {
                let (_result, failed) = execute_check(eval, check_func, &args, &name)?;
                check_count += 1;
                if failed {
                    failed_count += 1;
                }
            }
        }

        // Log and print results
        log::info!("TestBench '{}': {} checks executed", name, check_count);

        if failed_count == 0 && check_count > 0 {
            let check_word = if check_count == 1 { "check" } else { "checks" };
            println!(
                "\x1b[1m\x1b[32m✓ {}\x1b[0m: {} {} passed",
                name, check_count, check_word
            );
        }

        // Create and return the TestBenchValue
        let testbench = TestBenchValueGen::<Value> {
            name,
            module_loader: loader.clone(),
            evaluated_module,
            properties: SmallMap::new(),
            check_results: Vec::new(), // No longer needed
        };

        Ok(eval.heap().alloc(testbench))
    }
}
