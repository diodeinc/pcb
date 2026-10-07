//! Polygon regularization, fixed-grid simplification, and inward decimation.

use super::{ContourSet, Ring, Shape, edges_of, flatten_shapes, overlay_fill_rule, rings_bbox};
use crate::geom::accuracy::{lattice_per_mm, numerical_error};
use crate::geom::dist;
use crate::geom::{AccuracyError, BBox, FillRule, Point, tol};
use i_overlay::core::fill_rule::FillRule as OverlayFillRule;
use i_overlay::core::overlay::{IntOverlayOptions, Overlay};
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::core::simplify::Simplify;
use i_overlay::i_float::int::point::IntPoint;
use i_overlay::i_shape::int::shape::IntShapes;

/// Regularize rings under the given fill rule into non-overlapping shapes.
pub(crate) fn simplify_rings(rings: Vec<Ring>, fill_rule: FillRule) -> Vec<Ring> {
    flatten_shapes(simplify_shapes(rings, fill_rule))
}

/// Regularize rings keeping the connected-shape structure: each shape is its
/// outer ring followed by its holes, wound opposite.
///
/// Rings whose bounds never touch cannot nest or cross under any fill rule,
/// so each group of rings connected through their bounds is regularized on
/// its own. The overlay's split and sort stages then scale with the group,
/// not the layer, which matters for silkscreen and mask images made of
/// thousands of small, locally overlapping features.
pub fn simplify_shapes(rings: Vec<Ring>, fill_rule: FillRule) -> Vec<Shape> {
    regularize(rings, overlay_fill_rule(fill_rule))
}

fn regularize(rings: Vec<Ring>, rule: OverlayFillRule) -> Vec<Shape> {
    bounds_connected_groups(rings.into_iter().map(|ring| (ring, ())).collect())
        .into_iter()
        .flat_map(|group| overlay(&untagged(group), &[], OverlayRule::Subject, rule))
        .collect()
}

pub(super) fn overlay(
    subject: &[Ring],
    clip: &[Ring],
    rule: OverlayRule,
    fill_rule: OverlayFillRule,
) -> Vec<Shape> {
    let bbox = rings_bbox(subject).union(rings_bbox(clip));
    let scale = lattice_per_mm(bbox);
    let vertices = VertexColumns::new(&[subject, clip], bbox);
    let lattice = |rings: &[Ring]| {
        rings
            .iter()
            .map(|ring| {
                let mut lattice = Vec::with_capacity(ring.len());
                vertices.split(ring, |[x, y]| {
                    lattice.push(IntPoint::new(
                        (x * scale).round() as i64,
                        (y * scale).round() as i64,
                    ));
                });
                lattice
            })
            .collect::<Vec<_>>()
    };
    Overlay::with_contours(&lattice(subject), &lattice(clip))
        .overlay(rule, fill_rule)
        .into_iter()
        .map(|shape| {
            shape
                .into_iter()
                .map(|ring| {
                    ring.into_iter()
                        .map(|point| [point.x as f64 / scale, point.y as f64 / scale])
                        .collect()
                })
                .collect()
        })
        .collect()
}

/// The vertices of one overlay, indexed to find those lying on an edge.
///
/// A source tool cuts one fill along another's edge and rounds the ends of
/// the cut to its grid, so the fills' shared stretch is two edges a fraction
/// of a grid step apart and an exact overlay keeps the hairline between
/// them. Splitting every edge at the vertices that lie on it makes the
/// stretch one edge again, which the overlay cancels.
struct VertexColumns {
    /// Vertices by column, ascending in `y` within each.
    points: Vec<[f64; 2]>,
    /// Offsets into `points` per column; two longer than the column count.
    columns: Vec<u32>,
    origin: f64,
    pitch: f64,
}

impl VertexColumns {
    fn new(rings: &[&[Ring]], bbox: BBox) -> Self {
        let rings = || rings.iter().flat_map(|rings| rings.iter());
        let count = rings().map(Vec::len).sum::<usize>();
        // Columns about one edge wide: an edge then reads a column or two
        // and finds a vertex or two there, however the edges are spread.
        let extent = rings()
            .flat_map(|ring| edges_of(ring))
            .map(|(start, end)| (end.x - start.x).abs() + (end.y - start.y).abs())
            .sum::<f64>()
            .max(bbox.width());
        let (origin, pitch) = (bbox.min.x, (extent / count as f64).max(ON_EDGE_MM));
        let column = |x: f64| ((x - origin) / pitch) as usize;
        // Counted two slots ahead, a column's offset is first where its
        // vertices go next and then, once they are placed, where it starts.
        let mut columns = vec![0u32; column(bbox.max.x) + 3];
        for [x, _] in rings().flatten() {
            columns[column(*x) + 2] += 1;
        }
        for column in 1..columns.len() {
            columns[column] += columns[column - 1];
        }
        let mut points = vec![[0.0; 2]; count];
        for point in rings().flatten() {
            let slot = &mut columns[column(point[0]) + 1];
            points[*slot as usize] = *point;
            *slot += 1;
        }
        // Coincident vertices are one vertex, so stacked copies of a shape
        // cost no more to search than one.
        let mut kept = 0;
        for column in 0..columns.len() - 1 {
            let (start, end) = (columns[column] as usize, columns[column + 1] as usize);
            points[start..end].sort_unstable_by(|left, right| {
                left[1]
                    .total_cmp(&right[1])
                    .then(left[0].total_cmp(&right[0]))
            });
            columns[column] = kept as u32;
            for index in start..end {
                if kept == columns[column] as usize || points[index] != points[kept - 1] {
                    points[kept] = points[index];
                    kept += 1;
                }
            }
        }
        *columns.last_mut().unwrap() = kept as u32;
        points.truncate(kept);
        Self {
            points,
            columns,
            origin,
            pitch,
        }
    }

    fn column(&self, x: f64) -> usize {
        ((x - self.origin) / self.pitch) as usize
    }

    /// Emit `ring` with every edge split at the vertices lying on it.
    fn split(&self, ring: &[[f64; 2]], mut emit: impl FnMut([f64; 2])) {
        let reach = ON_EDGE_MM;
        let last = self.columns.len() - 3;
        let mut on_edge = Vec::new();
        for (index, &[ax, ay]) in ring.iter().enumerate() {
            let [bx, by] = ring[(index + 1) % ring.len()];
            let (dx, dy) = (bx - ax, by - ay);
            let length2 = dx * dx + dy * dy;
            let (min_x, max_x) = (ax.min(bx) - reach, ax.max(bx) + reach);
            let (min_y, max_y) = (ay.min(by) - reach, ay.max(by) + reach);
            for column in self.column(min_x)..=self.column(max_x).min(last) {
                // The edge's span of `y` within this column alone, so a long
                // diagonal reads the vertices along it, not its bounding box.
                let (low, high) = if dx == 0.0 {
                    (min_y, max_y)
                } else {
                    let left = self.origin + column as f64 * self.pitch;
                    let y_at = |x: f64| ay + (x.clamp(min_x, max_x) - ax) * dy / dx;
                    let (y0, y1) = (y_at(left - reach), y_at(left + self.pitch + reach));
                    (
                        (y0.min(y1) - reach).clamp(min_y, max_y),
                        (y0.max(y1) + reach).clamp(min_y, max_y),
                    )
                };
                let points =
                    &self.points[self.columns[column] as usize..self.columns[column + 1] as usize];
                let first = points.partition_point(|point| point[1] < low);
                for &[x, y] in points[first..].iter().take_while(|point| point[1] <= high) {
                    // Strictly between the ends, which the edge already has.
                    let along = (x - ax) * dx + (y - ay) * dy;
                    let across = (x - ax) * dy - (y - ay) * dx;
                    if along > 0.0 && along < length2 && across * across <= reach * reach * length2
                    {
                        on_edge.push((along, [x, y]));
                    }
                }
            }
            on_edge.sort_by(|left, right| left.0.total_cmp(&right.0));
            emit([ax, ay]);
            on_edge.drain(..).for_each(|(_, point)| emit(point));
        }
    }
}

/// How far a vertex may sit from an edge it lies on: source tools round
/// the vertex and the edge's ends to their grid, each by up to half a cell
/// diagonal.
const ON_EDGE_MM: f64 = std::f64::consts::SQRT_2 * tol::SOURCE_GRID_MM;

/// Resolve tagged rings one bounds-connected group at a time.
///
/// Rings whose bounds never touch cannot interact under any set operation,
/// so `resolve` sees only rings that can, each tagged with the operand it
/// came from, and decides what that group contributes. An operation over a
/// panel then costs what its local neighbourhoods cost, however the caller
/// happened to batch it.
pub(super) fn resolve_groups<T>(
    rings: Vec<(Ring, T)>,
    resolve: impl FnMut(Vec<(Ring, T)>) -> Vec<Ring>,
) -> Vec<Ring> {
    bounds_connected_groups(rings)
        .into_iter()
        .flat_map(resolve)
        .collect()
}

pub(super) fn untagged<T>(group: Vec<(Ring, T)>) -> Vec<Ring> {
    group.into_iter().map(|(ring, _)| ring).collect()
}

/// Partition rings into groups connected by overlapping bounds, each in
/// input order and the groups ordered by their first ring, so callers that
/// number the resulting shapes see the same numbering for the same input.
///
/// Bounds carry the overlay's rounding allowance and its reach from an edge
/// to a vertex on it: every group snaps to its own integer grid, and groups
/// that stay apart by more than that cannot be joined. A sweep in `x` keeps
/// one hull per open group and merges a ring into every group whose hull it
/// meets. The hull stands in for its members, so it may merge groups no
/// member pair joins; that costs only partitioning benefit, never
/// correctness, and lets a layer of long features degenerate to the single
/// overlay it needed before.
fn bounds_connected_groups<T>(rings: Vec<(Ring, T)>) -> Vec<Vec<(Ring, T)>> {
    struct Group {
        hull: BBox,
        root: usize,
    }
    let bounds = rings
        .iter()
        .map(|(ring, _)| rings_bbox(std::slice::from_ref(ring)))
        .collect::<Vec<_>>();
    let slack =
        numerical_error(bounds.iter().copied().fold(BBox::empty(), BBox::union)) + ON_EDGE_MM;
    let mut order = bounds
        .into_iter()
        .map(|bbox| bbox.expand(slack))
        .enumerate()
        .collect::<Vec<_>>();
    order.sort_by(|(_, a), (_, b)| a.min.x.total_cmp(&b.min.x));
    // Membership is a forest over ring indices: a group absorbed by a later
    // ring hangs its root under that ring, so a chain of touching rings
    // merges in constant time per link instead of recopying its members.
    let mut parent = (0..rings.len()).collect::<Vec<_>>();
    let mut open: Vec<Group> = Vec::new();
    for (index, bbox) in order {
        let mut hull = bbox;
        let mut i = 0;
        while i < open.len() {
            // A sweep line crossing a band of many separate features would
            // compare every ring against all of them; the surplus folds into
            // this group instead, bounding the work per ring.
            if open[i].hull.max.x < bbox.min.x {
                open.swap_remove(i);
            } else if open[i].hull.intersects(bbox) || open.len() > MAX_OPEN_GROUPS {
                let group = open.swap_remove(i);
                hull = hull.union(group.hull);
                parent[group.root] = index;
            } else {
                i += 1;
            }
        }
        open.push(Group { hull, root: index });
    }
    let mut group_of_root = vec![usize::MAX; rings.len()];
    let mut groups: Vec<Vec<(Ring, T)>> = Vec::new();
    for (index, ring) in rings.into_iter().enumerate() {
        let mut root = index;
        while parent[root] != root {
            parent[root] = parent[parent[root]];
            root = parent[root];
        }
        if group_of_root[root] == usize::MAX {
            group_of_root[root] = groups.len();
            groups.push(Vec::new());
        }
        groups[group_of_root[root]].push(ring);
    }
    groups
}

/// Open groups a sweep line compares each ring against before folding them.
const MAX_OPEN_GROUPS: usize = 256;

/// Integer coordinate on a fixed grid, using translation-invariant half-up
/// rounding. Ties are decided on a 1/1024 sub-grid to absorb floating-point
/// noise. Callers must supply a positive finite grid and coordinates within
/// the exact integer range (2^50 grid steps).
pub fn grid_coordinate(value: f64, grid: f64) -> i64 {
    // The sub-grid intermediate needs more range than the output coordinate.
    ((value / grid * 1024.0 + 0.5).floor() as i128 + 512).div_euclid(1024) as i64
}

/// Regularize filled rings on an exact output grid.
///
/// The fixed-scale integer overlay resolves crossings and removes coincident
/// vertices while snapping every result vertex to `grid`. Geometry that
/// collapses during coordinate quantization is not representable on that
/// output grid.
pub(super) fn integer_shapes_on_grid(
    rings: Vec<Ring>,
    fill_rule: FillRule,
    grid: f64,
    options: IntOverlayOptions<u128>,
) -> IntShapes<i64> {
    let rings = rings
        .into_iter()
        .map(|ring| {
            ring.into_iter()
                .map(|[x, y]| IntPoint::new(grid_coordinate(x, grid), grid_coordinate(y, grid)))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    rings
        .as_slice()
        .simplify(overlay_fill_rule(fill_rule), options)
}

/// Decimate rings so the region only shrinks: the result covers no point the
/// source did not, and no source vertex ends farther than `deviation_mm`
/// from the decimated boundary.
pub(crate) fn decimate_rings_inward<'a>(
    rings: impl Iterator<Item = &'a [[f64; 2]]>,
    deviation_mm: f64,
) -> Vec<Ring> {
    rings
        .map(|ring| decimate_ring_inward(ring, deviation_mm))
        .collect()
}

fn decimate_ring_inward(ring: &[[f64; 2]], deviation_mm: f64) -> Ring {
    if ring.len() < 4 {
        return ring.to_vec();
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
        let endpoint = point(end);
        let chord = endpoint - start;
        // A chord back to its own anchor has no side for a vertex to be on.
        if start == endpoint {
            return false;
        }
        (anchor + 1..end).all(|index| {
            let offset = point(index) - start;
            let cross = chord.x * offset.y - chord.y * offset.x;
            cross <= 0.0 && dist::point_segment(point(index), start, endpoint).0 <= deviation_mm
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
        return ring.to_vec();
    }
    kept
}

impl ContourSet {
    /// Decimate the region's boundary so it only shrinks; see
    /// `decimate_rings_inward`.
    ///
    /// A chord takes one turn of winding off what it cuts from its ring and
    /// changes nothing elsewhere, so the decimated rings wind no point more
    /// than the source did and positive winding keeps a subset of it. The
    /// nonzero rule would not: a hole left outside its ring by a chord winds
    /// negatively there and would fill.
    pub fn decimate_inward(&self) -> Result<Self, AccuracyError> {
        let inherited = self.uncertainty_mm + numerical_error(self.bbox);
        let deviation_mm = self.budget().allowance(inherited)?;
        let decimated = decimate_rings_inward(self.rings(), deviation_mm);
        Ok(Self::from_regularized(
            flatten_shapes(regularize(decimated, OverlayFillRule::Positive)),
            self.resolution,
            inherited + deviation_mm,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::super::ring_edges;
    use super::super::tests::res;
    use super::*;
    use crate::geom::{GeometryAccuracy, Resolution, shapes, tol};

    #[test]
    fn inward_decimation_only_shrinks_and_respects_deviation() {
        let spur = vec![[0.0, 0.0], [2.0, 0.0], [1.0, 1e-12], [1.0, 1.0], [0.0, 1.0]];
        assert!(decimate_ring_inward(&spur, 1e-10).contains(&[2.0, 0.0]));
        let ring = ContourSet::from_contours(
            &[shapes::circle(10.0).unwrap(), shapes::circle(6.0).unwrap()],
            FillRule::EvenOdd,
            res(tol::REGION_MM),
        )
        .unwrap();
        let decimated = ring.decimate_inward().unwrap();
        let deviation = decimated.uncertainty_mm - ring.uncertainty_mm;
        assert!(deviation > 0.0);

        assert!(decimated.difference(&ring).unwrap().area() <= ring.tolerance().powi(2));

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
    fn inward_decimation_does_not_fill_a_hole_inside_the_bulge_it_cuts() {
        // The chord across the shallow bulge passes above a hole within it,
        // which leaves the hole outside its ring, wound the wrong way.
        let outer = vec![
            [0.0, 0.0],
            [5.0, -0.01],
            [10.0, 0.0],
            [10.0, 10.0],
            [0.0, 10.0],
        ];
        let hole = vec![[4.0, -0.002], [6.0, -0.002], [5.0, -0.008]];
        let resolution = Resolution::new(tol::REGION_MM, GeometryAccuracy::new(0.05).unwrap());
        let region = ContourSet::from_regularized(vec![outer, hole], resolution, 0.0);
        assert!((region.area() - 100.044).abs() < 1e-9);

        let decimated = region.decimate_inward().unwrap();

        assert_eq!(decimated.rings.len(), 1);
        assert!(decimated.difference(&region).unwrap().is_empty());
    }

    #[test]
    fn inward_decimation_bounds_distance_to_segment_not_line() {
        // The spike is 0.01 mm from the chord's line, but 2 mm past its end.
        let ring: Ring = vec![
            [0.0, 0.0],
            [12.0, -0.01],
            [10.0, 0.0],
            [10.0, 10.0],
            [0.0, 10.0],
        ];
        let deviation = 0.05;
        let decimated = decimate_ring_inward(&ring, deviation);

        for [x, y] in ring {
            let distance = ring_edges(&decimated)
                .map(|(a, b)| dist::point_segment(Point::new(x, y), a, b).0)
                .fold(f64::INFINITY, f64::min);
            assert!(
                distance <= deviation,
                "vertex ({x}, {y}) is {distance} mm away"
            );
        }
    }
}
