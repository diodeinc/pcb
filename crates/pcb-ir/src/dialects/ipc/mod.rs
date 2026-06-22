use crate::common::*;
pub use crate::dialects::path::{PathCmd, PathOp};
use crate::dialects::{geom, path as common_path};

#[derive(Debug, Clone)]
pub struct GeometryDocument<Symbol, LayerFunction> {
    pub board_name: String,
    pub layout: LayoutGraph<Symbol>,
    pub layers: Vec<GeometryLayer<Symbol, LayerFunction>>,
    pub boards: Vec<BoardGeometry<Symbol>>,
    pub panels: Vec<PanelGeometry<Symbol>>,
    pub board_instances: Vec<BoardInstance<Symbol>>,
    pub profiles: Vec<BoardProfile<Symbol>>,
    pub profile_cutouts: Vec<BoardProfileCutout>,
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
            layout: LayoutGraph::new(),
            layers: Vec::new(),
            boards: Vec::new(),
            panels: Vec::new(),
            board_instances: Vec::new(),
            profiles: Vec::new(),
            profile_cutouts: Vec::new(),
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
        contours: impl IntoIterator<Item = impl Into<common_path::PathPayload>>,
    ) -> u32 {
        let contour_start = self.contours.len() as u32;
        let mut path_bbox = BBox::empty();
        for contour in contours {
            let contour = contour.into();
            path_bbox = path_bbox.union(contour.bbox);
            self.push_contour(contour.bbox, contour.cmds);
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

const PROFILE_STROKE_WIDTH: f64 = 0.1;

pub fn profiles_bbox<Symbol, LayerFunction>(
    doc: &GeometryDocument<Symbol, LayerFunction>,
) -> Option<BBox> {
    render_profiles(doc)
        .map(|profile| profile.bbox)
        .reduce(BBox::union)
        .filter(|bbox| !bbox.is_empty())
}

pub fn board_bbox<Symbol, LayerFunction>(
    doc: &GeometryDocument<Symbol, LayerFunction>,
) -> Option<BBox> {
    doc.boards
        .iter()
        .map(|board| board.bbox)
        .find(|bbox| !bbox.is_empty())
}

pub fn panel_bbox<Symbol, LayerFunction>(
    doc: &GeometryDocument<Symbol, LayerFunction>,
) -> Option<BBox> {
    doc.panels
        .iter()
        .map(|panel| panel.bbox)
        .find(|bbox| !bbox.is_empty())
}

pub fn lower_layer_to_geom<Symbol: Clone, LayerFunction: Clone>(
    doc: &GeometryDocument<Symbol, LayerFunction>,
    layer_index: usize,
    role: LayerRole,
    side: Side,
) -> geom::GeomDocument<LayerFunction, Option<Symbol>> {
    let mut geom = geom::GeomDocument::new(Unit::Millimeter);
    let layer = &doc.layers[layer_index];
    let geom_layer = geom.push_layer(geom::GeomLayer {
        name: layer.name.clone(),
        role,
        side,
        object_start: 0,
        object_count: 0,
        bbox: layer.bbox,
        meta: layer.layer_function.clone(),
    });

    for feature in &doc.features
        [layer.feature_start as usize..(layer.feature_start + layer.feature_count) as usize]
    {
        for path in &doc.paths
            [feature.path_start as usize..(feature.path_start + feature.path_count) as usize]
        {
            let Some(geom_path) = lower_path_kind(path) else {
                continue;
            };
            let path_id = geom.push_path(geom_path, path_payloads(doc, path));
            geom.push_object(
                geom_layer,
                geom::GeomObject {
                    paint: paint_polarity(feature.polarity),
                    path: path_id,
                    bbox: path.bbox,
                    meta: feature.net.clone(),
                },
            );
        }
    }

    geom.diagnostics.extend(doc.diagnostics.clone());
    geom
}

pub fn lower_layer_with_profiles_to_geom<Symbol: Clone, LayerFunction: Clone>(
    doc: &GeometryDocument<Symbol, LayerFunction>,
    layer_index: usize,
    role: LayerRole,
    side: Side,
) -> geom::GeomDocument<LayerFunction, Option<Symbol>> {
    let mut geom = lower_layer_to_geom(doc, layer_index, role, side);
    let layer = &doc.layers[layer_index];
    let outline_layer = geom.push_layer(geom::GeomLayer {
        name: "Profile".to_string(),
        role: LayerRole::Profile,
        side: Side::None,
        object_start: 0,
        object_count: 0,
        bbox: BBox::empty(),
        meta: layer.layer_function.clone(),
    });

    for profile in render_profiles(doc) {
        push_profile_path_to_geom(&mut geom, outline_layer, doc, profile.outer_path);
        for cutout in &doc.profile_cutouts
            [profile.cutout_start as usize..(profile.cutout_start + profile.cutout_count) as usize]
        {
            push_profile_path_to_geom(&mut geom, outline_layer, doc, cutout.path);
        }
    }

    geom
}

pub fn render_profiles<Symbol, LayerFunction>(
    doc: &GeometryDocument<Symbol, LayerFunction>,
) -> impl Iterator<Item = &BoardProfile<Symbol>> {
    let has_panel = !doc.panels.is_empty();
    doc.profiles.iter().filter(move |profile| {
        if has_panel {
            matches!(
                profile.kind,
                BoardProfileKind::PanelDefinition
                    | BoardProfileKind::PanelInstance
                    | BoardProfileKind::BoardInstance
                    | BoardProfileKind::StepInstance
            )
        } else {
            matches!(profile.kind, BoardProfileKind::BoardDefinition)
        }
    })
}

fn push_profile_path_to_geom<Symbol, LayerFunction>(
    geom: &mut geom::GeomDocument<LayerFunction, Option<Symbol>>,
    layer_id: u32,
    doc: &GeometryDocument<Symbol, LayerFunction>,
    path_index: u32,
) {
    let path = &doc.paths[path_index as usize];
    let path_id = geom.push_path(
        geom::GeomPath::stroked(PROFILE_STROKE_WIDTH, LineCap::Round, LineJoin::Round),
        path_payloads(doc, path),
    );
    geom.push_object(
        layer_id,
        geom::GeomObject {
            paint: PaintPolarity::Dark,
            path: path_id,
            bbox: path.bbox,
            meta: None,
        },
    );
    geom.layers[layer_id as usize].bbox = geom.layers[layer_id as usize].bbox.union(path.bbox);
}

fn lower_path_kind(path: &GeometryPath) -> Option<geom::GeomPath> {
    if path.flags.filled {
        Some(geom::GeomPath::filled(path.fill_rule))
    } else if path.flags.stroked {
        Some(geom::GeomPath::stroked(
            path.stroke_width,
            path.line_cap,
            LineJoin::Round,
        ))
    } else {
        None
    }
}

fn path_payloads<Symbol, LayerFunction>(
    doc: &GeometryDocument<Symbol, LayerFunction>,
    path: &GeometryPath,
) -> Vec<common_path::PathPayload> {
    doc.contours[path.contour_start as usize..(path.contour_start + path.contour_count) as usize]
        .iter()
        .map(|contour| common_path::PathPayload {
            bbox: contour.bbox,
            cmds: doc.path_cmds
                [contour.cmd_start as usize..(contour.cmd_start + contour.cmd_count) as usize]
                .to_vec(),
        })
        .collect()
}

fn paint_polarity(polarity: GeometryPolarity) -> PaintPolarity {
    match polarity {
        GeometryPolarity::Positive => PaintPolarity::Dark,
        GeometryPolarity::Negative => PaintPolarity::Clear,
    }
}

#[derive(Debug, Clone)]
pub struct LayoutGraph<Symbol> {
    pub root_step: Option<u32>,
    pub steps: Vec<LayoutStep<Symbol>>,
    pub repeats: Vec<LayoutRepeat<Symbol>>,
    pub instances: Vec<LayoutInstance<Symbol>>,
}

impl<Symbol> LayoutGraph<Symbol> {
    pub fn new() -> Self {
        Self {
            root_step: None,
            steps: Vec::new(),
            repeats: Vec::new(),
            instances: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.root_step.is_none()
            && self.steps.is_empty()
            && self.repeats.is_empty()
            && self.instances.is_empty()
    }
}

impl<Symbol> Default for LayoutGraph<Symbol> {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
pub struct LayoutStep<Symbol> {
    pub source_step_ref: Symbol,
    pub kind: LayoutStepKind,
    pub datum: Point,
    pub profile_start: u32,
    pub profile_count: u32,
    pub bbox: BBox,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutStepKind {
    Board,
    Panel,
    Coupon,
    Tooling,
    Ic,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct LayoutRepeat<Symbol> {
    pub parent_step: u32,
    pub parent_instance: Option<u32>,
    pub child_step: u32,
    pub source_step_ref: Symbol,
    pub x: f64,
    pub y: f64,
    pub nx: u32,
    pub ny: u32,
    pub dx: f64,
    pub dy: f64,
    pub angle: f64,
    pub mirror: bool,
    pub instance_start: u32,
    pub instance_count: u32,
    pub bbox: BBox,
}

#[derive(Debug, Clone)]
pub struct LayoutInstance<Symbol> {
    pub repeat: u32,
    pub parent_instance: Option<u32>,
    pub child_step: u32,
    pub source_step_ref: Symbol,
    pub parent_step_ref: Symbol,
    pub transform: Affine2,
    pub repeat_index_x: u32,
    pub repeat_index_y: u32,
    pub repeat_count_x: u32,
    pub repeat_count_y: u32,
    pub repeat_pitch_x: f64,
    pub repeat_pitch_y: f64,
    pub profile_start: u32,
    pub profile_count: u32,
    pub bbox: BBox,
}

#[derive(Debug, Clone)]
pub struct BoardGeometry<Symbol> {
    pub step_ref: Symbol,
    pub profile_start: u32,
    pub profile_count: u32,
    pub bbox: BBox,
}

#[derive(Debug, Clone)]
pub struct PanelGeometry<Symbol> {
    pub step_ref: Symbol,
    pub profile_start: u32,
    pub profile_count: u32,
    pub profile_bbox: BBox,
    pub board_instance_start: u32,
    pub board_instance_count: u32,
    pub instance_bbox: BBox,
    pub bbox: BBox,
}

#[derive(Debug, Clone)]
pub struct BoardInstance<Symbol> {
    pub board_index: u32,
    pub source_step_ref: Symbol,
    pub parent_step_ref: Symbol,
    pub transform: Affine2,
    pub repeat_index_x: u32,
    pub repeat_index_y: u32,
    pub repeat_count_x: u32,
    pub repeat_count_y: u32,
    pub repeat_pitch_x: f64,
    pub repeat_pitch_y: f64,
    pub profile_start: u32,
    pub profile_count: u32,
    pub bbox: BBox,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoardProfileKind {
    BoardDefinition,
    PanelDefinition,
    StepDefinition,
    BoardInstance,
    PanelInstance,
    StepInstance,
}

#[derive(Debug, Clone)]
pub struct BoardProfile<Symbol> {
    pub kind: BoardProfileKind,
    pub source_step_ref: Symbol,
    pub transform: Affine2,
    pub outer_path: u32,
    pub cutout_start: u32,
    pub cutout_count: u32,
    pub bbox: BBox,
}

#[derive(Debug, Clone)]
pub struct BoardProfileCutout {
    pub path: u32,
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
    pub source_layer_ref: Option<Symbol>,
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
            source_layer_ref: None,
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

    pub fn unpainted(bbox: BBox) -> Self {
        Self {
            contour_start: 0,
            contour_count: 0,
            bbox,
            fill_rule: FillRule::NonZero,
            stroke_width: 0.0,
            line_cap: LineCap::Round,
            flags: PathFlags::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct GeometryContour {
    pub cmd_start: u32,
    pub cmd_count: u32,
    pub bbox: BBox,
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
