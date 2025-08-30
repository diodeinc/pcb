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

            // Build nets, ports, and components dictionaries from the evaluated module
            let heap = eval.heap();
            let mut nets_dict_entries = Vec::new();
            let mut ports_dict_entries = Vec::new();
            let mut components_dict_entries = Vec::new();

            // Use the existing to_schematic() method to get deduplicated nets
            match module.to_schematic() {
                Ok(schematic) => {
                    // Build nets and ports dictionaries
                    for (net_name, net) in schematic.nets.iter() {
                        // Convert each port InstanceRef to a simplified string
                        let mut port_strings = Vec::new();
                        for port in &net.ports {
                            // Use just the instance path for cleaner output (skip module path)
                            let port_string = if port.instance_path.is_empty() {
                                "root".to_string()
                            } else {
                                port.instance_path.join(".")
                            };

                            // Add to nets dict (net -> list of ports)
                            port_strings.push(heap.alloc_str(&port_string).to_value());

                            // Add to ports dict (port -> net)
                            ports_dict_entries.push((
                                heap.alloc_str(&port_string).to_value(),
                                heap.alloc_str(net_name).to_value(),
                            ));
                        }

                        let ports_list = heap.alloc(port_strings);
                        nets_dict_entries.push((heap.alloc_str(net_name).to_value(), ports_list));
                    }

                    // Build components dictionary
                    for (instance_ref, instance) in schematic.instances.iter() {
                        // Only include components (not modules, ports, etc.)
                        if instance.kind == pcb_sch::InstanceKind::Component {
                            let component_name = if instance_ref.instance_path.is_empty() {
                                "root".to_string()
                            } else {
                                instance_ref.instance_path.join(".")
                            };

                            // Build attributes dictionary for this component
                            let mut component_attrs = Vec::new();

                            // Collect all pin names for this component from all nets
                            let mut pin_names = Vec::new();
                            for (_net_name, net) in schematic.nets.iter() {
                                for port in &net.ports {
                                    if port.instance_path.len() >= 2 {
                                        // Check if this port belongs to our current component
                                        let port_component_path = port.instance_path
                                            [..port.instance_path.len() - 1]
                                            .join(".");
                                        if port_component_path == component_name {
                                            // Extract pin name (last part of instance path)
                                            if let Some(pin_name) = port.instance_path.last() {
                                                if !pin_names.contains(pin_name) {
                                                    pin_names.push(pin_name.clone());
                                                }
                                            }
                                        }
                                    }
                                }
                            }

                            // Add pins as a comma-delimited string
                            if !pin_names.is_empty() {
                                pin_names.sort(); // Sort for consistent ordering
                                let pins_str = pin_names.join(",");
                                component_attrs.push((
                                    heap.alloc_str("Pins").to_value(),
                                    heap.alloc_str(&pins_str).to_value(),
                                ));
                            }

                            for (key, value) in instance.attributes.iter() {
                                // Skip verbose/internal attributes
                                if key == "footprint"
                                    || (key.starts_with("__") && key.ends_with("__"))
                                    || key == "symbol_path"
                                    || key == "symbol_name"
                                    || key.starts_with("__symbol_")
                                {
                                    continue;
                                }

                                let value_str = match value {
                                    pcb_sch::AttributeValue::String(s) => s.clone(),
                                    pcb_sch::AttributeValue::Number(n) => n.to_string(),
                                    pcb_sch::AttributeValue::Boolean(b) => b.to_string(),
                                    pcb_sch::AttributeValue::Physical(p) => format!("{:?}", p),
                                    pcb_sch::AttributeValue::Port(p) => p.clone(),
                                    _ => format!("{:?}", value), // Fallback for complex types
                                };
                                component_attrs.push((
                                    heap.alloc_str(key).to_value(),
                                    heap.alloc_str(&value_str).to_value(),
                                ));
                            }

                            let attrs_dict = heap.alloc(AllocDict(component_attrs));
                            components_dict_entries
                                .push((heap.alloc_str(&component_name).to_value(), attrs_dict));
                        }
                    }
                }
                Err(e) => {
                    eprintln!(
                        "Warning: Failed to convert module to schematic for collection: {}",
                        e
                    );
                    // Continue with empty dicts
                }
            }

            let nets_dict = heap.alloc(AllocDict(nets_dict_entries));
            let ports_dict = heap.alloc(AllocDict(ports_dict_entries));
            let components_dict = heap.alloc(AllocDict(components_dict_entries));

            // Execute each check function
            for check_func in checks_list.iter() {
                match eval.eval_function(check_func, &[nets_dict, ports_dict, components_dict], &[])
                {
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

        // Log detailed TestBench info
        if let Some(ref module) = evaluated_module {
            // Get net count using the same schematic conversion approach
            let net_count = match module.to_schematic() {
                Ok(schematic) => schematic.nets.len(),
                Err(_) => 0, // If conversion fails, show 0 nets
            };
            log::info!(
                "TestBench '{}': {} nets, {} checks",
                name,
                net_count,
                check_results.len()
            );
        } else {
            log::info!(
                "TestBench '{}': evaluation failed, {} checks",
                name,
                check_results.len()
            );
        }

        // Check for failed checks and add error diagnostics
        let mut all_checks_passed = true;
        if let (Some(checks_value), Some(ctx)) = (checks, eval.context_value()) {
            let checks_list = ListRef::from_value(checks_value).unwrap(); // We already validated this above

            for (i, (check_func, result)) in
                checks_list.iter().zip(check_results.iter()).enumerate()
            {
                // Check if result is false
                if let Some(bool_val) = result.unpack_bool() {
                    if !bool_val {
                        all_checks_passed = false;

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

        // Print success message if all checks passed
        if all_checks_passed && !check_results.is_empty() {
            let check_count = check_results.len();
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
            check_results,
        };

        Ok(eval.heap().alloc(testbench))
    }
}
