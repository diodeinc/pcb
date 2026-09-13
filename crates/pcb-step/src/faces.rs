//! Silkscreen and solder mask as flat faces just above the copper, the
//! way `kicad-cli` exports them: silkscreen is the union of its graphics
//! and text with the drills taken out, clipped to the board; the mask is
//! the board with every opening taken out.

use pcb_ir::geom::{BBox, ContourBuf, ContourSet, FillRule, PathCmd};

use crate::board::{Board, Machining, Mouth, Physical, Shape, ShapeKind, Tech, Text, Via};
use crate::copper::{
    PLATING, circle_edges, contour, grown_pad_outline, pad_drill, pad_outline, point, resolution,
    stroke_contour,
};
use crate::font;
use crate::geom::{Vec2, point_in_polygon, signed_area};
use crate::outline::{Edge, Frame, Loop, Solid, board_solids, flatten, orient};
use crate::rings::{Nested, fit, nest, nested_of, rings_of};

/// Height of the silkscreen above the outer copper.
const SILK_ABOVE_COPPER: f64 = 0.04;
/// Height of the solder mask above the outer copper.
const MASK_ABOVE_COPPER: f64 = 0.015;
/// Chord error for the board edge, KiCad's maximum.
const OUTLINE_ERROR: f64 = 0.005;

/// One face of a tech layer.
pub(crate) struct Face {
    pub(crate) outer: Loop,
    pub(crate) holes: Vec<Loop>,
}

/// The faces of one tech layer and where they sit.
pub(crate) struct TechLayer {
    pub(crate) tech: Tech,
    pub(crate) z: f64,
    pub(crate) faces: Vec<Face>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build(
    board: &Board,
    frame: Frame,
    physical: &Physical,
    silk: bool,
    mask: bool,
    variables: &[(String, String)],
    threads: usize,
    warnings: &mut Vec<String>,
) -> Result<Vec<TechLayer>, crate::Error> {
    let edge = BoardEdge::new(&board_solids(board, frame)?);
    let mut layers = Vec::new();
    if silk {
        layers.push(Tech::FrontSilk);
        layers.push(Tech::BackSilk);
    }
    if mask {
        layers.push(Tech::FrontMask);
        layers.push(Tech::BackMask);
    }
    let built = crate::parallel_map(threads, &layers, |tech| {
        let mut notes = Vec::new();
        let faces = if tech.silk() {
            silk_faces(board, frame, &edge, *tech, variables, threads, &mut notes)
        } else {
            mask_faces(board, frame, &edge, *tech, variables, threads, &mut notes)
        };
        (faces, notes)
    });
    let mut out = Vec::with_capacity(layers.len());
    for (tech, (faces, notes)) in layers.into_iter().zip(built) {
        warnings.extend(notes);
        let last = physical.copper_z.len() - 1;
        let above = if tech.silk() {
            SILK_ABOVE_COPPER
        } else {
            MASK_ABOVE_COPPER
        };
        let z = if tech.front() {
            physical.copper_z[0].1 + above
        } else {
            physical.copper_z[last].0 - above
        };
        out.push(TechLayer { tech, z, faces });
    }
    Ok(out)
}

/// The board edge flattened to chords within KiCad's maximum error: a
/// long arc of large radius does not fit pcb-ir's arc budget, and KiCad
/// clips to a polygon too.
struct BoardEdge {
    /// Outers counter-clockwise, cutouts clockwise.
    rings: Vec<Vec<Vec2>>,
    /// Bounding box of every ring segment.
    segments: Vec<(Vec2, Vec2)>,
}

impl BoardEdge {
    fn new(outline: &[Solid]) -> Self {
        let mut rings = Vec::new();
        for solid in outline {
            rings.push(flatten(&solid.outer.edges, OUTLINE_ERROR));
            for hole in &solid.holes {
                rings.push(flatten(&hole.edges, OUTLINE_ERROR));
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
    fn meets(&self, (lo, hi): (Vec2, Vec2)) -> bool {
        self.segments
            .iter()
            .any(|(slo, shi)| slo.x <= hi.x && lo.x <= shi.x && slo.y <= hi.y && lo.y <= shi.y)
    }

    /// Whether `p` is on the board: inside an outer and not in a cutout.
    fn contains(&self, p: Vec2) -> bool {
        self.rings.iter().filter(|r| point_in_polygon(p, r)).count() % 2 == 1
    }

    fn region(&self, warnings: &mut Vec<String>) -> Option<ContourSet> {
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
fn island_contours(island: &Nested) -> Vec<ContourBuf> {
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
fn drawn_items(
    board: &Board,
    frame: Frame,
    tech: Tech,
    variables: &[(String, String)],
    threads: usize,
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
    crate::parallel_map(threads, &items, |item| {
        let mut out = Vec::new();
        match item {
            Item::Shape(shape) => shape_contours(board, frame, shape, &mut out),
            Item::Text(text) => text_contours(frame, text, variables, &mut out),
        }
        out
    })
}

/// `${NAME}` replaced by the project's text variables; an unknown name
/// stays as written, as KiCad shows it.
pub(crate) fn expand(text: &str, variables: &[(String, String)]) -> String {
    let mut out = text.to_owned();
    // A value may name another variable, as a title block revision of
    // `${PCB_VERSION}` does; a few passes settle them.
    for _ in 0..4 {
        let next = variables.iter().fold(out.clone(), |t, (name, value)| {
            t.replace(&format!("${{{name}}}"), value)
        });
        if next == out {
            break;
        }
        out = next;
    }
    out
}

fn shape_contours(board: &Board, frame: Frame, shape: &Shape, out: &mut Vec<ContourBuf>) {
    let w = shape.width;
    let stroke = |a: Vec2, mid: Vec2, b: Vec2, out: &mut Vec<ContourBuf>| {
        if let Some(c) = stroke_contour(frame.point(a), frame.point(mid), frame.point(b), w) {
            out.push(c);
        }
    };
    match shape.kind {
        ShapeKind::Line { a, b } => stroke(a, a, b, out),
        ShapeKind::Arc { a, mid, b } => stroke(a, mid, b, out),
        ShapeKind::Circle { center, radius } => {
            let c = frame.point(center);
            if shape.filled {
                out.push(contour(&circle_edges(c, radius + w * 0.5)));
            } else if w > 0.0 {
                // A ring: the outer circle with the inner one as a hole.
                out.push(contour(&circle_edges(c, radius + w * 0.5)));
                if radius > w * 0.5 {
                    out.push(contour(&orient(circle_edges(c, radius - w * 0.5), false)));
                }
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
                for k in 0..pts.len() {
                    let (a, b) = (pts[k], pts[(k + 1) % pts.len()]);
                    if let Some(c) = stroke_contour(a, a, b, w) {
                        out.push(c);
                    }
                }
            }
        }
    }
}

fn text_contours(
    frame: Frame,
    text: &Text,
    variables: &[(String, String)],
    out: &mut Vec<ContourBuf>,
) {
    let pen = text.style.pen_width();
    let content = expand(&text.text, variables);
    for stroke in font::strokes(&content, text.at, &text.style) {
        let pts: Vec<Vec2> = stroke.iter().map(|p| frame.point(*p)).collect();
        if pts.len() == 1 {
            out.push(contour(&circle_edges(pts[0], pen * 0.5)));
        }
        for pair in pts.windows(2) {
            if let Some(c) = stroke_contour(pair[0], pair[0], pair[1], pen) {
                out.push(c);
            }
        }
    }
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

/// The cleaned rings of the union of `items`, unioned cell by cell:
/// items are binned by position, each bin is unioned on its own thread,
/// and only bins whose contours overlap are unioned together. Strokes
/// overlap within a text, openings within a footprint; the rest of the
/// board is disjoint and costs nothing to combine.
fn union_rings(
    items: Vec<Vec<ContourBuf>>,
    threads: usize,
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
    let per_axis = ((threads * 8) as f64).sqrt().ceil() as usize;
    let cell = |v: f64, lo: f64, hi: f64| {
        (((v - lo) / (hi - lo).max(1e-9) * per_axis as f64) as usize).min(per_axis - 1)
    };
    let mut bins: Vec<Vec<ContourBuf>> = vec![Vec::new(); per_axis * per_axis];
    for item in items.into_iter().filter(|i| !i.is_empty()) {
        let c = bbox_of(&item).center();
        let k = cell(c.y, all.min.y, all.max.y) * per_axis + cell(c.x, all.min.x, all.max.x);
        bins[k].extend(item);
    }
    bins.retain(|b| !b.is_empty());
    let unioned = crate::parallel_map(threads, &bins, |bin| {
        let mut notes = Vec::new();
        let contours = region(bin, what, &mut notes)
            .map(|r| r.to_contours())
            .unwrap_or_default();
        (contours, notes)
    });
    let mut bins = Vec::with_capacity(unioned.len());
    for (contours, notes) in unioned {
        warnings.extend(notes);
        bins.push(contours);
    }
    // Bins with a pair of overlapping contours are unioned together;
    // only contours reaching into the other bin's box can be that pair.
    let boxes: Vec<BBox> = bins.iter().map(|b| bbox_of(b)).collect();
    let touching = |i: usize, j: usize| {
        if !boxes[i].intersects(boxes[j]) {
            return false;
        }
        let shared = BBox::new(
            point(
                Vec2::new(boxes[i].min.x, boxes[i].min.y)
                    .max(Vec2::new(boxes[j].min.x, boxes[j].min.y)),
            ),
            point(
                Vec2::new(boxes[i].max.x, boxes[i].max.y)
                    .min(Vec2::new(boxes[j].max.x, boxes[j].max.y)),
            ),
        );
        let reaching = |bin: &[ContourBuf]| -> Vec<BBox> {
            bin.iter()
                .map(|c| c.bbox)
                .filter(|b| b.intersects(shared))
                .collect()
        };
        let (a, b) = (reaching(&bins[i]), reaching(&bins[j]));
        a.iter().any(|a| b.iter().any(|b| a.intersects(*b)))
    };
    let mut clusters: Vec<Vec<usize>> = Vec::new();
    let mut clustered = vec![false; bins.len()];
    for i in 0..bins.len() {
        if clustered[i] {
            continue;
        }
        clustered[i] = true;
        let mut members = vec![i];
        let mut k = 0;
        while k < members.len() {
            let found: Vec<usize> = (0..bins.len())
                .filter(|&j| !clustered[j] && touching(members[k], j))
                .collect();
            for j in found {
                clustered[j] = true;
                members.push(j);
            }
            k += 1;
        }
        clusters.push(members);
    }
    let merged = crate::parallel_map(threads, &clusters, |ids| {
        let mut notes = Vec::new();
        let rings = if let [only] = ids[..] {
            rings_of(&bins[only])
        } else {
            let all: Vec<ContourBuf> = ids.iter().flat_map(|&i| bins[i].iter().cloned()).collect();
            region(&all, what, &mut notes)
                .map(|r| rings_of(&r.to_contours()))
                .unwrap_or_default()
        };
        (rings, notes)
    });
    let mut rings = Vec::new();
    for (r, notes) in merged {
        rings.extend(r);
        warnings.extend(notes);
    }
    rings
}

fn face(island: &Nested, threads: usize) -> Face {
    let (outer, holes) = fit(island, threads);
    Face { outer, holes }
}

/// The silkscreen of one side. Drills are cut and the board edge
/// applied only to the islands they actually meet.
fn silk_faces(
    board: &Board,
    frame: Frame,
    edge: &BoardEdge,
    tech: Tech,
    variables: &[(String, String)],
    threads: usize,
    warnings: &mut Vec<String>,
) -> Vec<Face> {
    let what = format!("{} silkscreen", if tech.front() { "front" } else { "back" });
    let items = drawn_items(board, frame, tech, variables, threads);
    let islands = nest(union_rings(items, threads, &what, warnings));
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
    let work: Vec<(&Nested, (Vec2, Vec2))> = islands.iter().zip(boxes).collect();
    let trimmed = crate::parallel_map(threads, &work, |&(island, bbox)| {
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
            return (
                on_board.then(|| face(island, 1)).into_iter().collect(),
                notes,
            );
        }
        let Some(mut region) = self::region(&island_contours(island), &what, &mut notes) else {
            return (Vec::new(), notes);
        };
        if !hits.is_empty() {
            region = less(region, &hits, &what, &mut notes);
        }
        if let Some(outline) = outline {
            let result = region.intersection(outline);
            region = or_keep(result, region, &what, &mut notes);
        }
        let faces = nested_of(&region).iter().map(|i| face(i, 1)).collect();
        (faces, notes)
    });
    let mut faces = Vec::new();
    for (f, notes) in trimmed {
        faces.extend(f);
        warnings.extend(notes);
    }
    faces
}

/// The solder mask of one side: the board less every opening. Openings
/// clear of the board edge are holes as they are; only those meeting it
/// are taken out of the outline with a boolean.
fn mask_faces(
    board: &Board,
    frame: Frame,
    edge: &BoardEdge,
    tech: Tech,
    variables: &[(String, String)],
    threads: usize,
    warnings: &mut Vec<String>,
) -> Vec<Face> {
    let what = format!(
        "{} solder mask",
        if tech.front() { "front" } else { "back" }
    );
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
    openings.extend(drawn_items(board, frame, tech, variables, threads));
    openings.extend(
        pierce_edges(board, frame, tech)
            .iter()
            .map(|e| vec![contour(e)]),
    );
    let Some(mut region) = edge.region(warnings) else {
        return Vec::new();
    };
    let mut rings = Vec::new();
    let mut at_edge = Vec::new();
    for island in nest(union_rings(openings, threads, &what, warnings)) {
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
        .iter()
        .map(|island| face(island, threads))
        .collect()
}
