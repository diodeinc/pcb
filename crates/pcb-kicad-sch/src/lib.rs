//! KiCad schematic project model and electrical analysis.
//!
//! This crate owns a deliberately small modern `.kicad_sch` document model.
//! Zener netlists and persisted KiCad documents reduce independently into the
//! same source-independent connectivity graph before comparison.

mod component_slots;
pub mod connectivity;
pub mod identity;
pub mod kicad;
pub mod model;
mod project;
mod root_interface;
mod symbol;

pub mod analysis;

pub use identity::{
    KiCadUuidPath, ROOT_PAGE_KEY, SymbolSlotKey, UUID_NAMESPACE_URL, canonical_component_path,
    deterministic_page_id, deterministic_uuid, root_page_id,
};
pub use kicad::{KicadSchFile, KicadSchSource, parse_kicad_sch_page};
pub use model::{
    FieldHorizontalJustify, FieldJustify, FieldVerticalJustify, Junction, Label, LabelKind,
    LabelShape, LabelSpin, MirrorAxis, NoConnect, Paper, PinInstance, Point, Rotation, SchDocument,
    SchItem, SchPage, Sheet, SheetPin, Symbol, SymbolDefinition, SymbolField, SymbolLibrary,
    TextEffects, TextSize, Wire,
};
pub use project::{KicadProject, schematic_project_path};
