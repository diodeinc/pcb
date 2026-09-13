//! Copper: pads, tracks, zone fills and via rings as extruded solids.
//!
//! Follows KiCad's exporter in what is emitted. Pads are one prism per pad
//! per layer, kept exact (lines and arcs). Tracks, zone fills and via rings
//! on one layer and net are unioned in 2D with pcb-ir, the drills knocked
//! out, and each connected island extruded. Plated holes get a copper tube
//! through the whole stack. Unlike KiCad, layer copper is cut at the drill
//! wall and the tube fills the wall, so nothing overlaps.
//!
//! Layer indices are copper layer indices, top first, matching
//! `Physical::copper_z`.

use pcb_ir::geom::{
    ContourBuf, ContourSet, FillRule, GeometryAccuracy, PathCmd, Point, Resolution,
};

use crate::board::{Board, Fill, Pad, PadKind, PadShape, Physical, Primitive, Track, Via};
use crate::geom::{Vec2, ccw_sweep, circle_center, rotate_kicad};
use crate::outline::{Edge, Frame, Loop, Solid, crosses, orient};
use crate::rings::{IndexedRing, loops_of, polygons_of};

/// KiCad's plating thickness for through holes.
pub(crate) const PLATING: f64 = 0.025;
/// Chord error for flattened arcs, KiCad's default maximum error.
const MAX_ERROR: f64 = 0.005;

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct CopperOptions {
    pub(crate) pads: bool,
    pub(crate) tracks: bool,
    pub(crate) zones: bool,
    pub(crate) inner: bool,
}

impl CopperOptions {
    pub(crate) fn any(&self) -> bool {
        self.pads || self.tracks || self.zones
    }
}

/// A copper solid between two heights.
pub(crate) struct CopperSolid {
    pub(crate) z0: f64,
    pub(crate) z1: f64,
    pub(crate) solid: Solid,
}

#[derive(Default)]
pub(crate) struct Copper {
    /// Tracks, zone fills and via rings: one solid per island.
    pub(crate) islands: Vec<CopperSolid>,
    /// Pad prisms and their plating tubes.
    pub(crate) pads: Vec<CopperSolid>,
    /// Via barrels.
    pub(crate) vias: Vec<CopperSolid>,
}

pub(crate) fn point(v: Vec2) -> Point {
    Point { x: v.x, y: v.y }
}

/// Copper layers that are exported.
fn layer_mask(board: &Board, options: CopperOptions) -> u64 {
    let n = board.copper_layers.len().min(63);
    if n == 0 {
        return 0;
    }
    if options.inner {
        (1u64 << n) - 1
    } else {
        1 | (1u64 << (n - 1))
    }
}

pub(crate) fn build(
    board: &Board,
    frame: Frame,
    physical: &Physical,
    options: CopperOptions,
    threads: usize,
    warnings: &mut Vec<String>,
) -> Copper {
    let mut copper = Copper::default();
    let mask = layer_mask(board, options);
    let last = physical.copper_z.len().saturating_sub(1) as u32;
    if mask == 0 {
        return copper;
    }
    let layout = Layout {
        board,
        frame,
        physical,
        connectivity: Connectivity::new(board, frame),
        mask,
        last,
    };
    let mut knockouts = Knockouts::drills(board, frame, last);

    if options.pads {
        let results = crate::parallel_map(threads, &board.pads, |pad| {
            let mut out = Vec::new();
            let mut cut = Vec::new();
            let mut notes = Vec::new();
            pad_solids(&layout, pad, &knockouts, &mut out, &mut cut, &mut notes);
            (out, cut, notes)
        });
        for (solids, cut, notes) in results {
            copper.pads.extend(solids);
            for (layer, c) in cut {
                knockouts.pads[layer as usize].push(c);
            }
            warnings.extend(notes);
        }
    }

    if options.tracks {
        for via in &board.vias {
            if via.drill <= 0.0 {
                continue;
            }
            let top = via.top.min(last);
            let bottom = via.bottom.min(last);
            if let Some(tube) = tube(
                frame.point(via.at),
                frame.point(via.at),
                via.drill * 0.5,
                physical.copper_z[bottom as usize].0,
                physical.copper_z[top as usize].1,
            ) {
                copper.vias.push(tube);
            }
        }
    }

    if options.tracks || options.zones {
        let groups = group_by_layer_and_net(board, options, mask);
        let results = crate::parallel_map(threads, &groups, |group| {
            island_solids(&layout, group, &knockouts)
        });
        for (solids, notes) in results {
            copper.islands.extend(solids);
            warnings.extend(notes);
        }
    }
    copper
}

/// What every copper solid is built from.
struct Layout<'a> {
    board: &'a Board<'a>,
    frame: Frame,
    physical: &'a Physical,
    connectivity: Connectivity,
    /// Copper layers that are exported.
    mask: u64,
    /// Index of the last copper layer.
    last: u32,
}

/// Which copper layers a pad or via is flashed on when it removes unused
/// layers: the end layers if kept, and any layer where something of its
/// net touches it. KiCad's answer comes from full connectivity; touching
/// tracks and containing fills cover the cases that matter.
struct Connectivity {
    /// `(net, layer, point)` of every track end and `(net, layer, fill)`.
    track_ends: Vec<(u32, u32, Vec2)>,
    fills: Vec<(u32, u32, Vec<Vec2>)>,
}

impl Connectivity {
    fn new(board: &Board, frame: Frame) -> Self {
        let mut track_ends = Vec::with_capacity(board.tracks.len() * 2);
        for t in &board.tracks {
            track_ends.push((t.net, t.layer, frame.point(t.a)));
            track_ends.push((t.net, t.layer, frame.point(t.b)));
        }
        track_ends.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        let fills = board
            .fills
            .iter()
            .map(|f| {
                let pts = board.fill_points[f.points.start as usize..f.points.end as usize]
                    .iter()
                    .map(|p| frame.point(*p))
                    .collect();
                (f.net, f.layer, pts)
            })
            .collect();
        Self { track_ends, fills }
    }

    fn touches(&self, net: u32, layer: u32, center: Vec2, reach: f64) -> bool {
        let start = self
            .track_ends
            .partition_point(|e| (e.0, e.1) < (net, layer));
        for end in &self.track_ends[start..] {
            if (end.0, end.1) != (net, layer) {
                break;
            }
            if end.2.distance(center) <= reach {
                return true;
            }
        }
        self.fills
            .iter()
            .any(|f| f.0 == net && f.1 == layer && crate::geom::point_in_polygon(center, &f.2))
    }
}

/// Whether an item that removes unused layers is flashed on `layer`.
struct Flash {
    remove_unused: bool,
    keep_ends: bool,
    net: u32,
    center: Vec2,
    reach: f64,
}

impl Flash {
    fn on(&self, layer: u32, last: u32, connectivity: &Connectivity) -> bool {
        if !self.remove_unused {
            return true;
        }
        if self.keep_ends && (layer == 0 || layer == last) {
            return true;
        }
        connectivity.touches(self.net, layer, self.center, self.reach)
    }
}

/// A pad's copper on every exported layer, plus its plating tube. The
/// pad's outline on each layer goes to `cutters` so the layer's tracks and
/// zones are cut away under it instead of overlapping it.
fn pad_solids(
    layout: &Layout,
    pad: &Pad,
    drills: &Knockouts,
    out: &mut Vec<CopperSolid>,
    cutters: &mut Vec<(u32, PadCutter)>,
    warnings: &mut Vec<String>,
) {
    let (board, frame, physical) = (layout.board, layout.frame, layout.physical);
    let (connectivity, last) = (&layout.connectivity, layout.last);
    let layers = pad.layers & layout.mask;
    if layers == 0 {
        return;
    }
    let center = frame.point(pad.at);
    let drill = pad.has_hole.then(|| pad_drill(pad, frame));
    // An unplated hole that swallows its pad has no copper at all.
    if pad.kind == PadKind::NoPlate
        && let Some((a, b, r)) = drill
        && a == b
        && r >= pad.size.x.min(pad.size.y) * 0.5
    {
        return;
    }
    let Some(outline) = pad_outline(board, frame, pad, warnings) else {
        return;
    };
    let reach = pad.size.x.max(pad.size.y) * 0.5;
    let hole = drill.map(|(a, b, r)| Loop::stadium(a, b, r));
    let outline_loop = Loop::new(outline.clone());
    let hole_inside = hole.as_ref().is_some_and(|h| h.inside(&outline_loop));

    let flash = Flash {
        remove_unused: pad.remove_unused,
        keep_ends: pad.keep_ends,
        net: pad.net,
        center,
        reach,
    };
    for layer in 0..=last {
        if layers & (1 << layer) == 0 || !flash.on(layer, last, connectivity) {
            continue;
        }
        let (z0, z1) = physical.copper_z[layer as usize];
        cutters.extend(pad_cutter(&outline).into_iter().map(|contour| {
            let points = contour
                .cmds
                .iter()
                .filter(|k| {
                    matches!(
                        k.op,
                        pcb_ir::geom::PathOp::MoveTo | pcb_ir::geom::PathOp::LineTo
                    )
                })
                .map(|k| Vec2::new(k.p0.x, k.p0.y))
                .collect();
            (layer, PadCutter { points, contour })
        }));
        // Other drills through the pad (an unplated hole beside it, a via
        // in it) take their bite too.
        let others = drills.through(&outline_loop, drill, layer);
        if !others.is_empty() {
            let mut all: Vec<&Loop> = others.iter().collect();
            all.extend(hole.iter());
            match subtract(&outline_loop, &all) {
                Some(solids) => {
                    for solid in solids {
                        out.push(CopperSolid { z0, z1, solid });
                    }
                    continue;
                }
                None => warnings.push(format!(
                    "pad at ({:.3}, {:.3}) could not be cut by a hole through it",
                    pad.at.x, pad.at.y
                )),
            }
        }
        let solid = match (&hole, hole_inside) {
            (Some(h), true) => Solid {
                outer: outline_loop.clone(),
                holes: vec![h.clone()],
                round: Vec::new(),
            },
            (Some(h), false) => {
                // The drill crosses the pad edge: cut it in 2D.
                match subtract(&outline_loop, &[h]) {
                    Some(solids) => {
                        for solid in solids {
                            out.push(CopperSolid { z0, z1, solid });
                        }
                        continue;
                    }
                    None => Solid {
                        outer: outline_loop.clone(),
                        holes: Vec::new(),
                        round: Vec::new(),
                    },
                }
            }
            (None, _) => Solid {
                outer: outline_loop.clone(),
                holes: Vec::new(),
                round: Vec::new(),
            },
        };
        out.push(CopperSolid { z0, z1, solid });
    }

    if pad.kind == PadKind::ThroughHole
        && let Some((a, b, r)) = drill
        && layers & 1 != 0
        && layers & (1 << last) != 0
        && let Some(t) = tube(
            a,
            b,
            r,
            physical.copper_z[last as usize].0,
            physical.copper_z[0].1,
        )
    {
        out.push(t);
    }
}

/// How far a pad's cutter is grown past its outline.
const CUTTER_GROWTH: f64 = 0.0005;

/// The cutter that takes a pad out of its layer's tracks and zones: the
/// outline grown by a hair. Zone fills run exactly along pad edges, and a
/// boolean between coincident edges can leave a zero-width seam in its
/// output; the growth keeps the two apart.
fn pad_cutter(outline: &[Edge]) -> Vec<ContourBuf> {
    let exact = contour(outline);
    let resolution = resolution();
    let grown =
        ContourSet::from_contours(std::slice::from_ref(&exact), FillRule::NonZero, resolution)
            .and_then(|set| set.disk_dilate(CUTTER_GROWTH))
            .map(|set| set.to_contours());
    match grown {
        Ok(contours) if !contours.is_empty() => contours,
        _ => vec![exact],
    }
}

/// Drill of a pad in STEP coordinates: `(a, b, radius)` of the stadium.
pub(crate) fn pad_drill(pad: &Pad, frame: Frame) -> (Vec2, Vec2, f64) {
    let half = pad.drill * 0.5;
    let (r, half_len) = if half.x > half.y {
        (half.y, Vec2::new(half.x - half.y, 0.0))
    } else {
        (half.x, Vec2::new(0.0, half.y - half.x))
    };
    let half_len = rotate_kicad(half_len, pad.rotation);
    (
        frame.point(pad.at - half_len),
        frame.point(pad.at + half_len),
        r,
    )
}

/// A plated barrel: a tube of wall `PLATING` inside the drill.
fn tube(a: Vec2, b: Vec2, r: f64, z0: f64, z1: f64) -> Option<CopperSolid> {
    let inner = r - PLATING;
    if inner <= 1e-6 || z1 - z0 <= 1e-9 {
        return None;
    }
    let mut outer = Loop::stadium(a, b, r);
    outer.reverse();
    Some(CopperSolid {
        z0,
        z1,
        solid: Solid {
            outer,
            holes: vec![Loop::stadium(a, b, inner)],
            round: Vec::new(),
        },
    })
}

/// Map a pad-local point (KiCad frame, y down, unrotated) to STEP.
fn pad_point(pad: &Pad, frame: Frame, local: Vec2) -> Vec2 {
    frame.point(pad.at + rotate_kicad(local + pad.offset, pad.rotation))
}

/// The pad's copper outline as counter-clockwise STEP edges. Standard
/// shapes stay exact; custom pads are unioned from their primitives.
pub(crate) fn pad_outline(
    board: &Board,
    frame: Frame,
    pad: &Pad,
    warnings: &mut Vec<String>,
) -> Option<Vec<Edge>> {
    let (w, h) = (pad.size.x, pad.size.y);
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let edges = match pad.shape {
        PadShape::Circle => circle_edges(pad_point(pad, frame, Vec2::ZERO), w.min(h) * 0.5),
        PadShape::Rect => polygon_edges(
            pad,
            frame,
            &[
                Vec2::new(-w / 2.0, -h / 2.0),
                Vec2::new(w / 2.0, -h / 2.0),
                Vec2::new(w / 2.0, h / 2.0),
                Vec2::new(-w / 2.0, h / 2.0),
            ],
        ),
        PadShape::Trapezoid { delta } => {
            let (dx, dy) = (delta.x / 2.0, delta.y / 2.0);
            polygon_edges(
                pad,
                frame,
                &[
                    Vec2::new(-w / 2.0 - dy, h / 2.0 + dx),
                    Vec2::new(w / 2.0 + dy, h / 2.0 - dx),
                    Vec2::new(w / 2.0 - dy, -h / 2.0 + dx),
                    Vec2::new(-w / 2.0 + dy, -h / 2.0 - dx),
                ],
            )
        }
        PadShape::Oval => stadium_edges(pad, frame, w, h),
        PadShape::RoundRect {
            round_ratio,
            chamfer_ratio,
            chamfer,
        } => {
            let side = w.min(h);
            rounded_rect_edges(
                pad,
                frame,
                w,
                h,
                (round_ratio * side).clamp(0.0, side / 2.0),
                (chamfer_ratio * side).clamp(0.0, side / 2.0),
                chamfer,
            )
        }
        PadShape::Custom { anchor_circle } => {
            return custom_pad_outline(board, frame, pad, anchor_circle, warnings);
        }
    };
    Some(orient(edges, true))
}

/// The pad outline grown by `margin` (shrunk when negative) the way
/// KiCad inflates it: round corners grow, square corners round off, and
/// a shrunk corner keeps what radius it has left. Shapes KiCad inflates
/// less simply come back as `None`.
pub(crate) fn grown_pad_outline(pad: &Pad, frame: Frame, margin: f64) -> Option<Vec<Edge>> {
    let w = pad.size.x + 2.0 * margin;
    let h = pad.size.y + 2.0 * margin;
    if w <= 1e-9 || h <= 1e-9 {
        return None;
    }
    let edges = match pad.shape {
        PadShape::Circle => circle_edges(pad_point(pad, frame, Vec2::ZERO), w.min(h) * 0.5),
        PadShape::Oval => stadium_edges(pad, frame, w, h),
        PadShape::Rect => rounded_rect_edges(pad, frame, w, h, margin.max(0.0), 0.0, [false; 4]),
        PadShape::RoundRect {
            round_ratio,
            chamfer_ratio,
            chamfer,
        } if chamfer_ratio <= 0.0 || !chamfer.contains(&true) => {
            let side = pad.size.x.min(pad.size.y);
            let radius = (round_ratio * side).clamp(0.0, side / 2.0) + margin;
            rounded_rect_edges(pad, frame, w, h, radius.max(0.0), 0.0, [false; 4])
        }
        _ => return None,
    };
    Some(orient(edges, true))
}

pub(crate) fn circle_edges(c: Vec2, r: f64) -> Vec<Edge> {
    let p = c + Vec2::new(r, 0.0);
    vec![Edge::Arc {
        a: p,
        b: p,
        c,
        ccw: true,
    }]
}

fn polygon_edges(pad: &Pad, frame: Frame, local: &[Vec2]) -> Vec<Edge> {
    let points: Vec<Vec2> = local.iter().map(|p| pad_point(pad, frame, *p)).collect();
    (0..points.len())
        .map(|i| Edge::Line {
            a: points[i],
            b: points[(i + 1) % points.len()],
        })
        .collect()
}

/// Oval pad: a stadium along its longer axis.
fn stadium_edges(pad: &Pad, frame: Frame, w: f64, h: f64) -> Vec<Edge> {
    if (w - h).abs() < 1e-9 {
        return circle_edges(pad_point(pad, frame, Vec2::ZERO), w * 0.5);
    }
    let (r, half) = if w > h {
        (h / 2.0, Vec2::new(w / 2.0 - h / 2.0, 0.0))
    } else {
        (w / 2.0, Vec2::new(0.0, h / 2.0 - w / 2.0))
    };
    let a = pad_point(pad, frame, -half);
    let b = pad_point(pad, frame, half);
    let mut l = Loop::stadium(a, b, r);
    l.reverse();
    l.edges
}

/// Rectangle with each corner rounded, chamfered, or left square. Corners
/// are top-left, top-right, bottom-left, bottom-right in KiCad's y-down
/// pad frame.
fn rounded_rect_edges(
    pad: &Pad,
    frame: Frame,
    w: f64,
    h: f64,
    radius: f64,
    chamfer: f64,
    chamfered: [bool; 4],
) -> Vec<Edge> {
    // Walk the corners in the pad frame in the order top-left, top-right,
    // bottom-right, bottom-left, each with the inward directions of the
    // edge leaving it and the edge arriving at it.
    let corners = [
        (
            Vec2::new(-w / 2.0, -h / 2.0),
            Vec2::X,
            Vec2::Y,
            chamfered[0],
        ),
        (
            Vec2::new(w / 2.0, -h / 2.0),
            Vec2::Y,
            -Vec2::X,
            chamfered[1],
        ),
        (
            Vec2::new(w / 2.0, h / 2.0),
            -Vec2::X,
            -Vec2::Y,
            chamfered[3],
        ),
        (
            Vec2::new(-w / 2.0, h / 2.0),
            -Vec2::Y,
            Vec2::X,
            chamfered[2],
        ),
    ];
    let mut edges = Vec::with_capacity(8);
    let mut previous_end: Option<Vec2> = None;
    let mut first_start: Option<Vec2> = None;
    for (corner, along, back, is_chamfered) in corners {
        // `along` runs from this corner toward the next; `back` runs from
        // it toward the previous one. The cut starts `cut` back along the
        // arriving edge and ends `cut` along the leaving edge.
        let cut = if is_chamfered { chamfer } else { radius };
        let start = corner + back * cut;
        let end = corner + along * cut;
        let (start, end) = (pad_point(pad, frame, start), pad_point(pad, frame, end));
        if let Some(prev) = previous_end
            && prev.distance(start) > 1e-9
        {
            edges.push(Edge::Line { a: prev, b: start });
        }
        if cut > 1e-9 {
            if is_chamfered {
                edges.push(Edge::Line { a: start, b: end });
            } else {
                let c = pad_point(pad, frame, corner + (along + back) * cut);
                let mid_local =
                    corner + (along + back) * cut * (1.0 - std::f64::consts::FRAC_1_SQRT_2);
                let mid = pad_point(pad, frame, mid_local);
                edges.push(arc_through(start, mid, end, c));
            }
        }
        first_start.get_or_insert(start);
        previous_end = Some(end);
    }
    if let (Some(prev), Some(first)) = (previous_end, first_start)
        && prev.distance(first) > 1e-9
    {
        edges.push(Edge::Line { a: prev, b: first });
    }
    edges
}

/// Arc from `a` to `b` about `c` passing through `mid`.
fn arc_through(a: Vec2, mid: Vec2, b: Vec2, c: Vec2) -> Edge {
    let start = (a - c).y.atan2((a - c).x);
    let ccw = ccw_sweep(start, (mid - c).y.atan2((mid - c).x))
        < ccw_sweep(start, (b - c).y.atan2((b - c).x));
    Edge::Arc { a, b, c, ccw }
}

/// Custom pad: anchor shape plus primitives, unioned.
fn custom_pad_outline(
    board: &Board,
    frame: Frame,
    pad: &Pad,
    anchor_circle: bool,
    warnings: &mut Vec<String>,
) -> Option<Vec<Edge>> {
    let (w, h) = (pad.size.x, pad.size.y);
    let anchor = if anchor_circle {
        circle_edges(pad_point(pad, frame, Vec2::ZERO), w.min(h) * 0.5)
    } else {
        polygon_edges(
            pad,
            frame,
            &[
                Vec2::new(-w / 2.0, -h / 2.0),
                Vec2::new(w / 2.0, -h / 2.0),
                Vec2::new(w / 2.0, h / 2.0),
                Vec2::new(-w / 2.0, h / 2.0),
            ],
        )
    };
    let mut contours = vec![contour(&orient(anchor, true))];
    let local = |p: Vec2| pad_point(pad, frame, p);
    for primitive in &board.primitives[pad.primitives.start as usize..pad.primitives.end as usize] {
        match *primitive {
            Primitive::Line { a, b, width } => {
                if let Some(c) = stroke_contour(local(a), local(a), local(b), width) {
                    contours.push(c);
                }
            }
            Primitive::Arc { a, mid, b, width } => {
                if let Some(c) = stroke_contour(local(a), local(mid), local(b), width) {
                    contours.push(c);
                }
            }
            Primitive::Circle {
                center,
                radius,
                width,
                filled,
            } => {
                let c = local(center);
                if filled || width <= 0.0 {
                    contours.push(contour(&circle_edges(c, radius + width * 0.5)));
                } else {
                    contours.push(contour(&circle_edges(c, radius + width * 0.5)));
                    contours.push(contour(&orient(
                        circle_edges(c, radius - width * 0.5),
                        false,
                    )));
                }
            }
            Primitive::Rect {
                a,
                b,
                width,
                filled,
            } => {
                let corners = [a, Vec2::new(b.x, a.y), b, Vec2::new(a.x, b.y)];
                let pts: Vec<Vec2> = corners.iter().map(|p| local(*p)).collect();
                push_polygon(&pts, width, filled, &mut contours);
            }
            Primitive::Poly {
                points,
                width,
                filled,
            } => {
                let pts: Vec<Vec2> = board.primitive_points[points.0 as usize..points.1 as usize]
                    .iter()
                    .map(|p| local(*p))
                    .collect();
                push_polygon(&pts, width, filled, &mut contours);
            }
        }
    }
    let resolution = resolution();
    let region = match ContourSet::from_contours(&contours, FillRule::NonZero, resolution) {
        Ok(region) => region,
        Err(err) => {
            warnings.push(format!(
                "custom pad at ({:.3}, {:.3}) mm: {err}",
                pad.at.x, pad.at.y
            ));
            return None;
        }
    };
    let mut loops = loops_of(&region);
    if loops.len() != 1 {
        warnings.push(format!(
            "custom pad at ({:.3}, {:.3}) mm has {} pieces; using the largest",
            pad.at.x,
            pad.at.y,
            loops.len()
        ));
    }
    loops.sort_by(|a, b| b.0.area().abs().total_cmp(&a.0.area().abs()));
    loops.into_iter().next().map(|(outer, _)| outer.edges)
}

fn push_polygon(points: &[Vec2], width: f64, filled: bool, contours: &mut Vec<ContourBuf>) {
    if points.len() < 3 {
        return;
    }
    if filled {
        let edges: Vec<Edge> = (0..points.len())
            .map(|i| Edge::Line {
                a: points[i],
                b: points[(i + 1) % points.len()],
            })
            .collect();
        contours.push(contour(&orient(edges, true)));
    }
    if width > 0.0 {
        for i in 0..points.len() {
            let (a, b) = (points[i], points[(i + 1) % points.len()]);
            if let Some(c) = stroke_contour(a, a, b, width) {
                contours.push(c);
            }
        }
    }
}

pub(crate) fn resolution() -> Resolution {
    Resolution::new(0.0005, GeometryAccuracy::new(MAX_ERROR).unwrap())
}

/// A closed edge list as a pcb-ir contour. Full circles are split in two
/// since an arc command needs distinct ends.
pub(crate) fn contour(edges: &[Edge]) -> ContourBuf {
    let mut cmds = Vec::with_capacity(edges.len() + 2);
    cmds.push(PathCmd::move_to(point(edges[0].start())));
    for edge in edges {
        match *edge {
            Edge::Line { b, .. } => cmds.push(PathCmd::line_to(point(b))),
            Edge::Arc { a, b, c, ccw } => {
                if a.distance(b) < 1e-9 {
                    let opposite = c + (c - a);
                    cmds.push(PathCmd::arc_to(point(opposite), point(c), !ccw));
                    cmds.push(PathCmd::arc_to(point(a), point(c), !ccw));
                } else {
                    // An arc drawn through three points can end a few
                    // micrometres off its circle; pcb-ir charges that
                    // against the accuracy budget, so put the centre
                    // where both ends are on the circle.
                    let r = (a.distance(c) + b.distance(c)) * 0.5;
                    let c = crate::rings::centered(a, b, c, r);
                    cmds.push(PathCmd::arc_to(point(b), point(c), !ccw));
                }
            }
        }
    }
    cmds.push(PathCmd::close());
    ContourBuf::new(cmds)
}

/// The outline of a track from `a` through `mid` to `b` of the given
/// width, with round ends. `mid == a` is a straight segment.
pub(crate) fn stroke_contour(a: Vec2, mid: Vec2, b: Vec2, width: f64) -> Option<ContourBuf> {
    let r = width * 0.5;
    if r <= 0.0 {
        return None;
    }
    if a.distance(b) < 1e-9 && a.distance(mid) < 1e-9 {
        return Some(contour(&circle_edges(a, r)));
    }
    let arc = (mid.distance(a) > 1e-9)
        .then(|| circle_center(a, mid, b))
        .flatten();
    let Some(c) = arc else {
        let mut l = Loop::stadium(a, b, r);
        l.reverse();
        return Some(contour(&l.edges));
    };
    // Arc track: offset arcs either side joined by semicircular caps.
    let radius = a.distance(c);
    let start = (a - c).y.atan2((a - c).x);
    let ccw = ccw_sweep(start, (mid - c).y.atan2((mid - c).x))
        < ccw_sweep(start, (b - c).y.atan2((b - c).x));
    if radius <= r {
        // Inner offset collapses; the outline is the outer arc plus caps.
        let mut l = Loop::stadium(a, b, r);
        l.reverse();
        return Some(contour(&l.edges));
    }
    let unit = |p: Vec2| (p - c) / radius;
    let (a_out, a_in) = (c + unit(a) * (radius + r), c + unit(a) * (radius - r));
    let (b_out, b_in) = (c + unit(b) * (radius + r), c + unit(b) * (radius - r));
    let edges = vec![
        Edge::Arc {
            a: a_out,
            b: b_out,
            c,
            ccw,
        },
        Edge::Arc {
            a: b_out,
            b: b_in,
            c: b,
            ccw,
        },
        Edge::Arc {
            a: b_in,
            b: a_in,
            c,
            ccw: !ccw,
        },
        Edge::Arc {
            a: a_in,
            b: a_out,
            c: a,
            ccw,
        },
    ];
    Some(contour(&orient(edges, true)))
}

/// Everything unioned together on one layer for one net.
struct Group {
    layer: u32,
    net: u32,
    tracks: Vec<usize>,
    fills: Vec<usize>,
    vias: Vec<usize>,
}

fn group_by_layer_and_net(board: &Board, options: CopperOptions, mask: u64) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    let find = |layer: u32, net: u32, groups: &mut Vec<Group>| -> usize {
        match groups.iter().position(|g| g.layer == layer && g.net == net) {
            Some(i) => i,
            None => {
                groups.push(Group {
                    layer,
                    net,
                    tracks: Vec::new(),
                    fills: Vec::new(),
                    vias: Vec::new(),
                });
                groups.len() - 1
            }
        }
    };
    if options.tracks {
        for (i, t) in board.tracks.iter().enumerate() {
            if mask & (1 << t.layer.min(63)) != 0 {
                let g = find(t.layer, t.net, &mut groups);
                groups[g].tracks.push(i);
            }
        }
        for (i, v) in board.vias.iter().enumerate() {
            for layer in v.top..=v.bottom {
                if mask & (1 << layer.min(63)) != 0 {
                    let g = find(layer, v.net, &mut groups);
                    groups[g].vias.push(i);
                }
            }
        }
    }
    if options.zones {
        for (i, f) in board.fills.iter().enumerate() {
            if mask & (1 << f.layer.min(63)) != 0 {
                let g = find(f.layer, f.net, &mut groups);
                groups[g].fills.push(i);
            }
        }
    }
    // Largest groups first so the thread pool tails off evenly.
    groups.sort_by_key(|g| std::cmp::Reverse(g.tracks.len() + g.fills.len() * 8 + g.vias.len()));
    groups
}

/// Drill outlines per layer, as contours: `(layer, contour, bbox)`.
/// What is cut out of a layer's copper: the drills through it and the
/// pads exported as their own solids, wherever they actually overlap the
/// copper, whatever their net. Zone fills are not redone when a pad is
/// placed or its net changes, so clearance from other nets cannot be
/// assumed; testing each candidate against the island first keeps the
/// boolean small.
struct Knockouts {
    drills: Vec<Drill>,
    /// Pad cutters per copper layer.
    pads: Vec<Vec<PadCutter>>,
}

struct Drill {
    a: Vec2,
    b: Vec2,
    r: f64,
    bbox: (Vec2, Vec2),
    /// Copper layers spanned, inclusive.
    top: u32,
    bottom: u32,
    contour: ContourBuf,
}

/// A pad's grown outline, and its vertices for the overlap test.
struct PadCutter {
    points: Vec<Vec2>,
    contour: ContourBuf,
}

impl Knockouts {
    fn drills(board: &Board, frame: Frame, last: u32) -> Self {
        let mut drills = Vec::with_capacity(board.holes.len() + board.vias.len());
        let mut push = |a: Vec2, b: Vec2, r: f64, top: u32, bottom: u32| {
            let l = Loop::stadium(a, b, r);
            drills.push(Drill {
                a,
                b,
                r,
                bbox: (a.min(b) - Vec2::splat(r), a.max(b) + Vec2::splat(r)),
                top,
                bottom,
                contour: contour(&orient(l.edges, true)),
            });
        };
        for hole in &board.holes {
            push(frame.point(hole.a), frame.point(hole.b), hole.r, 0, last);
        }
        for via in &board.vias {
            if via.drill > 0.0 && via.top <= last {
                let at = frame.point(via.at);
                push(at, at, via.drill * 0.5, via.top, via.bottom.min(last));
            }
        }
        Self {
            drills,
            pads: (0..=last).map(|_| Vec::new()).collect(),
        }
    }

    /// Drills that span `layer` and whose bounding box meets `(lo, hi)`.
    fn drills_near(&self, layer: u32, (lo, hi): (Vec2, Vec2)) -> impl Iterator<Item = &Drill> {
        self.drills.iter().filter(move |d| {
            d.top <= layer
                && layer <= d.bottom
                && d.bbox.0.x <= hi.x
                && lo.x <= d.bbox.1.x
                && d.bbox.0.y <= hi.y
                && lo.y <= d.bbox.1.y
        })
    }

    /// The drills that touch a pad's `outline` on `layer` other than the
    /// pad's `own`, as hole loops.
    fn through(&self, outline: &Loop, own: Option<(Vec2, Vec2, f64)>, layer: u32) -> Vec<Loop> {
        let mut hits = Vec::new();
        for d in self.drills_near(layer, outline.bbox()) {
            if own.is_some_and(|(a, b, r)| {
                a.distance(d.a) < 1e-6 && b.distance(d.b) < 1e-6 && (r - d.r).abs() < 1e-6
            }) {
                continue;
            }
            let hole = Loop::stadium(d.a, d.b, d.r);
            if crosses(outline, &hole) || outline.contains(d.a) {
                hits.push(hole);
            }
        }
        hits
    }

    /// Everything on `layer` that overlaps the copper made of `rings`.
    fn cutters(&self, layer: u32, rings: &[Vec<Vec2>], index: &[IndexedRing]) -> Vec<ContourBuf> {
        let lo = index
            .iter()
            .fold(Vec2::splat(f64::INFINITY), |m, r| m.min(r.lo));
        let hi = index
            .iter()
            .fold(Vec2::splat(f64::NEG_INFINITY), |m, r| m.max(r.hi));
        // Inside the copper: inside an odd number of rings.
        let inside = |p: Vec2| {
            index
                .iter()
                .zip(rings)
                .filter(|(i, r)| i.contains(r, p))
                .count()
                % 2
                == 1
        };
        let mut out = Vec::new();
        // A drill's disc overlaps when its centre is inside or a ring
        // edge comes within its radius.
        for d in self.drills_near(layer, (lo, hi)) {
            let center = (d.a + d.b) * 0.5;
            let reach = d.a.distance(d.b) * 0.5 + d.r;
            let near = Vec2::splat(reach);
            let hit = inside(center)
                || index.iter().zip(rings).any(|(i, r)| {
                    i.edges_meeting(r, (center - near, center + near))
                        .into_iter()
                        .any(|(a, b)| segment_distance(center, a, b) < reach)
                });
            if hit {
                out.push(d.contour.clone());
            }
        }
        // A pad overlaps when a vertex of it is inside, or a ring edge
        // ends inside it or crosses its edge. A pad sitting in its
        // clearance hole passes neither.
        for p in &self.pads[layer as usize] {
            let b = &p.contour.bbox;
            let (plo, phi) = (Vec2::new(b.min.x, b.min.y), Vec2::new(b.max.x, b.max.y));
            if plo.x > hi.x || lo.x > phi.x || plo.y > hi.y || lo.y > phi.y {
                continue;
            }
            let n = p.points.len();
            let hit = inside(p.points[0])
                || index.iter().zip(rings).any(|(i, r)| {
                    i.edges_meeting(r, (plo, phi)).into_iter().any(|(a, b)| {
                        crate::geom::point_in_polygon(a, &p.points)
                            || crate::geom::point_in_polygon(b, &p.points)
                            || (0..n).any(|k| {
                                crate::geom::segments_cross(
                                    a,
                                    b,
                                    p.points[k],
                                    p.points[(k + 1) % n],
                                )
                            })
                    })
                });
            if hit {
                out.push(p.contour.clone());
            }
        }
        out
    }
}

/// Distance from `p` to segment `ab`.
fn segment_distance(p: Vec2, a: Vec2, b: Vec2) -> f64 {
    let d = b - a;
    let len2 = d.length_squared();
    let t = if len2 < 1e-18 {
        0.0
    } else {
        ((p - a).dot(d) / len2).clamp(0.0, 1.0)
    };
    p.distance(a + d * t)
}

fn island_solids(
    layout: &Layout,
    group: &Group,
    knockouts: &Knockouts,
) -> (Vec<CopperSolid>, Vec<String>) {
    let (board, frame, physical) = (layout.board, layout.frame, layout.physical);
    let (connectivity, last) = (&layout.connectivity, layout.last);
    let mut warnings = Vec::new();
    let mut contours =
        Vec::with_capacity(group.tracks.len() + group.fills.len() + group.vias.len());
    for &i in &group.tracks {
        let Track {
            a, mid, b, width, ..
        } = board.tracks[i];
        if let Some(c) = stroke_contour(frame.point(a), frame.point(mid), frame.point(b), width) {
            contours.push(c);
        }
    }
    for &i in &group.vias {
        let via: &Via = &board.vias[i];
        if via.size <= via.drill {
            continue;
        }
        let at = frame.point(via.at);
        let flash = Flash {
            remove_unused: via.remove_unused,
            keep_ends: via.keep_ends || group.layer == via.top || group.layer == via.bottom,
            net: via.net,
            center: at,
            reach: via.size * 0.5,
        };
        if !flash.on(group.layer, last, connectivity) {
            continue;
        }
        contours.push(contour(&circle_edges(at, via.size * 0.5)));
    }
    for &i in &group.fills {
        let Fill { points, .. } = &board.fills[i];
        let pts: Vec<Vec2> = board.fill_points[points.start as usize..points.end as usize]
            .iter()
            .map(|p| frame.point(*p))
            .collect();
        let edges: Vec<Edge> = (0..pts.len())
            .map(|k| Edge::Line {
                a: pts[k],
                b: pts[(k + 1) % pts.len()],
            })
            .collect();
        contours.push(contour(&orient(edges, true)));
    }
    if contours.is_empty() {
        return (Vec::new(), warnings);
    }
    let resolution = resolution();
    let region = match ContourSet::from_contours(&contours, FillRule::NonZero, resolution) {
        Ok(r) => r,
        Err(err) => {
            warnings.push(format!(
                "copper on layer {} net {}: {err}",
                board.copper_layers[group.layer as usize], group.net
            ));
            return (Vec::new(), warnings);
        }
    };
    let rings = polygons_of(&region);
    let index: Vec<IndexedRing> = rings.iter().map(|r| IndexedRing::new(r)).collect();
    let holes = knockouts.cutters(group.layer, &rings, &index);
    let region = if holes.is_empty() {
        region
    } else {
        match ContourSet::from_contours(&holes, FillRule::NonZero, resolution)
            .and_then(|cutters| region.difference(&cutters))
        {
            Ok(r) => r,
            Err(err) => {
                warnings.push(format!(
                    "copper on layer {} net {}: {err}",
                    board.copper_layers[group.layer as usize], group.net
                ));
                region
            }
        }
    };
    let (z0, z1) = physical.copper_z[group.layer as usize];
    let solids = loops_of(&region)
        .into_iter()
        .map(|(outer, holes)| CopperSolid {
            z0,
            z1,
            solid: Solid {
                outer,
                holes,
                round: Vec::new(),
            },
        })
        .collect();
    (solids, warnings)
}

/// Subtract a hole loop from an outline loop in 2D.
fn subtract(outline: &Loop, holes: &[&Loop]) -> Option<Vec<Solid>> {
    let resolution = resolution();
    let subject =
        ContourSet::from_contours(&[contour(&outline.edges)], FillRule::NonZero, resolution)
            .ok()?;
    let cutters: Vec<ContourBuf> = holes
        .iter()
        .map(|h| contour(&orient(h.edges.clone(), true)))
        .collect();
    let cutters = ContourSet::from_contours(&cutters, FillRule::NonZero, resolution).ok()?;
    let region = subject.difference(&cutters).ok()?;
    Some(
        loops_of(&region)
            .into_iter()
            .map(|(outer, holes)| Solid {
                outer,
                holes,
                round: Vec::new(),
            })
            .collect(),
    )
}
