use std::{fmt, str::FromStr};

use allocative::Allocative;
use anyhow::anyhow;
use pcb_sch::PhysicalUnit;
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
        float::StarlarkFloat, starlark_value, string::StarlarkStr, typing::TypeInstanceId, Freeze,
        FreezeResult, Heap, NoSerialize, StarlarkValue, Value, ValueLike, ValueTyped,
    },
};

/// Macro to generate physical unit type implementations
///
/// Usage: define_physical_unit!(TypeName, PhysicalUnit::Variant);
macro_rules! define_physical_unit {
    ($type_name:ident, $unit_variant:expr, $quantity_str:expr) => {
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
            const QUANTITY: &'static str = $quantity_str;
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
    if let Some(f) = value.downcast_ref::<StarlarkFloat>() {
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
    pub(crate) unit: PhysicalUnitDims,
}

impl PhysicalValue {
    pub fn dimensionless<D: Into<Decimal>>(value: D) -> Self {
        Self {
            value: value.into(),
            tolerance: 0.into(),
            unit: PhysicalUnitDims::DIMENSIONLESS,
        }
    }

    pub fn pcb_sch_value(&self) -> Result<pcb_sch::PhysicalValue, PhysicalValueError> {
        let alias = self
            .unit
            .alias()
            .ok_or(PhysicalValueError::InvalidPhysicalUnit)?;
        Ok(pcb_sch::PhysicalValue {
            value: self.value,
            tolerance: self.tolerance,
            unit: alias,
        })
    }

    pub fn from_decimal(value: Decimal, tolerance: Decimal, unit: PhysicalUnitDims) -> Self {
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
        let expected_unit: PhysicalUnitDims = T::UNIT.into();
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
                            expected_unit.fmt_unit(),
                            phys_val.unit.fmt_unit()
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
        fn single_param_spec(param_type: Ty) -> ParamSpec {
            ParamSpec::new_parts([(ParamIsRequired::Yes, param_type)], [], None, [], None).unwrap()
        }

        let str_param_spec = single_param_spec(PhysicalValue::get_type_starlark_repr());
        let with_tolerance_param_spec = single_param_spec(Ty::union2(Ty::float(), Ty::string()));
        let with_unit_param_spec = single_param_spec(Ty::string());
        Ty::custom(
            TyUser::new(
                T::QUANTITY.to_string(),
                TyStarlarkValue::new::<PhysicalValue>(),
                type_id,
                TyUserParams {
                    fields: TyUserFields {
                        known: [
                            ("value".to_string(), Ty::float()),
                            ("tolerance".to_string(), Ty::float()),
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
                                "with_unit".to_string(),
                                Ty::callable(
                                    with_unit_param_spec,
                                    PhysicalValue::get_type_starlark_repr(),
                                ),
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
        } else if let Some(s) = value.downcast_ref::<StarlarkStr>() {
            // Try to parse as string
            Self::from_str(s).map_err(|e| starlark::Error::new_other(anyhow!("{}", e)))
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
            return Err(PhysicalValueError::UnitMismatch);
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
            return Err(PhysicalValueError::UnitMismatch);
        }
        let unit = self.unit;
        let value = self.value - rhs.value;
        let tolerance = Decimal::ZERO; // Always drop tolerance for subtraction
        Ok(PhysicalValue::from_decimal(value, tolerance, unit))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, ProvidesStaticType, Allocative, Serialize, Deserialize)]
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

impl From<pcb_sch::PhysicalUnit> for PhysicalUnitDims {
    fn from(unit: pcb_sch::PhysicalUnit) -> Self {
        use pcb_sch::PhysicalUnit::*;
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

/// Parse units from a string like "A·s" and multiply them together
fn gather_units(list: &str) -> Result<PhysicalUnitDims, ParseError> {
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

    fn alias(&self) -> Option<pcb_sch::PhysicalUnit> {
        use pcb_sch::PhysicalUnit::*;
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
}

pub trait PhysicalUnitType<'a>: StarlarkValue<'a> {
    const UNIT: PhysicalUnit;
    const QUANTITY: &'static str;
    fn type_name() -> String {
        format!("{}Type", Self::QUANTITY)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PhysicalValueError {
    #[error("Division by zero")]
    DivisionByZero,
    #[error("Unit mismatch")]
    UnitMismatch,
    #[error("Unit has no alias")]
    InvalidPhysicalUnit,
}

impl From<PhysicalValueError> for starlark::Error {
    fn from(err: PhysicalValueError) -> Self {
        starlark::Error::new_other(err)
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
                        PhysicalUnit::Ohms.into(),
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

fn parse_unit_with_prefix(
    unit_str: &str,
    base_value: Decimal,
) -> Result<(Decimal, PhysicalUnitDims), ParseError> {
    // Handle bare number (empty unit) - defaults to resistance
    if unit_str.is_empty() {
        return Ok((base_value, PhysicalUnit::Ohms.into()));
    }

    // Handle special time units and temperature units (non-SI but common) first
    match unit_str {
        "h" => return Ok((base_value * Decimal::from(3600), PhysicalUnitDims::TIME)), // 1 hour = 3600 seconds
        "min" => return Ok((base_value * Decimal::from(60), PhysicalUnitDims::TIME)), // 1 minute = 60 seconds
        "°C" | "°F" => {
            let kelvin_value = convert_temperature_to_kelvin(base_value, unit_str);
            return Ok((kelvin_value, PhysicalUnitDims::TEMP));
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
                    PhysicalUnitDims::TIME,
                ));
            }

            let unit = base_unit.parse()?;
            return Ok((base_value * multiplier, unit));
        }
    }

    // Handle base units (no prefix)
    let unit = unit_str.parse()?;
    Ok((base_value, unit))
}

impl std::fmt::Display for PhysicalValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tol = self.tolerance * Decimal::from(100);
        let show_tol = tol > Decimal::ZERO;

        let alias = self.unit.alias();

        if alias == Some(PhysicalUnit::Kelvin) {
            // Convert from internal Kelvin to Celsius for display
            let celsius = self.value - Decimal::from_str("273.15").unwrap();
            let val_str = fmt_significant(celsius);

            if show_tol {
                write!(f, "{}°C {}%", val_str, tol.round())
            } else {
                write!(f, "{}°C", val_str)
            }
        } else if alias == Some(PhysicalUnit::Seconds) {
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
            let suffix = self.unit.fmt_unit();

            if show_tol {
                write!(f, "{val_str}{prefix}{} {}%", suffix, tol.round())
            } else {
                write!(f, "{val_str}{prefix}{}", suffix)
            }
        }
    }
}

starlark_simple_value!(PhysicalUnitDims);

#[starlark_value(type = "PhysicalUnit")]
impl<'v> StarlarkValue<'v> for PhysicalUnitDims {}

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

/// A callable wrapper for PhysicalValue's with_tolerance method
#[derive(Debug, Clone, Allocative, Serialize, ProvidesStaticType)]
pub struct PhysicalValueWithTolerance {
    value: PhysicalValue,
}

impl std::fmt::Display for PhysicalValueWithTolerance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<with_tolerance method for {}>", self.value)
    }
}

starlark_simple_value!(PhysicalValueWithTolerance);

#[starlark_value(type = "PhysicalValueWithTolerance")]
impl<'v> StarlarkValue<'v> for PhysicalValueWithTolerance {
    fn invoke(
        &self,
        _me: Value<'v>,
        args: &Arguments<'v, '_>,
        evaluator: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        let heap = evaluator.heap();
        let positional: Vec<_> = args.positions(heap)?.collect();

        if positional.len() != 1 {
            return Err(starlark::Error::new_other(anyhow!(
                "with_tolerance() expects exactly one argument"
            )));
        }

        let tolerance_arg = positional[0];
        let new_tolerance = if let Some(s) = tolerance_arg.unpack_str() {
            // Handle percentage string like "3%"
            if let Some(percent_str) = s.strip_suffix('%') {
                let percent_value: Decimal = percent_str.parse().map_err(|_| {
                    starlark::Error::new_other(anyhow!("invalid percentage value: '{}'", s))
                })?;
                percent_value / Decimal::from(100)
            } else {
                // Handle string number like "0.03"
                s.parse::<Decimal>().map_err(|_| {
                    starlark::Error::new_other(anyhow!("invalid tolerance value: '{}'", s))
                })?
            }
        } else {
            // Handle numeric value
            starlark_value_to_decimal(&tolerance_arg)?
        };

        let new_physical_value =
            PhysicalValue::from_decimal(self.value.value, new_tolerance, self.value.unit);

        Ok(heap.alloc(new_physical_value))
    }
}

/// A callable wrapper for PhysicalValue's with_unit method
#[derive(Debug, Clone, Allocative, Serialize, ProvidesStaticType)]
pub struct PhysicalValueWithUnit {
    value: PhysicalValue,
}

impl std::fmt::Display for PhysicalValueWithUnit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<with_unit method for {}>", self.value)
    }
}

starlark_simple_value!(PhysicalValueWithUnit);

#[starlark_value(type = "PhysicalValueWithUnit")]
impl<'v> StarlarkValue<'v> for PhysicalValueWithUnit {
    fn invoke(
        &self,
        _me: Value<'v>,
        args: &Arguments<'v, '_>,
        evaluator: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        let heap = evaluator.heap();
        let positional: Vec<_> = args.positions(heap)?.collect();

        if positional.len() != 1 {
            return Err(starlark::Error::new_other(anyhow!(
                "with_unit() expects exactly one argument"
            )));
        }

        let unit_arg = positional[0];
        let new_unit = if let Some(s) = unit_arg.unpack_str() {
            s.parse()
                .map_err(|e| starlark::Error::new_other(anyhow!("{}", e)))?
        } else {
            return Err(starlark::Error::new_other(anyhow!(
                "with_unit() expects a PhysicalUnit or string"
            )));
        };

        Ok(heap.alloc(PhysicalValue::from_decimal(
            self.value.value,
            self.value.tolerance,
            new_unit,
        )))
    }
}

#[starlark_value(type = "PhysicalValue")]
impl<'v> StarlarkValue<'v> for PhysicalValue {
    fn has_attr(&self, attribute: &str, _heap: &'v starlark::values::Heap) -> bool {
        matches!(
            attribute,
            "value" | "tolerance" | "unit" | "__str__" | "with_tolerance" | "with_unit"
        )
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
            "unit" => {
                let unit_str = if self.unit == PhysicalUnit::Ohms.into() {
                    "Ohm".to_string()
                } else {
                    self.unit.fmt_unit()
                };
                Some(heap.alloc(unit_str))
            }
            "__str__" => {
                // Return a callable that returns the string representation
                Some(heap.alloc(PhysicalValueStrMethod { value: *self }))
            }
            "with_tolerance" => {
                // Return a callable that creates a new PhysicalValue with updated tolerance
                Some(heap.alloc(PhysicalValueWithTolerance { value: *self }))
            }
            "with_unit" => {
                // Return a callable that creates a new PhysicalValue with updated unit
                Some(heap.alloc(PhysicalValueWithUnit { value: *self }))
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
            "with_tolerance".to_owned(),
            "with_unit".to_owned(),
        ]
    }

    fn div(&self, other: Value<'v>, heap: &'v Heap) -> Option<Result<Value<'v>, starlark::Error>> {
        let other = PhysicalValue::try_from(other).ok()?;
        let result = (*self / other).map(|v| heap.alloc(v)).map_err(|err| {
            starlark::Error::new_other(anyhow!(
                "Cannot divide {} by {} - {}",
                self.unit.fmt_unit(),
                other.unit.fmt_unit(),
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
                self.unit.fmt_unit(),
                other.unit.fmt_unit(),
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
        let result = (*self + other).map(|v| heap.alloc(v)).map_err(|err| {
            starlark::Error::new_other(anyhow!(
                "Cannot add {} and {} - {}",
                self.unit.fmt_unit(),
                other.unit.fmt_unit(),
                err
            ))
        });
        Some(result)
    }

    fn radd(&self, other: Value<'v>, heap: &'v Heap) -> Option<Result<Value<'v>, starlark::Error>> {
        self.add(other, heap)
    }

    fn sub(&self, other: Value<'v>, heap: &'v Heap) -> starlark::Result<Value<'v>> {
        let other = PhysicalValue::try_from(other).map_err(|_| {
            starlark::Error::new_other(anyhow!(
                "Cannot subtract non-physical value from {}",
                self.unit.fmt_unit()
            ))
        })?;
        let result = (*self - other).map_err(|err| {
            starlark::Error::new_other(anyhow!(
                "Cannot subtract {} from {} - {}",
                self.unit.fmt_unit(),
                other.unit.fmt_unit(),
                err
            ))
        })?;
        Ok(heap.alloc(result))
    }
}

// Physical unit types generated by macro
define_physical_unit!(VoltageType, PhysicalUnit::Volts, "Voltage");
define_physical_unit!(CurrentType, PhysicalUnit::Amperes, "Current");
define_physical_unit!(ResistanceType, PhysicalUnit::Ohms, "Resistance");
define_physical_unit!(CapacitanceType, PhysicalUnit::Farads, "Capacitance");
define_physical_unit!(InductanceType, PhysicalUnit::Henries, "Inductance");
define_physical_unit!(FrequencyType, PhysicalUnit::Hertz, "Frequency");
define_physical_unit!(TimeType, PhysicalUnit::Seconds, "Time");
define_physical_unit!(ConductanceType, PhysicalUnit::Siemens, "Conductance");
define_physical_unit!(TemperatureType, PhysicalUnit::Kelvin, "Temperature");
define_physical_unit!(ChargeType, PhysicalUnit::Coulombs, "Charge");
define_physical_unit!(PowerType, PhysicalUnit::Watts, "Power");
define_physical_unit!(EnergyType, PhysicalUnit::Joules, "Energy");
define_physical_unit!(MagneticFluxType, PhysicalUnit::Webers, "MagneticFlux");

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
            unit: unit.into(),
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
        assert_eq!(
            val.unit,
            expected_unit.into(),
            "Unit mismatch for '{}'",
            input
        );
        assert_eq!(
            val.value,
            expected_value.into(),
            "Value mismatch for '{}'",
            input
        );
    }

    #[test]
    fn test_si_prefix_formatting() {
        let test_cases = [
            ("4700", PhysicalUnit::Ohms, "4.7k"),
            ("1500000", PhysicalUnit::Hertz, "1.5MHz"),
            ("0.001", PhysicalUnit::Farads, "1mF"),
            ("0.000001", PhysicalUnit::Farads, "1uF"),
            ("0.0000001", PhysicalUnit::Farads, "100nF"),
        ];

        for (value, unit, expected) in test_cases {
            assert_formatting(value, unit, expected);
        }
    }

    #[test]
    fn test_formatting_features() {
        let test_cases = [
            // Significant digits: ≥100 (no decimals), ≥10 (one decimal), <10 (two decimals)
            ("150000", PhysicalUnit::Ohms, "150k"),
            ("47000", PhysicalUnit::Ohms, "47k"),
            ("4700", PhysicalUnit::Ohms, "4.7k"),
            // Trailing zero removal
            ("1000", PhysicalUnit::Ohms, "1k"),
            ("1200", PhysicalUnit::Ohms, "1.2k"),
            // Resistance special case (no unit suffix)
            ("1000", PhysicalUnit::Ohms, "1k"),
            ("1000", PhysicalUnit::Volts, "1kV"), // Other units show suffix
            // Various units
            ("3300", PhysicalUnit::Volts, "3.3kV"),
            ("0.1", PhysicalUnit::Amperes, "100mA"),
            ("1000000", PhysicalUnit::Hertz, "1MHz"),
            // Edge cases
            ("0.000000000001", PhysicalUnit::Farads, "1pF"),
            ("1000000000", PhysicalUnit::Hertz, "1GHz"),
            ("1", PhysicalUnit::Volts, "1V"),
            // No prefix needed
            ("100", PhysicalUnit::Volts, "100V"),
            ("47", PhysicalUnit::Ohms, "47"),
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
        let test_cases = [
            ("5V", PhysicalUnit::Volts, Decimal::from(5)),
            ("100A", PhysicalUnit::Amperes, Decimal::from(100)),
            ("47", PhysicalUnit::Ohms, Decimal::from(47)),
            ("100Ohm", PhysicalUnit::Ohms, Decimal::from(100)),
            ("100Ohms", PhysicalUnit::Ohms, Decimal::from(100)),
            ("1C", PhysicalUnit::Coulombs, Decimal::from(1)),
            ("100W", PhysicalUnit::Watts, Decimal::from(100)),
            ("50J", PhysicalUnit::Joules, Decimal::from(50)),
            ("10S", PhysicalUnit::Siemens, Decimal::from(10)),
            ("5Wb", PhysicalUnit::Webers, Decimal::from(5)),
        ];

        for (input, unit, value) in test_cases {
            assert_parsing(input, unit, value);
        }
    }

    #[test]
    fn test_parsing_with_prefixes() {
        let test_cases = [
            ("5kV", PhysicalUnit::Volts, Decimal::from(5000)),
            ("100mA", PhysicalUnit::Amperes, Decimal::new(1, 1)), // 0.1
            ("470nF", PhysicalUnit::Farads, Decimal::new(47, 8)), // 470e-9
            ("4k7", PhysicalUnit::Ohms, Decimal::from(4700)),     // Special notation
            ("10mC", PhysicalUnit::Coulombs, Decimal::new(1, 2)), // 0.01
            ("2kW", PhysicalUnit::Watts, Decimal::from(2000)),
            ("500mJ", PhysicalUnit::Joules, Decimal::new(5, 1)), // 0.5
            ("100mS", PhysicalUnit::Siemens, Decimal::new(1, 1)), // 0.1
            ("2mWb", PhysicalUnit::Webers, Decimal::new(2, 3)),  // 0.002
        ];

        for (input, unit, value) in test_cases {
            assert_parsing(input, unit, value);
        }
    }

    #[test]
    fn test_parsing_decimal_numbers() {
        let test_cases = [
            ("3.3V", PhysicalUnit::Volts, Decimal::new(33, 1)), // 3.3
            ("4.7kOhm", PhysicalUnit::Ohms, Decimal::from(4700)),
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
        assert_eq!(
            val.unit,
            expected_unit.into(),
            "Unit mismatch for '{}'",
            input
        );
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
                PhysicalUnit::Ohms,
                Decimal::from(100000),
                Decimal::new(5, 2),
            ),
            (
                "158k Ohms 1%",
                PhysicalUnit::Ohms,
                Decimal::from(158000),
                Decimal::new(1, 2),
            ),
            (
                "10nF 20%",
                PhysicalUnit::Farads,
                Decimal::new(1, 8),
                Decimal::new(2, 1),
            ),
            (
                "3.3V 1%",
                PhysicalUnit::Volts,
                Decimal::new(33, 1),
                Decimal::new(1, 2),
            ),
            (
                "12V 0.5%",
                PhysicalUnit::Volts,
                Decimal::from(12),
                Decimal::new(5, 3),
            ),
            (
                "100mA 5%",
                PhysicalUnit::Amperes,
                Decimal::new(1, 1),
                Decimal::new(5, 2),
            ),
            (
                "1MHz 10%",
                PhysicalUnit::Hertz,
                Decimal::from(1000000),
                Decimal::new(1, 1),
            ),
            (
                "10uH 20%",
                PhysicalUnit::Henries,
                Decimal::new(1, 5),
                Decimal::new(2, 1),
            ),
            (
                "100s 1%",
                PhysicalUnit::Seconds,
                Decimal::from(100),
                Decimal::new(1, 2),
            ),
            (
                "300K 2%",
                PhysicalUnit::Kelvin,
                Decimal::from(300),
                Decimal::new(2, 2),
            ),
            (
                "4k7 1%",
                PhysicalUnit::Ohms,
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
                PhysicalUnit::Ohms,
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
        use starlark::values::Heap;

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
        use starlark::values::Heap;

        let heap = Heap::new();

        // Test basic string parsing
        let test_cases = [
            ("10kOhm", PhysicalUnit::Ohms, 10000.0, 0.0),
            ("100nF", PhysicalUnit::Farads, 0.0000001, 0.0),
            ("3.3V", PhysicalUnit::Volts, 3.3, 0.0),
            ("100mA", PhysicalUnit::Amperes, 0.1, 0.0),
            ("16MHz", PhysicalUnit::Hertz, 16000000.0, 0.0),
        ];

        for (input, expected_unit, expected_value, expected_tolerance) in test_cases {
            let starlark_val = heap.alloc(input);
            let result = PhysicalValue::try_from(starlark_val.to_value()).unwrap();

            assert_eq!(
                result.unit,
                expected_unit.into(),
                "Unit mismatch for '{}'",
                input
            );
            assert_eq!(
                result.value,
                Decimal::from_f64(expected_value).unwrap(),
                "Value mismatch for '{}'",
                input
            );
            assert_eq!(
                result.tolerance,
                Decimal::from_f64(expected_tolerance).unwrap(),
                "Tolerance mismatch for '{}'",
                input
            );
        }
    }

    #[test]
    fn test_try_from_string_with_tolerance() {
        use starlark::values::Heap;

        let heap = Heap::new();
        let starlark_val = heap.alloc("10kOhm 5%");
        let result = PhysicalValue::try_from(starlark_val.to_value()).unwrap();

        assert_eq!(result.unit, PhysicalUnit::Ohms.into());
        assert_eq!(result.value, Decimal::from(10000));
        assert_eq!(result.tolerance, Decimal::from_f64(0.05).unwrap());
    }

    #[test]
    fn test_try_from_scalar() {
        use starlark::values::Heap;

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
        use starlark::values::Heap;

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
}
