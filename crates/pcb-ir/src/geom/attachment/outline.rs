//! Clearance of a boundary-following band on an immutable polygon substrate.
//!
//! Width is arclength, not a tangent rectangle's width. Each included edge
//! contributes its normal strip; triangular bevel joins connect strips at
//! vertices. Pointwise failures expand cyclically by half the requested width.
//! Thus a band crosses polygon vertices without requiring an arbitrary radius
//! threshold or changing the board. Collapsed offsets remain unknown.
//!
//! Results apply to the prepared polygon model. They do not certify source
//! curve topology, frame connectivity, router access, perforations or strength.

use super::{BoundaryId, BoundaryQuery, QueryError, QueryTolerance};
use crate::geom::region::ring_edges;
use crate::geom::{BBox, ContourSet, FillRule, Point};

/// Explicit millimeters; no manufacturing allowances are inferred.
#[derive(Debug, Clone, Copy)]
pub struct OutlineFootprint {
    /// Total cyclic boundary span, including both sides of the centre.
    pub width_mm: f64,
    pub inward_mm: f64,
    pub outward_mm: f64,
}

/// Missing or empty evidence cannot establish clearance. Omit an obstacle only
/// when the caller explicitly decides it is not applicable (e.g. a board feature).
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

/// Open interval on one original polygon edge. Endpoints carry no guarantee.
#[derive(Debug, Clone)]
pub struct OutlineInterval {
    pub boundary: BoundaryId,
    pub edge: usize,
    pub start_mm: f64,
    pub end_mm: f64,
    pub start: Point,
    pub end: Point,
    pub state: OutlineState,
    /// Inward containment, or Unknown if offset geometry folds/collapses.
    pub landing: OutlineState,
    /// Indices into the caller's obstacles, preserving all contributing sources.
    pub obstacles: Vec<usize>,
    pub uncertainty_mm: f64,
}

struct Span {
    lo: f64,
    hi: f64,
    obstacle: Option<usize>,
    certain: bool,
}

/// Partition all rings without changing their geometry or provenance.
///
/// Expanded/contracted depths and arclength spans bracket positional uncertainty.
/// The band is anchored to the polygon boundary; it is not a full 3D collision
/// test. Missing evidence is unknown; a resolved blocker takes precedence.
pub fn eligible_outline(
    substrate: &ContourSet,
    obstacles: &[OutlineObstacle<'_>],
    footprint: OutlineFootprint,
    tolerance: QueryTolerance,
) -> Result<Vec<OutlineInterval>, QueryError> {
    if !footprint.width_mm.is_finite()
        || footprint.width_mm <= 0.0
        || !footprint.inward_mm.is_finite()
        || footprint.inward_mm < 0.0
        || !footprint.outward_mm.is_finite()
        || footprint.outward_mm < 0.0
        || !(footprint.inward_mm + footprint.outward_mm).is_finite()
        || footprint.inward_mm + footprint.outward_mm <= 0.0
    {
        return Err(QueryError::InvalidInput("invalid outline footprint"));
    }
    let boundary = BoundaryQuery::new(substrate, tolerance)?;
    if substrate.is_empty() {
        return Err(QueryError::InvalidInput("missing board substrate"));
    }
    let mut missing = Vec::new();
    for (i, obstacle) in obstacles.iter().enumerate() {
        if let Some(region) = obstacle.region.filter(|r| !r.is_empty()) {
            super::validate_region(region)?;
        } else {
            missing.push(i);
        }
    }
    let band = std::iter::once(substrate)
        .chain(obstacles.iter().filter_map(|o| o.region))
        .map(|r| r.budget().max_error_mm().max(tolerance.boundary_mm))
        .fold(0.0, f64::max)
        + substrate.uncertainty_mm.max(tolerance.boundary_mm)
        + tolerance.numerical_mm;
    if !band.is_finite()
        || !(footprint.width_mm + 2.0 * band).is_finite()
        || !(footprint.inward_mm + footprint.outward_mm + band).is_finite()
    {
        return Err(QueryError::InvalidInput("outline uncertainty overflow"));
    }
    let mut result: Vec<OutlineInterval> = Vec::new();
    for id in boundary.boundaries() {
        let edges = ring_edges(&substrate.rings[id.ring]).collect::<Vec<_>>();
        let perimeter = boundary.perimeter(id)?;
        let mut spans = Vec::new();
        let mut station = 0.0;
        for (edge, &(start, end)) in edges.iter().enumerate() {
            let delta = end - start;
            let length = delta.length();
            let t = delta / length;
            let n = Point::new(t.y, -t.x);
            let (prev, _) = edges[(edge + edges.len() - 1) % edges.len()];
            let (_, next) = edges[(edge + 1) % edges.len()];
            let incoming = (start - prev) / start.distance_to(prev);
            let outgoing = (next - end) / next.distance_to(end);
            let prev_n = Point::new(incoming.y, -incoming.x);
            let next_n = Point::new(outgoing.y, -outgoing.x);
            let local = edge_frame(substrate, start, delta)?;
            let local_obstacles = obstacles
                .iter()
                .map(|o| {
                    o.region
                        .filter(|r| !r.is_empty())
                        .map(|r| edge_frame(r, start, delta))
                        .transpose()
                })
                .collect::<Result<Vec<_>, _>>()?;
            for (certain, padding) in [(false, band), (true, -band)] {
                let half = (footprint.width_mm / 2.0 + padding).max(0.0);
                let inward = (footprint.inward_mm + padding).max(0.0);
                let outward = (footprint.outward_mm + padding).max(0.0);
                let joins = [
                    bevel(&local, prev_n, t, n, -inward)?,
                    bevel(&local, prev_n, t, n, outward)?,
                ];
                let mut add = |lo, hi, obstacle, certain| {
                    expand_span(
                        &mut spans,
                        station + lo,
                        station + hi,
                        half,
                        perimeter,
                        obstacle,
                        certain,
                    );
                };
                // Miter offset endpoints must remain ordered. A folded offset
                // is unsupported geometry, not proof of a mechanical failure.
                let a = 1.0 + dot(prev_n, n);
                let b = 1.0 + dot(next_n, n);
                let slope = if a > 0.0 && b > 0.0 {
                    dot(next_n, t) / b - dot(prev_n, t) / a
                } else {
                    f64::INFINITY
                };
                if footprint.width_mm + 2.0 * band >= perimeter
                    || a <= 0.0
                    || b <= 0.0
                    || length - inward * slope <= 0.0
                    || length + outward * slope <= 0.0
                {
                    add(0.0, length, None, false);
                }
                if footprint.inward_mm > 0.0 && inward > 0.0 {
                    let strip = rectangle(&local, length, -inward, 0.0);
                    let mut void = strip.difference(&local)?;
                    if certain {
                        void = void.disk_erode(band)?;
                    }
                    for (lo, hi) in projections(&void) {
                        add(lo, hi, None, certain);
                    }
                    let mut void = joins[0].difference(&local)?;
                    if certain {
                        void = void.disk_erode(band)?;
                    }
                    if !void.is_empty() {
                        add(0.0, 0.0, None, certain);
                    }
                    // At a sharp join a normal may touch another board edge.
                    // Such contact is unresolved, including exact right angles;
                    // numerical slivers must not turn it into a definite block.
                    if !certain
                        && local
                            .prepare_query()
                            .signed_distance(Point::new(0.0, -inward))
                            .is_none_or(|d| d.mm >= -band)
                    {
                        add(0.0, 0.0, None, false);
                    }
                }
                for (i, obstacle) in local_obstacles.iter().enumerate() {
                    let Some(obstacle) = obstacle else {
                        continue;
                    };
                    for (lo, hi) in strip_contacts(obstacle, length, -inward, outward)? {
                        add(lo, hi, Some(i), certain);
                    }
                    for join in &joins {
                        if intersects_closed(obstacle, join)? {
                            add(0.0, 0.0, Some(i), certain);
                        }
                    }
                }
            }
            station += length;
        }
        station = 0.0;
        for (edge, &(start, end)) in edges.iter().enumerate() {
            let length = start.distance_to(end);
            let mut cuts = vec![station, station + length];
            for span in &spans {
                cuts.extend(
                    [span.lo, span.hi]
                        .into_iter()
                        .filter(|s| *s > station && *s < station + length),
                );
            }
            cuts.sort_by(f64::total_cmp);
            cuts.dedup();
            for pair in cuts.windows(2) {
                let mut sources = missing.clone();
                let mut landing = OutlineState::Eligible;
                let mut blocked = false;
                for span in &spans {
                    // All span endpoints are partition cuts: overlap means
                    // coverage of this open interval, without a sample point.
                    if span.lo < pair[1] && span.hi > pair[0] {
                        blocked |= span.certain;
                        if let Some(i) = span.obstacle {
                            sources.push(i);
                        } else if span.certain {
                            landing = OutlineState::Blocked;
                        } else if landing == OutlineState::Eligible {
                            landing = OutlineState::Unknown;
                        }
                    }
                }
                sources.sort_unstable();
                sources.dedup();
                let interval = OutlineInterval {
                    boundary: id,
                    edge,
                    start_mm: pair[0],
                    end_mm: pair[1],
                    start: start + (end - start) * ((pair[0] - station) / length),
                    end: start + (end - start) * ((pair[1] - station) / length),
                    state: if blocked {
                        OutlineState::Blocked
                    } else if sources.is_empty() && landing == OutlineState::Eligible {
                        OutlineState::Eligible
                    } else {
                        OutlineState::Unknown
                    },
                    landing,
                    obstacles: sources,
                    uncertainty_mm: band,
                };
                // Keep source edges, but discard redundant internal cuts.
                if let Some(previous) = result.last_mut().filter(|p| {
                    p.boundary == id
                        && p.edge == edge
                        && p.end_mm == interval.start_mm
                        && p.state == interval.state
                        && p.landing == landing
                        && p.obstacles == interval.obstacles
                }) {
                    previous.end_mm = interval.end_mm;
                    previous.end = interval.end;
                } else {
                    result.push(interval);
                }
            }
            station += length;
        }
    }
    Ok(result)
}

// Relative cross products keep attachment endpoints exactly on y=0 without
// snapping or losing the source preparation history.
fn edge_frame(region: &ContourSet, start: Point, delta: Point) -> Result<ContourSet, QueryError> {
    let length = delta.length();
    let rings = region
        .rings
        .iter()
        .map(|ring| {
            ring.iter()
                .map(|&[x, y]| {
                    let p = Point::new(x, y) - start;
                    [
                        dot(delta, p) / length,
                        (delta.y * p.x - delta.x * p.y) / length,
                    ]
                })
                .collect()
        })
        .collect();
    let mut local = ContourSet::from_rings(rings, FillRule::NonZero, region.resolution.strict())?;
    local.uncertainty_mm = local.uncertainty_mm.max(region.uncertainty_mm);
    local.budget().check(local.uncertainty_mm)?;
    Ok(local)
}

fn rectangle(region: &ContourSet, length: f64, bottom: f64, top: f64) -> ContourSet {
    ContourSet::rectangle(
        BBox::new(Point::new(0.0, bottom), Point::new(length, top)),
        region.resolution.strict(),
    )
}

fn bevel(
    region: &ContourSet,
    previous: Point,
    t: Point,
    n: Point,
    depth: f64,
) -> Result<ContourSet, QueryError> {
    Ok(ContourSet::from_rings(
        vec![vec![
            [0.0, 0.0],
            [depth * dot(previous, t), depth * dot(previous, n)],
            [0.0, depth],
        ]],
        FillRule::NonZero,
        region.resolution.strict(),
    )?)
}

fn dot(a: Point, b: Point) -> f64 {
    a.x * b.x + a.y * b.y
}

fn projections(region: &ContourSet) -> Vec<(f64, f64)> {
    region
        .connected_components()
        .iter()
        .map(|r| (r.bbox().min.x, r.bbox().max.x))
        .collect()
}

// Regularized booleans discard line/point contacts. Clip original edges to the
// CLOSED strip as well: even a point contact blocks a nonzero centre interval.
fn strip_contacts(
    region: &ContourSet,
    length: f64,
    bottom: f64,
    top: f64,
) -> Result<Vec<(f64, f64)>, QueryError> {
    if bottom >= top {
        return Ok(Vec::new());
    }
    let mut spans = projections(&region.intersection(&rectangle(region, length, bottom, top))?);
    for (a, b) in region.rings.iter().flat_map(ring_edges) {
        let mut lo: f64 = 0.0;
        let mut hi: f64 = 1.0;
        for (v, d, min, max) in [(a.x, b.x - a.x, 0.0, length), (a.y, b.y - a.y, bottom, top)] {
            if d == 0.0 {
                if v < min || v > max {
                    hi = -1.0;
                }
            } else {
                let p = (min - v) / d;
                let q = (max - v) / d;
                lo = lo.max(p.min(q));
                hi = hi.min(p.max(q));
            }
        }
        if lo <= hi {
            let p = a.x + lo * (b.x - a.x);
            let q = a.x + hi * (b.x - a.x);
            spans.push((p.min(q), p.max(q)));
        }
    }
    Ok(spans)
}

fn intersects_closed(a: &ContourSet, b: &ContourSet) -> Result<bool, QueryError> {
    if b.is_empty() {
        return Ok(false);
    }
    if !a.intersection(b)?.is_empty() {
        return Ok(true);
    }
    Ok(a.rings.iter().flat_map(ring_edges).any(|(p, q)| {
        b.rings
            .iter()
            .flat_map(ring_edges)
            .any(|(u, v)| crate::geom::dist::segments(p, q, u, v).0 == 0.0)
    }))
}

fn expand_span(
    spans: &mut Vec<Span>,
    lo: f64,
    hi: f64,
    half: f64,
    perimeter: f64,
    obstacle: Option<usize>,
    certain: bool,
) {
    if hi - lo + 2.0 * half >= perimeter {
        spans.push(Span {
            lo: 0.0,
            hi: perimeter,
            obstacle,
            certain,
        });
        return;
    }
    let start = (lo - half).rem_euclid(perimeter);
    let end = start + hi - lo + 2.0 * half;
    spans.push(Span {
        lo: start,
        hi: end.min(perimeter),
        obstacle,
        certain,
    });
    if end > perimeter {
        spans.push(Span {
            lo: 0.0,
            hi: end - perimeter,
            obstacle,
            certain,
        });
    }
}

#[cfg(test)]
mod tests;
