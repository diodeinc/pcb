//! Stable identity helpers for schematic documents.
//!
//! KiCad links schematics to layout through UUID paths: a sheet UUID chain plus
//! the symbol UUID. For netlist-backed symbols we generate the symbol UUID from
//! the same canonical component path string used by the layout pipeline.

use std::{
    fmt,
    path::{Component, Path, PathBuf},
};

pub use pcb_sch::kicad_identity::UUID_NAMESPACE_URL;

use crate::model::Id;

/// Python's `uuid.NAMESPACE_URL`, matching the layout sync code in pcb.
pub const ROOT_PAGE_KEY: &str = "root";

const PAGE_UUID_PREFIX: &str = "sch2:page:";

/// Deterministic UUID v5 used for KiCad IDs that should be recoverable from a
/// stable logical key.
pub fn deterministic_uuid(key: impl AsRef<str>) -> Id {
    pcb_sch::kicad_identity::uuid_for_path(key.as_ref())
}

/// Deterministic page UUID for pages created by sch2.
pub fn deterministic_page_id(page_key: impl AsRef<str>) -> Id {
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

/// Normalize `.` and `..` components without consulting the filesystem.
///
/// Project adapters use this when resolving sheet filenames so their page
/// names match the pure hierarchy analysis.
pub fn normalize_schematic_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(
                    normalized.components().next_back(),
                    Some(Component::Normal(_))
                ) {
                    normalized.pop();
                } else if !path.is_absolute() {
                    normalized.push("..");
                }
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
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

    pub fn from_path_segments<I, S>(segments: I, unit: u32) -> Option<Self>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        canonical_component_path(segments)
            .and_then(|component_path| Self::new(component_path, unit))
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
    pub fn uuid_key(&self) -> String {
        if self.unit == 1 {
            self.component_path.clone()
        } else {
            format!("{}@U{}", self.component_path, self.unit)
        }
    }

    pub fn symbol_id(&self) -> Id {
        deterministic_uuid(self.uuid_key())
    }

    pub fn component_id(&self) -> Id {
        deterministic_uuid(&self.component_path)
    }

    /// Current pcb layout-sync KIID path convention for managed footprints.
    pub fn layout_sync_footprint_path(&self) -> KiCadUuidPath {
        let id = self.component_id();
        KiCadUuidPath::from_segments([id.clone(), id])
    }

    /// Native KiCad schematic/layout association path for this symbol under a
    /// particular sheet path.
    pub fn symbol_path_in_sheet(&self, sheet_path: &KiCadUuidPath) -> KiCadUuidPath {
        sheet_path.with_child(self.symbol_id())
    }
}

/// A KiCad UUID path serialized as `/sheet_uuid/.../symbol_uuid`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct KiCadUuidPath {
    segments: Vec<Id>,
}

impl KiCadUuidPath {
    pub fn root() -> Self {
        Self::default()
    }

    pub fn from_segments<I, S>(segments: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<Id>,
    {
        Self {
            segments: segments.into_iter().map(Into::into).collect(),
        }
    }

    pub fn for_page(page_id: impl Into<Id>) -> Self {
        Self::from_segments([page_id.into()])
    }

    pub fn segments(&self) -> &[Id] {
        &self.segments
    }

    pub fn with_child(&self, id: impl Into<Id>) -> Self {
        let mut segments = self.segments.clone();
        segments.push(id.into());
        Self { segments }
    }

    pub fn to_kicad_string(&self) -> String {
        if self.segments.is_empty() {
            "/".to_string()
        } else {
            format!("/{}", self.segments.join("/"))
        }
    }
}

impl fmt::Display for KiCadUuidPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_kicad_string())
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
    fn symbol_slot_uses_bare_path_for_unit_one() {
        let slot = SymbolSlotKey::new("BUCK.U1", 1).expect("valid slot");

        assert_eq!(slot.uuid_key(), "BUCK.U1");
        assert_eq!(slot.symbol_id(), "6ea18345-0f07-5a15-a6d1-0870367a6dd4");
    }

    #[test]
    fn symbol_slot_suffixes_non_primary_units() {
        let slot = SymbolSlotKey::new("BUCK.U1", 2).expect("valid slot");

        assert_eq!(slot.uuid_key(), "BUCK.U1@U2");
        assert_eq!(slot.symbol_id(), "0a774e0e-ef6a-58f5-acae-b595eb1cd1fe");
        assert_eq!(slot.component_id(), "6ea18345-0f07-5a15-a6d1-0870367a6dd4");
    }

    #[test]
    fn symbol_slot_rejects_unit_zero() {
        assert_eq!(SymbolSlotKey::new("U1", 0), None);
    }

    #[test]
    fn formats_kicad_uuid_paths() {
        assert_eq!(KiCadUuidPath::root().to_kicad_string(), "/");
        assert_eq!(
            KiCadUuidPath::from_segments(["sheet", "symbol"]).to_kicad_string(),
            "/sheet/symbol"
        );
    }

    #[test]
    fn derives_native_symbol_and_layout_sync_paths() {
        let slot = SymbolSlotKey::new("R1", 1).expect("valid slot");
        let sheet_path = KiCadUuidPath::for_page(root_page_id());

        assert_eq!(
            sheet_path.to_kicad_string(),
            "/513d9cc1-d27a-514d-a37c-2f1ca04dddfa"
        );
        assert_eq!(
            slot.symbol_path_in_sheet(&sheet_path).to_kicad_string(),
            "/513d9cc1-d27a-514d-a37c-2f1ca04dddfa/993684ed-29bc-53ba-bc0d-39d7d84da9bd"
        );
        assert_eq!(
            slot.layout_sync_footprint_path().to_kicad_string(),
            pcb_sch::kicad_identity::footprint_kiid_path("R1")
        );
        assert_eq!(
            slot.symbol_id(),
            pcb_sch::kicad_identity::uuid_for_path("R1")
        );
    }
}
