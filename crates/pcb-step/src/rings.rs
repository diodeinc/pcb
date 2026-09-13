//! Rings from a 2D boolean, cleaned and turned into loops of lines and
//! arcs that OCCT reads as manifold shells.

use std::f64::consts::PI;

use pcb_ir::geom::{ContourBuf, ContourSet};

use crate::geom::{Vec2, circle_center, signed_area};
use crate::outline::{Edge, Loop, intersect, orient};

/// Points this close to a circle are refitted onto it. Polygonized arcs
/// have their vertices on the circle to well under a micrometre, so this
/// only has to cover the boolean's coordinate rounding.
const REFIT_TOLERANCE: f64 = 0.001;

/// Vertices closer than this are one vertex, and a ring passing through
/// the same vertex twice is split there. This is above OCCT's read
/// precision, so every edge that survives is one OCCT keeps too.
const VERTEX_TOLERANCE: f64 = 2e-4;

/// The rings of contours as point lists, with vertices closer than
/// `VERTEX_TOLERANCE` merged.
fn raw_rings(contours: &[ContourBuf]) -> Vec<Vec<Vec2>> {
    let mut rings = Vec::new();
    for contour in contours {
        let mut points: Vec<Vec2> = Vec::with_capacity(contour.cmds.len());
        for c in &contour.cmds {
            if !matches!(
                c.op,
                pcb_ir::geom::PathOp::MoveTo | pcb_ir::geom::PathOp::LineTo
            ) {
                continue;
            }
            let p = Vec2::new(c.p0.x, c.p0.y);
            if points
                .last()
                .is_none_or(|q| q.distance(p) > VERTEX_TOLERANCE)
            {
                points.push(p);
            }
        }
        while points.len() > 1 && points[0].distance(points[points.len() - 1]) <= VERTEX_TOLERANCE {
            points.pop();
        }
        if points.len() >= 3 {
            rings.push(points);
        }
    }
    rings
}

/// The rings of a boolean's output as point lists.
pub(crate) fn polygons_of(region: &ContourSet) -> Vec<Vec<Vec2>> {
    raw_rings(&region.to_contours())
}

/// The rings of a boolean's output, cleaned: pinched rings are split,
/// near-collinear vertices dropped, and slivers discarded.
pub(crate) fn rings_of(contours: &[ContourBuf]) -> Vec<Vec<Vec2>> {
    let mut rings: Vec<Vec<Vec2>> = Vec::new();
    for points in raw_rings(contours) {
        split_pinches(points, &mut rings);
    }
    for ring in &mut rings {
        simplify_ring(ring);
    }
    // A ring under a square micrometre is a sliver the boolean left
    // behind, whose winding OCCT cannot even tell.
    rings.retain(|r| r.len() >= 3 && signed_area(r).abs() > 1e-6);
    rings
}

/// One `(outer, holes)` per island of a region, with arcs refitted.
pub(crate) fn loops_of(region: &ContourSet) -> Vec<(Loop, Vec<Loop>)> {
    nested_of(region).iter().map(|n| fit(n, 1)).collect()
}

/// An island of a region as point lists: its outer ring and its holes.
pub(crate) struct Nested {
    pub(crate) outer: Vec<Vec2>,
    pub(crate) holes: Vec<Vec<Vec2>>,
}

/// The islands of a region: every outer and hole a simple loop, so the
/// extruded shell is manifold.
pub(crate) fn nested_of(region: &ContourSet) -> Vec<Nested> {
    nest(rings_of(&region.to_contours()))
}

/// Cleaned rings sorted into islands. Nesting depth decides outer versus
/// hole; a hole belongs to the smallest ring containing it.
pub(crate) fn nest(rings: Vec<Vec<Vec2>>) -> Vec<Nested> {
    let n = rings.len();
    let areas: Vec<f64> = rings.iter().map(|r| signed_area(r).abs()).collect();
    let indexed: Vec<IndexedRing> = rings.iter().map(|r| IndexedRing::new(r)).collect();
    let mut parent = vec![usize::MAX; n];
    let mut depth = vec![0usize; n];
    for i in 0..n {
        let probe = rings[i][0];
        let mut best = f64::INFINITY;
        for j in 0..n {
            if i != j && areas[j] > areas[i] && indexed[j].contains(&rings[j], probe) {
                depth[i] += 1;
                if areas[j] < best {
                    best = areas[j];
                    parent[i] = j;
                }
            }
        }
    }
    let mut islands: Vec<Nested> = Vec::new();
    let mut island_of = vec![usize::MAX; n];
    let mut holes: Vec<(usize, Vec<Vec2>)> = Vec::new();
    for (i, ring) in rings.into_iter().enumerate() {
        if depth[i].is_multiple_of(2) {
            island_of[i] = islands.len();
            islands.push(Nested {
                outer: ring,
                holes: Vec::new(),
            });
        } else {
            holes.push((parent[i], ring));
        }
    }
    for (parent, ring) in holes {
        let owner = island_of[parent];
        if owner != usize::MAX {
            islands[owner].holes.push(ring);
        }
    }
    islands
}

/// The islands of a region with the boolean's vertices kept as they are,
/// for callers that snap them back onto curves they know.
pub(crate) fn raw_islands(region: &ContourSet) -> Vec<Nested> {
    let mut rings: Vec<Vec<Vec2>> = Vec::new();
    for points in polygons_of(region) {
        split_pinches(points, &mut rings);
    }
    rings.retain(|r| r.len() >= 3 && signed_area(r).abs() > 1e-6);
    nest(rings)
}

/// The loops of one island with arcs refitted, the outer counter-clockwise
/// and the holes clockwise. Rings are refitted on `threads` threads.
pub(crate) fn fit(island: &Nested, threads: usize) -> (Loop, Vec<Loop>) {
    let rings: Vec<&[Vec2]> = std::iter::once(island.outer.as_slice())
        .chain(island.holes.iter().map(Vec::as_slice))
        .collect();
    let pieces = crate::parallel_map(if rings.len() >= 64 { threads } else { 1 }, &rings, |r| {
        refit_pieces(r)
    });
    let refs: Vec<&[Piece]> = pieces.iter().map(Vec::as_slice).collect();
    let demote = crossing_arcs(&refs);
    let mut loops = rings
        .iter()
        .zip(pieces)
        .zip(&demote)
        .enumerate()
        .map(|(i, ((ring, pieces), d))| Loop::new(orient(expand_pieces(ring, pieces, d), i == 0)));
    let outer = loops.next().unwrap();
    (outer, loops.collect())
}

/// Drop vertices that lie within `SIMPLIFY_TOLERANCE` of the line through
/// their neighbours, as KiCad's `SimplifyOutlines` does before it refits
/// arcs. Flattened arcs keep their chords since those deviate more.
fn simplify_ring(ring: &mut Vec<Vec2>) {
    const SIMPLIFY_TOLERANCE: f64 = 0.002;
    if ring.len() < 4 {
        return;
    }
    let n = ring.len();
    let mut kept: Vec<Vec2> = Vec::with_capacity(n);
    for i in 0..n {
        let prev = *kept.last().unwrap_or(&ring[n - 1]);
        let next = ring[(i + 1) % n];
        let p = ring[i];
        let d = next - prev;
        let len = d.length();
        let deviation = if len < 1e-9 {
            p.distance(prev)
        } else {
            d.perp_dot(p - prev).abs() / len
        };
        // Only a vertex lying between its neighbours is dropped; a spike
        // folding back on itself is kept.
        let between = (p - prev).dot(next - p) > 0.0;
        if deviation > SIMPLIFY_TOLERANCE || !between {
            kept.push(p);
        }
    }
    if kept.len() >= 3 {
        *ring = kept;
    }
}

/// A ring's edges bucketed by horizontal band, so a point test on a
/// plane with thousands of holes only visits the edges at its height.
pub(crate) struct IndexedRing {
    pub(crate) lo: Vec2,
    pub(crate) hi: Vec2,
    /// Edge indices per band; empty for short rings, which are scanned.
    bands: Vec<Vec<u32>>,
    inv_band: f64,
}

impl IndexedRing {
    const SCAN_BELOW: usize = 64;

    pub(crate) fn new(ring: &[Vec2]) -> Self {
        let mut lo = Vec2::splat(f64::INFINITY);
        let mut hi = Vec2::splat(f64::NEG_INFINITY);
        for p in ring {
            lo = lo.min(*p);
            hi = hi.max(*p);
        }
        let n = ring.len();
        let mut bands = Vec::new();
        let mut inv_band = 0.0;
        if n >= Self::SCAN_BELOW && hi.y > lo.y {
            let count = (n / 8).clamp(1, 4096);
            inv_band = count as f64 / (hi.y - lo.y);
            bands = vec![Vec::new(); count];
            for k in 0..n {
                let (a, b) = (ring[k], ring[(k + 1) % n]);
                let first = (((a.y.min(b.y) - lo.y) * inv_band) as usize).min(count - 1);
                let last = (((a.y.max(b.y) - lo.y) * inv_band) as usize).min(count - 1);
                for band in &mut bands[first..=last] {
                    band.push(k as u32);
                }
            }
        }
        Self {
            lo,
            hi,
            bands,
            inv_band,
        }
    }

    /// The ring's edges whose bounding box meets `(lo, hi)`.
    pub(crate) fn edges_meeting(&self, ring: &[Vec2], (lo, hi): (Vec2, Vec2)) -> Vec<(Vec2, Vec2)> {
        let mut out = Vec::new();
        if lo.x > self.hi.x || hi.x < self.lo.x || lo.y > self.hi.y || hi.y < self.lo.y {
            return out;
        }
        let n = ring.len();
        let mut take = |k: usize| {
            let (a, b) = (ring[k], ring[(k + 1) % n]);
            if a.min(b).x <= hi.x && lo.x <= a.max(b).x && a.min(b).y <= hi.y && lo.y <= a.max(b).y
            {
                out.push((a, b));
            }
        };
        if self.bands.is_empty() {
            (0..n).for_each(&mut take);
            return out;
        }
        let band = |y: f64| (((y - self.lo.y) * self.inv_band) as usize).min(self.bands.len() - 1);
        let (first, last) = (band(lo.y.max(self.lo.y)), band(hi.y.min(self.hi.y)));
        for bi in first..=last {
            for &k in &self.bands[bi] {
                // An edge lies in every band it spans; take it once,
                // from its first band inside the window.
                let (a, b) = (ring[k as usize], ring[(k as usize + 1) % n]);
                let top = band(a.y.min(b.y));
                if top == bi || (top < first && bi == first) {
                    take(k as usize);
                }
            }
        }
        out
    }

    pub(crate) fn contains(&self, ring: &[Vec2], p: Vec2) -> bool {
        if p.x < self.lo.x || p.x > self.hi.x || p.y < self.lo.y || p.y > self.hi.y {
            return false;
        }
        if self.bands.is_empty() {
            return crate::geom::point_in_polygon(p, ring);
        }
        let band = (((p.y - self.lo.y) * self.inv_band) as usize).min(self.bands.len() - 1);
        let n = ring.len();
        let mut inside = false;
        for &k in &self.bands[band] {
            let a = ring[k as usize];
            let b = ring[(k as usize + 1) % n];
            if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
                inside = !inside;
            }
        }
        inside
    }
}

/// Split a ring at every vertex it passes through twice.
fn split_pinches(points: Vec<Vec2>, out: &mut Vec<Vec<Vec2>>) {
    let n = points.len();
    if n < 6 {
        out.push(points);
        return;
    }
    // Sort vertices to find coincident pairs without a quadratic scan.
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|a, b| points[*a].x.total_cmp(&points[*b].x));
    for w in 0..n {
        let i = order[w];
        for &j in &order[w + 1..] {
            if points[j].x - points[i].x > VERTEX_TOLERANCE {
                break;
            }
            if points[i].distance(points[j]) <= VERTEX_TOLERANCE {
                let (lo, hi) = (i.min(j), i.max(j));
                let inner: Vec<Vec2> = points[lo..hi].to_vec();
                let mut rest: Vec<Vec2> = points[..lo].to_vec();
                rest.extend_from_slice(&points[hi..]);
                split_pinches(inner, out);
                split_pinches(rest, out);
                return;
            }
        }
    }
    out.push(points);
}

/// Replace runs of chords that lie on one circle with an arc, the way
/// KiCad refits its polygonized copper before writing STEP.
///
/// Only arcs that look like a polygonized arc are taken: a bounded radius,
/// evenly spaced chords turning the same way, and at least four of them.
/// A run is found with a circle through three neighbouring vertices, which
/// is a poor estimate, then refitted through its ends and middle and
/// checked tightly, so vertices that merely pass near a circle stay
/// chords. A polygon that lies entirely on one circle becomes a single
/// circle.
#[cfg(test)]
pub(crate) fn refit_arcs(points: &[Vec2]) -> Vec<Edge> {
    let pieces = refit_pieces(points);
    let demote = crossing_arcs(&[&pieces]);
    expand_pieces(points, pieces, &demote[0])
}

/// A piece of a refitted ring: an edge and the range of ring vertices it
/// stands for, so an arc can be given back its chords.
#[derive(Clone, Copy)]
struct Piece {
    edge: Edge,
    start: usize,
    end: usize,
}

fn refit_pieces(points: &[Vec2]) -> Vec<Piece> {
    const MAX_RADIUS: f64 = 25.0;
    const MIN_STEP: f64 = 0.5 * PI / 180.0;
    /// Slack for the first estimate of a run's circle.
    const COARSE_TOLERANCE: f64 = 0.004;
    let n = points.len();
    let chord = |i: usize| Piece {
        edge: Edge::Line {
            a: points[i],
            b: points[(i + 1) % n],
        },
        start: i,
        end: i + 1,
    };
    if n < 5 {
        return (0..n).map(chord).collect();
    }
    // Chord step angle about a candidate centre, or None if the chord is
    // not on the circle.
    let step = |c: Vec2, r: f64, k: usize, tolerance: f64| -> Option<f64> {
        let p = points[k % n];
        let q = points[(k + 1) % n];
        if (p.distance(c) - r).abs() > tolerance || (q.distance(c) - r).abs() > tolerance {
            return None;
        }
        let turn = (p - c).perp_dot(q - c);
        let angle = turn.atan2((p - c).dot(q - c));
        (angle.abs() >= MIN_STEP).then_some(angle)
    };
    let uniform = |angles: &[f64]| {
        let first = angles[0];
        angles.iter().all(|a| {
            a.signum() == first.signum() && (a.abs() - first.abs()).abs() <= 0.2 * first.abs()
        })
    };
    // The last vertex of the uniform run on the circle starting at `i`,
    // and the run's turning direction.
    let run = |c: Vec2, r: f64, i: usize, tolerance: f64| -> (usize, bool) {
        let mut angles = Vec::new();
        let mut end = i;
        while end + 1 < n {
            let Some(angle) = step(c, r, end, tolerance) else {
                break;
            };
            angles.push(angle);
            // Within a half turn: the corners of a rectangle share a
            // circle too, and three of its sides would pass as an arc.
            if !uniform(&angles) || angles.iter().sum::<f64>().abs() > PI + 1e-9 {
                angles.pop();
                break;
            }
            end += 1;
        }
        (end, angles.first().is_some_and(|a| *a > 0.0))
    };

    // Whole polygon on one circle.
    if n >= 8
        && let Some(c) = circle_center(points[0], points[n / 3], points[2 * n / 3])
    {
        let r = points[0].distance(c);
        if r <= MAX_RADIUS {
            let angles: Option<Vec<f64>> = (0..n).map(|k| step(c, r, k, REFIT_TOLERANCE)).collect();
            if let Some(angles) = angles
                && uniform(&angles)
            {
                let p = points[0];
                return vec![Piece {
                    edge: Edge::Arc {
                        a: p,
                        b: p,
                        c,
                        ccw: angles[0] > 0.0,
                    },
                    start: 0,
                    end: n,
                }];
            }
        }
    }

    let mut pieces: Vec<Piece> = Vec::with_capacity(n);
    let mut i = 0;
    while i < n {
        let mut best: Option<(usize, Edge)> = None;
        if i + 3 < n
            && let Some(coarse) = circle_center(points[i], points[i + 1], points[i + 3])
            && points[i].distance(coarse) <= MAX_RADIUS
        {
            let (end, _) = run(coarse, points[i].distance(coarse), i, COARSE_TOLERANCE);
            if end - i >= 3
                && let Some(c) = circle_center(points[i], points[(i + end) / 2], points[end])
            {
                let r = points[i].distance(c);
                let (end, ccw) = run(c, r, i, REFIT_TOLERANCE);
                if end - i >= 3 {
                    let (a, b) = (points[i], points[end]);
                    best = Some((
                        end,
                        Edge::Arc {
                            a,
                            b,
                            c: centered(a, b, c, r),
                            ccw,
                        },
                    ));
                }
            }
        }
        match best {
            Some((end, edge)) => {
                pieces.push(Piece {
                    edge,
                    start: i,
                    end,
                });
                i = end;
            }
            None => {
                pieces.push(chord(i));
                i += 1;
            }
        }
    }
    merge_arcs(pieces)
}

/// The edges of a refitted ring, with the demoted pieces as chords.
fn expand_pieces(points: &[Vec2], pieces: Vec<Piece>, demote: &[bool]) -> Vec<Edge> {
    let n = points.len();
    let mut edges = Vec::with_capacity(n);
    for (k, piece) in pieces.into_iter().enumerate() {
        if demote[k] {
            for j in piece.start..piece.end {
                edges.push(Edge::Line {
                    a: points[j],
                    b: points[(j + 1) % n],
                });
            }
        } else {
            edges.push(piece.edge);
        }
    }
    edges
}

/// Per ring, the arcs that cross another edge of the solid anywhere but
/// a shared vertex. The chords of a boolean's output never cross, but an
/// arc bulges out to the circle they were cut from, and where two
/// features pass within that sagitta of each other (two fits of one
/// circle, a corner a few micrometres from a hole, two holes nearly
/// touching) the arcs do cross; those go back to chords.
fn crossing_arcs(rings: &[&[Piece]]) -> Vec<Vec<bool>> {
    let mut demote: Vec<Vec<bool>> = rings.iter().map(|r| vec![false; r.len()]).collect();
    // Every piece of every ring as `(ring, index)`, on a grid of cells.
    let flat: Vec<(usize, usize)> = rings
        .iter()
        .enumerate()
        .flat_map(|(j, r)| (0..r.len()).map(move |k| (j, k)))
        .collect();
    let piece = |f: usize| -> &Piece { &rings[flat[f].0][flat[f].1] };
    if !flat
        .iter()
        .any(|&(j, k)| matches!(rings[j][k].edge, Edge::Arc { .. }))
    {
        return demote;
    }
    let boxes: Vec<(Vec2, Vec2)> = (0..flat.len()).map(|f| piece(f).edge.bbox()).collect();
    let lo = boxes
        .iter()
        .fold(Vec2::splat(f64::INFINITY), |m, b| m.min(b.0));
    let hi = boxes
        .iter()
        .fold(Vec2::splat(f64::NEG_INFINITY), |m, b| m.max(b.1));
    let cells = ((flat.len() as f64).sqrt() as usize).clamp(1, 128);
    let extent = (hi - lo).max(Vec2::splat(1e-9));
    let cell_of = |p: Vec2| -> (usize, usize) {
        let c = ((p - lo) / extent * cells as f64).floor();
        (
            (c.x.max(0.0) as usize).min(cells - 1),
            (c.y.max(0.0) as usize).min(cells - 1),
        )
    };
    let mut grid: Vec<Vec<u32>> = vec![Vec::new(); cells * cells];
    for (f, b) in boxes.iter().enumerate() {
        let (x0, y0) = cell_of(b.0);
        let (x1, y1) = cell_of(b.1);
        for y in y0..=y1 {
            for x in x0..=x1 {
                grid[y * cells + x].push(f as u32);
            }
        }
    }
    let mut seen = vec![usize::MAX; flat.len()];
    for f in 0..flat.len() {
        let Edge::Arc { .. } = piece(f).edge else {
            continue;
        };
        let (rf, kf) = flat[f];
        let (x0, y0) = cell_of(boxes[f].0);
        let (x1, y1) = cell_of(boxes[f].1);
        for y in y0..=y1 {
            for x in x0..=x1 {
                for &g in &grid[y * cells + x] {
                    let g = g as usize;
                    if g == f || seen[g] == f {
                        continue;
                    }
                    seen[g] = f;
                    let (a, b) = (&boxes[f], &boxes[g]);
                    if a.0.x > b.1.x || b.0.x > a.1.x || a.0.y > b.1.y || b.0.y > a.1.y {
                        continue;
                    }
                    // Neighbours in one ring share a vertex.
                    let (rg, kg) = flat[g];
                    let n = rings[rf].len();
                    let shared = if rf == rg && (kf + 1) % n == kg {
                        Some(piece(f).edge.end())
                    } else if rf == rg && (kg + 1) % n == kf {
                        Some(piece(g).edge.end())
                    } else {
                        None
                    };
                    let mut crossing = false;
                    intersect(&piece(f).edge, &piece(g).edge, &mut |p| {
                        if shared.is_none_or(|s| p.distance(s) > VERTEX_TOLERANCE) {
                            crossing = true;
                        }
                    });
                    if crossing {
                        demote[rf][kf] = true;
                        demote[rg][kg] |= matches!(piece(g).edge, Edge::Arc { .. });
                    }
                }
            }
        }
    }
    demote
}

/// Join neighbouring arcs of one circle into one arc. Two arcs fitted
/// separately to the same circle differ by a hair, and nearly identical
/// circles cross twice, so left as two edges they can intersect.
fn merge_arcs(pieces: Vec<Piece>) -> Vec<Piece> {
    let same = |p: &Edge, q: &Edge| match (p, q) {
        (
            Edge::Arc { b, c, ccw, .. },
            Edge::Arc {
                a: a2,
                c: c2,
                ccw: ccw2,
                ..
            },
        ) => {
            ccw == ccw2
                && b == a2
                && c.distance(*c2) <= 2.0 * REFIT_TOLERANCE
                && (p.radius() - q.radius()).abs() <= 2.0 * REFIT_TOLERANCE
        }
        _ => false,
    };
    let joined = |p: &Edge, q: &Edge| {
        let (Edge::Arc { a, c, ccw, .. }, Edge::Arc { b, .. }) = (p, q) else {
            unreachable!()
        };
        Edge::Arc {
            a: *a,
            b: *b,
            c: centered(*a, *b, *c, p.radius()),
            ccw: *ccw,
        }
    };
    let mut out: Vec<Piece> = Vec::with_capacity(pieces.len());
    for piece in pieces {
        match out.last_mut() {
            Some(last)
                if same(&last.edge, &piece.edge) && !joined(&last.edge, &piece.edge).is_full() =>
            {
                last.edge = joined(&last.edge, &piece.edge);
                last.end = piece.end;
            }
            _ => out.push(piece),
        }
    }
    out
}

/// The centre nearest `c` of a circle of radius `r` through both `a` and
/// `b`, so an arc's ends lie exactly on its circle.
pub(crate) fn centered(a: Vec2, b: Vec2, c: Vec2, r: f64) -> Vec2 {
    let mid = (a + b) * 0.5;
    let half = b.distance(a) * 0.5;
    let normal = (b - a).perp() / (2.0 * half);
    let h = (r * r - half * half).max(0.0).sqrt();
    if (c - mid).dot(normal) >= 0.0 {
        mid + normal * h
    } else {
        mid - normal * h
    }
}
