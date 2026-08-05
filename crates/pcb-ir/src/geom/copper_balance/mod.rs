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
    LatticeDensityKernel, density_evaluation_points, lattice_cell_coverage,
    normalized_stack_weights, project_box_sum, spatial_result_from_squared_radii,
};

const NUMERIC_EPSILON: f64 = 1e-9;
const RADIUS_SOLVE_TOLERANCE_MM: f64 = 1e-4;
const AREA_SOLVE_TOLERANCE_MM2: f64 = 1e-3;
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
    /// Safe, initially empty subset of the retained board-array area.
    pub safe_region: &'a ContourSet,
    /// Entire retained board-array area used as the density denominator.
    pub retained_area_mm2: f64,
    /// Fixed copper anywhere in that retained area on this layer.
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
    pub target_density: f64,
    /// Signed first-moment weight `z * thickness` from the physical stackup.
    pub stack_weight_mm2: f64,
}

/// Geometry shared by all layers in a joint spatial copper-balance solve.
#[derive(Debug, Clone, Copy)]
pub struct SpatialCopperBalanceRequest<'a> {
    /// Canonical retained panel geometry and density denominator.
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
        request.target_density * request.retained_area_mm2 - request.existing_copper_area_mm2;
    let initial_density = request.existing_copper_area_mm2 / request.retained_area_mm2;
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
        (request.existing_copper_area_mm2 + best.area_mm2) / request.retained_area_mm2;
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
) -> Result<Vec<DenseCopperBalanceResult>, DenseCopperBalanceError> {
    profile.validate()?;
    validate_spatial_request(request)?;
    let retained_area_mm2 = request.panel_region.area();

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
            .map(|(layer, region_index)| {
                let voidable = &region_voidable[*region_index];
                let lattice = &region_lattices[*region_index];
                scope.spawn(move || {
                    generate_dense_copper_balance_with_lattice(
                        profile,
                        DenseCopperBalanceRequest {
                            safe_region: layer.safe_region,
                            retained_area_mm2,
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
        return Ok(uniform);
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

    let lower = active_sites
        .iter()
        .map(|active| vec![profile.min_void_radius_mm.powi(2); active.len()])
        .collect::<Vec<_>>();
    let upper = active_sites
        .iter()
        .map(|active| vec![profile.max_void_radius_mm.powi(2); active.len()])
        .collect::<Vec<_>>();
    // The uniform solve already selected each layer's full-void area; its
    // radius is within the profile bounds, so the sum is directly feasible.
    let squared_radius_sums = uniform
        .iter()
        .enumerate()
        .map(|(layer_index, result)| match result.solution.mode {
            DenseCopperBalanceMode::Perforated { void_radius_mm } => {
                active_sites[layer_index].len() as f64 * void_radius_mm.powi(2)
            }
            DenseCopperBalanceMode::None | DenseCopperBalanceMode::Solid => 0.0,
        })
        .collect::<Vec<_>>();
    let mut squared_radii = uniform
        .iter()
        .enumerate()
        .map(|(layer_index, result)| match result.solution.mode {
            DenseCopperBalanceMode::Perforated { void_radius_mm } => project_box_sum(
                &vec![void_radius_mm.powi(2); active_sites[layer_index].len()],
                &lower[layer_index],
                &upper[layer_index],
                squared_radius_sums[layer_index],
            ),
            DenseCopperBalanceMode::None | DenseCopperBalanceMode::Solid => Vec::new(),
        })
        .collect::<Vec<Vec<f64>>>();
    let cell_area_mm2 = SQRT_3 * profile.pitch_mm.powi(2) / 2.0;
    let void_fraction_per_radius_squared = ROUNDED_HEXAGON_AREA_FACTOR / cell_area_mm2;
    let normalized_stack_weights = normalized_stack_weights(request.layers);
    let step = 0.25 / void_fraction_per_radius_squared.powi(2);

    for _ in 0..SPATIAL_SOLVE_ITERATIONS {
        let error = request
            .layers
            .iter()
            .enumerate()
            .map(|(layer_index, layer)| {
                let void_density = match uniform[layer_index].solution.mode {
                    DenseCopperBalanceMode::Perforated { .. } => {
                        let mut void_fraction = vec![0.0; panel_samples.len()];
                        for (sample_index, radius_squared) in active_sites[layer_index]
                            .iter()
                            .zip(&squared_radii[layer_index])
                        {
                            void_fraction[*sample_index] =
                                void_fraction_per_radius_squared * radius_squared;
                        }
                        density_kernel.smooth(&void_fraction)
                    }
                    DenseCopperBalanceMode::None | DenseCopperBalanceMode::Solid => {
                        vec![0.0; evaluation_points.len()]
                    }
                };
                fixed_density[layer_index]
                    .iter()
                    .enumerate()
                    .map(|(site_index, fixed)| {
                        let available = &region_available_density[layer_regions[layer_index]];
                        let generated_density = match uniform[layer_index].solution.mode {
                            DenseCopperBalanceMode::None => 0.0,
                            DenseCopperBalanceMode::Solid => available[site_index],
                            DenseCopperBalanceMode::Perforated { .. } => {
                                available[site_index]
                                    - partial_void_density[layer_index][site_index]
                                    - void_density[site_index]
                            }
                        };
                        fixed + generated_density - layer.target_density
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let stack_error = (0..evaluation_points.len())
            .map(|site_index| {
                normalized_stack_weights
                    .iter()
                    .enumerate()
                    .map(|(layer_index, weight)| weight * error[layer_index][site_index])
                    .sum::<f64>()
            })
            .collect::<Vec<_>>();

        for layer_index in 0..request.layers.len() {
            if squared_radii[layer_index].is_empty() {
                continue;
            }
            let residual = error[layer_index]
                .iter()
                .zip(&stack_error)
                .map(|(local, stack)| local + normalized_stack_weights[layer_index] * stack)
                .collect::<Vec<_>>();
            let influence = density_kernel.smooth_adjoint(&residual);
            let proposed = squared_radii[layer_index]
                .iter()
                .enumerate()
                .map(|(local_index, radius_squared)| {
                    radius_squared
                        + step
                            * void_fraction_per_radius_squared
                            * influence[active_sites[layer_index][local_index]]
                })
                .collect::<Vec<_>>();
            squared_radii[layer_index] = project_box_sum(
                &proposed,
                &lower[layer_index],
                &upper[layer_index],
                squared_radius_sums[layer_index],
            );
        }
    }

    Ok(uniform
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
                retained_area_mm2,
                profile,
            )
        })
        .collect())
}

fn validate_spatial_request(
    request: SpatialCopperBalanceRequest<'_>,
) -> Result<(), DenseCopperBalanceError> {
    if !request.panel_region.bbox.is_valid() || request.panel_region.is_empty() {
        return Err(DenseCopperBalanceError::InvalidInput(
            "panel region must be non-empty and have valid bounds".to_string(),
        ));
    }
    let retained_area_mm2 = request.panel_region.area();
    for layer in request.layers {
        if !layer.stack_weight_mm2.is_finite() {
            return Err(DenseCopperBalanceError::InvalidInput(
                "stack weights must be finite".to_string(),
            ));
        }
        if !layer
            .safe_region
            .difference(request.panel_region)
            .is_empty()
        {
            return Err(DenseCopperBalanceError::InvalidInput(
                "safe region must be contained by the panel region".to_string(),
            ));
        }
        if !layer
            .existing_copper
            .difference(request.panel_region)
            .is_empty()
        {
            return Err(DenseCopperBalanceError::InvalidInput(
                "existing copper must be contained by the panel region".to_string(),
            ));
        }
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
            retained_area_mm2,
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
    if !request.retained_area_mm2.is_finite() || request.retained_area_mm2 <= 0.0 {
        return Err(DenseCopperBalanceError::InvalidInput(
            "retained area must be finite and greater than zero".to_string(),
        ));
    }
    if !request.existing_copper_area_mm2.is_finite()
        || request.existing_copper_area_mm2 < 0.0
        || request.existing_copper_area_mm2 > request.retained_area_mm2 + NUMERIC_EPSILON
    {
        return Err(DenseCopperBalanceError::InvalidInput(
            "existing copper area must be between zero and retained area".to_string(),
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
        || usable_area_mm2 > request.retained_area_mm2 + NUMERIC_EPSILON
    {
        return Err(DenseCopperBalanceError::InvalidInput(
            "usable area must be between zero and retained area".to_string(),
        ));
    }
    if request.existing_copper_area_mm2 + usable_area_mm2
        > request.retained_area_mm2 + NUMERIC_EPSILON
    {
        return Err(DenseCopperBalanceError::InvalidInput(
            "existing copper and usable areas together exceed the retained area".to_string(),
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
                retained_area_mm2: 200.0,
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
                retained_area_mm2: 96.0,
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
                retained_area_mm2: 16.0,
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

        assert!(result.is_empty());
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
    fn spatial_solver_rejects_geometry_outside_the_panel() {
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
        let solve = |safe_region, existing_copper| {
            let layers = [SpatialCopperBalanceLayerRequest {
                safe_region,
                existing_copper,
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
            solve(&outside, &empty).unwrap_err(),
            DenseCopperBalanceError::InvalidInput(
                "safe region must be contained by the panel region".to_string()
            )
        );
        assert_eq!(
            solve(&inside, &outside).unwrap_err(),
            DenseCopperBalanceError::InvalidInput(
                "existing copper must be contained by the panel region".to_string()
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
            target_density,
            stack_weight_mm2: 0.0,
        };
        let baseline = generate_dense_copper_balance(
            profile,
            DenseCopperBalanceRequest {
                safe_region: &panel,
                retained_area_mm2: panel.area(),
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
        .pop()
        .unwrap();

        let (min, max) = result.full_void_radius_range_mm().unwrap();
        assert!(min + NUMERIC_EPSILON >= DenseCopperBalanceProfile::V1.min_void_radius_mm);
        assert!(max <= DenseCopperBalanceProfile::V1.max_void_radius_mm + NUMERIC_EPSILON);
        assert!((result.solution.achieved_density - 0.5).abs() <= 5e-3);
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
        let layers = [
            SpatialCopperBalanceLayerRequest {
                safe_region: &left,
                existing_copper: &existing,
                target_density: 0.25,
                stack_weight_mm2: 1.0,
            },
            SpatialCopperBalanceLayerRequest {
                safe_region: &right,
                existing_copper: &existing,
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
        .unwrap();

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
        let layer = SpatialCopperBalanceLayerRequest {
            safe_region: &safe,
            existing_copper: &existing,
            target_density: 0.75,
            stack_weight_mm2: 0.0,
        };
        let baseline = generate_dense_copper_balance(
            DenseCopperBalanceProfile::V1,
            DenseCopperBalanceRequest {
                safe_region: &safe,
                retained_area_mm2: panel.area(),
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
        let solve = |stack_weight_mm2: f64| {
            generate_spatial_dense_copper_balance(
                DenseCopperBalanceProfile::V1,
                SpatialCopperBalanceRequest {
                    panel_region: &panel,
                    lattice_origin: Point::ZERO,
                    layers: &[
                        SpatialCopperBalanceLayerRequest {
                            safe_region: &safe,
                            existing_copper: &top_copper,
                            target_density: 0.75,
                            stack_weight_mm2,
                        },
                        SpatialCopperBalanceLayerRequest {
                            safe_region: &safe,
                            existing_copper: &bottom_copper,
                            target_density: 0.50,
                            stack_weight_mm2: -stack_weight_mm2,
                        },
                    ],
                },
            )
            .unwrap()
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
                    retained_area_mm2: 100.0,
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
