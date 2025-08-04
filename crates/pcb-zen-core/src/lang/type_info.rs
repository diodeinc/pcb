use crate::lang::interface::{FrozenInterfaceFactory, InterfaceFactory};
use crate::lang::net::{NetType, NetValue};
use crate::{FrozenNetValue, InputValue};
use serde::{Deserialize, Serialize};
use starlark::values::enumeration::{EnumType, FrozenEnumType};
use starlark::values::record::{FrozenRecordType, RecordType};
use starlark::values::typing::TypeType;
use starlark::values::{UnpackValue, Value, ValueLike};
use std::collections::HashMap;

/// Structured representation of Starlark types for introspection
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TypeInfo {
    /// String type
    String,
    /// Integer type
    Int,
    /// Float type
    Float,
    /// Boolean type
    Bool,
    /// List type with element type
    List { element: Box<TypeInfo> },
    /// Dict type with key and value types
    Dict {
        key: Box<TypeInfo>,
        value: Box<TypeInfo>,
    },
    /// Net type for electrical connections
    Net,
    /// Enum type with possible variants
    Enum { name: String, variants: Vec<String> },
    /// Record type with named fields
    Record {
        name: String,
        fields: HashMap<String, TypeInfo>,
    },
    /// Interface type - a special record that contains nets and sub-interfaces
    Interface {
        name: String,
        /// Map of pin/signal names to their types (usually Net or sub-interfaces)
        pins: Vec<(String, TypeInfo)>,
    },
    /// Unknown or complex type
    Unknown { type_name: String },
}

impl TypeInfo {
    /// Check if this type represents an IO type (Net or Interface)
    pub fn is_io_type(&self) -> bool {
        matches!(self, TypeInfo::Net | TypeInfo::Interface { .. })
    }

    /// Check if this type is an enum
    pub fn is_enum(&self) -> bool {
        matches!(self, TypeInfo::Enum { .. })
    }

    /// Extract TypeInfo from a Starlark value representing a type
    pub fn from_value<'v>(value: Value<'v>) -> Self {
        // Get the type name for identification
        let type_name = value.get_type();

        if let Some(enum_type) = value.downcast_ref::<EnumType>() {
            let variants = enum_type
                .elements()
                .keys()
                .map(|v| v.unpack_str().unwrap_or_default().to_string())
                .collect();
            return TypeInfo::Enum {
                name: type_name.to_string(),
                variants,
            };
        }

        if let Some(enum_type) = value.downcast_ref::<FrozenEnumType>() {
            let variants = enum_type
                .elements()
                .keys()
                .map(|v| {
                    v.downcast_frozen_str()
                        .map(|s| s.to_string())
                        .unwrap_or_default()
                })
                .collect();
            return TypeInfo::Enum {
                name: type_name.to_string(),
                variants,
            };
        }

        // Check for record types
        if value.downcast_ref::<RecordType>().is_some()
            || value.downcast_ref::<FrozenRecordType>().is_some()
        {
            let fields = HashMap::new();
            // TODO: Extract field information when starlark provides an API for it
            return TypeInfo::Record {
                name: type_name.to_string(),
                fields,
            };
        }

        // Check for Net type by type name
        if value.downcast_ref::<NetType>().is_some()
            || value.downcast_ref::<NetValue>().is_some()
            || value.downcast_ref::<FrozenNetValue>().is_some()
        {
            return TypeInfo::Net;
        }

        // Check for Interface types by downcasting
        if let Some(iface) = value.downcast_ref::<InterfaceFactory>() {
            let mut pins = Vec::new();
            for (key, val) in iface.iter() {
                pins.push((key.to_string(), Self::from_value(val.to_value())));
            }

            return TypeInfo::Interface {
                name: value.to_string(),
                pins,
            };
        }

        if let Some(iface) = value.downcast_ref::<FrozenInterfaceFactory>() {
            let mut pins = Vec::new();
            for (key, val) in iface.iter() {
                pins.push((key.to_string(), Self::from_value(val.to_value())));
            }

            return TypeInfo::Interface {
                name: value.to_string(),
                pins,
            };
        }

        // Check if it's a TypeType (like str, int, float constructors)
        if TypeType::unpack_value_opt(value).is_some() {
            // Use the string representation to determine the type
            let type_str = value.to_string();
            match type_str.as_str() {
                "str" => return TypeInfo::String,
                "int" => return TypeInfo::Int,
                "float" => return TypeInfo::Float,
                "bool" => return TypeInfo::Bool,
                "list" => {
                    return TypeInfo::List {
                        element: Box::new(TypeInfo::Unknown {
                            type_name: "any".to_string(),
                        }),
                    };
                }
                "dict" => {
                    return TypeInfo::Dict {
                        key: Box::new(TypeInfo::String),
                        value: Box::new(TypeInfo::Unknown {
                            type_name: "any".to_string(),
                        }),
                    };
                }
                _ => {
                    // Unknown type constructor
                    return TypeInfo::Unknown {
                        type_name: type_str,
                    };
                }
            }
        }

        // Check for built-in types by examining the type
        match type_name {
            "type" => {
                // This is a type constructor like str, int, etc.
                let type_str = value.to_string();
                match type_str.as_str() {
                    "str" => TypeInfo::String,
                    "int" => TypeInfo::Int,
                    "float" => TypeInfo::Float,
                    "bool" => TypeInfo::Bool,
                    "list" => TypeInfo::List {
                        element: Box::new(TypeInfo::Unknown {
                            type_name: "any".to_string(),
                        }),
                    },
                    "dict" => TypeInfo::Dict {
                        key: Box::new(TypeInfo::String),
                        value: Box::new(TypeInfo::Unknown {
                            type_name: "any".to_string(),
                        }),
                    },
                    _ => TypeInfo::Unknown {
                        type_name: type_str,
                    },
                }
            }
            _ => {
                // For any other type, return Unknown with the type name
                TypeInfo::Unknown {
                    type_name: type_name.to_string(),
                }
            }
        }
    }
}

/// Parameter information with structured type
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParameterInfo {
    pub name: String,
    pub type_info: TypeInfo,
    pub required: bool,
    pub default_value: Option<InputValue>,
    pub help: Option<String>,
}

impl ParameterInfo {
    pub fn is_config(&self) -> bool {
        !self.type_info.is_io_type()
    }
}
