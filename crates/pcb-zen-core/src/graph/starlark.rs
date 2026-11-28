use crate::downcast_frozen_module;
use crate::graph::CircuitGraph;
use crate::lang::evaluator_ext::EvaluatorExt;
use allocative::Allocative;
use starlark::{
    environment::MethodsBuilder,
    eval::Evaluator,
    starlark_complex_value, starlark_module,
    values::{
        Freeze, Heap, NoSerialize, ProvidesStaticType, StarlarkValue, Trace, Value, ValueOf,
        ValueTyped,
        list::AllocList,
        none::{NoneOr, NoneType},
        starlark_value,
        tuple::StarlarkTuple,
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

    fn wrap_matcher_error(
        &self,
        original_error: starlark::Error,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Error {
        let Some(cs) = eval.call_stack_top_location() else {
            return original_error;
        };

        // Build child diagnostic directly from the Starlark error
        let child = crate::diagnostics::Diagnostic::from(original_error);

        // Create parent diagnostic with path.matches() context
        let parent = crate::diagnostics::Diagnostic {
            path: cs.file.filename().to_string(),
            span: Some(cs.resolve_span()),
            severity: starlark::errors::EvalSeverity::Error,
            body: format!("{} failed to match sequence", self.description()),
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

#[starlark_value(type = "ModuleGraph", methods = module_graph_methods)]
impl<'v> StarlarkValue<'v> for ModuleGraphValue<'v> {}

#[starlark_module]
fn module_graph_methods(methods: &mut MethodsBuilder) {
    fn paths<'v>(
        this: &ModuleGraphValue<'v>,
        #[starlark(require = named)] start: Value<'v>,
        #[starlark(require = named)] end: Value<'v>,
        #[starlark(require = named, default = NoneType)] max_depth: NoneOr<i32>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        let heap = eval.heap();
        let max_depth = max_depth.into_option().map(|d| d as usize);

        // Resolve start and end labels to PortIds
        let start_port = this.graph.resolve_label_to_port(start, heap)?;
        let end_port = this.graph.resolve_label_to_port(end, heap)?;

        // Find all simple paths using the CircuitGraph with factor tracking
        let mut paths_with_factors = Vec::new();
        this.graph.all_simple_paths_with_factors(
            start_port,
            end_port,
            max_depth,
            |path, factors| {
                paths_with_factors.push((path.to_vec(), factors.to_vec()));
            },
        );

        // Convert paths to PathValue objects
        let module_ref = downcast_frozen_module!(this.module);
        let components = eval.collect_components(module_ref.path());
        let path_objects: Vec<Value> = paths_with_factors
            .into_iter()
            .map(|(port_path, factors)| {
                this.graph
                    .create_path_value(&port_path, &factors, &components, heap)
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(heap.alloc(AllocList(path_objects)).to_value())
    }
}

// PathValue implementation
impl<'v> std::fmt::Display for PathValue<'v> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Path({} components)", self.components.len())
    }
}

#[starlark_value(type = "Path", methods = path_methods)]
impl<'v> StarlarkValue<'v> for PathValue<'v> {}

#[starlark_module]
fn path_methods(methods: &mut MethodsBuilder) {
    #[starlark(attribute)]
    fn ports<'v>(this: &PathValue<'v>, heap: Heap<'v>) -> starlark::Result<Value<'v>> {
        Ok(heap.alloc(AllocList(this.ports.clone())).to_value())
    }

    #[starlark(attribute)]
    fn components<'v>(this: &PathValue<'v>, heap: Heap<'v>) -> starlark::Result<Value<'v>> {
        Ok(heap.alloc(AllocList(this.components.clone())).to_value())
    }

    #[starlark(attribute)]
    fn nets<'v>(this: &PathValue<'v>, heap: Heap<'v>) -> starlark::Result<Value<'v>> {
        Ok(heap.alloc(AllocList(this.nets.clone())).to_value())
    }

    fn count<'v>(
        this: &PathValue<'v>,
        #[starlark(require = pos)] matcher: Value<'v>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<i32> {
        let mut count = 0;
        for component in &this.components {
            if eval
                .eval_function(matcher, &[component.to_value()], &[])
                .is_ok()
            {
                count += 1;
            }
        }
        Ok(count)
    }

    fn any<'v>(
        this: &PathValue<'v>,
        #[starlark(require = pos)] matcher: Value<'v>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<NoneType> {
        for component in &this.components {
            if eval
                .eval_function(matcher, &[component.to_value()], &[])
                .is_ok()
            {
                return Ok(NoneType);
            }
        }
        Err(starlark::Error::new_other(anyhow::anyhow!(
            "No components matched the condition"
        )))
    }

    fn all<'v>(
        this: &PathValue<'v>,
        #[starlark(require = pos)] matcher: Value<'v>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<NoneType> {
        for component in &this.components {
            eval.eval_function(matcher, &[component.to_value()], &[])?;
        }
        Ok(NoneType)
    }

    fn none<'v>(
        this: &PathValue<'v>,
        #[starlark(require = pos)] matcher: Value<'v>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<NoneType> {
        for component in &this.components {
            if eval
                .eval_function(matcher, &[component.to_value()], &[])
                .is_ok()
            {
                return Err(starlark::Error::new_other(anyhow::anyhow!(
                    "Found component that matched the condition (expected none)"
                )));
            }
        }
        Ok(NoneType)
    }

    fn matches<'v>(
        this: ValueTyped<'v, PathValue<'v>>,
        #[starlark(args)] matchers: ValueOf<'v, StarlarkTuple<'v>>,
        #[starlark(require = named, default = false)] suppress_errors: bool,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<bool> {
        let heap = eval.heap();

        if matchers.is_empty() {
            return Err(starlark::Error::new_other(anyhow::anyhow!(
                "matches() requires at least one matcher function"
            )));
        }

        let components = &this.components;
        let mut cursor = 0usize;

        for (matcher_idx, matcher) in matchers.values().enumerate() {
            let consumed_value = match eval.eval_function(
                matcher,
                &[this.to_value(), heap.alloc(cursor as i32).to_value()],
                &[],
            ) {
                Ok(value) => value,
                Err(e) => {
                    if suppress_errors {
                        return Ok(false);
                    } else {
                        return Err(this.wrap_matcher_error(e, eval));
                    }
                }
            };

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

            cursor = cursor.checked_add(consumed as usize).ok_or_else(|| {
                starlark::Error::new_other(anyhow::anyhow!(
                    "Integer overflow in cursor advancement"
                ))
            })?;

            if cursor > components.len() {
                if suppress_errors {
                    return Ok(false);
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

        if cursor != components.len() {
            if suppress_errors {
                return Ok(false);
            } else {
                return Err(starlark::Error::new_other(anyhow::anyhow!(
                    "Unconsumed components remaining ({} left)",
                    components.len() - cursor
                )));
            }
        }

        Ok(true)
    }
}
