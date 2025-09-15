#![allow(clippy::needless_lifetimes)]

use allocative::Allocative;
use starlark::starlark_complex_value;
use starlark::values::{Coerce, FreezeResult, Heap, ValueLike};
use starlark::{
    any::ProvidesStaticType,
    collections::SmallMap,
    eval::{Arguments, Evaluator},
    starlark_simple_value,
    values::{starlark_value, Freeze, NoSerialize, StarlarkValue, Trace, Value},
};
use std::cell::RefCell;

use super::eval::{copy_value, DeepCopyToHeap};
use super::validation::validate_identifier_name;
use crate::lang::context::ContextValue;

pub type NetId = u64;

// Deterministic per‐thread counter for net IDs. Using a thread‐local ensures that
// concurrent tests (which run in separate threads) do not interfere with one
// another, while still providing repeatable identifiers within a single
// evaluation.
std::thread_local! {
    static NEXT_NET_ID: RefCell<u64> = const { RefCell::new(1) };
}

/// Generate a new unique net ID using the thread-local counter.
pub fn generate_net_id() -> NetId {
    NEXT_NET_ID.with(|counter| {
        let mut c = counter.borrow_mut();
        let id = *c;
        *c += 1;
        id
    })
}

/// Reset the net ID counter to 1. This is only intended for use in tests
/// to ensure reproducible net IDs across test runs.
#[cfg(test)]
pub fn reset_net_id_counter() {
    NEXT_NET_ID.with(|counter| {
        *counter.borrow_mut() = 1;
    });
}

#[derive(
    Clone, PartialEq, Eq, ProvidesStaticType, NoSerialize, Allocative, Trace, Freeze, Coerce,
)]
#[repr(C)]
pub struct NetValueGen<V> {
    id: NetId,
    name: String,
    pub original_name: Option<String>, // The name originally requested before deduplication
    properties: SmallMap<String, V>,
    symbol: V, // The Symbol value if one was provided (None if not)
}

impl<V: std::fmt::Debug> std::fmt::Debug for NetValueGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = f.debug_struct("Net");
        debug.field("name", &self.name);
        debug.field("id", &"<ID>"); // Normalize ID for stable snapshots

        // Sort properties for deterministic output
        if !self.properties.is_empty() {
            let mut props: Vec<_> = self.properties.iter().collect();
            props.sort_by_key(|(k, _)| k.as_str());
            let props_map: std::collections::BTreeMap<_, _> =
                props.into_iter().map(|(k, v)| (k.as_str(), v)).collect();
            debug.field("properties", &props_map);
        }

        // Show symbol field
        debug.field("symbol", &self.symbol);

        debug.finish()
    }
}

starlark_complex_value!(pub NetValue);

#[starlark_value(type = "Net")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for NetValueGen<V>
where
    Self: ProvidesStaticType<'v>,
{
    fn provide(&'v self, demand: &mut starlark::values::Demand<'_, 'v>) {
        demand.provide_value::<&dyn DeepCopyToHeap>(self);
    }

    fn get_attr(&self, attribute: &str, heap: &'v Heap) -> Option<Value<'v>> {
        match attribute {
            "name" => Some(heap.alloc(self.name.clone())),
            "original_name" => self.original_name.as_ref().map(|n| heap.alloc(n.clone())),
            _ => None,
        }
    }

    fn has_attr(&self, attribute: &str, _heap: &'v Heap) -> bool {
        matches!(attribute, "name" | "original_name")
    }

    fn dir_attr(&self) -> Vec<String> {
        vec!["name".to_string(), "original_name".to_string()]
    }
}

impl<'v, V: ValueLike<'v>> DeepCopyToHeap for NetValueGen<V> {
    fn deep_copy_to<'dst>(&self, dst: &'dst Heap) -> anyhow::Result<Value<'dst>> {
        let properties = self
            .properties
            .iter()
            .map(|(k, v)| {
                let copied_value = copy_value(v.to_value(), dst)?;
                Ok((k.clone(), copied_value))
            })
            .collect::<Result<SmallMap<String, Value<'dst>>, anyhow::Error>>()?;

        Ok(dst.alloc(NetValue {
            id: self.id,
            name: self.name.clone(),
            original_name: self.original_name.clone(),
            properties,
            symbol: copy_value(self.symbol.to_value(), dst)?,
        }))
    }
}

impl<'v, V: ValueLike<'v>> std::fmt::Display for NetValueGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl<'v, V: ValueLike<'v>> NetValueGen<V> {
    pub fn new(id: NetId, name: String, properties: SmallMap<String, V>, symbol: V) -> Self {
        Self {
            id,
            name,
            original_name: None,
            properties,
            symbol,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Return the original requested name, falling back to the final name if no original was stored
    pub fn original_name(&self) -> &str {
        self.original_name.as_deref().unwrap_or(&self.name)
    }

    /// Return the globally‐unique identifier of this net instance.
    pub fn id(&self) -> NetId {
        self.id
    }

    /// Return the properties map of this net instance.
    pub fn properties(&self) -> &SmallMap<String, V> {
        &self.properties
    }

    /// Return the symbol associated with this net (if any).
    pub fn symbol(&self) -> &V {
        &self.symbol
    }
}

#[derive(Debug, ProvidesStaticType, NoSerialize, Allocative)]
pub struct NetType;
starlark_simple_value!(NetType);

impl std::fmt::Display for NetType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Net")
    }
}

#[starlark_value(type = "NetType")]
impl<'v> StarlarkValue<'v> for NetType
where
    Self: ProvidesStaticType<'v>,
{
    fn provide(&'v self, demand: &mut starlark::values::Demand<'_, 'v>) {
        demand.provide_value::<&dyn DeepCopyToHeap>(self);
    }

    fn invoke(
        &self,
        _me: Value<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        let heap = eval.heap();
        // Parse positional args for name
        let positions_iter = args.positions(heap)?;
        let positions: Vec<Value> = positions_iter.collect();
        if positions.len() > 1 {
            return Err(starlark::Error::new_other(anyhow::anyhow!(
                "Too many positional args to Net()"
            )));
        }
        let name_pos: Option<String> = if let Some(v) = positions.first() {
            let name = v
                .unpack_str()
                .ok_or_else(|| {
                    starlark::Error::new_other(anyhow::anyhow!("Expected string for net name"))
                })?
                .to_owned();

            // Validate the positional net name
            validate_identifier_name(&name, "Net name")?;

            Some(name)
        } else {
            None
        };

        // Check if "name" was provided as a kwarg
        let mut name_kwarg: Option<String> = None;
        let names_map = args.names_map()?;

        let mut symbol_val: Option<Value<'v>> = None;

        for (key, value) in names_map.iter() {
            match key.as_str() {
                "name" => {
                    // Special handling for "name" kwarg
                    let name = value
                        .unpack_str()
                        .ok_or_else(|| {
                            starlark::Error::new_other(anyhow::anyhow!(
                                "Expected string for net name"
                            ))
                        })?
                        .to_owned();

                    // Validate the kwarg net name
                    validate_identifier_name(&name, "Net name")?;

                    name_kwarg = Some(name);
                }
                "symbol" => {
                    // Check that the value is a Symbol
                    if value.get_type() != "Symbol" {
                        return Err(starlark::Error::new_other(anyhow::anyhow!(
                            "Expected Symbol for 'symbol' parameter, got {}",
                            value.get_type()
                        )));
                    }
                    symbol_val = Some(*value);
                }
                _ => {
                    // No other kwargs accepted
                    return Err(starlark::Error::new_other(anyhow::anyhow!(
                        "Net() does not accept keyword argument '{}'. Only 'name' and 'symbol' are allowed.",
                        key.as_str()
                    )));
                }
            }
        }

        // Generate a deterministic, per-thread unique ID for this net. A thread-local
        // counter guarantees deterministic results within a single evaluation and
        // avoids cross-test interference when Rust tests execute in parallel.
        let net_id = generate_net_id();

        // Use positional name if provided, otherwise use kwarg name (may be empty).
        let original_name = name_pos.or(name_kwarg);
        let net_name = original_name.clone().unwrap_or_default();

        // Initialize with empty properties map
        let mut properties = SmallMap::new();

        // Register this net with the current module context so its local name is
        // recorded at creation time. This ensures explicit names like "EN"
        // are associated with the module where the literal appears.
        let final_name = if let Some(ctx) = eval
            .module()
            .extra_value()
            .and_then(|e| e.downcast_ref::<ContextValue>())
        {
            match ctx.register_net(net_id, &net_name) {
                Ok(n) => n,
                Err(e) => return Err(starlark::Error::new_other(anyhow::anyhow!(e.to_string()))),
            }
        } else {
            net_name.clone()
        };

        // If a symbol was provided, extract its properties and add them to the net properties
        if let Some(symbol) = symbol_val {
            if let Some(symbol_value) = symbol.downcast_ref::<crate::lang::symbol::SymbolValue>() {
                // Add symbol_name
                if let Some(name) = symbol_value.name() {
                    properties.insert("symbol_name".to_string(), heap.alloc_str(name).to_value());
                }

                // Add symbol_path if available
                if let Some(path) = symbol_value.source_path() {
                    properties.insert("symbol_path".to_string(), heap.alloc_str(path).to_value());
                }

                // Add the raw s-expression if available
                if let Some(raw_sexp) = symbol_value.raw_sexp() {
                    properties.insert(
                        "__symbol_value".to_string(),
                        heap.alloc_str(raw_sexp).to_value(),
                    );
                }
            }
        }

        Ok(heap.alloc(NetValue {
            id: net_id,
            name: final_name,
            original_name,
            properties,
            symbol: symbol_val.unwrap_or_else(Value::new_none),
        }))
    }

    fn eval_type(&self) -> Option<starlark::typing::Ty> {
        Some(<NetValue as StarlarkValue>::get_type_starlark_repr())
    }
}

impl DeepCopyToHeap for NetType {
    fn deep_copy_to<'dst>(&self, dst: &'dst Heap) -> anyhow::Result<Value<'dst>> {
        Ok(dst.alloc(NetType))
    }
}
