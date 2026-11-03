use super::{ContourSet, edges_of, horizontal_crossing, signed_area_of};
use crate::geom::accuracy::numerical_error;
use crate::geom::dist::{self, Distance};
use crate::geom::{BBox, Point, tol};
use std::ops::Range;

/// An owned [`ContourSet`] snapshot with a bounding-box hierarchy for nearest
/// boundary and winding queries. Preparation takes O(n log n) time and O(n)
/// space; point queries allocate no scratch storage and take O(n) in the worst case.
/// Coordinates and distances use the source region's frame, in millimeters.
#[derive(Debug, Clone)]
pub struct PreparedRegion {
    pub(crate) segments: Vec<(Point, Point)>,
    order: Vec<usize>,
    nodes: Vec<Node>,
    /// Closed source rings, absent for an arbitrary segment collection.
    ring_bounds: Vec<BBox>,
    segment_rings: Vec<u32>,
    winding_bounds: Vec<BBox>,
    pub(crate) uncertainty_mm: f64,
}

#[derive(Debug, Clone)]
struct Node {
    bounds: BBox,
    kind: NodeKind,
}

#[derive(Debug, Clone)]
enum NodeKind {
    Leaf(Range<usize>),
    Branch(usize, usize),
}

const LEAF_SEGMENTS: usize = 8;

impl ContourSet {
    /// Prepare a reusable index over the region's existing polygon boundary.
    ///
    /// Requires finite, regularized rings. Zero-area rings are ignored.
    ///
    /// ```
    /// use pcb_ir::geom::{BBox, ContourSet, Point, Resolution};
    ///
    /// let region = ContourSet::rectangle(
    ///     BBox::new(Point::ZERO, Point::new(4.0, 2.0)),
    ///     Resolution::default(),
    /// );
    /// let prepared = region.prepare_query();
    /// let inside = prepared.signed_distance(Point::new(2.0, 0.5)).unwrap();
    /// assert_eq!(inside.mm, -0.5);
    /// assert_eq!(inside.second, Point::new(2.0, 0.0));
    /// ```
    pub fn prepare_query(&self) -> PreparedRegion {
        // A single ring cannot benefit from per-ring pruning within its BVH.
        let multiple = self.ring_count() >= 2;
        let count = self.rings().map(<[_]>::len).sum();
        let mut segments = Vec::with_capacity(count);
        let mut segment_rings = Vec::with_capacity(if multiple { count } else { 0 });
        let mut ring_bounds = Vec::new();
        let guard = if multiple {
            numerical_error(self.bbox)
        } else {
            0.0
        };
        for (ring, bounds) in self.bounded_rings() {
            if signed_area_of(ring) == 0.0 {
                continue;
            }
            segments.extend(edges_of(ring));
            if multiple {
                let id = ring_bounds.len() as u32;
                ring_bounds.push(bounds.expand(guard));
                segment_rings.extend(std::iter::repeat_n(id, ring.len()));
            }
        }
        let mut prepared = PreparedRegion::from_segments(segments, self.uncertainty_mm);
        if !multiple {
            return prepared;
        }
        prepared.segment_rings = segment_rings;
        prepared.ring_bounds = ring_bounds;
        // A whole closed ring winds zero outside its bounds. Keep the union
        // of source-ring bounds at each node so winding can skip distant
        // islands and holes, not walk every edge to the left of the point.
        // Nodes are in preorder, so their children are ready in reverse.
        prepared.winding_bounds = vec![BBox::empty(); prepared.nodes.len()];
        for id in (0..prepared.nodes.len()).rev() {
            let bounds = match &prepared.nodes[id].kind {
                NodeKind::Leaf(range) => prepared.order[range.clone()]
                    .iter()
                    .map(|&edge| prepared.ring_bounds[prepared.segment_rings[edge] as usize])
                    .fold(BBox::empty(), BBox::union),
                NodeKind::Branch(left, right) => {
                    prepared.winding_bounds[*left].union(prepared.winding_bounds[*right])
                }
            };
            prepared.winding_bounds[id] = bounds;
        }
        prepared
    }
}

impl PreparedRegion {
    /// Join disjoint closed regions without rebuilding their boundary trees.
    /// Segment order follows the regions, independently of tree traversal.
    pub(super) fn joined(regions: &[&Self], uncertainty_mm: f64) -> Self {
        let mut result = Self {
            segments: Vec::new(),
            order: Vec::new(),
            nodes: Vec::new(),
            ring_bounds: Vec::new(),
            segment_rings: Vec::new(),
            winding_bounds: Vec::new(),
            uncertainty_mm,
        };
        let mut sources = Vec::new();
        for &region in regions {
            if region.nodes.is_empty() {
                continue;
            }
            let segment_offset = result.segments.len();
            let order_offset = result.order.len();
            let ring_offset = result.ring_bounds.len() as u32;
            result.segments.extend_from_slice(&region.segments);
            result
                .order
                .extend(region.order.iter().map(|id| id + segment_offset));
            if region.ring_bounds.is_empty() {
                result.ring_bounds.push(region.bounds());
                result
                    .segment_rings
                    .extend(std::iter::repeat_n(ring_offset, region.segments.len()));
            } else {
                result.ring_bounds.extend_from_slice(&region.ring_bounds);
                result
                    .segment_rings
                    .extend(region.segment_rings.iter().map(|id| id + ring_offset));
            }
            sources.push((region, order_offset));
        }
        fn append(result: &mut PreparedRegion, sources: &mut [(&PreparedRegion, usize)]) -> usize {
            let root = result.nodes.len();
            if let [(source, order_offset)] = sources {
                result.nodes.extend(source.nodes.iter().map(|node| Node {
                    bounds: node.bounds,
                    kind: match &node.kind {
                        NodeKind::Leaf(range) => {
                            NodeKind::Leaf(range.start + *order_offset..range.end + *order_offset)
                        }
                        NodeKind::Branch(a, b) => NodeKind::Branch(a + root, b + root),
                    },
                }));
                if source.winding_bounds.is_empty() {
                    result
                        .winding_bounds
                        .extend(std::iter::repeat_n(source.bounds(), source.nodes.len()));
                } else {
                    result
                        .winding_bounds
                        .extend_from_slice(&source.winding_bounds);
                }
            } else {
                let bounds = sources.iter().fold(BBox::empty(), |bounds, (source, _)| {
                    bounds.union(source.bounds())
                });
                result.nodes.push(Node {
                    bounds,
                    kind: NodeKind::Leaf(0..0),
                });
                result.winding_bounds.push(bounds);
                let horizontal = bounds.width() >= bounds.height();
                let coordinate = |source: &PreparedRegion| {
                    let center = source.bounds().center();
                    if horizontal { center.x } else { center.y }
                };
                let middle = sources.len() / 2;
                sources.select_nth_unstable_by(middle, |(a, _), (b, _)| {
                    coordinate(a).total_cmp(&coordinate(b))
                });
                let (left, right) = sources.split_at_mut(middle);
                let left = append(result, left);
                let right = append(result, right);
                result.nodes[root].kind = NodeKind::Branch(left, right);
            }
            root
        }
        if !sources.is_empty() {
            append(&mut result, &mut sources);
        }
        result
    }

    pub(super) fn from_segments(segments: Vec<(Point, Point)>, uncertainty_mm: f64) -> Self {
        // Estimate a balanced tree's size; uneven spatial splits can grow it.
        // Reserve just one node for the common tiny boundary.
        let nodes = if segments.is_empty() {
            0
        } else {
            2 * segments.len().div_ceil(LEAF_SEGMENTS).next_power_of_two() - 1
        };
        let mut prepared = Self {
            order: (0..segments.len()).collect(),
            segments,
            nodes: Vec::with_capacity(nodes),
            ring_bounds: Vec::new(),
            segment_rings: Vec::new(),
            winding_bounds: Vec::new(),
            uncertainty_mm,
        };
        if !prepared.segments.is_empty() {
            let (bounds, midpoints): (Vec<_>, Vec<_>) = prepared
                .segments
                .iter()
                .map(|&(start, end)| (BBox::spanning(start, end), start.midpoint(end)))
                .unzip();
            prepared.build(0..prepared.segments.len(), &bounds, &midpoints);
        }
        prepared
    }

    /// Euclidean distance to the polygon boundary: negative inside,
    /// zero on the boundary, positive outside (including inside a hole).
    ///
    /// `first` is the query point; `second` is a closest boundary point.
    /// Ties may return any closest witness. Returns
    /// `None` when the region has no filled boundary or the point is non-finite.
    ///
    /// Uses floating-point arithmetic without snapping to the region tolerance.
    /// The distance is to the prepared boundary, every point of which lies
    /// within `uncertainty_mm` of a source boundary (see
    /// the accuracy module); the source geometry's sign is uncertain when
    /// the band includes zero. A prepared boundary may carry seams the exact
    /// composition would not, so the distance is a lower bound on the distance
    /// to the exact composed set, never an upper bound.
    pub fn signed_distance(&self, point: Point) -> Option<Distance> {
        self.point_distance(point, f64::INFINITY, true)
    }

    /// Nearest boundary point within `max_distance_mm`, with unsigned distance.
    pub fn nearest_within(&self, point: Point, max_distance_mm: f64) -> Option<Distance> {
        self.point_distance(point, max_distance_mm + tol::EPSILON_MM, false)
    }

    fn point_distance(&self, point: Point, maximum: f64, signed: bool) -> Option<Distance> {
        let root = self.nodes.first()?;
        if !point.is_finite() {
            return None;
        }
        let mut query = Query {
            point,
            distance: maximum,
            found: false,
            boundary: Point::ZERO,
            winding: 0,
            needs_winding: signed && root.bounds.contains_point(point),
        };
        self.visit(
            0,
            &mut query,
            root.bounds.distance_to(BBox::from_point(point)),
        );
        query.found.then_some(Distance {
            mm: if query.winding == 0 {
                query.distance
            } else {
                -query.distance
            },
            uncertainty_mm: self.uncertainty_mm,
            first: point,
            second: query.boundary,
        })
    }

    /// Apply [`Self::signed_distance`] in input order, reusing the index.
    pub fn signed_distances(&self, points: &[Point]) -> Vec<Option<Distance>> {
        points
            .iter()
            .map(|&point| self.signed_distance(point))
            .collect()
    }

    /// Nearest points between the query segment and boundary within the limit.
    pub fn segment_nearest_within(
        &self,
        start: Point,
        end: Point,
        max_distance_mm: f64,
    ) -> Option<Distance> {
        self.segment_ids_near(start, end, max_distance_mm)
            .into_iter()
            .map(|id| {
                let (a, b) = self.segments[id];
                let (mm, first, second) = dist::segments(start, end, a, b);
                Distance {
                    mm,
                    first,
                    second,
                    uncertainty_mm: self.uncertainty_mm,
                }
            })
            .filter(|distance| distance.mm <= max_distance_mm + tol::EPSILON_MM)
            .min_by(|left, right| left.mm.total_cmp(&right.mm))
    }

    /// Whether some boundary segment lies within `distance_mm` of the query
    /// segment, without the slack [`Self::segment_nearest_within`] allows.
    pub(crate) fn has_segment_within(&self, start: Point, end: Point, distance_mm: f64) -> bool {
        self.find_segment(segment_bounds_near(start, end, distance_mm), |id| {
            let (a, b) = self.segments[id];
            dist::segments(start, end, a, b).0 <= distance_mm
        })
        .is_some()
    }

    pub(super) fn has_boundary_within(&self, other: &Self, reach: f64) -> bool {
        !self.nodes.is_empty()
            && !other.nodes.is_empty()
            && self.boundaries_near_at(other, 0, 0, reach)
    }

    fn boundaries_near_at(&self, other: &Self, a: usize, b: usize, reach: f64) -> bool {
        let (left, right) = (&self.nodes[a], &other.nodes[b]);
        if !left.bounds.expand(reach).intersects(right.bounds)
            && !right.bounds.expand(reach).intersects(left.bounds)
        {
            return false;
        }
        match (&left.kind, &right.kind) {
            (NodeKind::Leaf(left), NodeKind::Leaf(right)) => left.clone().any(|first| {
                let (a, b) = self.segments[self.order[first]];
                right.clone().any(|second| {
                    let (c, d) = other.segments[other.order[second]];
                    BBox::spanning(a, b)
                        .expand(reach)
                        .intersects(BBox::spanning(c, d))
                        && dist::segments(a, b, c, d).0 <= reach
                })
            }),
            (NodeKind::Branch(l, r), _)
                if matches!(right.kind, NodeKind::Leaf(_))
                    || left.bounds.width() * left.bounds.height()
                        >= right.bounds.width() * right.bounds.height() =>
            {
                self.boundaries_near_at(other, *l, b, reach)
                    || self.boundaries_near_at(other, *r, b, reach)
            }
            (_, NodeKind::Branch(l, r)) => {
                self.boundaries_near_at(other, a, *l, reach)
                    || self.boundaries_near_at(other, a, *r, reach)
            }
            _ => unreachable!(),
        }
    }

    /// Bounds of the prepared boundary.
    pub(crate) fn bounds(&self) -> BBox {
        self.nodes.first().map_or(BBox::empty(), |root| root.bounds)
    }

    /// Winding number of the boundary around `point`, with the half-open
    /// crossing rule every other winding count in this module uses.
    pub fn winding(&self, point: Point) -> i32 {
        let mut query = Query {
            point,
            distance: f64::NEG_INFINITY,
            found: false,
            boundary: Point::ZERO,
            winding: 0,
            needs_winding: true,
        };
        if !self.nodes.is_empty() {
            self.visit(0, &mut query, 0.0);
        }
        query.winding
    }

    /// Nonzero winding for a regularized region with counterclockwise outer
    /// rings and clockwise holes. Unlike `winding`, this requires that the
    /// indexed segments describe such a region, not arbitrary open linework.
    /// The last crossing to the left enters material exactly when it travels
    /// downwards. Contacts and numerically indistinguishable crossings retain
    /// the full winding sum. Boundary points use `winding`'s half-open rule.
    pub fn contains_regular_point(&self, point: Point) -> bool {
        let Some(root) = self.nodes.first() else {
            return false;
        };
        if !point.is_finite() || !root.bounds.contains_point(point) {
            return false;
        }
        struct Crossing {
            x: f64,
            direction: i32,
            count: usize,
        }
        fn visit(
            region: &PreparedRegion,
            index: usize,
            point: Point,
            guard: f64,
            crossing: &mut Crossing,
        ) {
            let node = &region.nodes[index];
            if point.y < node.bounds.min.y
                || point.y >= node.bounds.max.y
                || node.bounds.min.x > point.x
                || node.bounds.max.x < crossing.x - guard
            {
                return;
            }
            match &node.kind {
                NodeKind::Leaf(range) => {
                    for &id in &region.order[range.clone()] {
                        let (a, b) = region.segments[id];
                        let Some((x, direction)) = horizontal_crossing(a, b, point.y) else {
                            continue;
                        };
                        if x > point.x || x < crossing.x - guard {
                            continue;
                        }
                        if x > crossing.x + guard {
                            crossing.count = 0;
                        }
                        crossing.count += 1;
                        if x > crossing.x {
                            crossing.x = x;
                            crossing.direction = direction;
                        }
                    }
                }
                NodeKind::Branch(left, right) => {
                    let (first, second) = if region.nodes[*left].bounds.max.x.min(point.x)
                        >= region.nodes[*right].bounds.max.x.min(point.x)
                    {
                        (*left, *right)
                    } else {
                        (*right, *left)
                    };
                    visit(region, first, point, guard, crossing);
                    visit(region, second, point, guard, crossing);
                }
            }
        }
        let mut crossing = Crossing {
            x: f64::NEG_INFINITY,
            direction: 0,
            count: 0,
        };
        visit(self, 0, point, numerical_error(root.bounds), &mut crossing);
        match crossing.count {
            0 => false,
            1 => crossing.direction < 0,
            _ => self.winding(point) != 0,
        }
    }

    /// Boundary segments whose bounds meet `bounds`, once each in source order.
    pub fn segments_meeting(&self, bounds: BBox) -> impl Iterator<Item = (Point, Point)> + '_ {
        self.segment_ids_meeting(bounds)
            .into_iter()
            .map(|id| self.segments[id])
    }

    pub(crate) fn segment_ids_near(&self, start: Point, end: Point, reach: f64) -> Vec<usize> {
        self.segment_ids_where(&segment_bounds_near(start, end, reach))
    }

    pub(crate) fn segment_ids_meeting(&self, bounds: BBox) -> Vec<usize> {
        self.segment_ids_where(&|candidate| candidate.intersects(bounds))
    }

    /// Mark segments participating in an accepted local pair. Walk node pairs
    /// once instead of querying the root independently for every segment.
    pub(super) fn paired_segments(
        &self,
        reach: f64,
        accept: impl Fn(usize, usize) -> bool,
    ) -> Vec<bool> {
        struct Search<'a, F> {
            index: &'a PreparedRegion,
            parents: Vec<usize>,
            leaves: Vec<usize>,
            remaining: Vec<usize>,
            marked: Vec<bool>,
            reach: f64,
            accept: F,
        }
        impl<F: Fn(usize, usize) -> bool> Search<'_, F> {
            fn mark(&mut self, segment: usize) {
                if std::mem::replace(&mut self.marked[segment], true) {
                    return;
                }
                let mut node = self.leaves[segment];
                loop {
                    self.remaining[node] -= 1;
                    if node == 0 {
                        break;
                    }
                    node = self.parents[node];
                }
            }

            fn visit(&mut self, a: usize, b: usize) {
                if self.remaining[a] == 0 && self.remaining[b] == 0 {
                    return;
                }
                let (left, right) = (&self.index.nodes[a], &self.index.nodes[b]);
                if !left.bounds.expand(self.reach).intersects(right.bounds)
                    && !right.bounds.expand(self.reach).intersects(left.bounds)
                {
                    return;
                }
                match (&left.kind, &right.kind) {
                    (NodeKind::Leaf(left), NodeKind::Leaf(right)) => {
                        for first in left.clone() {
                            let first = self.index.order[first];
                            for second in right.clone() {
                                let second = self.index.order[second];
                                if (a == b && second <= first)
                                    || (self.marked[first] && self.marked[second])
                                {
                                    continue;
                                }
                                if (self.accept)(first.min(second), first.max(second)) {
                                    self.mark(first);
                                    self.mark(second);
                                }
                            }
                        }
                    }
                    (NodeKind::Branch(l, r), _) if a == b => {
                        let (l, r) = (*l, *r);
                        self.visit(l, l);
                        self.visit(r, r);
                        self.visit(l, r);
                    }
                    (NodeKind::Branch(l, r), _)
                        if matches!(right.kind, NodeKind::Leaf(_))
                            || left.bounds.width() * left.bounds.height()
                                >= right.bounds.width() * right.bounds.height() =>
                    {
                        let (l, r) = (*l, *r);
                        self.visit(l, b);
                        self.visit(r, b);
                    }
                    (_, NodeKind::Branch(l, r)) => {
                        let (l, r) = (*l, *r);
                        self.visit(a, l);
                        self.visit(a, r);
                    }
                    _ => unreachable!(),
                }
            }
        }
        let mut search = Search {
            index: self,
            parents: vec![0; self.nodes.len()],
            leaves: vec![0; self.segments.len()],
            remaining: vec![0; self.nodes.len()],
            marked: vec![false; self.segments.len()],
            reach,
            accept,
        };
        for id in (0..self.nodes.len()).rev() {
            match &self.nodes[id].kind {
                NodeKind::Leaf(range) => {
                    search.remaining[id] = range.len();
                    for &segment in &self.order[range.clone()] {
                        search.leaves[segment] = id;
                    }
                }
                NodeKind::Branch(left, right) => {
                    search.parents[*left] = id;
                    search.parents[*right] = id;
                    search.remaining[id] = search.remaining[*left] + search.remaining[*right];
                }
            }
        }
        if !self.nodes.is_empty() {
            search.visit(0, 0);
        }
        search.marked
    }

    /// Find any accepted segment under a conservative bounds predicate.
    /// Unlike ordered collection, an existential query stops at its first hit.
    pub(crate) fn find_segment(
        &self,
        meets: impl Fn(BBox) -> bool,
        mut accept: impl FnMut(usize) -> bool,
    ) -> Option<usize> {
        (!self.nodes.is_empty())
            .then(|| self.find_segment_at(0, &meets, &mut accept))
            .flatten()
    }

    fn find_segment_at(
        &self,
        index: usize,
        meets: &impl Fn(BBox) -> bool,
        accept: &mut impl FnMut(usize) -> bool,
    ) -> Option<usize> {
        let node = &self.nodes[index];
        if !meets(node.bounds) {
            return None;
        }
        match &node.kind {
            NodeKind::Leaf(range) => self.order[range.clone()].iter().copied().find(|&id| {
                let (a, b) = self.segments[id];
                meets(BBox::spanning(a, b)) && accept(id)
            }),
            NodeKind::Branch(left, right) => self
                .find_segment_at(*left, meets, accept)
                .or_else(|| self.find_segment_at(*right, meets, accept)),
        }
    }

    fn segment_ids_where(&self, meets: &impl Fn(BBox) -> bool) -> Vec<usize> {
        let mut ids = Vec::new();
        if !self.nodes.is_empty() {
            self.collect_meeting(0, meets, &mut ids);
        }
        ids.sort_unstable();
        ids
    }

    fn collect_meeting(&self, index: usize, meets: &impl Fn(BBox) -> bool, ids: &mut Vec<usize>) {
        let node = &self.nodes[index];
        if !meets(node.bounds) {
            return;
        }
        match &node.kind {
            NodeKind::Leaf(range) => {
                ids.extend(self.order[range.clone()].iter().copied().filter(|&id| {
                    let (start, end) = self.segments[id];
                    meets(BBox::spanning(start, end))
                }))
            }
            NodeKind::Branch(left, right) => {
                self.collect_meeting(*left, meets, ids);
                self.collect_meeting(*right, meets, ids);
            }
        }
    }

    fn build(&mut self, range: Range<usize>, geometry: &[BBox], midpoints: &[Point]) -> usize {
        let bounds = self.order[range.clone()]
            .iter()
            .fold(BBox::empty(), |bounds, &id| bounds.union(geometry[id]));
        let node = self.nodes.len();
        self.nodes.push(Node {
            bounds,
            kind: NodeKind::Leaf(range.clone()),
        });
        if range.len() > LEAF_SEGMENTS {
            let centers =
                self.order[range.clone()]
                    .iter()
                    .fold(BBox::empty(), |mut centers, &id| {
                        centers.include_point(midpoints[id]);
                        centers
                    });
            let horizontal = centers.width() >= centers.height();
            let center = |&id: &usize| {
                let midpoint = midpoints[id];
                if horizontal { midpoint.x } else { midpoint.y }
            };
            let split = if horizontal {
                centers.min.x.midpoint(centers.max.x)
            } else {
                centers.min.y.midpoint(centers.max.y)
            };
            let mut middle = range.start;
            let mut end = range.end;
            while middle < end {
                if center(&self.order[middle]) < split {
                    middle += 1;
                } else {
                    end -= 1;
                    self.order.swap(middle, end);
                }
            }
            // Spatial splits separate clusters without median selection.
            // Bound depth even for overlapping or coincident segments.
            if middle - range.start < range.len() / 4 || range.end - middle < range.len() / 4 {
                middle = range.start + range.len() / 2;
                self.order[range.clone()].select_nth_unstable_by(range.len() / 2, |left, right| {
                    center(left).total_cmp(&center(right))
                });
            }
            let left = self.build(range.start..middle, geometry, midpoints);
            let right = self.build(middle..range.end, geometry, midpoints);
            self.nodes[node].kind = NodeKind::Branch(left, right);
        }
        node
    }

    fn visit(&self, index: usize, query: &mut Query, lower_bound: f64) {
        let node = &self.nodes[index];
        let measure = query.distance >= 0.0 && lower_bound <= query.distance;
        // Half-open height matches horizontal_crossing at shared vertices.
        let wind = query.needs_winding
            && node.bounds.min.y <= query.point.y
            && query.point.y < node.bounds.max.y
            && node.bounds.min.x <= query.point.x
            && (self.winding_bounds.is_empty()
                || self.winding_bounds[index].contains_point(query.point));
        if !measure && !wind {
            return;
        }
        match &node.kind {
            NodeKind::Leaf(range) => {
                for &id in &self.order[range.clone()] {
                    let (start, end) = self.segments[id];
                    if measure {
                        let (distance, boundary) = dist::point_segment(query.point, start, end);
                        if distance <= query.distance {
                            query.found = true;
                            query.distance = distance;
                            query.boundary = boundary;
                        }
                    }
                    // Skip every edge of an excluded ring, including in a
                    // node shared with a ring that does enclose the point:
                    // dropping only some of its crossings would change the sum.
                    if wind
                        && (self.segment_rings.is_empty()
                            || self.ring_bounds[self.segment_rings[id] as usize]
                                .contains_point(query.point))
                        && let Some((x, direction)) = horizontal_crossing(start, end, query.point.y)
                        && x <= query.point.x
                    {
                        query.winding += direction;
                    }
                }
            }
            NodeKind::Branch(left, right) if !measure => {
                // Only winding can matter below a node too far to measure.
                // Its sum is order independent; nearest-first traversal would
                // compute distances that this query does not need.
                self.visit(*left, query, lower_bound);
                self.visit(*right, query, lower_bound);
            }
            NodeKind::Branch(left, right) => {
                let distance = |bounds: BBox| {
                    let x = (query.point.x - bounds.max.x)
                        .max(bounds.min.x - query.point.x)
                        .max(0.0);
                    let y = (query.point.y - bounds.max.y)
                        .max(bounds.min.y - query.point.y)
                        .max(0.0);
                    let lower = x.max(y);
                    // A coordinate alone may already exclude this subtree.
                    // Only excluded children may change traversal order:
                    // every potential distance winner retains the exact norm.
                    if lower > query.distance {
                        lower
                    } else {
                        x.hypot(y)
                    }
                };
                let left_distance = distance(self.nodes[*left].bounds);
                let right_distance = distance(self.nodes[*right].bounds);
                let (first, second) = if left_distance <= right_distance {
                    ((*left, left_distance), (*right, right_distance))
                } else {
                    ((*right, right_distance), (*left, left_distance))
                };
                // Reuse these lower bounds after the nearer subtree
                // tightens the query. A pruned parent's bound also excludes
                // its children, even while winding still visits them.
                self.visit(first.0, query, first.1);
                self.visit(second.0, query, second.1);
            }
        }
    }
}

fn segment_bounds_near(start: Point, end: Point, reach: f64) -> impl Fn(BBox) -> bool {
    let query = BBox::spanning(start, end);
    let delta = end - start;
    move |bounds| {
        let bounds = bounds.expand(reach + tol::EPSILON_MM);
        let offset = bounds.center() - start;
        // The segment normal is the separating axis beyond x and y.
        query.intersects(bounds)
            && (delta.x * offset.y - delta.y * offset.x).abs()
                <= (delta.x.abs() * bounds.height() + delta.y.abs() * bounds.width()) * 0.5
    }
}

struct Query {
    point: Point,
    distance: f64,
    found: bool,
    boundary: Point,
    winding: i32,
    needs_winding: bool,
}
