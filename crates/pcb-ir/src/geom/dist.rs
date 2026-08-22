//! Euclidean distance primitives with closest-point witnesses.

use crate::geom::point::Point;

/// The shortest separation between two pieces of geometry, with the points
/// that realize it. Distances are expressed in the IR's canonical millimeters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClearanceMeasurement {
    pub distance_mm: f64,
    pub first: Point,
    pub second: Point,
}

/// Distance from a point to a closed segment, with the closest point on the
/// segment.
pub fn point_segment(point: Point, start: Point, end: Point) -> (f64, Point) {
    let delta = end - start;
    let length_squared = delta.x * delta.x + delta.y * delta.y;
    let closest = if length_squared <= f64::EPSILON {
        start
    } else {
        let t = (((point.x - start.x) * delta.x + (point.y - start.y) * delta.y) / length_squared)
            .clamp(0.0, 1.0);
        start + delta * t
    };
    (point.distance_to(closest), closest)
}

/// Distance between two closed segments, with the closest point on each.
/// Properly crossing segments measure zero at their intersection.
pub fn segments(
    first_start: Point,
    first_end: Point,
    second_start: Point,
    second_end: Point,
) -> (f64, Point, Point) {
    if let Some(point) = crossing_point(first_start, first_end, second_start, second_end) {
        return (0.0, point, point);
    }
    let candidates = [
        {
            let (distance, closest) = point_segment(first_start, second_start, second_end);
            (distance, first_start, closest)
        },
        {
            let (distance, closest) = point_segment(first_end, second_start, second_end);
            (distance, first_end, closest)
        },
        {
            let (distance, closest) = point_segment(second_start, first_start, first_end);
            (distance, closest, second_start)
        },
        {
            let (distance, closest) = point_segment(second_end, first_start, first_end);
            (distance, closest, second_end)
        },
    ];
    candidates
        .into_iter()
        .min_by(|left, right| left.0.total_cmp(&right.0))
        .expect("four segment endpoint candidates")
}

/// The intersection of two properly crossing segments. Touching, collinear,
/// and shared endpoints return `None`; the endpoint projections in
/// [`segments`] measure those as (near) zero without an epsilon on the
/// intersection parameters.
fn crossing_point(a: Point, b: Point, c: Point, d: Point) -> Option<Point> {
    let d1 = cross(d - c, a - c);
    let d2 = cross(d - c, b - c);
    let d3 = cross(b - a, c - a);
    let d4 = cross(b - a, d - a);
    if !(d1 * d2 < 0.0 && d3 * d4 < 0.0) {
        return None;
    }
    let ab = b - a;
    let cd = d - c;
    // Strictly opposite orientation signs imply a nonzero denominator.
    Some(a + ab * (cross(c - a, cd) / cross(ab, cd)))
}

fn cross(first: Point, second: Point) -> f64 {
    first.x * second.y - first.y * second.x
}
