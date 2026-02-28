use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::{collections::HashMap, fmt};

use allocative::Allocative;
use pcb_sch::physical::PhysicalValueType;
use starlark::typing::TyUserFields;
use starlark::{
    any::ProvidesStaticType,
    collections::SmallMap,
    environment::{Methods, MethodsBuilder, MethodsStatic},
    eval::{Arguments, Evaluator, ParametersSpec, ParametersSpecParam},
    starlark_complex_value, starlark_module,
    typing::{ParamIsRequired, ParamSpec, Ty, TyCallable, TyStarlarkValue, TyUser, TyUserParams},
    util::ArcStr,
    values::{
        Coerce, Freeze, FreezeResult, Freezer, FrozenHeap, FrozenValue, Heap, NoSerialize,
        StarlarkValue, Trace, Value, ValueLike,
        record::field::FieldGen,
        starlark_value,
        typing::{TypeCompiled, TypeInstanceId, TypeMatcher, TypeMatcherFactory},
    },
};
use starlark_map::sorted_map::SortedMap;

use crate::lang::evaluator_ext::EvaluatorExt;
use crate::lang::naming;
use crate::lang::path::normalize_path_to_package_uri;
use crate::lang::symbol::SymbolValue;

use super::context::ContextValue;
use super::symbol::SymbolType;
use super::validation::validate_identifier_name;

pub type NetId = u64;

/// Global atomic counter for net IDs. Must be global (not thread-local) to ensure
/// unique IDs across all threads when using parallel evaluation (rayon).
static NEXT_NET_ID: AtomicU64 = AtomicU64::new(1);

/// Generate a new unique net ID using the global atomic counter.
pub fn generate_net_id() -> NetId {
    NEXT_NET_ID.fetch_add(1, Ordering::Relaxed)
}

/// Reset the net ID counter to 1. This is only intended for use in tests
/// to ensure reproducible net IDs across test runs.
#[cfg(test)]
pub fn reset_net_id_counter() {
    NEXT_NET_ID.store(1, Ordering::Relaxed);
}

/// Create the default builtin Net type with standard fields (symbol, voltage, impedance)
pub fn make_default_net_type(heap: &FrozenHeap) -> FrozenNetType {
    let mut fields: SmallMap<String, FrozenValue> = SmallMap::new();

    // Field: symbol = Symbol
    fields.insert("symbol".to_owned(), heap.alloc(SymbolType));

    // Field: voltage = Voltage (unified type now handles ranges too)
    fields.insert(
        "voltage".to_owned(),
        heap.alloc(PhysicalValueType::new(pcb_sch::PhysicalUnit::Volts.into())),
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

    fn has_attr(&self, attribute: &str, _heap: &'v Heap) -> bool {
        self.properties.contains_key(attribute)
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
        write!(f, "{}", self.name)
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

    /// Create a new net with the same ID/name/properties but a different type name.
    /// Used for casting between net types (e.g., Power -> Net).
    pub fn with_net_type(&self, new_type_name: &str, heap: &'v Heap) -> Value<'v> {
        let properties: SmallMap<String, Value<'v>> = self
            .properties
            .iter()
            .map(|(k, v)| (k.clone(), v.to_value()))
            .collect();

        heap.alloc(NetValue {
            net_id: self.net_id,
            name: self.name.clone(),
            original_name: self.original_name.clone(),
            type_name: new_type_name.to_string(),
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

    /// Convert this net to base Net type, preserving all properties
    #[starlark(attribute)]
    fn NET<'v>(this: &NetValue<'v>, heap: &'v Heap) -> starlark::Result<Value<'v>> {
        Ok(this.with_net_type("Net", heap))
    }
}

/// A callable type constructor for creating typed nets
///
/// Created by `builtin.net_type(name)`, e.g.:
/// - `Net = builtin.net_type("Net")`
/// - `Power = builtin.net_type("Power")`
/// - `Ground = builtin.net_type("Ground")`
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
            let type_compiled_result =
                if let Some(field_gen) = field_value.downcast_ref::<FieldGen<Value<'v>>>() {
                    Ok(*field_gen.typ())
                } else {
                    TypeCompiled::new(field_value, eval.heap())
                };

            type_compiled_result.map_err(|e| {
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
        let mut named_params = vec![
            (ArcStr::from("NET"), ParamIsRequired::No, Ty::any()),
            (ArcStr::from("name"), ParamIsRequired::No, Ty::string()),
        ];

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
            named_params,                       // keyword-only (NET, name + fields)
            None,                               // **kwargs
        )
        .expect("ParamSpec creation should not fail")
    }

    /// Returns the runtime parameter specification for parsing arguments
    fn parameters_spec(&self) -> ParametersSpec<FrozenValue> {
        let mut named_params = vec![
            ("NET", ParametersSpecParam::Optional),
            ("name", ParametersSpecParam::Optional),
            ("__register", ParametersSpecParam::Optional),
        ];

        for field_name in self.fields.keys() {
            named_params.push((field_name.as_str(), ParametersSpecParam::Optional));
        }

        ParametersSpec::new_parts(
            self.instance_ty_name().as_str(),
            [("value", ParametersSpecParam::Optional)], // positional-only - accepts string or NetValue
            [],                                         // pos_or_named (args)
            false,
            named_params, // named-only (NET, name + fields + __register)
            false,
        )
    }
}

/// Process a field specification: validate provided value or apply default.
///
/// This is the single unified function for field validation used by both
/// builtin.net() and interface(). It handles:
/// 1. If value provided: extract type from spec, validate against it
/// 2. Else if field has default: use the default
/// 3. Else: return None
pub(crate) fn validate_field<'v>(
    field_name: &str,
    field_spec: Value<'v>,
    provided_value: Option<Value<'v>>,
    heap: &'v Heap,
) -> starlark::Result<Option<Value<'v>>> {
    // Try to extract default from field() spec first (before type compilation)
    let default = if let Some(fg) = field_spec.downcast_ref::<FieldGen<Value>>() {
        fg.default().map(|d| d.to_value())
    } else if let Some(fg) = field_spec.downcast_ref::<FieldGen<FrozenValue>>() {
        fg.default().map(|d| d.to_value())
    } else {
        None
    };

    // Extract TypeCompiled from field spec (FieldGen or direct type)
    let type_compiled = if let Some(field_gen) = field_spec.downcast_ref::<FieldGen<Value<'v>>>() {
        Ok(*field_gen.typ())
    } else {
        TypeCompiled::new(field_spec, heap)
    };

    let type_compiled = match type_compiled {
        Ok(t) => t,
        Err(_err) => {
            // Type compilation failed. If there's a default value, use it without validation.
            // If there's a provided value, we can't validate it, so just use it.
            // This is needed for forward compatibility with new field types.
            return Ok(provided_value.or(default));
        }
    };

    if let Some(provided_val) = provided_value {
        // User provided a value - validate it against the field's type spec
        // Validate provided value against type
        if type_compiled.matches(provided_val) {
            Ok(Some(provided_val))
        } else {
            Err(anyhow::anyhow!(
                "Field `{}` has wrong type: expected `{}`, got value `{}` of type `{}`",
                field_name,
                type_compiled,
                provided_val.to_repr(),
                provided_val.get_type()
            )
            .into())
        }
    } else {
        // No provided value - use default if available
        Ok(default)
    }
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

                // Parse arguments: positional value, NET= keyword, name= keyword, and field values
                let positional_value: Option<Value> = param_parser.next_opt()?;
                let net_keyword: Option<Value> = param_parser.next_opt()?;
                let name_keyword: Option<Value> = param_parser.next_opt()?;

                // Parse hidden __register parameter (for internal use only)
                let should_register: bool = param_parser.next_opt()?.unwrap_or(true);

                // Parse field values (all optional)
                let mut field_values = SmallMap::new();
                for field_name in self.fields.keys() {
                    if let Some(field_val) = param_parser.next_opt::<Value>()? {
                        field_values.insert(field_name.clone(), field_val);
                    }
                }

                // Extract name keyword as string if provided
                let name_from_kw: Option<String> = name_keyword
                    .map(|v| {
                        v.unpack_str()
                            .ok_or_else(|| {
                                anyhow::anyhow!(
                                    "{}() 'name' must be string, got {}",
                                    type_name,
                                    v.get_type()
                                )
                            })
                            .map(|s| s.to_owned())
                    })
                    .transpose()?;

                // Determine base_net and/or positional name
                let mut base_net: Option<&NetValue> = None;
                let mut name_from_pos: Option<String> = None;

                if let Some(v) = positional_value {
                    if let Some(s) = v.unpack_str() {
                        name_from_pos = Some(s.to_owned());
                    } else if let Some(nv) = NetValue::from_value(v) {
                        base_net = Some(nv);
                    } else {
                        return Err(anyhow::anyhow!(
                            "{}() expects string or Net value as positional, got {}",
                            type_name,
                            v.get_type()
                        )
                        .into());
                    }
                }

                if let Some(v) = net_keyword {
                    let nv = NetValue::from_value(v).ok_or_else(|| {
                        anyhow::anyhow!(
                            "{}() NET= expects Net value, got {}",
                            type_name,
                            v.get_type()
                        )
                    })?;
                    if base_net.is_some() {
                        return Err(anyhow::anyhow!(
                            "{}() cannot provide both a positional Net and NET=",
                            type_name
                        )
                        .into());
                    }
                    base_net = Some(nv);
                }

                // Choose requested name: name= overrides positional string, which overrides base net's original name
                let explicit_name = name_from_kw.or(name_from_pos);
                let requested_name: Option<String> = explicit_name
                    .clone()
                    .or_else(|| base_net.and_then(|n| n.original_name.clone()));

                if let Some(ref n) = requested_name {
                    validate_identifier_name(n, "Net name")?;
                }

                // Check naming convention for explicitly provided names (not inherited from base_net)
                if let Some(ref explicit) = explicit_name {
                    let (path, span) = eval
                        .call_stack_top_location()
                        .map(|loc| (loc.file.filename().to_string(), Some(loc.resolve_span())))
                        .unwrap_or_else(|| (eval.source_path().unwrap_or_default(), None));
                    if let Some(diag) =
                        naming::check_net_naming(explicit, span, std::path::Path::new(&path))
                    {
                        eval.add_diagnostic(diag);
                    }
                }

                // Build (original_name, properties, net_id)
                let (original_name, mut properties, net_id) = if let Some(n) = base_net {
                    (requested_name, n.properties.clone(), n.net_id)
                } else {
                    (requested_name, SmallMap::new(), generate_net_id())
                };

                // Merge field values into properties, applying defaults for unprovided fields
                for (field_name, field_spec) in &self.fields {
                    let result = validate_field(
                        field_name,
                        field_spec.to_value(),
                        field_values.get(field_name).copied(),
                        heap,
                    )?;

                    if let Some(field_value) = result {
                        properties.insert(field_name.clone(), field_value);
                    }
                    // If no value and no default, field won't be in properties
                }

                // Extract symbol metadata if a symbol field exists (from explicit value or default)
                if let Some(symbol_val) = properties.get("symbol")
                    && let Some(sym) = symbol_val.downcast_ref::<SymbolValue>()
                {
                    if let Some(name) = sym.name() {
                        properties
                            .insert("symbol_name".to_string(), heap.alloc_str(name).to_value());
                    }
                    if let Some(path) = sym.source_path() {
                        let normalized = normalize_path_to_package_uri(path, eval.eval_context());
                        properties.insert(
                            "symbol_path".to_string(),
                            heap.alloc_str(&normalized).to_value(),
                        );
                    }
                    if let Some(raw_sexp) = sym.raw_sexp() {
                        properties.insert(
                            "__symbol_value".to_string(),
                            heap.alloc_str(raw_sexp).to_value(),
                        );
                    }
                }

                // Register net in the current module (or skip if __register=false)
                let net_name = original_name.clone().unwrap_or_default();
                let call_stack = eval.call_stack();
                let final_name = if should_register {
                    eval.module()
                        .extra_value()
                        .and_then(|e| e.downcast_ref::<ContextValue>())
                        .map(|ctx| {
                            ctx.register_net(net_id, &net_name, &self.type_name, call_stack.clone())
                        })
                        .transpose()
                        .map_err(|e| anyhow::anyhow!(e.to_string()))?
                        .unwrap_or_else(|| net_name.clone())
                } else {
                    net_name.clone()
                };

                // Generate automatic moved directive for interface-like typed nets
                // to maintain backward compatibility with old single-net interface naming
                // Old format: {instance_name}_{LEGACY_SUFFIX} (e.g., "3V3_VCC" for Power)
                // New format: {instance_name} (e.g., "3V3")
                if base_net.is_none() && !net_name.is_empty() {
                    // Map typed net names to their legacy interface field suffixes
                    let legacy_suffix = match self.type_name.as_str() {
                        "Power" => Some("VCC"),
                        "Ground" => Some("GND"),
                        _ => None,
                    };

                    if let Some(suffix) = legacy_suffix {
                        let old_name = format!("{net_name}_{suffix}");
                        if let Some(ctx) = eval
                            .module()
                            .extra_value()
                            .and_then(|e| e.downcast_ref::<ContextValue>())
                        {
                            ctx.add_moved_directive(old_name, net_name.clone(), true);
                        }
                    }
                }

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

        // Build known fields from self.fields
        // TODO(type-hints): Extract proper Ty from field specs instead of Ty::any()
        let known_fields: SortedMap<String, Ty> = self
            .fields
            .keys()
            .map(|field_name| (field_name.clone(), Ty::any()))
            .collect();

        Some(Ty::custom(
            TyUser::new(
                self.instance_ty_name(),
                TyStarlarkValue::new::<NetValue>(),
                id,
                TyUserParams {
                    matcher: Some(TypeMatcherFactory::new(NetTypeMatcher {
                        type_name: self.type_name.clone(),
                    })),
                    fields: TyUserFields {
                        known: known_fields,
                        unknown: false,
                    },
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
