use std::{cell::RefCell, collections::HashMap, fmt, hash::Hash, sync::Mutex};

use allocative::Allocative;
use serde::{Deserialize, Serialize};
use starlark::{
    any::ProvidesStaticType,
    collections::SmallMap,
    environment::{Methods, MethodsBuilder, MethodsStatic},
    eval::{Arguments, Evaluator, ParametersSpec, ParametersSpecParam},
    starlark_complex_value, starlark_module, starlark_simple_value,
    typing::{ParamIsRequired, ParamSpec, Ty, TyCallable, TyStarlarkValue, TyUser, TyUserParams},
    util::ArcStr,
    values::{
        starlark_value, Heap,
        typing::{TypeInstanceId, TypeMatcher, TypeMatcherFactory},
        Coerce, Freeze, FreezeResult, FrozenValue, StarlarkValue, Trace, Value,
        ValueLike,
    },
};
use std::sync::OnceLock;

use super::context::ContextValue;
use super::validation::validate_identifier_name;

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
    Clone, PartialEq, Eq, ProvidesStaticType, Allocative, Trace, Freeze, Coerce, serde::Serialize, serde::Deserialize
)]
#[repr(C)]
#[serde(bound(serialize = "V: serde::Serialize", deserialize = "V: serde::Deserialize<'de>"))]
pub struct NetValueGen<V> {
    /// The globally unique identifier for this net
    pub(crate) net_id: NetId,
    /// The final name after deduplication
    pub(crate) name: String,
    /// The name originally requested before deduplication
    pub original_name: Option<String>,
    /// The type name (e.g., "Net", "Power", "Ground")
    pub(crate) type_name: String,
    /// Properties extracted from symbol (if provided)
    pub(crate) properties: SmallMap<String, V>,
    /// The Symbol value if one was provided
    pub(crate) symbol: V,
}

starlark_complex_value!(pub NetValue);

impl<V: std::fmt::Debug> std::fmt::Debug for NetValueGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Use "Net" as struct name for backwards compatibility with old snapshots
        let mut debug = f.debug_struct("Net");
        debug.field("name", &self.name);
        // Use "id" as field name for backwards compatibility
        debug.field("id", &"<ID>"); // Normalize ID for stable snapshots

        // Sort properties for deterministic output
        if !self.properties.is_empty() {
            let mut props: Vec<_> = self.properties.iter().collect();
            props.sort_by_key(|(k, _)| k.as_str());
            let props_map: std::collections::BTreeMap<_, _> =
                props.into_iter().map(|(k, v)| (k.as_str(), v)).collect();
            debug.field("properties", &props_map);
        }

        debug.field("symbol", &self.symbol);
        debug.finish()
    }
}

#[starlark_value(type = "Net")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for NetValueGen<V>
where
    Self: ProvidesStaticType<'v>,
{
    fn get_methods() -> Option<&'static Methods> {
        static RES: MethodsStatic = MethodsStatic::new();
        RES.methods(builtin_net_methods)
    }
}

impl<'v, V: ValueLike<'v>> std::fmt::Display for NetValueGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl<'v, V: ValueLike<'v>> NetValueGen<V> {
    /// Create a new NetValue
    pub fn new(net_id: NetId, name: String, properties: SmallMap<String, V>, symbol: V) -> Self {
        Self {
            net_id,
            name,
            original_name: None,
            type_name: "Net".to_string(),
            properties,
            symbol,
        }
    }

    /// Returns the instance name of this net
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the net ID (backwards compatible alias for net_id)
    pub fn id(&self) -> NetId {
        self.net_id
    }

    /// Returns the net ID
    pub fn net_id(&self) -> NetId {
        self.net_id
    }

    /// Returns the original requested name, falling back to the final name if no original was stored
    pub fn original_name(&self) -> &str {
        self.original_name.as_deref().unwrap_or(&self.name)
    }

    /// Returns the type name of this net
    pub fn net_type_name(&self) -> &str {
        &self.type_name
    }

    /// Return the properties map of this net instance.
    pub fn properties(&self) -> &SmallMap<String, V> {
        &self.properties
    }

    /// Return the symbol associated with this net (if any).
    pub fn symbol(&self) -> &V {
        &self.symbol
    }

    /// Return the original name as Option (None if auto-generated)
    pub fn original_name_opt(&self) -> Option<&str> {
        self.original_name.as_deref()
    }

    /// Create a new net with the same fields but a fresh net ID.
    /// This avoids deep copying - properties and symbols are shared via Value references.
    pub fn with_new_id(&self, heap: &'v Heap) -> Value<'v> {
        let properties: SmallMap<String, Value<'v>> = self
            .properties
            .iter()
            .map(|(k, v)| (k.clone(), v.to_value()))
            .collect();

        heap.alloc(NetValue {
            net_id: generate_net_id(),
            name: self.name.clone(),
            original_name: self.original_name.clone(),
            type_name: self.type_name.clone(),
            properties,
            symbol: self.symbol.to_value(),
        })
    }
}

#[starlark_module]
fn builtin_net_methods(methods: &mut MethodsBuilder) {
    #[starlark(attribute)]
    fn name<'v>(this: &NetValue<'v>) -> starlark::Result<String> {
        Ok(this.name().to_string())
    }

    #[starlark(attribute)]
    fn net_id<'v>(this: &NetValue<'v>) -> starlark::Result<i64> {
        Ok(this.net_id() as i64)
    }

    #[starlark(attribute)]
    fn original_name<'v>(this: &NetValue<'v>) -> starlark::Result<String> {
        Ok(this.original_name().to_string())
    }

    #[starlark(attribute)]
    fn r#type<'v>(this: &NetValue<'v>) -> starlark::Result<String> {
        Ok(this.net_type_name().to_string())
    }
}

/// A callable type constructor for creating typed nets
///
/// Created by `builtin.net(name)`, e.g.:
/// - `Net = builtin.net("Net")`
/// - `Power = builtin.net("Power")`
/// - `Ground = builtin.net("Ground")`
#[derive(Clone, Hash, Debug, PartialEq, ProvidesStaticType, Allocative, Serialize, Deserialize)]
pub struct NetType {
    /// The type name (e.g., "Net", "Power", "Ground")
    pub(crate) type_name: String,
}

impl Freeze for NetType {
    type Frozen = Self;
    fn freeze(self, _freezer: &starlark::values::Freezer) -> FreezeResult<Self::Frozen> {
        Ok(self)
    }
}

starlark_simple_value!(NetType);

impl fmt::Display for NetType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.instance_ty_name())
    }
}

impl NetType {
    /// Create a new NetType with the given type name
    pub fn new(type_name: String) -> Self {
        NetType { type_name }
    }

    /// Get the unique TypeInstanceId for this NetType
    /// Each type_name gets a unique ID that's cached globally
    fn type_instance_id(&self) -> TypeInstanceId {
        static CACHE: OnceLock<Mutex<HashMap<String, TypeInstanceId>>> = OnceLock::new();
        let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        *cache
            .lock()
            .unwrap()
            .entry(self.type_name.clone())
            .or_insert_with(TypeInstanceId::r#gen)
    }

    /// Returns the instance type name (e.g., "Net", "Power", "Ground")
    fn instance_ty_name(&self) -> String {
        self.type_name.to_string()
    }

    /// Returns the callable type name (e.g., "NetType", "PowerType", "GroundType")
    fn ty_name(&self) -> String {
        format!("{}Type", self.type_name)
    }

    /// Returns the parameter specification for this type's constructor
    fn param_spec(&self) -> ParamSpec {
        ParamSpec::new_parts(
            [(ParamIsRequired::No, Ty::any())], // positional-only - accepts string or NetValue
            [],                                 // pos_or_named
            None,                               // *args
            [
                (ArcStr::from("name"), ParamIsRequired::No, Ty::string()), // keyword-only
                (ArcStr::from("symbol"), ParamIsRequired::No, Ty::any()),  // keyword-only
            ],
            None, // **kwargs
        )
        .expect("ParamSpec creation should not fail")
    }

    /// Returns the runtime parameter specification for parsing arguments
    fn parameters_spec(&self) -> ParametersSpec<FrozenValue> {
        ParametersSpec::new_parts(
            self.instance_ty_name().as_str(),
            [("value", ParametersSpecParam::Optional)], // positional-only - accepts string or NetValue
            [],                                         // pos_or_named (args)
            false,
            [
                ("name", ParametersSpecParam::Optional), // named-only (kwargs)
                ("symbol", ParametersSpecParam::Optional), // named-only (kwargs)
            ],
            false,
        )
    }
}

#[starlark_value(type = "NetType")]
impl<'v> StarlarkValue<'v> for NetType
where
    Self: ProvidesStaticType<'v>,
{
    fn invoke(
        &self,
        _: Value<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        self.parameters_spec()
            .parser(args, eval, |param_parser, eval| {
                let heap = eval.heap();
                let type_name = self.instance_ty_name();

                // Parse arguments
                let value_param: Option<Value> = param_parser.next_opt()?;
                let name_keyword_str = param_parser
                    .next_opt()?
                    .map(|v: Value| {
                        v.unpack_str()
                            .ok_or_else(|| {
                                anyhow::anyhow!(
                                    "{}() 'name' must be string, got {}",
                                    type_name,
                                    v.get_type()
                                )
                            })
                            .map(|s| s.to_string())
                    })
                    .transpose()?;
                let symbol_value: Option<Value> = param_parser.next_opt()?;

                // Determine name, properties, and symbol from value_param
                let (original_name, mut properties, mut symbol) = match value_param {
                    Some(value) if value.unpack_str().is_some() => {
                        if name_keyword_str.is_some() {
                            return Err(anyhow::anyhow!(
                                "{}() cannot have both positional string and name keyword",
                                type_name
                            )
                            .into());
                        }
                        let name = value.unpack_str().unwrap().to_string();
                        validate_identifier_name(&name, "Net name")?;
                        (Some(name), SmallMap::new(), Value::new_none())
                    }
                    Some(value) => {
                        let source_net = NetValue::from_value(value).ok_or_else(|| {
                            anyhow::anyhow!(
                                "{}() expects string or {}, got {}",
                                type_name,
                                type_name,
                                value.get_type()
                            )
                        })?;

                        let name = match &name_keyword_str {
                            Some(n) => {
                                validate_identifier_name(n, "Net name")?;
                                n.clone()
                            }
                            None => source_net.name.clone(),
                        };
                        (
                            Some(name),
                            source_net.properties.clone(),
                            source_net.symbol.to_value(),
                        )
                    }
                    None => {
                        if let Some(n) = &name_keyword_str {
                            validate_identifier_name(n, "Net name")?;
                        }
                        (name_keyword_str, SmallMap::new(), Value::new_none())
                    }
                };

                // Process symbol parameter (overrides copied properties)
                if let Some(symbol_val) = symbol_value {
                    if symbol_val.get_type() != "Symbol" {
                        return Err(anyhow::anyhow!(
                            "'symbol' must be Symbol, got {}",
                            symbol_val.get_type()
                        )
                        .into());
                    }

                    if let Some(sym) = symbol_val.downcast_ref::<crate::lang::symbol::SymbolValue>()
                    {
                        if let Some(name) = sym.name() {
                            properties
                                .insert("symbol_name".to_string(), heap.alloc_str(name).to_value());
                        }
                        if let Some(path) = sym.source_path() {
                            properties
                                .insert("symbol_path".to_string(), heap.alloc_str(path).to_value());
                        }
                        if let Some(raw_sexp) = sym.raw_sexp() {
                            properties.insert(
                                "__symbol_value".to_string(),
                                heap.alloc_str(raw_sexp).to_value(),
                            );
                        }
                    }
                    symbol = symbol_val;
                }

                // Register net and create instance
                let net_id = generate_net_id();
                let net_name = original_name.clone().unwrap_or_default();
                let final_name = eval
                    .module()
                    .extra_value()
                    .and_then(|e| e.downcast_ref::<ContextValue>())
                    .map(|ctx| ctx.register_net(net_id, &net_name))
                    .transpose()
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?
                    .unwrap_or_else(|| net_name.clone());

                Ok(heap.alloc(NetValue {
                    net_id,
                    name: final_name,
                    original_name,
                    type_name: self.type_name.clone(),
                    properties,
                    symbol,
                }))
            })
    }

    fn eval_type(&self) -> Option<Ty> {
        let id = self.type_instance_id();
        Some(Ty::custom(
            TyUser::new(
                self.instance_ty_name(),
                TyStarlarkValue::new::<NetValue>(),
                id,
                TyUserParams {
                    matcher: Some(TypeMatcherFactory::new(NetTypeMatcher {
                        type_name: self.type_name.clone(),
                    })),
                    ..TyUserParams::default()
                },
            )
            .ok()?,
        ))
    }

    fn typechecker_ty(&self) -> Option<Ty> {
        Some(Ty::custom(
            TyUser::new(
                self.ty_name(),
                TyStarlarkValue::new::<Self>(),
                TypeInstanceId::r#gen(),
                TyUserParams {
                    callable: Some(TyCallable::new(self.param_spec(), self.eval_type()?)),
                    ..TyUserParams::default()
                },
            )
            .ok()?,
        ))
    }

    fn get_methods() -> Option<&'static Methods> {
        static RES: MethodsStatic = MethodsStatic::new();
        RES.methods(net_type_methods)
    }
}

#[starlark_module]
fn net_type_methods(methods: &mut MethodsBuilder) {
    #[starlark(attribute)]
    fn r#type(this: &NetType) -> starlark::Result<String> {
        Ok(this.ty_name())
    }

    #[starlark(attribute)]
    fn type_name(this: &NetType) -> starlark::Result<String> {
        Ok(this.type_name.to_string())
    }
}

/// Runtime type matcher for typed nets
///
/// Validates that a NetValue instance has the expected type_name
#[derive(Hash, Debug, PartialEq, Clone, Allocative)]
struct NetTypeMatcher {
    type_name: String,
}

impl TypeMatcher for NetTypeMatcher {
    fn matches(&self, value: Value) -> bool {
        match NetValue::from_value(value) {
            Some(net) => net.type_name == self.type_name,
            None => false,
        }
    }
}
