use crate::dialects::Side;
use crate::dialects::ipc::layout::LayoutStepKind;
use crate::geom::{Affine2, BBox, FillRule, LineCap, PaintKind, Point, Polarity, Span};

/// One extracted layer feature.
///
/// Geometry lives in `paths` (a span of `doc.arena.paths`); the scalar shape
/// fields (`center`, `width`, `radius`, ...) preserve the source primitive's
/// parameters and are meaningful only for the [`FeatureKind`] that set them:
///
/// - `Hole`/`Slot`: `center`, `width`/`height` (slot ends), `radius`.
/// - `Padstack`/`Primitive`: `center`, `width`, `height`, `rotation_degrees`,
///   `scale`, and `outer_diameter`/`inner_diameter` for annular shapes.
/// - `Trace`: `stroke_width`, `line_cap`.
#[derive(Debug, Clone)]
pub struct Feature<Symbol> {
    pub kind: FeatureKind,
    /// Export/render grouping, derived from `kind` and `intent` via
    /// [`FeatureBucket::classify`]. Extraction never writes this directly;
    /// lowering passes refine it (primitive path runs split into fill/trace
    /// buckets, layer flattening rewrites to `Fill`).
    pub bucket: FeatureBucket,
    pub polarity: Polarity,
    pub net: Option<Symbol>,
    pub source_layer_ref: Option<Symbol>,
    pub source_step_ref: Option<Symbol>,
    pub source_step_kind: LayoutStepKind,
    /// Index into `doc.feature_sets`, when the feature came from a set.
    pub set: Option<u32>,
    pub source: SourceRef,
    pub intent: FeatureIntent<Symbol>,
    pub fiducial_kind: FiducialKind,
    pub transform: Affine2,
    pub bbox: BBox,
    /// Spans `doc.arena.paths`.
    pub paths: Span,

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
    /// Spans `doc.pin_refs`.
    pub pin_refs: Span,
    pub flags: FeatureFlags,
}

impl<Symbol> Feature<Symbol> {
    pub fn new(kind: FeatureKind, polarity: Polarity) -> Self {
        let intent = FeatureIntent::default();
        Self {
            kind,
            bucket: FeatureBucket::classify(kind, &intent),
            polarity,
            net: None,
            source_layer_ref: None,
            source_step_ref: None,
            source_step_kind: LayoutStepKind::Unknown,
            set: None,
            source: SourceRef::default(),
            intent,
            fiducial_kind: FiducialKind::Unknown,
            transform: Affine2::IDENTITY,
            bbox: BBox::empty(),
            paths: Span::EMPTY,
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
            pin_refs: Span::EMPTY,
            flags: FeatureFlags::default(),
        }
    }

    /// Recompute `bucket` from `kind` and the current `intent`. Call after
    /// intent resolution at extraction time.
    pub fn reclassify(&mut self) {
        self.bucket = FeatureBucket::classify(self.kind, &self.intent);
    }

    pub fn is_fiducial(&self) -> bool {
        self.intent.role == FeatureRole::Fiducial
    }

    pub fn is_vscore(&self) -> bool {
        self.intent.role == FeatureRole::ArraySeparation
            && matches!(
                self.intent.domain,
                FeatureDomain::VCut | FeatureDomain::Score
            )
    }

    pub fn is_vcut(&self) -> bool {
        self.intent.role == FeatureRole::ArraySeparation
            && self.intent.domain == FeatureDomain::VCut
    }

    pub fn is_score(&self) -> bool {
        self.intent.role == FeatureRole::ArraySeparation
            && self.intent.domain == FeatureDomain::Score
    }

    pub fn is_drill_like(&self) -> bool {
        matches!(
            self.intent.operation,
            FeatureOperation::Drill | FeatureOperation::Route
        ) || matches!(self.intent.role, FeatureRole::Hole | FeatureRole::Slot)
    }

    pub fn is_nonplated_tooling_hole(&self) -> bool {
        self.intent.role == FeatureRole::Hole
            && self.intent.operation == FeatureOperation::Drill
            && self.intent.plating == PlatingKind::NonPlated
    }

    pub fn is_board_step_feature(&self) -> bool {
        self.source_step_kind == LayoutStepKind::Board
    }

    pub fn is_array_step_feature(&self) -> bool {
        self.source_step_kind == LayoutStepKind::Panel
    }
}

impl<Symbol: Clone> Feature<Symbol> {
    pub fn with_path_span(&self, bucket: FeatureBucket, paths: Span, bbox: BBox) -> Self {
        let mut feature = self.clone();
        feature.bucket = bucket;
        feature.bbox = bbox;
        feature.paths = paths;
        feature
    }
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
}

impl FeatureBucket {
    /// Classify a feature from its kind and fabrication intent.
    ///
    /// Holes and slots are always cutouts. Otherwise the intent's role
    /// decides, with pads split into through-hole/surface buckets by plating;
    /// features whose role carries no grouping of its own (conductors, array
    /// separation, outlines) fall back to trace-vs-fill by kind.
    pub fn classify<Symbol>(kind: FeatureKind, intent: &FeatureIntent<Symbol>) -> Self {
        match kind {
            FeatureKind::Hole | FeatureKind::Slot => Self::Cutout,
            _ => match intent.role {
                FeatureRole::Via => Self::Via,
                FeatureRole::Fiducial => Self::Fiducial,
                FeatureRole::Pad => match intent.plating {
                    PlatingKind::Via | PlatingKind::ViaCapped => Self::Via,
                    PlatingKind::Plated | PlatingKind::NonPlated => Self::Pth,
                    PlatingKind::Unknown | PlatingKind::None => Self::Smd,
                },
                FeatureRole::Cutout => Self::Cutout,
                _ => match kind {
                    FeatureKind::Trace => Self::Trace,
                    _ => Self::Fill,
                },
            },
        }
    }

    /// The bucket a lowered primitive path run belongs to, by paint kind.
    pub fn for_primitive_paint(kind: PaintKind) -> Option<Self> {
        match kind {
            PaintKind::Fill => Some(Self::Fill),
            PaintKind::Stroke => Some(Self::Trace),
            PaintKind::None => None,
        }
    }
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

#[derive(Debug, Clone, Copy, Default)]
pub struct FeatureFlags {
    pub expanded_padstack: bool,
    pub lowered_to_paths: bool,
    pub clears_previous_in_set: bool,
}

/// Position of a feature within its source feature set, for stable ordering.
#[derive(Debug, Clone, Copy, Default)]
pub struct SourceRef {
    pub set_index: u32,
    pub feature_index: u32,
}

/// One IPC `Set` of features on a layer.
#[derive(Debug, Clone)]
pub struct FeatureSet<Symbol> {
    pub layer: u32,
    pub source_set_index: u32,
    pub source_geometry_ref: Option<Symbol>,
    pub net: Option<Symbol>,
    pub polarity: Polarity,
    /// Spans `doc.spec_refs`.
    pub spec_refs: Span,
    /// Spans `doc.features`.
    pub features: Span,
    pub bbox: BBox,
}

#[derive(Debug, Clone)]
pub struct PinRef<Symbol> {
    pub component_ref: Option<Symbol>,
    pub pin: Symbol,
    pub title: Option<Symbol>,
}
