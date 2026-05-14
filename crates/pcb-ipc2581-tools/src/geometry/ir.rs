use ipc2581::Symbol;
use ipc2581::types::LayerFunction;

#[derive(Debug, Clone)]
pub struct GeometryDocument {
    pub board_name: String,
    pub layers: Vec<GeometryLayer>,
    pub board_outlines: Vec<BoardOutline>,
    pub features: Vec<GeometryFeature>,
    pub paths: Vec<GeometryPath>,
    pub contours: Vec<GeometryContour>,
    pub path_cmds: Vec<PathCmd>,
    pub diagnostics: Vec<GeometryDiagnostic>,
}

impl GeometryDocument {
    pub fn new(board_name: String) -> Self {
        Self {
            board_name,
            layers: Vec::new(),
            board_outlines: Vec::new(),
            features: Vec::new(),
            paths: Vec::new(),
            contours: Vec::new(),
            path_cmds: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    pub fn push_path(
        &mut self,
        path: GeometryPath,
        cmds: impl IntoIterator<Item = PathCmd>,
    ) -> u32 {
        let contour_start = self.contours.len() as u32;
        let bbox = path.bbox;
        self.push_contour(bbox, cmds);

        let mut path = path;
        path.contour_start = contour_start;
        path.contour_count = 1;

        let path_id = self.paths.len() as u32;
        self.paths.push(path);
        path_id
    }

    pub fn push_compound_path(
        &mut self,
        mut path: GeometryPath,
        contours: impl IntoIterator<Item = (BBox, Vec<PathCmd>)>,
    ) -> u32 {
        let contour_start = self.contours.len() as u32;
        let mut path_bbox = BBox::empty();
        for (bbox, cmds) in contours {
            path_bbox = path_bbox.union(bbox);
            self.push_contour(bbox, cmds);
        }
        let contour_count = self.contours.len() as u32 - contour_start;

        path.contour_start = contour_start;
        path.contour_count = contour_count;
        path.bbox = path_bbox;

        let path_id = self.paths.len() as u32;
        self.paths.push(path);
        path_id
    }

    fn push_contour(&mut self, bbox: BBox, cmds: impl IntoIterator<Item = PathCmd>) -> u32 {
        let cmd_start = self.path_cmds.len() as u32;
        self.path_cmds.extend(cmds);
        let cmd_count = self.path_cmds.len() as u32 - cmd_start;

        let contour_id = self.contours.len() as u32;
        self.contours.push(GeometryContour {
            cmd_start,
            cmd_count,
            bbox,
        });
        contour_id
    }

    pub fn warn(&mut self, message: impl Into<String>) {
        self.diagnostics.push(GeometryDiagnostic {
            severity: DiagnosticSeverity::Warning,
            message: message.into(),
        });
    }
}

#[derive(Debug, Clone)]
pub struct BoardOutline {
    pub path_start: u32,
    pub path_count: u32,
    pub bbox: BBox,
}

#[derive(Debug, Clone)]
pub struct GeometryLayer {
    pub name: String,
    pub source_layer_ref: Symbol,
    pub layer_function: LayerFunction,
    pub feature_start: u32,
    pub feature_count: u32,
    pub bbox: BBox,
}

#[derive(Debug, Clone)]
pub struct GeometryFeature {
    pub kind: FeatureKind,
    pub bucket: FeatureBucket,
    pub polarity: GeometryPolarity,
    pub net: Option<Symbol>,
    pub source: SourceRef,
    pub transform: Affine2,
    pub bbox: BBox,
    pub path_start: u32,
    pub path_count: u32,

    pub center: Point,
    pub width: f64,
    pub height: f64,
    pub radius: f64,
    pub outer_diameter: f64,
    pub inner_diameter: f64,
    pub stroke_width: f64,
    pub rotation_degrees: f64,
    pub scale: f64,

    pub line_cap: LineCap,
    pub fill_rule: FillRule,
    pub padstack_ref: Option<Symbol>,
    pub primitive_ref: Option<Symbol>,
    pub flags: FeatureFlags,
}

impl GeometryFeature {
    pub fn new(kind: FeatureKind, bucket: FeatureBucket, polarity: GeometryPolarity) -> Self {
        Self {
            kind,
            bucket,
            polarity,
            net: None,
            source: SourceRef::default(),
            transform: Affine2::identity(),
            bbox: BBox::empty(),
            path_start: 0,
            path_count: 0,
            center: Point::default(),
            width: 0.0,
            height: 0.0,
            radius: 0.0,
            outer_diameter: 0.0,
            inner_diameter: 0.0,
            stroke_width: 0.0,
            rotation_degrees: 0.0,
            scale: 1.0,
            line_cap: LineCap::Round,
            fill_rule: FillRule::NonZero,
            padstack_ref: None,
            primitive_ref: None,
            flags: FeatureFlags::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct GeometryPath {
    pub contour_start: u32,
    pub contour_count: u32,
    pub bbox: BBox,
    pub fill_rule: FillRule,
    pub stroke_width: f64,
    pub line_cap: LineCap,
    pub flags: PathFlags,
}

impl GeometryPath {
    pub fn filled(fill_rule: FillRule, bbox: BBox) -> Self {
        Self {
            contour_start: 0,
            contour_count: 0,
            bbox,
            fill_rule,
            stroke_width: 0.0,
            line_cap: LineCap::Round,
            flags: PathFlags {
                filled: true,
                stroked: false,
            },
        }
    }

    pub fn stroked(width: f64, line_cap: LineCap, bbox: BBox) -> Self {
        Self {
            contour_start: 0,
            contour_count: 0,
            bbox,
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

#[derive(Debug, Clone, Copy, Default)]
pub struct PathCmd {
    pub op: PathOp,
    pub p0: Point,
    pub p1: Point,
    pub p2: Point,
    pub p3: Point,
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

    pub fn cubic_to(p1: Point, p2: Point, p3: Point) -> Self {
        Self {
            op: PathOp::CubicTo,
            p0: p1,
            p1: p2,
            p2: p3,
            ..Self::default()
        }
    }

    pub fn arc_to(end: Point, center: Point, clockwise: bool) -> Self {
        Self {
            op: PathOp::ArcTo,
            p0: end,
            p1: center,
            clockwise,
            ..Self::default()
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
    CubicTo,
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeatureKind {
    Hole,
    Padstack,
    Primitive,
    Polygon,
    Slot,
    Trace,
    FlattenedBucket,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeatureBucket {
    Smd,
    Pth,
    Via,
    Trace,
    Fill,
    Cutout,
    Thermal,
    Antipad,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GeometryPolarity {
    Positive,
    Negative,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineCap {
    Round,
    Square,
    Butt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FillRule {
    NonZero,
    EvenOdd,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct FeatureFlags {
    pub expanded_padstack: bool,
    pub lowered_to_paths: bool,
    pub clears_previous_in_set: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PathFlags {
    pub filled: bool,
    pub stroked: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SourceRef {
    pub set_index: u32,
    pub feature_index: u32,
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

    pub fn placement(center: Point, rotation_degrees: f64, mirror: bool, scale: f64) -> Self {
        let mirror_scale = if mirror { -scale } else { scale };
        let radians = rotation_degrees.to_radians();
        let cos = radians.cos();
        let sin = radians.sin();

        Self {
            m00: cos * mirror_scale,
            m01: -sin * scale,
            m02: center.x,
            m10: sin * mirror_scale,
            m11: cos * scale,
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
