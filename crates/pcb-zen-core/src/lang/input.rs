//! Heap-agnostic storage of module input values.
//!
//! A parent module passes data to a child via a map of plain Rust values so we
//! don't hold any references into the parent's Starlark heap.  The child can
//! later materialise those inputs on *its* heap (and in the appropriate type)
//! on demand.

#![allow(clippy::needless_lifetimes)]

use std::fmt;

use allocative::Allocative;
use anyhow::anyhow;
use serde::{Deserialize, Serialize};
use starlark::collections::SmallMap;
use starlark::eval::Evaluator;
use starlark::values::dict::DictRef;
use starlark::values::float::StarlarkFloat;
use starlark::values::list::ListRef;
use starlark::values::record::{FrozenRecord, FrozenRecordType, Record, RecordType};
use starlark::values::{Trace, Value, ValueLike};

use crate::lang::interface::{FrozenInterfaceValue, InterfaceValue};
use crate::lang::net::NetValue;
use crate::{FrozenNetValue, NetId};

use super::interface::{FrozenInterfaceFactory, InterfaceFactory};

/// A heap-agnostic representation of a Starlark value that can be recreated on
/// any heap later.
#[derive(Debug, Clone, Trace, Allocative, Serialize, Deserialize)]
#[repr(C)]
pub enum InputValue {
    None,
    Bool(bool),
    Int(i32),
    String(String),
    Float(f64),
    List(Vec<InputValue>),
    Dict(SmallMap<String, InputValue>),

    // Stores the variant label of an EnumType / FrozenEnumType.
    Enum {
        variant: String,
    },

    // Stores the field map for a RecordType / FrozenRecordType.
    Record {
        fields: SmallMap<String, InputValue>,
    },

    /// Represents a Net value (name + unique id)
    Net {
        id: NetId,
        name: String,
        properties: SmallMap<String, InputValue>,
    },

    /// Represents an Interface instance (recursively stores its field values).
    Interface {
        fields: SmallMap<String, InputValue>,
    },

    /// Fallback for unsupported / complex values.
    #[allocative(skip)]
    Unsupported(String),
}

impl fmt::Display for InputValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InputValue::None => write!(f, "None"),
            InputValue::Bool(b) => write!(f, "{b}"),
            InputValue::Int(i) => write!(f, "{i}"),
            InputValue::String(s) => write!(f, "\"{s}\""),
            InputValue::Float(x) => write!(f, "{x}"),
            InputValue::List(l) => write!(f, "List(len={})", l.len()),
            InputValue::Dict(m) => write!(f, "Dict(len={})", m.len()),
            InputValue::Enum { variant } => write!(f, "Enum({variant})"),
            InputValue::Record { fields } => write!(f, "Record(len={})", fields.len()),
            InputValue::Net {
                name, properties, ..
            } => {
                if properties.is_empty() {
                    write!(f, "Net({name})")
                } else {
                    write!(f, "Net({name}, {} properties)", properties.len())
                }
            }
            InputValue::Interface { .. } => write!(f, "Interface(...)"),
            InputValue::Unsupported(s) => write!(f, "Unsupported({s})"),
        }
    }
}

impl InputValue {
    /// Render the value onto `heap`, optionally guided by an `expected_typ`
    /// provided by the caller (e.g. an `EnumType` or `RecordType`).
    pub fn to_value<'v>(
        &self,
        eval: &mut Evaluator<'v, '_, '_>,
        expected_typ: Option<Value<'v>>,
    ) -> anyhow::Result<Value<'v>> {
        let heap = eval.heap();
        match self {
            InputValue::None => Ok(Value::new_none().to_value()),
            InputValue::Bool(b) => Ok(Value::new_bool(*b).to_value()),
            InputValue::Int(i) => Ok(heap.alloc(*i)),
            InputValue::String(s) => Ok(heap.alloc_str(s).to_value()),
            InputValue::Float(x) => Ok(heap
                .alloc(starlark::values::float::StarlarkFloat(*x))
                .to_value()),
            InputValue::List(list) => {
                let mut out = Vec::with_capacity(list.len());
                for item in list {
                    out.push(item.to_value(eval, None)?);
                }
                Ok(heap.alloc(out))
            }
            InputValue::Dict(map) => {
                use starlark::values::dict::AllocDict;
                let mut out = Vec::with_capacity(map.len());
                for (k, v) in map.iter() {
                    out.push((heap.alloc_str(k), v.to_value(eval, None)?));
                }
                Ok(heap.alloc(AllocDict(out)))
            }
            InputValue::Enum { variant } => {
                let typ_val =
                    expected_typ.ok_or_else(|| anyhow!("enum input requires expected_typ"))?;

                eval.eval_function(typ_val, &[eval.heap().alloc_str(variant).to_value()], &[])
                    .map_err(|e| anyhow!(e.to_string()))
            }
            InputValue::Record { fields } => {
                let typ_val =
                    expected_typ.ok_or_else(|| anyhow!("record input requires expected_typ"))?;

                // Validate that expected typ is a RecordType.
                if typ_val.downcast_ref::<RecordType>().is_none()
                    && typ_val.downcast_ref::<FrozenRecordType>().is_none()
                {
                    return Err(anyhow!(
                        "expected a RecordType/FrozenRecordType for record input"
                    ));
                }

                // Build named argument slice.
                let mut named: Vec<(&str, Value<'v>)> = Vec::with_capacity(fields.len());
                for (k, v) in fields.iter() {
                    named.push((k.as_str(), v.to_value(eval, None)?));
                }

                eval.eval_function(typ_val, &[], &named)
                    .map_err(|e| anyhow!(e.to_string()))
            }
            InputValue::Net {
                id,
                name,
                properties,
            } => {
                use crate::lang::net::NetValue;
                // Convert properties from InputValue to Value
                let mut prop_map = SmallMap::new();
                for (k, v) in properties.iter() {
                    prop_map.insert(k.clone(), v.to_value(eval, None)?);
                }
                Ok(heap
                    .alloc(NetValue::new(
                        *id,
                        name.clone(),
                        prop_map,
                        Value::new_none(),
                    ))
                    .to_value())
            }
            InputValue::Interface { fields } => {
                // We rely on expected_typ (InterfaceFactory) to build a new instance.
                let typ_val =
                    expected_typ.ok_or_else(|| anyhow!("interface input requires expected_typ"))?;

                if typ_val.downcast_ref::<InterfaceFactory>().is_none()
                    && typ_val.downcast_ref::<FrozenInterfaceFactory>().is_none()
                {
                    return Err(anyhow!("Type mismatch: expected Net, received Interface"));
                }

                // Convert each stored field recursively, passing down the expected type if we can
                // determine it from the interface definition.
                let mut named: Vec<(&str, Value<'v>)> = Vec::with_capacity(fields.len());
                for (k, v) in fields.iter() {
                    // Look up the expected type for this field (if available) on the interface
                    // factory. We need to handle both the regular and the frozen variants.
                    let expected_field_typ: Option<Value<'v>> =
                        if let Some(fac) = typ_val.downcast_ref::<InterfaceFactory<'v>>() {
                            fac.field(k.as_str()).map(|val| val.to_value())
                        } else if let Some(fac) = typ_val.downcast_ref::<FrozenInterfaceFactory>() {
                            fac.field(k.as_str()).map(|val| val.to_value())
                        } else {
                            None
                        };

                    named.push((k.as_str(), v.to_value(eval, expected_field_typ)?));
                }

                eval.eval_function(typ_val, &[], &named)
                    .map_err(|e| anyhow!(e.to_string()))
            }
            InputValue::Unsupported(s) => Err(anyhow!("unsupported input value type: {s}")),
        }
    }

    pub fn from_value<'v>(value: Value<'v>) -> InputValue {
        let value = value.to_value();

        if value.is_none() {
            return InputValue::None;
        }

        if let Some(b) = value.unpack_bool() {
            return InputValue::Bool(b);
        }

        if let Some(i) = value.unpack_i32() {
            return InputValue::Int(i);
        }

        if let Some(s) = value.unpack_str() {
            return InputValue::String(s.to_owned());
        }

        if let Some(f) = value.downcast_ref::<StarlarkFloat>() {
            return InputValue::Float(f.0);
        }

        if let Some(list) = ListRef::from_value(value) {
            let mut out = Vec::new();
            for item in list.iter() {
                out.push(InputValue::from_value(item));
            }
            return InputValue::List(out);
        }

        if let Some(dict) = DictRef::from_value(value) {
            let mut out = SmallMap::new();
            for (k, v) in dict.iter() {
                let key_str = k
                    .unpack_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| k.to_string());
                out.insert(key_str, InputValue::from_value(v));
            }
            return InputValue::Dict(out);
        }

        // Parse enum from string: `EnumType("<variant>")`.
        if value.get_type() == "enum" {
            let repr = value.to_string();
            let variant = if let Some(first_quote) = repr.find('"') {
                if let Some(last_quote) = repr.rfind('"') {
                    if first_quote < last_quote {
                        repr[first_quote + 1..last_quote].to_owned()
                    } else {
                        repr
                    }
                } else {
                    repr
                }
            } else {
                repr
            };

            return InputValue::Enum { variant };
        }

        if let Some(record) = value.downcast_ref::<Record>() {
            return InputValue::Record {
                fields: record
                    .iter()
                    .map(|(k, v)| (k.to_string(), InputValue::from_value(v.to_value())))
                    .collect(),
            };
        }

        if let Some(record) = value.downcast_ref::<FrozenRecord>() {
            return InputValue::Record {
                fields: record
                    .iter()
                    .map(|(k, v)| (k.to_string(), InputValue::from_value(v.to_value())))
                    .collect(),
            };
        }

        if let Some(net) = value.downcast_ref::<NetValue>() {
            let mut prop_map = SmallMap::new();
            for (k, v) in net.properties().iter() {
                prop_map.insert(k.clone(), InputValue::from_value(v.to_value()));
            }
            return InputValue::Net {
                id: net.id(),
                name: net.name().to_owned(),
                properties: prop_map,
            };
        }

        if let Some(net) = value.downcast_ref::<FrozenNetValue>() {
            let mut prop_map = SmallMap::new();
            for (k, v) in net.properties().iter() {
                prop_map.insert(k.clone(), InputValue::from_value(v.to_value()));
            }
            return InputValue::Net {
                id: net.id(),
                name: net.name().to_owned(),
                properties: prop_map,
            };
        }

        if let Some(iface) = value.downcast_ref::<InterfaceValue>() {
            let mut field_map = SmallMap::new();
            for (k, v) in iface.fields().iter() {
                field_map.insert(k.clone(), InputValue::from_value(*v));
            }
            return InputValue::Interface { fields: field_map };
        }

        if let Some(iface) = value.downcast_ref::<FrozenInterfaceValue>() {
            let mut field_map = SmallMap::new();
            for (k, v) in iface.fields().iter() {
                field_map.insert(k.clone(), InputValue::from_value(v.to_value()));
            }
            return InputValue::Interface { fields: field_map };
        }

        InputValue::Unsupported(value.get_type().to_owned())
    }
}

/// A tiny wrapper that stores the map from input-name to `InputValue`.
#[derive(Debug, Clone, Trace, Allocative, Default, Serialize)]
#[repr(C)]
pub struct InputMap {
    inner: SmallMap<String, InputValue>,
}

impl InputMap {
    pub fn new() -> Self {
        Self {
            inner: SmallMap::new(),
        }
    }

    pub fn insert(&mut self, name: String, value: InputValue) {
        self.inner.insert(name, value);
    }

    pub fn get(&self, name: &str) -> Option<&InputValue> {
        self.inner.get(name)
    }
}
