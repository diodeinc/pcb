//! Spatial copper-redistribution machinery: normalized lattice convolution,
//! exact lattice-tile coverage, and sum-preserving box projection.

use super::lattice::{ROUNDED_HEXAGON_AREA_FACTOR, SiteTable};
use super::{
    DenseCopperBalanceMode, DenseCopperBalanceProfile, DenseCopperBalanceResult,
    DenseCopperBalanceSolution, DenseCopperLattice, DenseCopperLatticeSite, DenseCopperVoid,
    NUMERIC_EPSILON, SpatialCopperBalanceLayerRequest,
};
use crate::geom::{BBox, ContourSet, Point};

const DENSITY_KERNEL_TRUNCATION: f64 = 3.0;
// A safety cap only. The lattice-scale term makes the objective strongly
// convex with a condition number near the number of scales, so projected
// gradient at the Lipschitz step is within the tolerance below in tens of
// iterations.
const SPATIAL_SOLVE_ITERATIONS: usize = 512;
// Updates below this leave every radius well inside half a quantization step
// of its converged value, so the emitted lattice is already final.
const SPATIAL_SOLVE_CONVERGENCE_MM2: f64 = 1e-6;

pub(super) fn normalized_stack_weights(
    layers: &[SpatialCopperBalanceLayerRequest<'_>],
) -> Vec<f64> {
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

/// Sparse row-normalized Gaussian from panel-lattice samples to the density
/// seen over one interaction length, evaluated on a site subset about that
/// length apart. Stored CSR (row offsets over flat index/weight arrays) so the
/// per-iteration passes stream contiguously. `smooth_adjoint` is the exact
/// transpose of `smooth`, so projected gradient follows the same density model
/// used by the objective.
pub(super) struct LatticeDensityKernel {
    sample_count: usize,
    row_offsets: Vec<u32>,
    sample_indices: Vec<u32>,
    weights: Vec<f64>,
}

impl LatticeDensityKernel {
    pub(super) fn new(samples: &SiteTable, lattice: DenseCopperLattice, sigma_mm: f64) -> Self {
        let evaluation_sites = density_evaluation_sites(&samples.sites, lattice, sigma_mm);
        let support_mm = DENSITY_KERNEL_TRUNCATION * sigma_mm;
        let inverse_two_sigma_squared = 0.5 / sigma_mm.powi(2);
        let column_span = (support_mm / lattice.column_pitch_mm()).ceil() as i64;
        let row_span = (support_mm / lattice.pitch_mm).ceil() as i64 + 1;
        let offsets = [0_i64, 1_i64].map(|parity| {
            (-column_span..=column_span)
                .flat_map(|column| {
                    (-row_span..=row_span).filter_map(move |row| {
                        let neighbor_parity = (parity + column).rem_euclid(2);
                        let dx = column as f64 * lattice.column_pitch_mm();
                        let dy = (row as f64 + (neighbor_parity - parity) as f64 / 2.0)
                            * lattice.pitch_mm;
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
                .collect::<Vec<(i64, i64, f64)>>()
        });
        let mut row_offsets = Vec::with_capacity(evaluation_sites.len() + 1);
        let mut csr_sample_indices = Vec::new();
        let mut weights = Vec::new();
        row_offsets.push(0_u32);
        for site in &evaluation_sites {
            let row_start = weights.len();
            for &(column_offset, row_offset, weight) in &offsets[site.column.rem_euclid(2) as usize]
            {
                if let Some(sample_index) = samples.sample(DenseCopperLatticeSite {
                    column: site.column + column_offset,
                    row: site.row + row_offset,
                }) {
                    csr_sample_indices.push(sample_index as u32);
                    weights.push(weight);
                }
            }
            let weight_sum = weights[row_start..].iter().sum::<f64>();
            if weight_sum > 0.0 {
                for weight in &mut weights[row_start..] {
                    *weight /= weight_sum;
                }
            }
            row_offsets.push(weights.len() as u32);
        }
        Self {
            sample_count: samples.sites.len(),
            row_offsets,
            sample_indices: csr_sample_indices,
            weights,
        }
    }

    pub(super) fn row_count(&self) -> usize {
        self.row_offsets.len() - 1
    }

    /// `||H||_1`, the most any one sample contributes across all rows. Every
    /// row sums to one, so `||H||_inf = 1` and this bounds the squared
    /// operator norm: `||H||_2^2 <= ||H||_1 ||H||_inf`.
    fn max_column_sum(&self) -> f64 {
        let mut column_sums = vec![0.0; self.sample_count];
        for (sample_index, weight) in self.sample_indices.iter().zip(&self.weights) {
            column_sums[*sample_index as usize] += weight;
        }
        column_sums.into_iter().fold(0.0, f64::max)
    }

    pub(super) fn smooth(&self, values: &[f64]) -> Vec<f64> {
        let mut result = vec![0.0; self.row_count()];
        self.smooth_into(values, &mut result);
        result
    }

    pub(super) fn smooth_into(&self, values: &[f64], result: &mut [f64]) {
        debug_assert_eq!(values.len(), self.sample_count);
        debug_assert_eq!(result.len(), self.row_count());
        for (row, result) in result.iter_mut().enumerate() {
            let span = self.row_offsets[row] as usize..self.row_offsets[row + 1] as usize;
            *result = self.sample_indices[span.clone()]
                .iter()
                .zip(&self.weights[span])
                .map(|(sample_index, weight)| weight * values[*sample_index as usize])
                .sum();
        }
    }

    #[cfg(test)]
    pub(super) fn smooth_adjoint(&self, values: &[f64]) -> Vec<f64> {
        let mut result = vec![0.0; self.sample_count];
        self.smooth_adjoint_into(values, &mut result);
        result
    }

    pub(super) fn smooth_adjoint_into(&self, values: &[f64], result: &mut [f64]) {
        debug_assert_eq!(values.len(), self.row_count());
        debug_assert_eq!(result.len(), self.sample_count);
        result.fill(0.0);
        for (row, value) in values.iter().enumerate() {
            let span = self.row_offsets[row] as usize..self.row_offsets[row + 1] as usize;
            for (sample_index, weight) in self.sample_indices[span.clone()]
                .iter()
                .zip(&self.weights[span])
            {
                result[*sample_index as usize] += weight * value;
            }
        }
    }
}

/// The interaction lengths the fill is matched at, longest first: the process
/// scale, then one per octave below it down to the lattice, then the lattice
/// tile itself.
///
/// Etch loading and plating current respond to the copper around a feature
/// over a length nobody knows better than its range, so the fit minimises the
/// deficit at every length in that range rather than at one. Matched at the
/// longest length alone the problem is a deconvolution: bands of saturated
/// voids that average out over exactly that length fit best, and are uneven
/// at every shorter one. The shorter lengths see those bands as the density
/// errors they are, and the tile-scale term makes the optimum unique.
pub(super) fn density_scales_mm(profile: DenseCopperBalanceProfile) -> Vec<f64> {
    std::iter::successors(Some(profile.density_sigma_mm), |sigma| Some(sigma / 2.0))
        .take_while(|sigma| *sigma >= profile.pitch_mm)
        // Narrower than the gap between neighbouring sites, so the normalized
        // kernel is each tile alone.
        .chain([profile.pitch_mm / (2.0 * DENSITY_KERNEL_TRUNCATION)])
        .collect()
}

/// One layer's density at every scale as a function of its squared void
/// radii, `rho_s = H_s (base - beta P x)`.
pub(super) struct LayerDensityModel<'a> {
    /// The density kernels, longest interaction length first.
    pub(super) scales: &'a [LatticeDensityKernel],
    /// Sample index of each squared-radius variable: the scatter `P`.
    pub(super) active_sites: &'a [usize],
    /// Copper fraction of each scale's evaluation sites with every full void
    /// closed: fixed copper, plus the generated plane less its clipped edge
    /// voids.
    pub(super) base_density: Vec<Vec<f64>>,
    /// `beta`, the share of a lattice cell a void takes per unit squared
    /// radius.
    pub(super) void_fraction_per_radius_squared: f64,
}

impl LayerDensityModel<'_> {
    fn sample_count(&self) -> usize {
        self.scales[0].sample_count
    }

    /// `void_fraction` spans the samples and is written only at active sites,
    /// so it must arrive zero everywhere else.
    fn scatter(&self, squared_radii: &[f64], void_fraction: &mut [f64]) {
        for (sample_index, radius_squared) in self.active_sites.iter().zip(squared_radii) {
            void_fraction[*sample_index] = self.void_fraction_per_radius_squared * radius_squared;
        }
    }

    /// The modeled copper fraction at the process scale itself, not its
    /// distance from target: the stack moment is the copper the panel carries,
    /// and subtracting targets would leave it blind to whatever imbalance the
    /// boards were drawn with.
    pub(super) fn density(&self, squared_radii: &[f64]) -> Vec<f64> {
        let mut void_fraction = vec![0.0; self.sample_count()];
        self.scatter(squared_radii, &mut void_fraction);
        let mut density = self.scales[0].smooth(&void_fraction);
        for (density, base) in density.iter_mut().zip(&self.base_density[0]) {
            *density = base - *density;
        }
        density
    }

    /// Projected gradient on the layer's mean squared density error summed
    /// over the scales, over `{x : lower <= x <= upper, sum(x) = pinned_sum}`.
    ///
    /// The moment is not in the objective: the settlement already spent what
    /// the stack was owed, and pinning the sum keeps the layer redistributing
    /// within the panel rather than spending against the stack.
    pub(super) fn redistribute(
        &self,
        mut squared_radii: Vec<f64>,
        target_density: f64,
        (lower, upper): (f64, f64),
        pinned_sum: f64,
    ) -> Vec<f64> {
        let beta = self.void_fraction_per_radius_squared;
        // Each scale's share of the gradient of its mean squared error, and
        // the reciprocal of the Lipschitz bound they add up to: the longest
        // step projected gradient is guaranteed to descend with.
        let shares = self
            .scales
            .iter()
            .map(|scale| 1.0 / scale.row_count() as f64)
            .collect::<Vec<_>>();
        let step = 1.0
            / (beta.powi(2)
                * self
                    .scales
                    .iter()
                    .zip(&shares)
                    .map(|(scale, share)| share * scale.max_column_sum())
                    .sum::<f64>());
        let mut void_fraction = vec![0.0; self.sample_count()];
        let mut influence = vec![0.0; self.sample_count()];
        let mut scale_influence = vec![0.0; self.sample_count()];
        let mut residuals = self
            .scales
            .iter()
            .map(|scale| vec![0.0; scale.row_count()])
            .collect::<Vec<_>>();
        let mut proposal = squared_radii.clone();
        let mut shift = 0.0;
        for _ in 0..SPATIAL_SOLVE_ITERATIONS {
            self.scatter(&squared_radii, &mut void_fraction);
            influence.fill(0.0);
            for (((scale, base), residual), share) in self
                .scales
                .iter()
                .zip(&self.base_density)
                .zip(&mut residuals)
                .zip(&shares)
            {
                scale.smooth_into(&void_fraction, residual);
                for (residual, base) in residual.iter_mut().zip(base) {
                    *residual = share * (base - *residual - target_density);
                }
                scale.smooth_adjoint_into(residual, &mut scale_influence);
                for (influence, scale_influence) in influence.iter_mut().zip(&scale_influence) {
                    *influence += scale_influence;
                }
            }
            for ((proposal, radius_squared), sample_index) in proposal
                .iter_mut()
                .zip(&squared_radii)
                .zip(self.active_sites)
            {
                *proposal = radius_squared + step * beta * influence[*sample_index];
            }
            // Successive proposals differ by one gradient step, so the last
            // shift is near this one.
            shift = project_box_sum(&mut proposal, lower, upper, pinned_sum, shift);
            let update = squared_radii
                .iter()
                .zip(&proposal)
                .map(|(before, after)| (before - after).abs())
                .fold(0.0_f64, f64::max);
            std::mem::swap(&mut squared_radii, &mut proposal);
            if update < SPATIAL_SOLVE_CONVERGENCE_MM2 {
                break;
            }
        }
        squared_radii
    }
}

/// Sample one scale's objective on a deterministic subset of the fabrication
/// lattice about that scale apart. Geometry and output stay on the full
/// lattice.
fn density_evaluation_sites(
    samples: &[DenseCopperLatticeSite],
    lattice: DenseCopperLattice,
    sigma_mm: f64,
) -> Vec<DenseCopperLatticeSite> {
    let stride = (sigma_mm / lattice.pitch_mm).round().max(1.0) as i64;
    // Anchoring the coarse grid on the first sample keeps the result nonempty
    // for every nonempty input.
    let anchor = samples[0];
    samples
        .iter()
        .copied()
        .filter(|site| {
            (site.column - anchor.column).rem_euclid(stride) == 0
                && (site.row - anchor.row).rem_euclid(stride) == 0
        })
        .collect()
}

/// Fraction of each site's rectangular lattice tile covered by `region`.
///
/// The staggered columns tile the plane exactly with column-pitch × pitch
/// rectangles centered on the sites. Odd columns sit half a pitch up, so every
/// tile is two stacked cells of one rectangular grid half a pitch tall: an
/// even column's row `r` takes half-rows `2r - 1` and `2r`, an odd column's
/// `2r` and `2r + 1`. One exact grid measurement therefore serves both
/// parities, and a lattice of voids — periodic at the very pitch any sampling
/// would share — is measured rather than aliased.
pub(super) fn lattice_cell_coverage(
    sites: &[DenseCopperLatticeSite],
    region: &ContourSet,
    lattice: DenseCopperLattice,
) -> Vec<f64> {
    // Column, and the lower of the two half-rows, of each site's tile.
    let tiles = sites
        .iter()
        .map(|site| (site.column, 2 * site.row - 1 + site.column.rem_euclid(2)))
        .collect::<Vec<_>>();
    let span = |values: &mut dyn Iterator<Item = i64>| {
        values.fold((i64::MAX, i64::MIN), |(low, high), value| {
            (low.min(value), high.max(value))
        })
    };
    if tiles.is_empty() {
        return Vec::new();
    }
    let (first_column, last_column) = span(&mut tiles.iter().map(|tile| tile.0));
    let (first_half_row, last_half_row) = span(&mut tiles.iter().map(|tile| tile.1));
    let (width, height) = (lattice.column_pitch_mm(), lattice.pitch_mm / 2.0);
    let corner = |column: i64, half_row: i64| {
        Point::new(
            lattice.origin.x + (column as f64 - 0.5) * width,
            lattice.origin.y + half_row as f64 * height,
        )
    };
    let columns = (last_column - first_column + 1) as usize;
    let coverage = region.grid_coverage(
        BBox::new(
            corner(first_column, first_half_row),
            corner(last_column + 1, last_half_row + 2),
        ),
        columns,
        (last_half_row - first_half_row + 2) as usize,
    );
    tiles
        .iter()
        .map(|(column, half_row)| {
            let lower =
                (half_row - first_half_row) as usize * columns + (column - first_column) as usize;
            (coverage[lower] + coverage[lower + columns]) / 2.0
        })
        .collect()
}

/// Euclidean projection onto `{x : lower <= x <= upper, sum(x) = target}`, in
/// place. Returns the shift it settled on, to seed the next call.
///
/// Water-filling: one common shift moves every value and the box clamps. The
/// clamped sum is piecewise linear and non-increasing in the shift, with slope
/// minus the number of values the box leaves free, so Newton's step is exact
/// once it shares a linear piece with the root. It is kept inside a bracket
/// and gives way to bisection whenever it stops halving its own stride, which
/// bounds the pass count however the pieces fall. A target outside what the
/// box can reach saturates every value at the nearer bound, which is the
/// closest feasible point.
pub(super) fn project_box_sum(
    values: &mut [f64],
    lower: f64,
    upper: f64,
    target: f64,
    shift_guess: f64,
) -> f64 {
    let count = values.len() as f64;
    let target = target.clamp(count * lower, count * upper);
    let (least, greatest) = values
        .iter()
        .fold((f64::MAX, f64::MIN), |(least, greatest), value| {
            (least.min(*value), greatest.max(*value))
        });
    // Every value sits at `upper` for shifts up to `low`, at `lower` from
    // `high` on.
    let (mut low, mut high) = (least - upper, greatest - lower);
    // Not `clamp`: no values leaves an inverted bracket, which the loop below
    // leaves at once.
    let mut shift = shift_guess.max(low).min(high);
    let mut stride = high - low;
    loop {
        let (sum, free) = values.iter().fold((0.0, 0.0), |(sum, free), value| {
            let moved = value - shift;
            (
                sum + moved.clamp(lower, upper),
                free + f64::from(lower < moved && moved < upper),
            )
        });
        let excess = sum - target;
        if excess > 0.0 {
            low = shift;
        } else if excess < 0.0 {
            high = shift;
        } else {
            break;
        }
        let newton = shift + excess / free;
        let next = if low < newton && newton < high && 2.0 * (newton - shift).abs() <= stride {
            newton
        } else {
            (low + high) / 2.0
        };
        // The bracket has closed to adjacent floats.
        if next <= low || next >= high {
            break;
        }
        stride = (next - shift).abs();
        shift = next;
    }
    for value in values {
        *value = (*value - shift).clamp(lower, upper);
    }
    shift
}

/// The void-area level each site is emitted at, as a squared radius.
///
/// Squared radius is what gets quantized because rounded-hex area is
/// proportional to it, so uniform levels are uniform area increments. A solved
/// field is smooth, so rounding each site on its own rounds whole
/// neighbourhoods the same way: it moves their copper density by up to half a
/// level's worth at every scale the solve matched, and the layer's pinned area
/// by percents. The error of each rounding is instead handed to sites still to
/// come, so every neighbourhood and the layer keep the void area the solve
/// gave them and the error lives between adjacent voids one level apart.
///
/// Sites arrive column by column, rows ascending, as
/// [`DenseCopperLattice::sites_covering`] enumerates them. A site's six
/// nearest neighbours all lie one pitch away, and the three still to come are
/// the one above it and the two in the next column, so the error is split
/// evenly among those of them this layer has. A site with none ahead drops
/// its error, which is at most half a level at the few sites closing off the
/// trailing edge of a region.
pub(super) fn diffuse_to_levels(
    sites: &[DenseCopperLatticeSite],
    squared_radii: &[f64],
    profile: DenseCopperBalanceProfile,
) -> Vec<f64> {
    let mut table = SiteTable::spanning(sites.iter());
    for site in sites {
        table.admit(*site);
    }
    let mut carried = vec![0.0; sites.len()];
    sites
        .iter()
        .zip(squared_radii)
        .enumerate()
        .map(|(index, (site, radius_squared))| {
            let owed = radius_squared + carried[index];
            let level = profile.nearest_void_area_level(owed);
            let parity = site.column.rem_euclid(2);
            let ahead = [
                (site.column, site.row + 1),
                (site.column + 1, site.row + parity - 1),
                (site.column + 1, site.row + parity),
            ]
            .map(|(column, row)| table.sample(DenseCopperLatticeSite { column, row }));
            let heirs = ahead.iter().flatten().count() as f64;
            for heir in ahead.into_iter().flatten() {
                debug_assert!(heir > index, "sites must arrive in scan order");
                carried[heir] += (owed - level) / heirs;
            }
            level
        })
        .collect()
}

pub(super) fn spatial_result_from_squared_radii(
    sites: &[DenseCopperLatticeSite],
    squared_radii: &[f64],
    baseline: DenseCopperBalanceResult,
    layer: SpatialCopperBalanceLayerRequest<'_>,
    density_domain_area_mm2: f64,
    profile: DenseCopperBalanceProfile,
) -> DenseCopperBalanceResult {
    let full_voids = sites
        .iter()
        .zip(diffuse_to_levels(sites, squared_radii, profile))
        .map(|(site, level)| DenseCopperVoid {
            site: *site,
            radius_mm: level.sqrt(),
        })
        .collect::<Vec<_>>();
    let full_void_area_mm2 = ROUNDED_HEXAGON_AREA_FACTOR
        * full_voids
            .iter()
            .map(|void| void.radius_mm * void.radius_mm)
            .sum::<f64>();
    let clipped_edge_void_area_mm2 = baseline.edge_void_emission.area_mm2();
    let void_area_mm2 = full_void_area_mm2 + clipped_edge_void_area_mm2;
    let generated_area_mm2 = (baseline.usable.area() - void_area_mm2).max(0.0);
    let achieved_density =
        (layer.existing_copper.area() + generated_area_mm2) / density_domain_area_mm2;
    let equivalent_radius_mm = (full_voids
        .iter()
        .map(|void| void.radius_mm * void.radius_mm)
        .sum::<f64>()
        / full_voids.len() as f64)
        .sqrt();
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
        lattice: baseline.lattice,
        usable: baseline.usable,
        voidable: baseline.voidable,
        full_voids,
        edge_voids: baseline.edge_voids,
        edge_void_emission: baseline.edge_void_emission,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::{BBox, ContourSet, Point, Resolution, tol};

    fn res(tolerance_mm: f64) -> Resolution {
        Resolution::default().with_tolerance(tolerance_mm)
    }

    fn panel_samples(panel: &ContourSet, lattice: DenseCopperLattice) -> SiteTable {
        let sites = lattice.sites_covering(panel.bbox);
        let mut samples = SiteTable::spanning(sites.iter());
        for site in sites {
            if panel.contains_point(lattice.center(site)) {
                samples.admit(site);
            }
        }
        samples
    }

    fn projected(values: &[f64], lower: f64, upper: f64, target: f64, guess: f64) -> Vec<f64> {
        let mut values = values.to_vec();
        project_box_sum(&mut values, lower, upper, target, guess);
        values
    }

    /// The projection lands the sum on the target when the box can reach it,
    /// and saturates at the nearer bound when it cannot.
    #[test]
    fn box_sum_projection_hits_reachable_targets_and_saturates_otherwise() {
        let values: [f64; 5] = [0.10, 0.42, -0.30, 0.25, 0.61];
        let (lower, upper) = (0.04_f64, 0.42_f64);

        for target in [0.6, 1.0, 1.9] {
            let projected = projected(&values, lower, upper, target, 0.0);
            assert!(projected.iter().all(|v| (lower..=upper).contains(v)));
            assert!(
                (projected.iter().sum::<f64>() - target).abs() <= 1e-12,
                "{projected:?}"
            );
        }

        // Beyond the box's reach on either side, every value saturates.
        let low = projected(&values, lower, upper, 0.0, 0.0);
        assert!(low.iter().all(|v| (v - lower).abs() <= 1e-12));
        let high = projected(&values, lower, upper, 10.0, 0.0);
        assert!(high.iter().all(|v| (v - upper).abs() <= 1e-12));
    }

    /// The shift is unique wherever any value is left free, so bisecting for
    /// it to the last bit and stepping to it have to agree, from any seed.
    #[test]
    fn box_sum_projection_matches_bisection_from_any_seed() {
        let bisected = |values: &[f64], lower: f64, upper: f64, target: f64| {
            let clamped_sum = |shift: f64| -> f64 {
                values
                    .iter()
                    .map(|value| (value - shift).clamp(lower, upper))
                    .sum()
            };
            let (mut low, mut high) = (-10.0, 10.0);
            for _ in 0..200 {
                let trial = (low + high) / 2.0;
                if clamped_sum(trial) > target {
                    low = trial;
                } else {
                    high = trial;
                }
            }
            values
                .iter()
                .map(|value| (value - (low + high) / 2.0).clamp(lower, upper))
                .collect::<Vec<_>>()
        };
        let (lower, upper) = (0.04, 0.4225);
        // Clustered, spread, and mostly saturated fields.
        let fields = [0.002, 0.2, 3.0].map(|spread| {
            (0..997)
                .map(|index| 0.23 + spread * (((index * 7919) % 1013) as f64 / 1013.0 - 0.5))
                .collect::<Vec<_>>()
        });
        for values in &fields {
            for fraction in [0.0, 0.03, 0.5, 0.97, 1.0] {
                let target = values.len() as f64 * (lower + fraction * (upper - lower));
                let expected = bisected(values, lower, upper, target);
                for guess in [0.0, -5.0, 5.0, 0.1] {
                    let actual = projected(values, lower, upper, target, guess);
                    let worst = actual
                        .iter()
                        .zip(&expected)
                        .map(|(left, right)| (left - right).abs())
                        .fold(0.0_f64, f64::max);
                    assert!(worst <= 1e-12, "{fraction} from {guess}: {worst}");
                }
            }
        }
    }

    /// Tile coverage is an area, so a region cutting through tiles at any
    /// offset has to come back as each tile's exact overlap, on even and odd
    /// columns alike.
    #[test]
    fn lattice_cell_coverage_is_each_tiles_exact_overlap() {
        let profile = DenseCopperBalanceProfile::V1;
        let bounds = BBox::new(Point::new(-2.0, -3.0), Point::new(11.0, 8.0));
        let lattice = DenseCopperLattice {
            origin: Point::new(0.4, -0.3),
            pitch_mm: profile.pitch_mm,
        };
        let sites = lattice.sites_covering(bounds);
        let cut = BBox::new(Point::new(1.23, -0.71), Point::new(7.9, 4.56));
        let region = ContourSet::rectangle(cut, res(tol::REGION_MM));

        let (width, height) = (lattice.column_pitch_mm(), lattice.pitch_mm);
        let overlap = |low: f64, high: f64, cut_low: f64, cut_high: f64| {
            (high.min(cut_high) - low.max(cut_low)).max(0.0)
        };
        let coverage = lattice_cell_coverage(&sites, &region, lattice);
        assert_eq!(coverage.len(), sites.len());
        let mut partial = [0, 0];
        for (site, coverage) in sites.iter().zip(coverage) {
            let sample = lattice.center(*site);
            let expected = overlap(
                sample.x - width / 2.0,
                sample.x + width / 2.0,
                cut.min.x,
                cut.max.x,
            ) * overlap(
                sample.y - height / 2.0,
                sample.y + height / 2.0,
                cut.min.y,
                cut.max.y,
            ) / (width * height);
            assert!(
                (coverage - expected).abs() <= 1e-9,
                "{sample:?}: {coverage} != {expected}"
            );
            if expected > 0.0 && expected < 1.0 {
                partial[site.column.rem_euclid(2) as usize] += 1;
            }
        }
        assert!(partial[0] > 0 && partial[1] > 0);
    }

    fn v1_lattice() -> DenseCopperLattice {
        DenseCopperLattice {
            origin: Point::ZERO,
            pitch_mm: DenseCopperBalanceProfile::V1.pitch_mm,
        }
    }

    #[test]
    fn density_kernel_adjoint_matches_the_forward_operator() {
        let panel = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(20.0, 12.0)),
            res(tol::REGION_MM),
        );
        let samples = panel_samples(&panel, v1_lattice());
        for sigma_mm in density_scales_mm(DenseCopperBalanceProfile::V1) {
            let kernel = LatticeDensityKernel::new(&samples, v1_lattice(), sigma_mm);
            let source = (0..samples.sites.len())
                .map(|index| ((index * 17 % 29) as f64 - 14.0) / 29.0)
                .collect::<Vec<_>>();
            let residual = (0..kernel.row_count())
                .map(|index| ((index * 11 % 23) as f64 - 11.0) / 23.0)
                .collect::<Vec<_>>();

            let forward_inner_product = kernel
                .smooth(&source)
                .iter()
                .zip(&residual)
                .map(|(left, right)| left * right)
                .sum::<f64>();
            let adjoint_inner_product = source
                .iter()
                .zip(kernel.smooth_adjoint(&residual))
                .map(|(left, right)| left * right)
                .sum::<f64>();

            assert!((forward_inner_product - adjoint_inner_product).abs() <= 1e-12);
        }
    }

    /// A solved field that is locally uniform is the worst case for rounding:
    /// every site rounds the same way. Just under half a level off, rounding
    /// each site alone moves the copper density by a percent and a half
    /// everywhere, while handing the error on keeps the total void area to
    /// within the one site that has nowhere to hand it, and the density at
    /// every scale above the tile to a small fraction of the rounding bias.
    #[test]
    fn a_uniform_field_keeps_its_void_area_through_quantization() {
        let profile = DenseCopperBalanceProfile::V1;
        let panel = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(60.0, 40.0)),
            res(tol::REGION_MM),
        );
        let samples = panel_samples(&panel, v1_lattice());
        let sites = &samples.sites;
        let spacing = profile.void_area_level(1) - profile.void_area_level(0);
        let solved = vec![profile.void_area_level(7) + 0.45 * spacing; sites.len()];
        let void_fraction_per_radius_squared =
            ROUNDED_HEXAGON_AREA_FACTOR / (v1_lattice().column_pitch_mm() * profile.pitch_mm);
        let rounded = solved
            .iter()
            .map(|radius_squared| profile.nearest_void_area_level(*radius_squared))
            .collect::<Vec<_>>();
        let diffused = diffuse_to_levels(sites, &solved, profile);

        let is_level = |value: f64| {
            (0..profile.void_area_levels).any(|level| value == profile.void_area_level(level))
        };
        assert!(diffused.iter().all(|value| is_level(*value)));
        let drift = diffused.iter().sum::<f64>() - solved.iter().sum::<f64>();
        assert!(drift.abs() <= spacing / 2.0, "{drift}");

        let scales = density_scales_mm(profile);
        for sigma_mm in &scales[..scales.len() - 1] {
            let kernel = LatticeDensityKernel::new(&samples, v1_lattice(), *sigma_mm);
            // Worst copper-density error at this scale.
            let density_error = |emitted: &[f64]| {
                let error = emitted
                    .iter()
                    .zip(&solved)
                    .map(|(emitted, solved)| void_fraction_per_radius_squared * (emitted - solved))
                    .collect::<Vec<_>>();
                kernel
                    .smooth(&error)
                    .into_iter()
                    .fold(0.0_f64, |worst, error| worst.max(error.abs()))
            };
            assert!(density_error(&rounded) > 0.014);
            assert!(
                density_error(&diffused) < density_error(&rounded) / 10.0,
                "{sigma_mm}: {}",
                density_error(&diffused)
            );
        }
    }

    /// The ladder runs from the process scale down by octaves and ends on the
    /// tile itself: a kernel that reads every sample alone.
    #[test]
    fn density_scales_run_from_the_process_scale_to_the_tile() {
        let profile = DenseCopperBalanceProfile::V1;
        let scales = density_scales_mm(profile);
        assert_eq!(scales[..2], [5.0, 2.5]);
        assert_eq!(scales.len(), 3);

        let panel = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(20.0, 12.0)),
            res(tol::REGION_MM),
        );
        let samples = panel_samples(&panel, v1_lattice());
        let tile = LatticeDensityKernel::new(&samples, v1_lattice(), scales[2]);
        let source = (0..samples.sites.len())
            .map(|index| index as f64)
            .collect::<Vec<_>>();
        assert_eq!(tile.smooth(&source), source);
    }

    /// The solved field is the objective's one minimiser — not wherever an
    /// iteration happened to stop — so it does not depend on where the solve
    /// starts and solving again from it moves nothing.
    #[test]
    fn redistribution_is_independent_of_its_starting_field() {
        let profile = DenseCopperBalanceProfile::V1;
        let panel = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(60.0, 30.0)),
            res(tol::REGION_MM),
        );
        let samples = panel_samples(&panel, v1_lattice());
        let scales = density_scales_mm(profile)
            .into_iter()
            .map(|sigma_mm| LatticeDensityKernel::new(&samples, v1_lattice(), sigma_mm))
            .collect::<Vec<_>>();
        // Solid fixed copper on the left third, fill everywhere else.
        let centers = samples
            .sites
            .iter()
            .map(|site| v1_lattice().center(*site))
            .collect::<Vec<_>>();
        let active_sites = (0..centers.len())
            .filter(|site| centers[*site].x >= 20.0)
            .collect::<Vec<_>>();
        let base_coverage = vec![1.0; centers.len()];
        let model = LayerDensityModel {
            scales: &scales,
            active_sites: &active_sites,
            base_density: scales
                .iter()
                .map(|scale| scale.smooth(&base_coverage))
                .collect(),
            void_fraction_per_radius_squared: ROUNDED_HEXAGON_AREA_FACTOR
                / (v1_lattice().column_pitch_mm() * profile.pitch_mm),
        };
        let bounds = (
            profile.min_void_radius_mm.powi(2),
            profile.max_void_radius_mm.powi(2),
        );
        let middle = 0.5 * (bounds.0 + bounds.1);
        let pinned_sum = middle * active_sites.len() as f64;
        let solve = |start: Vec<f64>| model.redistribute(start, 0.75, bounds, pinned_sum);

        let from_uniform = solve(vec![middle; active_sites.len()]);
        // The stark field a single-scale fit drifts toward: saturated stripes.
        let from_stripes = solve(
            active_sites
                .iter()
                .map(|site| {
                    if (centers[*site].x / 5.0) as i64 % 2 == 0 {
                        bounds.0
                    } else {
                        bounds.1
                    }
                })
                .collect(),
        );
        let level_step = (bounds.1 - bounds.0) / (profile.void_area_levels - 1) as f64;
        let furthest = |left: &[f64], right: &[f64]| {
            left.iter()
                .zip(right)
                .map(|(left, right)| (left - right).abs())
                .fold(0.0, f64::max)
        };
        assert!(furthest(&from_uniform, &from_stripes) < 0.01 * level_step);
        assert!(furthest(&from_uniform, &solve(from_uniform.clone())) < 0.01 * level_step);
        // It answers the copper beside it rather than staying flat.
        assert!(furthest(&from_uniform, &vec![middle; active_sites.len()]) > level_step);
    }
}
