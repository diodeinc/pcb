#![allow(clippy::needless_lifetimes)]

use crate::lang::evaluator_ext::EvaluatorExt;
use crate::lang::input::{InputMap, InputValue};
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
        dict::{AllocDict, DictRef},
        list::ListRef,
        starlark_value, Coerce, Freeze, FreezeResult, NoSerialize, StarlarkValue, Trace, Value,
        ValueLifetimeless, ValueLike,
    },
};

/// Result from a single test case evaluation
#[derive(Clone, Coerce, Trace, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub struct TestCaseResultGen<V: ValueLifetimeless> {
    /// The test case parameters that were provided
    pub params: SmallMap<String, V>,
    /// The evaluated module (None if evaluation failed)
    #[freeze(identity)]
    pub evaluated: Option<FrozenModuleValue>,
    /// Results from running check functions for this case
    pub check_results: Vec<V>,
    /// Number of failed checks for this case
    pub failed_checks: u32,
}

impl<'v, V: ValueLike<'v>> std::fmt::Display for TestCaseResultGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "TestCaseResult(params: {}, evaluated: {})",
            self.params.len(),
            self.evaluated.is_some()
        )
    }
}

impl<'v, V: ValueLike<'v>> std::fmt::Debug for TestCaseResultGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestCaseResult")
            .field("params", &self.params.len())
            .field("evaluated", &self.evaluated.is_some())
            .field("failed_checks", &self.failed_checks)
            .finish()
    }
}

#[starlark_value(type = "TestCaseResult")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for TestCaseResultGen<V> where
    Self: ProvidesStaticType<'v>
{
}

starlark_complex_value!(pub TestCaseResult);

/// TestBench value that evaluates modules with explicit test cases
#[derive(Clone, Coerce, Trace, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub struct TestBenchValueGen<V: ValueLifetimeless> {
    /// Name of this TestBench instance
    name: String,
    /// The module loader that was used
    #[freeze(identity)]
    module_loader: ModuleLoader,
    /// Results from each test case
    cases: Vec<TestCaseResultGen<V>>,
    /// Summary statistics
    summary: SmallMap<String, V>,
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
        debug.field("cases", &self.cases.len());
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

    pub fn cases(&self) -> &Vec<TestCaseResultGen<V>> {
        &self.cases
    }

    pub fn case_count(&self) -> usize {
        self.cases.len()
    }

    pub fn summary(&self) -> &SmallMap<String, V> {
        &self.summary
    }
}

/// Extension to ModuleLoader for TestBench evaluation
impl ModuleLoader {
    /// Evaluate this module with specific inputs for TestBench
    pub fn evaluate_with_inputs<'v>(
        &self,
        testbench_name: String,
        eval: &mut Evaluator<'v, '_, '_>,
        inputs: InputMap,
        case_name: Option<&str>,
    ) -> anyhow::Result<Option<FrozenModuleValue>> {
        // Create a child context with strict_io_config = true
        let ctx = eval
            .eval_context()
            .expect("expected eval context")
            .child_context()
            .set_strict_io_config(true); // Strict mode - require all inputs

        let module_name = match case_name {
            Some(name) => format!("{}__{}", testbench_name, name),
            None => testbench_name,
        };

        let ctx = ctx
            .set_source_path(std::path::PathBuf::from(&self.source_path))
            .set_module_name(module_name)
            .set_inputs(inputs);

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
                    EvalSeverity::Error => {
                        let case_suffix = case_name
                            .map(|n| format!(" case '{}'", n))
                            .unwrap_or_default();
                        (
                            EvalSeverity::Error,
                            format!("Error in TestBench module `{}`{}", self.name, case_suffix),
                        )
                    }
                    EvalSeverity::Warning => {
                        let case_suffix = case_name
                            .map(|n| format!(" case '{}'", n))
                            .unwrap_or_default();
                        (
                            EvalSeverity::Warning,
                            format!(
                                "Warning from TestBench module `{}`{}",
                                self.name, case_suffix
                            ),
                        )
                    }
                    other => {
                        let case_suffix = case_name
                            .map(|n| format!(" case '{}'", n))
                            .unwrap_or_default();
                        (
                            other,
                            format!("Issue in TestBench module `{}`{}", self.name, case_suffix),
                        )
                    }
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

/// Build an InputMap from a test case dictionary
fn build_input_map<'v>(case_dict: &DictRef<'v>) -> anyhow::Result<InputMap> {
    let mut inputs = InputMap::new();

    for (key, value) in case_dict.iter() {
        let key_str = key
            .unpack_str()
            .ok_or_else(|| anyhow::anyhow!("test case keys must be strings, got: {}", key))?;

        let input_value = InputValue::from_value(value);
        inputs.insert(key_str.to_string(), input_value);
    }

    Ok(inputs)
}

/// Execute a single check function and handle the result
fn execute_check<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    check_func: Value<'v>,
    args: &[Value<'v>],
    testbench_name: &str,
    case_name: Option<&str>,
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
            let case_suffix = case_name
                .map(|n| format!(" case '{}'", n))
                .unwrap_or_default();
            ctx.add_diagnostic(Diagnostic {
                path: testbench_location.filename().to_string(),
                span: Some(testbench_location.resolve_span()),
                severity: EvalSeverity::Error,
                body: format!(
                    "TestBench '{}'{} check '{}' failed",
                    testbench_name, case_suffix, check_name
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
    /// Create a TestBench that evaluates modules with explicit test cases
    fn TestBench<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] module: Value<'v>,
        #[starlark(require = named)] test_cases: Value<'v>,
        #[starlark(require = named)] checks: Option<Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        // Extract ModuleLoader from the module parameter
        let loader = module.downcast_ref::<ModuleLoader>().ok_or_else(|| {
            anyhow::anyhow!("'module' parameter must be a ModuleLoader (created with Module())")
        })?;

        // Parse test_cases dict
        let test_cases_dict = DictRef::from_value(test_cases)
            .ok_or_else(|| anyhow::anyhow!("'test_cases' parameter must be a dictionary"))?;

        if test_cases_dict.is_empty() {
            return Err(anyhow::anyhow!("'test_cases' cannot be empty"));
        }

        let mut cases = Vec::new();
        let mut total_checks = 0;
        let mut total_failed_checks = 0;

        // Process each test case
        for (case_name, case_value) in test_cases_dict.iter() {
            let case_name_str = case_name.unpack_str().ok_or_else(|| {
                anyhow::anyhow!("test case names must be strings, got: {}", case_name)
            })?;

            let case_dict = DictRef::from_value(case_value).ok_or_else(|| {
                anyhow::anyhow!("test case '{}' must be a dictionary", case_name_str)
            })?;

            // Build InputMap from case parameters
            let inputs = build_input_map(&case_dict)?;

            // Evaluate the module with this test case
            let evaluated_module =
                loader.evaluate_with_inputs(name.clone(), eval, inputs, Some(case_name_str))?;

            // Execute check functions for this case
            let mut case_check_results = Vec::new();
            let mut case_failed_count = 0;

            if let (Some(checks_value), Some(ref module)) = (&checks, &evaluated_module) {
                let checks_list = ListRef::from_value(*checks_value).ok_or_else(|| {
                    anyhow::anyhow!("'checks' parameter must be a list of functions")
                })?;

                // Use frozen_heap to allocate the FrozenModuleValue
                let module_value = eval.frozen_heap().alloc(module.clone()).to_value();

                // Convert test case parameters to Starlark dict
                let inputs_dict = eval
                    .heap()
                    .alloc(AllocDict(case_dict.iter().collect::<Vec<_>>()))
                    .to_value();

                let args = [module_value, inputs_dict];

                for check_func in checks_list.iter() {
                    let (result, failed) =
                        execute_check(eval, check_func, &args, &name, Some(case_name_str))?;
                    case_check_results.push(result);
                    total_checks += 1;
                    if failed {
                        case_failed_count += 1;
                        total_failed_checks += 1;
                    }
                }
            }

            // Store case parameters for introspection
            let mut params = SmallMap::new();
            for (key, value) in case_dict.iter() {
                if let Some(key_str) = key.unpack_str() {
                    params.insert(key_str.to_string(), value);
                }
            }

            cases.push(TestCaseResultGen {
                params,
                evaluated: evaluated_module,
                check_results: case_check_results,
                failed_checks: case_failed_count,
            });
        }

        // Build summary
        let mut summary = SmallMap::new();
        summary.insert(
            "total_cases".to_string(),
            eval.heap().alloc(cases.len() as i32).to_value(),
        );
        summary.insert(
            "total_checks".to_string(),
            eval.heap().alloc(total_checks).to_value(),
        );
        summary.insert(
            "total_failed_checks".to_string(),
            eval.heap().alloc(total_failed_checks).to_value(),
        );

        // Log and print results
        log::info!(
            "TestBench '{}': {} cases, {} checks executed",
            name,
            cases.len(),
            total_checks
        );

        if total_failed_checks == 0 && total_checks > 0 {
            let case_word = if cases.len() == 1 { "case" } else { "cases" };
            let check_word = if total_checks == 1 { "check" } else { "checks" };
            eprintln!(
                "\x1b[1m\x1b[32m✓ {}\x1b[0m: {} {} passed across {} {}",
                name,
                total_checks,
                check_word,
                cases.len(),
                case_word
            );
        }

        // Create and return the TestBenchValue
        let testbench = TestBenchValueGen::<Value> {
            name,
            module_loader: loader.clone(),
            cases,
            summary,
        };

        Ok(eval.heap().alloc(testbench))
    }
}
