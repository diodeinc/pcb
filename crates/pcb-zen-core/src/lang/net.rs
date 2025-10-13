use std::{cell::RefCell, collections::HashMap, fmt, sync::Mutex};

use allocative::Allocative;
use starlark::{
    any::ProvidesStaticType,
    collections::SmallMap,
    environment::{Methods, MethodsBuilder, MethodsStatic},
    eval::{Arguments, Evaluator, ParametersSpec, ParametersSpecParam},
    starlark_complex_value, starlark_module,
    typing::{ParamIsRequired, ParamSpec, Ty, TyCallable, TyStarlarkValue, TyUser, TyUserParams},
    util::ArcStr,
    values::{
        starlark_value,
        typing::{TypeCompiled, TypeInstanceId, TypeMatcher, TypeMatcherFactory},
        Coerce, Freeze, FreezeResult, Freezer, FrozenHeap, FrozenValue, Heap, NoSerialize,
        StarlarkValue, Trace, Value, ValueLike,
    },
};
use std::sync::OnceLock;

use crate::lang::symbol::SymbolValue;

use super::context::ContextValue;
use super::physical::{PhysicalRangeType, PhysicalValueType};
use super::symbol::SymbolType;
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

/// Create the default builtin Net type with standard fields (symbol, voltage, impedance)
pub fn make_default_net_type(heap: &FrozenHeap) -> FrozenNetType {
    let mut fields: SmallMap<String, FrozenValue> = SmallMap::new();

    // Field: symbol = Symbol
    fields.insert("symbol".to_owned(), heap.alloc(SymbolType));

    // Field: voltage = VoltageRange
    fields.insert(
        "voltage".to_owned(),
        heap.alloc(PhysicalRangeType::new(pcb_sch::PhysicalUnit::Volts.into())),
    );

    // Field: impedance = Resistance (using PhysicalValueType for single values)
    fields.insert(
        "impedance".to_owned(),
        heap.alloc(PhysicalValueType::new(pcb_sch::PhysicalUnit::Ohms.into())),
    );

    FrozenNetType {
        type_name: "Net".to_owned(),
        fields,
    }
}

/// Extract the TypeCompiled from a field spec (FieldGen or direct type Value)
///
/// For FieldGen, extract the inner TypeCompiled. For direct types, create TypeCompiled.
fn field_spec_to_type_compiled<'v>(
    spec: Value<'v>,
    heap: &'v Heap,
) -> anyhow::Result<TypeCompiled<Value<'v>>> {
    use starlark::values::types::record::field::FieldGen;

    // If it's a FieldGen, extract the inner TypeCompiled directly
    if let Some(field_gen) = spec.downcast_ref::<FieldGen<Value<'v>>>() {
        return Ok(*field_gen.typ());
    }
    if spec.downcast_ref::<FieldGen<FrozenValue>>().is_some() {
        // For frozen FieldGen, we need to get the type and create a new TypeCompiled
        // The TypeCompiled inside is for FrozenValue, we need one for Value
        return TypeCompiled::new(spec, heap); // Let TypeCompiled handle the conversion
    }

    // Otherwise it's a direct type constructor - create TypeCompiled from it
    TypeCompiled::new(spec, heap)
}

#[derive(
    Clone,
    PartialEq,
    Eq,
    ProvidesStaticType,
    Allocative,
    Trace,
    Freeze,
    Coerce,
    serde::Serialize,
    serde::Deserialize,
)]
#[repr(C)]
#[serde(bound(
    serialize = "V: serde::Serialize",
    deserialize = "V: serde::Deserialize<'de>"
))]
pub struct NetValueGen<V> {
    /// The globally unique identifier for this net
    pub(crate) net_id: NetId,
    /// The final name after deduplication
    pub(crate) name: String,
    /// The name originally requested before deduplication
    pub original_name: Option<String>,
    /// The type name (e.g., "Net", "Power", "Ground")
    pub(crate) type_name: String,
    /// Properties (including symbol, voltage, impedance, etc. if provided)
    pub(crate) properties: SmallMap<String, V>,
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

    fn get_attr(&self, attribute: &str, _heap: &'v Heap) -> Option<Value<'v>> {
        // Check if the attribute is a field stored in properties
        self.properties.get(attribute).map(|v| v.to_value())
    }

    fn dir_attr(&self) -> Vec<String> {
        // Return all field names from properties
        let mut attrs: Vec<String> = self.properties.keys().cloned().collect();
        // Also include the built-in attributes from methods
        attrs.extend(vec![
            "name".to_string(),
            "net_id".to_string(),
            "original_name".to_string(),
            "type".to_string(),
        ]);
        attrs
    }
}

impl<'v, V: ValueLike<'v>> std::fmt::Display for NetValueGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl<'v, V: ValueLike<'v>> NetValueGen<V> {
    /// Create a new NetValue
    pub fn new(net_id: NetId, name: String, properties: SmallMap<String, V>) -> Self {
        Self {
            net_id,
            name,
            original_name: None,
            type_name: "Net".to_string(),
            properties,
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

    /// Return the original name as Option (None if auto-generated)
    pub fn original_name_opt(&self) -> Option<&str> {
        self.original_name.as_deref()
    }

    /// Create a new net with the same fields but a fresh net ID.
    /// This avoids deep copying - properties are shared via Value references.
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
#[derive(Clone, Debug, Trace, Coerce, ProvidesStaticType, Allocative, NoSerialize)]
#[repr(C)]
pub struct NetTypeGen<V> {
    /// The type name (e.g., "Net", "Power", "Ground")
    pub(crate) type_name: String,
    /// Field specifications: field name -> field spec value (FieldGen or type constructor)
    /// Types are validated at net type definition time, re-compiled at net instantiation time
    pub(crate) fields: SmallMap<String, V>,
}

starlark_complex_value!(pub NetType);

impl<'v> Freeze for NetType<'v> {
    type Frozen = FrozenNetType;
    fn freeze(self, freezer: &Freezer) -> FreezeResult<Self::Frozen> {
        Ok(FrozenNetType {
            type_name: self.type_name,
            fields: self.fields.freeze(freezer)?,
        })
    }
}

impl<V> NetTypeGen<V> {
    /// Returns the instance type name (e.g., "Net", "Power", "Ground")
    fn instance_ty_name(&self) -> String {
        self.type_name.to_string()
    }

    /// Returns the callable type name (e.g., "NetType", "PowerType", "GroundType")
    fn ty_name(&self) -> String {
        format!("{}Type", self.type_name)
    }
}

impl<V> fmt::Display for NetTypeGen<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.instance_ty_name())
    }
}

impl<'v> NetType<'v> {
    /// Create a new NetType with the given type name and field specifications
    pub fn new(
        type_name: String,
        kwargs: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<NetType<'v>> {
        let mut fields = SmallMap::new();

        // Process each field parameter and validate types
        for (field_name, field_value) in kwargs {
            // Reserved field name
            if field_name == "name" {
                return Err(starlark::Error::new_other(anyhow::anyhow!(
                    "Field name 'name' is reserved (conflicts with implicit name parameter)"
                )));
            }

            // Validate the field spec by compiling its type - this fails early if invalid
            // This handles field(), direct types (str/int), and custom types (Enum/PhysicalValue) uniformly
            field_spec_to_type_compiled(field_value, eval.heap()).map_err(|e| {
                starlark::Error::new_other(anyhow::anyhow!(
                    "Invalid type spec for field '{}': {}",
                    field_name,
                    e
                ))
            })?;

            fields.insert(field_name, field_value);
        }

        Ok(NetType { type_name, fields })
    }
}

impl<'v, V: ValueLike<'v>> NetTypeGen<V> {
    /// Get the unique TypeInstanceId for this NetType based on structural equivalence.
    /// Net types with identical type_name AND field names share the same TypeInstanceId.
    fn type_instance_id(&self) -> TypeInstanceId {
        type NetTypeCache = HashMap<(String, Vec<String>), TypeInstanceId>;
        static CACHE: OnceLock<Mutex<NetTypeCache>> = OnceLock::new();

        // Build field signature from field names only (not types, for backward compat)
        let mut field_names: Vec<String> = self.fields.keys().cloned().collect();

        // Sort by field name for structural equivalence
        field_names.sort();

        let cache_key = (self.type_name.clone(), field_names);
        let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        *cache
            .lock()
            .unwrap()
            .entry(cache_key)
            .or_insert_with(TypeInstanceId::r#gen)
    }

    /// Returns the parameter specification for this type's constructor
    fn param_spec(&self) -> ParamSpec {
        let mut named_params = vec![(ArcStr::from("name"), ParamIsRequired::No, Ty::string())];

        // Add all field parameters as optional named-only
        // TODO(type-hints): Extract Ty from field specs for better LSP hints. Currently Ty::any().
        for field_name in self.fields.keys() {
            named_params.push((
                ArcStr::from(field_name.as_str()),
                ParamIsRequired::No,
                Ty::any(),
            ));
        }

        ParamSpec::new_parts(
            [(ParamIsRequired::No, Ty::any())], // positional-only - accepts string or NetValue
            [],                                 // pos_or_named
            None,                               // *args
            named_params,                       // keyword-only (name + fields)
            None,                               // **kwargs
        )
        .expect("ParamSpec creation should not fail")
    }

    /// Returns the runtime parameter specification for parsing arguments
    fn parameters_spec(&self) -> ParametersSpec<FrozenValue> {
        let mut named_params = vec![("name", ParametersSpecParam::Optional)];

        for field_name in self.fields.keys() {
            named_params.push((field_name.as_str(), ParametersSpecParam::Optional));
        }

        ParametersSpec::new_parts(
            self.instance_ty_name().as_str(),
            [("value", ParametersSpecParam::Optional)], // positional-only - accepts string or NetValue
            [],                                         // pos_or_named (args)
            false,
            named_params, // named-only (name + fields)
            false,
        )
    }
}

/// Validate a field value using a pre-compiled TypeCompiled.
///
/// The TypeCompiled was validated and stored at net type definition time,
/// so we know it's valid and can directly use it for matching.
fn validate_field_value<'v>(
    field_name: &str,
    type_compiled: &TypeCompiled<Value<'v>>,
    provided_value: Value<'v>,
) -> anyhow::Result<Value<'v>> {
    if type_compiled.matches(provided_value) {
        return Ok(provided_value);
    }
    anyhow::bail!(
        "Field '{}' has wrong type: expected {}, got {}",
        field_name,
        type_compiled,
        provided_value.get_type()
    );
}

#[starlark_value(type = "NetType")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for NetTypeGen<V>
where
    Self: ProvidesStaticType<'v>,
{
    type Canonical = FrozenNetType;
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

                // Parse field values (all optional)
                let mut field_values = SmallMap::new();
                for field_name in self.fields.keys() {
                    if let Some(field_val) = param_parser.next_opt::<Value>()? {
                        field_values.insert(field_name.clone(), field_val);
                    }
                }

                // Determine name, properties, net_id, and original_name from value_param
                let (original_name, mut properties, net_id) = match value_param {
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
                        (Some(name), SmallMap::new(), generate_net_id())
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

                        // Allow overriding the name with explicit name= keyword, otherwise preserve original
                        let original_name = match &name_keyword_str {
                            Some(n) => {
                                validate_identifier_name(n, "Net name")?;
                                Some(n.clone())
                            }
                            None => source_net.original_name.clone(),
                        };
                        // Preserve net_id when casting between net types
                        (
                            original_name,
                            source_net.properties.clone(),
                            source_net.net_id,
                        )
                    }
                    None => {
                        if let Some(n) = &name_keyword_str {
                            validate_identifier_name(n, "Net name")?;
                        }
                        (name_keyword_str, SmallMap::new(), generate_net_id())
                    }
                };

                // Merge field values into properties, applying defaults for unprovided fields
                use starlark::values::types::record::field::FieldGen;
                for (field_name, field_spec) in &self.fields {
                    if let Some(provided_val) = field_values.get(field_name) {
                        // User provided a value - validate it against the field's type spec
                        let tc = field_spec_to_type_compiled(field_spec.to_value(), heap)
                            .map_err(|e| anyhow::anyhow!("Internal error: {}", e))
                            .map_err(starlark::Error::new_other)?;
                        let validated_val = validate_field_value(field_name, &tc, *provided_val)
                            .map_err(starlark::Error::new_other)?;
                        properties.insert(field_name.clone(), validated_val);
                    } else {
                        // User didn't provide value - try to apply default from field() spec
                        if let Some(field_gen) =
                            field_spec.to_value().downcast_ref::<FieldGen<Value>>()
                        {
                            if let Some(default_val) = field_gen.default() {
                                properties.insert(field_name.clone(), default_val.to_value());
                            }
                        } else if let Some(field_gen) = field_spec
                            .to_value()
                            .downcast_ref::<FieldGen<FrozenValue>>()
                        {
                            if let Some(default_val) = field_gen.default() {
                                properties.insert(field_name.clone(), default_val.to_value());
                            }
                        }
                        // If no field() wrapper or no default, field won't be in properties
                    }
                }

                // Extract symbol metadata if a symbol field exists (from explicit value or default)
                if let Some(symbol_val) = properties.get("symbol") {
                    if let Some(sym) = symbol_val.downcast_ref::<SymbolValue>() {
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
                }

                // Register net (or reuse existing registration for casts)
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
