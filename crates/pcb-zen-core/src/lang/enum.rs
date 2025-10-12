use allocative::Allocative;
use serde::{Deserialize, Serialize};
use starlark::{
    any::ProvidesStaticType,
    environment::{Methods, MethodsBuilder, MethodsStatic},
    eval::{Arguments, Evaluator},
    starlark_module, starlark_simple_value,
    typing::{ParamIsRequired, ParamSpec, Ty, TyCallable, TyStarlarkValue, TyUser, TyUserParams},
    values::{
        function::FUNCTION_TYPE,
        starlark_value,
        typing::{TypeInstanceId, TypeMatcher, TypeMatcherFactory},
        Freeze, FreezeResult, Heap, StarlarkValue, Value, ValueLike,
    },
};
use starlark_map::StarlarkHasher;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Mutex, OnceLock};

#[derive(thiserror::Error, Debug)]
enum EnumError {
    #[error("enum values must all be distinct, but repeated `{0}`")]
    DuplicateEnumValue(String),
    #[error("Unknown enum element `{0}`, expected one of: {1}")]
    InvalidElement(String, String),
    #[error("Invalid index {0} for enum with {1} variants")]
    InvalidIndex(i32, usize),
}

#[derive(Debug, Clone, Allocative)]
struct EnumTypeMatcher {
    enum_type: EnumType,
}

impl TypeMatcher for EnumTypeMatcher {
    fn matches(&self, value: Value) -> bool {
        value
            .downcast_ref::<EnumValue>()
            .is_some_and(|v| v.r#type == self.enum_type)
    }
}

#[derive(
    Clone, Hash, Debug, PartialEq, Eq, ProvidesStaticType, Allocative, Serialize, Deserialize,
)]
pub struct EnumType {
    variants: Vec<String>,
}

impl Freeze for EnumType {
    type Frozen = Self;
    fn freeze(self, _freezer: &starlark::values::Freezer) -> FreezeResult<Self::Frozen> {
        Ok(self)
    }
}

starlark_simple_value!(EnumType);

impl fmt::Display for EnumType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "enum({})", self.variants.join(", "))
    }
}

impl EnumType {
    pub fn new(variants: Vec<String>) -> starlark::Result<Self> {
        let mut seen = HashSet::new();
        for variant in &variants {
            if !seen.insert(variant) {
                return Err(starlark::Error::new_other(EnumError::DuplicateEnumValue(
                    variant.clone(),
                )));
            }
        }
        Ok(EnumType { variants })
    }

    pub fn variants(&self) -> &[String] {
        &self.variants
    }

    fn type_instance_id(&self) -> TypeInstanceId {
        static CACHE: OnceLock<Mutex<HashMap<EnumType, TypeInstanceId>>> = OnceLock::new();
        *CACHE
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap()
            .entry(self.clone())
            .or_insert_with(TypeInstanceId::r#gen)
    }

    fn param_spec(&self) -> ParamSpec {
        ParamSpec::new_parts([(ParamIsRequired::Yes, Ty::string())], [], None, [], None).unwrap()
    }

    fn construct(&self, val: &str) -> starlark::Result<EnumValue> {
        self.variants
            .iter()
            .position(|v| v == val)
            .map(|index| EnumValue {
                r#type: self.clone(),
                index: index as i32,
            })
            .ok_or_else(|| {
                starlark::Error::new_other(EnumError::InvalidElement(
                    val.to_string(),
                    self.variants.join(", "),
                ))
            })
    }

    fn get_by_index(&self, index: i32) -> starlark::Result<EnumValue> {
        if index >= 0 && (index as usize) < self.variants.len() {
            Ok(EnumValue {
                r#type: self.clone(),
                index,
            })
        } else {
            Err(starlark::Error::new_other(EnumError::InvalidIndex(
                index,
                self.variants.len(),
            )))
        }
    }
}

#[starlark_value(type = FUNCTION_TYPE)]
impl<'v> StarlarkValue<'v> for EnumType {
    fn write_hash(&self, hasher: &mut StarlarkHasher) -> starlark::Result<()> {
        std::hash::Hash::hash(self, hasher);
        Ok(())
    }

    fn invoke(
        &self,
        _me: Value<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        args.no_named_args()?;
        let val = args.positional1(eval.heap())?;
        let val_str = val.unpack_str().ok_or_else(|| {
            starlark::Error::new_other(anyhow::anyhow!("Enum variant must be a string"))
        })?;
        Ok(eval.heap().alloc(self.construct(val_str)?))
    }

    fn length(&self) -> starlark::Result<i32> {
        Ok(self.variants.len() as i32)
    }

    fn at(&self, index: Value, heap: &'v Heap) -> starlark::Result<Value<'v>> {
        let i = index.unpack_i32().ok_or_else(|| {
            starlark::Error::new_other(anyhow::anyhow!("Index must be an integer"))
        })?;
        Ok(heap.alloc(self.get_by_index(i)?))
    }

    unsafe fn iterate(&self, me: Value<'v>, _heap: &'v Heap) -> starlark::Result<Value<'v>> {
        Ok(me)
    }

    unsafe fn iter_size_hint(&self, index: usize) -> (usize, Option<usize>) {
        let rem = self.variants.len().saturating_sub(index);
        (rem, Some(rem))
    }

    unsafe fn iter_next(&self, index: usize, heap: &'v Heap) -> Option<Value<'v>> {
        (index < self.variants.len()).then(|| {
            heap.alloc(EnumValue {
                r#type: self.clone(),
                index: index as i32,
            })
        })
    }

    unsafe fn iter_stop(&self) {}

    fn get_methods() -> Option<&'static Methods> {
        static RES: MethodsStatic = MethodsStatic::new();
        RES.methods(enum_type_methods)
    }

    fn eval_type(&self) -> Option<Ty> {
        Some(Ty::custom(
            TyUser::new(
                "enum".to_owned(),
                TyStarlarkValue::new::<EnumValue>(),
                self.type_instance_id(),
                TyUserParams {
                    matcher: Some(TypeMatcherFactory::new(EnumTypeMatcher {
                        enum_type: self.clone(),
                    })),
                    ..TyUserParams::default()
                },
            )
            .ok()?,
        ))
    }

    fn typechecker_ty(&self) -> Option<Ty> {
        Some(Ty::custom(
            TyUser::new(
                "EnumType".to_owned(),
                TyStarlarkValue::new::<EnumType>(),
                TypeInstanceId::r#gen(),
                TyUserParams {
                    callable: Some(TyCallable::new(self.param_spec(), self.eval_type()?)),
                    ..TyUserParams::default()
                },
            )
            .ok()?,
        ))
    }
}

#[starlark_module]
fn enum_type_methods(builder: &mut MethodsBuilder) {
    #[starlark(attribute)]
    fn r#type<'v>(this: Value<'v>, heap: &'v Heap) -> starlark::Result<Value<'v>> {
        // Return the string "enum" as the type name
        Ok(heap.alloc("enum"))
    }

    fn values<'v>(this: Value<'v>, heap: &'v Heap) -> starlark::Result<Value<'v>> {
        let enum_type = this
            .downcast_ref::<EnumType>()
            .ok_or_else(|| starlark::Error::new_other(anyhow::anyhow!("Expected EnumType")))?;

        let variants: Vec<Value> = (0..enum_type.variants.len())
            .map(|i| {
                heap.alloc(EnumValue {
                    r#type: enum_type.clone(),
                    index: i as i32,
                })
            })
            .collect();

        Ok(heap.alloc(variants))
    }
}

#[derive(
    Clone, Hash, Debug, PartialEq, Eq, ProvidesStaticType, Allocative, Serialize, Deserialize,
)]
pub struct EnumValue {
    r#type: EnumType,
    index: i32,
}

impl Freeze for EnumValue {
    type Frozen = Self;
    fn freeze(self, _freezer: &starlark::values::Freezer) -> FreezeResult<Self::Frozen> {
        Ok(self)
    }
}

starlark_simple_value!(EnumValue);

impl fmt::Display for EnumValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "enum(\"{}\")", self.r#type.variants[self.index as usize])
    }
}

impl EnumValue {
    pub fn value(&self) -> &str {
        &self.r#type.variants[self.index as usize]
    }
}

#[starlark_value(type = "enum")]
impl<'v> StarlarkValue<'v> for EnumValue {
    fn write_hash(&self, hasher: &mut StarlarkHasher) -> starlark::Result<()> {
        std::hash::Hash::hash(self, hasher);
        Ok(())
    }

    fn equals(&self, other: Value<'v>) -> starlark::Result<bool> {
        Ok(other.downcast_ref::<EnumValue>() == Some(self))
    }

    fn compare(&self, other: Value<'v>) -> starlark::Result<std::cmp::Ordering> {
        match other.downcast_ref::<EnumValue>() {
            Some(other) if self.r#type == other.r#type => Ok(self.index.cmp(&other.index)),
            Some(_) => Err(starlark::Error::new_other(anyhow::anyhow!(
                "Cannot compare enum values from different enum types"
            ))),
            None => Err(starlark::Error::new_other(anyhow::anyhow!(
                "Cannot compare enum value with non-enum value"
            ))),
        }
    }

    fn typechecker_ty(&self) -> Option<Ty> {
        self.r#type.eval_type()
    }

    fn length(&self) -> starlark::Result<i32> {
        Ok(self.value().len() as i32)
    }

    fn at(&self, index: Value<'v>, heap: &'v Heap) -> starlark::Result<Value<'v>> {
        let s = self.value();
        let i = index.unpack_i32().ok_or_else(|| {
            starlark::Error::new_other(anyhow::anyhow!("Index must be an integer"))
        })?;

        let len = s.len() as i32;
        let actual_index = if i < 0 { len + i } else { i };

        if actual_index < 0 || actual_index >= len {
            return Err(starlark::Error::new_other(anyhow::anyhow!(
                "Index out of bounds: {} for string of length {}",
                i,
                len
            )));
        }

        let ch = s.chars().nth(actual_index as usize).ok_or_else(|| {
            starlark::Error::new_other(anyhow::anyhow!("Invalid character index"))
        })?;
        Ok(heap.alloc(ch.to_string()))
    }

    fn add(&self, other: Value<'v>, heap: &'v Heap) -> Option<starlark::Result<Value<'v>>> {
        other
            .unpack_str()
            .map(|s| Ok(heap.alloc(format!("{}{}", self.value(), s))))
    }

    fn radd(&self, other: Value<'v>, heap: &'v Heap) -> Option<starlark::Result<Value<'v>>> {
        other
            .unpack_str()
            .map(|s| Ok(heap.alloc(format!("{}{}", s, self.value()))))
    }

    fn get_methods() -> Option<&'static Methods> {
        static RES: MethodsStatic = MethodsStatic::new();
        RES.methods(enum_value_methods)
    }
}

#[starlark_module]
fn enum_value_methods(builder: &mut MethodsBuilder) {
    #[starlark(attribute)]
    fn index(this: &EnumValue) -> starlark::Result<i32> {
        Ok(this.index)
    }

    #[starlark(attribute)]
    fn value<'v>(this: &EnumValue, heap: &'v Heap) -> starlark::Result<Value<'v>> {
        Ok(heap.alloc(this.value()))
    }
}
