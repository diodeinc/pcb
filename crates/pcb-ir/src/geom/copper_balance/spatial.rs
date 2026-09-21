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

/// Sparse row-normalized convolution from panel-lattice samples to a shared
/// evaluation field, stored CSR (row offsets over flat index/weight arrays)
/// so the per-iteration passes stream contiguously. `smooth_adjoint` is the
/// exact transpose of `smooth`, so projected gradient follows the same
/// density model used by the objective.
pub(super) struct LatticeDensityKernel {
    sample_count: usize,
    row_offsets: Vec<u32>,
    sample_indices: Vec<u32>,
    weights: Vec<f64>,
}

impl LatticeDensityKernel {
    pub(super) fn new(
        samples: &SiteTable,
        evaluation_sites: &[DenseCopperLatticeSite],
        lattice: DenseCopperLattice,
        profile: DenseCopperBalanceProfile,
    ) -> Self {
        let support_mm = DENSITY_KERNEL_TRUNCATION * profile.density_sigma_mm;
        let inverse_two_sigma_squared = 0.5 / profile.density_sigma_mm.powi(2);
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
        for site in evaluation_sites {
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

    /// `||H||_1`, the most any one sample contributes across all rows.
    ///
    /// Every row sums to one, so `||H||_inf = 1` and this bounds the squared
    /// operator norm: `||H||_2^2 <= ||H||_1 ||H||_inf`. Each evaluation site is
    /// one of its own samples, so the sum is never zero.
    pub(super) fn max_column_sum(&self) -> f64 {
        let mut column_sums = vec![0.0; self.sample_count];
        for (sample_index, weight) in self.sample_indices.iter().zip(&self.weights) {
            column_sums[*sample_index as usize] += weight;
        }
        column_sums.into_iter().fold(0.0, f64::max)
    }

    /// `||H e_i||^2` for every sample: how strongly a change at that sample
    /// alone registers across the evaluation field.
    pub(super) fn column_square_sums(&self) -> Vec<f64> {
        let mut column_sums = vec![0.0; self.sample_count];
        for (sample_index, weight) in self.sample_indices.iter().zip(&self.weights) {
            column_sums[*sample_index as usize] += weight * weight;
        }
        column_sums
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

/// One layer's density at the evaluation sites as a function of its squared
/// void radii, `rho = base - beta H P x`.
pub(super) struct LayerDensityModel<'a> {
    pub(super) kernel: &'a LatticeDensityKernel,
    /// Sample index of each squared-radius variable: the scatter `P`.
    pub(super) active_sites: &'a [usize],
    /// Copper fraction with every full void closed: fixed copper, plus the
    /// generated plane less its clipped edge voids.
    pub(super) base_density: Vec<f64>,
    /// `beta`, the share of a lattice cell a void takes per unit squared
    /// radius.
    pub(super) void_fraction_per_radius_squared: f64,
}

impl LayerDensityModel<'_> {
    /// `void_fraction` spans the samples and is written only at active sites,
    /// so it must arrive zero everywhere else.
    fn density_into(&self, squared_radii: &[f64], void_fraction: &mut [f64], density: &mut [f64]) {
        for (sample_index, radius_squared) in self.active_sites.iter().zip(squared_radii) {
            void_fraction[*sample_index] = self.void_fraction_per_radius_squared * radius_squared;
        }
        self.kernel.smooth_into(void_fraction, density);
        for (density, base) in density.iter_mut().zip(&self.base_density) {
            *density = base - *density;
        }
    }

    /// The modeled copper fraction itself, not its distance from target: the
    /// stack moment is the copper the panel carries, and subtracting targets
    /// would leave it blind to whatever imbalance the boards were drawn with.
    pub(super) fn density(&self, squared_radii: &[f64]) -> Vec<f64> {
        let mut density = vec![0.0; self.kernel.row_count()];
        self.density_into(
            squared_radii,
            &mut vec![0.0; self.kernel.sample_count],
            &mut density,
        );
        density
    }

    /// What rounding the radii to their levels costs the objective, in
    /// expectation.
    ///
    /// A squared radius anywhere within half a level of an emitted one is
    /// emitted the same, so the solve cannot be held to displacements any
    /// finer. Spread independently and evenly over one level, `spacing`, they
    /// have variance `spacing^2 / 12`, and each reaches the objective
    /// `||rho - target||^2 / 2` through its own kernel column, so together
    /// they cost `(beta spacing)^2 / 24` times the summed `||H e_i||^2` of the
    /// layer's sites.
    pub(super) fn rounding_floor(&self, column_square_sums: &[f64], level_spacing: f64) -> f64 {
        (self.void_fraction_per_radius_squared * level_spacing).powi(2) / 24.0
            * self
                .active_sites
                .iter()
                .map(|sample_index| column_square_sums[*sample_index])
                .sum::<f64>()
    }

    /// Accelerated projected gradient (FISTA) on the layer's own squared
    /// density error, over `C = {x : lower <= x <= upper, sum(x) = pinned_sum}`,
    /// with `step` the reciprocal of the gradient's Lipschitz constant.
    ///
    /// The moment is not in the objective: the settlement already spent what
    /// the stack was owed, and pinning the sum keeps the layer redistributing
    /// within the panel rather than spending against the stack.
    ///
    /// It stops on a certificate rather than a tolerance. The objective is
    /// convex, so `f* >= f(y) + min over C of <g, z - y>` with `g` its gradient
    /// at the look-ahead point `y`, and the step taken from there lands within
    /// `f(y) + <g, x - y> + ||x - y||^2 / (2 step)`. Their difference bounds
    /// everything the new iterate `x` has left to gain, and once that is under
    /// `rounding_floor` no further iterate is one the emitted levels could
    /// tell from this one. Plain projected gradient does not get there: the
    /// smoothing leaves the problem so ill-conditioned that its bound is still
    /// several floors up after a thousand steps, where momentum certifies in a
    /// few hundred.
    pub(super) fn redistribute(
        &self,
        squared_radii: Vec<f64>,
        target_density: f64,
        (lower, upper): (f64, f64),
        pinned_sum: f64,
        step: f64,
        rounding_floor: f64,
    ) -> Vec<f64> {
        let mut void_fraction = vec![0.0; self.kernel.sample_count];
        let mut influence = vec![0.0; self.kernel.sample_count];
        let mut residual = vec![0.0; self.kernel.row_count()];
        let mut gradient = vec![0.0; squared_radii.len()];
        let mut iterate = squared_radii;
        let mut lookahead = iterate.clone();
        let mut proposal = iterate.clone();
        let (mut momentum, mut shift) = (1.0_f64, 0.0);
        // The safety bound is the method's own guarantee,
        // `f(x_k) - f* <= 2 ||x_0 - x*||^2 / (step (k + 1)^2)`, with no radius
        // farther from its optimum than the box is wide: past it the floor is
        // met whether or not the certificate has caught up. A single level
        // leaves `0 / 0` and nothing to iterate, which is what the cast makes
        // of it.
        let cap = (2.0 * iterate.len() as f64 * (upper - lower).powi(2) / (step * rounding_floor))
            .sqrt()
            .ceil() as usize;
        for _ in 0..cap {
            self.density_into(&lookahead, &mut void_fraction, &mut residual);
            for residual in &mut residual {
                *residual -= target_density;
            }
            self.kernel.smooth_adjoint_into(&residual, &mut influence);
            for (((gradient, proposal), lookahead), sample_index) in gradient
                .iter_mut()
                .zip(&mut proposal)
                .zip(&lookahead)
                .zip(self.active_sites)
            {
                *gradient = -self.void_fraction_per_radius_squared * influence[*sample_index];
                *proposal = lookahead - step * *gradient;
            }
            // Successive proposals differ by one small gradient step, so the
            // last shift is nearly this one.
            shift = project_box_sum(&mut proposal, lower, upper, pinned_sum, shift);

            let (along, moved) = gradient.iter().zip(&proposal).zip(&lookahead).fold(
                (0.0, 0.0),
                |(along, moved), ((gradient, proposal), lookahead)| {
                    (
                        along + gradient * proposal,
                        moved + (proposal - lookahead).powi(2),
                    )
                },
            );
            let left_to_gain = along - least_inner_product(&mut gradient, lower, upper, pinned_sum)
                + moved / (2.0 * step);

            let next_momentum = (1.0 + (1.0 + 4.0 * momentum * momentum).sqrt()) / 2.0;
            let carry = (momentum - 1.0) / next_momentum;
            for ((lookahead, proposal), iterate) in
                lookahead.iter_mut().zip(&proposal).zip(&iterate)
            {
                *lookahead = proposal + carry * (proposal - iterate);
            }
            momentum = next_momentum;
            std::mem::swap(&mut iterate, &mut proposal);
            if left_to_gain <= rounding_floor {
                break;
            }
        }
        iterate
    }
}

/// Sample the 5 mm-scale objective on a deterministic subset of the 1.35 mm
/// fabrication lattice. Geometry and output stay on the full lattice.
pub(super) fn density_evaluation_sites(
    samples: &[DenseCopperLatticeSite],
    profile: DenseCopperBalanceProfile,
) -> Vec<DenseCopperLatticeSite> {
    let stride = (profile.density_sigma_mm / profile.pitch_mm)
        .round()
        .max(1.0) as i64;
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

/// `min <weights, z>` over `{z : lower <= z <= upper, sum(z) = target}`, the
/// linear program behind the duality gap. Reorders `weights`.
///
/// Every value starts at `lower` and the rest of the sum is spent where it
/// costs least: whole box widths on the smallest weights, the remainder on the
/// next. The target saturates at what the box can reach, as it does in
/// [`project_box_sum`].
pub(super) fn least_inner_product(weights: &mut [f64], lower: f64, upper: f64, target: f64) -> f64 {
    let count = weights.len();
    let width = upper - lower;
    let total = weights.iter().sum::<f64>();
    // Box widths the sum has to spend above the floor of the box.
    let raised = if width > 0.0 {
        ((target - count as f64 * lower) / width).clamp(0.0, count as f64)
    } else {
        0.0
    };
    let whole = raised.floor() as usize;
    if whole < count {
        weights.select_nth_unstable_by(whole, f64::total_cmp);
    }
    let spent = weights[..whole].iter().sum::<f64>()
        + weights
            .get(whole)
            .map_or(0.0, |next| (raised - whole as f64) * next);
    lower * total + width * spent
}

/// The void-area level each site is emitted at, as a squared radius.
///
/// Squared radius is what gets quantized because rounded-hex area is
/// proportional to it, so uniform levels are uniform area increments. Rounding
/// each site on its own would round a locally uniform field the same way
/// everywhere, moving its copper density by up to half a level's worth, so the
/// error of each rounding is handed to sites still to come and every
/// neighbourhood keeps the void area the solve gave it.
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

    fn panel_kernel(
        panel: &ContourSet,
        profile: DenseCopperBalanceProfile,
    ) -> (SiteTable, Vec<DenseCopperLatticeSite>, LatticeDensityKernel) {
        let lattice = DenseCopperLattice {
            origin: Point::ZERO,
            pitch_mm: profile.pitch_mm,
        };
        let sites = lattice.sites_covering(panel.bbox);
        let mut samples = SiteTable::spanning(sites.iter());
        for site in sites {
            if panel.contains_point(lattice.center(site)) {
                samples.admit(site);
            }
        }
        let evaluation = density_evaluation_sites(&samples.sites, profile);
        let kernel = LatticeDensityKernel::new(&samples, &evaluation, lattice, profile);
        (samples, evaluation, kernel)
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

    /// The cheapest feasible point is a vertex of the box, so spending the sum
    /// on the smallest weights in sorted order has to agree, whole widths,
    /// remainder and unreachable targets alike.
    #[test]
    fn least_inner_product_spends_the_sum_on_the_smallest_weights() {
        let weights = (0..23)
            .map(|index| ((index * 7919) % 101) as f64 / 101.0 - 0.4)
            .collect::<Vec<_>>();
        let (lower, upper) = (0.04, 0.4225);
        let sorted = {
            let mut sorted = weights.clone();
            sorted.sort_by(f64::total_cmp);
            sorted
        };
        for raised in [-3.0, 0.0, 0.25, 1.0, 7.5, 22.99, 23.0, 40.0_f64] {
            let target = weights.len() as f64 * lower + raised * (upper - lower);
            let mut left = raised.clamp(0.0, weights.len() as f64);
            let expected = sorted
                .iter()
                .map(|weight| {
                    let share = left.min(1.0);
                    left -= share;
                    weight * (lower + share * (upper - lower))
                })
                .sum::<f64>();
            let actual = least_inner_product(&mut weights.clone(), lower, upper, target);
            assert!((actual - expected).abs() <= 1e-12, "{raised}: {actual}");
        }
        // One level leaves the box a point.
        let pinned = least_inner_product(&mut weights.clone(), lower, lower, 1.0);
        assert!((pinned - lower * weights.iter().sum::<f64>()).abs() <= 1e-12);
        assert_eq!(least_inner_product(&mut [], lower, upper, 1.0), 0.0);
    }

    /// The solve stops on its own certificate, so the certificate has to be
    /// true: a solve held to a ten-thousandth of the floor may not find more
    /// than one floor's worth that the ordinary one left behind.
    #[test]
    fn redistribution_stops_within_the_rounding_floor_of_the_optimum() {
        let profile = DenseCopperBalanceProfile::V1;
        let panel = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(60.0, 40.0)),
            res(tol::REGION_MM),
        );
        let lattice = DenseCopperLattice {
            origin: Point::ZERO,
            pitch_mm: profile.pitch_mm,
        };
        let (samples, evaluation, kernel) = panel_kernel(&panel, profile);
        let active_sites = (0..samples.sites.len()).collect::<Vec<_>>();
        let cell_area_mm2 = lattice.column_pitch_mm() * lattice.pitch_mm;
        // A plane whose copper thins from left to right, with a bare patch.
        let model = LayerDensityModel {
            kernel: &kernel,
            active_sites: &active_sites,
            base_density: evaluation
                .iter()
                .map(|site| {
                    let center = lattice.center(*site);
                    1.0 - 0.3 * center.x / 60.0 - 0.4 * f64::from(center.y > 28.0)
                })
                .collect(),
            void_fraction_per_radius_squared: ROUNDED_HEXAGON_AREA_FACTOR / cell_area_mm2,
        };
        let (lower, upper) = (
            profile.min_void_radius_mm.powi(2),
            profile.max_void_radius_mm.powi(2),
        );
        let objective = |squared_radii: &[f64]| {
            model
                .density(squared_radii)
                .iter()
                .map(|density| (density - 0.5).powi(2) / 2.0)
                .sum::<f64>()
        };
        let floor = model.rounding_floor(
            &kernel.column_square_sums(),
            profile.void_area_level(1) - profile.void_area_level(0),
        );
        assert!(floor > 0.0);
        let start = vec![(lower + upper) / 2.0; active_sites.len()];
        let pinned_sum = start.iter().sum::<f64>();
        let step = 1.0 / (model.void_fraction_per_radius_squared.powi(2) * kernel.max_column_sum());
        let solve = |floor: f64| {
            let solved =
                model.redistribute(start.clone(), 0.5, (lower, upper), pinned_sum, step, floor);
            assert!(solved.iter().all(|value| (lower..=upper).contains(value)));
            assert!((solved.iter().sum::<f64>() - pinned_sum).abs() <= 1e-9 * pinned_sum);
            objective(&solved)
        };
        let (stopped, exhaustive) = (solve(floor), solve(floor * 1e-4));
        assert!(objective(&start) - stopped > 100.0 * floor);
        assert!(stopped >= exhaustive - 1e-12 && stopped - exhaustive <= floor);
    }

    /// A solved field that is locally uniform is the worst case for rounding:
    /// every site rounds the same way. Just under half a level off, rounding
    /// each site alone moves the copper density by a percent and a half
    /// everywhere, while handing the error on keeps the total void area to
    /// within the one site that has nowhere to hand it, and the smoothed
    /// density to a small fraction of the rounding bias.
    #[test]
    fn a_uniform_field_keeps_its_void_area_through_quantization() {
        let profile = DenseCopperBalanceProfile::V1;
        let panel = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(60.0, 40.0)),
            res(tol::REGION_MM),
        );
        let (samples, _, kernel) = panel_kernel(&panel, profile);
        let sites = &samples.sites;
        let spacing = profile.void_area_level(1) - profile.void_area_level(0);
        let solved = vec![profile.void_area_level(7) + 0.45 * spacing; sites.len()];
        let lattice = DenseCopperLattice {
            origin: Point::ZERO,
            pitch_mm: profile.pitch_mm,
        };
        let void_fraction_per_radius_squared =
            ROUNDED_HEXAGON_AREA_FACTOR / (lattice.column_pitch_mm() * lattice.pitch_mm);
        // Worst copper-density error at the scale the objective sees.
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

        let rounded = solved
            .iter()
            .map(|radius_squared| profile.nearest_void_area_level(*radius_squared))
            .collect::<Vec<_>>();
        assert!(
            density_error(&rounded) > 0.014,
            "{}",
            density_error(&rounded)
        );

        let diffused = diffuse_to_levels(sites, &solved, profile);
        let is_level = |value: f64| {
            (0..profile.void_area_levels).any(|level| value == profile.void_area_level(level))
        };
        assert!(diffused.iter().all(|value| is_level(*value)));
        let drift = diffused.iter().sum::<f64>() - solved.iter().sum::<f64>();
        assert!(drift.abs() <= spacing / 2.0, "{drift}");
        assert!(
            density_error(&diffused) < density_error(&rounded) / 20.0,
            "{}",
            density_error(&diffused)
        );
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

    /// The gradient step is the reciprocal of this bound, so the bound has to
    /// hold: no field may come out of the kernel with more energy than the
    /// largest column sum allows. It also has to be far tighter than treating
    /// the kernel as dense, or the step it licenses converges no faster.
    #[test]
    fn max_column_sum_bounds_the_kernel_operator_norm() {
        let profile = DenseCopperBalanceProfile::V1;
        let panel = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(60.0, 40.0)),
            res(tol::REGION_MM),
        );
        let (samples, _, kernel) = panel_kernel(&panel, profile);
        let bound = kernel.max_column_sum();
        assert!(bound > 0.0 && bound < 0.5, "{bound}");

        // Power iteration on `H^T H` climbs to the squared operator norm from
        // below, so every iterate has to respect the bound.
        let mut field = (0..samples.sites.len())
            .map(|index| 1.0 + ((index * 17 % 29) as f64) / 29.0)
            .collect::<Vec<_>>();
        for _ in 0..50 {
            let image = kernel.smooth_adjoint(&kernel.smooth(&field));
            let gain = image.iter().map(|v| v * v).sum::<f64>().sqrt()
                / field.iter().map(|v| v * v).sum::<f64>().sqrt();
            assert!(gain <= bound * (1.0 + 1e-12), "{gain} > {bound}");
            field = image;
        }
    }

    #[test]
    fn density_kernel_adjoint_matches_the_forward_operator() {
        let profile = DenseCopperBalanceProfile::V1;
        let panel = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(20.0, 12.0)),
            res(tol::REGION_MM),
        );
        let (samples, evaluation, kernel) = panel_kernel(&panel, profile);
        let source = (0..samples.sites.len())
            .map(|index| ((index * 17 % 29) as f64 - 14.0) / 29.0)
            .collect::<Vec<_>>();
        let residual = (0..evaluation.len())
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
