use crate::Symbol;
use pcb_ir::geom::{Mirror, Polarity, Span};

/// Gerber load-mirroring state (`LM`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mirroring {
    None,
    X,
    Y,
    XY,
}

impl From<Mirroring> for Mirror {
    fn from(mirroring: Mirroring) -> Mirror {
        match mirroring {
            Mirroring::None => Mirror::NONE,
            Mirroring::X => Mirror::X,
            Mirroring::Y => Mirror::Y,
            Mirroring::XY => Mirror::XY,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Millimeter,
    Inch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoordinateFormat {
    pub x_integer_digits: u8,
    pub x_decimal_digits: u8,
    pub y_integer_digits: u8,
    pub y_decimal_digits: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Attribute {
    pub name: Symbol,
    pub fields: Vec<Symbol>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ApertureDefinition {
    pub code: i32,
    pub template: ApertureTemplate,
    pub geometry: Option<ApertureGeometry>,
    /// Aperture attributes active at definition time, in
    /// [`crate::GerberX2::attributes`].
    pub attributes: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ApertureTemplate {
    Circle {
        diameter: f64,
        hole_diameter: Option<f64>,
    },
    Rectangle {
        width: f64,
        height: f64,
        hole_diameter: Option<f64>,
    },
    Obround {
        width: f64,
        height: f64,
        hole_diameter: Option<f64>,
    },
    Polygon {
        outer_diameter: f64,
        vertices: i32,
        rotation_degrees: Option<f64>,
        hole_diameter: Option<f64>,
    },
    Macro {
        name: Symbol,
        parameters: Vec<f64>,
    },
    Block {
        objects: Vec<GraphicalObject>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ApertureGeometry {
    pub paths: Vec<GeometryPath>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GeometryPath {
    pub contours: Vec<GeometryContour>,
    pub polarity: Polarity,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GeometryContour {
    pub commands: Vec<PathCommand>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PathCommand {
    MoveTo(Point),
    LineTo(Point),
    ArcTo {
        end: Point,
        center: Point,
        clockwise: bool,
    },
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlotMode {
    Linear,
    ClockwiseArc,
    CounterclockwiseArc,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StepRepeat {
    pub x_repeats: i32,
    pub y_repeats: i32,
    pub x_step: f64,
    pub y_step: f64,
}

/// One `%SR` block: a run of the object stream imaged at every position of
/// a regular grid.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StepRepeatBlock {
    pub repeat: StepRepeat,
    /// The repeated run within [`crate::GerberX2::objects`].
    pub objects: Span,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ObjectKind {
    Draw {
        start: Point,
        end: Point,
        aperture: i32,
    },
    Arc {
        start: Point,
        end: Point,
        center_offset: Point,
        clockwise: bool,
        aperture: i32,
    },
    Flash {
        at: Point,
        aperture: i32,
    },
    Region {
        contours: Vec<Contour>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct GraphicalObject {
    pub kind: ObjectKind,
    pub polarity: Polarity,
    pub mirroring: Mirroring,
    pub rotation_degrees: f64,
    pub scaling: f64,
    /// Attribute sets in [`crate::GerberX2::attributes`]. Objects imaged
    /// under one dictionary state share one set.
    pub aperture_attributes: Span,
    pub object_attributes: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Contour {
    pub segments: Vec<ContourSegment>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ContourSegment {
    Line {
        start: Point,
        end: Point,
    },
    Arc {
        start: Point,
        end: Point,
        center_offset: Point,
        clockwise: bool,
    },
}
