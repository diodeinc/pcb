//! Dense copper balancing over an explicitly supplied safe region.
//!
//! This module intentionally does not discover safe panel regions or inspect
//! PCB layer semantics. Callers supply the addable region and the measured
//! per-layer areas; the solver chooses the closest manufacturable copper area
//! and generates a deterministic perforated plane.

use crate::geom::AccuracyError;

use crate::geom::{BBox, ContourSet, GeometryAccuracy, Point};

mod lattice;
mod spatial;

pub use lattice::{ROUNDED_HEXAGON_CORNER_RADIUS_RATIO, rounded_hexagonal_void};

use lattice::{LatticeCandidates, ROUNDED_HEXAGON_AREA_FACTOR, SiteTable};
use spatial::{
    LatticeDensityKernel, LayerDensityModel, density_scales_mm, lattice_cell_coverage,
    normalized_stack_weights, spatial_result_from_squared_radii,
};

const NUMERIC_EPSILON: f64 = 1e-9;
/// Leftover area tolerated when certifying that one region contains another.
/// Roughly a 30 um square: three orders of magnitude below the smallest void
/// the profile can place, and above the slivers a regularized difference
/// leaves where its operands share an edge.
const CONTAINMENT_AREA_TOLERANCE_MM2: f64 = 1e-3;
const SQRT_3: f64 = 1.732_050_807_568_877_2;

/// Independent per-layer work, in source order. Browsers cannot spawn native
/// threads; use the identical solve serially there, without requiring workers
/// or shared WebAssembly memory. Native builds retain per-layer concurrency.
pub fn map_layers<T: Send, R: Send>(
    items: impl IntoIterator<Item = T>,
    solve: impl Fn(T) -> R + Sync,
) -> Vec<R> {
    #[cfg(target_family = "wasm")]
    {
        items.into_iter().map(solve).collect()
    }
    #[cfg(not(target_family = "wasm"))]
    {
        std::thread::scope(|scope| {
            let solve = &solve;
            let handles = items
                .into_iter()
                .map(|item| scope.spawn(move || solve(item)))
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .map(|handle| handle.join().expect("copper-balance solve panicked"))
                .collect()
        })
    }
}

/// Fixed geometry constraints for a dense perforated copper plane.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DenseCopperBalanceProfile {
    pub pitch_mm: f64,
    pub min_void_radius_mm: f64,
    pub max_void_radius_mm: f64,
    pub min_copper_web_mm: f64,
    pub boundary_web_mm: f64,
    pub density_sigma_mm: f64,
    /// Number of uniformly spaced void-area levels used after the spatial
    /// solve. Void area is proportional to squared radius, so this quantizes
    /// the variable the solver controls directly.
    pub void_area_levels: usize,
    /// How far a layer's fill may step off its board's own density to flatten
    /// the stack's copper moment.
    ///
    /// This is the whole trade between the two things balancing is for, so it
    /// is stated in the units the losing side is stated in. Etch loading is a
    /// local effect — etchant works faster in sparse regions — so what it cares
    /// about is the step in density across the boundary between a board and the
    /// frame beside it. This bounds that step, and every layer's fill stays
    /// within it whatever the moment asks for.
    ///
    /// The moment needs a few percent on these panels. Plating already varies
    /// by under ten percent across a panel once thieving is doing its work, and
    /// fabricators quote ten to fifteen for mirrored-layer mismatch, so a step
    /// of a few percent sits well inside what the process already carries.
    ///
    /// Zero pins every layer to its own board density.
    pub stack_flex_density: f64,
    /// Boundary accuracy the balance geometry is prepared to. Fill geometry
    /// is bounded by the process rather than measured, so it takes a coarser
    /// budget than checks do; the construction clearance guard grows with it.
    pub accuracy: GeometryAccuracy,
}

impl DenseCopperBalanceProfile {
    /// Conservative first-party defaults for conventional rigid boards.
    pub const V1: Self = Self {
        pitch_mm: 1.35,
        min_void_radius_mm: 0.20,
        max_void_radius_mm: 0.65,
        min_copper_web_mm: 0.20,
        boundary_web_mm: 0.20,
        density_sigma_mm: 5.0,
        void_area_levels: 20,
        stack_flex_density: 0.05,
        accuracy: GeometryAccuracy::micrometres(50),
    };

    /// Squared-radius bounds of the void-area levels.
    fn void_area_bounds(self) -> (f64, f64) {
        (
            self.min_void_radius_mm.powi(2),
            self.max_void_radius_mm.powi(2),
        )
    }

    fn void_area_level(self, index: usize) -> f64 {
        let (minimum, maximum) = self.void_area_bounds();
        minimum + (maximum - minimum) * index as f64 / (self.void_area_levels - 1) as f64
    }

    /// The level a squared radius lands on once `slack` is taken off it and
    /// its place among the levels is rounded by `round`. Equal bounds divide
    /// by zero, which `max` lands on the one level there is.
    fn void_area_level_index(
        self,
        radius_squared: f64,
        slack: f64,
        round: fn(f64) -> f64,
    ) -> usize {
        let (minimum, maximum) = self.void_area_bounds();
        let place = (radius_squared.clamp(minimum, maximum) - minimum - slack)
            / (maximum - minimum)
            * (self.void_area_levels - 1) as f64;
        (round(place).max(0.0) as usize).min(self.void_area_levels - 1)
    }

    /// The void-area level nearest a squared radius, as a squared radius.
    fn nearest_void_area_level(self, radius_squared: f64) -> f64 {
        self.void_area_level(self.void_area_level_index(radius_squared, 0.0, f64::round))
    }

    /// The lowest void-area level holding at least this radius.
    fn void_area_level_up(self, radius_mm: f64) -> usize {
        self.void_area_level_index(radius_mm.powi(2), NUMERIC_EPSILON, f64::ceil)
    }

    fn quantize_void_radius_up(self, radius_mm: f64) -> f64 {
        self.void_area_level(self.void_area_level_up(radius_mm))
            .sqrt()
    }

    /// Disk radius used to reject partial voids narrower than
    /// `min_void_radius_mm`.
    pub fn minimum_partial_void_inradius_mm(self) -> f64 {
        self.min_void_radius_mm / 2.0
    }

    /// Rolling-disk radius applied to emitted partial-void geometry: half
    /// the partial-void inradius floor, so conforming components survive an
    /// opening intact while sub-floor clip tails are removed.
    pub fn void_regularization_radius_mm(self) -> f64 {
        self.minimum_partial_void_inradius_mm() / 2.0
    }

    /// Minimum flat-to-flat web between nearest-neighbor hexagonal voids.
    ///
    /// The center lattice is rotated 30° from the hexagon vertices, putting
    /// every nearest neighbor normal to a parallel pair of flats. A regular
    /// hexagon's flat-to-flat dimension is `√3 R`; rounding only shortens the
    /// corners and leaves those flats unchanged.
    pub fn nearest_neighbor_web_mm(self) -> f64 {
        self.pitch_mm - SQRT_3 * self.max_void_radius_mm
    }

    pub fn validate(self) -> Result<(), DenseCopperBalanceError> {
        for (name, value) in [
            ("pitch", self.pitch_mm),
            ("minimum void radius", self.min_void_radius_mm),
            ("maximum void radius", self.max_void_radius_mm),
            ("minimum copper web", self.min_copper_web_mm),
            ("boundary copper web", self.boundary_web_mm),
            ("density smoothing sigma", self.density_sigma_mm),
        ] {
            if !value.is_finite() || value <= 0.0 {
                return Err(DenseCopperBalanceError::InvalidProfile(format!(
                    "{name} must be finite and greater than zero"
                )));
            }
        }
        // Zero is meaningful here — it pins every layer to its uniform
        // selection — so this bound is separate from the strictly positive
        // geometry above.
        if !self.stack_flex_density.is_finite() || !(0.0..=1.0).contains(&self.stack_flex_density) {
            return Err(DenseCopperBalanceError::InvalidProfile(
                "stack flex density must be between zero and one".to_string(),
            ));
        }
        if self.min_void_radius_mm > self.max_void_radius_mm {
            return Err(DenseCopperBalanceError::InvalidProfile(
                "minimum void radius exceeds maximum void radius".to_string(),
            ));
        }
        if self.void_area_levels < 2 {
            return Err(DenseCopperBalanceError::InvalidProfile(
                "void area levels must be at least two".to_string(),
            ));
        }
        if self.nearest_neighbor_web_mm() + NUMERIC_EPSILON < self.min_copper_web_mm {
            return Err(DenseCopperBalanceError::InvalidProfile(format!(
                "pitch leaves {} mm between maximum-radius voids, below the {} mm minimum web",
                self.nearest_neighbor_web_mm(),
                self.min_copper_web_mm
            )));
        }
        Ok(())
    }
}

/// The selected topology and, for a perforated plane, its equivalent radius.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DenseCopperBalanceMode {
    None,
    Solid,
    /// A solid fill perforated by slightly rounded regular flat-top hexagons.
    ///
    /// A spatial result can use different per-site radii. This value is the
    /// root-mean-square radius, which preserves their total analytic area.
    Perforated {
        void_radius_mm: f64,
    },
}

/// Result of projecting the requested copper area onto the manufacturable set.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DenseCopperBalanceSolution {
    pub mode: DenseCopperBalanceMode,
    pub desired_added_area_mm2: f64,
    pub generated_area_mm2: f64,
    pub initial_density: f64,
    pub achieved_density: f64,
    pub target_density: f64,
    pub residual_error: f64,
}

/// One layer in a joint spatial copper-balance solve.
#[derive(Debug, Clone, Copy)]
pub struct SpatialCopperBalanceLayerRequest<'a> {
    /// Safe, initially empty region available on this copper layer.
    pub safe_region: &'a ContourSet,
    /// Fixed copper within `SpatialCopperBalanceRequest::panel_region`.
    pub existing_copper: &'a ContourSet,
    /// Region over which `target_density` is both measured and applied.
    ///
    /// This is the area that holds copper or could hold copper: the immutable
    /// footprints whose measured density set the target, this layer's safe
    /// region, and any fixed copper outside both. Permanently bare area —
    /// process margins, clearance rings, material removal, gaps narrower than
    /// the minimum web — must be excluded, so the solver never budgets copper
    /// for area no generated feature could occupy. Including it would inflate
    /// the request by `target_density` times that area, which the solver can
    /// only spend by over-filling the region it can reach.
    ///
    /// Must contain `safe_region` and `existing_copper`, and be contained by
    /// `SpatialCopperBalanceRequest::panel_region`.
    pub density_domain: &'a ContourSet,
    pub target_density: f64,
    /// Signed first-moment weight `z * thickness` from the physical stackup.
    pub stack_weight_mm2: f64,
}

/// Geometry shared by all layers in a joint spatial copper-balance solve.
#[derive(Debug, Clone, Copy)]
pub struct SpatialCopperBalanceRequest<'a> {
    /// Canonical panel geometry: the lattice extent and the domain over which
    /// local density error is evaluated. Each layer's density denominator is
    /// its own [`SpatialCopperBalanceLayerRequest::density_domain`].
    pub panel_region: &'a ContourSet,
    pub lattice_origin: Point,
    pub layers: &'a [SpatialCopperBalanceLayerRequest<'a>],
}

/// Integer address of one site on the staggered rounded-hex lattice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DenseCopperLatticeSite {
    pub column: i64,
    pub row: i64,
}

/// Geometry of the common staggered rounded-hex lattice.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DenseCopperLattice {
    pub origin: Point,
    pub pitch_mm: f64,
}

impl DenseCopperLattice {
    pub fn column_pitch_mm(self) -> f64 {
        self.pitch_mm * SQRT_3 / 2.0
    }

    pub fn center(self, site: DenseCopperLatticeSite) -> Point {
        Point::new(
            self.origin.x + site.column as f64 * self.column_pitch_mm(),
            self.origin.y
                + site.row as f64 * self.pitch_mm
                + site.column.rem_euclid(2) as f64 * self.pitch_mm / 2.0,
        )
    }

    /// The lattice site nearest to `point`, with its exact center.
    pub fn nearest_site(self, point: Point) -> (DenseCopperLatticeSite, Point) {
        let column = ((point.x - self.origin.x) / self.column_pitch_mm()).round() as i64;
        let row_origin = self.origin.y + column.rem_euclid(2) as f64 * self.pitch_mm / 2.0;
        let row = ((point.y - row_origin) / self.pitch_mm).round() as i64;
        let site = DenseCopperLatticeSite { column, row };
        (site, self.center(site))
    }

    /// Every site whose center may lie in `bbox`, column by column.
    ///
    /// Hexagon vertices are at 0°, 60°, ...; nearest-neighbor center vectors
    /// are at 30°, 90°, ... so parallel flats face each other, and odd columns
    /// sit half a pitch up.
    fn sites_covering(self, bbox: BBox) -> Vec<DenseCopperLatticeSite> {
        let first_column = ((bbox.min.x - self.origin.x) / self.column_pitch_mm()).floor() as i64;
        let last_column = ((bbox.max.x - self.origin.x) / self.column_pitch_mm()).ceil() as i64;
        (first_column..=last_column)
            .flat_map(|column| {
                let column_origin_y =
                    self.origin.y + column.rem_euclid(2) as f64 * self.pitch_mm / 2.0;
                let first_row = ((bbox.min.y - column_origin_y) / self.pitch_mm).floor() as i64;
                let last_row = ((bbox.max.y - column_origin_y) / self.pitch_mm).ceil() as i64;
                (first_row..=last_row).map(move |row| DenseCopperLatticeSite { column, row })
            })
            .collect()
    }

    fn centers(self, sites: &[DenseCopperLatticeSite]) -> Vec<Point> {
        sites.iter().map(|site| self.center(*site)).collect()
    }

    /// The `(center, radius)` candidate tuples geometry helpers consume.
    pub fn void_candidates(&self, voids: &[DenseCopperVoid]) -> Vec<(Point, f64)> {
        voids
            .iter()
            .map(|void| (self.center(void.site), void.radius_mm))
            .collect()
    }
}

/// One full, unclipped rounded-hex void.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DenseCopperVoid {
    pub site: DenseCopperLatticeSite,
    pub radius_mm: f64,
}

/// What the panel's copper-moment field measured before the spatial solve
/// redistributed anything, and after it settled.
///
/// The field's mean bends the panel into a spherical cap and its variation
/// into every other shape, so an RMS carries both: a falling RMS means the
/// field flattened, not merely that a positive lobe found a negative one to
/// cancel against. Both readings come from the same field over the same sites,
/// so the pair can be compared — a mean that drops while the RMS holds has
/// moved the bow into another shape, and that reading is only trustworthy
/// because neither number was measured its own way.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StackMomentField {
    pub initial_mean: f64,
    pub initial_rms: f64,
    pub achieved_mean: f64,
    pub achieved_rms: f64,
}

/// Every layer's solved geometry, plus what the solve did to the stack.
#[derive(Debug, Clone)]
pub struct SpatialCopperBalance {
    pub layers: Vec<DenseCopperBalanceResult>,
    /// `None` when the stackup supplied no weights, and so nothing was
    /// measured, or when no layer took a lattice and no solve ran.
    pub moment_field: Option<StackMomentField>,
}

/// Selected density solution plus its canonical copper geometry.
#[derive(Debug, Clone)]
pub struct DenseCopperBalanceResult {
    pub solution: DenseCopperBalanceSolution,
    pub lattice: DenseCopperLattice,
    /// Safe, initially empty region available for generated copper.
    pub usable: ContourSet,
    /// Eroded subset in which voids may remove the generated plane.
    pub voidable: ContourSet,
    /// Interior voids that retain the exact rounded-hex template.
    pub full_voids: Vec<DenseCopperVoid>,
    /// Boundary sites at their solved radii; emitted as
    /// [`Self::edge_void_emission`].
    pub edge_voids: Vec<DenseCopperVoid>,
    /// The one emitted form of the edge voids, computed once at solve time
    /// so density accounting and emission read the same geometry.
    pub edge_void_emission: EdgeVoidEmission,
}

/// Edge voids resolved to their emitted form: voids whose circumscribed
/// disk fits inside the voidable region keep the compact lattice template
/// form, and the rest become one clipped, regularized region. The disk is a
/// conservative containment proxy — a fitting hex it misclassifies still
/// emits its exact clip through the contour path, at a small size cost.
#[derive(Debug, Clone)]
pub struct EdgeVoidEmission {
    /// Edge voids emitted as ordinary lattice template instances.
    pub instanced: Vec<DenseCopperVoid>,
    /// Crossing voids, clipped to the voidable region and regularized.
    pub clipped: ContourSet,
    /// Union of both parts, for density fields and area accounting.
    pub region: ContourSet,
}

impl EdgeVoidEmission {
    /// `edge_voids` are `lattice`'s edge candidates at their solved radii, in
    /// candidate order.
    fn build_emission(
        lattice: &LatticeCandidates,
        voidable: &ContourSet,
        edge_voids: &[DenseCopperVoid],
        profile: DenseCopperBalanceProfile,
    ) -> Result<Self, AccuracyError> {
        let (instanced, crossing): (Vec<_>, Vec<_>) = edge_voids
            .iter()
            .copied()
            .zip(lattice.edge_center_depths_mm.iter().copied())
            .partition(|(void, depth_mm)| lattice::disk_fits(*depth_mm, void.radius_mm, voidable));
        let instanced = instanced
            .into_iter()
            .map(|(void, _)| void)
            .collect::<Vec<_>>();
        let (crossing, crossing_depths_mm): (Vec<_>, Vec<_>) = crossing.into_iter().unzip();
        let clipped = lattice::emission_partial_voids(
            voidable,
            &lattice.disk_center_region,
            &lattice.lattice.void_candidates(&crossing),
            &crossing_depths_mm,
            profile,
        )?;
        let region =
            lattice::void_set(&instanced, lattice.lattice, voidable.resolution)?.union(&clipped)?;
        Ok(Self {
            instanced,
            clipped,
            region,
        })
    }
}

impl DenseCopperBalanceResult {
    pub fn void_count(&self) -> usize {
        self.full_voids.len() + self.edge_voids.len()
    }

    pub fn full_void_radius_range_mm(&self) -> Option<(f64, f64)> {
        let radii = || self.full_voids.iter().map(|void| void.radius_mm);
        Some((
            radii().min_by(f64::total_cmp)?,
            radii().max_by(f64::total_cmp)?,
        ))
    }

    pub fn boundary_web(&self) -> Result<ContourSet, AccuracyError> {
        self.usable.difference(&self.voidable)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum DenseCopperBalanceError {
    Accuracy(AccuracyError),
    InvalidProfile(String),
    InvalidInput(String),
}

impl std::fmt::Display for DenseCopperBalanceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Accuracy(error) => error.fmt(f),
            Self::InvalidProfile(message) => write!(f, "invalid copper balance profile: {message}"),
            Self::InvalidInput(message) => write!(f, "invalid copper balance input: {message}"),
        }
    }
}

impl std::error::Error for DenseCopperBalanceError {}

/// One layer's uniform solve: the closest of no fill, solid fill and a
/// perforated fill at one void radius to the copper area its target asks for.
fn uniform_copper_balance(
    profile: DenseCopperBalanceProfile,
    layer: SpatialCopperBalanceLayerRequest<'_>,
    density_domain_area_mm2: f64,
    voidable: &ContourSet,
    lattice: &LatticeCandidates,
) -> Result<DenseCopperBalanceResult, AccuracyError> {
    let usable = layer.safe_region.clone();
    let existing_copper_area_mm2 = layer.existing_copper.area();
    let usable_area_mm2 = usable.area();
    let desired_added_area_mm2 =
        layer.target_density * density_domain_area_mm2 - existing_copper_area_mm2;
    let initial_density = existing_copper_area_mm2 / density_domain_area_mm2;
    let mut best = ProjectedArea::new(DenseCopperBalanceMode::None, 0.0, desired_added_area_mm2);
    best.consider(ProjectedArea::new(
        DenseCopperBalanceMode::Solid,
        usable_area_mm2,
        desired_added_area_mm2,
    ));

    if !lattice.is_empty() {
        best.consider(project_perforated_geometry(
            lattice,
            profile,
            usable_area_mm2,
            desired_added_area_mm2,
        ));
    }

    let (full_voids, edge_voids) = match best.mode {
        DenseCopperBalanceMode::Perforated { void_radius_mm } => (
            lattice
                .full_sites
                .iter()
                .map(|site| DenseCopperVoid {
                    site: *site,
                    radius_mm: void_radius_mm,
                })
                .collect(),
            lattice.edge_voids(void_radius_mm, profile),
        ),
        DenseCopperBalanceMode::None | DenseCopperBalanceMode::Solid => (Vec::new(), Vec::new()),
    };
    // Account generated copper from the emitted geometry, not the solve's
    // projection, so achieved density is truthful to the output.
    let edge_void_emission =
        EdgeVoidEmission::build_emission(lattice, voidable, &edge_voids, profile)?;
    let generated_area_mm2 = match best.mode {
        DenseCopperBalanceMode::None => 0.0,
        DenseCopperBalanceMode::Solid => usable_area_mm2,
        DenseCopperBalanceMode::Perforated { .. } => {
            let full_void_area_mm2 = ROUNDED_HEXAGON_AREA_FACTOR
                * full_voids
                    .iter()
                    .map(|void| void.radius_mm * void.radius_mm)
                    .sum::<f64>();
            (usable_area_mm2 - full_void_area_mm2 - edge_void_emission.region.area()).max(0.0)
        }
    };
    let achieved_density =
        (existing_copper_area_mm2 + generated_area_mm2) / density_domain_area_mm2;
    let solution = DenseCopperBalanceSolution {
        mode: best.mode,
        desired_added_area_mm2,
        generated_area_mm2,
        initial_density,
        achieved_density,
        target_density: layer.target_density,
        residual_error: (achieved_density - layer.target_density).abs(),
    };
    Ok(DenseCopperBalanceResult {
        solution,
        lattice: lattice.lattice,
        usable,
        voidable: voidable.clone(),
        full_voids,
        edge_voids,
        edge_void_emission,
    })
}

/// Distribute each layer's already-selected copper area in space.
///
/// The solver uses squared void radius as its variable. Each layer scatters
/// only its admitted variables onto one panel lattice, and one normalized
/// convolution maps every layer to the same evaluation sites. The stack's
/// copper moment is settled first, in closed form, as a bounded shift of each
/// layer's copper area. Projected gradient then minimizes each layer's own
/// local density error while preserving that area and the radius bounds, so
/// the layers iterate independently of one another.
///
/// For a perforated layer, `rho = H(c + s - p - beta P x)`: `c` and `s` are
/// fixed-copper and safe-region indicators, `p` is the clipped edge-void
/// indicator, `P` scatters local squared radii `x`, and `H` is the shared
/// normalized Gaussian convolution.
pub fn generate_spatial_dense_copper_balance(
    profile: DenseCopperBalanceProfile,
    request: SpatialCopperBalanceRequest<'_>,
) -> Result<SpatialCopperBalance, DenseCopperBalanceError> {
    profile.validate()?;
    validate_spatial_request(request)?;
    let density_domain_areas = request
        .layers
        .iter()
        .map(|layer| layer.density_domain.area())
        .collect::<Vec<_>>();

    // Layers frequently share one safe region — a fab panel's every copper
    // layer, a board array's layers with equal support scope — so erode and
    // classify each distinct region once, and the distinct ones side by side.
    let mut region_sources: Vec<&ContourSet> = Vec::new();
    let layer_regions = request
        .layers
        .iter()
        .map(|layer| {
            region_sources
                .iter()
                .position(|region| region.rings == layer.safe_region.rings)
                .unwrap_or_else(|| {
                    region_sources.push(layer.safe_region);
                    region_sources.len() - 1
                })
        })
        .collect::<Vec<_>>();
    let (region_voidable, region_lattices): (Vec<ContourSet>, Vec<LatticeCandidates>) =
        map_layers(region_sources.iter().copied(), |safe_region| {
            let voidable = safe_region.disk_erode(profile.boundary_web_mm)?;
            let lattice =
                LatticeCandidates::build_lattice(&voidable, request.lattice_origin, profile)?;
            Ok::<_, AccuracyError>((voidable, lattice))
        })
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .unzip();
    let uniform = map_layers(
        request
            .layers
            .iter()
            .zip(&layer_regions)
            .zip(&density_domain_areas),
        |((layer, region_index), density_domain_area_mm2)| {
            uniform_copper_balance(
                profile,
                *layer,
                *density_domain_area_mm2,
                &region_voidable[*region_index],
                &region_lattices[*region_index],
            )
        },
    )
    .into_iter()
    .collect::<Result<Vec<_>, _>>()?;

    // Sites are the key everywhere below: one dense table turns a site into
    // its sample, and a center is only ever derived from its site.
    let lattice = DenseCopperLattice {
        origin: request.lattice_origin,
        pitch_mm: profile.pitch_mm,
    };
    let bbox_sites = lattice.sites_covering(request.panel_region.bbox);
    let in_panel = request
        .panel_region
        .contains_points_batch(&lattice.centers(&bbox_sites));
    let full_sites = || region_lattices.iter().flat_map(|region| &region.full_sites);
    let mut samples = SiteTable::spanning(bbox_sites.iter().chain(full_sites()));
    for (site, _) in bbox_sites
        .iter()
        .zip(in_panel)
        .filter(|(_, inside)| *inside)
    {
        samples.admit(*site);
    }
    // Full centers lie in eroded safe regions inside the panel; admitting any
    // the strict point test left out keeps sample membership structural
    // rather than tolerance-dependent.
    let region_active_sites = region_lattices
        .iter()
        .map(|region| {
            region
                .full_sites
                .iter()
                .map(|site| samples.admit(*site))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    if region_active_sites.iter().all(Vec::is_empty) {
        return Ok(SpatialCopperBalance {
            layers: uniform,
            moment_field: None,
        });
    }

    // Each scale of the objective lives on its own subset of the panel
    // lattice. Every layer scatters only its own void variables onto the full
    // lattice, then the same normalized convolutions map those fields to the
    // scales' sites.
    let scales = density_scales_mm(profile)
        .into_iter()
        .map(|sigma_mm| LatticeDensityKernel::new(&samples, lattice, sigma_mm))
        .collect::<Vec<_>>();
    let tile_coverage =
        |region: &ContourSet| lattice_cell_coverage(&samples.sites, region, lattice);

    let mut coverage = map_layers(
        request
            .layers
            .iter()
            .map(|layer| layer.existing_copper)
            .chain(region_sources.iter().copied())
            .chain(
                uniform
                    .iter()
                    .map(|result| &result.edge_void_emission.region),
            ),
        tile_coverage,
    )
    .into_iter();
    let fixed_density = coverage
        .by_ref()
        .take(request.layers.len())
        .collect::<Vec<_>>();
    let region_available_density = coverage
        .by_ref()
        .take(region_sources.len())
        .collect::<Vec<_>>();
    let partial_void_density = coverage.collect::<Vec<_>>();

    let lower = profile.min_void_radius_mm.powi(2);
    let upper = profile.max_void_radius_mm.powi(2);
    // The uniform solve already selected each layer's full-void area at one
    // radius within the profile bounds, so the equal-radius field it implies
    // is already feasible and needs no projection.
    let squared_radii = uniform
        .iter()
        .enumerate()
        .map(|(layer_index, result)| match result.solution.mode {
            DenseCopperBalanceMode::Perforated { void_radius_mm } => {
                vec![void_radius_mm.powi(2); region_active_sites[layer_regions[layer_index]].len()]
            }
            DenseCopperBalanceMode::None | DenseCopperBalanceMode::Solid => Vec::new(),
        })
        .collect::<Vec<Vec<f64>>>();
    let squared_radius_sums = squared_radii
        .iter()
        .map(|radii| radii.iter().sum::<f64>())
        .collect::<Vec<_>>();
    let cell_area_mm2 = SQRT_3 * profile.pitch_mm.powi(2) / 2.0;
    let normalized_stack_weights = normalized_stack_weights(request.layers);
    // A stackup that located no conductors leaves every weight zero. There is
    // no moment to report and none to flatten, and saying it is zero would
    // claim we looked.
    let stack_is_weighed = normalized_stack_weights.iter().any(|weight| *weight != 0.0);
    // Where each layer's copper area lands is settled once, here, rather than
    // argued against the local term on every iteration. The moment is linear in
    // the copper each layer carries, so the position that flattens it is a
    // closed form, and the iteration below is left to the local density term
    // alone -- the term that does etch and plating work, at a scale the moment
    // cannot see.
    //
    // How far each layer may step, as copper area. Measured over the lattice it
    // actually controls, so the bound is the density step across the boundary
    // between a board and the frame beside it, and a layer that took no lattice
    // brings nothing to the trade.
    let slack_areas_mm2 = squared_radii
        .iter()
        .map(|radii| profile.stack_flex_density * radii.len() as f64 * cell_area_mm2)
        .collect::<Vec<_>>();
    // The moment the uniform selection carries, and the most those steps can
    // move it. A stackup that located no conductors leaves every weight zero,
    // which leaves both at zero and every layer on its own density.
    let moment_mm4 = uniform
        .iter()
        .zip(request.layers)
        .zip(&normalized_stack_weights)
        .map(|((result, layer), weight)| {
            weight * (layer.existing_copper.area() + result.solution.generated_area_mm2)
        })
        .sum::<f64>();
    // Shares are weighted by lever arm against the longest one, so the layer
    // with the most leverage spends its whole step and the others spend in
    // proportion. A layer the stackup gave no weight does not move, and none
    // can exceed the step it was given.
    let longest_arm = normalized_stack_weights
        .iter()
        .fold(0.0_f64, |longest, weight| longest.max(weight.abs()));
    let share = |weight: f64| weight / longest_arm.max(f64::MIN_POSITIVE);
    let reach_mm4 = normalized_stack_weights
        .iter()
        .zip(&slack_areas_mm2)
        .map(|(weight, slack)| weight * share(*weight) * slack)
        .sum::<f64>();
    let spend = (-moment_mm4).clamp(-reach_mm4, reach_mm4) / reach_mm4.max(f64::MIN_POSITIVE);
    // Void area moves opposite to copper area.
    let pinned_sums = squared_radius_sums
        .iter()
        .zip(&normalized_stack_weights)
        .zip(&slack_areas_mm2)
        .map(|((sum, weight), slack)| {
            sum - spend * share(*weight) * slack / ROUNDED_HEXAGON_AREA_FACTOR
        })
        .collect::<Vec<_>>();
    let void_fraction_per_radius_squared = ROUNDED_HEXAGON_AREA_FACTOR / cell_area_mm2;
    // Nothing couples the layers once the settlement has pinned their areas,
    // so each runs its whole iteration on its own thread and owns its scratch.
    // The density fields either side of it are what the moment is read from:
    // the field the uniform selection left behind, and the field of the
    // emitted radii — what ships is the quantized lattice, not the iterate it
    // was rounded from.
    let solved = map_layers(
        uniform.into_iter().zip(squared_radii).enumerate(),
        |(layer_index, (baseline, squared_radii))| {
            let available = &region_available_density[layer_regions[layer_index]];
            let partial_void = &partial_void_density[layer_index];
            let base_coverage = fixed_density[layer_index]
                .iter()
                .enumerate()
                .map(|(site, fixed)| match baseline.solution.mode {
                    DenseCopperBalanceMode::None => *fixed,
                    DenseCopperBalanceMode::Solid => fixed + available[site],
                    DenseCopperBalanceMode::Perforated { .. } => {
                        fixed + available[site] - partial_void[site]
                    }
                })
                .collect::<Vec<_>>();
            let model = LayerDensityModel {
                scales: &scales,
                active_sites: &region_active_sites[layer_regions[layer_index]],
                base_density: scales
                    .iter()
                    .map(|scale| scale.smooth(&base_coverage))
                    .collect(),
                void_fraction_per_radius_squared,
            };
            let initial_density = model.density(&squared_radii);
            let squared_radii = model.redistribute(
                squared_radii,
                request.layers[layer_index].target_density,
                (lower, upper),
                pinned_sums[layer_index],
            );
            let result = if squared_radii.is_empty() {
                baseline
            } else {
                spatial_result_from_squared_radii(
                    &region_lattices[layer_regions[layer_index]].full_sites,
                    &squared_radii,
                    baseline,
                    request.layers[layer_index],
                    density_domain_areas[layer_index],
                    profile,
                )
            };
            let emitted = result
                .full_voids
                .iter()
                .map(|void| void.radius_mm.powi(2))
                .collect::<Vec<_>>();
            let achieved_density = model.density(&emitted);
            (result, initial_density, achieved_density)
        },
    );

    // Mean and RMS of the panel's copper moment about its mid-plane, before
    // and after, so the summary can say what the solve bought.
    let moment_reading = |densities: Vec<&Vec<f64>>| {
        let field = (0..scales[0].row_count())
            .map(|site| {
                normalized_stack_weights
                    .iter()
                    .zip(&densities)
                    .map(|(weight, density)| weight * density[site])
                    .sum::<f64>()
            })
            .collect::<Vec<_>>();
        let count = field.len() as f64;
        (
            field.iter().sum::<f64>() / count,
            (field.iter().map(|moment| moment * moment).sum::<f64>() / count).sqrt(),
        )
    };
    let moment_field = stack_is_weighed.then(|| {
        let (initial_mean, initial_rms) =
            moment_reading(solved.iter().map(|layer| &layer.1).collect());
        let (achieved_mean, achieved_rms) =
            moment_reading(solved.iter().map(|layer| &layer.2).collect());
        StackMomentField {
            initial_mean,
            initial_rms,
            achieved_mean,
            achieved_rms,
        }
    });
    Ok(SpatialCopperBalance {
        layers: solved.into_iter().map(|(result, _, _)| result).collect(),
        moment_field,
    })
}

/// Whether `outer` contains `inner`, ignoring boolean sliver artifacts.
///
/// A regularized difference between operands that share an edge leaves
/// sub-micron slivers along it, so exact emptiness is not a usable containment
/// test here. A genuine containment error — a domain that omits real copper or
/// real fillable material — is orders of magnitude above this bound, which is
/// itself far below the smallest void the profile can place.
fn contains(outer: &ContourSet, inner: &ContourSet) -> Result<bool, AccuracyError> {
    let leftover = inner.difference(outer)?;
    Ok(leftover.is_empty() || leftover.area() <= CONTAINMENT_AREA_TOLERANCE_MM2)
}

fn ensure(holds: bool, message: &str) -> Result<(), DenseCopperBalanceError> {
    if holds {
        Ok(())
    } else {
        Err(DenseCopperBalanceError::InvalidInput(message.to_string()))
    }
}

fn validate_spatial_request(
    request: SpatialCopperBalanceRequest<'_>,
) -> Result<(), DenseCopperBalanceError> {
    ensure(
        request.panel_region.bbox.is_valid() && !request.panel_region.is_empty(),
        "panel region must be non-empty and have valid bounds",
    )?;
    // A fab panel gives every copper layer the same domain and safe region,
    // and a board array shares them across layers of equal support scope, so
    // certify each distinct pair once.
    let mut certified: Vec<(&ContourSet, &ContourSet)> = Vec::new();
    for layer in request.layers {
        ensure(
            layer.stack_weight_mm2.is_finite(),
            "stack weights must be finite",
        )?;
        // Containment through the density domain implies containment by the
        // panel region, so the safe region and fixed copper need no separate
        // panel-region check.
        if !certified.iter().any(|(domain, safe_region)| {
            domain.rings == layer.density_domain.rings
                && safe_region.rings == layer.safe_region.rings
        }) {
            ensure(
                contains(request.panel_region, layer.density_domain)?,
                "density domain must be contained by the panel region",
            )?;
            ensure(
                contains(layer.density_domain, layer.safe_region)?,
                "safe region must be contained by the density domain",
            )?;
            certified.push((layer.density_domain, layer.safe_region));
        }
    }
    // What is left weighs each layer's whole fixed copper against its own
    // regions, which no other layer shares, so the layers check side by side.
    map_layers(request.layers, |layer| {
        ensure(
            contains(layer.density_domain, layer.existing_copper)?,
            "existing copper must be contained by the density domain",
        )?;
        // Fixed copper and the safe region are separated by a clearance rule
        // rather than by a shared edge, so their overlap needs no tolerance.
        ensure(
            layer
                .safe_region
                .intersection(layer.existing_copper)?
                .is_empty(),
            "existing copper and safe region must be disjoint",
        )?;
        ensure(
            request.lattice_origin.is_finite(),
            "lattice origin must be finite",
        )?;
        ensure(
            layer.safe_region.bbox.is_valid(),
            "safe region has invalid bounds",
        )?;
        let domain_area_mm2 = layer.density_domain.area();
        ensure(
            domain_area_mm2.is_finite() && domain_area_mm2 > 0.0,
            "density domain area must be finite and greater than zero",
        )?;
        // The area bounds are the scalar shadow of the containments above, so
        // they tolerate the same sliver area: holding them tighter would
        // reject a request whose geometry passed containment, and the slack
        // only vanishes when the footprints are fully poured.
        let most_mm2 = domain_area_mm2 + CONTAINMENT_AREA_TOLERANCE_MM2;
        let existing_area_mm2 = layer.existing_copper.area();
        ensure(
            existing_area_mm2.is_finite() && (0.0..=most_mm2).contains(&existing_area_mm2),
            "existing copper area must be between zero and the density domain area",
        )?;
        ensure(
            layer.target_density.is_finite() && (0.0..=1.0).contains(&layer.target_density),
            "target density must be between zero and one",
        )?;
        let usable_area_mm2 = layer.safe_region.area();
        ensure(
            usable_area_mm2.is_finite() && (0.0..=most_mm2).contains(&usable_area_mm2),
            "usable area must be between zero and the density domain area",
        )?;
        ensure(
            existing_area_mm2 + usable_area_mm2 <= most_mm2,
            "existing copper and usable areas together exceed the density domain area",
        )
    })
    .into_iter()
    .collect()
}

#[derive(Debug, Clone, Copy)]
struct ProjectedArea {
    mode: DenseCopperBalanceMode,
    area_mm2: f64,
    error_mm2: f64,
}

impl ProjectedArea {
    fn new(mode: DenseCopperBalanceMode, area_mm2: f64, desired_area_mm2: f64) -> Self {
        Self {
            mode,
            area_mm2,
            error_mm2: (area_mm2 - desired_area_mm2).abs(),
        }
    }

    fn consider(&mut self, candidate: Self) {
        if candidate.error_mm2 + NUMERIC_EPSILON < self.error_mm2
            || ((candidate.error_mm2 - self.error_mm2).abs() <= NUMERIC_EPSILON
                && candidate.area_mm2 < self.area_mm2)
        {
            *self = candidate;
        }
    }
}

fn project_perforated_geometry(
    lattice: &LatticeCandidates,
    profile: DenseCopperBalanceProfile,
    usable_area_mm2: f64,
    desired_added_area_mm2: f64,
) -> ProjectedArea {
    // Each edge site has an activation radius aᵢ. At nominal radius r its
    // clipped hex uses max(r, aᵢ) rounded up to a void-area level, so total
    // void area is monotone in r: interior area linear in r², plus an edge
    // area that steps once per level. Within a level the closest radius is a
    // closed form, and the feasible set's projection is the best of them.
    let target_void_area_mm2 = usable_area_mm2 - desired_added_area_mm2;
    let full_area_per_radius_squared =
        lattice.full_sites.len() as f64 * ROUNDED_HEXAGON_AREA_FACTOR;
    let candidate = |radius: f64| {
        ProjectedArea::new(
            DenseCopperBalanceMode::Perforated {
                void_radius_mm: radius,
            },
            (usable_area_mm2 - lattice.void_area(radius, profile)).max(0.0),
            desired_added_area_mm2,
        )
    };
    let mut best = candidate(profile.min_void_radius_mm);
    for level in 1..profile.void_area_levels {
        // A level starts just past the one below, where rounding up first
        // lands on it. A lattice of edge sites alone leaves the quotient
        // infinite or undefined, which lands on whichever end is nearer.
        let squared_radius = ((target_void_area_mm2 - lattice.edge_area_by_level_mm2[level])
            / full_area_per_radius_squared)
            .max(profile.void_area_level(level - 1) + 2.0 * NUMERIC_EPSILON)
            .min(profile.void_area_level(level));
        best.consider(candidate(squared_radius.sqrt()));
    }
    best
}

impl From<AccuracyError> for DenseCopperBalanceError {
    fn from(error: AccuracyError) -> Self {
        Self::Accuracy(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::{BBox, Point, Resolution};

    const V1: DenseCopperBalanceProfile = DenseCopperBalanceProfile::V1;
    /// Radius quantization alone moves an achieved density this far.
    const QUANTIZATION: f64 = 5e-3;

    fn rect(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> ContourSet {
        ContourSet::rectangle(
            BBox::new(Point::new(min_x, min_y), Point::new(max_x, max_y)),
            Resolution::default(),
        )
    }

    fn empty() -> ContourSet {
        ContourSet::empty(Resolution::default())
    }

    fn layer<'a>(
        safe_region: &'a ContourSet,
        existing_copper: &'a ContourSet,
        density_domain: &'a ContourSet,
        target_density: f64,
        stack_weight_mm2: f64,
    ) -> SpatialCopperBalanceLayerRequest<'a> {
        SpatialCopperBalanceLayerRequest {
            safe_region,
            existing_copper,
            density_domain,
            target_density,
            stack_weight_mm2,
        }
    }

    fn solve(
        profile: DenseCopperBalanceProfile,
        panel_region: &ContourSet,
        layers: &[SpatialCopperBalanceLayerRequest<'_>],
    ) -> Result<SpatialCopperBalance, DenseCopperBalanceError> {
        generate_spatial_dense_copper_balance(
            profile,
            SpatialCopperBalanceRequest {
                panel_region,
                lattice_origin: Point::ZERO,
                layers,
            },
        )
    }

    fn deviations(balance: &SpatialCopperBalance) -> Vec<f64> {
        balance
            .layers
            .iter()
            .map(|result| result.solution.achieved_density - result.solution.target_density)
            .collect()
    }

    /// Fill one bare rectangle and return its emitted voids, which have to
    /// stay inside the voidable region, keep the web between them, and each
    /// hold the minimum partial-void disk.
    fn perforated_rectangle(
        profile: DenseCopperBalanceProfile,
        (width, height): (f64, f64),
        target_density: f64,
    ) -> (DenseCopperBalanceResult, ContourSet) {
        let safe = rect(0.0, 0.0, width, height);
        let result = solve(
            profile,
            &safe,
            &[layer(&safe, &empty(), &safe, target_density, 0.0)],
        )
        .unwrap()
        .layers
        .pop()
        .unwrap();
        assert!(matches!(
            result.solution.mode,
            DenseCopperBalanceMode::Perforated { .. }
        ));
        let voids = lattice::void_set(&result.full_voids, result.lattice, safe.resolution)
            .unwrap()
            .union(&result.edge_void_emission.region)
            .unwrap();
        let voidable = safe.disk_erode(profile.boundary_web_mm).unwrap();
        assert!(voids.difference(&voidable).unwrap().is_empty());
        assert!(
            voids
                .disk_inter_component_gap_violations(profile.min_copper_web_mm / 2.0)
                .unwrap()
                .is_empty()
        );
        let minimum_core_radius = profile.minimum_partial_void_inradius_mm();
        assert!(
            voids
                .connected_components()
                .into_iter()
                .all(|void| !void.disk_erode(minimum_core_radius).unwrap().is_empty())
        );
        (result, voids)
    }

    #[test]
    fn clipped_lattice_matches_target_and_preserves_both_webs() {
        let (result, _) = perforated_rectangle(V1, (20.0, 10.0), 0.75);
        assert!(!result.edge_voids.is_empty());
        assert!((result.solution.achieved_density - 0.75).abs() <= QUANTIZATION);

        // Pitch below twice the maximum void radius: boundary sites must be
        // classified by hexagon containment, not center proximity.
        let tight_pitch = DenseCopperBalanceProfile {
            pitch_mm: 1.2,
            min_copper_web_mm: 0.05,
            ..V1
        };
        let (result, _) = perforated_rectangle(tight_pitch, (12.0, 8.0), 0.15);
        let (smallest, _) = result.full_void_radius_range_mm().unwrap();
        assert!(smallest > 0.6, "expected near-maximum voids");
    }

    #[test]
    fn retains_useful_partial_voids_at_the_boundary() {
        let (result, voids) = perforated_rectangle(V1, (4.0, 4.0), 0.45);
        let (voidable, voids) = (result.voidable.bbox, voids.connected_components());
        let reach = result.voidable.budget().max_error_mm();
        let touches = |side: fn(BBox) -> f64| {
            voids
                .iter()
                .any(|void| (side(void.bbox) - side(voidable)).abs() <= reach)
        };
        assert!(touches(|bbox| bbox.min.x) && touches(|bbox| bbox.max.x));
        assert!(touches(|bbox| bbox.min.y) && touches(|bbox| bbox.max.y));
    }

    #[test]
    fn rejects_geometry_that_breaks_a_containment() {
        let panel = rect(0.0, 0.0, 20.0, 12.0);
        let inside = rect(10.0, 0.0, 20.0, 12.0);
        let outside = rect(-1.0, 0.0, 5.0, 12.0);
        let overlapping = rect(0.0, 0.0, 11.0, 12.0);
        let empty = empty();
        for (safe, existing, domain, message) in [
            (
                &inside,
                &empty,
                &outside,
                "density domain must be contained by the panel region",
            ),
            (
                &outside,
                &empty,
                &inside,
                "safe region must be contained by the density domain",
            ),
            (
                &inside,
                &outside,
                &inside,
                "existing copper must be contained by the density domain",
            ),
            (
                &inside,
                &overlapping,
                &panel,
                "existing copper and safe region must be disjoint",
            ),
        ] {
            assert_eq!(
                solve(V1, &panel, &[layer(safe, existing, domain, 0.5, 0.0)]).unwrap_err(),
                DenseCopperBalanceError::InvalidInput(message.to_string())
            );
        }
    }

    /// The scalar area bounds must tolerate what `contains` tolerates.
    ///
    /// Fixed copper, fillable region, and permanently bare area partition the
    /// density domain, so a domain whose measured area is short by a boolean
    /// sliver breaks the sum bound. There is no slack to absorb it once the
    /// footprints are fully poured, and rejecting then would fail a request
    /// whose geometry passed every containment check.
    #[test]
    fn area_bounds_tolerate_the_same_slivers_as_containment() {
        let safe = rect(0.0, 0.0, 20.0, 10.0);
        let existing = rect(21.0, 0.0, 31.0, 10.0);
        let sliver_mm = CONTAINMENT_AREA_TOLERANCE_MM2 / 2.0 / 10.0;
        let domain = safe
            .union(&rect(21.0, 0.0, 31.0 - sliver_mm, 10.0))
            .unwrap();
        assert!(existing.area() + safe.area() > domain.area());

        let result = solve(
            V1,
            &rect(0.0, 0.0, 31.0, 10.0),
            &[layer(&safe, &existing, &domain, 0.9, 0.0)],
        );
        assert!(result.is_ok(), "{:?}", result.err());
    }

    /// The pinned sum and the box leave the solve its layer's area: at a target
    /// only minimum-radius voids reach there is nothing to move, and anywhere
    /// else the radii stay inside their bounds.
    #[test]
    fn spatial_solver_preserves_area_and_radius_bounds() {
        let panel = rect(0.0, 0.0, 24.0, 16.0);
        let voidable = panel.disk_erode(V1.boundary_web_mm).unwrap();
        let lattice = LatticeCandidates::build_lattice(&voidable, Point::ZERO, V1).unwrap();
        let minimum_radius_target =
            (panel.area() - lattice.void_area(V1.min_void_radius_mm, V1)) / panel.area();
        for target_density in [minimum_radius_target, 0.5] {
            let result = solve(
                V1,
                &panel,
                &[layer(&panel, &empty(), &panel, target_density, 0.0)],
            )
            .unwrap()
            .layers
            .pop()
            .unwrap();
            let (min, max) = result.full_void_radius_range_mm().unwrap();
            assert!(min + NUMERIC_EPSILON >= V1.min_void_radius_mm);
            assert!(max <= V1.max_void_radius_mm + NUMERIC_EPSILON);
            assert!((result.solution.achieved_density - target_density).abs() <= QUANTIZATION);
            if target_density == minimum_radius_target {
                assert!(max <= V1.min_void_radius_mm + NUMERIC_EPSILON);
            }
        }
    }

    /// Unfillable panel material must not inflate the copper request.
    ///
    /// A denominator that spans the whole panel charges the layer for filling
    /// clearance it may never touch, and the solver can only spend that budget
    /// by over-filling the gutter it can reach — saturating to a solid pour
    /// whose local density far exceeds the footprint it is supposed to match.
    #[test]
    fn unfillable_clearance_stays_out_of_the_density_denominator() {
        let panel = rect(0.0, 0.0, 40.0, 20.0);
        // An immutable footprint poured to 80%, a gutter beside it, and a wide
        // clearance ring in between that no generated copper may enter.
        let footprint = rect(0.0, 0.0, 20.0, 20.0);
        let existing = rect(0.0, 0.0, 20.0, 16.0);
        let safe = rect(25.0, 0.0, 40.0, 20.0);
        let target_density = existing.area() / footprint.area();
        let density_domain = footprint.union(&safe).unwrap();

        let result = solve(
            V1,
            &panel,
            &[layer(
                &safe,
                &existing,
                &density_domain,
                target_density,
                0.0,
            )],
        )
        .unwrap()
        .layers
        .pop()
        .unwrap();

        // The gutter is perforated to the footprint's own density, not poured
        // solid to chase copper the clearance can never hold.
        assert!(
            matches!(
                result.solution.mode,
                DenseCopperBalanceMode::Perforated { .. }
            ),
            "{:?}",
            result.solution
        );
        assert!((result.solution.achieved_density - target_density).abs() <= QUANTIZATION);
        let gutter_fill = result.solution.generated_area_mm2 / safe.area();
        assert!(
            (gutter_fill - target_density).abs() <= QUANTIZATION,
            "gutter filled to {gutter_fill}, footprint sits at {target_density}"
        );
        // Charging the request against the whole panel would have demanded
        // more copper than the gutter can hold.
        assert!(target_density * panel.area() - existing.area() > safe.area());
    }

    #[test]
    fn spatial_solver_preserves_each_layers_safe_region() {
        let panel = rect(0.0, 0.0, 40.0, 20.0);
        let halves = [rect(0.0, 0.0, 20.0, 20.0), rect(20.0, 0.0, 40.0, 20.0)];
        let empty = empty();
        // Nothing outside each layer's own half can hold copper, so each
        // layer's density domain is exactly its safe region.
        let results = solve(
            V1,
            &panel,
            &[
                layer(&halves[0], &empty, &halves[0], 0.25, 1.0),
                layer(&halves[1], &empty, &halves[1], 0.25, -1.0),
            ],
        )
        .unwrap()
        .layers;

        assert_eq!(results.len(), 2);
        for (result, half) in results.iter().zip(&halves) {
            assert_eq!(result.usable.rings, half.rings);
            assert!(!result.full_voids.is_empty());
            assert!(
                result
                    .full_voids
                    .iter()
                    .all(|void| half.contains_point(result.lattice.center(void.site)))
            );
        }
    }

    #[test]
    fn spatial_solver_opposes_a_fixed_copper_gradient_without_changing_total_area() {
        // Fixed copper fills the left half and the right half is fillable, so
        // the density domain is the whole panel.
        let panel = rect(0.0, 0.0, 40.0, 20.0);
        let existing = rect(0.0, 0.0, 20.0, 20.0);
        let safe = rect(20.0, 0.0, 40.0, 20.0);
        let result = solve(V1, &panel, &[layer(&safe, &existing, &panel, 0.75, 0.0)])
            .unwrap()
            .layers
            .pop()
            .unwrap();
        let mean_radius = |minimum_x: f64, maximum_x: f64| {
            let radii = result
                .full_voids
                .iter()
                .filter(|void| (minimum_x..maximum_x).contains(&result.lattice.center(void.site).x))
                .map(|void| void.radius_mm)
                .collect::<Vec<_>>();
            radii.iter().sum::<f64>() / radii.len() as f64
        };

        assert!(mean_radius(20.0, 25.0) > mean_radius(35.0, 40.0));
        assert!((result.solution.achieved_density - 0.75).abs() <= QUANTIZATION);
    }

    /// Fitted at the process scale alone, the minimiser is bang-bang: nearly
    /// every void saturates at a radius bound, in stark bands that only
    /// average out over that one length. Fitted at every scale, a copper
    /// gradient is answered with graded voids.
    #[test]
    fn spatial_solver_grades_voids_instead_of_saturating_them() {
        let panel = rect(0.0, 0.0, 80.0, 40.0);
        let existing = rect(0.0, 0.0, 30.0, 40.0);
        let safe = rect(30.0, 0.0, 80.0, 40.0);
        let result = solve(V1, &panel, &[layer(&safe, &existing, &panel, 0.75, 0.0)])
            .unwrap()
            .layers
            .pop()
            .unwrap();
        let at_a_bound = |radius: f64| {
            (radius - V1.min_void_radius_mm).abs() < 1e-9
                || (radius - V1.max_void_radius_mm).abs() < 1e-9
        };
        let saturated = result
            .full_voids
            .iter()
            .filter(|void| at_a_bound(void.radius_mm))
            .count() as f64
            / result.full_voids.len() as f64;
        assert!(saturated < 0.5, "{saturated} of the voids sit at a bound");
    }

    /// A layer saturated to a solid pour has no lattice and brings no step to
    /// the trade, but the moment it creates is still there to answer: only its
    /// mirror can counterweight, within that layer's own bound.
    #[test]
    fn a_solid_layer_leaves_the_counterweight_to_its_mirror() {
        let panel = rect(0.0, 0.0, 30.0, 20.0);
        let empty = empty();
        // A target its whole safe region cannot reach saturates the first
        // layer to a solid pour, leaving it no radii while its sites still
        // exist.
        let results = solve(
            DenseCopperBalanceProfile {
                stack_flex_density: 0.01,
                ..V1
            },
            &panel,
            &[
                layer(&panel, &empty, &panel, 1.0, 1.0),
                layer(&panel, &empty, &panel, 0.5, -1.0),
            ],
        )
        .unwrap();

        assert_eq!(
            results.layers[0].solution.mode,
            DenseCopperBalanceMode::Solid
        );
        // The free layer takes copper on, against the solid pour opposite it,
        // and stops at its own bound.
        let deviation = deviations(&results)[1];
        assert!(deviation > QUANTIZATION, "{deviation}");
        assert!(deviation <= 0.01 + QUANTIZATION, "{deviation}");
    }

    /// Fixed copper on one side of one layer tilts the stack in a way no
    /// redistribution can answer: it is real copper sitting off the mid-plane,
    /// and only the layer opposite it can counterweight: the tilted layer
    /// sheds density and its mirror takes density on, each within the step its
    /// own fill region allows.
    #[test]
    fn stack_flex_trades_density_between_mirrored_layers() {
        let panel = rect(0.0, 0.0, 40.0, 20.0);
        let left_copper = rect(0.0, 0.0, 20.0, 20.0);
        let safe = rect(20.0, 0.0, 40.0, 20.0);
        let empty = empty();
        let solve = |stack_flex_density: f64, stack_weight_mm2: f64| {
            solve(
                DenseCopperBalanceProfile {
                    stack_flex_density,
                    ..V1
                },
                &panel,
                &[
                    layer(&safe, &left_copper, &panel, 0.75, stack_weight_mm2),
                    layer(&safe, &empty, &safe, 0.5, -stack_weight_mm2),
                ],
            )
            .unwrap()
        };
        let flex = V1.stack_flex_density;

        // Without stack weights there is no moment to flatten, and no moment
        // field is reported: claiming a flat moment would say we looked.
        let unweighed = solve(flex, 0.0);
        assert_eq!(unweighed.moment_field, None);
        // Unweighed or pinned, both layers are held on their own targets.
        for held in [unweighed, solve(0.0, 1.0)] {
            let deviations = deviations(&held);
            assert!(
                deviations.iter().all(|step| step.abs() <= QUANTIZATION),
                "{deviations:?}"
            );
        }

        // The layer holding the fixed copper is the one tilting the stack, so
        // it sheds density and its mirror takes density on, neither past its
        // step.
        let deviations = deviations(&solve(flex, 1.0));
        assert!(deviations[0] < -QUANTIZATION, "{deviations:?}");
        assert!(deviations[1] > QUANTIZATION, "{deviations:?}");
        assert!(
            deviations
                .iter()
                .all(|step| step.abs() <= flex + QUANTIZATION),
            "{deviations:?}"
        );
    }

    /// Mirrored layers over one bare panel, asked for markedly different
    /// copper: the imbalance is in the targets themselves.
    fn mirrored_pair(profile: DenseCopperBalanceProfile) -> SpatialCopperBalance {
        let panel = rect(0.0, 0.0, 40.0, 20.0);
        let empty = empty();
        solve(
            profile,
            &panel,
            &[
                layer(&panel, &empty, &panel, 0.70, 1.0),
                layer(&panel, &empty, &panel, 0.40, -1.0),
            ],
        )
        .unwrap()
    }

    /// Mirrored weights normalize to +/- 0.5, so this is the moment.
    fn mirrored_moment(balance: &SpatialCopperBalance) -> f64 {
        0.5 * (balance.layers[0].solution.achieved_density
            - balance.layers[1].solution.achieved_density)
    }

    /// Two layers that both sit exactly on their targets can still carry a
    /// copper moment, because their targets differ and the boards were drawn
    /// that way. Balancing against the deviation from target cannot see it —
    /// the deviations are zero. Balancing against the copper itself does, and
    /// spends the step pulling the heavy layer down and the light one up.
    #[test]
    fn stack_moment_shrinks_when_the_boards_themselves_are_asymmetric() {
        let ignored = mirrored_pair(DenseCopperBalanceProfile {
            stack_flex_density: 0.0,
            ..V1
        });
        let weighted = mirrored_pair(V1);

        // Ignored, the boards' own imbalance survives untouched: with a zero
        // step each layer holds its own target and has nothing to trade.
        let untouched = 0.5 * (0.70 - 0.40);
        assert!((mirrored_moment(&ignored) - untouched).abs() <= QUANTIZATION);
        // Weighted, the same panel carries measurably less.
        assert!(mirrored_moment(&weighted) < mirrored_moment(&ignored) - QUANTIZATION);
        // And it is paid for by a trade, not by removing copper from the panel.
        let deviations = deviations(&weighted);
        assert!(deviations[0] < 0.0 && deviations[1] > 0.0, "{deviations:?}");
        assert!(
            deviations.iter().sum::<f64>().abs() <= 1e-2,
            "{deviations:?}"
        );
        assert!(
            deviations
                .iter()
                .all(|step| step.abs() <= V1.stack_flex_density + QUANTIZATION),
            "{deviations:?}"
        );

        // The field metric records the same flattening. Both readings come
        // from one field over one set of sites, so the RMS bounds the mean's
        // magnitude; measuring them apart would let that slip unnoticed.
        let field = weighted.moment_field.expect("weights were supplied");
        assert!(field.initial_rms >= field.initial_mean.abs(), "{field:?}");
        assert!(field.achieved_rms >= field.achieved_mean.abs(), "{field:?}");
        assert!(field.achieved_rms < field.initial_rms, "{field:?}");
    }

    /// The lattice that ships is the quantized one, so the achieved moment has
    /// to be read from it. With two area levels every void snaps to an extreme
    /// and the emitted field sits far from the converged iterate; the reading
    /// has to follow the densities the layers actually achieved.
    #[test]
    fn achieved_moment_is_read_from_the_emitted_radii() {
        let balance = mirrored_pair(DenseCopperBalanceProfile {
            void_area_levels: 2,
            ..V1
        });
        let emitted = mirrored_moment(&balance);
        let field = balance.moment_field.expect("weights were supplied");
        assert!(
            (field.achieved_mean - emitted).abs() <= 0.02,
            "reported {} but emitted {emitted}",
            field.achieved_mean
        );
    }

    /// The closed-form area projection has to be at least as close to the
    /// request as any radius a scan of the whole range finds, including just
    /// either side of the steps the edge voids take from level to level.
    #[test]
    fn area_projection_is_the_best_radius_in_the_range() {
        let safe_region = rect(0.0, 0.0, 9.0, 6.0);
        let voidable = safe_region.disk_erode(V1.boundary_web_mm).unwrap();
        let lattice = LatticeCandidates::build_lattice(&voidable, Point::ZERO, V1).unwrap();
        let usable_area_mm2 = safe_region.area();
        // Edge voids dominate a region this small, so the steps are large.
        let steps = lattice.edge_area_by_level_mm2.windows(2);
        assert!(steps.clone().all(|pair| pair[1] >= pair[0]));
        assert!(steps.map(|pair| pair[1] - pair[0]).fold(0.0, f64::max) > 0.1);

        let (lower, upper) = V1.void_area_bounds();
        for request in 0..=40 {
            let desired_added_area_mm2 = usable_area_mm2 * request as f64 / 40.0;
            let projected =
                project_perforated_geometry(&lattice, V1, usable_area_mm2, desired_added_area_mm2);
            let scanned = (0..=20_000)
                .map(|index| (lower + (upper - lower) * index as f64 / 20_000.0).sqrt())
                .map(|radius| {
                    (usable_area_mm2 - lattice.void_area(radius, V1) - desired_added_area_mm2).abs()
                })
                .fold(f64::MAX, f64::min);
            assert!(
                projected.error_mm2 <= scanned + 1e-6,
                "request {request}: projected {} but a scan finds {scanned}",
                projected.error_mm2
            );
        }
    }

    #[test]
    fn geometric_projection_never_worsens_target_sweep() {
        let panel = rect(0.0, 0.0, 10.0, 10.0);
        let safe = rect(0.0, 0.0, 8.0, 5.0);
        let existing = rect(0.0, 6.0, 10.0, 8.0);
        for target_step in 0..=10 {
            let target_density = target_step as f64 / 10.0;
            let solution = solve(
                V1,
                &panel,
                &[layer(&safe, &existing, &panel, target_density, 0.0)],
            )
            .unwrap()
            .layers[0]
                .solution;
            assert!(
                solution.residual_error
                    <= (solution.initial_density - target_density).abs() + NUMERIC_EPSILON,
                "{solution:?}"
            );
        }
    }
}
