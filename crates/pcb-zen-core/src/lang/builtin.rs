use std::fmt;

use allocative::Allocative;
use serde::Serialize;
use starlark::{
    any::ProvidesStaticType,
    environment::GlobalsBuilder,
    starlark_module, starlark_simple_value,
    values::{starlark_value, Freeze, FreezeResult, Heap, StarlarkValue, Value},
};

use crate::lang::physical::*;

#[derive(Clone, Copy, Debug, ProvidesStaticType, Freeze, Allocative, Serialize)]
pub struct Builtin;

impl fmt::Display for Builtin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "builtin")
    }
}

starlark_simple_value!(Builtin);

#[starlark_value(type = "Builtin")]
impl<'v> StarlarkValue<'v> for Builtin {
    fn get_attr(&self, attribute: &str, heap: &'v Heap) -> Option<Value<'v>> {
        match attribute {
            "Voltage" => Some(heap.alloc_simple(VoltageType)),
            "Current" => Some(heap.alloc_simple(CurrentType)),
            "Resistance" => Some(heap.alloc_simple(ResistanceType)),
            "Time" => Some(heap.alloc_simple(TimeType)),
            "Frequency" => Some(heap.alloc_simple(FrequencyType)),
            "Conductance" => Some(heap.alloc_simple(ConductanceType)),
            "Inductance" => Some(heap.alloc_simple(InductanceType)),
            "Capacitance" => Some(heap.alloc_simple(CapacitanceType)),
            "Temperature" => Some(heap.alloc_simple(TemperatureType)),
            "Charge" => Some(heap.alloc_simple(ChargeType)),
            "Power" => Some(heap.alloc_simple(PowerType)),
            "Energy" => Some(heap.alloc_simple(EnergyType)),
            "MagneticFlux" => Some(heap.alloc_simple(MagneticFluxType)),
            _ => None,
        }
    }

    fn dir_attr(&self) -> Vec<String> {
        vec![
            "Voltage".to_string(),
            "Current".to_string(),
            "Resistance".to_string(),
            "Time".to_string(),
            "Frequency".to_string(),
            "Conductance".to_string(),
            "Inductance".to_string(),
            "Capacitance".to_string(),
            "Temperature".to_string(),
            "Charge".to_string(),
            "Power".to_string(),
            "Energy".to_string(),
            "MagneticFlux".to_string(),
        ]
    }

    fn has_attr(&self, attribute: &str, _heap: &'v starlark::values::Heap) -> bool {
        matches!(
            attribute,
            "Voltage"
                | "Current"
                | "Resistance"
                | "Time"
                | "Frequency"
                | "Conductance"
                | "Inductance"
                | "Capacitance"
                | "Temperature"
                | "Charge"
                | "Power"
                | "Energy"
                | "MagneticFlux"
        )
    }
}

#[starlark_module]
pub fn builtin_globals(builder: &mut GlobalsBuilder) {
    const builtin: Builtin = Builtin;
}
