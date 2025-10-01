use std::fmt;

use allocative::Allocative;
use serde::Serialize;
use starlark::{
    any::ProvidesStaticType,
    environment::{GlobalsBuilder, Methods, MethodsBuilder, MethodsStatic},
    eval::Evaluator,
    starlark_module, starlark_simple_value,
    values::{none::NoneType, starlark_value, Freeze, StarlarkValue, Value},
    Error,
};

use crate::lang::{evaluator_ext::EvaluatorExt, physical::*, stackup::BoardConfig};

#[derive(Clone, Copy, Debug, ProvidesStaticType, Freeze, Allocative, Serialize)]
pub struct Builtin;

impl fmt::Display for Builtin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "builtin")
    }
}

starlark_simple_value!(Builtin);

#[starlark_value(type = "builtin")]
impl<'v> StarlarkValue<'v> for Builtin {
    fn get_methods() -> Option<&'static Methods> {
        static RES: MethodsStatic = MethodsStatic::new();
        RES.methods(builtin_methods)
    }
}

#[starlark_module]
pub fn builtin_globals(builder: &mut GlobalsBuilder) {
    const builtin: Builtin = Builtin;
}

#[starlark_module]
fn builtin_methods(methods: &mut MethodsBuilder) {
    #[starlark(attribute)]
    fn Voltage(this: &Builtin) -> starlark::Result<VoltageType> {
        Ok(VoltageType)
    }
    #[starlark(attribute)]
    fn Current(this: &Builtin) -> starlark::Result<CurrentType> {
        Ok(CurrentType)
    }
    #[starlark(attribute)]
    fn Resistance(this: &Builtin) -> starlark::Result<ResistanceType> {
        Ok(ResistanceType)
    }
    #[starlark(attribute)]
    fn Time(this: &Builtin) -> starlark::Result<TimeType> {
        Ok(TimeType)
    }
    #[starlark(attribute)]
    fn Frequency(this: &Builtin) -> starlark::Result<FrequencyType> {
        Ok(FrequencyType)
    }
    #[starlark(attribute)]
    fn Conductance(this: &Builtin) -> starlark::Result<ConductanceType> {
        Ok(ConductanceType)
    }
    #[starlark(attribute)]
    fn Inductance(this: &Builtin) -> starlark::Result<InductanceType> {
        Ok(InductanceType)
    }
    #[starlark(attribute)]
    fn Capacitance(this: &Builtin) -> starlark::Result<CapacitanceType> {
        Ok(CapacitanceType)
    }
    #[starlark(attribute)]
    fn Temperature(this: &Builtin) -> starlark::Result<TemperatureType> {
        Ok(TemperatureType)
    }
    #[starlark(attribute)]
    fn Charge(this: &Builtin) -> starlark::Result<ChargeType> {
        Ok(ChargeType)
    }
    #[starlark(attribute)]
    fn Power(this: &Builtin) -> starlark::Result<PowerType> {
        Ok(PowerType)
    }
    #[starlark(attribute)]
    fn Energy(this: &Builtin) -> starlark::Result<EnergyType> {
        Ok(EnergyType)
    }
    #[starlark(attribute)]
    fn MagneticFlux(this: &Builtin) -> starlark::Result<MagneticFluxType> {
        Ok(MagneticFluxType)
    }
    #[starlark(attribute)]
    fn PhysicalValue(this: &Builtin) -> starlark::Result<PhysicalValueType> {
        Ok(PhysicalValueType)
    }

    fn add_board_config<'v>(
        #[allow(unused_variables)] this: &Builtin,
        name: String,
        default: bool,
        config: Value<'v>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<NoneType> {
        let heap = eval.heap();

        // Check if board config already exists
        let config_key = format!("board_config.{}", name);
        if let Some(ctx) = eval.context_value() {
            let module = ctx.module();
            if module.properties().contains_key(&config_key) {
                return Err(Error::new_other(anyhow::anyhow!(
                    "Board config '{}' already exists",
                    name
                )));
            }
        }

        // Handle default logic
        if default {
            if let Some(ctx) = eval.context_value() {
                let module = ctx.module();
                if let Some(existing_default) = module.properties().get("default_board_config") {
                    if let Some(existing_name) = existing_default.unpack_str() {
                        return Err(Error::new_other(anyhow::anyhow!(
                            "Default board config already set to '{}'. Cannot set '{}' as default.",
                            existing_name,
                            name
                        )));
                    }
                }
            }
            eval.add_property("default_board_config", heap.alloc(name.clone()));
        }

        // Convert value to pretty-printed JSON and store config directly
        let config_json = config.to_json().map_err(|e| {
            Error::new_other(anyhow::anyhow!("Failed to convert config to JSON: {}", e))
        })?;

        // Parse and validate the board configuration (including stackup validation)
        let _board_config = BoardConfig::from_json_str(&config_json).map_err(|e| {
            Error::new_other(anyhow::anyhow!("Board config validation failed: {}", e))
        })?;

        // Parse and pretty-print the JSON
        let pretty_config_json = serde_json::from_str::<serde_json::Value>(&config_json)
            .and_then(|v| serde_json::to_string_pretty(&v))
            .map_err(|e| Error::new_other(anyhow::anyhow!("Failed to pretty-print JSON: {}", e)))?;

        eval.add_property(&config_key, heap.alloc(&pretty_config_json));
        Ok(NoneType)
    }
}
