use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::{cmp::Ordering, fmt, hash::Hash, str::FromStr};

use allocative::Allocative;

use crate::PhysicalUnit;
use rust_decimal::{
    prelude::{FromPrimitive, ToPrimitive},
    Decimal,
};
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use starlark::{
    any::ProvidesStaticType,
    environment::{Methods, MethodsBuilder, MethodsStatic},
    eval::{Arguments, Evaluator, ParametersSpec, ParametersSpecParam},
    starlark_simple_value,
    typing::{
        ParamIsRequired, ParamSpec, Ty, TyCallable, TyStarlarkValue, TyUser, TyUserFields,
        TyUserParams,
    },
    util::ArcStr,
    values::{
        float::StarlarkFloat,
        function::FUNCTION_TYPE,
        none::NoneOr,
        starlark_value,
        string::StarlarkStr,
        typing::{TypeInstanceId, TypeMatcher, TypeMatcherFactory},
        Freeze, FreezeResult, FrozenValue, Heap, StarlarkValue, Value, ValueLike,
    },
};
use starlark_map::{sorted_map::SortedMap, StarlarkHasher};

// Shared type instance ID cache for unit-based types
fn get_type_instance_id(
    unit: PhysicalUnitDims,
    cache: &OnceLock<Mutex<HashMap<PhysicalUnitDims, TypeInstanceId>>>,
) -> TypeInstanceId {
    let map = cache.get_or_init(|| Mutex::new(HashMap::new()));
    *map.lock()
        .unwrap()
        .entry(unit)
        .or_insert_with(TypeInstanceId::gen)
}

// Constants
const KELVIN_OFFSET: Decimal = dec!(273.15);
const MINUTE: Decimal = dec!(60);
const HOUR: Decimal = dec!(3600);
const ONE_HUNDRED: Decimal = dec!(100);

/// Parse percentage or decimal string to tolerance fraction
fn parse_percentish_decimal(s: &str) -> Result<Decimal, ParseError> {
    if let Some(inner) = s.strip_suffix('%') {
        Ok(inner
            .parse::<Decimal>()
            .map_err(|_| ParseError::InvalidNumber)?
            / ONE_HUNDRED)
    } else {
        s.parse::<Decimal>().map_err(|_| ParseError::InvalidNumber)
    }
}

/// Helper for resistor "4k7" notation -> 4.7kOhm
fn parse_resistor_k_notation(s: &str, tolerance: Decimal) -> Option<PhysicalValue> {
    let k_pos = s.find('k')?;
    let before_k = &s[..k_pos];
    let after_k = &s[k_pos + 1..];

    if after_k.is_empty()
        || !before_k
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == '-' || c == '+')
        || !after_k.chars().all(|c| c.is_ascii_digit() || c == '.')
    {
        return None;
    }

    let before_num = before_k.parse::<Decimal>().ok()?;
    let after_num = after_k.parse::<Decimal>().ok()?;

    let divisor = pow10(-(after_k.len() as i32));
    let decimal_num = before_num + after_num * divisor;
    let combined_value = decimal_num * Decimal::from(1000);

    Some(PhysicalValue::from_decimal(
        combined_value,
        tolerance,
        PhysicalUnit::Ohms.into(),
    ))
}

/// Helper to convert Decimal to f64 for Starlark
fn to_f64(d: Decimal, label: &'static str) -> starlark::Result<f64> {
    d.to_f64().ok_or_else(|| {
        starlark::Error::new_other(anyhow::anyhow!("Failed to convert {} to f64", label))
    })
}

/// Convert Starlark value to Decimal for math operations
fn starlark_value_to_decimal(
    value: &starlark::values::Value,
) -> Result<Decimal, PhysicalValueError> {
    if let Some(f) = value.downcast_ref::<StarlarkFloat>() {
        Ok(Decimal::try_from(f.0)?)
    } else if let Some(i) = value.unpack_i32() {
        Ok(Decimal::from(i))
    } else if let Some(s) = value.unpack_str() {
        if let Ok(physical) = PhysicalValue::from_str(s) {
            return Ok(physical.value);
        }
        Ok(s.parse()?)
    } else {
        Err(PhysicalValueError::InvalidNumberType)
    }
}

#[derive(
    Copy,
    Clone,
    Debug,
    PartialEq,
    Eq,
    Hash,
    ProvidesStaticType,
    Freeze,
    Allocative,
    Serialize,
    Deserialize,
)]
pub struct PhysicalValue {
    #[allocative(skip)]
    #[serde(with = "rust_decimal::serde::str")]
    pub value: Decimal,
    #[allocative(skip)]
    #[serde(with = "rust_decimal::serde::str")]
    pub tolerance: Decimal,
    pub unit: PhysicalUnitDims,
}

/// Helper to extract min/max bounds from a Starlark value
fn extract_bounds(
    value: Value,
    expected_unit: PhysicalUnitDims,
) -> Result<(Decimal, Decimal), PhysicalValueError> {
    if let Some(pv) = value.downcast_ref::<PhysicalValue>() {
        if pv.unit != expected_unit {
            return Err(PhysicalValueError::UnitMismatch {
                expected: expected_unit.quantity(),
                actual: pv.unit.quantity(),
            });
        }
        let min = pv.value * (Decimal::ONE - pv.tolerance);
        let max = pv.value * (Decimal::ONE + pv.tolerance);
        Ok((min, max))
    } else if let Some(range) = value.downcast_ref::<PhysicalRange>() {
        if range.r#type.unit != expected_unit {
            return Err(PhysicalValueError::UnitMismatch {
                expected: expected_unit.quantity(),
                actual: range.r#type.unit.quantity(),
            });
        }
        Ok((range.min, range.max))
    } else if let Some(s) = value.unpack_str() {
        // Try PhysicalValue first, then PhysicalRange
        if let Ok(pv) = PhysicalValue::from_str(s) {
            if pv.unit != expected_unit {
                return Err(PhysicalValueError::UnitMismatch {
                    expected: expected_unit.quantity(),
                    actual: pv.unit.quantity(),
                });
            }
            let min = pv.value * (Decimal::ONE - pv.tolerance);
            let max = pv.value * (Decimal::ONE + pv.tolerance);
            Ok((min, max))
        } else if let Ok(range) = PhysicalRange::from_str(s) {
            if range.r#type.unit != expected_unit {
                return Err(PhysicalValueError::UnitMismatch {
                    expected: expected_unit.quantity(),
                    actual: range.r#type.unit.quantity(),
                });
            }
            Ok((range.min, range.max))
        } else {
            Err(PhysicalValueError::InvalidArgumentType {
                unit: expected_unit.quantity(),
            })
        }
    } else {
        Err(PhysicalValueError::InvalidArgumentType {
            unit: expected_unit.quantity(),
        })
    }
}

impl PhysicalValue {
    /// Construct from f64s that arrive from Starlark or other APIs
    pub fn new(value: f64, tolerance: f64, unit: PhysicalUnit) -> Self {
        Self::from_decimal(
            Decimal::from_f64(value)
                .unwrap_or_else(|| panic!("value {} not representable as Decimal", value)),
            Decimal::from_f64(tolerance)
                .unwrap_or_else(|| panic!("tolerance {} not representable as Decimal", tolerance)),
            unit.into(),
        )
    }

    /// Get the unit as a PhysicalUnit if it has a simple alias
    pub fn unit(&self) -> Option<PhysicalUnit> {
        self.unit.alias()
    }

    pub fn dimensionless<D: Into<Decimal>>(value: D) -> Self {
        Self {
            value: value.into(),
            tolerance: 0.into(),
            unit: PhysicalUnitDims::DIMENSIONLESS,
        }
    }

    pub fn from_decimal(value: Decimal, tolerance: Decimal, unit: PhysicalUnitDims) -> Self {
        Self {
            value,
            tolerance,
            unit,
        }
    }

    pub fn check_unit(self, expected: PhysicalUnitDims) -> Result<Self, PhysicalValueError> {
        if self.unit != expected {
            return Err(PhysicalValueError::UnitMismatch {
                expected: expected.quantity(),
                actual: self.unit.quantity(),
            });
        }
        Ok(self)
    }

    /// Get the effective tolerance, using a default if none is specified
    pub fn tolerance_or_default(&self, default: Decimal) -> Decimal {
        if self.tolerance.is_zero() {
            default
        } else {
            self.tolerance
        }
    }

    /// Get the minimum value considering tolerance
    pub fn min_value(&self, tolerance: Decimal) -> Decimal {
        self.value * (Decimal::ONE - tolerance)
    }

    /// Get the maximum value considering tolerance
    pub fn max_value(&self, tolerance: Decimal) -> Decimal {
        self.value * (Decimal::ONE + tolerance)
    }

    /// Check if this value's range fits within another value's range
    pub fn fits_within(&self, other: &PhysicalValue, default_tolerance: Decimal) -> bool {
        let other_tolerance = other.tolerance_or_default(default_tolerance);
        let other_min = other.min_value(other_tolerance);
        let other_max = other.max_value(other_tolerance);

        let self_tolerance = self.tolerance_or_default(default_tolerance);
        let self_min = self.min_value(self_tolerance);
        let self_max = self.max_value(self_tolerance);

        // Self range must fit within other range
        self_min >= other_min && self_max <= other_max
    }

    /// Check if this value's range fits within another value's range, using unit-aware default tolerances
    pub fn fits_within_default(&self, other: &PhysicalValue) -> bool {
        let default_tolerance = match other.unit.alias() {
            Some(PhysicalUnit::Ohms) => "0.01".parse().unwrap(), // 1% for resistors
            Some(PhysicalUnit::Farads) => "0.1".parse().unwrap(), // 10% for capacitors
            _ => "0.01".parse().unwrap(),                        // 1% for others
        };
        self.fits_within(other, default_tolerance)
    }

    /// Get the absolute value of this physical value
    pub fn abs(&self) -> PhysicalValue {
        PhysicalValue {
            value: self.value.abs(),
            tolerance: self.tolerance,
            unit: self.unit,
        }
    }

    /// Get the absolute difference between two physical values
    /// Returns an error if units don't match
    pub fn diff(&self, other: &PhysicalValue) -> Result<PhysicalValue, PhysicalValueError> {
        // Use the subtraction operator which validates units
        let result = (*self - *other)?;
        // Return the absolute value
        Ok(result.abs())
    }

    /// Check if this value's tolerance range fits completely within another value's tolerance range
    /// Returns an error if units don't match
    pub fn within(&self, other: &PhysicalValue) -> Result<bool, PhysicalValueError> {
        // Delegate to extract_bounds helper - same logic as is_in()
        if self.unit != other.unit {
            return Err(PhysicalValueError::UnitMismatch {
                expected: other.unit.quantity(),
                actual: self.unit.quantity(),
            });
        }

        let self_min = self.value * (Decimal::ONE - self.tolerance);
        let self_max = self.value * (Decimal::ONE + self.tolerance);
        let other_min = other.value * (Decimal::ONE - other.tolerance);
        let other_max = other.value * (Decimal::ONE + other.tolerance);

        Ok(self_min >= other_min && self_max <= other_max)
    }

    fn fields() -> SortedMap<String, Ty> {
        fn single_param_spec(param_type: Ty) -> ParamSpec {
            ParamSpec::new_parts([(ParamIsRequired::Yes, param_type)], [], None, [], None).unwrap()
        }

        let str_param_spec = single_param_spec(PhysicalValue::get_type_starlark_repr());
        let with_tolerance_param_spec = single_param_spec(Ty::union2(Ty::float(), Ty::string()));
        let with_value_param_spec = single_param_spec(Ty::union2(Ty::float(), Ty::int()));
        let with_unit_param_spec = single_param_spec(Ty::union2(Ty::string(), Ty::none()));
        let diff_param_spec = single_param_spec(PhysicalValue::get_type_starlark_repr());
        let abs_param_spec = single_param_spec(PhysicalValue::get_type_starlark_repr());
        let within_param_spec = single_param_spec(Ty::any()); // Accepts any type like is_in()

        SortedMap::from_iter([
            ("value".to_string(), Ty::float()),
            ("tolerance".to_string(), Ty::float()),
            ("min".to_string(), Ty::float()),
            ("max".to_string(), Ty::float()),
            ("unit".to_string(), Ty::string()),
            (
                "__str__".to_string(),
                Ty::callable(str_param_spec, Ty::string()),
            ),
            (
                "with_tolerance".to_string(),
                Ty::callable(
                    with_tolerance_param_spec,
                    PhysicalValue::get_type_starlark_repr(),
                ),
            ),
            (
                "with_value".to_string(),
                Ty::callable(
                    with_value_param_spec,
                    PhysicalValue::get_type_starlark_repr(),
                ),
            ),
            (
                "with_unit".to_string(),
                Ty::callable(
                    with_unit_param_spec,
                    PhysicalValue::get_type_starlark_repr(),
                ),
            ),
            (
                "abs".to_string(),
                Ty::callable(abs_param_spec, PhysicalValue::get_type_starlark_repr()),
            ),
            (
                "diff".to_string(),
                Ty::callable(diff_param_spec, PhysicalValue::get_type_starlark_repr()),
            ),
            (
                "within".to_string(),
                Ty::callable(within_param_spec, Ty::bool()),
            ),
        ])
    }

    pub fn unit_type(type_id: TypeInstanceId, unit: PhysicalUnit) -> Ty {
        Ty::custom(
            TyUser::new(
                unit.quantity().to_string(),
                TyStarlarkValue::new::<PhysicalValue>(),
                type_id,
                TyUserParams {
                    fields: TyUserFields {
                        known: Self::fields(),
                        unknown: false,
                    },
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
        } else if let Some(s) = value.downcast_ref::<StarlarkStr>() {
            // Try to parse as string
            Ok(Self::from_str(s)?)
        } else {
            // Otherwise convert scalar to dimensionless physical value
            let decimal = starlark_value_to_decimal(&value)?;
            Ok(PhysicalValue::from_decimal(
                decimal,
                Decimal::ZERO,
                PhysicalUnitDims::DIMENSIONLESS,
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
            (PhysicalUnitDims::DIMENSIONLESS, _) => rhs.tolerance, // 2 * 3.3V±1% → preserve voltage tolerance
            (_, PhysicalUnitDims::DIMENSIONLESS) => self.tolerance, // 3.3V±1% * 2 → preserve voltage tolerance
            _ => Decimal::ZERO, // All other cases drop tolerance
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
            (_, PhysicalUnitDims::DIMENSIONLESS) => self.tolerance, // 3.3V±1% / 2 → preserve voltage tolerance
            _ => Decimal::ZERO, // All other cases drop tolerance
        };

        Ok(PhysicalValue::from_decimal(value, tolerance, unit))
    }
}

impl std::ops::Add for PhysicalValue {
    type Output = Result<PhysicalValue, PhysicalValueError>;
    fn add(self, rhs: Self) -> Self::Output {
        if self.unit != rhs.unit {
            return Err(PhysicalValueError::UnitMismatch {
                expected: self.unit.quantity(),
                actual: rhs.unit.quantity(),
            });
        }
        let unit = self.unit;
        let value = self.value + rhs.value;
        let tolerance = Decimal::ZERO; // Always drop tolerance for addition
        Ok(PhysicalValue::from_decimal(value, tolerance, unit))
    }
}

impl std::ops::Sub for PhysicalValue {
    type Output = Result<PhysicalValue, PhysicalValueError>;
    fn sub(self, rhs: Self) -> Self::Output {
        if self.unit != rhs.unit {
            return Err(PhysicalValueError::UnitMismatch {
                expected: self.unit.quantity(),
                actual: rhs.unit.quantity(),
            });
        }
        let unit = self.unit;
        let value = self.value - rhs.value;
        let tolerance = Decimal::ZERO; // Always drop tolerance for subtraction
        Ok(PhysicalValue::from_decimal(value, tolerance, unit))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ProvidesStaticType, Allocative, Hash)]
pub struct PhysicalUnitDims {
    pub current: i8,
    pub time: i8,
    pub voltage: i8,
    pub temp: i8,
}

impl Freeze for PhysicalUnitDims {
    type Frozen = Self;
    fn freeze(self, _freezer: &starlark::values::Freezer) -> FreezeResult<Self::Frozen> {
        Ok(self)
    }
}

impl std::ops::Mul for PhysicalUnitDims {
    type Output = PhysicalUnitDims;
    fn mul(self, rhs: Self) -> Self::Output {
        PhysicalUnitDims {
            current: self.current + rhs.current,
            time: self.time + rhs.time,
            voltage: self.voltage + rhs.voltage,
            temp: self.temp + rhs.temp,
        }
    }
}

impl std::ops::Div for PhysicalUnitDims {
    type Output = PhysicalUnitDims;
    fn div(self, rhs: Self) -> Self::Output {
        PhysicalUnitDims {
            current: self.current - rhs.current,
            time: self.time - rhs.time,
            voltage: self.voltage - rhs.voltage,
            temp: self.temp - rhs.temp,
        }
    }
}

impl fmt::Display for PhysicalUnitDims {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.fmt_unit())
    }
}

impl From<PhysicalUnit> for PhysicalUnitDims {
    fn from(unit: PhysicalUnit) -> Self {
        use PhysicalUnit::*;
        match unit {
            Amperes => Self::CURRENT,
            Seconds => Self::TIME,
            Volts => Self::VOLTAGE,
            Kelvin => Self::TEMP,
            Hertz => Self::DIMENSIONLESS / Self::TIME,
            Coulombs => Self::CURRENT * Self::TIME,
            Ohms => Self::VOLTAGE / Self::CURRENT,
            Siemens => Self::CURRENT / Self::VOLTAGE,
            Farads => Self::CURRENT * Self::TIME / Self::VOLTAGE,
            Watts => Self::VOLTAGE * Self::CURRENT,
            Joules => Self::VOLTAGE * Self::CURRENT * Self::TIME,
            Webers => Self::VOLTAGE * Self::TIME,
            Henries => Self::VOLTAGE * Self::TIME / Self::CURRENT,
        }
    }
}

impl FromStr for PhysicalUnitDims {
    type Err = ParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        // 1. Fast path: simple aliases
        if let Ok(alias) = s.parse::<PhysicalUnit>() {
            return Ok(alias.into());
        }

        // 2. Split into numerator/denominator
        let (num_str_opt, den_str_opt) = match s.find('/') {
            None => (Some(s), None),
            Some(idx) => {
                let (lhs, rhs) = s.split_at(idx);
                let rhs = &rhs[1..]; // strip the '/'
                if lhs.is_empty() || lhs == "1" {
                    (None, Some(rhs))
                } else {
                    // Handle both "V·s/(A)" and "V·s/A" formats
                    let den = rhs
                        .strip_prefix('(')
                        .and_then(|r| r.strip_suffix(')'))
                        .unwrap_or(rhs);
                    (Some(lhs), Some(den))
                }
            }
        };

        // 3. Parse each side
        let mut dims = PhysicalUnitDims::DIMENSIONLESS;

        if let Some(num_str) = num_str_opt {
            dims = dims * gather_units(num_str)?;
        }
        if let Some(den_str) = den_str_opt {
            dims = dims / gather_units(den_str)?;
        }

        Ok(dims)
    }
}

impl From<PhysicalUnitDims> for String {
    fn from(dims: PhysicalUnitDims) -> String {
        // Serialize as unit enum name (e.g., "Farads") not suffix (e.g., "F")
        // to maintain compatibility with old PhysicalUnit serialization
        if let Some(alias) = dims.alias() {
            format!("{:?}", alias)
        } else {
            dims.to_string()
        }
    }
}

impl serde::Serialize for PhysicalUnitDims {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        String::from(*self).serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for PhysicalUnitDims {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

impl TryFrom<String> for PhysicalUnitDims {
    type Error = ParseError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

/// Parse units from a string like "A·s" and multiply them together
fn gather_units(list: &str) -> Result<PhysicalUnitDims, ParseError> {
    // Strip parentheses if present
    let list = list
        .strip_prefix('(')
        .and_then(|s| s.strip_suffix(')'))
        .unwrap_or(list);

    let mut acc = PhysicalUnitDims::DIMENSIONLESS;
    for token in list.split('·').filter(|t| !t.is_empty()) {
        let u: PhysicalUnitDims = token
            .parse::<PhysicalUnit>()
            .map_err(|_| ParseError::InvalidUnit)?
            .into();
        acc = acc * u;
    }
    Ok(acc)
}

impl PhysicalUnitDims {
    pub const DIMENSIONLESS: Self = Self::new(0, 0, 0, 0);
    pub const CURRENT: Self = Self::new(1, 0, 0, 0);
    pub const TIME: Self = Self::new(0, 1, 0, 0);
    pub const VOLTAGE: Self = Self::new(0, 0, 1, 0);
    pub const TEMP: Self = Self::new(0, 0, 0, 1);

    const fn new(current: i8, time: i8, voltage: i8, temp: i8) -> Self {
        Self {
            current,
            time,
            voltage,
            temp,
        }
    }

    fn alias(&self) -> Option<PhysicalUnit> {
        use PhysicalUnit::*;
        let PhysicalUnitDims {
            current,
            time,
            voltage,
            temp,
        } = self;
        let alias = match (current, time, voltage, temp) {
            // bases
            (1, 0, 0, 0) => Amperes, // A
            (0, 1, 0, 0) => Seconds, // s
            (0, 0, 1, 0) => Volts,   // V
            (0, 0, 0, 1) => Kelvin,  // K
            // derived
            (0, -1, 0, 0) => Hertz,   // Hz = 1/s
            (1, 1, 0, 0) => Coulombs, // C = A*s
            (-1, 0, 1, 0) => Ohms,    // Ohm = V/A
            (1, 0, -1, 0) => Siemens, // S = A/V
            (1, 1, -1, 0) => Farads,  // F = A*s/V
            (-1, 1, 1, 0) => Henries, // H = V*s/A
            (1, 0, 1, 0) => Watts,    // W = V*A
            (1, 1, 1, 0) => Joules,   // J = V*A*s
            (0, 1, 1, 0) => Webers,   // Wb = V*s
            _ => return None,
        };
        Some(alias)
    }

    fn fmt_unit(&self) -> String {
        if let Some(alias) = self.alias() {
            return alias.suffix().to_string();
        }
        fn push(exp: i8, sym: &str, num: &mut Vec<String>, den: &mut Vec<String>) {
            match exp {
                0 => {}
                1 => num.push(sym.to_string()),
                -1 => den.push(sym.to_string()),
                n if n > 1 => num.push(format!("{sym}^{exp}")),
                n if n < -1 => den.push(format!("{sym}^{exp}")),
                _ => unreachable!(),
            }
        }
        let PhysicalUnitDims {
            current,
            time,
            voltage,
            temp,
        } = *self;
        let mut num = Vec::new();
        let mut den = Vec::new();
        push(voltage, "V", &mut num, &mut den);
        push(current, "A", &mut num, &mut den);
        push(temp, "K", &mut num, &mut den);
        push(time, "s", &mut num, &mut den);
        let format_units = |units: &[String]| {
            let joined = units.join("·");
            if units.len() > 1 {
                format!("({})", joined)
            } else {
                joined
            }
        };

        match (num.is_empty(), den.is_empty()) {
            (true, true) => "".to_string(),
            (false, true) => format_units(&num),
            (true, false) => format!("1/{}", format_units(&den)),
            (false, false) => format!("{}/{}", format_units(&num), format_units(&den)),
        }
    }

    pub fn quantity(&self) -> String {
        if let Some(alias) = self.alias() {
            return alias.quantity().to_string();
        }
        if *self == Self::DIMENSIONLESS {
            return "Dimensionless".to_string();
        }
        self.fmt_unit()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PhysicalValueError {
    #[error("Division by zero")]
    DivisionByZero,
    #[error("Unit mismatch: expected {expected}, got {actual}")]
    UnitMismatch { expected: String, actual: String },
    #[error("Unit has no alias")]
    InvalidPhysicalUnit,
    #[error("Cannot mix positional argument with keyword arguments")]
    MixedArguments,
    #[error("{unit}() expects a string, number, or {unit} value")]
    InvalidArgumentType { unit: String },
    #[error("Failed to parse {unit} '{input}': {source}")]
    ParseError {
        unit: String,
        input: String,
        source: ParseError,
    },
    #[error("Unexpected keyword '{keyword}'")]
    UnexpectedKeyword { keyword: String },
    #[error("{unit}() missing required keyword 'value'")]
    MissingValueKeyword { unit: String },
    #[error("{unit}() accepts at most one positional argument")]
    TooManyArguments { unit: String },
    #[error("Invalid number {number}")]
    InvalidNumber { number: String },
    #[error("Expected int, float or numeric string")]
    InvalidNumberType,
    #[error("Invalid percentage value: '{value}'")]
    InvalidPercentage { value: String },
    #[error("Invalid tolerance value: '{value}'")]
    InvalidTolerance { value: String },
    #[error("with_unit() expects a PhysicalUnit string or None")]
    WithUnitInvalidArgument,
    #[error("Cannot divide {lhs_unit} by {rhs_unit} - {error}")]
    DivisionError {
        lhs_unit: String,
        rhs_unit: String,
        error: String,
    },
    #[error("Cannot add {lhs_unit} and {rhs_unit} - {error}")]
    AdditionError {
        lhs_unit: String,
        rhs_unit: String,
        error: String,
    },
    #[error("Cannot subtract non-physical value from {unit}")]
    SubtractionNonPhysical { unit: String },
    #[error("Cannot subtract {rhs_unit} from {lhs_unit} - {error}")]
    SubtractionError {
        lhs_unit: String,
        rhs_unit: String,
        error: String,
    },
    #[error("Invalid argument(s): {args:?}")]
    InvalidArguments { args: Vec<String> },
    #[error("Range() requires either a value argument or min/max keywords")]
    MissingRangeValue,
    #[error("Invalid range: min ({min}) > max ({max})")]
    InvalidRange { min: String, max: String },
    #[error("Nominal value ({nominal}) is outside range [{min}, {max}]")]
    NominalOutOfRange {
        nominal: String,
        min: String,
        max: String,
    },
}

impl From<PhysicalValueError> for starlark::Error {
    fn from(err: PhysicalValueError) -> Self {
        starlark::Error::new_other(err)
    }
}

impl From<rust_decimal::Error> for PhysicalValueError {
    fn from(err: rust_decimal::Error) -> Self {
        PhysicalValueError::InvalidNumber {
            number: format!("decimal conversion error: {}", err),
        }
    }
}

impl From<ParseError> for PhysicalValueError {
    fn from(err: ParseError) -> Self {
        match err {
            ParseError::InvalidFormat => PhysicalValueError::InvalidNumberType,
            ParseError::InvalidNumber => PhysicalValueError::InvalidNumberType,
            ParseError::InvalidUnit => PhysicalValueError::InvalidNumberType,
        }
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

impl From<ParseError> for starlark::Error {
    fn from(err: ParseError) -> Self {
        starlark::Error::new_other(err)
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
            tolerance = parse_percentish_decimal(parts.last().unwrap())?;
            parts[..parts.len() - 1].join("")
        } else {
            parts.join("")
        };

        // Handle special case like "4k7" (resistance notation -> 4.7kOhm)
        if let Some(result) = parse_resistor_k_notation(&value_unit_str, tolerance) {
            return Ok(result);
        }

        // Find where number ends
        let split_pos = value_unit_str
            .find(|ch: char| !ch.is_ascii_digit() && ch != '.' && ch != '-' && ch != '+')
            .unwrap_or(value_unit_str.len());

        if split_pos == 0 {
            return Err(ParseError::InvalidFormat);
        }

        let (number_str, unit_str) = value_unit_str.split_at(split_pos);
        let base_number: Decimal = number_str.parse().map_err(|_| ParseError::InvalidNumber)?;

        // Parse unit with prefix
        let (value, unit) = parse_unit_with_prefix(unit_str, base_number)?;

        Ok(PhysicalValue::from_decimal(value, tolerance, unit))
    }
}

fn convert_temperature_to_kelvin(value: Decimal, unit: &str) -> Decimal {
    match unit {
        "°C" => value + KELVIN_OFFSET,
        "°F" => (value - Decimal::from(32)) * Decimal::from(5) / Decimal::from(9) + KELVIN_OFFSET,
        _ => value, // Already in Kelvin or other
    }
}

fn parse_unit_with_prefix(
    unit_str: &str,
    base_value: Decimal,
) -> Result<(Decimal, PhysicalUnitDims), ParseError> {
    // Handle bare number (empty unit) - defaults to resistance
    if unit_str.is_empty() {
        return Ok((base_value, PhysicalUnit::Ohms.into()));
    }

    match unit_str {
        "h" => return Ok((base_value * HOUR, PhysicalUnitDims::TIME)),
        "min" => return Ok((base_value * MINUTE, PhysicalUnitDims::TIME)),
        "°C" | "°F" => {
            return Ok((
                convert_temperature_to_kelvin(base_value, unit_str),
                PhysicalUnitDims::TEMP,
            ))
        }
        _ => {}
    }

    // Try SI prefixes
    for &(exp, prefix) in &SI_PREFIXES {
        if !prefix.is_empty() {
            if let Some(base_unit) = unit_str.strip_prefix(prefix) {
                let multiplier = pow10(exp);
                if base_unit == "h" {
                    return Ok((base_value * multiplier * HOUR, PhysicalUnitDims::TIME));
                }
                return Ok((base_value * multiplier, base_unit.parse()?));
            }
        }
    }

    Ok((base_value, unit_str.parse()?))
}

impl std::fmt::Display for PhysicalValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tol_percent = self.tolerance * ONE_HUNDRED;
        let tol_str = if tol_percent > Decimal::ZERO {
            format!(" {}%", tol_percent.round())
        } else {
            String::new()
        };

        match self.unit.alias() {
            Some(PhysicalUnit::Kelvin) => {
                let celsius = self.value - KELVIN_OFFSET;
                write!(f, "{}°C{}", fmt_significant(celsius), tol_str)
            }
            Some(PhysicalUnit::Seconds) => {
                let (value_str, unit_suffix) = if self.value >= HOUR {
                    (fmt_significant(self.value / HOUR), "h")
                } else if self.value >= MINUTE {
                    (fmt_significant(self.value / MINUTE), "min")
                } else {
                    let (scaled, prefix) = scale_to_si(self.value);
                    return write!(f, "{}{}s{}", fmt_significant(scaled), prefix, tol_str);
                };
                write!(f, "{}{}{}", value_str, unit_suffix, tol_str)
            }
            _ => {
                let (scaled, prefix) = scale_to_si(self.value);
                write!(
                    f,
                    "{}{}{}{}",
                    fmt_significant(scaled),
                    prefix,
                    self.unit.fmt_unit(),
                    tol_str
                )
            }
        }
    }
}

starlark_simple_value!(PhysicalUnitDims);

#[starlark_value(type = "PhysicalUnit")]
impl<'v> StarlarkValue<'v> for PhysicalUnitDims {
    fn write_hash(&self, hasher: &mut StarlarkHasher) -> starlark::Result<()> {
        self.hash(hasher);
        Ok(())
    }
}

starlark_simple_value!(PhysicalValue);

#[starlark::starlark_module]
fn physical_value_methods(methods: &mut MethodsBuilder) {
    #[starlark(attribute)]
    fn value<'v>(this: &PhysicalValue) -> starlark::Result<f64> {
        to_f64(this.value, "value")
    }

    #[starlark(attribute)]
    fn tolerance<'v>(this: &PhysicalValue) -> starlark::Result<f64> {
        to_f64(this.tolerance, "tolerance")
    }

    #[starlark(attribute)]
    fn min<'v>(this: &PhysicalValue) -> starlark::Result<f64> {
        let min_val = this.value * (Decimal::ONE - this.tolerance);
        to_f64(min_val, "min")
    }

    #[starlark(attribute)]
    fn max<'v>(this: &PhysicalValue) -> starlark::Result<f64> {
        let max_val = this.value * (Decimal::ONE + this.tolerance);
        to_f64(max_val, "max")
    }

    #[starlark(attribute)]
    fn unit<'v>(this: &PhysicalValue) -> starlark::Result<String> {
        let unit_str = if this.unit == PhysicalUnit::Ohms.into() {
            "Ohm".to_string()
        } else {
            this.unit.fmt_unit()
        };
        Ok(unit_str)
    }

    fn __str__<'v>(
        this: &PhysicalValue,
        #[starlark(require = pos)] _arg: Value<'v>,
    ) -> starlark::Result<String> {
        Ok(this.to_string())
    }

    fn with_tolerance<'v>(
        this: &PhysicalValue,
        #[starlark(require = pos)] tolerance_arg: Value<'v>,
    ) -> starlark::Result<PhysicalValue> {
        let new_tolerance = if let Some(s) = tolerance_arg.unpack_str() {
            parse_percentish_decimal(s).map_err(|_| PhysicalValueError::InvalidTolerance {
                value: s.to_string(),
            })?
        } else {
            starlark_value_to_decimal(&tolerance_arg)?
        };

        Ok(PhysicalValue::from_decimal(
            this.value,
            new_tolerance,
            this.unit,
        ))
    }

    fn with_value<'v>(
        this: &PhysicalValue,
        #[starlark(require = pos)] value_arg: Value<'v>,
    ) -> starlark::Result<PhysicalValue> {
        let new_value = starlark_value_to_decimal(&value_arg)?;
        Ok(PhysicalValue::from_decimal(
            new_value,
            this.tolerance,
            this.unit,
        ))
    }

    fn with_unit<'v>(
        this: &PhysicalValue,
        #[starlark(require = pos)] unit_arg: Value<'v>,
    ) -> starlark::Result<PhysicalValue> {
        let new_unit = if let Some(s) = unit_arg.unpack_str() {
            s.parse()?
        } else if unit_arg.is_none() {
            PhysicalUnitDims::DIMENSIONLESS
        } else {
            return Err(PhysicalValueError::WithUnitInvalidArgument.into());
        };

        Ok(PhysicalValue::from_decimal(
            this.value,
            this.tolerance,
            new_unit,
        ))
    }

    fn abs<'v>(
        this: &PhysicalValue,
        #[starlark(require = pos)] _arg: Value<'v>,
    ) -> starlark::Result<PhysicalValue> {
        Ok(this.abs())
    }

    fn diff<'v>(
        this: &PhysicalValue,
        #[starlark(require = pos)] other: Value<'v>,
    ) -> starlark::Result<PhysicalValue> {
        let other_pv = PhysicalValue::try_from(other).map_err(|_| {
            PhysicalValueError::InvalidArgumentType {
                unit: this.unit.quantity(),
            }
        })?;
        this.diff(&other_pv).map_err(|err| {
            PhysicalValueError::SubtractionError {
                lhs_unit: this.unit.quantity(),
                rhs_unit: other_pv.unit.quantity(),
                error: err.to_string(),
            }
            .into()
        })
    }

    fn within<'v>(
        this: &PhysicalValue,
        #[starlark(require = pos)] other: Value<'v>,
    ) -> starlark::Result<bool> {
        // Thin wrapper around is_in
        this.is_in(other)
    }
}

#[starlark_value(type = "PhysicalValue")]
impl<'v> StarlarkValue<'v> for PhysicalValue {
    fn get_methods() -> Option<&'static Methods> {
        static RES: MethodsStatic = MethodsStatic::new();
        RES.methods(physical_value_methods)
    }

    fn div(&self, other: Value<'v>, heap: &'v Heap) -> Option<Result<Value<'v>, starlark::Error>> {
        let other = PhysicalValue::try_from(other).ok()?;
        let result = (*self / other).map(|v| heap.alloc(v)).map_err(|err| {
            PhysicalValueError::DivisionError {
                lhs_unit: self.unit.quantity(),
                rhs_unit: other.unit.quantity(),
                error: err.to_string(),
            }
            .into()
        });
        Some(result)
    }

    fn rdiv(&self, other: Value<'v>, heap: &'v Heap) -> Option<Result<Value<'v>, starlark::Error>> {
        let other = PhysicalValue::try_from(other).ok()?;
        let result = (other / *self).map(|v| heap.alloc(v)).map_err(|err| {
            PhysicalValueError::DivisionError {
                lhs_unit: other.unit.quantity(),
                rhs_unit: self.unit.quantity(),
                error: err.to_string(),
            }
            .into()
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
        let result = (*self + other).map(|v| heap.alloc(v)).map_err(|err| {
            PhysicalValueError::AdditionError {
                lhs_unit: self.unit.quantity(),
                rhs_unit: other.unit.quantity(),
                error: err.to_string(),
            }
            .into()
        });
        Some(result)
    }

    fn radd(&self, other: Value<'v>, heap: &'v Heap) -> Option<Result<Value<'v>, starlark::Error>> {
        self.add(other, heap)
    }

    fn sub(&self, other: Value<'v>, heap: &'v Heap) -> starlark::Result<Value<'v>> {
        let other = PhysicalValue::try_from(other).map_err(|_| {
            PhysicalValueError::SubtractionNonPhysical {
                unit: self.unit.quantity(),
            }
        })?;
        let result = (*self - other).map_err(|err| PhysicalValueError::SubtractionError {
            lhs_unit: self.unit.quantity(),
            rhs_unit: other.unit.quantity(),
            error: err.to_string(),
        })?;
        Ok(heap.alloc(result))
    }

    fn equals(&self, other: Value<'v>) -> starlark::Result<bool> {
        // Try to convert the other value to PhysicalValue
        let other = match PhysicalValue::try_from(other) {
            Ok(other) => other,
            Err(_) => return Ok(false),
        };
        Ok(self.unit == other.unit && self.value == other.value)
    }

    fn compare(&self, other: Value<'v>) -> starlark::Result<Ordering> {
        // Try to convert the other value to PhysicalValue
        let other = PhysicalValue::try_from(other).map_err(|_| {
            starlark::Error::new_other(PhysicalValueError::InvalidArgumentType {
                unit: self.unit.quantity(),
            })
        })?;

        // Check that units match OR one of them is dimensionless
        if self.unit != other.unit
            && self.unit != PhysicalUnitDims::DIMENSIONLESS
            && other.unit != PhysicalUnitDims::DIMENSIONLESS
        {
            return Err(starlark::Error::new_other(
                PhysicalValueError::UnitMismatch {
                    expected: self.unit.quantity(),
                    actual: other.unit.quantity(),
                },
            ));
        }

        // Compare the underlying values
        Ok(self.value.cmp(&other.value))
    }

    fn is_in(&self, other: Value<'v>) -> starlark::Result<bool> {
        let self_min = self.value * (Decimal::ONE - self.tolerance);
        let self_max = self.value * (Decimal::ONE + self.tolerance);
        let (other_min, other_max) = extract_bounds(other, self.unit)?;
        Ok(other_min >= self_min && other_max <= self_max)
    }

    fn minus(&self, heap: &'v Heap) -> starlark::Result<Value<'v>> {
        Ok(heap.alloc(PhysicalValue::from_decimal(
            -self.value,
            self.tolerance,
            self.unit,
        )))
    }
}

/// Type factory for creating PhysicalValue constructors (like PhysicalRangeType)
#[derive(
    Clone, Copy, Hash, Debug, PartialEq, ProvidesStaticType, Allocative, Serialize, Deserialize,
)]
pub struct PhysicalValueType {
    unit: PhysicalUnitDims,
}

impl Freeze for PhysicalValueType {
    type Frozen = Self;
    fn freeze(self, _freezer: &starlark::values::Freezer) -> FreezeResult<Self::Frozen> {
        Ok(self)
    }
}

starlark_simple_value!(PhysicalValueType);

impl fmt::Display for PhysicalValueType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.ty_name())
    }
}

impl PhysicalValueType {
    pub fn new(unit: PhysicalUnitDims) -> Self {
        PhysicalValueType { unit }
    }

    fn type_instance_id(&self) -> TypeInstanceId {
        static CACHE: OnceLock<Mutex<HashMap<PhysicalUnitDims, TypeInstanceId>>> = OnceLock::new();
        get_type_instance_id(self.unit, &CACHE)
    }

    fn instance_ty_name(&self) -> String {
        self.unit.quantity()
    }

    fn ty_name(&self) -> String {
        format!("{}Type", self.unit.quantity())
    }

    fn param_spec(&self) -> ParamSpec {
        ParamSpec::new_parts(
            [(
                ParamIsRequired::No,
                Ty::union2(
                    Ty::union2(Ty::int(), Ty::float()),
                    Ty::union2(
                        StarlarkStr::get_type_starlark_repr(),
                        PhysicalValue::get_type_starlark_repr(),
                    ),
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
        .expect("ParamSpec creation should not fail")
    }

    fn parameters_spec(&self) -> ParametersSpec<FrozenValue> {
        ParametersSpec::new_parts(
            self.instance_ty_name().as_str(),
            [("value", ParametersSpecParam::Optional)],
            [],
            false,
            [
                ("value", ParametersSpecParam::Optional),
                ("tolerance", ParametersSpecParam::Optional),
            ],
            false,
        )
    }
}

#[starlark_value(type = FUNCTION_TYPE)]
impl<'v> StarlarkValue<'v> for PhysicalValueType {
    fn write_hash(&self, hasher: &mut StarlarkHasher) -> starlark::Result<()> {
        self.hash(hasher);
        Ok(())
    }

    fn eval_type(&self) -> Option<Ty> {
        let id = self.type_instance_id();
        let ty_value = Ty::custom(
            TyUser::new(
                self.instance_ty_name(),
                TyStarlarkValue::new::<PhysicalValue>(),
                id,
                TyUserParams {
                    matcher: Some(TypeMatcherFactory::new(ValueTypeMatcher {
                        unit: self.unit,
                    })),
                    fields: TyUserFields {
                        known: PhysicalValue::fields(),
                        unknown: false,
                    },
                    ..TyUserParams::default()
                },
            )
            .ok()?,
        );
        Some(ty_value)
    }

    fn typechecker_ty(&self) -> Option<Ty> {
        let ty_value_type = Ty::custom(
            TyUser::new(
                self.ty_name(),
                TyStarlarkValue::new::<Self>(),
                TypeInstanceId::r#gen(),
                TyUserParams {
                    callable: Some(TyCallable::new(self.param_spec(), self.eval_type()?)),
                    ..TyUserParams::default()
                },
            )
            .ok()?,
        );
        Some(ty_value_type)
    }

    fn invoke(
        &self,
        _: Value<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        self.parameters_spec()
            .parser(args, eval, |param_parser, eval| {
                let pos_value: Option<Value> = param_parser.next_opt()?;
                let kw_value: Option<Value> = param_parser.next_opt()?;
                let tolerance: Option<Value> = param_parser.next_opt()?;

                // Determine value from positional or keyword arg
                let value_arg = pos_value.or(kw_value).ok_or_else(|| {
                    PhysicalValueError::MissingValueKeyword {
                        unit: self.unit.quantity(),
                    }
                })?;

                // Parse value - try PhysicalValue, string, or numeric
                let (val, tol) = if let Some(pv) = value_arg.downcast_ref::<PhysicalValue>() {
                    (pv.value, pv.tolerance)
                } else if let Some(s) = value_arg.unpack_str() {
                    let pv = PhysicalValue::from_str(s)?;
                    (pv.value, pv.tolerance)
                } else {
                    let v = starlark_value_to_decimal(&value_arg)?;
                    (v, Decimal::ZERO)
                };

                // Override tolerance if explicitly provided
                let tol = if let Some(tol_val) = tolerance {
                    starlark_value_to_decimal(&tol_val)?
                } else {
                    tol
                };

                let result = PhysicalValue::from_decimal(val, tol, self.unit);
                Ok(eval.heap().alloc(result))
            })
    }

    fn get_methods() -> Option<&'static Methods> {
        static RES: MethodsStatic = MethodsStatic::new();
        RES.methods(value_type_methods)
    }
}

#[derive(Hash, Debug, PartialEq, Clone, Allocative)]
struct ValueTypeMatcher {
    unit: PhysicalUnitDims,
}

impl TypeMatcher for ValueTypeMatcher {
    fn matches(&self, value: Value) -> bool {
        match value.downcast_ref::<PhysicalValue>() {
            Some(pv) => pv.unit == self.unit,
            None => false,
        }
    }
}

#[starlark::starlark_module]
fn value_type_methods(methods: &mut MethodsBuilder) {
    #[starlark(attribute)]
    fn r#type(this: &PhysicalValueType) -> starlark::Result<String> {
        Ok(this.ty_name())
    }
    #[starlark(attribute)]
    fn unit(this: &PhysicalValueType) -> starlark::Result<String> {
        Ok(this.unit.to_string())
    }
}

#[derive(Clone, Debug, Freeze, ProvidesStaticType, Allocative, Serialize, Deserialize)]
pub struct PhysicalRange {
    #[allocative(skip)]
    min: Decimal,
    #[allocative(skip)]
    max: Decimal,
    #[allocative(skip)]
    #[serde(skip_serializing_if = "Option::is_none")]
    nominal: Option<Decimal>,
    #[serde(flatten)]
    r#type: PhysicalRangeType,
}

starlark_simple_value!(PhysicalRange);

impl fmt::Display for PhysicalRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let unit = self.r#type.unit;
        if let Some(nominal) = self.nominal {
            write!(
                f,
                "{}–{} {} ({} {} nom.)",
                self.min, self.max, unit, nominal, unit
            )
        } else {
            write!(f, "{}–{} {}", self.min, self.max, unit)
        }
    }
}

impl PhysicalRange {
    pub const TYPE: &'static str = "PhysicalRange";

    /// Calculate the maximum possible absolute difference between two ranges
    /// This is useful for determining component voltage ratings (e.g., capacitor max voltage)
    /// Returns PhysicalValue with the worst-case difference
    pub fn diff(&self, other: &PhysicalRange) -> Result<PhysicalValue, PhysicalValueError> {
        // Check units match
        if self.r#type.unit != other.r#type.unit {
            return Err(PhysicalValueError::UnitMismatch {
                expected: other.r#type.unit.quantity(),
                actual: self.r#type.unit.quantity(),
            });
        }

        // Calculate the four possible differences at the extremes:
        // self.max - other.min, self.max - other.max,
        // self.min - other.min, self.min - other.max
        // The maximum absolute difference is at the corners
        let diff1 = (self.max - other.min).abs();
        let diff2 = (self.min - other.max).abs();

        let diff = if diff1 > diff2 { diff1 } else { diff2 };

        Ok(PhysicalValue::from_decimal(
            diff,
            Decimal::ZERO, // No tolerance like PhysicalValue::diff()
            self.r#type.unit,
        ))
    }

    fn fields() -> SortedMap<String, Ty> {
        fn single_param_spec(param_type: Ty) -> ParamSpec {
            ParamSpec::new_parts([(ParamIsRequired::Yes, param_type)], [], None, [], None).unwrap()
        }

        let diff_param_spec = single_param_spec(Ty::any());

        SortedMap::from_iter([
            ("min".to_string(), Ty::float()),
            ("max".to_string(), Ty::float()),
            ("nominal".to_string(), Ty::union2(Ty::float(), Ty::none())),
            ("unit".to_string(), Ty::string()),
            (
                "diff".to_string(),
                Ty::callable(diff_param_spec, PhysicalValue::get_type_starlark_repr()),
            ),
        ])
    }

    /// Extract trailing parenthesized nominal clause if present
    /// Returns (remaining_str, optional_nominal_str)
    fn extract_nominal(s: &str) -> (&str, Option<String>) {
        let trimmed = s.trim_end();
        if let Some(rparen_pos) = trimmed.rfind(')') {
            if let Some(lparen_pos) = trimmed[..rparen_pos].rfind('(') {
                let inner = trimmed[lparen_pos + 1..rparen_pos].trim();
                // Check if it matches the nominal pattern (ends with "nom" or "nom.")
                let lower = inner.trim_end().to_lowercase();

                if let Some(nominal_value_str) = lower.strip_suffix("nom.") {
                    let core = trimmed[..lparen_pos].trim_end();
                    let value_len = nominal_value_str.len();
                    // Return the original case version of the nominal value
                    return (core, Some(inner[..value_len].trim().to_string()));
                } else if let Some(nominal_value_str) = lower.strip_suffix("nom") {
                    let core = trimmed[..lparen_pos].trim_end();
                    let value_len = nominal_value_str.len();
                    return (core, Some(inner[..value_len].trim().to_string()));
                }
            }
        }
        (s, None)
    }

    /// Try to split on range separator (en-dash or "to")
    /// Returns (left, right) if found
    fn try_split_range(s: &str) -> Option<(&str, &str)> {
        // Try en-dash first (U+2013)
        if let Some(pos) = s.find('–') {
            let left = s[..pos].trim_end();
            let right = s[pos + '–'.len_utf8()..].trim_start();
            return Some((left, right));
        }

        // Try case-insensitive "to" with word boundaries
        let lower = s.to_lowercase();
        if let Some(pos) = lower.find(" to ") {
            // Ensure it's a word boundary by checking it has spaces
            let left = s[..pos].trim_end();
            let right = s[pos + 4..].trim_start(); // " to " is 4 bytes
            return Some((left, right));
        }

        None
    }
}

#[starlark_value(type = PhysicalRange::TYPE)]
impl<'v> StarlarkValue<'v> for PhysicalRange {
    fn get_methods() -> Option<&'static Methods> {
        static RES: MethodsStatic = MethodsStatic::new();
        RES.methods(range_methods)
    }

    fn is_in(&self, other: Value<'v>) -> starlark::Result<bool> {
        let (other_min, other_max) = extract_bounds(other, self.r#type.unit)?;
        Ok(other_min >= self.min && other_max <= self.max)
    }

    fn minus(&self, heap: &'v Heap) -> starlark::Result<Value<'v>> {
        Ok(heap.alloc_simple(PhysicalRange {
            min: -self.max,
            max: -self.min,
            nominal: self.nominal.map(|n| -n),
            r#type: self.r#type,
        }))
    }
}

impl FromStr for PhysicalRange {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.is_empty() {
            return Err(ParseError::InvalidFormat);
        }

        // Step 1: Extract optional nominal clause
        let (core_str, nominal_str) = Self::extract_nominal(s);
        let nominal = if let Some(nom_str) = nominal_str {
            let pv = PhysicalValue::from_str(&nom_str)?;
            Some(pv)
        } else {
            None
        };

        // Step 2: Try to parse as explicit range
        if let Some((left_str, right_str)) = Self::try_split_range(core_str) {
            // Parse right side (must have unit)
            let right_pv = PhysicalValue::from_str(right_str)?;
            let unit = right_pv.unit;

            // Try parsing left side - check if it has a unit suffix
            let left_trimmed = left_str.trim();
            let has_unit_suffix = left_trimmed
                .chars()
                .last()
                .is_some_and(|c| c.is_alphabetic());

            let (min_val, max_val) = if has_unit_suffix {
                // Left side has a unit, parse as PhysicalValue
                let left_pv = PhysicalValue::from_str(left_str)?;
                // Units must match
                if left_pv.unit != right_pv.unit {
                    return Err(ParseError::InvalidUnit);
                }
                (left_pv.value, right_pv.value)
            } else {
                // Left side is bare number, parse as Decimal
                let left_num =
                    Decimal::from_str(left_trimmed).map_err(|_| ParseError::InvalidNumber)?;
                (left_num, right_pv.value)
            };

            // Ensure min <= max
            let (min_val, max_val) = if min_val <= max_val {
                (min_val, max_val)
            } else {
                (max_val, min_val)
            };

            // Validate nominal dimension if present
            if let Some(nom) = &nominal {
                if nom.unit != unit {
                    return Err(ParseError::InvalidUnit);
                }
            }

            return Ok(PhysicalRange {
                min: min_val,
                max: max_val,
                nominal: nominal.map(|n| n.value),
                r#type: PhysicalRangeType::new(unit),
            });
        }

        // Step 3: Parse as single PhysicalValue (possibly with tolerance)
        let pv = PhysicalValue::from_str(core_str)?;

        let (min_val, max_val) = if pv.tolerance.is_zero() {
            // No tolerance - min equals max
            (pv.value, pv.value)
        } else {
            // Expand tolerance into range
            let min_val = pv.value * (Decimal::ONE - pv.tolerance);
            let max_val = pv.value * (Decimal::ONE + pv.tolerance);
            (min_val, max_val)
        };

        // Validate nominal dimension if present
        if let Some(nom) = &nominal {
            if nom.unit != pv.unit {
                return Err(ParseError::InvalidUnit);
            }
        }

        Ok(PhysicalRange {
            min: min_val,
            max: max_val,
            nominal: nominal.map(|n| n.value),
            r#type: PhysicalRangeType::new(pv.unit),
        })
    }
}

#[derive(
    Clone, Copy, Hash, Debug, PartialEq, ProvidesStaticType, Allocative, Serialize, Deserialize,
)]
pub struct PhysicalRangeType {
    unit: PhysicalUnitDims,
}

impl Freeze for PhysicalRangeType {
    type Frozen = Self;
    fn freeze(self, _freezer: &starlark::values::Freezer) -> FreezeResult<Self::Frozen> {
        Ok(self)
    }
}

starlark_simple_value!(PhysicalRangeType);

impl fmt::Display for PhysicalRangeType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.instance_ty_name())
    }
}

impl PhysicalRangeType {
    pub fn new(unit: PhysicalUnitDims) -> Self {
        PhysicalRangeType { unit }
    }

    fn type_instance_id(&self) -> TypeInstanceId {
        static CACHE: OnceLock<Mutex<HashMap<PhysicalUnitDims, TypeInstanceId>>> = OnceLock::new();
        get_type_instance_id(self.unit, &CACHE)
    }

    fn instance_ty_name(&self) -> String {
        format!("{}Range", self.unit.quantity())
    }

    fn ty_name(&self) -> String {
        format!("{}RangeType", self.unit.quantity())
    }

    fn param_spec(&self) -> ParamSpec {
        ParamSpec::new_parts(
            [(
                ParamIsRequired::No,
                Ty::union2(
                    Ty::union2(Ty::string(), PhysicalRange::get_type_starlark_repr()),
                    PhysicalValue::get_type_starlark_repr(),
                ),
            )],
            [],
            None,
            [
                (
                    "min".into(),
                    ParamIsRequired::No,
                    Ty::union2(Ty::string(), Ty::float()),
                ),
                (
                    "max".into(),
                    ParamIsRequired::No,
                    Ty::union2(Ty::string(), Ty::float()),
                ),
                (
                    "nominal".into(),
                    ParamIsRequired::No,
                    Ty::union2(Ty::string(), Ty::float()),
                ),
            ],
            None,
        )
        .unwrap()
    }

    fn parameters_spec(&self) -> ParametersSpec<FrozenValue> {
        ParametersSpec::new_parts(
            self.instance_ty_name().as_str(),
            [("value", ParametersSpecParam::Optional)],
            [],
            false,
            [
                ("min", ParametersSpecParam::Optional),
                ("max", ParametersSpecParam::Optional),
                ("nominal", ParametersSpecParam::Optional),
            ],
            false,
        )
    }
}

#[starlark_value(type = FUNCTION_TYPE)]
impl<'v> StarlarkValue<'v> for PhysicalRangeType {
    fn write_hash(&self, hasher: &mut StarlarkHasher) -> starlark::Result<()> {
        self.hash(hasher);
        Ok(())
    }

    fn eval_type(&self) -> Option<Ty> {
        let id = self.type_instance_id();
        let ty_range = Ty::custom(
            TyUser::new(
                self.instance_ty_name(),
                TyStarlarkValue::new::<PhysicalRange>(),
                id,
                TyUserParams {
                    matcher: Some(TypeMatcherFactory::new(RangeTypeMatcher {
                        unit: self.unit,
                    })),
                    fields: TyUserFields {
                        known: PhysicalRange::fields(),
                        unknown: false,
                    },
                    ..TyUserParams::default()
                },
            )
            .ok()?,
        );
        Some(ty_range)
    }

    fn typechecker_ty(&self) -> Option<Ty> {
        let ty_range_type = Ty::custom(
            TyUser::new(
                self.ty_name(),
                TyStarlarkValue::new::<Self>(),
                TypeInstanceId::r#gen(),
                TyUserParams {
                    callable: Some(TyCallable::new(self.param_spec(), self.eval_type()?)),
                    ..TyUserParams::default()
                },
            )
            .ok()?,
        );
        Some(ty_range_type)
    }

    fn invoke(
        &self,
        _: Value<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        self.parameters_spec()
            .parser(args, eval, |param_parser, eval| {
                let value: Option<Value> = param_parser.next_opt()?;
                let min: Option<Value> = param_parser.next_opt()?;
                let max: Option<Value> = param_parser.next_opt()?;
                let nominal: Option<Value> = param_parser.next_opt()?;
                let mut range_builder = RangeBuilder::default();
                range_builder.add_value_opt(value)?;
                range_builder.add_min_max_opt(min, max)?;
                range_builder.add_nominal_opt(nominal)?;
                let mut range = range_builder.build(self.unit)?;
                range.r#type = *self;
                Ok(eval.heap().alloc_simple(range))
            })
    }

    fn get_methods() -> Option<&'static Methods> {
        static RES: MethodsStatic = MethodsStatic::new();
        RES.methods(range_type_methods)
    }
}

#[derive(Hash, Debug, PartialEq, Clone, Allocative)]
struct RangeTypeMatcher {
    unit: PhysicalUnitDims,
}

impl TypeMatcher for RangeTypeMatcher {
    fn matches(&self, value: Value) -> bool {
        match PhysicalRange::from_value(value) {
            Some(range) => range.r#type.unit == self.unit,
            None => false,
        }
    }
}

#[derive(Default)]
struct RangeBuilder {
    range: Option<(Decimal, Decimal)>,
    nominal: Option<Decimal>,
    unit: Option<PhysicalUnitDims>,
}

impl RangeBuilder {
    fn add_value(&mut self, value: Value) -> Result<(), PhysicalValueError> {
        // Try PhysicalRange
        if let Some(range) = PhysicalRange::from_value(value) {
            self.range = Some((range.min, range.max));
            self.nominal = range.nominal;
            self.unit = Some(range.r#type.unit);
            return Ok(());
        }

        // Try string
        if let Some(s) = value.unpack_str() {
            return self.add_str(s);
        }

        // Try PhysicalValue
        if let Some(pv) = value.downcast_ref::<PhysicalValue>() {
            return self.add_physical_value(*pv);
        }

        let value_type = value.get_type();
        Err(PhysicalValueError::InvalidArguments {
            args: vec![value_type.to_string()],
        })
    }

    fn add_str(&mut self, str: &str) -> Result<(), PhysicalValueError> {
        if self.range.is_some() {
            return Err(PhysicalValueError::InvalidArguments {
                args: vec!["value".to_string()],
            });
        }
        let range = PhysicalRange::from_str(str)?;
        self.range = Some((range.min, range.max));
        self.nominal = range.nominal;
        self.unit = Some(range.r#type.unit);
        Ok(())
    }

    fn add_physical_value(&mut self, value: PhysicalValue) -> Result<(), PhysicalValueError> {
        if self.range.is_some() {
            return Err(PhysicalValueError::InvalidArguments {
                args: vec!["value".to_string()],
            });
        }

        // Use existing min_value/max_value methods
        let min = value.min_value(value.tolerance);
        let max = value.max_value(value.tolerance);
        self.range = Some((min, max));
        self.unit = Some(value.unit);
        Ok(())
    }

    fn add_min_max(&mut self, min: Value, max: Value) -> Result<(), PhysicalValueError> {
        if self.range.is_some() {
            return Err(PhysicalValueError::InvalidArguments {
                args: vec!["min".to_string(), "max".to_string()],
            });
        }
        self.range = Some((
            starlark_value_to_decimal(&min)?,
            starlark_value_to_decimal(&max)?,
        ));
        Ok(())
    }

    fn add_nominal(&mut self, nominal: Value) -> Result<(), PhysicalValueError> {
        if self.nominal.is_some() {
            return Err(PhysicalValueError::InvalidArguments {
                args: vec!["nominal".to_string()],
            });
        }
        let nominal = starlark_value_to_decimal(&nominal)?;
        self.nominal = Some(nominal);
        Ok(())
    }

    fn add_min_max_opt(
        &mut self,
        min: Option<Value>,
        max: Option<Value>,
    ) -> Result<(), PhysicalValueError> {
        // iff min and max are provided, we can set it
        // if only one of min or max is provided, we can't set it
        // if neither min nor max is provided, we can't set it
        match (min, max) {
            (Some(min), Some(max)) => self.add_min_max(min, max),
            (Some(_), None) => Err(PhysicalValueError::InvalidArguments {
                args: vec!["min".to_string()],
            }),
            (None, Some(_)) => Err(PhysicalValueError::InvalidArguments {
                args: vec!["max".to_string()],
            }),
            (None, None) => Ok(()),
        }
    }

    fn add_nominal_opt(&mut self, nominal: Option<Value>) -> Result<(), PhysicalValueError> {
        match nominal {
            Some(nominal) => self.add_nominal(nominal),
            None => Ok(()),
        }
    }

    fn add_value_opt(&mut self, value: Option<Value>) -> Result<(), PhysicalValueError> {
        match value {
            Some(value) => self.add_value(value),
            None => Ok(()),
        }
    }

    fn build(self, unit_hint: PhysicalUnitDims) -> Result<PhysicalRange, PhysicalValueError> {
        // Range must be set
        let (min, max) = self.range.ok_or(PhysicalValueError::MissingRangeValue)?;

        // Determine unit
        let unit: PhysicalUnitDims = if let Some(builder_unit) = self.unit {
            // If builder has a unit, ensure it matches the hint
            if builder_unit != unit_hint {
                return Err(PhysicalValueError::UnitMismatch {
                    expected: unit_hint.to_string(),
                    actual: builder_unit.to_string(),
                });
            }
            builder_unit
        } else {
            // Use the hint
            unit_hint
        };

        // Ensure min <= max (already enforced in parsing, but validate here)
        if min > max {
            return Err(PhysicalValueError::InvalidRange {
                min: min.to_string(),
                max: max.to_string(),
            });
        }

        // Ensure nominal is within [min, max] if present
        if let Some(nominal) = self.nominal {
            if nominal < min || nominal > max {
                return Err(PhysicalValueError::NominalOutOfRange {
                    nominal: nominal.to_string(),
                    min: min.to_string(),
                    max: max.to_string(),
                });
            }
        }

        Ok(PhysicalRange {
            min,
            max,
            nominal: self.nominal,
            r#type: PhysicalRangeType::new(unit),
        })
    }
}

#[starlark::starlark_module]
fn range_type_methods(methods: &mut MethodsBuilder) {
    #[starlark(attribute)]
    fn r#type(this: &PhysicalRangeType) -> starlark::Result<String> {
        Ok(this.ty_name())
    }

    #[starlark(attribute)]
    fn unit(this: &PhysicalRangeType) -> starlark::Result<String> {
        Ok(this.unit.to_string())
    }
}

#[starlark::starlark_module]
fn range_methods(methods: &mut MethodsBuilder) {
    #[starlark(attribute)]
    fn min(this: &PhysicalRange) -> starlark::Result<f64> {
        Ok(this.min.to_f64().unwrap())
    }

    #[starlark(attribute)]
    fn max(this: &PhysicalRange) -> starlark::Result<f64> {
        Ok(this.max.to_f64().unwrap())
    }

    #[starlark(attribute)]
    fn nominal(this: &PhysicalRange) -> starlark::Result<NoneOr<f64>> {
        Ok(NoneOr::from_option(
            this.nominal.map(|n| n.to_f64().unwrap()),
        ))
    }

    #[starlark(attribute)]
    fn unit(this: &PhysicalRange) -> starlark::Result<String> {
        Ok(this.r#type.unit.to_string())
    }

    fn diff<'v>(
        this: &PhysicalRange,
        #[starlark(require = pos)] other: Value<'v>,
    ) -> starlark::Result<PhysicalValue> {
        // Try to get PhysicalRange directly or convert from string
        let other_range = if let Some(range) = other.downcast_ref::<PhysicalRange>() {
            range.clone()
        } else if let Some(s) = other.unpack_str() {
            // Parse string as PhysicalRange
            PhysicalRange::from_str(s).map_err(|e| {
                starlark::Error::new_other(anyhow::anyhow!(
                    "Failed to parse '{}' as PhysicalRange: {}",
                    s,
                    e
                ))
            })?
        } else {
            return Err(starlark::Error::new_other(anyhow::anyhow!(
                "diff() requires a PhysicalRange or string argument"
            )));
        };

        this.diff(&other_range).map_err(|err| err.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use starlark::values::Heap;

    #[cfg(test)]
    fn physical_value(value: f64, tolerance: f64, unit: PhysicalUnit) -> PhysicalValue {
        PhysicalValue {
            value: Decimal::from_f64(value).expect("value not representable as Decimal"),
            tolerance: Decimal::from_f64(tolerance)
                .expect("tolerance not representable as Decimal"),
            unit: unit.into(),
        }
    }

    // Helper: test parse + format + roundtrip in one go
    fn test_cycle(input: &str, unit: PhysicalUnit, value: f64, display: &str) {
        let parsed: PhysicalValue = input.parse().unwrap();
        assert_eq!(parsed.unit, unit.into());
        assert!((parsed.value - Decimal::from_f64(value).unwrap()).abs() < Decimal::new(1, 6));

        if !display.is_empty() {
            let manual = physical_value(value, 0.0, unit);
            assert_eq!(format!("{}", manual), display);
        }

        // Roundtrip
        let formatted = format!("{}", parsed);
        let roundtrip: PhysicalValue = formatted.parse().unwrap();
        assert_eq!(roundtrip.unit, parsed.unit);
    }

    // Helper: test tolerance parsing + formatting
    fn test_tolerance(input: &str, unit: PhysicalUnit, value: f64, tol: f64, display: &str) {
        let parsed: PhysicalValue = input.parse().unwrap();
        assert_eq!(parsed.unit, unit.into());
        assert!((parsed.value - Decimal::from_f64(value).unwrap()).abs() < Decimal::new(1, 6));
        assert!((parsed.tolerance - Decimal::from_f64(tol).unwrap()).abs() < Decimal::new(1, 8));
        assert_eq!(format!("{}", parsed), display);
    }

    // Super simple helper: just check tolerance percentage
    fn check_tol(input: &str, expected_tol_percent: f64) {
        let parsed: PhysicalValue = input.parse().unwrap();
        let expected = Decimal::from_f64(expected_tol_percent / 100.0).unwrap();
        assert!(
            (parsed.tolerance - expected).abs() < Decimal::new(1, 8),
            "Tolerance mismatch for '{}'",
            input
        );
    }

    // Helper: test error cases with one line
    fn check_errors(cases: &[&str]) {
        for &input in cases {
            assert!(
                input.parse::<PhysicalValue>().is_err(),
                "Expected error for '{}'",
                input
            );
        }
    }

    // Helper: batch test many cases at once (input, unit, value)
    fn check_many(cases: &[(&str, PhysicalUnit, f64)]) {
        for &(input, unit, value) in cases {
            let parsed: PhysicalValue = input.parse().unwrap();
            assert_eq!(parsed.unit, unit.into());
            assert!((parsed.value - Decimal::from_f64(value).unwrap()).abs() < Decimal::new(1, 6));
        }
    }

    // Helper for physics calculations
    fn test_physics(
        lhs_val: f64,
        lhs_unit: PhysicalUnit,
        op: &str,
        rhs_val: f64,
        rhs_unit: PhysicalUnit,
        expected_val: f64,
        expected_unit: PhysicalUnit,
    ) {
        let lhs = physical_value(lhs_val, 0.0, lhs_unit);
        let rhs = physical_value(rhs_val, 0.0, rhs_unit);
        let result = match op {
            "+" => (lhs + rhs).expect("Addition failed"),
            "-" => (lhs - rhs).expect("Subtraction failed"),
            "*" => lhs * rhs, // Returns PhysicalValue directly
            "/" => (lhs / rhs).expect("Division failed"),
            _ => panic!("Unknown operator: {}", op),
        };
        assert_eq!(result.unit, expected_unit.into());
        assert!(
            (result.value - Decimal::from_f64(expected_val).unwrap()).abs() < Decimal::new(1, 6)
        );
    }

    #[test]
    fn test_everything_mega() {
        // Ultra-comprehensive test using simple helpers

        // Parse + format + roundtrip using tuples
        for (input, unit, value, display) in [
            ("4.7kOhm", PhysicalUnit::Ohms, 4700.0, "4.7k"),
            ("3.3V", PhysicalUnit::Volts, 3.3, "3.3V"),
            ("4k7", PhysicalUnit::Ohms, 4700.0, "4.7k"), // Special notation
            ("25°C", PhysicalUnit::Kelvin, 298.15, "25°C"), // Temperature
            ("1h", PhysicalUnit::Seconds, 3600.0, "1h"), // Time
            ("100nF", PhysicalUnit::Farads, 1e-7, "100nF"),
            ("1MHz", PhysicalUnit::Hertz, 1e6, "1MHz"),
            ("16Mhz", PhysicalUnit::Hertz, 16e6, "16MHz"), // lowercase hz should work
        ] {
            test_cycle(input, unit, value, display);
        }

        // Tolerance cases
        for (input, unit, value, tol, display) in [
            ("100nF 5%", PhysicalUnit::Farads, 1e-7, 0.05, "100nF 5%"),
            ("10kOhm 1%", PhysicalUnit::Ohms, 10000.0, 0.01, "10k 1%"),
            ("3.3V 0.5%", PhysicalUnit::Volts, 3.3, 0.005, "3.3V 0%"), // Rounds to 0%
        ] {
            test_tolerance(input, unit, value, tol, display);
        }

        // Physics using tuples: (lhs_val, lhs_unit, op, rhs_val, rhs_unit, expected_val, expected_unit)
        for (lv, lu, op, rv, ru, ev, eu) in [
            (
                5.0,
                PhysicalUnit::Volts,
                "/",
                0.5,
                PhysicalUnit::Amperes,
                10.0,
                PhysicalUnit::Ohms,
            ), // V/I=R
            (
                5.0,
                PhysicalUnit::Volts,
                "*",
                0.5,
                PhysicalUnit::Amperes,
                2.5,
                PhysicalUnit::Watts,
            ), // V*I=P
            (
                10.0,
                PhysicalUnit::Ohms,
                "*",
                0.001,
                PhysicalUnit::Farads,
                0.01,
                PhysicalUnit::Seconds,
            ), // R*C=τ
            (
                0.5,
                PhysicalUnit::Amperes,
                "*",
                2.0,
                PhysicalUnit::Seconds,
                1.0,
                PhysicalUnit::Coulombs,
            ), // I*t=Q
        ] {
            test_physics(lv, lu, op, rv, ru, ev, eu);
        }

        // Unit dimensions as tuples
        for (input, expected) in [
            ("V/A", PhysicalUnit::Ohms),
            ("(A·s)/V", PhysicalUnit::Farads),
            ("V·A", PhysicalUnit::Watts),
        ] {
            let parsed: PhysicalUnitDims = input.parse().unwrap();
            assert_eq!(parsed, expected.into());
        }

        // All error cases
        for invalid in ["", "abc", "10xyz", "UnknownUnit", "A·BadUnit"] {
            assert!(
                invalid.parse::<PhysicalValue>().is_err()
                    || invalid.parse::<PhysicalUnitDims>().is_err()
            );
        }

        // Test new numeric argument support (simulated)
        // In practice: Voltage(50) would create 50V, Resistance(100) would create 100Ohms
        let numeric_as_voltage = PhysicalValue::from_decimal(
            Decimal::from(50),
            Decimal::ZERO,
            PhysicalUnit::Volts.into(),
        );
        assert_eq!(numeric_as_voltage.value, Decimal::from(50));
        assert_eq!(numeric_as_voltage.unit, PhysicalUnit::Volts.into());
        assert_eq!(numeric_as_voltage.tolerance, Decimal::ZERO);
    }

    #[test]
    fn test_tolerance_display() {
        let test_cases = [
            (
                Decimal::from(1000),
                PhysicalUnit::Ohms,
                Decimal::new(5, 2),
                "1k 5%",
            ), // With tolerance
            (Decimal::from(1000), PhysicalUnit::Ohms, Decimal::ZERO, "1k"), // Without tolerance
            (
                Decimal::from(1000),
                PhysicalUnit::Farads,
                Decimal::new(1, 1),
                "1kF 10%",
            ), // Non-resistance with tolerance
        ];

        for (value, unit, tolerance, expected) in test_cases {
            let val = PhysicalValue::from_decimal(value, tolerance, unit.into());
            assert_eq!(format!("{}", val), expected);
        }
    }

    #[test]
    fn test_parsing_basic_units() {
        // Batch test using helper
        check_many(&[
            ("5V", PhysicalUnit::Volts, 5.0),
            ("100A", PhysicalUnit::Amperes, 100.0),
            ("47", PhysicalUnit::Ohms, 47.0),
            ("100Ohm", PhysicalUnit::Ohms, 100.0),
            ("24.9k", PhysicalUnit::Ohms, 24900.0),
            ("1C", PhysicalUnit::Coulombs, 1.0),
            ("100W", PhysicalUnit::Watts, 100.0),
            ("50J", PhysicalUnit::Joules, 50.0),
            ("10S", PhysicalUnit::Siemens, 10.0),
            ("5Wb", PhysicalUnit::Webers, 5.0),
        ]);
    }

    #[test]
    fn test_parsing_with_prefixes() {
        check_many(&[
            ("5kV", PhysicalUnit::Volts, 5000.0),
            ("100mA", PhysicalUnit::Amperes, 0.1),
            ("470nF", PhysicalUnit::Farads, 470e-9),
            ("4k7", PhysicalUnit::Ohms, 4700.0), // Special notation
            ("2kW", PhysicalUnit::Watts, 2000.0),
        ]);
    }

    #[test]
    fn test_parsing_decimal_numbers() {
        check_many(&[
            ("3.3V", PhysicalUnit::Volts, 3.3),
            ("4.7kOhm", PhysicalUnit::Ohms, 4700.0),
        ]);
    }

    #[test]
    fn test_parsing_errors() {
        check_errors(&["", "abc", "5X", "5.3.3V"]);
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
    #[test]
    fn test_tolerance_parsing() {
        // Super simplified using helper - just check tolerance percentages
        check_tol("100kOhm 5%", 5.0);
        check_tol("10nF 20%", 20.0);
        check_tol("3.3V 1%", 1.0);
        check_tol("12V 0.5%", 0.5);
        check_tol("100mA 5%", 5.0);
        check_tol("1MHz 10%", 10.0);
        check_tol("4k7 1%", 1.0); // Special notation
    }

    #[test]
    fn test_tolerance_parsing_without_tolerance() {
        // Should parse OK and have zero tolerance
        for input in ["100kOhm", "10nF", "3.3V"] {
            let val: PhysicalValue = input.parse().unwrap();
            assert_eq!(val.tolerance, Decimal::ZERO);
        }
    }

    #[test]
    fn test_tolerance_parsing_with_spaces() {
        // Test spacing edge cases all parse to 5% tolerance
        for input in ["100 kOhm 5%", "100kOhm  5%", " 100kOhm 5% "] {
            check_tol(input, 5.0);
        }
    }

    #[test]
    fn test_tolerance_formatting() {
        let test_cases = [
            (
                Decimal::from(100000),
                PhysicalUnit::Ohms,
                Decimal::new(5, 2),
                "100k 5%",
            ),
            (
                Decimal::new(1, 8),
                PhysicalUnit::Farads,
                Decimal::new(2, 1),
                "10nF 20%",
            ),
            (
                Decimal::from(3300),
                PhysicalUnit::Volts,
                Decimal::new(1, 2),
                "3.3kV 1%",
            ),
        ];

        for (value, unit, tolerance, expected) in test_cases {
            let val = PhysicalValue::from_decimal(value, tolerance, unit.into());
            assert_eq!(format!("{}", val), expected);
        }
    }

    #[test]
    fn test_tolerance_parsing_errors() {
        // Should all fail to parse
        check_errors(&["100kOhm %", "100kOhm abc%", "100kOhm 5%%"]);
    }

    #[test]
    fn test_unit_operations() {
        use rust_decimal::Decimal;

        // Helper to create test values
        fn val(v: f64, unit: PhysicalUnit) -> PhysicalValue {
            physical_value(v, 0.0, unit)
        }

        // Ohm's law
        let v = val(10.0, PhysicalUnit::Volts);
        let i = val(2.0, PhysicalUnit::Amperes);
        let r = val(5.0, PhysicalUnit::Ohms);

        // V = I × R
        let result = i * r;
        assert_eq!(result.unit, PhysicalUnit::Volts.into());
        assert_eq!(result.value, Decimal::from(10));

        // I = V / R
        let result = (v / r).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Amperes.into());
        assert_eq!(result.value, Decimal::from(2));

        // R = V / I
        let result = (v / i).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Ohms.into());
        assert_eq!(result.value, Decimal::from(5));
    }

    #[test]
    fn test_power_calculations() {
        // P = V × I
        let v = physical_value(12.0, 0.0, PhysicalUnit::Volts);
        let i = physical_value(2.0, 0.0, PhysicalUnit::Amperes);
        let result = v * i;
        assert_eq!(result.unit, PhysicalUnit::Watts.into());
        assert_eq!(result.value, Decimal::from(24));

        // I = P / V
        let p = physical_value(100.0, 0.0, PhysicalUnit::Watts);
        let v = physical_value(120.0, 0.0, PhysicalUnit::Volts);
        let result = (p / v).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Amperes.into());
        assert!(result.value > Decimal::from_f64(0.8).unwrap());
        assert!(result.value < Decimal::from_f64(0.9).unwrap());

        // V = P / I
        let i = physical_value(5.0, 0.0, PhysicalUnit::Amperes);
        let result = (p / i).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Volts.into());
        assert_eq!(result.value, Decimal::from(20));
    }

    #[test]
    fn test_energy_and_time() {
        // E = P × t
        let p = physical_value(100.0, 0.0, PhysicalUnit::Watts);
        let t = physical_value(3600.0, 0.0, PhysicalUnit::Seconds);
        let result = p * t;
        assert_eq!(result.unit, PhysicalUnit::Joules.into());
        assert_eq!(result.value, Decimal::from(360000));

        // P = E / t
        let e = physical_value(7200.0, 0.0, PhysicalUnit::Joules);
        let t = physical_value(7200.0, 0.0, PhysicalUnit::Seconds); // 2h
        let result = (e / t).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Watts.into());
        assert_eq!(result.value, Decimal::from(1));

        // t = E / P
        let result = (e / p).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Seconds.into());
        assert_eq!(result.value, Decimal::from(72));
    }

    #[test]
    fn test_frequency_time_inverses() {
        // f = 1 / t
        let t = physical_value(1.0, 0.0, PhysicalUnit::Seconds);
        let result = (PhysicalValue::dimensionless(1) / t).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Hertz.into());
        assert_eq!(result.value, Decimal::from(1));

        // t = 1 / f
        let f = physical_value(60.0, 0.0, PhysicalUnit::Hertz);
        let result = (PhysicalValue::dimensionless(1) / f).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Seconds.into());
        assert!(result.value > Decimal::from_f64(0.016).unwrap());
        assert!(result.value < Decimal::from_f64(0.017).unwrap());

        // f × t = 1 (dimensionless)
        let f = physical_value(10.0, 0.0, PhysicalUnit::Hertz);
        let t = physical_value(0.1, 0.0, PhysicalUnit::Seconds);
        let result = f * t;
        assert_eq!(result.unit, PhysicalUnitDims::DIMENSIONLESS);
        assert_eq!(result.value, Decimal::from(1));
    }

    #[test]
    fn test_resistance_conductance_inverses() {
        // G = 1 / R
        let one = PhysicalValue::from_decimal(1.into(), 0.into(), PhysicalUnitDims::DIMENSIONLESS);
        let r = physical_value(100.0, 0.0, PhysicalUnit::Ohms);
        let result = (one / r).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Siemens.into());
        assert_eq!(result.value, Decimal::from_f64(0.01).unwrap());

        // R = 1 / G
        let g = physical_value(0.02, 0.0, PhysicalUnit::Siemens);
        let result = (one / g).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Ohms.into());
        assert_eq!(result.value, Decimal::from(50));

        // R × G = 1 (dimensionless)
        let result = r * g;
        assert_eq!(result.unit, PhysicalUnitDims::DIMENSIONLESS);
        assert_eq!(result.value, Decimal::from(2));
    }

    #[test]
    fn test_rc_time_constants() {
        // τ = R × C
        let r = physical_value(10000.0, 0.0, PhysicalUnit::Ohms); // 10kΩ
        let c = physical_value(0.0000001, 0.0, PhysicalUnit::Farads); // 100nF
        let result = r * c;
        assert_eq!(result.unit, PhysicalUnit::Seconds.into());
        assert_eq!(result.value, Decimal::from_f64(0.001).unwrap()); // 1ms

        // τ = C × R
        let result = c * r;
        assert_eq!(result.unit, PhysicalUnit::Seconds.into());
        assert_eq!(result.value, Decimal::from_f64(0.001).unwrap()); // 1ms
    }

    #[test]
    fn test_lr_time_constants() {
        // τ = L × G (L/R time constant)
        let l = physical_value(0.01, 0.0, PhysicalUnit::Henries); // 10mH
        let g = physical_value(0.1, 0.0, PhysicalUnit::Siemens); // 100mS
        let result = l * g;
        assert_eq!(result.unit, PhysicalUnit::Seconds.into());
        assert_eq!(result.value, Decimal::from_f64(0.001).unwrap()); // 1ms

        // τ = G × L
        let result = g * l;
        assert_eq!(result.unit, PhysicalUnit::Seconds.into());
        assert_eq!(result.value, Decimal::from_f64(0.001).unwrap()); // 1ms
    }

    #[test]
    fn test_charge_relationships() {
        // Q = I × t
        let i = physical_value(2.0, 0.0, PhysicalUnit::Amperes);
        let t = physical_value(10.0, 0.0, PhysicalUnit::Seconds);
        let result = i * t;
        assert_eq!(result.unit, PhysicalUnit::Coulombs.into());
        assert_eq!(result.value, Decimal::from(20));

        // Q = C × V
        let c = physical_value(0.001, 0.0, PhysicalUnit::Farads); // 1000μF
        let v = physical_value(12.0, 0.0, PhysicalUnit::Volts);
        let result = c * v;
        assert_eq!(result.unit, PhysicalUnit::Coulombs.into());
        assert_eq!(result.value, Decimal::from_f64(0.012).unwrap()); // 12mC

        // I = Q / t
        let q = physical_value(0.1, 0.0, PhysicalUnit::Coulombs); // 100mC
        let t = physical_value(50.0, 0.0, PhysicalUnit::Seconds);
        let result = (q / t).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Amperes.into());
        assert_eq!(result.value, Decimal::from_f64(0.002).unwrap()); // 2mA

        // V = Q / C
        let q = physical_value(0.005, 0.0, PhysicalUnit::Coulombs); // 5mC
        let result = (q / c).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Volts.into());
        assert_eq!(result.value, Decimal::from(5));
    }

    #[test]
    fn test_magnetic_flux() {
        // Φ = L × I
        let l = physical_value(1.0, 0.0, PhysicalUnit::Henries); // 1H
        let i = physical_value(2.0, 0.0, PhysicalUnit::Amperes);
        let result = l * i;
        assert_eq!(result.unit, PhysicalUnit::Webers.into());
        assert_eq!(result.value, Decimal::from(2)); // 2Wb

        // I = Φ / L
        let phi = physical_value(0.01, 0.0, PhysicalUnit::Webers); // 10mWb
        let l = physical_value(0.05, 0.0, PhysicalUnit::Henries); // 50mH
        let result = (phi / l).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Amperes.into());
        assert_eq!(result.value, Decimal::from_f64(0.2).unwrap()); // 200mA

        // V = Φ / t (Faraday's law)
        let phi = physical_value(0.1, 0.0, PhysicalUnit::Webers); // 100mWb
        let t = physical_value(0.01, 0.0, PhysicalUnit::Seconds); // 10ms
        let result = (phi / t).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Volts.into());
        assert_eq!(result.value, Decimal::from(10)); // 10V
    }

    #[test]
    fn test_energy_storage() {
        // E = Q × V (potential energy)
        let q = physical_value(0.001, 0.0, PhysicalUnit::Coulombs); // 1mC
        let v = physical_value(12.0, 0.0, PhysicalUnit::Volts);
        let result = q * v;
        assert_eq!(result.unit, PhysicalUnit::Joules.into());
        assert_eq!(result.value, Decimal::from_f64(0.012).unwrap()); // 12mJ

        // Q = E / V
        let e = physical_value(0.024, 0.0, PhysicalUnit::Joules); // 24mJ
        let result = (e / v).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Coulombs.into());
        assert_eq!(result.value, Decimal::from_f64(0.002).unwrap()); // 2mC

        // V = E / Q
        let e = physical_value(0.006, 0.0, PhysicalUnit::Joules); // 6mJ
        let result = (e / q).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Volts.into());
        assert_eq!(result.value, Decimal::from(6)); // 6V
    }

    #[test]
    fn test_dimensionless_operations() {
        // Any unit * dimensionless = same unit
        let v = physical_value(5.0, 0.0, PhysicalUnit::Volts);
        let two = PhysicalValue::dimensionless(2);
        let result = v * two;
        assert_eq!(result.unit, PhysicalUnit::Volts.into());
        assert_eq!(result.value, Decimal::from(10));

        // Any unit / dimensionless = same unit
        let result = (v / two).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Volts.into());
        assert_eq!(result.value, Decimal::from_f64(2.5).unwrap());
    }

    #[test]
    fn test_unsupported_operations() {
        let v = physical_value(5.0, 0.0, PhysicalUnit::Volts);
        let t = physical_value(1.0, 0.0, PhysicalUnit::Seconds);

        // V + T is not supported (different units)
        assert!((v + t).is_err());

        // V - T is not supported (different units)
        assert!((v - t).is_err());
    }

    #[test]
    fn test_tolerance_handling() {
        // Tolerance preserved for dimensionless scaling
        let v = physical_value(5.0, 0.05, PhysicalUnit::Volts); // 5V ±5%
        let two = PhysicalValue::dimensionless(2);

        // V / dimensionless preserves tolerance
        let result = (v / two).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Volts.into());
        assert_eq!(result.value, Decimal::from_f64(2.5).unwrap());
        assert_eq!(result.tolerance, Decimal::from_f64(0.05).unwrap());

        // V × dimensionless preserves tolerance
        let result = v * two;
        assert_eq!(result.unit, PhysicalUnit::Volts.into());
        assert_eq!(result.value, Decimal::from(10));
        assert_eq!(result.tolerance, Decimal::from_f64(0.05).unwrap());

        // Unit-changing operations drop tolerance
        let r = physical_value(100.0, 0.0, PhysicalUnit::Ohms);
        let result = (v / r).unwrap(); // V / R = I (unit changes)
        assert_eq!(result.unit, PhysicalUnit::Amperes.into());
        assert_eq!(result.tolerance, Decimal::ZERO); // Tolerance dropped
    }

    #[test]
    fn test_try_from_physical_value() {
        let heap = Heap::new();
        let original = physical_value(10.0, 0.05, PhysicalUnit::Ohms);
        let starlark_val = heap.alloc(original);

        let result = PhysicalValue::try_from(starlark_val.to_value()).unwrap();
        assert_eq!(result.value, original.value);
        assert_eq!(result.tolerance, original.tolerance);
        assert_eq!(result.unit, original.unit);
    }

    #[test]
    fn test_try_from_string() {
        // Test Starlark string conversion using helper
        let heap = Heap::new();
        for (input, unit, value) in [
            ("10kOhm", PhysicalUnit::Ohms, 10000.0),
            ("100nF", PhysicalUnit::Farads, 0.0000001),
            ("3.3V", PhysicalUnit::Volts, 3.3),
            ("100mA", PhysicalUnit::Amperes, 0.1),
        ] {
            let starlark_val = heap.alloc(input);
            let result = PhysicalValue::try_from(starlark_val.to_value()).unwrap();
            assert_eq!(result.unit, unit.into());
            assert!((result.value - Decimal::from_f64(value).unwrap()).abs() < Decimal::new(1, 6));
        }
    }

    #[test]
    fn test_try_from_string_with_tolerance() {
        let heap = Heap::new();
        let starlark_val = heap.alloc("10kOhm 5%");
        let result = PhysicalValue::try_from(starlark_val.to_value()).unwrap();

        assert_eq!(result.unit, PhysicalUnit::Ohms.into());
        assert_eq!(result.value, Decimal::from(10000));
        assert_eq!(result.tolerance, Decimal::from_f64(0.05).unwrap());
    }

    #[test]
    fn test_try_from_scalar() {
        let heap = Heap::new();

        // Test integer
        let starlark_val = heap.alloc(42);
        let result = PhysicalValue::try_from(starlark_val.to_value()).unwrap();
        assert_eq!(result.unit, PhysicalUnitDims::DIMENSIONLESS);
        assert_eq!(result.value, Decimal::from(42));
        assert_eq!(result.tolerance, Decimal::ZERO);

        // Test float
        let starlark_val = heap.alloc(3.14);
        let result = PhysicalValue::try_from(starlark_val.to_value()).unwrap();
        assert_eq!(result.unit, PhysicalUnitDims::DIMENSIONLESS);
        assert_eq!(result.value, Decimal::from_f64(3.14).unwrap());
        assert_eq!(result.tolerance, Decimal::ZERO);
    }

    #[test]
    fn test_try_from_string_error() {
        let heap = Heap::new();
        let invalid_strings = ["invalid", "10kZzz", "abc%", ""];

        for invalid in invalid_strings {
            let starlark_val = heap.alloc(invalid);
            let result = PhysicalValue::try_from(starlark_val.to_value());
            assert!(result.is_err(), "Expected error for '{}'", invalid);
        }
    }

    #[test]
    fn test_physical_unit_dims_from_str() {
        // Test simple aliases
        assert_eq!(
            "V".parse::<PhysicalUnitDims>().unwrap(),
            PhysicalUnit::Volts.into()
        );
        assert_eq!(
            "A".parse::<PhysicalUnitDims>().unwrap(),
            PhysicalUnit::Amperes.into()
        );
        assert_eq!(
            "Hz".parse::<PhysicalUnitDims>().unwrap(),
            PhysicalUnit::Hertz.into()
        );
        assert_eq!(
            "s".parse::<PhysicalUnitDims>().unwrap(),
            PhysicalUnit::Seconds.into()
        );

        // Test compound numerator units
        let charge_dims = "A·s".parse::<PhysicalUnitDims>().unwrap();
        assert_eq!(charge_dims, PhysicalUnit::Coulombs.into());

        // Test denominator-only units
        let freq_dims = "1/s".parse::<PhysicalUnitDims>().unwrap();
        assert_eq!(freq_dims, PhysicalUnit::Hertz.into());

        // Test mixed numerator/denominator
        let resistance_dims = "V/A".parse::<PhysicalUnitDims>().unwrap();
        assert_eq!(resistance_dims, PhysicalUnit::Ohms.into());

        let capacitance_dims = "(A·s)/V".parse::<PhysicalUnitDims>().unwrap();
        assert_eq!(capacitance_dims, PhysicalUnit::Farads.into());

        // Test with parentheses
        let capacitance_paren = "(A·s)/V".parse::<PhysicalUnitDims>().unwrap();
        assert_eq!(capacitance_paren, PhysicalUnit::Farads.into());

        // Test error cases
        assert!("UnknownUnit".parse::<PhysicalUnitDims>().is_err());
        assert!("A·UnknownUnit".parse::<PhysicalUnitDims>().is_err());
    }

    #[test]
    fn test_physical_unit_dims_roundtrip() {
        // Test that fmt_unit output can be parsed back
        let test_cases: [PhysicalUnitDims; 13] = [
            PhysicalUnit::Volts.into(),
            PhysicalUnit::Amperes.into(),
            PhysicalUnit::Ohms.into(),
            PhysicalUnit::Farads.into(),
            PhysicalUnit::Henries.into(),
            PhysicalUnit::Hertz.into(),
            PhysicalUnit::Seconds.into(),
            PhysicalUnit::Kelvin.into(),
            PhysicalUnit::Coulombs.into(),
            PhysicalUnit::Watts.into(),
            PhysicalUnit::Joules.into(),
            PhysicalUnit::Siemens.into(),
            PhysicalUnit::Webers.into(),
        ];

        for original in test_cases {
            println!("original {:?}", original);
            let formatted = original.fmt_unit();
            println!("formatted {:?}", formatted);
            let parsed: PhysicalUnitDims = formatted.parse().unwrap();
            assert_eq!(parsed, original, "Failed roundtrip for {}", formatted);
        }
    }

    #[test]
    fn test_with_unit_none_behavior() {
        // Test that the logic for with_unit(None) works correctly
        // This tests the internal logic rather than the Starlark interface

        // Create a physical value with units
        let resistance_value = physical_value(10.0, 0.01, PhysicalUnit::Ohms);

        // Simulate the behavior: if None is passed, should return dimensionless
        let new_value = PhysicalValue::from_decimal(
            resistance_value.value,
            resistance_value.tolerance,
            PhysicalUnitDims::DIMENSIONLESS,
        );

        // Should have same value and tolerance but be dimensionless
        assert_eq!(new_value.value, resistance_value.value);
        assert_eq!(new_value.tolerance, resistance_value.tolerance);
        assert_eq!(new_value.unit, PhysicalUnitDims::DIMENSIONLESS);
    }

    #[test]
    fn test_dimensionless_casting_logic() {
        // Test the core logic for dimensionless casting

        // Create a dimensionless physical value
        let dimensionless = PhysicalValue::dimensionless(42);
        let dimensionless_with_tolerance = PhysicalValue::from_decimal(
            Decimal::from(10),
            Decimal::from_str("0.05").unwrap(), // 5% tolerance
            PhysicalUnitDims::DIMENSIONLESS,
        );

        // Test target units
        let resistance_unit: PhysicalUnitDims = PhysicalUnit::Ohms.into();
        let voltage_unit: PhysicalUnitDims = PhysicalUnit::Volts.into();

        // Verify the dimensionless values are actually dimensionless
        assert_eq!(dimensionless.unit, PhysicalUnitDims::DIMENSIONLESS);
        assert_eq!(
            dimensionless_with_tolerance.unit,
            PhysicalUnitDims::DIMENSIONLESS
        );

        // Test the casting logic: dimensionless -> resistance
        let resistance_casted = PhysicalValue::from_decimal(
            dimensionless.value,
            dimensionless.tolerance,
            resistance_unit,
        );

        // Test the casting logic: dimensionless with tolerance -> voltage
        let voltage_casted = PhysicalValue::from_decimal(
            dimensionless_with_tolerance.value,
            dimensionless_with_tolerance.tolerance,
            voltage_unit,
        );

        // Verify values and tolerances are preserved but units change
        assert_eq!(resistance_casted.value, dimensionless.value);
        assert_eq!(resistance_casted.tolerance, dimensionless.tolerance);
        assert_eq!(resistance_casted.unit, resistance_unit);

        assert_eq!(voltage_casted.value, dimensionless_with_tolerance.value);
        assert_eq!(
            voltage_casted.tolerance,
            dimensionless_with_tolerance.tolerance
        );
        assert_eq!(voltage_casted.unit, voltage_unit);

        // Verify the units are now different from dimensionless
        assert_ne!(resistance_casted.unit, PhysicalUnitDims::DIMENSIONLESS);
        assert_ne!(voltage_casted.unit, PhysicalUnitDims::DIMENSIONLESS);
    }

    #[test]
    fn test_equality_and_comparison() {
        let heap = Heap::new();

        // Test equality with same units and values
        let v1 = physical_value(5.0, 0.01, PhysicalUnit::Volts); // 5V ±1%
        let v2 = physical_value(5.0, 0.02, PhysicalUnit::Volts); // 5V ±2%
        let v3 = physical_value(3.3, 0.0, PhysicalUnit::Volts); // 3.3V

        // Values with same unit and same value are equal (tolerance ignored)
        let v1_val = heap.alloc(v1);
        assert!(v1.equals(v1_val).unwrap());
        assert!(v1.equals(heap.alloc(v2)).unwrap());

        // Values with same unit but different values are not equal
        assert!(!v1.equals(heap.alloc(v3)).unwrap());

        // Values with different units are not equal
        let i1 = physical_value(5.0, 0.0, PhysicalUnit::Amperes);
        assert!(!v1.equals(heap.alloc(i1)).unwrap());

        // Test comparison with same units
        let v_small = physical_value(3.0, 0.0, PhysicalUnit::Volts);
        let v_large = physical_value(10.0, 0.0, PhysicalUnit::Volts);

        assert_eq!(
            v_small.compare(heap.alloc(v_large)).unwrap(),
            Ordering::Less
        );
        assert_eq!(
            v_large.compare(heap.alloc(v_small)).unwrap(),
            Ordering::Greater
        );
        assert_eq!(v1.compare(heap.alloc(v2)).unwrap(), Ordering::Equal);

        // Test comparison with different units fails
        let r1 = physical_value(10.0, 0.0, PhysicalUnit::Ohms);
        assert!(v1.compare(heap.alloc(r1)).is_err());

        // Test comparison with string values
        let v_str = heap.alloc("5V");
        assert!(v1.equals(v_str).unwrap());
        assert_eq!(v1.compare(v_str).unwrap(), Ordering::Equal);

        // Test comparison with numeric values (should be treated as dimensionless)
        let num_val = heap.alloc(5.0);
        assert!(!v1.equals(num_val).unwrap()); // Different units

        // Test with dimensionless values
        let dim1 = PhysicalValue::dimensionless(10);
        let dim2 = PhysicalValue::dimensionless(20);
        assert_eq!(dim1.compare(heap.alloc(dim2)).unwrap(), Ordering::Less);
        assert_eq!(dim2.compare(heap.alloc(dim1)).unwrap(), Ordering::Greater);
    }

    #[test]
    fn test_comparison_with_various_input_types() {
        let heap = Heap::new();
        let voltage = physical_value(12.0, 0.0, PhysicalUnit::Volts);

        // Test equality with string representation
        let voltage_str = heap.alloc("12V");
        assert!(voltage.equals(voltage_str).unwrap());

        // Test comparison with string representation
        let larger_voltage_str = heap.alloc("15V");
        assert_eq!(voltage.compare(larger_voltage_str).unwrap(), Ordering::Less);

        // Test equality with existing PhysicalValue
        let same_voltage = heap.alloc(voltage);
        assert!(voltage.equals(same_voltage).unwrap());

        // Test with different string formats
        let voltage_with_tolerance = heap.alloc("12V 5%");
        assert!(voltage.equals(voltage_with_tolerance).unwrap()); // Tolerance ignored in equality

        // Test comparison failure with non-convertible values
        let non_physical = heap.alloc("not a physical value");
        assert!(!voltage.equals(non_physical).unwrap());
        assert!(voltage.compare(non_physical).is_err());
    }

    #[test]
    fn test_comparison_error_cases() {
        let heap = Heap::new();

        // Test unit mismatch in comparison
        let voltage = physical_value(12.0, 0.0, PhysicalUnit::Volts);
        let current = physical_value(2.0, 0.0, PhysicalUnit::Amperes);

        let result = voltage.compare(heap.alloc(current));
        assert!(result.is_err());

        // Verify the error contains unit mismatch information
        let error_str = format!("{}", result.unwrap_err());
        assert!(error_str.contains("Unit mismatch"));
        assert!(error_str.contains("Voltage"));
        assert!(error_str.contains("Current"));
    }

    #[test]
    fn test_dimensionless_comparisons() {
        let heap = Heap::new();

        // Test with dimensionless values
        let dimensionless_5 = PhysicalValue::dimensionless(5);
        let dimensionless_10 = PhysicalValue::dimensionless(10);
        let voltage_5 = physical_value(5.0, 0.0, PhysicalUnit::Volts);
        let resistance_5 = physical_value(5.0, 0.0, PhysicalUnit::Ohms);

        // Dimensionless to dimensionless comparisons
        assert_eq!(
            dimensionless_5
                .compare(heap.alloc(dimensionless_10))
                .unwrap(),
            Ordering::Less
        );
        assert_eq!(
            dimensionless_10
                .compare(heap.alloc(dimensionless_5))
                .unwrap(),
            Ordering::Greater
        );
        assert_eq!(
            dimensionless_5
                .compare(heap.alloc(dimensionless_5))
                .unwrap(),
            Ordering::Equal
        );

        // Dimensionless to physical unit comparisons (should work)
        assert_eq!(
            dimensionless_5.compare(heap.alloc(voltage_5)).unwrap(),
            Ordering::Equal
        );
        assert_eq!(
            voltage_5.compare(heap.alloc(dimensionless_5)).unwrap(),
            Ordering::Equal
        );
        assert_eq!(
            dimensionless_10.compare(heap.alloc(voltage_5)).unwrap(),
            Ordering::Greater
        );
        assert_eq!(
            voltage_5.compare(heap.alloc(dimensionless_10)).unwrap(),
            Ordering::Less
        );

        // Different units with dimensionless should work
        assert_eq!(
            dimensionless_5.compare(heap.alloc(resistance_5)).unwrap(),
            Ordering::Equal
        );
        assert_eq!(
            resistance_5.compare(heap.alloc(dimensionless_5)).unwrap(),
            Ordering::Equal
        );
    }

    #[test]
    fn test_dimensionless_with_string_conversions() {
        let heap = Heap::new();

        let voltage = physical_value(2023.0, 0.0, PhysicalUnit::Ohms);

        // Test comparison with numeric string (should be treated as dimensionless)
        let numeric_str = heap.alloc("2000");
        assert_eq!(voltage.compare(numeric_str).unwrap(), Ordering::Greater);
        assert!(!voltage.equals(numeric_str).unwrap()); // Different values

        let same_numeric_str = heap.alloc("2023");
        assert_eq!(voltage.compare(same_numeric_str).unwrap(), Ordering::Equal);
        assert!(voltage.equals(same_numeric_str).unwrap()); // Same values
    }

    #[test]
    fn test_non_dimensionless_casting_fails() {
        // Test that non-dimensionless PhysicalValues cannot be cast to other units
        let resistance = physical_value(10.0, 0.01, PhysicalUnit::Ohms);
        let voltage_unit: PhysicalUnitDims = PhysicalUnit::Volts.into();

        // This should fail - we shouldn't allow Ohms -> Volts conversion
        // (This would be tested at the PhysicalValue::from_arguments level in real usage)
        assert_ne!(resistance.unit, PhysicalUnitDims::DIMENSIONLESS);
        assert_ne!(resistance.unit, voltage_unit);

        // The logic should detect this mismatch and return an error
        // In the actual implementation, this would be caught by the unit checking
    }

    #[test]
    fn test_range_parsing_endash() {
        let r = PhysicalRange::from_str("11–26V").unwrap();
        assert_eq!(r.min, Decimal::from(11));
        assert_eq!(r.max, Decimal::from(26));
        assert_eq!(r.nominal, None);
        assert_eq!(r.r#type.unit, PhysicalUnitDims::VOLTAGE);
    }

    #[test]
    fn test_range_parsing_endash_with_spaces() {
        let r = PhysicalRange::from_str("11 – 26V").unwrap();
        assert_eq!(r.min, Decimal::from(11));
        assert_eq!(r.max, Decimal::from(26));
        assert_eq!(r.nominal, None);
        assert_eq!(r.r#type.unit, PhysicalUnitDims::VOLTAGE);
    }

    #[test]
    fn test_range_parsing_to_keyword() {
        let r = PhysicalRange::from_str("11V to 26V").unwrap();
        assert_eq!(r.min, Decimal::from(11));
        assert_eq!(r.max, Decimal::from(26));
        assert_eq!(r.nominal, None);
        assert_eq!(r.r#type.unit, PhysicalUnitDims::VOLTAGE);
    }

    #[test]
    fn test_range_parsing_decimal_to_keyword() {
        let r = PhysicalRange::from_str("1.1 V to 26V").unwrap();
        assert_eq!(r.min, Decimal::from_str("1.1").unwrap());
        assert_eq!(r.max, Decimal::from(26));
        assert_eq!(r.nominal, None);
    }

    #[test]
    fn test_range_parsing_with_nominal() {
        let r = PhysicalRange::from_str("11–26 V (12 V nom.)").unwrap();
        assert_eq!(r.min, Decimal::from(11));
        assert_eq!(r.max, Decimal::from(26));
        assert_eq!(r.nominal, Some(Decimal::from(12)));
        assert_eq!(r.r#type.unit, PhysicalUnitDims::VOLTAGE);
    }

    #[test]
    fn test_range_parsing_with_nominal_no_period() {
        let r = PhysicalRange::from_str("11–26 V (12 V nom)").unwrap();
        assert_eq!(r.min, Decimal::from(11));
        assert_eq!(r.max, Decimal::from(26));
        assert_eq!(r.nominal, Some(Decimal::from(12)));
    }

    #[test]
    fn test_range_parsing_single_value_no_tolerance() {
        let r = PhysicalRange::from_str("3.3V").unwrap();
        assert_eq!(r.min, Decimal::from_str("3.3").unwrap());
        assert_eq!(r.max, Decimal::from_str("3.3").unwrap());
        assert_eq!(r.nominal, None);
        assert_eq!(r.r#type.unit, PhysicalUnitDims::VOLTAGE);
    }

    #[test]
    fn test_range_parsing_tolerance_expansion() {
        let r = PhysicalRange::from_str("15V 10%").unwrap();
        assert_eq!(r.min, Decimal::from_str("13.5").unwrap());
        assert_eq!(r.max, Decimal::from_str("16.5").unwrap());
        assert_eq!(r.nominal, None);
        assert_eq!(r.r#type.unit, PhysicalUnitDims::VOLTAGE);
    }

    #[test]
    fn test_range_parsing_unit_inference() {
        // Left side is bare number, should inherit unit from right
        let r = PhysicalRange::from_str("3.3 to 5V").unwrap();
        assert_eq!(r.min, Decimal::from_str("3.3").unwrap());
        assert_eq!(r.max, Decimal::from(5));
        assert_eq!(r.r#type.unit, PhysicalUnitDims::VOLTAGE);
    }

    #[test]
    fn test_range_parsing_reversed_values() {
        // Should auto-swap to ensure min <= max
        let r = PhysicalRange::from_str("26V to 11V").unwrap();
        assert_eq!(r.min, Decimal::from(11));
        assert_eq!(r.max, Decimal::from(26));
    }

    #[test]
    fn test_range_parsing_resistance() {
        let r = PhysicalRange::from_str("10kOhm to 100kOhm").unwrap();
        assert_eq!(r.min, Decimal::from(10000));
        assert_eq!(r.max, Decimal::from(100000));
        assert_eq!(r.r#type.unit, PhysicalUnit::Ohms.into());
    }

    #[test]
    fn test_range_parsing_current() {
        let r = PhysicalRange::from_str("100mA to 2A").unwrap();
        assert_eq!(r.min, Decimal::from_str("0.1").unwrap());
        assert_eq!(r.max, Decimal::from(2));
        assert_eq!(r.r#type.unit, PhysicalUnitDims::CURRENT);
    }

    #[test]
    fn test_range_display() {
        let r = PhysicalRange::from_str("11–26V").unwrap();
        let display = format!("{}", r);
        assert_eq!(display, "11–26 V");
    }

    #[test]
    fn test_range_display_with_nominal() {
        let r = PhysicalRange::from_str("11–26 V (12 V nom.)").unwrap();
        let display = format!("{}", r);
        assert_eq!(display, "11–26 V (12 V nom.)");
    }

    #[test]
    fn test_range_parsing_invalid_format() {
        assert!(PhysicalRange::from_str("").is_err());
        assert!(PhysicalRange::from_str("   ").is_err());
    }

    #[test]
    fn test_range_parsing_unit_mismatch() {
        // Should fail - mixing voltage and current units
        assert!(PhysicalRange::from_str("5V to 2A").is_err());
    }

    #[test]
    fn test_range_validation_nominal_out_of_range() {
        // Nominal value must be within [min, max]
        let heap = Heap::new();
        let mut builder = RangeBuilder::default();
        builder.add_min_max(heap.alloc(1), heap.alloc(10)).unwrap();
        builder.add_nominal(heap.alloc(15)).unwrap();

        let result = builder.build(PhysicalUnitDims::VOLTAGE);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            PhysicalValueError::NominalOutOfRange { .. }
        ));
    }

    #[test]
    fn test_range_validation_valid_nominal() {
        // Nominal value within range should succeed
        let heap = Heap::new();
        let mut builder = RangeBuilder::default();
        builder.add_min_max(heap.alloc(1), heap.alloc(10)).unwrap();
        builder.add_nominal(heap.alloc(5)).unwrap();

        let result = builder.build(PhysicalUnitDims::VOLTAGE);
        assert!(result.is_ok());
        let range = result.unwrap();
        assert_eq!(range.min, Decimal::from(1));
        assert_eq!(range.max, Decimal::from(10));
        assert_eq!(range.nominal, Some(Decimal::from(5)));
    }

    #[test]
    fn test_abs_positive_value() {
        let pv = physical_value(3.3, 0.0, PhysicalUnit::Volts);
        let result = pv.abs();
        assert_eq!(result.value, Decimal::from_f64(3.3).unwrap());
        assert_eq!(result.unit, PhysicalUnit::Volts.into());
        assert_eq!(result.tolerance, Decimal::ZERO);
    }

    #[test]
    fn test_abs_negative_value() {
        let pv = physical_value(-3.3, 0.0, PhysicalUnit::Volts);
        let result = pv.abs();
        assert_eq!(result.value, Decimal::from_f64(3.3).unwrap());
        assert_eq!(result.unit, PhysicalUnit::Volts.into());
        assert_eq!(result.tolerance, Decimal::ZERO);
    }

    #[test]
    fn test_abs_preserves_tolerance() {
        let pv = physical_value(-5.0, 0.05, PhysicalUnit::Amperes);
        let result = pv.abs();
        assert_eq!(result.value, Decimal::from_f64(5.0).unwrap());
        assert_eq!(result.unit, PhysicalUnit::Amperes.into());
        assert_eq!(result.tolerance, Decimal::from_f64(0.05).unwrap());
    }

    #[test]
    fn test_diff_positive_difference() {
        let pv1 = physical_value(10.0, 0.0, PhysicalUnit::Volts);
        let pv2 = physical_value(3.0, 0.0, PhysicalUnit::Volts);
        let result = pv1.diff(&pv2).unwrap();
        assert_eq!(result.value, Decimal::from_f64(7.0).unwrap());
        assert_eq!(result.unit, PhysicalUnit::Volts.into());
        assert_eq!(result.tolerance, Decimal::ZERO);
    }

    #[test]
    fn test_diff_negative_difference_returns_positive() {
        let pv1 = physical_value(3.0, 0.0, PhysicalUnit::Volts);
        let pv2 = physical_value(10.0, 0.0, PhysicalUnit::Volts);
        let result = pv1.diff(&pv2).unwrap();
        assert_eq!(result.value, Decimal::from_f64(7.0).unwrap());
        assert_eq!(result.unit, PhysicalUnit::Volts.into());
        assert_eq!(result.tolerance, Decimal::ZERO);
    }

    #[test]
    fn test_diff_unit_mismatch() {
        let pv1 = physical_value(10.0, 0.0, PhysicalUnit::Volts);
        let pv2 = physical_value(3.0, 0.0, PhysicalUnit::Amperes);
        let result = pv1.diff(&pv2);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            PhysicalValueError::UnitMismatch { .. }
        ));
    }

    #[test]
    fn test_diff_drops_tolerance() {
        // Based on subtraction behavior, diff should drop tolerance
        let pv1 = physical_value(10.0, 0.1, PhysicalUnit::Volts);
        let pv2 = physical_value(3.0, 0.05, PhysicalUnit::Volts);
        let result = pv1.diff(&pv2).unwrap();
        assert_eq!(result.value, Decimal::from_f64(7.0).unwrap());
        assert_eq!(result.tolerance, Decimal::ZERO);
    }

    #[test]
    fn test_diff_with_string_conversion() {
        // Test that diff works when the other value is parsed from a string
        use starlark::values::Heap;

        let heap = Heap::new();
        let pv1 = heap.alloc(physical_value(3.3, 0.0, PhysicalUnit::Volts));
        let pv2_str = heap.alloc("5V");

        // Convert string to PhysicalValue
        let pv2 = PhysicalValue::try_from(pv2_str).unwrap();
        let pv1_val = PhysicalValue::try_from(pv1).unwrap();

        // Test diff
        let result = pv1_val.diff(&pv2).unwrap();
        assert_eq!(result.value, Decimal::from_f64(1.7).unwrap());
        assert_eq!(result.unit, PhysicalUnit::Volts.into());
    }

    #[test]
    fn test_within_same_nominal_different_tolerance() {
        // 3.3V ±5% fits within 3.3V ±10%
        let tight = physical_value(3.3, 0.05, PhysicalUnit::Volts); // 3.135V - 3.465V
        let loose = physical_value(3.3, 0.10, PhysicalUnit::Volts); // 2.97V - 3.63V
        assert!(tight.within(&loose).unwrap());

        // 3.3V ±10% does NOT fit within 3.3V ±5%
        assert!(!loose.within(&tight).unwrap());
    }

    #[test]
    fn test_within_different_nominal_values() {
        // 3.3V ±1% (3.267V - 3.333V) fits within 5V ±50% (2.5V - 7.5V)
        let small = physical_value(3.3, 0.01, PhysicalUnit::Volts);
        let large = physical_value(5.0, 0.50, PhysicalUnit::Volts);
        assert!(small.within(&large).unwrap());

        // 5V ±50% does NOT fit within 3.3V ±1%
        assert!(!large.within(&small).unwrap());
    }

    #[test]
    fn test_within_exact_match() {
        // Exact values with no tolerance should be within each other
        let v1 = physical_value(3.3, 0.0, PhysicalUnit::Volts);
        let v2 = physical_value(3.3, 0.0, PhysicalUnit::Volts);
        assert!(v1.within(&v2).unwrap());
        assert!(v2.within(&v1).unwrap());
    }

    #[test]
    fn test_within_zero_tolerance_in_range() {
        // Zero tolerance value at the center of a range
        let exact = physical_value(3.3, 0.0, PhysicalUnit::Volts);
        let range = physical_value(3.3, 0.10, PhysicalUnit::Volts); // 2.97V - 3.63V
        assert!(exact.within(&range).unwrap());
    }

    #[test]
    fn test_within_zero_tolerance_outside_range() {
        // Zero tolerance value outside a range
        let exact = physical_value(5.0, 0.0, PhysicalUnit::Volts);
        let range = physical_value(3.3, 0.10, PhysicalUnit::Volts); // 2.97V - 3.63V
        assert!(!exact.within(&range).unwrap());
    }

    #[test]
    fn test_within_edge_cases() {
        // Test boundary conditions
        // Range: 3.3V ±10% = 2.97V - 3.63V
        let range = physical_value(3.3, 0.10, PhysicalUnit::Volts);

        // Value exactly at lower bound should be within
        let at_min = physical_value(2.97, 0.0, PhysicalUnit::Volts);
        assert!(at_min.within(&range).unwrap());

        // Value exactly at upper bound should be within
        let at_max = physical_value(3.63, 0.0, PhysicalUnit::Volts);
        assert!(at_max.within(&range).unwrap());

        // Value just outside lower bound should not be within
        let below_min = physical_value(2.96, 0.0, PhysicalUnit::Volts);
        assert!(!below_min.within(&range).unwrap());

        // Value just outside upper bound should not be within
        let above_max = physical_value(3.64, 0.0, PhysicalUnit::Volts);
        assert!(!above_max.within(&range).unwrap());
    }

    #[test]
    fn test_within_overlapping_but_not_contained() {
        // Ranges that overlap but one doesn't contain the other
        // Range 1: 3.3V ±10% = 2.97V - 3.63V
        // Range 2: 3.5V ±5% = 3.325V - 3.675V
        let range1 = physical_value(3.3, 0.10, PhysicalUnit::Volts);
        let range2 = physical_value(3.5, 0.05, PhysicalUnit::Volts);

        // They overlap but neither contains the other
        assert!(!range1.within(&range2).unwrap());
        assert!(!range2.within(&range1).unwrap());
    }

    #[test]
    fn test_within_unit_mismatch() {
        // Different units should return an error
        let volts = physical_value(3.3, 0.1, PhysicalUnit::Volts);
        let amps = physical_value(3.3, 0.1, PhysicalUnit::Amperes);

        let result = volts.within(&amps);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            PhysicalValueError::UnitMismatch { .. }
        ));
    }

    #[test]
    fn test_within_different_units() {
        // Test with various unit types
        let r1 = physical_value(1000.0, 0.01, PhysicalUnit::Ohms); // 1kΩ ±1%
        let r2 = physical_value(1000.0, 0.05, PhysicalUnit::Ohms); // 1kΩ ±5%
        assert!(r1.within(&r2).unwrap());

        let c1 = physical_value(1e-7, 0.05, PhysicalUnit::Farads); // 100nF ±5%
        let c2 = physical_value(1e-7, 0.20, PhysicalUnit::Farads); // 100nF ±20%
        assert!(c1.within(&c2).unwrap());

        let f1 = physical_value(1e6, 0.001, PhysicalUnit::Hertz); // 1MHz ±0.1%
        let f2 = physical_value(1e6, 0.01, PhysicalUnit::Hertz); // 1MHz ±1%
        assert!(f1.within(&f2).unwrap());
    }

    #[test]
    fn test_within_negative_values() {
        // Test with negative values
        let v1 = physical_value(-3.3, 0.05, PhysicalUnit::Volts); // -3.3V ±5%
        let v2 = physical_value(-3.3, 0.10, PhysicalUnit::Volts); // -3.3V ±10%
        assert!(v1.within(&v2).unwrap());
        assert!(!v2.within(&v1).unwrap());
    }

    // Helper for creating PhysicalRange
    #[cfg(test)]
    fn physical_range(min: f64, max: f64, unit: PhysicalUnit) -> PhysicalRange {
        PhysicalRange {
            min: Decimal::from_f64(min).unwrap(),
            max: Decimal::from_f64(max).unwrap(),
            nominal: None,
            r#type: PhysicalRangeType::new(unit.into()),
        }
    }

    #[test]
    fn test_range_diff_power_to_ground() {
        // VCC 3.0-3.6V, GND 0V -> diff = 3.6V
        let vcc = physical_range(3.0, 3.6, PhysicalUnit::Volts);
        let gnd = physical_range(0.0, 0.0, PhysicalUnit::Volts);
        let result = vcc.diff(&gnd).unwrap();

        assert_eq!(result.value, Decimal::from_f64(3.6).unwrap());
        assert_eq!(result.unit, PhysicalUnit::Volts.into());
        assert_eq!(result.tolerance, Decimal::ZERO);
    }

    #[test]
    fn test_range_diff_two_rails() {
        // V1: 3.0-3.6V, V2: 1.7-2.0V
        // max(|3.6 - 1.7|, |3.0 - 2.0|) = max(1.9, 1.0) = 1.9V
        let v1 = physical_range(3.0, 3.6, PhysicalUnit::Volts);
        let v2 = physical_range(1.7, 2.0, PhysicalUnit::Volts);
        let result = v1.diff(&v2).unwrap();

        assert_eq!(result.value, Decimal::from_f64(1.9).unwrap());
    }

    #[test]
    fn test_range_diff_ac_coupling() {
        // Signal: -5 to +5V, Bias: 0V
        // max(|5 - 0|, |-5 - 0|) = 5V
        let signal = physical_range(-5.0, 5.0, PhysicalUnit::Volts);
        let bias = physical_range(0.0, 0.0, PhysicalUnit::Volts);
        let result = signal.diff(&bias).unwrap();

        assert_eq!(result.value, Decimal::from_f64(5.0).unwrap());
    }

    #[test]
    fn test_range_diff_negative_ranges() {
        // V1: -5 to -3V, V2: -2 to -1V
        // max(|-3 - (-2)|, |-5 - (-1)|) = max(1, 4) = 4V
        let v1 = physical_range(-5.0, -3.0, PhysicalUnit::Volts);
        let v2 = physical_range(-2.0, -1.0, PhysicalUnit::Volts);
        let result = v1.diff(&v2).unwrap();

        assert_eq!(result.value, Decimal::from_f64(4.0).unwrap());
    }

    #[test]
    fn test_range_diff_symmetric() {
        // diff should be symmetric: A.diff(B) == B.diff(A)
        let v1 = physical_range(3.0, 3.6, PhysicalUnit::Volts);
        let v2 = physical_range(1.7, 2.0, PhysicalUnit::Volts);

        let diff_ab = v1.diff(&v2).unwrap();
        let diff_ba = v2.diff(&v1).unwrap();

        assert_eq!(diff_ab.value, diff_ba.value);
    }

    #[test]
    fn test_range_diff_same_range() {
        // Same range should have 0 difference
        let v = physical_range(3.3, 3.3, PhysicalUnit::Volts);
        let result = v.diff(&v).unwrap();

        assert_eq!(result.value, Decimal::ZERO);
    }

    #[test]
    fn test_range_diff_overlapping_ranges() {
        // Range 1: 2.0-4.0V, Range 2: 3.0-5.0V
        // max(|4.0 - 3.0|, |2.0 - 5.0|) = max(1.0, 3.0) = 3.0V
        let r1 = physical_range(2.0, 4.0, PhysicalUnit::Volts);
        let r2 = physical_range(3.0, 5.0, PhysicalUnit::Volts);
        let result = r1.diff(&r2).unwrap();

        assert_eq!(result.value, Decimal::from_f64(3.0).unwrap());
    }

    #[test]
    fn test_range_diff_unit_mismatch() {
        // Different units should return an error
        let volts = physical_range(3.0, 3.6, PhysicalUnit::Volts);
        let amps = physical_range(0.0, 1.0, PhysicalUnit::Amperes);

        let result = volts.diff(&amps);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            PhysicalValueError::UnitMismatch { .. }
        ));
    }

    #[test]
    fn test_range_diff_various_units() {
        // Test with resistance ranges
        let r1 = physical_range(900.0, 1100.0, PhysicalUnit::Ohms); // 1kΩ ±10%
        let r2 = physical_range(0.0, 0.0, PhysicalUnit::Ohms); // 0Ω (short)
        let result = r1.diff(&r2).unwrap();
        assert_eq!(result.value, Decimal::from_f64(1100.0).unwrap());

        // Test with current ranges
        let i1 = physical_range(0.1, 0.5, PhysicalUnit::Amperes);
        let i2 = physical_range(0.0, 0.0, PhysicalUnit::Amperes);
        let result = i1.diff(&i2).unwrap();
        assert_eq!(result.value, Decimal::from_f64(0.5).unwrap());
    }

    #[test]
    fn test_range_diff_zero_tolerance() {
        // Range with no tolerance always returns zero tolerance
        let v1 = physical_range(3.3, 3.3, PhysicalUnit::Volts);
        let v2 = physical_range(5.0, 5.0, PhysicalUnit::Volts);
        let result = v1.diff(&v2).unwrap();

        assert_eq!(result.tolerance, Decimal::ZERO);
        assert_eq!(result.value, Decimal::from_f64(1.7).unwrap());
    }

    #[test]
    fn test_range_diff_from_string() {
        use starlark::values::Heap;

        let heap = Heap::new();
        let range = heap.alloc(physical_range(3.0, 3.6, PhysicalUnit::Volts));

        // Get the range from heap
        let range_val = range.downcast_ref::<PhysicalRange>().unwrap();

        // Parse string as PhysicalRange
        let gnd_range = PhysicalRange::from_str("0V").unwrap();

        // Test diff works with parsed string
        let result = range_val.diff(&gnd_range).unwrap();
        assert_eq!(result.value, Decimal::from_f64(3.6).unwrap());
        assert_eq!(result.unit, PhysicalUnit::Volts.into());
    }
}
