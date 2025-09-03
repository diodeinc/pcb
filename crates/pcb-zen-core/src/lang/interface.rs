use allocative::Allocative;
use once_cell::unsync::OnceCell;
use starlark::collections::SmallMap;
use starlark::environment::GlobalsBuilder;
use starlark::eval::{Arguments, Evaluator, ParametersSpec, ParametersSpecParam};
use starlark::starlark_complex_value;
use starlark::starlark_module;
use starlark::values::types::record::field::FieldGen;
use starlark::values::typing::TypeInstanceId;
use starlark::values::{
    starlark_value, Coerce, Freeze, FreezeResult, Heap, NoSerialize, ProvidesStaticType,
    StarlarkValue, Trace, Value, ValueLike,
};

use std::sync::Arc;

use crate::lang::context::ContextValue;
use crate::lang::eval::{copy_value, DeepCopyToHeap};
use crate::lang::interface_validation::ensure_field_compat;
use crate::lang::net::{generate_net_id, NetValue};

/// Get promotion key for any value
pub fn get_promotion_key(value: Value) -> anyhow::Result<String> {
    let value_type = value.get_type();
    if value_type == "NetType" || value_type == "Net" {
        Ok("Net".to_string())
    } else if value_type == "InterfaceValue" {
        let factory_val = if let Some(interface_val) = value.downcast_ref::<InterfaceValue>() {
            interface_val.factory().to_value()
        } else if let Some(frozen_interface_val) = value.downcast_ref::<FrozenInterfaceValue>() {
            frozen_interface_val.factory().to_value()
        } else {
            value // fallback
        };
        get_promotion_key(factory_val)
    } else {
        // Check for interface factories
        if let Some(factory) = value.downcast_ref::<InterfaceFactory>() {
            if let Some(type_data) = <Value as InterfaceCell>::get_ty(&factory.interface_type_data)
            {
                Ok(type_data.name.clone())
            } else {
                Err(anyhow::anyhow!(
                        "Anonymous interfaces are not allowed. All interfaces must be assigned to a variable (e.g., MyInterface = interface(...))"
                    ))
            }
        } else if let Some(frozen_factory) = value.downcast_ref::<FrozenInterfaceFactory>() {
            if let Some(type_data) = &frozen_factory.interface_type_data {
                Ok(type_data.name.clone())
            } else {
                Err(anyhow::anyhow!(
                        "Anonymous interfaces are not allowed. All interfaces must be assigned to a variable (e.g., MyInterface = interface(...))"
                    ))
            }
        } else {
            Ok(value.get_type().to_string())
        }
    }
}

/// Get promotion map for any value - handles both factories and instances
pub fn get_promotion_map(value: Value) -> SmallMap<String, String> {
    if let Some(factory) = value.downcast_ref::<InterfaceFactory>() {
        // Use the initialized type data if available (includes self-promotion)
        if let Some(type_data) = <Value as InterfaceCell>::get_ty(&factory.interface_type_data) {
            type_data.promotion_by_type.clone()
        } else {
            factory.promotion_by_type.clone()
        }
    } else if let Some(frozen_factory) = value.downcast_ref::<FrozenInterfaceFactory>() {
        // Use the type data if available (includes self-promotion)
        if let Some(type_data) = &frozen_factory.interface_type_data {
            type_data.promotion_by_type.clone()
        } else {
            frozen_factory.promotion_by_type.clone()
        }
    } else if value.get_type() == "InterfaceValue" {
        // Interface instance - get promotion map from its factory
        let factory_val = if let Some(interface_val) = value.downcast_ref::<InterfaceValue>() {
            interface_val.factory().to_value()
        } else if let Some(frozen_interface_val) = value.downcast_ref::<FrozenInterfaceValue>() {
            frozen_interface_val.factory().to_value()
        } else {
            value
        };
        get_promotion_map(factory_val)
    } else {
        SmallMap::new()
    }
}

/// Unwrap using() wrapper if present
pub fn unwrap_using(value: Value) -> Value {
    if let Some(using_val) = value.downcast_ref::<Using>() {
        using_val.value.to_value()
    } else {
        value
    }
}

/// Return the factory of an Interface instance (handles both frozen and unfrozen)
fn interface_instance_factory<'v>(value: Value<'v>, _heap: &'v Heap) -> anyhow::Result<Value<'v>> {
    if let Some(interface_val) = value.downcast_ref::<InterfaceValue>() {
        Ok(interface_val.factory().to_value())
    } else if let Some(frozen_interface_val) = value.downcast_ref::<FrozenInterfaceValue>() {
        Ok(frozen_interface_val.factory().to_value())
    } else {
        Err(anyhow::anyhow!(
            "expected InterfaceValue, got {}",
            value.get_type()
        ))
    }
}

/// Clone a Net template with proper prefix application and name generation
fn clone_net_template<'v>(
    template: Value<'v>,
    prefix_opt: Option<&str>,
    field_name_opt: Option<&str>,
    heap: &'v Heap,
    eval: &mut Evaluator<'v, '_, '_>,
) -> anyhow::Result<Value<'v>> {
    // Extract template properties
    let (template_name, template_props, template_symbol) =
        if let Some(net_val) = template.downcast_ref::<NetValue<'v>>() {
            (
                net_val.original_name().to_string(),
                net_val.properties().clone(),
                net_val.symbol().to_value(),
            )
        } else {
            // Handle frozen net by copying first
            let copied_template = copy_value(template, heap)?;
            if let Some(net_val) = copied_template.downcast_ref::<NetValue<'v>>() {
                (
                    net_val.original_name().to_string(),
                    net_val.properties().clone(),
                    net_val.symbol().to_value(),
                )
            } else {
                return Err(anyhow::anyhow!(
                    "Failed to extract properties from net template"
                ));
            }
        };

    // Apply prefix to template name
    let is_placeholder =
        template_name.starts_with('N') && template_name[1..].chars().all(|c| c.is_ascii_digit());
    let net_name = if !template_name.is_empty() && !is_placeholder {
        prefix_opt
            .map(|p| format!("{p}_{template_name}"))
            .unwrap_or(template_name)
    } else {
        // For placeholder nets, use field name if available, otherwise fallback
        match (prefix_opt, field_name_opt) {
            (Some(p), Some(f)) => format!("{}_{}", p, f.to_ascii_uppercase()),
            (Some(p), None) => p.to_string(),
            (None, Some(f)) => f.to_ascii_uppercase(),
            (None, None) => "NET".to_string(),
        }
    };

    let net_id = generate_net_id();
    let final_name = if let Some(ctx) = eval
        .module()
        .extra_value()
        .and_then(|e| e.downcast_ref::<ContextValue>())
    {
        ctx.register_net(net_id, &net_name)?
    } else {
        net_name
    };

    // Copy properties and symbol
    let mut new_props = SmallMap::new();
    for (k, v) in &template_props {
        new_props.insert(k.clone(), copy_value(v.to_value(), heap)?);
    }
    let copied_symbol = copy_value(template_symbol, heap)?;

    Ok(heap.alloc(NetValue::new(net_id, final_name, new_props, copied_symbol)))
}

/// Generic helper to instantiate from any interface factory (frozen or unfrozen)
fn instantiate_from_factory<'v, V>(
    factory: &InterfaceFactoryGen<V>,
    factory_value: Value<'v>,
    prefix_opt: Option<&str>,
    heap: &'v Heap,
    eval: &mut Evaluator<'v, '_, '_>,
) -> anyhow::Result<Value<'v>>
where
    V: ValueLike<'v> + InterfaceCell,
{
    // Build the field map, recursively creating values where necessary.
    let mut fields = SmallMap::with_capacity(factory.fields.len());

    for (field_name, field_spec) in factory.fields.iter() {
        let spec_value = field_spec.to_value();
        let spec_type = spec_value.get_type();

        let field_value: Value<'v> = if spec_type == "field" {
            // Handle field() specifications - extract default value
            if let Some(field_obj) = spec_value.downcast_ref::<FieldGen<Value<'v>>>() {
                field_obj.default().unwrap().to_value()
            } else if let Some(field_obj) =
                spec_value.downcast_ref::<FieldGen<starlark::values::FrozenValue>>()
            {
                field_obj.default().unwrap().to_value()
            } else {
                return Err(anyhow::anyhow!("Invalid field specification"));
            }
        } else if spec_type == "NetType" {
            // For backwards compatibility: Net type becomes an empty net
            let net_name = if let Some(p) = prefix_opt {
                format!("{}_{}", p, field_name.to_ascii_uppercase())
            } else {
                field_name.to_ascii_uppercase()
            };

            let net_id = generate_net_id();
            let final_name = if let Some(ctx) = eval
                .module()
                .extra_value()
                .and_then(|e| e.downcast_ref::<ContextValue>())
            {
                ctx.register_net(net_id, &net_name)?
            } else {
                net_name.clone()
            };
            heap.alloc(NetValue::new(
                net_id,
                final_name,
                SmallMap::new(),
                Value::new_none(),
            ))
        } else {
            // Build extended prefix for nested interfaces
            let next_prefix = match prefix_opt {
                Some(p) => format!("{}_{}", p, field_name.to_ascii_uppercase()),
                None => field_name.to_ascii_uppercase(),
            };

            // For interface factories, use extended prefix; for other types use original prefix
            let prefix_to_use = if spec_value.downcast_ref::<InterfaceFactory>().is_some()
                || spec_value
                    .downcast_ref::<FrozenInterfaceFactory>()
                    .is_some()
            {
                Some(next_prefix.as_str())
            } else {
                prefix_opt
            };

            instantiate_interface(spec_value, prefix_to_use, heap, eval)?
        };

        fields.insert(field_name.clone(), field_value);
    }

    // Create the interface instance with the original factory value
    let interface_instance = heap.alloc(InterfaceValue {
        fields,
        factory: factory_value,
    });

    // Execute __post_init__ if present
    if let Some(post_init_fn) = factory.post_init_fn.as_ref() {
        let post_init_val = post_init_fn.to_value();
        if !post_init_val.is_none() {
            eval.eval_function(post_init_val, &[interface_instance], &[])
                .map_err(|e| anyhow::anyhow!(e))?;
        }
    }

    Ok(interface_instance)
}

/// Recursively collect promotion paths that originate from *using()* fields.
///
/// `current_path` is the dot-qualified path that reaches `field_value` from the
/// parent interface ("uart", "spi.cs", …).
fn discover_promotion_paths<'v>(
    _field_name: &str,
    field_value: Value<'v>,
    current_path: &str,
) -> anyhow::Result<SmallMap<String, String>> {
    let mut paths = SmallMap::new();

    // Only proceed if the field itself is wrapped in using().
    let Some(using_val) = field_value.downcast_ref::<Using<'v>>() else {
        return Ok(paths); // regular field ⇒ no promotion
    };

    let inner = using_val.value.to_value();
    let type_name = get_promotion_key(inner)?;
    paths.insert(type_name, current_path.to_owned());

    // Add transitive promotion paths if inner value has them
    let nested_map = get_promotion_map(inner);
    for (nested_ty, nested_path) in &nested_map {
        let full_path = if nested_path.is_empty() {
            current_path.to_owned()
        } else {
            format!("{current_path}.{nested_path}")
        };
        paths.insert(nested_ty.clone(), full_path);
    }

    Ok(paths)
}

/// Wrapper for using() specifications that marks fields for promotion
#[derive(Clone, Debug, Trace, Coerce, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub struct UsingGen<V> {
    value: V,
}

starlark_complex_value!(pub Using);

#[starlark_value(type = "using")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for UsingGen<V>
where
    Self: ProvidesStaticType<'v>,
{
    type Canonical = FrozenUsing;

    fn provide(&'v self, demand: &mut starlark::values::Demand<'_, 'v>) {
        demand.provide_value::<&dyn DeepCopyToHeap>(self);
    }
}

impl<'v, V: ValueLike<'v>> std::fmt::Display for UsingGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "using({})", self.value.to_value())
    }
}

impl<'v, V: ValueLike<'v>> DeepCopyToHeap for UsingGen<V> {
    fn deep_copy_to<'dst>(&self, dst: &'dst Heap) -> anyhow::Result<Value<'dst>> {
        let copied_value = copy_value(self.value.to_value(), dst)?;
        Ok(dst.alloc(Using {
            value: copied_value,
        }))
    }
}

/// Build a consistent parameter spec for interface factories, excluding reserved field names
fn build_interface_param_spec<'v, V: ValueLike<'v>>(
    fields: &SmallMap<String, V>,
) -> ParametersSpec<starlark::values::FrozenValue> {
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
    parameter_spec: ParametersSpec<starlark::values::FrozenValue>,
    /// Track which fields are marked with using() for promotion, by type name
    promotion_by_type: SmallMap<String, String>,
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

impl InterfaceCell for starlark::values::FrozenValue {
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
    param_spec: ParametersSpec<starlark::values::FrozenValue>,
    promotion_by_type: SmallMap<String, String>,
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
            promotion_by_type: self.promotion_by_type,
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
        let heap = eval.heap();

        // Collect provided `name` (optional) and field values using the cached parameter spec.
        let mut provided_values: SmallMap<String, Value<'v>> =
            SmallMap::with_capacity(self.fields.len());
        let mut instance_name_opt: Option<String> = None;

        self.param_spec.parser(args, eval, |param_parser, _extra| {
            // First optional positional/named `name` parameter.
            if let Some(name_val) = param_parser.next_opt::<Value<'v>>()? {
                let name_str = name_val.unpack_str().ok_or_else(|| {
                    starlark::Error::new_other(anyhow::anyhow!("Interface name must be a string"))
                })?;
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

        // Then create the fields map with auto-created values where needed
        let mut fields = SmallMap::with_capacity(self.fields.len());

        // Helper closure to build a prefix string ("PARENT_FIELD") if instance name provided.
        let make_prefix = |parent: &str, field: &str| -> String {
            format!("{}_{}", parent, field.to_ascii_uppercase())
        };

        for (name, field_spec) in self.fields.iter() {
            let field_value: Value<'v> = if let Some(v) = provided_values.get(name) {
                // Value supplied by the caller - validate it matches the expected field type
                ensure_field_compat(field_spec.to_value(), *v, name)
                    .map_err(starlark::Error::new_other)?;
                v.to_value()
            } else {
                // Use the field spec to create a value
                let spec_value = field_spec.to_value();
                let spec_type = spec_value.get_type();

                if spec_type == "field" {
                    // Handle field() specifications - extract default value
                    let field_obj = spec_value.downcast_ref::<FieldGen<Value<'v>>>().unwrap();
                    field_obj.default().unwrap().to_value()
                } else if spec_type == "NetType" {
                    // Auto-generate fresh Net from Net type
                    let net_name = instance_name_opt
                        .as_ref()
                        .map(|p| make_prefix(p, name))
                        .unwrap_or_else(|| name.to_ascii_uppercase());
                    let net_id = generate_net_id();
                    let final_name = if let Some(ctx) = eval
                        .module()
                        .extra_value()
                        .and_then(|e| e.downcast_ref::<ContextValue>())
                    {
                        ctx.register_net(net_id, &net_name).map_err(|e| {
                            starlark::Error::new_other(anyhow::anyhow!(e.to_string()))
                        })?
                    } else {
                        net_name.clone()
                    };
                    heap.alloc(NetValue::new(
                        net_id,
                        final_name,
                        SmallMap::new(),
                        Value::new_none(),
                    ))
                } else if spec_type == "Net" {
                    // Clone Net template with naming rules using shared helper
                    clone_net_template(
                        spec_value,
                        instance_name_opt.as_deref(),
                        Some(name),
                        heap,
                        eval,
                    )
                    .map_err(starlark::Error::new_other)?
                } else if spec_type == "InterfaceValue" {
                    // Interface instance - extract factory and instantiate
                    let factory_val = interface_instance_factory(spec_value, heap)?;
                    instantiate_interface(
                        factory_val,
                        instance_name_opt
                            .as_ref()
                            .map(|p| make_prefix(p, name))
                            .as_deref(),
                        heap,
                        eval,
                    )?
                } else {
                    // Interface factories - delegate to instantiate_interface
                    instantiate_interface(
                        spec_value,
                        instance_name_opt
                            .as_ref()
                            .map(|p| make_prefix(p, name))
                            .as_deref(),
                        heap,
                        eval,
                    )?
                }
            };

            fields.insert(name.clone(), field_value);
        }

        // Create the interface instance
        let interface_instance = heap.alloc(InterfaceValue {
            fields,
            factory: _me,
        });

        // Execute __post_init__ if present
        if let Some(post_init_fn) = self.post_init_fn.as_ref() {
            let post_init_val = post_init_fn.to_value();
            if !post_init_val.is_none() {
                eval.eval_function(post_init_val, &[interface_instance], &[])?;
            }
        }

        Ok(interface_instance)
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
            // Add the interface's own type to promotion_by_type with empty path
            let mut promotion_by_type = self.promotion_by_type.clone();
            promotion_by_type.insert(variable_name.to_owned(), "".to_owned());

            Ok(Arc::new(InterfaceTypeData {
                name: variable_name.to_owned(),
                id: self.id,
                parameter_spec: build_interface_param_spec(&self.fields),
                promotion_by_type,
            }))
        })
    }

    fn dir_attr(&self) -> Vec<String> {
        self.fields.iter().map(|(k, _)| k.clone()).collect()
    }

    fn provide(&'v self, demand: &mut starlark::values::Demand<'_, 'v>) {
        demand.provide_value::<&dyn DeepCopyToHeap>(self);
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

// DeepCopyToHeap implementation for InterfaceFactory (generic, like NetValue)
impl<'v, V: ValueLike<'v> + InterfaceCell> DeepCopyToHeap for InterfaceFactoryGen<V> {
    fn deep_copy_to<'dst>(&self, dst: &'dst Heap) -> anyhow::Result<Value<'dst>> {
        // deep-copy every field
        let fields = self
            .fields
            .iter()
            .map(|(k, v)| Ok((k.clone(), copy_value(v.to_value(), dst)?)))
            .collect::<anyhow::Result<SmallMap<_, _>>>()?;

        // deep-copy optional __post_init__ callback
        let post_init_fn = match &self.post_init_fn {
            Some(v) => Some(copy_value(v.to_value(), dst)?),
            None => None,
        };

        // Preserve type data if it exists to avoid creating anonymous interfaces
        let interface_type_data = if let Some(type_data) = V::get_ty(&self.interface_type_data) {
            // Type data exists, create a new OnceCell with it
            let new_cell = OnceCell::new();
            let _ = new_cell.set(type_data.clone());
            new_cell
        } else {
            // No type data yet, create empty cell (this should be rare after export_as)
            OnceCell::new()
        };

        let new_fac = InterfaceFactoryGen {
            id: self.id,
            interface_type_data,
            fields,
            post_init_fn,
            param_spec: self.param_spec.clone(),
            promotion_by_type: self.promotion_by_type.clone(),
        };
        Ok(dst.alloc(new_fac))
    }
}

#[derive(Clone, Debug, Trace, Coerce, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub struct InterfaceValueGen<V> {
    fields: SmallMap<String, V>,
    factory: V, // store reference to the Interface *type* that created this instance
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

    fn provide(&'v self, demand: &mut starlark::values::Demand<'_, 'v>) {
        demand.provide_value::<&dyn DeepCopyToHeap>(self);
    }

    fn dir_attr(&self) -> Vec<String> {
        self.fields.keys().cloned().collect()
    }
}

impl<'v, V: ValueLike<'v>> std::fmt::Display for InterfaceValueGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut items: Vec<_> = self.fields.iter().collect();
        items.sort_by_key(|(k, _)| *k);

        // Get promotion information from the factory
        let promotion_by_type = get_promotion_map(self.factory.to_value());

        // Get the actual interface type name from the factory
        let type_name =
            get_promotion_key(self.factory.to_value()).unwrap_or_else(|_| "Interface".to_string());

        write!(f, "{type_name}(")?;
        for (i, (k, v)) in items.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }

            let value = v.to_value();
            let value_type_name =
                get_promotion_key(value).unwrap_or_else(|_| value.get_type().to_string());

            // Check if this field was marked with using() by checking if there's a promotion
            // entry for this type that points to this field name
            let is_using_field = promotion_by_type
                .get(&value_type_name)
                .is_some_and(|field_name| field_name == k.as_str());

            if is_using_field {
                write!(f, "{k}=using({value})")?;
            } else {
                write!(f, "{k}={value}")?;
            }
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

// Implement deep copy support
impl<'v, V: ValueLike<'v>> DeepCopyToHeap for InterfaceValueGen<V> {
    fn deep_copy_to<'dst>(&self, dst: &'dst Heap) -> anyhow::Result<Value<'dst>> {
        // Deep copy each field value using the shared helper.
        let fields = self
            .fields
            .iter()
            .map(|(k, v)| {
                let copied_value = copy_value(v.to_value(), dst)?;
                Ok((k.clone(), copied_value))
            })
            .collect::<Result<SmallMap<String, Value<'dst>>, anyhow::Error>>()?;

        // Deep copy the factory reference so that the new interface instance
        // remains connected to its type information in the destination heap.
        let factory = copy_value(self.factory.to_value(), dst)?;

        Ok(dst.alloc(InterfaceValue { fields, factory }))
    }
}

#[starlark_module]
pub(crate) fn interface_globals(builder: &mut GlobalsBuilder) {
    fn using<'v>(value: Value<'v>, eval: &mut Evaluator<'v, '_, '_>) -> anyhow::Result<Value<'v>> {
        let value_type = value.get_type();

        // Validate that only Net types/instances or Interface types/instances can be wrapped
        if value_type == "Net"
            || value_type == "NetType"
            || value_type == "InterfaceValue"
            || value.downcast_ref::<InterfaceFactory<'v>>().is_some()
            || value.downcast_ref::<FrozenInterfaceFactory>().is_some()
        {
            // If a Net instance was provided, unregister it from the current module
            // so it does not count as an introduced net. It will be registered when used.
            if let Some(net_val) = value.downcast_ref::<NetValue<'v>>() {
                if let Some(ctx) = eval
                    .module()
                    .extra_value()
                    .and_then(|e| e.downcast_ref::<ContextValue>())
                {
                    ctx.unregister_net(net_val.id());
                }
            }

            Ok(eval.heap().alloc(Using { value }))
        } else {
            Err(anyhow::anyhow!(
                "using() can only wrap Net or Interface types/instances, got {}",
                value_type
            ))
        }
    }

    fn interface<'v>(
        #[starlark(kwargs)] kwargs: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let heap = eval.heap();
        let mut fields = SmallMap::new();
        let mut post_init_fn = None;
        let mut all_promotion_paths: SmallMap<String, String> = SmallMap::new();

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
                // Discover all promotion paths from this field (direct and transitive)
                let field_promotion_paths = discover_promotion_paths(name, v.to_value(), name)?;

                // Check for conflicts with existing promotion paths
                for (type_name, path) in &field_promotion_paths {
                    if let Some(existing_path) = all_promotion_paths.get(type_name) {
                        return Err(anyhow::anyhow!(
                            "Interface has conflicting promotion paths to type '{}': '{}' and '{}'",
                            type_name,
                            existing_path,
                            path
                        ));
                    }
                }

                // Add all discovered paths to the global promotion map
                for (type_name, path) in field_promotion_paths {
                    all_promotion_paths.insert(type_name, path);
                }

                // Extract field value (unwrap using() if present)
                let field_value = unwrap_using(v.to_value());

                let type_str = field_value.get_type();

                // Accept Net type, Net instance, Interface factory, Interface instance, field() specs, or using() wrapped values
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
                    }
                    fields.insert(name.clone(), field_value);
                } else {
                    return Err(anyhow::anyhow!(
                        "Interface field `{}` must be Net type, Net instance, Interface type, Interface instance, field() specification, or using() wrapped value, got `{}`",
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
            promotion_by_type: all_promotion_paths,
        });

        // TODO: Add validation to ensure interfaces are assigned to variables
        // For now, anonymous interfaces will be caught when first used

        Ok(factory)
    }
}

// Helper function to instantiate an `InterfaceFactory` recursively, applying
// automatic naming to any `Net` fields as well as to nested `Interface`
// instances. The `prefix_opt` argument is the name of the *parent* interface
// instance (if provided by the user).  It is prepended to the individual
// field names (converted to upper-case) when auto-generating net names so
// that, for example, `Power("PWR")` will name the automatically-created
// `vcc` net `PWR_VCC`.
fn instantiate_interface<'v>(
    spec: Value<'v>,
    prefix_opt: Option<&str>,
    heap: &'v Heap,
    eval: &mut Evaluator<'v, '_, '_>,
) -> anyhow::Result<Value<'v>> {
    // 1. Interface factories
    if let Some(factory) = spec.downcast_ref::<InterfaceFactory<'v>>() {
        return instantiate_from_factory(factory, spec, prefix_opt, heap, eval);
    }
    if let Some(frozen_factory) = spec.downcast_ref::<FrozenInterfaceFactory>() {
        return instantiate_from_factory(frozen_factory, spec, prefix_opt, heap, eval);
    }

    // 2. Net type
    if spec.get_type() == "NetType" {
        let net_name = prefix_opt
            .map(|p| p.to_ascii_uppercase())
            .unwrap_or_else(|| "NET".to_string());
        let net_id = generate_net_id();
        let final_name = if let Some(ctx) = eval
            .module()
            .extra_value()
            .and_then(|e| e.downcast_ref::<ContextValue>())
        {
            ctx.register_net(net_id, &net_name)?
        } else {
            net_name
        };

        return Ok(heap.alloc(NetValue::new(
            net_id,
            final_name,
            SmallMap::new(),
            Value::new_none(),
        )));
    }

    // 3. Template Net instance - copy with prefix applied
    if spec.get_type() == "Net" {
        return clone_net_template(spec, prefix_opt, None, heap, eval);
    }

    // 4. Template Interface instance
    if spec.get_type() == "InterfaceValue" {
        return copy_value(spec, heap);
    }

    // 5. Fallback
    Err(anyhow::anyhow!(
        "internal error: expected spec to be InterfaceFactory/Net/InterfaceValue/NetType, got {}",
        spec.get_type()
    ))
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

    /// Return the promotion mapping for serialization purposes
    #[inline]
    pub fn promotion_by_type(&self) -> &SmallMap<String, String> {
        &self.promotion_by_type
    }
}

#[cfg(test)]
mod tests {
    use starlark::assert::Assert;
    use starlark::environment::GlobalsBuilder;

    use crate::lang::component::component_globals;
    use crate::lang::interface::interface_globals;

    #[test]
    fn interface_type_matches_instance() {
        let mut a = Assert::new();
        // Extend the default globals with the language constructs we need.
        a.globals_add(|builder: &mut GlobalsBuilder| {
            component_globals(builder);
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
            interface_globals(builder);
        });

        // Test 1: Net type should auto-generate name
        a.pass(
            r#"
Power1 = interface(vcc = Net)
instance1 = Power1()
assert_eq(instance1.vcc.name, "VCC")
"#,
        );

        // Test 2: Net with explicit name should use that name
        a.pass(
            r#"
Power2 = interface(vcc = Net("MY_VCC"))
instance2 = Power2()
assert_eq(instance2.vcc.name, "MY_VCC")
"#,
        );

        // Test 3: Net() with no name should generate a name (same as Net type)
        a.pass(
            r#"
Power3 = interface(vcc = Net())
instance3 = Power3()
# We want Net() to behave the same as Net type
assert_eq(instance3.vcc.name, "VCC")
"#,
        );

        // Test 4: With instance name prefix
        a.pass(
            r#"
Power4 = interface(vcc = Net)
instance4 = Power4("PWR")
assert_eq(instance4.vcc.name, "PWR_VCC")
"#,
        );

        // Test 5: Net() with instance name prefix should also generate a name
        a.pass(
            r#"
Power5 = interface(vcc = Net())
instance5 = Power5("PWR")
# Net() should behave the same as Net type with prefix
assert_eq(instance5.vcc.name, "PWR_VCC")
"#,
        );
    }

    #[test]
    fn using_function_basic() {
        let mut a = Assert::new();
        a.globals_add(|builder: &mut GlobalsBuilder| {
            component_globals(builder);
            interface_globals(builder);
        });

        // Test using() function availability and basic usage
        a.pass(
            r#"
# Test using() with Net instance - just verify it works
net = Net("TEST")
using_net = using(net)

# Test using() with Net type - should work now
using_net_type = using(Net)

# Test using() in interface definition
Power = interface(
    NET = using(Net("VCC")),
    voltage = field(str, "3.3V"),
)

power = Power()
assert_eq(power.NET.name, "VCC")
"#,
        );
    }

    #[test]
    fn using_net_type_functionality() {
        let mut a = Assert::new();
        a.globals_add(|builder: &mut GlobalsBuilder| {
            component_globals(builder);
            interface_globals(builder);
        });

        // Test that using(Net) behaves correctly in interface definitions
        a.pass(
            r#"
# Interface using Net type instead of Net instance
Power = interface(
    NET = using(Net),  # Should work like using(Net()) 
)

# Create instance with prefix
power = Power("VCC")
assert_eq(power.NET.name, "VCC_NET")

# Create instance without prefix
power_default = Power()
assert_eq(power_default.NET.name, "NET")
"#,
        );
    }

    #[test]
    fn using_validation() {
        let mut a = Assert::new();
        a.globals_add(|builder: &mut GlobalsBuilder| {
            component_globals(builder);
            interface_globals(builder);
        });

        // Test using() validation - should error with invalid types
        a.fail(
            r#"using("invalid")"#,
            "using() can only wrap Net or Interface types/instances",
        );
    }

    #[test]
    fn using_duplicate_promotion() {
        let mut a = Assert::new();
        a.globals_add(|builder: &mut GlobalsBuilder| {
            component_globals(builder);
            interface_globals(builder);
        });

        // Test duplicate using() fields of same type should error
        a.fail(
            r#"
interface(
    net1 = using(Net("NET1")),
    net2 = using(Net("NET2")),
)
"#,
            "Interface has conflicting promotion paths",
        );
    }

    #[test]
    fn using_transitive_conflict() {
        let mut a = Assert::new();
        a.globals_add(|builder: &mut GlobalsBuilder| {
            component_globals(builder);
            interface_globals(builder);
        });

        // Test transitive conflict detection
        a.fail(
            r#"
Power = interface(NET = using(Net("VCC")))
interface(
    power = using(Power()),        # Transitive Net via power.NET
    direct_net = using(Net("GND")) # CONFLICT: Direct Net
)
"#,
            "conflicting promotion paths to type 'Net'",
        );
    }

    #[test]
    fn using_chain_validation() {
        let mut a = Assert::new();
        a.globals_add(|builder: &mut GlobalsBuilder| {
            component_globals(builder);
            interface_globals(builder);
        });

        // Test that regular (non-using) fields don't contribute promotion paths
        a.pass(
            r#"
Power = interface(NET = using(Net("VCC")))
ValidSystem = interface(
    power = using(Power()),     # Complete using() chain - should work
    backup = Power(),           # Regular field - should NOT create conflicts
)
"#,
        );
    }
}
