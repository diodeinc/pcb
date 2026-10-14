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
    /// wide around every result edge, so the exact areas lie within the
    /// bounds. Each interval is about `2 × budget × perimeter` wide.
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
        let inner = ContourSet::union_all(strict, inner)?;
        let outer = ContourSet::union_all(strict, outer)?;
        let region = ContourSet {
            uncertainty_mm: 0.0,
            ..self.clone()
        };
        let bounds = |inside: ContourSet, outside: ContourSet| AreaBounds {
            lower: (inside.area() - rounding_area(&inside)).max(0.0),
            upper: outside.area() + rounding_area(&outside),
        };
        Ok(DiskUnionArea {
            covered: bounds(inner.intersection(&region)?, outer.intersection(&region)?),
            spill: bounds(inner.difference(&region)?, outer.difference(&region)?),
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

/// The most a computed boolean's area can differ from the exact one: every
/// point of its boundary lies within `uncertainty_mm` of the exact boundary,
/// so the two differ only inside a band that wide around its edges. The
/// shoelace sum's own rounding is charged per vertex.
fn rounding_area(region: &ContourSet) -> f64 {
    let delta = region.uncertainty_mm;
    region
        .bounded_rings()
        .map(|(ring, bounds)| {
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
