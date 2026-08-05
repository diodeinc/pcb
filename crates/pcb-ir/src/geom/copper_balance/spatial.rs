//! Spatial copper-redistribution machinery: normalized lattice convolution,
//! stratified cell-coverage sampling, and sum-preserving box projection.

use std::collections::HashMap;

use super::lattice::{ROUNDED_HEXAGON_AREA_FACTOR, lattice_index};
use super::{
    DenseCopperBalanceMode, DenseCopperBalanceProfile, DenseCopperBalanceResult,
    DenseCopperBalanceSolution, DenseCopperVoid, NUMERIC_EPSILON, SpatialCopperBalanceLayerRequest,
};
use crate::geom::{ContourSet, Point};

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
/// evaluation field. `smooth_adjoint` is the exact transpose of `smooth`, so
/// projected gradient follows the same density model used by the objective.
pub(super) struct LatticeDensityKernel {
    sample_count: usize,
    rows: Vec<Vec<(usize, f64)>>,
}

impl LatticeDensityKernel {
    pub(super) fn new(
        samples: &[Point],
        evaluation_points: &[Point],
        origin: Point,
        profile: DenseCopperBalanceProfile,
    ) -> Self {
        let index = |point| lattice_index(point, origin, profile);
        let sample_indices = samples
            .iter()
            .enumerate()
            .map(|(sample_index, point)| (index(*point), sample_index))
            .collect::<HashMap<_, _>>();
        let evaluation_sites = evaluation_points
            .iter()
            .map(|point| index(*point))
            .collect::<Vec<_>>();
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
                .collect::<Vec<(i64, i64, f64)>>()
        });
        let rows = evaluation_sites
            .iter()
            .map(|&(column, row)| {
                let mut neighbors = offsets[column.rem_euclid(2) as usize]
                    .iter()
                    .filter_map(|&(column_offset, row_offset, weight)| {
                        let sample_index =
                            sample_indices.get(&(column + column_offset, row + row_offset))?;
                        Some((*sample_index, weight))
                    })
                    .collect::<Vec<_>>();
                let weight_sum = neighbors.iter().map(|(_, weight)| weight).sum::<f64>();
                if weight_sum > 0.0 {
                    for (_, weight) in &mut neighbors {
                        *weight /= weight_sum;
                    }
                }
                neighbors
            })
            .collect();
        Self {
            sample_count: samples.len(),
            rows,
        }
    }

    pub(super) fn smooth(&self, values: &[f64]) -> Vec<f64> {
        debug_assert_eq!(values.len(), self.sample_count);
        self.rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|(sample_index, weight)| weight * values[*sample_index])
                    .sum()
            })
            .collect()
    }

    pub(super) fn smooth_adjoint(&self, values: &[f64]) -> Vec<f64> {
        debug_assert_eq!(values.len(), self.rows.len());
        let mut result = vec![0.0; self.sample_count];
        for (value, row) in values.iter().zip(&self.rows) {
            for (sample_index, weight) in row {
                result[*sample_index] += weight * value;
            }
        }
        result
    }
}

/// Sample the 5 mm-scale objective on a deterministic subset of the 1.35 mm
/// fabrication lattice. Geometry and output stay on the full lattice.
pub(super) fn density_evaluation_points(
    samples: &[Point],
    origin: Point,
    profile: DenseCopperBalanceProfile,
) -> Vec<Point> {
    let stride = (profile.density_sigma_mm / profile.pitch_mm)
        .round()
        .max(1.0) as i64;
    // Anchoring the coarse grid on the first sample keeps the result nonempty
    // for every nonempty input.
    let (anchor_column, anchor_row) = lattice_index(samples[0], origin, profile);
    samples
        .iter()
        .copied()
        .filter(|point| {
            let (column, row) = lattice_index(*point, origin, profile);
            (column - anchor_column).rem_euclid(stride) == 0
                && (row - anchor_row).rem_euclid(stride) == 0
        })
        .collect()
}

/// Fraction of each site's rectangular lattice tile covered by `region`.
///
/// The staggered columns tile the plane exactly with column-pitch × pitch
/// rectangles centered on the sites, so stratified subsamples of every tile
/// estimate local density without aliasing sub-pitch geometry to zero or one.
pub(super) fn lattice_cell_coverage(
    points: &[Point],
    region: &ContourSet,
    profile: DenseCopperBalanceProfile,
) -> Vec<f64> {
    const STRATA: usize = 3;
    let offset = |index: usize, span: f64| ((index as f64 + 0.5) / STRATA as f64 - 0.5) * span;
    let mut subsamples = Vec::with_capacity(points.len() * STRATA * STRATA);
    for point in points {
        for row in 0..STRATA {
            for column in 0..STRATA {
                subsamples.push(Point::new(
                    point.x + offset(column, profile.lattice_column_pitch_mm()),
                    point.y + offset(row, profile.pitch_mm),
                ));
            }
        }
    }
    scanline_indicator(&subsamples, region)
        .chunks_exact(STRATA * STRATA)
        .map(|tile| tile.iter().sum::<f64>() / (STRATA * STRATA) as f64)
        .collect()
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

pub(super) fn project_box_sum(
    values: &[f64],
    lower: &[f64],
    upper: &[f64],
    target: f64,
) -> Vec<f64> {
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

pub(super) fn spatial_result_from_squared_radii(
    centers: &[Point],
    squared_radii: &[f64],
    baseline: DenseCopperBalanceResult,
    layer: SpatialCopperBalanceLayerRequest<'_>,
    retained_area_mm2: f64,
    profile: DenseCopperBalanceProfile,
) -> DenseCopperBalanceResult {
    // Snap the continuous radius field to the fabrication step so the voids
    // form a small set of repeated templates; round-to-nearest keeps the
    // aggregate area error stochastic and far below the solve tolerance.
    let quantize = |radius_squared: f64| {
        ((radius_squared.sqrt() / profile.void_radius_step_mm).round()
            * profile.void_radius_step_mm)
            .clamp(profile.min_void_radius_mm, profile.max_void_radius_mm)
    };
    let full_voids = centers
        .iter()
        .zip(squared_radii)
        .map(|(center, radius_squared)| DenseCopperVoid {
            center: *center,
            radius_mm: quantize(*radius_squared),
        })
        .collect::<Vec<_>>();
    let full_void_area_mm2 = ROUNDED_HEXAGON_AREA_FACTOR
        * full_voids
            .iter()
            .map(|void| void.radius_mm * void.radius_mm)
            .sum::<f64>();
    let partial_voids = baseline.partial_voids;
    let void_area_mm2 = full_void_area_mm2 + partial_voids.area();
    let generated_area_mm2 = (baseline.usable.area() - void_area_mm2).max(0.0);
    let achieved_density = (layer.existing_copper.area() + generated_area_mm2) / retained_area_mm2;
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
        usable: baseline.usable,
        full_voids,
        partial_voids,
    }
}

#[cfg(test)]
mod tests {
    use super::super::lattice::hex_aligned_lattice_centers;
    use super::*;
    use crate::geom::{BBox, ContourSet, Point, tol};

    #[test]
    fn density_kernel_adjoint_matches_the_forward_operator() {
        let profile = DenseCopperBalanceProfile::V1;
        let panel = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(20.0, 12.0)),
            tol::REGION_MM,
        );
        let samples = hex_aligned_lattice_centers(panel.bbox, Point::ZERO, profile)
            .into_iter()
            .filter(|point| panel.contains_point(*point))
            .collect::<Vec<_>>();
        let evaluation = density_evaluation_points(&samples, Point::ZERO, profile);
        let kernel = LatticeDensityKernel::new(&samples, &evaluation, Point::ZERO, profile);
        let source = (0..samples.len())
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
