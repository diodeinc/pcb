use std::{collections::HashMap, fmt, hash::Hash, sync::Mutex};

use allocative::Allocative;
use serde::{Deserialize, Serialize};
use starlark::{
    any::ProvidesStaticType,
    collections::SmallMap,
    environment::{Methods, MethodsBuilder, MethodsStatic},
    eval::{Arguments, Evaluator, ParametersSpec, ParametersSpecParam},
    starlark_module, starlark_simple_value,
    typing::{ParamIsRequired, ParamSpec, Ty, TyCallable, TyStarlarkValue, TyUser, TyUserParams},
    util::ArcStr,
    values::{
        starlark_value,
        typing::{TypeInstanceId, TypeMatcher, TypeMatcherFactory},
        Freeze, FreezeResult, FrozenValue, StarlarkValue, Value, ValueLike,
    },
};
use std::sync::OnceLock;

use super::context::ContextValue;
use super::net::{generate_net_id, NetId};
use super::validation::validate_identifier_name;

/// A typed net instance created by calling a NetType constructor
#[derive(Clone, Debug, ProvidesStaticType, Allocative, Serialize, Deserialize)]
pub struct BuiltinNet {
    /// The globally unique identifier for this net
    #[serde(skip)] // Skip: runtime-only, not stable across evaluations
    net_id: NetId,
    /// The final name after deduplication
    name: String,
    /// The name originally requested before deduplication
    #[serde(skip_serializing_if = "Option::is_none")]
    original_name: Option<String>,
    /// The type name (e.g., "Net", "Power", "Ground")
    #[serde(rename = "type")]
    type_name: String,
    /// Properties extracted from symbol (if provided)
    #[serde(skip_serializing_if = "SmallMap::is_empty", default)]
    properties: SmallMap<String, String>,
    /// The Symbol value if one was provided
    #[serde(skip_serializing_if = "Option::is_none")]
    symbol_name: Option<String>,
}

impl fmt::Display for BuiltinNet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}(\"{}\")", self.type_name, self.name)
    }
}

impl Freeze for BuiltinNet {
    type Frozen = Self;
    fn freeze(self, _freezer: &starlark::values::Freezer) -> FreezeResult<Self::Frozen> {
        Ok(self)
    }
}

starlark_simple_value!(BuiltinNet);

#[starlark_value(type = "BuiltinNet")]
impl<'v> StarlarkValue<'v> for BuiltinNet {
    fn get_methods() -> Option<&'static Methods> {
        static RES: MethodsStatic = MethodsStatic::new();
        RES.methods(builtin_net_methods)
    }
}

impl BuiltinNet {
    /// Returns the instance name of this net
    pub fn name(&self) -> &str {
        &self.name
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
}

#[starlark_module]
fn builtin_net_methods(methods: &mut MethodsBuilder) {
    #[starlark(attribute)]
    fn name(this: &BuiltinNet) -> starlark::Result<String> {
        Ok(this.name().to_string())
    }

    #[starlark(attribute)]
    fn net_id(this: &BuiltinNet) -> starlark::Result<i64> {
        Ok(this.net_id() as i64)
    }

    #[starlark(attribute)]
    fn original_name(this: &BuiltinNet) -> starlark::Result<String> {
        Ok(this.original_name().to_string())
    }

    #[starlark(attribute)]
    fn r#type(this: &BuiltinNet) -> starlark::Result<String> {
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
    type_name: String,
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
        write!(f, "{}", self.ty_name())
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
            [(ParamIsRequired::No, Ty::any())], // positional-only - accepts string or BuiltinNet
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
            [("value", ParametersSpecParam::Optional)], // positional-only - accepts string or BuiltinNet
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
impl<'v> StarlarkValue<'v> for NetType {
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
                let (original_name, mut properties, mut symbol_name) = match value_param {
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
                        (Some(name), SmallMap::new(), None)
                    }
                    Some(value) => {
                        let source_net = BuiltinNet::from_value(value).ok_or_else(|| {
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
                            source_net.symbol_name.clone(),
                        )
                    }
                    None => {
                        if let Some(n) = &name_keyword_str {
                            validate_identifier_name(n, "Net name")?;
                        }
                        (name_keyword_str, SmallMap::new(), None)
                    }
                };

                // Process symbol parameter (overrides copied properties)
                if let Some(symbol) = symbol_value {
                    if symbol.get_type() != "Symbol" {
                        return Err(anyhow::anyhow!(
                            "'symbol' must be Symbol, got {}",
                            symbol.get_type()
                        )
                        .into());
                    }

                    if let Some(sym) = symbol.downcast_ref::<crate::lang::symbol::SymbolValue>() {
                        if let Some(name) = sym.name() {
                            symbol_name = Some(name.to_string());
                            properties.insert("symbol_name".to_string(), name.to_string());
                        }
                        if let Some(path) = sym.source_path() {
                            properties.insert("symbol_path".to_string(), path.to_string());
                        }
                        if let Some(raw_sexp) = sym.raw_sexp() {
                            properties.insert("__symbol_value".to_string(), raw_sexp.to_string());
                        }
                    }
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

                Ok(heap.alloc(BuiltinNet {
                    net_id,
                    name: final_name,
                    original_name,
                    type_name: self.type_name.clone(),
                    properties,
                    symbol_name,
                }))
            })
    }

    fn eval_type(&self) -> Option<Ty> {
        let id = self.type_instance_id();
        Some(Ty::custom(
            TyUser::new(
                self.instance_ty_name(),
                TyStarlarkValue::new::<BuiltinNet>(),
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
/// Validates that a BuiltinNet instance has the expected type_name
#[derive(Hash, Debug, PartialEq, Clone, Allocative)]
struct NetTypeMatcher {
    type_name: String,
}

impl TypeMatcher for NetTypeMatcher {
    fn matches(&self, value: Value) -> bool {
        match BuiltinNet::from_value(value) {
            Some(net) => net.type_name == self.type_name,
            None => false,
        }
    }
}
