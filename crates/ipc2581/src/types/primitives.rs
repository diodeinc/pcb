use super::Xform;
use crate::Symbol;
use std::fmt;

/// 2D point
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

/// 2D size (width × height)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Size {
    pub width: f64,
    pub height: f64,
}

/// Wrapper for primitives with optional fill and line styling
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Styled<T> {
    pub shape: T,
    pub fill_property: Option<FillProperty>,
    pub line_desc: Option<LineDesc>,
    pub line_desc_ref: Option<Symbol>,
    pub fill_desc: Option<FillDesc>,
    pub fill_desc_ref: Option<Symbol>,
}

/// Standard geometric primitives
#[derive(Debug, Clone, PartialEq)]
pub enum StandardPrimitive {
    Circle(Styled<Circle>),
    RectCenter(Styled<RectCenter>),
    RectRound(Styled<RectRound>),
    RectCham(Styled<RectCham>),
    RectCorner(Styled<RectCorner>),
    Oval(Styled<Oval>),
    Butterfly(Styled<Butterfly>),
    Diamond(Styled<Diamond>),
    Donut(Styled<Donut>),
    Ellipse(Styled<Ellipse>),
    Hexagon(Styled<Hexagon>),
    Moire(Moire), // Moire doesn't have styling
    Octagon(Styled<Octagon>),
    Thermal(Styled<Thermal>),
    Triangle(Styled<Triangle>),
    Contour(Contour), // Contour has its own structure
}

/// Circle primitive defined by diameter
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Circle {
    pub diameter: f64,
}

/// Rectangle centered at origin
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RectCenter {
    pub size: Size,
}

/// Rectangle with rounded corners
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RectRound {
    pub size: Size,
    pub radius: f64,
    pub upper_right: bool,
    pub upper_left: bool,
    pub lower_right: bool,
    pub lower_left: bool,
}

/// Rectangle with chamfered corners
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RectCham {
    pub size: Size,
    pub chamfer: f64,
    pub upper_right: bool,
    pub upper_left: bool,
    pub lower_right: bool,
    pub lower_left: bool,
}

/// Rectangle defined by corner coordinates
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RectCorner {
    pub lower_left: Point,
    pub upper_right: Point,
}

/// Oval (rectangle with rounded ends)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Oval {
    pub size: Size,
}

/// Butterfly shape (round or square with 2 quadrants removed)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Butterfly {
    pub shape: ButterflyShape,
    pub size: f64,
}

ipc_enum! {
    pub enum ButterflyShape("butterflyShape") {
        Round = "ROUND",
        Square = "SQUARE",
    }
}

/// Diamond (4-sided with equal sides)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Diamond {
    pub size: Size,
}

/// Donut (concentric shapes)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Donut {
    pub shape: ConcentricShape,
    pub outer_diameter: f64,
    pub inner_diameter: f64,
}

ipc_enum! {
    /// Shape used for Donut and Thermal primitives
    pub enum ConcentricShape("concentricShape") {
        Round = "ROUND",
        Square = "SQUARE",
        Hexagon = "HEXAGON",
        Octagon = "OCTAGON",
    }
}

/// Ellipse
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ellipse {
    pub size: Size,
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
    pub shape: ConcentricShape,
    pub outer_diameter: f64,
    pub inner_diameter: f64,
    pub spoke_count: u32,
    pub spoke_width: Option<f64>,
    pub spoke_start_angle: Option<f64>,
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

/// `Polygon`, `Cutout` or `Polyline`: a begin point and the steps from it.
///
/// A zone fill is tens of thousands of straight steps, so the points are one
/// table and the few curved steps a sparse one beside it.
#[derive(Clone, PartialEq)]
pub struct Polygon {
    /// `PolyBegin`, then the end point of every step.
    pub(crate) points: Vec<Point>,
    /// The curved steps, by ascending `point`.
    pub(crate) curves: Vec<PolyCurve>,
}

/// The arc that ends at `points[point]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PolyCurve {
    pub(crate) point: u32,
    pub(crate) clockwise: bool,
    pub(crate) center: Point,
}

impl Polygon {
    pub fn new(begin: Point, steps: impl IntoIterator<Item = PolyStep>) -> Self {
        let steps = steps.into_iter();
        let mut polygon = Self {
            points: Vec::with_capacity(steps.size_hint().0 + 1),
            curves: Vec::new(),
        };
        polygon.points.push(begin);
        for step in steps {
            let point = match step {
                PolyStep::Segment(segment) => segment.point,
                PolyStep::Curve(curve) => {
                    polygon.curves.push(PolyCurve {
                        point: polygon.points.len() as u32,
                        clockwise: curve.clockwise,
                        center: curve.center,
                    });
                    curve.point
                }
            };
            polygon.points.push(point);
        }
        polygon
    }

    pub fn begin(&self) -> Point {
        self.points[0]
    }

    /// `begin`, then the end point of every step.
    pub fn points(&self) -> &[Point] {
        &self.points
    }

    pub fn steps(&self) -> impl Iterator<Item = PolyStep> + '_ {
        let mut curves = self.curves.iter().peekable();
        (1..self.points.len()).map(move |index| {
            let point = self.points[index];
            match curves.next_if(|curve| curve.point as usize == index) {
                Some(curve) => PolyStep::Curve(PolyStepCurve {
                    point,
                    center: curve.center,
                    clockwise: curve.clockwise,
                }),
                None => PolyStep::Segment(PolyStepSegment { point }),
            }
        })
    }

    pub(crate) fn translate(&mut self, offset: Point) {
        let centers = self.curves.iter_mut().map(|curve| &mut curve.center);
        for point in self.points.iter_mut().chain(centers) {
            point.x += offset.x;
            point.y += offset.y;
        }
    }
}

/// Prints the begin point and each step, however they are stored.
impl fmt::Debug for Polygon {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        struct Steps<'a>(&'a Polygon);

        impl fmt::Debug for Steps<'_> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.debug_list().entries(self.0.steps()).finish()
            }
        }

        f.debug_struct("Polygon")
            .field("begin", &self.begin())
            .field("steps", &Steps(self))
            .finish()
    }
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
    pub point: Point,
}

/// Curved arc segment in polygon
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PolyStepCurve {
    pub point: Point,
    pub center: Point,
    pub clockwise: bool,
}

/// An open run of steps, which is a [`Polygon`] that need not close.
pub type Polyline = Polygon;

/// Line segment
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Line {
    pub start: Point,
    pub end: Point,
}

/// Arc segment
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Arc {
    pub start: Point,
    pub end: Point,
    pub center: Point,
    pub clockwise: bool,
}

/// Line description (width, end style, property)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LineDesc {
    pub line_width: f64,
    pub line_end: LineEnd,
    pub line_property: Option<LineProperty>,
}

/// IPC-2581C `LineDescGroup` substitution content.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LineDescGroup {
    Inline(LineDesc),
    Ref(Symbol),
}

ipc_enum! {
    pub enum LineEnd("lineEnd") {
        None = "NONE",
        Round = "ROUND",
        Square = "SQUARE",
    }
}

ipc_enum! {
    pub enum LineProperty("lineProperty") {
        Solid = "SOLID",
        Dashed = "DASHED",
        Dotted = "DOTTED",
        Center = "CENTER",
        Phantom = "PHANTOM",
        Erase = "ERASE",
    }
}

/// Fill description (fill style and color)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FillDesc {
    pub fill_property: FillProperty,
    pub line_width: Option<f64>,
    pub pitch1: Option<f64>,
    pub pitch2: Option<f64>,
    pub angle1: Option<f64>,
    pub angle2: Option<f64>,
    pub color: Option<ColorGroup>,
}

ipc_enum! {
    pub enum FillProperty("fillProperty") {
        Fill = "FILL",
        Hollow = "HOLLOW",
        Void = "VOID",
        Hatch = "HATCH",
        Mesh = "MESH",
    }
}

/// Color (RGB)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// IPC-2581C `ColorGroup` substitution content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorGroup {
    Color(Color),
    Ref(Symbol),
    Term {
        name: Symbol,
        comment: Option<Symbol>,
    },
}

/// IPC-2581C text primitive, with dimensional coordinates normalized to millimeters.
#[derive(Debug, Clone, PartialEq)]
pub struct Text {
    pub text_string: Symbol,
    pub font_size: u32,
    /// Required source `fontSize` text, retained as source provenance.
    pub font_size_raw: Symbol,
    pub xform: Option<Xform>,
    pub bounding_box: BoundingBox,
    pub font_ref: Option<Symbol>,
    pub color: Option<ColorGroup>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoundingBox {
    pub lower_left: Point,
    pub upper_right: Point,
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
///
/// A zone fill is a `UserSpecial` of thousands of contours, so what few of
/// them carry is boxed.
#[derive(Debug, Clone, PartialEq)]
pub struct UserShape {
    pub shape: UserShapeType,
    pub line_desc: Option<LineDesc>,
    pub line_desc_ref: Option<Symbol>,
    pub fill_desc: Option<Box<FillDesc>>,
    pub fill_desc_ref: Option<Symbol>,
}

/// Types of shapes that can appear in UserSpecial: the `Feature`
/// substitution group, plus the bare `Polygon` KiCad writes.
#[derive(Debug, Clone, PartialEq)]
pub enum UserShapeType {
    Circle(Circle),
    RectCenter(RectCenter),
    Oval(Oval),
    RectRound(RectRound),
    Contour(Contour),
    /// Any standard primitive without a variant of its own above.
    StandardPrimitive(Box<StandardPrimitive>),
    StandardPrimitiveRef(Symbol),
    Polygon(Polygon),
    Line(Line),
    Arc(Arc),
    Polyline(Polyline),
    Outline(Box<super::PackageOutline>),
    Text(Box<Text>),
    UserPrimitiveRef(Symbol),
    UserPrimitive(UserPrimitive),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_polygon_yields_the_steps_it_was_made_of() {
        let point = |x, y| Point { x, y };
        let curve = |x, y, clockwise| {
            PolyStep::Curve(PolyStepCurve {
                point: point(x, y),
                center: point(x, 0.0),
                clockwise,
            })
        };
        let segment = |x, y| PolyStep::Segment(PolyStepSegment { point: point(x, y) });
        let steps = [
            curve(1.0, 1.0, true),
            segment(2.0, 0.0),
            segment(3.0, 0.0),
            curve(4.0, 1.0, false),
            curve(5.0, 1.0, true),
        ];

        let mut polygon = Polygon::new(point(0.0, 0.0), steps);
        assert_eq!(polygon.begin(), point(0.0, 0.0));
        assert_eq!(polygon.points().len(), 6);
        assert_eq!(polygon.steps().collect::<Vec<_>>(), steps);
        assert_eq!(
            format!("{:?}", Polygon::new(point(0.0, 0.0), [segment(2.0, 0.0)])),
            "Polygon { begin: Point { x: 0.0, y: 0.0 }, steps: [Segment(PolyStepSegment { point: Point { x: 2.0, y: 0.0 } })] }"
        );

        polygon.translate(point(10.0, 20.0));
        assert_eq!(polygon.begin(), point(10.0, 20.0));
        assert_eq!(
            polygon.steps().next(),
            Some(PolyStep::Curve(PolyStepCurve {
                point: point(11.0, 21.0),
                center: point(11.0, 20.0),
                clockwise: true,
            }))
        );
        assert_eq!(Polygon::new(point(1.0, 2.0), []).steps().count(), 0);
    }
}
