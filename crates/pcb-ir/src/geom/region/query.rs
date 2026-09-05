use super::{ContourSet, horizontal_crossing, ring_edges, ring_signed_area};
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
    /// use pcb_ir::geom::{BBox, ContourSet, Point};
    ///
    /// let region = ContourSet::rectangle(
    ///     BBox::new(Point::ZERO, Point::new(4.0, 2.0)), 0.001,
    /// );
    /// let prepared = region.prepare_query();
    /// let inside = prepared.signed_distance(Point::new(2.0, 0.5)).unwrap();
    /// assert_eq!(inside.mm, -0.5);
    /// assert_eq!(inside.second, Point::new(2.0, 0.0));
    /// ```
    pub fn prepare_query(&self) -> PreparedRegion {
        PreparedRegion::from_segments(
            self.rings
                .iter()
                .filter(|ring| ring_signed_area(ring) != 0.0)
                .flat_map(ring_edges)
                .collect(),
            self.uncertainty_mm,
        )
    }
}

impl PreparedRegion {
    pub(super) fn from_segments(segments: Vec<(Point, Point)>, uncertainty_mm: f64) -> Self {
        let mut prepared = Self {
            order: (0..segments.len()).collect(),
            segments,
            nodes: Vec::new(),
            uncertainty_mm,
        };
        if !prepared.segments.is_empty() {
            prepared.build(0..prepared.segments.len());
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
    /// Uncertainty is inherited from the source geometry.
    /// The source geometry's sign is uncertain when this band includes zero.
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
        self.visit(0, &mut query);
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

    /// Boundary segments whose bounds meet `bounds`, once each in source order.
    pub fn segments_meeting(&self, bounds: BBox) -> impl Iterator<Item = (Point, Point)> + '_ {
        self.segment_ids_meeting(bounds)
            .into_iter()
            .map(|id| self.segments[id])
    }

    pub(crate) fn segment_ids_near(&self, start: Point, end: Point, reach: f64) -> Vec<usize> {
        let query = BBox::spanning(start, end);
        let delta = end - start;
        self.segment_ids_where(&|bounds| {
            let bounds = bounds.expand(reach + tol::EPSILON_MM);
            let offset = bounds.center() - start;
            // The segment normal is the separating axis beyond x and y.
            query.intersects(bounds)
                && (delta.x * offset.y - delta.y * offset.x).abs()
                    <= (delta.x.abs() * bounds.height() + delta.y.abs() * bounds.width()) * 0.5
        })
    }

    pub(crate) fn segment_ids_meeting(&self, bounds: BBox) -> Vec<usize> {
        self.segment_ids_where(&|candidate| candidate.intersects(bounds))
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

    fn build(&mut self, range: Range<usize>) -> usize {
        let bounds = self.order[range.clone()]
            .iter()
            .fold(BBox::empty(), |bounds, &id| {
                let (start, end) = self.segments[id];
                bounds.union(BBox::spanning(start, end))
            });
        let node = self.nodes.len();
        self.nodes.push(Node {
            bounds,
            kind: NodeKind::Leaf(range.clone()),
        });
        if range.len() > LEAF_SEGMENTS {
            let middle = range.start + range.len() / 2;
            let center = |&id: &usize| {
                let (start, end) = self.segments[id];
                let midpoint = start.midpoint(end);
                if bounds.width() >= bounds.height() {
                    midpoint.x
                } else {
                    midpoint.y
                }
            };
            self.order[range.clone()].select_nth_unstable_by(range.len() / 2, |left, right| {
                center(left).total_cmp(&center(right))
            });
            let left = self.build(range.start..middle);
            let right = self.build(middle..range.end);
            self.nodes[node].kind = NodeKind::Branch(left, right);
        }
        node
    }

    fn visit(&self, index: usize, query: &mut Query) {
        let node = &self.nodes[index];
        let measure = node.bounds.distance_to(BBox::from_point(query.point)) <= query.distance;
        // Half-open height matches horizontal_crossing at shared vertices.
        let wind = query.needs_winding
            && node.bounds.min.y <= query.point.y
            && query.point.y < node.bounds.max.y
            && node.bounds.min.x <= query.point.x;
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
                    if wind
                        && let Some((x, direction)) = horizontal_crossing(start, end, query.point.y)
                        && x <= query.point.x
                    {
                        query.winding += direction;
                    }
                }
            }
            NodeKind::Branch(left, right) => {
                let bounds = BBox::from_point(query.point);
                let left_distance = self.nodes[*left].bounds.distance_to(bounds);
                let right_distance = self.nodes[*right].bounds.distance_to(bounds);
                let (first, second) = if left_distance <= right_distance {
                    (*left, *right)
                } else {
                    (*right, *left)
                };
                self.visit(first, query);
                self.visit(second, query);
            }
        }
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
