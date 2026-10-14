//! Certified areas of a union of disks measured against a region.

use super::{ContourSet, Ring};
use crate::geom::{AccuracyError, FillRule, Point};
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
    /// The bounds are about the region's source: its `uncertainty_mm` is
    /// charged along its edges, so a region prepared at its full budget has
    /// no room left for the overlay and fails like any other boolean.
    pub fn disk_union_area(&self, disks: &[(Point, f64)]) -> Result<DiskUnionArea, AccuracyError> {
        if disks
            .iter()
            .any(|(center, radius)| !center.is_finite() || !(*radius >= 0.0 && radius.is_finite()))
        {
            return Err(AccuracyError::InvalidGeometry("invalid disk"));
        }
        let max_error = self.budget().max_error_mm();
        let strict = self.resolution.strict();
        // Counter-clockwise polygons union under the nonzero rule. The exact
        // boundary of each result lies on the polygons and the region's
        // source, within the result's uncertainty of the polygons and the
        // region, so a band that wide around them covers whatever the source
        // or rounding moved or erased, even a ring too thin to survive.
        let measure = |circumscribed: bool| {
            let polygons = disks
                .iter()
                .map(|&(center, radius)| disk_polygon(center, radius, max_error, circumscribed))
                .collect::<Result<Vec<_>, _>>()?;
            let union = ContourSet::from_rings(polygons.clone(), FillRule::NonZero, strict)?;
            let near = self.reaching(union.bbox);
            let read = || polygons.iter().map(Vec::as_slice).chain(near.rings());
            let area =
                |result: ContourSet| (result.area(), rounding_area(read(), result.uncertainty_mm));
            Ok::<_, AccuracyError>([
                area(union.intersection(self)?),
                area(union.difference(self)?),
            ])
        };
        let [inner_covered, inner_spill] = measure(false)?;
        let [outer_covered, outer_spill] = measure(true)?;
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

/// A counter-clockwise polygon inscribed in or circumscribed about a disk,
/// within `max_error` of its circle.
fn disk_polygon(
    center: Point,
    radius: f64,
    max_error: f64,
    circumscribed: bool,
) -> Result<Ring, AccuracyError> {
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
    let vertex_radius = if circumscribed {
        (radius + slack) / half_step.cos() * (1.0 + 4.0 * f64::EPSILON)
    } else {
        (radius - slack).max(0.0)
    };
    Ok((0..sides as usize)
        .map(|index| {
            let (sin, cos) = (2.0 * half_step * index as f64).sin_cos();
            [
                center.x + vertex_radius * cos,
                center.y + vertex_radius * sin,
            ]
        })
        .collect())
}

/// The area within `delta` of every edge of `rings`, each edge a stadium,
/// plus the shoelace sum's own rounding per vertex.
fn rounding_area<'a>(rings: impl Iterator<Item = &'a [[f64; 2]]>, delta: f64) -> f64 {
    rings
        .map(|ring| {
            let bounds = super::rings_bbox(&[ring.to_vec()]);
            let extent = bounds.width().max(bounds.height());
            super::edges_of(ring)
                .map(|(start, end)| {
                    2.0 * delta * (start.distance_to(end) + 2.0 * delta)
                        + PI * delta * delta
                        + 8.0 * f64::EPSILON * extent * extent
                })
                .sum::<f64>()
        })
        .sum()
}
