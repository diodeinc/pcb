//! Dense copper balancing over an explicitly supplied safe region.
//!
//! This module intentionally does not discover safe panel regions or inspect
//! PCB layer semantics. Callers supply the addable region and the measured
//! per-layer areas; the solver chooses the closest manufacturable copper area
//! and generates a deterministic perforated plane.

use std::collections::HashMap;

use crate::geom::{ContourSet, Point};

mod lattice;
mod spatial;

pub use lattice::rounded_hexagonal_void;

use lattice::{
    LatticeCandidates, ROUNDED_HEXAGON_AREA_FACTOR, hex_aligned_lattice_centers, lattice_index,
};
use spatial::{
    CoupledBand, LatticeDensityKernel, density_evaluation_points, lattice_cell_coverage,
    normalized_stack_weights, project_coupled_bands, spatial_result_from_squared_radii,
};

const NUMERIC_EPSILON: f64 = 1e-9;
const RADIUS_SOLVE_TOLERANCE_MM: f64 = 1e-4;
const AREA_SOLVE_TOLERANCE_MM2: f64 = 1e-3;
/// Leftover area tolerated when certifying that one region contains another.
/// Roughly a 30 um square: three orders of magnitude below the smallest void
/// the profile can place, and above the slivers a regularized difference
/// leaves where its operands share an edge.
const CONTAINMENT_AREA_TOLERANCE_MM2: f64 = 1e-3;
// Gradient information travels about one kernel support per iteration under
// the fixed conservative step, so the radius field needs a few hundred
// iterations to equilibrate across a panel; measured objectives plateau by
// roughly 500 at both board-array and fabrication-panel scale. Iterations are
// cheap next to lattice coverage sampling.
const SPATIAL_SOLVE_ITERATIONS: usize = 512;
const SQRT_3: f64 = 1.732_050_807_568_877_2;

/// Fixed geometry constraints for a dense perforated copper plane.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DenseCopperBalanceProfile {
    pub pitch_mm: f64,
    pub min_void_radius_mm: f64,
    pub max_void_radius_mm: f64,
    pub min_copper_web_mm: f64,
    pub boundary_web_mm: f64,
    pub density_sigma_mm: f64,
    /// Fabrication step the spatially solved void radii snap to. Etching
    /// cannot hold finer distinctions, and the shared grid keeps the voids a
    /// small set of repeated templates instead of thousands of unique shapes.
    pub void_radius_step_mm: f64,
    /// How far a layer's density may end up from its target to flatten the
    /// stack's copper moment, as a fraction of its density domain.
    ///
    /// The spatial solve otherwise conserves each layer's selected copper area
    /// exactly, which lets it redistribute copper but never trade a little of
    /// one layer's density match for a flatter moment. This band frees that
    /// trade and is the bound on it: no layer's achieved density leaves this
    /// distance from its target, whatever [`Self::stack_moment_weight`] asks
    /// for. Zero pins every layer to its uniform selection, the behavior
    /// before the band existed, and nothing below can move it.
    ///
    /// The cap earns its keep on boards the weight alone would maul. A
    /// two-layer panel poured 0.9 against 0.3 carries a moment worth a 30%
    /// correction; this is what refuses to spend it.
    pub stack_flex_density: f64,
    /// How hard the through-stack moment competes with each layer's own
    /// density match.
    ///
    /// The objective already holds both terms, so this only sets their
    /// exchange rate — but leaving it at one is far too timid to be useful.
    /// The local term contributes one squared error per layer against this
    /// term's one, so the moment that survives is `M_0 / (1 + w * sum_l w_l^2)`
    /// and a six-layer stack sheds only about a sixth of it. Raising the
    /// weight moves that toward zero; [`Self::stack_flex_density`] is what
    /// keeps the correction bounded while it does.
    ///
    /// Zero drops the moment from the objective, leaving each layer to match
    /// its own density alone. A stackup that supplies no weights reaches the
    /// same place on its own, whatever this is set to.
    pub stack_moment_weight: f64,
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
        void_radius_step_mm: 0.005,
        stack_flex_density: 0.01,
        stack_moment_weight: 10.0,
    };

    pub fn lattice_column_pitch_mm(self) -> f64 {
        self.pitch_mm * SQRT_3 / 2.0
    }

    /// Disk radius used to reject partial voids narrower than
    /// `min_void_radius_mm`.
    pub fn minimum_partial_void_inradius_mm(self) -> f64 {
        self.min_void_radius_mm / 2.0
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
            ("void radius step", self.void_radius_step_mm),
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
        if !self.stack_moment_weight.is_finite() || self.stack_moment_weight < 0.0 {
            return Err(DenseCopperBalanceError::InvalidProfile(
                "stack moment weight must be finite and non-negative".to_string(),
            ));
        }
        if self.min_void_radius_mm > self.max_void_radius_mm {
            return Err(DenseCopperBalanceError::InvalidProfile(
                "minimum void radius exceeds maximum void radius".to_string(),
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

/// Geometry inputs for the internal single-layer baseline solve.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DenseCopperBalanceRequest<'a> {
    /// Safe, initially empty subset of the density domain.
    pub safe_region: &'a ContourSet,
    /// Area of the density domain, used as the density denominator.
    pub density_domain_area_mm2: f64,
    /// Fixed copper anywhere in that density domain on this layer.
    pub existing_copper_area_mm2: f64,
    pub target_density: f64,
    pub lattice_origin: Point,
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

/// One full, unclipped rounded-hex void.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DenseCopperVoid {
    pub center: Point,
    pub radius_mm: f64,
}

/// What the panel's copper-moment field measured before the spatial solve
/// redistributed anything, and after it settled.
///
/// The field's mean is the panel's bow and its variation is twist, so an RMS
/// carries both: a falling RMS means the field flattened, not merely that a
/// positive lobe found a negative one to cancel against. Comparing it against
/// the mean alone separates the two — a mean that drops while the RMS holds
/// has moved bow into twist.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StackMomentField {
    pub initial_rms: f64,
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
    /// Safe, initially empty region available for generated copper.
    pub usable: ContourSet,
    /// Interior voids that retain the exact rounded-hex template.
    pub full_voids: Vec<DenseCopperVoid>,
    /// Edge voids that require clipping to the boundary web.
    pub partial_voids: ContourSet,
}

impl DenseCopperBalanceResult {
    pub fn void_count(&self) -> usize {
        self.full_voids.len() + self.partial_voids.connected_components().len()
    }

    pub fn full_void_radius_range_mm(&self) -> Option<(f64, f64)> {
        let min = self
            .full_voids
            .iter()
            .map(|void| void.radius_mm)
            .min_by(f64::total_cmp)?;
        let max = self
            .full_voids
            .iter()
            .map(|void| void.radius_mm)
            .max_by(f64::total_cmp)?;
        Some((min, max))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DenseCopperBalanceError {
    InvalidProfile(String),
    InvalidInput(String),
}

impl std::fmt::Display for DenseCopperBalanceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidProfile(message) => write!(f, "invalid copper balance profile: {message}"),
            Self::InvalidInput(message) => write!(f, "invalid copper balance input: {message}"),
        }
    }
}

impl std::error::Error for DenseCopperBalanceError {}

/// Generate a dense copper balance region from an explicit safe region.
///
/// Test-only wrapper over the internal baseline of
/// [`generate_spatial_dense_copper_balance`], which is the sole public entry
/// point; a single zero-weight layer over its own panel region reproduces
/// this uniform solve.
#[cfg(test)]
pub(crate) fn generate_dense_copper_balance(
    profile: DenseCopperBalanceProfile,
    request: DenseCopperBalanceRequest<'_>,
) -> Result<DenseCopperBalanceResult, DenseCopperBalanceError> {
    profile.validate()?;
    validate_request(request)?;

    let usable = request.safe_region.clone();
    let voidable = usable.disk_erode(profile.boundary_web_mm);
    let lattice = LatticeCandidates::new(&voidable, request.lattice_origin, profile);
    Ok(generate_dense_copper_balance_with_lattice(
        profile, request, usable, &voidable, &lattice,
    ))
}

fn generate_dense_copper_balance_with_lattice(
    profile: DenseCopperBalanceProfile,
    request: DenseCopperBalanceRequest<'_>,
    usable: ContourSet,
    voidable: &ContourSet,
    lattice: &LatticeCandidates,
) -> DenseCopperBalanceResult {
    let usable_area_mm2 = usable.area();
    let desired_added_area_mm2 =
        request.target_density * request.density_domain_area_mm2 - request.existing_copper_area_mm2;
    let initial_density = request.existing_copper_area_mm2 / request.density_domain_area_mm2;
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
            voidable,
            usable_area_mm2,
            desired_added_area_mm2,
        ));
    }

    let (full_voids, partial_voids) = match best.mode {
        DenseCopperBalanceMode::Perforated { void_radius_mm } => (
            lattice
                .full_centers
                .iter()
                .map(|center| DenseCopperVoid {
                    center: *center,
                    radius_mm: void_radius_mm,
                })
                .collect(),
            lattice.partial_voids(voidable, void_radius_mm, profile),
        ),
        DenseCopperBalanceMode::None | DenseCopperBalanceMode::Solid => {
            (Vec::new(), ContourSet::empty(usable.tolerance))
        }
    };
    let achieved_density =
        (request.existing_copper_area_mm2 + best.area_mm2) / request.density_domain_area_mm2;
    let solution = DenseCopperBalanceSolution {
        mode: best.mode,
        desired_added_area_mm2,
        generated_area_mm2: best.area_mm2,
        initial_density,
        achieved_density,
        target_density: request.target_density,
        residual_error: (achieved_density - request.target_density).abs(),
    };
    debug_assert!(
        solution.residual_error
            <= (solution.initial_density - request.target_density).abs() + NUMERIC_EPSILON
    );
    DenseCopperBalanceResult {
        solution,
        usable,
        full_voids,
        partial_voids,
    }
}

/// Jointly distribute each layer's already-selected copper area in space.
///
/// The solver uses squared void radius as its variable. Each layer scatters
/// only its admitted variables onto one panel lattice; normalized convolution
/// maps all layers to one evaluation field. Projected gradient then minimizes
/// local density error plus signed through-stack error while preserving each
/// layer's selected void area and radius bounds.
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
    // classify each distinct region once.
    let mut region_sources: Vec<&ContourSet> = Vec::new();
    let mut region_voidable: Vec<ContourSet> = Vec::new();
    let mut region_lattices: Vec<LatticeCandidates> = Vec::new();
    let layer_regions = request
        .layers
        .iter()
        .map(|layer| {
            if let Some(index) = region_sources
                .iter()
                .position(|region| region.rings == layer.safe_region.rings)
            {
                return index;
            }
            let voidable = layer.safe_region.disk_erode(profile.boundary_web_mm);
            region_lattices.push(LatticeCandidates::new(
                &voidable,
                request.lattice_origin,
                profile,
            ));
            region_voidable.push(voidable);
            region_sources.push(layer.safe_region);
            region_sources.len() - 1
        })
        .collect::<Vec<_>>();
    let uniform = std::thread::scope(|scope| {
        let solves = request
            .layers
            .iter()
            .zip(&layer_regions)
            .zip(&density_domain_areas)
            .map(|((layer, region_index), density_domain_area_mm2)| {
                let voidable = &region_voidable[*region_index];
                let lattice = &region_lattices[*region_index];
                scope.spawn(move || {
                    generate_dense_copper_balance_with_lattice(
                        profile,
                        DenseCopperBalanceRequest {
                            safe_region: layer.safe_region,
                            density_domain_area_mm2: *density_domain_area_mm2,
                            existing_copper_area_mm2: layer.existing_copper.area(),
                            target_density: layer.target_density,
                            lattice_origin: request.lattice_origin,
                        },
                        layer.safe_region.clone(),
                        voidable,
                        lattice,
                    )
                })
            })
            .collect::<Vec<_>>();
        solves
            .into_iter()
            .map(|solve| solve.join().expect("uniform copper-balance solve panicked"))
            .collect::<Vec<_>>()
    });

    let mut panel_samples =
        hex_aligned_lattice_centers(request.panel_region.bbox, request.lattice_origin, profile)
            .into_iter()
            .filter(|point| request.panel_region.contains_point(*point))
            .collect::<Vec<_>>();
    let mut sample_indices = panel_samples
        .iter()
        .enumerate()
        .map(|(index, point)| {
            (
                lattice_index(*point, request.lattice_origin, profile),
                index,
            )
        })
        .collect::<HashMap<_, _>>();
    // Full centers lie in eroded safe regions inside the panel; appending any
    // center the tolerance-bound point test missed keeps sample membership
    // structural rather than tolerance-dependent.
    let active_sites = layer_regions
        .iter()
        .map(|region_index| {
            region_lattices[*region_index]
                .full_centers
                .iter()
                .map(|center| {
                    *sample_indices
                        .entry(lattice_index(*center, request.lattice_origin, profile))
                        .or_insert_with(|| {
                            panel_samples.push(*center);
                            panel_samples.len() - 1
                        })
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    if active_sites.iter().all(Vec::is_empty) {
        return Ok(SpatialCopperBalance {
            layers: uniform,
            moment_field: None,
        });
    }

    // The objective lives on one coarse subset of the panel lattice. Every
    // layer scatters only its own void variables onto the full lattice, then
    // the same normalized convolution maps those fields to the common sites.
    let evaluation_points =
        density_evaluation_points(&panel_samples, request.lattice_origin, profile);
    let density_kernel = LatticeDensityKernel::new(
        &panel_samples,
        &evaluation_points,
        request.lattice_origin,
        profile,
    );
    let smooth_coverage = |region: &ContourSet| {
        density_kernel.smooth(&lattice_cell_coverage(&panel_samples, region, profile))
    };
    let (fixed_density, region_available_density, partial_void_density) =
        std::thread::scope(|scope| {
            let fixed = request
                .layers
                .iter()
                .map(|layer| scope.spawn(|| smooth_coverage(layer.existing_copper)))
                .collect::<Vec<_>>();
            let available = region_sources
                .iter()
                .map(|region| scope.spawn(|| smooth_coverage(region)))
                .collect::<Vec<_>>();
            let partial = uniform
                .iter()
                .map(|result| scope.spawn(|| smooth_coverage(&result.partial_voids)))
                .collect::<Vec<_>>();
            let join_all = |handles: Vec<std::thread::ScopedJoinHandle<'_, Vec<f64>>>| {
                handles
                    .into_iter()
                    .map(|handle| handle.join().expect("coverage sampling panicked"))
                    .collect::<Vec<_>>()
            };
            (join_all(fixed), join_all(available), join_all(partial))
        });

    let lower = profile.min_void_radius_mm.powi(2);
    let upper = profile.max_void_radius_mm.powi(2);
    // The uniform solve already selected each layer's full-void area at one
    // radius within the profile bounds, so the equal-radius field it implies
    // is already feasible and needs no projection.
    let mut squared_radii = uniform
        .iter()
        .enumerate()
        .map(|(layer_index, result)| match result.solution.mode {
            DenseCopperBalanceMode::Perforated { void_radius_mm } => {
                vec![void_radius_mm.powi(2); active_sites[layer_index].len()]
            }
            DenseCopperBalanceMode::None | DenseCopperBalanceMode::Solid => Vec::new(),
        })
        .collect::<Vec<Vec<f64>>>();
    let squared_radius_sums = squared_radii
        .iter()
        .map(|radii| radii.iter().sum::<f64>())
        .collect::<Vec<_>>();
    // Void area moves opposite to copper area, so a symmetric copper band is a
    // symmetric band on the squared-radius sum. Clamping to the range the box
    // itself can reach keeps the feasible set nonempty, and since each uniform
    // sum is already inside that range the band always contains its own center.
    let normalized_stack_weights = normalized_stack_weights(request.layers);
    // A stackup that located no conductors leaves every weight zero, so there
    // is no moment to flatten and nothing to spend the bands on. Holding them
    // shut is what keeps "balanced independently" true: an open band would
    // still let the local density error trade layers off their targets.
    let weights_measured = normalized_stack_weights.iter().any(|weight| *weight != 0.0);
    let squared_radius_bands = squared_radius_sums
        .iter()
        .enumerate()
        .map(|(layer_index, sum)| {
            let slack = if weights_measured {
                profile.stack_flex_density * density_domain_areas[layer_index]
                    / ROUNDED_HEXAGON_AREA_FACTOR
            } else {
                0.0
            };
            // A layer that took no lattice has no radii to trade with, and its
            // sites are not its own to spend: sizing the box range from them
            // would put the band above a sum that is fixed at zero.
            let site_count = squared_radii[layer_index].len() as f64;
            // The band always contains the sum it surrounds. Reaching that
            // through the clamps alone would rest on an accumulated sum
            // comparing exactly against a multiplied bound, and a layer whose
            // radii all sit on one bound can round across it.
            (
                (sum - slack).max(site_count * lower).min(*sum),
                (sum + slack).min(site_count * upper).max(*sum),
            )
        })
        .collect::<Vec<_>>();
    let cell_area_mm2 = SQRT_3 * profile.pitch_mm.powi(2) / 2.0;
    let void_fraction_per_radius_squared = ROUNDED_HEXAGON_AREA_FACTOR / cell_area_mm2;
    let target_densities = request
        .layers
        .iter()
        .map(|layer| layer.target_density)
        .collect::<Vec<_>>();
    let step = 0.25 / void_fraction_per_radius_squared.powi(2);

    // Updates below this leave every radius well inside half a quantization
    // step of its converged value, so the emitted lattice is already final.
    let convergence_mm2 = 1e-6;
    // Per-layer scratch reused across iterations: the void-fraction field is
    // written only at active sites (the rest stays zero), and the adjoint
    // fills its buffer, so neither needs re-zeroing per pass.
    let mut void_scratch = request
        .layers
        .iter()
        .map(|_| vec![0.0; panel_samples.len()])
        .collect::<Vec<_>>();
    let mut influence_scratch = void_scratch.clone();
    // The moment field is already built every iteration, so its RMS costs a
    // reduction. The first reading is the field the uniform selection left
    // behind; the last trails the emitted radii by one gradient step, which at
    // convergence is smaller than the radius quantization.
    let mut moment_rms: Option<(f64, f64)> = None;
    for _ in 0..SPATIAL_SOLVE_ITERATIONS {
        // The modeled final copper fraction of each layer, not its distance
        // from target: the moment below is the copper the panel carries, and
        // subtracting the targets would leave it blind to whatever imbalance
        // the boards were designed with.
        let density = std::thread::scope(|scope| {
            let uniform = &uniform;
            let active_sites = &active_sites;
            let fixed_density = &fixed_density;
            let region_available_density = &region_available_density;
            let layer_regions = &layer_regions;
            let partial_void_density = &partial_void_density;
            let density_kernel = &density_kernel;
            let evaluation_points = &evaluation_points;
            let handles = void_scratch
                .iter_mut()
                .enumerate()
                .map(|(layer_index, void_fraction)| {
                    let squared_radii = &squared_radii;
                    scope.spawn(move || {
                        let void_density = match uniform[layer_index].solution.mode {
                            DenseCopperBalanceMode::Perforated { .. } => {
                                for (sample_index, radius_squared) in active_sites[layer_index]
                                    .iter()
                                    .zip(&squared_radii[layer_index])
                                {
                                    void_fraction[*sample_index] =
                                        void_fraction_per_radius_squared * radius_squared;
                                }
                                density_kernel.smooth(void_fraction)
                            }
                            DenseCopperBalanceMode::None | DenseCopperBalanceMode::Solid => {
                                vec![0.0; evaluation_points.len()]
                            }
                        };
                        fixed_density[layer_index]
                            .iter()
                            .enumerate()
                            .map(|(site_index, fixed)| {
                                let available =
                                    &region_available_density[layer_regions[layer_index]];
                                let generated_density = match uniform[layer_index].solution.mode {
                                    DenseCopperBalanceMode::None => 0.0,
                                    DenseCopperBalanceMode::Solid => available[site_index],
                                    DenseCopperBalanceMode::Perforated { .. } => {
                                        available[site_index]
                                            - partial_void_density[layer_index][site_index]
                                            - void_density[site_index]
                                    }
                                };
                                fixed + generated_density
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .map(|handle| handle.join().expect("spatial density pass panicked"))
                .collect::<Vec<_>>()
        });
        // The panel's copper moment about its mid-plane. Squaring this field
        // pointwise charges for both warp modes at once: bow is its mean,
        // twist is its variation.
        let stack_moment = (0..evaluation_points.len())
            .map(|site_index| {
                normalized_stack_weights
                    .iter()
                    .enumerate()
                    .map(|(layer_index, weight)| weight * density[layer_index][site_index])
                    .sum::<f64>()
            })
            .collect::<Vec<_>>();
        let rms = (stack_moment
            .iter()
            .map(|moment| moment * moment)
            .sum::<f64>()
            / stack_moment.len() as f64)
            .sqrt();
        let initial_rms = moment_rms.map_or(rms, |(initial, _)| initial);
        moment_rms = Some((initial_rms, rms));

        let proposals = std::thread::scope(|scope| {
            let active_sites = &active_sites;
            let density_kernel = &density_kernel;
            let normalized_stack_weights = &normalized_stack_weights;
            let handles = squared_radii
                .iter()
                .zip(influence_scratch.iter_mut())
                .enumerate()
                .map(|(layer_index, (radii, influence))| {
                    let density = &density;
                    let stack_moment = &stack_moment;
                    let target_densities = &target_densities;
                    scope.spawn(move || {
                        if radii.is_empty() {
                            return Vec::new();
                        }
                        // Its own density error, plus its share of the moment
                        // it helps carry, weighted by how hard the moment is
                        // allowed to argue.
                        let residual = density[layer_index]
                            .iter()
                            .zip(stack_moment)
                            .map(|(density, moment)| {
                                (density - target_densities[layer_index])
                                    + profile.stack_moment_weight
                                        * normalized_stack_weights[layer_index]
                                        * moment
                            })
                            .collect::<Vec<_>>();
                        density_kernel.smooth_adjoint_into(&residual, influence);
                        radii
                            .iter()
                            .enumerate()
                            .map(|(local_index, radius_squared)| {
                                radius_squared
                                    + step
                                        * void_fraction_per_radius_squared
                                        * influence[active_sites[layer_index][local_index]]
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .map(|handle| handle.join().expect("spatial update pass panicked"))
                .collect::<Vec<_>>()
        });
        // The layers meet here: their bands are only useful against each other,
        // so the trade between them is settled before any radius is committed.
        let bands = proposals
            .iter()
            .enumerate()
            .map(|(layer_index, proposal)| CoupledBand {
                proposal,
                sum_bounds: squared_radius_bands[layer_index],
                center: squared_radius_sums[layer_index],
                domain_area_mm2: density_domain_areas[layer_index],
            })
            .collect::<Vec<_>>();
        let update = squared_radii
            .iter_mut()
            .zip(project_coupled_bands(&bands, lower, upper))
            .map(|(radii, projected)| {
                let update = radii
                    .iter()
                    .zip(&projected)
                    .map(|(before, after)| (before - after).abs())
                    .fold(0.0_f64, f64::max);
                *radii = projected;
                update
            })
            .fold(0.0_f64, f64::max);
        if update < convergence_mm2 {
            break;
        }
    }

    Ok(SpatialCopperBalance {
        layers: uniform
            .into_iter()
            .enumerate()
            .map(|(layer_index, baseline)| {
                if squared_radii[layer_index].is_empty() {
                    return baseline;
                }
                spatial_result_from_squared_radii(
                    &region_lattices[layer_regions[layer_index]].full_centers,
                    &squared_radii[layer_index],
                    baseline,
                    request.layers[layer_index],
                    density_domain_areas[layer_index],
                    profile,
                )
            })
            .collect(),
        moment_field: moment_rms
            .filter(|_| weights_measured)
            .map(|(initial_rms, achieved_rms)| StackMomentField {
                initial_rms,
                achieved_rms,
            }),
    })
}

/// Whether `outer` contains `inner`, ignoring boolean sliver artifacts.
///
/// A regularized difference between operands that share an edge leaves
/// sub-micron slivers along it, so exact emptiness is not a usable containment
/// test here. A genuine containment error — a domain that omits real copper or
/// real fillable material — is orders of magnitude above this bound, which is
/// itself far below the smallest void the profile can place.
fn contains(outer: &ContourSet, inner: &ContourSet) -> bool {
    let leftover = inner.difference(outer);
    leftover.is_empty() || leftover.area() <= CONTAINMENT_AREA_TOLERANCE_MM2
}

fn validate_spatial_request(
    request: SpatialCopperBalanceRequest<'_>,
) -> Result<(), DenseCopperBalanceError> {
    if !request.panel_region.bbox.is_valid() || request.panel_region.is_empty() {
        return Err(DenseCopperBalanceError::InvalidInput(
            "panel region must be non-empty and have valid bounds".to_string(),
        ));
    }
    // A fab panel gives every copper layer the same domain and safe region,
    // and a board array shares them across layers of equal support scope, so
    // certify each distinct pair once rather than repeating identical boolean
    // work per layer.
    let mut certified: Vec<(&ContourSet, &ContourSet)> = Vec::new();
    for layer in request.layers {
        if !layer.stack_weight_mm2.is_finite() {
            return Err(DenseCopperBalanceError::InvalidInput(
                "stack weights must be finite".to_string(),
            ));
        }
        // The density domain is assembled from the very regions checked
        // against it, so every containment predicate here compares operands
        // that share long stretches of boundary. Containment through the
        // density domain also implies containment by the panel region, so the
        // safe region and fixed copper need no separate panel-region check.
        if !certified.iter().any(|(domain, safe_region)| {
            domain.rings == layer.density_domain.rings
                && safe_region.rings == layer.safe_region.rings
        }) {
            if !contains(request.panel_region, layer.density_domain) {
                return Err(DenseCopperBalanceError::InvalidInput(
                    "density domain must be contained by the panel region".to_string(),
                ));
            }
            if !contains(layer.density_domain, layer.safe_region) {
                return Err(DenseCopperBalanceError::InvalidInput(
                    "safe region must be contained by the density domain".to_string(),
                ));
            }
            certified.push((layer.density_domain, layer.safe_region));
        }
        if !contains(layer.density_domain, layer.existing_copper) {
            return Err(DenseCopperBalanceError::InvalidInput(
                "existing copper must be contained by the density domain".to_string(),
            ));
        }
        // Fixed copper and the safe region are separated by a clearance rule
        // rather than by a shared edge, so their overlap needs no tolerance.
        if !layer
            .safe_region
            .intersection(layer.existing_copper)
            .is_empty()
        {
            return Err(DenseCopperBalanceError::InvalidInput(
                "existing copper and safe region must be disjoint".to_string(),
            ));
        }
        validate_request(DenseCopperBalanceRequest {
            safe_region: layer.safe_region,
            density_domain_area_mm2: layer.density_domain.area(),
            existing_copper_area_mm2: layer.existing_copper.area(),
            target_density: layer.target_density,
            lattice_origin: request.lattice_origin,
        })?;
    }
    Ok(())
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
    voidable: &ContourSet,
    usable_area_mm2: f64,
    desired_added_area_mm2: f64,
) -> ProjectedArea {
    // Each edge site has an activation radius aᵢ. At nominal radius r its
    // clipped hex uses max(r, aᵢ), making total void area monotone in r.
    // Project the requested area onto that one-dimensional feasible set.
    let target_void_area_mm2 = usable_area_mm2 - desired_added_area_mm2;
    let mut low_radius = profile.min_void_radius_mm;
    let mut high_radius = profile.max_void_radius_mm;
    let low_void_area = lattice.void_area(voidable, low_radius, profile);
    let high_void_area = lattice.void_area(voidable, high_radius, profile);
    let mut best = perforated_candidate(
        low_radius,
        low_void_area,
        usable_area_mm2,
        desired_added_area_mm2,
    );
    best.consider(perforated_candidate(
        high_radius,
        high_void_area,
        usable_area_mm2,
        desired_added_area_mm2,
    ));

    if target_void_area_mm2 <= low_void_area || target_void_area_mm2 >= high_void_area {
        return best;
    }

    // Every candidate evaluation clips the boundary voids against the safe
    // region, so the exit tolerance scales with the region: the absolute
    // floor governs small solves exactly, while large panels stop once the
    // area error is negligible relative to their usable area.
    let area_tolerance_mm2 = AREA_SOLVE_TOLERANCE_MM2.max(1e-6 * usable_area_mm2);
    let mut low_area = low_void_area;
    let mut high_area = high_void_area;
    while high_radius - low_radius > RADIUS_SOLVE_TOLERANCE_MM
        && best.error_mm2 > area_tolerance_mm2
    {
        // Interior hex area is linear in r². Interpolate there, with a
        // midpoint fallback that keeps convergence bracketed when clipped
        // edge area dominates.
        let low_squared = low_radius.powi(2);
        let high_squared = high_radius.powi(2);
        let fraction = (target_void_area_mm2 - low_area) / (high_area - low_area);
        let squared_radius = if (0.1..=0.9).contains(&fraction) {
            low_squared + fraction * (high_squared - low_squared)
        } else {
            (low_squared + high_squared) / 2.0
        };
        let radius = squared_radius.sqrt();
        let void_area = lattice.void_area(voidable, radius, profile);
        best.consider(perforated_candidate(
            radius,
            void_area,
            usable_area_mm2,
            desired_added_area_mm2,
        ));
        if void_area < target_void_area_mm2 {
            low_radius = radius;
            low_area = void_area;
        } else {
            high_radius = radius;
            high_area = void_area;
        }
    }
    best
}

fn perforated_candidate(
    radius: f64,
    void_area_mm2: f64,
    usable_area_mm2: f64,
    desired_added_area_mm2: f64,
) -> ProjectedArea {
    let area_mm2 = (usable_area_mm2 - void_area_mm2).max(0.0);
    ProjectedArea::new(
        DenseCopperBalanceMode::Perforated {
            void_radius_mm: radius,
        },
        area_mm2,
        desired_added_area_mm2,
    )
}

fn validate_request(request: DenseCopperBalanceRequest<'_>) -> Result<(), DenseCopperBalanceError> {
    if !request.lattice_origin.is_finite() {
        return Err(DenseCopperBalanceError::InvalidInput(
            "lattice origin must be finite".to_string(),
        ));
    }
    if !request.safe_region.bbox.is_valid() {
        return Err(DenseCopperBalanceError::InvalidInput(
            "safe region has invalid bounds".to_string(),
        ));
    }
    if !request.density_domain_area_mm2.is_finite() || request.density_domain_area_mm2 <= 0.0 {
        return Err(DenseCopperBalanceError::InvalidInput(
            "density domain area must be finite and greater than zero".to_string(),
        ));
    }
    // These three bounds are the scalar shadow of the geometric containments
    // `C subset D`, `S subset D`, and `C disjoint S`, so they tolerate the same
    // sliver area those predicates do. Holding them to NUMERIC_EPSILON would
    // reject a request whose geometry passed containment, and the slack only
    // vanishes when the footprints are fully poured.
    if !request.existing_copper_area_mm2.is_finite()
        || request.existing_copper_area_mm2 < 0.0
        || request.existing_copper_area_mm2
            > request.density_domain_area_mm2 + CONTAINMENT_AREA_TOLERANCE_MM2
    {
        return Err(DenseCopperBalanceError::InvalidInput(
            "existing copper area must be between zero and the density domain area".to_string(),
        ));
    }
    if !request.target_density.is_finite() || !(0.0..=1.0).contains(&request.target_density) {
        return Err(DenseCopperBalanceError::InvalidInput(
            "target density must be between zero and one".to_string(),
        ));
    }
    let usable_area_mm2 = request.safe_region.area();
    if !usable_area_mm2.is_finite()
        || usable_area_mm2 < 0.0
        || usable_area_mm2 > request.density_domain_area_mm2 + CONTAINMENT_AREA_TOLERANCE_MM2
    {
        return Err(DenseCopperBalanceError::InvalidInput(
            "usable area must be between zero and the density domain area".to_string(),
        ));
    }
    if request.existing_copper_area_mm2 + usable_area_mm2
        > request.density_domain_area_mm2 + CONTAINMENT_AREA_TOLERANCE_MM2
    {
        return Err(DenseCopperBalanceError::InvalidInput(
            "existing copper and usable areas together exceed the density domain area".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::lattice::hexagon_set_with_radii;
    use super::*;
    use crate::geom::{BBox, FillRule, Point, tol};

    fn result_voids(result: &DenseCopperBalanceResult) -> ContourSet {
        match result.solution.mode {
            DenseCopperBalanceMode::Perforated { .. } => {}
            _ => return ContourSet::empty(result.usable.tolerance),
        }
        let candidates = result
            .full_voids
            .iter()
            .map(|void| (void.center, void.radius_mm))
            .collect::<Vec<_>>();
        let mut rings = hexagon_set_with_radii(&candidates, result.usable.tolerance).rings;
        rings.extend(result.partial_voids.rings.clone());
        ContourSet::new(rings, FillRule::NonZero, result.usable.tolerance)
    }

    #[test]
    fn clipped_lattice_matches_target_and_preserves_both_webs() {
        let safe_region = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(20.0, 10.0)),
            tol::REGION_MM,
        );
        let result = generate_dense_copper_balance(
            DenseCopperBalanceProfile::V1,
            DenseCopperBalanceRequest {
                safe_region: &safe_region,
                density_domain_area_mm2: 200.0,
                existing_copper_area_mm2: 0.0,
                target_density: 0.75,
                lattice_origin: Point::new(10.0, 5.0),
            },
        )
        .unwrap();

        let DenseCopperBalanceMode::Perforated { void_radius_mm } = result.solution.mode else {
            panic!("expected perforated balance");
        };
        assert!((0.20..=0.65).contains(&void_radius_mm));
        assert!(result.void_count() > 0);
        assert!((result.solution.achieved_density - 0.75).abs() <= 5e-3);
        assert!(result.solution.residual_error < (result.solution.initial_density - 0.75).abs());
        let voids = result_voids(&result);
        let voidable = safe_region.disk_erode(DenseCopperBalanceProfile::V1.boundary_web_mm);
        assert!(voids.difference(&voidable).is_empty());
        assert!(
            voids
                .disk_inter_component_gap_violations(
                    DenseCopperBalanceProfile::V1.min_copper_web_mm / 2.0
                )
                .is_empty()
        );
        let minimum_core_radius = DenseCopperBalanceProfile::V1.minimum_partial_void_inradius_mm();
        assert!(
            voids
                .connected_components()
                .into_iter()
                .all(|void| !void.disk_erode(minimum_core_radius).is_empty())
        );
    }

    #[test]
    fn tight_pitch_profile_keeps_voids_inside_the_boundary_web() {
        // Pitch below twice the maximum void radius: boundary sites must be
        // classified by hexagon containment, not center proximity.
        let profile = DenseCopperBalanceProfile {
            pitch_mm: 1.2,
            min_void_radius_mm: 0.2,
            max_void_radius_mm: 0.65,
            min_copper_web_mm: 0.05,
            boundary_web_mm: 0.2,
            density_sigma_mm: 5.0,
            void_radius_step_mm: 0.005,
            stack_flex_density: 0.0,
            stack_moment_weight: 0.0,
        };
        profile.validate().unwrap();
        let safe_region = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(12.0, 8.0)),
            tol::REGION_MM,
        );
        let result = generate_dense_copper_balance(
            profile,
            DenseCopperBalanceRequest {
                safe_region: &safe_region,
                density_domain_area_mm2: 96.0,
                existing_copper_area_mm2: 0.0,
                target_density: 0.15,
                lattice_origin: Point::new(0.0, 0.0),
            },
        )
        .unwrap();

        let DenseCopperBalanceMode::Perforated { void_radius_mm } = result.solution.mode else {
            panic!("expected perforated balance");
        };
        assert!(void_radius_mm > 0.6, "expected near-maximum voids");
        let voids = result_voids(&result);
        let voidable = safe_region.disk_erode(profile.boundary_web_mm);
        assert!(voids.difference(&voidable).is_empty());
    }

    #[test]
    fn retains_useful_partial_voids_at_the_boundary() {
        let safe_region = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(4.0, 4.0)),
            tol::REGION_MM,
        );
        let result = generate_dense_copper_balance(
            DenseCopperBalanceProfile::V1,
            DenseCopperBalanceRequest {
                safe_region: &safe_region,
                density_domain_area_mm2: 16.0,
                existing_copper_area_mm2: 0.0,
                target_density: 0.45,
                lattice_origin: Point::new(0.0, 0.0),
            },
        )
        .unwrap();

        assert!(matches!(
            result.solution.mode,
            DenseCopperBalanceMode::Perforated { .. }
        ));
        let voidable = safe_region.disk_erode(DenseCopperBalanceProfile::V1.boundary_web_mm);
        let voids = result_voids(&result).connected_components();
        let touches_each_boundary = [
            voids
                .iter()
                .any(|void| (void.bbox.min.x - voidable.bbox.min.x).abs() <= 2.0 * tol::FLATTEN_MM),
            voids
                .iter()
                .any(|void| (void.bbox.min.y - voidable.bbox.min.y).abs() <= 2.0 * tol::FLATTEN_MM),
            voids
                .iter()
                .any(|void| (void.bbox.max.x - voidable.bbox.max.x).abs() <= 2.0 * tol::FLATTEN_MM),
            voids
                .iter()
                .any(|void| (void.bbox.max.y - voidable.bbox.max.y).abs() <= 2.0 * tol::FLATTEN_MM),
        ];
        assert!(touches_each_boundary.into_iter().all(|touches| touches));
    }

    #[test]
    fn spatial_solver_accepts_no_layers() {
        let panel = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(24.0, 16.0)),
            tol::REGION_MM,
        );
        let result = generate_spatial_dense_copper_balance(
            DenseCopperBalanceProfile::V1,
            SpatialCopperBalanceRequest {
                panel_region: &panel,
                lattice_origin: Point::ZERO,
                layers: &[],
            },
        )
        .unwrap();

        assert!(result.layers.is_empty());
    }

    #[test]
    fn spatial_solver_rejects_existing_copper_in_the_safe_region() {
        let panel = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(20.0, 12.0)),
            tol::REGION_MM,
        );
        let safe = ContourSet::rectangle(
            BBox::new(Point::new(10.0, 0.0), Point::new(20.0, 12.0)),
            tol::REGION_MM,
        );
        let existing = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(11.0, 12.0)),
            tol::REGION_MM,
        );
        let layers = [SpatialCopperBalanceLayerRequest {
            safe_region: &safe,
            existing_copper: &existing,
            density_domain: &panel,
            target_density: 0.5,
            stack_weight_mm2: 0.0,
        }];

        let error = generate_spatial_dense_copper_balance(
            DenseCopperBalanceProfile::V1,
            SpatialCopperBalanceRequest {
                panel_region: &panel,
                lattice_origin: Point::ZERO,
                layers: &layers,
            },
        )
        .unwrap_err();

        assert_eq!(
            error,
            DenseCopperBalanceError::InvalidInput(
                "existing copper and safe region must be disjoint".to_string()
            )
        );
    }

    #[test]
    fn spatial_solver_rejects_geometry_outside_its_containing_region() {
        let panel = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(20.0, 12.0)),
            tol::REGION_MM,
        );
        let inside = ContourSet::rectangle(
            BBox::new(Point::new(10.0, 0.0), Point::new(20.0, 12.0)),
            tol::REGION_MM,
        );
        let outside = ContourSet::rectangle(
            BBox::new(Point::new(-1.0, 0.0), Point::new(5.0, 12.0)),
            tol::REGION_MM,
        );
        let empty = ContourSet::empty(tol::REGION_MM);
        let solve = |safe_region, existing_copper, density_domain| {
            let layers = [SpatialCopperBalanceLayerRequest {
                safe_region,
                existing_copper,
                density_domain,
                target_density: 0.5,
                stack_weight_mm2: 0.0,
            }];
            generate_spatial_dense_copper_balance(
                DenseCopperBalanceProfile::V1,
                SpatialCopperBalanceRequest {
                    panel_region: &panel,
                    lattice_origin: Point::ZERO,
                    layers: &layers,
                },
            )
        };

        assert_eq!(
            solve(&inside, &empty, &outside).unwrap_err(),
            DenseCopperBalanceError::InvalidInput(
                "density domain must be contained by the panel region".to_string()
            )
        );
        assert_eq!(
            solve(&outside, &empty, &inside).unwrap_err(),
            DenseCopperBalanceError::InvalidInput(
                "safe region must be contained by the density domain".to_string()
            )
        );
        assert_eq!(
            solve(&inside, &outside, &inside).unwrap_err(),
            DenseCopperBalanceError::InvalidInput(
                "existing copper must be contained by the density domain".to_string()
            )
        );
    }

    #[test]
    fn spatial_solver_preserves_minimum_radius_area() {
        let profile = DenseCopperBalanceProfile::V1;
        let panel = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(24.0, 16.0)),
            tol::REGION_MM,
        );
        let voidable = panel.disk_erode(profile.boundary_web_mm);
        let lattice = LatticeCandidates::new(&voidable, Point::ZERO, profile);
        let target_density = (panel.area()
            - lattice.void_area(&voidable, profile.min_void_radius_mm, profile))
            / panel.area();
        let existing = ContourSet::empty(tol::REGION_MM);
        let layer = SpatialCopperBalanceLayerRequest {
            safe_region: &panel,
            existing_copper: &existing,
            density_domain: &panel,
            target_density,
            stack_weight_mm2: 0.0,
        };
        let baseline = generate_dense_copper_balance(
            profile,
            DenseCopperBalanceRequest {
                safe_region: &panel,
                density_domain_area_mm2: panel.area(),
                existing_copper_area_mm2: 0.0,
                target_density,
                lattice_origin: Point::ZERO,
            },
        )
        .unwrap();
        let result = generate_spatial_dense_copper_balance(
            profile,
            SpatialCopperBalanceRequest {
                panel_region: &panel,
                lattice_origin: Point::ZERO,
                layers: &[layer],
            },
        )
        .unwrap()
        .layers
        .pop()
        .unwrap();

        assert_eq!(
            baseline.solution.mode,
            DenseCopperBalanceMode::Perforated {
                void_radius_mm: profile.min_void_radius_mm
            }
        );
        assert!(
            (result.solution.generated_area_mm2 - baseline.solution.generated_area_mm2).abs()
                <= AREA_SOLVE_TOLERANCE_MM2
        );
    }

    #[test]
    fn spatial_solver_preserves_area_and_radius_bounds_for_constant_inputs() {
        let panel = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(24.0, 16.0)),
            tol::REGION_MM,
        );
        let existing = ContourSet::empty(tol::REGION_MM);
        let layers = [SpatialCopperBalanceLayerRequest {
            safe_region: &panel,
            existing_copper: &existing,
            density_domain: &panel,
            target_density: 0.5,
            stack_weight_mm2: 0.0,
        }];
        let result = generate_spatial_dense_copper_balance(
            DenseCopperBalanceProfile::V1,
            SpatialCopperBalanceRequest {
                panel_region: &panel,
                lattice_origin: Point::ZERO,
                layers: &layers,
            },
        )
        .unwrap()
        .layers
        .pop()
        .unwrap();

        let (min, max) = result.full_void_radius_range_mm().unwrap();
        assert!(min + NUMERIC_EPSILON >= DenseCopperBalanceProfile::V1.min_void_radius_mm);
        assert!(max <= DenseCopperBalanceProfile::V1.max_void_radius_mm + NUMERIC_EPSILON);
        assert!((result.solution.achieved_density - 0.5).abs() <= 5e-3);
    }

    /// Unfillable panel material must not inflate the copper request.
    ///
    /// A denominator that spans the whole panel charges the layer for filling
    /// clearance it may never touch, and the solver can only spend that budget
    /// by over-filling the gutter it can reach — saturating to a solid pour
    /// whose local density far exceeds the footprint it is supposed to match.
    #[test]
    fn unfillable_clearance_stays_out_of_the_density_denominator() {
        let panel = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(40.0, 20.0)),
            tol::REGION_MM,
        );
        // An immutable footprint poured to 80%, a gutter beside it, and a wide
        // clearance ring in between that no generated copper may enter.
        let footprint = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(20.0, 20.0)),
            tol::REGION_MM,
        );
        let existing = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(20.0, 16.0)),
            tol::REGION_MM,
        );
        let safe = ContourSet::rectangle(
            BBox::new(Point::new(25.0, 0.0), Point::new(40.0, 20.0)),
            tol::REGION_MM,
        );
        let target_density = existing.area() / footprint.area();
        let density_domain = footprint.union(&safe);
        let layers = [SpatialCopperBalanceLayerRequest {
            safe_region: &safe,
            existing_copper: &existing,
            density_domain: &density_domain,
            target_density,
            stack_weight_mm2: 0.0,
        }];

        let result = generate_spatial_dense_copper_balance(
            DenseCopperBalanceProfile::V1,
            SpatialCopperBalanceRequest {
                panel_region: &panel,
                lattice_origin: Point::ZERO,
                layers: &layers,
            },
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
        assert!((result.solution.achieved_density - target_density).abs() <= 5e-3);
        let gutter_fill = result.solution.generated_area_mm2 / safe.area();
        assert!(
            (gutter_fill - target_density).abs() <= 5e-3,
            "gutter filled to {gutter_fill}, footprint sits at {target_density}"
        );
        // Charging the request against the whole panel would have demanded
        // more copper than the gutter can hold.
        assert!(target_density * panel.area() - existing.area() > safe.area());
    }

    #[test]
    fn spatial_solver_preserves_each_layers_safe_region() {
        let panel = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(40.0, 20.0)),
            tol::REGION_MM,
        );
        let left = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(20.0, 20.0)),
            tol::REGION_MM,
        );
        let right = ContourSet::rectangle(
            BBox::new(Point::new(20.0, 0.0), Point::new(40.0, 20.0)),
            tol::REGION_MM,
        );
        let existing = ContourSet::empty(tol::REGION_MM);
        // Nothing outside each layer's own half can hold copper, so each
        // layer's density domain is exactly its safe region.
        let layers = [
            SpatialCopperBalanceLayerRequest {
                safe_region: &left,
                existing_copper: &existing,
                density_domain: &left,
                target_density: 0.25,
                stack_weight_mm2: 1.0,
            },
            SpatialCopperBalanceLayerRequest {
                safe_region: &right,
                existing_copper: &existing,
                density_domain: &right,
                target_density: 0.25,
                stack_weight_mm2: -1.0,
            },
        ];

        let results = generate_spatial_dense_copper_balance(
            DenseCopperBalanceProfile::V1,
            SpatialCopperBalanceRequest {
                panel_region: &panel,
                lattice_origin: Point::ZERO,
                layers: &layers,
            },
        )
        .unwrap()
        .layers;

        assert_eq!(results.len(), 2);
        assert!(results[0].usable.difference(&left).is_empty());
        assert!(left.difference(&results[0].usable).is_empty());
        assert!(results[1].usable.difference(&right).is_empty());
        assert!(right.difference(&results[1].usable).is_empty());
        assert!(
            results[0]
                .full_voids
                .iter()
                .all(|void| void.center.x < 20.0)
        );
        assert!(
            results[1]
                .full_voids
                .iter()
                .all(|void| void.center.x >= 20.0)
        );
    }

    #[test]
    fn spatial_solver_opposes_a_fixed_copper_gradient_without_changing_total_area() {
        let panel = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(40.0, 20.0)),
            tol::REGION_MM,
        );
        let existing = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(20.0, 20.0)),
            tol::REGION_MM,
        );
        let safe = ContourSet::rectangle(
            BBox::new(Point::new(20.0, 0.0), Point::new(40.0, 20.0)),
            tol::REGION_MM,
        );
        // Fixed copper fills the left half and the right half is fillable, so
        // the density domain is the whole panel.
        let layer = SpatialCopperBalanceLayerRequest {
            safe_region: &safe,
            existing_copper: &existing,
            density_domain: &panel,
            target_density: 0.75,
            stack_weight_mm2: 0.0,
        };
        let baseline = generate_dense_copper_balance(
            DenseCopperBalanceProfile::V1,
            DenseCopperBalanceRequest {
                safe_region: &safe,
                density_domain_area_mm2: panel.area(),
                existing_copper_area_mm2: existing.area(),
                target_density: layer.target_density,
                lattice_origin: Point::ZERO,
            },
        )
        .unwrap();
        let result = generate_spatial_dense_copper_balance(
            DenseCopperBalanceProfile::V1,
            SpatialCopperBalanceRequest {
                panel_region: &panel,
                lattice_origin: Point::ZERO,
                layers: &[layer],
            },
        )
        .unwrap()
        .layers
        .pop()
        .unwrap();
        let mean_radius = |minimum_x: f64, maximum_x: f64| {
            let radii = result
                .full_voids
                .iter()
                .filter(|void| (minimum_x..maximum_x).contains(&void.center.x))
                .map(|void| void.radius_mm)
                .collect::<Vec<_>>();
            radii.iter().sum::<f64>() / radii.len() as f64
        };

        assert!(mean_radius(20.0, 25.0) > mean_radius(35.0, 40.0));
        let quantization_bound_mm2 = 0.01 * result.full_voids.len() as f64;
        assert!(
            (result.solution.generated_area_mm2 - baseline.solution.generated_area_mm2).abs()
                <= AREA_SOLVE_TOLERANCE_MM2 + quantization_bound_mm2
        );
    }

    /// A layer that takes no lattice still sits in the joint solve, and its
    /// band has to describe a sum that cannot move. Sizing that band from the
    /// sites it declined would place it above zero and invert it.
    #[test]
    fn a_layer_without_a_lattice_keeps_a_degenerate_band() {
        let panel = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(30.0, 20.0)),
            tol::REGION_MM,
        );
        let empty = ContourSet::empty(tol::REGION_MM);
        // A target its whole safe region cannot reach saturates this layer to
        // a solid pour, leaving it no radii while its sites still exist.
        let results = generate_spatial_dense_copper_balance(
            DenseCopperBalanceProfile {
                stack_flex_density: 0.01,
                ..DenseCopperBalanceProfile::V1
            },
            SpatialCopperBalanceRequest {
                panel_region: &panel,
                lattice_origin: Point::ZERO,
                layers: &[
                    SpatialCopperBalanceLayerRequest {
                        safe_region: &panel,
                        existing_copper: &empty,
                        density_domain: &panel,
                        target_density: 1.0,
                        stack_weight_mm2: 1.0,
                    },
                    SpatialCopperBalanceLayerRequest {
                        safe_region: &panel,
                        existing_copper: &empty,
                        density_domain: &panel,
                        target_density: 0.5,
                        stack_weight_mm2: -1.0,
                    },
                ],
            },
        )
        .unwrap()
        .layers;

        assert_eq!(results[0].solution.mode, DenseCopperBalanceMode::Solid);
        assert!(matches!(
            results[1].solution.mode,
            DenseCopperBalanceMode::Perforated { .. }
        ));
        // The saturated layer cannot trade, so the free layer has no partner
        // and stays on its own target.
        assert!(
            (results[1].solution.achieved_density - results[1].solution.target_density).abs()
                <= 5e-3,
            "{:?}",
            results[1].solution
        );
    }

    /// A layer whose radii all sit on one bound is where the band is most
    /// fragile: its accumulated sum is compared against a multiplied bound, so
    /// rounding can carry it across, and with the band shut there is no slack
    /// to absorb that. The band has to hold the sum it surrounds regardless.
    #[test]
    fn a_saturated_radius_field_still_yields_a_usable_band() {
        let panel = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(30.0, 20.0)),
            tol::REGION_MM,
        );
        let empty = ContourSet::empty(tol::REGION_MM);
        // Just under the sparsest fill the lattice can reach, so a perforated
        // plane is still the nearest projection but every radius pins to the
        // maximum; zero weights hold the band shut around that sum.
        let results = generate_spatial_dense_copper_balance(
            DenseCopperBalanceProfile::V1,
            SpatialCopperBalanceRequest {
                panel_region: &panel,
                lattice_origin: Point::ZERO,
                layers: &[SpatialCopperBalanceLayerRequest {
                    safe_region: &panel,
                    existing_copper: &empty,
                    density_domain: &panel,
                    target_density: 0.30,
                    stack_weight_mm2: 0.0,
                }],
            },
        )
        .unwrap();

        let (min, max) = results.layers[0]
            .full_void_radius_range_mm()
            .expect("a perforated layer has voids");
        assert!(
            (max - DenseCopperBalanceProfile::V1.max_void_radius_mm).abs() <= 1e-9,
            "expected saturated radii, got {min}..{max}"
        );
    }

    /// Without stack weights there is no moment to flatten, so the bands buy
    /// nothing and must stay shut. An open band would still let the local
    /// density error trade layers off their targets, which is exactly what a
    /// panel told its layers were balanced independently must not do.
    #[test]
    fn layers_hold_their_targets_when_the_stackup_weighs_nothing() {
        let panel = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(40.0, 20.0)),
            tol::REGION_MM,
        );
        // Fixed copper on one layer only, so the two layers see different
        // local fields and would trade if anything let them.
        let left_copper = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(20.0, 20.0)),
            tol::REGION_MM,
        );
        let safe = ContourSet::rectangle(
            BBox::new(Point::new(20.0, 0.0), Point::new(40.0, 20.0)),
            tol::REGION_MM,
        );
        let empty = ContourSet::empty(tol::REGION_MM);
        let results = generate_spatial_dense_copper_balance(
            DenseCopperBalanceProfile::V1,
            SpatialCopperBalanceRequest {
                panel_region: &panel,
                lattice_origin: Point::ZERO,
                layers: &[
                    SpatialCopperBalanceLayerRequest {
                        safe_region: &safe,
                        existing_copper: &left_copper,
                        density_domain: &panel,
                        target_density: 0.75,
                        stack_weight_mm2: 0.0,
                    },
                    SpatialCopperBalanceLayerRequest {
                        safe_region: &safe,
                        existing_copper: &empty,
                        density_domain: &safe,
                        target_density: 0.5,
                        stack_weight_mm2: 0.0,
                    },
                ],
            },
        )
        .unwrap();

        assert_eq!(results.moment_field, None);
        for result in &results.layers {
            assert!(
                (result.solution.achieved_density - result.solution.target_density).abs() <= 5e-3,
                "{:?}",
                result.solution
            );
        }
    }

    /// The field metric reports what the solve did to the moment, and reports
    /// nothing at all when the stackup gave it no weights to measure with —
    /// an absence of measurement rather than a flat panel.
    #[test]
    fn moment_field_is_measured_only_when_the_stackup_weighs_the_layers() {
        let panel = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(40.0, 20.0)),
            tol::REGION_MM,
        );
        let empty = ContourSet::empty(tol::REGION_MM);
        let solve = |stack_weight_mm2: f64| {
            generate_spatial_dense_copper_balance(
                DenseCopperBalanceProfile::V1,
                SpatialCopperBalanceRequest {
                    panel_region: &panel,
                    lattice_origin: Point::ZERO,
                    layers: &[
                        SpatialCopperBalanceLayerRequest {
                            safe_region: &panel,
                            existing_copper: &empty,
                            density_domain: &panel,
                            target_density: 0.70,
                            stack_weight_mm2,
                        },
                        SpatialCopperBalanceLayerRequest {
                            safe_region: &panel,
                            existing_copper: &empty,
                            density_domain: &panel,
                            target_density: 0.40,
                            stack_weight_mm2: -stack_weight_mm2,
                        },
                    ],
                },
            )
            .unwrap()
        };

        assert_eq!(solve(0.0).moment_field, None);

        let weighed = solve(1.0).moment_field.expect("weights were supplied");
        // These layers are asked for markedly different densities, so the
        // field starts well away from zero and the solve flattens it.
        assert!(weighed.initial_rms > 0.0, "{weighed:?}");
        assert!(weighed.achieved_rms < weighed.initial_rms, "{weighed:?}");
    }

    /// Two layers that both sit exactly on their targets can still carry a
    /// copper moment, because their targets differ and the boards were drawn
    /// that way. Balancing against the deviation from target cannot see it —
    /// the deviations are zero. Balancing against the copper itself does, and
    /// spends the band pulling the heavy layer down and the light one up.
    #[test]
    fn stack_moment_shrinks_when_the_boards_themselves_are_asymmetric() {
        let panel = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(40.0, 20.0)),
            tol::REGION_MM,
        );
        let empty = ContourSet::empty(tol::REGION_MM);
        // Mirrored layers, no fixed copper, but one is asked for markedly more
        // copper than the other: the imbalance is in the targets themselves.
        let (heavy, light) = (0.70, 0.40);
        let solve = |stack_moment_weight: f64| {
            generate_spatial_dense_copper_balance(
                DenseCopperBalanceProfile {
                    stack_moment_weight,
                    ..DenseCopperBalanceProfile::V1
                },
                SpatialCopperBalanceRequest {
                    panel_region: &panel,
                    lattice_origin: Point::ZERO,
                    layers: &[
                        SpatialCopperBalanceLayerRequest {
                            safe_region: &panel,
                            existing_copper: &empty,
                            density_domain: &panel,
                            target_density: heavy,
                            stack_weight_mm2: 1.0,
                        },
                        SpatialCopperBalanceLayerRequest {
                            safe_region: &panel,
                            existing_copper: &empty,
                            density_domain: &panel,
                            target_density: light,
                            stack_weight_mm2: -1.0,
                        },
                    ],
                },
            )
            .unwrap()
            .layers
        };
        // Mirrored weights normalize to +/- 0.5, so this is the moment.
        let moment = |results: &[DenseCopperBalanceResult]| {
            0.5 * results[0].solution.achieved_density - 0.5 * results[1].solution.achieved_density
        };

        let ignored = solve(0.0);
        let weighted = solve(DenseCopperBalanceProfile::V1.stack_moment_weight);

        // Ignored, the boards' own imbalance survives: the layers stay within
        // their bands of their targets, and nothing pulls them together.
        let untouched = 0.5 * (heavy - light);
        let band_reach = DenseCopperBalanceProfile::V1.stack_flex_density + 5e-3;
        assert!(
            (moment(&ignored) - untouched).abs() <= band_reach,
            "{} vs {untouched}",
            moment(&ignored)
        );
        // Weighted, the same panel carries measurably less.
        assert!(
            moment(&weighted) < moment(&ignored) - 5e-3,
            "{} vs {}",
            moment(&weighted),
            moment(&ignored)
        );
        // And it is paid for by a trade, not by removing copper from the panel.
        let deviations = weighted
            .iter()
            .map(|result| result.solution.achieved_density - result.solution.target_density)
            .collect::<Vec<_>>();
        assert!(deviations[0] < 0.0 && deviations[1] > 0.0, "{deviations:?}");
        assert!(
            deviations.iter().sum::<f64>().abs() <= 1e-2,
            "{deviations:?}"
        );
        for deviation in &deviations {
            assert!(
                deviation.abs() <= DenseCopperBalanceProfile::V1.stack_flex_density + 5e-3,
                "{deviations:?}"
            );
        }
    }

    /// Fixed copper on one side of one layer tilts the stack in a way no
    /// redistribution can answer: it is real copper sitting off the mid-plane,
    /// and only the layer opposite it can counterweight. The band lets the
    /// tilted layer shed density and its mirror take the same amount on,
    /// bounded on both sides, and the trade nets to zero so the stack gains no
    /// copper overall.
    #[test]
    fn stack_flex_trades_density_between_mirrored_layers() {
        let panel = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(40.0, 20.0)),
            tol::REGION_MM,
        );
        let left_copper = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(20.0, 20.0)),
            tol::REGION_MM,
        );
        let safe = ContourSet::rectangle(
            BBox::new(Point::new(20.0, 0.0), Point::new(40.0, 20.0)),
            tol::REGION_MM,
        );
        let empty = ContourSet::empty(tol::REGION_MM);
        let solve = |stack_flex_density: f64| {
            generate_spatial_dense_copper_balance(
                DenseCopperBalanceProfile {
                    stack_flex_density,
                    ..DenseCopperBalanceProfile::V1
                },
                SpatialCopperBalanceRequest {
                    panel_region: &panel,
                    lattice_origin: Point::ZERO,
                    layers: &[
                        SpatialCopperBalanceLayerRequest {
                            safe_region: &safe,
                            existing_copper: &left_copper,
                            density_domain: &panel,
                            target_density: 0.75,
                            stack_weight_mm2: 1.0,
                        },
                        SpatialCopperBalanceLayerRequest {
                            safe_region: &safe,
                            existing_copper: &empty,
                            density_domain: &safe,
                            target_density: 0.5,
                            stack_weight_mm2: -1.0,
                        },
                    ],
                },
            )
            .unwrap()
            .layers
        };
        // Radius quantization alone moves the achieved density this far.
        let quantization = 5e-3;
        let flex = 0.02;

        let pinned = solve(0.0);
        let flexed = solve(flex);

        // Pinned, both layers are held on their own targets.
        for result in &pinned {
            assert!(
                (result.solution.achieved_density - result.solution.target_density).abs()
                    <= quantization,
                "{:?}",
                result.solution
            );
        }

        let deviations = flexed
            .iter()
            .map(|result| result.solution.achieved_density - result.solution.target_density)
            .collect::<Vec<_>>();
        // The layer holding the fixed copper is the one tilting the stack, so
        // it sheds density and its mirror takes the same amount on.
        assert!(deviations[0] < -quantization, "{deviations:?}");
        assert!(deviations[1] > quantization, "{deviations:?}");
        // Neither leaves its band, and the stack gains nothing overall.
        for deviation in &deviations {
            assert!(deviation.abs() <= flex + quantization, "{deviations:?}");
        }
        assert!(
            deviations.iter().sum::<f64>().abs() <= 2.0 * quantization,
            "{deviations:?}"
        );
    }

    #[test]
    fn spatial_solver_uses_signed_stack_weights() {
        let panel = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(40.0, 20.0)),
            tol::REGION_MM,
        );
        let safe = ContourSet::rectangle(
            BBox::new(Point::new(20.0, 0.0), Point::new(40.0, 20.0)),
            tol::REGION_MM,
        );
        let top_copper = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(20.0, 20.0)),
            tol::REGION_MM,
        );
        let bottom_copper = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(20.0, 10.0)),
            tol::REGION_MM,
        );
        // The layers hold different fixed copper, so their density domains
        // differ: the bottom layer's upper-left quadrant is bare and
        // unfillable, and stays out of its denominator.
        let top_domain = top_copper.union(&safe);
        let bottom_domain = bottom_copper.union(&safe);
        // This is the redistribution the stack weights drive at a fixed copper
        // area, so the band stays shut: with it open the two solves are free
        // to trade area as well, and the area assertion below would be
        // asserting the opposite of what the band exists to allow.
        let solve = |stack_weight_mm2: f64| {
            generate_spatial_dense_copper_balance(
                DenseCopperBalanceProfile {
                    stack_flex_density: 0.0,
                    ..DenseCopperBalanceProfile::V1
                },
                SpatialCopperBalanceRequest {
                    panel_region: &panel,
                    lattice_origin: Point::ZERO,
                    layers: &[
                        SpatialCopperBalanceLayerRequest {
                            safe_region: &safe,
                            existing_copper: &top_copper,
                            density_domain: &top_domain,
                            target_density: 0.75,
                            stack_weight_mm2,
                        },
                        SpatialCopperBalanceLayerRequest {
                            safe_region: &safe,
                            existing_copper: &bottom_copper,
                            density_domain: &bottom_domain,
                            target_density: 0.50,
                            stack_weight_mm2: -stack_weight_mm2,
                        },
                    ],
                },
            )
            .unwrap()
            .layers
        };
        let independent = solve(0.0);
        let stack_aware = solve(1.0);
        let radius_difference = independent
            .iter()
            .zip(&stack_aware)
            .flat_map(|(independent, stack_aware)| {
                independent
                    .full_voids
                    .iter()
                    .zip(&stack_aware.full_voids)
                    .map(|(independent, stack_aware)| {
                        (independent.radius_mm - stack_aware.radius_mm).abs()
                    })
            })
            .sum::<f64>();

        assert!(radius_difference > 0.01);
        assert!(independent.iter().zip(&stack_aware).all(|(left, right)| {
            let quantization_bound_mm2 = 0.01 * left.full_voids.len() as f64;
            (left.solution.generated_area_mm2 - right.solution.generated_area_mm2).abs()
                <= AREA_SOLVE_TOLERANCE_MM2 + quantization_bound_mm2
        }));
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
        let safe_region = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(20.0, 10.0)),
            tol::REGION_MM,
        );
        let existing_copper_area_mm2 = 100.0;
        let exact_domain_area_mm2 = existing_copper_area_mm2 + safe_region.area();
        let result = generate_dense_copper_balance(
            DenseCopperBalanceProfile::V1,
            DenseCopperBalanceRequest {
                safe_region: &safe_region,
                density_domain_area_mm2: exact_domain_area_mm2
                    - CONTAINMENT_AREA_TOLERANCE_MM2 / 2.0,
                existing_copper_area_mm2,
                target_density: 0.9,
                lattice_origin: Point::ZERO,
            },
        );

        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[test]
    fn geometric_projection_never_worsens_target_sweep() {
        let safe_region = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(8.0, 5.0)),
            tol::REGION_MM,
        );
        for target_step in 0..=10 {
            let target_density = target_step as f64 / 10.0;
            let result = generate_dense_copper_balance(
                DenseCopperBalanceProfile::V1,
                DenseCopperBalanceRequest {
                    safe_region: &safe_region,
                    density_domain_area_mm2: 100.0,
                    existing_copper_area_mm2: 20.0,
                    target_density,
                    lattice_origin: Point::new(0.0, 0.0),
                },
            )
            .unwrap();
            assert!(
                result.solution.residual_error
                    <= (result.solution.initial_density - target_density).abs() + NUMERIC_EPSILON
            );
        }
    }
}
