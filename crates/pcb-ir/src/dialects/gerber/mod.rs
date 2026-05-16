use crate::common::*;
use crate::dialects::{geom, path as common_path};

#[derive(Debug, Clone)]
pub struct GeometryDocument<Attribute = ()> {
    pub file_function: Vec<String>,
    pub features: Vec<GeometryFeature<Attribute>>,
    pub paths: Vec<GeometryPath>,
    pub contours: Vec<GeometryContour>,
    pub path_cmds: Vec<PathCmd>,
    pub bbox: BBox,
    pub diagnostics: Vec<GeometryDiagnostic>,
}

impl<Attribute> GeometryDocument<Attribute> {
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

    pub fn push_feature(
        &mut self,
        mut feature: GeometryFeature<Attribute>,
        paths: Vec<PathPayload>,
    ) {
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

pub fn lower_to_geom<Attribute: Clone>(
    doc: &GeometryDocument<Attribute>,
) -> geom::GeomDocument<Vec<String>, Vec<Attribute>> {
    let mut geom = geom::GeomDocument::new(Unit::Millimeter);
    let layer = geom.push_layer(geom::GeomLayer {
        name: doc.file_function.join(", "),
        role: layer_role(&doc.file_function),
        side: layer_side(&doc.file_function),
        object_start: 0,
        object_count: 0,
        bbox: doc.bbox,
        meta: doc.file_function.clone(),
    });

    for feature in &doc.features {
        for path in &doc.paths
            [feature.path_start as usize..(feature.path_start + feature.path_count) as usize]
        {
            let geom_path = if path.flags.filled {
                geom::GeomPath::filled(path.fill_rule)
            } else if path.flags.stroked {
                geom::GeomPath::stroked(path.stroke_width, path.line_cap, LineJoin::Round)
            } else {
                continue;
            };
            let path_id = geom.push_path(geom_path, path_payloads(doc, path));
            geom.push_object(
                layer,
                geom::GeomObject {
                    paint: paint_polarity(path.polarity),
                    path: path_id,
                    bbox: path.bbox,
                    meta: feature.object_attributes.clone(),
                },
            );
        }
    }

    geom.diagnostics.extend(doc.diagnostics.clone());
    geom
}

fn path_payloads<Attribute>(
    doc: &GeometryDocument<Attribute>,
    path: &GeometryPath,
) -> Vec<common_path::PathPayload> {
    doc.contours[path.contour_start as usize..(path.contour_start + path.contour_count) as usize]
        .iter()
        .map(|contour| common_path::PathPayload {
            bbox: contour.bbox,
            cmds: doc.path_cmds
                [contour.cmd_start as usize..(contour.cmd_start + contour.cmd_count) as usize]
                .iter()
                .map(path_cmd)
                .collect(),
        })
        .collect()
}

fn path_cmd(cmd: &PathCmd) -> common_path::PathCmd {
    match cmd.op {
        PathOp::MoveTo => common_path::PathCmd::move_to(cmd.p0),
        PathOp::LineTo => common_path::PathCmd::line_to(cmd.p0),
        PathOp::ArcTo => common_path::PathCmd::arc_to(cmd.p0, cmd.p1, cmd.clockwise),
        PathOp::Close => common_path::PathCmd::close(),
    }
}

fn paint_polarity(polarity: Polarity) -> PaintPolarity {
    match polarity {
        Polarity::Dark => PaintPolarity::Dark,
        Polarity::Clear => PaintPolarity::Clear,
    }
}

fn layer_role(file_function: &[String]) -> LayerRole {
    match file_function.first().map(String::as_str) {
        Some("Copper") => LayerRole::Copper,
        Some("Soldermask") => LayerRole::Soldermask,
        Some("Paste") => LayerRole::Paste,
        Some("Legend") => LayerRole::Legend,
        Some("Profile") => LayerRole::Profile,
        _ => LayerRole::Other,
    }
}

fn layer_side(file_function: &[String]) -> Side {
    if file_function.iter().any(|field| field == "Top") {
        Side::Top
    } else if file_function.iter().any(|field| field == "Bot") {
        Side::Bottom
    } else if file_function.iter().any(|field| field == "Inr") {
        Side::Inner
    } else {
        Side::None
    }
}

#[derive(Debug, Clone)]
pub struct GeometryFeature<Attribute = ()> {
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

impl<Attribute> GeometryFeature<Attribute> {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Polarity {
    Dark,
    Clear,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mirroring {
    None,
    X,
    Y,
    XY,
}

impl MirrorAxes for Mirroring {
    fn mirror_x(self) -> bool {
        matches!(self, Self::X | Self::XY)
    }

    fn mirror_y(self) -> bool {
        matches!(self, Self::Y | Self::XY)
    }
}

pub mod compare;
pub mod process;
pub mod raster;
pub mod svg;
pub mod terminal;
