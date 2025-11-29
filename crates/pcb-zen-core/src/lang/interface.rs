use allocative::Allocative;
use once_cell::unsync::OnceCell;
use starlark::collections::SmallMap;
use starlark::environment::GlobalsBuilder;
use starlark::eval::{Arguments, Evaluator, ParametersSpec, ParametersSpecParam};
use starlark::starlark_complex_value;
use starlark::starlark_module;
use starlark::values::typing::TypeInstanceId;
use starlark::values::{
    starlark_value, Coerce, Freeze, FrozenValue, Heap, NoSerialize, ProvidesStaticType,
    StarlarkValue, Trace, Value, ValueLike,
};

use std::sync::Arc;

use crate::lang::context::ContextValue;
use crate::lang::evaluator_ext::EvaluatorExt;
use crate::lang::net::{validate_field, NetValue};
use crate::lang::validation::validate_identifier_name;

/// Tracks both old and new style instance prefixes for backward compatibility
#[derive(Debug, Clone, Default)]
pub(crate) struct InstancePrefix {
    old_style: String, // legacy: "DEBUG_UART_TX"
    new_style: String, // modern: "debug_uart_tx"
}

impl InstancePrefix {
    #[inline]
    pub(crate) fn empty() -> Self {
        Self::default()
    }

    #[inline]
    pub(crate) fn from_root(root: &str) -> Self {
        Self {
            old_style: root.to_owned(),
            new_style: root.to_owned(),
        }
    }

    /// Underscore-joins `segment` after `prefix` unless `prefix` is empty
    #[inline]
    fn join(prefix: &str, segment: &str) -> String {
        if prefix.is_empty() {
            segment.to_owned()
        } else {
            format!("{}_{}", prefix, segment)
        }
    }

    fn child(&self, field: &str) -> Self {
        Self {
            old_style: Self::join(&self.old_style, &field.to_ascii_uppercase()),
            new_style: Self::join(&self.new_style, field),
        }
    }

    /// Compute the pair (new_name, old_name) for a net leaf
    fn net_names(&self, leaf: &str) -> (String, String) {
        if self.new_style.is_empty() {
            // No prefix
            (format!("_{}", leaf), leaf.to_ascii_uppercase())
        } else {
            // With prefix - always suffix the leaf
            (
                Self::join(&self.new_style, leaf),
                Self::join(&self.old_style, &leaf.to_ascii_uppercase()),
            )
        }
    }
}

/// Return the factory of an Interface instance (handles both frozen and unfrozen)
/// Recursively unregister all nets within an interface instance
fn unregister_interface_nets<'v>(interface: &InterfaceValue<'v>, ctx: &ContextValue) {
    for (_field_name, field_value) in interface.fields.iter() {
        if let Some(net_val) = field_value.downcast_ref::<NetValue<'v>>() {
            ctx.unregister_net(net_val.id());
        } else if let Some(nested_interface) = field_value.downcast_ref::<InterfaceValue<'v>>() {
            // Recursively unregister nets in nested interfaces
            unregister_interface_nets(nested_interface, ctx);
        }
    }
}

/// Clone a Net template with proper prefix application and name generation
fn clone_net_template<'v>(
    template: Value<'v>,
    prefix: &InstancePrefix,
    field_name_opt: Option<&str>,
    should_register: bool,
    heap: &'v Heap,
    eval: &mut Evaluator<'v, '_, '_>,
) -> starlark::Result<Value<'v>> {
    use crate::lang::net::{FrozenNetValue, NetValue};

    // Extract original name (None if auto-generated) and create new net with fresh ID
    let (template_name_opt, new_net_value) = if let Some(net_val) =
        template.downcast_ref::<NetValue<'v>>()
    {
        (
            net_val.original_name_opt().map(|s| s.to_owned()),
            net_val.with_new_id(heap, eval.call_stack_top_location()),
        )
    } else if let Some(frozen_net) = template.downcast_ref::<FrozenNetValue>() {
        (
            frozen_net.original_name_opt().map(|s| s.to_owned()),
            frozen_net.with_new_id(heap, eval.call_stack_top_location()),
        )
    } else {
        return Err(anyhow::anyhow!("Expected Net template, got {}", template.get_type()).into());
    };

    let net_name = compute_net_name(prefix, template_name_opt.as_deref(), field_name_opt, eval);
    let new_net = new_net_value.downcast_ref::<NetValue<'v>>().unwrap();

    // Register and get final name (or skip registration if should_register=false)
    let final_name = if should_register {
        eval.module()
            .extra_value()
            .and_then(|e| e.downcast_ref::<ContextValue>())
            .map(|ctx| ctx.register_net(new_net.id(), &net_name))
            .transpose()?
            .unwrap_or(net_name)
    } else {
        net_name
    };

    // Create new net preserving original_name from template for future cloning
    Ok(heap.alloc(NetValue {
        net_id: new_net.id(),
        name: final_name,
        original_name: new_net.original_name_opt().map(|s| s.to_owned()),
        type_name: new_net.type_name.clone(),
        properties: new_net.properties().clone(),
        span: new_net.span.clone(),
    }))
}

fn compute_net_name<'v>(
    prefix: &InstancePrefix,
    template_name: Option<&str>,
    field_name: Option<&str>,
    eval: &mut Evaluator<'v, '_, '_>,
) -> String {
    let leaf = template_name.or(field_name).unwrap_or("NET");
    let (new_name, old_name) = prefix.net_names(leaf);

    // Register moved directive if names differ
    if old_name != new_name {
        if let Some(ctx) = eval.context_value() {
            ctx.add_moved_directive(old_name, new_name.clone(), true);
        }
    }

    new_name
}

/// Clone an InterfaceValue template with a new prefix, recursively renaming nets.
/// Non-structural field values (primitives, enums, etc.) are reused directly.
fn clone_interface_template<'v>(
    instance: Value<'v>,
    prefix: &InstancePrefix,
    should_register: bool,
    heap: &'v Heap,
    eval: &mut Evaluator<'v, '_, '_>,
) -> starlark::Result<Value<'v>> {
    // Helper to clone a field value based on its type
    fn clone_field<'v>(
        name: &str,
        val: Value<'v>,
        prefix: &InstancePrefix,
        should_register: bool,
        heap: &'v Heap,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        match val.get_type() {
            "Net" => clone_net_template(val, prefix, Some(name), should_register, heap, eval),
            "InterfaceValue" => {
                clone_interface_template(val, &prefix.child(name), should_register, heap, eval)
            }
            _ => Ok(val),
        }
    }

    // Extract factory and clone fields based on instance type
    let (factory_val, fields) = if let Some(iv) = instance.downcast_ref::<InterfaceValue<'v>>() {
        let mut cloned = SmallMap::new();
        for (name, value) in iv.fields.iter() {
            cloned.insert(
                name.clone(),
                clone_field(name, value.to_value(), prefix, should_register, heap, eval)?,
            );
        }
        (iv.factory().to_value(), cloned)
    } else if let Some(fiv) = instance.downcast_ref::<FrozenInterfaceValue>() {
        let mut cloned = SmallMap::new();
        for (name, value) in fiv.fields.iter() {
            cloned.insert(
                name.clone(),
                clone_field(name, value.to_value(), prefix, should_register, heap, eval)?,
            );
        }
        (fiv.factory().to_value(), cloned)
    } else {
        return Err(anyhow::anyhow!("expected InterfaceValue, got {}", instance.get_type()).into());
    };

    // Create new InterfaceValue with cloned fields
    Ok(heap.alloc(InterfaceValue {
        fields,
        factory: factory_val,
    }))
}

/// Create a single field value from a spec, handling all field types uniformly
fn create_field_value<'v>(
    field_name: &str,
    field_spec: Value<'v>,
    provided_value: Option<Value<'v>>,
    prefix: &InstancePrefix,
    should_register: bool,
    heap: &'v Heap,
    eval: &mut Evaluator<'v, '_, '_>,
) -> starlark::Result<Value<'v>> {
    if let Some(value) = validate_field(field_name, field_spec, provided_value, heap)? {
        return Ok(value);
    }

    // Handle different field types
    let child_prefix = prefix.child(field_name);
    if field_spec.get_type() == "InterfaceValue" {
        // Clone the interface template with new prefix, reusing non-net values
        clone_interface_template(field_spec, &child_prefix, should_register, heap, eval)
    } else if field_spec.get_type() == "Net" {
        // For Net templates, use clone_net_template directly with field name
        clone_net_template(
            field_spec,
            prefix,
            Some(field_name),
            should_register,
            heap,
            eval,
        )
    } else if field_spec.get_type() == "NetType" {
        // Invoke the NetType constructor to apply defaults and extract metadata
        let new_name = compute_net_name(prefix, None, Some(field_name), eval);
        let args = vec![heap.alloc(new_name)];
        if should_register {
            // Normal case - omit __register kwarg (defaults to true)
            eval.eval_function(field_spec, &args, &[])
        } else {
            // Metadata-only - explicitly pass __register=false
            let kwargs = vec![("__register", heap.alloc(false))];
            eval.eval_function(field_spec, &args, &kwargs)
        }
    } else {
        // For InterfaceFactory, delegate to instantiate_interface
        instantiate_interface(field_spec, &child_prefix, should_register, heap, eval)
    }
}

/// Core function to create an interface instance from a factory
fn create_interface_instance<'v, V>(
    factory: &InterfaceFactoryGen<V>,
    factory_value: Value<'v>,
    provided_values: SmallMap<String, Value<'v>>,
    prefix: &InstancePrefix,
    should_register: bool,
    heap: &'v Heap,
    eval: &mut Evaluator<'v, '_, '_>,
) -> starlark::Result<Value<'v>>
where
    V: ValueLike<'v> + InterfaceCell,
{
    // Build the field map, recursively creating values where necessary
    let mut fields = SmallMap::with_capacity(factory.fields.len());

    for (field_name, field_spec) in factory.fields.iter() {
        let field_value = create_field_value(
            field_name,
            field_spec.to_value(),
            provided_values.get(field_name).copied(),
            prefix,
            should_register,
            heap,
            eval,
        )?;

        fields.insert(field_name.clone(), field_value);
    }

    // Create the interface instance
    let interface_instance = heap.alloc(InterfaceValue {
        fields,
        factory: factory_value,
    });

    // Execute __post_init__ if present
    if let Some(post_init_fn) = factory.post_init_fn.as_ref() {
        let post_init_val = post_init_fn.to_value();
        if !post_init_val.is_none() {
            eval.eval_function(post_init_val, &[interface_instance], &[])?;
        }
    }

    Ok(interface_instance)
}

/// Build a consistent parameter spec for interface factories, excluding reserved field names
fn build_interface_param_spec<'v, V: ValueLike<'v>>(
    fields: &SmallMap<String, V>,
) -> ParametersSpec<FrozenValue> {
    ParametersSpec::new_parts(
        "InterfaceInstance",
        std::iter::empty::<(&str, ParametersSpecParam<_>)>(),
        [("name", ParametersSpecParam::Optional)],
        false,
        fields
            .iter()
            .filter(|(k, _)| k.as_str() != "name") // Exclude reserved "name" field
            .map(|(k, _)| (k.as_str(), ParametersSpecParam::Optional)),
        false,
    )
}

// Interface type data, similar to TyRecordData
#[derive(Debug, Allocative)]
pub struct InterfaceTypeData {
    /// Name of the interface type.
    name: String,
    /// Globally unique id of the interface type.
    id: TypeInstanceId,
    /// Creating these on every invoke is pretty expensive (profiling shows)
    /// so compute them in advance and cache.
    parameter_spec: ParametersSpec<FrozenValue>,
}

// Trait to handle the difference between mutable and frozen values
pub trait InterfaceCell: starlark::values::ValueLifetimeless {
    type InterfaceTypeDataOpt: std::fmt::Debug;

    fn get_or_init_ty(
        ty: &Self::InterfaceTypeDataOpt,
        f: impl FnOnce() -> starlark::Result<Arc<InterfaceTypeData>>,
    ) -> starlark::Result<()>;
    fn get_ty(ty: &Self::InterfaceTypeDataOpt) -> Option<&Arc<InterfaceTypeData>>;
}

impl InterfaceCell for Value<'_> {
    type InterfaceTypeDataOpt = OnceCell<Arc<InterfaceTypeData>>;

    fn get_or_init_ty(
        ty: &Self::InterfaceTypeDataOpt,
        f: impl FnOnce() -> starlark::Result<Arc<InterfaceTypeData>>,
    ) -> starlark::Result<()> {
        ty.get_or_try_init(f)?;
        Ok(())
    }

    fn get_ty(ty: &Self::InterfaceTypeDataOpt) -> Option<&Arc<InterfaceTypeData>> {
        ty.get()
    }
}

impl InterfaceCell for FrozenValue {
    type InterfaceTypeDataOpt = Option<Arc<InterfaceTypeData>>;

    fn get_or_init_ty(
        ty: &Self::InterfaceTypeDataOpt,
        f: impl FnOnce() -> starlark::Result<Arc<InterfaceTypeData>>,
    ) -> starlark::Result<()> {
        let _ignore = (ty, f);
        Ok(())
    }

    fn get_ty(ty: &Self::InterfaceTypeDataOpt) -> Option<&Arc<InterfaceTypeData>> {
        ty.as_ref()
    }
}

#[derive(Clone, Debug, Trace, Coerce, ProvidesStaticType, NoSerialize, Allocative)]
#[repr(C)]
pub struct InterfaceFactoryGen<V: InterfaceCell> {
    id: TypeInstanceId,
    #[allocative(skip)]
    #[trace(unsafe_ignore)]
    interface_type_data: V::InterfaceTypeDataOpt,
    fields: SmallMap<String, V>,
    post_init_fn: Option<V>,
    param_spec: ParametersSpec<FrozenValue>,
}

starlark_complex_value!(pub InterfaceFactory);

impl Freeze for InterfaceFactory<'_> {
    type Frozen = FrozenInterfaceFactory;
    fn freeze(
        self,
        freezer: &starlark::values::Freezer,
    ) -> starlark::values::FreezeResult<Self::Frozen> {
        Ok(FrozenInterfaceFactory {
            id: self.id,
            interface_type_data: self.interface_type_data.into_inner(),
            fields: self.fields.freeze(freezer)?,
            post_init_fn: self.post_init_fn.freeze(freezer)?,
            param_spec: self.param_spec,
        })
    }
}

#[starlark_value(type = "InterfaceFactory")]
impl<'v, V: ValueLike<'v> + InterfaceCell + 'v> StarlarkValue<'v> for InterfaceFactoryGen<V>
where
    Self: ProvidesStaticType<'v>,
{
    type Canonical = FrozenInterfaceFactory;

    fn invoke(
        &self,
        _me: Value<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        // Collect provided `name` (optional) and field values using the cached parameter spec.
        let mut provided_values = SmallMap::with_capacity(self.fields.len());
        let mut instance_name_opt: Option<String> = None;

        self.param_spec.parser(args, eval, |param_parser, _extra| {
            // First optional positional/named `name` parameter.
            if let Some(name_val) = param_parser.next_opt::<Value<'v>>()? {
                let name_str = name_val.unpack_str().ok_or_else(|| {
                    starlark::Error::new_other(anyhow::anyhow!("Interface name must be a string"))
                })?;

                // Validate the interface instance name
                validate_identifier_name(name_str, "Interface name")?;

                instance_name_opt = Some(name_str.to_owned());
            }

            // Then the field values in the order of `fields`.
            for (fld_name, _) in self.fields.iter() {
                if let Some(v) = param_parser.next_opt()? {
                    provided_values.insert(fld_name.clone(), v);
                }
            }
            Ok(())
        })?;

        // Delegate to the unified creation function
        let prefix = if let Some(name) = instance_name_opt {
            InstancePrefix::from_root(&name)
        } else {
            InstancePrefix::empty()
        };
        // Normal instantiation - always register nets
        create_interface_instance(self, _me, provided_values, &prefix, true, eval.heap(), eval)
    }

    fn eval_type(&self) -> Option<starlark::typing::Ty> {
        // An instance created by this factory evaluates to `InterfaceValue`,
        // so expose that as the type annotation for static/runtime checks.
        // This mirrors how `NetType` maps to `NetValue`.
        Some(<InterfaceValue as StarlarkValue>::get_type_starlark_repr())
    }

    fn export_as(
        &self,
        variable_name: &str,
        _eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<()> {
        V::get_or_init_ty(&self.interface_type_data, || {
            Ok(Arc::new(InterfaceTypeData {
                name: variable_name.to_owned(),
                id: self.id,
                parameter_spec: build_interface_param_spec(&self.fields),
            }))
        })
    }

    fn dir_attr(&self) -> Vec<String> {
        self.fields.iter().map(|(k, _)| k.clone()).collect()
    }
}

impl<'v, V: ValueLike<'v> + InterfaceCell> std::fmt::Display for InterfaceFactoryGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // If we have a name from export_as, use it
        if let Some(type_data) = V::get_ty(&self.interface_type_data) {
            write!(f, "{}", type_data.name)
        } else {
            // Otherwise show the structure
            write!(f, "interface(")?;
            for (i, (name, value)) in self.fields.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                // Show the type of the field value, with special handling for interfaces
                let val = value.to_value();
                let type_str = if val.downcast_ref::<InterfaceFactory<'v>>().is_some()
                    || val.downcast_ref::<FrozenInterfaceFactory>().is_some()
                {
                    // For nested interfaces, show their full signature
                    val.to_string()
                } else {
                    // For other types, just show the type name
                    val.get_type().to_string()
                };
                write!(f, "{name}: {type_str}")?;
            }
            write!(f, ")")
        }
    }
}

impl<'v, V: ValueLike<'v> + InterfaceCell> InterfaceFactoryGen<V> {
    pub fn iter(&self) -> impl Iterator<Item = (&str, &V)> {
        self.fields.iter().map(|(k, v)| (k.as_str(), v))
    }
}

#[derive(Clone, Debug, Trace, Coerce, ProvidesStaticType, Allocative, Freeze, serde::Serialize)]
#[repr(C)]
#[serde(bound = "V: serde::Serialize")]
pub struct InterfaceValueGen<V> {
    fields: SmallMap<String, V>,
    #[serde(skip)]
    factory: V, // Runtime only - factory has NoSerialize so can't be JSON-serialized
}

starlark_complex_value!(pub InterfaceValue);

#[starlark_value(type = "InterfaceValue")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for InterfaceValueGen<V>
where
    Self: ProvidesStaticType<'v>,
{
    type Canonical = FrozenInterfaceValue;

    fn get_attr(&self, attr: &str, _heap: &'v Heap) -> Option<Value<'v>> {
        self.fields.get(attr).map(|v| v.to_value())
    }

    fn dir_attr(&self) -> Vec<String> {
        self.fields.keys().cloned().collect()
    }
}

impl<'v, V: ValueLike<'v>> std::fmt::Display for InterfaceValueGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut items: Vec<_> = self.fields.iter().collect();
        items.sort_by_key(|(k, _)| *k);

        let name = if let Some(factory) = self.factory.downcast_ref::<InterfaceFactory>() {
            factory
                .interface_type_data
                .get()
                .map(|type_data| type_data.name.clone())
        } else if let Some(factory) = self.factory.downcast_ref::<FrozenInterfaceFactory>() {
            factory
                .interface_type_data
                .as_ref()
                .map(|type_data| type_data.name.clone())
        } else {
            None
        };
        let type_name = name.unwrap_or_else(|| "<Unknown>".to_string());

        write!(f, "{type_name}(")?;
        for (i, (k, v)) in items.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            let value = v.to_value();
            write!(f, "{k}={value}")?;
        }
        write!(f, ")")
    }
}

impl<'v, V: ValueLike<'v>> InterfaceValueGen<V> {
    // Provide read-only access to the underlying fields map so other modules
    // (e.g. the schematic generator) can traverse the interface hierarchy
    // without relying on private internals.
    #[inline]
    pub fn fields(&self) -> &SmallMap<String, V> {
        &self.fields
    }

    // Provide read-only access to the factory for serialization purposes
    #[inline]
    pub fn factory(&self) -> &V {
        &self.factory
    }
}

#[starlark_module]
pub(crate) fn interface_globals(builder: &mut GlobalsBuilder) {
    fn using<'v>(value: Value<'v>, _eval: &mut Evaluator<'v, '_, '_>) -> anyhow::Result<Value<'v>> {
        // Passthrough using() for backwards compatibility
        Ok(value)
    }

    fn interface<'v>(
        #[starlark(kwargs)] kwargs: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let heap = eval.heap();
        let mut fields = SmallMap::new();
        let mut post_init_fn = None;

        // Process field specifications and validate reserved names
        for (name, v) in &kwargs {
            if name == "__post_init__" {
                // Handle __post_init__ as direct function assignment
                post_init_fn = Some(v.to_value());
            } else if name == "name" {
                // Reject "name" as field name to avoid conflict with implicit parameter
                return Err(anyhow::anyhow!(
                    "Field name 'name' is reserved (conflicts with implicit name parameter)"
                ));
            } else {
                // Extract field value
                let field_value = v.to_value();
                let type_str = field_value.get_type();

                // Accept Net type, Net instance, Interface factory, Interface instance, or field() specs
                if type_str == "NetType"
                    || type_str == "Net"
                    || type_str == "InterfaceValue"
                    || type_str == "field"
                    || field_value.downcast_ref::<InterfaceFactory<'v>>().is_some()
                    || field_value
                        .downcast_ref::<FrozenInterfaceFactory>()
                        .is_some()
                {
                    // If a Net instance literal was provided as a template field,
                    // unregister it from the current module so it does not count as
                    // an introduced net of this module. It will be (re)registered
                    // when an interface instance is created.
                    if type_str == "Net" {
                        if let Some(net_val) = field_value.downcast_ref::<NetValue<'v>>() {
                            if let Some(ctx) = eval
                                .module()
                                .extra_value()
                                .and_then(|e| e.downcast_ref::<ContextValue>())
                            {
                                ctx.unregister_net(net_val.id());
                            }
                        }
                    } else if type_str == "InterfaceValue" {
                        // If an Interface instance was provided as a template field,
                        // recursively unregister all nets inside it
                        if let Some(interface_val) =
                            field_value.downcast_ref::<InterfaceValue<'v>>()
                        {
                            if let Some(ctx) = eval
                                .module()
                                .extra_value()
                                .and_then(|e| e.downcast_ref::<ContextValue>())
                            {
                                unregister_interface_nets(interface_val, ctx);
                            }
                        }
                    }
                    fields.insert(name.clone(), field_value);
                } else {
                    return Err(anyhow::anyhow!(
                        "Interface field `{}` must be Net type, Net instance, Interface type, Interface instance, or  field() specification, got `{}`",
                        name,
                        type_str
                    ));
                }
            }
        }

        // Build parameter spec: optional first positional/named `name`, then
        // all interface fields as optional named‑only parameters.
        let param_spec = build_interface_param_spec(&fields);

        let factory = heap.alloc(InterfaceFactory {
            id: TypeInstanceId::r#gen(),
            interface_type_data: OnceCell::new(),
            fields,
            post_init_fn,
            param_spec,
        });

        // TODO: Add validation to ensure interfaces are assigned to variables
        // For now, anonymous interfaces will be caught when first used

        Ok(factory)
    }
}

/// Helper function to instantiate an interface spec recursively
/// This is a simplified dispatcher that delegates to the appropriate creation function
pub(crate) fn instantiate_interface<'v>(
    spec: Value<'v>,
    prefix: &InstancePrefix,
    should_register: bool,
    heap: &'v Heap,
    eval: &mut Evaluator<'v, '_, '_>,
) -> starlark::Result<Value<'v>> {
    // Handle interface factories first
    if let Some(factory) = spec.downcast_ref::<InterfaceFactory<'v>>() {
        return create_interface_instance(
            factory,
            spec,
            SmallMap::new(),
            prefix,
            should_register,
            heap,
            eval,
        );
    }
    if let Some(factory) = spec.downcast_ref::<FrozenInterfaceFactory>() {
        return create_interface_instance(
            factory,
            spec,
            SmallMap::new(),
            prefix,
            should_register,
            heap,
            eval,
        );
    }

    // This path should never be hit - all InterfaceValue instances should have
    // been converted to factories above (lines 1007-1011)
    Err(anyhow::anyhow!(
        "internal error: unexpected value type in instantiate_interface: {} (expected InterfaceFactory)",
        spec.get_type()
    ).into())
}

impl<'v, V: ValueLike<'v> + InterfaceCell> InterfaceFactoryGen<V> {
    /// Return the map of field specifications (field name -> type value) that
    /// define this interface. This is primarily used by the input
    /// deserialization logic to determine the expected type for nested
    /// interface fields when reconstructing an instance from a serialised
    /// `InputValue`.
    #[inline]
    pub fn fields(&self) -> &SmallMap<String, V> {
        &self.fields
    }

    #[inline]
    pub fn field(&self, name: &str) -> Option<&V> {
        self.fields.get(name)
    }
}

#[cfg(test)]
mod tests {
    use starlark::assert::Assert;
    use starlark::environment::GlobalsBuilder;

    use crate::lang::component::{component_globals, init_net_global};
    use crate::lang::interface::interface_globals;

    #[test]
    fn interface_type_matches_instance() {
        let mut a = Assert::new();
        // Extend the default globals with the language constructs we need.
        a.globals_add(|builder: &mut GlobalsBuilder| {
            component_globals(builder);
            init_net_global(builder);
            interface_globals(builder);
        });

        // `eval_type(Power)` should match an instance returned by `Power()`.
        a.is_true(
            r#"
Power = interface(vcc = Net)
instance = Power()

eval_type(Power).matches(instance)
"#,
        );
    }

    #[test]
    fn interface_name_captured() {
        let mut a = Assert::new();
        a.globals_add(|builder: &mut GlobalsBuilder| {
            component_globals(builder);
            init_net_global(builder);
            interface_globals(builder);
        });

        // When assigned to a global, the interface should display its name
        a.pass(
            r#"
Power = interface(vcc = Net, gnd = Net)
assert_eq(str(Power), "Power")
"#,
        );
    }

    #[test]
    fn interface_dir_attr() {
        let mut a = Assert::new();
        a.globals_add(|builder: &mut GlobalsBuilder| {
            component_globals(builder);
            init_net_global(builder);
            interface_globals(builder);
        });

        // Test dir() on interface type
        a.pass(
            r#"
Power = interface(vcc = Net, gnd = Net)
attrs = dir(Power)
assert_eq(sorted(attrs), ["gnd", "vcc"])
"#,
        );

        // Test dir() on interface instance
        a.pass(
            r#"
Power = interface(vcc = Net, gnd = Net)
power_instance = Power()
attrs = dir(power_instance)
assert_eq(sorted(attrs), ["gnd", "vcc"])
"#,
        );

        // Test dir() on nested interface
        a.pass(
            r#"
Power = interface(vcc = Net, gnd = Net)
System = interface(power = Power, data = Net)
system_instance = System()
assert_eq(sorted(dir(System)), ["data", "power"])
assert_eq(sorted(dir(system_instance)), ["data", "power"])
assert_eq(sorted(dir(system_instance.power)), ["gnd", "vcc"])
"#,
        );
    }

    #[test]
    fn interface_net_naming_behavior() {
        let mut a = Assert::new();
        a.globals_add(|builder: &mut GlobalsBuilder| {
            component_globals(builder);
            init_net_global(builder);
            interface_globals(builder);
        });

        // Test 1: Net type should auto-generate name
        a.pass(
            r#"
Power1 = interface(vcc = Net)
instance1 = Power1()
assert_eq(instance1.vcc.name, "_vcc")
"#,
        );

        // Test 2: Net with explicit name should use that name
        a.pass(
            r#"
Power2 = interface(vcc = Net("MY_VCC"))
instance2 = Power2()
assert_eq(instance2.vcc.name, "_MY_VCC")
"#,
        );

        // Test 3: Net() with no name should generate a name (same as Net type)
        a.pass(
            r#"
Power3 = interface(vcc = Net())
instance3 = Power3()
# We want Net() to behave the same as Net type
assert_eq(instance3.vcc.name, "_vcc")
"#,
        );

        // Test 4: With instance name prefix - always includes field name
        a.pass(
            r#"
Power4 = interface(vcc = Net)
instance4 = Power4("PWR")
assert_eq(instance4.vcc.name, "PWR_vcc")
"#,
        );

        // Test 5: Net() with instance name prefix should also generate a name
        a.pass(
            r#"
Power5 = interface(vcc = Net())
instance5 = Power5("PWR")
# Net() behaves the same as Net type with prefix
assert_eq(instance5.vcc.name, "PWR_vcc")
"#,
        );
    }
}
