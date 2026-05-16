#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LineCap {
    Round,
    Square,
    Butt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LineJoin {
    Round,
    Miter,
    Bevel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FillRule {
    NonZero,
    EvenOdd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Unit {
    Millimeter,
    Inch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    Top,
    Bottom,
    Inner,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LayerRole {
    Copper,
    Soldermask,
    Paste,
    Legend,
    Profile,
    Drill,
    Mechanical,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PaintPolarity {
    Dark,
    Clear,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PathFlags {
    pub filled: bool,
    pub stroked: bool,
}

#[derive(Debug, Clone)]
pub struct GeometryDiagnostic {
    pub severity: DiagnosticSeverity,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticSeverity {
    Warning,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    pub fn distance_to(self, other: Point) -> f64 {
        ((self.x - other.x).powi(2) + (self.y - other.y).powi(2)).sqrt()
    }

    pub fn angle_from(self, center: Point) -> f64 {
        (self.y - center.y).atan2(self.x - center.x)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BBox {
    pub min: Point,
    pub max: Point,
}

impl BBox {
    pub fn empty() -> Self {
        Self {
            min: Point::new(f64::INFINITY, f64::INFINITY),
            max: Point::new(f64::NEG_INFINITY, f64::NEG_INFINITY),
        }
    }

    pub fn from_point(p: Point) -> Self {
        Self { min: p, max: p }
    }

    pub fn include_point(&mut self, p: Point) {
        self.min.x = self.min.x.min(p.x);
        self.min.y = self.min.y.min(p.y);
        self.max.x = self.max.x.max(p.x);
        self.max.y = self.max.y.max(p.y);
    }

    pub fn include_circular_arc(
        &mut self,
        start: Point,
        end: Point,
        center: Point,
        clockwise: bool,
    ) {
        self.include_point(start);
        self.include_point(end);

        let radius = start.distance_to(center).max(end.distance_to(center));
        if radius <= 0.0 {
            return;
        }

        let start_angle = start.angle_from(center);
        let end_angle = end.angle_from(center);
        for angle in [
            0.0,
            std::f64::consts::FRAC_PI_2,
            std::f64::consts::PI,
            std::f64::consts::PI * 1.5,
        ] {
            if angle_is_on_arc(start_angle, end_angle, angle, clockwise) {
                self.include_point(Point::new(
                    center.x + radius * angle.cos(),
                    center.y + radius * angle.sin(),
                ));
            }
        }
    }

    pub fn union(mut self, other: BBox) -> Self {
        if other.is_empty() {
            return self;
        }
        self.include_point(other.min);
        self.include_point(other.max);
        self
    }

    pub fn expand(self, amount: f64) -> Self {
        if self.is_empty() {
            return self;
        }
        Self {
            min: Point::new(self.min.x - amount, self.min.y - amount),
            max: Point::new(self.max.x + amount, self.max.y + amount),
        }
    }

    pub fn width(&self) -> f64 {
        self.max.x - self.min.x
    }

    pub fn height(&self) -> f64 {
        self.max.y - self.min.y
    }

    pub fn is_empty(&self) -> bool {
        self.min.x.is_infinite() || self.min.y.is_infinite()
    }
}

impl Default for BBox {
    fn default() -> Self {
        Self::empty()
    }
}

pub fn arc_sweep_radians(start: Point, end: Point, center: Point, clockwise: bool) -> f64 {
    if start.distance_to(end) <= 1e-9 && start.distance_to(center) > 1e-9 {
        return std::f64::consts::TAU;
    }

    let start_angle = start.angle_from(center);
    let end_angle = end.angle_from(center);
    if clockwise {
        normalize_angle(start_angle - end_angle)
    } else {
        normalize_angle(end_angle - start_angle)
    }
}

fn angle_is_on_arc(start: f64, end: f64, angle: f64, clockwise: bool) -> bool {
    if normalize_angle(end - start) <= 1e-12 {
        return true;
    }

    if clockwise {
        normalize_angle(start - angle) <= normalize_angle(start - end) + 1e-12
    } else {
        normalize_angle(angle - start) <= normalize_angle(end - start) + 1e-12
    }
}

fn normalize_angle(angle: f64) -> f64 {
    angle.rem_euclid(std::f64::consts::TAU)
}

pub trait MirrorAxes: Copy {
    fn mirror_x(self) -> bool;
    fn mirror_y(self) -> bool;
}

impl MirrorAxes for bool {
    fn mirror_x(self) -> bool {
        self
    }

    fn mirror_y(self) -> bool {
        false
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Affine2 {
    pub m00: f64,
    pub m01: f64,
    pub m02: f64,
    pub m10: f64,
    pub m11: f64,
    pub m12: f64,
}

impl Affine2 {
    pub fn identity() -> Self {
        Self {
            m00: 1.0,
            m01: 0.0,
            m02: 0.0,
            m10: 0.0,
            m11: 1.0,
            m12: 0.0,
        }
    }

    pub fn placement<M: MirrorAxes>(
        center: Point,
        rotation_degrees: f64,
        mirror: M,
        scale: f64,
    ) -> Self {
        let sx = if mirror.mirror_x() { -scale } else { scale };
        let sy = if mirror.mirror_y() { -scale } else { scale };
        let radians = rotation_degrees.to_radians();
        let cos = radians.cos();
        let sin = radians.sin();

        Self {
            m00: cos * sx,
            m01: -sin * sy,
            m02: center.x,
            m10: sin * sx,
            m11: cos * sy,
            m12: center.y,
        }
    }

    pub fn transform_point(&self, p: Point) -> Point {
        Point::new(
            self.m00 * p.x + self.m01 * p.y + self.m02,
            self.m10 * p.x + self.m11 * p.y + self.m12,
        )
    }

    pub fn concat(&self, child: Self) -> Self {
        Self {
            m00: self.m00 * child.m00 + self.m01 * child.m10,
            m01: self.m00 * child.m01 + self.m01 * child.m11,
            m02: self.m00 * child.m02 + self.m01 * child.m12 + self.m02,
            m10: self.m10 * child.m00 + self.m11 * child.m10,
            m11: self.m10 * child.m01 + self.m11 * child.m11,
            m12: self.m10 * child.m02 + self.m11 * child.m12 + self.m12,
        }
    }

    pub fn determinant(&self) -> f64 {
        self.m00 * self.m11 - self.m01 * self.m10
    }

    pub fn transform_vector(rotation_degrees: f64, mirror: bool, scale: f64, p: Point) -> Point {
        let tx = if mirror { -p.x * scale } else { p.x * scale };
        let ty = p.y * scale;
        let radians = rotation_degrees.to_radians();
        let cos = radians.cos();
        let sin = radians.sin();
        Point::new(tx * cos - ty * sin, tx * sin + ty * cos)
    }
}
