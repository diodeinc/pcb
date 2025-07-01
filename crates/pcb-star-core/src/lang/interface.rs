use allocative::Allocative;
use once_cell::unsync::OnceCell;
use starlark::collections::SmallMap;
use starlark::environment::GlobalsBuilder;
use starlark::eval::{Arguments, Evaluator, ParametersSpec, ParametersSpecParam};
use starlark::starlark_complex_value;
use starlark::starlark_module;
use starlark::values::typing::TypeInstanceId;
use starlark::values::{
    starlark_value, Coerce, Freeze, FreezeResult, Heap, NoSerialize, ProvidesStaticType,
    StarlarkValue, Trace, Value, ValueLike,
};
use std::sync::Arc;
use uuid::Uuid;

use crate::lang::eval::{copy_value, DeepCopyToHeap};
use crate::lang::net::NetValue;

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
    param_spec: ParametersSpec<starlark::values::FrozenValue>,
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
        let heap = eval.heap();

        // Collect provided `name` (optional) and field values using the
        // cached parameter spec.
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
            for fld_name in self.fields.keys() {
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
                // Value supplied by the caller.
                v.to_value()
            } else {
                // Use the field spec to create a value
                let spec_value = field_spec.to_value();
                let spec_type = spec_value.get_type();

                if spec_type == "NetType" {
                    // For backwards compatibility: Net type becomes an empty net
                    let net_name = if let Some(ref inst_name) = instance_name_opt {
                        make_prefix(inst_name, name)
                    } else {
                        String::new()
                    };

                    heap.alloc(NetValue::new(
                        Uuid::new_v4().as_u64_pair().1,
                        net_name,
                        SmallMap::new(),
                    ))
                } else if spec_type == "Net" {
                    // Net instance - use as template
                    let (template_name, template_props) =
                        if let Some(net_val) = spec_value.downcast_ref::<NetValue<'v>>() {
                            (net_val.name().to_string(), net_val.properties().clone())
                        } else {
                            // Handle frozen net by copying first
                            let copied_template = copy_value(spec_value, heap)?;
                            if let Some(net_val) = copied_template.downcast_ref::<NetValue<'v>>() {
                                (net_val.name().to_string(), net_val.properties().clone())
                            } else {
                                return Err(starlark::Error::new_other(anyhow::anyhow!(
                                    "Failed to extract properties from net template"
                                )));
                            }
                        };

                    // Create new net with template name and properties
                    let net_name = if !template_name.is_empty() {
                        if let Some(ref inst_name) = instance_name_opt {
                            format!("{inst_name}_{template_name}")
                        } else {
                            template_name
                        }
                    } else {
                        // Fall back to standard naming
                        if let Some(ref inst_name) = instance_name_opt {
                            make_prefix(inst_name, name)
                        } else {
                            String::new()
                        }
                    };

                    // Deep copy the properties
                    let mut new_props = SmallMap::new();
                    for (k, v) in template_props.iter() {
                        new_props.insert(k.clone(), copy_value(v.to_value(), heap)?);
                    }

                    heap.alloc(NetValue::new(
                        Uuid::new_v4().as_u64_pair().1,
                        net_name,
                        new_props,
                    ))
                } else if spec_value.downcast_ref::<InterfaceFactory<'v>>().is_some()
                    || spec_value
                        .downcast_ref::<FrozenInterfaceFactory>()
                        .is_some()
                {
                    // Interface factory - instantiate it
                    let child_prefix = instance_name_opt.as_ref().map(|p| make_prefix(p, name));
                    instantiate_interface(spec_value, child_prefix.as_deref(), heap)?
                } else if spec_type == "InterfaceValue" {
                    // Interface instance - use as template
                    let factory_val = if let Some(interface_val) =
                        spec_value.downcast_ref::<InterfaceValue<'v>>()
                    {
                        interface_val.factory.to_value()
                    } else {
                        // Handle frozen interface
                        let copied_template = copy_value(spec_value, heap)?;
                        if let Some(interface_val) =
                            copied_template.downcast_ref::<InterfaceValue<'v>>()
                        {
                            interface_val.factory.to_value()
                        } else {
                            return Err(starlark::Error::new_other(anyhow::anyhow!(
                                "Failed to extract factory from interface template"
                            )));
                        }
                    };

                    let child_prefix = instance_name_opt.as_ref().map(|p| make_prefix(p, name));
                    instantiate_interface(factory_val, child_prefix.as_deref(), heap)?
                } else {
                    return Err(starlark::Error::new_other(anyhow::anyhow!(
                        "Invalid field type: {} for field {}",
                        spec_type,
                        name
                    )));
                }
            };

            fields.insert(name.clone(), field_value);
        }

        Ok(heap.alloc(InterfaceValue {
            fields,
            factory: _me,
        }))
    }

    fn eval_type(&self) -> Option<starlark::typing::Ty> {
        // An instance created by this factory evaluates to `InterfaceValue`,
        // so expose that as the type annotation for static/runtime checks.
        // This mirrors how `NetType` maps to `NetValue`.
        Some(<InterfaceValue as StarlarkValue>::get_type_starlark_repr())
    }

    fn provide(&'v self, demand: &mut starlark::values::Demand<'_, 'v>) {
        demand.provide_value::<&dyn DeepCopyToHeap>(self);
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
                parameter_spec: ParametersSpec::new_parts(
                    variable_name,
                    std::iter::empty::<(&str, ParametersSpecParam<_>)>(),
                    [("name", ParametersSpecParam::Optional)],
                    false,
                    self.fields
                        .iter()
                        .map(|(k, _)| (k.as_str(), ParametersSpecParam::Optional)),
                    false,
                ),
            }))
        })
    }

    fn dir_attr(&self) -> Vec<String> {
        self.fields.keys().cloned().collect()
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
        let mut keys: Vec<_> = self.fields.keys().collect();
        keys.sort();
        write!(f, "Interface(")?;
        for (i, k) in keys.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{k}")?;
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

// Deep copy support for InterfaceFactory
impl<'v, V: ValueLike<'v> + InterfaceCell> DeepCopyToHeap for InterfaceFactoryGen<V> {
    fn deep_copy_to<'dst>(&self, dst: &'dst Heap) -> anyhow::Result<Value<'dst>> {
        // Deep copy each field value
        let fields = self
            .fields
            .iter()
            .map(|(k, v)| {
                let copied_value = copy_value(v.to_value(), dst)?;
                Ok((k.clone(), copied_value))
            })
            .collect::<Result<SmallMap<String, Value<'dst>>, anyhow::Error>>()?;

        // Note: We don't copy the interface_type_data because it will be re-initialized
        // when the interface is exported in the new heap
        Ok(dst.alloc(InterfaceFactory {
            id: self.id,
            interface_type_data: OnceCell::new(),
            fields,
            param_spec: self.param_spec.clone(),
        }))
    }
}

#[starlark_module]
pub(crate) fn interface_globals(builder: &mut GlobalsBuilder) {
    fn interface<'v>(
        #[starlark(kwargs)] kwargs: SmallMap<String, Value<'v>>,
        heap: &'v Heap,
    ) -> anyhow::Result<Value<'v>> {
        let mut fields = SmallMap::new();

        // Validate field types
        for (name, v) in &kwargs {
            let type_str = v.get_type();

            // Accept Net type, Net instance, Interface factory, or Interface instance
            if type_str == "NetType"
                || type_str == "Net"
                || type_str == "InterfaceValue"
                || v.downcast_ref::<InterfaceFactory<'v>>().is_some()
                || v.downcast_ref::<FrozenInterfaceFactory>().is_some()
            {
                fields.insert(name.clone(), v.to_value());
            } else {
                return Err(anyhow::anyhow!(
                    "Interface field `{}` must be Net type, Net instance, Interface type, or Interface instance, got `{}`",
                    name,
                    type_str
                ));
            }
        }

        // Build parameter spec: optional first positional/named `name`, then
        // all interface fields as optional named‑only parameters.
        let param_spec = ParametersSpec::new_parts(
            "InterfaceInstance",
            std::iter::empty::<(&str, ParametersSpecParam<_>)>(),
            [("name", ParametersSpecParam::Optional)].into_iter(),
            false,
            fields
                .iter()
                .map(|(k, _)| (k.as_str(), ParametersSpecParam::Optional)),
            false,
        );

        Ok(heap.alloc(InterfaceFactory {
            id: TypeInstanceId::r#gen(),
            interface_type_data: OnceCell::new(),
            fields,
            param_spec,
        }))
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
    factory_value: Value<'v>,
    prefix_opt: Option<&str>,
    heap: &'v Heap,
) -> anyhow::Result<Value<'v>> {
    // Handle frozen factories by copying to current heap
    let factory_value = if factory_value
        .downcast_ref::<FrozenInterfaceFactory>()
        .is_some()
    {
        copy_value(factory_value, heap)?
    } else {
        factory_value
    };

    // Ensure we have a reference to the underlying factory data.
    let factory = factory_value
        .downcast_ref::<InterfaceFactory<'v>>()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "internal error: value is not InterfaceFactory after copy, got type: {}",
                factory_value.get_type()
            )
        })?;

    // Build the field map, recursively creating values where necessary.
    let mut fields = SmallMap::with_capacity(factory.fields.len());

    for (field_name, field_spec) in factory.fields.iter() {
        let spec_value = field_spec.to_value();
        let spec_type = spec_value.get_type();

        let field_value: Value<'v> = if spec_type == "NetType" {
            // For backwards compatibility: Net type becomes an empty net
            let net_name = if let Some(p) = prefix_opt {
                format!("{}_{}", p, field_name.to_ascii_uppercase())
            } else {
                String::new()
            };

            heap.alloc(NetValue::new(
                Uuid::new_v4().as_u64_pair().1,
                net_name,
                SmallMap::new(),
            ))
        } else if spec_type == "Net" {
            // Net instance - use as template
            let (template_name, template_props) =
                if let Some(net_val) = spec_value.downcast_ref::<NetValue<'v>>() {
                    (net_val.name().to_string(), net_val.properties().clone())
                } else {
                    // Handle frozen net by copying first
                    let copied_template = copy_value(spec_value, heap)?;
                    if let Some(net_val) = copied_template.downcast_ref::<NetValue<'v>>() {
                        (net_val.name().to_string(), net_val.properties().clone())
                    } else {
                        return Err(anyhow::anyhow!(
                            "Failed to extract properties from net template"
                        ));
                    }
                };

            let net_name = if !template_name.is_empty() {
                prefix_opt
                    .map(|p| format!("{p}_{template_name}"))
                    .unwrap_or(template_name)
            } else {
                // Fall back to standard naming
                let name_suffix = field_name.to_ascii_uppercase();
                prefix_opt
                    .map(|p| format!("{p}_{name_suffix}"))
                    .unwrap_or_default()
            };

            // Deep copy the properties
            let mut new_props = SmallMap::new();
            for (k, v) in template_props.iter() {
                new_props.insert(k.clone(), copy_value(v.to_value(), heap)?);
            }

            heap.alloc(NetValue::new(
                Uuid::new_v4().as_u64_pair().1,
                net_name,
                new_props,
            ))
        } else if spec_value.downcast_ref::<InterfaceFactory<'v>>().is_some()
            || spec_value
                .downcast_ref::<FrozenInterfaceFactory>()
                .is_some()
        {
            // Interface factory - instantiate it
            let nested_prefix =
                prefix_opt.map(|p| format!("{}_{}", p, field_name.to_ascii_uppercase()));
            instantiate_interface(spec_value, nested_prefix.as_deref(), heap)?
        } else if spec_type == "InterfaceValue" {
            // Interface instance - use as template
            let factory_val = if let Some(interface_val) =
                spec_value.downcast_ref::<InterfaceValue<'v>>()
            {
                interface_val.factory.to_value()
            } else {
                // Handle frozen interface
                let copied_template = copy_value(spec_value, heap)?;
                if let Some(interface_val) = copied_template.downcast_ref::<InterfaceValue<'v>>() {
                    interface_val.factory.to_value()
                } else {
                    return Err(anyhow::anyhow!(
                        "Failed to extract factory from interface template"
                    ));
                }
            };

            let nested_prefix =
                prefix_opt.map(|p| format!("{}_{}", p, field_name.to_ascii_uppercase()));
            instantiate_interface(factory_val, nested_prefix.as_deref(), heap)?
        } else {
            return Err(anyhow::anyhow!(
                "Invalid field type: {} for field {}",
                spec_type,
                field_name
            ));
        };

        fields.insert(field_name.clone(), field_value);
    }

    Ok(heap.alloc(InterfaceValue {
        fields,
        factory: factory_value,
    }))
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
}
