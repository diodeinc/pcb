use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use allocative::Allocative;
use anyhow::anyhow;
use serde::{Deserialize, Serialize};
use starlark::any::ProvidesStaticType;
use starlark::environment::GlobalsBuilder;
use starlark::eval::{Arguments, Evaluator};
use starlark::starlark_module;
use starlark::starlark_simple_value;
use starlark::typing::Ty;
use starlark::values::list::AllocList;
use starlark::values::starlark_value;
use starlark::values::typing::TypeCompiled;
use starlark::values::{Freeze, Heap, NoSerialize, StarlarkValue, Value, ValueLike};

use crate::lang::evaluator_ext::EvaluatorExt;
use crate::lang::input::InputValue;
use crate::lang::physical::{physical_unit_from_ty, PhysicalValue};

/// Helper to access metadata store with error handling
fn with_metadata_store<T>(
    eval: &Evaluator,
    f: impl FnOnce(&MetadataStore) -> T,
) -> anyhow::Result<T> {
    eval.context_value()
        .ok_or_else(|| anyhow!("No evaluation context available for metadata operation"))?
        .parent_context()
        .with_metadata_store(f)
}

/// Helper to access mutable metadata store with error handling  
fn with_metadata_store_mut<T>(
    eval: &Evaluator,
    f: impl FnOnce(&mut MetadataStore) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    eval.context_value()
        .ok_or_else(|| anyhow!("No evaluation context available for metadata operation"))?
        .parent_context()
        .with_metadata_store_mut(f)?
}

/// Global storage for metadata containers that persists across module boundaries.
/// Uses InputValue for storage to ensure values can cross module boundaries efficiently.
#[derive(Debug)]
pub struct MetadataStore {
    /// Map from reference ID to list of InputValue entries
    containers: HashMap<u64, Vec<InputValue>>,
    /// Map from reference ID to type name strings for validation and deserialization
    types: HashMap<u64, Ty>,
    /// Atomic counter for generating unique reference IDs
    ref_allocator: AtomicU64,
}

impl Default for MetadataStore {
    fn default() -> Self {
        Self {
            containers: HashMap::new(),
            types: HashMap::new(),
            ref_allocator: AtomicU64::new(1),
        }
    }
}

impl MetadataStore {
    /// Generate a unique reference ID
    pub fn allocate_ref(&self) -> u64 {
        self.ref_allocator.fetch_add(1, Ordering::SeqCst)
    }

    /// Register type name for a container
    pub fn register_ty(&mut self, ref_id: u64, ty: Ty) {
        self.types.insert(ref_id, ty);
    }

    /// Get type name for a container
    pub fn ty(&self, ref_id: u64) -> Option<&Ty> {
        self.types.get(&ref_id)
    }

    /// Push an InputValue to a container
    pub fn push(&mut self, ref_id: u64, input_value: InputValue) -> anyhow::Result<()> {
        self.containers.entry(ref_id).or_default().push(input_value);
        Ok(())
    }

    /// Get the most recently pushed value (latest)
    pub fn get_latest(&self, ref_id: u64) -> Option<&InputValue> {
        self.containers.get(&ref_id)?.last()
    }

    /// Get all values in chronological order
    pub fn get_all(&self, ref_id: u64) -> Option<&Vec<InputValue>> {
        self.containers.get(&ref_id)
    }
}

/// A metadata container that holds a reference ID
#[derive(
    Debug, Copy, Clone, PartialEq, ProvidesStaticType, Serialize, Deserialize, Allocative, Freeze,
)]
pub struct MetadataContainer {
    pub ref_id: u64,
}

starlark_simple_value!(MetadataContainer);

impl MetadataContainer {
    pub fn new(ref_id: u64) -> Self {
        Self { ref_id }
    }
}

impl std::fmt::Display for MetadataContainer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MetadataContainer({})", self.ref_id)
    }
}

#[starlark_value(type = "MetadataContainer")]
impl<'v> StarlarkValue<'v> for MetadataContainer {
    type Canonical = MetadataContainer;

    fn get_attr(&self, attribute: &str, heap: &'v Heap) -> Option<Value<'v>> {
        match attribute {
            "push" => {
                let push_wrapper = MetadataContainerPush::new(*self);
                Some(heap.alloc(push_wrapper))
            }
            "get" => {
                let get_wrapper = MetadataContainerGet::new(*self);
                Some(heap.alloc(get_wrapper))
            }
            "list" => {
                let list_wrapper = MetadataContainerList::new(*self);
                Some(heap.alloc(list_wrapper))
            }
            _ => None,
        }
    }

    fn has_attr(&self, attribute: &str, _heap: &'v Heap) -> bool {
        matches!(attribute, "push" | "get" | "list")
    }

    fn dir_attr(&self) -> Vec<String> {
        vec!["push".to_string(), "get".to_string(), "list".to_string()]
    }
}

/// Callable wrapper for metadata container push operation
#[derive(Debug, Copy, Clone, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
pub struct MetadataContainerPush {
    container: MetadataContainer,
}

starlark_simple_value!(MetadataContainerPush);

impl MetadataContainerPush {
    pub fn new(container: MetadataContainer) -> Self {
        Self { container }
    }
}

impl std::fmt::Display for MetadataContainerPush {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MetadataContainer.push")
    }
}

#[starlark_value(type = "MetadataContainerPush")]
impl<'v> StarlarkValue<'v> for MetadataContainerPush {
    type Canonical = MetadataContainerPush;

    fn invoke(
        &self,
        _me: Value<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        let heap = eval.heap();
        let positional: Vec<_> = args.positions(heap)?.collect();

        if positional.len() != 1 {
            return Err(starlark::Error::new_other(anyhow::anyhow!(
                "push() takes exactly 1 argument ({} given)",
                positional.len()
            )));
        }

        let value = positional[0];
        let Some(ty) = with_metadata_store(eval, |store| store.ty(self.container.ref_id).cloned())?
        else {
            return Err(starlark::Error::new_other(anyhow::anyhow!(
                "Type not found for container ref_id: {}",
                self.container.ref_id
            )));
        };
        let type_compiled = TypeCompiled::from_ty(&ty, heap);
        if !type_compiled.matches(value) {
            return Err(
                anyhow::anyhow!("Expected type {}, received {}", &type_compiled, value).into(),
            );
        }
        // If physical value, do unit checking in addition to type checking
        if let Some(expected) = physical_unit_from_ty(type_compiled.as_ty()) {
            if let Some(physical_value) = value.downcast_ref::<PhysicalValue>() {
                physical_value.check_unit(expected.into())?;
            }
        }

        let input_value = InputValue::from_value(value);
        with_metadata_store_mut(eval, |store| store.push(self.container.ref_id, input_value))?;

        Ok(Value::new_none())
    }
}

/// Callable wrapper for metadata container get operation
#[derive(Debug, Clone, Copy, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
pub struct MetadataContainerGet {
    container: MetadataContainer,
}

starlark_simple_value!(MetadataContainerGet);

impl MetadataContainerGet {
    pub fn new(container: MetadataContainer) -> Self {
        Self { container }
    }
}

impl std::fmt::Display for MetadataContainerGet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MetadataContainer.get")
    }
}

#[starlark_value(type = "MetadataContainerGet")]
impl<'v> StarlarkValue<'v> for MetadataContainerGet {
    type Canonical = MetadataContainerGet;

    fn invoke(
        &self,
        _me: Value<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        let heap = eval.heap();
        let positional: Vec<_> = args.positions(heap)?.collect();

        if !positional.is_empty() {
            return Err(starlark::Error::new_other(anyhow::anyhow!(
                "get() takes no arguments ({} given)",
                positional.len()
            )));
        }
        match with_metadata_store(eval, |store| {
            store.get_latest(self.container.ref_id).cloned()
        })? {
            Some(input_value) => Ok(input_value.to_value(eval, None)?),
            None => Ok(Value::new_none()),
        }
    }
}

/// Callable wrapper for metadata container list operation
#[derive(Debug, Clone, Copy, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
pub struct MetadataContainerList {
    container: MetadataContainer,
}

starlark_simple_value!(MetadataContainerList);

impl MetadataContainerList {
    pub fn new(container: MetadataContainer) -> Self {
        Self { container }
    }
}

impl std::fmt::Display for MetadataContainerList {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MetadataContainer.list")
    }
}

#[starlark_value(type = "MetadataContainerList")]
impl<'v> StarlarkValue<'v> for MetadataContainerList {
    type Canonical = MetadataContainerList;

    fn invoke(
        &self,
        _me: Value<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        let heap = eval.heap();
        let positional: Vec<_> = args.positions(heap)?.collect();

        if !positional.is_empty() {
            return Err(starlark::Error::new_other(anyhow::anyhow!(
                "list() takes no arguments ({} given)",
                positional.len()
            )));
        }

        let input_values = with_metadata_store(eval, |store| {
            store
                .get_all(self.container.ref_id)
                .cloned()
                .unwrap_or_default()
        })?;

        let list_values = input_values
            .iter()
            .map(|input_value| input_value.to_value(eval, None))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(eval.heap().alloc(AllocList(list_values)).to_value())
    }
}

/// Check if a type is supported by metadata containers
fn is_supported_metadata_type(ty: &Ty) -> bool {
    let type_str = ty.to_string();

    // Check for basic types
    if matches!(
        type_str.as_str(),
        "str" | "int" | "float" | "bool" | "list" | "dict"
    ) {
        return true;
    }

    // Check for PhysicalValue types
    if physical_unit_from_ty(ty).is_some() {
        return true;
    }

    false
}

/// Create a new MetadataContainer from an existing one, preserving the type but with a new ref_id.
/// This is used when interface fields need their own metadata containers.
pub fn clone_metadata_container<'v>(
    original_container: Value<'v>,
    eval: &mut Evaluator<'v, '_, '_>,
) -> anyhow::Result<Value<'v>> {
    // Allocate a new ref_id
    let new_ref_id = with_metadata_store(eval, |store| store.allocate_ref())?;

    // Extract the Ty from the original's type_value and register it in the store
    let metadata_container = *original_container
        .downcast_ref::<MetadataContainer>()
        .ok_or(anyhow::anyhow!("Expected MetadataContainer"))?;
    with_metadata_store_mut(eval, |store| {
        let ty = store.ty(metadata_container.ref_id).ok_or(anyhow::anyhow!(
            "Type not found for container ref_id: {}",
            metadata_container.ref_id
        ))?;
        store.register_ty(new_ref_id, ty.clone());
        Ok(())
    })?;

    // Create and return the new container with the same type_value
    let new_container = MetadataContainer::new(new_ref_id);
    Ok(eval.heap().alloc(new_container))
}

#[starlark_module]
pub fn metadata_globals(builder: &mut GlobalsBuilder) {
    /// Create a new typed metadata container
    fn metadata<'v>(
        type_value: Value<'v>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let ref_id = with_metadata_store(eval, |store| store.allocate_ref())?;
        let type_compiled = TypeCompiled::new(type_value, eval.heap())?;
        let ty = type_compiled.as_ty().clone();

        // Validate that the type is supported
        if !is_supported_metadata_type(&ty) {
            return Err(anyhow!(
                "Unsupported type for metadata container: {}. Only basic types (str, int, float, bool, list, dict) and PhysicalValue types are supported.",
                ty
            ));
        }

        // Register the type name in the store
        with_metadata_store_mut(eval, |store| {
            store.register_ty(ref_id, ty);
            Ok(())
        })?;

        let container = MetadataContainer::new(ref_id);
        Ok(eval.heap().alloc(container))
    }
}
