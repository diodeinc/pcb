//! Safe copper-balancing regions for board arrays.
//!
//! This module separates IPC semantic collection from planar set operations:
//! callers provide the canonical layout document, its fabrication profile, and
//! the already-extracted `ArraySupport` layer documents. The resulting input
//! is then handled entirely as regularized [`ContourSet`] geometry.
//!
//! For panel material `P`, obstacle union `O`, construction gap radius `g`,
//! minimum-feature radius `q`, and minimum-gap radius `v`, the construction is:
//!
//! ```text
//! M = (P ⊖ disk(g)) \ (O ⊕ disk(g))
//! A = ((M ⊖ disk(q)) ⊕ disk(q)) ∩ M
//! ```
//!
//! `M` is the clearance-safe set. Treating the outside of `P` and `O` as one
//! forbidden phase prevents narrow diagonal gaps where an obstacle meets the
//! panel edge. `A` is a disk opening: the union of all radius-`q` disks that
//! fit in `M`, which removes copper slivers and rounds outward corners.
//!
//! Finally, the connected components of `A` form a proximity graph with an
//! edge when `distance(C_i, C_j) < 2v`. The exact maximum-area independent set
//! is retained. Thus distinct output components are separated by at least `2v`
//! while preserving the greatest possible area from the opened components,
//! without feature-specific rules or traversal-order policy.

use std::fmt;

use crate::dialects::ipc::{
    BoardArrayFabricationProfile, Document, ProfileOccurrenceRole, ProfileSet,
    profile_occurrences_for, relief::is_vcut_operation_feature,
};
use crate::geom::{ContourSet, FillRule, Paint, tol};

/// Default Euclidean clearance from every protected feature.
pub const DEFAULT_BALANCING_CLEARANCE_MM: f64 = 0.5;

/// Default rolling-disk radius; surviving copper can accommodate this disk.
pub const DEFAULT_BALANCING_MINIMUM_FEATURE_RADIUS_MM: f64 = 1.0;

/// Half the default minimum Euclidean gap between distinct safe components.
pub const DEFAULT_BALANCING_MINIMUM_GAP_RADIUS_MM: f64 = 1.0;

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
    /// Painted physical geometry from every `ArraySupport` layer.
    pub support_features: ContourSet,
}

/// Parameters controlling safe-region construction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BalancingRegionOptions {
    /// Required Euclidean clearance from the panel boundary and all obstacles.
    pub clearance_mm: f64,
    /// Radius `q` of the disk opening. The opened result is the union of all
    /// radius-`q` disks contained in the pre-regularized safe set.
    pub minimum_feature_radius_mm: f64,
    /// Gap radius `v`; distinct final components must be at least `2v` apart.
    pub minimum_gap_radius_mm: f64,
    /// Additional conservative allowance for numerical approximation.
    pub numerical_guard_mm: f64,
}

impl Default for BalancingRegionOptions {
    fn default() -> Self {
        Self {
            clearance_mm: DEFAULT_BALANCING_CLEARANCE_MM,
            minimum_feature_radius_mm: DEFAULT_BALANCING_MINIMUM_FEATURE_RADIUS_MM,
            minimum_gap_radius_mm: DEFAULT_BALANCING_MINIMUM_GAP_RADIUS_MM,
            numerical_guard_mm: DEFAULT_BALANCING_NUMERICAL_GUARD_MM,
        }
    }
}

impl BalancingRegionOptions {
    /// `clearance_mm + numerical_guard_mm`.
    pub fn construction_clearance_mm(self) -> f64 {
        self.clearance_mm + self.numerical_guard_mm
    }

    /// `max(clearance_mm, minimum_gap_radius_mm) + numerical_guard_mm`.
    ///
    /// Applying the same radius as a panel erosion and obstacle dilation makes
    /// the entire forbidden phase obey one construction-gap rule.
    pub fn construction_gap_radius_mm(self) -> f64 {
        self.clearance_mm.max(self.minimum_gap_radius_mm) + self.numerical_guard_mm
    }
}

/// Set-theoretic intermediates retained for diagnostics and validation.
#[derive(Debug, Clone)]
pub struct BoardArrayBalancingIntermediates {
    /// `board_footprints ∪ material_removal ∪ support_features`.
    pub raw_obstacles: ContourSet,
    /// `panel_outer ⊖ disk(construction_clearance)`.
    pub panel_clearance_keep_in: ContourSet,
    /// `panel_outer ⊖ disk(construction_gap_radius)`.
    pub panel_keep_in: ContourSet,
    /// `raw_obstacles ⊕ disk(construction_clearance)`.
    pub obstacle_clearance: ContourSet,
    /// `raw_obstacles ⊕ disk(construction_gap_radius)`.
    pub obstacle_gap_envelope: ContourSet,
    /// `panel_keep_in \ obstacle_gap_envelope`.
    pub maximal_safe_region: ContourSet,
    /// `maximal_safe_region ⊖ disk(minimum_feature_radius)`.
    pub regularization_core: ContourSet,
    /// Disk opening of `maximal_safe_region`, before component separation.
    pub opened_safe_region: ContourSet,
    /// Material removed from the maximal set by the disk opening alone.
    pub removed_by_opening: ContourSet,
    /// Whole components excluded by the maximum-area independent set.
    pub removed_by_gap_separation: ContourSet,
    /// Total material removed by opening and component separation.
    pub removed_by_regularization: ContourSet,
}

/// Independent proof geometry for a computed safe region.
#[derive(Debug, Clone)]
pub struct ClearanceCertificate {
    /// Safe region dilated by the nominal requested clearance.
    pub swept_safe_region: ContourSet,
    /// Regularized safe material outside the maximal pre-regularization set.
    pub safe_outside_maximal_region: ContourSet,
    /// Internal consistency violation: safe material outside `panel_keep_in`.
    pub safe_outside_keep_in: ContourSet,
    /// Internal consistency violation: safe material inside cleared obstacles.
    pub safe_inside_obstacle_clearance: ContourSet,
    /// Internal consistency violation: safe material inside the minimum-gap
    /// obstacle envelope.
    pub safe_inside_obstacle_gap_envelope: ContourSet,
    /// `safe_region \ open(safe_region, q)`, after numerical denoising.
    pub minimum_feature_violations: ContourSet,
    /// `((C_i ⊕ disk(v)) ∩ (C_j ⊕ disk(v))) \ safe_region`, unioned over
    /// distinct components. Non-empty geometry proves `distance(C_i, C_j) <
    /// 2v` within polygon tolerance.
    pub minimum_gap_violations: ContourSet,
    /// Broader diagnostic geometry that disk closing would fill. This includes
    /// narrow notches in a single component as well as inter-component gaps,
    /// so it is retained for inspection but is not an inter-component
    /// separation failure.
    pub void_closing_additions: ContourSet,
    /// Nominal-clearance sweep outside the raw panel.
    pub outside_panel: ContourSet,
    /// Nominal-clearance sweep intersecting raw obstacles.
    pub obstacle_overlap: ContourSet,
}

impl ClearanceCertificate {
    /// Whether all required subset, clearance, rolling-disk, and pairwise-gap
    /// violations are below the supplied area tolerance.
    ///
    /// [`Self::void_closing_additions`] is intentionally diagnostic-only:
    /// closing also fills concave notches within a single component, whereas
    /// the enforced gap contract concerns distinct components.
    pub fn passes(&self, area_tolerance_mm2: f64) -> bool {
        area_tolerance_mm2.is_finite()
            && area_tolerance_mm2 >= 0.0
            && [
                &self.safe_outside_maximal_region,
                &self.safe_outside_keep_in,
                &self.safe_inside_obstacle_clearance,
                &self.safe_inside_obstacle_gap_envelope,
                &self.minimum_feature_violations,
                &self.minimum_gap_violations,
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
    /// Final subset whose local copper scale is at least the rolling-disk scale
    /// and whose distinct components are separated by the requested gap.
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
pub struct BoardArraySupportLayerGeometry {
    pub source_feature_count: usize,
    pub feature_count: usize,
    pub source_path_count: usize,
    pub path_count: usize,
    pub excluded_documentation_path_count: usize,
    pub unpainted_path_count: usize,
    pub region: ContourSet,
}

/// IPC-derived inputs and diagnostics, before safe-region computation.
#[derive(Debug, Clone)]
pub struct BoardArrayBalancingCollection {
    pub input: BoardArrayBalancingInput,
    pub board_instance_count: usize,
    pub support_layers: Vec<BoardArraySupportLayerGeometry>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BalancingRegionError {
    InvalidClearance(f64),
    InvalidMinimumFeatureRadius(f64),
    InvalidMinimumGapRadius(f64),
    InvalidNumericalGuard(f64),
    EmptyPanelOutline,
    EmptyBoardFootprints,
    UnpaintedSupportPaths(usize),
}

impl fmt::Display for BalancingRegionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidClearance(value) => write!(
                f,
                "balancing-region clearance must be finite and positive; got {value}"
            ),
            Self::InvalidMinimumFeatureRadius(value) => write!(
                f,
                "balancing-region minimum feature radius must be finite and positive; got {value}"
            ),
            Self::InvalidMinimumGapRadius(value) => write!(
                f,
                "balancing-region minimum gap radius must be finite and positive; got {value}"
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
            Self::UnpaintedSupportPaths(count) => write!(
                f,
                "board array has {count} support paths without a physical painted footprint"
            ),
        }
    }
}

impl std::error::Error for BalancingRegionError {}

/// Compute a clearance-safe, minimum-feature, minimum-gap copper region.
///
/// Let `P` be [`BoardArrayBalancingInput::panel_outer`], `O` the union of the
/// three obstacle inputs, `g` [`BalancingRegionOptions::construction_gap_radius_mm`],
/// `q` [`BalancingRegionOptions::minimum_feature_radius_mm`], and `v`
/// [`BalancingRegionOptions::minimum_gap_radius_mm`]. The geometric stages are:
///
/// ```text
/// maximal = (P ⊖ disk(g)) \ (O ⊕ disk(g))
/// opened  = ((maximal ⊖ disk(q)) ⊕ disk(q)) ∩ maximal
/// ```
///
/// For each connected component `C_i` of `opened`, choose `x_i ∈ {0, 1}` to
/// maximize `Σ area(C_i) x_i`, subject to `x_i + x_j ≤ 1` whenever
/// `distance(C_i, C_j) < 2v`. Independent conflict clusters are solved exactly.
/// This makes the unavoidable topology choice order-independent and preserves
/// the greatest possible area without recognizing board features or encoding
/// special cases.
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
    let construction_gap_radius_mm = options.construction_gap_radius_mm();
    let panel_clearance_keep_in = input.panel_outer.disk_erode(construction_clearance_mm);
    let panel_keep_in = input.panel_outer.disk_erode(construction_gap_radius_mm);
    let obstacle_clearance = raw_obstacles.disk_dilate(construction_clearance_mm);
    let obstacle_gap_envelope = raw_obstacles.disk_dilate(construction_gap_radius_mm);
    let maximal_safe_region = panel_keep_in.difference(&obstacle_gap_envelope);
    let regularization_core = maximal_safe_region.disk_erode(options.minimum_feature_radius_mm);
    let opened_safe_region = regularization_core
        .disk_dilate(options.minimum_feature_radius_mm)
        .intersection(&maximal_safe_region);
    let removed_by_opening = maximal_safe_region.difference(&opened_safe_region);
    let (safe_region, removed_by_gap_separation) =
        opened_safe_region.disk_separate_components(options.minimum_gap_radius_mm);
    let removed_by_regularization = maximal_safe_region.difference(&safe_region);

    // Certify against the nominal requirement, independently of the
    // construction guard used above.
    let swept_safe_region = safe_region.disk_dilate(options.clearance_mm);
    let minimum_feature_violations = safe_region
        .difference(&safe_region.disk_open(options.minimum_feature_radius_mm))
        .disk_open(options.numerical_guard_mm);
    let minimum_gap_violations = safe_region
        .disk_inter_component_gap_violations(options.minimum_gap_radius_mm)
        .disk_open(options.numerical_guard_mm);
    let void_closing_additions = safe_region
        .disk_close(options.minimum_gap_radius_mm)
        .difference(&safe_region)
        .disk_open(options.numerical_guard_mm);
    let certificate = ClearanceCertificate {
        safe_outside_maximal_region: safe_region.difference(&maximal_safe_region),
        safe_outside_keep_in: safe_region.difference(&panel_keep_in),
        safe_inside_obstacle_clearance: safe_region.intersection(&obstacle_clearance),
        safe_inside_obstacle_gap_envelope: safe_region.intersection(&obstacle_gap_envelope),
        minimum_feature_violations,
        minimum_gap_violations,
        void_closing_additions,
        outside_panel: swept_safe_region.difference(&input.panel_outer),
        obstacle_overlap: swept_safe_region.intersection(&raw_obstacles),
        swept_safe_region,
    };

    Ok(BoardArrayBalancingResult {
        safe_region,
        intermediates: BoardArrayBalancingIntermediates {
            raw_obstacles,
            panel_clearance_keep_in,
            panel_keep_in,
            obstacle_clearance,
            obstacle_gap_envelope,
            maximal_safe_region,
            regularization_core,
            opened_safe_region,
            removed_by_opening,
            removed_by_gap_separation,
            removed_by_regularization,
        },
        certificate,
    })
}

/// Collect safe-region inputs from canonical IPC layout/profile IR and
/// already-extracted `ArraySupport` layers.
///
/// Source-file traversal and view extraction stay outside `pcb-ir`; all
/// geometry classification after extraction lives here.
pub fn collect_board_array_balancing_input<'a, Symbol, LayerFunction>(
    layout: &Document<Symbol, LayerFunction>,
    fabrication_profile: &BoardArrayFabricationProfile,
    support_documents: impl IntoIterator<Item = BoardArraySupportDocument<'a, Symbol, LayerFunction>>,
) -> Result<BoardArrayBalancingCollection, BalancingRegionError>
where
    Symbol: PartialEq + 'a,
    LayerFunction: 'a,
{
    let collection =
        inspect_board_array_balancing_input(layout, fabrication_profile, support_documents)?;
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
    support_documents: impl IntoIterator<Item = BoardArraySupportDocument<'a, Symbol, LayerFunction>>,
) -> Result<BoardArrayBalancingCollection, BalancingRegionError>
where
    Symbol: PartialEq + 'a,
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
        .map(collect_support_layer_geometry)
        .collect::<Vec<_>>();
    let support_features = support_layers
        .iter()
        .fold(ContourSet::empty(tol::REGION_MM), |region, layer| {
            region.union(&layer.region)
        });

    Ok(BoardArrayBalancingCollection {
        input: BoardArrayBalancingInput {
            panel_outer,
            board_footprints,
            material_removal,
            support_features,
        },
        board_instance_count,
        support_layers,
    })
}

fn collect_support_layer_geometry<Symbol: PartialEq, LayerFunction>(
    source: BoardArraySupportDocument<'_, Symbol, LayerFunction>,
) -> BoardArraySupportLayerGeometry {
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
    let region = ContourSet::from_painted_paths(
        &source.document.arena,
        paths.iter().copied(),
        tol::REGION_MM,
    );

    BoardArraySupportLayerGeometry {
        source_feature_count: source.document.features.len(),
        feature_count: features.len(),
        source_path_count,
        path_count: paths.len(),
        excluded_documentation_path_count: source_path_count.saturating_sub(paths.len()),
        unpainted_path_count,
        region,
    }
}

fn validate_options(options: BalancingRegionOptions) -> Result<(), BalancingRegionError> {
    if !options.clearance_mm.is_finite() || options.clearance_mm <= 0.0 {
        return Err(BalancingRegionError::InvalidClearance(options.clearance_mm));
    }
    if !options.minimum_feature_radius_mm.is_finite() || options.minimum_feature_radius_mm <= 0.0 {
        return Err(BalancingRegionError::InvalidMinimumFeatureRadius(
            options.minimum_feature_radius_mm,
        ));
    }
    if !options.minimum_gap_radius_mm.is_finite() || options.minimum_gap_radius_mm <= 0.0 {
        return Err(BalancingRegionError::InvalidMinimumGapRadius(
            options.minimum_gap_radius_mm,
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
        Feature, FeatureDomain, FeatureKind, FeatureRole, FeatureSet, LayoutInstance, LayoutStep,
        LayoutStepKind, Spec, SpecItem, SpecItemKind, SpecRef, StepProfile,
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
            "outside maximal {:.9}, outside keep-in {:.9}, inside obstacle clearance {:.9}, inside gap envelope {:.9}, feature violations {:.9}, gap violations {:.9}, outside panel {:.9}, obstacle overlap {:.9}",
            result.certificate.safe_outside_maximal_region.area(),
            result.certificate.safe_outside_keep_in.area(),
            result.certificate.safe_inside_obstacle_clearance.area(),
            result.certificate.safe_inside_obstacle_gap_envelope.area(),
            result.certificate.minimum_feature_violations.area(),
            result.certificate.minimum_gap_violations.area(),
            result.certificate.outside_panel.area(),
            result.certificate.obstacle_overlap.area(),
        );
        assert!(
            result
                .safe_region
                .difference(&result.intermediates.maximal_safe_region)
                .is_empty()
        );
        assert!(result.intermediates.removed_by_regularization.area() > 0.0);
        assert!(
            result
                .safe_region
                .difference(&result.intermediates.panel_keep_in)
                .is_empty()
        );
        assert!(
            result
                .safe_region
                .intersection(&result.intermediates.obstacle_clearance)
                .is_empty()
        );
    }

    #[test]
    fn increasing_clearance_cannot_increase_safe_region() {
        let input = balancing_input(0.0, ContourSet::empty(tol::REGION_MM));
        let smaller = board_array_balancing_region(
            &input,
            BalancingRegionOptions {
                clearance_mm: 0.25,
                minimum_feature_radius_mm: 0.5,
                minimum_gap_radius_mm: 0.5,
                numerical_guard_mm: 0.0,
            },
        )
        .unwrap();
        let larger = board_array_balancing_region(
            &input,
            BalancingRegionOptions {
                clearance_mm: 1.0,
                minimum_feature_radius_mm: 0.5,
                minimum_gap_radius_mm: 0.5,
                numerical_guard_mm: 0.0,
            },
        )
        .unwrap();

        let larger_outside_smaller = larger.safe_region.difference(&smaller.safe_region);
        assert!(
            larger_outside_smaller.is_empty(),
            "larger opening added {:.9} mm²",
            larger_outside_smaller.area()
        );
        assert!(larger.safe_region.area() < smaller.safe_region.area());
    }

    #[test]
    fn increasing_minimum_feature_radius_cannot_increase_safe_region() {
        let input = balancing_input(0.0, ContourSet::empty(tol::REGION_MM));
        let smaller = board_array_balancing_region(
            &input,
            BalancingRegionOptions {
                clearance_mm: 0.5,
                minimum_feature_radius_mm: 0.25,
                minimum_gap_radius_mm: 0.5,
                numerical_guard_mm: 0.0,
            },
        )
        .unwrap();
        let larger = board_array_balancing_region(
            &input,
            BalancingRegionOptions {
                clearance_mm: 0.5,
                minimum_feature_radius_mm: 1.0,
                minimum_gap_radius_mm: 0.5,
                numerical_guard_mm: 0.0,
            },
        )
        .unwrap();

        let larger_outside_smaller = larger.safe_region.difference(&smaller.safe_region);
        assert!(
            larger_outside_smaller.is_empty(),
            "larger opening added {:.9} mm²",
            larger_outside_smaller.area()
        );
        assert!(larger.safe_region.area() < smaller.safe_region.area());
    }

    #[test]
    fn minimum_gap_radius_widens_corridors_around_line_obstacles() {
        let input = BoardArrayBalancingInput {
            panel_outer: ContourSet::rectangle(bbox(0.0, 0.0, 30.0, 20.0), tol::REGION_MM),
            board_footprints: ContourSet::rectangle(bbox(2.0, 2.0, 4.0, 4.0), tol::REGION_MM),
            material_removal: ContourSet::empty(tol::REGION_MM),
            support_features: ContourSet::rectangle(bbox(14.95, 0.0, 15.05, 20.0), tol::REGION_MM),
        };
        let narrow = board_array_balancing_region(
            &input,
            BalancingRegionOptions {
                clearance_mm: 0.5,
                minimum_feature_radius_mm: 0.5,
                minimum_gap_radius_mm: 0.5,
                numerical_guard_mm: 0.025,
            },
        )
        .unwrap();
        let wide = board_array_balancing_region(
            &input,
            BalancingRegionOptions {
                clearance_mm: 0.5,
                minimum_feature_radius_mm: 0.5,
                minimum_gap_radius_mm: 1.0,
                numerical_guard_mm: 0.025,
            },
        )
        .unwrap();

        assert!(
            wide.safe_region.difference(&narrow.safe_region).is_empty(),
            "wider gap envelope added {:.9} mm²",
            wide.safe_region.difference(&narrow.safe_region).area()
        );
        assert!(wide.certificate.minimum_gap_violations.is_empty());

        let mut components = wide.safe_region.connected_components();
        components.sort_by(|left, right| left.bbox.min.x.total_cmp(&right.bbox.min.x));
        assert_eq!(
            components.len(),
            2,
            "rings {}, bbox {:?}",
            wide.safe_region.rings.len(),
            wide.safe_region.bbox
        );
        let gap = components[1].bbox.min.x - components[0].bbox.max.x;
        assert!(gap >= 2.0, "expected a 2 mm gap, got {gap:.9} mm");
    }

    #[test]
    fn adding_an_obstacle_cannot_increase_safe_region() {
        let baseline_input = balancing_input(0.0, ContourSet::empty(tol::REGION_MM));
        let added_obstacle = ContourSet::rectangle(bbox(10.0, 1.0, 11.0, 9.0), tol::REGION_MM);
        let blocked_input = balancing_input(0.0, added_obstacle);
        let options = BalancingRegionOptions::default();

        let baseline = board_array_balancing_region(&baseline_input, options).unwrap();
        let blocked = board_array_balancing_region(&blocked_input, options).unwrap();

        assert!(
            blocked
                .safe_region
                .difference(&baseline.safe_region)
                .is_empty()
        );
        assert!(blocked.safe_region.area() < baseline.safe_region.area());
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
            "original outside maximal {:.9}, outside panel {:.9}, obstacle overlap {:.9}",
            original.certificate.safe_outside_maximal_region.area(),
            original.certificate.outside_panel.area(),
            original.certificate.obstacle_overlap.area(),
        );
        assert!(
            translated.certificate.passes(1e-4),
            "translated outside maximal {:.9}, outside panel {:.9}, obstacle overlap {:.9}",
            translated.certificate.safe_outside_maximal_region.area(),
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
                    minimum_feature_radius_mm: 0.5,
                    minimum_gap_radius_mm: 0.5,
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
                    minimum_feature_radius_mm: 0.0,
                    minimum_gap_radius_mm: 0.5,
                    numerical_guard_mm: 0.0,
                }
            )
            .unwrap_err(),
            BalancingRegionError::InvalidMinimumFeatureRadius(0.0)
        );
        assert_eq!(
            board_array_balancing_region(
                &input,
                BalancingRegionOptions {
                    clearance_mm: 0.5,
                    minimum_feature_radius_mm: 0.5,
                    minimum_gap_radius_mm: 0.0,
                    numerical_guard_mm: 0.0,
                }
            )
            .unwrap_err(),
            BalancingRegionError::InvalidMinimumGapRadius(0.0)
        );
        assert_eq!(
            board_array_balancing_region(
                &input,
                BalancingRegionOptions {
                    clearance_mm: 0.5,
                    minimum_feature_radius_mm: 0.5,
                    minimum_gap_radius_mm: 0.5,
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
        support.features.push(Feature {
            paths: Span::single(path),
            ..Feature::new(FeatureKind::Primitive, Polarity::Dark)
        });

        let collection = collect_board_array_balancing_input(
            &layout,
            &profile,
            [BoardArraySupportDocument::new(
                &support,
                BoardArraySupportLayerPolicy::AllPaintedFeatures,
            )],
        )
        .unwrap();

        assert_eq!(collection.board_instance_count, 1);
        assert_eq!(collection.support_layers.len(), 1);
        assert_eq!(collection.support_layers[0].path_count, 1);
        assert!((collection.input.panel_outer.area() - 200.0).abs() <= 1e-6);
        assert!((collection.input.board_footprints.area() - 12.0).abs() <= 1e-6);
        assert!((collection.input.material_removal.area() - 1.0).abs() <= 1e-6);
        assert!((collection.input.support_features.area() - 1.0).abs() <= 1e-6);
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

        let geometry = collect_support_layer_geometry(BoardArraySupportDocument::new(
            &support,
            BoardArraySupportLayerPolicy::VCutOperationsOnly,
        ));

        assert_eq!(geometry.source_feature_count, 2);
        assert_eq!(geometry.feature_count, 1);
        assert_eq!(geometry.source_path_count, 2);
        assert_eq!(geometry.path_count, 1);
        assert_eq!(geometry.excluded_documentation_path_count, 1);
        assert!(geometry.region.bbox.max.y < 1.0);
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
            datum: Point::default(),
            profiles: Span::single(0),
            bbox: bbox(0.0, 0.0, 20.0, 10.0),
        });
        layout.layout.steps.push(LayoutStep {
            source_step_ref: 2,
            kind: LayoutStepKind::Board,
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
            array_outlines: vec![vec![rectangle_contour(0.0, 0.0, 20.0, 10.0)]],
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
