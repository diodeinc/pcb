use crate::graph::CircuitGraph;
use crate::{downcast_frozen_module, lang::module::FrozenModuleValue};
use allocative::Allocative;
use starlark::{
    eval::{Arguments, Evaluator},
    starlark_complex_value,
    values::{
        starlark_value, Coerce, Freeze, FreezeResult, Heap, NoSerialize, ProvidesStaticType,
        StarlarkValue, Trace, Value, ValueLifetimeless, ValueLike,
    },
};
use std::sync::Arc;

/// ModuleGraph that contains the circuit graph and module reference
#[derive(Clone, Debug, Coerce, Trace, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub struct ModuleGraphValueGen<V: ValueLifetimeless> {
    pub module: V,
    #[freeze(identity)]
    pub graph: Arc<CircuitGraph>,
}

starlark_complex_value!(pub ModuleGraphValue);

/// PathsCallable for the ModuleGraph.paths() method
#[derive(Clone, Debug, Coerce, Trace, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub struct PathsCallableGen<V: ValueLifetimeless> {
    pub module: V,
    #[freeze(identity)]
    pub graph: Arc<CircuitGraph>,
}

starlark_complex_value!(pub PathsCallable);

/// Path object representing a circuit path with pre-computed data
#[derive(Clone, Debug, Coerce, Trace, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub struct PathValueGen<V: ValueLifetimeless> {
    pub ports: Vec<V>,      // List of port tuples
    pub components: Vec<V>, // List of component objects
    pub nets: Vec<V>,       // List of net objects
}

starlark_complex_value!(pub PathValue);

/// Callables for Path validation methods
#[derive(Clone, Debug, PartialEq, Eq, Allocative, Freeze)]
pub enum PathValidationOp {
    Count,
    Any,
    All,
    None,
}

#[derive(Clone, Debug, Coerce, Trace, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub struct PathValidationCallableGen<V: ValueLifetimeless> {
    pub path_value: V,
    pub operation: PathValidationOp,
}

starlark_complex_value!(pub PathValidationCallable);

// Implementations for ModuleGraphValue
impl<V: ValueLifetimeless> std::fmt::Display for ModuleGraphValueGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ModuleGraph")
    }
}

#[starlark_value(type = "ModuleGraph")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for ModuleGraphValueGen<V>
where
    Self: ProvidesStaticType<'v>,
{
    fn get_attr(&self, attr: &str, heap: &'v Heap) -> Option<Value<'v>> {
        match attr {
            "paths" => {
                let callable = PathsCallableGen {
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
impl<V: ValueLifetimeless> std::fmt::Display for PathsCallableGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "paths")
    }
}

#[starlark_value(type = "builtin_function_or_method")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for PathsCallableGen<V>
where
    Self: ProvidesStaticType<'v>,
{
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
            .unwrap_or(10) as usize;

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

        // Find all simple paths using the CircuitGraph
        let mut paths = Vec::new();
        self.graph
            .all_simple_paths(start_port, end_port, max_depth, |path| {
                paths.push(path.to_vec());
            });

        // Convert paths to PathValue objects
        let module_ref = downcast_frozen_module!(self.module);

        let components = module_ref.collect_components("");
        let path_objects: Vec<Value> = paths
            .into_iter()
            .map(|port_path| self.graph.create_path_value(&port_path, &components, heap))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(heap.alloc(path_objects))
    }
}

// PathValue implementation
impl<V: ValueLifetimeless> std::fmt::Display for PathValueGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Path({} components)", self.components.len())
    }
}

#[starlark_value(type = "Path")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for PathValueGen<V>
where
    Self: ProvidesStaticType<'v>,
{
    fn get_attr(&self, attr: &str, heap: &'v Heap) -> Option<Value<'v>> {
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
            _ => None,
        }
    }
}

impl<'v, V: ValueLike<'v>> PathValueGen<V>
where
    Self: ProvidesStaticType<'v>,
{
    fn create_validation_callable(&self, heap: &'v Heap, operation: PathValidationOp) -> Value<'v> {
        let callable = PathValidationCallableGen {
            path_value: heap.alloc_complex(self.clone()).to_value(),
            operation,
        };
        heap.alloc_complex(callable)
    }
}

// PathValidationCallable implementation
impl<V: ValueLifetimeless> std::fmt::Display for PathValidationCallableGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.operation {
            PathValidationOp::Count => write!(f, "count"),
            PathValidationOp::Any => write!(f, "any"),
            PathValidationOp::All => write!(f, "all"),
            PathValidationOp::None => write!(f, "none"),
        }
    }
}

#[starlark_value(type = "builtin_function_or_method")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for PathValidationCallableGen<V>
where
    Self: ProvidesStaticType<'v>,
{
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
            .downcast_ref::<PathValueGen<Value>>()
            .ok_or_else(|| starlark::Error::new_other(anyhow::anyhow!("Invalid path value")))?;

        match self.operation {
            PathValidationOp::Count => {
                let mut count = 0;
                self.eval_components(path_value, matcher, eval, |matches| {
                    if matches {
                        count += 1;
                    }
                    None::<Value>
                })?;
                Ok(heap.alloc(count))
            }
            PathValidationOp::Any => self
                .eval_components(path_value, matcher, eval, |matches| {
                    if matches {
                        Some(Value::new_bool(true))
                    } else {
                        None
                    }
                })
                .or_else(|_| Ok(Value::new_bool(false))),
            PathValidationOp::All => self
                .eval_components(path_value, matcher, eval, |matches| {
                    if !matches {
                        Some(Value::new_bool(false))
                    } else {
                        None
                    }
                })
                .or_else(|_| Ok(Value::new_bool(true))),
            PathValidationOp::None => self
                .eval_components(path_value, matcher, eval, |matches| {
                    if matches {
                        Some(Value::new_bool(false))
                    } else {
                        None
                    }
                })
                .or_else(|_| Ok(Value::new_bool(true))),
        }
    }
}

impl<'v, V: ValueLike<'v>> PathValidationCallableGen<V>
where
    Self: ProvidesStaticType<'v>,
{
    fn eval_components<F, R>(
        &self,
        path_value: &PathValueGen<Value<'v>>,
        matcher: Value<'v>,
        eval: &mut Evaluator<'v, '_, '_>,
        mut handler: F,
    ) -> starlark::Result<R>
    where
        F: FnMut(bool) -> Option<R>,
    {
        for component in &path_value.components {
            let result = eval.eval_function(matcher, &[component.to_value()], &[])?;
            let matches = result.unpack_bool().unwrap_or(false);

            if let Some(early_return) = handler(matches) {
                return Ok(early_return);
            }
        }
        // If we get here, no early return occurred - let caller handle the completion case
        Err(starlark::Error::new_other(anyhow::anyhow!(
            "No early return in eval_components"
        )))
    }
}
