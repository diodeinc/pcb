//! Hexagonal void lattice: site enumeration and addressing, containment
//! classification, per-site activation radii, and the rounded-hex template.

use crate::geom::{AccuracyError, Resolution};
use std::f64::consts::PI;

use super::{
    DenseCopperBalanceProfile, DenseCopperLattice, DenseCopperLatticeSite, DenseCopperVoid, SQRT_3,
};
use crate::geom::accuracy::numerical_error;
use crate::geom::region::rings_bbox;
use crate::geom::shapes;
use crate::geom::{BBox, ContourBuf, ContourSet, FillRule, Point, tol};

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
    /// How far inside the voidable region each edge candidate's center lies,
    /// negative outside it: the radius of the largest disk about the center
    /// the region contains. Indexed like `edge_candidates`.
    pub(super) edge_center_depths_mm: Vec<f64>,
    /// Where the center of a minimum partial-void disk may sit: the voidable
    /// region eroded by that disk.
    pub(super) disk_center_region: ContourSet,
}

impl LatticeCandidates {
    pub(super) fn build_lattice(
        voidable: &ContourSet,
        origin: Point,
        profile: DenseCopperBalanceProfile,
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
                edge_center_depths_mm: Vec::new(),
                disk_center_region: ContourSet::empty(voidable.resolution),
            });
        }

        let candidate_region = voidable.disk_dilate(profile.max_void_radius_mm)?;
        let centers = hex_aligned_lattice_centers(candidate_region.bbox, origin, profile)
            .into_iter()
            .filter(|center| candidate_region.contains_point(*center))
            .collect::<Vec<_>>();
        let fully_contained = fully_contained_hexagons(voidable, &centers, profile)?;
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
        // A site's depth does not depend on the radius tried at it, so it is
        // measured once here for every clip and emission that asks.
        let edge_depths_mm = center_depths_mm(voidable, &edge_centers);
        let disk_center_region = voidable.disk_erode(profile.minimum_partial_void_inradius_mm())?;
        let activation_radii = minimum_partial_candidates(
            &disk_center_region,
            &edge_centers,
            &edge_depths_mm,
            profile,
        )?;
        let (edge_candidates, edge_center_depths_mm) = edge_centers
            .into_iter()
            .zip(activation_radii)
            .zip(edge_depths_mm)
            .filter_map(|((center, radius), depth_mm)| {
                let (column, row) = lattice_index(center, origin, profile);
                Some(((DenseCopperLatticeSite { column, row }, radius?), depth_mm))
            })
            .unzip();
        Ok(Self {
            lattice,
            full_sites,
            edge_candidates,
            edge_center_depths_mm,
            disk_center_region,
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
    ) -> Result<ContourSet, AccuracyError> {
        let candidates = self
            .lattice
            .void_candidates(&self.edge_voids(radius, profile));
        clipped_partial_voids(
            voidable,
            &self.disk_center_region,
            &candidates,
            &self.edge_center_depths_mm,
            profile,
        )
    }

    pub(super) fn void_area(
        &self,
        voidable: &ContourSet,
        radius: f64,
        profile: DenseCopperBalanceProfile,
    ) -> Result<f64, AccuracyError> {
        Ok(self.full_void_area(radius) + self.partial_voids(voidable, radius, profile)?.area())
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

/// The smallest radius at which each center's clipped void holds the minimum
/// disk, or `None` where no radius does.
fn minimum_partial_candidates(
    disk_center_region: &ContourSet,
    centers: &[Point],
    depths_mm: &[f64],
    profile: DenseCopperBalanceProfile,
) -> Result<Vec<Option<f64>>, AccuracyError> {
    let accuracy = disk_center_region.budget();
    if centers.is_empty() {
        return Ok(Vec::new());
    }

    let min_radius = profile.min_void_radius_mm;
    let max_radius = profile.max_void_radius_mm;
    let min_trials = uniform_candidates(centers, min_radius);
    let max_trials = uniform_candidates(centers, max_radius);
    let accepted_at_min =
        accepted_candidate_mask(disk_center_region, &min_trials, depths_mm, profile)?;
    let accepted_at_max =
        accepted_candidate_mask(disk_center_region, &max_trials, depths_mm, profile)?;
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
                (high - low > accuracy.max_error_mm()).then_some((
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
        let trial_depths_mm = trials
            .iter()
            .map(|(index, ..)| depths_mm[*index])
            .collect::<Vec<_>>();
        let accepted = accepted_candidate_mask(
            disk_center_region,
            &trial_geometry,
            &trial_depths_mm,
            profile,
        )?;
        for ((index, _, radius), accepted) in trials.into_iter().zip(accepted) {
            let (low, high) = bounds[index].as_mut().expect("trial has radius bounds");
            if accepted {
                *high = radius;
            } else {
                *low = radius;
            }
        }
    }

    Ok(bounds
        .into_iter()
        .map(|bounds| bounds.map(|(_, high)| high))
        .collect())
}

fn fully_contained_hexagons(
    voidable: &ContourSet,
    centers: &[Point],
    profile: DenseCopperBalanceProfile,
) -> Result<Vec<bool>, AccuracyError> {
    let candidates = uniform_candidates(centers, profile.max_void_radius_mm);
    let outside = hexagon_set_with_radii(&candidates, voidable.resolution)?.difference(voidable)?;
    let outside_points = representative_points(&outside);
    Ok(candidate_point_mask(
        &candidates,
        &outside_points,
        profile,
        outside.uncertainty_mm,
    )
    .into_iter()
    .map(|has_outside_point| !has_outside_point)
    .collect())
}

/// How far inside `region` each center lies, negative outside it.
fn center_depths_mm(region: &ContourSet, centers: &[Point]) -> Vec<f64> {
    let boundary = region.prepare_query();
    centers
        .iter()
        .map(|center| {
            boundary
                .signed_distance(*center)
                .map_or(f64::NEG_INFINITY, |distance| -distance.mm)
        })
        .collect()
}

fn uniform_candidates(centers: &[Point], radius: f64) -> Vec<(Point, f64)> {
    centers.iter().map(|center| (*center, radius)).collect()
}

/// Which candidates' clipped voids hold the minimum partial-void disk.
fn accepted_candidate_mask(
    disk_center_region: &ContourSet,
    candidates: &[(Point, f64)],
    depths_mm: &[f64],
    profile: DenseCopperBalanceProfile,
) -> Result<Vec<bool>, AccuracyError> {
    let (core_points, slack_mm) =
        minimum_disk_core_points(disk_center_region, candidates, depths_mm, profile)?;
    Ok(candidate_point_mask(
        candidates,
        &core_points,
        profile,
        slack_mm,
    ))
}

/// The clipped partial-void geometry the solver accounts with: components of
/// `hex ∩ voidable` that contain the minimum partial-void disk. `depths_mm`
/// is each candidate center's depth inside `voidable`.
pub(super) fn clipped_partial_voids(
    voidable: &ContourSet,
    disk_center_region: &ContourSet,
    candidates: &[(Point, f64)],
    depths_mm: &[f64],
    profile: DenseCopperBalanceProfile,
) -> Result<ContourSet, AccuracyError> {
    let raw = hexagon_set_with_radii(candidates, voidable.resolution)?.intersection(voidable)?;
    let (mut core_points, _) =
        minimum_disk_core_points(disk_center_region, candidates, depths_mm, profile)?;
    core_points.sort_by(|left, right| left.x.total_cmp(&right.x));
    let rings = raw
        .connected_components()
        .into_iter()
        .filter(|component| component_contains_any_point(component, &core_points))
        .flat_map(|component| component.rings)
        .collect();
    Ok(ContourSet::from_regularized(
        rings,
        raw.resolution,
        raw.uncertainty_mm,
    ))
}

/// The emitted form of the clipped partial voids: opened at the profile's
/// regularization radius to shed near-tangent clip tails, then decimated
/// inward to the arc-flattening tolerance.
pub(super) fn emission_partial_voids(
    voidable: &ContourSet,
    disk_center_region: &ContourSet,
    candidates: &[(Point, f64)],
    depths_mm: &[f64],
    profile: DenseCopperBalanceProfile,
) -> Result<ContourSet, AccuracyError> {
    let clipped =
        clipped_partial_voids(voidable, disk_center_region, candidates, depths_mm, profile)?
            .disk_open(profile.void_regularization_radius_mm())?;
    clipped.decimate_inward()
}

/// Points proving where the minimum partial-void disk fits inside a clipped
/// void, with the positional uncertainty of the region they were read from.
///
/// Those are the centers the disk may take: the clipped void eroded by the
/// disk. Erosion distributes over intersection, so that is the eroded hexagon
/// met with the eroded region — a closed-form template against one erosion
/// per region, where eroding the clipped voids themselves would offset every
/// fragment again for every radius tried.
fn minimum_disk_core_points(
    disk_center_region: &ContourSet,
    candidates: &[(Point, f64)],
    depths_mm: &[f64],
    profile: DenseCopperBalanceProfile,
) -> Result<(Vec<Point>, f64), AccuracyError> {
    let core = placed_templates(candidates, disk_center_region.resolution, |radius| {
        minimum_disk_centers(radius, profile)
    })?
    .intersection(disk_center_region)?;
    let mut points = representative_points(&core);
    // Preserve the exact equality case: a clipped void exactly one minimum
    // disk in diameter leaves a degenerate point or segment of centers that
    // falls below the ring-area floor, even though the disk itself fits.
    // Every hexagon holds that disk about its own center, so the center is a
    // core point wherever the region holds it too.
    let minimum_radius = profile.minimum_partial_void_inradius_mm();
    points.extend(
        candidates
            .iter()
            .zip(depths_mm)
            .filter_map(|((center, _), depth_mm)| {
                disk_fits(*depth_mm, minimum_radius, disk_center_region).then_some(*center)
            }),
    );
    Ok((points, core.uncertainty_mm))
}

/// Where the minimum partial-void disk's center may sit inside one rounded
/// hexagonal void at the origin: the void eroded by the disk.
///
/// The void is a sharp hexagon opened by its corner fillet, so eroding it by
/// no more than the fillet shrinks the fillet, and by more than the fillet
/// leaves the sharp hexagon whose flats have moved in by the disk radius.
fn minimum_disk_centers(radius: f64, profile: DenseCopperBalanceProfile) -> Option<ContourBuf> {
    let disk_radius = profile.minimum_partial_void_inradius_mm();
    let inset_radius = radius - 2.0 * disk_radius / SQRT_3;
    let fillet = radius * ROUNDED_HEXAGON_CORNER_RADIUS_RATIO - disk_radius;
    if fillet > 0.0 {
        shapes::rounded_hexagon(inset_radius, fillet, 0.0)
    } else {
        shapes::regular_polygon(2.0 * inset_radius, 6, 0.0)
    }
}

/// Whether a closed disk fits about a center `depth_mm` inside a region, to
/// the tolerance the region resolves containment at.
pub(super) fn disk_fits(depth_mm: f64, radius: f64, region: &ContourSet) -> bool {
    depth_mm + region.tolerance().max(tol::EPSILON_MM) >= radius
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
/// sharp hexagon. `slack_mm` is the positional uncertainty of the region the
/// points were read from, so a point that the boolean pipeline moved off a
/// hexagon edge still finds its owner.
fn candidate_point_mask(
    candidates: &[(Point, f64)],
    points: &[Point],
    profile: DenseCopperBalanceProfile,
    slack_mm: f64,
) -> Vec<bool> {
    let mut matched = vec![false; candidates.len()];
    let mut by_x = candidates
        .iter()
        .enumerate()
        .map(|(index, (center, radius))| (index, *center, *radius))
        .collect::<Vec<_>>();
    by_x.sort_by(|left, right| left.1.x.total_cmp(&right.1.x));
    let search_radius = profile.max_void_radius_mm + slack_mm;
    for point in points {
        let first = by_x.partition_point(|(_, center, _)| center.x < point.x - search_radius);
        let owner = by_x[first..]
            .iter()
            .take_while(|(_, center, _)| center.x <= point.x + search_radius)
            .find(|&&(_, center, radius)| hexagon_contains(center, radius, *point, slack_mm));
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

/// The union of rounded hexagonal voids on lattice sites.
pub(super) fn hexagon_set_with_radii(
    candidates: &[(Point, f64)],
    resolution: Resolution,
) -> Result<ContourSet, AccuracyError> {
    placed_templates(candidates, resolution, rounded_hexagonal_void)
}

/// The union of one origin-centered convex template per lattice site, each
/// sized by its site's void radius and no larger than that void.
///
/// The profile guarantees a positive web between neighboring voids, so the
/// placed templates are pairwise disjoint convex rings: already the
/// regularized form a union would return. Each distinct radius is flattened
/// once and its ring moved to every site that uses it.
fn placed_templates(
    candidates: &[(Point, f64)],
    resolution: Resolution,
    template: impl Fn(f64) -> Option<ContourBuf>,
) -> Result<ContourSet, AccuracyError> {
    let mut templates: Vec<(f64, ContourSet)> = Vec::new();
    let mut rings = Vec::with_capacity(candidates.len());
    for (center, radius) in candidates {
        let index = match templates.iter().position(|(known, _)| known == radius) {
            Some(index) => index,
            None => {
                let contour = template(*radius).expect("candidate radius is validated");
                let prepared =
                    ContourSet::from_contours(&[contour], FillRule::EvenOdd, resolution.strict())?;
                templates.push((*radius, prepared));
                templates.len() - 1
            }
        };
        rings.extend(templates[index].1.rings.iter().map(|ring| {
            ring.iter()
                .map(|[x, y]| [x + center.x, y + center.y])
                .collect::<Vec<_>>()
        }));
    }
    let uncertainty_mm = templates
        .iter()
        .map(|(_, template)| template.uncertainty_mm)
        .fold(0.0, f64::max)
        + numerical_error(rings_bbox(&rings));
    resolution.accuracy.check(uncertainty_mm)?;
    Ok(ContourSet::from_regularized(
        rings,
        resolution,
        uncertainty_mm,
    ))
}

/// One slightly rounded, flat-top regular hexagonal void centered at zero.
pub fn rounded_hexagonal_void(radius: f64) -> Option<ContourBuf> {
    shapes::rounded_hexagon(radius, radius * ROUNDED_HEXAGON_CORNER_RADIUS_RATIO, 0.0)
}

pub(super) fn void_set(
    voids: &[DenseCopperVoid],
    lattice: DenseCopperLattice,
    resolution: Resolution,
) -> Result<ContourSet, AccuracyError> {
    let candidates = voids
        .iter()
        .map(|void| (lattice.center(void.site), void.radius_mm))
        .collect::<Vec<_>>();
    hexagon_set_with_radii(&candidates, resolution)
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
    fn res(tolerance_mm: f64) -> Resolution {
        Resolution::default().with_tolerance(tolerance_mm)
    }

    use crate::geom::tol;
    use crate::geom::{BBox, ContourSet, PathOp, Point};

    #[test]
    fn rejects_partial_voids_that_cannot_hold_the_minimum_disk() {
        let profile = DenseCopperBalanceProfile::V1;
        let voidable = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(0.15, 10.0)),
            res(tol::REGION_MM),
        );
        let lattice =
            LatticeCandidates::build_lattice(&voidable, Point::new(0.0, 0.0), profile).unwrap();

        assert!(lattice.edge_candidates.is_empty());
    }

    #[test]
    fn exact_hex_containment_is_not_circumcircle_conservative() {
        let profile = DenseCopperBalanceProfile::V1;
        let voidable = ContourSet::rectangle(
            BBox::new(Point::new(-0.66, -0.57), Point::new(0.66, 0.57)),
            res(tol::REGION_MM),
        );
        let center = Point::new(0.0, 0.0);

        let depth_mm = center_depths_mm(&voidable, &[center])[0];
        assert!(!disk_fits(depth_mm, profile.max_void_radius_mm, &voidable));
        assert_eq!(
            fully_contained_hexagons(&voidable, &[center], profile).unwrap(),
            vec![true]
        );
    }

    /// A center's depth is its distance to the nearest boundary of any ring,
    /// holes included, signed by which side of it the center is on.
    #[test]
    fn center_depth_is_the_signed_distance_to_the_nearest_boundary() {
        let resolution = res(tol::REGION_MM);
        let plate = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(10.0, 6.0)),
            resolution,
        );
        let hole = ContourSet::rectangle(
            BBox::new(Point::new(4.0, 2.0), Point::new(6.0, 4.0)),
            resolution,
        );
        let region = plate.difference(&hole).unwrap();
        let centers = [
            Point::new(1.0, 3.0),
            Point::new(3.25, 3.0),
            Point::new(5.0, 3.0),
            Point::new(-0.5, 3.0),
        ];
        let depths_mm = center_depths_mm(&region, &centers);
        for (depth_mm, expected) in depths_mm.iter().zip([1.0, 0.75, -1.0, -0.5]) {
            assert!((depth_mm - expected).abs() <= 1e-12, "{depths_mm:?}");
        }
        assert!(disk_fits(depths_mm[1], 0.75, &region));
        assert!(!disk_fits(depths_mm[1], 0.76, &region));
        assert!(!disk_fits(depths_mm[2], 0.0, &region));
    }

    #[test]
    fn partial_void_activation_is_monotone_in_radius() {
        let profile = DenseCopperBalanceProfile::V1;
        let voidable = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(4.0, 4.0)),
            res(tol::REGION_MM),
        );
        let centers = hex_aligned_lattice_centers(
            voidable
                .disk_dilate(profile.max_void_radius_mm)
                .unwrap()
                .bbox,
            Point::new(0.0, 0.0),
            profile,
        );
        let depths_mm = center_depths_mm(&voidable, &centers);
        let disk_center_region = voidable
            .disk_erode(profile.minimum_partial_void_inradius_mm())
            .unwrap();
        let mut previously_accepted = vec![false; centers.len()];
        for radius in [0.20, 0.30, 0.40, 0.50, 0.60, 0.65] {
            let candidates = uniform_candidates(&centers, radius);
            let accepted =
                accepted_candidate_mask(&disk_center_region, &candidates, &depths_mm, profile)
                    .unwrap();
            assert!(
                previously_accepted
                    .iter()
                    .zip(&accepted)
                    .all(|(previous, current)| !previous || *current)
            );
            previously_accepted = accepted;
        }
    }

    /// The closed-form centers template has to be the void eroded by the
    /// minimum disk, whether the disk is smaller or larger than the fillet.
    #[test]
    fn minimum_disk_centers_are_the_eroded_void() {
        let resolution = res(tol::REGION_MM);
        for (radius, min_void_radius_mm) in [(0.65, 0.2), (0.2, 0.2), (2.0, 0.2), (2.0, 0.9)] {
            let profile = DenseCopperBalanceProfile {
                min_void_radius_mm,
                ..DenseCopperBalanceProfile::V1
            };
            let void = ContourSet::from_filled_contours(
                &[rounded_hexagonal_void(radius).unwrap()],
                resolution,
            )
            .unwrap();
            let eroded = void
                .disk_erode(profile.minimum_partial_void_inradius_mm())
                .unwrap();
            let centers = ContourSet::from_filled_contours(
                &[minimum_disk_centers(radius, profile).unwrap()],
                resolution,
            )
            .unwrap();
            let mismatch = eroded.difference(&centers).unwrap().area()
                + centers.difference(&eroded).unwrap().area();
            assert!(
                mismatch <= 1e-3 * eroded.area(),
                "radius {radius}, disk {}: {mismatch} of {}",
                profile.minimum_partial_void_inradius_mm(),
                eroded.area()
            );
        }
    }

    /// Eroding the hexagon and the region apart has to accept exactly the
    /// candidates that eroding each clipped void would, around convex and
    /// reflex boundary alike.
    #[test]
    fn distributed_erosion_accepts_what_eroding_each_clipped_void_accepts() {
        let profile = DenseCopperBalanceProfile::V1;
        let resolution = res(tol::REGION_MM);
        let plate = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(14.0, 9.0)),
            resolution,
        );
        let cutouts = ContourSet::from_filled_contours(
            &[
                shapes::circle(3.1)
                    .unwrap()
                    .transformed(crate::geom::Affine2::translation(Point::new(4.2, 4.4))),
                shapes::rect(2.3, 0.9)
                    .unwrap()
                    .transformed(crate::geom::Affine2::translation(Point::new(10.1, 5.2))),
            ],
            resolution,
        )
        .unwrap();
        let voidable = plate.difference(&cutouts).unwrap();
        let minimum_radius = profile.minimum_partial_void_inradius_mm();
        let disk_center_region = voidable.disk_erode(minimum_radius).unwrap();
        let centers = hex_aligned_lattice_centers(
            voidable
                .disk_dilate(profile.max_void_radius_mm)
                .unwrap()
                .bbox,
            Point::new(0.17, 0.31),
            profile,
        );
        let depths_mm = center_depths_mm(&voidable, &centers);

        let mut accepted_anywhere = 0;
        for radius in [0.20, 0.35, 0.50, 0.65] {
            let candidates = uniform_candidates(&centers, radius);
            let eroded = hexagon_set_with_radii(&candidates, resolution)
                .unwrap()
                .intersection(&voidable)
                .unwrap()
                .disk_erode(minimum_radius)
                .unwrap();
            let expected = candidate_point_mask(
                &candidates,
                &representative_points(&eroded),
                profile,
                eroded.uncertainty_mm,
            );
            let accepted =
                accepted_candidate_mask(&disk_center_region, &candidates, &depths_mm, profile)
                    .unwrap();
            // The equality case is granted on top of the erosion either way.
            let expected = expected
                .iter()
                .zip(&depths_mm)
                .map(|(eroded, depth_mm)| {
                    *eroded || disk_fits(*depth_mm, minimum_radius, &voidable)
                })
                .collect::<Vec<_>>();
            assert_eq!(accepted, expected, "radius {radius}");
            accepted_anywhere += accepted.iter().filter(|accepted| **accepted).count();
        }
        assert!(accepted_anywhere > 0);
    }

    /// Placing flattened templates has to give the region a union of the
    /// individually prepared hexagons gives.
    #[test]
    fn placed_templates_match_the_union_of_prepared_hexagons() {
        let profile = DenseCopperBalanceProfile::V1;
        let resolution = res(tol::REGION_MM);
        let bounds = BBox::new(Point::new(-3.0, -2.0), Point::new(9.0, 7.0));
        let radii = [0.2, 0.41, 0.65];
        let candidates = hex_aligned_lattice_centers(bounds, Point::new(0.3, -0.1), profile)
            .into_iter()
            .enumerate()
            .map(|(index, center)| (center, radii[index % radii.len()]))
            .collect::<Vec<_>>();
        let contours = candidates
            .iter()
            .map(|(center, radius)| {
                rounded_hexagonal_void(*radius)
                    .unwrap()
                    .transformed(crate::geom::Affine2::translation(*center))
            })
            .collect::<Vec<_>>();
        let unioned = ContourSet::from_filled_contours(&contours, resolution).unwrap();

        let placed = hexagon_set_with_radii(&candidates, resolution).unwrap();
        assert_eq!(placed.rings.len(), candidates.len());
        assert_eq!(placed.rings.len(), unioned.rings.len());
        assert!((placed.area() - unioned.area()).abs() <= 1e-9 * unioned.area());
        assert!(placed.difference(&unioned).unwrap().area() <= 1e-9);
        assert!(unioned.difference(&placed).unwrap().area() <= 1e-9);
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
                (arc.p0.distance_to(arc.p1) - radius * ROUNDED_HEXAGON_CORNER_RADIUS_RATIO).abs()
                    <= 1e-12
            );
        }

        let region = ContourSet::from_filled_contours(&[hexagon], res(tol::REGION_MM)).unwrap();
        let expected_area = ROUNDED_HEXAGON_AREA_FACTOR * radius.powi(2);
        assert!(
            (region.area() - expected_area).abs() <= expected_area * 2e-3,
            "geometric area {}, analytic area {}",
            region.area(),
            expected_area
        );
    }
}
