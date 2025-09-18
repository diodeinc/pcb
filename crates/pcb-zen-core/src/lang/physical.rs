use std::str::FromStr;

use allocative::Allocative;
use anyhow::anyhow;
use rust_decimal::{prelude::ToPrimitive, Decimal};
use serde::{Deserialize, Serialize};
use starlark::{
    any::ProvidesStaticType,
    collections::SmallMap,
    eval::{Arguments, Evaluator},
    starlark_simple_value,
    typing::{
        ParamIsRequired, ParamSpec, Ty, TyCallable, TyStarlarkValue, TyUser, TyUserFields,
        TyUserParams,
    },
    util::ArcStr,
    values::{
        float::StarlarkFloat, starlark_value, string::StarlarkStr, type_repr::StarlarkTypeRepr,
        typing::TypeInstanceId, Freeze, FreezeResult, Heap, NoSerialize, StarlarkValue, Value,
        ValueLike, ValueTyped,
    },
};

/// Macro to generate physical unit type implementations
///
/// Usage: define_physical_unit!(TypeName, PhysicalUnit::Variant);
macro_rules! define_physical_unit {
    ($type_name:ident, $unit_variant:expr) => {
        #[derive(
            Clone, Copy, Debug, PartialEq, ProvidesStaticType, NoSerialize, Freeze, Allocative,
        )]
        pub struct $type_name;

        starlark_simple_value!($type_name);

        impl std::fmt::Display for $type_name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", $unit_variant)
            }
        }

        impl<'a> PhysicalUnitType<'a> for $type_name {
            const UNIT: PhysicalUnit = $unit_variant;
        }

        impl $type_name {
            fn type_id() -> TypeInstanceId {
                use std::sync::OnceLock;
                static TYPE_ID: OnceLock<TypeInstanceId> = OnceLock::new();
                *TYPE_ID.get_or_init(TypeInstanceId::r#gen)
            }

            fn callable_type_id() -> TypeInstanceId {
                use std::sync::OnceLock;
                static CALLABLE_TYPE_ID: OnceLock<TypeInstanceId> = OnceLock::new();
                *CALLABLE_TYPE_ID.get_or_init(TypeInstanceId::r#gen)
            }
        }

        #[starlark_value(type = stringify!($type_name))]
        impl<'v> StarlarkValue<'v> for $type_name {
            fn invoke(
                &self,
                _me: Value<'v>,
                args: &starlark::eval::Arguments<'v, '_>,
                eval: &mut starlark::eval::Evaluator<'v, '_, '_>,
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
    };
}

/// Convert Starlark value to Decimal for math operations
fn starlark_value_to_decimal(value: &starlark::values::Value) -> starlark::Result<Decimal> {
    if let Some(s) = value.unpack_str() {
        s.parse::<Decimal>()
            .map_err(|_| starlark::Error::new_other(anyhow!("invalid number '{s}'")))
    } else if let Some(f) = value.downcast_ref::<StarlarkFloat>() {
        Decimal::try_from(f.0)
            .map_err(|_| starlark::Error::new_other(anyhow!("invalid number {}", f.0)))
    } else if let Some(i) = value.unpack_i32() {
        Ok(Decimal::from(i))
    } else {
        Err(starlark::Error::new_other(anyhow!(
            "expected int, float or numeric string"
        )))
    }
}

#[derive(Copy, Clone, Debug, ProvidesStaticType, Freeze, Allocative, Serialize, Deserialize)]
pub struct PhysicalValue {
    #[allocative(skip)]
    #[serde(with = "rust_decimal::serde::str")]
    pub(crate) value: Decimal,
    #[allocative(skip)]
    #[serde(with = "rust_decimal::serde::str")]
    pub(crate) tolerance: Decimal,
    pub(crate) unit: PhysicalUnit,
}

impl PhysicalValue {
    pub fn from_decimal(value: Decimal, tolerance: Decimal, unit: PhysicalUnit) -> Self {
        Self {
            value,
            tolerance,
            unit,
        }
    }

    pub fn from_arguments<'v, T: PhysicalUnitType<'v>>(
        positional: &[Value<'v>],
        kwargs: &SmallMap<ValueTyped<'v, StarlarkStr>, Value<'v>>,
    ) -> starlark::Result<Self> {
        let expected_unit = T::UNIT;
        match positional {
            // Single positional argument (string or PhysicalValue)
            [single] => {
                if !kwargs.is_empty() {
                    return Err(starlark::Error::new_other(anyhow!(
                        "cannot mix positional argument with keyword arguments"
                    )));
                }

                // Check if it's already a PhysicalValue
                if let Some(phys_val) = single.downcast_ref::<PhysicalValue>() {
                    if phys_val.unit != expected_unit {
                        return Err(starlark::Error::new_other(anyhow!(
                            "expected {}, got {}",
                            expected_unit,
                            phys_val.unit
                        )));
                    }
                    return Ok(*phys_val);
                }

                // Otherwise, try to parse as string
                let src = single.unpack_str().ok_or_else(|| {
                    starlark::Error::new_other(anyhow!(
                        "{}() expects a string or {} value",
                        expected_unit,
                        expected_unit
                    ))
                })?;
                let parsed: PhysicalValue = src.parse().map_err(|e| {
                    starlark::Error::new_other(anyhow!(
                        "failed to parse {} '{}': {}",
                        expected_unit,
                        src,
                        e
                    ))
                })?;
                if parsed.unit != expected_unit {
                    return Err(starlark::Error::new_other(anyhow!(
                        "expected {}, got {}",
                        expected_unit,
                        parsed.unit
                    )));
                }
                Ok(parsed)
            }

            // Keyword mode
            [] => {
                // fail fast on unknown keyword names and extract values
                let mut value_v: Option<Value<'v>> = None;
                let mut tolerance_v: Option<Value<'v>> = None;

                for (k, v) in kwargs.iter() {
                    match k.as_str() {
                        "value" => value_v = Some(*v),
                        "tolerance" => tolerance_v = Some(*v),
                        other => {
                            return Err(starlark::Error::new_other(anyhow!(
                                "unexpected keyword '{}'",
                                other
                            )))
                        }
                    }
                }

                let value_v = value_v.ok_or_else(|| {
                    starlark::Error::new_other(anyhow!(
                        "{}() missing required keyword 'value'",
                        expected_unit
                    ))
                })?;

                let value = starlark_value_to_decimal(&value_v)?;
                let tolerance = tolerance_v
                    .map(|v| starlark_value_to_decimal(&v))
                    .transpose()?
                    .unwrap_or(Decimal::ZERO);

                Ok(PhysicalValue::from_decimal(value, tolerance, expected_unit))
            }

            // Too many args
            _ => Err(starlark::Error::new_other(anyhow!(
                "{}() accepts at most one positional argument",
                expected_unit
            ))),
        }
    }

    pub fn unit_type<'a, T: PhysicalUnitType<'a>>(type_id: TypeInstanceId) -> Ty {
        let str_param_spec = ParamSpec::new_parts(
            [(
                ParamIsRequired::Yes,
                PhysicalValue::get_type_starlark_repr(),
            )],
            [],
            None,
            [],
            None,
        )
        .unwrap();
        Ty::custom(
            TyUser::new(
                T::name(),
                TyStarlarkValue::new::<PhysicalValue>(),
                type_id,
                TyUserParams {
                    fields: TyUserFields {
                        known: [
                            ("value".to_string(), Ty::float()),
                            ("tolerance".to_string(), Ty::float()),
                            ("unit".to_string(), PhysicalUnit::starlark_type_repr()),
                            (
                                "__str__".to_string(),
                                Ty::callable(str_param_spec, Ty::string()),
                            ),
                        ]
                        .into_iter()
                        .collect(),
                        unknown: false,
                    },
                    ..Default::default()
                },
            )
            .unwrap(),
        )
    }

    pub fn callable_type<'a, T: PhysicalUnitType<'a>>(
        type_id: TypeInstanceId,
        callable_type_id: TypeInstanceId,
    ) -> Ty {
        let param_spec = ParamSpec::new_parts(
            [(
                ParamIsRequired::No,
                Ty::union2(
                    StarlarkStr::get_type_starlark_repr(),
                    PhysicalValue::get_type_starlark_repr(),
                ),
            )],
            [],
            None,
            [
                (
                    ArcStr::from("value"),
                    ParamIsRequired::No,
                    StarlarkFloat::get_type_starlark_repr(),
                ),
                (
                    ArcStr::from("tolerance"),
                    ParamIsRequired::No,
                    StarlarkFloat::get_type_starlark_repr(),
                ),
            ],
            None,
        )
        .expect("ParamSpec creation should not fail");

        Ty::custom(
            TyUser::new(
                T::type_name(),
                TyStarlarkValue::new::<T>(),
                callable_type_id,
                TyUserParams {
                    callable: Some(TyCallable::new(param_spec, Self::unit_type::<T>(type_id))),
                    ..Default::default()
                },
            )
            .unwrap(),
        )
    }
}

impl TryFrom<starlark::values::Value<'_>> for PhysicalValue {
    type Error = starlark::Error;

    fn try_from(value: starlark::values::Value<'_>) -> Result<Self, Self::Error> {
        // First try to downcast to PhysicalValue
        if let Some(physical) = value.downcast_ref::<PhysicalValue>() {
            Ok(*physical)
        } else {
            // Otherwise convert scalar to dimensionless physical value
            let decimal = starlark_value_to_decimal(&value)?;
            Ok(PhysicalValue::from_decimal(
                decimal,
                Decimal::ZERO,
                PhysicalUnit::Dimensionless,
            ))
        }
    }
}

impl std::ops::Mul for PhysicalValue {
    type Output = PhysicalValue;
    fn mul(self, rhs: Self) -> Self::Output {
        let value = self.value * rhs.value;
        let unit = self.unit * rhs.unit;

        // Preserve tolerance only for dimensionless scaling
        let tolerance = match (self.unit, rhs.unit) {
            (PhysicalUnit::Dimensionless, _) => rhs.tolerance, // 2 * 3.3V±1% → preserve voltage tolerance
            (_, PhysicalUnit::Dimensionless) => self.tolerance, // 3.3V±1% * 2 → preserve voltage tolerance
            _ => Decimal::ZERO,                                 // All other cases drop tolerance
        };

        PhysicalValue::from_decimal(value, tolerance, unit)
    }
}

impl std::ops::Div for PhysicalValue {
    type Output = Result<PhysicalValue, PhysicalValueError>;
    fn div(self, rhs: Self) -> Self::Output {
        if rhs.value == Decimal::ZERO {
            return Err(PhysicalValueError::DivisionByZero);
        }
        let value = self.value / rhs.value;
        let unit = self.unit / rhs.unit;

        // Preserve tolerance only for dimensionless scaling
        let tolerance = match (self.unit, rhs.unit) {
            (_, PhysicalUnit::Dimensionless) => self.tolerance, // 3.3V±1% / 2 → preserve voltage tolerance
            _ => Decimal::ZERO,                                 // All other cases drop tolerance
        };

        Ok(PhysicalValue::from_decimal(value, tolerance, unit))
    }
}

impl std::ops::Add for PhysicalValue {
    type Output = PhysicalValue;
    fn add(self, rhs: Self) -> Self::Output {
        let unit = self.unit + rhs.unit;
        let value = self.value + rhs.value;
        let tolerance = Decimal::ZERO; // Always drop tolerance for addition
        PhysicalValue::from_decimal(value, tolerance, unit)
    }
}

impl std::ops::Sub for PhysicalValue {
    type Output = PhysicalValue;
    fn sub(self, rhs: Self) -> Self::Output {
        let unit = self.unit - rhs.unit;
        let value = self.value - rhs.value;
        let tolerance = Decimal::ZERO; // Always drop tolerance for subtraction
        PhysicalValue::from_decimal(value, tolerance, unit)
    }
}

#[derive(
    Clone, Copy, Debug, PartialEq, ProvidesStaticType, Freeze, Allocative, Serialize, Deserialize,
)]
pub enum PhysicalUnit {
    Time,
    Current,
    Voltage,
    Capacitance,
    Resistance,
    Inductance,
    Frequency,
    Temperature,
    Charge,
    Power,
    Energy,
    Conductance,
    MagneticFlux,
    Dimensionless,
}

pub trait PhysicalUnitType<'a>: StarlarkValue<'a> {
    const UNIT: PhysicalUnit;
    fn name() -> String {
        format!("{}", Self::UNIT)
    }
    fn type_name() -> String {
        format!("{}Type", Self::UNIT)
    }
}

impl PhysicalUnit {
    fn suffix(self) -> &'static str {
        use PhysicalUnit::*;
        match self {
            Resistance => "", // This should be "Ohm", but keep as empty for backward compatibility
            Time => "s",
            Current => "A",
            Voltage => "V",
            Capacitance => "F",
            Inductance => "H",
            Frequency => "Hz",
            Temperature => "K",
            Charge => "C",
            Power => "W",
            Energy => "J",
            Conductance => "S",
            MagneticFlux => "Wb",
            Dimensionless => "",
        }
    }

    pub fn name(self) -> &'static str {
        use PhysicalUnit::*;
        match self {
            Resistance => "Ohm",
            Time => "Second",
            Current => "Ampere",
            Voltage => "Volt",
            Capacitance => "Farad",
            Inductance => "Henry",
            Frequency => "Hertz",
            Temperature => "Kelvin",
            Charge => "Coulomb",
            Power => "Watt",
            Energy => "Joule",
            Conductance => "Siemens",
            MagneticFlux => "Weber",
            Dimensionless => "Dimensionless",
        }
    }
}

impl From<PhysicalUnit> for pcb_sch::PhysicalUnit {
    fn from(value: PhysicalUnit) -> Self {
        match value {
            PhysicalUnit::Resistance => pcb_sch::PhysicalUnit::Ohms,
            PhysicalUnit::Capacitance => pcb_sch::PhysicalUnit::Farads,
            PhysicalUnit::Inductance => pcb_sch::PhysicalUnit::Henries,
            PhysicalUnit::Frequency => pcb_sch::PhysicalUnit::Hertz,
            PhysicalUnit::Temperature => pcb_sch::PhysicalUnit::Kelvin,
            PhysicalUnit::Charge => pcb_sch::PhysicalUnit::Coulombs,
            PhysicalUnit::Power => pcb_sch::PhysicalUnit::Watts,
            PhysicalUnit::Energy => pcb_sch::PhysicalUnit::Joules,
            PhysicalUnit::Conductance => pcb_sch::PhysicalUnit::Siemens,
            PhysicalUnit::MagneticFlux => pcb_sch::PhysicalUnit::Webers,
            PhysicalUnit::Dimensionless => pcb_sch::PhysicalUnit::Dimensionless,
            PhysicalUnit::Time => pcb_sch::PhysicalUnit::Seconds,
            PhysicalUnit::Current => pcb_sch::PhysicalUnit::Amperes,
            PhysicalUnit::Voltage => pcb_sch::PhysicalUnit::Volts,
        }
    }
}

impl std::ops::Div for PhysicalUnit {
    type Output = Self;
    fn div(self, rhs: Self) -> Self::Output {
        use PhysicalUnit::*;
        match (self, rhs) {
            // Ohm's law
            (Voltage, Current) => Resistance,
            (Voltage, Resistance) => Current,

            // Time/Frequency inverses
            (Dimensionless, Time) => Frequency,
            (Dimensionless, Frequency) => Time,

            // Resistance/Conductance inverses
            (Dimensionless, Resistance) => Conductance,
            (Dimensionless, Conductance) => Resistance,

            // Power relationships
            (Power, Voltage) => Current,
            (Power, Current) => Voltage,
            (Energy, Time) => Power,
            (Energy, Power) => Time,      // E/P = (P*t)/P = t
            (Power, Frequency) => Energy, // P/f = P/(1/t) = P*t = E

            // Charge relationships
            (Charge, Time) => Current,
            (Charge, Current) => Time,

            // Capacitance relationships
            (Charge, Voltage) => Capacitance,
            (Charge, Capacitance) => Voltage,

            // Magnetic flux relationships
            (MagneticFlux, Time) => Voltage, // Faraday's law: V = dΦ/dt
            (MagneticFlux, Voltage) => Time,
            (MagneticFlux, Inductance) => Current,
            (MagneticFlux, Current) => Inductance,

            // Energy-charge relationships (exact, no constants needed)
            (Energy, Voltage) => Charge, // Q = E/V (from E = Q*V)
            (Energy, Charge) => Voltage, // V = E/Q (from E = Q*V)

            // Dimensionless operations (any unit / dimensionless = same unit)
            (unit, Dimensionless) => unit,

            _ => Self::Dimensionless,
        }
    }
}

impl std::ops::Mul for PhysicalUnit {
    type Output = Self;
    fn mul(self, rhs: Self) -> Self::Output {
        use PhysicalUnit::*;
        match (self, rhs) {
            // Ohm's law
            (Current, Resistance) => Voltage,
            (Resistance, Current) => Voltage,

            // RC time constant
            (Resistance, Capacitance) => Time,
            (Capacitance, Resistance) => Time,

            // Power formulas
            (Voltage, Current) => Power,
            (Current, Voltage) => Power,
            (Power, Time) => Energy,
            (Time, Power) => Energy,
            (Energy, Frequency) => Power, // E*f = E*(1/t) = E/t = P

            // Charge formulas
            (Current, Time) => Charge,
            (Time, Current) => Charge,
            (Capacitance, Voltage) => Charge,
            (Voltage, Capacitance) => Charge,

            // Inductance formulas
            (Inductance, Current) => MagneticFlux,
            (Current, Inductance) => MagneticFlux,

            // Unit inverses (result in dimensionless)
            (Frequency, Time) => Dimensionless,
            (Time, Frequency) => Dimensionless,
            (Conductance, Resistance) => Dimensionless,
            (Resistance, Conductance) => Dimensionless,

            // L/R time constant (L * G = L * (1/R) = L/R = Time)
            (Inductance, Conductance) => Time,
            (Conductance, Inductance) => Time,

            // Additional useful combinations
            (Voltage, Charge) => Energy, // E = Q*V (potential energy)
            (Charge, Voltage) => Energy, // E = Q*V (potential energy)

            // Dimensionless operations (multiplication with dimensionless preserves original unit)
            (unit, Dimensionless) => unit, // Any unit * dimensionless = same unit
            (Dimensionless, unit) => unit, // Dimensionless * any unit = same unit

            _ => Dimensionless,
        }
    }
}

impl std::ops::Add for PhysicalUnit {
    type Output = Self;
    fn add(self, rhs: Self) -> Self::Output {
        match (self, rhs) {
            // Addition only allowed for same units
            (unit1, unit2) if unit1 == unit2 => unit1,
            _ => Self::Dimensionless,
        }
    }
}

impl std::ops::Sub for PhysicalUnit {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self::Output {
        match (self, rhs) {
            // Subtraction only allowed for same units
            (unit1, unit2) if unit1 == unit2 => unit1,
            _ => Self::Dimensionless,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PhysicalValueError {
    #[error("Division by zero")]
    DivisionByZero,
}

impl From<PhysicalValueError> for starlark::Error {
    fn from(err: PhysicalValueError) -> Self {
        starlark::Error::new_other(err)
    }
}

impl std::fmt::Display for PhysicalUnit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

const SI_PREFIXES: [(i32, &str); 17] = [
    (24, "Y"),
    (21, "Z"),
    (18, "E"),
    (15, "P"),
    (12, "T"),
    (9, "G"),
    (6, "M"),
    (3, "k"),
    (0, ""),
    (-3, "m"),
    (-6, "u"),
    (-9, "n"),
    (-12, "p"),
    (-15, "f"),
    (-18, "a"),
    (-21, "z"),
    (-24, "y"),
];

#[inline]
fn pow10(exp: i32) -> Decimal {
    if exp >= 0 {
        Decimal::from_i128_with_scale(10i128.pow(exp as u32), 0)
    } else {
        Decimal::new(1, (-exp) as u32)
    }
}

fn scale_to_si(raw: Decimal) -> (Decimal, &'static str) {
    for &(exp, sym) in &SI_PREFIXES {
        let factor = pow10(exp);
        if raw.abs() >= factor {
            return (raw / factor, sym);
        }
    }
    (raw, "")
}

fn fmt_significant(x: Decimal) -> String {
    let formatted = format!("{}", x);

    if formatted.contains('.') {
        formatted
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    } else {
        formatted
    }
}

#[derive(Debug)]
pub enum ParseError {
    InvalidFormat,
    InvalidNumber,
    InvalidUnit,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::InvalidFormat => write!(f, "Invalid physical value format"),
            ParseError::InvalidNumber => write!(f, "Invalid number"),
            ParseError::InvalidUnit => write!(f, "Invalid unit"),
        }
    }
}

impl std::error::Error for ParseError {}

impl From<PhysicalValue> for pcb_sch::PhysicalValue {
    fn from(value: PhysicalValue) -> Self {
        let PhysicalValue {
            value,
            tolerance,
            unit,
        } = value;
        pcb_sch::PhysicalValue {
            value,
            tolerance,
            unit: unit.into(),
        }
    }
}

impl FromStr for PhysicalValue {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.is_empty() {
            return Err(ParseError::InvalidFormat);
        }

        // Split by spaces to check for tolerance
        let parts: Vec<&str> = s.split_whitespace().collect();

        // Extract tolerance if provided (last token ending with "%")
        let mut tolerance = Decimal::ZERO;
        let value_unit_str = if parts.len() > 1 && parts.last().unwrap().ends_with('%') {
            let tolerance_str = parts.last().unwrap();
            let tolerance_digits = tolerance_str
                .strip_suffix('%')
                .ok_or(ParseError::InvalidFormat)?;
            if tolerance_digits.is_empty() {
                return Err(ParseError::InvalidFormat);
            }
            tolerance = tolerance_digits
                .parse::<Decimal>()
                .map_err(|_| ParseError::InvalidNumber)?
                / Decimal::from(100);

            // Rejoin all parts except the last one
            parts[..parts.len() - 1].join("")
        } else {
            // No tolerance, join all parts
            parts.join("")
        };

        // Handle special case like "4k7" (resistance notation -> 4.7kOhm)
        if let Some(k_pos) = value_unit_str.find('k') {
            let before_k = &value_unit_str[..k_pos];
            let after_k = &value_unit_str[k_pos + 1..];

            if !after_k.is_empty()
                && before_k
                    .chars()
                    .all(|c| c.is_ascii_digit() || c == '.' || c == '-' || c == '+')
                && after_k.chars().all(|c| c.is_ascii_digit() || c == '.')
            {
                if let (Ok(before_num), Ok(after_num)) =
                    (before_k.parse::<Decimal>(), after_k.parse::<Decimal>())
                {
                    // Treat as decimal notation: "4k7" -> "4.7k" -> 4700
                    let divisor = pow10(-(after_k.len() as i32));
                    let decimal_num = before_num + after_num * divisor;
                    let combined_value = decimal_num * Decimal::from(1000);
                    return Ok(PhysicalValue::from_decimal(
                        combined_value,
                        tolerance,
                        PhysicalUnit::Resistance,
                    ));
                }
            }
        }

        // Standard parsing: find where number ends
        let mut split_pos = value_unit_str.len();
        for (i, ch) in value_unit_str.char_indices() {
            if !ch.is_ascii_digit() && ch != '.' && ch != '-' && ch != '+' {
                split_pos = i;
                break;
            }
        }

        if split_pos == 0 {
            return Err(ParseError::InvalidFormat);
        }

        let number_str = &value_unit_str[..split_pos];
        let unit_str = &value_unit_str[split_pos..];

        let base_number: Decimal = number_str.parse().map_err(|_| ParseError::InvalidNumber)?;

        // Parse unit with prefix
        let (value, unit) = parse_unit_with_prefix(unit_str, base_number)?;

        Ok(PhysicalValue::from_decimal(value, tolerance, unit))
    }
}

fn convert_temperature_to_kelvin(value: Decimal, unit: &str) -> Decimal {
    match unit {
        "°C" => value + Decimal::from_str("273.15").unwrap(),
        "°F" => {
            (value - Decimal::from(32)) * Decimal::from(5) / Decimal::from(9)
                + Decimal::from_str("273.15").unwrap()
        }
        _ => value, // Already in Kelvin or other
    }
}

fn parse_base_unit(unit_str: &str) -> Result<PhysicalUnit, ParseError> {
    match unit_str {
        "V" => Ok(PhysicalUnit::Voltage),
        "A" => Ok(PhysicalUnit::Current),
        "F" => Ok(PhysicalUnit::Capacitance),
        "H" => Ok(PhysicalUnit::Inductance),
        "Hz" => Ok(PhysicalUnit::Frequency),
        "s" => Ok(PhysicalUnit::Time),
        "K" => Ok(PhysicalUnit::Temperature),
        "C" => Ok(PhysicalUnit::Charge),
        "W" => Ok(PhysicalUnit::Power),
        "J" => Ok(PhysicalUnit::Energy),
        "S" => Ok(PhysicalUnit::Conductance),
        "Wb" => Ok(PhysicalUnit::MagneticFlux),
        "Ohm" | "ohm" => Ok(PhysicalUnit::Resistance),
        "" => Ok(PhysicalUnit::Resistance), // Handle bare prefix for resistance
        _ => Err(ParseError::InvalidUnit),
    }
}

fn parse_unit_with_prefix(
    unit_str: &str,
    base_value: Decimal,
) -> Result<(Decimal, PhysicalUnit), ParseError> {
    // Handle bare number (empty unit) - defaults to resistance
    if unit_str.is_empty() {
        return Ok((base_value, PhysicalUnit::Resistance));
    }

    // Handle special time units and temperature units (non-SI but common) first
    match unit_str {
        "h" => return Ok((base_value * Decimal::from(3600), PhysicalUnit::Time)), // 1 hour = 3600 seconds
        "min" => return Ok((base_value * Decimal::from(60), PhysicalUnit::Time)), // 1 minute = 60 seconds
        "°C" | "°F" => {
            let kelvin_value = convert_temperature_to_kelvin(base_value, unit_str);
            return Ok((kelvin_value, PhysicalUnit::Temperature));
        }
        _ => {}
    }

    // Try each SI prefix from longest to shortest
    for &(exp, prefix) in &SI_PREFIXES {
        if prefix.is_empty() {
            continue; // Handle base unit separately
        }

        if let Some(base_unit) = unit_str.strip_prefix(prefix) {
            let multiplier = pow10(exp);

            // Special handling for prefixed hours
            if base_unit == "h" {
                let hour_multiplier = Decimal::from(3600);
                return Ok((
                    base_value * multiplier * hour_multiplier,
                    PhysicalUnit::Time,
                ));
            }

            let unit = parse_base_unit(base_unit)?;
            return Ok((base_value * multiplier, unit));
        }
    }

    // Handle base units (no prefix)
    let unit = parse_base_unit(unit_str)?;
    Ok((base_value, unit))
}

impl std::fmt::Display for PhysicalValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tol = self.tolerance * Decimal::from(100);
        let show_tol = tol > Decimal::ZERO;

        if self.unit == PhysicalUnit::Temperature {
            // Convert from internal Kelvin to Celsius for display
            let celsius = self.value - Decimal::from_str("273.15").unwrap();
            let val_str = fmt_significant(celsius);

            if show_tol {
                write!(f, "{}°C {}%", val_str, tol.round())
            } else {
                write!(f, "{}°C", val_str)
            }
        } else if self.unit == PhysicalUnit::Time {
            let seconds = self.value;

            // Format time in the most natural unit
            if seconds >= Decimal::from(3600) {
                // Display in hours if >= 1 hour
                let hours = seconds / Decimal::from(3600);
                let val_str = fmt_significant(hours);
                if show_tol {
                    write!(f, "{}h {}%", val_str, tol.round())
                } else {
                    write!(f, "{}h", val_str)
                }
            } else if seconds >= Decimal::from(60) {
                // Display in minutes if >= 1 minute
                let minutes = seconds / Decimal::from(60);
                let val_str = fmt_significant(minutes);
                if show_tol {
                    write!(f, "{}min {}%", val_str, tol.round())
                } else {
                    write!(f, "{}min", val_str)
                }
            } else {
                // Display in seconds with SI prefixes
                let (scaled, prefix) = scale_to_si(seconds);
                let val_str = fmt_significant(scaled);
                if show_tol {
                    write!(f, "{}{}s {}%", val_str, prefix, tol.round())
                } else {
                    write!(f, "{}{}s", val_str, prefix)
                }
            }
        } else {
            // Standard formatting for all other units
            let (scaled, prefix) = scale_to_si(self.value);
            let val_str = fmt_significant(scaled);
            let suffix = self.unit.suffix();

            if show_tol {
                write!(f, "{val_str}{prefix}{} {}%", suffix, tol.round())
            } else {
                write!(f, "{val_str}{prefix}{}", suffix)
            }
        }
    }
}

starlark_simple_value!(PhysicalUnit);

#[starlark_value(type = "PhysicalUnit")]
impl<'v> StarlarkValue<'v> for PhysicalUnit {}

starlark_simple_value!(PhysicalValue);

/// A callable wrapper for PhysicalValue's __str__ method to match stdlib units.zen API
#[derive(Debug, Clone, Allocative, Serialize, ProvidesStaticType)]
pub struct PhysicalValueStrMethod {
    value: PhysicalValue,
}

impl std::fmt::Display for PhysicalValueStrMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<__str__ method for {}>", self.value)
    }
}

starlark_simple_value!(PhysicalValueStrMethod);

#[starlark_value(type = "PhysicalValueStrMethod")]
impl<'v> StarlarkValue<'v> for PhysicalValueStrMethod {
    fn invoke(
        &self,
        _me: Value<'v>,
        _args: &Arguments<'v, '_>,
        evaluator: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        Ok(evaluator.heap().alloc(self.value.to_string()))
    }
}

#[starlark_value(type = "PhysicalValue")]
impl<'v> StarlarkValue<'v> for PhysicalValue {
    fn has_attr(&self, attribute: &str, _heap: &'v starlark::values::Heap) -> bool {
        matches!(attribute, "value" | "tolerance" | "unit" | "__str__")
    }

    fn get_attr(&self, attribute: &str, heap: &'v starlark::values::Heap) -> Option<Value<'v>> {
        match attribute {
            "value" => {
                let f = self.value.to_f64()?;
                Some(heap.alloc(StarlarkFloat(f)))
            }
            "tolerance" => {
                let f = self.tolerance.to_f64()?;
                Some(heap.alloc(StarlarkFloat(f)))
            }
            "unit" => Some(heap.alloc(self.unit)),
            "__str__" => {
                // Return a callable that returns the string representation
                Some(heap.alloc(PhysicalValueStrMethod { value: *self }))
            }
            _ => None,
        }
    }

    fn dir_attr(&self) -> Vec<String> {
        vec![
            "value".to_owned(),
            "tolerance".to_owned(),
            "unit".to_owned(),
            "__str__".to_owned(),
        ]
    }

    fn div(&self, other: Value<'v>, heap: &'v Heap) -> Option<Result<Value<'v>, starlark::Error>> {
        let other = PhysicalValue::try_from(other).ok()?;
        let result = (*self / other).map(|v| heap.alloc(v)).map_err(|err| {
            starlark::Error::new_other(anyhow!(
                "Cannot divide {} by {} - {}",
                self.unit.name(),
                other.unit.name(),
                err
            ))
        });
        Some(result)
    }

    fn rdiv(&self, other: Value<'v>, heap: &'v Heap) -> Option<Result<Value<'v>, starlark::Error>> {
        let other = PhysicalValue::try_from(other).ok()?;
        let result = (other / *self).map(|v| heap.alloc(v)).map_err(|err| {
            starlark::Error::new_other(anyhow!(
                "Cannot divide {} by {} - {}",
                self.unit.name(),
                other.unit.name(),
                err
            ))
        });
        Some(result)
    }

    fn mul(&self, other: Value<'v>, heap: &'v Heap) -> Option<Result<Value<'v>, starlark::Error>> {
        let other = PhysicalValue::try_from(other).ok()?;
        let result = heap.alloc(*self * other);
        Some(Ok(result))
    }

    fn rmul(&self, other: Value<'v>, heap: &'v Heap) -> Option<Result<Value<'v>, starlark::Error>> {
        let other = PhysicalValue::try_from(other).ok()?;
        let result = heap.alloc(other * *self);
        Some(Ok(result))
    }

    fn add(&self, other: Value<'v>, heap: &'v Heap) -> Option<Result<Value<'v>, starlark::Error>> {
        let other = PhysicalValue::try_from(other).ok()?;
        let result = heap.alloc(*self + other);
        Some(Ok(result))
    }

    fn radd(&self, other: Value<'v>, heap: &'v Heap) -> Option<Result<Value<'v>, starlark::Error>> {
        let other = PhysicalValue::try_from(other).ok()?;
        let result = heap.alloc(other + *self);
        Some(Ok(result))
    }

    fn sub(&self, other: Value<'v>, heap: &'v Heap) -> starlark::Result<Value<'v>> {
        let other = PhysicalValue::try_from(other).map_err(|_| {
            starlark::Error::new_other(anyhow!(
                "Cannot subtract non-physical value from {}",
                self.unit.name()
            ))
        })?;
        let result = heap.alloc(*self - other);
        Ok(result)
    }
}

// Physical unit types generated by macro
define_physical_unit!(VoltageType, PhysicalUnit::Voltage);
define_physical_unit!(CurrentType, PhysicalUnit::Current);
define_physical_unit!(ResistanceType, PhysicalUnit::Resistance);
define_physical_unit!(CapacitanceType, PhysicalUnit::Capacitance);
define_physical_unit!(InductanceType, PhysicalUnit::Inductance);
define_physical_unit!(FrequencyType, PhysicalUnit::Frequency);
define_physical_unit!(TimeType, PhysicalUnit::Time);
define_physical_unit!(ConductanceType, PhysicalUnit::Conductance);
define_physical_unit!(TemperatureType, PhysicalUnit::Temperature);
define_physical_unit!(ChargeType, PhysicalUnit::Charge);
define_physical_unit!(PowerType, PhysicalUnit::Power);
define_physical_unit!(EnergyType, PhysicalUnit::Energy);
define_physical_unit!(MagneticFluxType, PhysicalUnit::MagneticFlux);

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::prelude::*;

    #[cfg(test)]
    fn physical_value(value: f64, tolerance: f64, unit: PhysicalUnit) -> PhysicalValue {
        PhysicalValue {
            value: Decimal::from_f64(value).expect("value not representable as Decimal"),
            tolerance: Decimal::from_f64(tolerance)
                .expect("tolerance not representable as Decimal"),
            unit,
        }
    }

    // Helper function for formatting tests
    fn assert_formatting(value_str: &str, unit: PhysicalUnit, expected: &str) {
        let val = physical_value(value_str.parse().unwrap(), 0.0, unit);
        assert_eq!(
            format!("{}", val),
            expected,
            "Formatting mismatch for {} {}",
            value_str,
            unit
        );
    }

    // Helper function for parsing tests
    fn assert_parsing(input: &str, expected_unit: PhysicalUnit, expected_value: Decimal) {
        let val: PhysicalValue = input.parse().unwrap();
        assert_eq!(val.unit, expected_unit, "Unit mismatch for '{}'", input);
        assert_eq!(val.value, expected_value, "Value mismatch for '{}'", input);
    }

    #[test]
    fn test_si_prefix_formatting() {
        let test_cases = [
            ("4700", PhysicalUnit::Resistance, "4.7k"),
            ("1500000", PhysicalUnit::Frequency, "1.5MHz"),
            ("0.001", PhysicalUnit::Capacitance, "1mF"),
            ("0.000001", PhysicalUnit::Capacitance, "1uF"),
            ("0.0000001", PhysicalUnit::Capacitance, "100nF"),
        ];

        for (value, unit, expected) in test_cases {
            assert_formatting(value, unit, expected);
        }
    }

    #[test]
    fn test_formatting_features() {
        let test_cases = [
            // Significant digits: ≥100 (no decimals), ≥10 (one decimal), <10 (two decimals)
            ("150000", PhysicalUnit::Resistance, "150k"),
            ("47000", PhysicalUnit::Resistance, "47k"),
            ("4700", PhysicalUnit::Resistance, "4.7k"),
            // Trailing zero removal
            ("1000", PhysicalUnit::Resistance, "1k"),
            ("1200", PhysicalUnit::Resistance, "1.2k"),
            // Resistance special case (no unit suffix)
            ("1000", PhysicalUnit::Resistance, "1k"),
            ("1000", PhysicalUnit::Voltage, "1kV"), // Other units show suffix
            // Various units
            ("3300", PhysicalUnit::Voltage, "3.3kV"),
            ("0.1", PhysicalUnit::Current, "100mA"),
            ("1000000", PhysicalUnit::Frequency, "1MHz"),
            // Edge cases
            ("0.000000000001", PhysicalUnit::Capacitance, "1pF"),
            ("1000000000", PhysicalUnit::Frequency, "1GHz"),
            ("1", PhysicalUnit::Voltage, "1V"),
            // No prefix needed
            ("100", PhysicalUnit::Voltage, "100V"),
            ("47", PhysicalUnit::Resistance, "47"),
        ];

        for (value, unit, expected) in test_cases {
            assert_formatting(value, unit, expected);
        }
    }

    #[test]
    fn test_tolerance_display() {
        let test_cases = [
            (
                Decimal::from(1000),
                PhysicalUnit::Resistance,
                Decimal::new(5, 2),
                "1k 5%",
            ), // With tolerance
            (
                Decimal::from(1000),
                PhysicalUnit::Resistance,
                Decimal::ZERO,
                "1k",
            ), // Without tolerance
            (
                Decimal::from(1000),
                PhysicalUnit::Capacitance,
                Decimal::new(1, 1),
                "1kF 10%",
            ), // Non-resistance with tolerance
        ];

        for (value, unit, tolerance, expected) in test_cases {
            let val = PhysicalValue::from_decimal(value, tolerance, unit);
            assert_eq!(format!("{}", val), expected);
        }
    }

    #[test]
    fn test_parsing_basic_units() {
        let test_cases = [
            ("5V", PhysicalUnit::Voltage, Decimal::from(5)),
            ("100A", PhysicalUnit::Current, Decimal::from(100)),
            ("47", PhysicalUnit::Resistance, Decimal::from(47)),
            ("100Ohm", PhysicalUnit::Resistance, Decimal::from(100)),
            ("1C", PhysicalUnit::Charge, Decimal::from(1)),
            ("100W", PhysicalUnit::Power, Decimal::from(100)),
            ("50J", PhysicalUnit::Energy, Decimal::from(50)),
            ("10S", PhysicalUnit::Conductance, Decimal::from(10)),
            ("5Wb", PhysicalUnit::MagneticFlux, Decimal::from(5)),
        ];

        for (input, unit, value) in test_cases {
            assert_parsing(input, unit, value);
        }
    }

    #[test]
    fn test_parsing_with_prefixes() {
        let test_cases = [
            ("5kV", PhysicalUnit::Voltage, Decimal::from(5000)),
            ("100mA", PhysicalUnit::Current, Decimal::new(1, 1)), // 0.1
            ("470nF", PhysicalUnit::Capacitance, Decimal::new(47, 8)), // 470e-9
            ("4k7", PhysicalUnit::Resistance, Decimal::from(4700)), // Special notation
            ("10mC", PhysicalUnit::Charge, Decimal::new(1, 2)),   // 0.01
            ("2kW", PhysicalUnit::Power, Decimal::from(2000)),
            ("500mJ", PhysicalUnit::Energy, Decimal::new(5, 1)), // 0.5
            ("100mS", PhysicalUnit::Conductance, Decimal::new(1, 1)), // 0.1
            ("2mWb", PhysicalUnit::MagneticFlux, Decimal::new(2, 3)), // 0.002
        ];

        for (input, unit, value) in test_cases {
            assert_parsing(input, unit, value);
        }
    }

    #[test]
    fn test_parsing_decimal_numbers() {
        let test_cases = [
            ("3.3V", PhysicalUnit::Voltage, Decimal::new(33, 1)), // 3.3
            ("4.7kOhm", PhysicalUnit::Resistance, Decimal::from(4700)),
        ];

        for (input, unit, value) in test_cases {
            assert_parsing(input, unit, value);
        }
    }

    #[test]
    fn test_parsing_errors() {
        let invalid_cases = ["", "abc", "5X", "5.3.3V"];

        for input in invalid_cases {
            assert!(
                input.parse::<PhysicalValue>().is_err(),
                "Expected error for '{}'",
                input
            );
        }
    }

    #[test]
    fn test_roundtrip_parsing() {
        let test_cases = ["5V", "100mA", "4k7", "470nF", "3.3kV", "100Ohm"];

        for input in test_cases {
            let parsed: PhysicalValue = input.parse().unwrap();
            // Note: roundtrip may not be exact due to SI prefix selection
            let _formatted = format!("{}", parsed);
            // Just ensure parsing succeeds - exact roundtrip not guaranteed due to SI prefix normalization
        }
    }

    // Helper function for tolerance parsing tests
    fn assert_tolerance_parsing(
        input: &str,
        expected_unit: PhysicalUnit,
        expected_value: Decimal,
        expected_tolerance: Decimal,
    ) {
        let val: PhysicalValue = input.parse().unwrap();
        assert_eq!(val.unit, expected_unit, "Unit mismatch for '{}'", input);
        assert_eq!(val.value, expected_value, "Value mismatch for '{}'", input);
        assert_eq!(
            val.tolerance, expected_tolerance,
            "Tolerance mismatch for '{}'",
            input
        );
    }

    #[test]
    fn test_tolerance_parsing() {
        // Test basic tolerance parsing across different units
        let test_cases = [
            (
                "100kOhm 5%",
                PhysicalUnit::Resistance,
                Decimal::from(100000),
                Decimal::new(5, 2),
            ),
            (
                "10nF 20%",
                PhysicalUnit::Capacitance,
                Decimal::new(1, 8),
                Decimal::new(2, 1),
            ),
            (
                "3.3V 1%",
                PhysicalUnit::Voltage,
                Decimal::new(33, 1),
                Decimal::new(1, 2),
            ),
            (
                "12V 0.5%",
                PhysicalUnit::Voltage,
                Decimal::from(12),
                Decimal::new(5, 3),
            ),
            (
                "100mA 5%",
                PhysicalUnit::Current,
                Decimal::new(1, 1),
                Decimal::new(5, 2),
            ),
            (
                "1MHz 10%",
                PhysicalUnit::Frequency,
                Decimal::from(1000000),
                Decimal::new(1, 1),
            ),
            (
                "10uH 20%",
                PhysicalUnit::Inductance,
                Decimal::new(1, 5),
                Decimal::new(2, 1),
            ),
            (
                "100s 1%",
                PhysicalUnit::Time,
                Decimal::from(100),
                Decimal::new(1, 2),
            ),
            (
                "300K 2%",
                PhysicalUnit::Temperature,
                Decimal::from(300),
                Decimal::new(2, 2),
            ),
            (
                "4k7 1%",
                PhysicalUnit::Resistance,
                Decimal::from(4700),
                Decimal::new(1, 2),
            ), // Special notation
        ];

        for (input, unit, value, tolerance) in test_cases {
            assert_tolerance_parsing(input, unit, value, tolerance);
        }
    }

    #[test]
    fn test_tolerance_parsing_without_tolerance() {
        let test_cases = ["100kOhm", "10nF", "3.3V"];
        for input in test_cases {
            let val: PhysicalValue = input.parse().unwrap();
            assert_eq!(
                val.tolerance,
                Decimal::ZERO,
                "Expected zero tolerance for '{}'",
                input
            );
        }
    }

    #[test]
    fn test_tolerance_parsing_with_spaces() {
        let test_cases = [
            "100 kOhm 5%",  // Space in unit
            "100kOhm  5%",  // Multiple spaces before tolerance
            " 100kOhm 5% ", // Leading/trailing spaces
        ];

        for input in test_cases {
            assert_tolerance_parsing(
                input,
                PhysicalUnit::Resistance,
                Decimal::from(100000),
                Decimal::new(5, 2),
            );
        }
    }

    #[test]
    fn test_tolerance_formatting() {
        let test_cases = [
            (
                Decimal::from(100000),
                PhysicalUnit::Resistance,
                Decimal::new(5, 2),
                "100k 5%",
            ),
            (
                Decimal::new(1, 8),
                PhysicalUnit::Capacitance,
                Decimal::new(2, 1),
                "10nF 20%",
            ),
            (
                Decimal::from(3300),
                PhysicalUnit::Voltage,
                Decimal::new(1, 2),
                "3.3kV 1%",
            ),
        ];

        for (value, unit, tolerance, expected) in test_cases {
            let val = PhysicalValue::from_decimal(value, tolerance, unit);
            assert_eq!(format!("{}", val), expected);
        }
    }

    #[test]
    fn test_tolerance_parsing_errors() {
        let invalid_cases = [
            "100kOhm %",    // Empty tolerance
            "100kOhm abc%", // Invalid tolerance number
            "100kOhm 5%%",  // Multiple percent signs
        ];

        for input in invalid_cases {
            assert!(
                input.parse::<PhysicalValue>().is_err(),
                "Expected error for '{}'",
                input
            );
        }
    }

    #[test]
    fn test_unit_operations() {
        use rust_decimal::Decimal;

        // Helper to create test values
        fn val(v: f64, unit: PhysicalUnit) -> PhysicalValue {
            physical_value(v, 0.0, unit)
        }

        // Ohm's law
        let v = val(10.0, PhysicalUnit::Voltage);
        let i = val(2.0, PhysicalUnit::Current);
        let r = val(5.0, PhysicalUnit::Resistance);

        // V = I × R
        let result = i * r;
        assert_eq!(result.unit, PhysicalUnit::Voltage);
        assert_eq!(result.value, Decimal::from(10));

        // I = V / R
        let result = (v / r).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Current);
        assert_eq!(result.value, Decimal::from(2));

        // R = V / I
        let result = (v / i).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Resistance);
        assert_eq!(result.value, Decimal::from(5));
    }

    #[test]
    fn test_power_calculations() {
        // P = V × I
        let v = physical_value(12.0, 0.0, PhysicalUnit::Voltage);
        let i = physical_value(2.0, 0.0, PhysicalUnit::Current);
        let result = v * i;
        assert_eq!(result.unit, PhysicalUnit::Power);
        assert_eq!(result.value, Decimal::from(24));

        // I = P / V
        let p = physical_value(100.0, 0.0, PhysicalUnit::Power);
        let v = physical_value(120.0, 0.0, PhysicalUnit::Voltage);
        let result = (p / v).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Current);
        assert!(result.value > Decimal::from_f64(0.8).unwrap());
        assert!(result.value < Decimal::from_f64(0.9).unwrap());

        // V = P / I
        let i = physical_value(5.0, 0.0, PhysicalUnit::Current);
        let result = (p / i).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Voltage);
        assert_eq!(result.value, Decimal::from(20));
    }

    #[test]
    fn test_energy_and_time() {
        // E = P × t
        let p = physical_value(100.0, 0.0, PhysicalUnit::Power);
        let t = physical_value(3600.0, 0.0, PhysicalUnit::Time);
        let result = p * t;
        assert_eq!(result.unit, PhysicalUnit::Energy);
        assert_eq!(result.value, Decimal::from(360000));

        // P = E / t
        let e = physical_value(7200.0, 0.0, PhysicalUnit::Energy);
        let t = physical_value(7200.0, 0.0, PhysicalUnit::Time); // 2h
        let result = (e / t).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Power);
        assert_eq!(result.value, Decimal::from(1));

        // t = E / P
        let result = (e / p).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Time);
        assert_eq!(result.value, Decimal::from(72));
    }

    #[test]
    fn test_frequency_time_inverses() {
        // f = 1 / t
        let one = physical_value(1.0, 0.0, PhysicalUnit::Dimensionless);
        let t = physical_value(1.0, 0.0, PhysicalUnit::Time);
        let result = (one / t).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Frequency);
        assert_eq!(result.value, Decimal::from(1));

        // t = 1 / f
        let f = physical_value(60.0, 0.0, PhysicalUnit::Frequency);
        let result = (one / f).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Time);
        assert!(result.value > Decimal::from_f64(0.016).unwrap());
        assert!(result.value < Decimal::from_f64(0.017).unwrap());

        // f × t = 1 (dimensionless)
        let f = physical_value(10.0, 0.0, PhysicalUnit::Frequency);
        let t = physical_value(0.1, 0.0, PhysicalUnit::Time);
        let result = f * t;
        assert_eq!(result.unit, PhysicalUnit::Dimensionless);
        assert_eq!(result.value, Decimal::from(1));
    }

    #[test]
    fn test_resistance_conductance_inverses() {
        // G = 1 / R
        let one = physical_value(1.0, 0.0, PhysicalUnit::Dimensionless);
        let r = physical_value(100.0, 0.0, PhysicalUnit::Resistance);
        let result = (one / r).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Conductance);
        assert_eq!(result.value, Decimal::from_f64(0.01).unwrap());

        // R = 1 / G
        let g = physical_value(0.02, 0.0, PhysicalUnit::Conductance);
        let result = (one / g).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Resistance);
        assert_eq!(result.value, Decimal::from(50));

        // R × G = 1 (dimensionless)
        let result = r * g;
        assert_eq!(result.unit, PhysicalUnit::Dimensionless);
        assert_eq!(result.value, Decimal::from(2));
    }

    #[test]
    fn test_rc_time_constants() {
        // τ = R × C
        let r = physical_value(10000.0, 0.0, PhysicalUnit::Resistance); // 10kΩ
        let c = physical_value(0.0000001, 0.0, PhysicalUnit::Capacitance); // 100nF
        let result = r * c;
        assert_eq!(result.unit, PhysicalUnit::Time);
        assert_eq!(result.value, Decimal::from_f64(0.001).unwrap()); // 1ms

        // τ = C × R
        let result = c * r;
        assert_eq!(result.unit, PhysicalUnit::Time);
        assert_eq!(result.value, Decimal::from_f64(0.001).unwrap()); // 1ms
    }

    #[test]
    fn test_lr_time_constants() {
        // τ = L × G (L/R time constant)
        let l = physical_value(0.01, 0.0, PhysicalUnit::Inductance); // 10mH
        let g = physical_value(0.1, 0.0, PhysicalUnit::Conductance); // 100mS
        let result = l * g;
        assert_eq!(result.unit, PhysicalUnit::Time);
        assert_eq!(result.value, Decimal::from_f64(0.001).unwrap()); // 1ms

        // τ = G × L
        let result = g * l;
        assert_eq!(result.unit, PhysicalUnit::Time);
        assert_eq!(result.value, Decimal::from_f64(0.001).unwrap()); // 1ms
    }

    #[test]
    fn test_charge_relationships() {
        // Q = I × t
        let i = physical_value(2.0, 0.0, PhysicalUnit::Current);
        let t = physical_value(10.0, 0.0, PhysicalUnit::Time);
        let result = i * t;
        assert_eq!(result.unit, PhysicalUnit::Charge);
        assert_eq!(result.value, Decimal::from(20));

        // Q = C × V
        let c = physical_value(0.001, 0.0, PhysicalUnit::Capacitance); // 1000μF
        let v = physical_value(12.0, 0.0, PhysicalUnit::Voltage);
        let result = c * v;
        assert_eq!(result.unit, PhysicalUnit::Charge);
        assert_eq!(result.value, Decimal::from_f64(0.012).unwrap()); // 12mC

        // I = Q / t
        let q = physical_value(0.1, 0.0, PhysicalUnit::Charge); // 100mC
        let t = physical_value(50.0, 0.0, PhysicalUnit::Time);
        let result = (q / t).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Current);
        assert_eq!(result.value, Decimal::from_f64(0.002).unwrap()); // 2mA

        // V = Q / C
        let q = physical_value(0.005, 0.0, PhysicalUnit::Charge); // 5mC
        let result = (q / c).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Voltage);
        assert_eq!(result.value, Decimal::from(5));
    }

    #[test]
    fn test_magnetic_flux() {
        // Φ = L × I
        let l = physical_value(1.0, 0.0, PhysicalUnit::Inductance); // 1H
        let i = physical_value(2.0, 0.0, PhysicalUnit::Current);
        let result = l * i;
        assert_eq!(result.unit, PhysicalUnit::MagneticFlux);
        assert_eq!(result.value, Decimal::from(2)); // 2Wb

        // I = Φ / L
        let phi = physical_value(0.01, 0.0, PhysicalUnit::MagneticFlux); // 10mWb
        let l = physical_value(0.05, 0.0, PhysicalUnit::Inductance); // 50mH
        let result = (phi / l).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Current);
        assert_eq!(result.value, Decimal::from_f64(0.2).unwrap()); // 200mA

        // V = Φ / t (Faraday's law)
        let phi = physical_value(0.1, 0.0, PhysicalUnit::MagneticFlux); // 100mWb
        let t = physical_value(0.01, 0.0, PhysicalUnit::Time); // 10ms
        let result = (phi / t).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Voltage);
        assert_eq!(result.value, Decimal::from(10)); // 10V
    }

    #[test]
    fn test_energy_storage() {
        // E = Q × V (potential energy)
        let q = physical_value(0.001, 0.0, PhysicalUnit::Charge); // 1mC
        let v = physical_value(12.0, 0.0, PhysicalUnit::Voltage);
        let result = q * v;
        assert_eq!(result.unit, PhysicalUnit::Energy);
        assert_eq!(result.value, Decimal::from_f64(0.012).unwrap()); // 12mJ

        // Q = E / V
        let e = physical_value(0.024, 0.0, PhysicalUnit::Energy); // 24mJ
        let result = (e / v).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Charge);
        assert_eq!(result.value, Decimal::from_f64(0.002).unwrap()); // 2mC

        // V = E / Q
        let e = physical_value(0.006, 0.0, PhysicalUnit::Energy); // 6mJ
        let result = (e / q).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Voltage);
        assert_eq!(result.value, Decimal::from(6)); // 6V
    }

    #[test]
    fn test_dimensionless_operations() {
        // Any unit * dimensionless = same unit
        let v = physical_value(5.0, 0.0, PhysicalUnit::Voltage);
        let two = physical_value(2.0, 0.0, PhysicalUnit::Dimensionless);
        let result = v * two;
        assert_eq!(result.unit, PhysicalUnit::Voltage);
        assert_eq!(result.value, Decimal::from(10));

        // Any unit / dimensionless = same unit
        let result = (v / two).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Voltage);
        assert_eq!(result.value, Decimal::from_f64(2.5).unwrap());
    }

    #[test]
    fn test_unsupported_operations() {
        let v = physical_value(5.0, 0.0, PhysicalUnit::Voltage);
        let t = physical_value(1.0, 0.0, PhysicalUnit::Time);

        // V × T is not supported (no physical meaning)
        assert!((v * t).unit == PhysicalUnit::Dimensionless);

        // V + T is not supported (different units)
        assert!((v + t).unit == PhysicalUnit::Dimensionless);

        // V - T is not supported (different units)
        assert!((v - t).unit == PhysicalUnit::Dimensionless);
    }

    #[test]
    fn test_tolerance_handling() {
        // Tolerance preserved for dimensionless scaling
        let v = physical_value(5.0, 0.05, PhysicalUnit::Voltage); // 5V ±5%
        let two = physical_value(2.0, 0.0, PhysicalUnit::Dimensionless);

        // V / dimensionless preserves tolerance
        let result = (v / two).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Voltage);
        assert_eq!(result.value, Decimal::from_f64(2.5).unwrap());
        assert_eq!(result.tolerance, Decimal::from_f64(0.05).unwrap());

        // V × dimensionless preserves tolerance
        let result = v * two;
        assert_eq!(result.unit, PhysicalUnit::Voltage);
        assert_eq!(result.value, Decimal::from(10));
        assert_eq!(result.tolerance, Decimal::from_f64(0.05).unwrap());

        // Unit-changing operations drop tolerance
        let r = physical_value(100.0, 0.0, PhysicalUnit::Resistance);
        let result = (v / r).unwrap(); // V / R = I (unit changes)
        assert_eq!(result.unit, PhysicalUnit::Current);
        assert_eq!(result.tolerance, Decimal::ZERO); // Tolerance dropped
    }
}
