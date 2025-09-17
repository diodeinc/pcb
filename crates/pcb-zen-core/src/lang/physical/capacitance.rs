use std::sync::OnceLock;

use allocative::Allocative;
use starlark::{
    any::ProvidesStaticType,
    eval::{Arguments, Evaluator},
    starlark_simple_value,
    typing::Ty,
    values::{
        starlark_value, typing::TypeInstanceId, Freeze, FreezeResult, NoSerialize, StarlarkValue,
        Value,
    },
};

use super::{PhysicalUnit, PhysicalUnitType, PhysicalValue};

#[derive(Clone, Copy, Debug, PartialEq, ProvidesStaticType, NoSerialize, Freeze, Allocative)]
pub struct CapacitanceType;

starlark_simple_value!(CapacitanceType);

impl std::fmt::Display for CapacitanceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Capacitance")
    }
}

impl<'a> PhysicalUnitType<'a> for CapacitanceType {
    const UNIT: PhysicalUnit = PhysicalUnit::Capacitance;
}

impl CapacitanceType {
    fn type_id() -> TypeInstanceId {
        static TYPE_ID: OnceLock<TypeInstanceId> = OnceLock::new();
        *TYPE_ID.get_or_init(TypeInstanceId::r#gen)
    }

    fn callable_type_id() -> TypeInstanceId {
        static TYPE_ID: OnceLock<TypeInstanceId> = OnceLock::new();
        *TYPE_ID.get_or_init(TypeInstanceId::r#gen)
    }
}

#[starlark_value(type = "CapacitanceType")]
impl<'v> StarlarkValue<'v> for CapacitanceType {
    fn invoke(
        &self,
        _me: Value<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        let heap = eval.heap();
        let kwargs = args.names_map()?;
        let positional: Vec<_> = args.positions(heap)?.collect();
        let physical_value = PhysicalValue::from_arguments::<Self>(&positional, &kwargs)?;
        Ok(heap.alloc(physical_value))
    }

    fn get_type_starlark_repr() -> Ty {
        PhysicalValue::unit_type::<Self>(Self::type_id())
    }

    fn typechecker_ty(&self) -> Option<Ty> {
        Some(PhysicalValue::callable_type::<Self>(
            Self::type_id(),
            Self::callable_type_id(),
        ))
    }

    fn eval_type(&self) -> Option<starlark::typing::Ty> {
        Some(Self::get_type_starlark_repr())
    }
}
