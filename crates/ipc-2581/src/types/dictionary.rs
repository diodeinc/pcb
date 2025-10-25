use super::primitives::{Color, FillDesc, LineDesc, StandardPrimitive, UserPrimitive};
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
