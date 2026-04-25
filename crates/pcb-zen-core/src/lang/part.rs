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
impl<'v> StarlarkValue<'v> for PartValue
where
    Self: ProvidesStaticType<'v>,
{
    fn get_attr(&self, attr: &str, heap: &'v Heap) -> Option<Value<'v>> {
        match attr {
            "mpn" => Some(heap.alloc_str(self.mpn()).to_value()),
            "manufacturer" => Some(heap.alloc_str(self.manufacturer()).to_value()),
            "datasheet" => Some(
                self.datasheet()
                    .map(|datasheet| heap.alloc_str(datasheet).to_value())
                    .unwrap_or_else(Value::new_none),
            ),
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
        matches!(
            attr,
            "mpn" | "manufacturer" | "qualifications" | "datasheet"
        )
    }

    fn dir_attr(&self) -> Vec<String> {
        vec![
            "mpn".to_string(),
            "manufacturer".to_string(),
            "qualifications".to_string(),
            "datasheet".to_string(),
        ]
    }
}
