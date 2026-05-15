use crate::common::*;

#[derive(Debug, Clone)]
pub struct GeometryDocument<Symbol, LayerFunction> {
    pub board_name: String,
    pub layers: Vec<GeometryLayer<Symbol, LayerFunction>>,
    pub board_outlines: Vec<BoardOutline>,
    pub features: Vec<GeometryFeature<Symbol>>,
    pub paths: Vec<GeometryPath>,
    pub contours: Vec<GeometryContour>,
    pub path_cmds: Vec<PathCmd>,
    pub diagnostics: Vec<GeometryDiagnostic>,
}

impl<Symbol, LayerFunction> GeometryDocument<Symbol, LayerFunction> {
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
pub struct GeometryLayer<Symbol, LayerFunction> {
    pub name: String,
    pub source_layer_ref: Symbol,
    pub layer_function: LayerFunction,
    pub feature_start: u32,
    pub feature_count: u32,
    pub bbox: BBox,
}

#[derive(Debug, Clone)]
pub struct GeometryFeature<Symbol> {
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

impl<Symbol> GeometryFeature<Symbol> {
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

#[derive(Debug, Clone, Copy, Default)]
pub struct FeatureFlags {
    pub expanded_padstack: bool,
    pub lowered_to_paths: bool,
    pub clears_previous_in_set: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SourceRef {
    pub set_index: u32,
    pub feature_index: u32,
}

pub mod process;
pub mod raster;
pub mod svg;
pub mod terminal;
