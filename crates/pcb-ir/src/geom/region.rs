//! Regularized planar regions and boolean composition.
//!
//! The flattened polygon form used for boolean set operations is a list of
//! [`Ring`]s (closed polygon boundaries). [`ContourSet`] is the regularized
//! region type built on top: union, difference, intersection, and disk
//! dilation over filled point sets, shared by every dialect so IPC, Gerber,
//! SVG, and comparison all use the same geometry semantics.

use std::fmt;

use boostvoronoi::prelude::{
    Builder as VoronoiBuilder, CellIndex as VoronoiCellIndex, Diagram as VoronoiDiagram,
    EdgeIndex as VoronoiEdgeIndex, Line as VoronoiLine, Point as VoronoiPoint, SourceCategory,
    VoronoiVisualUtils,
};
use boostvoronoi::utils::visual_utils::SimpleAffine;
use i_overlay::core::fill_rule::FillRule as OverlayFillRule;
use i_overlay::core::overlay::IntOverlayOptions;
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::core::simplify::Simplify;
use i_overlay::float::simplify::SimplifyShape;
use i_overlay::float::single::SingleFloatOverlay;
use i_overlay::i_float::int::point::IntPoint;
use i_overlay::mesh::outline::offset::OutlineOffset;
use i_overlay::mesh::style::{LineJoin as OutlineLineJoin, OutlineStyle};

use crate::geom::affine::Affine2;
use crate::geom::bbox::BBox;
use crate::geom::dist::{self, Distance};
use crate::geom::grid::CellGrid;
use crate::geom::path::{ContourBuf, PathCmd, contours_to_kurbo, stroke_to_fill, transform_cmds};
use crate::geom::point::Point;
use crate::geom::store::{Path, PathArena};
use crate::geom::style::{FillRule, Paint, Polarity};
use crate::geom::tol;

/// A closed polygon boundary, flattened to line segments.
pub type Ring = Vec<[f64; 2]>;

/// One connected polygon: an outer ring plus hole rings.
pub type Shape = Vec<Ring>;

/// Flatten contours to polygon rings using the shared chord tolerance.
pub fn rings_from_contours(contours: &[ContourBuf]) -> Vec<Ring> {
    let bez_path = contours_to_kurbo(contours);
    let mut rings = Vec::new();
    let mut current = Vec::new();
    kurbo::flatten(bez_path, tol::FLATTEN_MM, |element| match element {
        kurbo::PathEl::MoveTo(point) => {
            push_ring(&mut rings, &mut current);
            current.push([point.x, point.y]);
        }
        kurbo::PathEl::LineTo(point) => current.push([point.x, point.y]),
        kurbo::PathEl::ClosePath => push_ring(&mut rings, &mut current),
        kurbo::PathEl::QuadTo(..) | kurbo::PathEl::CurveTo(..) => {
            unreachable!("kurbo::flatten emits lines")
        }
    });
    push_ring(&mut rings, &mut current);
    rings
}

/// Convert polygon rings back into closed line contours.
pub fn rings_to_contours(rings: Vec<Ring>) -> Vec<ContourBuf> {
    rings.into_iter().filter_map(ring_to_contour).collect()
}

/// Parameter intervals of a segment inside a filled region. Boundary
/// crossings supply exact split points in the flattened representation;
/// midpoint containment then decides each interval, including cutouts.
pub(crate) fn segment_inside_intervals(
    region: &ContourSet,
    start: Point,
    end: Point,
) -> Vec<(f64, f64)> {
    let delta = end - start;
    let length_squared = delta.x * delta.x + delta.y * delta.y;
    if length_squared <= tol::EPSILON_MM * tol::EPSILON_MM {
        return Vec::new();
    }
    let cross = |a: Point, b: Point| a.x * b.y - a.y * b.x;
    let mut stations = vec![0.0, 1.0];
    for (a, b) in region.rings.iter().flat_map(ring_edges) {
        let edge = b - a;
        let denominator = cross(delta, edge);
        if denominator.abs() > tol::EPSILON_MM * delta.length().max(edge.length()) {
            let t = cross(a - start, edge) / denominator;
            let u = cross(a - start, delta) / denominator;
            if (0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u) {
                stations.push(t);
            }
        } else if cross(a - start, delta).abs() <= tol::EPSILON_MM * delta.length() {
            for point in [a, b] {
                let relative = point - start;
                let t = (relative.x * delta.x + relative.y * delta.y) / length_squared;
                if (0.0..=1.0).contains(&t) {
                    stations.push(t);
                }
            }
        }
    }
    stations.sort_by(f64::total_cmp);
    stations.dedup_by(|left, right| (*left - *right).abs() <= f64::EPSILON);
    let midpoints = stations
        .windows(2)
        .map(|pair| start + delta * ((pair[0] + pair[1]) / 2.0))
        .collect::<Vec<_>>();
    region
        .contains_points_batch(&midpoints)
        .into_iter()
        .zip(stations.windows(2))
        .filter_map(|(inside, pair)| inside.then_some((pair[0], pair[1])))
        .collect()
}

/// Sublevel set of a continuous convex function on [0, 1]. The geometric
/// uses here are sums of point-to-segment distances, so there is at most one
/// interval and bisection locates its ends independently of render sampling.
fn convex_sublevel_interval(value: impl Fn(f64) -> f64, limit: f64) -> Option<(f64, f64)> {
    let (first, last) = (value(0.0), value(1.0));
    if first <= limit && last <= limit {
        return Some((0.0, 1.0));
    }
    let (mut low, mut high) = (0.0, 1.0);
    for _ in 0..48 {
        let a = (2.0 * low + high) / 3.0;
        let b = (low + 2.0 * high) / 3.0;
        if value(a) < value(b) {
            high = b;
        } else {
            low = a;
        }
    }
    let minimum = (low + high) / 2.0;
    if value(minimum) >= limit {
        return None;
    }
    let left = if first <= limit {
        0.0
    } else {
        let (mut outside, mut inside) = (0.0, minimum);
        for _ in 0..48 {
            let middle = (outside + inside) / 2.0;
            if value(middle) < limit {
                inside = middle;
            } else {
                outside = middle;
            }
        }
        inside
    };
    let right = if last <= limit {
        1.0
    } else {
        let (mut inside, mut outside) = (minimum, 1.0);
        for _ in 0..48 {
            let middle = (inside + outside) / 2.0;
            if value(middle) < limit {
                inside = middle;
            } else {
                outside = middle;
            }
        }
        inside
    };
    Some((left, right))
}

/// Regularize rings under the given fill rule into non-overlapping shapes.
pub fn simplify_rings(rings: Vec<Ring>, fill_rule: FillRule) -> Vec<Ring> {
    flatten_shapes(simplify_shapes(rings, fill_rule))
}

/// Regularize rings keeping the connected-shape structure: each shape is its
/// outer ring followed by its holes, wound opposite.
pub fn simplify_shapes(rings: Vec<Ring>, fill_rule: FillRule) -> Vec<Shape> {
    rings.simplify_shape(overlay_fill_rule(fill_rule))
}

/// Regularize filled rings on an exact output grid.
///
/// The fixed-scale integer overlay resolves crossings, removes coincident
/// vertices and merges collinear edges while snapping every result vertex to
/// `grid`. Geometry that collapses during coordinate quantization is not
/// representable on that output grid.
pub fn simplify_shapes_on_grid(rings: Vec<Ring>, fill_rule: FillRule, grid: f64) -> Vec<Shape> {
    let rings = rings
        .into_iter()
        .map(|ring| {
            ring.into_iter()
                .map(|[x, y]| IntPoint::new((x / grid).round() as i64, (y / grid).round() as i64))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    rings
        .as_slice()
        .simplify(overlay_fill_rule(fill_rule), IntOverlayOptions::default())
        .into_iter()
        .map(|shape| {
            shape
                .into_iter()
                .map(|ring| {
                    ring.into_iter()
                        .map(|point| [point.x as f64 * grid, point.y as f64 * grid])
                        .collect::<Ring>()
                })
                .collect::<Shape>()
        })
        .collect()
}

pub fn union_rings(rings: Vec<Ring>, fill_rule: FillRule) -> Vec<Ring> {
    simplify_rings(rings, fill_rule)
}

pub fn difference_rings(subject: Vec<Ring>, cutters: Vec<Ring>) -> Vec<Ring> {
    flatten_shapes(difference_shapes(subject, cutters))
}

pub fn intersection_rings(subject: Vec<Ring>, clip: Vec<Ring>) -> Vec<Ring> {
    if subject.is_empty() || clip.is_empty() {
        return Vec::new();
    }
    flatten_shapes(subject.overlay(&clip, OverlayRule::Intersect, OverlayFillRule::NonZero))
}

/// Difference keeping the connected-shape structure of the result.
pub fn difference_shapes(subject: Vec<Ring>, cutters: Vec<Ring>) -> Vec<Shape> {
    if subject.is_empty() || cutters.is_empty() {
        return subject.simplify_shape(OverlayFillRule::NonZero);
    }
    subject.overlay(&cutters, OverlayRule::Difference, OverlayFillRule::NonZero)
}

pub fn rings_bbox(rings: &[Ring]) -> BBox {
    rings
        .iter()
        .flat_map(|ring| ring.iter())
        .fold(BBox::empty(), |mut bbox, &[x, y]| {
            bbox.include_point(Point::new(x, y));
            bbox
        })
}

/// Decimate rings so the region only shrinks: the result covers no point the
/// source did not, and no source vertex ends farther than `deviation_mm`
/// from the decimated boundary.
pub fn decimate_rings_inward(rings: &[Ring], deviation_mm: f64) -> Vec<Ring> {
    rings
        .iter()
        .map(|ring| decimate_ring_inward(ring, deviation_mm))
        .collect()
}

fn decimate_ring_inward(ring: &Ring, deviation_mm: f64) -> Ring {
    if ring.len() < 4 {
        return ring.clone();
    }
    let point = |index: usize| {
        let [x, y] = ring[index % ring.len()];
        Point::new(x, y)
    };
    // Rings keep material on the left of travel, so a chord absorbs the
    // vertices between its ends exactly when every one lies on the chord's
    // right — the removed bulge is material — and within the deviation.
    // Every chord re-checks its whole chain, so error cannot accumulate.
    let chord_absorbs = |anchor: usize, end: usize| {
        let start = point(anchor);
        let chord = point(end) - start;
        let length = chord.length();
        if length <= f64::EPSILON {
            return false;
        }
        (anchor + 1..end).all(|index| {
            let offset = point(index) - start;
            let cross = chord.x * offset.y - chord.y * offset.x;
            cross <= 0.0 && -cross / length <= deviation_mm
        })
    };

    let mut kept = vec![ring[0]];
    let mut anchor = 0;
    while anchor + 1 < ring.len() {
        // Grow the chord greedily; `end == ring.len()` is the closing chord
        // back to the first vertex, which absorbs the remaining tail.
        let mut end = anchor + 1;
        while end < ring.len() && chord_absorbs(anchor, end + 1) {
            end += 1;
        }
        if end == ring.len() {
            break;
        }
        kept.push(ring[end]);
        anchor = end;
    }
    if kept.len() < 3 {
        return ring.clone();
    }
    kept
}

/// The closed edge cycle of one ring, as start/end point pairs.
pub fn ring_edges(ring: &Ring) -> impl Iterator<Item = (Point, Point)> + '_ {
    ring.iter()
        .copied()
        .zip(ring.iter().copied().cycle().skip(1))
        .take(ring.len())
        .map(|([x0, y0], [x1, y1])| (Point::new(x0, y0), Point::new(x1, y1)))
}

/// Signed area of one ring (positive when counter-clockwise).
pub fn ring_signed_area(ring: &Ring) -> f64 {
    if ring.len() < 3 {
        return 0.0;
    }
    let mut area = 0.0;
    for index in 0..ring.len() {
        let [x0, y0] = ring[index];
        let [x1, y1] = ring[(index + 1) % ring.len()];
        area += x0 * y1 - x1 * y0;
    }
    area / 2.0
}

/// Sutherland-Hodgman clip of a closed ring against a half-plane, kept where
/// `inside` is non-negative.
///
/// A concave ring can come back with edges doubled back along the cut. They
/// enclose nothing, so the signed area of the result is the true clipped area,
/// which is all this is used for.
fn clip_half_plane(ring: &Ring, inside: impl Fn([f64; 2]) -> f64) -> Ring {
    let mut clipped = Ring::new();
    for index in 0..ring.len() {
        let start = ring[index];
        let end = ring[(index + 1) % ring.len()];
        let (from, to) = (inside(start), inside(end));
        if (from < 0.0) != (to < 0.0) {
            let step = from / (from - to);
            clipped.push([
                start[0] + step * (end[0] - start[0]),
                start[1] + step * (end[1] - start[1]),
            ]);
        }
        if to >= 0.0 {
            clipped.push(end);
        }
    }
    clipped
}

/// Net enclosed area of a regularized ring set (holes are wound opposite the
/// outer boundary, so summing signed areas subtracts them).
pub fn rings_area(rings: &[Ring]) -> f64 {
    rings.iter().map(ring_signed_area).sum::<f64>().abs()
}

/// Regularized filled planar point set.
///
/// A `ContourSet` is always in canonical form: rings are regularized
/// (non-overlapping, holes wound opposite their outer boundary) and contours
/// smaller than `tolerance²` in area are discarded. The winding/fill rule of
/// the *source* geometry matters only at construction; every subsequent
/// operation is a regularized set operation.
#[derive(Debug, Clone)]
pub struct ContourSet {
    pub bbox: BBox,
    pub rings: Vec<Ring>,
    /// Bounds of each ring, indexed like `rings`.
    pub ring_bounds: Vec<BBox>,
    pub tolerance: f64,
}

/// Result of enforcing a minimum width for every two-sided void gap.
#[derive(Debug, Clone)]
pub struct DiskGapRegularization {
    /// Input material retained after local gap trimming and disk opening.
    pub kept: ContourSet,
    /// `source \ kept`.
    pub removed: ContourSet,
}

/// One connected opening/closing residue that two distinct source boundary
/// branches wall. `width` is the diameter of the narrowest maximal inscribed
/// disk inside it — the local width of the material or void `region`
/// represents, exact for the flattened polygon representation — counting
/// both branches as flattened inputs.
#[derive(Debug, Clone)]
pub(crate) struct TwoSidedResidualComponent {
    pub region: ContourSet,
    pub width: Distance,
    pub disk: InscribedDisk,
    pub axis: Vec<WidthAxisSegment>,
}

/// Failure to construct a narrow void's medial axis for gap regularization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GapRegularizationError(String);

impl fmt::Display for GapRegularizationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for GapRegularizationError {}

impl ContourSet {
    pub fn new(rings: Vec<Ring>, fill_rule: FillRule, tolerance: f64) -> Self {
        let rings = filter_significant_rings(simplify_rings(rings, fill_rule), tolerance);
        let ring_bounds = rings
            .iter()
            .map(|ring| rings_bbox(std::slice::from_ref(ring)))
            .collect::<Vec<_>>();
        Self {
            bbox: ring_bounds.iter().copied().fold(BBox::empty(), BBox::union),
            rings,
            ring_bounds,
            tolerance,
        }
    }

    /// A region from rings that are already regularized, kept as they are.
    fn from_regularized(rings: Vec<Ring>, tolerance: f64) -> Self {
        let rings = filter_significant_rings(rings, tolerance);
        let ring_bounds = rings
            .iter()
            .map(|ring| rings_bbox(std::slice::from_ref(ring)))
            .collect::<Vec<_>>();
        Self {
            bbox: ring_bounds.iter().copied().fold(BBox::empty(), BBox::union),
            rings,
            ring_bounds,
            tolerance,
        }
    }

    pub fn empty(tolerance: f64) -> Self {
        Self {
            bbox: BBox::empty(),
            rings: Vec::new(),
            ring_bounds: Vec::new(),
            tolerance,
        }
    }

    pub fn from_contours(contours: &[ContourBuf], fill_rule: FillRule, tolerance: f64) -> Self {
        Self::new(rings_from_contours(contours), fill_rule, tolerance)
    }

    /// Build the union of independently filled contours.
    ///
    /// Each contour is filled on its own (even-odd, so nesting makes holes and
    /// winding direction is irrelevant), then the contours are unioned. Use
    /// this when sibling contours are separate features; applying even-odd
    /// across the whole list would XOR duplicated geometry away.
    pub fn from_filled_contours(contours: &[ContourBuf], tolerance: f64) -> Self {
        let rings = contours
            .iter()
            .flat_map(|contour| {
                simplify_rings(
                    rings_from_contours(std::slice::from_ref(contour)),
                    FillRule::EvenOdd,
                )
            })
            .collect();
        Self::new(rings, FillRule::NonZero, tolerance)
    }

    /// Build the union of the geometric images painted by a set of paths.
    ///
    /// Filled paths are interpreted under their own fill rule and stroked
    /// paths are expanded with their native width, cap, and join. Unpainted
    /// paths are ignored. Object/feature polarity is deliberately outside
    /// this operation: this constructs geometric footprints, not a composed
    /// positive/negative layer image.
    pub fn from_painted_paths<'a>(
        arena: &PathArena,
        paths: impl IntoIterator<Item = &'a Path>,
        tolerance: f64,
    ) -> Self {
        Self::from_placed_painted_paths(
            arena,
            paths.into_iter().map(|path| (path, Affine2::IDENTITY)),
            tolerance,
        )
    }

    /// Build the union of painted path occurrences after applying placement.
    ///
    /// Stroke outlines are constructed in the path's local frame before the
    /// affine transform is applied, so mirrored and scaled IPC placements
    /// retain the same geometric meaning as a materialized feature.
    pub fn from_placed_painted_paths<'a>(
        arena: &PathArena,
        paths: impl IntoIterator<Item = (&'a Path, Affine2)>,
        tolerance: f64,
    ) -> Self {
        let mut rings = Vec::new();
        for (path, placement) in paths {
            let contours = arena.path_contours(path);
            let contours = match path.paint {
                Paint::Fill { .. } => contours,
                Paint::Stroke(stroke) => {
                    stroke_to_fill(&contours, stroke.into()).unwrap_or_default()
                }
                Paint::None => continue,
            };
            let contours = if placement.is_identity() {
                contours
            } else {
                contours
                    .into_iter()
                    .map(|contour| transform_cmds(contour.cmds, placement))
                    .collect()
            };
            let fill_rule = match path.paint {
                Paint::Fill { rule } => rule,
                Paint::Stroke(_) => FillRule::NonZero,
                Paint::None => unreachable!("unpainted paths were skipped"),
            };
            let path_rings = simplify_rings(rings_from_contours(&contours), fill_rule);
            rings.extend(path_rings);
        }
        Self::new(rings, FillRule::NonZero, tolerance)
    }

    pub fn rectangle(bbox: BBox, tolerance: f64) -> Self {
        if bbox.is_empty() {
            return Self::empty(tolerance);
        }
        let ring = vec![
            [bbox.min.x, bbox.min.y],
            [bbox.max.x, bbox.min.y],
            [bbox.max.x, bbox.max.y],
            [bbox.min.x, bbox.max.y],
        ];
        Self::new(vec![ring], FillRule::NonZero, tolerance)
    }

    pub fn is_empty(&self) -> bool {
        self.rings.is_empty()
    }

    pub fn bbox(&self) -> BBox {
        self.bbox
    }

    /// Net enclosed area.
    pub fn area(&self) -> f64 {
        rings_area(&self.rings)
    }

    /// Regularized union: `self ∪ other`.
    pub fn union(&self, other: &Self) -> Self {
        if self.is_empty() {
            return other.clone();
        }
        if other.is_empty() {
            return self.clone();
        }
        Self::new(
            flatten_shapes(self.rings.overlay(
                &other.rings,
                OverlayRule::Union,
                OverlayFillRule::NonZero,
            )),
            FillRule::NonZero,
            self.tolerance,
        )
    }

    pub fn union_assign(&mut self, other: &Self) {
        *self = self.union(other);
    }

    /// Regularized difference: `self \ cutters`.
    pub fn difference(&self, cutters: &Self) -> Self {
        Self::new(
            difference_rings(self.rings.clone(), cutters.rings.clone()),
            FillRule::NonZero,
            self.tolerance,
        )
    }

    /// Regularized intersection: `self ∩ clip`.
    pub fn intersection(&self, clip: &Self) -> Self {
        Self::new(
            intersection_rings(self.rings.clone(), clip.rings.clone()),
            FillRule::NonZero,
            self.tolerance,
        )
    }

    /// Connected components, each retaining its own hole rings.
    pub fn connected_components(&self) -> Vec<Self> {
        simplify_shapes(self.rings.clone(), FillRule::NonZero)
            .into_iter()
            .map(|shape| Self::new(shape, FillRule::NonZero, self.tolerance))
            .collect()
    }

    /// The component of each ring, by ring index, and the outer ring of
    /// each component. Regularized rings nest without crossing and holes
    /// are wound opposite their outer ring, so a hole belongs to the
    /// smallest outer ring around it.
    fn ring_components(&self) -> (Vec<usize>, Vec<usize>) {
        let areas = self.rings.iter().map(ring_signed_area).collect::<Vec<_>>();
        let mut outers = (0..self.rings.len())
            .filter(|&ring| areas[ring] > 0.0)
            .collect::<Vec<_>>();
        outers.sort_by(|&left, &right| areas[left].total_cmp(&areas[right]));
        let mut components = vec![usize::MAX; self.rings.len()];
        for (component, &outer) in outers.iter().enumerate() {
            components[outer] = component;
        }
        for (index, ring) in self.rings.iter().enumerate() {
            if areas[index] > 0.0 {
                continue;
            }
            let Some(&[x, y]) = ring.first() else {
                continue;
            };
            let point = Point::new(x, y);
            if let Some(&outer) = outers.iter().find(|&&outer| {
                self.ring_bounds[outer].contains_point(point)
                    && ring_contains_point(&self.rings[outer], point)
            }) {
                components[index] = components[outer];
            }
        }
        (components, outers)
    }

    /// The material within `reach_mm` of walls that face each other across
    /// material within `material_mm` or across void within `void_mm`:
    /// every ring within reach of such a wall, with the outer ring of each
    /// component so reached, clipped to the reach around the facing walls.
    /// A hole farther away is filled and material farther away is cut,
    /// since neither reaches the facing walls; the cut's own edges disturb
    /// the morphology only within a disk diameter of themselves, so a reach
    /// of three radii keeps the residue at the facing walls exact. Walls
    /// face when they are not
    /// incident: on one ring, neither adjacent nor turned the same way,
    /// exactly as the width construction judges a wall pair. Material lies
    /// to the left of travel, so two walls of one component face across
    /// material when each has the other's nearest point on its left and
    /// across void when each has it on its right; a nearest point along a
    /// wall's own line, as at the corners of a notch or of aligned holes,
    /// leaves the side open and both reaches apply. Distinct components
    /// always face across void.
    fn facing_components(&self, material_mm: f64, void_mm: f64, reach_mm: f64) -> Self {
        let (components, outers) = self.ring_components();
        let reach = material_mm.max(void_mm).max(reach_mm);
        let grid = SegmentGrid::new(source_boundary_segments(self), reach);
        let segments = &grid.segments;
        let sees_left = |wall: &OrientedBoundarySegment, across: Point| {
            let along = wall.end - wall.start;
            let turn = along.x * across.y - along.y * across.x;
            let scale = 1e-6 * along.length() * across.length();
            (turn.abs() > scale).then_some(turn > 0.0)
        };
        let faces = |left: &OrientedBoundarySegment, right: &OrientedBoundarySegment| {
            if left.topology.ring == right.topology.ring
                && (boundary_segments_are_incident(left.topology, right.topology)
                    || left.tangent.x * right.tangent.x + left.tangent.y * right.tangent.y >= 0.0)
            {
                return false;
            }
            let same_component = components[left.topology.ring] == components[right.topology.ring];
            let farthest = if same_component {
                material_mm.max(void_mm)
            } else {
                void_mm
            };
            if !left.bbox.expand(farthest).intersects(right.bbox) {
                return false;
            }
            let (separation, nearest_left, nearest_right) =
                dist::segments(left.start, left.end, right.start, right.end);
            let across = nearest_right - nearest_left;
            let reach = match (
                same_component,
                sees_left(left, across),
                sees_left(right, -across),
            ) {
                (false, ..) | (true, Some(false), Some(false)) => void_mm,
                (true, Some(true), Some(true)) => material_mm,
                _ => material_mm.max(void_mm),
            };
            separation <= reach
        };
        let mut facing = vec![false; segments.len()];
        for (id, segment) in segments.iter().enumerate() {
            for other in grid.near_ids(segment.bbox.expand(material_mm.max(void_mm))) {
                let partner = &segments[other as usize];
                if other as usize <= id
                    || (facing[id] && facing[other as usize])
                    || !faces(segment, partner)
                {
                    continue;
                }
                facing[id] = true;
                facing[other as usize] = true;
            }
        }
        let mut kept = vec![false; self.rings.len()];
        let mut windows = Vec::new();
        for segment in segments
            .iter()
            .zip(&facing)
            .filter_map(|(segment, &facing)| facing.then_some(segment))
        {
            let window = segment.bbox.expand(reach_mm);
            windows.push(vec![
                [window.min.x, window.min.y],
                [window.max.x, window.min.y],
                [window.max.x, window.max.y],
                [window.min.x, window.max.y],
            ]);
            for other in grid.near_ids(window) {
                kept[segments[other as usize].topology.ring] = true;
            }
        }
        for (ring, &component) in components.iter().enumerate() {
            if kept[ring] {
                kept[outers[component]] = true;
            }
        }
        let material = Self::from_regularized(
            self.rings
                .iter()
                .zip(&kept)
                .filter(|&(_, &keep)| keep)
                .map(|(ring, _)| ring.clone())
                .collect(),
            self.tolerance,
        );
        material.intersection(&Self::new(windows, FillRule::NonZero, self.tolerance))
    }

    /// Whether the region contains each point, as a one-or-zero indicator.
    ///
    /// Testing points one at a time walks every edge per point. This sweeps
    /// them instead: sort by height, and at each height solve the edge
    /// crossings once and share them across every point on that line. Sampling
    /// a region's coverage means asking about tens of thousands of points at
    /// once, where the difference is the difference between usable and not.
    fn contains_points(&self, points: &[Point]) -> Vec<f64> {
        let mut result = vec![0.0; points.len()];
        if self.is_empty() {
            return result;
        }
        // Only a ring whose bounds reach the query line crosses it, and a
        // ring wholly to one side of every point on the line contributes
        // crossings that balance to nothing, so those rings are skipped.
        let crossings_at = |y: f64, min_x: f64, max_x: f64| {
            self.rings
                .iter()
                .zip(&self.ring_bounds)
                .filter(move |(_, bounds)| {
                    bounds.min.y <= y
                        && y <= bounds.max.y
                        && bounds.min.x <= max_x + tol::EPSILON_MM
                        && min_x - tol::EPSILON_MM <= bounds.max.x
                })
                .flat_map(|(ring, _)| {
                    ring.iter()
                        .copied()
                        .zip(ring.iter().copied().cycle().skip(1))
                        .take(ring.len())
                })
                .filter_map(move |edge| horizontal_crossing(edge, y))
        };
        if let [point] = points {
            let winding = crossings_at(point.y, point.x, point.x)
                .filter_map(|(x, direction)| (x <= point.x).then_some(direction))
                .sum::<i32>();
            result[0] = f64::from(winding != 0);
            return result;
        }
        let mut by_height = (0..points.len()).collect::<Vec<_>>();
        by_height.sort_by(|left, right| {
            points[*left]
                .y
                .total_cmp(&points[*right].y)
                .then_with(|| points[*left].x.total_cmp(&points[*right].x))
        });

        let mut first = 0;
        while first < by_height.len() {
            let y = points[by_height[first]].y;
            let mut last = first + 1;
            while last < by_height.len() && (points[by_height[last]].y - y).abs() <= tol::EPSILON_MM
            {
                last += 1;
            }
            let (min_x, max_x) = by_height[first..last]
                .iter()
                .map(|&point| points[point].x)
                .fold((f64::INFINITY, f64::NEG_INFINITY), |(low, high), x| {
                    (low.min(x), high.max(x))
                });
            let mut crossings = crossings_at(y, min_x, max_x).collect::<Vec<_>>();
            crossings.sort_by(|left, right| left.0.total_cmp(&right.0));
            let mut crossing = 0;
            let mut winding = 0;
            for &point in &by_height[first..last] {
                while crossing < crossings.len() && crossings[crossing].0 <= points[point].x {
                    winding += crossings[crossing].1;
                    crossing += 1;
                }
                result[point] = f64::from(winding != 0);
            }
            first = last;
        }
        result
    }

    /// Decimate the region's boundary so it only shrinks; see
    /// [`decimate_rings_inward`].
    pub fn decimate_inward(&self, deviation_mm: f64) -> Self {
        Self::new(
            decimate_rings_inward(&self.rings, deviation_mm),
            FillRule::NonZero,
            self.tolerance,
        )
    }

    /// Test many points against the same region in one sweep.
    ///
    /// This is substantially cheaper than repeated [`Self::contains_point`]
    /// calls for geometry checks over thousands of drill locations. Unlike
    /// `contains_point` it tests the strict interior by winding number:
    /// points on or within tolerance of the boundary may land on either side.
    pub fn contains_points_batch(&self, points: &[Point]) -> Vec<bool> {
        self.contains_points(points)
            .into_iter()
            .map(|coverage| coverage > 0.0)
            .collect()
    }

    /// What fraction of each cell of a regular grid over `bounds` the region
    /// covers, row-major from the bottom-left cell.
    ///
    /// Measured as an area rather than sampled. A periodic fill — a hatch, a
    /// thieving lattice — beats against any sampling pitch and comes back as a
    /// moire pattern that is an artefact of the sampling and not of the copper.
    ///
    /// Area is additive over the rings of a regularized region, holes included
    /// with their sign, so each ring is clipped to the cells its bounds reach
    /// and its signed area accumulated there. Clipping runs a row at a time so a
    /// ring meets only the columns of the band it actually crosses.
    pub fn grid_coverage(&self, bounds: BBox, columns: usize, rows: usize) -> Vec<f64> {
        assert!(columns > 0 && rows > 0, "a grid needs at least one cell");
        let width = bounds.width() / columns as f64;
        let height = bounds.height() / rows as f64;
        let index = |value: f64, origin: f64, span: f64, count: usize| {
            ((value - origin) / span)
                .floor()
                .clamp(0.0, count as f64 - 1.0) as usize
        };
        let mut areas = vec![0.0; columns * rows];
        for (ring, &ring_bbox) in self.rings.iter().zip(&self.ring_bounds) {
            for row in index(ring_bbox.min.y, bounds.min.y, height, rows)
                ..=index(ring_bbox.max.y, bounds.min.y, height, rows)
            {
                let floor = bounds.min.y + row as f64 * height;
                let band =
                    clip_half_plane(&clip_half_plane(ring, |point| point[1] - floor), |point| {
                        floor + height - point[1]
                    });
                let band_bbox = rings_bbox(std::slice::from_ref(&band));
                for column in index(band_bbox.min.x, bounds.min.x, width, columns)
                    ..=index(band_bbox.max.x, bounds.min.x, width, columns)
                {
                    let left = bounds.min.x + column as f64 * width;
                    let cell = clip_half_plane(
                        &clip_half_plane(&band, |point| point[0] - left),
                        |point| left + width - point[0],
                    );
                    areas[row * columns + column] += ring_signed_area(&cell);
                }
            }
        }
        areas.iter().map(|area| area / (width * height)).collect()
    }

    /// What fraction of each cell the region covers.
    ///
    /// Cells are centred on `centers` and share one `(width, height)`. Coverage
    /// is estimated by stratified subsampling rather than by intersecting
    /// geometry, so a trace narrower than a cell contributes its true share
    /// instead of aliasing to nothing or to everything.
    pub fn cell_coverage(&self, centers: &[Point], cell: (f64, f64)) -> Vec<f64> {
        const STRATA: usize = 3;
        let offset = |index: usize, span: f64| ((index as f64 + 0.5) / STRATA as f64 - 0.5) * span;
        let subsamples = centers
            .iter()
            .flat_map(|center| {
                (0..STRATA).flat_map(move |row| {
                    (0..STRATA).map(move |column| {
                        Point::new(
                            center.x + offset(column, cell.0),
                            center.y + offset(row, cell.1),
                        )
                    })
                })
            })
            .collect::<Vec<_>>();
        let coverage = self.contains_points(&subsamples);
        let (tiles, _) = coverage.as_chunks::<{ STRATA * STRATA }>();
        tiles
            .iter()
            .map(|tile| tile.iter().sum::<f64>() / (STRATA * STRATA) as f64)
            .collect()
    }

    /// Portions of `start..end` covered by the region, in query direction.
    ///
    /// The region boundary is covered. Point-only contacts are omitted.
    pub fn segment_spans(&self, start: Point, end: Point) -> Vec<(Point, Point)> {
        let direction = end - start;
        let length = direction.length();
        if self.is_empty() || !start.is_finite() || !end.is_finite() || length == 0.0 {
            return Vec::new();
        }

        let epsilon = self.tolerance.max(tol::EPSILON_MM);
        let parameter_epsilon = (epsilon / length).min(1.0);
        let cross = |left: Point, right: Point| left.x * right.y - left.y * right.x;
        let mut breaks = vec![0.0, 1.0];
        for (edge_start, edge_end) in self.rings.iter().flat_map(ring_edges) {
            let edge = edge_end - edge_start;
            let offset = edge_start - start;
            let denominator = cross(direction, edge);
            let parallel_epsilon = f64::EPSILON * length * edge.length() * 8.0;
            if denominator.abs() <= parallel_epsilon {
                // A coincident edge contributes both ends of its overlap. The
                // midpoint classification below then includes that boundary.
                if cross(direction, offset).abs() <= epsilon * length {
                    let length_squared = length * length;
                    for point in [edge_start, edge_end] {
                        let t = ((point - start).x * direction.x + (point - start).y * direction.y)
                            / length_squared;
                        if t >= -parameter_epsilon && t <= 1.0 + parameter_epsilon {
                            breaks.push(t.clamp(0.0, 1.0));
                        }
                    }
                }
                continue;
            }

            let t = cross(offset, edge) / denominator;
            let u = cross(offset, direction) / denominator;
            if t >= -parameter_epsilon
                && t <= 1.0 + parameter_epsilon
                && u >= -parameter_epsilon
                && u <= 1.0 + parameter_epsilon
            {
                breaks.push(t.clamp(0.0, 1.0));
            }
        }

        breaks.sort_by(f64::total_cmp);
        breaks.dedup_by(|left, right| (*left - *right).abs() <= parameter_epsilon);
        let point_at = |t: f64| start + direction * t;
        let mut spans: Vec<(Point, Point)> = Vec::new();
        for interval in breaks.windows(2) {
            let (from, to) = (interval[0], interval[1]);
            if to - from <= parameter_epsilon || !self.contains_point(point_at((from + to) / 2.0)) {
                continue;
            }
            let next = (point_at(from), point_at(to));
            if let Some(previous) = spans.last_mut()
                && previous.1.distance_to(next.0) <= epsilon
            {
                previous.1 = next.1;
            } else {
                spans.push(next);
            }
        }
        spans
    }

    /// Whether the regularized region contains the point, including its boundary.
    pub fn contains_point(&self, point: Point) -> bool {
        if self.is_empty()
            || point.x < self.bbox.min.x
            || point.x > self.bbox.max.x
            || point.y < self.bbox.min.y
            || point.y > self.bbox.max.y
        {
            return false;
        }

        let epsilon = self.tolerance.max(tol::EPSILON_MM);
        let rings_near = |reach: f64| {
            self.rings
                .iter()
                .zip(&self.ring_bounds)
                .filter(move |(_, bounds)| bounds.expand(reach).contains_point(point))
                .map(|(ring, _)| ring)
        };
        if rings_near(epsilon).any(|ring| ring_boundary_distance(ring, point) <= epsilon) {
            return true;
        }

        // Regularized boolean output nests pairwise-disjoint rings, so
        // even-odd parity across rings coincides with the nonzero fill rule.
        // A ring whose bounds miss the point cannot enclose it.
        rings_near(tol::EPSILON_MM).fold(false, |inside, ring| {
            inside ^ ring_contains_point(ring, point)
        })
    }

    /// Whether a closed disk is fully contained in the regularized region.
    ///
    /// The boundary check is exact for the flattened representation used by
    /// `ContourSet` boolean operations.
    pub fn contains_disk(&self, center: Point, radius: f64) -> bool {
        if !radius.is_finite() || radius < 0.0 || !center.is_finite() {
            return false;
        }
        if !self.contains_point(center) {
            return false;
        }
        if radius == 0.0 {
            return true;
        }
        if center.x - radius < self.bbox.min.x
            || center.x + radius > self.bbox.max.x
            || center.y - radius < self.bbox.min.y
            || center.y + radius > self.bbox.max.y
        {
            return false;
        }

        let epsilon = self.tolerance.max(tol::EPSILON_MM);
        self.rings
            .iter()
            .all(|ring| ring_boundary_distance(ring, center) + epsilon >= radius)
    }

    /// Minkowski sum with a disk: `self ⊕ D_radius`. This is the standard
    /// "buffer out" operation used for manufacturability checks. The
    /// topology-aware offset expands outer contours, contracts holes, and
    /// regularizes all resulting contours together.
    pub fn disk_dilate(&self, radius: f64) -> Self {
        if self.is_empty() || radius <= 0.0 {
            return self.clone();
        }

        self.disk_offset(radius)
    }

    /// Minkowski erosion by a disk: `self ⊖ D_radius`.
    ///
    /// This removes the radius-wide interior band swept by every boundary
    /// ring. It therefore contracts outer boundaries, expands holes, removes
    /// necks narrower than twice the radius, and may split or erase connected
    /// components.
    pub fn disk_erode(&self, radius: f64) -> Self {
        if self.is_empty() || radius <= 0.0 {
            return self.clone();
        }

        self.disk_offset(-radius)
    }

    /// Morphological opening by a disk: `(self ⊖ D_radius) ⊕ D_radius`.
    ///
    /// Equivalently, this is the union of every radius-sized disk contained in
    /// the source region. It is therefore a subset of the source that removes
    /// tips and islands too small to accommodate the disk while rounding the
    /// surviving outward corners.
    pub fn disk_open(&self, radius: f64) -> Self {
        if self.is_empty() || radius <= 0.0 {
            return self.clone();
        }

        // Round-offset tessellation approximates the mathematical disks. Clip
        // the regrown result to the source so the operation retains its
        // defining anti-extensive property at polygon tolerance.
        self.disk_erode(radius)
            .disk_dilate(radius)
            .intersection(self)
    }

    /// Morphological closing by a disk: `(self ⊕ D_radius) ⊖ D_radius`.
    ///
    /// Equivalently, the complement of the result is the union of every
    /// radius-sized disk contained in the source complement. Closing therefore
    /// fills void tips and gaps too small to accommodate the disk while
    /// rounding the surviving inward corners.
    pub fn disk_close(&self, radius: f64) -> Self {
        if self.is_empty() || radius <= 0.0 {
            return self.clone();
        }

        // Round-offset tessellation approximates the mathematical disks. Union
        // the contracted result with the source so the operation retains its
        // defining extensive property at polygon tolerance.
        self.disk_dilate(radius).disk_erode(radius).union(self)
    }

    /// Diagnose distinct components whose Euclidean separation is less than
    /// the disk diameter `2 * radius`.
    ///
    /// For connected components `C_i`, returns
    /// `⋃_{i<j} (((C_i ⊕ D_r) ∩ (C_j ⊕ D_r)) \ self)` for pairs with
    /// `distance(C_i, C_j) < 2r`. Subtracting `self` localizes the diagnostic
    /// to the intervening void. Bounding-box pruning and segment distance avoid
    /// constructing dilations for non-conflicting pairs.
    ///
    /// Verification-only diagnostic: production gap analysis goes through
    /// [`ContourSet::disk_gap_violations`], which also covers gaps within one
    /// connected component.
    #[cfg(test)]
    pub(crate) fn disk_inter_component_gap_violations(&self, radius: f64) -> Self {
        if self.is_empty() || radius <= 0.0 {
            return Self::empty(self.tolerance);
        }

        let components = self.connected_components();
        let mut violations = Self::empty(self.tolerance);
        for (index, left) in components.iter().enumerate() {
            for right in &components[index + 1..] {
                if !regions_within_distance(left, right, 2.0 * radius) {
                    continue;
                }
                let overlap = left
                    .disk_dilate(radius)
                    .intersection(&right.disk_dilate(radius))
                    .difference(self);
                violations = violations.union(&overlap);
            }
        }
        violations
    }

    /// Enforce a diameter-`2 * gap_radius` minimum for every two-sided void gap.
    ///
    /// The result is the fixed point, reached from `self`, of one pure
    /// contraction `T` that removes a guard-widened tube around the boundary
    /// medial axis inside the narrow void phase and reopens with the
    /// filled-region disk:
    ///
    /// ```text
    /// T(S) = open(S \ (Γ(G_gap_radius(S)) ⊕ disk(gap_radius + guard)), disk(filled_radius)) ∩ S
    /// ```
    ///
    /// `G_r(X)` is the two-sided part of `close(X, disk(r)) \ X`, and `Γ(N)`
    /// is the boundary medial axis inside that phase — the least local cut,
    /// trimming both sides of every narrow gap without widening one-sided edge
    /// clearance. The guard keeps every checked quantity strictly separated
    /// from every constructed one: a cut leaves a `2 (gap_radius + guard)`
    /// void, so construction noise cannot push a trimmed gap back under the
    /// nominal test. `T` only removes material, so iterating from the source
    /// converges; a step that removes almost nothing is reported as an error
    /// instead of a silent stall.
    pub fn disk_regularize_gaps(
        &self,
        gap_radius: f64,
        filled_radius: f64,
        guard: f64,
    ) -> Result<DiskGapRegularization, GapRegularizationError> {
        if !gap_radius.is_finite()
            || gap_radius <= 0.0
            || !filled_radius.is_finite()
            || filled_radius <= 0.0
        {
            return Err(GapRegularizationError(
                "gap and filled-region radii must be finite and positive".to_string(),
            ));
        }
        if !guard.is_finite() || guard < 0.0 {
            return Err(GapRegularizationError(
                "gap-regularization guard must be finite and non-negative".to_string(),
            ));
        }

        let mut kept = self.clone();
        while let Some(next) = kept.narrow_gap_trim(gap_radius, filled_radius, guard)? {
            if kept.difference(&next).area() <= self.tolerance * self.tolerance {
                return Err(GapRegularizationError(format!(
                    "gap regularization stalled with {:.9} mm² of void-gap violations",
                    kept.disk_gap_violations(gap_radius).area()
                )));
            }
            kept = next;
        }
        Ok(DiskGapRegularization {
            removed: self.difference(&kept),
            kept,
        })
    }

    /// One application of the gap-regularization contraction `T`, or `None`
    /// at its fixed point, where every two-sided void gap already admits the
    /// rolling disk.
    fn narrow_gap_trim(
        &self,
        gap_radius: f64,
        filled_radius: f64,
        guard: f64,
    ) -> Result<Option<Self>, GapRegularizationError> {
        let narrow_voids = self.disk_gap_violations(gap_radius);
        if narrow_voids.is_empty() {
            return Ok(None);
        }
        let keep_out = narrow_void_keep_out(self, &narrow_voids, gap_radius + guard)?;
        Ok(Some(
            self.difference(&keep_out)
                .disk_open(filled_radius)
                .intersection(self),
        ))
    }

    /// Unfilled material that violates the two-sided void-gap radius.
    ///
    /// The raw closing residual `close(self, disk(radius)) \ self` also contains
    /// the rounded bite at an isolated concave corner. A residual component is
    /// a gap when it contacts nonincident, separated source-boundary segments
    /// on distinct rings, or on one ring with opposing tangents — the latter
    /// distinguishes hairpins and notches from the bite of a single smooth
    /// concavity. An empty result proves no two facing boundary branches fail
    /// the rolling-disk test.
    pub fn disk_gap_violations(&self, radius: f64) -> Self {
        if self.is_empty() || !(radius > 0.0 && radius.is_finite()) {
            return Self::empty(self.tolerance);
        }
        two_sided_gap_residual(self, &closing_residual(self, radius))
    }

    /// Connected material residues of the opening by `radius` whose local
    /// width can be under `width_mm`, with the local width of each.
    pub(crate) fn disk_feature_violation_components(
        &self,
        radius: f64,
        width_mm: f64,
    ) -> Vec<TwoSidedResidualComponent> {
        // Opening is the union of the disks inside a region, and a disk is
        // connected, so every component opens on its own, and the disks
        // through a residue point reach a diameter from it. A width is a
        // disk touching two facing walls, so it is at least their
        // separation: material whose walls never face each other that
        // closely has no width to find, and only the material within a
        // diameter of facing walls decides the residue within a radius of
        // them, where any width lies. Separate components share a width
        // only where they touch within tolerance.
        // The snap-rounded width construction moves walls by a tolerance,
        // and `M \ (X ∩ M)` is `M \ X`, so the opening's clip to the
        // source is not needed to find what the opening removed.
        self.two_sided_residual(radius, |region, radius| {
            let facing = width_mm + 4.0 * region.tolerance;
            let touching = 3.0 * region.tolerance + tol::FLATTEN_MM;
            let reach = 3.0 * (radius + 2.0 * region.tolerance);
            let candidates = region.facing_components(facing, touching, reach);
            candidates.difference(&candidates.disk_erode(radius).disk_dilate(radius))
        })
    }

    /// Connected void residues of the closing by `radius` whose local width
    /// can be under `width_mm`, with the local width of each.
    pub(crate) fn disk_gap_violation_components(
        &self,
        radius: f64,
        width_mm: f64,
    ) -> Vec<TwoSidedResidualComponent> {
        // Closing fills only void narrower than the disk diameter, between
        // walls facing across it, and a width is at least the separation of
        // the walls it touches. A residue point lies within a radius of its
        // walls, and the disks that decide it reach a diameter further, so
        // everything within two diameters comes along as context and the
        // closing of that neighbourhood is the closing of the whole region
        // there.
        self.two_sided_residual(radius, |region, radius| {
            let facing = width_mm + 4.0 * region.tolerance;
            let reach = 4.0 * (radius + 2.0 * region.tolerance);
            let candidates = region.facing_components(0.0, facing, reach);
            closing_residual(&candidates, radius)
        })
    }

    /// Morphological residue of this region, kept only where two distinct
    /// source-boundary branches wall it. A degenerate disk has no residue.
    fn two_sided_residual(
        &self,
        radius: f64,
        residual: impl FnOnce(&Self, f64) -> Self,
    ) -> Vec<TwoSidedResidualComponent> {
        if self.is_empty() || !(radius > 0.0 && radius.is_finite()) {
            return Vec::new();
        }
        two_sided_residual_components(self, &residual(self, radius), radius)
    }

    pub fn to_contours(&self) -> Vec<ContourBuf> {
        rings_to_contours(self.rings.clone())
    }

    /// Convert each connected component to one positive contour.
    ///
    /// Hole rings are connected to their outer ring with zero-width bridges,
    /// allowing formats without compound-polygon holes to carry the same
    /// local positive geometry without layer-wide clear features.
    pub fn to_bridged_contours(&self) -> Vec<ContourBuf> {
        simplify_shapes(self.rings.clone(), FillRule::NonZero)
            .into_iter()
            .map(crate::geom::bridge::bridge_shape)
            .filter(|ring| ring.len() >= 3)
            .filter_map(ring_to_contour)
            .collect()
    }

    fn disk_offset(&self, offset: f64) -> Self {
        let radius = offset.abs();
        let max_sagitta = tol::STROKE_OUTLINE_MM.min(radius);
        // i_overlay rounds each join into floor(sweep / join_angle)
        // segments. Use half the central angle allowed by the sagitta budget
        // so that flooring cannot make the emitted segments too coarse.
        let join_angle = (1.0 - max_sagitta / radius)
            .acos()
            .clamp(0.01 * std::f64::consts::PI, 0.25 * std::f64::consts::PI);
        let style = OutlineStyle::new(offset).line_join(OutlineLineJoin::Round(join_angle));
        let shapes = self.rings.outline_as::<i64>(&style);
        Self::new(flatten_shapes(shapes), FillRule::NonZero, self.tolerance)
    }
}

/// Compose an ordered dark/clear paint stream into a final positive image.
///
/// Consecutive same-polarity pushes are batched into one boolean operation.
#[derive(Debug, Default)]
pub struct PaintComposer {
    image: Vec<Ring>,
    run: Vec<Ring>,
    run_polarity: Option<Polarity>,
}

impl PaintComposer {
    pub fn push(&mut self, polarity: Polarity, mut rings: Vec<Ring>) {
        if rings.is_empty() {
            return;
        }
        if self.run_polarity != Some(polarity) {
            self.flush_run();
            self.run_polarity = Some(polarity);
        }
        self.run.append(&mut rings);
    }

    pub fn finish(mut self) -> Vec<Ring> {
        self.flush_run();
        self.image
    }

    pub fn finish_set(self, tolerance: f64) -> ContourSet {
        ContourSet::new(self.finish(), FillRule::NonZero, tolerance)
    }

    fn flush_run(&mut self) {
        let Some(polarity) = self.run_polarity.take() else {
            return;
        };
        if self.run.is_empty() {
            return;
        }

        match polarity {
            Polarity::Dark => {
                let mut rings = std::mem::take(&mut self.image);
                rings.append(&mut self.run);
                self.image = union_rings(rings, FillRule::NonZero);
            }
            Polarity::Clear => {
                if self.image.is_empty() {
                    self.run.clear();
                } else {
                    let cutters = union_rings(std::mem::take(&mut self.run), FillRule::NonZero);
                    self.image = difference_rings(std::mem::take(&mut self.image), cutters);
                }
            }
        }
    }
}

pub(crate) fn overlay_fill_rule(fill_rule: FillRule) -> OverlayFillRule {
    match fill_rule {
        FillRule::EvenOdd => OverlayFillRule::EvenOdd,
        FillRule::NonZero => OverlayFillRule::NonZero,
    }
}

fn horizontal_crossing(([x0, y0], [x1, y1]): ([f64; 2], [f64; 2]), y: f64) -> Option<(f64, i32)> {
    // Half-open in y so a vertex shared by two edges is counted once, which
    // keeps the winding number honest.
    let direction = if y0 <= y && y < y1 {
        1
    } else if y1 <= y && y < y0 {
        -1
    } else {
        return None;
    };
    Some((x0 + (y - y0) * (x1 - x0) / (y1 - y0), direction))
}

fn flatten_shapes(shapes: Vec<Shape>) -> Vec<Ring> {
    shapes.into_iter().flatten().collect()
}

fn filter_significant_rings(mut rings: Vec<Ring>, tolerance: f64) -> Vec<Ring> {
    if tolerance > 0.0 {
        let min_area = tolerance.powi(2);
        rings.retain(|ring| ring_signed_area(ring).abs() > min_area);
    }
    rings
}

fn push_ring(out: &mut Vec<Ring>, ring: &mut Ring) {
    if ring.first() == ring.last() {
        ring.pop();
    }
    if ring.len() >= 3 {
        out.push(std::mem::take(ring));
    } else {
        ring.clear();
    }
}

fn ring_to_contour(ring: Ring) -> Option<ContourBuf> {
    if ring.len() < 3 {
        return None;
    }
    let mut bbox = BBox::empty();
    let mut cmds = Vec::with_capacity(ring.len() + 1);
    for (index, [x, y]) in ring.into_iter().enumerate() {
        let point = Point::new(x, y);
        bbox.include_point(point);
        if index == 0 {
            cmds.push(PathCmd::move_to(point));
        } else {
            cmds.push(PathCmd::line_to(point));
        }
    }
    cmds.push(PathCmd::close());
    Some(ContourBuf::from_parts(bbox, cmds))
}

fn ring_contains_point(ring: &Ring, point: Point) -> bool {
    if ring.len() < 3 {
        return false;
    }

    let mut inside = false;
    for index in 0..ring.len() {
        let [x0, y0] = ring[index];
        let [x1, y1] = ring[(index + 1) % ring.len()];
        if (y0 > point.y) != (y1 > point.y) {
            let crossing_x = x0 + (point.y - y0) * (x1 - x0) / (y1 - y0);
            if point.x < crossing_x {
                inside = !inside;
            }
        }
    }
    inside
}

fn ring_boundary_distance(ring: &Ring, point: Point) -> f64 {
    if ring.is_empty() {
        return f64::INFINITY;
    }

    (0..ring.len())
        .map(|index| {
            let [x0, y0] = ring[index];
            let [x1, y1] = ring[(index + 1) % ring.len()];
            dist::point_segment(point, Point::new(x0, y0), Point::new(x1, y1)).0
        })
        .fold(f64::INFINITY, f64::min)
}

const VORONOI_COORDINATES_PER_MM: f64 = 100_000.0;

#[derive(Debug, Clone, Copy)]
struct BoundarySegment {
    ring: usize,
    index: usize,
    ring_len: usize,
}

#[derive(Debug, Clone, Copy)]
struct OrientedBoundarySegment {
    topology: BoundarySegment,
    start: Point,
    end: Point,
    /// Source-ring direction averaged across one flattening-tolerance on
    /// either side. Sub-resolution backtracking must not turn one wall into
    /// two opposing walls.
    tangent: Point,
    bbox: BBox,
}

fn closing_residual(region: &ContourSet, radius: f64) -> ContourSet {
    region.disk_close(radius).difference(region)
}

/// Every source boundary edge longer than the region tolerance, in ring
/// order. The kept edges are numbered consecutively, so two that meet
/// across a dropped sub-tolerance edge remain adjacent.
fn source_boundary_segments(source: &ContourSet) -> Vec<OrientedBoundarySegment> {
    source
        .rings
        .iter()
        .enumerate()
        .flat_map(|(ring_id, ring)| {
            let metric = RingArcLength::new(ring);
            // Keep the two-sided chord local even when the whole ring is
            // smaller than the ordinary geometry resolution.
            let tangent_radius = tol::FLATTEN_MM.min(metric.perimeter() / 8.0);
            let kept = ring_edges(ring)
                .enumerate()
                .filter(|(_, (start, end))| start.distance_to(*end) > source.tolerance)
                .collect::<Vec<_>>();
            let ring_len = kept.len();
            kept.into_iter()
                .enumerate()
                .map(
                    move |(index, (edge, (start, end)))| OrientedBoundarySegment {
                        topology: BoundarySegment {
                            ring: ring_id,
                            index,
                            ring_len,
                        },
                        start,
                        end,
                        tangent: metric.edge_tangent(edge, tangent_radius),
                        bbox: segment_bbox(start, end),
                    },
                )
        })
        .collect()
}

/// Canonical arc-length parameterization of a closed polygonal ring.
/// Consecutive entries are the stations at the ends of each source edge.
struct RingArcLength<'a> {
    ring: &'a Ring,
    stations: Vec<f64>,
}

impl<'a> RingArcLength<'a> {
    fn new(ring: &'a Ring) -> Self {
        let stations = std::iter::once(0.0)
            .chain(ring_edges(ring).scan(0.0, |station, (start, end)| {
                *station += start.distance_to(end);
                Some(*station)
            }))
            .collect();
        Self { ring, stations }
    }

    fn perimeter(&self) -> f64 {
        self.stations[self.ring.len()]
    }

    /// Point at a periodic arc-length station. Selecting by edge-end station
    /// skips zero-length edges as a consequence of the parameterization.
    fn point_at(&self, station: f64) -> Point {
        let station = station.rem_euclid(self.perimeter());
        let index = self.stations[1..].partition_point(|&end| end <= station);
        let start_station = self.stations[index];
        let end_station = self.stations[index + 1];
        let [start_x, start_y] = self.ring[index];
        let [end_x, end_y] = self.ring[(index + 1) % self.ring.len()];
        let start = Point::new(start_x, start_y);
        let end = Point::new(end_x, end_y);
        start + (end - start) * ((station - start_station) / (end_station - start_station))
    }

    /// Direction at one edge, measured as the chord between equal arc-length
    /// offsets around its midpoint. Tiny reversals therefore retain the
    /// direction of their resolution-scale wall instead of becoming an
    /// opposing wall.
    fn edge_tangent(&self, index: usize, radius: f64) -> Point {
        let midpoint = (self.stations[index] + self.stations[index + 1]) / 2.0;
        self.point_at(midpoint + radius) - self.point_at(midpoint - radius)
    }
}

/// Each connected component of `residual` that two distinct source-boundary
/// branches wall, with its local width.
///
/// A point of the residue is nearer than `reach` to the source boundary —
/// no disk of that radius covers it — so the boundary segments within
/// `reach` of the component are every segment its inscribed disks can
/// touch, and their Voronoi diagram restricted to the component is the
/// medial axis there. The narrowest maximal inscribed disk on that axis is
/// the component's width. Disks tangent only to incident segments are
/// corner spokes, not widths: discarding those leaves one-sided residue —
/// the bite an isolated corner sheds — with no width at all.
fn two_sided_residual_components(
    source: &ContourSet,
    residual: &ContourSet,
    reach: f64,
) -> Vec<TwoSidedResidualComponent> {
    if residual.is_empty() {
        return Vec::new();
    }
    let segments = SegmentGrid::new(source_boundary_segments(source), reach);
    let contact_tolerance = source.tolerance.max(residual.tolerance);
    residual
        .connected_components()
        .into_iter()
        .filter_map(|component| {
            let sites = segments.near(component.bbox.expand(reach));
            component_width(&sites, &component, reach, contact_tolerance).map(|geometry| {
                TwoSidedResidualComponent {
                    region: component,
                    width: geometry.disk.width(),
                    disk: geometry.disk,
                    axis: geometry.axis,
                }
            })
        })
        .collect()
}

/// Boundary segments bucketed on a uniform grid, so the segments around one
/// residue component are found without scanning the whole boundary.
struct SegmentGrid {
    segments: Vec<OrientedBoundarySegment>,
    grid: CellGrid,
}

/// Grid pitch changes candidate lookup cost only. A fixed floor prevents a
/// small DFM limit from dividing a board-length edge into thousands of cells.
const MIN_SEGMENT_GRID_CELL_MM: f64 = 1.0;

impl SegmentGrid {
    fn new(segments: Vec<OrientedBoundarySegment>, cell_mm: f64) -> Self {
        let bounds = segments
            .iter()
            .map(|segment| segment.bbox)
            .fold(BBox::empty(), BBox::union);
        let pitch = cell_mm.max(MIN_SEGMENT_GRID_CELL_MM);
        let grid = CellGrid::new(
            pitch,
            bounds,
            segments.iter().enumerate().flat_map(|(id, segment)| {
                CellGrid::cells_of(segment.bbox, pitch).map(move |cell| (id as u32, cell))
            }),
        );
        Self { segments, grid }
    }

    /// Ids of the segments whose bounds meet `query`; a segment in several
    /// cells repeats.
    fn near_ids(&self, query: BBox) -> impl Iterator<Item = u32> + '_ {
        self.grid
            .rectangle(query)
            .filter(move |&id| self.segments[id as usize].bbox.intersects(query))
    }

    /// The segments whose bounds meet `query`, in index order, each once.
    fn near(&self, query: BBox) -> Vec<OrientedBoundarySegment> {
        let mut indices = self.near_ids(query).collect::<Vec<_>>();
        indices.sort_unstable();
        indices.dedup();
        indices
            .into_iter()
            .map(|index| self.segments[index as usize])
            .collect()
    }
}

/// A boundary segment snapped to the tolerance grid, with its topology.
type GridSite = (VoronoiLine<i32>, OrientedBoundarySegment);

/// The sites snap-rounded to a planar set on the tolerance grid. The
/// Voronoi builder accepts segments that meet only at endpoints, while
/// regularized rings may touch along a seam (two rings sharing an edge),
/// at a vertex on another ring's edge, or fold into hairpins narrower than
/// the tolerance. On the grid, points within tolerance are one point:
/// zero-length and duplicate segments drop, a segment splits where another
/// endpoint lies on it, and of two segments that still cross — noise at
/// tolerance scale — the shorter drops. Pieces keep their parent's topology
/// and so remain incident to each other and to its neighbors.
fn planar_grid_sites(
    sites: &[OrientedBoundarySegment],
    quantize: impl Fn(Point) -> VoronoiPoint<i32>,
) -> Vec<GridSite> {
    let orient = |a: VoronoiPoint<i32>, b: VoronoiPoint<i32>, c: VoronoiPoint<i32>| -> i128 {
        (i128::from(b.x) - i128::from(a.x)) * (i128::from(c.y) - i128::from(a.y))
            - (i128::from(b.y) - i128::from(a.y)) * (i128::from(c.x) - i128::from(a.x))
    };
    let on_interior = |line: &VoronoiLine<i32>, point: VoronoiPoint<i32>| {
        point != line.start
            && point != line.end
            && orient(line.start, line.end, point) == 0
            && (line.start.x.min(line.end.x)..=line.start.x.max(line.end.x)).contains(&point.x)
            && (line.start.y.min(line.end.y)..=line.start.y.max(line.end.y)).contains(&point.y)
    };
    let length2 = |line: &VoronoiLine<i32>| {
        let dx = i128::from(line.end.x) - i128::from(line.start.x);
        let dy = i128::from(line.end.y) - i128::from(line.start.y);
        dx * dx + dy * dy
    };
    let crosses = |a: &VoronoiLine<i32>, b: &VoronoiLine<i32>| {
        orient(a.start, a.end, b.start).signum() * orient(a.start, a.end, b.end).signum() < 0
            && orient(b.start, b.end, a.start).signum() * orient(b.start, b.end, a.end).signum() < 0
    };
    // Sites keep their ring's traversal direction; a segment and its
    // reverse are one site.
    let key = |line: &VoronoiLine<i32>| {
        let (a, b) = ((line.start.x, line.start.y), (line.end.x, line.end.y));
        (a.min(b), a.max(b))
    };
    let snapped = sites
        .iter()
        .map(|site| {
            (
                VoronoiLine::new(quantize(site.start), quantize(site.end)),
                *site,
            )
        })
        .filter(|(line, _)| line.start != line.end)
        .collect::<Vec<_>>();
    let endpoints = snapped
        .iter()
        .flat_map(|(line, _)| [line.start, line.end])
        .collect::<Vec<_>>();
    let mut seen = std::collections::HashSet::new();
    let split = snapped
        .iter()
        .flat_map(|&(line, source)| {
            let mut stations = endpoints
                .iter()
                .copied()
                .filter(|&point| on_interior(&line, point))
                .collect::<Vec<_>>();
            // Collinear with the segment, so distance from its start orders
            // them along it whichever way it runs.
            let along = |point: &VoronoiPoint<i32>| {
                (i64::from(point.x) - i64::from(line.start.x)).abs()
                    + (i64::from(point.y) - i64::from(line.start.y)).abs()
            };
            stations.sort_by_key(along);
            stations.dedup();
            std::iter::once(line.start)
                .chain(stations)
                .chain(std::iter::once(line.end))
                .collect::<Vec<_>>()
                .windows(2)
                .map(|pair| (VoronoiLine::new(pair[0], pair[1]), source))
                .collect::<Vec<_>>()
        })
        .filter(|(line, _)| seen.insert(key(line)))
        .collect::<Vec<_>>();
    split
        .iter()
        .enumerate()
        .filter(|(index, (line, _))| {
            !split.iter().enumerate().any(|(other_index, (other, _))| {
                other_index != *index
                    && crosses(line, other)
                    && (length2(other), other_index) > (length2(line), *index)
            })
        })
        .map(|(_, site)| *site)
        .collect()
}

/// Whether every two sites are incident, and so one wall: segments of one
/// ring that are adjacent or turned no more than a quarter turn from each
/// other, the same judgement the width construction makes of a wall pair.
/// Nothing in such a set faces anything else, so a residue it walls alone
/// is the bite of one corner or gentle arc and has no width. A sharp tip
/// turns its walls to face each other and is measured.
fn one_wall(sites: &[OrientedBoundarySegment]) -> bool {
    sites.iter().enumerate().all(|(position, left)| {
        sites[position + 1..].iter().all(|right| {
            left.topology.ring == right.topology.ring
                && (boundary_segments_are_incident(left.topology, right.topology)
                    || left.tangent.x * right.tangent.x + left.tangent.y * right.tangent.y >= 0.0)
        })
    })
}

/// A maximal inscribed disk of the boundary's Voronoi diagram: its center,
/// radius, and the two tangency points on distinct walls.
#[derive(Debug, Clone, Copy)]
pub(crate) struct InscribedDisk {
    pub center: Point,
    pub radius: f64,
    pub first: Point,
    pub second: Point,
}

impl InscribedDisk {
    fn width(self) -> Distance {
        Distance::flattened(2.0 * self.radius, self.first, self.second, 2)
    }
}

/// One cell of the boundary medial axis, with the two walls defining it.
/// A vertex has equal start/end points; curved edges are polylines within the
/// shared flattening tolerance. These are candidates until clipped to the
/// residue and the requested width.
#[derive(Debug, Clone, Copy)]
pub(crate) struct WidthAxisSegment {
    pub start: Point,
    pub end: Point,
    pub first_wall: (Point, Point),
    pub second_wall: (Point, Point),
    pub uncertainty_mm: f64,
}

struct ComponentWidth {
    disk: InscribedDisk,
    axis: Vec<WidthAxisSegment>,
}

/// The narrowest maximal inscribed disk of `sites` inside `component`, as a
/// width between its two tangency points.
///
/// Candidates are the Voronoi vertices, and every non-incident edge sampled
/// along its length, at the apex of a parabolic or point–point edge, and
/// where it crosses the component boundary — the clearance along an edge is
/// linear or convex, so its minimum over the inside is at one of those.
/// Flattening a curve sprouts axis branches the source does not have, whose
/// disks sit inside a neighbor's disk up to the flattening tolerance; those
/// are pruned, so a flattened arc measures its diameter and a taper keeps
/// its tip.
fn component_width(
    sites: &[OrientedBoundarySegment],
    component: &ContourSet,
    reach: f64,
    contact_tolerance: f64,
) -> Option<ComponentWidth> {
    if one_wall(sites) {
        return None;
    }
    let origin = component.bbox.min;
    let units_per_mm = 1.0 / contact_tolerance;
    let quantize = |point: Point| {
        VoronoiPoint::new(
            ((point.x - origin.x) * units_per_mm).round() as i32,
            ((point.y - origin.y) * units_per_mm).round() as i32,
        )
    };
    let unquantize =
        |[x, y]: [f64; 2]| Point::new(x / units_per_mm + origin.x, y / units_per_mm + origin.y);
    let grid = planar_grid_sites(sites, quantize);
    let lines = grid.iter().map(|(line, _)| *line).collect::<Vec<_>>();
    let sites = grid
        .iter()
        .map(|(line, source)| {
            let start = unquantize([f64::from(line.start.x), f64::from(line.start.y)]);
            let end = unquantize([f64::from(line.end.x), f64::from(line.end.y)]);
            OrientedBoundarySegment {
                topology: source.topology,
                start,
                end,
                tangent: source.tangent,
                bbox: segment_bbox(start, end),
            }
        })
        .collect::<Vec<_>>();
    if lines.len() < 2 {
        return None;
    }
    // Which sites are one wall. A ring's two sides of a channel are
    // traversed in opposite directions, so two segments of one ring face
    // each other only when their directions oppose; ring-adjacent segments
    // and segments whose resolution-scale tangents are no more than a
    // quarter turn apart are the same wall, as a disk touching both edges
    // of a square corner is a corner disk, not a width. The averaged
    // tangent prevents a microscopic reversal from manufacturing an
    // opposing branch. Segments of different rings are one wall only where
    // they touch.
    let incident = |i: usize, j: usize| {
        let (a, b) = (&sites[i], &sites[j]);
        if a.topology.ring == b.topology.ring {
            boundary_segments_are_incident(a.topology, b.topology)
                || a.tangent.x * b.tangent.x + a.tangent.y * b.tangent.y >= 0.0
        } else {
            [lines[i].start, lines[i].end]
                .iter()
                .any(|point| [lines[j].start, lines[j].end].contains(point))
        }
    };
    let component_edges = component
        .rings
        .iter()
        .flat_map(ring_edges)
        .collect::<Vec<_>>();
    // A maximal disk centered in the component is no larger than the
    // component's clearance to its nearest wall, which the farthest vertex
    // from that wall bounds. Both walls of a width therefore lie within that
    // reach of the component; a corner's own bite is walled by its two edges
    // alone and never reaches the far side of the feature. Any disk within
    // the morphology reach that touches two eligible walls also proves those
    // walls are no farther apart than its diameter. Allow one contact
    // tolerance at each wall for the snap-rounded residual, and the chord
    // deviation of a sampled curved axis, then leave every surviving
    // measurement to the Voronoi construction below.
    let candidate_diameter = 2.0 * (reach + contact_tolerance);
    let farthest_vertex = |site: &OrientedBoundarySegment| {
        component
            .rings
            .iter()
            .flat_map(|ring| ring.iter())
            .map(|&[x, y]| dist::point_segment(Point::new(x, y), site.start, site.end).0)
            .fold(0.0, f64::max)
    };
    let clearance_bound = sites
        .iter()
        .map(farthest_vertex)
        .fold(f64::INFINITY, f64::min)
        + 2.0 * contact_tolerance
        + tol::FLATTEN_MM;
    let within_reach = sites
        .iter()
        .map(|site| {
            component_edges.iter().any(|&(start, end)| {
                dist::segments(start, end, site.start, site.end).0 <= clearance_bound
            })
        })
        .collect::<Vec<_>>();
    let has_reachable_wall_pair = (0..sites.len())
        .flat_map(|first| (first + 1..sites.len()).map(move |second| (first, second)))
        .any(|(first, second)| {
            within_reach[first]
                && within_reach[second]
                && !incident(first, second)
                && dist::segments(
                    sites[first].start,
                    sites[first].end,
                    sites[second].start,
                    sites[second].end,
                )
                .0 <= candidate_diameter
        });
    if !has_reachable_wall_pair {
        return None;
    }
    let diagram = VoronoiBuilder::<i32>::default()
        .with_segments(lines.iter())
        .and_then(VoronoiBuilder::build)
        .expect("snap-rounded boundary segments do not cross");
    // A cell's site index, and its point when the site is a segment end.
    let site_of = |cell: VoronoiCellIndex| {
        let cell = diagram.cell(cell).expect("diagram cell");
        let index = cell.source_index().usize();
        let point = match cell.source_category() {
            SourceCategory::SegmentStart => Some(sites[index].start),
            SourceCategory::SegmentEnd => Some(sites[index].end),
            SourceCategory::Segment | SourceCategory::SinglePoint => None,
        };
        (index, point)
    };
    let disk = |center: Point, first: usize, second: usize| {
        let (first_distance, first) =
            dist::point_segment(center, sites[first].start, sites[first].end);
        let (second_distance, second) =
            dist::point_segment(center, sites[second].start, sites[second].end);
        InscribedDisk {
            center,
            radius: (first_distance + second_distance) / 2.0,
            first,
            second,
        }
    };

    // Vertices: tangent to every site around them; a width needs two that
    // are not incident.
    let at_vertices = diagram
        .vertices()
        .iter()
        .filter_map(|vertex| {
            let around = diagram
                .edge_rot_next_iterator(vertex.get_incident_edge().ok()?)
                .filter_map(|edge| diagram.edge(edge).ok()?.cell().ok())
                .map(|cell| site_of(cell).0)
                .collect::<Vec<_>>();
            around
                .iter()
                .enumerate()
                .flat_map(|(position, &first)| {
                    around[position + 1..]
                        .iter()
                        .map(move |&second| (first, second))
                })
                .find(|&(first, second)| !incident(first, second))
                .map(|(first, second)| {
                    let center = unquantize([vertex.x(), vertex.y()]);
                    (
                        disk(center, first, second),
                        WidthAxisSegment {
                            start: center,
                            end: center,
                            first_wall: (sites[first].start, sites[first].end),
                            second_wall: (sites[second].start, sites[second].end),
                            uncertainty_mm: 0.0,
                        },
                    )
                })
        })
        .collect::<Vec<_>>();

    // Edges between non-incident sites, sampled along their length and at
    // the apex of a parabola (reflex vertex against a segment) or of a
    // point–point bisector; plus where they cross the component boundary.
    let (mut on_axis, mut on_boundary) = (Vec::new(), Vec::new());
    let mut axis = Vec::new();
    for edge in diagram.edges() {
        let twin = edge.twin().expect("diagram edge twin");
        let (first, first_point) = site_of(edge.cell().expect("diagram edge cell"));
        let (second, second_point) = site_of(
            diagram
                .edge(twin)
                .and_then(|twin| twin.cell())
                .expect("twin cell"),
        );
        if edge.id() > twin || !edge.is_primary() || incident(first, second) {
            continue;
        }
        let samples =
            voronoi_edge_samples(&diagram, edge.id(), &lines, component, reach, units_per_mm)
                .expect("voronoi edge samples")
                .into_iter()
                .map(unquantize)
                .collect::<Vec<_>>();
        axis.extend(samples.windows(2).map(|pair| WidthAxisSegment {
            start: pair[0],
            end: pair[1],
            first_wall: (sites[first].start, sites[first].end),
            second_wall: (sites[second].start, sites[second].end),
            uncertainty_mm: if edge.is_curved() {
                tol::FLATTEN_MM
            } else {
                0.0
            },
        }));
        let foot = |point: Point, site: usize| {
            dist::point_segment(point, sites[site].start, sites[site].end).1
        };
        let apex = match (first_point, second_point) {
            (Some(point), Some(other)) => Some(point.midpoint(other)),
            (Some(point), None) => Some(point.midpoint(foot(point, second))),
            (None, Some(point)) => Some(point.midpoint(foot(point, first))),
            (None, None) => None,
        };
        on_boundary.extend(samples.windows(2).flat_map(|pair| {
            component_edges.iter().filter_map(move |&(start, end)| {
                let (distance, on_edge, _) = dist::segments(pair[0], pair[1], start, end);
                (distance <= contact_tolerance).then(|| disk(on_edge, first, second))
            })
        }));
        on_axis.extend(
            samples
                .into_iter()
                .chain(apex)
                .map(|center| disk(center, first, second)),
        );
    }
    on_axis.extend(at_vertices.iter().map(|(disk, _)| *disk));

    // A disk is maximal only if its radius is its clearance to every site;
    // the builder's degenerate edges can put a center next to a wall it
    // does not touch.
    let clearance = |center: Point| {
        sites
            .iter()
            .map(|site| dist::point_segment(center, site.start, site.end).0)
            .fold(f64::INFINITY, f64::min)
    };
    let centers = on_axis.iter().map(|disk| disk.center).collect::<Vec<_>>();
    let present = on_axis
        .into_iter()
        .zip(component.contains_points_batch(&centers))
        .filter_map(|(disk, inside)| inside.then_some(disk))
        .chain(on_boundary)
        .filter(|disk| disk.radius <= clearance(disk.center) + contact_tolerance)
        .collect::<Vec<_>>();
    // A disk inside a larger present disk up to the flattening tolerance is
    // a branch the flattening sprouted, not a width of the source. Only
    // present disks prune: a void's exterior axis must not swallow a slit
    // thinner than the tolerance.
    let pruned = |disk: &InscribedDisk| {
        present.iter().any(|other| {
            other.radius > disk.radius
                && disk.center.distance_to(other.center) + disk.radius
                    <= other.radius + tol::FLATTEN_MM
        })
    };
    let minimum = present
        .iter()
        .filter(|disk| !pruned(disk))
        // Below two grid units a width is snapping noise, not geometry.
        .filter(|disk| disk.width().mm > 2.0 * contact_tolerance)
        .min_by(|left, right| left.width().mm.total_cmp(&right.width().mm))
        .copied()?;
    // Retain the actual interior axis, removing the portions rejected by the
    // same maximal-disk pruning. Both containment inequalities are convex
    // along a straight axis piece, so their endpoints can be located without
    // turning the candidate's bounding box into a claimed violation region.
    let span_axis = axis.into_iter().flat_map(|segment| {
        let delta = segment.end - segment.start;
        let at = |t| segment.start + delta * t;
        let radius = |t| {
            let point = at(t);
            (dist::point_segment(point, segment.first_wall.0, segment.first_wall.1).0
                + dist::point_segment(point, segment.second_wall.0, segment.second_wall.1).0)
                / 2.0
        };
        let mut retained = segment_inside_intervals(component, segment.start, segment.end);
        for other in &present {
            if retained.is_empty() {
                break;
            }
            if dist::point_segment(other.center, segment.start, segment.end).0
                > other.radius + tol::FLATTEN_MM
            {
                continue;
            }
            let Some(larger) = convex_sublevel_interval(radius, other.radius - tol::EPSILON_MM)
            else {
                continue;
            };
            let Some(contained) = convex_sublevel_interval(
                |t| at(t).distance_to(other.center) + radius(t),
                other.radius + tol::FLATTEN_MM,
            ) else {
                continue;
            };
            let removed = (larger.0.max(contained.0), larger.1.min(contained.1));
            if removed.0 >= removed.1 {
                continue;
            }
            retained = retained
                .into_iter()
                .flat_map(|(start, end)| {
                    let mut pieces = Vec::with_capacity(2);
                    if start < removed.0 {
                        pieces.push((start, end.min(removed.0)));
                    }
                    if end > removed.1 {
                        pieces.push((start.max(removed.1), end));
                    }
                    pieces
                })
                .collect();
        }
        retained
            .into_iter()
            .filter_map(move |(start, end)| {
                let (start, end) = (at(start), at(end));
                (start.distance_to(end) > contact_tolerance).then_some(WidthAxisSegment {
                    start,
                    end,
                    ..segment
                })
            })
            .collect::<Vec<_>>()
    });
    // Voronoi vertices are the zero-dimensional cells of the same medial-axis
    // complex. Preserve the maximal ones as zero-length axis segments so
    // islands and symmetric tips use the exact same construction as spans.
    let vertex_axis = at_vertices
        .into_iter()
        .filter(|(disk, _)| {
            component.contains_point(disk.center)
                && disk.radius <= clearance(disk.center) + contact_tolerance
                && disk.width().mm > 2.0 * contact_tolerance
                && !pruned(disk)
        })
        .map(|(_, segment)| segment);
    let axis = span_axis.chain(vertex_axis).collect();
    Some(ComponentWidth {
        disk: minimum,
        axis,
    })
}

/// The closing residue kept only where two distinct source-boundary
/// branches wall it — the balancing certificate's conservative notion of a
/// narrow void. Contacts on distinct rings always face each other across
/// void, whatever their relative angle; same-ring contacts must oppose, so
/// the rounded bite of one smooth concavity is not mistaken for a gap.
fn two_sided_gap_residual(source: &ContourSet, residual: &ContourSet) -> ContourSet {
    let source_segments = source_boundary_segments(source);
    let contact_tolerance = source.tolerance.max(residual.tolerance);

    let rings = residual
        .connected_components()
        .into_iter()
        .filter(|component| {
            let contacts = source_segments
                .iter()
                .filter(|segment| {
                    segment
                        .bbox
                        .expand(contact_tolerance)
                        .intersects(component.bbox)
                        && region_boundary_within_distance(
                            component,
                            segment.start,
                            segment.end,
                            contact_tolerance,
                        )
                })
                .collect::<Vec<_>>();
            contacts.iter().enumerate().any(|(index, left)| {
                contacts[index + 1..].iter().any(|right| {
                    let (separation, _, _) =
                        dist::segments(left.start, left.end, right.start, right.end);
                    // Contacts on distinct rings always face each other across
                    // void, whatever their relative angle. Same-ring pairs
                    // must additionally oppose so the rounded bite of one
                    // smooth concavity is not mistaken for a gap; walls at
                    // exactly 90° remain ambiguous there by construction.
                    !boundary_segments_are_incident(left.topology, right.topology)
                        && separation > contact_tolerance
                        && (left.topology.ring != right.topology.ring
                            || boundary_tangents_oppose(left, right))
                })
            })
        })
        .flat_map(|component| component.rings)
        .collect();
    ContourSet::new(rings, FillRule::NonZero, residual.tolerance)
}

fn region_boundary_within_distance(
    region: &ContourSet,
    start: Point,
    end: Point,
    distance: f64,
) -> bool {
    let expanded = segment_bbox(start, end).expand(distance);
    region.rings.iter().any(|ring| {
        (0..ring.len()).any(|index| {
            let [other_start_x, other_start_y] = ring[index];
            let [other_end_x, other_end_y] = ring[(index + 1) % ring.len()];
            let other_start = Point::new(other_start_x, other_start_y);
            let other_end = Point::new(other_end_x, other_end_y);
            expanded.intersects(segment_bbox(other_start, other_end))
                && dist::segments(start, end, other_start, other_end).0 <= distance
        })
    })
}

fn boundary_tangents_oppose(
    left: &OrientedBoundarySegment,
    right: &OrientedBoundarySegment,
) -> bool {
    left.tangent.x * right.tangent.x + left.tangent.y * right.tangent.y < 0.0
}

/// Keep-out whose removal widens every narrow void: a radius-`radius` tube
/// around the boundary medial axis inside the narrow phase. A void component
/// thinner than the axis stroke has no representable axis and is swept whole
/// instead; it sits far below the regularization scale, so even that blunt
/// cut stays local, and the keep-out always covers every component.
fn narrow_void_keep_out(
    source: &ContourSet,
    narrow_voids: &ContourSet,
    radius: f64,
) -> Result<ContourSet, GapRegularizationError> {
    let axis_keep_out = narrow_void_medial_axis_keep_out(source, narrow_voids, radius)?;
    let axisless = narrow_voids
        .connected_components()
        .into_iter()
        .filter(|component| component.intersection(&axis_keep_out).is_empty())
        .collect::<Vec<_>>();
    Ok(axisless.into_iter().fold(axis_keep_out, |keep_out, thin| {
        keep_out.union(&thin.disk_dilate(radius))
    }))
}

fn narrow_void_medial_axis_keep_out(
    source: &ContourSet,
    narrow_voids: &ContourSet,
    radius: f64,
) -> Result<ContourSet, GapRegularizationError> {
    if narrow_voids.is_empty() {
        return Ok(ContourSet::empty(source.tolerance));
    }
    let origin = Point::new(source.bbox.min.x, source.bbox.min.y);
    let mut segments = Vec::<VoronoiLine<i32>>::new();
    let mut boundary_segments = Vec::new();
    for (ring_id, ring) in source.rings.iter().enumerate() {
        for index in 0..ring.len() {
            let [start_x, start_y] = ring[index];
            let [end_x, end_y] = ring[(index + 1) % ring.len()];
            if (end_x - start_x).hypot(end_y - start_y) <= source.tolerance {
                continue;
            }
            let start = quantize_voronoi_point(ring[index], origin)?;
            let end = quantize_voronoi_point(ring[(index + 1) % ring.len()], origin)?;
            if start == end {
                continue;
            }
            segments.push(VoronoiLine::new(start, end));
            boundary_segments.push(BoundarySegment {
                ring: ring_id,
                index,
                ring_len: ring.len(),
            });
        }
    }

    let diagram = VoronoiBuilder::<i32>::default()
        .with_segments(segments.iter())
        .and_then(VoronoiBuilder::build)
        .map_err(|error| {
            GapRegularizationError(format!(
                "could not construct boundary Voronoi diagram: {error}"
            ))
        })?;
    let mut contours = Vec::new();
    for edge in diagram.edges() {
        let twin = edge.twin().map_err(gap_regularization_error)?;
        if edge.id() > twin || !edge.is_primary() {
            continue;
        }
        let left = diagram
            .cell(edge.cell().map_err(gap_regularization_error)?)
            .map_err(gap_regularization_error)?;
        let right = diagram
            .cell(
                diagram
                    .edge(twin)
                    .and_then(|edge| edge.cell())
                    .map_err(gap_regularization_error)?,
            )
            .map_err(gap_regularization_error)?;
        let left_boundary = boundary_segments
            .get(left.source_index().usize())
            .ok_or_else(|| {
                GapRegularizationError(
                    "Voronoi cell references an unknown boundary segment".to_string(),
                )
            })?;
        let right_boundary = boundary_segments
            .get(right.source_index().usize())
            .ok_or_else(|| {
                GapRegularizationError(
                    "Voronoi cell references an unknown boundary segment".to_string(),
                )
            })?;
        if boundary_segments_are_incident(*left_boundary, *right_boundary) {
            continue;
        }
        let samples = voronoi_edge_samples(
            &diagram,
            edge.id(),
            &segments,
            source,
            radius,
            VORONOI_COORDINATES_PER_MM,
        )?;
        let mut commands = Vec::with_capacity(samples.len());
        for sample in samples {
            let point = Point::new(
                sample[0] / VORONOI_COORDINATES_PER_MM + origin.x,
                sample[1] / VORONOI_COORDINATES_PER_MM + origin.y,
            );
            if commands
                .last()
                .and_then(|command: &PathCmd| command.end_point())
                .is_some_and(|previous| previous.distance_to(point) <= tol::EPSILON_MM)
            {
                continue;
            }
            commands.push(if commands.is_empty() {
                PathCmd::move_to(point)
            } else {
                PathCmd::line_to(point)
            });
        }
        if commands.len() >= 2 {
            contours.push(ContourBuf::new(commands));
        }
    }

    if contours.is_empty() {
        return Ok(ContourSet::empty(source.tolerance));
    }
    // A narrow filled stroke makes the one-dimensional axis available to the
    // existing set algebra. Intersecting with a slightly eroded narrow-void
    // phase removes the finite stroke's boundary fringe and the exterior axis.
    let axis_stroke_radius = tol::REGION_MM;
    let mut arena = PathArena::default();
    let path = arena.push_path(
        Paint::Stroke(crate::geom::StrokeStyle::round(2.0 * axis_stroke_radius)),
        contours,
    );
    let medial_axis = ContourSet::from_painted_paths(
        &arena,
        std::iter::once(&arena.paths[path as usize]),
        source.tolerance,
    );
    let interior_axis =
        medial_axis.intersection(&narrow_voids.disk_erode(2.0 * axis_stroke_radius));
    let keep_out = interior_axis.disk_dilate(radius);
    Ok(keep_out.intersection(&source.disk_dilate(radius)))
}

fn boundary_segments_are_incident(left: BoundarySegment, right: BoundarySegment) -> bool {
    if left.ring != right.ring {
        return false;
    }
    let distance = left.index.abs_diff(right.index);
    distance.min(left.ring_len - distance) <= 1
}

fn quantize_voronoi_point(
    [x, y]: [f64; 2],
    origin: Point,
) -> Result<VoronoiPoint<i32>, GapRegularizationError> {
    fn coordinate(value: f64, origin: f64) -> Result<i32, GapRegularizationError> {
        let scaled = ((value - origin) * VORONOI_COORDINATES_PER_MM).round();
        if !scaled.is_finite() || scaled < i32::MIN as f64 || scaled > i32::MAX as f64 {
            return Err(GapRegularizationError(
                "component geometry exceeds the Voronoi coordinate range".to_string(),
            ));
        }
        Ok(scaled as i32)
    }

    Ok(VoronoiPoint::new(
        coordinate(x, origin.x)?,
        coordinate(y, origin.y)?,
    ))
}

fn gap_regularization_error(error: boostvoronoi::BvError) -> GapRegularizationError {
    GapRegularizationError(format!("invalid boundary Voronoi diagram: {error}"))
}

fn voronoi_edge_samples(
    diagram: &VoronoiDiagram,
    edge_id: VoronoiEdgeIndex,
    segments: &[VoronoiLine<i32>],
    region: &ContourSet,
    radius: f64,
    units_per_mm: f64,
) -> Result<Vec<[f64; 2]>, GapRegularizationError> {
    let edge = diagram.edge(edge_id).map_err(gap_regularization_error)?;
    let affine = SimpleAffine::default();
    let mut samples = if let (Some(start), Some(end)) = (
        edge.vertex0(),
        diagram
            .edge_get_vertex1(edge_id)
            .map_err(gap_regularization_error)?,
    ) {
        let start = diagram.vertex(start).map_err(gap_regularization_error)?;
        let end = diagram.vertex(end).map_err(gap_regularization_error)?;
        vec![
            affine.transform(start.x(), start.y()),
            affine.transform(end.x(), end.y()),
        ]
    } else {
        clip_infinite_voronoi_edge(diagram, edge_id, segments, region, radius, units_per_mm)?
    };

    if edge.is_curved() {
        let cell = edge.cell().map_err(gap_regularization_error)?;
        let twin_cell = diagram
            .edge(edge.twin().map_err(gap_regularization_error)?)
            .and_then(|edge| edge.cell())
            .map_err(gap_regularization_error)?;
        let (point_cell, segment_cell) = if diagram
            .cell(cell)
            .map_err(gap_regularization_error)?
            .contains_point()
        {
            (cell, twin_cell)
        } else {
            (twin_cell, cell)
        };
        let point = voronoi_cell_point(diagram, point_cell, segments)?;
        let segment = voronoi_cell_segment(diagram, segment_cell, segments)?;
        VoronoiVisualUtils::discretize(
            &point,
            segment,
            tol::FLATTEN_MM * units_per_mm,
            &affine,
            &mut samples,
        );
    }
    Ok(samples)
}

fn clip_infinite_voronoi_edge(
    diagram: &VoronoiDiagram,
    edge_id: VoronoiEdgeIndex,
    segments: &[VoronoiLine<i32>],
    region: &ContourSet,
    radius: f64,
    units_per_mm: f64,
) -> Result<Vec<[f64; 2]>, GapRegularizationError> {
    let edge = diagram.edge(edge_id).map_err(gap_regularization_error)?;
    let cell = edge.cell().map_err(gap_regularization_error)?;
    let twin_cell = diagram
        .edge(edge.twin().map_err(gap_regularization_error)?)
        .and_then(|edge| edge.cell())
        .map_err(gap_regularization_error)?;
    let left = diagram.cell(cell).map_err(gap_regularization_error)?;
    let right = diagram.cell(twin_cell).map_err(gap_regularization_error)?;
    let (origin, direction) = if left.contains_point() && right.contains_point() {
        let left = voronoi_cell_point(diagram, cell, segments)?;
        let right = voronoi_cell_point(diagram, twin_cell, segments)?;
        (
            [
                (left.x as f64 + right.x as f64) * 0.5,
                (left.y as f64 + right.y as f64) * 0.5,
            ],
            [
                left.y as f64 - right.y as f64,
                right.x as f64 - left.x as f64,
            ],
        )
    } else {
        let (point_cell, segment_cell) = if left.contains_segment() {
            (twin_cell, cell)
        } else {
            (cell, twin_cell)
        };
        let point = voronoi_cell_point(diagram, point_cell, segments)?;
        let segment = voronoi_cell_segment(diagram, segment_cell, segments)?;
        let origin = [point.x as f64, point.y as f64];
        let dx = segment.end.x - segment.start.x;
        let dy = segment.end.y - segment.start.y;
        let direction = if ([segment.start.x as f64, segment.start.y as f64] == origin)
            ^ left.contains_point()
        {
            [dy as f64, -dx as f64]
        } else {
            [-dy as f64, dx as f64]
        };
        (origin, direction)
    };
    let reach = (region.bbox.width().max(region.bbox.height()) + 4.0 * radius) * units_per_mm;
    let direction_scale = direction[0].abs().max(direction[1].abs());
    if direction_scale == 0.0 {
        return Err(GapRegularizationError(
            "infinite Voronoi edge has no direction".to_string(),
        ));
    }
    let coefficient = reach / direction_scale;
    let affine = SimpleAffine::default();
    let start = edge
        .vertex0()
        .map(|vertex| {
            diagram
                .vertex(vertex)
                .map(|vertex| affine.transform(vertex.x(), vertex.y()))
        })
        .transpose()
        .map_err(gap_regularization_error)?
        .unwrap_or([
            origin[0] - direction[0] * coefficient,
            origin[1] - direction[1] * coefficient,
        ]);
    let end = diagram
        .edge_get_vertex1(edge_id)
        .map_err(gap_regularization_error)?
        .map(|vertex| {
            diagram
                .vertex(vertex)
                .map(|vertex| affine.transform(vertex.x(), vertex.y()))
        })
        .transpose()
        .map_err(gap_regularization_error)?
        .unwrap_or([
            origin[0] + direction[0] * coefficient,
            origin[1] + direction[1] * coefficient,
        ]);
    Ok(vec![start, end])
}

fn voronoi_cell_point(
    diagram: &VoronoiDiagram,
    cell: VoronoiCellIndex,
    segments: &[VoronoiLine<i32>],
) -> Result<VoronoiPoint<i32>, GapRegularizationError> {
    let cell = diagram.cell(cell).map_err(gap_regularization_error)?;
    let segment = segments.get(cell.source_index().usize()).ok_or_else(|| {
        GapRegularizationError("Voronoi point references an unknown segment".to_string())
    })?;
    Ok(match cell.source_category() {
        SourceCategory::SegmentStart => segment.start,
        SourceCategory::Segment | SourceCategory::SegmentEnd => segment.end,
        SourceCategory::SinglePoint => {
            return Err(GapRegularizationError(
                "unexpected standalone point in component Voronoi diagram".to_string(),
            ));
        }
    })
}

fn voronoi_cell_segment<'a>(
    diagram: &VoronoiDiagram,
    cell: VoronoiCellIndex,
    segments: &'a [VoronoiLine<i32>],
) -> Result<&'a VoronoiLine<i32>, GapRegularizationError> {
    let index = diagram
        .cell(cell)
        .map_err(gap_regularization_error)?
        .source_index()
        .usize();
    segments.get(index).ok_or_else(|| {
        GapRegularizationError("Voronoi cell references an unknown segment".to_string())
    })
}

#[cfg(test)]
fn regions_within_distance(left: &ContourSet, right: &ContourSet, distance: f64) -> bool {
    if !left.bbox.expand(distance).intersects(right.bbox) {
        return false;
    }
    let threshold = distance + left.tolerance.max(right.tolerance);
    for left_ring in &left.rings {
        for left_index in 0..left_ring.len() {
            let [left_start_x, left_start_y] = left_ring[left_index];
            let [left_end_x, left_end_y] = left_ring[(left_index + 1) % left_ring.len()];
            let left_start = Point::new(left_start_x, left_start_y);
            let left_end = Point::new(left_end_x, left_end_y);
            let left_bbox = segment_bbox(left_start, left_end).expand(threshold);
            for right_ring in &right.rings {
                for right_index in 0..right_ring.len() {
                    let [right_start_x, right_start_y] = right_ring[right_index];
                    let [right_end_x, right_end_y] =
                        right_ring[(right_index + 1) % right_ring.len()];
                    let right_start = Point::new(right_start_x, right_start_y);
                    let right_end = Point::new(right_end_x, right_end_y);
                    if !left_bbox.intersects(segment_bbox(right_start, right_end)) {
                        continue;
                    }
                    let (separation, _, _) =
                        dist::segments(left_start, left_end, right_start, right_end);
                    if separation <= threshold {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn segment_bbox(start: Point, end: Point) -> BBox {
    BBox::new(
        Point::new(start.x.min(end.x), start.y.min(end.y)),
        Point::new(start.x.max(end.x), start.y.max(end.y)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::shapes;

    #[test]
    fn component_width_prunes_only_beyond_grid_uncertainty() {
        let measure = |height| {
            let component = ContourSet::rectangle(rect(0.0, 0.0, 1.0, height), tol::REGION_MM);
            component_width(
                &source_boundary_segments(&component),
                &component,
                0.05,
                tol::REGION_MM,
            )
        };

        let width = measure(0.101).expect("grid uncertainty keeps the reachable opposing walls");
        assert!((width.disk.width().mm - 0.101).abs() < 1e-9);

        assert!(measure(0.103).is_none());
    }

    #[test]
    fn ring_components_group_holes_with_the_smallest_outer_ring_around_them() {
        let region = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), tol::REGION_MM)
            .difference(&ContourSet::rectangle(
                rect(2.0, 2.0, 8.0, 8.0),
                tol::REGION_MM,
            ))
            .union(&ContourSet::rectangle(
                rect(3.0, 3.0, 7.0, 7.0),
                tol::REGION_MM,
            ))
            .difference(&ContourSet::rectangle(
                rect(4.0, 4.0, 6.0, 6.0),
                tol::REGION_MM,
            ))
            .union(&ContourSet::rectangle(
                rect(20.0, 0.0, 30.0, 10.0),
                tol::REGION_MM,
            ));
        assert_eq!(region.rings.len(), 5);

        let (components, _) = region.ring_components();
        let component_of = |min_x: f64| {
            let ring = region
                .ring_bounds
                .iter()
                .position(|bounds| bounds.min.x == min_x)
                .unwrap();
            components[ring]
        };
        assert_eq!(component_of(0.0), component_of(2.0));
        assert_eq!(component_of(3.0), component_of(4.0));
        assert_ne!(component_of(0.0), component_of(3.0));
        assert_ne!(component_of(0.0), component_of(20.0));
        assert_ne!(component_of(3.0), component_of(20.0));
    }

    #[test]
    fn facing_components_keep_only_walls_within_reach_of_each_other() {
        let trace = ContourSet::rectangle(rect(0.0, 0.0, 5.0, 0.1), tol::REGION_MM);
        let pad = ContourSet::rectangle(rect(10.0, 0.0, 12.0, 2.0), tol::REGION_MM);
        let neighbour = ContourSet::rectangle(rect(12.15, 0.0, 14.0, 2.0), tol::REGION_MM);
        let region = trace.union(&pad).union(&neighbour);
        assert_eq!(region.rings.len(), 3);

        let same_bounds = |left: BBox, right: BBox| {
            left.min.distance_to(right.min) < 1e-6 && left.max.distance_to(right.max) < 1e-6
        };
        // The trace's long walls face across material; a reach past the
        // whole trace keeps all of it and none of the pads.
        let material = region.facing_components(0.2, 0.01, 1.0);
        assert_eq!(material.rings.len(), 1);
        assert!(same_bounds(material.bbox, trace.bbox));

        // The pads face each other across their gap, along their whole
        // aligned edges, and the trace is out of reach.
        let void = region.facing_components(0.0, 0.2, 0.5);
        assert_eq!(void.rings.len(), 2);
        assert!(same_bounds(void.bbox, pad.bbox.union(neighbour.bbox)));

        let with_context = region.facing_components(0.0, 0.2, 10.0);
        assert_eq!(with_context.rings.len(), 3);

        // A notch faces across void; a plane web between holes across material.
        let notched = ContourSet::rectangle(rect(0.0, 0.0, 4.0, 4.0), tol::REGION_MM).difference(
            &ContourSet::rectangle(rect(1.9, 2.0, 2.1, 4.5), tol::REGION_MM),
        );
        assert_eq!(notched.facing_components(0.0, 0.3, 1.0).rings.len(), 1);
        assert!(notched.facing_components(0.15, 0.15, 1.0).is_empty());
        let webbed = ContourSet::rectangle(rect(0.0, 0.0, 8.0, 4.0), tol::REGION_MM)
            .difference(&ContourSet::rectangle(
                rect(1.0, 1.0, 1.9, 3.0),
                tol::REGION_MM,
            ))
            .difference(&ContourSet::rectangle(
                rect(2.1, 1.0, 3.0, 3.0),
                tol::REGION_MM,
            ))
            .difference(&ContourSet::rectangle(
                rect(6.0, 1.0, 7.0, 3.0),
                tol::REGION_MM,
            ));
        let nearby_web = webbed.facing_components(0.3, 0.0, 1.0);
        assert_eq!(nearby_web.rings.len(), 3);
        assert!(nearby_web.contains_point(Point::new(2.0, 2.0)));
        assert!(nearby_web.contains_point(Point::new(2.0, 3.5)));
        assert!(!nearby_web.contains_point(Point::new(6.5, 2.0)));
        assert!(!nearby_web.contains_point(Point::new(5.0, 3.5)));
        assert!(webbed.facing_components(0.0, 0.15, 1.0).is_empty());
    }

    #[test]
    fn segment_grid_uses_coarse_cells_without_losing_local_edges() {
        let region = ContourSet::rectangle(rect(0.0, 0.0, 400.0, 10.0), tol::REGION_MM);
        let grid = SegmentGrid::new(source_boundary_segments(&region), 0.05);

        assert_eq!(grid.grid.pitch_mm(), MIN_SEGMENT_GRID_CELL_MM);
        let near = grid.near(rect(199.9, -0.1, 200.1, 0.1));
        assert_eq!(near.len(), 1);
        assert_eq!(near[0].start.y, 0.0);
        assert_eq!(near[0].end.y, 0.0);
    }

    #[test]
    fn planar_sites_split_along_the_segment_whichever_way_it_runs() {
        let site = |start: (f64, f64), end: (f64, f64), index: usize| OrientedBoundarySegment {
            topology: BoundarySegment {
                ring: index / 10,
                index,
                ring_len: 10,
            },
            start: Point::new(start.0, start.1),
            end: Point::new(end.0, end.1),
            tangent: Point::new(end.0 - start.0, end.1 - start.1),
            bbox: segment_bbox(Point::new(start.0, start.1), Point::new(end.0, end.1)),
        };
        // A right-to-left host touched at two interior points by other rings.
        let sites = [
            site((10.0, 0.0), (0.0, 0.0), 0),
            site((7.0, 5.0), (7.0, 0.0), 10),
            site((3.0, 5.0), (3.0, 0.0), 20),
        ];
        let grid = planar_grid_sites(&sites, |point| {
            VoronoiPoint::new(point.x as i32, point.y as i32)
        });

        let host = grid
            .iter()
            .filter(|(_, site)| site.topology.index == 0)
            .map(|(line, _)| line)
            .collect::<Vec<_>>();
        assert_eq!(host.len(), 3);
        assert_eq!(host[0].start, VoronoiPoint::new(10, 0));
        for pair in host.windows(2) {
            assert_eq!(pair[0].end, pair[1].start, "pieces chain along the host");
        }
        assert_eq!(host[2].end, VoronoiPoint::new(0, 0));
    }

    #[test]
    fn inward_decimation_only_shrinks_and_respects_deviation() {
        let ring = ContourSet::from_contours(
            &[shapes::circle(10.0).unwrap(), shapes::circle(6.0).unwrap()],
            FillRule::EvenOdd,
            tol::REGION_MM,
        );
        let deviation = 0.05;
        let decimated = ring.decimate_inward(deviation);

        // The outer boundary decimates; the convex hole cannot lose a vertex
        // without growing the region, so it stays exact.
        let ring_len = |set: &ContourSet, hole: bool| {
            set.rings
                .iter()
                .find(|ring| (ring_signed_area(ring) < 0.0) == hole)
                .expect("annulus ring")
                .len()
        };
        assert_eq!(
            ring_len(&decimated, true),
            ring_len(&ring, true),
            "hole ring must stay exact"
        );
        assert!(
            ring_len(&decimated, false) * 2 < ring_len(&ring, false),
            "outer ring kept {} of {} vertices",
            ring_len(&decimated, false),
            ring_len(&ring, false)
        );

        // The region only shrinks: nothing outside the source survives.
        assert!(decimated.difference(&ring).area() < 1e-9);

        // Area loss is bounded by the deviation times the boundary length.
        let perimeter: f64 = ring
            .rings
            .iter()
            .flat_map(ring_edges)
            .map(|(start, end)| start.distance_to(end))
            .sum();
        assert!(ring.area() - decimated.area() <= deviation * perimeter);
    }

    #[test]
    fn fixed_grid_regularization_removes_sub_grid_geometry() {
        let shapes = simplify_shapes_on_grid(
            vec![
                vec![
                    [0.0004, 0.0004],
                    [1.0004, 0.0004],
                    [1.00049, 0.00049],
                    [1.0004, 1.0004],
                    [0.0004, 1.0004],
                ],
                vec![
                    [2.0001, 0.0],
                    [2.0004, 0.0],
                    [2.0004, 0.0003],
                    [2.0001, 0.0003],
                ],
            ],
            FillRule::NonZero,
            0.001,
        );

        assert_eq!(shapes.len(), 1);
        assert!((rings_area(&shapes[0]) - 1.0).abs() < 1e-9);
        for ring in &shapes[0] {
            for point in ring {
                assert!((point[0] * 1000.0 - (point[0] * 1000.0).round()).abs() < 1e-9);
                assert!((point[1] * 1000.0 - (point[1] * 1000.0).round()).abs() < 1e-9);
            }
            for (start, end) in ring
                .iter()
                .zip(ring.iter().cycle().skip(1))
                .take(ring.len())
            {
                assert!((start[0] - end[0]).hypot(start[1] - end[1]) >= 0.001);
            }
        }
    }

    /// Partial cells are the whole point: a sampled estimate would round each
    /// of these to nothing or to everything.
    #[test]
    fn grid_coverage_measures_partly_covered_cells_exactly() {
        let square = ContourSet::rectangle(rect(0.5, 0.5, 2.5, 2.5), tol::REGION_MM);

        let coverage = square.grid_coverage(rect(0.0, 0.0, 3.0, 3.0), 3, 3);

        #[rustfmt::skip]
        let expected = [
            0.25, 0.5, 0.25,
            0.5,  1.0, 0.5,
            0.25, 0.5, 0.25,
        ];
        for (measured, expected) in coverage.iter().zip(expected) {
            assert!(
                (measured - expected).abs() < 1e-12,
                "{measured} != {expected}"
            );
        }
    }

    /// Holes are separate rings wound against their outer, and the cell they
    /// fall in has to see that sign.
    #[test]
    fn grid_coverage_subtracts_holes() {
        let ring = ContourSet::rectangle(rect(0.0, 0.0, 4.0, 4.0), tol::REGION_MM).difference(
            &ContourSet::rectangle(rect(1.0, 1.0, 2.0, 3.0), tol::REGION_MM),
        );

        let coverage = ring.grid_coverage(rect(0.0, 0.0, 4.0, 4.0), 1, 1);

        assert!((coverage[0] - 14.0 / 16.0).abs() < 1e-12, "{coverage:?}");
    }

    #[test]
    fn contour_set_composes_region_operations() {
        let outer = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), tol::REGION_MM);
        let inner = ContourSet::rectangle(rect(3.0, 3.0, 7.0, 7.0), tol::REGION_MM);
        let clip = ContourSet::rectangle(rect(5.0, 0.0, 10.0, 10.0), tol::REGION_MM);

        let ring = outer.difference(&inner);
        let clipped = ring.intersection(&clip);
        let expanded = clipped.disk_dilate(0.5);

        assert!(!expanded.is_empty());
        assert!((expanded.bbox.min.x - 4.5).abs() <= 1e-9);
        assert!((expanded.bbox.max.x - 10.5).abs() <= 1e-9);
    }

    #[test]
    fn filled_contour_region_is_winding_insensitive() {
        let clockwise = rectangle_contour(0.0, 0.0, 10.0, 5.0);
        let counter_clockwise = ContourBuf::new(vec![
            PathCmd::move_to(Point::new(0.0, 5.0)),
            PathCmd::line_to(Point::new(10.0, 5.0)),
            PathCmd::line_to(Point::new(10.0, 0.0)),
            PathCmd::line_to(Point::new(0.0, 0.0)),
            PathCmd::close(),
        ]);

        let a = ContourSet::from_filled_contours(std::slice::from_ref(&clockwise), tol::REGION_MM);
        let b = ContourSet::from_filled_contours(
            std::slice::from_ref(&counter_clockwise),
            tol::REGION_MM,
        );
        let unioned =
            ContourSet::from_filled_contours(&[clockwise, counter_clockwise], tol::REGION_MM);

        assert!(!a.is_empty());
        assert!((a.area() - b.area()).abs() <= 1e-9);
        assert!((unioned.area() - 50.0).abs() <= 1e-6);
    }

    #[test]
    fn area_subtracts_holes() {
        let outer = ContourSet::rectangle(rect(0.0, 0.0, 4.0, 4.0), tol::REGION_MM);
        let inner = ContourSet::rectangle(rect(1.0, 1.0, 3.0, 3.0), tol::REGION_MM);

        let ring = outer.difference(&inner);

        assert!((ring.area() - 12.0).abs() <= 1e-6);
    }

    #[test]
    fn containment_observes_boundaries_and_holes() {
        let outer = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), tol::REGION_MM);
        let hole = ContourSet::rectangle(rect(4.0, 4.0, 6.0, 6.0), tol::REGION_MM);
        let region = outer.difference(&hole);

        assert!(region.contains_point(Point::new(2.0, 2.0)));
        assert!(region.contains_point(Point::new(0.0, 5.0)));
        assert!(!region.contains_point(Point::new(5.0, 5.0)));
        assert!(region.contains_disk(Point::new(2.0, 2.0), 2.0));
        assert!(!region.contains_disk(Point::new(2.0, 2.0), 2.01));
        assert!(!region.contains_disk(Point::new(3.5, 5.0), 0.6));
    }

    type ExpectedSpan = ((f64, f64), (f64, f64));

    fn assert_spans(actual: Vec<(Point, Point)>, expected: &[ExpectedSpan]) {
        assert_eq!(actual.len(), expected.len(), "{actual:?}");
        for ((start, end), &(from, to)) in actual.iter().zip(expected) {
            assert!(
                start.distance_to(Point::new(from.0, from.1)) <= 1e-8,
                "{actual:?}"
            );
            assert!(
                end.distance_to(Point::new(to.0, to.1)) <= 1e-8,
                "{actual:?}"
            );
        }
    }

    #[test]
    fn segment_spans_preserve_holes_and_clip_to_the_query() {
        let outer = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), tol::REGION_MM);
        let hole = ContourSet::rectangle(rect(4.0, 2.0, 6.0, 8.0), tol::REGION_MM);
        let ring = outer.difference(&hole);

        assert_spans(
            ring.segment_spans(Point::new(-2.0, 5.0), Point::new(12.0, 5.0)),
            &[((0.0, 5.0), (4.0, 5.0)), ((6.0, 5.0), (10.0, 5.0))],
        );
        assert_spans(
            ring.segment_spans(Point::new(2.0, 5.0), Point::new(9.0, 5.0)),
            &[((2.0, 5.0), (4.0, 5.0)), ((6.0, 5.0), (9.0, 5.0))],
        );
    }

    #[test]
    fn segment_spans_preserve_disconnected_and_concave_regions() {
        let left = ContourSet::rectangle(rect(0.0, 0.0, 2.0, 2.0), tol::REGION_MM);
        let concave = ContourSet::new(
            vec![vec![
                [4.0, 0.0],
                [8.0, 0.0],
                [8.0, 1.0],
                [5.0, 1.0],
                [5.0, 2.0],
                [4.0, 2.0],
            ]],
            FillRule::NonZero,
            tol::REGION_MM,
        );
        assert_spans(
            left.union(&concave)
                .segment_spans(Point::new(-1.0, 1.5), Point::new(9.0, 1.5)),
            &[((0.0, 1.5), (2.0, 1.5)), ((4.0, 1.5), (5.0, 1.5))],
        );
    }

    #[test]
    fn segment_spans_follow_reversed_arbitrary_direction() {
        let square = ContourSet::rectangle(rect(0.0, 0.0, 4.0, 4.0), tol::REGION_MM);
        assert_spans(
            square.segment_spans(Point::new(6.0, 6.0), Point::new(-2.0, -2.0)),
            &[((4.0, 4.0), (0.0, 0.0))],
        );
    }

    #[test]
    fn segment_spans_include_boundary_but_not_tangencies() {
        let square = ContourSet::rectangle(rect(0.0, 0.0, 4.0, 4.0), tol::REGION_MM);
        assert_spans(
            square.segment_spans(Point::new(-1.0, 0.0), Point::new(3.0, 0.0)),
            &[((0.0, 0.0), (3.0, 0.0))],
        );
        assert!(
            square
                .segment_spans(Point::new(-1.0, 1.0), Point::new(1.0, -1.0))
                .is_empty()
        );
    }

    #[test]
    fn segment_spans_omit_degenerate_and_sub_tolerance_intervals() {
        let square = ContourSet::rectangle(rect(0.0, 0.0, 4.0, 4.0), tol::EPSILON_MM);
        assert!(
            square
                .segment_spans(Point::new(1.0, 1.0), Point::new(1.0, 1.0))
                .is_empty()
        );
        assert!(
            square
                .segment_spans(Point::new(-1e-10, 2.0), Point::new(0.0, 2.0))
                .is_empty()
        );
        assert_spans(
            square.segment_spans(Point::new(-1e-5, 2.0), Point::new(1e-5, 2.0)),
            &[((0.0, 2.0), (1e-5, 2.0))],
        );
    }

    #[test]
    fn bridged_contour_preserves_local_holes_without_clear_polarity() {
        let outer = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), tol::REGION_MM);
        let circle = shapes::circle(2.0).unwrap();
        let circle = crate::geom::path::transform_cmds(
            circle.cmds,
            crate::geom::Affine2::translation(Point::new(5.0, 5.0)),
        );
        let hole = ContourSet::from_filled_contours(&[circle], tol::REGION_MM);
        let region = outer.difference(&hole);

        let contours = region.to_bridged_contours();
        let round_trip = ContourSet::from_contours(&contours, FillRule::NonZero, tol::REGION_MM);

        assert_eq!(contours.len(), 1);
        assert!(
            (round_trip.area() - region.area()).abs() <= 0.01,
            "bridged area {}, source area {}",
            round_trip.area(),
            region.area()
        );
        assert!(!round_trip.contains_point(Point::new(5.0, 5.0)));
    }

    #[test]
    fn erodes_outer_boundaries_and_expands_holes() {
        let outer = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), tol::REGION_MM);
        let hole = ContourSet::rectangle(rect(4.0, 4.0, 6.0, 6.0), tol::REGION_MM);

        let eroded = outer.difference(&hole).disk_erode(0.5);

        assert!((eroded.bbox.min.x - 0.5).abs() <= 1e-9);
        assert!((eroded.bbox.min.y - 0.5).abs() <= 1e-9);
        assert!((eroded.bbox.max.x - 9.5).abs() <= 1e-9);
        assert!((eroded.bbox.max.y - 9.5).abs() <= 1e-9);
        let area = eroded.area();
        assert!(
            (area - 72.214601837).abs() <= 2e-2,
            "unexpected eroded area {area}"
        );
    }

    #[test]
    fn erosion_can_remove_an_entire_region() {
        let region = ContourSet::rectangle(rect(0.0, 0.0, 0.5, 0.5), tol::REGION_MM);

        let eroded = region.disk_erode(0.5);

        assert!(eroded.is_empty());
    }

    #[test]
    fn disk_opening_rounds_corners_and_stays_inside_source() {
        let region = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), tol::REGION_MM);

        let opened = region.disk_open(0.5);

        assert!(opened.difference(&region).is_empty());
        assert!((opened.bbox.min.x - 0.0).abs() <= 1e-9);
        assert!((opened.bbox.min.y - 0.0).abs() <= 1e-9);
        assert!((opened.bbox.max.x - 10.0).abs() <= 1e-9);
        assert!((opened.bbox.max.y - 10.0).abs() <= 1e-9);
        assert!((opened.area() - (99.0 + std::f64::consts::PI / 4.0)).abs() <= 2e-2);
    }

    #[test]
    fn disk_opening_removes_sub_diameter_slivers_and_small_islands() {
        let body = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), tol::REGION_MM);
        let sliver = ContourSet::rectangle(rect(12.0, 0.0, 20.0, 0.8), tol::REGION_MM);
        let island = ContourSet::rectangle(rect(22.0, 0.0, 22.8, 0.8), tol::REGION_MM);
        let region = body.union(&sliver).union(&island);

        let opened = region.disk_open(0.5);

        assert_eq!(opened.connected_components().len(), 1);
        assert!(opened.intersection(&body).area() > 99.0);
        assert!(opened.intersection(&sliver).is_empty());
        assert!(opened.intersection(&island).is_empty());
    }

    #[test]
    fn disk_opening_is_idempotent_within_offset_tolerance() {
        let outer = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), tol::REGION_MM);
        let notch = ContourSet::rectangle(rect(4.0, 8.0, 6.0, 10.0), tol::REGION_MM);
        let region = outer.difference(&notch);

        let once = region.disk_open(0.5);
        let twice = once.disk_open(0.5);
        let symmetric_difference = once.difference(&twice).union(&twice.difference(&once));

        assert!(
            symmetric_difference.area() <= 2e-2,
            "opening changed by {:.9} mm² on repetition",
            symmetric_difference.area()
        );
    }

    #[test]
    fn disk_closing_fills_sub_diameter_gaps_and_stays_outside_source() {
        let left = ContourSet::rectangle(rect(0.0, 0.0, 4.0, 10.0), tol::REGION_MM);
        let right = ContourSet::rectangle(rect(4.8, 0.0, 10.0, 10.0), tol::REGION_MM);
        let region = left.union(&right);

        let closed = region.disk_close(0.5);
        let middle = ContourSet::rectangle(rect(4.0, 1.0, 4.8, 9.0), tol::REGION_MM);

        assert!(region.difference(&closed).is_empty());
        assert!(closed.intersection(&middle).area() > 6.3);
    }

    #[test]
    fn disk_closing_preserves_wide_gaps() {
        let left = ContourSet::rectangle(rect(0.0, 0.0, 4.0, 10.0), tol::REGION_MM);
        let right = ContourSet::rectangle(rect(5.2, 0.0, 10.0, 10.0), tol::REGION_MM);
        let region = left.union(&right);

        let closed = region.disk_close(0.5);
        let middle = ContourSet::rectangle(rect(4.0, 1.0, 5.2, 9.0), tol::REGION_MM);

        assert!(closed.intersection(&middle).is_empty());
    }

    #[test]
    fn disk_gap_violations_report_close_distinct_components() {
        let left = ContourSet::rectangle(rect(0.0, 0.0, 4.0, 10.0), tol::REGION_MM);
        let close = ContourSet::rectangle(rect(4.8, 0.0, 10.0, 10.0), tol::REGION_MM);
        let wide = ContourSet::rectangle(rect(5.2, 0.0, 10.0, 10.0), tol::REGION_MM);

        let close_violations = left.union(&close).disk_gap_violations(0.5);
        let wide_violations = left.union(&wide).disk_gap_violations(0.5);

        assert!(close_violations.area() > 1.5);
        assert!(wide_violations.is_empty());
        assert!(left.disk_gap_violations(0.5).is_empty());
    }

    #[test]
    fn disk_gap_regularization_sweeps_a_void_thinner_than_the_axis_stroke() {
        // A 3 µm gap is two-sided but too thin to carry a medial-axis stroke;
        // the whole-component sweep must still make progress instead of
        // stalling into an error.
        let left = ContourSet::rectangle(rect(0.0, 0.0, 5.0, 6.0), tol::REGION_MM);
        let right = ContourSet::rectangle(rect(5.003, 0.0, 10.0, 6.0), tol::REGION_MM);
        let region = left.union(&right);
        assert!(!region.disk_gap_violations(0.5).is_empty());

        let regularization = region.disk_regularize_gaps(0.5, 0.5, 0.025).unwrap();

        assert!(regularization.kept.disk_gap_violations(0.5).is_empty());
        assert!(regularization.removed.area() > 0.0);
    }

    #[test]
    fn disk_gap_violations_exclude_isolated_void_corners() {
        let outer = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), tol::REGION_MM);
        let wide_hole = ContourSet::rectangle(rect(3.0, 3.0, 7.0, 7.0), tol::REGION_MM);
        let region = outer.difference(&wide_hole);
        let raw_closing_residual = region.disk_close(0.5).difference(&region);

        assert!(raw_closing_residual.area() > 0.1);
        assert!(region.disk_gap_violations(0.5).is_empty());
    }

    #[test]
    fn disk_gap_regularization_rejects_invalid_scales() {
        let region = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), tol::REGION_MM);

        assert!(region.disk_regularize_gaps(0.0, 0.5, 0.025).is_err());
        assert!(region.disk_regularize_gaps(0.5, f64::NAN, 0.025).is_err());
        assert!(region.disk_regularize_gaps(0.5, 0.5, -0.025).is_err());
    }

    #[test]
    fn disk_gap_regularization_widens_a_gap_thinner_than_the_guard() {
        let left = ContourSet::rectangle(rect(0.0, 0.0, 4.0, 10.0), tol::REGION_MM);
        let right = ContourSet::rectangle(rect(4.01, 0.0, 10.0, 10.0), tol::REGION_MM);
        let region = left.union(&right);

        let before = region.disk_gap_violations(0.5);
        let result = region.disk_regularize_gaps(0.5, 0.5, 0.025).unwrap();

        assert!(before.area() > 0.05);
        assert!(before.disk_open(0.025).is_empty());
        assert!(result.removed.area() > 0.0);
        assert!(result.kept.disk_gap_violations(0.5).is_empty());
    }

    #[test]
    fn disk_gap_regularization_trims_a_close_pair_locally() {
        let left = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), tol::REGION_MM);
        let right = ContourSet::rectangle(rect(10.8, 0.0, 20.8, 10.0), tol::REGION_MM);
        let distant = ContourSet::rectangle(rect(24.0, 0.0, 30.0, 10.0), tol::REGION_MM);
        let region = left.union(&right).union(&distant);

        let result = region.disk_regularize_gaps(0.5, 0.5, 0.025).unwrap();

        assert_eq!(result.kept.connected_components().len(), 3);
        assert!(result.kept.difference(&region).is_empty());
        assert!(
            result.removed.area() < 4.0,
            "removed {:.9} mm²",
            result.removed.area()
        );
        assert!(result.removed.intersection(&distant).area() <= 0.25);
        assert!(result.kept.disk_gap_violations(0.5).is_empty());
    }

    #[test]
    fn disk_gap_regularization_is_symmetric_at_a_three_way_conflict() {
        let lower_left = ContourSet::rectangle(rect(0.0, 0.0, 4.0, 4.0), tol::REGION_MM);
        let lower_right = ContourSet::rectangle(rect(4.8, 0.0, 8.8, 4.0), tol::REGION_MM);
        let upper = ContourSet::rectangle(rect(2.4, 4.8, 6.4, 8.8), tol::REGION_MM);
        let region = lower_left.union(&lower_right).union(&upper);

        let result = region.disk_regularize_gaps(0.5, 0.5, 0.025).unwrap();

        assert_eq!(result.kept.connected_components().len(), 3);
        for source in [&lower_left, &lower_right, &upper] {
            assert!(result.kept.intersection(source).area() > 13.0);
        }
        let violations = result.kept.disk_gap_violations(0.5);
        assert!(
            violations.is_empty(),
            "remaining void-gap violation area {:.9} mm²",
            violations.area(),
        );
    }

    #[test]
    fn disk_gap_regularization_widens_a_same_component_hairpin() {
        let outer = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), tol::REGION_MM);
        let narrow_notch = ContourSet::rectangle(rect(4.6, 3.0, 5.4, 10.0), tol::REGION_MM);
        let hairpin = outer.difference(&narrow_notch);

        let before = hairpin.disk_gap_violations(0.5);
        let result = hairpin.disk_regularize_gaps(0.5, 0.5, 0.025).unwrap();
        let after = result.kept.disk_gap_violations(0.5);

        assert_eq!(hairpin.connected_components().len(), 1);
        assert!(before.area() > 1.0);
        assert!(result.removed.area() > 1.0);
        assert!(
            after.is_empty(),
            "remaining hairpin gap {:.9} mm²",
            after.area()
        );
    }

    #[test]
    fn disk_gap_regularization_widens_a_narrow_internal_void() {
        let outer = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), tol::REGION_MM);
        let narrow_hole = ContourSet::rectangle(rect(4.6, 3.0, 5.4, 7.0), tol::REGION_MM);
        let region = outer.difference(&narrow_hole);

        let before = region.disk_gap_violations(0.5);
        let result = region.disk_regularize_gaps(0.5, 0.5, 0.025).unwrap();
        let after = result.kept.disk_gap_violations(0.5);

        assert!(before.area() > 3.0);
        assert!(result.removed.area() > 1.0);
        assert!(result.kept.contains_point(Point::new(2.0, 5.0)));
        assert!(
            after.is_empty(),
            "remaining internal-void gap {:.9} mm²",
            after.area(),
        );
    }

    #[test]
    fn dilation_shrinks_but_preserves_a_large_hole() {
        let outer = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), tol::REGION_MM);
        let hole = ContourSet::rectangle(rect(3.0, 3.0, 7.0, 7.0), tol::REGION_MM);

        let dilated = outer.difference(&hole).disk_dilate(0.5);
        let expected_hole = ContourSet::rectangle(rect(3.5, 3.5, 6.5, 6.5), tol::REGION_MM);

        assert!(dilated.intersection(&expected_hole).is_empty());
        let area = dilated.area();
        assert!(
            (area - 111.785398163).abs() <= 2e-2,
            "unexpected dilated area {area}"
        );
    }

    #[test]
    fn union_contains_both_regions_when_a_hole_overlaps_filled_material() {
        let outer = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), tol::REGION_MM);
        let hole = ContourSet::rectangle(rect(3.0, 3.0, 7.0, 7.0), tol::REGION_MM);
        let frame = outer.difference(&hole);
        let plug = ContourSet::rectangle(rect(4.0, 4.0, 6.0, 6.0), tol::REGION_MM);

        let union = frame.union(&plug);

        assert!(frame.difference(&union).is_empty());
        assert!(plug.difference(&union).is_empty());
        assert!((union.area() - 88.0).abs() <= 1e-6);
    }

    /// Reduced chain of narrow complement pockets from the ControlHub A5
    /// rounded-board/V-score corner. A positive offset must contain every
    /// source point even when all of these holes collapse.
    #[test]
    fn dilation_is_monotone_for_a5_corner_hole_chain() {
        let outer =
            ContourSet::rectangle(rect(22.8473, 110.5175, 27.8973, 115.5675), tol::REGION_MM);
        let holes = ContourSet::from_filled_contours(
            &[
                contour_from_vertices(&[
                    [22.947299957275405, 111.0119310617447],
                    [22.947299957275405, 111.0245145559311],
                    [22.95425641536714, 111.06290817260744],
                ]),
                contour_from_vertices(&[
                    [23.024561643600478, 111.42717397212984],
                    [23.034708142280593, 111.47974538803102],
                    [23.044173359870925, 111.51395440101625],
                ]),
                contour_from_vertices(&[
                    [23.121961593627944, 111.79509580135347],
                    [23.14569699764253, 111.88088023662569],
                    [23.17426168918611, 111.95898854732515],
                ]),
                contour_from_vertices(&[
                    [23.255928039550795, 112.18230032920839],
                    [23.288409471511855, 112.27111899852754],
                    [23.340039849281325, 112.38374710083009],
                ]),
                contour_from_vertices(&[
                    [23.42500483989717, 112.56909239292146],
                    [23.46326601505281, 112.65255665779115],
                    [23.54809403419496, 112.80427730083467],
                ]),
                contour_from_vertices(&[
                    [23.626791238784804, 112.94503259658815],
                    [23.66619455814363, 113.01550805568696],
                    [23.791095137596145, 113.20204389095308],
                ]),
                contour_from_vertices(&[
                    [23.864429831504836, 113.31156766414644],
                    [23.89930665493013, 113.36365520954134],
                    [24.080685615539565, 113.59284710884096],
                ]),
                contour_from_vertices(&[
                    [24.131775259971633, 113.65740430355073],
                    [24.158974766731276, 113.69177389144899],
                    [24.444096326828017, 113.99821996688844],
                    [24.75268936157228, 114.28101646900178],
                    [25.08276188373567, 114.53819620609285],
                    [25.115122795104995, 114.55951189994813],
                    [24.76722896099092, 114.29204106330873],
                    [24.431027054786696, 113.98377299308778],
                ]),
                contour_from_vertices(&[
                    [25.20675671100618, 114.61986958980562],
                    [25.432661533355727, 114.76866948604585],
                    [25.4813165664673, 114.795392036438],
                ]),
                contour_from_vertices(&[
                    [25.624097824096694, 114.87381088733675],
                    [25.797136902809157, 114.96884799003602],
                    [25.857525467872634, 114.99598038196565],
                ]),
                contour_from_vertices(&[
                    [26.056700825691237, 115.08546924591066],
                    [26.179885625839248, 115.1408157348633],
                    [26.24467504024507, 115.16395568847658],
                ]),
                contour_from_vertices(&[
                    [26.486665248870864, 115.25038421154024],
                    [26.5711922645569, 115.2805736064911],
                    [26.634812474250808, 115.29765975475313],
                ]),
                contour_from_vertices(&[
                    [26.93655109405519, 115.37869584560396],
                    [26.973154783248916, 115.38852632045747],
                    [27.011330246925368, 115.39559543132783],
                ]),
                contour_from_vertices(&[
                    [27.390587329864516, 115.46582400798799],
                    [27.396905422210708, 115.46750009059907],
                    [27.402869582176223, 115.46750009059907],
                ]),
            ],
            tol::REGION_MM,
        );
        let source = outer.difference(&holes);

        let dilated = source.disk_dilate(0.525);
        let removed_source = source.difference(&dilated);

        assert!(
            removed_source.is_empty(),
            "dilation removed {:.9} mm² from its source",
            removed_source.area()
        );
    }

    #[test]
    fn painted_path_region_unions_fills_and_native_strokes() {
        let mut arena = PathArena::default();
        let filled = arena.push_path(
            Paint::Fill {
                rule: FillRule::EvenOdd,
            },
            [rectangle_contour(0.0, 0.0, 1.0, 1.0)],
        );
        let stroked = arena.push_path(
            Paint::Stroke(crate::geom::StrokeStyle::round(1.0)),
            [ContourBuf::new(vec![
                PathCmd::move_to(Point::new(2.0, 0.5)),
                PathCmd::line_to(Point::new(4.0, 0.5)),
            ])],
        );
        let unpainted = arena.push_path(Paint::None, [rectangle_contour(10.0, 10.0, 20.0, 20.0)]);

        let region = ContourSet::from_painted_paths(
            &arena,
            [filled, stroked, unpainted]
                .iter()
                .map(|&index| arena.path(index)),
            tol::REGION_MM,
        );

        assert!((region.bbox.min.x - 0.0).abs() <= 1e-9);
        assert!((region.bbox.min.y - 0.0).abs() <= 1e-9);
        assert!((region.bbox.max.x - 4.5).abs() <= 1e-9);
        assert!((region.bbox.max.y - 1.0).abs() <= 1e-9);
        assert!(region.area() > 3.5);
        assert!(region.area() < 4.0);
    }

    fn rect(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> BBox {
        BBox::new(Point::new(min_x, min_y), Point::new(max_x, max_y))
    }

    fn rectangle_contour(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> ContourBuf {
        ContourBuf::new(vec![
            PathCmd::move_to(Point::new(min_x, min_y)),
            PathCmd::line_to(Point::new(max_x, min_y)),
            PathCmd::line_to(Point::new(max_x, max_y)),
            PathCmd::line_to(Point::new(min_x, max_y)),
            PathCmd::close(),
        ])
    }

    fn contour_from_vertices(vertices: &[[f64; 2]]) -> ContourBuf {
        let mut cmds = Vec::with_capacity(vertices.len() + 1);
        for (index, &[x, y]) in vertices.iter().enumerate() {
            let point = Point::new(x, y);
            cmds.push(if index == 0 {
                PathCmd::move_to(point)
            } else {
                PathCmd::line_to(point)
            });
        }
        cmds.push(PathCmd::close());
        ContourBuf::new(cmds)
    }

    /// Regression: V-score relief tool-center region from a real board whose
    /// boolean output carried sub-micrometer float-debris segments. Dilation
    /// must handle it without panicking or losing the region.
    #[test]
    fn dilates_boolean_debris_with_submicron_segments() {
        let contour = contour_from_vertices(&[
            [38.0, 160.0],
            [38.0, 156.894598],
            [38.0171578, 156.764663],
            [38.0503974, 156.684556],
            [38.0504384, 156.684832],
            [38.1270673, 157.070071],
            [38.1389899, 157.117669],
            [38.2530098, 157.493541],
            [38.2695398, 157.539739],
            [38.419852, 157.902626],
            [38.4408314, 157.946984],
            [38.6259894, 158.29339],
            [38.6512156, 158.335477],
            [38.8694365, 158.662066],
            [38.8986657, 158.701478],
            [39.1478457, 159.005105],
            [39.1807983, 159.041462],
            [39.4585402, 159.319203],
            [39.4948957, 159.352154],
            [39.7985227, 159.601335],
            [39.8379354, 159.630565],
            [40.1645255, 159.848785],
            [40.2066116, 159.874011],
            [40.5530176, 160.059169],
            [40.5973749, 160.080148],
            [40.9602618, 160.23046],
            [41.0064602, 160.24699],
            [41.3823323, 160.36101],
            [41.4299297, 160.372933],
            [41.8151686, 160.449562],
            [41.8154452, 160.449603],
            [41.735338, 160.482842],
            [41.6054032, 160.5],
            [38.5, 160.5],
            [38.3675704, 160.482272],
            [38.2503393, 160.433321],
            [38.1464467, 160.353553],
            [38.0666795, 160.249661],
            [38.0177281, 160.13243],
        ]);
        let region = ContourSet::from_filled_contours(&[contour], tol::REGION_MM);

        let grown = region.disk_dilate(0.5);

        assert!(grown.area() > region.area());
    }

    /// Regression: minimal boundary fragment from a real board that crashed
    /// an arc-preserving offset library's slice stitching when grown by the
    /// route-tool radius.
    #[test]
    fn dilates_relief_boundary_fragment() {
        let contour = contour_from_vertices(&[
            [31.901232957840, 63.057707951027],
            [31.859204053879, 63.115636036354],
            [31.806460976601, 63.248603985268],
            [31.793315052986, 63.391045973259],
            [32.526947975159, 63.811510965782],
            [32.643206000328, 63.728166029411],
            [32.689244031906, 63.673370048958],
            [33.861821055412, 62.191123053986],
        ]);
        let region = ContourSet::from_filled_contours(&[contour], tol::REGION_MM);

        let grown = region.disk_dilate(0.5);

        assert!(grown.area() > region.area());
    }
}
