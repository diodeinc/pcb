//! Safe copper-balancing regions for board arrays and fabrication panels.
//!
//! This module separates IPC semantic collection from planar set operations:
//! callers provide the canonical layout document, its fabrication profile, the
//! copper-layer list, and already-extracted `ArraySupport` documents. Each
//! support path is stored once with copper reach derived from its existing IR
//! feature span and side. Per-layer inputs are derived from that collection,
//! then handled entirely as regularized [`ContourSet`] geometry.
//!
//! For panel material `P`, obstacle union `O`, construction clearance `c`,
//! filled-region radius `q`, and void-gap radius `v`, the construction is:
//!
//! ```text
//! F = (P ⊖ disk(c)) \ (O ⊕ disk(c))
//! A = open(F, disk(q))
//! ```
//!
//! `F` is the clearance-safe set: the complement of the clearance-dilated
//! forbidden phase formed by the panel exterior and every physical obstacle.
//! `A` is a disk opening, the union of all radius-`q` disks that fit in `F`;
//! it removes copper slivers and rounds outward corners without inflating the
//! safety clearance to the regularization scale.
//!
//! Let `G_v(X)` retain the components of `close(X, disk(v)) \ X` that touch two
//! nonincident, opposing boundary branches. This excludes the normal rounded
//! bite at a single concave corner. Each pass removes a radius-`v` tube around
//! the boundary medial axis inside `G_v`, then disk-opens, trimming both sides
//! of every narrow void locally; the same cut repeats until `G_v` is empty.
//! Thus filled features admit a disk of radius `q`, while intervening void
//! gaps admit a disk of radius `v`, without component ranking or
//! feature-specific rules.

use std::fmt;

use crate::dialects::Side;
use crate::dialects::ipc::{
    BoardArrayFabricationProfile, Document, Feature, FeatureSpan, LayoutPurpose,
    ProfileOccurrenceRole, ProfileSet, profile_occurrences_for, relief::is_vcut_operation_feature,
};
use crate::geom::{ContourSet, FillRule, Paint, tol};

/// Default Euclidean clearance from every protected feature.
pub const DEFAULT_BALANCING_CLEARANCE_MM: f64 = 0.5;

/// Default rolling-disk radius for filled balancing regions.
pub const DEFAULT_BALANCING_REGULARIZATION_RADIUS_MM: f64 = 0.5;

/// Half the default minimum width of a two-sided void gap.
pub const DEFAULT_BALANCING_GAP_RADIUS_MM: f64 = 0.5;

/// Conservative allowance for curve flattening and round-offset construction.
pub const DEFAULT_BALANCING_NUMERICAL_GUARD_MM: f64 =
    2.0 * tol::STROKE_OUTLINE_MM + tol::FLATTEN_MM;

/// Inputs to the geometry-only safe-region computation.
#[derive(Debug, Clone)]
pub struct BoardArrayBalancingInput {
    /// Filled root-panel profile.
    pub panel_outer: ContourSet,
    /// Filled final board-instance profiles.
    pub board_footprints: ContourSet,
    /// Profile cutouts, board cutouts, and generated routing reliefs.
    pub material_removal: ContourSet,
    /// Painted physical geometry whose IR span reaches the target copper layer.
    pub support_features: ContourSet,
}

/// One copper layer available to board-array balancing.
///
/// The list passed to the collector is the only copper-stack description used
/// to resolve feature spans and surface-side geometry to copper layers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoardArrayCopperLayer<Symbol> {
    pub name: Symbol,
    pub side: Side,
}

impl<Symbol> BoardArrayCopperLayer<Symbol> {
    pub fn new(name: Symbol, side: Side) -> Self {
        Self { name, side }
    }
}

/// Copper layers affected by one physical support-geometry region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoardArrayCopperReach<Symbol> {
    /// Through-stack or conservatively unresolved physical geometry.
    All,
    /// Geometry confined to one copper layer or its adjacent surface.
    Layer(Symbol),
}

impl<Symbol: PartialEq> BoardArrayCopperReach<Symbol> {
    fn includes(&self, layer: &Symbol) -> bool {
        match self {
            Self::All => true,
            Self::Layer(affected) => affected == layer,
        }
    }
}

/// One disjoint copper-reach bucket from an extracted support layer.
#[derive(Debug, Clone)]
pub struct BoardArrayScopedObstacle<Symbol> {
    pub reach: BoardArrayCopperReach<Symbol>,
    pub region: ContourSet,
}

/// Parameters controlling safe-region construction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BalancingRegionOptions {
    /// Required Euclidean clearance from the panel boundary and all obstacles.
    pub clearance_mm: f64,
    /// Radius of the filled-feature opening. Surviving regions admit disks of
    /// this radius.
    pub regularization_radius_mm: f64,
    /// Radius of the rolling-disk test for two-sided void gaps. The minimum
    /// nominal gap width is twice this radius.
    pub gap_radius_mm: f64,
    /// Additional conservative allowance for numerical approximation.
    pub numerical_guard_mm: f64,
}

impl Default for BalancingRegionOptions {
    fn default() -> Self {
        Self {
            clearance_mm: DEFAULT_BALANCING_CLEARANCE_MM,
            regularization_radius_mm: DEFAULT_BALANCING_REGULARIZATION_RADIUS_MM,
            gap_radius_mm: DEFAULT_BALANCING_GAP_RADIUS_MM,
            numerical_guard_mm: DEFAULT_BALANCING_NUMERICAL_GUARD_MM,
        }
    }
}

impl BalancingRegionOptions {
    /// `clearance_mm + numerical_guard_mm`.
    pub fn construction_clearance_mm(self) -> f64 {
        self.clearance_mm + self.numerical_guard_mm
    }
}

/// Set-theoretic intermediates retained for diagnostics and validation.
#[derive(Debug, Clone)]
pub struct BoardArrayBalancingIntermediates {
    /// `board_footprints ∪ material_removal ∪ support_features`.
    pub raw_obstacles: ContourSet,
    /// `panel_outer ⊖ disk(construction_clearance)`.
    pub panel_keep_in: ContourSet,
    /// `raw_obstacles ⊕ disk(construction_clearance)`.
    pub obstacle_keep_out: ContourSet,
    /// `panel_keep_in \ obstacle_keep_out`.
    pub clearance_safe_region: ContourSet,
    /// Disk opening of `clearance_safe_region`, before void-gap regularization.
    pub opened_candidates: ContourSet,
    /// Material removed from the clearance-safe set by the disk opening alone.
    pub removed_by_opening: ContourSet,
    /// Material locally trimmed to widen two-sided void gaps. Together with
    /// `removed_by_opening` this is the total regularization removal
    /// `clearance_safe_region \ safe_region`.
    pub removed_by_gap_regularization: ContourSet,
}

/// Independent proof geometry for a computed safe region.
#[derive(Debug, Clone)]
pub struct ClearanceCertificate {
    /// Safe region dilated by the nominal requested clearance.
    pub swept_safe_region: ContourSet,
    /// Regularized safe material outside the clearance-safe set.
    pub safe_outside_clearance_region: ContourSet,
    /// `safe_region \ open(safe_region, region_radius)`, after denoising.
    pub regularization_violations: ContourSet,
    /// Two-sided components of
    /// `close(safe_region, disk(gap_radius)) \ safe_region`. Non-empty geometry
    /// proves a void gap narrower than twice the gap radius, including within
    /// one connected filled component.
    pub gap_violations: ContourSet,
    /// Nominal-clearance sweep outside the raw panel.
    pub outside_panel: ContourSet,
    /// Nominal-clearance sweep intersecting raw obstacles.
    pub obstacle_overlap: ContourSet,
}

impl ClearanceCertificate {
    /// Whether the two-sided gap set is empty and every other violation is
    /// below the supplied area tolerance.
    pub fn passes(&self, area_tolerance_mm2: f64) -> bool {
        area_tolerance_mm2.is_finite()
            && area_tolerance_mm2 >= 0.0
            && self.gap_violations.is_empty()
            && [
                &self.safe_outside_clearance_region,
                &self.regularization_violations,
                &self.outside_panel,
                &self.obstacle_overlap,
            ]
            .into_iter()
            .all(|region| region.area() <= area_tolerance_mm2)
    }
}

/// Safe region plus all data required to inspect and certify it.
#[derive(Debug, Clone)]
pub struct BoardArrayBalancingResult {
    /// Final subset whose filled features and two-sided void gaps satisfy their
    /// independently requested rolling-disk radii.
    pub safe_region: ContourSet,
    pub intermediates: BoardArrayBalancingIntermediates,
    pub certificate: ClearanceCertificate,
}

/// How an extracted array-support layer contributes physical obstacles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoardArraySupportLayerPolicy {
    /// Every painted feature is physical support geometry.
    AllPaintedFeatures,
    /// Only features referencing a specification with a `V_Cut` process item
    /// are physical; same-layer callout arrows and labels are documentation.
    VCutOperationsOnly,
}

/// One already-extracted IPC `ArraySupport` layer.
#[derive(Debug, Clone, Copy)]
pub struct BoardArraySupportDocument<'a, Symbol, LayerFunction> {
    pub document: &'a Document<Symbol, LayerFunction>,
    pub policy: BoardArraySupportLayerPolicy,
}

impl<'a, Symbol, LayerFunction> BoardArraySupportDocument<'a, Symbol, LayerFunction> {
    pub fn new(
        document: &'a Document<Symbol, LayerFunction>,
        policy: BoardArraySupportLayerPolicy,
    ) -> Self {
        Self { document, policy }
    }
}

/// Geometry and coverage accounting for one array-support layer.
#[derive(Debug, Clone)]
pub struct BoardArraySupportLayerGeometry<Symbol> {
    pub source_feature_count: usize,
    pub feature_count: usize,
    pub source_path_count: usize,
    pub path_count: usize,
    pub excluded_documentation_path_count: usize,
    pub unpainted_path_count: usize,
    /// Canonical physical geometry, partitioned by copper reach. Each included
    /// support path contributes to exactly one bucket.
    pub obstacles: Vec<BoardArrayScopedObstacle<Symbol>>,
}

impl<Symbol: Copy + PartialEq> BoardArraySupportLayerGeometry<Symbol> {
    /// Derive this source layer's physical obstacle region for one copper
    /// layer. The scoped buckets remain the sole stored geometry.
    pub fn region_for_layer(&self, layer: Symbol) -> ContourSet {
        self.obstacles
            .iter()
            .filter(|obstacle| obstacle.reach.includes(&layer))
            .fold(ContourSet::empty(tol::REGION_MM), |region, obstacle| {
                region.union(&obstacle.region)
            })
    }
}

/// IPC-derived inputs and diagnostics, before safe-region computation.
#[derive(Debug, Clone)]
pub struct BoardArrayBalancingCollection<Symbol> {
    pub panel_outer: ContourSet,
    pub board_footprints: ContourSet,
    pub material_removal: ContourSet,
    pub board_instance_count: usize,
    pub support_layers: Vec<BoardArraySupportLayerGeometry<Symbol>>,
}

impl<Symbol: Copy + PartialEq> BoardArrayBalancingCollection<Symbol> {
    /// Derive the geometry-only input for one copper layer from the canonical
    /// scoped support geometry.
    pub fn input_for_layer(&self, layer: Symbol) -> BoardArrayBalancingInput {
        let support_features = self.support_features_for_layer(layer);
        BoardArrayBalancingInput {
            panel_outer: self.panel_outer.clone(),
            board_footprints: self.board_footprints.clone(),
            material_removal: self.material_removal.clone(),
            support_features,
        }
    }

    /// Union support geometry whose physical reach includes `layer`.
    pub fn support_features_for_layer(&self, layer: Symbol) -> ContourSet {
        self.support_layers
            .iter()
            .fold(ContourSet::empty(tol::REGION_MM), |region, source| {
                region.union(&source.region_for_layer(layer))
            })
    }

    /// Whether two copper layers select the same canonical support-obstacle
    /// buckets. The remaining balancing input is common to every layer, so an
    /// equal scope produces the same safe region without comparing geometry.
    pub fn has_same_support_scope(&self, left: Symbol, right: Symbol) -> bool {
        self.support_layers
            .iter()
            .flat_map(|source| &source.obstacles)
            .all(|obstacle| obstacle.reach.includes(&left) == obstacle.reach.includes(&right))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum BalancingRegionError {
    InvalidClearance(f64),
    InvalidRegularizationRadius(f64),
    InvalidGapRadius(f64),
    InvalidNumericalGuard(f64),
    EmptyPanelOutline,
    EmptyBoardFootprints,
    EmptyAssemblyPanels,
    NotAFabricationPanel,
    UnpaintedSupportPaths(usize),
    GapRegularization(String),
}

impl fmt::Display for BalancingRegionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidClearance(value) => write!(
                f,
                "balancing-region clearance must be finite and positive; got {value}"
            ),
            Self::InvalidRegularizationRadius(value) => write!(
                f,
                "balancing-region regularization radius must be finite and positive; got {value}"
            ),
            Self::InvalidGapRadius(value) => write!(
                f,
                "balancing-region gap radius must be finite and positive; got {value}"
            ),
            Self::InvalidNumericalGuard(value) => write!(
                f,
                "balancing-region numerical guard must be finite and non-negative; got {value}"
            ),
            Self::EmptyPanelOutline => {
                write!(f, "board array has no usable root panel outline")
            }
            Self::EmptyBoardFootprints => {
                write!(f, "board array has no final board-instance profiles")
            }
            Self::EmptyAssemblyPanels => {
                write!(f, "fabrication panel has no placed assembly panels")
            }
            Self::NotAFabricationPanel => {
                write!(f, "layout root is not a fabrication panel")
            }
            Self::UnpaintedSupportPaths(count) => write!(
                f,
                "board array has {count} support paths without a physical painted footprint"
            ),
            Self::GapRegularization(message) => {
                write!(f, "could not regularize balancing-region gaps: {message}")
            }
        }
    }
}

impl std::error::Error for BalancingRegionError {}

/// Compute a clearance-safe, radius-regularized copper region.
///
/// Let `P` be [`BoardArrayBalancingInput::panel_outer`], `O` the union of the
/// three obstacle inputs, `c`
/// [`BalancingRegionOptions::construction_clearance_mm`], `q`
/// [`BalancingRegionOptions::regularization_radius_mm`], and `v`
/// [`BalancingRegionOptions::gap_radius_mm`]. The geometric stages are:
///
/// ```text
/// clearance_safe = (P ⊖ disk(c)) \ (O ⊕ disk(c))
/// candidates     = open(clearance_safe, disk(q))
/// ```
///
/// Gap regularization then repeatedly removes a radius-`v + guard` tube around
/// the boundary medial axis inside the two-sided subset of
/// `close(candidates, disk(v + guard)) \ candidates` until that subset is
/// empty. It widens inter-component gaps, hairpins, notches, and internal
/// voids locally without widening one-sided edge clearance.
pub fn board_array_balancing_region(
    input: &BoardArrayBalancingInput,
    options: BalancingRegionOptions,
) -> Result<BoardArrayBalancingResult, BalancingRegionError> {
    validate_options(options)?;
    if input.panel_outer.is_empty() {
        return Err(BalancingRegionError::EmptyPanelOutline);
    }
    if input.board_footprints.is_empty() {
        return Err(BalancingRegionError::EmptyBoardFootprints);
    }

    let raw_obstacles = input
        .board_footprints
        .union(&input.material_removal)
        .union(&input.support_features);
    let construction_clearance_mm = options.construction_clearance_mm();
    let panel_keep_in = input.panel_outer.disk_erode(construction_clearance_mm);
    let obstacle_keep_out = raw_obstacles.disk_dilate(construction_clearance_mm);
    let clearance_safe_region = panel_keep_in.difference(&obstacle_keep_out);
    let opened_candidates = clearance_safe_region.disk_open(options.regularization_radius_mm);
    let removed_by_opening = clearance_safe_region.difference(&opened_candidates);
    let gap_regularization = opened_candidates
        .disk_regularize_gaps(
            options.gap_radius_mm,
            options.regularization_radius_mm,
            options.numerical_guard_mm,
        )
        .map_err(|error| BalancingRegionError::GapRegularization(error.to_string()))?;
    let safe_region = gap_regularization.kept;
    let removed_by_gap_regularization = gap_regularization.removed;

    // Certify against the nominal requirement, independently of the
    // construction guard used above.
    let swept_safe_region = safe_region.disk_dilate(options.clearance_mm);
    let regularization_violations = safe_region
        .difference(&safe_region.disk_open(options.regularization_radius_mm))
        .disk_open(options.numerical_guard_mm);
    let gap_violations = safe_region.disk_gap_violations(options.gap_radius_mm);
    let certificate = ClearanceCertificate {
        safe_outside_clearance_region: safe_region.difference(&clearance_safe_region),
        regularization_violations,
        gap_violations,
        outside_panel: swept_safe_region.difference(&input.panel_outer),
        obstacle_overlap: swept_safe_region.intersection(&raw_obstacles),
        swept_safe_region,
    };

    Ok(BoardArrayBalancingResult {
        safe_region,
        intermediates: BoardArrayBalancingIntermediates {
            raw_obstacles,
            panel_keep_in,
            obstacle_keep_out,
            clearance_safe_region,
            opened_candidates,
            removed_by_opening,
            removed_by_gap_regularization,
        },
        certificate,
    })
}

/// Collect canonical board geometry and copper-scoped support geometry from
/// IPC layout/profile IR and already-extracted `ArraySupport` layers.
///
/// Source-file traversal and view extraction stay outside `pcb-ir`; all
/// geometry classification after extraction lives here.
pub fn collect_board_array_balancing_input<'a, Symbol, LayerFunction>(
    layout: &Document<Symbol, LayerFunction>,
    fabrication_profile: &BoardArrayFabricationProfile,
    copper_layers: &[BoardArrayCopperLayer<Symbol>],
    support_documents: impl IntoIterator<Item = BoardArraySupportDocument<'a, Symbol, LayerFunction>>,
) -> Result<BoardArrayBalancingCollection<Symbol>, BalancingRegionError>
where
    Symbol: Copy + PartialEq + 'a,
    LayerFunction: 'a,
{
    let collection = inspect_board_array_balancing_input(
        layout,
        fabrication_profile,
        copper_layers,
        support_documents,
    )?;
    let unpainted_path_count = collection
        .support_layers
        .iter()
        .map(|layer| layer.unpainted_path_count)
        .sum();
    if unpainted_path_count != 0 {
        return Err(BalancingRegionError::UnpaintedSupportPaths(
            unpainted_path_count,
        ));
    }
    Ok(collection)
}

/// Collect the geometry-only balancing input for a fabrication panel.
///
/// The domain is the caller-supplied usable region between the stock's
/// reserved process margins, so the margins never enter the density
/// denominator and stay bare. The placed assembly panels are the only
/// footprint obstacles: every cutout, score, and copper feature of a placed
/// panel lies inside its nominal outline, and the fabrication-panel step adds
/// no per-layer geometry of its own, so one input serves every copper layer.
/// Profile cutouts still ride along as material removal for completeness.
pub fn collect_fab_panel_balancing_input(
    usable_region: ContourSet,
    fabrication_profile: &BoardArrayFabricationProfile,
) -> Result<BoardArrayBalancingInput, BalancingRegionError> {
    if fabrication_profile.purpose != LayoutPurpose::FabricationPanel {
        return Err(BalancingRegionError::NotAFabricationPanel);
    }
    let panel_contours = fabrication_profile
        .assembly_panel_outlines
        .iter()
        .flatten()
        .cloned()
        .collect::<Vec<_>>();
    let board_footprints = ContourSet::from_filled_contours(&panel_contours, tol::REGION_MM);
    if board_footprints.is_empty() {
        return Err(BalancingRegionError::EmptyAssemblyPanels);
    }

    Ok(BoardArrayBalancingInput {
        panel_outer: usable_region,
        board_footprints,
        material_removal: ContourSet::from_contours(
            &fabrication_profile.material_removal,
            FillRule::NonZero,
            tol::REGION_MM,
        ),
        support_features: ContourSet::empty(tol::REGION_MM),
    })
}

/// Collect the same inputs and coverage data as
/// [`collect_board_array_balancing_input`], but retain incomplete support
/// layers for diagnostic rendering.
///
/// Production consumers should use the fail-closed collector. This inspection
/// entry point exists so a debug harness can serialize the offending geometry
/// before reporting incomplete coverage.
pub fn inspect_board_array_balancing_input<'a, Symbol, LayerFunction>(
    layout: &Document<Symbol, LayerFunction>,
    fabrication_profile: &BoardArrayFabricationProfile,
    copper_layers: &[BoardArrayCopperLayer<Symbol>],
    support_documents: impl IntoIterator<Item = BoardArraySupportDocument<'a, Symbol, LayerFunction>>,
) -> Result<BoardArrayBalancingCollection<Symbol>, BalancingRegionError>
where
    Symbol: Copy + PartialEq + 'a,
    LayerFunction: 'a,
{
    let panel_contours = fabrication_profile
        .array_outlines
        .iter()
        .flatten()
        .cloned()
        .collect::<Vec<_>>();
    let panel_outer = ContourSet::from_filled_contours(&panel_contours, tol::REGION_MM);
    if panel_outer.is_empty() {
        return Err(BalancingRegionError::EmptyPanelOutline);
    }

    let mut board_contours = Vec::new();
    let mut board_instance_count = 0;
    for occurrence in profile_occurrences_for(layout, ProfileSet::FabricationOutlines)
        .into_iter()
        .filter(|occurrence| occurrence.role == ProfileOccurrenceRole::BoardInstance)
    {
        board_instance_count += 1;
        board_contours.extend(
            layout.transformed_path_contours(occurrence.profile.outer_path, occurrence.transform),
        );
    }
    let board_footprints = ContourSet::from_filled_contours(&board_contours, tol::REGION_MM);
    if board_instance_count == 0 || board_footprints.is_empty() {
        return Err(BalancingRegionError::EmptyBoardFootprints);
    }

    let material_removal = ContourSet::from_contours(
        &fabrication_profile.material_removal,
        FillRule::NonZero,
        tol::REGION_MM,
    );
    let support_layers = support_documents
        .into_iter()
        .map(|source| collect_support_layer_geometry(source, copper_layers))
        .collect::<Vec<_>>();

    Ok(BoardArrayBalancingCollection {
        panel_outer,
        board_footprints,
        material_removal,
        board_instance_count,
        support_layers,
    })
}

fn collect_support_layer_geometry<Symbol: Copy + PartialEq, LayerFunction>(
    source: BoardArraySupportDocument<'_, Symbol, LayerFunction>,
    copper_layers: &[BoardArrayCopperLayer<Symbol>],
) -> BoardArraySupportLayerGeometry<Symbol> {
    let source_path_count = source
        .document
        .features
        .iter()
        .flat_map(|feature| feature.paths.slice(&source.document.arena.paths))
        .count();
    let features = source
        .document
        .features
        .iter()
        .filter(|feature| {
            source.policy == BoardArraySupportLayerPolicy::AllPaintedFeatures
                || is_vcut_operation_feature(source.document, feature)
        })
        .collect::<Vec<_>>();
    let paths = features
        .iter()
        .flat_map(|feature| feature.paths.slice(&source.document.arena.paths))
        .collect::<Vec<_>>();
    let unpainted_path_count = paths
        .iter()
        .filter(|path| matches!(path.paint, Paint::None))
        .count();
    let mut scoped_features: Vec<(BoardArrayCopperReach<Symbol>, Vec<&Feature<Symbol>>)> =
        Vec::new();
    for feature in &features {
        let reach = copper_reach(feature, copper_layers);
        if let Some((_, grouped)) = scoped_features
            .iter_mut()
            .find(|(candidate, _)| *candidate == reach)
        {
            grouped.push(feature);
        } else {
            scoped_features.push((reach, vec![feature]));
        }
    }
    let obstacles = scoped_features
        .into_iter()
        .map(|(reach, features)| {
            let region = ContourSet::from_painted_paths(
                &source.document.arena,
                features
                    .into_iter()
                    .flat_map(|feature| feature.paths.slice(&source.document.arena.paths)),
                tol::REGION_MM,
            );
            BoardArrayScopedObstacle { reach, region }
        })
        .collect();

    BoardArraySupportLayerGeometry {
        source_feature_count: source.document.features.len(),
        feature_count: features.len(),
        source_path_count,
        path_count: paths.len(),
        excluded_documentation_path_count: source_path_count.saturating_sub(paths.len()),
        unpainted_path_count,
        obstacles,
    }
}

/// Resolve physical feature span to copper reach without recognizing feature
/// shapes or roles. Exact copper-layer spans stay local, surface geometry maps
/// to the corresponding outer copper, and every unresolved/through span is
/// conservatively stack-wide.
fn copper_reach<Symbol: Copy + PartialEq>(
    feature: &Feature<Symbol>,
    copper_layers: &[BoardArrayCopperLayer<Symbol>],
) -> BoardArrayCopperReach<Symbol> {
    if let FeatureSpan::Layer(layer) = feature.intent.span
        && copper_layers.iter().any(|copper| copper.name == layer)
    {
        return BoardArrayCopperReach::Layer(layer);
    }

    let side = feature.intent.side;
    if matches!(
        feature.intent.span,
        FeatureSpan::Layer(_) | FeatureSpan::Unknown
    ) && matches!(side, Side::Top | Side::Bottom)
        && let Some(copper) = copper_layers.iter().find(|copper| copper.side == side)
    {
        return BoardArrayCopperReach::Layer(copper.name);
    }

    BoardArrayCopperReach::All
}

fn validate_options(options: BalancingRegionOptions) -> Result<(), BalancingRegionError> {
    if !options.clearance_mm.is_finite() || options.clearance_mm <= 0.0 {
        return Err(BalancingRegionError::InvalidClearance(options.clearance_mm));
    }
    if !options.regularization_radius_mm.is_finite() || options.regularization_radius_mm <= 0.0 {
        return Err(BalancingRegionError::InvalidRegularizationRadius(
            options.regularization_radius_mm,
        ));
    }
    if !options.gap_radius_mm.is_finite() || options.gap_radius_mm <= 0.0 {
        return Err(BalancingRegionError::InvalidGapRadius(
            options.gap_radius_mm,
        ));
    }
    if !options.numerical_guard_mm.is_finite() || options.numerical_guard_mm < 0.0 {
        return Err(BalancingRegionError::InvalidNumericalGuard(
            options.numerical_guard_mm,
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialects::ipc::{
        Feature, FeatureDomain, FeatureKind, FeatureRole, FeatureSet, LayoutInstance,
        LayoutPurpose, LayoutStep, LayoutStepKind, Spec, SpecItem, SpecItemKind, SpecRef,
        StepProfile,
    };
    use crate::geom::{
        Affine2, BBox, ContourBuf, LineCap, Paint, PathCmd, Point, Polarity, Span, StrokeStyle,
    };

    type TestDocument = Document<u32, ()>;

    #[test]
    fn computes_and_certifies_safe_region() {
        let input = balancing_input(0.0, ContourSet::empty(tol::REGION_MM));

        let result =
            board_array_balancing_region(&input, BalancingRegionOptions::default()).unwrap();

        assert!(!result.safe_region.is_empty());
        assert!(
            result.certificate.passes(1e-4),
            "outside clearance-safe {:.9}, filled-feature violations {:.9}, void-gap violations {:.9}, outside panel {:.9}, obstacle overlap {:.9}",
            result.certificate.safe_outside_clearance_region.area(),
            result.certificate.regularization_violations.area(),
            result.certificate.gap_violations.area(),
            result.certificate.outside_panel.area(),
            result.certificate.obstacle_overlap.area(),
        );
        assert!(
            result
                .safe_region
                .difference(&result.intermediates.clearance_safe_region)
                .is_empty()
        );
        assert!(
            result
                .intermediates
                .clearance_safe_region
                .difference(&result.safe_region)
                .area()
                > 0.0
        );
        assert!(
            result
                .safe_region
                .difference(&result.intermediates.panel_keep_in)
                .is_empty()
        );
        assert!(
            result
                .safe_region
                .intersection(&result.intermediates.obstacle_keep_out)
                .is_empty()
        );
    }

    #[test]
    fn larger_clearance_shrinks_clearance_safe_region_for_simple_fixture() {
        let input = balancing_input(0.0, ContourSet::empty(tol::REGION_MM));
        let smaller = board_array_balancing_region(
            &input,
            BalancingRegionOptions {
                clearance_mm: 0.25,
                regularization_radius_mm: 0.5,
                gap_radius_mm: 0.5,
                numerical_guard_mm: 0.0,
            },
        )
        .unwrap();
        let larger = board_array_balancing_region(
            &input,
            BalancingRegionOptions {
                clearance_mm: 1.0,
                regularization_radius_mm: 0.5,
                gap_radius_mm: 0.5,
                numerical_guard_mm: 0.0,
            },
        )
        .unwrap();

        let larger_outside_smaller = larger
            .intermediates
            .clearance_safe_region
            .difference(&smaller.intermediates.clearance_safe_region);
        assert!(
            larger_outside_smaller.is_empty(),
            "larger clearance added {:.9} mm² to the maximal region",
            larger_outside_smaller.area()
        );
        assert!(
            larger.intermediates.clearance_safe_region.area()
                < smaller.intermediates.clearance_safe_region.area()
        );
    }

    #[test]
    fn larger_regularization_disk_shrinks_opened_region_for_simple_fixture() {
        let input = balancing_input(0.0, ContourSet::empty(tol::REGION_MM));
        let smaller = board_array_balancing_region(
            &input,
            BalancingRegionOptions {
                clearance_mm: 0.5,
                regularization_radius_mm: 0.25,
                gap_radius_mm: 0.5,
                numerical_guard_mm: 0.0,
            },
        )
        .unwrap();
        let larger = board_array_balancing_region(
            &input,
            BalancingRegionOptions {
                clearance_mm: 0.5,
                regularization_radius_mm: 1.0,
                gap_radius_mm: 0.5,
                numerical_guard_mm: 0.0,
            },
        )
        .unwrap();

        let larger_outside_smaller = larger
            .intermediates
            .opened_candidates
            .difference(&smaller.intermediates.opened_candidates);
        assert!(
            larger_outside_smaller.is_empty(),
            "larger feature disk added {:.9} mm² to the opened region",
            larger_outside_smaller.area()
        );
        assert!(
            larger.intermediates.opened_candidates.area()
                < smaller.intermediates.opened_candidates.area()
        );
    }

    #[test]
    fn regularization_does_not_inflate_obstacle_clearance() {
        let input = BoardArrayBalancingInput {
            panel_outer: ContourSet::rectangle(bbox(0.0, 0.0, 30.0, 20.0), tol::REGION_MM),
            board_footprints: ContourSet::rectangle(bbox(2.0, 2.0, 4.0, 4.0), tol::REGION_MM),
            material_removal: ContourSet::empty(tol::REGION_MM),
            support_features: ContourSet::rectangle(bbox(14.95, 0.0, 15.05, 20.0), tol::REGION_MM),
        };
        let result = board_array_balancing_region(
            &input,
            BalancingRegionOptions {
                clearance_mm: 0.5,
                regularization_radius_mm: 1.0,
                gap_radius_mm: 0.5,
                numerical_guard_mm: 0.025,
            },
        )
        .unwrap();

        let mut components = result
            .intermediates
            .clearance_safe_region
            .connected_components();
        components.sort_by(|left, right| left.bbox.min.x.total_cmp(&right.bbox.min.x));
        assert_eq!(components.len(), 2);
        let gap = components[1].bbox.min.x - components[0].bbox.max.x;
        assert!(
            (gap - 1.15).abs() <= 0.01,
            "expected physical stroke plus two 0.525 mm clearances, got {gap:.9} mm"
        );
        assert_eq!(result.safe_region.connected_components().len(), 2);
        assert!(
            result.intermediates.removed_by_gap_regularization.area() <= 1e-4,
            "unexpected gap trimming {:.9} mm²",
            result.intermediates.removed_by_gap_regularization.area(),
        );
        assert!(result.certificate.gap_violations.is_empty());
    }

    #[test]
    fn adding_an_obstacle_shrinks_clearance_safe_region_for_simple_fixture() {
        let baseline_input = balancing_input(0.0, ContourSet::empty(tol::REGION_MM));
        let added_obstacle = ContourSet::rectangle(bbox(10.0, 1.0, 11.0, 9.0), tol::REGION_MM);
        let blocked_input = balancing_input(0.0, added_obstacle);
        let options = BalancingRegionOptions::default();

        let baseline = board_array_balancing_region(&baseline_input, options).unwrap();
        let blocked = board_array_balancing_region(&blocked_input, options).unwrap();

        assert!(
            blocked
                .intermediates
                .clearance_safe_region
                .difference(&baseline.intermediates.clearance_safe_region)
                .is_empty()
        );
        assert!(
            blocked.intermediates.clearance_safe_region.area()
                < baseline.intermediates.clearance_safe_region.area()
        );
    }

    #[test]
    fn translation_preserves_safe_region_area_and_certificate() {
        let original = balancing_input(0.0, ContourSet::empty(tol::REGION_MM));
        let translated = balancing_input(37.0, ContourSet::empty(tol::REGION_MM));

        let original =
            board_array_balancing_region(&original, BalancingRegionOptions::default()).unwrap();
        let translated =
            board_array_balancing_region(&translated, BalancingRegionOptions::default()).unwrap();

        assert!((translated.safe_region.area() - original.safe_region.area()).abs() <= 1e-6);
        assert!(
            (translated.safe_region.bbox.min.x - original.safe_region.bbox.min.x - 37.0).abs()
                <= 1e-9
        );
        assert!(
            original.certificate.passes(1e-4),
            "original outside clearance-safe {:.9}, outside panel {:.9}, obstacle overlap {:.9}",
            original.certificate.safe_outside_clearance_region.area(),
            original.certificate.outside_panel.area(),
            original.certificate.obstacle_overlap.area(),
        );
        assert!(
            translated.certificate.passes(1e-4),
            "translated outside clearance-safe {:.9}, outside panel {:.9}, obstacle overlap {:.9}",
            translated.certificate.safe_outside_clearance_region.area(),
            translated.certificate.outside_panel.area(),
            translated.certificate.obstacle_overlap.area(),
        );
    }

    #[test]
    fn an_empty_safe_region_is_valid() {
        let panel = ContourSet::rectangle(bbox(0.0, 0.0, 10.0, 10.0), tol::REGION_MM);
        let input = BoardArrayBalancingInput {
            panel_outer: panel.clone(),
            board_footprints: panel,
            material_removal: ContourSet::empty(tol::REGION_MM),
            support_features: ContourSet::empty(tol::REGION_MM),
        };

        let result =
            board_array_balancing_region(&input, BalancingRegionOptions::default()).unwrap();

        assert!(result.safe_region.is_empty());
        assert!(result.certificate.passes(0.0));
    }

    #[test]
    fn rejects_invalid_options_and_required_empty_inputs() {
        let input = balancing_input(0.0, ContourSet::empty(tol::REGION_MM));
        assert_eq!(
            board_array_balancing_region(
                &input,
                BalancingRegionOptions {
                    clearance_mm: 0.0,
                    regularization_radius_mm: 0.5,
                    gap_radius_mm: 0.5,
                    numerical_guard_mm: 0.0,
                }
            )
            .unwrap_err(),
            BalancingRegionError::InvalidClearance(0.0)
        );
        assert_eq!(
            board_array_balancing_region(
                &input,
                BalancingRegionOptions {
                    clearance_mm: 0.5,
                    regularization_radius_mm: 0.0,
                    gap_radius_mm: 0.5,
                    numerical_guard_mm: 0.0,
                }
            )
            .unwrap_err(),
            BalancingRegionError::InvalidRegularizationRadius(0.0)
        );
        assert_eq!(
            board_array_balancing_region(
                &input,
                BalancingRegionOptions {
                    clearance_mm: 0.5,
                    regularization_radius_mm: 1.0,
                    gap_radius_mm: 0.0,
                    numerical_guard_mm: 0.0,
                }
            )
            .unwrap_err(),
            BalancingRegionError::InvalidGapRadius(0.0)
        );
        assert_eq!(
            board_array_balancing_region(
                &input,
                BalancingRegionOptions {
                    clearance_mm: 0.5,
                    regularization_radius_mm: 0.5,
                    gap_radius_mm: 0.5,
                    numerical_guard_mm: -0.1,
                }
            )
            .unwrap_err(),
            BalancingRegionError::InvalidNumericalGuard(-0.1)
        );

        let mut empty_panel = input.clone();
        empty_panel.panel_outer = ContourSet::empty(tol::REGION_MM);
        assert_eq!(
            board_array_balancing_region(&empty_panel, BalancingRegionOptions::default())
                .unwrap_err(),
            BalancingRegionError::EmptyPanelOutline
        );

        let mut empty_boards = input;
        empty_boards.board_footprints = ContourSet::empty(tol::REGION_MM);
        assert_eq!(
            board_array_balancing_region(&empty_boards, BalancingRegionOptions::default())
                .unwrap_err(),
            BalancingRegionError::EmptyBoardFootprints
        );
    }

    #[test]
    fn collector_derives_panel_boards_material_removal_and_support() {
        let (layout, profile) = layout_and_profile();
        let mut support = TestDocument::new();
        let path = support.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            [rectangle_contour(1.0, 1.0, 2.0, 2.0)],
        );
        let mut feature = Feature::new(FeatureKind::Primitive, Polarity::Dark);
        feature.paths = Span::single(path);
        feature.intent.span = FeatureSpan::Layer(100);
        support.features.push(feature);

        let copper_layers = [
            BoardArrayCopperLayer::new(100, Side::Top),
            BoardArrayCopperLayer::new(200, Side::Inner),
            BoardArrayCopperLayer::new(300, Side::Inner),
        ];

        let collection = collect_board_array_balancing_input(
            &layout,
            &profile,
            &copper_layers,
            [BoardArraySupportDocument::new(
                &support,
                BoardArraySupportLayerPolicy::AllPaintedFeatures,
            )],
        )
        .unwrap();

        assert_eq!(collection.board_instance_count, 1);
        assert_eq!(collection.support_layers.len(), 1);
        assert_eq!(collection.support_layers[0].path_count, 1);
        assert!((collection.panel_outer.area() - 200.0).abs() <= 1e-6);
        assert!((collection.board_footprints.area() - 12.0).abs() <= 1e-6);
        assert!((collection.material_removal.area() - 1.0).abs() <= 1e-6);
        assert!((collection.support_features_for_layer(100).area() - 1.0).abs() <= 1e-6);
        assert!(collection.support_features_for_layer(200).is_empty());
        assert!(!collection.has_same_support_scope(100, 200));
        assert!(collection.has_same_support_scope(200, 300));
    }

    #[test]
    fn support_geometry_follows_ir_feature_span_and_surface_side() {
        let mut support = TestDocument::new();
        let top_surface_path = support.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            [rectangle_contour(1.0, 1.0, 2.0, 2.0)],
        );
        let through_path = support.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            [rectangle_contour(4.0, 1.0, 5.0, 2.0)],
        );
        let bottom_copper_path = support.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            [rectangle_contour(7.0, 1.0, 8.0, 2.0)],
        );

        let mut top_surface = Feature::new(FeatureKind::Primitive, Polarity::Dark);
        top_surface.paths = Span::single(top_surface_path);
        top_surface.intent.span = FeatureSpan::Layer(900);
        top_surface.intent.side = Side::Top;
        support.features.push(top_surface);

        let mut through = Feature::new(FeatureKind::Hole, Polarity::Dark);
        through.paths = Span::single(through_path);
        through.intent.span = FeatureSpan::ThroughBoard;
        support.features.push(through);

        let mut bottom_copper = Feature::new(FeatureKind::Primitive, Polarity::Dark);
        bottom_copper.paths = Span::single(bottom_copper_path);
        bottom_copper.intent.span = FeatureSpan::Layer(200);
        bottom_copper.intent.side = Side::Bottom;
        support.features.push(bottom_copper);

        let copper_layers = [
            BoardArrayCopperLayer::new(100, Side::Top),
            BoardArrayCopperLayer::new(200, Side::Bottom),
        ];
        let geometry = collect_support_layer_geometry(
            BoardArraySupportDocument::new(
                &support,
                BoardArraySupportLayerPolicy::AllPaintedFeatures,
            ),
            &copper_layers,
        );
        let top = geometry.region_for_layer(100);
        let bottom = geometry.region_for_layer(200);

        assert!((top.area() - 2.0).abs() <= 1e-6);
        assert!((bottom.area() - 2.0).abs() <= 1e-6);
        assert!(
            top.intersection(&ContourSet::rectangle(
                bbox(7.0, 1.0, 8.0, 2.0),
                tol::REGION_MM
            ))
            .is_empty()
        );
        assert!(
            bottom
                .intersection(&ContourSet::rectangle(
                    bbox(1.0, 1.0, 2.0, 2.0),
                    tol::REGION_MM
                ))
                .is_empty()
        );
        assert!(
            (top.intersection(&bottom).area() - 1.0).abs() <= 1e-6,
            "only through-stack geometry should be shared"
        );
    }

    #[test]
    fn vcut_policy_excludes_documentation_but_keeps_operation_geometry() {
        let mut support = TestDocument::new();
        let operation_path = support.push_path(
            Paint::Stroke(StrokeStyle::new(0.1, LineCap::Round)),
            [line_contour(0.0, 0.0, 10.0, 0.0)],
        );
        let annotation_path = support.push_path(
            Paint::Stroke(StrokeStyle::new(0.1, LineCap::Round)),
            [line_contour(0.0, 2.0, 10.0, 2.0)],
        );
        support.spec_items.push(SpecItem {
            element: 1,
            kind: SpecItemKind::VCut,
            item_type: None,
            comment: None,
            properties: Span::EMPTY,
        });
        support.specs.push(Spec {
            name: 10,
            items: Span::single(0),
        });
        support.spec_refs.push(SpecRef { spec: 10 });
        support.feature_sets.push(FeatureSet {
            layer: 0,
            source_set_index: 0,
            source_geometry_ref: None,
            net: None,
            polarity: Polarity::Dark,
            spec_refs: Span::single(0),
            features: Span::single(0),
            bbox: BBox::empty(),
        });
        support.feature_sets.push(FeatureSet {
            layer: 0,
            source_set_index: 1,
            source_geometry_ref: None,
            net: None,
            polarity: Polarity::Dark,
            spec_refs: Span::EMPTY,
            features: Span::single(1),
            bbox: BBox::empty(),
        });
        support.features.push(vcut_feature(operation_path, 0));
        support.features.push(vcut_feature(annotation_path, 1));

        let geometry = collect_support_layer_geometry(
            BoardArraySupportDocument::new(
                &support,
                BoardArraySupportLayerPolicy::VCutOperationsOnly,
            ),
            &[BoardArrayCopperLayer::new(100, Side::Top)],
        );

        assert_eq!(geometry.source_feature_count, 2);
        assert_eq!(geometry.feature_count, 1);
        assert_eq!(geometry.source_path_count, 2);
        assert_eq!(geometry.path_count, 1);
        assert_eq!(geometry.excluded_documentation_path_count, 1);
        assert!(geometry.region_for_layer(100).bbox.max.y < 1.0);
    }

    #[test]
    fn fab_panel_collector_uses_assembly_panels_as_the_only_footprints() {
        let usable_region = ContourSet::rectangle(bbox(10.0, 10.0, 90.0, 60.0), tol::REGION_MM);
        let profile = BoardArrayFabricationProfile {
            purpose: LayoutPurpose::FabricationPanel,
            array_outlines: vec![vec![rectangle_contour(0.0, 0.0, 100.0, 70.0)]],
            assembly_panel_outlines: vec![
                vec![rectangle_contour(15.0, 15.0, 45.0, 55.0)],
                vec![rectangle_contour(50.0, 15.0, 85.0, 55.0)],
            ],
            material_removal: vec![rectangle_contour(20.0, 20.0, 22.0, 22.0)],
        };

        let input = collect_fab_panel_balancing_input(usable_region.clone(), &profile).unwrap();

        assert!((input.panel_outer.area() - usable_region.area()).abs() <= 1e-9);
        assert!((input.board_footprints.area() - (30.0 * 40.0 + 35.0 * 40.0)).abs() <= 1e-6);
        assert!((input.material_removal.area() - 4.0).abs() <= 1e-6);
        assert!(input.support_features.is_empty());

        let result =
            board_array_balancing_region(&input, BalancingRegionOptions::default()).unwrap();
        assert!(result.certificate.passes(1e-4));
        assert!(!result.safe_region.is_empty());
        assert!(
            result
                .safe_region
                .intersection(&input.board_footprints)
                .is_empty()
        );
        assert!(result.safe_region.difference(&usable_region).is_empty());
    }

    #[test]
    fn fab_panel_collector_rejects_wrong_purpose_and_missing_panels() {
        let usable_region = ContourSet::rectangle(bbox(0.0, 0.0, 50.0, 50.0), tol::REGION_MM);
        let mut profile = BoardArrayFabricationProfile {
            purpose: LayoutPurpose::Product,
            array_outlines: vec![vec![rectangle_contour(0.0, 0.0, 60.0, 60.0)]],
            assembly_panel_outlines: vec![vec![rectangle_contour(5.0, 5.0, 25.0, 25.0)]],
            material_removal: Vec::new(),
        };

        assert_eq!(
            collect_fab_panel_balancing_input(usable_region.clone(), &profile).unwrap_err(),
            BalancingRegionError::NotAFabricationPanel
        );

        profile.purpose = LayoutPurpose::FabricationPanel;
        profile.assembly_panel_outlines.clear();
        assert_eq!(
            collect_fab_panel_balancing_input(usable_region, &profile).unwrap_err(),
            BalancingRegionError::EmptyAssemblyPanels
        );
    }

    #[test]
    fn collector_fails_closed_on_unpainted_support_geometry() {
        let (layout, profile) = layout_and_profile();
        let mut support = TestDocument::new();
        let path = support.push_path(Paint::None, [rectangle_contour(1.0, 1.0, 2.0, 2.0)]);
        support.features.push(Feature {
            paths: Span::single(path),
            ..Feature::new(FeatureKind::Primitive, Polarity::Dark)
        });

        let error = collect_board_array_balancing_input(
            &layout,
            &profile,
            &[BoardArrayCopperLayer::new(100, Side::Top)],
            [BoardArraySupportDocument::new(
                &support,
                BoardArraySupportLayerPolicy::AllPaintedFeatures,
            )],
        )
        .unwrap_err();

        assert_eq!(error, BalancingRegionError::UnpaintedSupportPaths(1));
    }

    fn balancing_input(offset_x: f64, support_features: ContourSet) -> BoardArrayBalancingInput {
        BoardArrayBalancingInput {
            panel_outer: ContourSet::rectangle(
                bbox(offset_x, 0.0, offset_x + 20.0, 10.0),
                tol::REGION_MM,
            ),
            board_footprints: ContourSet::rectangle(
                bbox(offset_x + 2.0, 2.0, offset_x + 8.0, 8.0),
                tol::REGION_MM,
            ),
            material_removal: ContourSet::rectangle(
                bbox(offset_x + 17.0, 4.0, offset_x + 18.0, 6.0),
                tol::REGION_MM,
            ),
            support_features,
        }
    }

    fn layout_and_profile() -> (TestDocument, BoardArrayFabricationProfile) {
        let mut layout = TestDocument::new();
        let panel_path = layout.push_path(Paint::None, [rectangle_contour(0.0, 0.0, 20.0, 10.0)]);
        let board_path = layout.push_path(Paint::None, [rectangle_contour(0.0, 0.0, 4.0, 3.0)]);
        layout.profiles.push(StepProfile {
            outer_path: panel_path,
            cutouts: Span::EMPTY,
            bbox: bbox(0.0, 0.0, 20.0, 10.0),
        });
        layout.profiles.push(StepProfile {
            outer_path: board_path,
            cutouts: Span::EMPTY,
            bbox: bbox(0.0, 0.0, 4.0, 3.0),
        });
        layout.layout.root_step = Some(0);
        layout.layout.steps.push(LayoutStep {
            source_step_ref: 1,
            kind: LayoutStepKind::Panel,
            purpose: LayoutPurpose::Product,
            datum: Point::default(),
            profiles: Span::single(0),
            bbox: bbox(0.0, 0.0, 20.0, 10.0),
        });
        layout.layout.steps.push(LayoutStep {
            source_step_ref: 2,
            kind: LayoutStepKind::Board,
            purpose: LayoutPurpose::Product,
            datum: Point::default(),
            profiles: Span::single(1),
            bbox: bbox(0.0, 0.0, 4.0, 3.0),
        });
        layout.layout.instances.push(LayoutInstance {
            repeat: 0,
            parent_instance: None,
            child_step: 1,
            source_step_ref: 2,
            parent_step_ref: 1,
            transform: Affine2::translation(Point::new(3.0, 2.0)),
            repeat_index_x: 0,
            repeat_index_y: 0,
            repeat_count_x: 1,
            repeat_count_y: 1,
            repeat_pitch_x: 0.0,
            repeat_pitch_y: 0.0,
            bbox: bbox(3.0, 2.0, 7.0, 5.0),
        });

        let profile = BoardArrayFabricationProfile {
            purpose: Default::default(),
            array_outlines: vec![vec![rectangle_contour(0.0, 0.0, 20.0, 10.0)]],
            assembly_panel_outlines: Vec::new(),
            material_removal: vec![rectangle_contour(18.0, 4.0, 19.0, 5.0)],
        };
        (layout, profile)
    }

    fn vcut_feature(path: u32, set: u32) -> Feature<u32> {
        let mut feature = Feature::new(FeatureKind::Trace, Polarity::Dark);
        feature.intent.domain = FeatureDomain::VCut;
        feature.intent.role = FeatureRole::ArraySeparation;
        feature.paths = Span::single(path);
        feature.set = Some(set);
        feature
    }

    fn bbox(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> BBox {
        BBox {
            min: Point::new(min_x, min_y),
            max: Point::new(max_x, max_y),
        }
    }

    fn rectangle_contour(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> ContourBuf {
        ContourBuf::new(vec![
            PathCmd::move_to(Point::new(min_x, min_y)),
            PathCmd::line_to(Point::new(max_x, min_y)),
            PathCmd::line_to(Point::new(max_x, max_y)),
            PathCmd::line_to(Point::new(min_x, max_y)),
            PathCmd::close(),
        ])
    }

    fn line_contour(start_x: f64, start_y: f64, end_x: f64, end_y: f64) -> ContourBuf {
        ContourBuf::new(vec![
            PathCmd::move_to(Point::new(start_x, start_y)),
            PathCmd::line_to(Point::new(end_x, end_y)),
        ])
    }
}
