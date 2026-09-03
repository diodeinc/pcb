use std::collections::BTreeMap;

use pcb_sexpr::Sexpr;
use serde::{Deserialize, Serialize};

pub type Id = String;
pub type LibId = String;

const fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SchDocument {
    pub pages: Vec<SchPage>,
    /// UUIDs of the project's top-level schematic pages, in project order.
    pub root_page_ids: Vec<Id>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SymbolLibrary {
    pub definitions: BTreeMap<LibId, SymbolDefinition>,
    /// Library children that this crate does not interpret.
    pub unsupported: Vec<Sexpr>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SymbolDefinition {
    pub lib_id: LibId,
    /// Raw KiCad library-symbol S-expression. We keep this opaque until the
    /// editor needs to own symbol graphics and pin definitions directly.
    pub sexpr: Sexpr,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SchPage {
    pub id: Id,
    pub file_name: Option<String>,
    pub library: SymbolLibrary,
    pub paper: Paper,
    pub items: Vec<SchItem>,
}

impl SchPage {
    pub fn new(id: impl Into<Id>) -> Self {
        Self {
            id: id.into(),
            file_name: None,
            library: SymbolLibrary::default(),
            paper: Paper::default(),
            items: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Paper {
    Named { name: String, portrait: bool },
    Custom { width_mm: f64, height_mm: f64 },
}

impl Default for Paper {
    fn default() -> Self {
        Self::Named {
            name: "A4".to_string(),
            portrait: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SchItem {
    Symbol(Symbol),
    Wire(Wire),
    Junction(Junction),
    NoConnect(NoConnect),
    Label(Label),
    Sheet(Box<Sheet>),
    /// A KiCad top-level item that this crate does not interpret.
    Unsupported(Sexpr),
}

impl SchItem {
    /// Return the KiCad UUID for a typed item.
    ///
    /// Unsupported items remain opaque and do not expose a typed identity.
    pub fn id(&self) -> Option<&str> {
        match self {
            Self::Symbol(item) => Some(&item.id),
            Self::Wire(item) => Some(&item.id),
            Self::Junction(item) => Some(&item.id),
            Self::NoConnect(item) => Some(&item.id),
            Self::Label(item) => Some(&item.id),
            Self::Sheet(item) => Some(&item.id),
            Self::Unsupported(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sheet {
    pub id: Id,
    pub at: Option<Point>,
    pub size: Option<Point>,
    pub name: Option<SymbolField>,
    pub file: SymbolField,
    pub pins: Vec<SheetPin>,
    /// Direct child expressions not represented by the semantic fields above.
    pub unsupported: Vec<Sexpr>,
}

impl Sheet {
    pub fn file_name(&self) -> &str {
        &self.file.value
    }

    pub fn bounds(&self) -> Option<(Point, Point)> {
        let at = self.at?;
        let size = self.size?;
        Some((at, Point::new(at.x + size.x, at.y + size.y)))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SheetPin {
    pub id: Id,
    pub name: String,
    pub at: Point,
    pub rotation: Rotation,
    pub shape: LabelShape,
    /// Direct child expressions not represented by the semantic fields above.
    pub unsupported: Vec<Sexpr>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Symbol {
    pub id: Id,
    pub lib_id: LibId,
    pub unit: u32,
    pub body_style: u32,
    pub at: Point,
    pub rotation: Rotation,
    pub mirror: Option<MirrorAxis>,
    #[serde(default)]
    pub dnp: bool,
    #[serde(default = "default_true")]
    pub in_bom: bool,
    #[serde(default = "default_true")]
    pub on_board: bool,
    #[serde(default = "default_true")]
    pub in_pos_files: bool,
    pub fields_autoplaced: bool,
    pub fields: BTreeMap<String, SymbolField>,
    pub pins: Vec<PinInstance>,
    /// Direct child expressions not represented by the semantic fields above.
    pub unsupported: Vec<Sexpr>,
}

impl Symbol {
    pub fn reference(&self) -> Option<&str> {
        self.field_value("Reference")
    }

    pub fn field(&self, name: &str) -> Option<&SymbolField> {
        self.fields.get(name)
    }

    pub fn field_value(&self, name: &str) -> Option<&str> {
        self.field(name).map(|field| field.value.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SymbolField {
    pub private: bool,
    pub name: String,
    pub value: String,
    /// Absolute schematic editor position in millimeters.
    pub at: Point,
    pub rotation_deg: f64,
    pub effects: TextEffects,
    pub justify: Option<FieldJustify>,
    pub hidden: bool,
    pub do_not_autoplace: bool,
    /// Direct child expressions not represented by the semantic fields above.
    pub unsupported: Vec<Sexpr>,
}

impl SymbolField {
    pub fn new(name: impl Into<String>, value: impl Into<String>, at: Point) -> Self {
        Self {
            private: false,
            name: name.into(),
            value: value.into(),
            at,
            rotation_deg: 0.0,
            effects: TextEffects::default(),
            justify: None,
            hidden: false,
            do_not_autoplace: false,
            unsupported: Vec::new(),
        }
    }

    pub fn with_rotation_deg(mut self, rotation_deg: f64) -> Self {
        self.rotation_deg = rotation_deg;
        self
    }

    pub fn with_hidden(mut self, hidden: bool) -> Self {
        self.hidden = hidden;
        self
    }

    pub fn with_justify(mut self, justify: Option<FieldJustify>) -> Self {
        self.justify = justify;
        self
    }

    pub fn with_do_not_autoplace(mut self, do_not_autoplace: bool) -> Self {
        self.do_not_autoplace = do_not_autoplace;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Label {
    pub id: Id,
    pub text: String,
    /// Electrical anchor point in schematic editor coordinates.
    pub at: Point,
    pub kind: LabelKind,
    /// KiCad's label spin abstraction: orientation plus anchor justification.
    pub spin: LabelSpin,
    pub effects: TextEffects,
    pub fields_autoplaced: bool,
    pub fields: BTreeMap<String, SymbolField>,
    /// Direct child expressions not represented by the semantic fields above.
    pub unsupported: Vec<Sexpr>,
}

impl Label {
    pub fn new(id: impl Into<Id>, text: impl Into<String>, at: Point) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
            at,
            kind: LabelKind::Local,
            spin: LabelSpin::default(),
            effects: TextEffects::default(),
            fields_autoplaced: false,
            fields: BTreeMap::new(),
            unsupported: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum LabelKind {
    #[default]
    Local,
    Global {
        shape: LabelShape,
    },
    Hierarchical {
        shape: LabelShape,
    },
    /// A connectable KiCad netclass/directive flag. It carries no net name.
    Directive {
        shape: LabelShape,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum LabelShape {
    Input,
    Output,
    #[default]
    Bidirectional,
    TriState,
    Passive,
    Dot,
    Round,
    Diamond,
    Rectangle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
pub enum LabelSpin {
    Left,
    Up,
    #[default]
    Right,
    Bottom,
}

impl LabelSpin {
    pub const fn horizontal_justify(self) -> FieldHorizontalJustify {
        match self {
            Self::Left | Self::Bottom => FieldHorizontalJustify::Right,
            Self::Up | Self::Right => FieldHorizontalJustify::Left,
        }
    }

    pub const fn is_vertical(self) -> bool {
        matches!(self, Self::Up | Self::Bottom)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextEffects {
    pub font_size: TextSize,
    pub thickness: Option<f64>,
    pub bold: bool,
    pub italic: bool,
    /// Font children not represented by the semantic fields above.
    pub font_unsupported: Vec<Sexpr>,
    /// Effects children not represented by the semantic fields above.
    pub unsupported: Vec<Sexpr>,
}

impl Default for TextEffects {
    fn default() -> Self {
        Self {
            font_size: TextSize::new(1.27, 1.27),
            thickness: None,
            bold: false,
            italic: false,
            font_unsupported: Vec::new(),
            unsupported: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TextSize {
    pub x: f64,
    pub y: f64,
}

impl TextSize {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct FieldJustify {
    pub horizontal: Option<FieldHorizontalJustify>,
    pub vertical: Option<FieldVerticalJustify>,
}

impl FieldJustify {
    pub const fn new(
        horizontal: Option<FieldHorizontalJustify>,
        vertical: Option<FieldVerticalJustify>,
    ) -> Self {
        Self {
            horizontal,
            vertical,
        }
    }

    pub const fn left() -> Self {
        Self::new(Some(FieldHorizontalJustify::Left), None)
    }

    pub const fn right() -> Self {
        Self::new(Some(FieldHorizontalJustify::Right), None)
    }

    pub const fn top() -> Self {
        Self::new(None, Some(FieldVerticalJustify::Top))
    }

    pub const fn bottom() -> Self {
        Self::new(None, Some(FieldVerticalJustify::Bottom))
    }

    pub const fn centered() -> Self {
        Self::new(
            Some(FieldHorizontalJustify::Center),
            Some(FieldVerticalJustify::Center),
        )
    }

    pub const fn is_empty(self) -> bool {
        self.horizontal.is_none() && self.vertical.is_none()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldHorizontalJustify {
    Left,
    Right,
    Center,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldVerticalJustify {
    Top,
    Bottom,
    Center,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PinInstance {
    pub number: String,
    pub id: Id,
    pub alternate: Option<String>,
    /// Direct child expressions not represented by the semantic fields above.
    pub unsupported: Vec<Sexpr>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Wire {
    pub id: Id,
    pub a: Point,
    pub b: Point,
    /// Direct child expressions not represented by the semantic fields above.
    pub unsupported: Vec<Sexpr>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Junction {
    pub id: Id,
    pub at: Point,
    /// Direct child expressions not represented by the semantic fields above.
    pub unsupported: Vec<Sexpr>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NoConnect {
    pub id: Id,
    pub at: Point,
    /// Direct child expressions not represented by the semantic fields above.
    pub unsupported: Vec<Sexpr>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct Point {
    /// KiCad schematic X coordinate in millimeters.
    pub x: f64,
    /// KiCad schematic Y coordinate in millimeters. Positive Y points down the page.
    pub y: f64,
}

impl Point {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Rotation {
    #[default]
    Deg0,
    Deg90,
    Deg180,
    Deg270,
}

impl Rotation {
    pub fn from_degrees(degrees: i64) -> Option<Self> {
        match degrees.rem_euclid(360) {
            0 => Some(Self::Deg0),
            90 => Some(Self::Deg90),
            180 => Some(Self::Deg180),
            270 => Some(Self::Deg270),
            _ => None,
        }
    }

    pub const fn degrees(self) -> i64 {
        match self {
            Self::Deg0 => 0,
            Self::Deg90 => 90,
            Self::Deg180 => 180,
            Self::Deg270 => 270,
        }
    }

    pub fn rotated_by(self, degrees: i64) -> Option<Self> {
        Self::from_degrees(self.degrees() + degrees)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MirrorAxis {
    X,
    Y,
}
