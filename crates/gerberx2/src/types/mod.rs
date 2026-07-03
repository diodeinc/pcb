use crate::Symbol;
use pcb_ir::geom::{Mirror, Polarity};

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
    /// Aperture attributes active at definition time.
    pub attributes: Vec<Attribute>,
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
pub struct ApertureMacro {
    pub name: Symbol,
    pub primitives: Vec<MacroPrimitive>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MacroPrimitive {
    Comment(Symbol),
    VariableDefinition {
        variable: usize,
        expression: MacroExpression,
    },
    Shape {
        code: i32,
        parameters: Vec<MacroExpression>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum MacroExpression {
    Number(f64),
    Variable(usize),
    UnaryMinus(Box<MacroExpression>),
    Add(Box<MacroExpression>, Box<MacroExpression>),
    Subtract(Box<MacroExpression>, Box<MacroExpression>),
    Multiply(Box<MacroExpression>, Box<MacroExpression>),
    Divide(Box<MacroExpression>, Box<MacroExpression>),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationCode {
    Plot,
    Move,
    Flash,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CoordinateFields {
    pub x: Option<i64>,
    pub y: Option<i64>,
    pub i: Option<i64>,
    pub j: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Comment(Symbol),
    Unit(Unit),
    Format(CoordinateFormat),
    ApertureDefinition(ApertureDefinition),
    ApertureMacro(ApertureMacro),
    SetCurrentAperture(i32),
    PlotMode(PlotMode),
    QuadrantModeMulti,
    Operation {
        fields: CoordinateFields,
        code: OperationCode,
    },
    LoadPolarity(Polarity),
    LoadMirroring(Mirroring),
    LoadRotation(f64),
    LoadScaling(f64),
    BeginRegion,
    EndRegion,
    BeginBlockAperture(i32),
    EndBlockAperture,
    BeginStepRepeat(StepRepeat),
    EndStepRepeat,
    FileAttribute(Attribute),
    ApertureAttribute(Attribute),
    ObjectAttribute(Attribute),
    DeleteAttribute(Option<Symbol>),
    EndOfFile,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StepRepeat {
    pub x_repeats: i32,
    pub y_repeats: i32,
    pub x_step: f64,
    pub y_step: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GraphicsState {
    pub unit: Option<Unit>,
    pub coordinate_format: Option<CoordinateFormat>,
    pub current_point: Option<Point>,
    pub current_aperture: Option<i32>,
    pub plot_mode: Option<PlotMode>,
    pub polarity: Polarity,
    pub mirroring: Mirroring,
    pub rotation_degrees: f64,
    pub scaling: f64,
}

impl Default for GraphicsState {
    fn default() -> Self {
        Self {
            unit: None,
            coordinate_format: None,
            current_point: None,
            current_aperture: None,
            plot_mode: None,
            polarity: Polarity::Dark,
            mirroring: Mirroring::None,
            rotation_degrees: 0.0,
            scaling: 1.0,
        }
    }
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
    pub aperture_attributes: Vec<Attribute>,
    pub object_attributes: Vec<Attribute>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ObjectStream {
    pub objects: Vec<GraphicalObject>,
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
