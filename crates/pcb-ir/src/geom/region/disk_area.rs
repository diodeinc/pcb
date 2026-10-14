//! Certified areas of a union of disks measured against a region.

use super::{ContourSet, Ring};
use crate::geom::{AccuracyError, Point};
use std::f64::consts::PI;

/// Polygon sides past which one disk is being measured to an absurd budget.
const MAX_SIDES: f64 = 1.0e6;

/// A closed interval that certainly contains an exact area, in mm².
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AreaBounds {
    pub lower: f64,
    pub upper: f64,
}

/// How a union of disks lies against a region; see
/// [`ContourSet::disk_union_area`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DiskUnionArea {
    /// `area(region ∩ ⋃ disks)`.
    pub covered: AreaBounds,
    /// `area(⋃ disks \ region)`.
    pub spill: AreaBounds,
}

impl ContourSet {
    /// Certified bounds on the area a union of `(center, radius)` disks
    /// covers of this region and the area it spills outside it.
    ///
    /// Each disk lies between an inscribed and a circumscribed polygon, both
    /// within the region's budget of the circle, so the unions of each kind
    /// enclose the exact union from inside and outside. Overlapping disks
    /// are unioned before measuring and count once. The overlay's coordinate
    /// rounding is charged as the area of a band its recorded displacement
    /// wide around every edge the operations read, so the exact areas lie
    /// within the bounds. Each interval is about `2 × budget × perimeter` wide.
    ///
    /// The region's polygons are what is measured: approximation they carry
    /// relative to their own source is not part of the bounds.
    pub fn disk_union_area(&self, disks: &[(Point, f64)]) -> Result<DiskUnionArea, AccuracyError> {
        let max_error = self.budget().max_error_mm();
        let strict = self.resolution.strict();
        let mut inner = Vec::with_capacity(disks.len());
        let mut outer = Vec::with_capacity(disks.len());
        for &(center, radius) in disks {
            if !center.is_finite() || !radius.is_finite() || radius < 0.0 {
                return Err(AccuracyError::InvalidGeometry("invalid disk"));
            }
            if radius == 0.0 {
                continue;
            }
            let (inscribed, circumscribed) = disk_polygons(center, radius, max_error)?;
            inner.push(ContourSet::from_regularized(vec![inscribed], strict, 0.0));
            outer.push(ContourSet::from_regularized(
                vec![circumscribed],
                strict,
                0.0,
            ));
        }
        let region = ContourSet {
            uncertainty_mm: 0.0,
            ..self.clone()
        };
        // The exact boundary of every result lies on the disk polygons, their
        // computed union, and the region, so a band around all of those
        // covers whatever rounding moved or erased, even a ring too thin
        // to survive in the result.
        let measure = |polygons: Vec<ContourSet>| {
            let mut boundary = polygons.iter().map(Boundary::of).sum::<Boundary>();
            let union = ContourSet::union_all(strict, polygons)?;
            boundary = boundary + Boundary::of(&union) + Boundary::of(&region.reaching(union.bbox));
            let area = |result: ContourSet| (result.area(), boundary.band(result.uncertainty_mm));
            Ok::<_, AccuracyError>((
                area(union.intersection(&region)?),
                area(union.difference(&region)?),
            ))
        };
        let (inner_covered, inner_spill) = measure(inner)?;
        let (outer_covered, outer_spill) = measure(outer)?;
        let bounds =
            |(inside, inside_error): (f64, f64), (outside, outside_error): (f64, f64)| AreaBounds {
                lower: (inside - inside_error).max(0.0),
                upper: outside + outside_error,
            };
        Ok(DiskUnionArea {
            covered: bounds(inner_covered, outer_covered),
            spill: bounds(inner_spill, outer_spill),
        })
    }
}

/// Counter-clockwise polygons inscribed in and circumscribed about a disk,
/// each within `max_error` of its circle.
fn disk_polygons(
    center: Point,
    radius: f64,
    max_error: f64,
) -> Result<(Ring, Ring), AccuracyError> {
    // Rounding in placing a vertex, kept out of both polygons.
    let slack = 8.0 * f64::EPSILON * (center.x.abs() + center.y.abs() + radius);
    // The circumscribed polygon strays farther from the circle than the
    // inscribed one, `radius × (sec(π / sides) − 1)`, so it sets the count.
    let sides = (PI / (radius / (radius + max_error)).acos())
        .ceil()
        .max(3.0);
    if sides > MAX_SIDES {
        return Err(AccuracyError::SubdivisionLimit);
    }
    let half_step = PI / sides;
    let polygon = |vertex_radius: f64| {
        (0..sides as usize)
            .map(|index| {
                let (sin, cos) = (2.0 * half_step * index as f64).sin_cos();
                [
                    center.x + vertex_radius * cos,
                    center.y + vertex_radius * sin,
                ]
            })
            .collect()
    };
    Ok((
        polygon((radius - slack).max(0.0)),
        polygon((radius + slack) / half_step.cos() * (1.0 + 4.0 * f64::EPSILON)),
    ))
}

/// What the area of a band around some polygon edges depends on.
#[derive(Default)]
struct Boundary {
    length: f64,
    edges: f64,
    /// The shoelace sum's own rounding, charged per vertex.
    shoelace: f64,
}

impl Boundary {
    fn of(region: &ContourSet) -> Self {
        region
            .bounded_rings()
            .map(|(ring, bounds)| {
                let extent = bounds.width().max(bounds.height());
                let edges = ring.len() as f64;
                Self {
                    length: super::edges_of(ring)
                        .map(|(start, end)| start.distance_to(end))
                        .sum(),
                    edges,
                    shoelace: edges * 8.0 * f64::EPSILON * extent * extent,
                }
            })
            .sum()
    }

    /// The area within `delta` of these edges, each a stadium.
    fn band(&self, delta: f64) -> f64 {
        2.0 * delta * (self.length + 2.0 * delta * self.edges)
            + PI * delta * delta * self.edges
            + self.shoelace
    }
}

impl std::ops::Add for Boundary {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self {
            length: self.length + other.length,
            edges: self.edges + other.edges,
            shoelace: self.shoelace + other.shoelace,
        }
    }
}

impl std::iter::Sum for Boundary {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::default(), std::ops::Add::add)
    }
}
