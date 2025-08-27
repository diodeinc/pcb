use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use allocative::Allocative;
use anyhow::anyhow;
use serde_json;
use starlark::any::ProvidesStaticType;
use starlark::environment::GlobalsBuilder;
use starlark::eval::Evaluator;
use starlark::starlark_module;
use starlark::starlark_simple_value;
use starlark::values::list::AllocList;
use starlark::values::starlark_value;
use starlark::values::types::record::ty_record_type::TyRecordData;
use starlark::values::types::record::{FrozenRecordType, RecordType};
use starlark::values::{Demand, Heap, NoSerialize, StarlarkValue, Value, ValueLike};

use crate::lang::eval::DeepCopyToHeap;
use crate::lang::evaluator_ext::EvaluatorExt;
use crate::lang::input::InputValue;

/// Helper to access metadata store with error handling
fn with_metadata_store<T, F>(eval: &Evaluator, f: F) -> anyhow::Result<T>
where
    F: FnOnce(&MetadataStore) -> T,
{
    eval.context_value()
        .ok_or_else(|| anyhow!("No evaluation context available for metadata operation"))?
        .parent_context()
        .with_metadata_store(f)
}

/// Helper to access mutable metadata store with error handling  
fn with_metadata_store_mut<T, F>(eval: &Evaluator, f: F) -> anyhow::Result<anyhow::Result<T>>
where
    F: FnOnce(&mut MetadataStore) -> anyhow::Result<T>,
{
    eval.context_value()
        .ok_or_else(|| anyhow!("No evaluation context available for metadata operation"))?
        .parent_context()
        .with_metadata_store_mut(f)
}

/// Type information stored in metadata containers
#[derive(Debug, Clone, Allocative)]
pub enum MetadataTypeInfo {
    /// Simple built-in types (str, int, float, bool, etc.)
    Simple(String),
    /// Complex record types with full type information
    Record(Arc<TyRecordData>),
}

/// Global storage for metadata containers that persists across module boundaries.
/// Uses JSON strings for storage to ensure values can cross module boundaries.
#[derive(Debug)]
pub struct MetadataStore {
    /// Map from reference ID to list of JSON-serialized values
    containers: HashMap<String, Vec<String>>,
    /// Atomic counter for generating unique reference IDs
    ref_allocator: AtomicU64,
}

impl Default for MetadataStore {
    fn default() -> Self {
        Self {
            containers: HashMap::new(),
            ref_allocator: AtomicU64::new(1),
        }
    }
}

impl MetadataStore {
    /// Generate a unique reference ID
    pub fn allocate_ref(&self) -> String {
        format!("meta_{}", self.ref_allocator.fetch_add(1, Ordering::SeqCst))
    }

    /// Push a JSON-serialized value to a container
    pub fn push(&mut self, ref_id: &str, json_value: String) -> anyhow::Result<()> {
        self.containers
            .entry(ref_id.to_string())
            .or_default()
            .push(json_value);
        Ok(())
    }

    /// Get the most recently pushed value (latest)
    pub fn get_latest(&self, ref_id: &str) -> Option<&String> {
        self.containers.get(ref_id)?.last()
    }

    /// Get all values in chronological order
    pub fn get_all(&self, ref_id: &str) -> Option<&Vec<String>> {
        self.containers.get(ref_id)
    }
}

/// A metadata container that holds a reference ID and type information.
/// The actual data is stored in the global MetadataStore.
#[derive(Debug, Clone, NoSerialize, ProvidesStaticType, Allocative)]
pub struct MetadataContainer {
    /// Unique reference ID for this container
    pub ref_id: String,
    /// Type information for validation and deserialization
    pub type_info: MetadataTypeInfo,
}

impl MetadataContainer {
    pub fn new(ref_id: String, type_info: MetadataTypeInfo) -> Self {
        Self { ref_id, type_info }
    }

    /// Get the type name for display and validation purposes
    pub fn type_name(&self) -> &str {
        match &self.type_info {
            MetadataTypeInfo::Simple(name) => name,
            MetadataTypeInfo::Record(ty_record_data) => &ty_record_data.name,
        }
    }

    /// Deserialize a JSON string to a Starlark value based on type info
    fn deserialize_value<'v>(
        &self,
        json_str: &str,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let input_value: InputValue = serde_json::from_str(json_str)
            .map_err(|e| anyhow!("Failed to deserialize JSON: {}", e))?;

        match &self.type_info {
            MetadataTypeInfo::Simple(_) => input_value.to_value(eval, None),
            MetadataTypeInfo::Record(ty_record_data) => {
                let type_value = eval.module().get(&ty_record_data.name);
                input_value.to_value(eval, type_value)
            }
        }
    }

    /// Validate that a value matches this container's type
    fn validate_type(&self, value: Value) -> anyhow::Result<()> {
        let value_type = value.get_type();
        let expected_type_name = self.type_name();

        let type_matches = match &self.type_info {
            MetadataTypeInfo::Simple(expected_type) => {
                validate_simple_type(expected_type, value_type)
            }
            MetadataTypeInfo::Record(_) => value_type == "record",
        };

        if !type_matches && expected_type_name != "typing.Any" {
            return Err(anyhow!(
                "Type mismatch: expected {}, got {}",
                expected_type_name,
                value_type
            ));
        }

        Ok(())
    }
}

starlark_simple_value!(MetadataContainer);

impl std::fmt::Display for MetadataContainer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MetadataContainer({}:{})", self.type_name(), self.ref_id)
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
        // MetadataContainer can be copied directly since it only contains
        // a reference ID string and type information, both of which are cloneable
        Ok(dst.alloc(self.clone()))
    }
}

/// Validate that a simple type matches the expected type
fn validate_simple_type(expected_type: &str, value_type: &str) -> bool {
    match expected_type {
        "str" => value_type == "string",
        "int" => value_type == "int",
        "float" => value_type == "float",
        "bool" => value_type == "bool",
        "list" => value_type == "list",
        "dict" => value_type == "dict",
        other => {
            if other.starts_with("enum(") && value_type == "enum" {
                true
            } else {
                value_type == other
            }
        }
    }
}

/// Extract type info from a Starlark value
fn extract_type_info(type_value: Value) -> anyhow::Result<MetadataTypeInfo> {
    if let Some(rt) = type_value.downcast_ref::<RecordType>() {
        let ty_record_data = rt
            .ty_record_data()
            .cloned()
            .ok_or_else(|| anyhow!("Record type must be assigned to a variable"))?;
        Ok(MetadataTypeInfo::Record(ty_record_data))
    } else if let Some(frt) = type_value.downcast_ref::<FrozenRecordType>() {
        let ty_record_data = frt
            .ty_record_data()
            .cloned()
            .ok_or_else(|| anyhow!("Record type must be assigned to a variable"))?;
        Ok(MetadataTypeInfo::Record(ty_record_data))
    } else {
        let type_name = if type_value.get_type() == "function" {
            type_value.to_string()
        } else {
            type_value.get_type().to_string()
        };
        Ok(MetadataTypeInfo::Simple(type_name))
    }
}

#[starlark_module]
pub fn metadata_globals(builder: &mut GlobalsBuilder) {
    /// Create a new typed metadata container
    fn metadata<'v>(
        type_value: Value<'v>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let ref_id = with_metadata_store(eval, |store| store.allocate_ref())?;
        let type_info = extract_type_info(type_value)?;
        let container = MetadataContainer::new(ref_id, type_info);
        Ok(eval.heap().alloc(container))
    }

    /// Push a value to a metadata container
    fn push_metadata<'v>(
        container: Value<'v>,
        value: Value<'v>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let container = container
            .downcast_ref::<MetadataContainer>()
            .ok_or_else(|| anyhow!("Expected MetadataContainer"))?;

        container.validate_type(value)?;

        let input_value = InputValue::from_value(value);
        let json_value = serde_json::to_string(&input_value)
            .map_err(|e| anyhow!("Failed to serialize value to JSON: {}", e))?;

        with_metadata_store_mut(eval, |store| store.push(&container.ref_id, json_value))??;

        Ok(Value::new_none())
    }

    /// Get all values from a metadata container as a list of automatically deserialized Starlark values
    fn list_metadata<'v>(
        container: Value<'v>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let container = container
            .downcast_ref::<MetadataContainer>()
            .ok_or_else(|| anyhow!("Expected MetadataContainer"))?;

        let json_values = with_metadata_store(eval, |store| {
            store
                .get_all(&container.ref_id)
                .cloned()
                .unwrap_or_default()
        })?;

        let mut list_values = Vec::new();
        for json_str in &json_values {
            let starlark_value = container.deserialize_value(json_str, eval)?;
            list_values.push(starlark_value);
        }

        Ok(eval.heap().alloc(AllocList(list_values)).to_value())
    }

    /// Get the most recent value from a metadata container, automatically deserialized
    fn get_metadata<'v>(
        container: Value<'v>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let container = container
            .downcast_ref::<MetadataContainer>()
            .ok_or_else(|| anyhow!("Expected MetadataContainer"))?;

        let json_opt =
            with_metadata_store(eval, |store| store.get_latest(&container.ref_id).cloned())?;

        match json_opt {
            Some(json_value) => container.deserialize_value(&json_value, eval),
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

        store.push(&ref_id, "\"hello\"".to_string()).unwrap();
        store.push(&ref_id, "\"world\"".to_string()).unwrap();

        assert_eq!(store.get_latest(&ref_id), Some(&"\"world\"".to_string()));
        assert_eq!(store.get_all(&ref_id).unwrap().len(), 2);
    }

    #[test]
    fn test_metadata_container_creation() {
        let container = MetadataContainer::new(
            "test_ref".to_string(),
            MetadataTypeInfo::Simple("str".to_string()),
        );
        assert_eq!(container.ref_id, "test_ref");
        assert_eq!(container.type_name(), "str");
    }

    #[test]
    fn test_metadata_type_info() {
        // Test simple type info
        let simple_info = MetadataTypeInfo::Simple("str".to_string());
        match simple_info {
            MetadataTypeInfo::Simple(name) => assert_eq!(name, "str"),
            _ => panic!("Expected Simple type info"),
        }

        // Test container creation and type_name method
        let container = MetadataContainer::new(
            "test_ref".to_string(),
            MetadataTypeInfo::Simple("int".to_string()),
        );

        assert_eq!(container.ref_id, "test_ref");
        assert_eq!(container.type_name(), "int");

        match &container.type_info {
            MetadataTypeInfo::Simple(name) => assert_eq!(name, "int"),
            _ => panic!("Container should have Simple type info"),
        }
    }
}
