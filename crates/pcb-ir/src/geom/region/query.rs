use super::{ContourSet, horizontal_crossing, ring_edges, ring_signed_area};
use crate::geom::dist::{self, Distance};
use crate::geom::{BBox, Point, tol};
use std::ops::Range;

/// An owned [`ContourSet`] snapshot with a bounding-box hierarchy for nearest
/// boundary and winding queries. Preparation takes O(n log n) time and O(n)
/// space; queries allocate no scratch storage and take O(n) in the worst case.
/// Coordinates and distances use the source region's frame, in millimeters.
#[derive(Debug, Clone)]
pub struct PreparedRegion {
    segments: Vec<(Point, Point)>,
    nodes: Vec<Node>,
    uncertainty_mm: f64,
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
        let mut prepared = PreparedRegion {
            segments: self
                .rings
                .iter()
                .filter(|ring| ring_signed_area(ring) != 0.0)
                .flat_map(ring_edges)
                .collect(),
            nodes: Vec::new(),
            uncertainty_mm: tol::FLATTEN_MM,
        };
        if !prepared.segments.is_empty() {
            prepared.build(0..prepared.segments.len());
        }
        prepared
    }
}

impl PreparedRegion {
    /// Euclidean distance to the polygon boundary: negative inside,
    /// zero on the boundary, positive outside (including inside a hole).
    ///
    /// `first` is the query point; `second` is a closest boundary point.
    /// Ties may return any closest witness. Returns
    /// `None` when the region has no filled boundary or the point is non-finite.
    ///
    /// Uses floating-point arithmetic without snapping to the region tolerance.
    /// Like [`Distance::flattened`], uncertainty counts one flattened boundary;
    /// it does not bound prior approximations, offsets, or discarded features.
    /// The source geometry's sign is uncertain when this band includes zero.
    pub fn signed_distance(&self, point: Point) -> Option<Distance> {
        let root = self.nodes.first()?;
        if !point.is_finite() {
            return None;
        }
        let mut query = Query {
            point,
            distance: f64::INFINITY,
            boundary: Point::ZERO,
            winding: 0,
            needs_winding: root.bounds.contains_point(point),
        };
        self.visit(0, &mut query);
        Some(Distance {
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

    fn build(&mut self, range: Range<usize>) -> usize {
        let bounds = self.segments[range.clone()]
            .iter()
            .fold(BBox::empty(), |bounds, &(start, end)| {
                bounds.union(BBox::spanning(start, end))
            });
        let node = self.nodes.len();
        self.nodes.push(Node {
            bounds,
            kind: NodeKind::Leaf(range.clone()),
        });
        if range.len() > LEAF_SEGMENTS {
            let middle = range.start + range.len() / 2;
            let center = |&(start, end): &(Point, Point)| {
                let midpoint = start.midpoint(end);
                if bounds.width() >= bounds.height() {
                    midpoint.x
                } else {
                    midpoint.y
                }
            };
            self.segments[range.clone()].select_nth_unstable_by(range.len() / 2, |left, right| {
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
                for &(start, end) in &self.segments[range.clone()] {
                    if measure {
                        let (distance, boundary) = dist::point_segment(query.point, start, end);
                        if distance < query.distance {
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
    boundary: Point,
    winding: i32,
    needs_winding: bool,
}
