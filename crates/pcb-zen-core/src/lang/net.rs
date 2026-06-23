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
        typing::{TypeCompiled, TypeInstanceId, TypeMatcher, TypeMatcherDyn, TypeMatcherFactory},
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

pub(crate) fn compatible_net_type_view<'a>(actual: &str, expected: &'a str) -> Option<&'a str> {
    (actual != expected && expected == "Net").then_some(expected)
}

pub(crate) fn net_type_name_from_value<'v>(value: Value<'v>) -> Option<&'v str> {
    if let Some(net) = value.downcast_ref::<NetValue<'v>>() {
        Some(net.net_type_name())
    } else if let Some(net) = value.downcast_ref::<FrozenNetValue>() {
        Some(net.net_type_name())
    } else {
        None
    }
}

pub(crate) fn net_matches_type_name(value: Value<'_>, expected_type_name: &str) -> Option<bool> {
    if let Some(net) = value.downcast_ref::<NetValue<'_>>() {
        Some(net.is_open() || net.net_type_name() == expected_type_name)
    } else {
        value
            .downcast_ref::<FrozenNetValue>()
            .map(|net| net.is_open() || net.net_type_name() == expected_type_name)
    }
}

pub(crate) fn net_kind_requires_name(kind: &str) -> bool {
    kind != "NotConnected"
}

#[cfg(test)]
mod net_type_view_tests {
    use super::compatible_net_type_view;

    #[test]
    fn compatible_net_type_view_is_not_canonical_promotion() {
        for (actual, expected, view) in [
            ("Power", "Net", Some("Net")),
            ("Ground", "Net", Some("Net")),
            ("Power", "Power", None),
            ("Net", "Power", None),
            ("Ground", "Power", None),
        ] {
            assert_eq!(compatible_net_type_view(actual, expected), view);
        }
    }
}

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
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    ProvidesStaticType,
    Allocative,
    Trace,
    Freeze,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ConnectionIntent {
    #[default]
    Connected,
    Open,
}

impl ConnectionIntent {
    fn is_connected(&self) -> bool {
        *self == Self::Connected
    }

    fn is_open(&self) -> bool {
        *self == Self::Open
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
    /// The source-level name for this net, or empty while awaiting assignment inference.
    pub(crate) name: String,
    /// The explicit constructor-provided leaf name used when cloning templates.
    #[serde(skip, default)]
    pub(crate) template_name: Option<String>,
    /// The explicit constructor-provided source name, if any.
    pub original_name: Option<String>,
    /// Whether this net may adopt an assigned variable name after construction.
    #[serde(skip, default)]
    pub(crate) assignment_inferable: bool,
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
    /// Whether this net is intended to be connected or intentionally left open.
    #[serde(default, skip_serializing_if = "ConnectionIntent::is_connected")]
    pub(crate) connection_intent: ConnectionIntent,
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
        static RES: MethodsStatic = MethodsStatic::new("Net", builtin_net_methods);
        Some(RES.methods())
    }

    fn get_attr(&self, attribute: &str, heap: Heap<'v>) -> Option<Value<'v>> {
        match attribute {
            "NET" => Some(self.with_net_type("Net", heap)),
            _ => self
                .properties
                .get(attribute)
                .map(|v| v.to_value())
                .or_else(|| {
                    self.is_builtin_optional_attr(attribute)
                        .then(|| heap.alloc(starlark::values::none::NoneType))
                }),
        }
    }

    fn has_attr(&self, attribute: &str, _heap: Heap<'v>) -> bool {
        attribute == "NET"
            || self.properties.contains_key(attribute)
            || self.is_builtin_optional_attr(attribute)
    }

    fn dir_attr(&self) -> Vec<String> {
        let mut attrs: Vec<String> = self.properties.keys().cloned().collect();
        if !attrs.iter().any(|existing| existing == "NET") {
            attrs.push("NET".to_string());
        }
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
        self.original_name.as_deref()
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
    fn alloc_clone(
        &self,
        heap: Heap<'v>,
        net_id: NetId,
        type_name: String,
        connection_intent: ConnectionIntent,
    ) -> Value<'v> {
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
            was_bound: Self::clone_once_lock(&self.was_bound),
            inferred_name: Self::clone_once_lock(&self.inferred_name),
            declaration_path: self.declaration_path.clone(),
            declaration_span: self.declaration_span,
            type_name,
            connection_intent,
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

        let final_name = if let Some(ctx) = eval.context_value() {
            ctx.infer_net_name(self.net_id, inferred_name)?
        } else {
            inferred_name.to_owned()
        };

        let _ignore = self.inferred_name.set(final_name.clone());

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
            was_bound: OnceLock::new(),
            inferred_name: OnceLock::new(),
            declaration_path: String::new(),
            declaration_span: None,
            type_name: "Net".to_string(),
            connection_intent: ConnectionIntent::Connected,
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

    /// Returns the serialized/netlist kind for this net.
    pub fn net_kind_name(&self) -> &str {
        if self.connection_intent.is_open() {
            "NotConnected"
        } else {
            &self.type_name
        }
    }

    pub(crate) fn is_open(&self) -> bool {
        self.connection_intent.is_open()
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

    /// Return the explicit constructor-provided source name, if any.
    pub fn original_name_opt(&self) -> Option<&str> {
        self.resolved_original_name_opt()
    }

    pub(crate) fn template_name_opt(&self) -> Option<&str> {
        self.template_name.as_deref()
    }

    pub(crate) fn was_bound(&self) -> bool {
        self.was_bound.get().is_some()
    }

    pub(crate) fn is_external_reference(&self) -> bool {
        self.was_bound()
    }

    pub(crate) fn is_template_owned(&self) -> bool {
        !self.was_bound()
    }

    pub(crate) fn skips_implicit_checks(&self) -> bool {
        self.is_external_reference()
    }

    /// Create a new net with the same fields but a fresh net ID.
    /// This avoids deep copying - properties are shared via Value references.
    pub fn with_new_id(&self, heap: Heap<'v>) -> Value<'v> {
        self.alloc_clone(
            heap,
            generate_net_id(),
            self.type_name.clone(),
            self.connection_intent,
        )
    }

    /// Create a typed compatibility view with the same identity and properties.
    pub fn with_net_type(&self, new_type_name: &str, heap: Heap<'v>) -> Value<'v> {
        self.alloc_clone(
            heap,
            self.net_id,
            new_type_name.to_string(),
            self.connection_intent,
        )
    }

    /// Materialize this net on the current heap without changing its type or intent.
    pub fn to_current_heap(&self, heap: Heap<'v>) -> Value<'v> {
        self.alloc_clone(
            heap,
            self.net_id,
            self.type_name.clone(),
            self.connection_intent,
        )
    }

    /// Create a new net with identical runtime identity but updated declaration metadata.
    pub fn with_declaration_site(
        &self,
        declaration_path: impl Into<String>,
        declaration_span: Option<starlark::codemap::ResolvedSpan>,
        heap: Heap<'v>,
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
            was_bound: Self::clone_once_lock(&self.was_bound),
            inferred_name: Self::clone_once_lock(&self.inferred_name),
            declaration_path: declaration_path.into(),
            declaration_span,
            type_name: self.type_name.clone(),
            connection_intent: self.connection_intent,
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

pub(crate) fn instantiate_not_connected<'v>(
    args: &Arguments<'v, '_>,
    eval: &mut Evaluator<'v, '_, '_>,
) -> starlark::Result<Value<'v>> {
    let mut ignored_name = None;
    let positional_values: Vec<Value<'v>> = args.positions(eval.heap())?.collect();
    match positional_values.as_slice() {
        [] => {}
        [value] => {
            ignored_name = Some(value.unpack_str().ok_or_else(|| {
                starlark::Error::new_other(anyhow::anyhow!(
                    "NotConnected() expects an optional name string, got {}",
                    value.get_type()
                ))
            })?);
        }
        _ => {
            return Err(starlark::Error::new_other(anyhow::anyhow!(
                "NotConnected() accepts at most one positional argument"
            )));
        }
    }

    for (arg_name, value) in args.names_map()? {
        match arg_name.as_str() {
            "name" => {
                ignored_name = Some(value.unpack_str().ok_or_else(|| {
                    starlark::Error::new_other(anyhow::anyhow!(
                        "NotConnected() `name` must be string, got {}",
                        value.get_type()
                    ))
                })?);
            }
            other => {
                return Err(starlark::Error::new_other(anyhow::anyhow!(
                    "NotConnected() got unexpected named argument `{other}`"
                )));
            }
        }
    }

    let (declaration_path, declaration_span) = eval
        .call_stack_top_location()
        .map(|loc| (loc.file.filename().to_string(), Some(loc.resolve_span())))
        .unwrap_or_else(|| (eval.source_path().unwrap_or_default(), None));

    if ignored_name.is_some() {
        eval.add_diagnostic(
            crate::Diagnostic::categorized(
                &declaration_path,
                "NotConnected does not support names; name ignored",
                "net.not_connected_name_ignored",
                starlark::errors::EvalSeverity::Warning,
            )
            .with_span(declaration_span),
        );
    }

    Ok(eval.heap().alloc(NetValue {
        net_id: generate_net_id(),
        name: String::new(),
        template_name: None,
        original_name: None,
        assignment_inferable: false,
        was_bound: OnceLock::new(),
        inferred_name: OnceLock::new(),
        declaration_path,
        declaration_span,
        type_name: "Net".to_string(),
        connection_intent: ConnectionIntent::Open,
        properties: SmallMap::new(),
    }))
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
pub(crate) enum NetInstantiateIntent {
    Connected,
    PreserveBase,
}

#[derive(Clone, Copy)]
pub(crate) struct NetInstantiateOptions {
    pub(crate) should_register: bool,
    pub(crate) assignment_inferable: bool,
    pub(crate) intent: NetInstantiateIntent,
}

impl<'v, V: ValueLike<'v>> NetTypeGen<V> {
    pub(crate) fn instantiate(
        &self,
        base_net: Option<&NetValue<'v>>,
        mut explicit_name: Option<String>,
        field_values: SmallMap<String, Value<'v>>,
        options: NetInstantiateOptions,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        let heap = eval.heap();
        let (declaration_path, declaration_span) = eval
            .call_stack_top_location()
            .map(|loc| (loc.file.filename().to_string(), Some(loc.resolve_span())))
            .unwrap_or_else(|| (eval.source_path().unwrap_or_default(), None));

        let base_intent = base_net
            .map(|net| net.connection_intent)
            .unwrap_or(ConnectionIntent::Connected);
        let connection_intent = match options.intent {
            NetInstantiateIntent::Connected => ConnectionIntent::Connected,
            NetInstantiateIntent::PreserveBase => base_intent,
        };
        let source_names_enabled = !connection_intent.is_open();
        if !source_names_enabled && explicit_name.is_some() {
            eval.add_diagnostic(
                crate::Diagnostic::categorized(
                    &declaration_path,
                    "NotConnected does not support names; name ignored",
                    "net.not_connected_name_ignored",
                    starlark::errors::EvalSeverity::Warning,
                )
                .with_span(declaration_span),
            );
            explicit_name = None;
        }

        let source_named_base = source_names_enabled.then_some(base_net).flatten();
        let requested_name = explicit_name
            .clone()
            .or_else(|| source_named_base.and_then(|n| n.original_name_opt().map(str::to_owned)));
        let runtime_name = requested_name
            .clone()
            .or_else(|| source_named_base.map(|n| n.name().to_owned()));
        let assignment_inferable = options.assignment_inferable && source_names_enabled;
        let template_name = if assignment_inferable {
            None
        } else {
            explicit_name.clone()
        };

        if let Some(ref n) = requested_name {
            validate_identifier_name(n, "Net name")?;
        }

        let (template_name, original_name, mut properties, net_id) =
            if let Some(base_net) = base_net {
                (
                    source_named_base.and_then(|n| n.template_name_opt().map(str::to_owned)),
                    requested_name,
                    base_net.properties.clone(),
                    base_net.net_id,
                )
            } else {
                (
                    template_name,
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
        let was_bound = base_net
            .map(|net| net.cloned_bound_marker())
            .unwrap_or_default();
        let final_name = if options.should_register {
            eval.module()
                .extra_value()
                .and_then(|e| e.downcast_ref::<ContextValue>())
                .map(|ctx| {
                    let kind = if connection_intent.is_open() {
                        "NotConnected"
                    } else {
                        &self.type_name
                    };
                    ctx.register_net(net_id, &net_name, assignment_inferable, kind)
                })
                .transpose()
                .map_err(|e| anyhow::anyhow!(e.to_string()))?
                .unwrap_or_else(|| net_name.clone())
        } else {
            net_name.clone()
        };

        Ok(heap.alloc(NetValue {
            net_id,
            name: final_name,
            template_name,
            original_name,
            assignment_inferable,
            was_bound,
            inferred_name: OnceLock::new(),
            declaration_path,
            declaration_span,
            type_name: self.type_name.clone(),
            connection_intent,
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
            named_params, // named-only (name + fields + __register)
            false,
        )
    }
}

fn compile_field_type<'v>(
    field_spec: Value<'v>,
    heap: Heap<'v>,
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
                intent: NetInstantiateIntent::Connected,
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
                intent: NetInstantiateIntent::Connected,
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

                // Parse arguments: positional value, name= keyword, and field values
                let positional_value: Option<Value> = param_parser.next_opt()?;
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

                // Choose requested name: name= overrides positional string, which overrides base net's original name
                let explicit_name = name_from_kw.or(name_from_pos);
                let assignment_inferable =
                    explicit_name.is_none() && base_net.is_none_or(NetValue::is_open);

                self.instantiate(
                    base_net,
                    explicit_name,
                    field_values,
                    NetInstantiateOptions {
                        should_register,
                        assignment_inferable,
                        intent: NetInstantiateIntent::Connected,
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
        static RES: MethodsStatic = MethodsStatic::new("NetType", net_type_methods);
        Some(RES.methods())
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
#[derive(Hash, Debug, PartialEq, Clone, Allocative, pagable::Pagable)]
#[pagable::pagable_typetag(TypeMatcherDyn)]
struct NetTypeMatcher {
    type_name: String,
}

#[starlark::type_matcher]
impl TypeMatcher for NetTypeMatcher {
    fn matches(&self, value: Value) -> bool {
        net_matches_type_name(value, &self.type_name).unwrap_or(false)
    }
}
