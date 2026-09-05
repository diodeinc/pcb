//! Hexagonal void lattice: site enumeration and addressing, containment
//! classification, per-site activation radii, and the rounded-hex template.

use crate::geom::{AccuracyError, GeometryAccuracy};
use std::f64::consts::PI;

use super::{
    DenseCopperBalanceProfile, DenseCopperLattice, DenseCopperLatticeSite, DenseCopperVoid, SQRT_3,
};
use crate::geom::shapes;
use crate::geom::{Affine2, BBox, ContourBuf, ContourSet, Point};

pub const ROUNDED_HEXAGON_CORNER_RADIUS_RATIO: f64 = 0.15;
// A sharp regular hexagon has area 3√3 R² / 2. Rounding each 120° corner
// inward by fillet radius kR removes (2√3 - π)k²R² across all six corners.
pub(super) const ROUNDED_HEXAGON_AREA_FACTOR: f64 = 3.0 * SQRT_3 / 2.0
    - (2.0 * SQRT_3 - PI)
        * ROUNDED_HEXAGON_CORNER_RADIUS_RATIO
        * ROUNDED_HEXAGON_CORNER_RADIUS_RATIO;

#[derive(Debug)]
pub(super) struct LatticeCandidates {
    pub(super) lattice: DenseCopperLattice,
    pub(super) full_sites: Vec<DenseCopperLatticeSite>,
    pub(super) edge_candidates: Vec<(DenseCopperLatticeSite, f64)>,
}

impl LatticeCandidates {
    pub(super) fn build_lattice(
        voidable: &ContourSet,
        origin: Point,
        profile: DenseCopperBalanceProfile,
        accuracy: GeometryAccuracy,
    ) -> Result<Self, AccuracyError> {
        let lattice = DenseCopperLattice {
            origin,
            pitch_mm: profile.pitch_mm,
        };
        if voidable.is_empty() {
            return Ok(Self {
                lattice,
                full_sites: Vec::new(),
                edge_candidates: Vec::new(),
            });
        }

        let candidate_region = voidable.disk_dilate(profile.max_void_radius_mm, accuracy)?;
        let centers = hex_aligned_lattice_centers(candidate_region.bbox, origin, profile)
            .into_iter()
            .filter(|center| candidate_region.contains_point(*center))
            .collect::<Vec<_>>();
        let fully_contained = fully_contained_hexagons(voidable, &centers, profile, accuracy)?;
        let mut full_sites = Vec::new();
        let mut edge_centers = Vec::new();
        for (center, full) in centers.into_iter().zip(fully_contained) {
            if full {
                let (column, row) = lattice_index(center, origin, profile);
                full_sites.push(DenseCopperLatticeSite { column, row });
            } else {
                edge_centers.push(center);
            }
        }
        let edge_candidates =
            minimum_partial_candidates(voidable, &edge_centers, profile, accuracy)?
                .into_iter()
                .map(|(center, radius)| {
                    let (column, row) = lattice_index(center, origin, profile);
                    (DenseCopperLatticeSite { column, row }, radius)
                })
                .collect();
        Ok(Self {
            lattice,
            full_sites,
            edge_candidates,
        })
    }

    pub(super) fn is_empty(&self) -> bool {
        self.full_sites.is_empty() && self.edge_candidates.is_empty()
    }

    fn full_void_area(&self, radius: f64) -> f64 {
        self.full_sites.len() as f64 * ROUNDED_HEXAGON_AREA_FACTOR * radius.powi(2)
    }

    pub(super) fn edge_voids(
        &self,
        radius: f64,
        profile: DenseCopperBalanceProfile,
    ) -> Vec<DenseCopperVoid> {
        self.edge_candidates
            .iter()
            .map(|(site, activation_radius)| DenseCopperVoid {
                site: *site,
                radius_mm: profile.quantize_void_radius_up(radius.max(*activation_radius)),
            })
            .collect()
    }

    pub(super) fn partial_voids(
        &self,
        voidable: &ContourSet,
        radius: f64,
        profile: DenseCopperBalanceProfile,
        accuracy: GeometryAccuracy,
    ) -> Result<ContourSet, AccuracyError> {
        let candidates = self
            .lattice
            .void_candidates(&self.edge_voids(radius, profile));
        clipped_partial_voids(voidable, &candidates, profile, accuracy)
    }

    pub(super) fn void_area(
        &self,
        voidable: &ContourSet,
        radius: f64,
        profile: DenseCopperBalanceProfile,
        accuracy: GeometryAccuracy,
    ) -> Result<f64, AccuracyError> {
        Ok(self.full_void_area(radius)
            + self
                .partial_voids(voidable, radius, profile, accuracy)?
                .area())
    }
}

pub(super) fn hex_aligned_lattice_centers(
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
    accuracy: GeometryAccuracy,
) -> Result<Vec<(Point, f64)>, AccuracyError> {
    if centers.is_empty() {
        return Ok(Vec::new());
    }

    let min_radius = profile.min_void_radius_mm;
    let max_radius = profile.max_void_radius_mm;
    let min_trials = uniform_candidates(centers, min_radius);
    let max_trials = uniform_candidates(centers, max_radius);
    let accepted_at_min = accepted_candidate_mask(voidable, &min_trials, profile, accuracy)?;
    let accepted_at_max = accepted_candidate_mask(voidable, &max_trials, profile, accuracy)?;
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
                (high - low > accuracy.max_error_mm() / 4.0).then_some((
                    index,
                    center,
                    (low + high) / 2.0,
                ))
            })
            .collect::<Vec<_>>();
        if trials.is_empty() {
            break;
        }
        let trial_geometry = trials
            .iter()
            .map(|(_, center, radius)| (*center, *radius))
            .collect::<Vec<_>>();
        let accepted = accepted_candidate_mask(voidable, &trial_geometry, profile, accuracy)?;
        for ((index, _, radius), accepted) in trials.into_iter().zip(accepted) {
            let (low, high) = bounds[index].as_mut().expect("trial has radius bounds");
            if accepted {
                *high = radius;
            } else {
                *low = radius;
            }
        }
    }

    Ok(centers
        .iter()
        .copied()
        .zip(bounds)
        .filter_map(|(center, bounds)| bounds.map(|(_, high)| (center, high)))
        .collect())
}

fn fully_contained_hexagons(
    voidable: &ContourSet,
    centers: &[Point],
    profile: DenseCopperBalanceProfile,
    accuracy: GeometryAccuracy,
) -> Result<Vec<bool>, AccuracyError> {
    let candidates = uniform_candidates(centers, profile.max_void_radius_mm);
    let outside =
        hexagon_set_with_radii(&candidates, voidable.tolerance, accuracy)?.difference(voidable);
    let outside_points = representative_points(&outside);
    Ok(
        candidate_point_mask(&candidates, &outside_points, profile, accuracy)
            .into_iter()
            .map(|has_outside_point| !has_outside_point)
            .collect(),
    )
}

fn uniform_candidates(centers: &[Point], radius: f64) -> Vec<(Point, f64)> {
    centers.iter().map(|center| (*center, radius)).collect()
}

fn accepted_candidate_mask(
    voidable: &ContourSet,
    candidates: &[(Point, f64)],
    profile: DenseCopperBalanceProfile,
    accuracy: GeometryAccuracy,
) -> Result<Vec<bool>, AccuracyError> {
    let raw =
        hexagon_set_with_radii(candidates, voidable.tolerance, accuracy)?.intersection(voidable);
    let core_points = minimum_disk_core_points(&raw, voidable, candidates, profile, accuracy)?;
    Ok(candidate_point_mask(
        candidates,
        &core_points,
        profile,
        accuracy,
    ))
}

/// The clipped partial-void geometry the solver accounts with: components of
/// `hex ∩ voidable` that contain the minimum partial-void disk.
pub(super) fn clipped_partial_voids(
    voidable: &ContourSet,
    candidates: &[(Point, f64)],
    profile: DenseCopperBalanceProfile,
    accuracy: GeometryAccuracy,
) -> Result<ContourSet, AccuracyError> {
    let raw =
        hexagon_set_with_radii(candidates, voidable.tolerance, accuracy)?.intersection(voidable);
    let mut core_points = minimum_disk_core_points(&raw, voidable, candidates, profile, accuracy)?;
    core_points.sort_by(|left, right| left.x.total_cmp(&right.x));
    let rings = raw
        .connected_components()
        .into_iter()
        .filter(|component| component_contains_any_point(component, &core_points))
        .flat_map(|component| component.rings)
        .collect();
    Ok(ContourSet::from_regularized(
        rings,
        voidable.tolerance,
        raw.uncertainty_mm,
    ))
}

/// The emitted form of the clipped partial voids: opened at the profile's
/// regularization radius to shed near-tangent clip tails, then decimated
/// inward to the arc-flattening tolerance.
pub(super) fn emission_partial_voids(
    voidable: &ContourSet,
    candidates: &[(Point, f64)],
    profile: DenseCopperBalanceProfile,
    accuracy: GeometryAccuracy,
) -> Result<ContourSet, AccuracyError> {
    clipped_partial_voids(voidable, candidates, profile, accuracy)?
        .disk_open(profile.void_regularization_radius_mm(), accuracy)?
        .decimate_inward(accuracy.max_error_mm(), accuracy)
}

fn minimum_disk_core_points(
    raw: &ContourSet,
    voidable: &ContourSet,
    candidates: &[(Point, f64)],
    profile: DenseCopperBalanceProfile,
    accuracy: GeometryAccuracy,
) -> Result<Vec<Point>, AccuracyError> {
    let minimum_radius = profile.minimum_partial_void_inradius_mm();
    let mut points = representative_points(&raw.disk_erode(minimum_radius, accuracy)?);
    // Preserve the exact equality case: a clipped void exactly one minimum
    // disk in diameter erodes to a degenerate point or segment that falls
    // below the ring-area floor, even though the disk itself fits.
    points.extend(candidates.iter().filter_map(|(center, _)| {
        voidable
            .contains_disk(*center, minimum_radius)
            .then_some(*center)
    }));
    Ok(points)
}

fn representative_points(region: &ContourSet) -> Vec<Point> {
    region
        .connected_components()
        .into_iter()
        .filter_map(|component| component.rings.first()?.first().copied())
        .map(|[x, y]| Point::new(x, y))
        .collect()
}

/// Mark each candidate whose hexagon contains one of the points.
///
/// The candidate hexagons are pairwise disjoint, so hexagon membership
/// identifies the unique owner of every point produced by boolean operations
/// over their union — corner rounding only removes area strictly inside the
/// sharp hexagon.
fn candidate_point_mask(
    candidates: &[(Point, f64)],
    points: &[Point],
    profile: DenseCopperBalanceProfile,
    accuracy: GeometryAccuracy,
) -> Vec<bool> {
    let mut matched = vec![false; candidates.len()];
    let mut by_x = candidates
        .iter()
        .enumerate()
        .map(|(index, (center, radius))| (index, *center, *radius))
        .collect::<Vec<_>>();
    by_x.sort_by(|left, right| left.1.x.total_cmp(&right.1.x));
    let search_radius = profile.max_void_radius_mm + accuracy.max_error_mm() / 4.0;
    for point in points {
        let first = by_x.partition_point(|(_, center, _)| center.x < point.x - search_radius);
        let owner = by_x[first..]
            .iter()
            .take_while(|(_, center, _)| center.x <= point.x + search_radius)
            .find(|&&(_, center, radius)| {
                hexagon_contains(center, radius, *point, accuracy.max_error_mm() / 4.0)
            });
        if let Some((index, _, _)) = owner {
            matched[*index] = true;
        }
    }
    matched
}

/// Whether the sharp flat-top hexagon of circumradius `radius` centered at
/// `center` contains `point`, within `tolerance`.
fn hexagon_contains(center: Point, radius: f64, point: Point, tolerance: f64) -> bool {
    let delta = point - center;
    // The three flat-pair normals of a flat-top hexagon are at 30°, 90°, and
    // 150°; each flat lies one apothem from the center.
    let axis = |x: f64, y: f64| (delta.x * x + delta.y * y).abs();
    let reach = axis(SQRT_3 / 2.0, 0.5)
        .max(axis(0.0, 1.0))
        .max(axis(SQRT_3 / 2.0, -0.5));
    reach <= radius * SQRT_3 / 2.0 + tolerance
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

pub(super) fn hexagon_set_with_radii(
    candidates: &[(Point, f64)],
    tolerance: f64,
    accuracy: GeometryAccuracy,
) -> Result<ContourSet, AccuracyError> {
    let contours = candidates
        .iter()
        .map(|(center, radius)| {
            let hexagon = rounded_hexagonal_void(*radius).expect("candidate radius is validated");
            hexagon.transformed(Affine2::translation(*center), accuracy)
        })
        .collect::<Result<Vec<_>, _>>()?;
    ContourSet::from_filled_contours(&contours, tolerance, accuracy)
}

/// One slightly rounded, flat-top regular hexagonal void centered at zero.
pub fn rounded_hexagonal_void(radius: f64) -> Option<ContourBuf> {
    shapes::rounded_hexagon(radius, radius * ROUNDED_HEXAGON_CORNER_RADIUS_RATIO, 0.0)
}

pub(super) fn void_set(
    voids: &[DenseCopperVoid],
    lattice: DenseCopperLattice,
    tolerance: f64,
    accuracy: GeometryAccuracy,
) -> Result<ContourSet, AccuracyError> {
    let candidates = voids
        .iter()
        .map(|void| (lattice.center(void.site), void.radius_mm))
        .collect::<Vec<_>>();
    hexagon_set_with_radii(&candidates, tolerance, accuracy)
}

pub(super) fn lattice_index(
    point: Point,
    origin: Point,
    profile: DenseCopperBalanceProfile,
) -> (i64, i64) {
    let column = ((point.x - origin.x) / profile.lattice_column_pitch_mm()).round() as i64;
    let column_offset = column.rem_euclid(2) as f64 * profile.pitch_mm / 2.0;
    let row = ((point.y - origin.y - column_offset) / profile.pitch_mm).round() as i64;
    (column, row)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::tol;
    use crate::geom::{BBox, ContourSet, PathOp, Point};

    #[test]
    fn rejects_partial_voids_that_cannot_hold_the_minimum_disk() {
        let profile = DenseCopperBalanceProfile::V1;
        let voidable = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(0.15, 10.0)),
            tol::REGION_MM,
        );
        let lattice = LatticeCandidates::build_lattice(
            &voidable,
            Point::new(0.0, 0.0),
            profile,
            GeometryAccuracy::default(),
        )
        .unwrap();

        assert!(lattice.edge_candidates.is_empty());
    }

    #[test]
    fn exact_hex_containment_is_not_circumcircle_conservative() {
        let accuracy = GeometryAccuracy::default();

        let profile = DenseCopperBalanceProfile::V1;
        let voidable = ContourSet::rectangle(
            BBox::new(Point::new(-0.66, -0.57), Point::new(0.66, 0.57)),
            tol::REGION_MM,
        );
        let center = Point::new(0.0, 0.0);

        assert!(!voidable.contains_disk(center, profile.max_void_radius_mm));
        assert_eq!(
            fully_contained_hexagons(&voidable, &[center], profile, accuracy).unwrap(),
            vec![true]
        );
    }

    #[test]
    fn partial_void_activation_is_monotone_in_radius() {
        let accuracy = GeometryAccuracy::default();

        let profile = DenseCopperBalanceProfile::V1;
        let voidable = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(4.0, 4.0)),
            tol::REGION_MM,
        );
        let centers = hex_aligned_lattice_centers(
            voidable
                .disk_dilate(profile.max_void_radius_mm, accuracy)
                .unwrap()
                .bbox,
            Point::new(0.0, 0.0),
            profile,
        );
        let mut previously_accepted = vec![false; centers.len()];
        for radius in [0.20, 0.30, 0.40, 0.50, 0.60, 0.65] {
            let candidates = uniform_candidates(&centers, radius);
            let accepted =
                accepted_candidate_mask(&voidable, &candidates, profile, accuracy).unwrap();
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
    fn rounded_hexagon_uses_six_scaled_corner_arcs_and_tracks_analytic_area() {
        let accuracy = GeometryAccuracy::default();

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
                (arc.p0.distance_to(arc.p1) - radius * ROUNDED_HEXAGON_CORNER_RADIUS_RATIO).abs()
                    <= 1e-12
            );
        }

        let region =
            ContourSet::from_filled_contours(&[hexagon], tol::REGION_MM, accuracy).unwrap();
        let expected_area = ROUNDED_HEXAGON_AREA_FACTOR * radius.powi(2);
        assert!(
            (region.area() - expected_area).abs() <= expected_area * 2e-3,
            "geometric area {}, analytic area {}",
            region.area(),
            expected_area
        );
    }
}
