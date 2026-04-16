use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::{collections::HashMap, fmt};

use allocative::Allocative;
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
        Coerce, Freeze, FreezeResult, Freezer, FrozenValue, Heap, NoSerialize, StarlarkValue,
        Trace, Value, ValueLike,
        record::field::FieldGen,
        starlark_value,
        typing::{TypeCompiled, TypeInstanceId, TypeMatcher, TypeMatcherFactory},
    },
};
use starlark_map::sorted_map::SortedMap;

use crate::lang::evaluator_ext::EvaluatorExt;
use crate::lang::symbol::SymbolValue;
use crate::lang::type_conversion::{
    try_implicit_type_conversion, try_physical_conversion_from_compiled_type,
    try_physical_conversion_from_default,
};

use super::context::ContextValue;
use super::validation::validate_identifier_name;

pub type NetId = u64;

/// Global atomic counter for net IDs. Must be global (not thread-local) to ensure
/// unique IDs across all threads when using parallel evaluation (rayon).
static NEXT_NET_ID: AtomicU64 = AtomicU64::new(1);

/// Generate a new unique net ID using the global atomic counter.
pub fn generate_net_id() -> NetId {
    NEXT_NET_ID.fetch_add(1, Ordering::Relaxed)
}

fn builtin_optional_net_fields(type_name: &str) -> &'static [&'static str] {
    match type_name {
        "Net" => &["voltage", "impedance"],
        "Power" => &["voltage"],
        _ => &[],
    }
}

fn is_builtin_optional_net_field(type_name: &str, field_name: &str) -> bool {
    builtin_optional_net_fields(type_name).contains(&field_name)
}

fn is_unset_builtin_optional_net_field<'v>(
    type_name: &str,
    field_name: &str,
    value: Value<'v>,
) -> bool {
    value.is_none() && is_builtin_optional_net_field(type_name, field_name)
}

/// Reset the net ID counter to 1. This is only intended for use in tests
/// to ensure reproducible net IDs across test runs.
#[cfg(test)]
pub fn reset_net_id_counter() {
    NEXT_NET_ID.store(1, Ordering::Relaxed);
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
    /// The explicit constructor-provided leaf name used when cloning templates.
    #[serde(skip, default)]
    pub(crate) template_name: Option<String>,
    /// The name originally requested before deduplication
    pub original_name: Option<String>,
    /// Whether this net may adopt an assigned variable name after construction.
    #[serde(skip, default)]
    pub(crate) assignment_inferable: bool,
    /// Whether this net was constructed from another net value.
    #[serde(skip, default)]
    pub(crate) derived_from_base_net: bool,
    /// Whether this net value has been bound to a variable.
    #[serde(skip, default)]
    #[freeze(identity)]
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    pub(crate) was_bound: OnceLock<()>,
    #[serde(skip, default)]
    #[freeze(identity)]
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    pub(crate) inferred_name: OnceLock<String>,
    #[serde(skip, default)]
    #[freeze(identity)]
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    pub(crate) inferred_original_name: OnceLock<Option<String>>,
    /// Source file path where this net was created.
    #[serde(skip, default)]
    pub(crate) declaration_path: String,
    /// Source span where this net was created.
    #[serde(skip, default)]
    #[freeze(identity)]
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    pub(crate) declaration_span: Option<starlark::codemap::ResolvedSpan>,
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
        debug.field("name", &self.resolved_name());
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

    fn get_attr(&self, attribute: &str, heap: &'v Heap) -> Option<Value<'v>> {
        self.properties
            .get(attribute)
            .map(|v| v.to_value())
            .or_else(|| {
                self.is_builtin_optional_attr(attribute)
                    .then(|| heap.alloc(starlark::values::none::NoneType))
            })
    }

    fn has_attr(&self, attribute: &str, _heap: &'v Heap) -> bool {
        self.properties.contains_key(attribute) || self.is_builtin_optional_attr(attribute)
    }

    fn dir_attr(&self) -> Vec<String> {
        let mut attrs: Vec<String> = self.properties.keys().cloned().collect();
        for attr in self.builtin_optional_attrs() {
            if !attrs.iter().any(|existing| existing == attr) {
                attrs.push(attr.to_string());
            }
        }
        attrs.extend(vec![
            "name".to_string(),
            "net_id".to_string(),
            "original_name".to_string(),
            "type".to_string(),
        ]);
        attrs
    }

    fn export_as(
        &self,
        variable_name: &str,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<()> {
        self.mark_bound();
        self.infer_assignment_name(variable_name, eval)
    }
}

impl<'v, V: ValueLike<'v>> std::fmt::Display for NetValueGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name())
    }
}

impl<V> NetValueGen<V> {
    fn is_builtin_optional_attr(&self, attribute: &str) -> bool {
        is_builtin_optional_net_field(&self.type_name, attribute)
    }

    fn builtin_optional_attrs(&self) -> &'static [&'static str] {
        builtin_optional_net_fields(&self.type_name)
    }

    fn resolved_name(&self) -> &str {
        self.inferred_name
            .get()
            .map_or(&self.name, |name| name.as_str())
    }

    fn resolved_original_name_opt(&self) -> Option<&str> {
        self.inferred_original_name
            .get()
            .map_or(self.original_name.as_deref(), |name| name.as_deref())
    }

    fn clone_once_lock<T: Clone>(value: &OnceLock<T>) -> OnceLock<T> {
        let cloned = OnceLock::new();
        if let Some(value) = value.get() {
            let _ignore = cloned.set(value.clone());
        }
        cloned
    }
}

impl<'v, V: ValueLike<'v>> NetValueGen<V> {
    fn alloc_clone(&self, heap: &'v Heap, net_id: NetId, type_name: String) -> Value<'v> {
        let properties: SmallMap<String, Value<'v>> = self
            .properties
            .iter()
            .map(|(k, v)| (k.clone(), v.to_value()))
            .collect();

        heap.alloc(NetValue {
            net_id,
            name: self.name().to_owned(),
            template_name: self.template_name.clone(),
            original_name: self.original_name_opt().map(str::to_owned),
            assignment_inferable: self.assignment_inferable,
            derived_from_base_net: self.derived_from_base_net,
            was_bound: Self::clone_once_lock(&self.was_bound),
            inferred_name: Self::clone_once_lock(&self.inferred_name),
            inferred_original_name: Self::clone_once_lock(&self.inferred_original_name),
            declaration_path: self.declaration_path.clone(),
            declaration_span: self.declaration_span,
            type_name,
            properties,
        })
    }

    pub(crate) fn mark_bound(&self) {
        let _ignore = self.was_bound.set(());
    }

    pub(crate) fn cloned_bound_marker(&self) -> OnceLock<()> {
        Self::clone_once_lock(&self.was_bound)
    }

    pub fn infer_assignment_name(
        &self,
        inferred_name: &str,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<()> {
        if !self.assignment_inferable {
            return Ok(());
        }

        let (final_name, original_name) = if let Some(ctx) = eval.context_value() {
            ctx.infer_net_name(self.net_id, inferred_name)?
        } else {
            (inferred_name.to_owned(), None)
        };

        let _ignore = self.inferred_name.set(final_name.clone());
        let _ignore = self.inferred_original_name.set(original_name);

        Ok(())
    }

    /// Create a new NetValue
    pub fn new(net_id: NetId, name: String, properties: SmallMap<String, V>) -> Self {
        Self {
            net_id,
            name,
            template_name: None,
            original_name: None,
            assignment_inferable: false,
            derived_from_base_net: false,
            was_bound: OnceLock::new(),
            inferred_name: OnceLock::new(),
            inferred_original_name: OnceLock::new(),
            declaration_path: String::new(),
            declaration_span: None,
            type_name: "Net".to_string(),
            properties,
        }
    }

    /// Returns the instance name of this net
    pub fn name(&self) -> &str {
        self.resolved_name()
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
        self.resolved_original_name_opt()
            .unwrap_or_else(|| self.name())
    }

    /// Returns the type name of this net
    pub fn net_type_name(&self) -> &str {
        &self.type_name
    }

    /// Return the properties map of this net instance.
    pub fn properties(&self) -> &SmallMap<String, V> {
        &self.properties
    }

    pub fn declaration_path(&self) -> Option<&str> {
        (!self.declaration_path.is_empty()).then_some(self.declaration_path.as_str())
    }

    pub fn declaration_span(&self) -> Option<starlark::codemap::ResolvedSpan> {
        self.declaration_span
    }

    /// Return the original name as Option (None if auto-generated)
    pub fn original_name_opt(&self) -> Option<&str> {
        self.resolved_original_name_opt()
    }

    pub(crate) fn template_name_opt(&self) -> Option<&str> {
        self.template_name.as_deref()
    }

    pub(crate) fn derived_from_base_net(&self) -> bool {
        self.derived_from_base_net
    }

    pub(crate) fn was_bound(&self) -> bool {
        self.was_bound.get().is_some()
    }

    pub(crate) fn skips_implicit_checks(&self) -> bool {
        self.derived_from_base_net() || self.was_bound()
    }

    /// Create a new net with the same fields but a fresh net ID.
    /// This avoids deep copying - properties are shared via Value references.
    pub fn with_new_id(&self, heap: &'v Heap) -> Value<'v> {
        self.alloc_clone(heap, generate_net_id(), self.type_name.clone())
    }

    /// Create a new net with the same ID/name/properties but a different type name.
    /// Used for casting between net types (e.g., Power -> Net).
    pub fn with_net_type(&self, new_type_name: &str, heap: &'v Heap) -> Value<'v> {
        self.alloc_clone(heap, self.net_id, new_type_name.to_string())
    }

    /// Create a new net with identical runtime identity but updated declaration metadata.
    pub fn with_declaration_site(
        &self,
        declaration_path: impl Into<String>,
        declaration_span: Option<starlark::codemap::ResolvedSpan>,
        heap: &'v Heap,
    ) -> Value<'v> {
        let properties: SmallMap<String, Value<'v>> = self
            .properties
            .iter()
            .map(|(k, v)| (k.clone(), v.to_value()))
            .collect();

        heap.alloc(NetValue {
            net_id: self.net_id,
            name: self.name().to_owned(),
            template_name: self.template_name.clone(),
            original_name: self.original_name_opt().map(str::to_owned),
            assignment_inferable: self.assignment_inferable,
            derived_from_base_net: self.derived_from_base_net,
            was_bound: Self::clone_once_lock(&self.was_bound),
            inferred_name: Self::clone_once_lock(&self.inferred_name),
            inferred_original_name: Self::clone_once_lock(&self.inferred_original_name),
            declaration_path: declaration_path.into(),
            declaration_span,
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

#[derive(Clone, Copy)]
pub(crate) struct NetInstantiateOptions {
    pub(crate) should_register: bool,
    pub(crate) assignment_inferable: bool,
}

impl<'v, V: ValueLike<'v>> NetTypeGen<V> {
    pub(crate) fn instantiate(
        &self,
        base_net: Option<&NetValue<'v>>,
        explicit_name: Option<String>,
        field_values: SmallMap<String, Value<'v>>,
        options: NetInstantiateOptions,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        let heap = eval.heap();
        let (declaration_path, declaration_span) = eval
            .call_stack_top_location()
            .map(|loc| (loc.file.filename().to_string(), Some(loc.resolve_span())))
            .unwrap_or_else(|| (eval.source_path().unwrap_or_default(), None));

        let requested_name = explicit_name
            .clone()
            .or_else(|| base_net.and_then(|n| n.original_name_opt().map(str::to_owned)));
        let runtime_name = requested_name
            .clone()
            .or_else(|| base_net.map(|n| n.name().to_owned()));
        let template_name_for_new_net = (!options.assignment_inferable)
            .then(|| explicit_name.as_ref().cloned())
            .flatten();

        if let Some(ref n) = requested_name {
            validate_identifier_name(n, "Net name")?;
        }

        let (template_name, original_name, mut properties, net_id) = if let Some(n) = base_net {
            (
                n.template_name_opt().map(str::to_owned),
                requested_name,
                n.properties.clone(),
                n.net_id,
            )
        } else {
            (
                template_name_for_new_net,
                requested_name,
                SmallMap::new(),
                generate_net_id(),
            )
        };

        for (field_name, field_spec) in &self.fields {
            let provided_value = field_values.get(field_name).copied();
            let result = validate_field(field_name, field_spec.to_value(), provided_value, eval)?;

            if let Some(field_value) = result {
                match (
                    is_unset_builtin_optional_net_field(&self.type_name, field_name, field_value),
                    provided_value.is_some(),
                ) {
                    // Preserve inherited built-in values when the field was omitted.
                    (true, false) => {}
                    // But let an explicit `field=None` clear any inherited value.
                    (true, true) => {
                        properties.shift_remove(field_name.as_str());
                    }
                    (false, _) => {
                        properties.insert(field_name.clone(), field_value);
                    }
                }
            }
        }

        if let Some(symbol_val) = properties.get("symbol")
            && let Some(sym) = symbol_val.downcast_ref::<SymbolValue>()
        {
            if let Some(name) = sym.name() {
                properties.insert("symbol_name".to_string(), heap.alloc_str(name).to_value());
            }
            if let Some(path) = sym.source_uri() {
                properties.insert("symbol_path".to_string(), heap.alloc_str(path).to_value());
            }
            if let Some(raw_sexp) = sym.raw_sexp() {
                properties.insert(
                    "__symbol_value".to_string(),
                    heap.alloc_str(raw_sexp).to_value(),
                );
            }
        }

        let net_name = runtime_name.unwrap_or_default();
        let call_stack = eval.call_stack();
        let final_name = if options.should_register {
            eval.module()
                .extra_value()
                .and_then(|e| e.downcast_ref::<ContextValue>())
                .map(|ctx| {
                    ctx.register_net(
                        net_id,
                        &net_name,
                        options.assignment_inferable,
                        &self.type_name,
                        call_stack.clone(),
                    )
                })
                .transpose()
                .map_err(|e| anyhow::anyhow!(e.to_string()))?
                .unwrap_or_else(|| net_name.clone())
        } else {
            net_name.clone()
        };

        if base_net.is_none() && !net_name.is_empty() {
            let legacy_suffix = match self.type_name.as_str() {
                "Power" => Some("VCC"),
                "Ground" => Some("GND"),
                _ => None,
            };

            if let Some(suffix) = legacy_suffix
                && let Some(ctx) = eval
                    .module()
                    .extra_value()
                    .and_then(|e| e.downcast_ref::<ContextValue>())
            {
                let old_name = format!("{net_name}_{suffix}");
                ctx.add_moved_directive(old_name, net_name.clone(), true);
            }
        }

        Ok(heap.alloc(NetValue {
            net_id,
            name: final_name,
            template_name,
            original_name,
            assignment_inferable: options.assignment_inferable,
            derived_from_base_net: base_net.is_some(),
            was_bound: OnceLock::new(),
            inferred_name: OnceLock::new(),
            inferred_original_name: OnceLock::new(),
            declaration_path,
            declaration_span,
            type_name: self.type_name.clone(),
            properties,
        }))
    }

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

fn compile_field_type<'v>(
    field_spec: Value<'v>,
    heap: &'v Heap,
) -> anyhow::Result<TypeCompiled<Value<'v>>> {
    if let Some(field_gen) = field_spec.downcast_ref::<FieldGen<Value<'v>>>() {
        Ok(TypeCompiled::from_ty(field_gen.typ().as_ty(), heap))
    } else if let Some(field_gen) = field_spec.downcast_ref::<FieldGen<FrozenValue>>() {
        // Loaded modules freeze field(...) specs, but we still want to honor
        // the original compiled matcher for validation and coercion.
        Ok(TypeCompiled::from_ty(field_gen.typ().as_ty(), heap))
    } else {
        TypeCompiled::new(field_spec, heap)
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
    eval: &mut Evaluator<'v, '_, '_>,
) -> starlark::Result<Option<Value<'v>>> {
    let heap = eval.heap();

    // Try to extract default from field() spec first (before type compilation)
    let default = if let Some(fg) = field_spec.downcast_ref::<FieldGen<Value>>() {
        fg.default().map(|d| d.to_value())
    } else if let Some(fg) = field_spec.downcast_ref::<FieldGen<FrozenValue>>() {
        fg.default().map(|d| d.to_value())
    } else {
        None
    };

    // Extract TypeCompiled from field spec (FieldGen or direct type)
    let type_compiled = compile_field_type(field_spec, heap);

    let type_compiled = match type_compiled {
        Ok(t) => t,
        Err(_err) => {
            // Type compilation failed. If there's a default value, use it without validation.
            // If there's a provided value, we can't validate it, so just use it.
            // This is needed for forward compatibility with new field types.
            return Ok(provided_value.or(default));
        }
    };

    let field_type_error = |provided_val: Value<'v>| {
        anyhow::anyhow!(
            "Field `{}` has wrong type: expected `{}`, got value `{}` of type `{}`",
            field_name,
            type_compiled,
            provided_val.to_repr(),
            provided_val.get_type()
        )
    };

    if let Some(provided_val) = provided_value {
        if type_compiled.matches(provided_val) {
            Ok(Some(provided_val))
        } else {
            let converted = match try_implicit_type_conversion(provided_val, field_spec, eval)? {
                Some(converted) => Some(converted),
                None => {
                    match try_physical_conversion_from_compiled_type(
                        provided_val,
                        &type_compiled,
                        eval,
                    )? {
                        Some(converted) => Some(converted),
                        None => try_physical_conversion_from_default(provided_val, default, eval)?,
                    }
                }
            };

            match converted {
                Some(converted) if type_compiled.matches(converted) => Ok(Some(converted)),
                _ => Err(field_type_error(provided_val).into()),
            }
        }
    } else {
        // No provided value - use default if available
        Ok(default)
    }
}

pub(crate) fn instantiate_generated_net<'v>(
    spec: Value<'v>,
    generated_name: String,
    should_register: bool,
    assignment_inferable: bool,
    eval: &mut Evaluator<'v, '_, '_>,
) -> starlark::Result<Value<'v>> {
    if let Some(net_type) = spec.downcast_ref::<NetType<'v>>() {
        return net_type.instantiate(
            None,
            Some(generated_name),
            SmallMap::new(),
            NetInstantiateOptions {
                should_register,
                assignment_inferable,
            },
            eval,
        );
    }

    if let Some(net_type) = spec.downcast_ref::<FrozenNetType>() {
        return net_type.instantiate(
            None,
            Some(generated_name),
            SmallMap::new(),
            NetInstantiateOptions {
                should_register,
                assignment_inferable,
            },
            eval,
        );
    }

    Err(anyhow::anyhow!(
        "internal error: expected NetType when instantiating generated net, got {}",
        spec.get_type()
    )
    .into())
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
                let assignment_inferable = explicit_name.is_none() && base_net.is_none();

                self.instantiate(
                    base_net,
                    explicit_name,
                    field_values,
                    NetInstantiateOptions {
                        should_register,
                        assignment_inferable,
                    },
                    eval,
                )
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
