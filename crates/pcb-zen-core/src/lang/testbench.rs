#![allow(clippy::needless_lifetimes)]

use allocative::Allocative;
use starlark::environment::GlobalsBuilder;
use starlark::{
    any::ProvidesStaticType,
    collections::SmallMap,
    eval::Evaluator,
    starlark_complex_value, starlark_module,
    values::{
        dict::AllocDict, list::ListRef, starlark_value, Coerce, Freeze, FreezeResult, NoSerialize,
        StarlarkValue, Trace, Value, ValueLifetimeless, ValueLike,
    },
};

use crate::convert::ToSchematic;
use crate::lang::evaluator_ext::EvaluatorExt;
use crate::lang::input::InputMap;
use crate::lang::module::{FrozenModuleValue, ModuleLoader};
use crate::Diagnostic;
use starlark::errors::EvalSeverity;

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
        let mut check_results = Vec::new();

        if let (Some(checks_value), Some(ref module)) = (checks, &evaluated_module) {
            // Convert checks to a list
            let checks_list = ListRef::from_value(checks_value)
                .ok_or_else(|| anyhow::anyhow!("'checks' parameter must be a list of functions"))?;

            // Build nets dictionary from the evaluated module using existing schematic conversion
            let heap = eval.heap();
            let mut nets_dict_entries = Vec::new();

            // Use the existing to_schematic() method to get deduplicated nets
            match module.to_schematic() {
                Ok(schematic) => {
                    for net_name in schematic.nets.keys() {
                        // Create empty list for each net (for now)
                        let empty_list = heap.alloc(Vec::<Value>::new());
                        nets_dict_entries.push((heap.alloc_str(net_name).to_value(), empty_list));
                    }
                }
                Err(e) => {
                    eprintln!(
                        "Warning: Failed to convert module to schematic for net collection: {}",
                        e
                    );
                    // Continue with empty nets dict
                }
            }

            let nets_dict = heap.alloc(AllocDict(nets_dict_entries));

            // Execute each check function
            for check_func in checks_list.iter() {
                match eval.eval_function(check_func, &[nets_dict], &[]) {
                    Ok(result) => {
                        // Convert result to bool if possible, otherwise store as-is
                        check_results.push(result);
                    }
                    Err(e) => {
                        eprintln!("Warning: Check function failed: {}", e);
                        // Store a false value for failed checks
                        check_results.push(heap.alloc(false).to_value());
                    }
                }
            }
        }

        // Print statistics about the evaluated module
        if let Some(ref module) = evaluated_module {
            eprintln!("TestBench '{}' created:", name);
            eprintln!("  - Module: {}", loader.name);
            eprintln!("  - Source: {}", loader.source_path);
            eprintln!("  - Children: {}", module.children().len());
            // Get net count using the same schematic conversion approach
            let net_count = match module.to_schematic() {
                Ok(schematic) => schematic.nets.len(),
                Err(_) => 0, // If conversion fails, show 0 nets
            };
            eprintln!("  - Total nets: {}", net_count);
            eprintln!("  - Properties: {}", module.properties().len());
            eprintln!("  - Checks run: {}", check_results.len());
        } else {
            eprintln!("TestBench '{}' created (module evaluation failed)", name);
            eprintln!("  - Module: {}", loader.name);
            eprintln!("  - Source: {}", loader.source_path);
            eprintln!("  - Checks run: {}", check_results.len());
        }

        // Check for failed checks and add error diagnostics
        if let (Some(checks_value), Some(ctx)) = (checks, eval.context_value()) {
            let checks_list = ListRef::from_value(checks_value).unwrap(); // We already validated this above

            for (i, (check_func, result)) in
                checks_list.iter().zip(check_results.iter()).enumerate()
            {
                // Check if result is false
                if let Some(bool_val) = result.unpack_bool() {
                    if !bool_val {
                        // Get check function name from str() representation
                        let full_name = check_func.to_string();
                        let check_name = full_name
                            .rsplit('.')
                            .next()
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| format!("Check #{}", i + 1));

                        let diagnostic = Diagnostic {
                            path: eval
                                .call_stack_top_location()
                                .map(|l| l.filename().to_string())
                                .unwrap_or_else(|| loader.source_path.clone()),
                            span: eval.call_stack_top_location().map(|l| l.resolve_span()),
                            severity: EvalSeverity::Error,
                            body: format!("TestBench check '{}' failed", check_name),
                            call_stack: Some(eval.call_stack().clone()),
                            child: None,
                            source_error: None,
                        };

                        ctx.add_diagnostic(diagnostic);
                    }
                }
            }
        }

        // Create and return the TestBenchValue
        let testbench = TestBenchValueGen::<Value> {
            name,
            module_loader: loader.clone(),
            evaluated_module,
            properties: SmallMap::new(),
            check_results,
        };

        Ok(eval.heap().alloc(testbench))
    }
}
