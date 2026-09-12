//! Board outline loops and the holes cut through them.
//!
//! An outline is a closed chain of line and arc edges in STEP coordinates
//! (millimetres, y up). Outer loops wind counter-clockwise, holes clockwise.
//! Drills become hole loops. A drill that touches the outline or another
//! drill is taken out with a polygon boolean, and the rings that come back
//! are snapped onto the lines and circles they came from, so the result is
//! always a set of simple, exact loops a BRep shell can be built from.

use std::f64::consts::{PI, TAU};

use pcb_ir::geom::{ContourBuf, ContourSet, FillRule, PathCmd, Point};

use crate::Error;
use crate::board::{Board, RawEdge};
use crate::geom::{Vec2, ccw_sweep, circle_center, point_in_polygon, signed_area};
use crate::holes::RoundHole;
use crate::rings::raw_islands;

/// Endpoints closer than this are joined, matching KiCad's chaining epsilon.
const CHAIN_TOLERANCE: f64 = 0.01;
/// Chord error when the profile is flattened for the boolean.
const FLATTEN_ERROR: f64 = 0.002;
/// A boolean vertex this close to a source curve lies on it: the flattening
/// error plus pcb-ir's coordinate grid.
const ON_CURVE: f64 = 0.003;
/// A chord midpoint this close to a circle belongs to it.
const MID_ON_CURVE: f64 = 0.005;
/// Arc tessellation step for point-in-loop tests.
const TESSELLATION_STEP: f64 = PI / 18.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Edge {
    Line {
        a: Vec2,
        b: Vec2,
    },
    /// Arc from `a` to `b` about `c`. `a == b` is a full circle.
    Arc {
        a: Vec2,
        b: Vec2,
        c: Vec2,
        ccw: bool,
    },
}

impl Edge {
    pub(crate) fn start(&self) -> Vec2 {
        match *self {
            Edge::Line { a, .. } | Edge::Arc { a, .. } => a,
        }
    }

    pub(crate) fn end(&self) -> Vec2 {
        match *self {
            Edge::Line { b, .. } | Edge::Arc { b, .. } => b,
        }
    }

    /// The same edge re-pointed to end exactly at `a` and `b`: a line is
    /// simply moved, an arc keeps its old midpoint and takes the circle
    /// through the three points, so it stays exact through its new ends.
    fn with_ends(&self, a: Vec2, b: Vec2) -> Self {
        match *self {
            Edge::Line { .. } => Edge::Line { a, b },
            Edge::Arc { .. } => {
                let mid = self.point_at(0.5);
                match circle_center(a, mid, b) {
                    Some(c) => {
                        let start = angle_of(a - c);
                        let ccw =
                            ccw_sweep(start, angle_of(mid - c)) < ccw_sweep(start, angle_of(b - c));
                        Edge::Arc { a, b, c, ccw }
                    }
                    None => Edge::Line { a, b },
                }
            }
        }
    }

    pub(crate) fn reversed(&self) -> Self {
        match *self {
            Edge::Line { a, b } => Edge::Line { a: b, b: a },
            Edge::Arc { a, b, c, ccw } => Edge::Arc {
                a: b,
                b: a,
                c,
                ccw: !ccw,
            },
        }
    }

    /// An arc that closes on itself: a whole circle.
    pub(crate) fn is_full(&self) -> bool {
        matches!(self, Edge::Arc { a, b, .. } if a.distance(*b) < 1e-9)
    }

    pub(crate) fn radius(&self) -> f64 {
        match *self {
            Edge::Line { .. } => 0.0,
            Edge::Arc { a, c, .. } => a.distance(c),
        }
    }

    /// Signed angular sweep: positive counter-clockwise.
    fn sweep(&self) -> f64 {
        match *self {
            Edge::Line { .. } => 0.0,
            Edge::Arc { a, b, c, ccw } => {
                if a.distance(b) < 1e-9 {
                    return if ccw { TAU } else { -TAU };
                }
                let from = angle_of(a - c);
                let to = angle_of(b - c);
                if ccw {
                    ccw_sweep(from, to)
                } else {
                    -ccw_sweep(to, from)
                }
            }
        }
    }

    fn point_at(&self, t: f64) -> Vec2 {
        match *self {
            Edge::Line { a, b } => a.lerp(b, t),
            Edge::Arc { a, c, .. } => {
                let angle = angle_of(a - c) + self.sweep() * t;
                c + Vec2::from_angle(angle) * self.radius()
            }
        }
    }

    /// Parameter of a point known to lie on the edge.
    fn length(&self) -> f64 {
        match *self {
            Edge::Line { a, b } => a.distance(b),
            Edge::Arc { .. } => self.sweep().abs() * self.radius(),
        }
    }

    fn tessellate(&self, out: &mut Vec<Vec2>) {
        match *self {
            Edge::Line { a, .. } => out.push(a),
            Edge::Arc { a, .. } => {
                let steps = ((self.sweep().abs() / TESSELLATION_STEP).ceil() as usize).max(1);
                out.push(a);
                for i in 1..steps {
                    out.push(self.point_at(i as f64 / steps as f64));
                }
            }
        }
    }

    pub(crate) fn bbox(&self) -> (Vec2, Vec2) {
        match *self {
            Edge::Line { a, b } => (a.min(b), a.max(b)),
            Edge::Arc { c, .. } => {
                let r = Vec2::splat(self.radius());
                (c - r, c + r)
            }
        }
    }

    /// Whether `p`, assumed on the edge's supporting curve, lies within the
    /// edge, allowing a little slack at the ends.
    fn spans(&self, p: Vec2) -> bool {
        match *self {
            Edge::Line { a, b } => {
                let d = b - a;
                let t = (p - a).dot(d) / d.length_squared();
                (-1e-9..=1.0 + 1e-9).contains(&t)
            }
            Edge::Arc { a, b, c, ccw } => {
                if a.distance(b) < 1e-9 {
                    return true;
                }
                let from = angle_of(a - c);
                let to = angle_of(p - c);
                let turned = if ccw {
                    ccw_sweep(from, to)
                } else {
                    ccw_sweep(to, from)
                };
                turned <= self.sweep().abs() + 1e-9
            }
        }
    }
}

fn angle_of(v: Vec2) -> f64 {
    v.y.atan2(v.x)
}

#[derive(Debug, Clone)]
pub(crate) struct Loop {
    pub(crate) edges: Vec<Edge>,
    /// Polyline approximation, for containment tests only.
    poly: Vec<Vec2>,
}

impl Loop {
    pub(crate) fn new(edges: Vec<Edge>) -> Self {
        let mut poly = Vec::new();
        for edge in &edges {
            edge.tessellate(&mut poly);
        }
        Self { edges, poly }
    }

    pub(crate) fn area(&self) -> f64 {
        signed_area(&self.poly)
    }

    /// The same loop walked the other way.
    pub(crate) fn reversed(&self) -> Self {
        let mut l = Self {
            edges: self.edges.clone(),
            poly: self.poly.clone(),
        };
        l.reverse();
        l
    }

    pub(crate) fn reverse(&mut self) {
        self.edges.reverse();
        for edge in &mut self.edges {
            *edge = edge.reversed();
        }
        self.poly.reverse();
    }

    pub(crate) fn contains(&self, p: Vec2) -> bool {
        point_in_polygon(p, &self.poly)
    }

    /// Whether every vertex of `self` lies inside `outer`.
    pub(crate) fn inside(&self, outer: &Loop) -> bool {
        self.poly.iter().all(|p| outer.contains(*p))
    }

    /// A point on the boundary, for testing this loop against others.
    fn probe(&self) -> Vec2 {
        self.poly[0]
    }

    pub(crate) fn bbox(&self) -> (Vec2, Vec2) {
        let mut lo = Vec2::splat(f64::INFINITY);
        let mut hi = Vec2::splat(f64::NEG_INFINITY);
        for edge in &self.edges {
            let (a, b) = edge.bbox();
            lo = lo.min(a);
            hi = hi.max(b);
        }
        (lo, hi)
    }

    fn bbox_overlaps(&self, other: &Loop) -> bool {
        let (alo, ahi) = self.bbox();
        let (blo, bhi) = other.bbox();
        alo.x <= bhi.x && blo.x <= ahi.x && alo.y <= bhi.y && blo.y <= ahi.y
    }

    /// Stadium (or circle) hole, wound clockwise.
    pub(crate) fn stadium(a: Vec2, b: Vec2, r: f64) -> Self {
        if a.distance(b) < 1e-6 {
            let p = a + Vec2::new(r, 0.0);
            return Self::new(vec![Edge::Arc {
                a: p,
                b: p,
                c: a,
                ccw: false,
            }]);
        }
        let axis = (b - a).normalize();
        let left = Vec2::new(-axis.y, axis.x) * r;
        let mut hole = Self::new(vec![
            Edge::Line {
                a: a + left,
                b: b + left,
            },
            Edge::Arc {
                a: b + left,
                b: b - left,
                c: b,
                ccw: false,
            },
            Edge::Line {
                a: b - left,
                b: a - left,
            },
            Edge::Arc {
                a: a - left,
                b: a + left,
                c: a,
                ccw: false,
            },
        ]);
        debug_assert!(hole.area() < 0.0);
        if hole.area() > 0.0 {
            hole.reverse();
        }
        hole
    }
}

/// A closed edge list wound counter-clockwise (or clockwise).
pub(crate) fn orient(edges: Vec<Edge>, ccw: bool) -> Vec<Edge> {
    let mut l = Loop::new(edges);
    if (l.area() > 0.0) != ccw {
        l.reverse();
    }
    l.edges
}

/// One board solid: an outer loop and the holes through it.
pub(crate) struct Solid {
    pub(crate) outer: Loop,
    pub(crate) holes: Vec<Loop>,
    /// Round holes with a depth profile, kept clear of everything else.
    pub(crate) round: Vec<RoundHole>,
}

/// Board-to-STEP coordinate mapping: origin shift, then y flipped.
#[derive(Clone, Copy)]
pub(crate) struct Frame {
    pub(crate) origin: Vec2,
}

impl Frame {
    pub(crate) fn point(&self, p: Vec2) -> Vec2 {
        Vec2::new(p.x - self.origin.x, self.origin.y - p.y)
    }
}

/// Chain the `Edge.Cuts` graphics into solids, one per outer loop.
pub(crate) fn board_solids(board: &Board, frame: Frame) -> Result<Vec<Solid>, Error> {
    let mut edges = Vec::with_capacity(board.edges.len());
    for raw in &board.edges {
        match *raw {
            RawEdge::Line { a, b } => edges.push(Edge::Line {
                a: frame.point(a),
                b: frame.point(b),
            }),
            RawEdge::Arc { a, mid, b } => {
                let (a, mid, b) = (frame.point(a), frame.point(mid), frame.point(b));
                match circle_center(a, mid, b) {
                    Some(c) => {
                        let start = angle_of(a - c);
                        let ccw =
                            ccw_sweep(start, angle_of(mid - c)) < ccw_sweep(start, angle_of(b - c));
                        edges.push(Edge::Arc { a, b, c, ccw });
                    }
                    None => edges.push(Edge::Line { a, b }),
                }
            }
            RawEdge::Circle { center, radius } => {
                let c = frame.point(center);
                let p = c + Vec2::new(radius, 0.0);
                edges.push(Edge::Arc {
                    a: p,
                    b: p,
                    c,
                    ccw: true,
                });
            }
        }
    }
    // Sub-micrometre edges are drawing noise (KiCad chains across gaps of
    // 10 µm anyway) and make degenerate faces; the gap they leave is
    // closed exactly when the loop is joined.
    edges.retain(|e| e.length() > 1e-3);
    if edges.is_empty() {
        return Err(Error::Outline("board has no Edge.Cuts outline"));
    }

    let mut loops = chain(edges, CHAIN_TOLERANCE)?;
    loops.retain(|l| l.area().abs() > 1e-9);
    if loops.is_empty() {
        return Err(Error::Outline("board outline encloses no area"));
    }

    // Nesting depth decides outer versus hole; a hole belongs to the
    // smallest loop containing it.
    let n = loops.len();
    let mut depth = vec![0usize; n];
    let mut parent = vec![usize::MAX; n];
    for i in 0..n {
        let probe = loops[i].probe();
        let mut best_area = f64::INFINITY;
        for (j, other) in loops.iter().enumerate() {
            if i != j && other.contains(probe) {
                depth[i] += 1;
                let area = other.area().abs();
                if area < best_area {
                    best_area = area;
                    parent[i] = j;
                }
            }
        }
    }
    let mut solid_of_loop = vec![usize::MAX; n];
    let mut solids = Vec::new();
    let mut loops: Vec<Option<Loop>> = loops.into_iter().map(Some).collect();
    for i in 0..n {
        if depth[i] % 2 == 0 {
            let mut outer = loops[i].take().unwrap();
            if outer.area() < 0.0 {
                outer.reverse();
            }
            solid_of_loop[i] = solids.len();
            solids.push(Solid {
                outer,
                holes: Vec::new(),
                round: Vec::new(),
            });
        }
    }
    for i in 0..n {
        if depth[i] % 2 == 1 {
            let mut hole = loops[i].take().unwrap();
            if hole.area() > 0.0 {
                hole.reverse();
            }
            let Some(&solid) = solid_of_loop.get(parent[i]).filter(|&&s| s != usize::MAX) else {
                return Err(Error::Outline("board outline loops cross each other"));
            };
            solids[solid].holes.push(hole);
        }
    }
    Ok(solids)
}

/// Cut plain through drills. A drill clear of the outline and of every
/// other drill is a hole as it is; the rest are taken out of their solid
/// with a polygon boolean whose rings are snapped back onto the source
/// lines and circles, so tangent, coincident and overlapping drills all
/// come out exact.
pub(crate) fn cut_holes(
    solids: Vec<Solid>,
    drills: Vec<Loop>,
    warnings: &mut Vec<String>,
) -> Vec<Solid> {
    let boxes: Vec<(Vec2, Vec2)> = drills.iter().map(Loop::bbox).collect();
    let mut touching = vec![false; drills.len()];
    let mut order: Vec<usize> = (0..drills.len()).collect();
    order.sort_by(|&i, &j| boxes[i].0.x.total_cmp(&boxes[j].0.x));
    for (k, &i) in order.iter().enumerate() {
        for &j in &order[k + 1..] {
            if boxes[j].0.x > boxes[i].1.x {
                break;
            }
            if overlaps(boxes[i], boxes[j]) {
                touching[i] = true;
                touching[j] = true;
            }
        }
    }
    let mut edge_boxes: Vec<(Vec2, Vec2)> = solids
        .iter()
        .flat_map(|s| std::iter::once(&s.outer).chain(&s.holes))
        .flat_map(|l| l.edges.iter().map(Edge::bbox))
        .collect();
    edge_boxes.sort_by(|a, b| a.0.x.total_cmp(&b.0.x));
    for (i, b) in boxes.iter().enumerate() {
        if touching[i] {
            continue;
        }
        let from = edge_boxes.partition_point(|e| e.1.x < b.0.x);
        touching[i] = edge_boxes[from..]
            .iter()
            .take_while(|e| e.0.x <= b.1.x)
            .any(|e| overlaps(*e, *b));
    }

    let mut out = Vec::new();
    for solid in solids {
        let (lo, hi) = solid.outer.bbox();
        let mine: Vec<usize> = (0..drills.len())
            .filter(|&i| overlaps(boxes[i], (lo, hi)))
            .collect();
        let cut: Vec<&Loop> = mine
            .iter()
            .filter(|&&i| touching[i])
            .map(|&i| &drills[i])
            .collect();
        let clear: Vec<&Loop> = mine
            .iter()
            .filter(|&&i| !touching[i])
            .map(|&i| &drills[i])
            .filter(|d| solid.outer.contains(d.probe()))
            .filter(|d| {
                !solid
                    .holes
                    .iter()
                    .any(|h| h.bbox_overlaps(d) && h.contains(d.probe()))
            })
            .collect();
        if cut.is_empty() {
            let mut solid = solid;
            solid.holes.extend(clear.into_iter().cloned());
            out.push(solid);
            continue;
        }
        let profile = match cut_profile(&solid, &cut) {
            Ok(p) => p,
            Err(err) => {
                warnings.push(format!(
                    "board outline: {err}; {} drills left uncut",
                    cut.len()
                ));
                let mut solid = solid;
                solid.holes.extend(clear.into_iter().cloned());
                out.push(solid);
                continue;
            }
        };
        let first = out.len();
        out.extend(profile);
        for hole in clear {
            if let Some(owner) = out[first..]
                .iter_mut()
                .find(|s| s.outer.contains(hole.probe()))
            {
                owner.holes.push(hole.clone());
            }
        }
    }
    out
}

fn overlaps((alo, ahi): (Vec2, Vec2), (blo, bhi): (Vec2, Vec2)) -> bool {
    alo.x <= bhi.x && blo.x <= ahi.x && alo.y <= bhi.y && blo.y <= ahi.y
}

/// `solid` less `drills`, through pcb-ir, with every ring snapped back
/// onto the curves it came from.
fn cut_profile(solid: &Solid, drills: &[&Loop]) -> Result<Vec<Solid>, pcb_ir::geom::AccuracyError> {
    let resolution = crate::copper::resolution();
    let mut prims = Vec::new();
    let mut subject = vec![flat_contour(&solid.outer.edges)];
    prims_of(&solid.outer.edges, &mut prims);
    for hole in &solid.holes {
        subject.push(flat_contour(&hole.edges));
        prims_of(&hole.edges, &mut prims);
    }
    let cutters: Vec<ContourBuf> = drills
        .iter()
        .map(|d| {
            prims_of(&d.edges, &mut prims);
            flat_contour(&orient(d.edges.clone(), true))
        })
        .collect();
    let region = ContourSet::from_contours(&subject, FillRule::NonZero, resolution)?.difference(
        &ContourSet::from_contours(&cutters, FillRule::NonZero, resolution)?,
    )?;
    Ok(raw_islands(&region)
        .iter()
        .map(|island| Solid {
            outer: Loop::new(orient(snap_ring(&island.outer, &prims), true)),
            holes: island
                .holes
                .iter()
                .map(|h| Loop::new(orient(snap_ring(h, &prims), false)))
                .collect(),
            round: Vec::new(),
        })
        .collect())
}

/// The vertices of a closed edge list, arcs as chords within `max_error`.
pub(crate) fn flatten(edges: &[Edge], max_error: f64) -> Vec<Vec2> {
    let mut points: Vec<Vec2> = Vec::new();
    for edge in edges {
        match *edge {
            Edge::Line { a, .. } => points.push(a),
            Edge::Arc { a, c, .. } => {
                let r = edge.radius();
                let sweep = edge.sweep();
                let from = angle_of(a - c);
                let step = 2.0 * (1.0 - max_error / r).clamp(-1.0, 1.0).acos();
                let n = ((sweep.abs() / step.max(1e-3)).ceil() as usize).max(1);
                for k in 0..n {
                    let angle = from + sweep * k as f64 / n as f64;
                    points.push(c + Vec2::new(angle.cos(), angle.sin()) * r);
                }
            }
        }
    }
    points
}

fn flat_contour(edges: &[Edge]) -> ContourBuf {
    let points = flatten(edges, FLATTEN_ERROR);
    let point = |p: Vec2| Point { x: p.x, y: p.y };
    let mut cmds = Vec::with_capacity(points.len() + 1);
    cmds.push(PathCmd::move_to(point(points[0])));
    cmds.extend(points[1..].iter().map(|p| PathCmd::line_to(point(*p))));
    cmds.push(PathCmd::close());
    ContourBuf::new(cmds)
}

/// A source curve a boolean vertex can lie on.
#[derive(Clone, Copy, Debug)]
enum Curve {
    Line { a: Vec2, b: Vec2 },
    Circle { c: Vec2, r: f64 },
}

/// A source edge: its curve and its two ends, which are exact points on
/// it and on its neighbours.
#[derive(Clone, Copy, Debug)]
struct Prim {
    curve: Curve,
    ends: [Vec2; 2],
}

fn prims_of(edges: &[Edge], out: &mut Vec<Prim>) {
    for edge in edges {
        let curve = match *edge {
            Edge::Line { a, b } => Curve::Line { a, b },
            Edge::Arc { c, .. } => Curve::Circle {
                c,
                r: edge.radius(),
            },
        };
        let same = out.iter().any(|p| match (p.curve, curve) {
            (Curve::Circle { c: c1, r: r1 }, Curve::Circle { c: c2, r: r2 }) => {
                c1.distance(c2) < 1e-6 && (r1 - r2).abs() < 1e-6
            }
            _ => false,
        });
        if !same {
            out.push(Prim {
                curve,
                ends: [edge.start(), edge.end()],
            });
        }
    }
}

impl Prim {
    fn is_line(&self) -> bool {
        matches!(self.curve, Curve::Line { .. })
    }

    /// The exact meeting point of two curves nearest `near`: a source
    /// vertex both share, else their analytic intersection.
    fn junction(&self, other: &Prim, near: Vec2) -> Option<Vec2> {
        let shared = self
            .ends
            .iter()
            .chain(&other.ends)
            .filter(|e| e.distance(near) <= 4.0 * MID_ON_CURVE)
            .filter(|e| self.distance(**e) <= ON_CURVE && other.distance(**e) <= ON_CURVE)
            .min_by(|p, q| p.distance(near).total_cmp(&q.distance(near)));
        shared
            .copied()
            .or_else(|| self.curve.junction(&other.curve, near))
    }

    fn distance(&self, p: Vec2) -> f64 {
        self.curve.distance(p)
    }

    fn project(&self, p: Vec2) -> Vec2 {
        self.curve.project(p)
    }
}

impl Curve {
    /// Distance from `p` to the curve; a line counts a little beyond its
    /// ends, since a boolean vertex may sit just past them.
    fn distance(&self, p: Vec2) -> f64 {
        match *self {
            Curve::Line { a, b } => {
                let d = b - a;
                let len = d.length();
                if len < 1e-12 {
                    return a.distance(p);
                }
                let t = ((p - a).dot(d) / (len * len)).clamp(-ON_CURVE / len, 1.0 + ON_CURVE / len);
                (a + d * t).distance(p)
            }
            Curve::Circle { c, r } => (c.distance(p) - r).abs(),
        }
    }

    fn project(&self, p: Vec2) -> Vec2 {
        match *self {
            Curve::Line { a, b } => {
                let d = b - a;
                a + d * ((p - a).dot(d) / d.length_squared().max(1e-24))
            }
            Curve::Circle { c, r } => {
                let v = p - c;
                if v.length() < 1e-12 {
                    p
                } else {
                    c + v.normalize() * r
                }
            }
        }
    }

    /// The exact meeting point of two curves nearest `near`, if they meet
    /// close to it.
    fn junction(&self, other: &Curve, near: Vec2) -> Option<Vec2> {
        let mut candidates: Vec<Vec2> = Vec::new();
        match (*self, *other) {
            (Curve::Line { a: p, b: q }, Curve::Line { a: r, b: s }) => {
                let (d1, d2) = (q - p, s - r);
                let denom = d1.perp_dot(d2);
                if denom.abs() > 1e-12 {
                    candidates.push(p + d1 * ((r - p).perp_dot(d2) / denom));
                }
            }
            (Curve::Line { a, b }, Curve::Circle { c, r })
            | (Curve::Circle { c, r }, Curve::Line { a, b }) => {
                let d = b - a;
                let f = a - c;
                let qa = d.length_squared();
                let qb = 2.0 * f.dot(d);
                let qc = f.length_squared() - r * r;
                let disc = qb * qb - 4.0 * qa * qc;
                if qa > 1e-18 && disc >= 0.0 {
                    let root = disc.sqrt();
                    candidates.push(a + d * ((-qb - root) / (2.0 * qa)));
                    candidates.push(a + d * ((-qb + root) / (2.0 * qa)));
                }
            }
            (Curve::Circle { c: c1, r: r1 }, Curve::Circle { c: c2, r: r2 }) => {
                let d = c1.distance(c2);
                if d > 1e-9 && d <= r1 + r2 + 1e-6 && d >= (r1 - r2).abs() - 1e-6 {
                    let x = (d * d + r1 * r1 - r2 * r2) / (2.0 * d);
                    let h = (r1 * r1 - x * x).max(0.0).sqrt();
                    let along = (c2 - c1) / d;
                    let base = c1 + along * x;
                    let side = Vec2::new(-along.y, along.x) * h;
                    candidates.push(base + side);
                    candidates.push(base - side);
                }
            }
        }
        candidates
            .into_iter()
            .filter(|p| p.distance(near) <= 4.0 * MID_ON_CURVE)
            .min_by(|p, q| p.distance(near).total_cmp(&q.distance(near)))
    }
}

/// Turn a boolean ring back into exact edges: runs of vertices that lie on
/// one source curve become one line or arc on it, and the vertex where two
/// runs meet is moved to the curves' exact meeting point.
fn snap_ring(ring: &[Vec2], prims: &[Prim]) -> Vec<Edge> {
    let n = ring.len();
    let label = |i: usize| -> Option<usize> {
        let (u, v) = (ring[i], ring[(i + 1) % n]);
        let mid = (u + v) * 0.5;
        let fits = |k: usize| {
            let p = &prims[k];
            p.distance(u) <= ON_CURVE
                && p.distance(v) <= ON_CURVE
                && p.distance(mid) <= MID_ON_CURVE
        };
        let line = (0..prims.len()).find(|&k| prims[k].is_line() && fits(k));
        line.or_else(|| (0..prims.len()).find(|&k| fits(k)))
    };
    let labels: Vec<Option<usize>> = (0..n).map(label).collect();
    if let Some(k) = labels[0]
        && labels.iter().all(|l| *l == Some(k))
        && let Curve::Circle { c, r } = prims[k].curve
    {
        let a = c + (ring[0] - c).normalize() * r;
        return vec![Edge::Arc {
            a,
            b: a,
            c,
            ccw: signed_area(ring) > 0.0,
        }];
    }
    // Runs of segments with one label, starting at a label change.
    let start = (0..n)
        .find(|&i| labels[i] != labels[(i + n - 1) % n])
        .unwrap_or(0);
    let mut runs: Vec<(Option<usize>, usize, usize)> = Vec::new();
    let mut i = 0;
    while i < n {
        let s = (start + i) % n;
        let l = labels[s];
        let mut len = 1;
        while len < n - i && labels[(s + len) % n] == l {
            len += 1;
        }
        runs.push((l, s, len));
        i += len;
    }
    // Vertex at the start of each run, moved onto the curves that meet there.
    let m = runs.len();
    let mut starts: Vec<Vec2> = Vec::with_capacity(m);
    for k in 0..m {
        let (l, s, _) = runs[k];
        let prev = runs[(k + m - 1) % m].0;
        let v = ring[s];
        let snapped = match (prev, l) {
            (Some(p), Some(q)) if p != q => prims[p]
                .junction(&prims[q], v)
                .unwrap_or_else(|| prims[q].project(v)),
            (_, Some(q)) => prims[q].project(v),
            (Some(p), None) => prims[p].project(v),
            (None, None) => v,
        };
        starts.push(snapped);
    }
    let mut edges = Vec::new();
    for k in 0..m {
        let (l, s, len) = runs[k];
        let a = starts[k];
        let b = starts[(k + 1) % m];
        match l.map(|k| prims[k].curve) {
            Some(Curve::Line { .. }) => {
                if a.distance(b) > 1e-9 {
                    edges.push(Edge::Line { a, b });
                }
            }
            Some(Curve::Circle { c, .. }) => {
                if a.distance(b) <= 1e-9 {
                    continue;
                }
                let ccw = if len >= 2 {
                    let mid = ring[(s + len / 2) % n];
                    let from = angle_of(a - c);
                    ccw_sweep(from, angle_of(mid - c)) < ccw_sweep(from, angle_of(b - c))
                } else {
                    (a - c).perp_dot(b - c) > 0.0
                };
                edges.push(Edge::Arc { a, b, c, ccw });
            }
            _ => {
                // No curve claims these vertices: keep them as they are.
                let mut prev = a;
                for j in 1..len {
                    let p = ring[(s + j) % n];
                    if prev.distance(p) > 1e-9 {
                        edges.push(Edge::Line { a: prev, b: p });
                    }
                    prev = p;
                }
                if prev.distance(b) > 1e-9 {
                    edges.push(Edge::Line { a: prev, b });
                }
            }
        }
    }
    edges
}

/// Cut a round hole with a depth profile. It must stand clear of the
/// outline and of every other hole; if it does not, a plain through drill
/// is cut instead where there is one.
/// Returns the plain drill to cut instead when the machined shape cannot
/// be placed.
pub(crate) fn cut_round(
    solids: &mut [Solid],
    hole: RoundHole,
    warnings: &mut Vec<String>,
) -> Option<Loop> {
    let center = hole.center;
    let radius = hole.max_radius();
    let ring = Loop::stadium(center, center, radius);
    let probe = ring.probe();
    for solid in solids.iter_mut() {
        if !solid.outer.contains(center) {
            continue;
        }
        let touches = |other: &Loop| {
            other.bbox_overlaps(&ring)
                && (crosses(other, &ring) || other.contains(probe) || ring.contains(other.probe()))
        };
        let clear = !crosses(&solid.outer, &ring)
            && !solid.holes.iter().any(touches)
            && !solid
                .round
                .iter()
                .any(|r| r.center.distance(center) < r.max_radius() + radius + 1e-6);
        if clear {
            solid.round.push(hole);
            return None;
        }
        return match hole.fallback {
            Some(r) => {
                if !hole.is_plain() {
                    warnings.push(format!(
                        "drill at ({:.3}, {:.3}) mm touches other geometry; machining dropped",
                        center.x, -center.y
                    ));
                }
                Some(Loop::stadium(center, center, r))
            }
            None => {
                warnings.push(format!(
                    "skipped a blind hole at ({:.3}, {:.3}) mm that touches other geometry",
                    center.x, -center.y
                ));
                None
            }
        };
    }
    None
}

/// Join edges end to end into closed loops.
fn chain(edges: Vec<Edge>, tolerance: f64) -> Result<Vec<Loop>, Error> {
    let mut ends: Vec<(Vec2, u32, bool)> = Vec::with_capacity(edges.len() * 2);
    for (i, edge) in edges.iter().enumerate() {
        ends.push((edge.start(), i as u32, false));
        ends.push((edge.end(), i as u32, true));
    }
    ends.sort_by(|a, b| a.0.x.total_cmp(&b.0.x));
    let mut used = vec![false; edges.len()];
    let find = |p: Vec2, used: &[bool]| -> Option<(u32, bool)> {
        let lo = ends.partition_point(|e| e.0.x < p.x - tolerance);
        let mut best: Option<(f64, u32, bool)> = None;
        for e in &ends[lo..] {
            if e.0.x > p.x + tolerance {
                break;
            }
            if used[e.1 as usize] {
                continue;
            }
            let d = e.0.distance(p);
            if d <= tolerance && best.is_none_or(|b| d < b.0) {
                best = Some((d, e.1, e.2));
            }
        }
        best.map(|b| (b.1, b.2))
    };

    let mut loops = Vec::new();
    for first in 0..edges.len() {
        if used[first] {
            continue;
        }
        used[first] = true;
        let mut chain = vec![edges[first]];
        let start = edges[first].start();
        let mut current = edges[first].end();
        while current.distance(start) > tolerance {
            let Some((index, at_end)) = find(current, &used) else {
                return Err(Error::OpenOutline(current));
            };
            used[index as usize] = true;
            let mut edge = edges[index as usize];
            if at_end {
                edge = edge.reversed();
            }
            current = edge.end();
            chain.push(edge);
        }
        loops.push(Loop::new(join(chain)));
    }
    Ok(loops)
}

/// Close every gap between consecutive edges exactly: both meet at the
/// midpoint of the gap, arcs re-derived through their new ends. KiCad
/// tolerates gaps up to its chaining epsilon; a shell cannot.
fn join(mut edges: Vec<Edge>) -> Vec<Edge> {
    let n = edges.len();
    for i in 0..n {
        let j = (i + 1) % n;
        let (end, start) = (edges[i].end(), edges[j].start());
        if end.distance(start) > 1e-9 {
            let at = (end + start) * 0.5;
            edges[i] = edges[i].with_ends(edges[i].start(), at);
            edges[j] = edges[j].with_ends(at, edges[j].end());
        }
    }
    edges
}

/// Whether the boundaries of two loops cross.
pub(crate) fn crosses(a: &Loop, b: &Loop) -> bool {
    let mut any = false;
    for ea in &a.edges {
        for eb in &b.edges {
            intersect(ea, eb, &mut |_| any = true);
            if any {
                return true;
            }
        }
    }
    false
}

/// Report every crossing of two edges to `visit`.
pub(crate) fn intersect(a: &Edge, b: &Edge, visit: &mut dyn FnMut(Vec2)) {
    match (*a, *b) {
        (Edge::Line { a: p, b: q }, Edge::Line { a: r, b: s }) => {
            let d1 = q - p;
            let d2 = s - r;
            let denom = d1.perp_dot(d2);
            if denom.abs() < 1e-12 {
                return;
            }
            let w = r - p;
            let t = w.perp_dot(d2) / denom;
            let u = w.perp_dot(d1) / denom;
            if (-1e-9..=1.0 + 1e-9).contains(&t) && (-1e-9..=1.0 + 1e-9).contains(&u) {
                visit(p + d1 * t);
            }
        }
        (Edge::Line { .. }, Edge::Arc { .. }) => line_arc(a, b, visit),
        (Edge::Arc { .. }, Edge::Line { .. }) => line_arc(b, a, visit),
        (Edge::Arc { c: c1, .. }, Edge::Arc { c: c2, .. }) => {
            let r1 = a.radius();
            let r2 = b.radius();
            let d = c1.distance(c2);
            if d < 1e-12 || d > r1 + r2 + 1e-9 || d < (r1 - r2).abs() - 1e-9 {
                return;
            }
            let x = (d * d + r1 * r1 - r2 * r2) / (2.0 * d);
            let h2 = r1 * r1 - x * x;
            let h = h2.max(0.0).sqrt();
            let along = (c2 - c1) / d;
            let base = c1 + along * x;
            let side = Vec2::new(-along.y, along.x) * h;
            for p in [base + side, base - side] {
                if a.spans(p) && b.spans(p) {
                    visit(p);
                }
                if h < 1e-9 {
                    break;
                }
            }
        }
    }
}

fn line_arc(line: &Edge, arc: &Edge, visit: &mut dyn FnMut(Vec2)) {
    let Edge::Line { a: p, b: q } = *line else {
        return;
    };
    let Edge::Arc { c, .. } = *arc else {
        return;
    };
    let r = arc.radius();
    let d = q - p;
    let f = p - c;
    let qa = d.length_squared();
    let qb = 2.0 * f.dot(d);
    let qc = f.length_squared() - r * r;
    let disc = qb * qb - 4.0 * qa * qc;
    if disc < 0.0 || qa < 1e-18 {
        return;
    }
    let root = disc.sqrt();
    for t in [(-qb - root) / (2.0 * qa), (-qb + root) / (2.0 * qa)] {
        if (-1e-9..=1.0 + 1e-9).contains(&t) {
            let point = p + d * t;
            if arc.spans(point) {
                visit(point);
            }
        }
        if root < 1e-12 {
            break;
        }
    }
}
