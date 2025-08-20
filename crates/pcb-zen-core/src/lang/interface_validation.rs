/// Type validation utilities for interface field compatibility checking.
/// This module provides reusable functionality for validating that values
/// passed to interface fields are compatible with the expected field types.
use starlark::values::{Value, ValueLike};

use crate::lang::interface::{get_promotion_key, FrozenInterfaceFactory, InterfaceFactory};

/// Categorization of Starlark values for interface field type checking.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum FieldKind {
    /// Concrete Net instance
    Net,
    /// The NetType sentinel (Net type without instance)
    NetType,
    /// Interface factory (can create interface instances)
    InterfaceFactory,
    /// Interface instance (created from a factory)
    InterfaceValue,
}

impl FieldKind {
    /// Categorize a Starlark value into a field kind.
    pub fn of(v: Value) -> FieldKind {
        match v.get_type() {
            "Net" => FieldKind::Net,
            "NetType" => FieldKind::NetType,
            "InterfaceValue" => FieldKind::InterfaceValue,
            // factories are identified via down-cast:
            _ if v.downcast_ref::<InterfaceFactory>().is_some()
                || v.downcast_ref::<FrozenInterfaceFactory>().is_some() =>
            {
                FieldKind::InterfaceFactory
            }
            _ => FieldKind::Net, // unreachable for legal interface fields
        }
    }

    /// Check if a value of `self` type satisfies a field whose specification is `spec`.
    pub fn matches(self, spec: FieldKind) -> bool {
        use FieldKind::*;
        matches!(
            (spec, self),
            (NetType | Net, Net) | (InterfaceFactory | InterfaceValue, InterfaceValue)
        )
    }
}

/// Get a user-friendly type name for error messages.
///
/// For interface instances and factories, this attempts to extract the actual
/// interface type name (e.g., "Ground", "Uart") rather than showing generic
/// type names like "InterfaceValue".
pub fn get_type_name_for_error(value: Value) -> String {
    match value.get_type() {
        "Net" => "Net".to_string(),
        "NetType" => "Net".to_string(),
        "InterfaceValue" => {
            // Extract the actual interface type name
            if let Ok(type_name) = get_promotion_key(value) {
                type_name
            } else {
                "Interface".to_string()
            }
        }
        _ if value.downcast_ref::<InterfaceFactory>().is_some()
            || value.downcast_ref::<FrozenInterfaceFactory>().is_some() =>
        {
            // Extract factory type name
            if let Ok(type_name) = get_promotion_key(value) {
                type_name
            } else {
                "Interface".to_string()
            }
        }
        other => other.to_string(),
    }
}

/// Check that a provided value is compatible with an interface field specification.
///
/// This is the main validation function used during interface instantiation
/// to ensure that field overrides match the expected types.
///
/// # Arguments
/// * `spec_val` - The field specification from the interface definition
/// * `provided_val` - The value provided by the caller during instantiation  
/// * `field_name` - Name of the field (for error messages)
///
/// # Returns
/// * `Ok(())` if the value is compatible
/// * `Err(...)` with a user-friendly error message if incompatible
pub fn ensure_field_compat(
    spec_val: Value,
    provided_val: Value,
    field_name: &str,
) -> anyhow::Result<()> {
    let spec_kind = FieldKind::of(spec_val);
    let provided_kind = FieldKind::of(provided_val);

    if provided_kind.matches(spec_kind) {
        Ok(())
    } else {
        let expected_name = get_type_name_for_error(spec_val);
        let provided_name = get_type_name_for_error(provided_val);
        Err(anyhow::anyhow!(
            "Interface field '{}' expects {}, got {}",
            field_name,
            expected_name,
            provided_name
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_field_kind_matches() {
        use FieldKind::*;

        // Net compatibility
        assert!(Net.matches(Net));
        assert!(Net.matches(NetType));
        assert!(!Net.matches(InterfaceValue));
        assert!(!Net.matches(InterfaceFactory));

        // Interface compatibility
        assert!(InterfaceValue.matches(InterfaceValue));
        assert!(InterfaceValue.matches(InterfaceFactory));
        assert!(!InterfaceValue.matches(Net));
        assert!(!InterfaceValue.matches(NetType));
    }
}
