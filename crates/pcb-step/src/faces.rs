//! Silkscreen, solder mask and paste in 2D, the way `kicad-cli` exports
//! them: silkscreen is the union of its graphics and text with the drills
//! taken out, clipped to the board; the mask is the union of its openings,
//! which the scene takes out of the board; paste is the union of the pad
//! apertures and the paste layer's graphics.

use std::f64::consts::TAU;

use pcb_ir::geom::{BBox, ContourBuf, ContourSet, FillRule, PathCmd};
use rayon::prelude::*;

use crate::board::{Board, Knockout, Machining, Mouth, Shape, ShapeKind, Tech, Text, Via};
use crate::copper::{
    PLATING, circle_edges, contour, grown_pad_outline, pad_drill, pad_outline, point, resolution,
    stroke_contour,
};
use crate::font;
use crate::geom::{
    BoxGrid, KICAD_MAX_ERROR, Vec2, ccw_sweep, circle_center, point_in_polygon, rotate_kicad,
    signed_area,
};
use crate::outline::{Edge, Frame, Loop, Solid, flatten, orient};
use crate::outline_font::Fonts;
use crate::rings::{IndexedRing, Nested, cleaned, fit, nest, nested_of, raw_islands, rings_of};

/// One face of a tech layer.
pub struct Face {
    pub outer: Loop,
    pub holes: Vec<Loop>,
}

/// The silkscreen and mask openings of each side asked for, as islands:
/// front and back silkscreen, then front and back mask.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build(
    board: &Board,
    frame: Frame,
    edge: &BoardEdge,
    silk: bool,
    mask: bool,
    cut_vias: bool,
    fonts: &Fonts,
    variables: &[(String, String)],
    warnings: &mut Vec<String>,
) -> Vec<(Tech, Vec<Nested>)> {
    let mut layers = Vec::new();
    if silk {
        layers.push(Tech::FrontSilk);
        layers.push(Tech::BackSilk);
    }
    if mask {
        layers.push(Tech::FrontMask);
        layers.push(Tech::BackMask);
    }
    let built: Vec<_> = layers
        .par_iter()
        .with_max_len(1)
        .map(|tech| {
            let mut notes = Vec::new();
            let islands = if tech.silk() {
                silk_islands(board, frame, edge, fonts, *tech, variables, &mut notes)
            } else {
                mask_openings(board, frame, fonts, *tech, cut_vias, variables, &mut notes)
            };
            (islands, notes)
        })
        .collect();
    layers
        .into_iter()
        .zip(built)
        .map(|(tech, (islands, notes))| {
            warnings.extend(notes);
            (tech, islands)
        })
        .collect()
}

/// The board edge flattened to chords within KiCad's maximum error: a
/// long arc of large radius does not fit pcb-ir's arc budget, and KiCad
/// clips to a polygon too.
pub(crate) struct BoardEdge {
    /// Outers counter-clockwise, cutouts clockwise.
    rings: Vec<Vec<Vec2>>,
    /// Bounding box of every ring segment.
    segments: Vec<(Vec2, Vec2)>,
}

impl BoardEdge {
    pub(crate) fn new(outline: &[Solid]) -> Self {
        let mut rings = Vec::new();
        for solid in outline {
            rings.push(flatten(&solid.outer.edges, KICAD_MAX_ERROR));
            for hole in &solid.holes {
                rings.push(flatten(&hole.edges, KICAD_MAX_ERROR));
            }
        }
        let segments = rings
            .iter()
            .flat_map(|r| {
                (0..r.len()).map(move |k| {
                    let (a, b) = (r[k], r[(k + 1) % r.len()]);
                    (a.min(b), a.max(b))
                })
            })
            .collect();
        Self { rings, segments }
    }

    /// Whether a segment of the edge runs through the box.
    pub(crate) fn meets(&self, (lo, hi): (Vec2, Vec2)) -> bool {
        self.segments
            .iter()
            .any(|(slo, shi)| slo.x <= hi.x && lo.x <= shi.x && slo.y <= hi.y && lo.y <= shi.y)
    }

    /// Whether `p` is on the board: inside an outer and not in a cutout.
    pub(crate) fn contains(&self, p: Vec2) -> bool {
        self.rings.iter().filter(|r| point_in_polygon(p, r)).count() % 2 == 1
    }

    pub(crate) fn region(&self, warnings: &mut Vec<String>) -> Option<ContourSet> {
        let contours: Vec<ContourBuf> = self.rings.iter().map(|r| polyline_contour(r)).collect();
        region(&contours, "board outline", warnings)
    }
}

/// A closed polyline as a pcb-ir contour, wound as given.
fn polyline_contour(points: &[Vec2]) -> ContourBuf {
    let mut cmds = Vec::with_capacity(points.len() + 1);
    cmds.push(PathCmd::move_to(point(points[0])));
    cmds.extend(points[1..].iter().map(|p| PathCmd::line_to(point(*p))));
    cmds.push(PathCmd::close());
    ContourBuf::new(cmds)
}

/// An island of a boolean as contours: the outer counter-clockwise, the
/// holes clockwise.
pub(crate) fn island_contours(island: &Nested) -> Vec<ContourBuf> {
    let wound = |ring: &[Vec2], ccw: bool| {
        if (signed_area(ring) > 0.0) == ccw {
            polyline_contour(ring)
        } else {
            let reversed: Vec<Vec2> = ring.iter().rev().copied().collect();
            polyline_contour(&reversed)
        }
    };
    std::iter::once(wound(&island.outer, true))
        .chain(island.holes.iter().map(|h| wound(h, false)))
        .collect()
}

fn ring_bbox(ring: &[Vec2]) -> (Vec2, Vec2) {
    ring.iter().fold(
        (Vec2::splat(f64::INFINITY), Vec2::splat(f64::NEG_INFINITY)),
        |(lo, hi), p| (lo.min(*p), hi.max(*p)),
    )
}

fn edges_bbox(edges: &[Edge]) -> (Vec2, Vec2) {
    edges.iter().map(Edge::bbox).fold(
        (Vec2::splat(f64::INFINITY), Vec2::splat(f64::NEG_INFINITY)),
        |(lo, hi), (a, b)| (lo.min(a), hi.max(b)),
    )
}

fn overlaps((alo, ahi): (Vec2, Vec2), (blo, bhi): (Vec2, Vec2)) -> bool {
    alo.x <= bhi.x && blo.x <= ahi.x && alo.y <= bhi.y && blo.y <= ahi.y
}

/// The graphics and text of `tech` as contours, one list per item.
#[allow(clippy::too_many_arguments)]
fn drawn_items(
    board: &Board,
    frame: Frame,
    fonts: &Fonts,
    tech: Tech,
    variables: &[(String, String)],
    warnings: &mut Vec<String>,
) -> Vec<Vec<ContourBuf>> {
    enum Item<'a> {
        Shape(&'a Shape),
        Text(&'a Text),
    }
    let items: Vec<Item> = board
        .shapes
        .iter()
        .filter(|s| s.layer == tech)
        .map(Item::Shape)
        .chain(
            board
                .texts
                .iter()
                .filter(|t| t.layer == tech)
                .map(Item::Text),
        )
        .collect();
    let drawn: Vec<_> = items
        .par_iter()
        .map(|item| {
            let mut out = Vec::new();
            let mut notes = Vec::new();
            match item {
                Item::Shape(shape) => shape_contours(board, frame, shape, &mut out),
                Item::Text(text) => {
                    text_contours(board, frame, fonts, text, variables, &mut notes, &mut out)
                }
            }
            (out, notes)
        })
        .collect();
    drawn
        .into_iter()
        .map(|(out, notes)| {
            warnings.extend(notes);
            out
        })
        .collect()
}

/// The lit intervals of a dash `pattern` along a curve `len` long; the
/// whole curve when the pattern is empty. The pattern starts lit at 0
/// and the last dash is clipped at the end, as KiCad plots it.
pub(crate) fn dashes(pattern: &[f64], len: f64) -> Vec<(f64, f64)> {
    if pattern.iter().sum::<f64>() <= 0.0 {
        return vec![(0.0, len)];
    }
    let mut out = Vec::new();
    let (mut s, mut i) = (0.0, 0);
    while s < len {
        let e = s + pattern[i % pattern.len()];
        if i % 2 == 0 {
            out.push((s, e.min(len)));
        }
        s = e;
        i += 1;
    }
    out
}

/// `text` with the project's text variables substituted as KiCad does;
/// an unknown `${NAME}` stays as written, as KiCad shows it.
pub(crate) fn expand(text: &str, variables: &[(String, String)]) -> String {
    pcb_sexpr::kicad::substitute_text_variables(text, |name| {
        variables
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, value)| value.as_str())
    })
}

/// The contours of one graphic. Strokes are cut into dashes by the
/// shape's line style, each line, arc and polygon edge starting its
/// pattern afresh, as KiCad plots them.
pub(crate) fn shape_contours(
    board: &Board,
    frame: Frame,
    shape: &Shape,
    out: &mut Vec<ContourBuf>,
) {
    let w = shape.width;
    let pattern = shape.style.pattern(w, board.dash_ratio, board.gap_ratio);
    let stroke = |a: Vec2, mid: Vec2, b: Vec2, out: &mut Vec<ContourBuf>| {
        if let Some(c) = stroke_contour(frame.point(a), frame.point(mid), frame.point(b), w) {
            out.push(c);
        }
    };
    let angle_of = |p: Vec2| p.y.atan2(p.x);
    let line = |a: Vec2, b: Vec2, out: &mut Vec<ContourBuf>| {
        let len = a.distance(b);
        if len < 1e-9 {
            return stroke(a, a, b, out);
        }
        let at = |s: f64| a + (b - a) * (s / len);
        for (s0, s1) in dashes(&pattern, len) {
            stroke(at(s0), at(s0), at(s1), out);
        }
    };
    // An arc from `start` sweeping `sweep` radians, positive in KiCad's
    // angle sense, dashed along its length.
    let arc = |c: Vec2, r: f64, start: f64, sweep: f64, out: &mut Vec<ContourBuf>| {
        let at = |s: f64| c + Vec2::from_angle(start + sweep.signum() * s / r) * r;
        for (s0, s1) in dashes(&pattern, sweep.abs() * r) {
            stroke(at(s0), at((s0 + s1) * 0.5), at(s1), out);
        }
    };
    match shape.kind {
        ShapeKind::Line { a, b } => line(a, b, out),
        ShapeKind::Arc { a, mid, b } => match circle_center(a, mid, b) {
            Some(c) => {
                let start = angle_of(a - c);
                let (to_mid, to_b) = (
                    ccw_sweep(start, angle_of(mid - c)),
                    ccw_sweep(start, angle_of(b - c)),
                );
                let sweep = if to_mid < to_b { to_b } else { to_b - TAU };
                arc(c, a.distance(c), start, sweep, out);
            }
            None => line(a, b, out),
        },
        ShapeKind::Circle { center, end } => {
            let radius = center.distance(end);
            let c = frame.point(center);
            // A dash at least as long as the circumference is the whole
            // ring, which the arc stroke cannot close on itself.
            let solid = pattern.first().is_none_or(|&dash| dash >= TAU * radius);
            if solid {
                if shape.filled {
                    out.push(contour(&circle_edges(c, radius + w * 0.5)));
                } else if w > 0.0 {
                    // A ring: the outer circle with the inner one as a hole.
                    out.push(contour(&circle_edges(c, radius + w * 0.5)));
                    if radius > w * 0.5 {
                        out.push(contour(&orient(circle_edges(c, radius - w * 0.5), false)));
                    }
                }
            } else {
                if shape.filled {
                    out.push(contour(&circle_edges(c, radius)));
                }
                arc(center, radius, angle_of(end - center), TAU, out);
            }
        }
        ShapeKind::Poly { points } => {
            let pts: Vec<Vec2> = board.shape_points[points.0 as usize..points.1 as usize]
                .iter()
                .map(|p| frame.point(*p))
                .collect();
            if pts.len() < 2 {
                return;
            }
            if shape.filled && pts.len() >= 3 {
                let edges: Vec<Edge> = (0..pts.len())
                    .map(|k| Edge::Line {
                        a: pts[k],
                        b: pts[(k + 1) % pts.len()],
                    })
                    .collect();
                out.push(contour(&orient(edges, true)));
            }
            if w > 0.0 {
                let raw = &board.shape_points[points.0 as usize..points.1 as usize];
                for k in 0..raw.len() {
                    line(raw[k], raw[(k + 1) % raw.len()], out);
                }
            }
        }
    }
}

/// The contours of one text: its glyphs, stroked in the stroke font or
/// filled in an outline font, or, for a knockout, the hull with the
/// glyphs cut out of it.
#[allow(clippy::too_many_arguments)]
fn text_contours(
    board: &Board,
    frame: Frame,
    fonts: &Fonts,
    text: &Text,
    variables: &[(String, String)],
    warnings: &mut Vec<String>,
    out: &mut Vec<ContourBuf>,
) {
    let pen = text.style.pen_width();
    let content = expand(&text.text, variables);
    let face = text
        .style
        .face
        .as_deref()
        .map(|f| fonts.resolve(f, text.style.bold, text.style.italic));
    let measure = |s: &str, size: Vec2, script: i8| match &face {
        Some(face) => face.advance(s, size, script),
        None => font::measure(s, size, script),
    };
    let content = match text.column {
        Some(column) => font::wrap(&content, column, &text.style, measure),
        None => content,
    };
    let (rings, strokes) = match &face {
        Some(face) => face.glyphs(&content, text.at, &text.style),
        None => (Vec::new(), font::strokes(&content, text.at, &text.style)),
    };
    let mut glyphs = Vec::new();
    for ring in &rings {
        let pts: Vec<Vec2> = ring.iter().map(|p| frame.point(*p)).collect();
        glyphs.push(polyline_contour(&pts));
    }
    for stroke in &strokes {
        let pts: Vec<Vec2> = stroke.iter().map(|p| frame.point(*p)).collect();
        if pts.len() == 1 {
            glyphs.push(contour(&circle_edges(pts[0], pen * 0.5)));
        }
        for pair in pts.windows(2) {
            if let Some(c) = stroke_contour(pair[0], pair[0], pair[1], pen) {
                glyphs.push(c);
            }
        }
    }
    let hull = match text.knockout {
        Knockout::No => return out.extend(glyphs),
        Knockout::Hull => text_hull(&rings, &strokes, text, pen),
        Knockout::Frame((a, b)) => board.shape_points[a as usize..b as usize].to_vec(),
    };
    if hull.is_empty() {
        return;
    }
    let hull: Vec<Vec2> = hull.into_iter().map(|p| frame.point(p)).collect();
    let what = "knockout text";
    if let Some(filled) = region(&[polyline_contour(&hull)], what, warnings) {
        out.extend(less(filled, &glyphs, what, warnings).to_contours());
    }
}

/// The rectangle KiCad knocks a text out of: the bounding box of the
/// drawn glyphs in the text's own frame, the strokes as wide as the
/// pen, grown by the knockout margin and turned back to the text's
/// angle. Empty for a text that draws nothing.
fn text_hull(rings: &[Vec<Vec2>], strokes: &[Vec<Vec2>], text: &Text, pen: f64) -> Vec<Vec2> {
    let (at, angle) = (text.at, text.style.angle);
    let points = rings
        .iter()
        .flatten()
        .map(|p| (*p, 0.0))
        .chain(strokes.iter().flatten().map(|p| (*p, pen * 0.5)));
    let (lo, hi) = points.fold(
        (Vec2::splat(f64::INFINITY), Vec2::splat(f64::NEG_INFINITY)),
        |(lo, hi), (p, r)| {
            let local = rotate_kicad(p - at, -angle);
            (lo.min(local - r), hi.max(local + r))
        },
    );
    if lo.x > hi.x {
        return Vec::new();
    }
    let grow = Vec2::splat((pen * 0.5).max(text.style.size.y / 9.0));
    let (lo, hi) = (lo - grow, hi + grow);
    [lo, Vec2::new(hi.x, lo.y), hi, Vec2::new(lo.x, hi.y)]
        .into_iter()
        .map(|c| at + rotate_kicad(c, angle))
        .collect()
}

/// Drills and machining that pierce `tech`'s side, as counter-clockwise
/// cutters. Silk loses the full drill; the mask loses the drill shrunk
/// to the barrel, as KiCad does, and only where a pad or via opens the
/// mask there.
fn pierce_edges(board: &Board, frame: Frame, tech: Tech) -> Vec<Vec<Edge>> {
    let front = tech.front();
    let side = usize::from(!front);
    let last = board.copper_layers.len().saturating_sub(1) as u32;
    let mouth = |m: &Machining| -> Option<f64> {
        let mouth = if front { m.front } else { m.back };
        let radius = match mouth {
            Some(Mouth::Counterbore { r, .. }) | Some(Mouth::Countersink { r, .. }) => Some(r),
            None => None,
        };
        let backdrill = m.backdrills.iter().flatten().find_map(|b| {
            let reaches = if front { b.start == 0 } else { b.end == last };
            reaches.then_some(b.r)
        });
        radius.into_iter().chain(backdrill).reduce(f64::max)
    };
    let mut out = Vec::new();
    for pad in board.pads.iter().filter(|p| p.has_hole) {
        let (a, b, r) = pad_drill(pad, frame);
        if tech.silk() {
            out.push(orient(Loop::stadium(a, b, r).edges, true));
        } else if pad.mask[side] && r > PLATING * 0.5 {
            out.push(orient(Loop::stadium(a, b, r - PLATING * 0.5).edges, true));
        }
    }
    for hole in &board.holes {
        if let Some(r) = mouth(&hole.machining) {
            out.push(circle_edges(frame.point(hole.a), r));
        }
    }
    for via in board.vias.iter().filter(|v| v.drill > 0.0) {
        let at = frame.point(via.at);
        if via_reaches(board, via, front) {
            let r = via.drill * 0.5;
            if tech.silk() {
                out.push(circle_edges(at, r));
            } else if open_via(board, via, side) && r > PLATING * 0.5 {
                out.push(circle_edges(at, r - PLATING * 0.5));
            }
        }
        if let Some(r) = mouth(&via.machining) {
            out.push(circle_edges(at, r));
        }
    }
    out
}

fn via_reaches(board: &Board, via: &Via, front: bool) -> bool {
    if front {
        via.top == 0
    } else {
        via.bottom >= board.copper_layers.len().saturating_sub(1) as u32
    }
}

/// Whether a via is left open in the mask on `side` (0 front, 1 back).
fn open_via(board: &Board, via: &Via, side: usize) -> bool {
    !via.tented[side].unwrap_or(board.tent[side])
}

fn region(contours: &[ContourBuf], what: &str, warnings: &mut Vec<String>) -> Option<ContourSet> {
    if contours.is_empty() {
        return None;
    }
    match ContourSet::from_contours(contours, FillRule::NonZero, resolution()) {
        Ok(r) => Some(r),
        Err(err) => {
            warnings.push(format!("{what}: {err}"));
            None
        }
    }
}

/// The result of a boolean, or `keep` when it failed.
fn or_keep(
    result: Result<ContourSet, pcb_ir::geom::AccuracyError>,
    keep: ContourSet,
    what: &str,
    warnings: &mut Vec<String>,
) -> ContourSet {
    result.unwrap_or_else(|err| {
        warnings.push(format!("{what}: {err}"));
        keep
    })
}

/// `region` less `cutters`.
fn less(
    region: ContourSet,
    cutters: &[ContourBuf],
    what: &str,
    warnings: &mut Vec<String>,
) -> ContourSet {
    match self::region(cutters, what, warnings) {
        Some(cut) => {
            let result = region.difference(&cut);
            or_keep(result, region, what, warnings)
        }
        None => region,
    }
}

/// Cells per side of the grid `union_rings` bins by. Finer cells spread
/// dense text over more threads; coarser ones leave fewer islands that
/// cross cells to merge again.
const UNION_CELLS: usize = 16;

/// The cleaned rings of the union of `items`, unioned cell by cell:
/// items are binned by position and each bin is unioned in parallel.
/// Only islands of different bins that meet are unioned again, group by
/// group, from the boolean's own vertices so shared edges still coincide.
/// Strokes overlap within a text, openings within a footprint; the rest
/// of the board is disjoint and costs nothing to combine.
fn union_rings(
    items: Vec<Vec<ContourBuf>>,
    what: &str,
    warnings: &mut Vec<String>,
) -> Vec<Vec<Vec2>> {
    let bbox_of =
        |contours: &[ContourBuf]| contours.iter().fold(BBox::empty(), |b, c| b.union(c.bbox));
    let all = items
        .iter()
        .fold(BBox::empty(), |b, item| b.union(bbox_of(item)));
    if !all.is_valid() {
        return Vec::new();
    }
    let cell = |v: f64, lo: f64, hi: f64| {
        (((v - lo) / (hi - lo).max(1e-9) * UNION_CELLS as f64) as usize).min(UNION_CELLS - 1)
    };
    let mut bins: Vec<Vec<ContourBuf>> = vec![Vec::new(); UNION_CELLS * UNION_CELLS];
    for item in items.into_iter().filter(|i| !i.is_empty()) {
        let c = bbox_of(&item).center();
        let k = cell(c.y, all.min.y, all.max.y) * UNION_CELLS + cell(c.x, all.min.x, all.max.x);
        bins[k].extend(item);
    }
    bins.retain(|b| !b.is_empty());
    let unioned: Vec<_> = bins
        .par_iter()
        .with_max_len(1)
        .map(|bin| {
            let mut notes = Vec::new();
            let islands = region(bin, what, &mut notes)
                .map(|r| raw_islands(&r))
                .unwrap_or_default();
            (islands, notes)
        })
        .collect();
    // (bin, island, its rings indexed); the outer ring comes first.
    let mut islands: Vec<(usize, Nested, Vec<IndexedRing>)> = Vec::new();
    for (k, (found, notes)) in unioned.into_iter().enumerate() {
        warnings.extend(notes);
        islands.extend(found.into_iter().map(|island| {
            let indexed = std::iter::once(&island.outer)
                .chain(&island.holes)
                .map(|r| IndexedRing::new(r))
                .collect();
            (k, island, indexed)
        }));
    }
    let bounds = |x: usize| (islands[x].2[0].lo, islands[x].2[0].hi);
    // Islands of different bins meet when the box of one reaches the
    // other's edges or lies inside it; otherwise they are disjoint.
    let meet = |a: usize, b: usize| {
        let (island, indexed) = (&islands[a].1, &islands[a].2);
        let rings = std::iter::once(&island.outer).chain(&island.holes);
        let reaches = indexed
            .iter()
            .zip(rings.clone())
            .any(|(ix, r)| !ix.edges_meeting(r, bounds(b)).is_empty());
        let p = islands[b].1.outer[0];
        let covers = indexed
            .iter()
            .zip(rings)
            .filter(|(ix, r)| ix.contains(r, p))
            .count()
            % 2
            == 1;
        reaches || covers
    };
    // Group the islands that meet. `meet` tests from the island whose box
    // starts further left.
    let boxes: Vec<(Vec2, Vec2)> = (0..islands.len()).map(bounds).collect();
    let grid = BoxGrid::new(&boxes);
    let mut group: Vec<usize> = (0..islands.len()).collect();
    fn root(group: &mut [usize], mut x: usize) -> usize {
        while group[x] != x {
            group[x] = group[group[x]];
            x = group[x];
        }
        x
    }
    let mut seen = vec![usize::MAX; islands.len()];
    for i in 0..islands.len() {
        let (lo, hi) = boxes[i];
        for j in grid.near(boxes[i]) {
            if j <= i || seen[j] == i || islands[i].0 == islands[j].0 {
                continue;
            }
            seen[j] = i;
            let (jlo, jhi) = boxes[j];
            if jlo.x > hi.x || lo.x > jhi.x || jlo.y > hi.y || lo.y > jhi.y {
                continue;
            }
            let (a, b) = if (lo.x, i) <= (jlo.x, j) {
                (i, j)
            } else {
                (j, i)
            };
            if meet(a, b) {
                let (ra, rb) = (root(&mut group, a), root(&mut group, b));
                group[ra.max(rb)] = ra.min(rb);
            }
        }
    }
    let mut groups: Vec<Vec<usize>> = vec![Vec::new(); islands.len()];
    for x in 0..islands.len() {
        let r = root(&mut group, x);
        groups[r].push(x);
    }
    groups.retain(|g| !g.is_empty());
    let merged: Vec<_> = groups
        .par_iter()
        .with_max_len(1)
        .map(|ids| {
            let mut notes = Vec::new();
            let rings = if let [only] = ids[..] {
                let island = &islands[only].1;
                std::iter::once(&island.outer)
                    .chain(&island.holes)
                    .filter_map(|r| cleaned(r.clone()))
                    .collect()
            } else {
                let all: Vec<ContourBuf> = ids
                    .iter()
                    .flat_map(|&i| island_contours(&islands[i].1))
                    .collect();
                region(&all, what, &mut notes)
                    .map(|r| rings_of(&r.to_contours()))
                    .unwrap_or_default()
            };
            (rings, notes)
        })
        .collect();
    let mut rings = Vec::new();
    for (r, notes) in merged {
        rings.extend(r);
        warnings.extend(notes);
    }
    rings
}

pub(crate) fn face(island: &Nested) -> Face {
    let (outer, holes) = fit(island);
    Face { outer, holes }
}

/// The silkscreen of one side. Drills are cut and the board edge
/// applied only to the islands they actually meet.
#[allow(clippy::too_many_arguments)]
fn silk_islands(
    board: &Board,
    frame: Frame,
    edge: &BoardEdge,
    fonts: &Fonts,
    tech: Tech,
    variables: &[(String, String)],
    warnings: &mut Vec<String>,
) -> Vec<Nested> {
    let what = format!("{} silkscreen", if tech.front() { "front" } else { "back" });
    let items = drawn_items(board, frame, fonts, tech, variables, warnings);
    let islands = nest(union_rings(items, &what, warnings));
    let boxes: Vec<(Vec2, Vec2)> = islands.iter().map(|i| ring_bbox(&i.outer)).collect();
    let outline = boxes
        .iter()
        .any(|b| edge.meets(*b))
        .then(|| edge.region(warnings))
        .flatten();
    let cutters: Vec<(ContourBuf, (Vec2, Vec2))> = pierce_edges(board, frame, tech)
        .iter()
        .map(|edges| (contour(edges), edges_bbox(edges)))
        .collect();
    let work: Vec<(Nested, (Vec2, Vec2))> = islands.into_iter().zip(boxes).collect();
    let trimmed: Vec<_> = work
        .into_par_iter()
        .with_max_len(1)
        .map(|(island, bbox)| {
            let mut notes = Vec::new();
            let hits: Vec<ContourBuf> = cutters
                .iter()
                .filter(|(_, b)| overlaps(*b, bbox))
                .map(|(c, _)| c.clone())
                .collect();
            let outline = outline.as_ref().filter(|_| edge.meets(bbox));
            if hits.is_empty() && outline.is_none() {
                // Untouched: wholly on the board, or wholly off it.
                let on_board = edge.contains(island.outer[0]);
                return (on_board.then_some(island).into_iter().collect(), notes);
            }
            let Some(mut region) = self::region(&island_contours(&island), &what, &mut notes)
            else {
                return (Vec::new(), notes);
            };
            if !hits.is_empty() {
                region = less(region, &hits, &what, &mut notes);
            }
            if let Some(outline) = outline {
                let result = region.intersection(outline);
                region = or_keep(result, region, &what, &mut notes);
            }
            (nested_of(&region), notes)
        })
        .collect();
    let mut out = Vec::new();
    for (islands, notes) in trimmed {
        out.extend(islands);
        warnings.extend(notes);
    }
    out
}

fn mask_what(front: bool) -> String {
    format!("{} solder mask", if front { "front" } else { "back" })
}

/// The solder mask openings of one side, unioned.
fn mask_openings(
    board: &Board,
    frame: Frame,
    fonts: &Fonts,
    tech: Tech,
    cut_vias: bool,
    variables: &[(String, String)],
    warnings: &mut Vec<String>,
) -> Vec<Nested> {
    let what = mask_what(tech.front());
    let side = usize::from(!tech.front());
    let mut openings: Vec<Vec<ContourBuf>> = Vec::new();
    // Pads open the mask by their shape grown by the mask margin; a pad
    // with no copper opens exactly its shape.
    for pad in board.pads.iter().filter(|p| p.mask[side]) {
        let margin = if pad.layers == 0 {
            0.0
        } else {
            pad.mask_margin
                .unwrap_or(board.mask_expansion)
                .max(-pad.size.x.min(pad.size.y) * 0.5)
        };
        if let Some(edges) = grown_pad_outline(pad, frame, margin) {
            openings.push(vec![contour(&edges)]);
            continue;
        }
        let Some(outline) = pad_outline(board, frame, pad, warnings) else {
            continue;
        };
        let exact = contour(&outline);
        if margin.abs() < 1e-9 {
            openings.push(vec![exact]);
            continue;
        }
        let grown = ContourSet::from_contours(
            std::slice::from_ref(&exact),
            FillRule::NonZero,
            resolution(),
        )
        .and_then(|set| {
            if margin > 0.0 {
                set.disk_dilate(margin)
            } else {
                set.disk_erode(-margin)
            }
        });
        openings.push(match grown {
            Ok(set) => set.to_contours(),
            Err(_) => vec![exact],
        });
    }
    // An untented via opens the mask by its ring plus the board margin.
    for via in board.vias.iter().filter(|v| v.size > 0.0) {
        if via_reaches(board, via, tech.front()) && open_via(board, via, side) {
            openings.push(vec![contour(&circle_edges(
                frame.point(via.at),
                via.size * 0.5 + board.mask_expansion,
            ))]);
        }
    }
    // As KiCad's VRML export, tented or not.
    if cut_vias {
        for via in board.vias.iter().filter(|v| v.drill > 0.0) {
            if via_reaches(board, via, tech.front()) {
                openings.push(vec![contour(&circle_edges(
                    frame.point(via.at),
                    via.drill * 0.5,
                ))]);
            }
        }
    }
    openings.extend(drawn_items(board, frame, fonts, tech, variables, warnings));
    openings.extend(
        pierce_edges(board, frame, tech)
            .iter()
            .map(|e| vec![contour(e)]),
    );
    nest(union_rings(openings, &what, warnings))
}

/// The solder mask of one side: the board less its `openings`. Openings
/// clear of the board edge are holes as they are; only those meeting it
/// are taken out of the outline with a boolean.
pub(crate) fn mask_coverage(
    edge: &BoardEdge,
    front: bool,
    openings: Vec<Nested>,
    warnings: &mut Vec<String>,
) -> Vec<Nested> {
    let what = mask_what(front);
    let Some(mut region) = edge.region(warnings) else {
        return Vec::new();
    };
    let mut rings = Vec::new();
    let mut at_edge = Vec::new();
    for island in openings {
        if edge.meets(ring_bbox(&island.outer)) {
            at_edge.extend(island_contours(&island));
        } else if edge.contains(island.outer[0]) {
            rings.push(island.outer);
            rings.extend(island.holes);
        }
    }
    if !at_edge.is_empty() {
        region = less(region, &at_edge, &what, warnings);
    }
    rings.extend(rings_of(&region.to_contours()));
    nest(rings)
}

/// Solder paste as KiCad's VRML export draws it, front side first: the
/// sides with any paste and their islands: the pads' apertures and what
/// is drawn on the paste layer. DNP footprints' pads are left out unless
/// `include_dnp`.
pub(crate) fn paste(
    board: &Board,
    frame: Frame,
    fonts: &Fonts,
    include_dnp: bool,
    variables: &[(String, String)],
    warnings: &mut Vec<String>,
) -> Vec<(bool, Vec<Nested>)> {
    let mut layers = Vec::new();
    for front in [true, false] {
        let side = usize::from(!front);
        let what = format!("{} solder paste", if front { "front" } else { "back" });
        let mut openings: Vec<Vec<ContourBuf>> = Vec::new();
        for pad in board.pads.iter().filter(|p| {
            p.paste[side] && (include_dnp || !board.footprints[p.footprint as usize].dnp)
        }) {
            let margin = paste_margin(board, pad);
            if let Some(contours) = paste_aperture(board, frame, pad, margin, warnings) {
                openings.push(contours);
            }
        }
        let tech = if front {
            Tech::FrontPaste
        } else {
            Tech::BackPaste
        };
        openings.extend(drawn_items(board, frame, fonts, tech, variables, warnings));
        if openings.is_empty() {
            continue;
        }
        let islands = nest(union_rings(openings, &what, warnings));
        if !islands.is_empty() {
            layers.push((front, islands));
        }
    }
    layers
}

/// `PAD::GetSolderPasteMargin`: the margin on each axis.
fn paste_margin(board: &Board, pad: &crate::board::Pad) -> Vec2 {
    let margin = pad.paste_margin.unwrap_or(board.paste_margin);
    let ratio = pad.paste_ratio.unwrap_or(board.paste_ratio);
    Vec2::new(
        (margin + pad.size.x * ratio).max(-pad.size.x * 0.5),
        (margin + pad.size.y * ratio).max(-pad.size.y * 0.5),
    )
}

/// An even margin offsets the outline; an uneven one resizes the pad.
fn paste_aperture(
    board: &Board,
    frame: Frame,
    pad: &crate::board::Pad,
    margin: Vec2,
    warnings: &mut Vec<String>,
) -> Option<Vec<ContourBuf>> {
    use crate::board::PadShape;
    let custom = matches!(pad.shape, PadShape::Custom { .. });
    if margin.x.abs().max(margin.y.abs()) < 1e-9 {
        return pad_outline(board, frame, pad, warnings).map(|e| vec![contour(&e)]);
    }
    let even = (margin.x - margin.y).abs() < 1e-9;
    if even || custom {
        // Custom pads take the x margin on both axes.
        if let Some(edges) = grown_pad_outline(pad, frame, margin.x) {
            return Some(vec![contour(&edges)]);
        }
        let exact = contour(&pad_outline(board, frame, pad, warnings)?);
        let grown = ContourSet::from_contours(
            std::slice::from_ref(&exact),
            FillRule::NonZero,
            resolution(),
        )
        .and_then(|set| {
            if margin.x > 0.0 {
                set.disk_dilate(margin.x)
            } else {
                set.disk_erode(-margin.x)
            }
        });
        return Some(match grown {
            Ok(set) => set.to_contours(),
            Err(_) => vec![exact],
        });
    }
    let mut resized = pad.clone();
    resized.size = Vec2::new(pad.size.x + 2.0 * margin.x, pad.size.y + 2.0 * margin.y);
    if resized.size.x <= 1e-9 || resized.size.y <= 1e-9 {
        return None;
    }
    pad_outline(board, frame, &resized, warnings).map(|e| vec![contour(&e)])
}
