use allocative::Allocative;
use serde_json::{Map as JsonMap, Value as JsonValue};
use starlark::{
    any::ProvidesStaticType,
    starlark_simple_value,
    values::{
        Freeze, Heap, NoSerialize, StarlarkValue, Trace, Value, list::AllocList, starlark_value,
    },
};

/// Part represents a typed manufacturer part selection for components and alternatives.
#[derive(
    Clone, Debug, Trace, ProvidesStaticType, NoSerialize, Allocative, Freeze, PartialEq, Eq,
)]
#[repr(C)]
pub struct PartValue {
    mpn: String,
    manufacturer: String,
    qualifications: Vec<String>,
}

impl PartValue {
    pub fn new(mpn: String, manufacturer: String, qualifications: Vec<String>) -> Self {
        Self {
            mpn,
            manufacturer,
            qualifications,
        }
    }

    pub fn mpn(&self) -> &str {
        &self.mpn
    }

    pub fn manufacturer(&self) -> &str {
        &self.manufacturer
    }

    pub fn qualifications(&self) -> &[String] {
        &self.qualifications
    }

    pub fn to_json_value(&self) -> JsonValue {
        JsonValue::Object(JsonMap::from_iter([
            ("mpn".to_string(), JsonValue::String(self.mpn.clone())),
            (
                "manufacturer".to_string(),
                JsonValue::String(self.manufacturer.clone()),
            ),
            (
                "qualifications".to_string(),
                JsonValue::Array(
                    self.qualifications
                        .iter()
                        .cloned()
                        .map(JsonValue::String)
                        .collect(),
                ),
            ),
        ]))
    }
}

impl std::fmt::Display for PartValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Part({}, {})", self.mpn, self.manufacturer)
    }
}

starlark_simple_value!(PartValue);

#[starlark_value(type = "Part")]
impl<'v> StarlarkValue<'v> for PartValue
where
    Self: ProvidesStaticType<'v>,
{
    fn get_attr(&self, attr: &str, heap: &'v Heap) -> Option<Value<'v>> {
        match attr {
            "mpn" => Some(heap.alloc_str(self.mpn()).to_value()),
            "manufacturer" => Some(heap.alloc_str(self.manufacturer()).to_value()),
            "qualifications" => Some(
                heap.alloc(AllocList(
                    self.qualifications()
                        .iter()
                        .map(|q| heap.alloc_str(q).to_value())
                        .collect::<Vec<_>>(),
                )),
            ),
            _ => None,
        }
    }

    fn has_attr(&self, attr: &str, _heap: &'v Heap) -> bool {
        matches!(attr, "mpn" | "manufacturer" | "qualifications")
    }

    fn dir_attr(&self) -> Vec<String> {
        vec![
            "mpn".to_string(),
            "manufacturer".to_string(),
            "qualifications".to_string(),
        ]
    }
}
