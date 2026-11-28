use allocative::Allocative;
use pcb_sch::physical::PcbTypeIdDomain;
use serde::{Deserialize, Serialize};
use starlark::{
    any::ProvidesStaticType,
    environment::MethodsBuilder,
    eval::{Arguments, Evaluator},
    starlark_module, starlark_simple_value,
    typing::{ParamIsRequired, ParamSpec, Ty, TyCallable, TyStarlarkValue, TyUser, TyUserParams},
    values::{
        Freeze, Heap, StarlarkValue, Value, ValueTyped,
        function::FUNCTION_TYPE,
        list::AllocList,
        starlark_value,
        typing::{TypeInstanceId, TypeMatcher, TypeMatcherDyn, TypeMatcherFactory},
    },
};
use starlark_map::StarlarkHasher;
use std::collections::HashSet;
use std::fmt;

#[derive(thiserror::Error, Debug)]
enum EnumError {
    #[error("enum values must all be distinct, but repeated `{0}`")]
    DuplicateEnumValue(String),
    #[error("Unknown enum element `{0}`, expected one of: {1}")]
    InvalidElement(String, String),
    #[error("Invalid index {0} for enum with {1} variants")]
    InvalidIndex(i32, usize),
}

#[derive(Debug, Clone, Allocative, pagable::Pagable)]
#[pagable::pagable_typetag(TypeMatcherDyn)]
struct EnumTypeMatcher {
    enum_type: EnumType,
}

#[starlark::type_matcher]
impl TypeMatcher for EnumTypeMatcher {
    fn matches(&self, value: Value) -> bool {
        value
            .downcast_ref::<EnumValue>()
            .is_some_and(|v| v.r#type == self.enum_type)
    }

    fn is_stable(&self) -> bool {
        true
    }
}

#[derive(
    Clone,
    Hash,
    Debug,
    PartialEq,
    Eq,
    ProvidesStaticType,
    Allocative,
    Freeze,
    Serialize,
    Deserialize,
    pagable::Pagable,
)]
pub struct EnumType {
    variants: Vec<String>,
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

    fn type_instance_id(&self, domain: PcbTypeIdDomain) -> TypeInstanceId {
        TypeInstanceId::from_identity(domain, &self.variants)
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

    fn eval_type_ty(&self) -> Option<Ty> {
        Some(Ty::custom(
            TyUser::new(
                "enum".to_owned(),
                TyStarlarkValue::new::<EnumValue>(),
                self.type_instance_id(PcbTypeIdDomain::Enum),
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
}

#[starlark_value(type = FUNCTION_TYPE, methods = enum_type_methods)]
impl<'v> StarlarkValue<'v> for EnumType {
    fn write_hash(this: &Self, hasher: &mut StarlarkHasher) -> starlark::Result<()> {
        std::hash::Hash::hash(this, hasher);
        Ok(())
    }

    fn invoke(
        this: &Self,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        args.no_named_args()?;
        let val = args.positional1(eval.heap())?;
        let val_str = val.unpack_str().ok_or_else(|| {
            starlark::Error::new_other(anyhow::anyhow!("Enum variant must be a string"))
        })?;
        Ok(eval.heap().alloc(this.construct(val_str)?).to_value())
    }

    fn length(this: &Self) -> starlark::Result<i32> {
        Ok(this.variants.len() as i32)
    }

    fn at(this: &Self, index: Value, heap: Heap<'v>) -> starlark::Result<Value<'v>> {
        let i = index.unpack_i32().ok_or_else(|| {
            starlark::Error::new_other(anyhow::anyhow!("Index must be an integer"))
        })?;
        Ok(heap.alloc(this.get_by_index(i)?).to_value())
    }

    unsafe fn iterate(this: ValueTyped<'v, Self>, _heap: Heap<'v>) -> starlark::Result<Value<'v>> {
        Ok(this.to_value())
    }

    unsafe fn iter_size_hint(this: &Self, index: usize) -> (usize, Option<usize>) {
        let rem = this.variants.len().saturating_sub(index);
        (rem, Some(rem))
    }

    unsafe fn iter_next(this: &Self, index: usize, heap: Heap<'v>) -> Option<Value<'v>> {
        (index < this.variants.len()).then(|| {
            heap.alloc(EnumValue {
                r#type: this.clone(),
                index: index as i32,
            })
            .to_value()
        })
    }

    unsafe fn iter_stop(_this: &Self) {}

    fn eval_type(this: &Self) -> Option<Ty> {
        this.eval_type_ty()
    }

    fn typechecker_ty(this: &Self) -> Option<Ty> {
        Some(Ty::custom(
            TyUser::new(
                "EnumType".to_owned(),
                TyStarlarkValue::new::<EnumType>(),
                this.type_instance_id(PcbTypeIdDomain::EnumType),
                TyUserParams {
                    callable: Some(TyCallable::new(this.param_spec(), this.eval_type_ty()?)),
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
    fn r#type(this: Value) -> starlark::Result<&'static str> {
        // Return the string "enum" as the type name
        let _ = this;
        Ok("enum")
    }

    fn values<'v>(this: Value<'v>, heap: Heap<'v>) -> starlark::Result<Value<'v>> {
        let enum_type = this
            .downcast_ref::<EnumType>()
            .ok_or_else(|| starlark::Error::new_other(anyhow::anyhow!("Expected EnumType")))?;

        let variants: Vec<Value> = (0..enum_type.variants.len())
            .map(|i| {
                heap.alloc(EnumValue {
                    r#type: enum_type.clone(),
                    index: i as i32,
                })
                .to_value()
            })
            .collect();

        Ok(heap.alloc(AllocList(variants)).to_value())
    }
}

#[derive(
    Clone,
    Hash,
    Debug,
    PartialEq,
    Eq,
    ProvidesStaticType,
    Allocative,
    Freeze,
    Serialize,
    Deserialize,
    pagable::Pagable,
)]
pub struct EnumValue {
    r#type: EnumType,
    index: i32,
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

#[starlark_value(type = "enum", methods = enum_value_methods)]
impl<'v> StarlarkValue<'v> for EnumValue {
    fn write_hash(this: &Self, hasher: &mut StarlarkHasher) -> starlark::Result<()> {
        std::hash::Hash::hash(this, hasher);
        Ok(())
    }

    fn equals(this: &Self, other: Value<'v>) -> starlark::Result<bool> {
        Ok(other.downcast_ref::<EnumValue>() == Some(this))
    }

    fn compare(this: &Self, other: Value<'v>) -> starlark::Result<std::cmp::Ordering> {
        match other.downcast_ref::<EnumValue>() {
            Some(other) if this.r#type == other.r#type => Ok(this.index.cmp(&other.index)),
            Some(_) => Err(starlark::Error::new_other(anyhow::anyhow!(
                "Cannot compare enum values from different enum types"
            ))),
            None => Err(starlark::Error::new_other(anyhow::anyhow!(
                "Cannot compare enum value with non-enum value"
            ))),
        }
    }

    fn typechecker_ty(this: &Self) -> Option<Ty> {
        this.r#type.eval_type_ty()
    }

    fn length(this: &Self) -> starlark::Result<i32> {
        Ok(this.value().len() as i32)
    }

    fn at(this: &Self, index: Value<'v>, heap: Heap<'v>) -> starlark::Result<Value<'v>> {
        let s = this.value();
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
        Ok(heap.alloc(ch.to_string()).to_value())
    }

    fn add(this: &Self, other: Value<'v>, heap: Heap<'v>) -> Option<starlark::Result<Value<'v>>> {
        other
            .unpack_str()
            .map(|s| Ok(heap.alloc(format!("{}{}", this.value(), s)).to_value()))
    }

    fn radd(this: &Self, other: Value<'v>, heap: Heap<'v>) -> Option<starlark::Result<Value<'v>>> {
        other
            .unpack_str()
            .map(|s| Ok(heap.alloc(format!("{}{}", s, this.value())).to_value()))
    }
}

#[starlark_module]
fn enum_value_methods(builder: &mut MethodsBuilder) {
    #[starlark(attribute)]
    fn index(this: &EnumValue) -> starlark::Result<i32> {
        Ok(this.index)
    }

    #[starlark(attribute)]
    fn value(this: &EnumValue) -> starlark::Result<String> {
        Ok(this.value().to_owned())
    }
}
