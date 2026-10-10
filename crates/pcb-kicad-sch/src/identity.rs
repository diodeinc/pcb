//! Stable identity helpers for schematic documents.
//!
//! KiCad links schematics to layout through UUID paths: a sheet UUID chain plus
//! the symbol UUID. For netlist-backed symbols we generate the symbol UUID from
//! the same canonical component path string used by the layout pipeline.

use std::fmt;

use crate::model::Id;

/// Logical page key of the root sheet, used to derive its deterministic UUID.
pub(crate) const ROOT_PAGE_KEY: &str = "root";

const PAGE_UUID_PREFIX: &str = "sch2:page:";

/// Deterministic UUID v5 used for KiCad IDs that should be recoverable from a
/// stable logical key.
pub fn deterministic_uuid(key: impl AsRef<str>) -> Id {
    pcb_sch::kicad_identity::uuid_for_path(key.as_ref())
}

/// Deterministic page UUID for pages created by sch2.
pub(crate) fn deterministic_page_id(page_key: impl AsRef<str>) -> Id {
    deterministic_uuid(format!("{PAGE_UUID_PREFIX}{}", page_key.as_ref()))
}

pub fn root_page_id() -> Id {
    deterministic_page_id(ROOT_PAGE_KEY)
}

/// Join a netlist instance path the same way the pcb layout pipeline does.
///
/// Returns `None` for the root instance, which is not a placeable component.
pub fn canonical_component_path<I, S>(segments: I) -> Option<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut path = String::new();

    for segment in segments {
        let segment = segment.as_ref();
        if segment.is_empty() {
            return None;
        }
        if !path.is_empty() {
            path.push('.');
        }
        path.push_str(segment);
    }

    (!path.is_empty()).then_some(path)
}

/// A netlist component/unit slot that can map to one KiCad schematic symbol.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SymbolSlotKey {
    component_path: String,
    unit: u32,
}

impl SymbolSlotKey {
    pub fn new(component_path: impl Into<String>, unit: u32) -> Option<Self> {
        let component_path = component_path.into();
        if component_path.is_empty() || unit == 0 {
            return None;
        }
        Some(Self {
            component_path,
            unit,
        })
    }

    pub fn component_path(&self) -> &str {
        &self.component_path
    }

    pub fn unit(&self) -> u32 {
        self.unit
    }

    /// Key string used for deterministic UUID generation.
    ///
    /// Unit 1 intentionally uses the bare component path so single-unit symbol
    /// IDs match the existing pcb layout UUID convention.
    pub(crate) fn uuid_key(&self) -> String {
        if self.unit == 1 {
            self.component_path.clone()
        } else {
            format!("{}@U{}", self.component_path, self.unit)
        }
    }

    pub fn symbol_id(&self) -> Id {
        deterministic_uuid(self.uuid_key())
    }
}

impl fmt::Display for SymbolSlotKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.uuid_key())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_uuid_matches_python_namespace_url() {
        assert_eq!(
            deterministic_uuid("R1"),
            "993684ed-29bc-53ba-bc0d-39d7d84da9bd"
        );
        assert_eq!(
            deterministic_uuid("BUCK.U1"),
            "6ea18345-0f07-5a15-a6d1-0870367a6dd4"
        );
    }

    #[test]
    fn canonical_component_path_matches_layout_convention() {
        assert_eq!(
            canonical_component_path(["BUCK", "U1"]).as_deref(),
            Some("BUCK.U1")
        );
        assert_eq!(canonical_component_path(Vec::<String>::new()), None);
        assert_eq!(canonical_component_path(["BUCK", ""]), None);
    }

    #[test]
    fn symbol_slot_suffixes_non_primary_units() {
        let slot = SymbolSlotKey::new("BUCK.U1", 2).expect("valid slot");

        assert_eq!(slot.uuid_key(), "BUCK.U1@U2");
        assert_eq!(slot.symbol_id(), "0a774e0e-ef6a-58f5-acae-b595eb1cd1fe");
    }

    #[test]
    fn symbol_slot_rejects_unit_zero() {
        assert_eq!(SymbolSlotKey::new("U1", 0), None);
    }
}
