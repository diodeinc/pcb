use crate::dialects::Side;
use crate::dialects::ipc::layout::LayoutStepKind;
use crate::geom::{Affine2, BBox, PaintKind, Point, Polarity, Span};

/// One extracted layer feature.
///
/// Geometry lives in `paths` (a span of `doc.arena.paths`), already placed by
/// `transform` unless the feature belongs to a placement group.
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
    /// Source name for named physical features such as holes and slots.
    pub source_name: Option<Symbol>,
    /// Source-local references declared directly on this feature.
    pub spec_refs: Span,
    /// Materialized occurrence of `source_step_ref` in the layout graph.
    /// `None` identifies geometry owned directly by the root step.
    pub source_instance: Option<u32>,
    /// Materialized placement within an IPC `Features` container.
    pub source_placement: Option<u32>,
    pub source_step_kind: LayoutStepKind,
    /// Index into `doc.feature_sets`, when the feature came from a set.
    pub set: Option<u32>,
    /// Shared placement group for source-local geometry. An absent group means
    /// the feature paths are already in layer coordinates.
    pub placement_group: Option<u32>,
    pub source: SourceRef,
    pub intent: FeatureIntent<Symbol>,
    pub fiducial_kind: FiducialKind,
    pub transform: Affine2,
    pub bbox: BBox,
    /// Spans `doc.arena.paths`.
    pub paths: Span,

    pub center: Point,
    /// The simple shape the geometry is exactly, when it is one.
    pub shape: Option<SimpleShape>,
    pub padstack_ref: Option<Symbol>,
    pub primitive_ref: Option<PrimitiveRef<Symbol>>,
    /// Spans `doc.pin_refs`.
    pub pin_refs: Span,
    pub flags: FeatureFlags,
}

/// A reference into one of the source document's two shape dictionaries.
/// The dictionary matters: standard entries are exact catalogue primitives
/// (circles, rectangles, ovals), user entries are arbitrary contour shapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimitiveRef<Symbol> {
    Standard(Symbol),
    User(Symbol),
}

impl<Symbol: Copy> PrimitiveRef<Symbol> {
    pub fn id(self) -> Symbol {
        match self {
            Self::Standard(id) | Self::User(id) => id,
        }
    }
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
            source_name: None,
            spec_refs: Span::EMPTY,
            source_instance: None,
            source_placement: None,
            source_step_kind: LayoutStepKind::Unknown,
            set: None,
            placement_group: None,
            source: SourceRef::default(),
            intent,
            fiducial_kind: FiducialKind::Unknown,
            transform: Affine2::IDENTITY,
            bbox: BBox::empty(),
            paths: Span::EMPTY,
            center: Point::default(),
            shape: None,
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
        feature.shape = self.shape.filter(|_| paths == self.paths);
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
}

/// A shape a target may have to name rather than trace: a flash aperture, a
/// drill tool, a routed slot. It is centered on [`Feature::center`], sized in
/// layer units, and turned by [`Feature::transform`]. A feature carries one
/// only while its paths are exactly that shape, so a pass that rewrites the
/// paths clears it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SimpleShape {
    Circle {
        diameter: f64,
    },
    Square {
        side: f64,
    },
    /// A stadium `width` along the transform's x axis by `height` along y.
    Oval {
        width: f64,
        height: f64,
    },
}

impl SimpleShape {
    pub fn scaled(self, scale: f64) -> Self {
        match self {
            Self::Circle { diameter } => Self::Circle {
                diameter: diameter * scale,
            },
            Self::Square { side } => Self::Square { side: side * scale },
            Self::Oval { width, height } => Self::Oval {
                width: width * scale,
                height: height * scale,
            },
        }
    }

    /// The diameter of the round tool that drills this shape, if one does.
    pub fn drill_diameter(self) -> Option<f64> {
        match self {
            Self::Circle { diameter } => Some(diameter),
            Self::Square { .. } | Self::Oval { .. } => None,
        }
    }

    /// The width a routed slot states: the diameter of the tool that routs it.
    pub fn slot_width(self) -> Option<f64> {
        match self {
            Self::Oval { width, height } => Some(width.min(height)),
            Self::Circle { .. } | Self::Square { .. } => None,
        }
    }

    /// The size a hole table states: a round hole's diameter or a square
    /// hole's side.
    pub fn hole_size(self) -> Option<f64> {
        match self {
            Self::Circle { diameter: size } | Self::Square { side: size } => Some(size),
            Self::Oval { .. } => None,
        }
    }
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
    pub clears_previous_in_set: bool,
    /// Generated copper balancing inherited from the source IPC feature set.
    pub copper_balance: bool,
    /// Validated source parameters of a generated rounded-hex balance void.
    pub copper_balance_void: Option<CopperBalanceVoid>,
}

/// A flat-top rounded hexagon of circumradius `radius_mm` centered on a site
/// of `lattice`, whose sites tile the plane with flat-top hexagonal cells one
/// pitch across the flats.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CopperBalanceVoid {
    pub lattice: crate::geom::copper_balance::DenseCopperLattice,
    pub radius_mm: f64,
}

/// Position of a feature within its source feature set, for stable ordering.
#[derive(Debug, Clone, Copy, Default)]
pub struct SourceRef {
    pub set_index: u32,
    pub feature_index: u32,
    /// Stable feature-definition index in a whole imported design.
    pub definition: Option<u32>,
}

/// One IPC `Set` of features on a layer.
#[derive(Debug, Clone)]
pub struct FeatureSet<Symbol> {
    pub layer: u32,
    pub source_set_index: u32,
    pub source_geometry_ref: Option<Symbol>,
    pub component_ref: Option<Symbol>,
    pub geometry_usage: Option<GeometryUsage>,
    pub net: Option<Symbol>,
    pub polarity: Polarity,
    /// Spans `doc.spec_refs`.
    pub spec_refs: Span,
    /// Spans `doc.features`.
    pub features: Span,
    pub bbox: BBox,
}

/// Intended use declared by IPC `Set/geometryUsage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GeometryUsage {
    Thieving,
    ThermalRelief,
    Text,
    Teardrop,
    Graphic,
    None,
}

/// Shared placements for one IPC `Features` container.
///
/// Every feature that references this group keeps one local path definition;
/// the placements stamp the complete ordered group without copying geometry.
#[derive(Debug, Clone, Copy)]
pub struct FeaturePlacementGroup {
    /// Spans `doc.feature_placements`.
    pub placements: Span,
    /// Spans the local definitions in `doc.features`.
    pub features: Span,
}

#[derive(Debug, Clone)]
pub struct PinRef<Symbol> {
    pub component_ref: Option<Symbol>,
    pub pin: Symbol,
    pub title: Option<Symbol>,
}
