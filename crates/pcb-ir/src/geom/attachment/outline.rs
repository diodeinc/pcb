//! Local rectangular-footprint eligibility along an immutable polygon boundary.
//!
//! Obstacles are declared filled exclusions, not inferred component bodies. A
//! courtyard is a useful exclusion contract; validating its source semantics
//! belongs to the caller. Front/back, population, copper and process policy are
//! likewise caller decisions. No substrate is removed by this operation.
//!
//! On each edge, intersect an obstacle with the footprint's normal strip. Each
//! connected intersection projects to a tangent interval, expanded by half the
//! footprint width. This is the configuration-space collision interval for a
//! translating rectangle, including concave obstacles and holes, without a
//! placement sampling grid. Contracted/expanded rectangles bracket a positional
//! uncertainty band. As with BoundaryQuery, these are polygon-model results:
//! neither source topology nor source-curve tangent/arclength error is certified.
//! Frame connection, inward-facing edge policy, router access and strength are
//! deliberately not evaluated. Interval endpoints are excluded from guarantees.

use super::{BoundaryId, BoundaryQuery, QueryError, QueryTolerance, transform_region};
use crate::geom::region::ring_edges;
use crate::geom::{Affine2, BBox, ContourSet, Point};

/// Millimeters, in the local outgoing-tangent / out-of-material-normal frame.
/// Include any desired rectangular process allowance in these dimensions;
/// there is no implicit clearance, cutter radius, or tab recipe.
#[derive(Debug, Clone, Copy)]
pub struct OutlineFootprint {
    pub width_mm: f64,
    pub inward_mm: f64,
    pub outward_mm: f64,
}

impl OutlineFootprint {
    fn validate(self) -> Result<(), QueryError> {
        if !self.width_mm.is_finite()
            || self.width_mm <= 0.0
            || !self.inward_mm.is_finite()
            || self.inward_mm < 0.0
            || !self.outward_mm.is_finite()
            || self.outward_mm < 0.0
            || !(self.inward_mm + self.outward_mm).is_finite()
            || self.inward_mm + self.outward_mm <= 0.0
        {
            return Err(QueryError::InvalidInput("invalid outline footprint"));
        }
        Ok(())
    }
}

/// Missing or empty evidence cannot establish clearance anywhere. Omit an
/// obstacle only when the caller has explicitly decided it is not applicable.
pub struct OutlineObstacle<'a> {
    pub id: &'a str,
    pub region: Option<&'a ContourSet>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutlineState {
    Eligible,
    Blocked,
    Unknown,
}

/// An open interval on one original polygon edge. No merging across corners
/// or cyclic seams: the rectangle orientation changes at an edge boundary.
#[derive(Debug, Clone)]
pub struct OutlineInterval {
    pub boundary: BoundaryId,
    pub edge: usize,
    pub start_mm: f64,
    pub end_mm: f64,
    pub start: Point,
    pub end: Point,
    pub state: OutlineState,
    /// Indices into the supplied obstacles, preserving all relevant sources.
    pub obstacles: Vec<usize>,
    /// Positional comparison band, not a bound on polygon arclength.
    pub uncertainty_mm: f64,
}

/// Partition every boundary edge into full-footprint eligibility intervals.
///
/// Eligible means every centre in the interval clears all declared exclusions.
/// Blocked means one exclusion intersects even the contracted footprint.
/// Unknown means only the expanded footprint intersects, or evidence is absent.
/// A known blocker takes precedence over missing evidence. Errors are errors,
/// not empty geometry or geometric rejection. Input regions must be canonical;
/// prepare with zero significance when narrow rings must survive.
pub fn eligible_outline(
    substrate: &ContourSet,
    obstacles: &[OutlineObstacle<'_>],
    footprint: OutlineFootprint,
    tolerance: QueryTolerance,
) -> Result<Vec<OutlineInterval>, QueryError> {
    footprint.validate()?;
    let boundary = BoundaryQuery::new(substrate, tolerance)?;
    if substrate.is_empty() {
        return Err(QueryError::InvalidInput("missing board substrate"));
    }
    for obstacle in obstacles {
        if let Some(region) = obstacle.region {
            super::validate_region(region)?;
        }
    }
    let mut result = Vec::new();
    for id in boundary.boundaries() {
        let mut station = 0.0;
        for (edge, (start, end)) in ring_edges(&substrate.rings[id.ring]).enumerate() {
            let length = start.distance_to(end);
            let tangent = (end - start) / length;
            let normal = Point::new(tangent.y, -tangent.x);
            let local_from_board = Affine2 {
                m00: tangent.x,
                m01: tangent.y,
                m02: -tangent.x * start.x - tangent.y * start.y,
                m10: normal.x,
                m11: normal.y,
                m12: -normal.x * start.x - normal.y * start.y,
            };
            let mut spans = Vec::new();
            let mut missing = Vec::new();
            let mut cuts = vec![0.0, length];
            let mut band: f64 = 0.0;
            for (index, obstacle) in obstacles.iter().enumerate() {
                let Some(region) = obstacle.region.filter(|region| !region.is_empty()) else {
                    missing.push(index);
                    continue;
                };
                // The whole preparation budget reserves room for the transform
                // and strip boolean, rather than forgetting their rounding.
                let uncertainty = substrate.uncertainty_mm.max(tolerance.boundary_mm)
                    + region.budget().max_error_mm().max(tolerance.boundary_mm)
                    + tolerance.numerical_mm;
                if !uncertainty.is_finite() {
                    return Err(QueryError::InvalidInput("outline uncertainty overflow"));
                }
                band = band.max(uncertainty);
                let local = super::unfiltered(&transform_region(region, local_from_board)?);
                for (certain, padding) in [(false, uncertainty), (true, -uncertainty)] {
                    for (lo, hi) in collision_spans(&local, footprint, padding, length)? {
                        cuts.extend([lo, hi]);
                        spans.push((lo, hi, index, certain));
                    }
                }
            }
            cuts.sort_by(f64::total_cmp);
            cuts.dedup();
            for pair in cuts.windows(2) {
                let midpoint = pair[0] + (pair[1] - pair[0]) / 2.0;
                if midpoint <= pair[0]
                    || midpoint >= pair[1]
                    || station + pair[0] >= station + pair[1]
                {
                    return Err(QueryError::Numerical(
                        "outline interval below station precision",
                    ));
                }
                let mut contributors = missing.clone();
                let mut blocked = false;
                for &(lo, hi, index, certain) in &spans {
                    if lo <= midpoint && midpoint <= hi {
                        contributors.push(index);
                        blocked |= certain;
                    }
                }
                contributors.sort_unstable();
                contributors.dedup();
                result.push(OutlineInterval {
                    boundary: id,
                    edge,
                    start_mm: station + pair[0],
                    end_mm: station + pair[1],
                    start: start + tangent * pair[0],
                    end: start + tangent * pair[1],
                    state: if blocked {
                        OutlineState::Blocked
                    } else if contributors.is_empty() {
                        OutlineState::Eligible
                    } else {
                        OutlineState::Unknown
                    },
                    obstacles: contributors,
                    uncertainty_mm: band,
                });
            }
            station += length;
        }
    }
    Ok(result)
}

fn collision_spans(
    obstacle: &ContourSet,
    footprint: OutlineFootprint,
    padding: f64,
    length: f64,
) -> Result<Vec<(f64, f64)>, QueryError> {
    let half_width = footprint.width_mm / 2.0 + padding;
    let bottom = -footprint.inward_mm - padding;
    let top = footprint.outward_mm + padding;
    if !half_width.is_finite() || !bottom.is_finite() || !(top + length + half_width).is_finite() {
        return Err(QueryError::InvalidInput("outline footprint overflow"));
    }
    if half_width <= 0.0 || bottom >= top {
        return Ok(Vec::new());
    }
    // Clip in X too: obstacles far from this edge cannot contribute. The
    // half-width expansion restores all possible centre positions in [0,L].
    let strip = ContourSet::rectangle(
        BBox::new(
            Point::new(-half_width, bottom),
            Point::new(length + half_width, top),
        ),
        obstacle.resolution.strict(),
    );
    let clipped = obstacle.intersection(&strip)?;
    Ok(clipped
        .connected_components()
        .into_iter()
        .filter_map(|part| {
            let bounds = part.bbox();
            let lo = (bounds.min.x - half_width).max(0.0);
            let hi = (bounds.max.x + half_width).min(length);
            (hi > lo).then_some((lo, hi))
        })
        .collect())
}

#[cfg(test)]
mod tests;
