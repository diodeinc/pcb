use crate::downcast_frozen_module;
use crate::graph::CircuitGraph;
use crate::lang::evaluator_ext::EvaluatorExt;
use allocative::Allocative;
use starlark::{
    eval::{Arguments, Evaluator},
    starlark_complex_value,
    values::{
        Freeze, Heap, NoSerialize, ProvidesStaticType, StarlarkValue, Trace, Value, starlark_value,
    },
};
use std::sync::Arc;

/// ModuleGraph that contains the circuit graph and module reference
#[derive(Clone, Debug, Trace, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub struct ModuleGraphValue<'v> {
    pub module: Value<'v>,
    #[freeze(identity)]
    pub graph: Arc<CircuitGraph>,
}

starlark_complex_value!(pub ModuleGraphValue);

/// PathsCallable for the ModuleGraph.paths() method
#[derive(Clone, Debug, Trace, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub struct PathsCallable<'v> {
    pub module: Value<'v>,
    #[freeze(identity)]
    pub graph: Arc<CircuitGraph>,
}

starlark_complex_value!(pub PathsCallable);

/// Path object representing a circuit path with pre-computed data
#[derive(Clone, Debug, Trace, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub struct PathValue<'v> {
    pub ports: Vec<Value<'v>>,      // List of port tuples
    pub components: Vec<Value<'v>>, // List of component objects
    pub nets: Vec<Value<'v>>,       // List of net objects
}

starlark_complex_value!(pub PathValue);

impl<'v> PathValue<'v> {
    pub fn description(&self) -> String {
        let port_names: Vec<String> = self.ports.iter().map(|port| format!("{}", port)).collect();
        format!("Path [{}]", port_names.join(", "))
    }
}

/// Callables for Path validation methods
#[derive(Clone, Debug, PartialEq, Eq, Allocative, Freeze)]
pub enum PathValidationOp {
    Count,
    Any,
    All,
    None,
    Matches,
}

#[derive(Clone, Debug, Trace, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub struct PathValidationCallable<'v> {
    pub path_value: Value<'v>,
    pub operation: PathValidationOp,
}

starlark_complex_value!(pub PathValidationCallable);

/// PathMatchesCallable for sequential component matching
#[derive(Clone, Debug, Trace, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub struct PathMatchesCallable<'v> {
    pub path_value: Value<'v>,
}

starlark_complex_value!(pub PathMatchesCallable);

impl<'v> PathMatchesCallable<'v> {
    fn wrap_matcher_error(
        &self,
        original_error: starlark::Error,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Error {
        let Some(cs) = eval.call_stack_top_location() else {
            return original_error;
        };

        let Some(path_val) = self.path_value.to_value().downcast_ref::<PathValue<'_>>() else {
            return original_error;
        };

        // Build child diagnostic directly from the Starlark error
        let child = crate::diagnostics::Diagnostic::from(original_error);

        // Create parent diagnostic with path.matches() context
        let parent = crate::diagnostics::Diagnostic {
            path: cs.file.filename().to_string(),
            span: Some(cs.resolve_span()),
            severity: starlark::errors::EvalSeverity::Error,
            body: format!("{} failed to match sequence", path_val.description()),
            call_stack: None,
            child: Some(Box::new(child)),
            source_error: None,
            related: Vec::new(),
            suppressed: false,
            fix: Vec::new(),
        };

        parent.into()
    }
}

// Implementations for ModuleGraphValue
impl<'v> std::fmt::Display for ModuleGraphValue<'v> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ModuleGraph")
    }
}

#[starlark_value(type = "ModuleGraph")]
impl<'v> StarlarkValue<'v> for ModuleGraphValue<'v> {
    fn get_attr(&self, attr: &str, heap: Heap<'v>) -> Option<Value<'v>> {
        match attr {
            "paths" => {
                let callable = PathsCallable {
                    module: self.module.to_value(),
                    graph: self.graph.clone(),
                };
                Some(heap.alloc_complex(callable))
            }
            _ => None,
        }
    }
}

// PathsCallable implementation
impl<'v> std::fmt::Display for PathsCallable<'v> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "paths")
    }
}

#[starlark_value(type = "builtin_function_or_method")]
impl<'v> StarlarkValue<'v> for PathsCallable<'v> {
    fn invoke(
        &self,
        _me: Value<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        let heap = eval.heap();

        // Extract arguments from named parameters
        let args_map = args.names_map()?;
        let start = args_map.get(&heap.alloc_str("start")).copied();
        let end = args_map.get(&heap.alloc_str("end")).copied();
        let max_depth = args_map
            .get(&heap.alloc_str("max_depth"))
            .and_then(|v| v.unpack_i32())
            .map(|d| d as usize);

        // Validate required arguments
        let start = start.ok_or_else(|| {
            starlark::Error::new_other(anyhow::anyhow!("paths() requires 'start' argument"))
        })?;
        let end = end.ok_or_else(|| {
            starlark::Error::new_other(anyhow::anyhow!("paths() requires 'end' argument"))
        })?;

        // Resolve start and end labels to PortIds
        let start_port = self.graph.resolve_label_to_port(start, heap)?;
        let end_port = self.graph.resolve_label_to_port(end, heap)?;

        // Find all simple paths using the CircuitGraph with factor tracking
        let mut paths_with_factors = Vec::new();
        self.graph.all_simple_paths_with_factors(
            start_port,
            end_port,
            max_depth,
            |path, factors| {
                paths_with_factors.push((path.to_vec(), factors.to_vec()));
            },
        );

        // Convert paths to PathValue objects
        let module_ref = downcast_frozen_module!(self.module);
        let components = eval.collect_components(module_ref.path());
        let path_objects: Vec<Value> = paths_with_factors
            .into_iter()
            .map(|(port_path, factors)| {
                self.graph
                    .create_path_value(&port_path, &factors, &components, heap)
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(heap.alloc(path_objects))
    }
}

// PathValue implementation
impl<'v> std::fmt::Display for PathValue<'v> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Path({} components)", self.components.len())
    }
}

#[starlark_value(type = "Path")]
impl<'v> StarlarkValue<'v> for PathValue<'v> {
    fn get_attr(&self, attr: &str, heap: Heap<'v>) -> Option<Value<'v>> {
        match attr {
            "ports" => {
                Some(heap.alloc(self.ports.iter().map(|v| v.to_value()).collect::<Vec<_>>()))
            }
            "components" => Some(
                heap.alloc(
                    self.components
                        .iter()
                        .map(|v| v.to_value())
                        .collect::<Vec<_>>(),
                ),
            ),
            "nets" => Some(heap.alloc(self.nets.iter().map(|v| v.to_value()).collect::<Vec<_>>())),
            "count" => Some(self.create_validation_callable(heap, PathValidationOp::Count)),
            "any" => Some(self.create_validation_callable(heap, PathValidationOp::Any)),
            "all" => Some(self.create_validation_callable(heap, PathValidationOp::All)),
            "none" => Some(self.create_validation_callable(heap, PathValidationOp::None)),
            "matches" => Some(self.create_matches_callable(heap)),
            _ => None,
        }
    }
}

impl<'v> PathValue<'v> {
    fn create_validation_callable(&self, heap: Heap<'v>, operation: PathValidationOp) -> Value<'v> {
        let callable = PathValidationCallable {
            path_value: heap.alloc_complex(self.clone()).to_value(),
            operation,
        };
        heap.alloc_complex(callable)
    }

    fn create_matches_callable(&self, heap: Heap<'v>) -> Value<'v> {
        let callable = PathMatchesCallable {
            path_value: heap.alloc_complex(self.clone()).to_value(),
        };
        heap.alloc_complex(callable)
    }
}

// PathValidationCallable implementation
impl<'v> std::fmt::Display for PathValidationCallable<'v> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.operation {
            PathValidationOp::Count => write!(f, "count"),
            PathValidationOp::Any => write!(f, "any"),
            PathValidationOp::All => write!(f, "all"),
            PathValidationOp::None => write!(f, "none"),
            PathValidationOp::Matches => write!(f, "matches"),
        }
    }
}

#[starlark_value(type = "builtin_function_or_method")]
impl<'v> StarlarkValue<'v> for PathValidationCallable<'v> {
    fn invoke(
        &self,
        _me: Value<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        let heap = eval.heap();

        // Extract the matcher function
        let matcher = args.positional1(heap)?;

        // Get the path value
        let path_value = self
            .path_value
            .to_value()
            .downcast_ref::<PathValue<'_>>()
            .ok_or_else(|| starlark::Error::new_other(anyhow::anyhow!("Invalid path value")))?;

        match self.operation {
            PathValidationOp::Count => {
                let mut count = 0;
                for component in &path_value.components {
                    // Catch errors - if matcher succeeds (no error), increment count
                    if eval
                        .eval_function(matcher, &[component.to_value()], &[])
                        .is_ok()
                    {
                        count += 1;
                    }
                }
                Ok(heap.alloc(count))
            }
            PathValidationOp::Any => {
                for component in &path_value.components {
                    // If any matcher succeeds (no error), succeed silently
                    if eval
                        .eval_function(matcher, &[component.to_value()], &[])
                        .is_ok()
                    {
                        return Ok(heap.alloc(starlark::values::none::NoneType));
                    }
                }
                // If no component passed, error
                Err(starlark::Error::new_other(anyhow::anyhow!(
                    "No components matched the condition"
                )))
            }
            PathValidationOp::All => {
                for component in &path_value.components {
                    // Fail fast on first error
                    eval.eval_function(matcher, &[component.to_value()], &[])?;
                }
                // All succeeded
                Ok(heap.alloc(starlark::values::none::NoneType))
            }
            PathValidationOp::None => {
                for component in &path_value.components {
                    // If any matcher succeeds (no error), this is a failure for "none"
                    if eval
                        .eval_function(matcher, &[component.to_value()], &[])
                        .is_ok()
                    {
                        return Err(starlark::Error::new_other(anyhow::anyhow!(
                            "Found component that matched the condition (expected none)"
                        )));
                    }
                }
                // All failed the matcher, which is success for "none"
                Ok(heap.alloc(starlark::values::none::NoneType))
            }
            PathValidationOp::Matches => {
                // This case should never be reached - matches() uses PathMatchesCallable
                Err(starlark::Error::new_other(anyhow::anyhow!(
                    "matches() operation should use PathMatchesCallable, not PathValidationCallable"
                )))
            }
        }
    }
}

// PathMatchesCallable implementation
impl<'v> std::fmt::Display for PathMatchesCallable<'v> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "matches")
    }
}

#[starlark_value(type = "builtin_function_or_method")]
impl<'v> StarlarkValue<'v> for PathMatchesCallable<'v> {
    fn invoke(
        &self,
        _me: Value<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        let heap = eval.heap();

        // Extract all matchers from positional arguments
        let matchers: Vec<Value> = args.positions(heap)?.collect();

        // Extract suppress_errors named parameter
        let args_map = args.names_map()?;
        let suppress_errors = args_map
            .get("suppress_errors")
            .and_then(|v| v.unpack_bool())
            .unwrap_or(false);

        if matchers.is_empty() {
            return Err(starlark::Error::new_other(anyhow::anyhow!(
                "matches() requires at least one matcher function"
            )));
        }

        // Get the path value
        let path_value = self
            .path_value
            .to_value()
            .downcast_ref::<PathValue<'_>>()
            .ok_or_else(|| starlark::Error::new_other(anyhow::anyhow!("Invalid path value")))?;

        let components = &path_value.components;
        let mut cursor = 0usize;

        // Execute each matcher sequentially
        for (matcher_idx, matcher) in matchers.iter().enumerate() {
            // Call matcher(path_object, current_cursor_index)
            let consumed_value = match eval.eval_function(
                *matcher,
                &[self.path_value.to_value(), heap.alloc(cursor as i32)],
                &[],
            ) {
                Ok(value) => value,
                Err(e) => {
                    if suppress_errors {
                        // Return false instead of erroring
                        return Ok(heap.alloc(false));
                    } else {
                        return Err(self.wrap_matcher_error(e, eval));
                    }
                }
            };

            // Extract consumed count
            let consumed = consumed_value.unpack_i32().ok_or_else(|| {
                starlark::Error::new_other(anyhow::anyhow!(
                    "Matcher {} did not return an int",
                    matcher_idx + 1
                ))
            })?;

            if consumed < 0 {
                return Err(starlark::Error::new_other(anyhow::anyhow!(
                    "Matcher {} returned negative consumption: {}",
                    matcher_idx + 1,
                    consumed
                )));
            }

            // Advance cursor
            cursor = cursor.checked_add(consumed as usize).ok_or_else(|| {
                starlark::Error::new_other(anyhow::anyhow!(
                    "Integer overflow in cursor advancement"
                ))
            })?;

            // Check cursor bounds
            if cursor > components.len() {
                if suppress_errors {
                    return Ok(heap.alloc(false));
                } else {
                    return Err(starlark::Error::new_other(anyhow::anyhow!(
                        "Matcher {} consumed past end of path (cursor {} > path length {})",
                        matcher_idx + 1,
                        cursor,
                        components.len()
                    )));
                }
            }
        }

        // Verify all components were consumed
        if cursor != components.len() {
            if suppress_errors {
                return Ok(heap.alloc(false));
            } else {
                return Err(starlark::Error::new_other(anyhow::anyhow!(
                    "Unconsumed components remaining ({} left)",
                    components.len() - cursor
                )));
            }
        }

        // Success - all matchers executed and consumed entire path
        Ok(heap.alloc(true))
    }
}
