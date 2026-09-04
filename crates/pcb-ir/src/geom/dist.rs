//! Euclidean distance primitives with closest-point witnesses.

use crate::geom::point::Point;
use crate::geom::tol;

/// A measured length between two witness points, in the IR's canonical
/// millimeters, with the uncertainty its inputs carry.
///
/// `uncertainty_mm` carries the preparation history of the measured inputs.
/// Use [`Distance::with_uncertainty`] for prepared regions. The legacy
/// [`Distance::flattened`] helper is for untracked default-tolerance linework.
/// `mm` is signed where the quantity is: a negative enclosure is a breach.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Distance {
    pub mm: f64,
    pub uncertainty_mm: f64,
    pub first: Point,
    pub second: Point,
}

impl Distance {
    /// A length taken from exact geometry: stated primitives, or analytic
    /// shapes such as disks.
    pub fn exact(mm: f64, first: Point, second: Point) -> Self {
        Self {
            mm,
            uncertainty_mm: 0.0,
            first,
            second,
        }
    }

    /// A measurement using the accumulated positional errors of its inputs.
    pub fn with_uncertainty(mm: f64, first: Point, second: Point, uncertainty_mm: f64) -> Self {
        Self {
            mm,
            first,
            second,
            uncertainty_mm,
        }
    }

    pub fn also_uncertain(self, uncertainty_mm: f64) -> Self {
        Self {
            uncertainty_mm: self.uncertainty_mm + uncertainty_mm,
            ..self
        }
    }

    /// A length measured against `flattened_boundaries` tessellated curves.
    pub fn flattened(mm: f64, first: Point, second: Point, flattened_boundaries: u32) -> Self {
        Self {
            mm,
            uncertainty_mm: f64::from(flattened_boundaries) * tol::FLATTEN_MM,
            first,
            second,
        }
    }

    /// The same length, with `flattened_boundaries` more tessellated inputs
    /// counted toward its uncertainty.
    pub fn also_flattened(self, flattened_boundaries: u32) -> Self {
        Self {
            uncertainty_mm: self.uncertainty_mm + f64::from(flattened_boundaries) * tol::FLATTEN_MM,
            ..self
        }
    }

    /// Whether the length falls short of `limit_mm` even at the top of its
    /// uncertainty band.
    pub fn certainly_below(&self, limit_mm: f64) -> bool {
        self.mm + self.uncertainty_mm < limit_mm
    }

    pub fn midpoint(&self) -> Point {
        self.first.midpoint(self.second)
    }
}

/// Distance from a point to a closed segment, with the closest point on the
/// segment.
pub fn point_segment(point: Point, start: Point, end: Point) -> (f64, Point) {
    let delta = end - start;
    let length_squared = delta.x * delta.x + delta.y * delta.y;
    let closest = if length_squared == 0.0 {
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
