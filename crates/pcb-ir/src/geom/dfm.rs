//! Geometry measurements shared by manufacturability checks.

use std::collections::HashMap;

use crate::geom::point::Point;
use crate::geom::region::{ContourSet, Ring};
use crate::geom::tol;

/// The shortest separation between two pieces of geometry, with the points
/// that realize it. Distances are expressed in the IR's canonical millimeters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClearanceMeasurement {
    pub distance_mm: f64,
    pub first: Point,
    pub second: Point,
}

/// The minimum radial material enclosure around a circular cutout.
///
/// `enclosure_mm` is signed. A positive value is material outside the cutout;
/// a negative value is the amount by which the cutout breaches the material.
/// The witness points identify the cutout edge and material boundary that
/// realize the measurement when the cutout center lies in material.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CircularEnclosureMeasurement {
    pub enclosure_mm: f64,
    pub cutout_boundary: Point,
    pub material_boundary: Option<Point>,
    pub center_in_material: bool,
}

/// Result of testing a circular cutout against a minimum radial enclosure.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CircularEnclosureCheck {
    Satisfied,
    Violated(CircularEnclosureMeasurement),
}

/// Uniform-grid index over a region's boundary segments.
///
/// DFM rules commonly ask whether thousands of points lie within a small
/// clearance of the same composed layer boundary. This keeps those queries
/// local instead of walking every polygon edge for every point.
#[derive(Debug, Clone)]
pub struct RegionBoundaryIndex {
    cell_size_mm: f64,
    segments: Vec<(Point, Point)>,
    cells: HashMap<(i64, i64), Vec<usize>>,
    seen: Vec<u32>,
    generation: u32,
}

impl RegionBoundaryIndex {
    pub fn new(region: &ContourSet, cell_size_mm: f64) -> Option<Self> {
        if !(cell_size_mm.is_finite() && cell_size_mm > 0.0) {
            return None;
        }
        let segments = region.rings.iter().flat_map(ring_edges).collect::<Vec<_>>();
        let mut cells: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
        for (index, &(start, end)) in segments.iter().enumerate() {
            let min_x = grid_cell(start.x.min(end.x), cell_size_mm);
            let max_x = grid_cell(start.x.max(end.x), cell_size_mm);
            let min_y = grid_cell(start.y.min(end.y), cell_size_mm);
            let max_y = grid_cell(start.y.max(end.y), cell_size_mm);
            for x in min_x..=max_x {
                for y in min_y..=max_y {
                    cells.entry((x, y)).or_default().push(index);
                }
            }
        }
        Some(Self {
            cell_size_mm,
            seen: vec![0; segments.len()],
            segments,
            cells,
            generation: 0,
        })
    }

    /// Nearest boundary point no farther than `max_distance_mm` from `point`.
    pub fn nearest_within(
        &mut self,
        point: Point,
        max_distance_mm: f64,
    ) -> Option<ClearanceMeasurement> {
        if !(max_distance_mm.is_finite() && max_distance_mm >= 0.0) {
            return None;
        }
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            self.seen.fill(0);
            self.generation = 1;
        }
        let generation = self.generation;
        let min_x = grid_cell(point.x - max_distance_mm, self.cell_size_mm);
        let max_x = grid_cell(point.x + max_distance_mm, self.cell_size_mm);
        let min_y = grid_cell(point.y - max_distance_mm, self.cell_size_mm);
        let max_y = grid_cell(point.y + max_distance_mm, self.cell_size_mm);
        let mut nearest: Option<ClearanceMeasurement> = None;
        for x in min_x..=max_x {
            for y in min_y..=max_y {
                let Some(indices) = self.cells.get(&(x, y)) else {
                    continue;
                };
                for &index in indices {
                    if self.seen[index] == generation {
                        continue;
                    }
                    self.seen[index] = generation;
                    let (start, end) = self.segments[index];
                    let candidate = point_segment_clearance(point, start, end, true);
                    if candidate.distance_mm > max_distance_mm + tol::EPSILON_MM {
                        continue;
                    }
                    if nearest.is_none_or(|current| candidate.distance_mm < current.distance_mm) {
                        nearest = Some(candidate);
                    }
                }
            }
        }
        nearest
    }

    /// Test whether material contains a circular cutout plus its required
    /// radial enclosure.
    ///
    /// This is the indexed form of a morphological containment test. A disk
    /// of radius `cutout_radius_mm + minimum_enclosure_mm` is contained by the
    /// material exactly when its center is in the material and the nearest
    /// material boundary is at least that far from the center. The returned
    /// violation includes the signed enclosure and geometric witnesses.
    ///
    /// `tolerance_mm` is subtracted from the required enclosure to absorb the
    /// polygon-flattening error of the source artwork. Invalid or negative
    /// inputs return `None`.
    pub fn check_circular_enclosure(
        &mut self,
        center: Point,
        cutout_radius_mm: f64,
        minimum_enclosure_mm: f64,
        tolerance_mm: f64,
        center_in_material: bool,
    ) -> Option<CircularEnclosureCheck> {
        if !center.is_finite()
            || !(cutout_radius_mm.is_finite() && cutout_radius_mm >= 0.0)
            || !(minimum_enclosure_mm.is_finite() && minimum_enclosure_mm >= 0.0)
            || !(tolerance_mm.is_finite() && tolerance_mm >= 0.0)
        {
            return None;
        }

        let required_center_clearance =
            (cutout_radius_mm + minimum_enclosure_mm - tolerance_mm).max(0.0);
        if !center_in_material {
            let material_boundary = self
                .nearest_within(center, required_center_clearance)
                .map(|measurement| measurement.second);
            return Some(CircularEnclosureCheck::Violated(
                CircularEnclosureMeasurement {
                    // With no material at the center, no positive radial
                    // enclosure exists in every direction around the cutout.
                    enclosure_mm: -cutout_radius_mm,
                    cutout_boundary: center,
                    material_boundary,
                    center_in_material: false,
                },
            ));
        }

        let Some(nearest) = self.nearest_within(center, required_center_clearance) else {
            return Some(CircularEnclosureCheck::Satisfied);
        };
        let enclosure_mm = nearest.distance_mm - cutout_radius_mm;
        if enclosure_mm + tolerance_mm >= minimum_enclosure_mm {
            return Some(CircularEnclosureCheck::Satisfied);
        }

        let direction = nearest.second - center;
        let direction = if direction.length() <= f64::EPSILON {
            Point::new(1.0, 0.0)
        } else {
            direction / direction.length()
        };
        Some(CircularEnclosureCheck::Violated(
            CircularEnclosureMeasurement {
                enclosure_mm,
                cutout_boundary: center + direction * cutout_radius_mm,
                material_boundary: Some(nearest.second),
                center_in_material: true,
            },
        ))
    }
}

fn grid_cell(value: f64, cell_size_mm: f64) -> i64 {
    (value / cell_size_mm).floor() as i64
}

/// Shortest boundary-to-boundary clearance between two filled regions.
///
/// Overlapping, contained, or touching regions have zero clearance. `None`
/// means at least one input is empty.
pub fn region_clearance(first: &ContourSet, second: &ContourSet) -> Option<ClearanceMeasurement> {
    if first.is_empty() || second.is_empty() {
        return None;
    }

    if let Some(point) = first
        .rings
        .iter()
        .flat_map(|ring| ring.iter())
        .map(|&[x, y]| Point::new(x, y))
        .find(|&point| second.contains_point(point))
        .or_else(|| {
            second
                .rings
                .iter()
                .flat_map(|ring| ring.iter())
                .map(|&[x, y]| Point::new(x, y))
                .find(|&point| first.contains_point(point))
        })
    {
        return Some(ClearanceMeasurement {
            distance_mm: 0.0,
            first: point,
            second: point,
        });
    }

    closest_ring_edges(&first.rings, &second.rings)
}

/// Shortest clearance from a line segment to a filled region.
///
/// This is useful for checks whose reference is a tool centerline, such as a
/// V-score. A segment that touches or passes through the region returns zero.
pub fn segment_region_clearance(
    start: Point,
    end: Point,
    region: &ContourSet,
) -> Option<ClearanceMeasurement> {
    if region.is_empty() {
        return None;
    }
    if region.contains_point(start) || region.contains_point(end) {
        let point = if region.contains_point(start) {
            start
        } else {
            end
        };
        return Some(ClearanceMeasurement {
            distance_mm: 0.0,
            first: point,
            second: point,
        });
    }

    region
        .rings
        .iter()
        .flat_map(ring_edges)
        .map(|(edge_start, edge_end)| segment_clearance(start, end, edge_start, edge_end))
        .min_by(|left, right| left.distance_mm.total_cmp(&right.distance_mm))
}

fn closest_ring_edges(first: &[Ring], second: &[Ring]) -> Option<ClearanceMeasurement> {
    first
        .iter()
        .flat_map(ring_edges)
        .flat_map(|(first_start, first_end)| {
            second.iter().flat_map(move |ring| {
                ring_edges(ring).map(move |(second_start, second_end)| {
                    segment_clearance(first_start, first_end, second_start, second_end)
                })
            })
        })
        .min_by(|left, right| left.distance_mm.total_cmp(&right.distance_mm))
}

fn ring_edges(ring: &Ring) -> impl Iterator<Item = (Point, Point)> + '_ {
    ring.iter()
        .copied()
        .zip(ring.iter().copied().cycle().skip(1))
        .take(ring.len())
        .map(|([x0, y0], [x1, y1])| (Point::new(x0, y0), Point::new(x1, y1)))
}

fn segment_clearance(
    first_start: Point,
    first_end: Point,
    second_start: Point,
    second_end: Point,
) -> ClearanceMeasurement {
    if segments_intersect(first_start, first_end, second_start, second_end) {
        let point = segment_intersection(first_start, first_end, second_start, second_end)
            .unwrap_or(first_start);
        return ClearanceMeasurement {
            distance_mm: 0.0,
            first: point,
            second: point,
        };
    }

    [
        point_segment_clearance(first_start, second_start, second_end, true),
        point_segment_clearance(first_end, second_start, second_end, true),
        point_segment_clearance(second_start, first_start, first_end, false),
        point_segment_clearance(second_end, first_start, first_end, false),
    ]
    .into_iter()
    .min_by(|left, right| left.distance_mm.total_cmp(&right.distance_mm))
    .expect("four segment endpoint candidates")
}

fn point_segment_clearance(
    point: Point,
    start: Point,
    end: Point,
    point_is_first: bool,
) -> ClearanceMeasurement {
    let delta = end - start;
    let length_squared = delta.x * delta.x + delta.y * delta.y;
    let projection = if length_squared <= f64::EPSILON {
        start
    } else {
        let t = (((point.x - start.x) * delta.x + (point.y - start.y) * delta.y) / length_squared)
            .clamp(0.0, 1.0);
        start + delta * t
    };
    let (first, second) = if point_is_first {
        (point, projection)
    } else {
        (projection, point)
    };
    ClearanceMeasurement {
        distance_mm: first.distance_to(second),
        first,
        second,
    }
}

fn cross(first: Point, second: Point) -> f64 {
    first.x * second.y - first.y * second.x
}

fn segments_intersect(a: Point, b: Point, c: Point, d: Point) -> bool {
    let ab = b - a;
    let cd = d - c;
    let ac = c - a;
    let denominator = cross(ab, cd);
    if denominator.abs() <= tol::EPSILON_MM {
        if cross(ac, ab).abs() > tol::EPSILON_MM {
            return false;
        }
        let axis = if ab.x.abs() >= ab.y.abs() { 0 } else { 1 };
        let coordinate = |point: Point| if axis == 0 { point.x } else { point.y };
        let (a0, a1) = ordered(coordinate(a), coordinate(b));
        let (c0, c1) = ordered(coordinate(c), coordinate(d));
        return a0 <= c1 + tol::EPSILON_MM && c0 <= a1 + tol::EPSILON_MM;
    }
    let t = cross(ac, cd) / denominator;
    let u = cross(ac, ab) / denominator;
    (-tol::EPSILON_MM..=1.0 + tol::EPSILON_MM).contains(&t)
        && (-tol::EPSILON_MM..=1.0 + tol::EPSILON_MM).contains(&u)
}

fn segment_intersection(a: Point, b: Point, c: Point, d: Point) -> Option<Point> {
    let ab = b - a;
    let cd = d - c;
    let denominator = cross(ab, cd);
    if denominator.abs() <= tol::EPSILON_MM {
        return None;
    }
    Some(a + ab * (cross(c - a, cd) / denominator))
}

fn ordered(first: f64, second: f64) -> (f64, f64) {
    (first.min(second), first.max(second))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::path::{ContourBuf, PathCmd};
    use crate::geom::{FillRule, shapes};

    fn rect_at(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> ContourBuf {
        ContourBuf::new(vec![
            PathCmd::move_to(Point::new(min_x, min_y)),
            PathCmd::line_to(Point::new(max_x, min_y)),
            PathCmd::line_to(Point::new(max_x, max_y)),
            PathCmd::line_to(Point::new(min_x, max_y)),
            PathCmd::close(),
        ])
    }

    #[test]
    fn clearance_reports_distance_and_witness_points() {
        let left = ContourSet::from_contours(
            &[rect_at(0.0, 0.0, 2.0, 2.0)],
            FillRule::NonZero,
            tol::REGION_MM,
        );
        let right = ContourSet::from_contours(
            &[rect_at(3.5, 0.5, 5.0, 1.5)],
            FillRule::NonZero,
            tol::REGION_MM,
        );

        let between_regions = region_clearance(&left, &right).unwrap();
        assert!((between_regions.distance_mm - 1.5).abs() < 1e-9);
        assert!((between_regions.first.x - 2.0).abs() < 1e-9);
        assert!((between_regions.second.x - 3.5).abs() < 1e-9);

        let from_segment =
            segment_region_clearance(Point::new(-1.0, 3.0), Point::new(3.0, 3.0), &left).unwrap();
        assert!((from_segment.distance_mm - 1.0).abs() < 1e-9);
    }

    #[test]
    fn intersecting_geometry_has_zero_clearance() {
        let region = ContourSet::from_contours(
            &[rect_at(0.0, 0.0, 2.0, 2.0)],
            FillRule::NonZero,
            tol::REGION_MM,
        );
        let crossing =
            segment_region_clearance(Point::new(-1.0, 1.0), Point::new(3.0, 1.0), &region).unwrap();
        assert_eq!(crossing.distance_mm, 0.0);
    }

    #[test]
    fn circular_enclosure_is_signed_and_reports_witnesses() {
        let copper = ContourSet::from_contours(
            &[shapes::circle(5.0).unwrap()],
            FillRule::NonZero,
            tol::REGION_MM,
        );
        let mut index = RegionBoundaryIndex::new(&copper, 1.475).unwrap();

        assert_eq!(
            index
                .check_circular_enclosure(Point::default(), 1.35, 0.125, 0.0, true)
                .unwrap(),
            CircularEnclosureCheck::Satisfied
        );

        let check = index
            .check_circular_enclosure(Point::new(1.3, 0.0), 1.35, 0.125, 0.0, true)
            .unwrap();
        let CircularEnclosureCheck::Violated(measurement) = check else {
            panic!("hole extending beyond copper must violate enclosure");
        };
        assert!((measurement.enclosure_mm + 0.15).abs() < tol::FLATTEN_MM);
        assert!((measurement.cutout_boundary.x - 2.65).abs() < tol::FLATTEN_MM);
        assert!((measurement.material_boundary.unwrap().x - 2.5).abs() < tol::FLATTEN_MM);
        assert!(measurement.center_in_material);
    }

    #[test]
    fn circular_enclosure_requires_material_at_the_hole_center() {
        let copper = ContourSet::empty(tol::REGION_MM);
        let mut index = RegionBoundaryIndex::new(&copper, 1.0).unwrap();

        let check = index
            .check_circular_enclosure(Point::default(), 0.3, 0.1, 0.0, false)
            .unwrap();
        let CircularEnclosureCheck::Violated(measurement) = check else {
            panic!("an empty copper layer cannot enclose a hole");
        };
        assert_eq!(measurement.enclosure_mm, -0.3);
        assert_eq!(measurement.material_boundary, None);
    }

    #[test]
    fn circular_enclosure_handles_noncircular_lands() {
        let copper = ContourSet::from_contours(
            &[rect_at(-3.0, -1.5, 3.0, 1.5)],
            FillRule::NonZero,
            tol::REGION_MM,
        );
        let mut index = RegionBoundaryIndex::new(&copper, 1.6).unwrap();

        let check = index
            .check_circular_enclosure(Point::default(), 1.0, 0.6, 0.0, true)
            .unwrap();
        let CircularEnclosureCheck::Violated(measurement) = check else {
            panic!("the short side of a rectangular land must set the enclosure");
        };
        assert!((measurement.enclosure_mm - 0.5).abs() < 1e-9);
        assert!((measurement.material_boundary.unwrap().y.abs() - 1.5).abs() < 1e-9);
    }
}
