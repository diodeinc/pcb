use crate::types::{Attribute, Mirroring, Polarity};

#[derive(Debug, Clone)]
pub struct GeometryDocument {
    pub file_function: Vec<String>,
    pub features: Vec<GeometryFeature>,
    pub paths: Vec<GeometryPath>,
    pub contours: Vec<GeometryContour>,
    pub path_cmds: Vec<PathCmd>,
    pub bbox: BBox,
    pub diagnostics: Vec<GeometryDiagnostic>,
}

impl GeometryDocument {
    pub fn new(file_function: Vec<String>) -> Self {
        Self {
            file_function,
            features: Vec::new(),
            paths: Vec::new(),
            contours: Vec::new(),
            path_cmds: Vec::new(),
            bbox: BBox::empty(),
            diagnostics: Vec::new(),
        }
    }

    pub fn push_feature(&mut self, mut feature: GeometryFeature, paths: Vec<PathPayload>) {
        let path_start = self.paths.len() as u32;
        for payload in paths {
            self.push_path(payload.path, payload.contours);
        }
        feature.path_start = path_start;
        feature.path_count = self.paths.len() as u32 - path_start;
        self.features.push(feature);
    }

    pub fn push_path(&mut self, mut path: GeometryPath, contours: Vec<ContourPayload>) -> u32 {
        let contour_start = self.contours.len() as u32;
        let mut bbox = BBox::empty();
        for contour in contours {
            bbox = bbox.union(contour.bbox);
            self.push_contour(contour);
        }
        path.contour_start = contour_start;
        path.contour_count = self.contours.len() as u32 - contour_start;
        path.bbox = bbox;
        let id = self.paths.len() as u32;
        self.paths.push(path);
        id
    }

    fn push_contour(&mut self, contour: ContourPayload) {
        let cmd_start = self.path_cmds.len() as u32;
        self.path_cmds.extend(contour.cmds);
        self.contours.push(GeometryContour {
            cmd_start,
            cmd_count: self.path_cmds.len() as u32 - cmd_start,
            bbox: contour.bbox,
        });
    }

    pub fn warn(&mut self, message: impl Into<String>) {
        self.diagnostics.push(GeometryDiagnostic {
            severity: DiagnosticSeverity::Warning,
            message: message.into(),
        });
    }
}

#[derive(Debug, Clone)]
pub struct GeometryFeature {
    pub kind: FeatureKind,
    pub bucket: FeatureBucket,
    pub polarity: Polarity,
    pub path_start: u32,
    pub path_count: u32,
    pub bbox: BBox,
    pub aperture: Option<i32>,
    pub object_index: u32,
    pub aperture_attributes: Vec<Attribute>,
    pub object_attributes: Vec<Attribute>,
    pub mirroring: Mirroring,
    pub rotation_degrees: f64,
    pub scaling: f64,
}

impl GeometryFeature {
    pub fn new(kind: FeatureKind, bucket: FeatureBucket, polarity: Polarity) -> Self {
        Self {
            kind,
            bucket,
            polarity,
            path_start: 0,
            path_count: 0,
            bbox: BBox::empty(),
            aperture: None,
            object_index: 0,
            aperture_attributes: Vec::new(),
            object_attributes: Vec::new(),
            mirroring: Mirroring::None,
            rotation_degrees: 0.0,
            scaling: 1.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PathPayload {
    pub path: GeometryPath,
    pub contours: Vec<ContourPayload>,
}

#[derive(Debug, Clone)]
pub struct ContourPayload {
    pub bbox: BBox,
    pub cmds: Vec<PathCmd>,
}

#[derive(Debug, Clone)]
pub struct GeometryPath {
    pub contour_start: u32,
    pub contour_count: u32,
    pub bbox: BBox,
    pub polarity: Polarity,
    pub fill_rule: FillRule,
    pub stroke_width: f64,
    pub line_cap: LineCap,
    pub flags: PathFlags,
}

impl GeometryPath {
    pub fn filled(fill_rule: FillRule) -> Self {
        Self::filled_with_polarity(fill_rule, Polarity::Dark)
    }

    pub fn filled_with_polarity(fill_rule: FillRule, polarity: Polarity) -> Self {
        Self {
            contour_start: 0,
            contour_count: 0,
            bbox: BBox::empty(),
            polarity,
            fill_rule,
            stroke_width: 0.0,
            line_cap: LineCap::Round,
            flags: PathFlags {
                filled: true,
                stroked: false,
            },
        }
    }

    pub fn stroked(width: f64, line_cap: LineCap) -> Self {
        Self {
            contour_start: 0,
            contour_count: 0,
            bbox: BBox::empty(),
            polarity: Polarity::Dark,
            fill_rule: FillRule::NonZero,
            stroke_width: width,
            line_cap,
            flags: PathFlags {
                filled: false,
                stroked: true,
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct GeometryContour {
    pub cmd_start: u32,
    pub cmd_count: u32,
    pub bbox: BBox,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PathCmd {
    pub op: PathOp,
    pub p0: Point,
    pub p1: Point,
    pub clockwise: bool,
}

impl PathCmd {
    pub fn move_to(p: Point) -> Self {
        Self {
            op: PathOp::MoveTo,
            p0: p,
            ..Self::default()
        }
    }
    pub fn line_to(p: Point) -> Self {
        Self {
            op: PathOp::LineTo,
            p0: p,
            ..Self::default()
        }
    }
    pub fn arc_to(end: Point, center: Point, clockwise: bool) -> Self {
        Self {
            op: PathOp::ArcTo,
            p0: end,
            p1: center,
            clockwise,
        }
    }
    pub fn close() -> Self {
        Self {
            op: PathOp::Close,
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PathOp {
    #[default]
    MoveTo,
    LineTo,
    ArcTo,
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeatureKind {
    Flash,
    Draw,
    Arc,
    Region,
    Composite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeatureBucket {
    Pad,
    Trace,
    Fill,
    Cutout,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FillRule {
    NonZero,
    EvenOdd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineCap {
    Round,
    Square,
    Butt,
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
            self
        } else {
            Self {
                min: Point::new(self.min.x - amount, self.min.y - amount),
                max: Point::new(self.max.x + amount, self.max.y + amount),
            }
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
    pub fn placement(
        center: Point,
        rotation_degrees: f64,
        mirroring: Mirroring,
        scale: f64,
    ) -> Self {
        let sx = match mirroring {
            Mirroring::X | Mirroring::XY => -scale,
            _ => scale,
        };
        let sy = match mirroring {
            Mirroring::Y | Mirroring::XY => -scale,
            _ => scale,
        };
        let r = rotation_degrees.to_radians();
        let (sin, cos) = r.sin_cos();
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

    pub fn determinant(&self) -> f64 {
        self.m00 * self.m11 - self.m01 * self.m10
    }
}
