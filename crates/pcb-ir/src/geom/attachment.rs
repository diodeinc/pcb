//! Policy-free attachment geometry on the canonical regularized polygon model.
//!
//! All lengths are millimeters. Callers supply material, complete footprints,
//! obstacles (including component overhangs and forbidden holes), cutter entry
//! points, and swept break-removal regions. This module selects no tab pattern,
//! support count, fixture, or fracture model.
//!
//! # Scope of guarantees
//! Boundary stations and topology refer to the supplied [`ContourSet`], not its
//! pre-flattened curves. Flattening does not bound arc-length or tangent error,
//! and a Hausdorff error alone does not preserve topology. Region booleans use
//! the existing floating-point/integer overlay backend; these are not exact
//! predicates. Missing features already discarded by a caller's region
//! tolerance cannot be recovered. [`QueryTolerance`] is an explicit uncertainty
//! budget, not an independently certified bound on that backend. In particular,
//! [`PolygonTopology`] and [`CutterReachability`] are model results, **not**
//! certified source-curve topology, toolpath generation, or physical separation.

use super::dist::{self, Distance};
use super::region::{ring_edges, segment_inside_intervals};
use super::{Affine2, ContourSet, FillRule, Point, PreparedRegion};

/// Caller-supplied position uncertainty for each input boundary, and numerical
/// guard for comparisons. Include prior transforms, flattening, and offsets in
/// `boundary_mm`. Neither value is an arc-length, angular, or topology bound.
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
}

impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidInput(message) => write!(f, "invalid geometry query: {message}"),
            Self::Numerical(message) => write!(f, "numerical geometry failure: {message}"),
        }
    }
}

impl std::error::Error for QueryError {}

fn validate_region(region: &ContourSet) -> Result<(), QueryError> {
    if !region.tolerance.is_finite()
        || region.tolerance < 0.0
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
    ContourSet::from_regularized(region.rings.clone(), 0.0)
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

/// Non-wrapping polygon arc interval. An interval through the cyclic seam is
/// returned as two intervals, so no small interval needs to be dropped.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UsableInterval {
    pub boundary: BoundaryId,
    pub start_mm: f64,
    pub end_mm: f64,
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
        if components.contains(&usize::MAX) {
            return Err(QueryError::InvalidInput("unassigned boundary component"));
        }
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
                uncertainty_mm: self.tolerance.boundary_mm + self.tolerance.numerical_mm,
                first: point,
                second: closest,
            },
        })
    }

    /// Intersect every boundary edge with a caller-supplied admissible *site*
    /// region. Crossings, not a sampling grid, delimit intervals. These are
    /// polygon-model intervals; endpoints near uncertainty bands and source
    /// curve arc lengths are not certified. Isolated tangencies have no usable
    /// positive-length interval. Full-footprint clearance is a separate query.
    pub fn usable_intervals(
        &self,
        id: BoundaryId,
        admissible: &ContourSet,
    ) -> Result<Vec<UsableInterval>, QueryError> {
        let stations = self.index(id)?;
        validate_region(admissible)?;
        let mut result: Vec<UsableInterval> = Vec::new();
        for (edge, (a, b)) in ring_edges(&self.region.rings[id.ring]).enumerate() {
            let length = stations[edge + 1] - stations[edge];
            for (start, end) in segment_inside_intervals(admissible, a, b) {
                let start_mm = stations[edge] + start * length;
                let end_mm = stations[edge] + end * length;
                if end_mm <= start_mm {
                    return Err(QueryError::Numerical(
                        "usable interval below arc-length resolution",
                    ));
                }
                if let Some(previous) = result
                    .last_mut()
                    .filter(|previous| previous.end_mm == start_mm)
                {
                    previous.end_mm = end_mm;
                } else {
                    result.push(UsableInterval {
                        boundary: id,
                        start_mm,
                        end_mm,
                    });
                }
            }
        }
        Ok(result)
    }

    /// One representative midpoint per interval, with no truncation. This is
    /// not an enumeration of all feasible placements or an optimization grid.
    pub fn interval_site(&self, interval: UsableInterval) -> Result<BoundarySite, QueryError> {
        if interval.start_mm < 0.0
            || interval.end_mm > self.perimeter(interval.boundary)?
            || interval.start_mm >= interval.end_mm
        {
            return Err(QueryError::InvalidInput("invalid boundary interval"));
        }
        self.site(
            interval.boundary,
            interval.start_mm + (interval.end_mm - interval.start_mm) / 2.0,
        )
    }
}

/// Transform the polygon model with the existing IR affine convention. Rebuild
/// winding after reflections. The caller must scale its uncertainty budget by
/// the transform's largest singular value; arbitrary affine transforms do not
/// preserve stations, angles, or circular cutters.
pub fn transform_region(region: &ContourSet, transform: Affine2) -> Result<ContourSet, QueryError> {
    validate_region(region)?;
    if transform.inverse().is_none() {
        return Err(QueryError::InvalidInput("singular affine transform"));
    }
    let rings = region
        .rings
        .iter()
        .map(|ring| {
            ring.iter()
                .map(|p| {
                    let p = transform.transform_point(Point::new(p[0], p[1]));
                    [p.x, p.y]
                })
                .collect()
        })
        .collect();
    let raw = ContourSet::from_regularized(rings, 0.0);
    validate_region(&raw)?;
    Ok(ContourSet::new(raw.rings, FillRule::NonZero, 0.0))
}

#[derive(Debug, Clone, PartialEq)]
pub enum GeometricRejection {
    FootprintOverlap,
    InsufficientClearance { required_mm: f64, measured_mm: f64 },
    OutsideCutterSpace { point: usize },
    Unreachable { target: usize },
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FootprintPart {
    Attachment,
    RouterShoulder,
}

#[derive(Debug, Clone)]
pub struct FootprintCheck<'a> {
    pub part: FootprintPart,
    pub obstacle: &'a str,
    pub decision: Decision,
    pub boundary_distance: Option<Distance>,
    /// Entire overlapping region, useful for actionable diagnostics.
    pub overlap: ContourSet,
}

/// Check both complete filled footprints against every supplied obstacle. No
/// center-only or vertex-only acceptance; containment, holes and intersecting
/// edges are checked by filled-region intersection and all-edge distance.
/// Empty shoulders/obstacles are allowed; an empty attachment is invalid.
/// Touching within the numerical guard is unresolved, even for zero clearance.
/// A source-uncertain overlap is unresolved rather than a claim that flattened
/// curves prove collision. Supply holes as obstacles when occupying them is
/// forbidden; holes *in* an obstacle are free space under ContourSet semantics.
pub fn check_footprints<'a>(
    attachment: &ContourSet,
    router_shoulder: &ContourSet,
    obstacles: &[Obstacle<'a>],
    clearance_mm: f64,
    tolerance: QueryTolerance,
) -> Result<Vec<FootprintCheck<'a>>, QueryError> {
    tolerance.validate()?;
    if !clearance_mm.is_finite() || clearance_mm < 0.0 || attachment.is_empty() {
        return Err(QueryError::InvalidInput(
            "expected nonempty attachment and nonnegative clearance",
        ));
    }
    validate_region(attachment)?;
    validate_region(router_shoulder)?;
    let mut checks = Vec::new();
    for obstacle in obstacles {
        validate_region(obstacle.region)?;
        let prepared = obstacle.region.prepare_query();
        for (part, footprint) in [
            (FootprintPart::Attachment, attachment),
            (FootprintPart::RouterShoulder, router_shoulder),
        ] {
            let overlap = unfiltered(footprint).intersection(&unfiltered(obstacle.region));
            let bounds = footprint.bbox().union(obstacle.region.bbox());
            let reach = bounds.width().hypot(bounds.height());
            if !reach.is_finite() && !footprint.is_empty() && !obstacle.region.is_empty() {
                return Err(QueryError::Numerical("footprint bounds overflow"));
            }
            let distance = footprint
                .rings
                .iter()
                .flat_map(ring_edges)
                .filter_map(|(a, b)| prepared.segment_nearest_within(a, b, reach))
                .min_by(|a, b| a.mm.total_cmp(&b.mm))
                .map(|d| Distance {
                    uncertainty_mm: tolerance.pair_band(),
                    ..d
                });
            let decision = if !overlap.is_empty() {
                if tolerance.boundary_mm > 0.0 {
                    Decision::Unresolved(
                        "polygon footprints overlap; source-boundary uncertainty requires refinement",
                    )
                } else {
                    Decision::Rejected(GeometricRejection::FootprintOverlap)
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
            } else if footprint.is_empty() || obstacle.region.is_empty() {
                Decision::Admissible
            } else {
                return Err(QueryError::Numerical("missing footprint distance"));
            };
            checks.push(FootprintCheck {
                part,
                obstacle: obstacle.id,
                decision,
                boundary_distance: distance,
                overlap,
            });
        }
    }
    Ok(checks)
}

/// Witness classification in a polygon-model snapshot. Outside is geometric;
/// BoundaryBand requests refined geometry or a witness farther from the edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionMembership {
    Component(usize),
    Outside,
    BoundaryBand,
}

/// Polygon component membership with a guard at boundaries. This is shared by
/// cutter-space and retained-material queries; boundary witnesses never silently
/// count as material or provide a zero-width connecting ligament.
fn membership(
    components: &[PreparedRegion],
    point: Point,
    guard: f64,
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
        if d.mm.abs() <= guard {
            return Ok(RegionMembership::BoundaryBand);
        }
        if d.mm < 0.0 {
            return Ok(RegionMembership::Component(index));
        }
    }
    Ok(RegionMembership::Outside)
}

#[derive(Debug, Clone)]
pub struct CutterReachability {
    /// Explicit approximate disk-offset model, including all disconnected voids.
    pub center_space: ContourSet,
    /// Every supplied entry is classified; outside and uncertain entries do
    /// not silently become access points.
    pub entries: Vec<RegionMembership>,
    /// One decision for each target, in caller order.
    pub targets: Vec<Decision>,
    pub tolerance: QueryTolerance,
}

/// A disk cutter can translate between points in the same connected component
/// of free space eroded by its radius. `workspace` is a supplied bounded access
/// envelope, **not** automatically the board outline; obstacles include retained
/// material and other forbidden volumes projected into this 2D model. At least
/// one explicit entry is required. This checks planar translation only, not
/// plunge access, Z collision, shoulder shape, or a generated machining path.
/// Disk offsets and topology use the existing approximate region backend; the
/// returned center_space is reviewable and is not a certified exact disk erosion.
pub fn cutter_reachability(
    workspace: &ContourSet,
    obstacles: &ContourSet,
    radius_mm: f64,
    entries: &[Point],
    targets: &[Point],
    tolerance: QueryTolerance,
) -> Result<CutterReachability, QueryError> {
    tolerance.validate()?;
    validate_region(workspace)?;
    validate_region(obstacles)?;
    if !radius_mm.is_finite() || radius_mm <= 0.0 || entries.is_empty() {
        return Err(QueryError::InvalidInput(
            "expected positive cutter radius and explicit entries",
        ));
    }
    let center_space = unfiltered(workspace)
        .difference(&unfiltered(obstacles))
        .disk_erode(radius_mm);
    let components = center_space
        .connected_components()
        .iter()
        .map(ContourSet::prepare_query)
        .collect::<Vec<_>>();
    let entry_components = entries
        .iter()
        .map(|&p| membership(&components, p, tolerance.pair_band()))
        .collect::<Result<Vec<_>, _>>()?;
    let targets = targets
        .iter()
        .enumerate()
        .map(|(target, &p)| {
            Ok(match membership(&components, p, tolerance.pair_band())? {
                RegionMembership::Component(component)
                    if entry_components.contains(&RegionMembership::Component(component)) =>
                {
                    Decision::Admissible
                }
                RegionMembership::Component(_)
                    if entry_components.contains(&RegionMembership::BoundaryBand) =>
                {
                    Decision::Unresolved("entry is within the cutter-space uncertainty band")
                }
                RegionMembership::Component(_) => {
                    Decision::Rejected(GeometricRejection::Unreachable { target })
                }
                RegionMembership::Outside => {
                    Decision::Rejected(GeometricRejection::OutsideCutterSpace { point: target })
                }
                RegionMembership::BoundaryBand => {
                    Decision::Unresolved("target is within the cutter-space uncertainty band")
                }
            })
        })
        .collect::<Result<Vec<_>, QueryError>>()?;
    Ok(CutterReachability {
        center_space,
        entries: entry_components,
        targets,
        tolerance,
    })
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
        let a = self
            .witnesses
            .get(first)
            .ok_or(QueryError::InvalidInput("unknown material witness"))?;
        let b = self
            .witnesses
            .get(second)
            .ok_or(QueryError::InvalidInput("unknown material witness"))?;
        Ok(match (a, b) {
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
    let retained = unfiltered(material).difference(&unfiltered(removal));
    let components = retained.connected_components();
    let prepared = components
        .iter()
        .map(ContourSet::prepare_query)
        .collect::<Vec<_>>();
    let witnesses = witnesses
        .iter()
        .map(|&p| membership(&prepared, p, tolerance.pair_band()))
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
