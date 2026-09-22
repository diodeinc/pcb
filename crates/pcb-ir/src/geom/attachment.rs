//! Policy-free attachment geometry on the canonical regularized polygon model.
//!
//! All lengths are millimeters. Callers supply material, complete footprints,
//! obstacles (including component overhangs and forbidden holes), and swept
//! break-removal regions. This module selects no tab pattern, support count,
//! fixture, or fracture model.
//!
//! # Scope of guarantees
//! Boundary stations and topology refer to the supplied [`ContourSet`], not its
//! pre-flattened curves. Flattening does not bound arc-length or tangent error,
//! and a Hausdorff error alone does not preserve topology. Region booleans use
//! the existing floating-point/integer overlay backend; these are not exact
//! predicates. Missing features already discarded by a caller's region
//! tolerance cannot be recovered. [`QueryTolerance`] is an explicit uncertainty
//! budget, not an independently certified bound on that backend. In particular,
//! [`PolygonTopology`] is a model result, **not** certified source-curve
//! topology or physical separation.

pub mod outline;

use super::accuracy::numerical_error;
use super::dist::{self, Distance};
use super::region::{ring_edges, segment_inside_intervals};
use super::{AccuracyError, Affine2, ContourSet, Point, PreparedRegion};

/// Caller-supplied position uncertainty for each input boundary, and numerical
/// guard for comparisons. Stored region uncertainty is always a floor;
/// `boundary_mm` may supply a larger external uncertainty. Neither value is an
/// arc-length, angular, or topology bound. Operations preserve the region's
/// preparation budget and propagate AccuracyError rather than widening it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QueryTolerance {
    pub boundary_mm: f64,
    pub numerical_mm: f64,
}

impl QueryTolerance {
    fn validate(self) -> Result<(), QueryError> {
        if !self.boundary_mm.is_finite()
            || self.boundary_mm < 0.0
            || !self.numerical_mm.is_finite()
            || self.numerical_mm <= 0.0
            || !self.pair_band().is_finite()
        {
            return Err(QueryError::InvalidInput("invalid uncertainty budget"));
        }
        Ok(())
    }

    fn pair_band(self) -> f64 {
        2.0 * self.boundary_mm + self.numerical_mm
    }
}

/// Invalid inputs and numerical failures are not geometric rejections.
#[derive(Debug, Clone, PartialEq)]
pub enum QueryError {
    InvalidInput(&'static str),
    Numerical(&'static str),
    Accuracy(AccuracyError),
}

impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidInput(message) => write!(f, "invalid geometry query: {message}"),
            Self::Numerical(message) => write!(f, "numerical geometry failure: {message}"),
            Self::Accuracy(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for QueryError {}

impl From<AccuracyError> for QueryError {
    fn from(error: AccuracyError) -> Self {
        Self::Accuracy(error)
    }
}

pub(crate) fn validate_region(region: &ContourSet) -> Result<(), QueryError> {
    region.budget().check(region.uncertainty_mm)?;
    if !region.tolerance().is_finite()
        || region.tolerance() < 0.0
        || region.rings.iter().any(|ring| {
            ring.len() < 3 || ring.iter().any(|p| !p[0].is_finite() || !p[1].is_finite())
        })
    {
        return Err(QueryError::InvalidInput(
            "expected finite regularized rings",
        ));
    }
    Ok(())
}

// Do not discard newly created narrow pieces according to an input region's
// significance threshold. This cannot restore pieces lost before the query.
fn unfiltered(region: &ContourSet) -> ContourSet {
    ContourSet::from_regularized(
        region.rings.clone(),
        region.resolution.strict(),
        region.uncertainty_mm,
    )
}

/// Stable only within a BoundaryQuery's immutable source snapshot. `ring` is
/// the original ContourSet ring index; holes retain their material component.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoundaryId {
    pub component: usize,
    pub ring: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoundarySite {
    pub boundary: BoundaryId,
    /// Cyclic polygon arc length, in [0, perimeter).
    pub station_mm: f64,
    pub point: Point,
    /// Unit outgoing tangent at a vertex; not an averaged corner tangent.
    pub tangent: Point,
    /// Unit normal pointing out of material (into the void on a hole).
    pub outward_normal: Point,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoundaryProjection {
    pub site: BoundarySite,
    pub distance: Distance,
}

/// Arc-length index borrowing the canonical geometry, not another polygon
/// representation. Construct again after a transform or boolean operation.
pub struct BoundaryQuery<'a> {
    region: &'a ContourSet,
    tolerance: QueryTolerance,
    components: Vec<usize>,
    stations: Vec<Vec<f64>>,
}

impl<'a> BoundaryQuery<'a> {
    pub fn new(region: &'a ContourSet, tolerance: QueryTolerance) -> Result<Self, QueryError> {
        tolerance.validate()?;
        validate_region(region)?;
        let stations = region
            .rings
            .iter()
            .map(|ring| {
                let mut stations = vec![0.0];
                for (a, b) in ring_edges(ring) {
                    let next = stations.last().unwrap() + a.distance_to(b);
                    if !next.is_finite() || next <= *stations.last().unwrap() {
                        return Err(QueryError::Numerical(
                            "degenerate boundary edge or arc-length overflow",
                        ));
                    }
                    stations.push(next);
                }
                Ok(stations)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let (components, _) = region.ring_components();
        Ok(Self {
            region,
            tolerance,
            components,
            stations,
        })
    }

    pub fn boundaries(&self) -> impl Iterator<Item = BoundaryId> + '_ {
        self.components
            .iter()
            .enumerate()
            .map(|(ring, &component)| BoundaryId { component, ring })
    }

    fn index(&self, id: BoundaryId) -> Result<&[f64], QueryError> {
        if self.components.get(id.ring) != Some(&id.component) {
            return Err(QueryError::InvalidInput("unknown boundary identity"));
        }
        Ok(&self.stations[id.ring])
    }

    pub fn perimeter(&self, id: BoundaryId) -> Result<f64, QueryError> {
        Ok(*self.index(id)?.last().unwrap())
    }

    pub fn site(&self, id: BoundaryId, station_mm: f64) -> Result<BoundarySite, QueryError> {
        let stations = self.index(id)?;
        if !station_mm.is_finite() {
            return Err(QueryError::InvalidInput("non-finite boundary station"));
        }
        let perimeter = *stations.last().unwrap();
        let s = station_mm.rem_euclid(perimeter);
        // rem_euclid can round a tiny negative argument up to the divisor.
        let s = if s == perimeter { 0.0 } else { s };
        let edge = stations.partition_point(|&v| v <= s) - 1;
        let ring = &self.region.rings[id.ring];
        let a = Point::new(ring[edge][0], ring[edge][1]);
        let b = Point::new(
            ring[(edge + 1) % ring.len()][0],
            ring[(edge + 1) % ring.len()][1],
        );
        let tangent = (b - a) / (stations[edge + 1] - stations[edge]);
        Ok(BoundarySite {
            boundary: id,
            station_mm: s,
            point: a + tangent * (s - stations[edge]),
            tangent,
            outward_normal: Point::new(tangent.y, -tangent.x),
        })
    }

    /// Nearest point on a specified ring; equal-distance ties choose its first
    /// edge. Repeat over boundaries when all equally near components matter.
    pub fn project(&self, id: BoundaryId, point: Point) -> Result<BoundaryProjection, QueryError> {
        let stations = self.index(id)?;
        if !point.is_finite() {
            return Err(QueryError::InvalidInput("non-finite projection point"));
        }
        let (edge, mm, closest) = ring_edges(&self.region.rings[id.ring])
            .enumerate()
            .map(|(edge, (a, b))| {
                let (mm, closest) = dist::point_segment(point, a, b);
                (edge, mm, closest)
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .ok_or(QueryError::InvalidInput("empty boundary"))?;
        let a = self.region.rings[id.ring][edge];
        if !mm.is_finite() {
            return Err(QueryError::Numerical("projection overflow"));
        }
        let site = self.site(
            id,
            stations[edge] + Point::new(a[0], a[1]).distance_to(closest),
        )?;
        Ok(BoundaryProjection {
            site,
            distance: Distance {
                mm,
                uncertainty_mm: self.tolerance.boundary_mm.max(self.region.uncertainty_mm)
                    + self.tolerance.numerical_mm,
                first: point,
                second: closest,
            },
        })
    }
}

/// Transform the polygon model with the existing IR affine convention. An
/// invertible affine map takes a regularized polygon to a regularized polygon,
/// so vertices are mapped as they are and a reflection only reverses each
/// ring's winding: nothing is flattened or regularized again. Stored
/// uncertainty scales without widening the preparation budget. Callers must
/// separately scale any larger external QueryTolerance by the largest singular
/// value. Arbitrary affine transforms do not preserve stations, angles, or
/// circular cutters.
pub fn transform_region(region: &ContourSet, transform: Affine2) -> Result<ContourSet, QueryError> {
    validate_region(region)?;
    if transform.inverse().is_none() || !transform.m02.is_finite() || !transform.m12.is_finite() {
        return Err(QueryError::InvalidInput(
            "singular or non-finite affine transform",
        ));
    }
    let rings = region
        .rings
        .iter()
        .map(|ring| {
            let mapped = ring.iter().map(|&[x, y]| {
                let p = transform.transform_point(Point::new(x, y));
                [p.x, p.y]
            });
            if transform.determinant() < 0.0 {
                mapped.rev().collect()
            } else {
                mapped.collect()
            }
        })
        .collect::<Vec<Vec<_>>>();
    let uncertainty = region.uncertainty_mm * transform.max_scale()
        + numerical_error(super::region::rings_bbox(&rings));
    // Significance was applied when the region was prepared.
    let mut transformed =
        ContourSet::from_regularized(rings, region.resolution.strict(), uncertainty);
    transformed.resolution = region.resolution;
    transformed.budget().check(transformed.uncertainty_mm)?;
    Ok(transformed)
}

#[derive(Debug, Clone, PartialEq)]
pub enum GeometricRejection {
    FootprintOverlap,
    InsufficientClearance { required_mm: f64, measured_mm: f64 },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    /// Passes in the supplied polygon model and caller's uncertainty budget.
    Admissible,
    Rejected(GeometricRejection),
    /// Do not silently treat an uncertainty band as geometric infeasibility.
    Unresolved(&'static str),
}

pub struct Obstacle<'a> {
    /// Caller identity, e.g. a component reference or a hole identifier.
    pub id: &'a str,
    pub region: &'a ContourSet,
}

#[derive(Debug, Clone)]
pub struct FootprintCheck<'a> {
    pub obstacle: &'a str,
    pub decision: Decision,
    pub boundary_distance: Option<Distance>,
    /// Entire overlapping region, useful for actionable diagnostics.
    pub overlap: ContourSet,
}

/// Check one complete filled footprint against every supplied obstacle. No
/// center-only or vertex-only acceptance; containment, holes and intersecting
/// edges are checked by filled-region intersection and all-edge distance.
/// Empty obstacles are allowed; an empty footprint is invalid.
/// Touching within the numerical guard is unresolved, even for zero clearance.
/// A source-uncertain overlap is unresolved rather than a claim that flattened
/// curves prove collision. Supply holes as obstacles when occupying them is
/// forbidden; holes *in* an obstacle are free space under ContourSet semantics.
pub fn check_footprint<'a>(
    footprint: &ContourSet,
    obstacles: &[Obstacle<'a>],
    clearance_mm: f64,
    tolerance: QueryTolerance,
) -> Result<Vec<FootprintCheck<'a>>, QueryError> {
    tolerance.validate()?;
    if !clearance_mm.is_finite() || clearance_mm < 0.0 || footprint.is_empty() {
        return Err(QueryError::InvalidInput(
            "expected nonempty footprint and nonnegative clearance",
        ));
    }
    validate_region(footprint)?;
    let footprint_query = footprint.prepare_query();
    obstacles
        .iter()
        .map(|obstacle| {
            validate_region(obstacle.region)?;
            let prepared = obstacle.region.prepare_query();
            let overlap = unfiltered(footprint).intersection(&unfiltered(obstacle.region))?;
            let bounds = footprint.bbox().union(obstacle.region.bbox());
            let reach = bounds.width().hypot(bounds.height());
            if !reach.is_finite() && !obstacle.region.is_empty() {
                return Err(QueryError::Numerical("footprint bounds overflow"));
            }
            let distance = footprint
                .rings
                .iter()
                .flat_map(ring_edges)
                .filter_map(|(a, b)| prepared.segment_nearest_within(a, b, reach))
                .min_by(|a, b| a.mm.total_cmp(&b.mm))
                .map(|d| Distance {
                    uncertainty_mm: tolerance.boundary_mm.max(footprint.uncertainty_mm)
                        + tolerance.boundary_mm.max(obstacle.region.uncertainty_mm)
                        + tolerance.numerical_mm,
                    ..d
                });
            let decision = if !overlap.is_empty() {
                if tolerance.boundary_mm > 0.0
                    || footprint.uncertainty_mm > 0.0
                    || obstacle.region.uncertainty_mm > 0.0
                {
                    Decision::Unresolved(
                        "polygon footprints overlap; source-boundary uncertainty requires refinement",
                    )
                } else {
                    // A nonempty numerical intersection alone is not a
                    // collision certificate. Verify an interior witness
                    // against both original input boundaries.
                    let guard = tolerance.numerical_mm + overlap.uncertainty_mm;
                    if has_penetration_witness(&overlap, [&footprint_query, &prepared], guard) {
                        Decision::Rejected(GeometricRejection::FootprintOverlap)
                    } else {
                        Decision::Unresolved(
                            "overlap has no penetration witness beyond the numerical uncertainty band",
                        )
                    }
                }
            } else if let Some(d) = distance {
                if !d.mm.is_finite() {
                    return Err(QueryError::Numerical("footprint distance overflow"));
                }
                if d.mm - d.uncertainty_mm > clearance_mm {
                    Decision::Admissible
                } else if d.certainly_below(clearance_mm) {
                    Decision::Rejected(GeometricRejection::InsufficientClearance {
                        required_mm: clearance_mm,
                        measured_mm: d.mm,
                    })
                } else {
                    Decision::Unresolved(
                        "clearance lies within the boundary/numerical uncertainty band",
                    )
                }
            } else if obstacle.region.is_empty() {
                Decision::Admissible
            } else {
                return Err(QueryError::Numerical("missing footprint distance"));
            };
            Ok(FootprintCheck {
                obstacle: obstacle.id,
                decision,
                boundary_distance: distance,
                overlap,
            })
        })
        .collect()
}

/// Enumerate every horizontal vertex slab and every filled span, including
/// concavities and holes. These are sufficient witnesses, not a penetration
/// depth optimizer: absence of a deep witness leaves the query unresolved.
fn has_penetration_witness(overlap: &ContourSet, inputs: [&PreparedRegion; 2], guard: f64) -> bool {
    let mut heights = overlap
        .rings
        .iter()
        .flatten()
        .map(|p| p[1])
        .collect::<Vec<_>>();
    heights.sort_by(f64::total_cmp);
    heights.dedup();
    heights.windows(2).any(|heights| {
        let y = heights[0] + (heights[1] - heights[0]) / 2.0;
        let start = Point::new(overlap.bbox.min.x, y);
        let end = Point::new(overlap.bbox.max.x, y);
        segment_inside_intervals(overlap, start, end)
            .into_iter()
            .any(|(a, b)| {
                let point = start + (end - start) * (a + (b - a) / 2.0);
                inputs.iter().all(|input| {
                    input
                        .signed_distance(point)
                        .is_some_and(|distance| distance.mm + distance.uncertainty_mm < -guard)
                })
            })
    })
}

/// Witness classification in a polygon-model snapshot. Outside is geometric;
/// BoundaryBand requests refined geometry or a witness farther from the edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionMembership {
    Component(usize),
    Outside,
    BoundaryBand,
}

/// Polygon component membership with a guard at boundaries: boundary witnesses
/// never silently count as material or provide a zero-width connecting ligament.
fn membership(
    components: &[PreparedRegion],
    point: Point,
    tolerance: QueryTolerance,
) -> Result<RegionMembership, QueryError> {
    if !point.is_finite() {
        return Err(QueryError::InvalidInput("non-finite membership witness"));
    }
    for (index, component) in components.iter().enumerate() {
        let d = component
            .signed_distance(point)
            .ok_or(QueryError::Numerical("missing component boundary"))?;
        if !d.mm.is_finite() {
            return Err(QueryError::Numerical("membership distance overflow"));
        }
        let guard = (2.0 * tolerance.boundary_mm).max(d.uncertainty_mm) + tolerance.numerical_mm;
        if d.mm.abs() <= guard {
            return Ok(RegionMembership::BoundaryBand);
        }
        if d.mm < 0.0 {
            return Ok(RegionMembership::Component(index));
        }
    }
    Ok(RegionMembership::Outside)
}

/// Connectivity of the regularized retained polygon material, not a source-curve
/// or physical-fracture certificate. Component IDs belong only to this result.
#[derive(Debug, Clone)]
pub struct PolygonTopology {
    pub retained: ContourSet,
    pub components: Vec<ContourSet>,
    /// In caller order, distinguishing removed/outside witnesses from numerical
    /// or source-boundary ambiguity. Neither counts as successful separation.
    pub witnesses: Vec<RegionMembership>,
    pub tolerance: QueryTolerance,
}

impl PolygonTopology {
    /// Whether two supplied material witnesses remain connected. Missing or
    /// boundary-band witnesses are unresolved, not successful separation.
    pub fn connected(&self, first: usize, second: usize) -> Result<Option<bool>, QueryError> {
        let witness = |index: usize| {
            self.witnesses
                .get(index)
                .ok_or(QueryError::InvalidInput("unknown material witness"))
        };
        Ok(match (witness(first)?, witness(second)?) {
            (RegionMembership::Component(a), RegionMembership::Component(b)) => Some(a == b),
            _ => None,
        })
    }
}

/// Subtract a caller-supplied *filled swept removal region*, not a zero-width
/// line or inferred kerf. Supply an empty removal for pre-break connectivity.
/// Holes, cutouts, and disconnected retained islands are preserved. Call
/// `connected(a,b)` on witness pairs for desired retention and separation;
/// `Some(false)` means separated in this polygon model only. Construct swept
/// regions with geom/path's stroke_to_fill and the caller's explicit width/caps.
pub fn material_after_break(
    material: &ContourSet,
    removal: &ContourSet,
    witnesses: &[Point],
    tolerance: QueryTolerance,
) -> Result<PolygonTopology, QueryError> {
    tolerance.validate()?;
    validate_region(material)?;
    validate_region(removal)?;
    let retained = unfiltered(material).difference(&unfiltered(removal))?;
    let components = retained.connected_components();
    let prepared = components
        .iter()
        .map(ContourSet::prepare_query)
        .collect::<Vec<_>>();
    let witnesses = witnesses
        .iter()
        .map(|&p| membership(&prepared, p, tolerance))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(PolygonTopology {
        retained,
        components,
        witnesses,
        tolerance,
    })
}

#[cfg(test)]
mod tests;
