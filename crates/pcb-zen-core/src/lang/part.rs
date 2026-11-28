use allocative::Allocative;
use serde_json::{Map as JsonMap, Value as JsonValue};
use starlark::{
    any::ProvidesStaticType,
    starlark_simple_value,
    values::{
        Freeze, Heap, NoSerialize, StarlarkValue, Trace, Value, list::AllocList, starlark_value,
    },
};

use crate::config::ManifestPart;

/// Part represents a typed manufacturer part selection for components and alternatives.
#[derive(
    Clone, Debug, Trace, ProvidesStaticType, NoSerialize, Allocative, Freeze, PartialEq, Eq,
)]
#[repr(C)]
pub struct PartValue {
    mpn: String,
    manufacturer: String,
    qualifications: Vec<String>,
    datasheet: Option<String>,
}

impl PartValue {
    pub fn new(
        mpn: String,
        manufacturer: String,
        qualifications: Vec<String>,
        datasheet: Option<String>,
    ) -> Self {
        Self {
            mpn,
            manufacturer,
            qualifications,
            datasheet,
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

    pub fn datasheet(&self) -> Option<&str> {
        self.datasheet.as_deref()
    }

    pub fn to_json_value(&self) -> JsonValue {
        let mut object = JsonMap::from_iter([
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
        ]);
        if let Some(datasheet) = &self.datasheet {
            object.insert(
                "datasheet".to_string(),
                JsonValue::String(datasheet.clone()),
            );
        }
        JsonValue::Object(object)
    }
}

impl From<ManifestPart> for PartValue {
    fn from(part: ManifestPart) -> Self {
        Self::new(
            part.mpn,
            part.manufacturer,
            part.qualifications,
            part.datasheet,
        )
    }
}

impl std::fmt::Display for PartValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Part({}, {})", self.mpn, self.manufacturer)
    }
}

starlark_simple_value!(PartValue);

#[starlark_value(type = "Part")]
impl<'v> StarlarkValue<'v> for PartValue {
    fn get_attr(this: &Self, attr: &str, heap: Heap<'v>) -> Option<Value<'v>> {
        match attr {
            "mpn" => Some(heap.alloc(this.mpn()).to_value()),
            "manufacturer" => Some(heap.alloc(this.manufacturer()).to_value()),
            "datasheet" => Some(
                this.datasheet()
                    .map(|datasheet| heap.alloc(datasheet).to_value())
                    .unwrap_or_else(Value::new_none),
            ),
            "qualifications" => Some(
                heap.alloc(AllocList(
                    this.qualifications()
                        .iter()
                        .map(|q| heap.alloc(q.as_str()).to_value())
                        .collect::<Vec<_>>(),
                ))
                .to_value(),
            ),
            _ => None,
        }
    }

    fn has_attr(this: &Self, attr: &str, _heap: Heap<'v>) -> bool {
        let _ = this;
        matches!(
            attr,
            "mpn" | "manufacturer" | "qualifications" | "datasheet"
        )
    }

    fn dir_attr(this: &Self) -> Vec<String> {
        let _ = this;
        vec![
            "mpn".to_string(),
            "manufacturer".to_string(),
            "qualifications".to_string(),
            "datasheet".to_string(),
        ]
    }
}
