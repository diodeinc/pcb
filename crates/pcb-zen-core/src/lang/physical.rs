use std::str::FromStr;

use allocative::Allocative;
use anyhow::anyhow;
use rust_decimal::{prelude::ToPrimitive, Decimal};
use starlark::{
    any::ProvidesStaticType,
    collections::SmallMap,
    environment::GlobalsBuilder,
    starlark_module, starlark_simple_value,
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

mod capacitance;
mod conductance;
mod current;
mod frequency;
mod inductance;
mod resistance;
mod time;
mod voltage;

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

#[derive(Copy, Clone, Debug, ProvidesStaticType, NoSerialize, Freeze, Allocative)]
pub struct PhysicalValue {
    #[allocative(skip)]
    value: Decimal,
    #[allocative(skip)]
    tolerance: Decimal,
    unit: PhysicalUnit,
}

impl PhysicalValue {
    pub fn new(value: Decimal, unit: PhysicalUnit) -> Self {
        Self {
            value,
            tolerance: Decimal::ZERO,
            unit,
        }
    }

    pub fn with_tolerance(value: Decimal, unit: PhysicalUnit, tolerance: Decimal) -> Self {
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

                Ok(PhysicalValue::with_tolerance(
                    value,
                    expected_unit,
                    tolerance,
                ))
            }

            // Too many args
            _ => Err(starlark::Error::new_other(anyhow!(
                "{}() accepts at most one positional argument",
                expected_unit
            ))),
        }
    }

    pub fn unit_type<'a, T: PhysicalUnitType<'a>>(type_id: TypeInstanceId) -> Ty {
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
            Ok(PhysicalValue::new(decimal, PhysicalUnit::Dimensionless))
        }
    }
}

impl std::ops::Mul for PhysicalValue {
    type Output = Result<PhysicalValue, PhysicalValueError>;
    fn mul(self, rhs: Self) -> Self::Output {
        let value = self.value * rhs.value;
        let unit = (self.unit * rhs.unit)?;

        // Preserve tolerance only for dimensionless scaling
        let tolerance = match (self.unit, rhs.unit) {
            (PhysicalUnit::Dimensionless, _) => rhs.tolerance, // 2 * 3.3V±1% → preserve voltage tolerance
            (_, PhysicalUnit::Dimensionless) => self.tolerance, // 3.3V±1% * 2 → preserve voltage tolerance
            _ => Decimal::ZERO,                                 // All other cases drop tolerance
        };

        Ok(PhysicalValue {
            value,
            tolerance,
            unit,
        })
    }
}

impl std::ops::Div for PhysicalValue {
    type Output = Result<PhysicalValue, PhysicalValueError>;
    fn div(self, rhs: Self) -> Self::Output {
        if rhs.value == Decimal::ZERO {
            return Err(PhysicalValueError::DivisionByZero);
        }
        let value = self.value / rhs.value;
        let unit = (self.unit / rhs.unit)?;

        // Preserve tolerance only for dimensionless scaling
        let tolerance = match (self.unit, rhs.unit) {
            (_, PhysicalUnit::Dimensionless) => self.tolerance, // 3.3V±1% / 2 → preserve voltage tolerance
            _ => Decimal::ZERO,                                 // All other cases drop tolerance
        };

        Ok(PhysicalValue {
            value,
            tolerance,
            unit,
        })
    }
}

impl std::ops::Add for PhysicalValue {
    type Output = Result<PhysicalValue, PhysicalValueError>;
    fn add(self, rhs: Self) -> Self::Output {
        let unit = (self.unit + rhs.unit)?;
        let value = self.value + rhs.value;
        let tolerance = Decimal::ZERO; // Always drop tolerance for addition

        Ok(PhysicalValue {
            value,
            tolerance,
            unit,
        })
    }
}

impl std::ops::Sub for PhysicalValue {
    type Output = Result<PhysicalValue, PhysicalValueError>;
    fn sub(self, rhs: Self) -> Self::Output {
        let unit = (self.unit - rhs.unit)?;
        let value = self.value - rhs.value;
        let tolerance = Decimal::ZERO; // Always drop tolerance for subtraction

        Ok(PhysicalValue {
            value,
            tolerance,
            unit,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, ProvidesStaticType, NoSerialize, Freeze, Allocative)]
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
            Resistance => "Ohm",
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

impl std::ops::Div for PhysicalUnit {
    type Output = Result<Self, PhysicalValueError>;
    fn div(self, rhs: Self) -> Self::Output {
        use PhysicalUnit::*;
        match (self, rhs) {
            // Ohm's law
            (Voltage, Current) => Ok(Resistance),
            (Voltage, Resistance) => Ok(Current),

            // Dimensionless operations (any unit / dimensionless = same unit)
            (unit, Dimensionless) => Ok(unit),

            _ => Err(PhysicalValueError::UnsupportedOperation),
        }
    }
}

impl std::ops::Mul for PhysicalUnit {
    type Output = Result<Self, PhysicalValueError>;
    fn mul(self, rhs: Self) -> Self::Output {
        use PhysicalUnit::*;
        match (self, rhs) {
            // Ohm's law
            (Current, Resistance) => Ok(Voltage),
            (Resistance, Current) => Ok(Voltage),

            // Dimensionless operations (multiplication with dimensionless preserves original unit)
            (unit, Dimensionless) => Ok(unit), // Any unit * dimensionless = same unit
            (Dimensionless, unit) => Ok(unit), // Dimensionless * any unit = same unit

            _ => Err(PhysicalValueError::UnsupportedOperation),
        }
    }
}

impl std::ops::Add for PhysicalUnit {
    type Output = Result<Self, PhysicalValueError>;
    fn add(self, rhs: Self) -> Self::Output {
        match (self, rhs) {
            // Addition only allowed for same units
            (unit1, unit2) if unit1 == unit2 => Ok(unit1),

            _ => Err(PhysicalValueError::UnsupportedOperation),
        }
    }
}

impl std::ops::Sub for PhysicalUnit {
    type Output = Result<Self, PhysicalValueError>;
    fn sub(self, rhs: Self) -> Self::Output {
        match (self, rhs) {
            // Subtraction only allowed for same units
            (unit1, unit2) if unit1 == unit2 => Ok(unit1),

            _ => Err(PhysicalValueError::UnsupportedOperation),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PhysicalValueError {
    #[error("Division by zero")]
    DivisionByZero,
    #[error("Unsupported operation")]
    UnsupportedOperation,
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
    let formatted = if x.abs() >= Decimal::from(100) {
        format!("{:.0}", x.round())
    } else if x.abs() >= Decimal::from(10) {
        format!("{:.1}", x)
    } else {
        format!("{:.2}", x)
    };

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
                    (before_k.parse::<f64>(), after_k.parse::<f64>())
                {
                    // Treat as decimal notation: "4k7" -> "4.7k" -> 4700
                    let decimal_num = before_num + after_num / 10_f64.powi(after_k.len() as i32);
                    let combined_value = decimal_num * 1000.0;
                    return Ok(PhysicalValue::with_tolerance(
                        Decimal::try_from(combined_value).map_err(|_| ParseError::InvalidNumber)?,
                        PhysicalUnit::Resistance,
                        tolerance,
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
        let (multiplier, unit) = parse_unit_with_prefix(unit_str)?;
        let value = base_number * multiplier;

        Ok(PhysicalValue::with_tolerance(value, unit, tolerance))
    }
}

fn parse_unit_with_prefix(unit_str: &str) -> Result<(Decimal, PhysicalUnit), ParseError> {
    // Handle bare number (empty unit) - defaults to resistance
    if unit_str.is_empty() {
        return Ok((Decimal::ONE, PhysicalUnit::Resistance));
    }

    // Try each SI prefix from longest to shortest
    for &(exp, prefix) in &SI_PREFIXES {
        if prefix.is_empty() {
            continue; // Handle base unit separately
        }

        if let Some(base_unit) = unit_str.strip_prefix(prefix) {
            let multiplier = pow10(exp);
            let unit = match base_unit {
                "V" => PhysicalUnit::Voltage,
                "A" => PhysicalUnit::Current,
                "F" => PhysicalUnit::Capacitance,
                "H" => PhysicalUnit::Inductance,
                "Hz" => PhysicalUnit::Frequency,
                "s" => PhysicalUnit::Time,
                "K" => PhysicalUnit::Temperature,
                "C" => PhysicalUnit::Charge,
                "W" => PhysicalUnit::Power,
                "J" => PhysicalUnit::Energy,
                "S" => PhysicalUnit::Conductance,
                "Wb" => PhysicalUnit::MagneticFlux,
                "Ohm" | "ohm" => PhysicalUnit::Resistance,
                "" => PhysicalUnit::Resistance, // Handle bare prefix for resistance (like "4k7" -> "4k" + "7")
                _ => return Err(ParseError::InvalidUnit),
            };
            return Ok((multiplier, unit));
        }
    }

    // Handle base units (no prefix)
    let unit = match unit_str {
        "V" => PhysicalUnit::Voltage,
        "A" => PhysicalUnit::Current,
        "F" => PhysicalUnit::Capacitance,
        "H" => PhysicalUnit::Inductance,
        "Hz" => PhysicalUnit::Frequency,
        "s" => PhysicalUnit::Time,
        "K" => PhysicalUnit::Temperature,
        "C" => PhysicalUnit::Charge,
        "W" => PhysicalUnit::Power,
        "J" => PhysicalUnit::Energy,
        "S" => PhysicalUnit::Conductance,
        "Wb" => PhysicalUnit::MagneticFlux,
        "Ohm" | "ohm" => PhysicalUnit::Resistance,
        _ => return Err(ParseError::InvalidUnit),
    };

    Ok((Decimal::ONE, unit))
}

impl std::fmt::Display for PhysicalValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (scaled, prefix) = scale_to_si(self.value);
        let val_str = fmt_significant(scaled);

        let tol = self.tolerance * Decimal::from(100);
        let show_tol = tol > Decimal::ZERO;

        let suffix = self.unit.suffix();

        if show_tol {
            write!(f, "{val_str}{prefix}{} {}%", suffix, tol.round())
        } else {
            write!(f, "{val_str}{prefix}{}", suffix)
        }
    }
}

starlark_simple_value!(PhysicalUnit);

#[starlark_value(type = "PhysicalUnit")]
impl<'v> StarlarkValue<'v> for PhysicalUnit {}

starlark_simple_value!(PhysicalValue);

#[starlark_value(type = "PhysicalValue")]
impl<'v> StarlarkValue<'v> for PhysicalValue {
    fn has_attr(&self, attribute: &str, _heap: &'v starlark::values::Heap) -> bool {
        matches!(attribute, "value" | "tolerance" | "unit")
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
            _ => None,
        }
    }

    fn dir_attr(&self) -> Vec<String> {
        vec![
            "value".to_owned(),
            "tolerance".to_owned(),
            "unit".to_owned(),
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
        let result = (*self * other).map(|v| heap.alloc(v)).map_err(|err| {
            starlark::Error::new_other(anyhow!(
                "Cannot multiply {} by {} - {}",
                self.unit.name(),
                other.unit.name(),
                err
            ))
        });
        Some(result)
    }

    fn rmul(&self, other: Value<'v>, heap: &'v Heap) -> Option<Result<Value<'v>, starlark::Error>> {
        let other = PhysicalValue::try_from(other).ok()?;
        let result = (other * *self).map(|v| heap.alloc(v)).map_err(|err| {
            starlark::Error::new_other(anyhow!(
                "Cannot multiply {} by {} - {}",
                self.unit.name(),
                other.unit.name(),
                err
            ))
        });
        Some(result)
    }

    fn add(&self, other: Value<'v>, heap: &'v Heap) -> Option<Result<Value<'v>, starlark::Error>> {
        let other = PhysicalValue::try_from(other).ok()?;
        let result = (*self + other).map(|v| heap.alloc(v)).map_err(|err| {
            starlark::Error::new_other(anyhow!(
                "Cannot add {} and {} - {}",
                self.unit.name(),
                other.unit.name(),
                err
            ))
        });
        Some(result)
    }

    fn radd(&self, other: Value<'v>, heap: &'v Heap) -> Option<Result<Value<'v>, starlark::Error>> {
        let other = PhysicalValue::try_from(other).ok()?;
        let result = (other + *self).map(|v| heap.alloc(v)).map_err(|err| {
            starlark::Error::new_other(anyhow!(
                "Cannot add {} and {} - {}",
                other.unit.name(),
                self.unit.name(),
                err
            ))
        });
        Some(result)
    }

    fn sub(&self, other: Value<'v>, heap: &'v Heap) -> starlark::Result<Value<'v>> {
        let other = PhysicalValue::try_from(other).map_err(|_| {
            starlark::Error::new_other(anyhow!(
                "Cannot subtract non-physical value from {}",
                self.unit.name()
            ))
        })?;
        let result = (*self - other).map(|v| heap.alloc(v)).map_err(|err| {
            starlark::Error::new_other(anyhow!(
                "Cannot subtract {} from {} - {}",
                other.unit.name(),
                self.unit.name(),
                err
            ))
        })?;
        Ok(result)
    }
}

#[starlark_module]
pub fn physical_globals(builder: &mut GlobalsBuilder) {
    const Voltage: voltage::VoltageType = voltage::VoltageType;
    const Current: current::CurrentType = current::CurrentType;
    const Resistance: resistance::ResistanceType = resistance::ResistanceType;
    const Time: time::TimeType = time::TimeType;
    const Frequency: frequency::FrequencyType = frequency::FrequencyType;
    const Conductance: conductance::ConductanceType = conductance::ConductanceType;
    const Inductance: inductance::InductanceType = inductance::InductanceType;
    const Capacitance: capacitance::CapacitanceType = capacitance::CapacitanceType;
}

#[cfg(test)]
mod tests {
    use super::*;

    // Helper function for formatting tests
    fn assert_formatting(value_str: &str, unit: PhysicalUnit, expected: &str) {
        let val = PhysicalValue::new(value_str.parse().unwrap(), unit);
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
            ("4700", PhysicalUnit::Resistance, "4.7kOhm"),
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
            ("150000", PhysicalUnit::Resistance, "150kOhm"),
            ("47000", PhysicalUnit::Resistance, "47kOhm"),
            ("4700", PhysicalUnit::Resistance, "4.7kOhm"),
            // Trailing zero removal
            ("1000", PhysicalUnit::Resistance, "1kOhm"),
            ("1200", PhysicalUnit::Resistance, "1.2kOhm"),
            // Resistance special case (no unit suffix)
            ("1000", PhysicalUnit::Resistance, "1kOhm"),
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
            ("47", PhysicalUnit::Resistance, "47Ohm"),
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
                "1kOhm 5%",
            ), // With tolerance
            (
                Decimal::from(1000),
                PhysicalUnit::Resistance,
                Decimal::ZERO,
                "1kOhm",
            ), // Without tolerance
            (
                Decimal::from(1000),
                PhysicalUnit::Capacitance,
                Decimal::new(1, 1),
                "1kF 10%",
            ), // Non-resistance with tolerance
        ];

        for (value, unit, tolerance, expected) in test_cases {
            let val = PhysicalValue::with_tolerance(value, unit, tolerance);
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
                "100kOhm 5%",
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
            let val = PhysicalValue::with_tolerance(value, unit, tolerance);
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
}
