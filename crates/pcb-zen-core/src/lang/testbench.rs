#![allow(clippy::needless_lifetimes)]

use std::collections::HashMap;

use allocative::Allocative;
use starlark::environment::GlobalsBuilder;
use starlark::{
    any::ProvidesStaticType,
    collections::SmallMap,
    eval::Evaluator,
    starlark_complex_value, starlark_module, starlark_simple_value,
    values::{
        dict::AllocDict, list::ListRef, starlark_value, Coerce, Freeze, FreezeResult, Heap,
        NoSerialize, StarlarkValue, Trace, Value, ValueLifetimeless, ValueLike,
    },
};

use crate::convert::ToSchematic;
use crate::lang::evaluator_ext::EvaluatorExt;
use crate::lang::input::InputMap;
use crate::lang::module::{FrozenModuleValue, ModuleLoader};
use crate::Diagnostic;
use starlark::errors::EvalSeverity;

/// Type marker for ModuleView, used in type annotations
#[derive(Debug, Trace, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub struct ModuleViewType;

starlark_simple_value!(ModuleViewType);

#[starlark_value(type = "ModuleViewType")]
impl<'v> StarlarkValue<'v> for ModuleViewType {
    fn eval_type(&self) -> Option<starlark::typing::Ty> {
        Some(<ModuleView as StarlarkValue>::get_type_starlark_repr())
    }
}

impl std::fmt::Display for ModuleViewType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ModuleView")
    }
}

/// ModuleView provides access to nets, ports, and components data for TestBench check functions
#[derive(Clone, Coerce, Trace, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub struct ModuleViewGen<V: ValueLifetimeless> {
    /// Dictionary of net names to connected ports
    nets: V,
    /// Dictionary of port names to net names
    ports: V,
    /// Dictionary of component names to their attributes
    components: V,
    /// Dictionary of component names to their ComponentValue objects
    component_values: V,
}

starlark_complex_value!(pub ModuleView);

#[starlark_value(type = "ModuleView")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for ModuleViewGen<V>
where
    Self: ProvidesStaticType<'v>,
{
    fn get_attr(&self, attr: &str, _heap: &'v Heap) -> Option<Value<'v>> {
        match attr {
            "nets" => Some(self.nets.to_value()),
            "ports" => Some(self.ports.to_value()),
            "components" => Some(self.components.to_value()),
            "component_values" => Some(self.component_values.to_value()),
            _ => None,
        }
    }

    fn has_attr(&self, attr: &str, _heap: &'v Heap) -> bool {
        matches!(attr, "nets" | "ports" | "components" | "component_values")
    }

    fn dir_attr(&self) -> Vec<String> {
        vec![
            "nets".to_string(),
            "ports".to_string(),
            "components".to_string(),
            "component_values".to_string(),
        ]
    }
}

impl<'v, V: ValueLike<'v>> std::fmt::Display for ModuleViewGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ModuleView(nets, ports, components)")
    }
}

impl<'v, V: ValueLike<'v>> std::fmt::Debug for ModuleViewGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModuleView")
            .field("nets", &"<dict>")
            .field("ports", &"<dict>")
            .field("components", &"<dict>")
            .finish()
    }
}

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

/// Format an instance path for display
fn format_instance_path(path: &[pcb_sch::Symbol]) -> String {
    if path.is_empty() {
        "<root>".to_string()
    } else {
        path.join(".")
    }
}

/// Walk the module tree and collect ComponentValue objects with their paths
fn collect_components<'v>(
    module: &crate::lang::module::FrozenModuleValue,
    path_prefix: &str,
) -> HashMap<String, Value<'v>> {
    let mut components = HashMap::new();

    for child in module.children() {
        if let Some(component) = child.downcast_ref::<crate::FrozenComponentValue>() {
            let path = if path_prefix.is_empty() {
                component.name().to_string()
            } else {
                format!("{}.{}", path_prefix, component.name())
            };
            components.insert(path, child.to_value());
        } else if let Some(submodule) =
            child.downcast_ref::<crate::lang::module::FrozenModuleValue>()
        {
            let subpath = if path_prefix.is_empty() {
                submodule.name().to_string()
            } else {
                format!("{}.{}", path_prefix, submodule.name())
            };
            components.extend(collect_components(submodule, &subpath));
        }
    }

    components
}

/// Build a ModuleView from a schematic and module
fn build_module_view<'v>(
    schematic: &pcb_sch::Schematic,
    module: &crate::lang::module::FrozenModuleValue,
    heap: &'v Heap,
) -> ModuleViewGen<Value<'v>> {
    let mut nets_dict = Vec::new();
    let mut ports_dict = Vec::new();
    let mut components_dict = HashMap::<String, HashMap<String, Value<'v>>>::new();

    // Collect ComponentValue objects first so we can reference them
    let component_values_dict = collect_components(module, "");

    // Build nets and ports dictionaries
    for (net_name, net) in &schematic.nets {
        let mut port_strings = Vec::new();
        for port in &net.ports {
            let port_string = format_instance_path(&port.instance_path);
            let port_val = heap.alloc_str(&port_string).to_value();
            let net_val = heap.alloc_str(net_name).to_value();
            port_strings.push(port_val);
            ports_dict.push((port_val, net_val));
        }
        nets_dict.push((
            heap.alloc_str(net_name).to_value(),
            heap.alloc(port_strings),
        ));
    }

    // Build components dictionary directly from component_values_dict
    for (component_name, comp_val) in &component_values_dict {
        if let Some(frozen_comp) = comp_val.downcast_ref::<crate::FrozenComponentValue>() {
            let mut component_attrs = HashMap::new();

            // Add pins as a comma-delimited string
            let pin_names: Vec<String> = frozen_comp.connections().keys().cloned().collect();
            component_attrs.insert("Pins".to_string(), heap.alloc(pin_names));

            // Add component properties (excluding internal ones)
            for (key, value) in frozen_comp.properties() {
                if matches!(key.as_str(), "footprint" | "symbol_path" | "symbol_name")
                    || key.starts_with("__")
                {
                    continue;
                }
                component_attrs.insert(key.clone(), value.to_value());
            }

            // Promote typed attributes over string attributes
            if let Some(capacitance) = frozen_comp.properties().get("__capacitance__") {
                component_attrs.insert("Capacitance".to_string(), capacitance.to_value());
            }
            if let Some(resistance) = frozen_comp.properties().get("__resistance__") {
                component_attrs.insert("Resistance".to_string(), resistance.to_value());
            }

            // Add built-in component attributes
            if let Some(mpn) = frozen_comp.mpn() {
                component_attrs.insert("MPN".to_string(), heap.alloc_str(mpn).to_value());
            }
            component_attrs.insert(
                "Prefix".to_string(),
                heap.alloc_str(frozen_comp.prefix()).to_value(),
            );

            components_dict.insert(component_name.clone(), component_attrs);
        }
    }

    // Convert HashMaps back to Vecs for Starlark dictionaries
    let component_values_vec: Vec<(Value<'v>, Value<'v>)> = component_values_dict
        .into_iter()
        .map(|(path, comp_val)| (heap.alloc_str(&path).to_value(), comp_val))
        .collect();

    let components_dict_vec: Vec<(Value<'v>, Value<'v>)> = components_dict
        .into_iter()
        .map(|(comp_name, comp_attrs)| {
            let attrs_vec: Vec<(Value<'v>, Value<'v>)> = comp_attrs
                .into_iter()
                .map(|(key, value)| (heap.alloc_str(&key).to_value(), value))
                .collect();
            (
                heap.alloc_str(&comp_name).to_value(),
                heap.alloc(AllocDict(attrs_vec)),
            )
        })
        .collect();

    ModuleViewGen::<Value> {
        nets: heap.alloc(AllocDict(nets_dict)),
        ports: heap.alloc(AllocDict(ports_dict)),
        components: heap.alloc(AllocDict(components_dict_vec)),
        component_values: heap.alloc(AllocDict(component_values_vec)),
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
    const ModuleView: ModuleViewType = ModuleViewType;

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

            let heap = eval.heap();
            let schematic = module.to_schematic()?;
            let module_view = build_module_view(&schematic, module, heap);
            let args = [heap.alloc(module_view)];

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
