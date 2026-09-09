use std::{
    cmp::Ordering,
    collections::HashMap,
    fmt,
    hash::Hash,
    str::FromStr,
    sync::{Arc, Mutex, OnceLock},
};

use allocative::Allocative;
use rust_decimal::{Decimal, prelude::ToPrimitive};
use serde::{Deserialize, Serialize};
use starlark::{
    any::ProvidesStaticType,
    environment::{Methods, MethodsBuilder},
    eval::{Arguments, Evaluator, ParametersSpec, ParametersSpecParam},
    starlark_simple_value,
    typing::{
        ParamIsRequired, ParamSpec, Ty, TyCallable, TyStarlarkValue, TyUser, TyUserFields,
        TyUserParams,
    },
    util::ArcStr,
    values::{
        Freeze, FreezeResult, FrozenValue, Heap, StarlarkValue, Value, ValueLike,
        float::StarlarkFloat,
        function::FUNCTION_TYPE,
        starlark_value,
        string::StarlarkStr,
        typing::{TypeInstanceId, TypeMatcher, TypeMatcherDyn, TypeMatcherFactory},
    },
};
use starlark_map::{StarlarkHasher, sorted_map::SortedMap};

use super::{
    ParseError, PhysicalUnit, PhysicalUnitDims, PhysicalValue, PhysicalValueError,
    parse_percentish_decimal, parse_physical_value, split_number_and_unit,
};

// Shared type instance ID cache for unit-based types
fn get_type_instance_id(
    unit: PhysicalUnitDims,
    cache: &OnceLock<Mutex<HashMap<PhysicalUnitDims, TypeInstanceId>>>,
) -> TypeInstanceId {
    let map = cache.get_or_init(|| Mutex::new(HashMap::new()));
    *map.lock()
        .unwrap()
        .entry(unit)
        .or_insert_with(TypeInstanceId::r#gen)
}

/// Helper to convert Decimal to f64 for Starlark
fn to_f64(d: Decimal, label: &'static str) -> starlark::Result<f64> {
    d.to_f64().ok_or_else(|| {
        starlark::Error::new_other(anyhow::anyhow!("Failed to convert {} to f64", label))
    })
}

/// Convert Starlark value to Decimal for math operations
fn starlark_value_to_decimal(value: &Value) -> Result<Decimal, PhysicalValueError> {
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
        if let Ok(pv) = parse_physical_value(s, Some(expected_unit)) {
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
        let matches_param_spec = single_param_spec(Ty::any());
        let abs_param_spec = no_param_spec();
        let spice_param_spec = no_param_spec();
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
                "spice".to_string(),
                Ty::callable(spice_param_spec, Ty::string()),
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
                "matches".to_string(),
                Ty::callable(matches_param_spec, Ty::bool()),
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

impl TryFrom<Value<'_>> for PhysicalValue {
    type Error = starlark::Error;

    fn try_from(value: Value<'_>) -> Result<Self, Self::Error> {
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

impl Freeze for PhysicalUnitDims {
    type Frozen = Self;
    fn freeze(self, _freezer: &starlark::values::Freezer) -> FreezeResult<Self::Frozen> {
        Ok(self)
    }
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

impl From<ParseError> for starlark::Error {
    fn from(err: ParseError) -> Self {
        starlark::Error::new_other(err)
    }
}

impl PhysicalValue {
    fn same_value(&self, other: &PhysicalValue) -> bool {
        self.unit == other.unit
            && self.nominal == other.nominal
            && self.min == other.min
            && self.max == other.max
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
starlark::methods_static!(PHYSICAL_VALUE_METHODS = physical_value_methods);

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

    /// Format the nominal value for a SPICE netlist (ngspice scale factors)
    fn spice<'v>(this: &PhysicalValue) -> starlark::Result<String> {
        Ok(this.to_spice_string())
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

    fn matches<'v>(
        this: &PhysicalValue,
        #[starlark(require = pos)] other: Value<'v>,
    ) -> starlark::Result<bool> {
        let Ok(other) = PhysicalValue::try_from(other) else {
            return Ok(false);
        };
        Ok(this.same_value(&other))
    }
}

#[starlark_value(type = "PhysicalValue")]
impl<'v> StarlarkValue<'v> for PhysicalValue {
    fn get_methods() -> Option<&'static Methods> {
        Some(PHYSICAL_VALUE_METHODS.methods())
    }

    fn write_hash(&self, hasher: &mut StarlarkHasher) -> starlark::Result<()> {
        self.hash(hasher);
        Ok(())
    }

    fn div(&self, other: Value<'v>, heap: Heap<'v>) -> Option<Result<Value<'v>, starlark::Error>> {
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

    fn rdiv(&self, other: Value<'v>, heap: Heap<'v>) -> Option<Result<Value<'v>, starlark::Error>> {
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

    fn mul(&self, other: Value<'v>, heap: Heap<'v>) -> Option<Result<Value<'v>, starlark::Error>> {
        let other = PhysicalValue::try_from(other).ok()?;
        let result = heap.alloc(*self * other);
        Some(Ok(result))
    }

    fn rmul(&self, other: Value<'v>, heap: Heap<'v>) -> Option<Result<Value<'v>, starlark::Error>> {
        let other = PhysicalValue::try_from(other).ok()?;
        let result = heap.alloc(other * *self);
        Some(Ok(result))
    }

    fn add(&self, other: Value<'v>, heap: Heap<'v>) -> Option<Result<Value<'v>, starlark::Error>> {
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

    fn radd(&self, other: Value<'v>, heap: Heap<'v>) -> Option<Result<Value<'v>, starlark::Error>> {
        self.add(other, heap)
    }

    fn sub(&self, other: Value<'v>, heap: Heap<'v>) -> starlark::Result<Value<'v>> {
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
        // Equality must stay symmetric and consistent with hashing, so
        // hashable PhysicalValue instances only compare equal to PhysicalValue.
        let Some(other) = other.downcast_ref::<PhysicalValue>() else {
            return Ok(false);
        };
        Ok(self.same_value(other))
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

    fn minus(&self, heap: Heap<'v>) -> starlark::Result<Value<'v>> {
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
#[derive(Clone, Debug, ProvidesStaticType, Allocative, Serialize, Deserialize)]
pub struct PhysicalValueType {
    unit: PhysicalUnitDims,
    #[allocative(skip)]
    #[serde(skip, default)]
    exported_name: Arc<OnceLock<String>>,
}

impl Freeze for PhysicalValueType {
    type Frozen = Self;
    fn freeze(self, _freezer: &starlark::values::Freezer) -> FreezeResult<Self::Frozen> {
        Ok(self)
    }
}

starlark_simple_value!(PhysicalValueType);
starlark::methods_static!(PHYSICAL_VALUE_TYPE_METHODS = value_type_methods);

impl fmt::Display for PhysicalValueType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.ty_name())
    }
}

impl PhysicalValueType {
    pub fn new(unit: PhysicalUnitDims) -> Self {
        PhysicalValueType {
            unit,
            exported_name: Default::default(),
        }
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

impl PartialEq for PhysicalValueType {
    fn eq(&self, other: &Self) -> bool {
        self.unit == other.unit
    }
}

impl Eq for PhysicalValueType {}

impl Hash for PhysicalValueType {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.unit.hash(state);
    }
}

#[starlark_value(type = FUNCTION_TYPE)]
impl<'v> StarlarkValue<'v> for PhysicalValueType {
    fn write_hash(&self, hasher: &mut StarlarkHasher) -> starlark::Result<()> {
        self.hash(hasher);
        Ok(())
    }

    fn mul(&self, other: Value<'v>, heap: Heap<'v>) -> Option<starlark::Result<Value<'v>>> {
        let other = other.downcast_ref::<PhysicalValueType>()?;
        let result = PhysicalValueType::new(self.unit * other.unit);
        Some(Ok(heap.alloc(result)))
    }

    fn div(&self, other: Value<'v>, heap: Heap<'v>) -> Option<starlark::Result<Value<'v>>> {
        let other = other.downcast_ref::<PhysicalValueType>()?;
        let result = PhysicalValueType::new(self.unit / other.unit);
        Some(Ok(heap.alloc(result)))
    }

    fn rdiv(&self, other: Value<'v>, heap: Heap<'v>) -> Option<starlark::Result<Value<'v>>> {
        if other.unpack_i32() != Some(1) {
            return None;
        }
        Some(Ok(heap.alloc(PhysicalValueType::new(
            PhysicalUnitDims::DIMENSIONLESS / self.unit,
        ))))
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

    fn export_as(
        &self,
        variable_name: &str,
        _eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<()> {
        let _ignore = self.exported_name.get_or_init(|| variable_name.to_owned());
        Ok(())
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
                        if let Ok((number, unit_str)) = split_number_and_unit(s)
                            && unit_str.is_empty()
                        {
                            return Ok(PhysicalValue::point(number, self.unit));
                        }
                        // Unit-suffixed strings must match the constructor's unit.
                        let pv = parse_physical_value(s, Some(self.unit)).map_err(|err| {
                            PhysicalValueError::ParseError {
                                unit: self.unit.quantity(),
                                input: s.to_string(),
                                source: err,
                            }
                        })?;
                        return Ok(pv.check_unit(self.unit)?);
                    }

                    let v = starlark_value_to_decimal(&value)?;
                    Ok(PhysicalValue::point(v, self.unit))
                };

                let parse_bound = |value: Value, label: &str| -> starlark::Result<Decimal> {
                    let pv = parse_value(value)?;
                    if !pv.is_point() {
                        return Err(PhysicalValueError::InvalidArguments {
                            args: vec![label.to_string()],
                        }
                        .into());
                    }
                    Ok(pv.nominal)
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
        Some(PHYSICAL_VALUE_TYPE_METHODS.methods())
    }
}

#[derive(Hash, Debug, PartialEq, Clone, Allocative, pagable::Pagable)]
#[pagable::pagable_typetag(TypeMatcherDyn)]
struct ValueTypeMatcher {
    unit: PhysicalUnitDims,
}

#[starlark::type_matcher]
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
    use rust_decimal::prelude::FromPrimitive;
    use starlark::values::{FrozenHeap, Heap};

    fn physical_value(value: f64, tolerance: f64, unit: PhysicalUnit) -> PhysicalValue {
        PhysicalValue::new(value, tolerance, unit)
    }

    fn physical_value_bounds(min: f64, max: f64, unit: PhysicalUnit) -> PhysicalValue {
        PhysicalValue::from_bounds(
            Decimal::from_f64(min).unwrap(),
            Decimal::from_f64(max).unwrap(),
            unit.into(),
        )
    }

    #[test]
    fn test_try_from_physical_value() {
        Heap::temp(|heap| {
            let original = physical_value(10.0, 0.05, PhysicalUnit::Ohms);
            let starlark_val = heap.alloc(original);

            let result = PhysicalValue::try_from(starlark_val.to_value()).unwrap();
            assert_eq!(result.nominal, original.nominal);
            assert_eq!(result.tolerance(), original.tolerance());
            assert_eq!(result.unit, original.unit);
        });
    }

    #[test]
    fn test_physical_value_is_hashable_in_starlark() {
        Heap::temp(|heap| {
            let v1 = heap.alloc(physical_value(10.0, 0.05, PhysicalUnit::Ohms));
            let v2 = heap.alloc(physical_value(10.0, 0.05, PhysicalUnit::Ohms));
            let v3 = heap.alloc(physical_value(11.0, 0.05, PhysicalUnit::Ohms));

            let v1_hashed = v1.to_value().get_hashed().unwrap();
            let v2_hashed = v2.to_value().get_hashed().unwrap();
            let v3_hashed = v3.to_value().get_hashed().unwrap();

            assert_eq!(v1_hashed.hash(), v2_hashed.hash());
            assert_ne!(v1_hashed.hash(), v3_hashed.hash());
            assert!(v1.to_value().equals(v2.to_value()).unwrap());
            assert!(!v1.to_value().equals(v3.to_value()).unwrap());
        });
    }

    #[test]
    fn test_physical_value_hash_is_stable_across_freeze() {
        Heap::temp(|heap| {
            let physical = physical_value(10.0, 0.05, PhysicalUnit::Ohms);
            let value = heap.alloc(physical);
            let unfrozen_hash = value.to_value().get_hashed().unwrap().hash();

            let frozen_heap = FrozenHeap::new();
            let frozen = frozen_heap.alloc(physical);
            let frozen_hash = frozen.get_hashed().unwrap().hash();

            assert_eq!(unfrozen_hash, frozen_hash);
        });
    }

    #[test]
    fn test_try_from_string() {
        // Test Starlark string conversion using helper
        Heap::temp(|heap| {
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
        });
    }

    #[test]
    fn test_try_from_string_with_tolerance() {
        Heap::temp(|heap| {
            let starlark_val = heap.alloc("10kOhm 5%");
            let result = PhysicalValue::try_from(starlark_val.to_value()).unwrap();

            assert_eq!(result.unit, PhysicalUnit::Ohms.into());
            assert_eq!(result.nominal, Decimal::from(10000));
            assert_eq!(result.tolerance(), Decimal::from_f64(0.05).unwrap());
        });
    }

    #[test]
    fn test_try_from_scalar() {
        Heap::temp(|heap| {
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
        });
    }

    #[test]
    fn test_try_from_string_error() {
        Heap::temp(|heap| {
            let invalid_strings = ["invalid", "10kZzz", "abc%", ""];

            for invalid in invalid_strings {
                let starlark_val = heap.alloc(invalid);
                let result = PhysicalValue::try_from(starlark_val.to_value());
                assert!(result.is_err(), "Expected error for '{}'", invalid);
            }
        });
    }

    #[test]
    fn test_equality_and_comparison() {
        Heap::temp(|heap| {
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
            assert!(!v_point.equals(v_str).unwrap());
            assert_eq!(v1.compare(v_str).unwrap(), Ordering::Equal);

            // Test comparison with numeric values (should be treated as dimensionless)
            let num_val = heap.alloc(5.0);
            assert!(!v1.equals(num_val).unwrap()); // Different units

            // Test with dimensionless values
            let dim1 = PhysicalValue::dimensionless(10);
            let dim2 = PhysicalValue::dimensionless(20);
            assert_eq!(dim1.compare(heap.alloc(dim2)).unwrap(), Ordering::Less);
            assert_eq!(dim2.compare(heap.alloc(dim1)).unwrap(), Ordering::Greater);
        });
    }

    #[test]
    fn test_comparison_with_various_input_types() {
        Heap::temp(|heap| {
            let voltage = physical_value(12.0, 0.0, PhysicalUnit::Volts);

            // Equality remains type-specific even though compare accepts coercions
            let voltage_str = heap.alloc("12V");
            assert!(!voltage.equals(voltage_str).unwrap());

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
        });
    }

    #[test]
    fn test_comparison_error_cases() {
        Heap::temp(|heap| {
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
        });
    }

    #[test]
    fn test_dimensionless_comparisons() {
        Heap::temp(|heap| {
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
        });
    }

    #[test]
    fn test_dimensionless_with_string_conversions() {
        Heap::temp(|heap| {
            let voltage = physical_value(2023.0, 0.0, PhysicalUnit::Ohms);

            // Test comparison with numeric string (should be treated as dimensionless)
            let numeric_str = heap.alloc("2000");
            assert_eq!(voltage.compare(numeric_str).unwrap(), Ordering::Greater);
            assert!(!voltage.equals(numeric_str).unwrap()); // Different values

            let same_numeric_str = heap.alloc("2023");
            assert_eq!(voltage.compare(same_numeric_str).unwrap(), Ordering::Equal);
            assert!(!voltage.equals(same_numeric_str).unwrap()); // Different types
        });
    }

    #[test]
    fn test_diff_with_string_conversion() {
        // Test that diff works when the other value is parsed from a string
        use starlark::values::Heap;

        Heap::temp(|heap| {
            let pv1 = heap.alloc(physical_value(3.3, 0.0, PhysicalUnit::Volts));
            let pv2_str = heap.alloc("5V");

            // Convert string to PhysicalValue
            let pv2 = PhysicalValue::try_from(pv2_str).unwrap();
            let pv1_val = PhysicalValue::try_from(pv1).unwrap();

            // Test diff
            let result = pv1_val.diff(&pv2).unwrap();
            assert_eq!(result.nominal, Decimal::from_f64(1.7).unwrap());
            assert_eq!(result.unit, PhysicalUnit::Volts.into());
        });
    }

    #[test]
    fn test_within_same_nominal_different_tolerance() {
        use starlark::values::Heap;

        Heap::temp(|heap| {
            // 3.3V ±5% fits within 3.3V ±10%
            let tight = heap.alloc(physical_value(3.3, 0.05, PhysicalUnit::Volts)); // 3.135V - 3.465V
            let loose = heap.alloc(physical_value(3.3, 0.10, PhysicalUnit::Volts)); // 2.97V - 3.63V
            assert!(
                loose
                    .downcast_ref::<PhysicalValue>()
                    .unwrap()
                    .is_in(tight)
                    .unwrap()
            );

            // 3.3V ±10% does NOT fit within 3.3V ±5%
            assert!(
                !tight
                    .downcast_ref::<PhysicalValue>()
                    .unwrap()
                    .is_in(loose)
                    .unwrap()
            );
        });
    }

    #[test]
    fn test_within_different_nominal_values() {
        use starlark::values::Heap;

        Heap::temp(|heap| {
            // 3.3V ±1% (3.267V - 3.333V) fits within 5V ±50% (2.5V - 7.5V)
            let small = heap.alloc(physical_value(3.3, 0.01, PhysicalUnit::Volts));
            let large = heap.alloc(physical_value(5.0, 0.50, PhysicalUnit::Volts));
            assert!(
                large
                    .downcast_ref::<PhysicalValue>()
                    .unwrap()
                    .is_in(small)
                    .unwrap()
            );

            // 5V ±50% does NOT fit within 3.3V ±1%
            assert!(
                !small
                    .downcast_ref::<PhysicalValue>()
                    .unwrap()
                    .is_in(large)
                    .unwrap()
            );
        });
    }

    #[test]
    fn test_within_exact_match() {
        use starlark::values::Heap;

        Heap::temp(|heap| {
            // Exact values with no tolerance should be within each other
            let v1 = heap.alloc(physical_value(3.3, 0.0, PhysicalUnit::Volts));
            let v2 = heap.alloc(physical_value(3.3, 0.0, PhysicalUnit::Volts));
            assert!(
                v1.downcast_ref::<PhysicalValue>()
                    .unwrap()
                    .is_in(v2)
                    .unwrap()
            );
            assert!(
                v2.downcast_ref::<PhysicalValue>()
                    .unwrap()
                    .is_in(v1)
                    .unwrap()
            );
        });
    }

    #[test]
    fn test_within_zero_tolerance_in_range() {
        use starlark::values::Heap;

        Heap::temp(|heap| {
            // Zero tolerance value at the center of a range
            let exact = heap.alloc(physical_value(3.3, 0.0, PhysicalUnit::Volts));
            let range = heap.alloc(physical_value(3.3, 0.10, PhysicalUnit::Volts)); // 2.97V - 3.63V
            assert!(
                range
                    .downcast_ref::<PhysicalValue>()
                    .unwrap()
                    .is_in(exact)
                    .unwrap()
            );
        });
    }

    #[test]
    fn test_within_zero_tolerance_outside_range() {
        use starlark::values::Heap;

        Heap::temp(|heap| {
            // Zero tolerance value outside a range
            let exact = heap.alloc(physical_value(5.0, 0.0, PhysicalUnit::Volts));
            let range = heap.alloc(physical_value(3.3, 0.10, PhysicalUnit::Volts)); // 2.97V - 3.63V
            assert!(
                !range
                    .downcast_ref::<PhysicalValue>()
                    .unwrap()
                    .is_in(exact)
                    .unwrap()
            );
        });
    }

    #[test]
    fn test_within_edge_cases() {
        use starlark::values::Heap;

        Heap::temp(|heap| {
            // Test boundary conditions
            // Range: 3.3V ±10% = 2.97V - 3.63V
            let range = heap.alloc(physical_value(3.3, 0.10, PhysicalUnit::Volts));

            // Value exactly at lower bound should be within
            let at_min = heap.alloc(physical_value(2.97, 0.0, PhysicalUnit::Volts));
            assert!(
                range
                    .downcast_ref::<PhysicalValue>()
                    .unwrap()
                    .is_in(at_min)
                    .unwrap()
            );

            // Value exactly at upper bound should be within
            let at_max = heap.alloc(physical_value(3.63, 0.0, PhysicalUnit::Volts));
            assert!(
                range
                    .downcast_ref::<PhysicalValue>()
                    .unwrap()
                    .is_in(at_max)
                    .unwrap()
            );

            // Value just outside lower bound should not be within
            let below_min = heap.alloc(physical_value(2.96, 0.0, PhysicalUnit::Volts));
            assert!(
                !range
                    .downcast_ref::<PhysicalValue>()
                    .unwrap()
                    .is_in(below_min)
                    .unwrap()
            );

            // Value just outside upper bound should not be within
            let above_max = heap.alloc(physical_value(3.64, 0.0, PhysicalUnit::Volts));
            assert!(
                !range
                    .downcast_ref::<PhysicalValue>()
                    .unwrap()
                    .is_in(above_max)
                    .unwrap()
            );
        });
    }

    #[test]
    fn test_within_overlapping_but_not_contained() {
        use starlark::values::Heap;

        Heap::temp(|heap| {
            // Ranges that overlap but one doesn't contain the other
            // Range 1: 3.3V ±10% = 2.97V - 3.63V
            // Range 2: 3.5V ±5% = 3.325V - 3.675V
            let range1 = heap.alloc(physical_value(3.3, 0.10, PhysicalUnit::Volts));
            let range2 = heap.alloc(physical_value(3.5, 0.05, PhysicalUnit::Volts));

            // They overlap but neither contains the other
            assert!(
                !range2
                    .downcast_ref::<PhysicalValue>()
                    .unwrap()
                    .is_in(range1)
                    .unwrap()
            );
            assert!(
                !range1
                    .downcast_ref::<PhysicalValue>()
                    .unwrap()
                    .is_in(range2)
                    .unwrap()
            );
        });
    }

    #[test]
    fn test_within_unit_mismatch() {
        use starlark::values::Heap;

        Heap::temp(|heap| {
            // Different units should return an error
            let volts = heap.alloc(physical_value(3.3, 0.1, PhysicalUnit::Volts));
            let amps = heap.alloc(physical_value(3.3, 0.1, PhysicalUnit::Amperes));

            let result = volts.downcast_ref::<PhysicalValue>().unwrap().is_in(amps);
            assert!(result.is_err());
        });
    }

    #[test]
    fn test_within_different_units() {
        use starlark::values::Heap;

        Heap::temp(|heap| {
            // Test with various unit types
            let r1 = heap.alloc(physical_value(1000.0, 0.01, PhysicalUnit::Ohms)); // 1kΩ ±1%
            let r2 = heap.alloc(physical_value(1000.0, 0.05, PhysicalUnit::Ohms)); // 1kΩ ±5%
            assert!(
                r2.downcast_ref::<PhysicalValue>()
                    .unwrap()
                    .is_in(r1)
                    .unwrap()
            );

            let c1 = heap.alloc(physical_value(1e-7, 0.05, PhysicalUnit::Farads)); // 100nF ±5%
            let c2 = heap.alloc(physical_value(1e-7, 0.20, PhysicalUnit::Farads)); // 100nF ±20%
            assert!(
                c2.downcast_ref::<PhysicalValue>()
                    .unwrap()
                    .is_in(c1)
                    .unwrap()
            );

            let f1 = heap.alloc(physical_value(1e6, 0.001, PhysicalUnit::Hertz)); // 1MHz ±0.1%
            let f2 = heap.alloc(physical_value(1e6, 0.01, PhysicalUnit::Hertz)); // 1MHz ±1%
            assert!(
                f2.downcast_ref::<PhysicalValue>()
                    .unwrap()
                    .is_in(f1)
                    .unwrap()
            );
        });
    }

    #[test]
    fn test_within_negative_values() {
        use starlark::values::Heap;

        Heap::temp(|heap| {
            // Test with negative values
            let v1 = heap.alloc(physical_value(-3.3, 0.05, PhysicalUnit::Volts)); // -3.3V ±5%
            let v2 = heap.alloc(physical_value(-3.3, 0.10, PhysicalUnit::Volts)); // -3.3V ±10%
            assert!(
                v2.downcast_ref::<PhysicalValue>()
                    .unwrap()
                    .is_in(v1)
                    .unwrap()
            );
            assert!(
                !v1.downcast_ref::<PhysicalValue>()
                    .unwrap()
                    .is_in(v2)
                    .unwrap()
            );
        });
    }

    #[test]
    fn test_physical_value_unary_minus() {
        use starlark::values::Heap;

        Heap::temp(|heap| {
            let v = heap.alloc(physical_value(3.3, 0.05, PhysicalUnit::Volts));

            // Test unary minus
            let neg = v
                .downcast_ref::<PhysicalValue>()
                .unwrap()
                .minus(heap)
                .unwrap();
            let neg_val = neg.downcast_ref::<PhysicalValue>().unwrap();

            assert_eq!(neg_val.nominal, Decimal::from_f64(-3.3).unwrap());
            assert_eq!(neg_val.tolerance(), Decimal::from_f64(0.05).unwrap()); // Tolerance preserved
            assert_eq!(neg_val.unit, PhysicalUnit::Volts.into());
        });
    }

    #[test]
    fn test_physical_value_bounds_unary_minus() {
        use starlark::values::Heap;

        Heap::temp(|heap| {
            let range = heap.alloc_simple(physical_value_bounds(1.0, 3.0, PhysicalUnit::Volts));

            // Test unary minus (should flip and negate)
            let neg = range
                .downcast_ref::<PhysicalValue>()
                .unwrap()
                .minus(heap)
                .unwrap();
            let neg_range = neg.downcast_ref::<PhysicalValue>().unwrap();

            assert_eq!(neg_range.min, Decimal::from_f64(-3.0).unwrap());
            assert_eq!(neg_range.max, Decimal::from_f64(-1.0).unwrap());
            assert_eq!(neg_range.unit, PhysicalUnit::Volts.into());
        });
    }

    #[test]
    fn test_physical_value_bounds_unary_minus_with_nominal() {
        use starlark::values::Heap;

        Heap::temp(|heap| {
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
                .minus(heap)
                .unwrap();
            let neg_range = neg.downcast_ref::<PhysicalValue>().unwrap();

            assert_eq!(neg_range.nominal, Decimal::from_f64(-2.0).unwrap());
        });
    }

    #[test]
    fn test_is_in_value_in_range() {
        use starlark::values::Heap;

        Heap::temp(|heap| {
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
        });
    }

    #[test]
    fn test_is_in_value_with_tolerance_in_range() {
        use starlark::values::Heap;

        Heap::temp(|heap| {
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
        });
    }

    #[test]
    fn test_is_in_range_in_range() {
        use starlark::values::Heap;

        Heap::temp(|heap| {
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
        });
    }

    #[test]
    fn test_is_in_value_in_value() {
        use starlark::values::Heap;

        Heap::temp(|heap| {
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
        });
    }

    #[test]
    fn test_is_in_range_in_value() {
        use starlark::values::Heap;

        Heap::temp(|heap| {
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
        });
    }

    #[test]
    fn test_is_in_string_arguments() {
        use starlark::values::Heap;

        Heap::temp(|heap| {
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
        });
    }

    #[test]
    fn test_is_in_unit_mismatch() {
        use starlark::values::Heap;

        Heap::temp(|heap| {
            let volts = heap.alloc_simple(physical_value_bounds(3.0, 3.6, PhysicalUnit::Volts));
            let amps = heap.alloc(physical_value(1.0, 0.0, PhysicalUnit::Amperes));

            // Unit mismatch should error
            let result = volts.downcast_ref::<PhysicalValue>().unwrap().is_in(amps);
            assert!(result.is_err());
        });
    }

    #[test]
    fn test_within_method() {
        use starlark::environment::Module;
        use starlark::eval::Evaluator;

        Module::with_temp_heap(|module| {
            let heap = module.heap();
            let mut eval = Evaluator::new(&module);

            // Test case from the regression: 4.7uF ±10% should fit within a 4.7uF requirement.
            let candidate = heap.alloc(physical_value(4.7e-6, 0.1, PhysicalUnit::Farads));
            let requirement = heap.alloc_str("4.7uF"); // 4.7uF (no tolerance = 0%)

            // within() should check if the candidate fits within the requirement.
            let result = eval.eval_function(
                candidate.get_attr("within", heap).unwrap().unwrap(),
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
        });
    }

    #[test]
    fn test_within_vs_is_in_semantics() {
        use starlark::environment::Module;
        use starlark::eval::Evaluator;

        Module::with_temp_heap(|module| {
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
        });
    }

    #[test]
    fn test_physical_value_bounds_compare_disjoint() {
        use starlark::values::Value;

        Heap::temp(|heap| {
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
        });
    }

    #[test]
    fn test_physical_value_bounds_compare_overlapping() {
        use starlark::values::Value;

        Heap::temp(|heap| {
            // range1 = 1V to 3V, range2 = 2V to 4V (overlapping)
            let range1: PhysicalValue = "1V to 3V".parse().unwrap();
            let range2: PhysicalValue = "2V to 4V".parse().unwrap();

            let v1: Value = heap.alloc_simple(range1);
            let v2: Value = heap.alloc_simple(range2);

            // Overlapping ranges use max comparison as tiebreaker
            // range1.max (3V) < range2.max (4V)
            let cmp = v1.compare(v2).unwrap();
            assert_eq!(cmp, std::cmp::Ordering::Less);
        });
    }

    #[test]
    fn test_physical_value_bounds_compare_with_value() {
        use starlark::values::Value;

        Heap::temp(|heap| {
            // range = 1V to 2V, value = 5V (no tolerance)
            let range: PhysicalValue = "1V to 2V".parse().unwrap();
            let value = physical_value(5.0, 0.0, PhysicalUnit::Volts);

            let v_range: Value = heap.alloc_simple(range);
            let v_value: Value = heap.alloc(value);

            // range.max (2V) < value.min (5V), so range < value
            let cmp = v_range.compare(v_value).unwrap();
            assert_eq!(cmp, std::cmp::Ordering::Less);
        });
    }

    #[test]
    fn test_physical_value_bounds_compare_unit_mismatch() {
        use starlark::values::Value;

        Heap::temp(|heap| {
            // range1 = 1V to 2V, range2 = 1A to 2A (different units)
            let range1: PhysicalValue = "1V to 2V".parse().unwrap();
            let range2: PhysicalValue = "1A to 2A".parse().unwrap();

            let v1: Value = heap.alloc_simple(range1);
            let v2: Value = heap.alloc_simple(range2);

            // Should error on unit mismatch
            let result = v1.compare(v2);
            assert!(result.is_err());
        });
    }

    #[test]
    fn test_physical_value_bounds_equals() {
        use starlark::values::Value;

        Heap::temp(|heap| {
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
        });
    }

    #[test]
    fn test_physical_value_bounds_compare_with_string() {
        use starlark::values::Value;

        Heap::temp(|heap| {
            // range = 1V to 2V
            let range: PhysicalValue = "1V to 2V".parse().unwrap();
            let v_range: Value = heap.alloc_simple(range);

            // Compare with string "5V"
            let v_str: Value = heap.alloc_str("5V").to_value();

            // range.max (2V) < 5V, so range < "5V"
            let cmp = v_range.compare(v_str).unwrap();
            assert_eq!(cmp, std::cmp::Ordering::Less);
        });
    }

    #[test]
    fn test_physical_value_bounds_add_value() {
        Heap::temp(|heap| {
            // range = 1V to 2V (1.5V nominal), offset = 3V
            let range: PhysicalValue = "1V to 2V".parse().unwrap();
            let offset = physical_value(3.0, 0.0, PhysicalUnit::Volts);

            let v_range = heap.alloc_simple(range);
            let v_offset = heap.alloc(offset);

            // 1.5V (nominal) + 3V = 4.5V (point value - bounds dropped)
            let result = v_range
                .downcast_ref::<PhysicalValue>()
                .unwrap()
                .add(v_offset, heap)
                .unwrap()
                .unwrap();
            let result_val = result.downcast_ref::<PhysicalValue>().unwrap();

            // Add returns point value based on nominal
            assert_eq!(result_val.nominal, Decimal::from_str("4.5").unwrap());
            assert!(result_val.is_point()); // Bounds are dropped
            assert_eq!(result_val.unit, PhysicalUnit::Volts.into());
        });
    }

    #[test]
    fn test_physical_value_bounds_add_value_with_nominal() {
        Heap::temp(|heap| {
            // range = 1V to 3V (2V nom.), offset = 5V
            let range: PhysicalValue = "1V to 3V (2V nom.)".parse().unwrap();
            let offset = physical_value(5.0, 0.0, PhysicalUnit::Volts);

            let v_range = heap.alloc_simple(range);
            let v_offset = heap.alloc(offset);

            // 2V (nominal) + 5V = 7V (point value - bounds dropped)
            let result = v_range
                .downcast_ref::<PhysicalValue>()
                .unwrap()
                .add(v_offset, heap)
                .unwrap()
                .unwrap();
            let result_val = result.downcast_ref::<PhysicalValue>().unwrap();

            // Add returns point value based on nominal
            assert_eq!(result_val.nominal, Decimal::from(7));
            assert!(result_val.is_point()); // Bounds are dropped
        });
    }

    #[test]
    fn test_physical_value_bounds_sub_value() {
        Heap::temp(|heap| {
            // range = 5V to 10V (7.5V nominal), offset = 2V
            let range: PhysicalValue = "5V to 10V".parse().unwrap();
            let offset = physical_value(2.0, 0.0, PhysicalUnit::Volts);

            let v_range = heap.alloc_simple(range);
            let v_offset = heap.alloc(offset);

            // 7.5V (nominal) - 2V = 5.5V (point value - bounds dropped)
            let result = v_range
                .downcast_ref::<PhysicalValue>()
                .unwrap()
                .sub(v_offset, heap)
                .unwrap();
            let result_val = result.downcast_ref::<PhysicalValue>().unwrap();

            // Sub returns point value based on nominal
            assert_eq!(result_val.nominal, Decimal::from_str("5.5").unwrap());
            assert!(result_val.is_point()); // Bounds are dropped
        });
    }

    #[test]
    fn test_physical_value_bounds_add_unit_mismatch() {
        Heap::temp(|heap| {
            // range = 1V to 2V, offset = 1A (wrong unit)
            let range: PhysicalValue = "1V to 2V".parse().unwrap();
            let offset = physical_value(1.0, 0.0, PhysicalUnit::Amperes);

            let v_range = heap.alloc_simple(range);
            let v_offset = heap.alloc(offset);

            // Should error on unit mismatch
            let result = v_range
                .downcast_ref::<PhysicalValue>()
                .unwrap()
                .add(v_offset, heap)
                .unwrap();
            assert!(result.is_err());
        });
    }

    #[test]
    fn test_physical_value_bounds_add_dimensionless() {
        Heap::temp(|heap| {
            // range = 1V to 2V, offset = 3 (dimensionless)
            let range: PhysicalValue = "1V to 2V".parse().unwrap();
            let offset = PhysicalValue::dimensionless(3);

            let v_range = heap.alloc_simple(range);
            let v_offset = heap.alloc(offset);

            // Adding dimensionless to voltage should fail (unit mismatch)
            let result = v_range
                .downcast_ref::<PhysicalValue>()
                .unwrap()
                .add(v_offset, heap)
                .unwrap();
            assert!(result.is_err());
        });
    }
}
