use crate::common::*;
pub use crate::dialects::path::{PathCmd, PathOp};
use crate::dialects::{artwork, path as common_path};

#[derive(Debug, Clone)]
pub struct GeometryDocument<Symbol, LayerFunction> {
    pub layout: LayoutGraph<Symbol>,
    pub layers: Vec<GeometryLayer<Symbol, LayerFunction>>,
    pub profiles: Vec<StepProfile>,
    pub profile_cutouts: Vec<StepProfileCutout>,
    pub specs: Vec<IpcSpec<Symbol>>,
    pub spec_items: Vec<IpcSpecItem<Symbol>>,
    pub spec_properties: Vec<IpcSpecProperty<Symbol>>,
    pub spec_refs: Vec<IpcSpecRef<Symbol>>,
    pub feature_sets: Vec<GeometryFeatureSet<Symbol>>,
    pub features: Vec<GeometryFeature<Symbol>>,
    pub pin_refs: Vec<IpcPinRef<Symbol>>,
    pub paths: Vec<GeometryPath>,
    pub contours: Vec<GeometryContour>,
    pub path_cmds: Vec<PathCmd>,
    pub diagnostics: Vec<GeometryDiagnostic>,
}

impl<Symbol, LayerFunction> GeometryDocument<Symbol, LayerFunction> {
    pub fn new() -> Self {
        Self::default()
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

impl<Symbol, LayerFunction> Default for GeometryDocument<Symbol, LayerFunction> {
    fn default() -> Self {
        Self {
            layout: LayoutGraph::new(),
            layers: Vec::new(),
            profiles: Vec::new(),
            profile_cutouts: Vec::new(),
            specs: Vec::new(),
            spec_items: Vec::new(),
            spec_properties: Vec::new(),
            spec_refs: Vec::new(),
            feature_sets: Vec::new(),
            features: Vec::new(),
            pin_refs: Vec::new(),
            paths: Vec::new(),
            contours: Vec::new(),
            path_cmds: Vec::new(),
            diagnostics: Vec::new(),
        }
    }
}

const PROFILE_STROKE_WIDTH: f64 = 0.1;

pub fn board_bbox<Symbol, LayerFunction>(
    doc: &GeometryDocument<Symbol, LayerFunction>,
) -> Option<BBox> {
    layout_steps_by_kind(doc, LayoutStepKind::Board)
        .map(|(_, step)| step.bbox)
        .find(|bbox| !bbox.is_empty())
}

pub fn panel_bbox<Symbol, LayerFunction>(
    doc: &GeometryDocument<Symbol, LayerFunction>,
) -> Option<BBox> {
    root_panel_step(doc)
        .map(|(_, step)| step.bbox)
        .filter(|bbox| !bbox.is_empty())
        .or_else(|| {
            layout_steps_by_kind(doc, LayoutStepKind::Panel)
                .map(|(_, step)| step.bbox)
                .find(|bbox| !bbox.is_empty())
        })
}

pub fn root_step<Symbol, LayerFunction>(
    doc: &GeometryDocument<Symbol, LayerFunction>,
) -> Option<(u32, &LayoutStep<Symbol>)> {
    let index = doc.layout.root_step?;
    doc.layout
        .steps
        .get(index as usize)
        .map(|step| (index, step))
}

pub fn root_panel_step<Symbol, LayerFunction>(
    doc: &GeometryDocument<Symbol, LayerFunction>,
) -> Option<(u32, &LayoutStep<Symbol>)> {
    root_step(doc).filter(|(_, step)| step.kind == LayoutStepKind::Panel)
}

pub fn layout_steps_by_kind<Symbol, LayerFunction>(
    doc: &GeometryDocument<Symbol, LayerFunction>,
    kind: LayoutStepKind,
) -> impl Iterator<Item = (u32, &LayoutStep<Symbol>)> {
    doc.layout
        .steps
        .iter()
        .enumerate()
        .filter_map(move |(index, step)| (step.kind == kind).then_some((index as u32, step)))
}

pub fn layout_instances_by_kind<Symbol, LayerFunction>(
    doc: &GeometryDocument<Symbol, LayerFunction>,
    kind: LayoutStepKind,
) -> impl Iterator<Item = (u32, &LayoutInstance<Symbol>)> {
    doc.layout
        .instances
        .iter()
        .enumerate()
        .filter_map(move |(index, instance)| {
            let step = doc.layout.steps.get(instance.child_step as usize)?;
            (step.kind == kind).then_some((index as u32, instance))
        })
}

pub fn layout_child_repeats<Symbol, LayerFunction>(
    doc: &GeometryDocument<Symbol, LayerFunction>,
    parent_step: u32,
    parent_instance: Option<u32>,
) -> impl Iterator<Item = (u32, &LayoutRepeat<Symbol>)> {
    doc.layout
        .repeats
        .iter()
        .enumerate()
        .filter_map(move |(index, repeat)| {
            (repeat.parent_step == parent_step && repeat.parent_instance == parent_instance)
                .then_some((index as u32, repeat))
        })
}

pub fn layout_repeat_instances<'a, Symbol, LayerFunction>(
    doc: &'a GeometryDocument<Symbol, LayerFunction>,
    repeat: &LayoutRepeat<Symbol>,
) -> impl Iterator<Item = (u32, &'a LayoutInstance<Symbol>)> {
    let start = repeat.instance_start;
    let end = repeat.instance_start + repeat.instance_count;
    (start..end).filter_map(move |index| {
        doc.layout
            .instances
            .get(index as usize)
            .map(|instance| (index, instance))
    })
}

pub fn board_step_count<Symbol, LayerFunction>(
    doc: &GeometryDocument<Symbol, LayerFunction>,
) -> usize {
    layout_steps_by_kind(doc, LayoutStepKind::Board).count()
}

pub fn board_instance_count<Symbol, LayerFunction>(
    doc: &GeometryDocument<Symbol, LayerFunction>,
) -> usize {
    layout_instances_by_kind(doc, LayoutStepKind::Board).count()
}

pub fn panel_step_count<Symbol, LayerFunction>(
    doc: &GeometryDocument<Symbol, LayerFunction>,
) -> usize {
    layout_steps_by_kind(doc, LayoutStepKind::Panel).count()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileSet {
    /// The canonical board step profile only.
    BoardOutlines,
    /// Physical outlines for manufacturing exports.
    ///
    /// For a panel root, this means the root panel profile plus final board
    /// instance profiles. Nested panel boundaries are intentionally excluded.
    FabricationOutlines,
    /// Every placed profile boundary in the layout graph, including nested
    /// panel/subpanel boundaries.
    LayoutBoundaries,
    /// Only the root step profile.
    RootOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeometryView {
    /// Canonical board-step geometry only.
    Board,
    /// Root array-step geometry only, with no repeated child board materialization.
    ArrayLocal,
    /// Root array-step geometry plus repeated child board/sub-array geometry in array coordinates.
    ArrayFlattened,
    /// Root-step geometry plus the symbolic layout graph, without repeated feature materialization.
    LayoutSymbolic,
}

impl GeometryView {
    pub fn profile_set(self) -> ProfileSet {
        match self {
            Self::Board => ProfileSet::BoardOutlines,
            Self::ArrayLocal => ProfileSet::RootOnly,
            Self::ArrayFlattened => ProfileSet::FabricationOutlines,
            Self::LayoutSymbolic => ProfileSet::LayoutBoundaries,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileOccurrenceRole {
    Unplaced,
    RootBoard,
    RootPanel,
    RootStep,
    BoardDefinition,
    BoardInstance,
    PanelInstance,
    StepInstance,
}

#[derive(Debug, Clone, Copy)]
pub struct ProfileOccurrence<'a> {
    pub profile_index: u32,
    pub profile: &'a StepProfile,
    pub step: Option<u32>,
    pub instance: Option<u32>,
    pub transform: Affine2,
    pub role: ProfileOccurrenceRole,
    pub depth: u32,
}

fn profile_range_indices(start: u32, count: u32) -> impl Iterator<Item = u32> {
    start..start + count
}

pub fn profile_occurrences_for<Symbol, LayerFunction>(
    doc: &GeometryDocument<Symbol, LayerFunction>,
    profile_set: ProfileSet,
) -> Vec<ProfileOccurrence<'_>> {
    if profile_set == ProfileSet::BoardOutlines {
        return board_profile_occurrences(doc);
    }

    let Some((root_index, root)) = root_step(doc) else {
        return doc
            .profiles
            .iter()
            .enumerate()
            .map(|(profile_index, profile)| ProfileOccurrence {
                profile_index: profile_index as u32,
                profile,
                step: None,
                instance: None,
                transform: Affine2::identity(),
                role: ProfileOccurrenceRole::Unplaced,
                depth: 0,
            })
            .collect();
    };

    let mut occurrences = Vec::new();
    push_profile_occurrences(
        &mut occurrences,
        doc,
        ProfileOccurrenceSpec {
            start: root.profile_start,
            count: root.profile_count,
            step: Some(root_index),
            instance: None,
            transform: Affine2::identity(),
            role: root_profile_role(root.kind),
            depth: 0,
        },
    );

    if profile_set == ProfileSet::RootOnly {
        return occurrences;
    }

    for (instance_index, instance) in doc.layout.instances.iter().enumerate() {
        let Some(step) = doc.layout.steps.get(instance.child_step as usize) else {
            continue;
        };
        if !include_instance_profiles(profile_set, root.kind, step.kind) {
            continue;
        }

        push_profile_occurrences(
            &mut occurrences,
            doc,
            ProfileOccurrenceSpec {
                start: step.profile_start,
                count: step.profile_count,
                step: Some(instance.child_step),
                instance: Some(instance_index as u32),
                transform: instance.transform,
                role: instance_profile_role(step.kind),
                depth: instance_depth(doc, instance_index as u32),
            },
        );
    }
    occurrences
}

#[derive(Debug, Clone, Copy)]
struct ProfileOccurrenceSpec {
    start: u32,
    count: u32,
    step: Option<u32>,
    instance: Option<u32>,
    transform: Affine2,
    role: ProfileOccurrenceRole,
    depth: u32,
}

fn push_profile_occurrences<'a, Symbol, LayerFunction>(
    occurrences: &mut Vec<ProfileOccurrence<'a>>,
    doc: &'a GeometryDocument<Symbol, LayerFunction>,
    spec: ProfileOccurrenceSpec,
) {
    for profile_index in profile_range_indices(spec.start, spec.count) {
        let Some(profile) = doc.profiles.get(profile_index as usize) else {
            continue;
        };
        occurrences.push(ProfileOccurrence {
            profile_index,
            profile,
            step: spec.step,
            instance: spec.instance,
            transform: spec.transform,
            role: spec.role,
            depth: spec.depth,
        });
    }
}

fn include_instance_profiles(
    profile_set: ProfileSet,
    root_kind: LayoutStepKind,
    child_kind: LayoutStepKind,
) -> bool {
    match profile_set {
        ProfileSet::FabricationOutlines => {
            root_kind == LayoutStepKind::Panel && child_kind == LayoutStepKind::Board
        }
        ProfileSet::LayoutBoundaries => true,
        ProfileSet::BoardOutlines | ProfileSet::RootOnly => false,
    }
}

fn board_profile_occurrences<Symbol, LayerFunction>(
    doc: &GeometryDocument<Symbol, LayerFunction>,
) -> Vec<ProfileOccurrence<'_>> {
    let Some((step_index, step)) = layout_steps_by_kind(doc, LayoutStepKind::Board).next() else {
        return Vec::new();
    };
    let role = if root_step(doc).is_some_and(|(root_index, _)| root_index == step_index) {
        ProfileOccurrenceRole::RootBoard
    } else {
        ProfileOccurrenceRole::BoardDefinition
    };
    let mut occurrences = Vec::new();
    push_profile_occurrences(
        &mut occurrences,
        doc,
        ProfileOccurrenceSpec {
            start: step.profile_start,
            count: step.profile_count,
            step: Some(step_index),
            instance: None,
            transform: Affine2::identity(),
            role,
            depth: 0,
        },
    );
    occurrences
}

fn root_profile_role(kind: LayoutStepKind) -> ProfileOccurrenceRole {
    match kind {
        LayoutStepKind::Board => ProfileOccurrenceRole::RootBoard,
        LayoutStepKind::Panel => ProfileOccurrenceRole::RootPanel,
        _ => ProfileOccurrenceRole::RootStep,
    }
}

fn instance_profile_role(kind: LayoutStepKind) -> ProfileOccurrenceRole {
    match kind {
        LayoutStepKind::Board => ProfileOccurrenceRole::BoardInstance,
        LayoutStepKind::Panel => ProfileOccurrenceRole::PanelInstance,
        _ => ProfileOccurrenceRole::StepInstance,
    }
}

fn instance_depth<Symbol, LayerFunction>(
    doc: &GeometryDocument<Symbol, LayerFunction>,
    instance_index: u32,
) -> u32 {
    let mut depth = 1;
    let mut remaining = doc.layout.instances.len();
    let mut parent = doc
        .layout
        .instances
        .get(instance_index as usize)
        .and_then(|instance| instance.parent_instance);

    while let Some(parent_index) = parent {
        if remaining == 0 {
            break;
        }
        remaining -= 1;
        depth += 1;
        parent = doc
            .layout
            .instances
            .get(parent_index as usize)
            .and_then(|instance| instance.parent_instance);
    }
    depth
}

pub fn lower_layer_to_artwork<Symbol: Clone, LayerFunction: Clone>(
    doc: &GeometryDocument<Symbol, LayerFunction>,
    layer_index: usize,
    role: LayerRole,
    side: Side,
) -> artwork::ArtworkDocument<LayerFunction, Option<Symbol>> {
    let mut artwork = artwork::ArtworkDocument::new(Unit::Millimeter);
    let layer = &doc.layers[layer_index];
    let artwork_layer = artwork.push_layer(artwork::ArtworkLayer {
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
            let Some((artwork_path, geometry)) = lower_path_kind(path) else {
                continue;
            };
            let path_id = artwork.push_path(artwork_path, path_payloads(doc, path));
            artwork.push_object(
                artwork_layer,
                artwork::ArtworkObject {
                    paint: paint_polarity(feature.polarity),
                    order: paint_order(feature),
                    geometry: geometry(path_id),
                    net: None,
                    bbox: path.bbox,
                    meta: feature.net.clone(),
                },
            );
        }
    }

    artwork.diagnostics.extend(doc.diagnostics.clone());
    artwork::normalize_bounds(&mut artwork);
    artwork
}

pub fn lower_layer_with_profile_set_to_artwork<Symbol: Clone, LayerFunction: Clone>(
    doc: &GeometryDocument<Symbol, LayerFunction>,
    layer_index: usize,
    role: LayerRole,
    side: Side,
    profile_set: ProfileSet,
) -> artwork::ArtworkDocument<LayerFunction, Option<Symbol>> {
    let mut artwork = lower_layer_to_artwork(doc, layer_index, role, side);
    let layer = &doc.layers[layer_index];
    let outline_layer = artwork.push_layer(artwork::ArtworkLayer {
        name: "Profile".to_string(),
        role: LayerRole::Profile,
        side: Side::None,
        object_start: 0,
        object_count: 0,
        bbox: BBox::empty(),
        meta: layer.layer_function.clone(),
    });

    for occurrence in profile_occurrences_for(doc, profile_set) {
        push_profile_path_to_artwork(
            &mut artwork,
            outline_layer,
            doc,
            occurrence.profile.outer_path,
            occurrence.transform,
        );
        for cutout in &doc.profile_cutouts[occurrence.profile.cutout_start as usize
            ..(occurrence.profile.cutout_start + occurrence.profile.cutout_count) as usize]
        {
            push_profile_path_to_artwork(
                &mut artwork,
                outline_layer,
                doc,
                cutout.path,
                occurrence.transform,
            );
        }
    }

    artwork::normalize_bounds(&mut artwork);
    artwork
}

fn push_profile_path_to_artwork<Symbol, LayerFunction>(
    artwork: &mut artwork::ArtworkDocument<LayerFunction, Option<Symbol>>,
    layer_id: u32,
    doc: &GeometryDocument<Symbol, LayerFunction>,
    path_index: u32,
    transform: Affine2,
) {
    let payloads = transformed_path_payloads(doc, path_index, transform);
    let path_id = artwork.push_path(
        artwork::ArtworkPath::stroked(PROFILE_STROKE_WIDTH, LineCap::Round, LineJoin::Round),
        payloads,
    );
    let bbox = artwork.paths[path_id as usize].bbox;
    artwork.push_object(
        layer_id,
        artwork::ArtworkObject {
            paint: PaintPolarity::Dark,
            order: artwork::PaintOrder {
                stage: artwork::PaintStage::Overlay,
            },
            geometry: artwork::ArtworkGeometry::Stroke { path: path_id },
            net: None,
            bbox,
            meta: None,
        },
    );
    artwork.layers[layer_id as usize].bbox = artwork.layers[layer_id as usize].bbox.union(bbox);
}

type ArtworkGeometryFactory = fn(u32) -> artwork::ArtworkGeometry;

fn lower_path_kind(path: &GeometryPath) -> Option<(artwork::ArtworkPath, ArtworkGeometryFactory)> {
    match path.paint_class().ok()? {
        Some(GeometryPathPaintClass::Filled) => Some((
            artwork::ArtworkPath::filled(path.style.fill.rule),
            |path| artwork::ArtworkGeometry::Region { path },
        )),
        Some(GeometryPathPaintClass::Stroked) => Some((
            artwork::ArtworkPath::stroked(
                path.style.stroke.width,
                path.style.stroke.line_cap,
                LineJoin::Round,
            ),
            |path| artwork::ArtworkGeometry::Stroke { path },
        )),
        None => None,
    }
}

fn paint_order<Symbol>(feature: &GeometryFeature<Symbol>) -> artwork::PaintOrder {
    let stage = if feature.bucket == FeatureBucket::Cutout {
        artwork::PaintStage::FinalCutout
    } else if feature.polarity == GeometryPolarity::Negative || feature.flags.clears_previous_in_set
    {
        artwork::PaintStage::Base
    } else if matches!(
        feature.bucket,
        FeatureBucket::Fill | FeatureBucket::Thermal | FeatureBucket::Antipad
    ) {
        artwork::PaintStage::Base
    } else {
        artwork::PaintStage::Overlay
    };
    artwork::PaintOrder { stage }
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

pub fn transformed_path_payloads<Symbol, LayerFunction>(
    doc: &GeometryDocument<Symbol, LayerFunction>,
    path_index: u32,
    transform: Affine2,
) -> Vec<common_path::PathPayload> {
    let path = &doc.paths[path_index as usize];
    doc.contours[path.contour_start as usize..(path.contour_start + path.contour_count) as usize]
        .iter()
        .map(|contour| {
            common_path::transform_cmds(
                doc.path_cmds
                    [contour.cmd_start as usize..(contour.cmd_start + contour.cmd_count) as usize]
                    .iter()
                    .copied(),
                transform,
            )
            .into()
        })
        .collect()
}

pub fn transformed_path_bbox<Symbol, LayerFunction>(
    doc: &GeometryDocument<Symbol, LayerFunction>,
    path_index: u32,
    transform: Affine2,
) -> BBox {
    transformed_path_payloads(doc, path_index, transform)
        .iter()
        .fold(BBox::empty(), |bbox, payload| bbox.union(payload.bbox))
}

fn paint_polarity(polarity: GeometryPolarity) -> PaintPolarity {
    match polarity {
        GeometryPolarity::Positive => PaintPolarity::Dark,
        GeometryPolarity::Negative => PaintPolarity::Clear,
    }
}

/// Canonical IPC layout graph.
///
/// IPC-2581 stores panelization as reusable `Step` definitions plus
/// `StepRepeat` placements. This graph mirrors that model directly: `steps`
/// are reusable definitions, `repeats` are compact placement edges, and
/// `instances` are the materialized placements generated by each repeat.
/// Flattened board or board-array geometry is a derived export/rendering view, not
/// independent IR state.
#[derive(Debug, Clone)]
pub struct LayoutGraph<Symbol> {
    /// Root step selected by IPC Content/StepRef or by parser fallback.
    pub root_step: Option<u32>,
    /// Reusable step definitions. Indices are referenced by repeats and instances.
    pub steps: Vec<LayoutStep<Symbol>>,
    /// Compact parent-to-child repeat records. Instances for one repeat are contiguous.
    pub repeats: Vec<LayoutRepeat<Symbol>>,
    /// Materialized placements. Nested panel children reference their parent instance.
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

/// Reusable IPC step definition.
///
/// A step owns its local profile/layer features. A board array is represented
/// by a panel step with child board instances, not by duplicating board data.
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

/// Compact placement edge from a parent step or parent instance to a child step.
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

/// One materialized placement of a child step.
///
/// `transform` maps from the child step's local coordinate system into the
/// root layout coordinate system. Repeated boards, subpanels, coupons, and
/// tooling steps are all represented by this same structure.
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
    pub bbox: BBox,
}

#[derive(Debug, Clone)]
pub struct StepProfile {
    pub outer_path: u32,
    pub cutout_start: u32,
    pub cutout_count: u32,
    pub bbox: BBox,
}

#[derive(Debug, Clone)]
pub struct StepProfileCutout {
    pub path: u32,
    pub bbox: BBox,
}

#[derive(Debug, Clone)]
pub struct IpcSpec<Symbol> {
    pub name: Symbol,
    pub item_start: u32,
    pub item_count: u32,
}

#[derive(Debug, Clone)]
pub struct IpcSpecItem<Symbol> {
    pub element: Symbol,
    pub kind: IpcSpecItemKind,
    pub item_type: Option<Symbol>,
    pub comment: Option<Symbol>,
    pub property_start: u32,
    pub property_count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpcSpecItemKind {
    General,
    Dielectric,
    Conductor,
    SurfaceFinish,
    VCut,
    Other,
}

#[derive(Debug, Clone)]
pub struct IpcSpecProperty<Symbol> {
    pub value: Option<f64>,
    pub text: Option<Symbol>,
    pub unit: Option<Symbol>,
    pub plus_tol: Option<f64>,
    pub minus_tol: Option<f64>,
    pub tol_percent: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct IpcSpecRef<Symbol> {
    pub spec: Symbol,
}

#[derive(Debug, Clone)]
pub struct GeometryFeatureSet<Symbol> {
    pub layer: u32,
    pub source_set_index: u32,
    pub net: Option<Symbol>,
    pub polarity: GeometryPolarity,
    pub spec_ref_start: u32,
    pub spec_ref_count: u32,
    pub feature_start: u32,
    pub feature_count: u32,
    pub bbox: BBox,
}

#[derive(Debug, Clone)]
pub struct GeometryLayer<Symbol, LayerFunction> {
    pub name: String,
    pub source_layer_ref: Symbol,
    pub layer_function: LayerFunction,
    pub spec_ref_start: u32,
    pub spec_ref_count: u32,
    pub set_start: u32,
    pub set_count: u32,
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
    pub set: Option<u32>,
    pub source: SourceRef,
    pub intent: FeatureIntent<Symbol>,
    pub fiducial_kind: FiducialKind,
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
    pub pin_ref_start: u32,
    pub pin_ref_count: u32,
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
            set: None,
            source: SourceRef::default(),
            intent: FeatureIntent::default(),
            fiducial_kind: FiducialKind::Unknown,
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
            pin_ref_start: 0,
            pin_ref_count: 0,
            flags: FeatureFlags::default(),
        }
    }
}

impl<Symbol: Clone> GeometryFeature<Symbol> {
    pub fn with_path_range(
        &self,
        bucket: FeatureBucket,
        path_start: u32,
        path_count: u32,
        bbox: BBox,
    ) -> Self {
        let mut feature = self.clone();
        feature.bucket = bucket;
        feature.bbox = bbox;
        feature.path_start = path_start;
        feature.path_count = path_count;
        feature
    }
}

#[derive(Debug, Clone)]
pub struct IpcPinRef<Symbol> {
    pub component_ref: Option<Symbol>,
    pub pin: Symbol,
    pub title: Option<Symbol>,
}

#[derive(Debug, Clone)]
pub struct GeometryPath {
    pub contour_start: u32,
    pub contour_count: u32,
    pub bbox: BBox,
    pub style: GeometryPathStyle,
    pub flags: PathFlags,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct GeometryPathStyle {
    pub fill: GeometryFillStyle,
    pub stroke: GeometryStrokeStyle,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GeometryFillStyle {
    pub property: GeometryFillProperty,
    pub rule: FillRule,
    pub hatch: Option<GeometryHatchStyle>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GeometryFillProperty {
    Solid,
    Hollow,
    Void,
    Hatch,
    Mesh,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GeometryHatchStyle {
    pub angle1_degrees: Option<f64>,
    pub angle2_degrees: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GeometryStrokeStyle {
    pub width: f64,
    pub line_cap: LineCap,
    pub line_join: LineJoin,
    pub pattern: LinePattern,
}

impl GeometryPathStyle {
    pub fn filled(rule: FillRule) -> Self {
        Self {
            fill: GeometryFillStyle {
                property: GeometryFillProperty::Solid,
                rule,
                hatch: None,
            },
            stroke: GeometryStrokeStyle::default(),
        }
    }

    pub fn stroked(width: f64, line_cap: LineCap) -> Self {
        Self {
            fill: GeometryFillStyle::default(),
            stroke: GeometryStrokeStyle {
                width,
                line_cap,
                line_join: LineJoin::Round,
                pattern: LinePattern::Solid,
            },
        }
    }
}

impl Default for GeometryFillStyle {
    fn default() -> Self {
        Self {
            property: GeometryFillProperty::Solid,
            rule: FillRule::NonZero,
            hatch: None,
        }
    }
}

impl Default for GeometryStrokeStyle {
    fn default() -> Self {
        Self {
            width: 0.0,
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            pattern: LinePattern::Solid,
        }
    }
}

impl GeometryPath {
    pub fn filled(fill_rule: FillRule, bbox: BBox) -> Self {
        Self {
            contour_start: 0,
            contour_count: 0,
            bbox,
            style: GeometryPathStyle::filled(fill_rule),
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
            style: GeometryPathStyle::stroked(width, line_cap),
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
            style: GeometryPathStyle::default(),
            flags: PathFlags::default(),
        }
    }

    pub fn paint_class(&self) -> Result<Option<GeometryPathPaintClass>, String> {
        match (self.flags.filled, self.flags.stroked) {
            (true, false) => Ok(Some(GeometryPathPaintClass::Filled)),
            (false, true) => Ok(Some(GeometryPathPaintClass::Stroked)),
            (false, false) => Ok(None),
            (true, true) => Err("path is both filled and stroked".to_string()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GeometryPathPaintClass {
    Filled,
    Stroked,
}

impl GeometryPathPaintClass {
    pub fn primitive_bucket(self) -> FeatureBucket {
        match self {
            Self::Filled => FeatureBucket::Fill,
            Self::Stroked => FeatureBucket::Trace,
        }
    }
}

#[derive(Debug, Clone)]
pub struct GeometryContour {
    pub cmd_start: u32,
    pub cmd_count: u32,
    pub bbox: BBox,
}

pub fn split_primitive_feature_path_runs<Symbol: Clone, LayerFunction>(
    doc: &GeometryDocument<Symbol, LayerFunction>,
    feature: GeometryFeature<Symbol>,
) -> Result<Vec<GeometryFeature<Symbol>>, String> {
    let path_end = checked_range_end(
        "feature paths",
        feature.path_start,
        feature.path_count,
        doc.paths.len(),
    )?;
    let mut features = Vec::new();
    let mut run_start = feature.path_start;
    let mut run_class = None;

    for path_index in feature.path_start..path_end {
        let path = &doc.paths[path_index as usize];
        let class = path.paint_class().map_err(|error| {
            format!(
                "feature source {:?} path {path_index} has invalid paint flags: {error}",
                feature.source
            )
        })?;
        if class == run_class {
            continue;
        }

        if let Some(class) = run_class {
            push_primitive_path_run(&mut features, doc, &feature, run_start, path_index, class);
        }
        run_start = path_index;
        run_class = class;
    }

    if let Some(class) = run_class {
        push_primitive_path_run(&mut features, doc, &feature, run_start, path_end, class);
    }

    Ok(features)
}

fn push_primitive_path_run<Symbol: Clone, LayerFunction>(
    features: &mut Vec<GeometryFeature<Symbol>>,
    doc: &GeometryDocument<Symbol, LayerFunction>,
    feature: &GeometryFeature<Symbol>,
    run_start: u32,
    run_end: u32,
    class: GeometryPathPaintClass,
) {
    if run_start == run_end {
        return;
    }
    features.push(feature.with_path_range(
        class.primitive_bucket(),
        run_start,
        run_end - run_start,
        paths_bbox(doc, run_start, run_end - run_start),
    ));
}

pub fn validate_artwork_ready<Symbol, LayerFunction>(
    doc: &GeometryDocument<Symbol, LayerFunction>,
) -> Result<(), String> {
    validate_homogeneous_features(doc)?;
    for (feature_index, feature) in doc.features.iter().enumerate() {
        if feature.path_count == 0 {
            continue;
        }
        if feature.flags.clears_previous_in_set {
            return Err(format!(
                "feature {feature_index} still has unresolved set-void clear semantics"
            ));
        }
        if feature.bucket != FeatureBucket::Cutout && feature.polarity != GeometryPolarity::Positive
        {
            return Err(format!(
                "feature {feature_index} still has unresolved negative polarity"
            ));
        }
        validate_feature_arcs(doc, feature_index, feature)?;
    }
    Ok(())
}

pub fn validate_homogeneous_features<Symbol, LayerFunction>(
    doc: &GeometryDocument<Symbol, LayerFunction>,
) -> Result<(), String> {
    for (feature_index, feature) in doc.features.iter().enumerate() {
        let path_end = checked_range_end(
            "feature paths",
            feature.path_start,
            feature.path_count,
            doc.paths.len(),
        )
        .map_err(|error| format!("feature {feature_index}: {error}"))?;
        let mut feature_class = None;
        for path_index in feature.path_start..path_end {
            let path_class = doc.paths[path_index as usize]
                .paint_class()
                .map_err(|error| format!("feature {feature_index} path {path_index}: {error}"))?
                .ok_or_else(|| format!("feature {feature_index} path {path_index} is unpainted"))?;

            match feature_class {
                Some(previous) if previous != path_class => {
                    return Err(format!(
                        "feature {feature_index} mixes {previous:?} and {path_class:?} paths"
                    ));
                }
                None => feature_class = Some(path_class),
                _ => {}
            }
        }
    }
    Ok(())
}

fn validate_feature_arcs<Symbol, LayerFunction>(
    doc: &GeometryDocument<Symbol, LayerFunction>,
    feature_index: usize,
    feature: &GeometryFeature<Symbol>,
) -> Result<(), String> {
    let path_end = checked_range_end(
        "feature paths",
        feature.path_start,
        feature.path_count,
        doc.paths.len(),
    )
    .map_err(|error| format!("feature {feature_index}: {error}"))?;
    for path_index in feature.path_start..path_end {
        validate_path_arcs(doc, feature_index, path_index)?;
    }
    Ok(())
}

fn validate_path_arcs<Symbol, LayerFunction>(
    doc: &GeometryDocument<Symbol, LayerFunction>,
    feature_index: usize,
    path_index: u32,
) -> Result<(), String> {
    let path = &doc.paths[path_index as usize];
    let contour_end = checked_range_end(
        "path contours",
        path.contour_start,
        path.contour_count,
        doc.contours.len(),
    )
    .map_err(|error| format!("feature {feature_index} path {path_index}: {error}"))?;
    for contour_index in path.contour_start..contour_end {
        let contour = &doc.contours[contour_index as usize];
        let cmd_end = checked_range_end(
            "contour commands",
            contour.cmd_start,
            contour.cmd_count,
            doc.path_cmds.len(),
        )
        .map_err(|error| {
            format!("feature {feature_index} path {path_index} contour {contour_index}: {error}")
        })?;
        let mut current = Point::default();
        for cmd_index in contour.cmd_start..cmd_end {
            let cmd = doc.path_cmds[cmd_index as usize];
            match cmd.op {
                PathOp::MoveTo | PathOp::LineTo => current = cmd.p0,
                PathOp::ArcTo => {
                    validate_arc_command(feature_index, path_index, cmd_index, current, cmd)?;
                    current = cmd.p0;
                }
                PathOp::CubicTo => current = cmd.p2,
                PathOp::Close => {}
            }
        }
    }
    Ok(())
}

fn validate_arc_command(
    feature_index: usize,
    path_index: u32,
    cmd_index: u32,
    start: Point,
    cmd: PathCmd,
) -> Result<(), String> {
    let start_radius = start.distance_to(cmd.p1);
    let end_radius = cmd.p0.distance_to(cmd.p1);
    if start_radius <= 0.0 || end_radius <= 0.0 {
        return Err(format!(
            "feature {feature_index} path {path_index} command {cmd_index} has a zero-radius arc"
        ));
    }
    if !arc_radii_nearly_equal(start_radius, end_radius) {
        return Err(format!(
            "feature {feature_index} path {path_index} command {cmd_index} has non-circular arc radii {start_radius} and {end_radius}"
        ));
    }
    Ok(())
}

fn checked_range_end(label: &str, start: u32, count: u32, len: usize) -> Result<u32, String> {
    let end = start
        .checked_add(count)
        .ok_or_else(|| format!("{label} range overflows"))?;
    if end as usize > len {
        return Err(format!(
            "{label} range {start}..{end} exceeds available length {len}"
        ));
    }
    Ok(end)
}

fn paths_bbox<Symbol, LayerFunction>(
    doc: &GeometryDocument<Symbol, LayerFunction>,
    path_start: u32,
    path_count: u32,
) -> BBox {
    doc.paths[path_start as usize..(path_start + path_count) as usize]
        .iter()
        .fold(BBox::empty(), |bbox, path| bbox.union(path.bbox))
}

const GEOMETRY_EPSILON: f64 = 1e-9;
const ARC_RADIUS_ABSOLUTE_TOLERANCE_MM: f64 = 1e-4;

fn arc_radii_nearly_equal(left: f64, right: f64) -> bool {
    (left - right).abs()
        <= ARC_RADIUS_ABSOLUTE_TOLERANCE_MM
            .max(GEOMETRY_EPSILON * left.abs().max(right.abs()).max(1.0))
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
    Fiducial,
    Trace,
    Fill,
    Cutout,
    Thermal,
    Antipad,
}

/// Source-level fabrication meaning carried with geometry through processing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FeatureIntent<Symbol> {
    pub domain: FeatureDomain,
    pub role: FeatureRole,
    pub operation: FeatureOperation,
    pub material: FeatureMaterial,
    pub plating: PlatingKind,
    pub span: FeatureSpan<Symbol>,
    pub side: Side,
}

impl<Symbol> Default for FeatureIntent<Symbol> {
    fn default() -> Self {
        Self {
            domain: FeatureDomain::Unknown,
            role: FeatureRole::Unknown,
            operation: FeatureOperation::Unknown,
            material: FeatureMaterial::Unknown,
            plating: PlatingKind::Unknown,
            span: FeatureSpan::Unknown,
            side: Side::None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FeatureDomain {
    Unknown,
    Copper,
    Soldermask,
    Paste,
    Legend,
    Drill,
    Rout,
    VCut,
    Score,
    Profile,
    Mechanical,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FeatureRole {
    Unknown,
    Conductor,
    Pad,
    Via,
    Hole,
    Slot,
    Fiducial,
    BoardOutline,
    ArraySeparation,
    Route,
    Cutout,
    Thermal,
    Antipad,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FeatureOperation {
    Unknown,
    AddMaterial,
    OpenMask,
    Print,
    Drill,
    Route,
    Score,
    Profile,
    Mark,
    RemoveMaterial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FeatureMaterial {
    Unknown,
    None,
    Copper,
    Soldermask,
    Paste,
    Ink,
    Substrate,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlatingKind {
    Unknown,
    None,
    Plated,
    NonPlated,
    Via,
    ViaCapped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FeatureSpan<Symbol> {
    Unknown,
    Layer(Symbol),
    ThroughBoard,
    FromTo {
        from: Option<Symbol>,
        to: Option<Symbol>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FiducialKind {
    Unknown,
    Local,
    Global,
    Panel,
    BadBoard,
    GoodPanel,
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
