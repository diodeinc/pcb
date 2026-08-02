//! Dense copper balancing over an explicitly supplied safe region.
//!
//! This module intentionally does not discover safe panel regions or inspect
//! PCB layer semantics. Callers supply the addable region and the measured
//! per-layer areas; the solver chooses the closest manufacturable copper area
//! and generates a deterministic perforated plane.

use std::{collections::HashMap, f64::consts::PI};

use crate::geom::path::transform_cmds;
use crate::geom::{Affine2, BBox, ContourBuf, ContourSet, FillRule, PathCmd, Point, tol};

const NUMERIC_EPSILON: f64 = 1e-9;
const RADIUS_SOLVE_TOLERANCE_MM: f64 = 1e-4;
const AREA_SOLVE_TOLERANCE_MM2: f64 = 1e-3;
const SPATIAL_SOLVE_ITERATIONS: usize = 64;
const DENSITY_KERNEL_TRUNCATION: f64 = 3.0;
const SQRT_3: f64 = 1.732_050_807_568_877_2;
const HEXAGON_CORNER_RADIUS_RATIO: f64 = 0.15;
// A sharp regular hexagon has area 3√3 R² / 2. Rounding each 120° corner
// inward by fillet radius kR removes (2√3 - π)k²R² across all six corners.
const ROUNDED_HEXAGON_AREA_FACTOR: f64 = 3.0 * SQRT_3 / 2.0
    - (2.0 * SQRT_3 - PI) * HEXAGON_CORNER_RADIUS_RATIO * HEXAGON_CORNER_RADIUS_RATIO;

/// Fixed geometry constraints for a dense perforated copper plane.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DenseCopperBalanceProfile {
    pub pitch_mm: f64,
    pub min_void_radius_mm: f64,
    pub max_void_radius_mm: f64,
    pub min_copper_web_mm: f64,
    pub boundary_web_mm: f64,
    pub density_sigma_mm: f64,
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

impl Default for DenseCopperBalanceProfile {
    fn default() -> Self {
        Self::V1
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

/// Geometry inputs once a safe region is available.
#[derive(Debug, Clone, Copy)]
pub struct DenseCopperBalanceRequest<'a> {
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
    pub existing_copper: &'a ContourSet,
    pub existing_copper_area_mm2: f64,
    pub target_density: f64,
    /// Signed first-moment weight `z * thickness` from the physical stackup.
    pub stack_weight_mm2: f64,
}

/// Geometry shared by all layers in a joint spatial copper-balance solve.
#[derive(Debug, Clone, Copy)]
pub struct SpatialCopperBalanceRequest<'a> {
    pub panel_region: &'a ContourSet,
    pub safe_region: &'a ContourSet,
    pub retained_area_mm2: f64,
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
pub fn generate_dense_copper_balance(
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
/// The solver uses squared void radius as its variable. It minimizes local
/// density error plus the signed through-stack first moment, then projects
/// every layer back onto its original total void area and radius bounds.
pub fn generate_spatial_dense_copper_balance(
    profile: DenseCopperBalanceProfile,
    request: SpatialCopperBalanceRequest<'_>,
) -> Result<Vec<DenseCopperBalanceResult>, DenseCopperBalanceError> {
    profile.validate()?;
    validate_spatial_request(request)?;

    let voidable = request.safe_region.disk_erode(profile.boundary_web_mm);
    let lattice = LatticeCandidates::new(&voidable, request.lattice_origin, profile);
    let uniform = request
        .layers
        .iter()
        .map(|layer| {
            generate_dense_copper_balance_with_lattice(
                profile,
                DenseCopperBalanceRequest {
                    safe_region: request.safe_region,
                    retained_area_mm2: request.retained_area_mm2,
                    existing_copper_area_mm2: layer.existing_copper_area_mm2,
                    target_density: layer.target_density,
                    lattice_origin: request.lattice_origin,
                },
                request.safe_region.clone(),
                &voidable,
                &lattice,
            )
        })
        .collect::<Vec<_>>();

    if lattice.is_empty() {
        return Ok(uniform);
    }

    let centers = &lattice.full_centers;
    if centers.is_empty() {
        return Ok(uniform);
    }
    let panel_samples =
        hex_aligned_lattice_centers(request.panel_region.bbox, request.lattice_origin, profile)
            .into_iter()
            .filter(|point| request.panel_region.contains_point(*point))
            .collect::<Vec<_>>();
    let density_kernel =
        LatticeDensityKernel::new(&panel_samples, centers, request.lattice_origin, profile);
    let fixed_density = request
        .layers
        .iter()
        .map(|layer| {
            density_kernel.smooth(&scanline_indicator(&panel_samples, layer.existing_copper))
        })
        .collect::<Vec<_>>();
    let available_density =
        density_kernel.smooth(&scanline_indicator(&panel_samples, request.safe_region));

    let lower = vec![profile.min_void_radius_mm.powi(2); centers.len()];
    let upper = vec![profile.max_void_radius_mm.powi(2); centers.len()];
    let squared_radius_sums = uniform
        .iter()
        .map(|result| match result.solution.mode {
            DenseCopperBalanceMode::Perforated { .. } => {
                let target_full_void_area_mm2 = result.usable.area()
                    - result.solution.generated_area_mm2
                    - result.partial_voids.area();
                (target_full_void_area_mm2 / ROUNDED_HEXAGON_AREA_FACTOR)
                    .clamp(lower.iter().sum(), upper.iter().sum())
            }
            DenseCopperBalanceMode::None | DenseCopperBalanceMode::Solid => 0.0,
        })
        .collect::<Vec<_>>();
    let mut squared_radii = uniform
        .iter()
        .enumerate()
        .map(|(layer_index, result)| match result.solution.mode {
            DenseCopperBalanceMode::Perforated { void_radius_mm } => project_box_sum(
                &vec![void_radius_mm.powi(2); centers.len()],
                &lower,
                &upper,
                squared_radius_sums[layer_index],
            ),
            DenseCopperBalanceMode::None | DenseCopperBalanceMode::Solid => Vec::new(),
        })
        .collect::<Vec<Vec<f64>>>();
    let cell_area_mm2 = SQRT_3 * profile.pitch_mm.powi(2) / 2.0;
    let void_fraction_per_radius_squared = ROUNDED_HEXAGON_AREA_FACTOR / cell_area_mm2;
    let normalized_stack_weights = normalized_stack_weights(request.layers);
    let step = 0.5 / void_fraction_per_radius_squared;

    for _ in 0..SPATIAL_SOLVE_ITERATIONS {
        let error = request
            .layers
            .iter()
            .enumerate()
            .map(|(layer_index, layer)| {
                centers
                    .iter()
                    .enumerate()
                    .map(|(site_index, _)| {
                        let generated_density = match uniform[layer_index].solution.mode {
                            DenseCopperBalanceMode::None => 0.0,
                            DenseCopperBalanceMode::Solid => available_density[site_index],
                            DenseCopperBalanceMode::Perforated { .. } => {
                                available_density[site_index]
                                    * (1.0
                                        - void_fraction_per_radius_squared
                                            * squared_radii[layer_index][site_index])
                                        .clamp(0.0, 1.0)
                            }
                        };
                        fixed_density[layer_index][site_index] + generated_density
                            - layer.target_density
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let stack_error = (0..centers.len())
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
            let proposed = squared_radii[layer_index]
                .iter()
                .enumerate()
                .map(|(site_index, radius_squared)| {
                    radius_squared
                        + step
                            * available_density[site_index]
                            * (error[layer_index][site_index]
                                + normalized_stack_weights[layer_index] * stack_error[site_index])
                })
                .collect::<Vec<_>>();
            squared_radii[layer_index] =
                project_box_sum(&proposed, &lower, &upper, squared_radius_sums[layer_index]);
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
                centers,
                &squared_radii[layer_index],
                baseline,
                request.layers[layer_index],
                request.retained_area_mm2,
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
    for layer in request.layers {
        if !layer.stack_weight_mm2.is_finite() {
            return Err(DenseCopperBalanceError::InvalidInput(
                "stack weights must be finite".to_string(),
            ));
        }
        validate_request(DenseCopperBalanceRequest {
            safe_region: request.safe_region,
            retained_area_mm2: request.retained_area_mm2,
            existing_copper_area_mm2: layer.existing_copper_area_mm2,
            target_density: layer.target_density,
            lattice_origin: request.lattice_origin,
        })?;
    }
    Ok(())
}

fn normalized_stack_weights(layers: &[SpatialCopperBalanceLayerRequest<'_>]) -> Vec<f64> {
    let scale = layers
        .iter()
        .map(|layer| layer.stack_weight_mm2.abs())
        .sum::<f64>();
    layers
        .iter()
        .map(|layer| {
            if scale > NUMERIC_EPSILON {
                layer.stack_weight_mm2 / scale
            } else {
                0.0
            }
        })
        .collect()
}

struct LatticeDensityKernel {
    samples: HashMap<(i64, i64), usize>,
    evaluation_sites: Vec<(i64, i64)>,
    offsets: [Vec<(i64, i64, f64)>; 2],
}

impl LatticeDensityKernel {
    fn new(
        samples: &[Point],
        evaluation_points: &[Point],
        origin: Point,
        profile: DenseCopperBalanceProfile,
    ) -> Self {
        let index = |point| lattice_index(point, origin, profile);
        let samples = samples
            .iter()
            .enumerate()
            .map(|(sample_index, point)| (index(*point), sample_index))
            .collect();
        let evaluation_sites = evaluation_points
            .iter()
            .map(|point| index(*point))
            .collect();
        let support_mm = DENSITY_KERNEL_TRUNCATION * profile.density_sigma_mm;
        let inverse_two_sigma_squared = 0.5 / profile.density_sigma_mm.powi(2);
        let column_span = (support_mm / profile.lattice_column_pitch_mm()).ceil() as i64;
        let row_span = (support_mm / profile.pitch_mm).ceil() as i64 + 1;
        let offsets = [0_i64, 1_i64].map(|parity| {
            (-column_span..=column_span)
                .flat_map(|column| {
                    (-row_span..=row_span).filter_map(move |row| {
                        let neighbor_parity = (parity + column).rem_euclid(2);
                        let dx = column as f64 * profile.lattice_column_pitch_mm();
                        let dy = (row as f64 + (neighbor_parity - parity) as f64 / 2.0)
                            * profile.pitch_mm;
                        let distance_squared = dx * dx + dy * dy;
                        (distance_squared <= support_mm.powi(2)).then(|| {
                            (
                                column,
                                row,
                                (-distance_squared * inverse_two_sigma_squared).exp(),
                            )
                        })
                    })
                })
                .collect()
        });
        Self {
            samples,
            evaluation_sites,
            offsets,
        }
    }

    fn smooth(&self, values: &[f64]) -> Vec<f64> {
        self.evaluation_sites
            .iter()
            .map(|&(column, row)| {
                let (weighted_sum, weight_sum) = self.offsets[column.rem_euclid(2) as usize]
                    .iter()
                    .filter_map(|&(column_offset, row_offset, weight)| {
                        let sample_index = self
                            .samples
                            .get(&(column + column_offset, row + row_offset))?;
                        Some((weight * values[*sample_index], weight))
                    })
                    .fold((0.0, 0.0), |(sum, weights), (value, weight)| {
                        (sum + value, weights + weight)
                    });
                if weight_sum > 0.0 {
                    weighted_sum / weight_sum
                } else {
                    0.0
                }
            })
            .collect()
    }
}

fn lattice_index(point: Point, origin: Point, profile: DenseCopperBalanceProfile) -> (i64, i64) {
    let column = ((point.x - origin.x) / profile.lattice_column_pitch_mm()).round() as i64;
    let column_offset = column.rem_euclid(2) as f64 * profile.pitch_mm / 2.0;
    let row = ((point.y - origin.y - column_offset) / profile.pitch_mm).round() as i64;
    (column, row)
}

fn scanline_indicator(points: &[Point], region: &ContourSet) -> Vec<f64> {
    let mut result = vec![0.0; points.len()];
    let mut by_y = (0..points.len()).collect::<Vec<_>>();
    by_y.sort_by(|left, right| {
        points[*left]
            .y
            .total_cmp(&points[*right].y)
            .then_with(|| points[*left].x.total_cmp(&points[*right].x))
    });
    let edges = region
        .rings
        .iter()
        .flat_map(|ring| {
            ring.iter()
                .copied()
                .zip(ring.iter().copied().cycle().skip(1))
                .take(ring.len())
        })
        .collect::<Vec<_>>();

    let mut first = 0;
    while first < by_y.len() {
        let y = points[by_y[first]].y;
        let mut last = first + 1;
        while last < by_y.len() && (points[by_y[last]].y - y).abs() <= NUMERIC_EPSILON {
            last += 1;
        }
        let mut crossings = edges
            .iter()
            .filter_map(|([x0, y0], [x1, y1])| {
                let direction = if *y0 <= y && y < *y1 {
                    1
                } else if *y1 <= y && y < *y0 {
                    -1
                } else {
                    return None;
                };
                let x = x0 + (y - y0) * (x1 - x0) / (y1 - y0);
                Some((x, direction))
            })
            .collect::<Vec<_>>();
        crossings.sort_by(|left, right| left.0.total_cmp(&right.0));
        let mut crossing_index = 0;
        let mut winding = 0;
        for &point_index in &by_y[first..last] {
            while crossing_index < crossings.len()
                && crossings[crossing_index].0 <= points[point_index].x
            {
                winding += crossings[crossing_index].1;
                crossing_index += 1;
            }
            result[point_index] = (winding != 0) as u8 as f64;
        }
        first = last;
    }
    result
}

fn project_box_sum(values: &[f64], lower: &[f64], upper: &[f64], target: f64) -> Vec<f64> {
    debug_assert_eq!(values.len(), lower.len());
    debug_assert_eq!(values.len(), upper.len());
    let mut low_shift = values
        .iter()
        .zip(upper)
        .map(|(value, bound)| value - bound)
        .min_by(f64::total_cmp)
        .unwrap_or(0.0);
    let mut high_shift = values
        .iter()
        .zip(lower)
        .map(|(value, bound)| value - bound)
        .max_by(f64::total_cmp)
        .unwrap_or(0.0);
    for _ in 0..64 {
        let shift = (low_shift + high_shift) / 2.0;
        let sum = values
            .iter()
            .zip(lower)
            .zip(upper)
            .map(|((value, lower), upper)| (value - shift).clamp(*lower, *upper))
            .sum::<f64>();
        if sum > target {
            low_shift = shift;
        } else {
            high_shift = shift;
        }
    }
    let shift = (low_shift + high_shift) / 2.0;
    values
        .iter()
        .zip(lower)
        .zip(upper)
        .map(|((value, lower), upper)| (value - shift).clamp(*lower, *upper))
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

    let mut low_area = low_void_area;
    let mut high_area = high_void_area;
    while high_radius - low_radius > RADIUS_SOLVE_TOLERANCE_MM
        && best.error_mm2 > AREA_SOLVE_TOLERANCE_MM2
    {
        // Interior hex area is approximately linear in r² after flattening.
        // Interpolate there, with a midpoint fallback that keeps convergence
        // bracketed when clipped edge area dominates.
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
            "existing copper and usable regions must be disjoint within retained area".to_string(),
        ));
    }
    Ok(())
}

#[derive(Debug, Default)]
struct LatticeCandidates {
    full_centers: Vec<Point>,
    edge_candidates: Vec<(Point, f64)>,
}

impl LatticeCandidates {
    fn new(voidable: &ContourSet, origin: Point, profile: DenseCopperBalanceProfile) -> Self {
        if voidable.is_empty() {
            return Self::default();
        }

        let candidate_region = voidable.disk_dilate(profile.max_void_radius_mm);
        let centers = hex_aligned_lattice_centers(candidate_region.bbox, origin, profile)
            .into_iter()
            .filter(|center| candidate_region.contains_point(*center))
            .collect::<Vec<_>>();
        let fully_contained = fully_contained_hexagons(voidable, &centers, profile);
        let mut full_centers = Vec::new();
        let mut edge_centers = Vec::new();
        for (center, full) in centers.into_iter().zip(fully_contained) {
            if full {
                full_centers.push(center);
            } else {
                edge_centers.push(center);
            }
        }
        let edge_candidates = minimum_partial_candidates(voidable, &edge_centers, profile);
        Self {
            full_centers,
            edge_candidates,
        }
    }

    fn is_empty(&self) -> bool {
        self.full_centers.is_empty() && self.edge_candidates.is_empty()
    }

    fn full_void_area(&self, radius: f64, tolerance: f64) -> f64 {
        let template = hexagon_set_with_radii(&[(Point::ZERO, radius)], tolerance);
        self.full_centers.len() as f64 * template.area()
    }

    fn partial_voids(
        &self,
        voidable: &ContourSet,
        radius: f64,
        profile: DenseCopperBalanceProfile,
    ) -> ContourSet {
        let candidates = self
            .edge_candidates
            .iter()
            .map(|(center, activation_radius)| (*center, radius.max(*activation_radius)))
            .collect::<Vec<_>>();
        clipped_voids_containing_minimum_disk(voidable, &candidates, profile)
    }

    fn void_area(
        &self,
        voidable: &ContourSet,
        radius: f64,
        profile: DenseCopperBalanceProfile,
    ) -> f64 {
        self.full_void_area(radius, voidable.tolerance)
            + self.partial_voids(voidable, radius, profile).area()
    }
}

fn spatial_result_from_squared_radii(
    centers: &[Point],
    squared_radii: &[f64],
    baseline: DenseCopperBalanceResult,
    layer: SpatialCopperBalanceLayerRequest<'_>,
    retained_area_mm2: f64,
) -> DenseCopperBalanceResult {
    let full_voids = centers
        .iter()
        .zip(squared_radii)
        .map(|(center, radius_squared)| DenseCopperVoid {
            center: *center,
            radius_mm: radius_squared.sqrt(),
        })
        .collect::<Vec<_>>();
    let full_void_area_mm2 = ROUNDED_HEXAGON_AREA_FACTOR * squared_radii.iter().sum::<f64>();
    let partial_voids = baseline.partial_voids;
    let void_area_mm2 = full_void_area_mm2 + partial_voids.area();
    let generated_area_mm2 = (baseline.usable.area() - void_area_mm2).max(0.0);
    let achieved_density =
        (layer.existing_copper_area_mm2 + generated_area_mm2) / retained_area_mm2;
    let equivalent_radius_mm =
        (squared_radii.iter().sum::<f64>() / squared_radii.len() as f64).sqrt();
    let solution = DenseCopperBalanceSolution {
        mode: DenseCopperBalanceMode::Perforated {
            void_radius_mm: equivalent_radius_mm,
        },
        desired_added_area_mm2: baseline.solution.desired_added_area_mm2,
        generated_area_mm2,
        initial_density: baseline.solution.initial_density,
        achieved_density,
        target_density: layer.target_density,
        residual_error: (achieved_density - layer.target_density).abs(),
    };
    DenseCopperBalanceResult {
        solution,
        usable: baseline.usable,
        full_voids,
        partial_voids,
    }
}

fn hex_aligned_lattice_centers(
    bbox: BBox,
    origin: Point,
    profile: DenseCopperBalanceProfile,
) -> Vec<Point> {
    // Hexagon vertices are at 0°, 60°, ...; nearest-neighbor center vectors
    // are at 30°, 90°, ... so parallel flats face each other.
    let column_pitch = profile.lattice_column_pitch_mm();
    let first_column = ((bbox.min.x - origin.x) / column_pitch).floor() as i64;
    let last_column = ((bbox.max.x - origin.x) / column_pitch).ceil() as i64;
    let mut centers = Vec::new();

    for column in first_column..=last_column {
        let x = origin.x + column as f64 * column_pitch;
        let column_offset = if column.rem_euclid(2) == 0 {
            0.0
        } else {
            profile.pitch_mm / 2.0
        };
        let column_origin_y = origin.y + column_offset;
        let first_row = ((bbox.min.y - column_origin_y) / profile.pitch_mm).floor() as i64;
        let last_row = ((bbox.max.y - column_origin_y) / profile.pitch_mm).ceil() as i64;

        for row in first_row..=last_row {
            centers.push(Point::new(
                x,
                column_origin_y + row as f64 * profile.pitch_mm,
            ));
        }
    }

    centers
}

fn minimum_partial_candidates(
    voidable: &ContourSet,
    centers: &[Point],
    profile: DenseCopperBalanceProfile,
) -> Vec<(Point, f64)> {
    if centers.is_empty() {
        return Vec::new();
    }

    let min_radius = profile.min_void_radius_mm;
    let max_radius = profile.max_void_radius_mm;
    let min_trials = uniform_candidates(centers, min_radius);
    let max_trials = uniform_candidates(centers, max_radius);
    let accepted_at_min = accepted_candidate_mask(voidable, &min_trials, profile);
    let accepted_at_max = accepted_candidate_mask(voidable, &max_trials, profile);
    let mut bounds = accepted_at_min
        .into_iter()
        .zip(accepted_at_max)
        .map(|(at_min, at_max)| match (at_min, at_max) {
            (true, _) => Some((min_radius, min_radius)),
            (false, true) => Some((min_radius, max_radius)),
            (false, false) => None,
        })
        .collect::<Vec<_>>();

    loop {
        let trials = centers
            .iter()
            .copied()
            .zip(&bounds)
            .enumerate()
            .filter_map(|(index, (center, bounds))| {
                let &(low, high) = bounds.as_ref()?;
                (high - low > tol::FLATTEN_MM).then_some((index, center, (low + high) / 2.0))
            })
            .collect::<Vec<_>>();
        if trials.is_empty() {
            break;
        }
        let trial_geometry = trials
            .iter()
            .map(|(_, center, radius)| (*center, *radius))
            .collect::<Vec<_>>();
        let accepted = accepted_candidate_mask(voidable, &trial_geometry, profile);
        for ((index, _, radius), accepted) in trials.into_iter().zip(accepted) {
            let (low, high) = bounds[index].as_mut().expect("trial has radius bounds");
            if accepted {
                *high = radius;
            } else {
                *low = radius;
            }
        }
    }

    centers
        .iter()
        .copied()
        .zip(bounds)
        .filter_map(|(center, bounds)| bounds.map(|(_, high)| (center, high)))
        .collect()
}

fn fully_contained_hexagons(
    voidable: &ContourSet,
    centers: &[Point],
    profile: DenseCopperBalanceProfile,
) -> Vec<bool> {
    let candidates = uniform_candidates(centers, profile.max_void_radius_mm);
    let outside = hexagon_set_with_radii(&candidates, voidable.tolerance).difference(voidable);
    let outside_points = representative_points(&outside);
    candidate_point_mask(&candidates, &outside_points, profile)
        .into_iter()
        .map(|has_outside_point| !has_outside_point)
        .collect()
}

fn uniform_candidates(centers: &[Point], radius: f64) -> Vec<(Point, f64)> {
    centers.iter().map(|center| (*center, radius)).collect()
}

fn accepted_candidate_mask(
    voidable: &ContourSet,
    candidates: &[(Point, f64)],
    profile: DenseCopperBalanceProfile,
) -> Vec<bool> {
    let raw = hexagon_set_with_radii(candidates, voidable.tolerance).intersection(voidable);
    let core_points = minimum_disk_core_points(&raw, voidable, candidates, profile);
    candidate_point_mask(candidates, &core_points, profile)
}

fn clipped_voids_containing_minimum_disk(
    voidable: &ContourSet,
    candidates: &[(Point, f64)],
    profile: DenseCopperBalanceProfile,
) -> ContourSet {
    let raw = hexagon_set_with_radii(candidates, voidable.tolerance).intersection(voidable);
    let mut core_points = minimum_disk_core_points(&raw, voidable, candidates, profile);
    core_points.sort_by(|left, right| left.x.total_cmp(&right.x));
    let rings = raw
        .connected_components()
        .into_iter()
        .filter(|component| component_contains_any_point(component, &core_points))
        .flat_map(|component| component.rings)
        .collect();
    ContourSet::new(rings, FillRule::NonZero, voidable.tolerance)
}

fn minimum_disk_core_points(
    raw: &ContourSet,
    voidable: &ContourSet,
    candidates: &[(Point, f64)],
    profile: DenseCopperBalanceProfile,
) -> Vec<Point> {
    let minimum_radius = profile.minimum_partial_void_inradius_mm();
    let mut points = representative_points(&raw.disk_erode(minimum_radius));
    // Preserve the exact equality case, where erosion can collapse a valid
    // minimum disk to a zero-area point that regularization omits.
    points.extend(candidates.iter().filter_map(|(center, _)| {
        voidable
            .contains_disk(*center, minimum_radius)
            .then_some(*center)
    }));
    points
}

fn representative_points(region: &ContourSet) -> Vec<Point> {
    region
        .connected_components()
        .into_iter()
        .filter_map(|component| component.rings.first()?.first().copied())
        .map(|[x, y]| Point::new(x, y))
        .collect()
}

fn candidate_point_mask(
    candidates: &[(Point, f64)],
    points: &[Point],
    profile: DenseCopperBalanceProfile,
) -> Vec<bool> {
    let mut matched = vec![false; candidates.len()];
    let mut by_x = candidates
        .iter()
        .enumerate()
        .map(|(index, (center, radius))| (index, *center, *radius))
        .collect::<Vec<_>>();
    by_x.sort_by(|left, right| left.1.x.total_cmp(&right.1.x));
    let search_radius = profile.max_void_radius_mm + tol::FLATTEN_MM;
    for point in points {
        let first = by_x.partition_point(|(_, center, _)| center.x < point.x - search_radius);
        let nearest = by_x[first..]
            .iter()
            .take_while(|(_, center, _)| center.x <= point.x + search_radius)
            .filter_map(|&(index, center, radius)| {
                let distance = center.distance_to(*point);
                (distance <= radius + tol::FLATTEN_MM).then_some((index, distance))
            })
            .min_by(|left, right| left.1.total_cmp(&right.1));
        if let Some((index, _)) = nearest {
            matched[index] = true;
        }
    }
    matched
}

fn component_contains_any_point(component: &ContourSet, points: &[Point]) -> bool {
    let first = points.partition_point(|point| point.x < component.bbox.min.x);
    points[first..]
        .iter()
        .take_while(|point| point.x <= component.bbox.max.x)
        .any(|point| {
            point.y >= component.bbox.min.y
                && point.y <= component.bbox.max.y
                && component.contains_point(*point)
        })
}

fn hexagon_set_with_radii(candidates: &[(Point, f64)], tolerance: f64) -> ContourSet {
    let contours = candidates
        .iter()
        .map(|(center, radius)| {
            let hexagon = rounded_hexagonal_void(*radius).expect("candidate radius is validated");
            transform_cmds(hexagon.cmds.iter().copied(), Affine2::translation(*center))
        })
        .collect::<Vec<_>>();
    ContourSet::from_filled_contours(&contours, tolerance)
}

/// One slightly rounded, flat-top regular hexagonal void centered at zero.
pub fn rounded_hexagonal_void(radius: f64) -> Option<ContourBuf> {
    if !radius.is_finite() || radius <= 0.0 {
        return None;
    }

    let corner_radius = radius * HEXAGON_CORNER_RADIUS_RATIO;
    let tangent_distance = corner_radius / SQRT_3;
    let center_inset = 2.0 * corner_radius / SQRT_3;
    let vertices = (0..6)
        .map(|index| {
            let angle = index as f64 * PI / 3.0;
            Point::new(radius * angle.cos(), radius * angle.sin())
        })
        .collect::<Vec<_>>();

    let corner = |index: usize| {
        let previous = vertices[(index + 5) % 6];
        let vertex = vertices[index];
        let next = vertices[(index + 1) % 6];
        let incoming = vertex + (previous - vertex) * (tangent_distance / radius);
        let outgoing = vertex + (next - vertex) * (tangent_distance / radius);
        let center = vertex * ((radius - center_inset) / radius);
        (incoming, outgoing, center)
    };

    let (first_incoming, _, _) = corner(0);
    let mut commands = Vec::with_capacity(14);
    commands.push(PathCmd::move_to(first_incoming));
    for index in 0..6 {
        let (incoming, outgoing, center) = corner(index);
        if index > 0 {
            commands.push(PathCmd::line_to(incoming));
        }
        commands.push(PathCmd::arc_to(outgoing, center, false));
    }
    commands.push(PathCmd::close());
    Some(ContourBuf::new(commands))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::{BBox, PathOp, Point, tol};

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
        assert!(
            (DenseCopperBalanceProfile::V1.nearest_neighbor_web_mm() - (1.35 - SQRT_3 * 0.65))
                .abs()
                <= 1e-12
        );
        assert_eq!(DenseCopperBalanceProfile::V1.min_copper_web_mm, 0.20);
        assert_eq!(DenseCopperBalanceProfile::V1.boundary_web_mm, 0.20);
        assert_eq!(2.0 * minimum_core_radius, 0.20);
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
                safe_region: &panel,
                retained_area_mm2: panel.area(),
                lattice_origin: Point::ZERO,
                layers: &[],
            },
        )
        .unwrap();

        assert!(result.is_empty());
    }

    #[test]
    fn spatial_solver_keeps_constant_fields_uniform() {
        let panel = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(24.0, 16.0)),
            tol::REGION_MM,
        );
        let existing = ContourSet::empty(tol::REGION_MM);
        let layers = [SpatialCopperBalanceLayerRequest {
            existing_copper: &existing,
            existing_copper_area_mm2: 0.0,
            target_density: 0.5,
            stack_weight_mm2: 0.0,
        }];
        let result = generate_spatial_dense_copper_balance(
            DenseCopperBalanceProfile::V1,
            SpatialCopperBalanceRequest {
                panel_region: &panel,
                safe_region: &panel,
                retained_area_mm2: panel.area(),
                lattice_origin: Point::ZERO,
                layers: &layers,
            },
        )
        .unwrap()
        .pop()
        .unwrap();

        let (min, max) = result.full_void_radius_range_mm().unwrap();
        assert!(max - min <= 1e-9);
        assert!((result.solution.achieved_density - 0.5).abs() <= 5e-3);
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
            existing_copper: &existing,
            existing_copper_area_mm2: existing.area(),
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
                safe_region: &safe,
                retained_area_mm2: panel.area(),
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
        assert!(
            (result.solution.generated_area_mm2 - baseline.solution.generated_area_mm2).abs()
                <= AREA_SOLVE_TOLERANCE_MM2
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
                    safe_region: &safe,
                    retained_area_mm2: panel.area(),
                    lattice_origin: Point::ZERO,
                    layers: &[
                        SpatialCopperBalanceLayerRequest {
                            existing_copper: &top_copper,
                            existing_copper_area_mm2: top_copper.area(),
                            target_density: 0.75,
                            stack_weight_mm2,
                        },
                        SpatialCopperBalanceLayerRequest {
                            existing_copper: &bottom_copper,
                            existing_copper_area_mm2: bottom_copper.area(),
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
            (left.solution.generated_area_mm2 - right.solution.generated_area_mm2).abs()
                <= AREA_SOLVE_TOLERANCE_MM2
        }));
    }

    #[test]
    fn rejects_partial_voids_that_cannot_hold_the_minimum_disk() {
        let profile = DenseCopperBalanceProfile::V1;
        let voidable = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(0.15, 10.0)),
            tol::REGION_MM,
        );
        let lattice = LatticeCandidates::new(&voidable, Point::new(0.0, 0.0), profile);

        assert!(lattice.edge_candidates.is_empty());
    }

    #[test]
    fn exact_hex_containment_is_not_circumcircle_conservative() {
        let profile = DenseCopperBalanceProfile::V1;
        let voidable = ContourSet::rectangle(
            BBox::new(Point::new(-0.66, -0.57), Point::new(0.66, 0.57)),
            tol::REGION_MM,
        );
        let center = Point::new(0.0, 0.0);

        assert!(!voidable.contains_disk(center, profile.max_void_radius_mm));
        assert_eq!(
            fully_contained_hexagons(&voidable, &[center], profile),
            vec![true]
        );
    }

    #[test]
    fn partial_void_activation_is_monotone_in_radius() {
        let profile = DenseCopperBalanceProfile::V1;
        let voidable = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(4.0, 4.0)),
            tol::REGION_MM,
        );
        let centers = hex_aligned_lattice_centers(
            voidable.disk_dilate(profile.max_void_radius_mm).bbox,
            Point::new(0.0, 0.0),
            profile,
        );
        let mut previously_accepted = vec![false; centers.len()];
        for radius in [0.20, 0.30, 0.40, 0.50, 0.60, 0.65] {
            let candidates = uniform_candidates(&centers, radius);
            let accepted = accepted_candidate_mask(&voidable, &candidates, profile);
            assert!(
                previously_accepted
                    .iter()
                    .zip(&accepted)
                    .all(|(previous, current)| !previous || *current)
            );
            previously_accepted = accepted;
        }
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

    #[test]
    fn rounded_hexagon_uses_six_scaled_corner_arcs_and_tracks_analytic_area() {
        let radius = 0.8;
        let hexagon = rounded_hexagonal_void(radius).unwrap();
        let arcs = hexagon
            .cmds
            .iter()
            .filter(|command| command.op == PathOp::ArcTo)
            .collect::<Vec<_>>();
        assert_eq!(arcs.len(), 6);
        for arc in arcs {
            assert!(
                (arc.p0.distance_to(arc.p1) - radius * HEXAGON_CORNER_RADIUS_RATIO).abs() <= 1e-12
            );
        }

        let region = ContourSet::from_filled_contours(&[hexagon], tol::REGION_MM);
        let expected_area = ROUNDED_HEXAGON_AREA_FACTOR * radius.powi(2);
        assert!(
            (region.area() - expected_area).abs() <= expected_area * 2e-3,
            "geometric area {}, analytic area {}",
            region.area(),
            expected_area
        );
    }
}
