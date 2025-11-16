use crate::Symbol;
use std::str::FromStr;

/// Standard geometric primitives
#[derive(Debug, Clone, PartialEq)]
pub enum StandardPrimitive {
    Circle(Circle),
    RectCenter(RectCenter),
    RectRound(RectRound),
    RectCham(RectCham),
    RectCorner(RectCorner),
    Oval(Oval),
    Butterfly(Butterfly),
    Diamond(Diamond),
    Donut(Donut),
    Ellipse(Ellipse),
    Hexagon(Hexagon),
    Moire(Moire),
    Octagon(Octagon),
    Thermal(Thermal),
    Triangle(Triangle),
    Contour(Contour),
}

/// Circle primitive defined by diameter
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Circle {
    pub diameter: f64,
    pub fill_property: Option<FillProperty>,
    pub line_desc_ref: Option<Symbol>,
}

/// Rectangle centered at origin
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RectCenter {
    pub width: f64,
    pub height: f64,
    pub fill_property: Option<FillProperty>,
    pub line_desc_ref: Option<Symbol>,
}

/// Rectangle with rounded corners
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RectRound {
    pub width: f64,
    pub height: f64,
    pub radius: f64,
    pub upper_right: bool,
    pub upper_left: bool,
    pub lower_right: bool,
    pub lower_left: bool,
}

/// Rectangle with chamfered corners
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RectCham {
    pub width: f64,
    pub height: f64,
    pub chamfer: f64,
    pub upper_right: bool,
    pub upper_left: bool,
    pub lower_right: bool,
    pub lower_left: bool,
}

/// Rectangle defined by corner coordinates
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RectCorner {
    pub lower_left_x: f64,
    pub lower_left_y: f64,
    pub upper_right_x: f64,
    pub upper_right_y: f64,
}

/// Oval (rectangle with rounded ends)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Oval {
    pub width: f64,
    pub height: f64,
}

/// Butterfly shape (round or square with 2 quadrants removed)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Butterfly {
    pub shape: ButterflyShape,
    pub size: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButterflyShape {
    Round,
    Square,
}

/// Diamond (4-sided with equal sides)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Diamond {
    pub width: f64,
    pub height: f64,
}

/// Donut (concentric shapes)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Donut {
    pub shape: DonutShape,
    pub outer_diameter: f64,
    pub inner_diameter: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DonutShape {
    Round,
    Square,
    Hexagon,
    Octagon,
}

/// Ellipse
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ellipse {
    pub width: f64,
    pub height: f64,
}

/// Hexagon (6-sided regular polygon)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hexagon {
    pub point_to_point: f64,
}

/// Moire pattern (registration target)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Moire {
    pub diameter: f64,
    pub ring_width: f64,
    pub ring_gap: f64,
    pub ring_number: u32,
    pub line_width: Option<f64>,
    pub line_length: Option<f64>,
    pub line_angle: Option<f64>,
}

/// Octagon (8-sided regular polygon)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Octagon {
    pub point_to_point: f64,
}

/// Thermal relief pattern
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thermal {
    pub shape: ThermalShape,
    pub outer_diameter: f64,
    pub inner_diameter: f64,
    pub spoke_count: u32,
    pub spoke_width: Option<f64>,
    pub spoke_start_angle: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThermalShape {
    Round,
    Square,
    Hexagon,
    Octagon,
}

/// Triangle (isosceles)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Triangle {
    pub base: f64,
    pub height: f64,
}

/// Contour (arbitrary polygon with optional cutouts)
#[derive(Debug, Clone, PartialEq)]
pub struct Contour {
    pub polygon: Polygon,
    pub cutouts: Vec<Polygon>,
}

/// Polygon (closed shape)
#[derive(Debug, Clone, PartialEq)]
pub struct Polygon {
    pub begin: PolyBegin,
    pub steps: Vec<PolyStep>,
}

/// Polygon starting point
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PolyBegin {
    pub x: f64,
    pub y: f64,
}

/// Polygon continuation step
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PolyStep {
    Segment(PolyStepSegment),
    Curve(PolyStepCurve),
}

/// Straight line segment in polygon
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PolyStepSegment {
    pub x: f64,
    pub y: f64,
}

/// Curved arc segment in polygon
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PolyStepCurve {
    pub x: f64,
    pub y: f64,
    pub center_x: f64,
    pub center_y: f64,
    pub clockwise: bool,
}

/// Polyline (open shape - series of connected lines)
#[derive(Debug, Clone, PartialEq)]
pub struct Polyline {
    pub begin: PolyBegin,
    pub steps: Vec<PolyStep>,
}

/// Line segment
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Line {
    pub start_x: f64,
    pub start_y: f64,
    pub end_x: f64,
    pub end_y: f64,
}

/// Arc segment
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Arc {
    pub start_x: f64,
    pub start_y: f64,
    pub end_x: f64,
    pub end_y: f64,
    pub center_x: f64,
    pub center_y: f64,
    pub clockwise: bool,
}

/// Line description (width, end style, property)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LineDesc {
    pub line_width: f64,
    pub line_end: LineEnd,
    pub line_property: Option<LineProperty>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnd {
    Round,
    Square,
    Flat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineProperty {
    Solid,
    Dashed,
    Dotted,
}

/// Fill description (fill style and color)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FillDesc {
    pub fill_property: FillProperty,
    pub angle1: Option<f64>,
    pub angle2: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillProperty {
    Fill,
    Hollow,
    Void,
    Hatch,
    Mesh,
}

/// Color (RGB)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// Reference to a dictionary entry
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DictRef {
    pub id: Symbol,
}

/// User-defined geometric primitives (from DictionaryUser)
#[derive(Debug, Clone, PartialEq)]
pub enum UserPrimitive {
    UserSpecial(UserSpecial),
    // Other user primitives can be added here (e.g., Text)
}

/// UserSpecial - combination of shapes with line/fill descriptions
#[derive(Debug, Clone, PartialEq)]
pub struct UserSpecial {
    pub shapes: Vec<UserShape>,
}

/// A shape within a UserSpecial, with optional line and fill descriptions
#[derive(Debug, Clone, PartialEq)]
pub struct UserShape {
    pub shape: UserShapeType,
    pub line_desc: Option<LineDesc>,
    pub fill_desc: Option<FillDesc>,
}

/// Types of shapes that can appear in UserSpecial
#[derive(Debug, Clone, PartialEq)]
pub enum UserShapeType {
    Circle(Circle),
    RectCenter(RectCenter),
    Oval(Oval),
    Polygon(Polygon),
    // Other standard shapes as needed (e.g., Polyline)
}

// FromStr implementations for shape enums
impl FromStr for ButterflyShape {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "ROUND" => Ok(ButterflyShape::Round),
            "SQUARE" => Ok(ButterflyShape::Square),
            _ => Err(format!("Unknown butterflyShape: {}", s)),
        }
    }
}

impl FromStr for DonutShape {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "ROUND" => Ok(DonutShape::Round),
            "SQUARE" => Ok(DonutShape::Square),
            "HEXAGON" => Ok(DonutShape::Hexagon),
            "OCTAGON" => Ok(DonutShape::Octagon),
            _ => Err(format!("Unknown donutShape: {}", s)),
        }
    }
}

impl FromStr for ThermalShape {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "ROUND" => Ok(ThermalShape::Round),
            "SQUARE" => Ok(ThermalShape::Square),
            "HEXAGON" => Ok(ThermalShape::Hexagon),
            "OCTAGON" => Ok(ThermalShape::Octagon),
            _ => Err(format!("Unknown thermalShape: {}", s)),
        }
    }
}
