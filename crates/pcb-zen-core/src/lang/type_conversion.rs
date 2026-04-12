use pcb_sch::physical::{PhysicalUnitDims, PhysicalValue, PhysicalValueType};
use starlark::eval::Evaluator;
use starlark::values::{Value, ValueLike, float::StarlarkFloat};

use crate::lang::r#enum::{EnumType, EnumValue};
use crate::lang::module::unwrap_config_value;
use crate::lang::net::{FrozenNetType, FrozenNetValue, NetType, NetValue};

fn is_float_type<'v>(typ: Value<'v>) -> bool {
    matches!(typ.get_type(), "float" | "Float")
        || matches!(typ.to_string().as_str(), "float" | "Float")
}

fn is_supported_scalar<'v>(value: Value<'v>) -> bool {
    let value = unwrap_config_value(value).unwrap_or(value);
    value.unpack_str().is_some()
        || value.unpack_i32().is_some()
        || value.downcast_ref::<StarlarkFloat>().is_some()
}

fn try_function_conversion<'v>(
    converter: Value<'v>,
    value: Value<'v>,
    eval: &mut Evaluator<'v, '_, '_>,
) -> anyhow::Result<Option<Value<'v>>> {
    match eval.eval_function(converter, &[value], &[]) {
        Ok(converted) => Ok(Some(converted)),
        Err(_) => Ok(None),
    }
}

/// Determines if a net type can be promoted/demoted to another.
///
/// Net type promotion hierarchy:
///   - NotConnected -> any type (universal donor)
///   - Power, Ground, etc. -> Net (demotion to base type)
///   - Net -> nothing (cannot be promoted)
///   - Nothing -> NotConnected (NotConnected only accepts NotConnected)
fn can_convert_net_type<'a>(actual: &'a str, expected: &'a str) -> Option<&'a str> {
    match (actual, expected) {
        (a, e) if a == e => None,
        ("NotConnected", expected) => Some(expected),
        (_, "Net") => Some("Net"),
        _ => None,
    }
}

/// Attempt to convert a value to another compatible net type.
pub(crate) fn try_net_conversion<'v>(
    value: Value<'v>,
    expected_typ: Value<'v>,
    eval: &mut Evaluator<'v, '_, '_>,
) -> anyhow::Result<Option<Value<'v>>> {
    let value = unwrap_config_value(value)?;
    let expected = expected_typ
        .downcast_ref::<NetType>()
        .map(|nt| nt.type_name.as_str())
        .or_else(|| {
            expected_typ
                .downcast_ref::<FrozenNetType>()
                .map(|fnt| fnt.type_name.as_str())
        });

    let Some(expected) = expected else {
        return Ok(None);
    };

    if let Some(nv) = value.downcast_ref::<NetValue>() {
        if let Some(target) = can_convert_net_type(nv.net_type_name(), expected) {
            return Ok(Some(nv.with_net_type(target, eval.heap())));
        }
    } else if let Some(fnv) = value.downcast_ref::<FrozenNetValue>()
        && let Some(target) = can_convert_net_type(fnv.net_type_name(), expected)
    {
        return Ok(Some(fnv.with_net_type(target, eval.heap())));
    }

    Ok(None)
}

/// Attempt to convert a plain string/scalar to an enum variant.
pub(crate) fn try_enum_conversion<'v>(
    value: Value<'v>,
    typ: Value<'v>,
    eval: &mut Evaluator<'v, '_, '_>,
) -> anyhow::Result<Option<Value<'v>>> {
    let value = unwrap_config_value(value)?;
    if typ.downcast_ref::<EnumType>().is_none() {
        return Ok(None);
    }

    if value.downcast_ref::<EnumValue>().is_some() {
        return Ok(None);
    }

    try_function_conversion(typ, value, eval)
}

fn try_physical_conversion_for_unit<'v>(
    value: Value<'v>,
    unit: PhysicalUnitDims,
    eval: &mut Evaluator<'v, '_, '_>,
) -> anyhow::Result<Option<Value<'v>>> {
    let value = unwrap_config_value(value)?;
    if !is_supported_scalar(value) {
        return Ok(None);
    }

    try_function_conversion(eval.heap().alloc(PhysicalValueType::new(unit)), value, eval)
}

/// Attempt to convert scalar/string inputs to a PhysicalValue via the
/// PhysicalValueType constructor.
pub(crate) fn try_physical_conversion<'v>(
    value: Value<'v>,
    typ: Value<'v>,
    eval: &mut Evaluator<'v, '_, '_>,
) -> anyhow::Result<Option<Value<'v>>> {
    let value = unwrap_config_value(value)?;
    if typ.downcast_ref::<PhysicalValueType>().is_none() {
        return Ok(None);
    }

    if !is_supported_scalar(value) {
        return Ok(None);
    }

    try_function_conversion(typ, value, eval)
}

/// Attempt physical-value conversion by inferring the unit from a typed default.
///
/// This is primarily used for `field(Voltage, default=Voltage("0V"))` style net
/// fields, where the Starlark `field(...)` wrapper preserves the compiled matcher
/// and default value but not the original constructor value.
pub(crate) fn try_physical_conversion_from_default<'v>(
    value: Value<'v>,
    default: Option<Value<'v>>,
    eval: &mut Evaluator<'v, '_, '_>,
) -> anyhow::Result<Option<Value<'v>>> {
    let Some(default) = default else {
        return Ok(None);
    };
    let Some(physical) = default.downcast_ref::<PhysicalValue>() else {
        return Ok(None);
    };

    try_physical_conversion_for_unit(value, physical.unit, eval)
}

/// Try the same implicit conversions used by module placeholders.
pub(crate) fn try_implicit_type_conversion<'v>(
    value: Value<'v>,
    typ: Value<'v>,
    eval: &mut Evaluator<'v, '_, '_>,
) -> anyhow::Result<Option<Value<'v>>> {
    let value = unwrap_config_value(value)?;

    if let Some(converted) = try_net_conversion(value, typ, eval)? {
        return Ok(Some(converted));
    }

    if let Some(converted) = try_enum_conversion(value, typ, eval)? {
        return Ok(Some(converted));
    }

    if let Some(converted) = try_physical_conversion(value, typ, eval)? {
        return Ok(Some(converted));
    }

    if is_float_type(typ)
        && let Some(i) = value.unpack_i32()
    {
        return Ok(Some(eval.heap().alloc(StarlarkFloat(i as f64))));
    }

    Ok(None)
}
