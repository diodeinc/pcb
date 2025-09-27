use std::fmt;

use allocative::Allocative;
use serde::Serialize;
use starlark::{
    any::ProvidesStaticType,
    environment::GlobalsBuilder,
    eval::{Arguments, Evaluator},
    starlark_module, starlark_simple_value,
    values::{starlark_value, Freeze, Heap, StarlarkValue, Value},
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
            "PhysicalValue" => Some(heap.alloc_simple(PhysicalValueType)),
            "add_board_config" => Some(heap.alloc_simple(AddBoardConfig)),
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
            "PhysicalValue".to_string(),
            "add_board_config".to_string(),
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
                | "PhysicalValue"
                | "add_board_config"
        )
    }
}

#[derive(Clone, Copy, Debug, ProvidesStaticType, Freeze, Allocative, Serialize)]
pub struct AddBoardConfig;

impl fmt::Display for AddBoardConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "add_board_config")
    }
}

starlark_simple_value!(AddBoardConfig);

#[starlark_value(type = "builtin_function_or_method")]
impl<'v> StarlarkValue<'v> for AddBoardConfig {
    fn invoke(
        &self,
        _me: Value<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        let heap = eval.heap();

        // Extract arguments from named parameters
        let args_map = args.names_map()?;
        let name_val = args_map
            .get(&heap.alloc_str("name"))
            .copied()
            .ok_or_else(|| {
                Error::new_other(anyhow::anyhow!(
                    "add_board_config() requires 'name' argument"
                ))
            })?;
        let default_val = args_map
            .get(&heap.alloc_str("default"))
            .copied()
            .ok_or_else(|| {
                Error::new_other(anyhow::anyhow!(
                    "add_board_config() requires 'default' argument"
                ))
            })?;
        let config_val = args_map
            .get(&heap.alloc_str("config"))
            .copied()
            .ok_or_else(|| {
                Error::new_other(anyhow::anyhow!(
                    "add_board_config() requires 'config' argument"
                ))
            })?;

        let name = name_val
            .unpack_str()
            .ok_or_else(|| Error::new_other(anyhow::anyhow!("name must be a string")))?
            .to_string();
        let default = default_val
            .unpack_bool()
            .ok_or_else(|| Error::new_other(anyhow::anyhow!("default must be a boolean")))?;

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
        let config_json = config_val.to_json().map_err(|e| {
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

        Ok(Value::new_none())
    }
}

#[starlark_module]
pub fn builtin_globals(builder: &mut GlobalsBuilder) {
    const builtin: Builtin = Builtin;
}
