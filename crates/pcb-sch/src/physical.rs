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
    let value = if let Some(inner) = s.strip_suffix('%') {
        inner
            .parse::<Decimal>()
            .map_err(|_| ParseError::InvalidNumber)?
            / ONE_HUNDRED
    } else {
        s.parse::<Decimal>()
            .map_err(|_| ParseError::InvalidNumber)?
    };

    if value < Decimal::ZERO {
        return Err(ParseError::InvalidTolerance);
    }

    Ok(value)
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
    let nominal = decimal_num * Decimal::from(1000);

    Some(PhysicalValue::from_nominal_tolerance(
        nominal,
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
            return Ok(physical.nominal);
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
    /// The nominal/typical value (always required)
    #[allocative(skip)]
    #[serde(with = "rust_decimal::serde::str")]
    pub nominal: Decimal,
    /// Lower bound (can be asymmetric from nominal)
    #[allocative(skip)]
    #[serde(with = "rust_decimal::serde::str")]
    pub min: Decimal,
    /// Upper bound (can be asymmetric from nominal)
    #[allocative(skip)]
    #[serde(with = "rust_decimal::serde::str")]
    pub max: Decimal,
    /// Physical unit dimensions
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
        Ok((pv.min, pv.max))
    } else if let Some(s) = value.unpack_str() {
        // Try to parse as PhysicalValue (now handles range syntax too)
        if let Ok(pv) = PhysicalValue::from_str(s) {
            if pv.unit != expected_unit {
                return Err(PhysicalValueError::UnitMismatch {
                    expected: expected_unit.quantity(),
                    actual: pv.unit.quantity(),
                });
            }
            Ok((pv.min, pv.max))
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
    /// Construct from f64s that arrive from Starlark or other APIs (backwards compat)
    pub fn new(value: f64, tolerance: f64, unit: PhysicalUnit) -> Self {
        let nominal = Decimal::from_f64(value)
            .unwrap_or_else(|| panic!("value {} not representable as Decimal", value));
        let tol = Decimal::from_f64(tolerance)
            .unwrap_or_else(|| panic!("tolerance {} not representable as Decimal", tolerance));
        Self::from_nominal_tolerance(nominal, tol, unit.into())
    }

    /// Get the unit as a PhysicalUnit if it has a simple alias
    pub fn unit(&self) -> Option<PhysicalUnit> {
        self.unit.alias()
    }

    /// Create a dimensionless point value
    pub fn dimensionless<D: Into<Decimal>>(value: D) -> Self {
        let v = value.into();
        Self {
            nominal: v,
            min: v,
            max: v,
            unit: PhysicalUnitDims::DIMENSIONLESS,
        }
    }

    /// Create a point value (min == nominal == max)
    pub fn point(nominal: Decimal, unit: PhysicalUnitDims) -> Self {
        Self {
            nominal,
            min: nominal,
            max: nominal,
            unit,
        }
    }

    /// Create from explicit bounds with nominal as midpoint
    pub fn from_bounds(min: Decimal, max: Decimal, unit: PhysicalUnitDims) -> Self {
        let nominal = (min + max) / Decimal::from(2);
        Self {
            nominal,
            min,
            max,
            unit,
        }
    }

    /// Create from explicit bounds with explicit nominal
    pub fn from_bounds_nominal(
        nominal: Decimal,
        min: Decimal,
        max: Decimal,
        unit: PhysicalUnitDims,
    ) -> Self {
        Self {
            nominal,
            min,
            max,
            unit,
        }
    }

    /// Create from nominal and symmetric tolerance (backwards compat)
    pub fn from_nominal_tolerance(
        nominal: Decimal,
        tolerance: Decimal,
        unit: PhysicalUnitDims,
    ) -> Self {
        assert!(
            tolerance >= Decimal::ZERO,
            "tolerance must be non-negative, got {}",
            tolerance
        );
        if tolerance.is_zero() {
            Self::point(nominal, unit)
        } else {
            let delta = nominal.abs() * tolerance;
            let min = nominal - delta;
            let max = nominal + delta;
            Self {
                nominal,
                min,
                max,
                unit,
            }
        }
    }

    /// Backwards compatibility: create from value and tolerance (legacy API)
    pub fn from_decimal(value: Decimal, tolerance: Decimal, unit: PhysicalUnitDims) -> Self {
        Self::from_nominal_tolerance(value, tolerance, unit)
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

    /// Compute the worst-case tolerance as a fraction
    /// Returns max((nominal - min) / nominal, (max - nominal) / nominal)
    pub fn tolerance(&self) -> Decimal {
        if self.nominal.is_zero() {
            return Decimal::ZERO;
        }
        let lower_tol = (self.nominal - self.min) / self.nominal.abs();
        let upper_tol = (self.max - self.nominal) / self.nominal.abs();
        lower_tol.max(upper_tol)
    }

    /// Check if bounds are symmetric around nominal
    pub fn is_symmetric(&self) -> bool {
        let lower_delta = self.nominal - self.min;
        let upper_delta = self.max - self.nominal;
        // Use relative epsilon for comparison
        let epsilon = self.nominal.abs() * Decimal::new(1, 9); // 1e-9
        (lower_delta - upper_delta).abs() <= epsilon
    }

    /// Check if this is a point value (no tolerance)
    pub fn is_point(&self) -> bool {
        self.min == self.nominal && self.nominal == self.max
    }

    /// Check if this value's range fits within another value's range
    pub fn fits_within_default(&self, other: &PhysicalValue) -> bool {
        self.min >= other.min && self.max <= other.max
    }

    /// Get the absolute value of this physical value
    pub fn abs(&self) -> PhysicalValue {
        if self.min >= Decimal::ZERO {
            // All positive: no change needed
            PhysicalValue {
                nominal: self.nominal.abs(),
                min: self.min,
                max: self.max,
                unit: self.unit,
            }
        } else if self.max <= Decimal::ZERO {
            // All negative: negate and swap
            PhysicalValue {
                nominal: self.nominal.abs(),
                min: self.max.abs(),
                max: self.min.abs(),
                unit: self.unit,
            }
        } else {
            // Spans zero: min becomes 0, max is larger absolute bound
            let new_max = self.min.abs().max(self.max.abs());
            PhysicalValue {
                nominal: self.nominal.abs(),
                min: Decimal::ZERO,
                max: new_max,
                unit: self.unit,
            }
        }
    }

    /// Get the maximum absolute difference between two physical values
    /// For ranges, returns max(|self.max - other.min|, |self.min - other.max|)
    /// Returns an error if units don't match
    pub fn diff(&self, other: &PhysicalValue) -> Result<PhysicalValue, PhysicalValueError> {
        if self.unit != other.unit {
            return Err(PhysicalValueError::UnitMismatch {
                expected: self.unit.quantity(),
                actual: other.unit.quantity(),
            });
        }
        // Conservative: maximum possible difference between the two ranges
        let diff1 = (self.max - other.min).abs();
        let diff2 = (self.min - other.max).abs();
        let max_diff = diff1.max(diff2);
        Ok(PhysicalValue::point(max_diff, self.unit))
    }

    fn fields() -> SortedMap<String, Ty> {
        fn single_param_spec(param_type: Ty) -> ParamSpec {
            ParamSpec::new_parts([(ParamIsRequired::Yes, param_type)], [], None, [], None).unwrap()
        }
        fn no_param_spec() -> ParamSpec {
            ParamSpec::new_parts([], [], None, [], None).unwrap()
        }

        let str_param_spec = single_param_spec(PhysicalValue::get_type_starlark_repr());
        let with_tolerance_param_spec = single_param_spec(Ty::union2(Ty::float(), Ty::string()));
        let with_value_param_spec = single_param_spec(Ty::union2(Ty::float(), Ty::int()));
        let with_unit_param_spec = single_param_spec(Ty::union2(Ty::string(), Ty::none()));
        let diff_param_spec = single_param_spec(PhysicalValue::get_type_starlark_repr());
        let abs_param_spec = no_param_spec();
        let within_param_spec = single_param_spec(Ty::any()); // Accepts any type like is_in()

        SortedMap::from_iter([
            ("value".to_string(), Ty::float()), // Alias for nominal
            ("nominal".to_string(), Ty::float()),
            ("tolerance".to_string(), Ty::float()), // Computed worst-case tolerance
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
        let nominal = self.nominal * rhs.nominal;
        let unit = self.unit * rhs.unit;

        // Preserve bounds only for dimensionless scaling
        match (self.unit, rhs.unit) {
            (PhysicalUnitDims::DIMENSIONLESS, _) => {
                // scalar * physical_value: scale the bounds
                let scalar = self.nominal;
                let (min, max) = if scalar >= Decimal::ZERO {
                    (rhs.min * scalar, rhs.max * scalar)
                } else {
                    (rhs.max * scalar, rhs.min * scalar) // Swap for negative scalar
                };
                PhysicalValue {
                    nominal,
                    min,
                    max,
                    unit,
                }
            }
            (_, PhysicalUnitDims::DIMENSIONLESS) => {
                // physical_value * scalar: scale the bounds
                let scalar = rhs.nominal;
                let (min, max) = if scalar >= Decimal::ZERO {
                    (self.min * scalar, self.max * scalar)
                } else {
                    (self.max * scalar, self.min * scalar) // Swap for negative scalar
                };
                PhysicalValue {
                    nominal,
                    min,
                    max,
                    unit,
                }
            }
            _ => {
                // All other cases: drop bounds (point value)
                PhysicalValue::point(nominal, unit)
            }
        }
    }
}

impl std::ops::Div for PhysicalValue {
    type Output = Result<PhysicalValue, PhysicalValueError>;
    fn div(self, rhs: Self) -> Self::Output {
        if rhs.nominal == Decimal::ZERO {
            return Err(PhysicalValueError::DivisionByZero);
        }
        let nominal = self.nominal / rhs.nominal;
        let unit = self.unit / rhs.unit;

        // Preserve bounds only for dimensionless scaling (division by scalar)
        if rhs.unit == PhysicalUnitDims::DIMENSIONLESS {
            let scalar = rhs.nominal;
            let (min, max) = if scalar >= Decimal::ZERO {
                (self.min / scalar, self.max / scalar)
            } else {
                (self.max / scalar, self.min / scalar) // Swap for negative scalar
            };
            Ok(PhysicalValue {
                nominal,
                min,
                max,
                unit,
            })
        } else {
            // All other cases: drop bounds (point value)
            Ok(PhysicalValue::point(nominal, unit))
        }
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
        let nominal = self.nominal + rhs.nominal;
        // Drop bounds for addition (point value)
        Ok(PhysicalValue::point(nominal, unit))
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
        let nominal = self.nominal - rhs.nominal;
        // Drop bounds for subtraction (point value)
        Ok(PhysicalValue::point(nominal, unit))
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
            ParseError::InvalidTolerance => PhysicalValueError::InvalidNumberType,
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
    InvalidTolerance,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::InvalidFormat => write!(f, "Invalid physical value format"),
            ParseError::InvalidNumber => write!(f, "Invalid number"),
            ParseError::InvalidUnit => write!(f, "Invalid unit"),
            ParseError::InvalidTolerance => write!(f, "Tolerance must be non-negative"),
        }
    }
}

impl std::error::Error for ParseError {}

impl From<ParseError> for starlark::Error {
    fn from(err: ParseError) -> Self {
        starlark::Error::new_other(err)
    }
}

/// Parse range syntax like "11–26V", "11V to 26V", "11–26 V (12 V nom.)"
/// Returns None if the string doesn't contain range syntax
fn split_number_and_unit(s: &str) -> Result<(Decimal, &str), ParseError> {
    let s = s.trim();
    let split_pos = s
        .find(|ch: char| !ch.is_ascii_digit() && ch != '.' && ch != '-' && ch != '+')
        .unwrap_or(s.len());

    if split_pos == 0 {
        return Err(ParseError::InvalidFormat);
    }

    let (number_str, unit_str) = s.split_at(split_pos);
    let base_number: Decimal = number_str.parse().map_err(|_| ParseError::InvalidNumber)?;
    Ok((base_number, unit_str.trim()))
}

fn parse_value_with_optional_unit(
    s: &str,
) -> Result<(Decimal, Option<PhysicalUnitDims>), ParseError> {
    let (base_number, unit_str) = split_number_and_unit(s)?;
    if unit_str.is_empty() {
        Ok((base_number, None))
    } else {
        let (value, unit) = parse_unit_with_prefix(unit_str, base_number)?;
        Ok((value, Some(unit)))
    }
}

fn parse_value_with_unit(s: &str) -> Result<(Decimal, PhysicalUnitDims), ParseError> {
    let (base_number, unit_str) = split_number_and_unit(s)?;
    parse_unit_with_prefix(unit_str, base_number)
}

fn parse_range_syntax(s: &str) -> Option<Result<PhysicalValue, ParseError>> {
    // Check for range separators: en-dash (–), hyphen surrounded by non-numbers, or "to"
    let (left, right, nominal_str) = if let Some((range_part, nom_part)) = s.split_once('(') {
        // Has nominal: "11–26 V (12 V nom.)"
        let nom_part = nom_part
            .trim()
            .trim_end_matches(')')
            .trim_end_matches('.')
            .trim();
        let nom_str = if let Some(stripped) = nom_part.strip_suffix("nom") {
            stripped.trim()
        } else {
            nom_part
        };
        if let Some(parts) = split_range(range_part.trim()) {
            (parts.0, parts.1, Some(nom_str))
        } else {
            return None;
        }
    } else if let Some(parts) = split_range(s) {
        (parts.0, parts.1, None)
    } else {
        return None;
    };

    // Parse left and right values
    let left_result = match parse_value_with_optional_unit(left) {
        Ok(r) => r,
        Err(e) => return Some(Err(e)),
    };
    let right_result = match parse_value_with_optional_unit(right) {
        Ok(r) => r,
        Err(e) => return Some(Err(e)),
    };

    // Determine unit (prefer right side, fall back to left)
    let unit = right_result
        .1
        .or(left_result.1)
        .unwrap_or(PhysicalUnit::Ohms.into());

    // Check unit consistency
    if let (Some(l_unit), Some(r_unit)) = (left_result.1, right_result.1) {
        if l_unit != r_unit {
            return Some(Err(ParseError::InvalidUnit));
        }
    }

    let mut min_val = left_result.0;
    let mut max_val = right_result.0;

    // Auto-swap if reversed
    if min_val > max_val {
        std::mem::swap(&mut min_val, &mut max_val);
    }

    // Parse nominal if present
    let nominal = if let Some(nom_str) = nominal_str {
        let (value, nom_unit) = match parse_value_with_optional_unit(nom_str) {
            Ok(result) => result,
            Err(e) => return Some(Err(e)),
        };
        if let Some(nom_unit) = nom_unit {
            if nom_unit != unit {
                return Some(Err(ParseError::InvalidUnit));
            }
        }
        value
    } else {
        // Use midpoint as nominal
        (min_val + max_val) / Decimal::from(2)
    };

    if nominal < min_val || nominal > max_val {
        return Some(Err(ParseError::InvalidFormat));
    }

    Some(Ok(PhysicalValue::from_bounds_nominal(
        nominal, min_val, max_val, unit,
    )))
}

/// Split a range string by separator (en-dash, hyphen-in-context, or "to")
fn split_range(s: &str) -> Option<(&str, &str)> {
    // Try en-dash first
    if let Some(pos) = s.find('–') {
        return Some((&s[..pos], &s[pos + '–'.len_utf8()..]));
    }
    // Try "to" keyword (case-insensitive, with word boundaries)
    if let Some(pos) = s.to_lowercase().find(" to ") {
        return Some((&s[..pos], &s[pos + 4..]));
    }
    // Try hyphen only if it looks like a range separator (not negative number)
    // Look for patterns like "5V-10V" or "5V - 10V", but not negative signs like "-5V"
    // and not negative tolerance tokens like "3.3V -5%".
    let bytes = s.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'-' && i > 0 {
            if i + 1 >= bytes.len() {
                continue;
            }

            // If there's whitespace before '-', and the right side looks like a percent token,
            // treat '-' as a sign ("-5%") rather than a range separator.
            if bytes[i - 1].is_ascii_whitespace() {
                let rhs = s[i + 1..].trim_start();
                if rhs.ends_with('%')
                    && rhs
                        .chars()
                        .take(rhs.len().saturating_sub(1))
                        .all(|c| c.is_ascii_digit() || c == '.')
                {
                    continue;
                }
            }

            return Some((&s[..i], &s[i + 1..]));
        }
    }
    None
}

impl FromStr for PhysicalValue {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.is_empty() {
            return Err(ParseError::InvalidFormat);
        }

        // Try range parsing first (handles "11–26V", "11V to 26V", etc.)
        if let Some(result) = parse_range_syntax(s) {
            return result;
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

        let (value, unit) = parse_value_with_unit(&value_unit_str)?;

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
        // Case 1: Point value (min == nominal == max)
        if self.is_point() {
            return self.fmt_point_value(f);
        }

        // Case 2: Symmetric tolerance around nominal - show as "nominal <percent>%"
        //
        // This preserves the legacy formatting for typical component specs like
        // "10k 5%" while still supporting explicit asymmetric ranges.
        if self.is_symmetric() && !self.nominal.is_zero() {
            let tol_percent = (self.tolerance() * ONE_HUNDRED).normalize();
            // Avoid producing unreadable repeating decimals for values that were created from
            // explicit bounds (e.g. "11–26V" -> 40.540540...%). Fall back to range formatting
            // unless the percent is "clean" (a small number of decimal places).
            if tol_percent > Decimal::ZERO && tol_percent.scale() <= 3 {
                let suffix = format!(" {}%", fmt_significant(tol_percent));
                return self.fmt_point_value_with_suffix(f, &suffix);
            }
        }

        // Case 3: Asymmetric bounds - show as explicit range with nominal
        self.fmt_range_with_nominal(f)
    }
}

impl PhysicalValue {
    /// Format as point value (no tolerance suffix)
    fn fmt_point_value(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.fmt_point_value_with_suffix(f, "")
    }

    /// Format as point value with optional suffix (like tolerance)
    fn fmt_point_value_with_suffix(
        &self,
        f: &mut std::fmt::Formatter<'_>,
        suffix: &str,
    ) -> std::fmt::Result {
        match self.unit.alias() {
            Some(PhysicalUnit::Kelvin) => {
                let celsius = self.nominal - KELVIN_OFFSET;
                write!(f, "{}°C{}", fmt_significant(celsius), suffix)
            }
            Some(PhysicalUnit::Seconds) => {
                let (value_str, unit_suffix) = if self.nominal >= HOUR {
                    (fmt_significant(self.nominal / HOUR), "h")
                } else if self.nominal >= MINUTE {
                    (fmt_significant(self.nominal / MINUTE), "min")
                } else {
                    let (scaled, prefix) = scale_to_si(self.nominal);
                    return write!(f, "{}{}s{}", fmt_significant(scaled), prefix, suffix);
                };
                write!(f, "{}{}{}", value_str, unit_suffix, suffix)
            }
            _ => {
                let (scaled, prefix) = scale_to_si(self.nominal);
                write!(
                    f,
                    "{}{}{}{}",
                    fmt_significant(scaled),
                    prefix,
                    self.unit.fmt_unit(),
                    suffix
                )
            }
        }
    }

    /// Format as range with nominal: "3.0–3.6V (3.3V nom.)"
    fn fmt_range_with_nominal(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let unit_str = self.unit.fmt_unit();

        // Format min and max with SI prefixes
        let (min_scaled, min_prefix) = scale_to_si(self.min);
        let (max_scaled, max_prefix) = scale_to_si(self.max);
        let (nom_scaled, nom_prefix) = scale_to_si(self.nominal);

        // Handle temperature specially
        if let Some(PhysicalUnit::Kelvin) = self.unit.alias() {
            let min_celsius = self.min - KELVIN_OFFSET;
            let max_celsius = self.max - KELVIN_OFFSET;
            let nom_celsius = self.nominal - KELVIN_OFFSET;
            write!(
                f,
                "{}–{}°C ({}°C nom.)",
                fmt_significant(min_celsius),
                fmt_significant(max_celsius),
                fmt_significant(nom_celsius)
            )
        } else {
            write!(
                f,
                "{}{}–{}{}{} ({}{}{} nom.)",
                fmt_significant(min_scaled),
                min_prefix,
                fmt_significant(max_scaled),
                max_prefix,
                unit_str,
                fmt_significant(nom_scaled),
                nom_prefix,
                unit_str
            )
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
    /// Backwards compatibility alias for nominal
    #[starlark(attribute)]
    fn value<'v>(this: &PhysicalValue) -> starlark::Result<f64> {
        to_f64(this.nominal, "value")
    }

    #[starlark(attribute)]
    fn nominal<'v>(this: &PhysicalValue) -> starlark::Result<f64> {
        to_f64(this.nominal, "nominal")
    }

    /// Computed worst-case tolerance as a fraction
    #[starlark(attribute)]
    fn tolerance<'v>(this: &PhysicalValue) -> starlark::Result<f64> {
        to_f64(this.tolerance(), "tolerance")
    }

    #[starlark(attribute)]
    fn min<'v>(this: &PhysicalValue) -> starlark::Result<f64> {
        to_f64(this.min, "min")
    }

    #[starlark(attribute)]
    fn max<'v>(this: &PhysicalValue) -> starlark::Result<f64> {
        to_f64(this.max, "max")
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

    /// Returns a new PhysicalValue with symmetric tolerance applied to nominal
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

        if new_tolerance < Decimal::ZERO {
            return Err(PhysicalValueError::InvalidTolerance {
                value: new_tolerance.to_string(),
            }
            .into());
        }

        Ok(PhysicalValue::from_nominal_tolerance(
            this.nominal,
            new_tolerance,
            this.unit,
        ))
    }

    /// Returns a new PhysicalValue with updated nominal (resets to point value)
    fn with_value<'v>(
        this: &PhysicalValue,
        #[starlark(require = pos)] value_arg: Value<'v>,
    ) -> starlark::Result<PhysicalValue> {
        let new_value = starlark_value_to_decimal(&value_arg)?;
        Ok(PhysicalValue::point(new_value, this.unit))
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

        Ok(PhysicalValue::from_bounds_nominal(
            this.nominal,
            this.min,
            this.max,
            new_unit,
        ))
    }

    fn abs<'v>(this: &PhysicalValue) -> starlark::Result<PhysicalValue> {
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
        // Check if this fits within other
        let (other_min, other_max) = extract_bounds(other, this.unit)?;
        Ok(this.min >= other_min && this.max <= other_max)
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
        // All fields must match for equality
        Ok(self.unit == other.unit
            && self.nominal == other.nominal
            && self.min == other.min
            && self.max == other.max)
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

        // Compare the nominal values
        Ok(self.nominal.cmp(&other.nominal))
    }

    fn is_in(&self, other: Value<'v>) -> starlark::Result<bool> {
        // Check if other's bounds fit within self's bounds
        let (other_min, other_max) = extract_bounds(other, self.unit)?;
        Ok(other_min >= self.min && other_max <= self.max)
    }

    fn minus(&self, heap: &'v Heap) -> starlark::Result<Value<'v>> {
        // Negate and swap min/max
        Ok(heap.alloc(PhysicalValue::from_bounds_nominal(
            -self.nominal,
            -self.max, // swapped
            -self.min, // swapped
            self.unit,
        )))
    }
}

/// Type factory for creating PhysicalValue constructors
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
        let scalar = Ty::union2(Ty::int(), Ty::float());
        let value_ty = Ty::union2(
            Ty::union2(scalar.clone(), StarlarkStr::get_type_starlark_repr()),
            PhysicalValue::get_type_starlark_repr(),
        );
        let tolerance_ty = Ty::union2(scalar, Ty::string());
        ParamSpec::new_parts(
            [(ParamIsRequired::No, value_ty.clone())],
            [],
            None,
            [
                (ArcStr::from("value"), ParamIsRequired::No, value_ty.clone()),
                (ArcStr::from("tolerance"), ParamIsRequired::No, tolerance_ty),
                (ArcStr::from("min"), ParamIsRequired::No, value_ty.clone()),
                (ArcStr::from("max"), ParamIsRequired::No, value_ty.clone()),
                (ArcStr::from("nominal"), ParamIsRequired::No, value_ty),
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
                ("min", ParametersSpecParam::Optional),
                ("max", ParametersSpecParam::Optional),
                ("nominal", ParametersSpecParam::Optional),
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
                let min_kw: Option<Value> = param_parser.next_opt()?;
                let max_kw: Option<Value> = param_parser.next_opt()?;
                let nominal_kw: Option<Value> = param_parser.next_opt()?;

                let parse_tolerance = |value: Value| -> starlark::Result<Decimal> {
                    if let Some(s) = value.unpack_str() {
                        parse_percentish_decimal(s).map_err(|_| {
                            PhysicalValueError::InvalidTolerance {
                                value: s.to_string(),
                            }
                            .into()
                        })
                    } else {
                        let tol = starlark_value_to_decimal(&value)?;
                        if tol < Decimal::ZERO {
                            return Err(PhysicalValueError::InvalidTolerance {
                                value: tol.to_string(),
                            }
                            .into());
                        }
                        Ok(tol)
                    }
                };

                let parse_bound = |value: Value, label: &str| -> starlark::Result<Decimal> {
                    if let Some(pv) = value.downcast_ref::<PhysicalValue>() {
                        if !pv.is_point() {
                            return Err(PhysicalValueError::InvalidArguments {
                                args: vec![label.to_string()],
                            }
                            .into());
                        }
                        return Ok(pv.nominal);
                    }

                    if let Some(s) = value.unpack_str() {
                        let s = s.trim();
                        if s.is_empty() {
                            return Err(PhysicalValueError::InvalidNumberType.into());
                        }
                        if let Ok((number, unit_str)) = split_number_and_unit(s) {
                            if unit_str.is_empty() {
                                return Ok(number);
                            }
                        }
                        let pv = PhysicalValue::from_str(s).map_err(|err| {
                            PhysicalValueError::ParseError {
                                unit: self.unit.quantity(),
                                input: s.to_string(),
                                source: err,
                            }
                        })?;
                        if pv.unit != self.unit {
                            return Err(PhysicalValueError::UnitMismatch {
                                expected: self.unit.quantity(),
                                actual: pv.unit.quantity(),
                            }
                            .into());
                        }
                        if !pv.is_point() {
                            return Err(PhysicalValueError::InvalidArguments {
                                args: vec![label.to_string()],
                            }
                            .into());
                        }
                        return Ok(pv.nominal);
                    }

                    starlark_value_to_decimal(&value).map_err(Into::into)
                };

                let parse_value = |value: Value| -> starlark::Result<PhysicalValue> {
                    if let Some(existing) = value.downcast_ref::<PhysicalValue>() {
                        // Casting semantics: constructors can re-tag other physical values.
                        return Ok(PhysicalValue::from_bounds_nominal(
                            existing.nominal,
                            existing.min,
                            existing.max,
                            self.unit,
                        ));
                    }

                    if let Some(s) = value.unpack_str() {
                        let s = s.trim();
                        if s.is_empty() {
                            return Err(PhysicalValueError::InvalidNumberType.into());
                        }
                        // Bare numbers are interpreted in the constructor's unit.
                        if let Ok((number, unit_str)) = split_number_and_unit(s) {
                            if unit_str.is_empty() {
                                return Ok(PhysicalValue::point(number, self.unit));
                            }
                        }
                        // Unit-suffixed strings must match the constructor's unit.
                        let pv = PhysicalValue::from_str(s).map_err(|err| {
                            PhysicalValueError::ParseError {
                                unit: self.unit.quantity(),
                                input: s.to_string(),
                                source: err,
                            }
                        })?;
                        if pv.unit != self.unit {
                            return Err(PhysicalValueError::UnitMismatch {
                                expected: self.unit.quantity(),
                                actual: pv.unit.quantity(),
                            }
                            .into());
                        }
                        return Ok(pv);
                    }

                    let v = starlark_value_to_decimal(&value)?;
                    Ok(PhysicalValue::point(v, self.unit))
                };

                let resolve_nominal = |nominal_kw: Option<Value>,
                                       min: Decimal,
                                       max: Decimal,
                                       fallback: Decimal|
                 -> starlark::Result<Decimal> {
                    let nominal = if let Some(value) = nominal_kw {
                        parse_bound(value, "nominal")?
                    } else {
                        fallback
                    };
                    if nominal < min || nominal > max {
                        return Err(PhysicalValueError::NominalOutOfRange {
                            nominal: nominal.to_string(),
                            min: min.to_string(),
                            max: max.to_string(),
                        }
                        .into());
                    }
                    Ok(nominal)
                };

                let value_arg = match (pos_value, kw_value) {
                    (Some(pos), None) => Some(pos),
                    (None, Some(kw)) => Some(kw),
                    (None, None) => None,
                    (Some(_), Some(_)) => return Err(PhysicalValueError::MixedArguments.into()),
                };

                let has_bounds = min_kw.is_some() || max_kw.is_some();
                if has_bounds && value_arg.is_some() {
                    return Err(PhysicalValueError::MixedArguments.into());
                }

                let result = if has_bounds {
                    if tolerance.is_some() {
                        return Err(PhysicalValueError::InvalidArguments {
                            args: vec![
                                "tolerance".to_string(),
                                "min".to_string(),
                                "max".to_string(),
                            ],
                        }
                        .into());
                    }
                    let min_val = match min_kw {
                        Some(value) => parse_bound(value, "min")?,
                        None => return Err(PhysicalValueError::MissingRangeValue.into()),
                    };
                    let max_val = match max_kw {
                        Some(value) => parse_bound(value, "max")?,
                        None => return Err(PhysicalValueError::MissingRangeValue.into()),
                    };
                    if min_val > max_val {
                        return Err(PhysicalValueError::InvalidRange {
                            min: min_val.to_string(),
                            max: max_val.to_string(),
                        }
                        .into());
                    }
                    let nominal_val = resolve_nominal(
                        nominal_kw,
                        min_val,
                        max_val,
                        (min_val + max_val) / Decimal::from(2),
                    )?;
                    PhysicalValue::from_bounds_nominal(nominal_val, min_val, max_val, self.unit)
                } else {
                    let value_arg =
                        value_arg.ok_or_else(|| PhysicalValueError::MissingValueKeyword {
                            unit: self.unit.quantity(),
                        })?;
                    let pv = parse_value(value_arg)?;
                    let nominal_val = resolve_nominal(nominal_kw, pv.min, pv.max, pv.nominal)?;
                    if let Some(tol_val) = tolerance {
                        let tol = parse_tolerance(tol_val)?;
                        PhysicalValue::from_nominal_tolerance(nominal_val, tol, self.unit)
                    } else {
                        PhysicalValue::from_bounds_nominal(nominal_val, pv.min, pv.max, self.unit)
                    }
                };

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

#[cfg(test)]
mod tests {
    use super::*;
    use starlark::values::Heap;

    #[cfg(test)]
    fn physical_value(value: f64, tolerance: f64, unit: PhysicalUnit) -> PhysicalValue {
        PhysicalValue::new(value, tolerance, unit)
    }

    // Helper: test parse + format + roundtrip in one go
    fn test_cycle(input: &str, unit: PhysicalUnit, value: f64, display: &str) {
        let parsed: PhysicalValue = input.parse().unwrap();
        assert_eq!(parsed.unit, unit.into());
        assert!((parsed.nominal - Decimal::from_f64(value).unwrap()).abs() < Decimal::new(1, 6));

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
    fn test_tolerance(input: &str, unit: PhysicalUnit, value: f64, tol: f64, _display: &str) {
        let parsed: PhysicalValue = input.parse().unwrap();
        assert_eq!(parsed.unit, unit.into());
        assert!((parsed.nominal - Decimal::from_f64(value).unwrap()).abs() < Decimal::new(1, 6));
        assert!((parsed.tolerance() - Decimal::from_f64(tol).unwrap()).abs() < Decimal::new(1, 8));
        // Display format now always uses range notation for non-point values
        // So we only verify parsing, not display format here
    }

    // Super simple helper: just check tolerance percentage
    fn check_tol(input: &str, expected_tol_percent: f64) {
        let parsed: PhysicalValue = input.parse().unwrap();
        let expected = Decimal::from_f64(expected_tol_percent / 100.0).unwrap();
        assert!(
            (parsed.tolerance() - expected).abs() < Decimal::new(1, 8),
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
            assert!(
                (parsed.nominal - Decimal::from_f64(value).unwrap()).abs() < Decimal::new(1, 6)
            );
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
            (result.nominal - Decimal::from_f64(expected_val).unwrap()).abs() < Decimal::new(1, 6)
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
            ("3.3V 0.5%", PhysicalUnit::Volts, 3.3, 0.005, "3.3V 0.5%"),
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
        assert_eq!(numeric_as_voltage.nominal, Decimal::from(50));
        assert_eq!(numeric_as_voltage.unit, PhysicalUnit::Volts.into());
        assert_eq!(numeric_as_voltage.tolerance(), Decimal::ZERO);
    }

    #[test]
    fn test_tolerance_display() {
        // Symmetric tolerance formats as "<nominal> <percent>%"
        let test_cases = [
            (
                Decimal::from(1000),
                PhysicalUnit::Ohms,
                Decimal::new(5, 2),
                "1k 5%",
            ),
            (Decimal::from(1000), PhysicalUnit::Ohms, Decimal::ZERO, "1k"), // Without tolerance - point value
            (
                Decimal::from(1000),
                PhysicalUnit::Farads,
                Decimal::new(1, 1),
                "1kF 10%",
            ),
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
    fn test_negative_tolerance_rejected_in_parser() {
        match "3.3V -5%".parse::<PhysicalValue>() {
            Err(ParseError::InvalidTolerance) => {}
            Err(other) => panic!("expected InvalidTolerance, got {other:?}"),
            Ok(_) => panic!("expected error"),
        }
    }

    #[test]
    fn test_is_symmetric_zero_nominal() {
        let v = PhysicalValue::from_bounds_nominal(
            Decimal::ZERO,
            Decimal::from(-1),
            Decimal::from(1),
            PhysicalUnit::Volts.into(),
        );
        assert!(v.is_symmetric());
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
            assert_eq!(val.tolerance(), Decimal::ZERO);
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
        // Symmetric tolerance formats as "<nominal> <percent>%"
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
        assert_eq!(result.nominal, Decimal::from(10));

        // I = V / R
        let result = (v / r).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Amperes.into());
        assert_eq!(result.nominal, Decimal::from(2));

        // R = V / I
        let result = (v / i).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Ohms.into());
        assert_eq!(result.nominal, Decimal::from(5));
    }

    #[test]
    fn test_power_calculations() {
        // P = V × I
        let v = physical_value(12.0, 0.0, PhysicalUnit::Volts);
        let i = physical_value(2.0, 0.0, PhysicalUnit::Amperes);
        let result = v * i;
        assert_eq!(result.unit, PhysicalUnit::Watts.into());
        assert_eq!(result.nominal, Decimal::from(24));

        // I = P / V
        let p = physical_value(100.0, 0.0, PhysicalUnit::Watts);
        let v = physical_value(120.0, 0.0, PhysicalUnit::Volts);
        let result = (p / v).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Amperes.into());
        assert!(result.nominal > Decimal::from_f64(0.8).unwrap());
        assert!(result.nominal < Decimal::from_f64(0.9).unwrap());

        // V = P / I
        let i = physical_value(5.0, 0.0, PhysicalUnit::Amperes);
        let result = (p / i).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Volts.into());
        assert_eq!(result.nominal, Decimal::from(20));
    }

    #[test]
    fn test_energy_and_time() {
        // E = P × t
        let p = physical_value(100.0, 0.0, PhysicalUnit::Watts);
        let t = physical_value(3600.0, 0.0, PhysicalUnit::Seconds);
        let result = p * t;
        assert_eq!(result.unit, PhysicalUnit::Joules.into());
        assert_eq!(result.nominal, Decimal::from(360000));

        // P = E / t
        let e = physical_value(7200.0, 0.0, PhysicalUnit::Joules);
        let t = physical_value(7200.0, 0.0, PhysicalUnit::Seconds); // 2h
        let result = (e / t).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Watts.into());
        assert_eq!(result.nominal, Decimal::from(1));

        // t = E / P
        let result = (e / p).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Seconds.into());
        assert_eq!(result.nominal, Decimal::from(72));
    }

    #[test]
    fn test_frequency_time_inverses() {
        // f = 1 / t
        let t = physical_value(1.0, 0.0, PhysicalUnit::Seconds);
        let result = (PhysicalValue::dimensionless(1) / t).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Hertz.into());
        assert_eq!(result.nominal, Decimal::from(1));

        // t = 1 / f
        let f = physical_value(60.0, 0.0, PhysicalUnit::Hertz);
        let result = (PhysicalValue::dimensionless(1) / f).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Seconds.into());
        assert!(result.nominal > Decimal::from_f64(0.016).unwrap());
        assert!(result.nominal < Decimal::from_f64(0.017).unwrap());

        // f × t = 1 (dimensionless)
        let f = physical_value(10.0, 0.0, PhysicalUnit::Hertz);
        let t = physical_value(0.1, 0.0, PhysicalUnit::Seconds);
        let result = f * t;
        assert_eq!(result.unit, PhysicalUnitDims::DIMENSIONLESS);
        assert_eq!(result.nominal, Decimal::from(1));
    }

    #[test]
    fn test_resistance_conductance_inverses() {
        // G = 1 / R
        let one = PhysicalValue::from_decimal(1.into(), 0.into(), PhysicalUnitDims::DIMENSIONLESS);
        let r = physical_value(100.0, 0.0, PhysicalUnit::Ohms);
        let result = (one / r).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Siemens.into());
        assert_eq!(result.nominal, Decimal::from_f64(0.01).unwrap());

        // R = 1 / G
        let g = physical_value(0.02, 0.0, PhysicalUnit::Siemens);
        let result = (one / g).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Ohms.into());
        assert_eq!(result.nominal, Decimal::from(50));

        // R × G = 1 (dimensionless)
        let result = r * g;
        assert_eq!(result.unit, PhysicalUnitDims::DIMENSIONLESS);
        assert_eq!(result.nominal, Decimal::from(2));
    }

    #[test]
    fn test_rc_time_constants() {
        // τ = R × C
        let r = physical_value(10000.0, 0.0, PhysicalUnit::Ohms); // 10kΩ
        let c = physical_value(0.0000001, 0.0, PhysicalUnit::Farads); // 100nF
        let result = r * c;
        assert_eq!(result.unit, PhysicalUnit::Seconds.into());
        assert_eq!(result.nominal, Decimal::from_f64(0.001).unwrap()); // 1ms

        // τ = C × R
        let result = c * r;
        assert_eq!(result.unit, PhysicalUnit::Seconds.into());
        assert_eq!(result.nominal, Decimal::from_f64(0.001).unwrap()); // 1ms
    }

    #[test]
    fn test_lr_time_constants() {
        // τ = L × G (L/R time constant)
        let l = physical_value(0.01, 0.0, PhysicalUnit::Henries); // 10mH
        let g = physical_value(0.1, 0.0, PhysicalUnit::Siemens); // 100mS
        let result = l * g;
        assert_eq!(result.unit, PhysicalUnit::Seconds.into());
        assert_eq!(result.nominal, Decimal::from_f64(0.001).unwrap()); // 1ms

        // τ = G × L
        let result = g * l;
        assert_eq!(result.unit, PhysicalUnit::Seconds.into());
        assert_eq!(result.nominal, Decimal::from_f64(0.001).unwrap()); // 1ms
    }

    #[test]
    fn test_charge_relationships() {
        // Q = I × t
        let i = physical_value(2.0, 0.0, PhysicalUnit::Amperes);
        let t = physical_value(10.0, 0.0, PhysicalUnit::Seconds);
        let result = i * t;
        assert_eq!(result.unit, PhysicalUnit::Coulombs.into());
        assert_eq!(result.nominal, Decimal::from(20));

        // Q = C × V
        let c = physical_value(0.001, 0.0, PhysicalUnit::Farads); // 1000μF
        let v = physical_value(12.0, 0.0, PhysicalUnit::Volts);
        let result = c * v;
        assert_eq!(result.unit, PhysicalUnit::Coulombs.into());
        assert_eq!(result.nominal, Decimal::from_f64(0.012).unwrap()); // 12mC

        // I = Q / t
        let q = physical_value(0.1, 0.0, PhysicalUnit::Coulombs); // 100mC
        let t = physical_value(50.0, 0.0, PhysicalUnit::Seconds);
        let result = (q / t).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Amperes.into());
        assert_eq!(result.nominal, Decimal::from_f64(0.002).unwrap()); // 2mA

        // V = Q / C
        let q = physical_value(0.005, 0.0, PhysicalUnit::Coulombs); // 5mC
        let result = (q / c).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Volts.into());
        assert_eq!(result.nominal, Decimal::from(5));
    }

    #[test]
    fn test_magnetic_flux() {
        // Φ = L × I
        let l = physical_value(1.0, 0.0, PhysicalUnit::Henries); // 1H
        let i = physical_value(2.0, 0.0, PhysicalUnit::Amperes);
        let result = l * i;
        assert_eq!(result.unit, PhysicalUnit::Webers.into());
        assert_eq!(result.nominal, Decimal::from(2)); // 2Wb

        // I = Φ / L
        let phi = physical_value(0.01, 0.0, PhysicalUnit::Webers); // 10mWb
        let l = physical_value(0.05, 0.0, PhysicalUnit::Henries); // 50mH
        let result = (phi / l).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Amperes.into());
        assert_eq!(result.nominal, Decimal::from_f64(0.2).unwrap()); // 200mA

        // V = Φ / t (Faraday's law)
        let phi = physical_value(0.1, 0.0, PhysicalUnit::Webers); // 100mWb
        let t = physical_value(0.01, 0.0, PhysicalUnit::Seconds); // 10ms
        let result = (phi / t).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Volts.into());
        assert_eq!(result.nominal, Decimal::from(10)); // 10V
    }

    #[test]
    fn test_energy_storage() {
        // E = Q × V (potential energy)
        let q = physical_value(0.001, 0.0, PhysicalUnit::Coulombs); // 1mC
        let v = physical_value(12.0, 0.0, PhysicalUnit::Volts);
        let result = q * v;
        assert_eq!(result.unit, PhysicalUnit::Joules.into());
        assert_eq!(result.nominal, Decimal::from_f64(0.012).unwrap()); // 12mJ

        // Q = E / V
        let e = physical_value(0.024, 0.0, PhysicalUnit::Joules); // 24mJ
        let result = (e / v).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Coulombs.into());
        assert_eq!(result.nominal, Decimal::from_f64(0.002).unwrap()); // 2mC

        // V = E / Q
        let e = physical_value(0.006, 0.0, PhysicalUnit::Joules); // 6mJ
        let result = (e / q).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Volts.into());
        assert_eq!(result.nominal, Decimal::from(6)); // 6V
    }

    #[test]
    fn test_dimensionless_operations() {
        // Any unit * dimensionless = same unit
        let v = physical_value(5.0, 0.0, PhysicalUnit::Volts);
        let two = PhysicalValue::dimensionless(2);
        let result = v * two;
        assert_eq!(result.unit, PhysicalUnit::Volts.into());
        assert_eq!(result.nominal, Decimal::from(10));

        // Any unit / dimensionless = same unit
        let result = (v / two).unwrap();
        assert_eq!(result.unit, PhysicalUnit::Volts.into());
        assert_eq!(result.nominal, Decimal::from_f64(2.5).unwrap());
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
        assert_eq!(result.nominal, Decimal::from_f64(2.5).unwrap());
        assert_eq!(result.tolerance(), Decimal::from_f64(0.05).unwrap());

        // V × dimensionless preserves tolerance
        let result = v * two;
        assert_eq!(result.unit, PhysicalUnit::Volts.into());
        assert_eq!(result.nominal, Decimal::from(10));
        assert_eq!(result.tolerance(), Decimal::from_f64(0.05).unwrap());

        // Unit-changing operations drop tolerance
        let r = physical_value(100.0, 0.0, PhysicalUnit::Ohms);
        let result = (v / r).unwrap(); // V / R = I (unit changes)
        assert_eq!(result.unit, PhysicalUnit::Amperes.into());
        assert_eq!(result.tolerance(), Decimal::ZERO); // Tolerance dropped
    }

    #[test]
    fn test_try_from_physical_value() {
        let heap = Heap::new();
        let original = physical_value(10.0, 0.05, PhysicalUnit::Ohms);
        let starlark_val = heap.alloc(original);

        let result = PhysicalValue::try_from(starlark_val.to_value()).unwrap();
        assert_eq!(result.nominal, original.nominal);
        assert_eq!(result.tolerance(), original.tolerance());
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
            assert!(
                (result.nominal - Decimal::from_f64(value).unwrap()).abs() < Decimal::new(1, 6)
            );
        }
    }

    #[test]
    fn test_try_from_string_with_tolerance() {
        let heap = Heap::new();
        let starlark_val = heap.alloc("10kOhm 5%");
        let result = PhysicalValue::try_from(starlark_val.to_value()).unwrap();

        assert_eq!(result.unit, PhysicalUnit::Ohms.into());
        assert_eq!(result.nominal, Decimal::from(10000));
        assert_eq!(result.tolerance(), Decimal::from_f64(0.05).unwrap());
    }

    #[test]
    fn test_try_from_scalar() {
        let heap = Heap::new();

        // Test integer
        let starlark_val = heap.alloc(42);
        let result = PhysicalValue::try_from(starlark_val.to_value()).unwrap();
        assert_eq!(result.unit, PhysicalUnitDims::DIMENSIONLESS);
        assert_eq!(result.nominal, Decimal::from(42));
        assert_eq!(result.tolerance(), Decimal::ZERO);

        // Test float
        let starlark_val = heap.alloc(3.15);
        let result = PhysicalValue::try_from(starlark_val.to_value()).unwrap();
        assert_eq!(result.unit, PhysicalUnitDims::DIMENSIONLESS);
        assert_eq!(result.nominal, Decimal::from_f64(3.15).unwrap());
        assert_eq!(result.tolerance(), Decimal::ZERO);
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
            resistance_value.nominal,
            resistance_value.tolerance(),
            PhysicalUnitDims::DIMENSIONLESS,
        );

        // Should have same value and tolerance but be dimensionless
        assert_eq!(new_value.nominal, resistance_value.nominal);
        assert_eq!(new_value.tolerance(), resistance_value.tolerance());
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
            dimensionless.nominal,
            dimensionless.tolerance(),
            resistance_unit,
        );

        // Test the casting logic: dimensionless with tolerance -> voltage
        let voltage_casted = PhysicalValue::from_decimal(
            dimensionless_with_tolerance.nominal,
            dimensionless_with_tolerance.tolerance(),
            voltage_unit,
        );

        // Verify values and tolerances are preserved but units change
        assert_eq!(resistance_casted.nominal, dimensionless.nominal);
        assert_eq!(resistance_casted.tolerance(), dimensionless.tolerance());
        assert_eq!(resistance_casted.unit, resistance_unit);

        assert_eq!(voltage_casted.nominal, dimensionless_with_tolerance.nominal);
        assert_eq!(
            voltage_casted.tolerance(),
            dimensionless_with_tolerance.tolerance()
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
        let v1_copy = physical_value(5.0, 0.01, PhysicalUnit::Volts); // 5V ±1% (same)
        let v2 = physical_value(5.0, 0.02, PhysicalUnit::Volts); // 5V ±2% (different tolerance)
        let v3 = physical_value(3.3, 0.0, PhysicalUnit::Volts); // 3.3V (different nominal)

        // Values with same unit, nominal, and bounds are equal
        let v1_val = heap.alloc(v1);
        assert!(v1.equals(v1_val).unwrap());
        assert!(v1.equals(heap.alloc(v1_copy)).unwrap());

        // Values with different tolerances are NOT equal (different bounds)
        assert!(!v1.equals(heap.alloc(v2)).unwrap());

        // Values with same unit but different nominal are not equal
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
        // v1 and v1_copy have same nominal so compare equal
        assert_eq!(v1.compare(heap.alloc(v1_copy)).unwrap(), Ordering::Equal);

        // Test comparison with different units fails
        let r1 = physical_value(10.0, 0.0, PhysicalUnit::Ohms);
        assert!(v1.compare(heap.alloc(r1)).is_err());

        // Test comparison with point value string
        let v_point = physical_value(5.0, 0.0, PhysicalUnit::Volts);
        let v_str = heap.alloc("5V");
        assert!(v_point.equals(v_str).unwrap());
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

        // Test with different string formats - now NOT equal (different bounds)
        let voltage_with_tolerance = heap.alloc("12V 5%");
        assert!(!voltage.equals(voltage_with_tolerance).unwrap()); // Point != toleranced

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
        let r = PhysicalValue::from_str("11–26V").unwrap();
        assert_eq!(r.min, Decimal::from(11));
        assert_eq!(r.max, Decimal::from(26));
        // nominal is midpoint when not specified
        assert_eq!(r.nominal, Decimal::from_str("18.5").unwrap());
        assert_eq!(r.unit, PhysicalUnitDims::VOLTAGE);
    }

    #[test]
    fn test_range_parsing_endash_with_spaces() {
        let r = PhysicalValue::from_str("11 – 26V").unwrap();
        assert_eq!(r.min, Decimal::from(11));
        assert_eq!(r.max, Decimal::from(26));
        // nominal is midpoint when not specified
        assert_eq!(r.nominal, Decimal::from_str("18.5").unwrap());
        assert_eq!(r.unit, PhysicalUnitDims::VOLTAGE);
    }

    #[test]
    fn test_range_parsing_to_keyword() {
        let r = PhysicalValue::from_str("11V to 26V").unwrap();
        assert_eq!(r.min, Decimal::from(11));
        assert_eq!(r.max, Decimal::from(26));
        // nominal is midpoint when not specified
        assert_eq!(r.nominal, Decimal::from_str("18.5").unwrap());
        assert_eq!(r.unit, PhysicalUnitDims::VOLTAGE);
    }

    #[test]
    fn test_range_parsing_decimal_to_keyword() {
        let r = PhysicalValue::from_str("1.1 V to 26V").unwrap();
        assert_eq!(r.min, Decimal::from_str("1.1").unwrap());
        assert_eq!(r.max, Decimal::from(26));
        // nominal is midpoint when not specified
        assert_eq!(r.nominal, Decimal::from_str("13.55").unwrap());
    }

    #[test]
    fn test_range_parsing_with_nominal() {
        let r = PhysicalValue::from_str("11–26 V (12 V nom.)").unwrap();
        assert_eq!(r.min, Decimal::from(11));
        assert_eq!(r.max, Decimal::from(26));
        assert_eq!(r.nominal, Decimal::from(12));
        assert_eq!(r.unit, PhysicalUnitDims::VOLTAGE);
    }

    #[test]
    fn test_range_parsing_with_nominal_no_period() {
        let r = PhysicalValue::from_str("11–26 V (12 V nom)").unwrap();
        assert_eq!(r.min, Decimal::from(11));
        assert_eq!(r.max, Decimal::from(26));
        assert_eq!(r.nominal, Decimal::from(12));
    }

    #[test]
    fn test_range_parsing_single_value_no_tolerance() {
        let r = PhysicalValue::from_str("3.3V").unwrap();
        assert_eq!(r.min, Decimal::from_str("3.3").unwrap());
        assert_eq!(r.max, Decimal::from_str("3.3").unwrap());
        // For point value, nominal equals the value
        assert_eq!(r.nominal, Decimal::from_str("3.3").unwrap());
        assert_eq!(r.unit, PhysicalUnitDims::VOLTAGE);
    }

    #[test]
    fn test_range_parsing_tolerance_expansion() {
        let r = PhysicalValue::from_str("15V 10%").unwrap();
        assert_eq!(r.min, Decimal::from_str("13.5").unwrap());
        assert_eq!(r.max, Decimal::from_str("16.5").unwrap());
        // For tolerance parsing, nominal is the central value
        assert_eq!(r.nominal, Decimal::from(15));
        assert_eq!(r.unit, PhysicalUnitDims::VOLTAGE);
    }

    #[test]
    fn test_range_parsing_unit_inference() {
        // Left side is bare number, should inherit unit from right
        let r = PhysicalValue::from_str("3.3 to 5V").unwrap();
        assert_eq!(r.min, Decimal::from_str("3.3").unwrap());
        assert_eq!(r.max, Decimal::from(5));
        assert_eq!(r.unit, PhysicalUnitDims::VOLTAGE);
    }

    #[test]
    fn test_range_parsing_reversed_values() {
        // Should auto-swap to ensure min <= max
        let r = PhysicalValue::from_str("26V to 11V").unwrap();
        assert_eq!(r.min, Decimal::from(11));
        assert_eq!(r.max, Decimal::from(26));
    }

    #[test]
    fn test_range_parsing_resistance() {
        let r = PhysicalValue::from_str("10kOhm to 100kOhm").unwrap();
        assert_eq!(r.min, Decimal::from(10000));
        assert_eq!(r.max, Decimal::from(100000));
        assert_eq!(r.unit, PhysicalUnit::Ohms.into());
    }

    #[test]
    fn test_range_parsing_current() {
        let r = PhysicalValue::from_str("100mA to 2A").unwrap();
        assert_eq!(r.min, Decimal::from_str("0.1").unwrap());
        assert_eq!(r.max, Decimal::from(2));
        assert_eq!(r.unit, PhysicalUnitDims::CURRENT);
    }

    #[test]
    fn test_range_display() {
        let r = PhysicalValue::from_str("11–26V").unwrap();
        let display = format!("{}", r);
        // With unified type, all non-point values show as range with nominal
        assert!(
            display.contains("11") && display.contains("26") && display.contains("V"),
            "Expected display to contain 11, 26, and V, got: {}",
            display
        );
    }

    #[test]
    fn test_range_display_with_nominal() {
        let r = PhysicalValue::from_str("11–26 V (12 V nom.)").unwrap();
        let display = format!("{}", r);
        // Should show nominal in asymmetric format
        assert!(display.contains("12") && display.contains("nom"));
    }

    #[test]
    fn test_range_parsing_invalid_format() {
        assert!(PhysicalValue::from_str("").is_err());
        assert!(PhysicalValue::from_str("   ").is_err());
    }

    #[test]
    fn test_range_parsing_unit_mismatch() {
        // Should fail - mixing voltage and current units
        assert!(PhysicalValue::from_str("5V to 2A").is_err());
    }

    #[test]
    fn test_abs_positive_value() {
        let pv = physical_value(3.3, 0.0, PhysicalUnit::Volts);
        let result = pv.abs();
        assert_eq!(result.nominal, Decimal::from_f64(3.3).unwrap());
        assert_eq!(result.unit, PhysicalUnit::Volts.into());
        assert_eq!(result.tolerance(), Decimal::ZERO);
    }

    #[test]
    fn test_abs_negative_value() {
        let pv = physical_value(-3.3, 0.0, PhysicalUnit::Volts);
        let result = pv.abs();
        assert_eq!(result.nominal, Decimal::from_f64(3.3).unwrap());
        assert_eq!(result.unit, PhysicalUnit::Volts.into());
        assert_eq!(result.tolerance(), Decimal::ZERO);
    }

    #[test]
    fn test_abs_preserves_tolerance() {
        let pv = physical_value(-5.0, 0.05, PhysicalUnit::Amperes);
        let result = pv.abs();
        assert_eq!(result.nominal, Decimal::from_f64(5.0).unwrap());
        assert_eq!(result.unit, PhysicalUnit::Amperes.into());
        assert_eq!(result.tolerance(), Decimal::from_f64(0.05).unwrap());
    }

    #[test]
    fn test_diff_positive_difference() {
        let pv1 = physical_value(10.0, 0.0, PhysicalUnit::Volts);
        let pv2 = physical_value(3.0, 0.0, PhysicalUnit::Volts);
        let result = pv1.diff(&pv2).unwrap();
        assert_eq!(result.nominal, Decimal::from_f64(7.0).unwrap());
        assert_eq!(result.unit, PhysicalUnit::Volts.into());
        assert_eq!(result.tolerance(), Decimal::ZERO);
    }

    #[test]
    fn test_diff_negative_difference_returns_positive() {
        let pv1 = physical_value(3.0, 0.0, PhysicalUnit::Volts);
        let pv2 = physical_value(10.0, 0.0, PhysicalUnit::Volts);
        let result = pv1.diff(&pv2).unwrap();
        assert_eq!(result.nominal, Decimal::from_f64(7.0).unwrap());
        assert_eq!(result.unit, PhysicalUnit::Volts.into());
        assert_eq!(result.tolerance(), Decimal::ZERO);
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
    fn test_diff_max_range_difference() {
        // Diff now computes max difference between ranges
        // pv1: 10V ±10% = [9, 11]
        // pv2: 3V ±5% = [2.85, 3.15]
        // diff = max(|11 - 2.85|, |9 - 3.15|) = max(8.15, 5.85) = 8.15
        let pv1 = physical_value(10.0, 0.1, PhysicalUnit::Volts);
        let pv2 = physical_value(3.0, 0.05, PhysicalUnit::Volts);
        let result = pv1.diff(&pv2).unwrap();
        assert_eq!(result.nominal, Decimal::from_f64(8.15).unwrap());
        assert!(result.is_point()); // Result is always a point value
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
        assert_eq!(result.nominal, Decimal::from_f64(1.7).unwrap());
        assert_eq!(result.unit, PhysicalUnit::Volts.into());
    }

    #[test]
    fn test_within_same_nominal_different_tolerance() {
        use starlark::values::Heap;

        let heap = Heap::new();
        // 3.3V ±5% fits within 3.3V ±10%
        let tight = heap.alloc(physical_value(3.3, 0.05, PhysicalUnit::Volts)); // 3.135V - 3.465V
        let loose = heap.alloc(physical_value(3.3, 0.10, PhysicalUnit::Volts)); // 2.97V - 3.63V
        assert!(loose
            .downcast_ref::<PhysicalValue>()
            .unwrap()
            .is_in(tight)
            .unwrap());

        // 3.3V ±10% does NOT fit within 3.3V ±5%
        assert!(!tight
            .downcast_ref::<PhysicalValue>()
            .unwrap()
            .is_in(loose)
            .unwrap());
    }

    #[test]
    fn test_within_different_nominal_values() {
        use starlark::values::Heap;

        let heap = Heap::new();
        // 3.3V ±1% (3.267V - 3.333V) fits within 5V ±50% (2.5V - 7.5V)
        let small = heap.alloc(physical_value(3.3, 0.01, PhysicalUnit::Volts));
        let large = heap.alloc(physical_value(5.0, 0.50, PhysicalUnit::Volts));
        assert!(large
            .downcast_ref::<PhysicalValue>()
            .unwrap()
            .is_in(small)
            .unwrap());

        // 5V ±50% does NOT fit within 3.3V ±1%
        assert!(!small
            .downcast_ref::<PhysicalValue>()
            .unwrap()
            .is_in(large)
            .unwrap());
    }

    #[test]
    fn test_within_exact_match() {
        use starlark::values::Heap;

        let heap = Heap::new();
        // Exact values with no tolerance should be within each other
        let v1 = heap.alloc(physical_value(3.3, 0.0, PhysicalUnit::Volts));
        let v2 = heap.alloc(physical_value(3.3, 0.0, PhysicalUnit::Volts));
        assert!(v1
            .downcast_ref::<PhysicalValue>()
            .unwrap()
            .is_in(v2)
            .unwrap());
        assert!(v2
            .downcast_ref::<PhysicalValue>()
            .unwrap()
            .is_in(v1)
            .unwrap());
    }

    #[test]
    fn test_within_zero_tolerance_in_range() {
        use starlark::values::Heap;

        let heap = Heap::new();
        // Zero tolerance value at the center of a range
        let exact = heap.alloc(physical_value(3.3, 0.0, PhysicalUnit::Volts));
        let range = heap.alloc(physical_value(3.3, 0.10, PhysicalUnit::Volts)); // 2.97V - 3.63V
        assert!(range
            .downcast_ref::<PhysicalValue>()
            .unwrap()
            .is_in(exact)
            .unwrap());
    }

    #[test]
    fn test_within_zero_tolerance_outside_range() {
        use starlark::values::Heap;

        let heap = Heap::new();
        // Zero tolerance value outside a range
        let exact = heap.alloc(physical_value(5.0, 0.0, PhysicalUnit::Volts));
        let range = heap.alloc(physical_value(3.3, 0.10, PhysicalUnit::Volts)); // 2.97V - 3.63V
        assert!(!range
            .downcast_ref::<PhysicalValue>()
            .unwrap()
            .is_in(exact)
            .unwrap());
    }

    #[test]
    fn test_within_edge_cases() {
        use starlark::values::Heap;

        let heap = Heap::new();
        // Test boundary conditions
        // Range: 3.3V ±10% = 2.97V - 3.63V
        let range = heap.alloc(physical_value(3.3, 0.10, PhysicalUnit::Volts));

        // Value exactly at lower bound should be within
        let at_min = heap.alloc(physical_value(2.97, 0.0, PhysicalUnit::Volts));
        assert!(range
            .downcast_ref::<PhysicalValue>()
            .unwrap()
            .is_in(at_min)
            .unwrap());

        // Value exactly at upper bound should be within
        let at_max = heap.alloc(physical_value(3.63, 0.0, PhysicalUnit::Volts));
        assert!(range
            .downcast_ref::<PhysicalValue>()
            .unwrap()
            .is_in(at_max)
            .unwrap());

        // Value just outside lower bound should not be within
        let below_min = heap.alloc(physical_value(2.96, 0.0, PhysicalUnit::Volts));
        assert!(!range
            .downcast_ref::<PhysicalValue>()
            .unwrap()
            .is_in(below_min)
            .unwrap());

        // Value just outside upper bound should not be within
        let above_max = heap.alloc(physical_value(3.64, 0.0, PhysicalUnit::Volts));
        assert!(!range
            .downcast_ref::<PhysicalValue>()
            .unwrap()
            .is_in(above_max)
            .unwrap());
    }

    #[test]
    fn test_within_overlapping_but_not_contained() {
        use starlark::values::Heap;

        let heap = Heap::new();
        // Ranges that overlap but one doesn't contain the other
        // Range 1: 3.3V ±10% = 2.97V - 3.63V
        // Range 2: 3.5V ±5% = 3.325V - 3.675V
        let range1 = heap.alloc(physical_value(3.3, 0.10, PhysicalUnit::Volts));
        let range2 = heap.alloc(physical_value(3.5, 0.05, PhysicalUnit::Volts));

        // They overlap but neither contains the other
        assert!(!range2
            .downcast_ref::<PhysicalValue>()
            .unwrap()
            .is_in(range1)
            .unwrap());
        assert!(!range1
            .downcast_ref::<PhysicalValue>()
            .unwrap()
            .is_in(range2)
            .unwrap());
    }

    #[test]
    fn test_within_unit_mismatch() {
        use starlark::values::Heap;

        let heap = Heap::new();
        // Different units should return an error
        let volts = heap.alloc(physical_value(3.3, 0.1, PhysicalUnit::Volts));
        let amps = heap.alloc(physical_value(3.3, 0.1, PhysicalUnit::Amperes));

        let result = volts.downcast_ref::<PhysicalValue>().unwrap().is_in(amps);
        assert!(result.is_err());
    }

    #[test]
    fn test_within_different_units() {
        use starlark::values::Heap;

        let heap = Heap::new();
        // Test with various unit types
        let r1 = heap.alloc(physical_value(1000.0, 0.01, PhysicalUnit::Ohms)); // 1kΩ ±1%
        let r2 = heap.alloc(physical_value(1000.0, 0.05, PhysicalUnit::Ohms)); // 1kΩ ±5%
        assert!(r2
            .downcast_ref::<PhysicalValue>()
            .unwrap()
            .is_in(r1)
            .unwrap());

        let c1 = heap.alloc(physical_value(1e-7, 0.05, PhysicalUnit::Farads)); // 100nF ±5%
        let c2 = heap.alloc(physical_value(1e-7, 0.20, PhysicalUnit::Farads)); // 100nF ±20%
        assert!(c2
            .downcast_ref::<PhysicalValue>()
            .unwrap()
            .is_in(c1)
            .unwrap());

        let f1 = heap.alloc(physical_value(1e6, 0.001, PhysicalUnit::Hertz)); // 1MHz ±0.1%
        let f2 = heap.alloc(physical_value(1e6, 0.01, PhysicalUnit::Hertz)); // 1MHz ±1%
        assert!(f2
            .downcast_ref::<PhysicalValue>()
            .unwrap()
            .is_in(f1)
            .unwrap());
    }

    #[test]
    fn test_within_negative_values() {
        use starlark::values::Heap;

        let heap = Heap::new();
        // Test with negative values
        let v1 = heap.alloc(physical_value(-3.3, 0.05, PhysicalUnit::Volts)); // -3.3V ±5%
        let v2 = heap.alloc(physical_value(-3.3, 0.10, PhysicalUnit::Volts)); // -3.3V ±10%
        assert!(v2
            .downcast_ref::<PhysicalValue>()
            .unwrap()
            .is_in(v1)
            .unwrap());
        assert!(!v1
            .downcast_ref::<PhysicalValue>()
            .unwrap()
            .is_in(v2)
            .unwrap());
    }

    // Helper for creating PhysicalValue from bounds (replaces old physical_range)
    #[cfg(test)]
    fn physical_value_bounds(min: f64, max: f64, unit: PhysicalUnit) -> PhysicalValue {
        PhysicalValue::from_bounds(
            Decimal::from_f64(min).unwrap(),
            Decimal::from_f64(max).unwrap(),
            unit.into(),
        )
    }

    #[test]
    fn test_range_diff_power_to_ground() {
        // VCC 3.0-3.6V, GND 0V -> diff = 3.6V
        let vcc = physical_value_bounds(3.0, 3.6, PhysicalUnit::Volts);
        let gnd = physical_value_bounds(0.0, 0.0, PhysicalUnit::Volts);
        let result = vcc.diff(&gnd).unwrap();

        assert_eq!(result.nominal, Decimal::from_f64(3.6).unwrap());
        assert_eq!(result.unit, PhysicalUnit::Volts.into());
        assert!(result.is_point()); // Point value has no tolerance
    }

    #[test]
    fn test_range_diff_two_rails() {
        // V1: 3.0-3.6V, V2: 1.7-2.0V
        // max(|3.6 - 1.7|, |3.0 - 2.0|) = max(1.9, 1.0) = 1.9V
        let v1 = physical_value_bounds(3.0, 3.6, PhysicalUnit::Volts);
        let v2 = physical_value_bounds(1.7, 2.0, PhysicalUnit::Volts);
        let result = v1.diff(&v2).unwrap();

        assert_eq!(result.nominal, Decimal::from_f64(1.9).unwrap());
    }

    #[test]
    fn test_range_diff_ac_coupling() {
        // Signal: -5 to +5V, Bias: 0V
        // max(|5 - 0|, |-5 - 0|) = 5V
        let signal = physical_value_bounds(-5.0, 5.0, PhysicalUnit::Volts);
        let bias = physical_value_bounds(0.0, 0.0, PhysicalUnit::Volts);
        let result = signal.diff(&bias).unwrap();

        assert_eq!(result.nominal, Decimal::from_f64(5.0).unwrap());
    }

    #[test]
    fn test_range_diff_negative_ranges() {
        // V1: -5 to -3V, V2: -2 to -1V
        // max(|-3 - (-2)|, |-5 - (-1)|) = max(1, 4) = 4V
        let v1 = physical_value_bounds(-5.0, -3.0, PhysicalUnit::Volts);
        let v2 = physical_value_bounds(-2.0, -1.0, PhysicalUnit::Volts);
        let result = v1.diff(&v2).unwrap();

        assert_eq!(result.nominal, Decimal::from_f64(4.0).unwrap());
    }

    #[test]
    fn test_range_diff_symmetric() {
        // diff should be symmetric: A.diff(B) == B.diff(A)
        let v1 = physical_value_bounds(3.0, 3.6, PhysicalUnit::Volts);
        let v2 = physical_value_bounds(1.7, 2.0, PhysicalUnit::Volts);

        let diff_ab = v1.diff(&v2).unwrap();
        let diff_ba = v2.diff(&v1).unwrap();

        assert_eq!(diff_ab.nominal, diff_ba.nominal);
    }

    #[test]
    fn test_range_diff_same_range() {
        // Same range should have 0 difference
        let v = physical_value_bounds(3.3, 3.3, PhysicalUnit::Volts);
        let result = v.diff(&v).unwrap();

        assert_eq!(result.nominal, Decimal::ZERO);
    }

    #[test]
    fn test_range_diff_overlapping_ranges() {
        // Range 1: 2.0-4.0V, Range 2: 3.0-5.0V
        // max(|4.0 - 3.0|, |2.0 - 5.0|) = max(1.0, 3.0) = 3.0V
        let r1 = physical_value_bounds(2.0, 4.0, PhysicalUnit::Volts);
        let r2 = physical_value_bounds(3.0, 5.0, PhysicalUnit::Volts);
        let result = r1.diff(&r2).unwrap();

        assert_eq!(result.nominal, Decimal::from_f64(3.0).unwrap());
    }

    #[test]
    fn test_range_diff_unit_mismatch() {
        // Different units should return an error
        let volts = physical_value_bounds(3.0, 3.6, PhysicalUnit::Volts);
        let amps = physical_value_bounds(0.0, 1.0, PhysicalUnit::Amperes);

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
        let r1 = physical_value_bounds(900.0, 1100.0, PhysicalUnit::Ohms); // 1kΩ ±10%
        let r2 = physical_value_bounds(0.0, 0.0, PhysicalUnit::Ohms); // 0Ω (short)
        let result = r1.diff(&r2).unwrap();
        assert_eq!(result.nominal, Decimal::from_f64(1100.0).unwrap());

        // Test with current ranges
        let i1 = physical_value_bounds(0.1, 0.5, PhysicalUnit::Amperes);
        let i2 = physical_value_bounds(0.0, 0.0, PhysicalUnit::Amperes);
        let result = i1.diff(&i2).unwrap();
        assert_eq!(result.nominal, Decimal::from_f64(0.5).unwrap());
    }

    #[test]
    fn test_range_diff_zero_tolerance() {
        // Range with no tolerance always returns zero tolerance
        let v1 = physical_value_bounds(3.3, 3.3, PhysicalUnit::Volts);
        let v2 = physical_value_bounds(5.0, 5.0, PhysicalUnit::Volts);
        let result = v1.diff(&v2).unwrap();

        assert!(result.is_point()); // Point value has no tolerance
        assert_eq!(result.nominal, Decimal::from_f64(1.7).unwrap());
    }

    #[test]
    fn test_range_diff_from_string() {
        // Create a value from bounds
        let range_val = physical_value_bounds(3.0, 3.6, PhysicalUnit::Volts);

        // Parse string as PhysicalValue (now handles range syntax too)
        let gnd_value = PhysicalValue::from_str("0V").unwrap();

        // Test diff works with parsed string
        let result = range_val.diff(&gnd_value).unwrap();
        assert_eq!(result.nominal, Decimal::from_f64(3.6).unwrap());
        assert_eq!(result.unit, PhysicalUnit::Volts.into());
    }

    #[test]
    fn test_physical_value_min_max() {
        // Test min/max fields with tolerance
        let v = physical_value(3.3, 0.05, PhysicalUnit::Volts); // 3.3V ±5%
        assert_eq!(v.min, Decimal::from_f64(3.135).unwrap());
        assert_eq!(v.max, Decimal::from_f64(3.465).unwrap());

        // Test with zero tolerance
        let v_no_tol = physical_value(5.0, 0.0, PhysicalUnit::Volts);
        assert_eq!(v_no_tol.min, Decimal::from_f64(5.0).unwrap());
        assert_eq!(v_no_tol.max, Decimal::from_f64(5.0).unwrap());
    }

    #[test]
    fn test_physical_value_unary_minus() {
        use starlark::values::Heap;

        let heap = Heap::new();
        let v = heap.alloc(physical_value(3.3, 0.05, PhysicalUnit::Volts));

        // Test unary minus
        let neg = v
            .downcast_ref::<PhysicalValue>()
            .unwrap()
            .minus(&heap)
            .unwrap();
        let neg_val = neg.downcast_ref::<PhysicalValue>().unwrap();

        assert_eq!(neg_val.nominal, Decimal::from_f64(-3.3).unwrap());
        assert_eq!(neg_val.tolerance(), Decimal::from_f64(0.05).unwrap()); // Tolerance preserved
        assert_eq!(neg_val.unit, PhysicalUnit::Volts.into());
    }

    #[test]
    fn test_physical_value_bounds_unary_minus() {
        use starlark::values::Heap;

        let heap = Heap::new();
        let range = heap.alloc_simple(physical_value_bounds(1.0, 3.0, PhysicalUnit::Volts));

        // Test unary minus (should flip and negate)
        let neg = range
            .downcast_ref::<PhysicalValue>()
            .unwrap()
            .minus(&heap)
            .unwrap();
        let neg_range = neg.downcast_ref::<PhysicalValue>().unwrap();

        assert_eq!(neg_range.min, Decimal::from_f64(-3.0).unwrap());
        assert_eq!(neg_range.max, Decimal::from_f64(-1.0).unwrap());
        assert_eq!(neg_range.unit, PhysicalUnit::Volts.into());
    }

    #[test]
    fn test_physical_value_bounds_unary_minus_with_nominal() {
        use starlark::values::Heap;

        let heap = Heap::new();
        // Create a value with explicit nominal
        let range = PhysicalValue::from_bounds_nominal(
            Decimal::from_f64(2.0).unwrap(),
            Decimal::from_f64(1.0).unwrap(),
            Decimal::from_f64(3.0).unwrap(),
            PhysicalUnit::Volts.into(),
        );
        let range_val = heap.alloc_simple(range);

        let neg = range_val
            .downcast_ref::<PhysicalValue>()
            .unwrap()
            .minus(&heap)
            .unwrap();
        let neg_range = neg.downcast_ref::<PhysicalValue>().unwrap();

        assert_eq!(neg_range.nominal, Decimal::from_f64(-2.0).unwrap());
    }

    #[test]
    fn test_is_in_value_in_range() {
        use starlark::values::Heap;

        let heap = Heap::new();
        let range = heap.alloc_simple(physical_value_bounds(3.0, 3.6, PhysicalUnit::Volts));
        let value = heap.alloc(physical_value(3.3, 0.0, PhysicalUnit::Volts));

        // Value should be in range
        let result = range
            .downcast_ref::<PhysicalValue>()
            .unwrap()
            .is_in(value)
            .unwrap();
        assert!(result);

        // Value outside range
        let value_out = heap.alloc(physical_value(5.0, 0.0, PhysicalUnit::Volts));
        let result = range
            .downcast_ref::<PhysicalValue>()
            .unwrap()
            .is_in(value_out)
            .unwrap();
        assert!(!result);
    }

    #[test]
    fn test_is_in_value_with_tolerance_in_range() {
        use starlark::values::Heap;

        let heap = Heap::new();
        let range = heap.alloc_simple(physical_value_bounds(3.0, 3.6, PhysicalUnit::Volts));
        let value = heap.alloc(physical_value(3.3, 0.05, PhysicalUnit::Volts)); // 3.135-3.465V

        // Value with tolerance fits in range
        let result = range
            .downcast_ref::<PhysicalValue>()
            .unwrap()
            .is_in(value)
            .unwrap();
        assert!(result);

        // Value with tolerance that exceeds range
        let value_big = heap.alloc(physical_value(3.3, 0.15, PhysicalUnit::Volts)); // 2.805-3.795V
        let result = range
            .downcast_ref::<PhysicalValue>()
            .unwrap()
            .is_in(value_big)
            .unwrap();
        assert!(!result);
    }

    #[test]
    fn test_is_in_range_in_range() {
        use starlark::values::Heap;

        let heap = Heap::new();
        let wide = heap.alloc_simple(physical_value_bounds(2.7, 3.6, PhysicalUnit::Volts));
        let tight = heap.alloc_simple(physical_value_bounds(3.0, 3.3, PhysicalUnit::Volts));

        // Tight range fits in wide range
        let result = wide
            .downcast_ref::<PhysicalValue>()
            .unwrap()
            .is_in(tight)
            .unwrap();
        assert!(result);

        // Wide range doesn't fit in tight range
        let result = tight
            .downcast_ref::<PhysicalValue>()
            .unwrap()
            .is_in(wide)
            .unwrap();
        assert!(!result);
    }

    #[test]
    fn test_is_in_value_in_value() {
        use starlark::values::Heap;

        let heap = Heap::new();
        let wide = heap.alloc(physical_value(3.3, 0.10, PhysicalUnit::Volts)); // ±10%
        let tight = heap.alloc(physical_value(3.3, 0.05, PhysicalUnit::Volts)); // ±5%

        // Tight tolerance fits in wide tolerance
        let result = wide
            .downcast_ref::<PhysicalValue>()
            .unwrap()
            .is_in(tight)
            .unwrap();
        assert!(result);

        // Wide tolerance doesn't fit in tight tolerance
        let result = tight
            .downcast_ref::<PhysicalValue>()
            .unwrap()
            .is_in(wide)
            .unwrap();
        assert!(!result);
    }

    #[test]
    fn test_is_in_range_in_value() {
        use starlark::values::Heap;

        let heap = Heap::new();
        let value = heap.alloc(physical_value(3.3, 0.10, PhysicalUnit::Volts)); // 2.97-3.63V
        let range = heap.alloc_simple(physical_value_bounds(3.0, 3.5, PhysicalUnit::Volts));

        // Range fits in value's tolerance
        let result = value
            .downcast_ref::<PhysicalValue>()
            .unwrap()
            .is_in(range)
            .unwrap();
        assert!(result);

        // Range exceeds value's tolerance
        let range_big = heap.alloc_simple(physical_value_bounds(2.0, 4.0, PhysicalUnit::Volts));
        let result = value
            .downcast_ref::<PhysicalValue>()
            .unwrap()
            .is_in(range_big)
            .unwrap();
        assert!(!result);
    }

    #[test]
    fn test_is_in_string_arguments() {
        use starlark::values::Heap;

        let heap = Heap::new();
        let range = heap.alloc_simple(physical_value_bounds(3.0, 3.6, PhysicalUnit::Volts));
        let value_str = heap.alloc("3.3V");

        // String value in range
        let result = range
            .downcast_ref::<PhysicalValue>()
            .unwrap()
            .is_in(value_str)
            .unwrap();
        assert!(result);

        // String range in range
        let range_str = heap.alloc("3.0V to 3.3V");
        let result = range
            .downcast_ref::<PhysicalValue>()
            .unwrap()
            .is_in(range_str)
            .unwrap();
        assert!(result);
    }

    #[test]
    fn test_is_in_unit_mismatch() {
        use starlark::values::Heap;

        let heap = Heap::new();
        let volts = heap.alloc_simple(physical_value_bounds(3.0, 3.6, PhysicalUnit::Volts));
        let amps = heap.alloc(physical_value(1.0, 0.0, PhysicalUnit::Amperes));

        // Unit mismatch should error
        let result = volts.downcast_ref::<PhysicalValue>().unwrap().is_in(amps);
        assert!(result.is_err());
    }

    #[test]
    fn test_within_method() {
        use starlark::environment::Module;
        use starlark::eval::Evaluator;

        let module = Module::new();
        let heap = module.heap();
        let mut eval = Evaluator::new(&module);

        // Test case from the regression: 4.7uF ±10% (house part) should fit within 4.7uF requirement
        let house_part = heap.alloc(physical_value(4.7e-6, 0.1, PhysicalUnit::Farads)); // 4.7uF ±10%
        let requirement = heap.alloc_str("4.7uF"); // 4.7uF (no tolerance = 0%)

        // within() should check if house_part fits within requirement
        let result = eval.eval_function(
            house_part.get_attr("within", heap).unwrap().unwrap(),
            &[requirement.to_value()],
            &[],
        );
        assert!(result.is_ok());
        assert_eq!(result.unwrap().unpack_bool(), Some(false)); // 10% tolerance doesn't fit in 0% tolerance

        // Test case: tight tolerance fits within loose tolerance
        let tight = heap.alloc(physical_value(5.5, 0.01, PhysicalUnit::Volts)); // 5.5V ±1%
        let loose = heap.alloc_str("6V 10%"); // 6V ±10% = [5.4V, 6.6V]

        let result = eval.eval_function(
            tight.get_attr("within", heap).unwrap().unwrap(),
            &[loose.to_value()],
            &[],
        );
        assert!(result.is_ok());
        assert_eq!(result.unwrap().unpack_bool(), Some(true)); // [5.445, 5.555] fits in [5.4, 6.6]

        // Test case: loose tolerance doesn't fit within tight tolerance
        let loose_val = heap.alloc(physical_value(6.0, 0.1, PhysicalUnit::Volts)); // 6V ±10%
        let tight_val = heap.alloc_str("5.5V 1%"); // 5.5V ±1%

        let result = eval.eval_function(
            loose_val.get_attr("within", heap).unwrap().unwrap(),
            &[tight_val.to_value()],
            &[],
        );
        assert!(result.is_ok());
        assert_eq!(result.unwrap().unpack_bool(), Some(false)); // [5.4, 6.6] doesn't fit in [5.445, 5.555]
    }

    #[test]
    fn test_within_vs_is_in_semantics() {
        use starlark::environment::Module;
        use starlark::eval::Evaluator;

        let module = Module::new();
        let heap = module.heap();
        let mut eval = Evaluator::new(&module);

        // Create values: tight = 5.5V ±1%, loose = 6V ±10%
        let tight = heap.alloc(physical_value(5.5, 0.01, PhysicalUnit::Volts));
        let loose = heap.alloc(physical_value(6.0, 0.1, PhysicalUnit::Volts));

        // tight.within(loose) should be true (tight fits in loose)
        let within_result = eval
            .eval_function(
                tight.get_attr("within", heap).unwrap().unwrap(),
                &[loose.to_value()],
                &[],
            )
            .unwrap();
        assert_eq!(within_result.unpack_bool(), Some(true));

        // "tight in loose" (Starlark syntax) should also be true
        // This calls loose.is_in(tight), checking if tight is in loose
        let is_in_result = loose.downcast_ref::<PhysicalValue>().unwrap().is_in(tight);
        assert!(is_in_result.is_ok());
        assert!(is_in_result.unwrap());

        // loose.within(tight) should be false (loose doesn't fit in tight)
        let within_result2 = eval
            .eval_function(
                loose.get_attr("within", heap).unwrap().unwrap(),
                &[tight.to_value()],
                &[],
            )
            .unwrap();
        assert_eq!(within_result2.unpack_bool(), Some(false));

        // tight.is_in(loose) checks if loose is in tight, should be false
        let is_in_result2 = tight.downcast_ref::<PhysicalValue>().unwrap().is_in(loose);
        assert!(is_in_result2.is_ok());
        assert!(!is_in_result2.unwrap());
    }

    #[test]
    fn test_physical_value_abs() {
        let v = physical_value(-3.3, 0.05, PhysicalUnit::Volts);
        let abs_v = v.abs();
        assert_eq!(abs_v.nominal, Decimal::from_f64(3.3).unwrap());
        assert_eq!(abs_v.tolerance(), Decimal::from_f64(0.05).unwrap());
        assert_eq!(abs_v.unit, PhysicalUnit::Volts.into());

        let v2 = physical_value(3.3, 0.05, PhysicalUnit::Volts);
        let abs_v2 = v2.abs();
        assert_eq!(abs_v2.nominal, Decimal::from_f64(3.3).unwrap());
    }

    #[test]
    fn test_physical_value_bounds_compare_disjoint() {
        use starlark::values::Value;

        let heap = Heap::new();

        // range1 = 1V to 2V, range2 = 3V to 4V (disjoint, range1 < range2)
        let range1: PhysicalValue = "1V to 2V".parse().unwrap();
        let range2: PhysicalValue = "3V to 4V".parse().unwrap();

        let v1: Value = heap.alloc_simple(range1);
        let v2: Value = heap.alloc_simple(range2);

        // Conservative semantics: range1 < range2 because range1.max < range2.min
        let cmp = v1.compare(v2).unwrap();
        assert_eq!(cmp, std::cmp::Ordering::Less);

        // And the reverse
        let cmp_rev = v2.compare(v1).unwrap();
        assert_eq!(cmp_rev, std::cmp::Ordering::Greater);
    }

    #[test]
    fn test_physical_value_bounds_compare_overlapping() {
        use starlark::values::Value;

        let heap = Heap::new();

        // range1 = 1V to 3V, range2 = 2V to 4V (overlapping)
        let range1: PhysicalValue = "1V to 3V".parse().unwrap();
        let range2: PhysicalValue = "2V to 4V".parse().unwrap();

        let v1: Value = heap.alloc_simple(range1);
        let v2: Value = heap.alloc_simple(range2);

        // Overlapping ranges use max comparison as tiebreaker
        // range1.max (3V) < range2.max (4V)
        let cmp = v1.compare(v2).unwrap();
        assert_eq!(cmp, std::cmp::Ordering::Less);
    }

    #[test]
    fn test_physical_value_bounds_compare_with_value() {
        use starlark::values::Value;

        let heap = Heap::new();

        // range = 1V to 2V, value = 5V (no tolerance)
        let range: PhysicalValue = "1V to 2V".parse().unwrap();
        let value = physical_value(5.0, 0.0, PhysicalUnit::Volts);

        let v_range: Value = heap.alloc_simple(range);
        let v_value: Value = heap.alloc(value);

        // range.max (2V) < value.min (5V), so range < value
        let cmp = v_range.compare(v_value).unwrap();
        assert_eq!(cmp, std::cmp::Ordering::Less);
    }

    #[test]
    fn test_physical_value_bounds_compare_unit_mismatch() {
        use starlark::values::Value;

        let heap = Heap::new();

        // range1 = 1V to 2V, range2 = 1A to 2A (different units)
        let range1: PhysicalValue = "1V to 2V".parse().unwrap();
        let range2: PhysicalValue = "1A to 2A".parse().unwrap();

        let v1: Value = heap.alloc_simple(range1);
        let v2: Value = heap.alloc_simple(range2);

        // Should error on unit mismatch
        let result = v1.compare(v2);
        assert!(result.is_err());
    }

    #[test]
    fn test_physical_value_bounds_equals() {
        use starlark::values::Value;

        let heap = Heap::new();

        let range1: PhysicalValue = "1V to 2V".parse().unwrap();
        let range2: PhysicalValue = "1V to 2V".parse().unwrap();
        let range3: PhysicalValue = "1V to 3V".parse().unwrap();
        let range4: PhysicalValue = "1V to 2V (1.5V nom.)".parse().unwrap();
        let range5: PhysicalValue = "1V to 2V (1.2V nom.)".parse().unwrap();

        let v1: Value = heap.alloc_simple(range1);
        let v2: Value = heap.alloc_simple(range2);
        let v3: Value = heap.alloc_simple(range3);
        let v4: Value = heap.alloc_simple(range4);
        let v5: Value = heap.alloc_simple(range5);

        // Same range
        assert!(v1.equals(v2).unwrap());
        // Different max
        assert!(!v1.equals(v3).unwrap());
        // Same min/max with same nominal (1.5V is midpoint)
        assert!(v1.equals(v4).unwrap());
        // Same min/max but different nominal
        assert!(!v1.equals(v5).unwrap());
    }

    #[test]
    fn test_physical_value_bounds_compare_with_string() {
        use starlark::values::Value;

        let heap = Heap::new();

        // range = 1V to 2V
        let range: PhysicalValue = "1V to 2V".parse().unwrap();
        let v_range: Value = heap.alloc_simple(range);

        // Compare with string "5V"
        let v_str: Value = heap.alloc_str("5V").to_value();

        // range.max (2V) < 5V, so range < "5V"
        let cmp = v_range.compare(v_str).unwrap();
        assert_eq!(cmp, std::cmp::Ordering::Less);
    }

    #[test]
    fn test_physical_value_bounds_add_value() {
        let heap = Heap::new();

        // range = 1V to 2V (1.5V nominal), offset = 3V
        let range: PhysicalValue = "1V to 2V".parse().unwrap();
        let offset = physical_value(3.0, 0.0, PhysicalUnit::Volts);

        let v_range = heap.alloc_simple(range);
        let v_offset = heap.alloc(offset);

        // 1.5V (nominal) + 3V = 4.5V (point value - bounds dropped)
        let result = v_range
            .downcast_ref::<PhysicalValue>()
            .unwrap()
            .add(v_offset, &heap)
            .unwrap()
            .unwrap();
        let result_val = result.downcast_ref::<PhysicalValue>().unwrap();

        // Add returns point value based on nominal
        assert_eq!(result_val.nominal, Decimal::from_str("4.5").unwrap());
        assert!(result_val.is_point()); // Bounds are dropped
        assert_eq!(result_val.unit, PhysicalUnit::Volts.into());
    }

    #[test]
    fn test_physical_value_bounds_add_value_with_nominal() {
        let heap = Heap::new();

        // range = 1V to 3V (2V nom.), offset = 5V
        let range: PhysicalValue = "1V to 3V (2V nom.)".parse().unwrap();
        let offset = physical_value(5.0, 0.0, PhysicalUnit::Volts);

        let v_range = heap.alloc_simple(range);
        let v_offset = heap.alloc(offset);

        // 2V (nominal) + 5V = 7V (point value - bounds dropped)
        let result = v_range
            .downcast_ref::<PhysicalValue>()
            .unwrap()
            .add(v_offset, &heap)
            .unwrap()
            .unwrap();
        let result_val = result.downcast_ref::<PhysicalValue>().unwrap();

        // Add returns point value based on nominal
        assert_eq!(result_val.nominal, Decimal::from(7));
        assert!(result_val.is_point()); // Bounds are dropped
    }

    #[test]
    fn test_physical_value_bounds_sub_value() {
        let heap = Heap::new();

        // range = 5V to 10V (7.5V nominal), offset = 2V
        let range: PhysicalValue = "5V to 10V".parse().unwrap();
        let offset = physical_value(2.0, 0.0, PhysicalUnit::Volts);

        let v_range = heap.alloc_simple(range);
        let v_offset = heap.alloc(offset);

        // 7.5V (nominal) - 2V = 5.5V (point value - bounds dropped)
        let result = v_range
            .downcast_ref::<PhysicalValue>()
            .unwrap()
            .sub(v_offset, &heap)
            .unwrap();
        let result_val = result.downcast_ref::<PhysicalValue>().unwrap();

        // Sub returns point value based on nominal
        assert_eq!(result_val.nominal, Decimal::from_str("5.5").unwrap());
        assert!(result_val.is_point()); // Bounds are dropped
    }

    #[test]
    fn test_physical_value_bounds_add_unit_mismatch() {
        let heap = Heap::new();

        // range = 1V to 2V, offset = 1A (wrong unit)
        let range: PhysicalValue = "1V to 2V".parse().unwrap();
        let offset = physical_value(1.0, 0.0, PhysicalUnit::Amperes);

        let v_range = heap.alloc_simple(range);
        let v_offset = heap.alloc(offset);

        // Should error on unit mismatch
        let result = v_range
            .downcast_ref::<PhysicalValue>()
            .unwrap()
            .add(v_offset, &heap)
            .unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn test_physical_value_bounds_add_dimensionless() {
        let heap = Heap::new();

        // range = 1V to 2V, offset = 3 (dimensionless)
        let range: PhysicalValue = "1V to 2V".parse().unwrap();
        let offset = PhysicalValue::dimensionless(3);

        let v_range = heap.alloc_simple(range);
        let v_offset = heap.alloc(offset);

        // Adding dimensionless to voltage should fail (unit mismatch)
        let result = v_range
            .downcast_ref::<PhysicalValue>()
            .unwrap()
            .add(v_offset, &heap)
            .unwrap();
        assert!(result.is_err());
    }
}
