#![allow(clippy::needless_lifetimes)]

use std::sync::Arc;

use crate::lang::eval::EvalMode;
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
        dict::DictRef, list::ListRef, starlark_value, tuple::TupleRef, Coerce, Freeze, NoSerialize,
        StarlarkValue, Trace, Value, ValueLifetimeless, ValueLike,
    },
};

/// A single deferred check function with optional custom name
#[derive(Clone, Coerce, Trace, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub struct DeferredCheckGen<V: ValueLifetimeless> {
    /// The check function to execute later
    pub check_func: V,
    /// Optional custom name for the check (from tuple syntax)
    pub custom_name: Option<String>,
}

/// A test case with deferred check execution
#[derive(Clone, Coerce, Trace, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub struct DeferredTestCaseGen<V: ValueLifetimeless> {
    /// The name of this test case
    pub case_name: String,
    /// The test case parameters that were provided
    pub params: SmallMap<String, V>,
    /// The evaluated module (None if evaluation failed)
    #[freeze(identity)]
    pub evaluated: Option<FrozenModuleValue>,
    /// Check functions to run later
    pub checks: Vec<DeferredCheckGen<V>>,
}

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
    /// Deferred test cases (used when checks are deferred)
    deferred_cases: Vec<DeferredTestCaseGen<V>>,
    /// Source file path where TestBench was defined (for diagnostic context)
    source_path: String,
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
        debug.field("deferred_cases", &self.deferred_cases.len());
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

    pub fn deferred_cases(&self) -> &Vec<DeferredTestCaseGen<V>> {
        &self.deferred_cases
    }

    pub fn case_count(&self) -> usize {
        self.deferred_cases.len()
    }

    pub fn source_path(&self) -> &str {
        &self.source_path
    }
}

/// Extension to ModuleLoader for TestBench evaluation
impl ModuleLoader {
    /// Evaluate this module with specific inputs for TestBench
    pub fn evaluate_with_inputs<'v>(
        &self,
        test_bench_name: String,
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
            Some(name) => format!("{}__{}", test_bench_name, name),
            None => test_bench_name,
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

/// Execute a single check function and handle the result (legacy - no longer used with deferred execution)
#[allow(dead_code)]
fn execute_check<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    check_func: Value<'v>,
    args: &[Value<'v>],
    test_bench_name: &str,
    case_name: Option<&str>,
    custom_name: Option<&str>,
) -> anyhow::Result<(Value<'v>, bool)> {
    let check_name = if let Some(name) = custom_name {
        name.to_string()
    } else {
        let check_func_str = check_func.to_string();
        check_func_str
            .rsplit('.')
            .next()
            .unwrap_or("check")
            .to_string()
    };

    let case_suffix = case_name
        .map(|n| format!(" case '{}'", n))
        .unwrap_or_default();

    // Validate that check_func is actually callable before trying to call it
    if check_func.get_type() != "function" && check_func.get_type() != "native_function" {
        return Err(anyhow::anyhow!(
            "Check function '{}' is not callable (type: {})",
            check_func.to_string(),
            check_func.get_type()
        ));
    }

    match eval.eval_function(check_func, args, &[]) {
        Ok(result) => {
            // Only add diagnostics if context is available
            if let (Some(ctx), Some(test_bench_location)) =
                (eval.context_value(), eval.call_stack_top_location())
            {
                // Create structured test result for tracking
                let test_result = crate::lang::error::BenchTestResult {
                    test_bench_name: test_bench_name.to_string(),
                    case_name: case_name.map(|s| s.to_string()),
                    check_name: check_name.clone(),
                    file_path: test_bench_location.filename().to_string(),
                    passed: true,
                };

                // Add as a non-error diagnostic for collection purposes
                ctx.add_diagnostic(Diagnostic {
                    path: test_bench_location.filename().to_string(),
                    span: Some(test_bench_location.resolve_span()),
                    severity: EvalSeverity::Advice,
                    body: format!(
                        "TestBench '{}'{} check '{}' passed",
                        test_bench_name, case_suffix, check_name
                    ),
                    call_stack: Some(eval.call_stack().clone()),
                    child: None,
                    source_error: Some(std::sync::Arc::new(test_result.into())),
                });
            }

            Ok((result, false)) // Success, no failure
        }
        Err(e) => {
            // Only add diagnostics if context is available
            if let (Some(ctx), Some(test_bench_location)) =
                (eval.context_value(), eval.call_stack_top_location())
            {
                // Convert error to diagnostic - this will handle DiagnosticError chains properly
                let child_diagnostic = Diagnostic::from(e);
                let child = Some(Box::new(child_diagnostic));

                // Create structured test result for tracking
                let test_result = crate::lang::error::BenchTestResult {
                    test_bench_name: test_bench_name.to_string(),
                    case_name: case_name.map(|s| s.to_string()),
                    check_name: check_name.clone(),
                    file_path: test_bench_location.filename().to_string(),
                    passed: false,
                };

                // Parent diagnostic for TestBench context
                ctx.add_diagnostic(Diagnostic {
                    path: test_bench_location.filename().to_string(),
                    span: Some(test_bench_location.resolve_span()),
                    severity: EvalSeverity::Error,
                    body: format!(
                        "TestBench '{}'{} check '{}' failed",
                        test_bench_name, case_suffix, check_name
                    ),
                    call_stack: Some(eval.call_stack().clone()),
                    child,
                    source_error: Some(Arc::new(test_result.into())),
                });
            }

            Ok((eval.heap().alloc(false).to_value(), true)) // Failure
        }
    }
}

#[starlark_module]
pub fn test_bench_globals(builder: &mut GlobalsBuilder) {
    /// Create a TestBench that evaluates modules with explicit test cases
    fn TestBench<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] module: Value<'v>,
        #[starlark(require = named)] test_cases: Value<'v>,
        #[starlark(require = named)] checks: Option<Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        // Check eval mode - in Build mode, TestBench is a no-op
        if let Some(ctx) = eval.eval_context() {
            if ctx.eval_mode == EvalMode::Build {
                // Return None to indicate no-op
                return Ok(Value::new_none());
            }
        }

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

        // Get source path for diagnostic context
        let source_path = eval
            .call_stack_top_location()
            .map(|cs| cs.filename().to_string())
            .unwrap_or_else(|| "<unknown>".to_string());

        // Parse checks list once if provided
        let checks_list =
            if let Some(checks_value) = checks {
                Some(ListRef::from_value(checks_value).ok_or_else(|| {
                    anyhow::anyhow!("'checks' parameter must be a list of functions")
                })?)
            } else {
                None
            };

        let mut deferred_cases = Vec::new();

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

            // Store case parameters for later
            let mut params = SmallMap::new();
            for (key, value) in case_dict.iter() {
                if let Some(key_str) = key.unpack_str() {
                    params.insert(key_str.to_string(), value);
                }
            }

            // Store check functions for later execution
            let mut deferred_checks = Vec::new();
            if let Some(checks_list_ref) = checks_list {
                for check_item in checks_list_ref.iter() {
                    // Check if it's a tuple (name, function) or just a function
                    let (check_func, custom_name) =
                        if let Some(tuple_ref) = TupleRef::from_value(check_item) {
                            if tuple_ref.len() == 2 {
                                let tuple_items: Vec<_> = tuple_ref.iter().collect();
                                let name = tuple_items[0].unpack_str().ok_or_else(|| {
                                    anyhow::anyhow!(
                                        "First element of check tuple must be a string name"
                                    )
                                })?;
                                let func = tuple_items[1];
                                (func, Some(name.to_string()))
                            } else {
                                return Err(anyhow::anyhow!(
                                    "Check tuple must have exactly 2 elements: (name, function)"
                                ));
                            }
                        } else {
                            (check_item, None)
                        };

                    deferred_checks.push(DeferredCheckGen {
                        check_func,
                        custom_name,
                    });
                }
            }

            deferred_cases.push(DeferredTestCaseGen {
                case_name: case_name_str.to_string(),
                params,
                evaluated: evaluated_module,
                checks: deferred_checks,
            });
        }

        log::info!(
            "TestBench '{}': {} cases registered with deferred checks",
            name,
            deferred_cases.len()
        );

        // Create and return the TestBenchValue with deferred checks
        let testbench = TestBenchValueGen::<Value> {
            name,
            module_loader: loader.clone(),
            deferred_cases,
            source_path,
        };

        let testbench_value = eval.heap().alloc(testbench);

        // Add the TestBench to the module's children so it can be collected later
        if let Some(ctx) = eval.context_value() {
            ctx.add_child(testbench_value);
        }

        Ok(testbench_value)
    }
}

/// Execute a single deferred check and add diagnostic
pub fn execute_deferred_check<'v, V: ValueLike<'v>>(
    eval: &mut Evaluator<'v, '_, '_>,
    check: &DeferredCheckGen<V>,
    module_value: Value<'v>,
    inputs_dict: Value<'v>,
    test_bench_name: &str,
    case_name: &str,
    test_bench_source_path: &str,
) -> (bool, Vec<Diagnostic>) {
    let mut diagnostics = Vec::new();

    let check_name = if let Some(ref name) = check.custom_name {
        name.clone()
    } else {
        let check_func_str = check.check_func.to_value().to_string();
        check_func_str
            .rsplit('.')
            .next()
            .unwrap_or("check")
            .to_string()
    };

    let case_suffix = format!(" case '{}'", case_name);
    let args = [module_value, inputs_dict];

    match eval.eval_function(check.check_func.to_value(), &args, &[]) {
        Ok(_result) => {
            // Create structured test result for tracking
            let test_result = crate::lang::error::BenchTestResult {
                test_bench_name: test_bench_name.to_string(),
                case_name: Some(case_name.to_string()),
                check_name: check_name.clone(),
                file_path: test_bench_source_path.to_string(),
                passed: true,
            };

            // Add as a non-error diagnostic for collection purposes
            diagnostics.push(Diagnostic {
                path: test_bench_source_path.to_string(),
                span: None,
                severity: EvalSeverity::Advice,
                body: format!(
                    "TestBench '{}'{} check '{}' passed",
                    test_bench_name, case_suffix, check_name
                ),
                call_stack: None,
                child: None,
                source_error: Some(std::sync::Arc::new(test_result.into())),
            });

            (true, diagnostics) // Success
        }
        Err(e) => {
            // Convert error to diagnostic
            let child_diagnostic = Diagnostic::from(e);
            let child = Some(Box::new(child_diagnostic));

            // Create structured test result for tracking
            let test_result = crate::lang::error::BenchTestResult {
                test_bench_name: test_bench_name.to_string(),
                case_name: Some(case_name.to_string()),
                check_name: check_name.clone(),
                file_path: test_bench_source_path.to_string(),
                passed: false,
            };

            // Parent diagnostic for TestBench context
            diagnostics.push(Diagnostic {
                path: test_bench_source_path.to_string(),
                span: None,
                severity: EvalSeverity::Error,
                body: format!(
                    "TestBench '{}'{} check '{}' failed",
                    test_bench_name, case_suffix, check_name
                ),
                call_stack: None,
                child,
                source_error: Some(Arc::new(test_result.into())),
            });

            (false, diagnostics) // Failure
        }
    }
}
