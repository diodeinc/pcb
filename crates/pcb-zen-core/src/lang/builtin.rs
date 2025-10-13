use std::fmt;

use allocative::Allocative;
use serde::Serialize;
use starlark::{
    any::ProvidesStaticType,
    collections::SmallMap,
    environment::{GlobalsBuilder, Methods, MethodsBuilder, MethodsStatic},
    eval::Evaluator,
    starlark_module, starlark_simple_value,
    values::{none::NoneType, starlark_value, tuple::UnpackTuple, Freeze, StarlarkValue, Value},
    Error,
};

use crate::lang::{evaluator_ext::EvaluatorExt, net::*, physical::*, stackup::BoardConfig};

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

    fn r#enum<'v>(
        #[starlark(args)] args: UnpackTuple<Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        let mut variant_strings = Vec::new();
        for val in args.items {
            let variant = val.unpack_str().ok_or_else(|| {
                starlark::Error::new_other(anyhow::anyhow!("All enum variants must be strings"))
            })?;
            variant_strings.push(variant.to_string());
        }
        let enum_type = crate::lang::r#enum::EnumType::new(variant_strings)?;
        Ok(eval.heap().alloc(enum_type))
    }
}

#[starlark_module]
fn builtin_methods(methods: &mut MethodsBuilder) {
    // Backward compatibility attributes - return factory instances
    #[starlark(attribute)]
    fn Voltage(this: &Builtin) -> starlark::Result<PhysicalValueType> {
        Ok(PhysicalValueType::new(pcb_sch::PhysicalUnit::Volts.into()))
    }
    #[starlark(attribute)]
    fn Current(this: &Builtin) -> starlark::Result<PhysicalValueType> {
        Ok(PhysicalValueType::new(
            pcb_sch::PhysicalUnit::Amperes.into(),
        ))
    }
    #[starlark(attribute)]
    fn Resistance(this: &Builtin) -> starlark::Result<PhysicalValueType> {
        Ok(PhysicalValueType::new(pcb_sch::PhysicalUnit::Ohms.into()))
    }
    #[starlark(attribute)]
    fn Time(this: &Builtin) -> starlark::Result<PhysicalValueType> {
        Ok(PhysicalValueType::new(
            pcb_sch::PhysicalUnit::Seconds.into(),
        ))
    }
    #[starlark(attribute)]
    fn Frequency(this: &Builtin) -> starlark::Result<PhysicalValueType> {
        Ok(PhysicalValueType::new(pcb_sch::PhysicalUnit::Hertz.into()))
    }
    #[starlark(attribute)]
    fn Conductance(this: &Builtin) -> starlark::Result<PhysicalValueType> {
        Ok(PhysicalValueType::new(
            pcb_sch::PhysicalUnit::Siemens.into(),
        ))
    }
    #[starlark(attribute)]
    fn Inductance(this: &Builtin) -> starlark::Result<PhysicalValueType> {
        Ok(PhysicalValueType::new(
            pcb_sch::PhysicalUnit::Henries.into(),
        ))
    }
    #[starlark(attribute)]
    fn Capacitance(this: &Builtin) -> starlark::Result<PhysicalValueType> {
        Ok(PhysicalValueType::new(pcb_sch::PhysicalUnit::Farads.into()))
    }
    #[starlark(attribute)]
    fn Temperature(this: &Builtin) -> starlark::Result<PhysicalValueType> {
        Ok(PhysicalValueType::new(pcb_sch::PhysicalUnit::Kelvin.into()))
    }
    #[starlark(attribute)]
    fn Charge(this: &Builtin) -> starlark::Result<PhysicalValueType> {
        Ok(PhysicalValueType::new(
            pcb_sch::PhysicalUnit::Coulombs.into(),
        ))
    }
    #[starlark(attribute)]
    fn Power(this: &Builtin) -> starlark::Result<PhysicalValueType> {
        Ok(PhysicalValueType::new(pcb_sch::PhysicalUnit::Watts.into()))
    }
    #[starlark(attribute)]
    fn Energy(this: &Builtin) -> starlark::Result<PhysicalValueType> {
        Ok(PhysicalValueType::new(pcb_sch::PhysicalUnit::Joules.into()))
    }
    #[starlark(attribute)]
    fn MagneticFlux(this: &Builtin) -> starlark::Result<PhysicalValueType> {
        Ok(PhysicalValueType::new(pcb_sch::PhysicalUnit::Webers.into()))
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

    fn physical_range(
        #[allow(unused_variables)] this: &Builtin,
        unit: String,
    ) -> starlark::Result<PhysicalRangeType> {
        let unit: pcb_sch::PhysicalUnit = unit
            .parse()
            .map_err(|err| Error::new_other(anyhow::anyhow!("Failed to parse unit: {}", err)))?;
        Ok(PhysicalRangeType::new(unit.into()))
    }

    fn physical_value(
        #[allow(unused_variables)] this: &Builtin,
        unit: String,
    ) -> starlark::Result<PhysicalValueType> {
        let unit: pcb_sch::PhysicalUnit = unit
            .parse()
            .map_err(|err| Error::new_other(anyhow::anyhow!("Failed to parse unit: {}", err)))?;
        Ok(PhysicalValueType::new(unit.into()))
    }

    fn net<'v>(
        #[allow(unused_variables)] this: &Builtin,
        name: String,
        #[starlark(kwargs)] kwargs: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        let net_type = NetType::new(name, kwargs, eval)?;
        Ok(eval.heap().alloc(net_type))
    }

    fn add_electrical_check<'v>(
        #[allow(unused_variables)] this: &Builtin,
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] check_fn: Value<'v>,
        #[starlark(require = named, default = SmallMap::default())] inputs: SmallMap<
            String,
            Value<'v>,
        >,
        #[starlark(require = named, default = "error".to_string())] severity: String,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<NoneType> {
        use crate::lang::electrical_check::ElectricalCheckGen;

        if !["error", "warning", "advice"].contains(&severity.as_str()) {
            return Err(Error::new_other(anyhow::anyhow!(
                "Invalid severity '{}'. Must be 'error', 'warning', or 'advice'",
                severity
            )));
        }

        let call_site = eval.call_stack_top_location();
        let source_path = call_site
            .as_ref()
            .map(|cs| cs.filename().to_string())
            .unwrap_or_default();
        let call_span = call_site.map(|cs| cs.resolve_span());

        let check = ElectricalCheckGen::<Value> {
            name,
            inputs,
            check_func: check_fn,
            severity,
            source_path,
            call_span,
        };

        if let Some(ctx) = eval.context_value() {
            ctx.add_child(eval.heap().alloc(check));
        }

        Ok(NoneType)
    }
}
