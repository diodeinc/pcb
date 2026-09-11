//! Polygon regularization, fixed-grid simplification, and inward decimation.

use super::{ContourSet, Ring, Shape, flatten_shapes, overlay_fill_rule, rings_bbox};
use crate::geom::accuracy::numerical_error;
use crate::geom::dist;
use crate::geom::{AccuracyError, BBox, FillRule, Point};
use i_overlay::core::overlay::IntOverlayOptions;
use i_overlay::core::simplify::Simplify;
use i_overlay::float::simplify::SimplifyShape;
use i_overlay::i_float::int::point::IntPoint;

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
    let rule = overlay_fill_rule(fill_rule);
    bounds_connected_groups(rings)
        .into_iter()
        .flat_map(|group| group.simplify_shape_as::<i64>(rule))
        .collect()
}

/// Partition rings into groups connected by overlapping bounds.
///
/// Bounds carry the overlay's rounding allowance: every group snaps to its
/// own integer grid, and groups that stay apart by more than the allowance
/// cannot be snapped together. A sweep in `x` keeps one hull per open group
/// and merges a ring into every group whose hull it meets. The hull stands
/// in for its members, so it may merge groups no member pair joins; that
/// costs only partitioning benefit, never correctness, and lets a layer of
/// long features degenerate to the single overlay it needed before.
fn bounds_connected_groups(rings: Vec<Ring>) -> Vec<Vec<Ring>> {
    struct Group {
        hull: BBox,
        members: Vec<usize>,
    }
    let slack = numerical_error(rings_bbox(&rings));
    let mut order = rings
        .iter()
        .map(|ring| rings_bbox(std::slice::from_ref(ring)).expand(slack))
        .enumerate()
        .collect::<Vec<_>>();
    order.sort_by(|(_, a), (_, b)| a.min.x.total_cmp(&b.min.x));
    let mut open: Vec<Group> = Vec::new();
    let mut closed: Vec<Group> = Vec::new();
    for (index, bbox) in order {
        let mut merged = Group {
            hull: bbox,
            members: vec![index],
        };
        let mut i = 0;
        while i < open.len() {
            // A sweep line crossing a band of many separate features would
            // compare every ring against all of them; the surplus folds into
            // this group instead, bounding the work per ring.
            if open[i].hull.max.x < bbox.min.x {
                closed.push(open.swap_remove(i));
            } else if open[i].hull.intersects(bbox) || open.len() > MAX_OPEN_GROUPS {
                let group = open.swap_remove(i);
                merged.hull = merged.hull.union(group.hull);
                merged.members.extend(group.members);
            } else {
                i += 1;
            }
        }
        open.push(merged);
    }
    closed.extend(open);
    let mut rings = rings.into_iter().map(Some).collect::<Vec<_>>();
    closed
        .into_iter()
        .map(|group| {
            group
                .members
                .into_iter()
                .filter_map(|index| rings[index].take())
                .collect()
        })
        .collect()
}

/// Open groups a sweep line compares each ring against before folding them.
const MAX_OPEN_GROUPS: usize = 256;

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

/// Decimate rings so the region only shrinks: the result covers no point the
/// source did not, and no source vertex ends farther than `deviation_mm`
/// from the decimated boundary.
pub(crate) fn decimate_rings_inward(rings: &[Ring], deviation_mm: f64) -> Vec<Ring> {
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
        let endpoint = point(end);
        let chord = endpoint - start;
        let length = chord.length();
        if length <= f64::EPSILON {
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
        return ring.clone();
    }
    kept
}

impl ContourSet {
    /// Decimate the region's boundary so it only shrinks; see
    /// [`decimate_rings_inward`].
    pub fn decimate_inward(&self) -> Result<Self, AccuracyError> {
        let inherited = self.uncertainty_mm + numerical_error(self.bbox);
        let deviation_mm = self.budget().allowance(inherited)?;
        Ok(Self::from_regularized(
            simplify_rings(
                decimate_rings_inward(&self.rings, deviation_mm),
                FillRule::NonZero,
            ),
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
    use crate::geom::{shapes, tol};

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
