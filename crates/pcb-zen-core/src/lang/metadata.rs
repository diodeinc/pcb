use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use allocative::Allocative;
use anyhow::anyhow;
use starlark::any::ProvidesStaticType;
use starlark::environment::GlobalsBuilder;
use starlark::eval::Evaluator;
use starlark::starlark_module;
use starlark::starlark_simple_value;
use starlark::values::list::AllocList;
use starlark::values::starlark_value;
use starlark::values::{Demand, Heap, NoSerialize, StarlarkValue, Value, ValueLike};

use crate::lang::eval::DeepCopyToHeap;
use crate::lang::evaluator_ext::EvaluatorExt;
use crate::lang::input::InputValue;
use crate::lang::module::validate_or_convert;

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
    containers: HashMap<String, Vec<InputValue>>,
    /// Map from reference ID to type name strings for validation and deserialization
    type_names: HashMap<String, String>,
    /// Atomic counter for generating unique reference IDs
    ref_allocator: AtomicU64,
}

impl Default for MetadataStore {
    fn default() -> Self {
        Self {
            containers: HashMap::new(),
            type_names: HashMap::new(),
            ref_allocator: AtomicU64::new(1),
        }
    }
}

impl MetadataStore {
    /// Generate a unique reference ID
    pub fn allocate_ref(&self) -> String {
        format!("meta_{}", self.ref_allocator.fetch_add(1, Ordering::SeqCst))
    }

    /// Register type name for a container
    pub fn register_type_name(&mut self, ref_id: &str, type_name: String) {
        self.type_names.insert(ref_id.to_string(), type_name);
    }

    /// Get type name for a container
    pub fn get_type_name(&self, ref_id: &str) -> Option<&String> {
        self.type_names.get(ref_id)
    }

    /// Push an InputValue to a container
    pub fn push(&mut self, ref_id: &str, input_value: InputValue) -> anyhow::Result<()> {
        self.containers
            .entry(ref_id.to_string())
            .or_default()
            .push(input_value);
        Ok(())
    }

    /// Get the most recently pushed value (latest)
    pub fn get_latest(&self, ref_id: &str) -> Option<&InputValue> {
        self.containers.get(ref_id)?.last()
    }

    /// Get all values in chronological order
    pub fn get_all(&self, ref_id: &str) -> Option<&Vec<InputValue>> {
        self.containers.get(ref_id)
    }
}

/// A metadata container that holds only a reference ID.
/// The actual data and type information are stored in the global MetadataStore.
#[derive(Debug, Clone, NoSerialize, ProvidesStaticType, Allocative)]
pub struct MetadataContainer {
    /// Unique reference ID for this container
    pub ref_id: String,
}

impl MetadataContainer {
    pub fn new(ref_id: String) -> Self {
        Self { ref_id }
    }

    /// Get the type name for display purposes - requires access to MetadataStore
    pub fn type_name(&self, store: &MetadataStore) -> String {
        store
            .get_type_name(&self.ref_id)
            .cloned()
            .unwrap_or_else(|| "unknown".to_string())
    }
}

starlark_simple_value!(MetadataContainer);

impl std::fmt::Display for MetadataContainer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MetadataContainer({})", self.ref_id)
    }
}

#[starlark_value(type = "MetadataContainer")]
impl<'v> StarlarkValue<'v> for MetadataContainer {
    type Canonical = MetadataContainer;

    fn provide(&'v self, demand: &mut Demand<'_, 'v>) {
        demand.provide_value::<&dyn DeepCopyToHeap>(self);
    }
}

impl DeepCopyToHeap for MetadataContainer {
    fn deep_copy_to<'dst>(&self, dst: &'dst Heap) -> anyhow::Result<Value<'dst>> {
        // MetadataContainer can be copied directly since it only contains a reference ID
        Ok(dst.alloc(self.clone()))
    }
}

/// Extract the type name that should be stored for a type constructor value.
fn extract_type_name_for_storage<'v>(
    type_value: Value<'v>,
    eval: &Evaluator<'v, '_, '_>,
) -> String {
    let repr = type_value.to_string();

    match (type_value.get_type(), repr.as_str()) {
        ("function", builtin @ ("str" | "int" | "float" | "bool" | "list" | "dict")) => {
            builtin.to_string()
        }
        ("function", _) => {
            // For custom types, find the variable name in the module
            eval.module()
                .names()
                .filter_map(|name| {
                    let name_str = name.as_str();
                    eval.module()
                        .get(name_str)
                        .filter(|&value| value == type_value)
                        .map(|_| name_str.to_string())
                })
                .next()
                .unwrap_or(repr)
        }
        _ => repr, // Built-in types or other types
    }
}

/// Extract MetadataContainer from a Starlark Value
fn extract_container(value: Value<'_>) -> anyhow::Result<&MetadataContainer> {
    value
        .downcast_ref::<MetadataContainer>()
        .ok_or_else(|| anyhow!("Expected MetadataContainer"))
}

/// Get the type value for deserialization based on stored type name
fn get_type_value_for_deserialization<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    ref_id: &str,
) -> anyhow::Result<Option<Value<'v>>> {
    let type_name = with_metadata_store(eval, |store| store.get_type_name(ref_id).cloned())?
        .ok_or_else(|| anyhow!("No type information found for container {}", ref_id))?;

    // Built-in types don't need type guidance for deserialization
    match type_name.as_str() {
        "str" | "int" | "float" | "bool" | "list" | "dict" => Ok(None),
        _ => Ok(eval.module().get(&type_name)), // Look up custom type constructor
    }
}

/// Create a new MetadataContainer from an existing one, preserving the type but with a new ref_id.
/// This is used when interface fields need their own metadata containers.
pub fn clone_metadata_container<'v>(
    original_container: Value<'v>,
    eval: &mut Evaluator<'v, '_, '_>,
) -> anyhow::Result<Value<'v>> {
    let original = extract_container(original_container)?;

    // Get the type name from the original container
    let type_name =
        with_metadata_store(eval, |store| store.get_type_name(&original.ref_id).cloned())?
            .ok_or_else(|| {
                anyhow!(
                    "No type information found for container {}",
                    original.ref_id
                )
            })?;

    // Allocate a new ref_id
    let new_ref_id = with_metadata_store(eval, |store| store.allocate_ref())?;

    // Register the type name for the new container
    with_metadata_store_mut(eval, |store| {
        store.register_type_name(&new_ref_id, type_name);
        Ok(())
    })?;

    // Create and return the new container
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

        let type_name = extract_type_name_for_storage(type_value, eval);

        // Register the type name in the store
        with_metadata_store_mut(eval, |store| {
            store.register_type_name(&ref_id, type_name);
            Ok(())
        })?;

        let container = MetadataContainer::new(ref_id);
        Ok(eval.heap().alloc(container))
    }

    /// Push a value to a metadata container
    fn push_metadata<'v>(
        container: Value<'v>,
        value: Value<'v>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let container = extract_container(container)?;

        let type_value_opt = get_type_value_for_deserialization(eval, &container.ref_id)?;

        let validated_value = type_value_opt
            .map(|type_value| validate_or_convert("metadata_value", value, type_value, None, eval))
            .transpose()?
            .unwrap_or(value); // Built-in types - accept as-is

        let input_value = InputValue::from_value(validated_value);
        with_metadata_store_mut(eval, |store| store.push(&container.ref_id, input_value))?;

        Ok(Value::new_none())
    }

    /// Get all values from a metadata container as a list of automatically deserialized Starlark values
    fn list_metadata<'v>(
        container: Value<'v>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let container = extract_container(container)?;

        let input_values = with_metadata_store(eval, |store| {
            store
                .get_all(&container.ref_id)
                .cloned()
                .unwrap_or_default()
        })?;

        let type_value = get_type_value_for_deserialization(eval, &container.ref_id)?;
        let list_values = input_values
            .iter()
            .map(|input_value| input_value.to_value(eval, type_value))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(eval.heap().alloc(AllocList(list_values)).to_value())
    }

    /// Get the most recent value from a metadata container, automatically deserialized
    fn get_metadata<'v>(
        container: Value<'v>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let container = extract_container(container)?;

        match with_metadata_store(eval, |store| store.get_latest(&container.ref_id).cloned())? {
            Some(input_value) => {
                let type_value = get_type_value_for_deserialization(eval, &container.ref_id)?;
                input_value.to_value(eval, type_value)
            }
            None => Ok(Value::new_none()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metadata_store_allocation() {
        let store = MetadataStore::default();
        let ref1 = store.allocate_ref();
        let ref2 = store.allocate_ref();

        assert_ne!(ref1, ref2);
        assert!(ref1.starts_with("meta_"));
        assert!(ref2.starts_with("meta_"));
    }

    #[test]
    fn test_metadata_store_push_get() {
        let mut store = MetadataStore::default();
        let ref_id = store.allocate_ref();

        store
            .push(&ref_id, InputValue::String("hello".to_string()))
            .unwrap();
        store
            .push(&ref_id, InputValue::String("world".to_string()))
            .unwrap();

        assert_eq!(
            store.get_latest(&ref_id),
            Some(&InputValue::String("world".to_string()))
        );
        assert_eq!(store.get_all(&ref_id).unwrap().len(), 2);
    }

    #[test]
    fn test_metadata_container_creation() {
        let container = MetadataContainer::new("test_ref".to_string());
        assert_eq!(container.ref_id, "test_ref");
    }

    #[test]
    fn test_metadata_type_name_storage() {
        let mut store = MetadataStore::default();
        let ref_id = store.allocate_ref();

        // Register type name
        store.register_type_name(&ref_id, "str".to_string());

        // Check type name retrieval
        assert_eq!(store.get_type_name(&ref_id), Some(&"str".to_string()));

        // Test type name display with store
        let container = MetadataContainer::new(ref_id.clone());
        assert_eq!(container.type_name(&store), "str");
    }
}
