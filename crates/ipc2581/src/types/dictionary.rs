use super::{
    ecad::PackageOutline,
    primitives::{
        BoundingBox, Color, FillDesc, LineDesc, LineDescGroup, StandardPrimitive, UserPrimitive,
        UserShape,
    },
};
use crate::Symbol;

/// Dictionary of colors
#[derive(Debug, Clone, Default)]
pub struct DictionaryColor {
    pub entries: Vec<EntryColor>,
}

#[derive(Debug, Clone)]
pub struct EntryColor {
    pub id: Symbol,
    pub color: Color,
}

/// Dictionary of line descriptions
#[derive(Debug, Clone, Default)]
pub struct DictionaryLineDesc {
    pub units: Option<Units>,
    pub entries: Vec<EntryLineDesc>,
}

#[derive(Debug, Clone)]
pub struct EntryLineDesc {
    pub id: Symbol,
    pub line_desc: LineDesc,
}

/// Dictionary of fill descriptions
#[derive(Debug, Clone, Default)]
pub struct DictionaryFillDesc {
    pub units: Option<Units>,
    pub entries: Vec<EntryFillDesc>,
}

#[derive(Debug, Clone)]
pub struct EntryFillDesc {
    pub id: Symbol,
    pub fill_desc: FillDesc,
}

/// Dictionary of cached firmware payloads.
#[derive(Debug, Clone, Default)]
pub struct DictionaryFirmware {
    pub entries: Vec<EntryFirmware>,
}

#[derive(Debug, Clone)]
pub struct EntryFirmware {
    pub id: Symbol,
    pub hex_encoded_binary: Symbol,
}

/// Dictionary of embedded or externally referenced fonts.
#[derive(Debug, Clone, Default)]
pub struct DictionaryFont {
    pub units: Option<Units>,
    pub entries: Vec<EntryFont>,
}

#[derive(Debug, Clone)]
pub struct EntryFont {
    pub id: Symbol,
    pub definition: FontDefinition,
}

#[derive(Debug, Clone)]
pub enum FontDefinition {
    Embedded(EmbeddedFont),
    External(ExternalFont),
}

#[derive(Debug, Clone)]
pub struct EmbeddedFont {
    pub name: Symbol,
    pub line_desc: LineDescGroup,
    pub glyphs: Vec<FontGlyph>,
}

#[derive(Debug, Clone)]
pub struct ExternalFont {
    pub name: Symbol,
    pub urn: Symbol,
}

#[derive(Debug, Clone)]
pub struct FontGlyph {
    /// Required IPC-2581C `hexBinary` source text.
    pub char_code: Symbol,
    pub bounding_box: BoundingBox,
    pub shapes: Vec<FontShape>,
}

/// Content allowed by the IPC-2581C `Simple` substitution group in a glyph.
#[derive(Debug, Clone)]
pub enum FontShape {
    Shape(UserShape),
    Outline(PackageOutline),
}

/// Dictionary of standard primitives
#[derive(Debug, Clone, Default)]
pub struct DictionaryStandard {
    pub units: Option<Units>,
    pub entries: Vec<EntryStandard>,
}

#[derive(Debug, Clone)]
pub struct EntryStandard {
    pub id: Symbol,
    pub primitive: StandardPrimitive,
}

/// Dictionary of user-defined primitives
#[derive(Debug, Clone, Default)]
pub struct DictionaryUser {
    pub units: Option<Units>,
    pub entries: Vec<EntryUser>,
}

#[derive(Debug, Clone)]
pub struct EntryUser {
    pub id: Symbol,
    pub primitive: UserPrimitive,
}

/// Units of measurement
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Units {
    Millimeter,
    Inch,
    Micron,
    Mils,
}
