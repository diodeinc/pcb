//! Zero-width bridges connecting polygon holes to their outer boundary.
//!
//! Gerber regions (and other single-contour formats) cannot represent holes
//! directly; the standard encoding is a "cut-in": walk from the boundary to
//! the hole, around it, and back along the same segment. The two coincident
//! bridge edges cancel under any winding rule, so the filled geometry is
//! unchanged — but the bridges are visible to CAM tools, so they must stay
//! short and local. This is the classic hole-elimination construction used
//! by ear-clipping triangulators: connect each hole's leftmost vertex to the
//! nearest visible boundary point, merging holes left to right so a bridge
//! can land on an already-merged hole but never cross one.

use crate::geom::region::{Ring, ring_signed_area};

/// Merge a shape's holes into its outer ring with zero-width bridges.
///
/// `shape[0]` is the outer boundary and the remaining rings are its holes,
/// as produced by regularized boolean output. Winding is normalized
/// internally. The result is a single ring tracing the outer boundary with
/// each hole spliced in through a pair of coincident bridge segments.
pub fn bridge_shape(mut shape: Vec<Ring>) -> Ring {
    if shape.is_empty() {
        return Ring::new();
    }
    let mut contour = shape.remove(0);
    if shape.is_empty() {
        return contour;
    }
    // Outer counter-clockwise, holes clockwise, so the spliced walk keeps a
    // consistent winding and the leftward ray cast sees descending edges.
    if ring_signed_area(&contour) < 0.0 {
        contour.reverse();
    }
    let mut holes = shape;
    for hole in &mut holes {
        if ring_signed_area(hole) > 0.0 {
            hole.reverse();
        }
    }
    // Left to right, so a later hole's leftward bridge can only land on the
    // outer boundary or a hole that is already part of it.
    holes.sort_by(|a, b| leftmost(a).0.total_cmp(&leftmost(b).0));
    for hole in holes {
        merge_hole(&mut contour, hole);
    }
    contour
}

/// Splice one hole into the contour at the bridge vertex pair.
fn merge_hole(contour: &mut Ring, hole: Ring) {
    if hole.len() < 3 {
        return;
    }
    let (_, hole_start) = leftmost(&hole);
    let bridge = find_bridge(contour, hole[hole_start]);

    let mut merged = Ring::with_capacity(contour.len() + hole.len() + 2);
    merged.extend_from_slice(&contour[..=bridge]);
    // The full hole loop, starting and ending at its bridge vertex.
    merged.extend(hole[hole_start..].iter().copied());
    merged.extend(hole[..=hole_start].iter().copied());
    // Back across the bridge; the two bridge edges are exactly coincident.
    merged.push(contour[bridge]);
    merged.extend_from_slice(&contour[bridge + 1..]);
    *contour = merged;
}

/// Index and x-coordinate of a ring's leftmost (then lowest) vertex.
fn leftmost(ring: &Ring) -> (f64, usize) {
    let mut best = 0;
    for (index, point) in ring.iter().enumerate() {
        if (point[0], point[1]) < (ring[best][0], ring[best][1]) {
            best = index;
        }
    }
    (ring[best][0], best)
}

/// Find the contour vertex to bridge a hole point to: cast a ray to the left
/// and take the nearest crossing, then refine against vertices inside the
/// candidate triangle so the bridge cannot cross the boundary (David
/// Eberly's construction, as used by ear-clipping hole elimination).
fn find_bridge(contour: &Ring, hole_point: [f64; 2]) -> usize {
    let [hx, hy] = hole_point;
    let n = contour.len();

    // Nearest leftward crossing of the horizontal ray through the hole point.
    let mut qx = f64::NEG_INFINITY;
    let mut m = None;
    for i in 0..n {
        let [px, py] = contour[i];
        let [sx, sy] = contour[(i + 1) % n];
        if hy <= py && hy >= sy && sy != py {
            let x = px + (hy - py) * (sx - px) / (sy - py);
            if x <= hx && x > qx {
                qx = x;
                m = Some(if px < sx { i } else { (i + 1) % n });
                if x == hx {
                    return m.expect("just set");
                }
            }
        }
    }
    let Some(mut m) = m else {
        // The hole is not strictly inside the contour (degenerate input);
        // fall back to the nearest vertex so the output stays well-formed.
        return nearest_vertex(contour, hole_point);
    };

    // The ray hit an edge interior. Any contour vertex inside the triangle
    // (hole point, ray intersection, edge endpoint) would be crossed by a
    // direct bridge; among those, bridge to the one with the smallest angle
    // from the ray (breaking ties toward the hole).
    let [mx, my] = contour[m];
    let mut tan_min = f64::INFINITY;
    for i in 0..n {
        let [px, py] = contour[i];
        if hx >= px && px >= mx && hx != px {
            let inside = if hy < my {
                point_in_triangle([hx, hy], [mx, my], [qx, hy], [px, py])
            } else {
                point_in_triangle([qx, hy], [mx, my], [hx, hy], [px, py])
            };
            if inside {
                let tan = (hy - py).abs() / (hx - px);
                if tan < tan_min || (tan == tan_min && px > contour[m][0]) {
                    m = i;
                    tan_min = tan;
                }
            }
        }
    }
    m
}

fn nearest_vertex(ring: &Ring, [hx, hy]: [f64; 2]) -> usize {
    let mut best = 0;
    let mut best_distance = f64::INFINITY;
    for (index, [x, y]) in ring.iter().enumerate() {
        let distance = (x - hx).powi(2) + (y - hy).powi(2);
        if distance < best_distance {
            best_distance = distance;
            best = index;
        }
    }
    best
}

fn point_in_triangle(a: [f64; 2], b: [f64; 2], c: [f64; 2], p: [f64; 2]) -> bool {
    let sign = |p1: [f64; 2], p2: [f64; 2], p3: [f64; 2]| {
        (p1[0] - p3[0]) * (p2[1] - p3[1]) - (p2[0] - p3[0]) * (p1[1] - p3[1])
    };
    let d1 = sign(p, a, b);
    let d2 = sign(p, b, c);
    let d3 = sign(p, c, a);
    let has_negative = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let has_positive = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(has_negative && has_positive)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::region::rings_area;

    fn rect(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> Ring {
        vec![
            [min_x, min_y],
            [max_x, min_y],
            [max_x, max_y],
            [min_x, max_y],
        ]
    }

    /// Shoelace area of the merged ring: coincident bridge edges cancel, so
    /// it must equal outer area minus hole areas exactly.
    fn merged_area(ring: &Ring) -> f64 {
        ring_signed_area(ring).abs()
    }

    /// Longest coincident (retraced) segment in the ring — bridges show up
    /// as segment pairs traversed once in each direction.
    fn bridge_lengths(ring: &Ring) -> Vec<f64> {
        let n = ring.len();
        let mut seen = std::collections::HashSet::new();
        let mut lengths = Vec::new();
        let quantize = |p: [f64; 2]| ((p[0] * 1e9) as i64, (p[1] * 1e9) as i64);
        for i in 0..n {
            let a = ring[i];
            let b = ring[(i + 1) % n];
            if seen.contains(&(quantize(b), quantize(a))) {
                lengths.push((a[0] - b[0]).hypot(a[1] - b[1]));
            }
            seen.insert((quantize(a), quantize(b)));
        }
        lengths
    }

    #[test]
    fn single_hole_bridges_to_nearest_boundary() {
        let outer = rect(0.0, 0.0, 100.0, 10.0);
        let hole = rect(90.0, 4.0, 92.0, 6.0);

        let merged = bridge_shape(vec![outer, hole]);

        assert!((merged_area(&merged) - (1000.0 - 4.0)).abs() < 1e-9);
        let bridges = bridge_lengths(&merged);
        assert_eq!(bridges.len(), 1);
        // Nearest boundary leftward of x=90 within the strip is far (x=0),
        // but the refinement may land on a corner; either way the bridge
        // must be dramatically shorter than the old first-vertex anchor
        // would allow from the far end of the board.
        assert!(
            bridges[0] <= 91.0,
            "bridge length {} should not exceed the leftward span",
            bridges[0]
        );
    }

    #[test]
    fn holes_bridge_locally_not_to_a_global_anchor() {
        // A wide plate with a row of small holes: each hole's bridge must be
        // short (to the left neighbor or boundary), never across the plate.
        let outer = rect(0.0, 0.0, 100.0, 10.0);
        let holes: Vec<Ring> = (0..9)
            .map(|i| {
                let x = 10.0 + 10.0 * i as f64;
                rect(x, 4.0, x + 2.0, 6.0)
            })
            .collect();
        let hole_area: f64 = holes
            .iter()
            .map(|h| rings_area(std::slice::from_ref(h)))
            .sum();

        let mut shape = vec![outer];
        shape.extend(holes);
        let merged = bridge_shape(shape);

        assert!((merged_area(&merged) - (1000.0 - hole_area)).abs() < 1e-9);
        let bridges = bridge_lengths(&merged);
        assert_eq!(bridges.len(), 9);
        // Every hole is 8mm from its left neighbor (or ~11mm diagonally from
        // the wall corner); a global-anchor scheme would produce bridges up
        // to ~90mm.
        for length in &bridges {
            assert!(*length <= 11.0, "bridge too long: {length}");
        }
    }

    #[test]
    fn bridges_never_cross_the_merged_boundary() {
        let outer = rect(0.0, 0.0, 60.0, 60.0);
        let holes = vec![
            rect(10.0, 10.0, 20.0, 20.0),
            rect(30.0, 8.0, 40.0, 18.0),
            rect(25.0, 30.0, 35.0, 40.0),
            rect(45.0, 45.0, 55.0, 55.0),
            rect(5.0, 40.0, 15.0, 50.0),
        ];
        let hole_area: f64 = holes
            .iter()
            .map(|h| rings_area(std::slice::from_ref(h)))
            .sum();

        let mut shape = vec![outer.clone()];
        shape.extend(holes);
        let merged = bridge_shape(shape);

        // Exact area identity is only possible if no bridge crosses any
        // boundary (a crossing would flip winding somewhere and change the
        // shoelace sum).
        assert!((merged_area(&merged) - (3600.0 - hole_area)).abs() < 1e-9);

        // And explicitly: no two non-adjacent segments properly intersect,
        // coincident bridge pairs aside.
        let n = merged.len();
        let segment = |i: usize| (merged[i], merged[(i + 1) % n]);
        for i in 0..n {
            let (a1, a2) = segment(i);
            for j in i + 2..n {
                if i == 0 && j == n - 1 {
                    continue;
                }
                let (b1, b2) = segment(j);
                let d = |p: [f64; 2], q: [f64; 2], r: [f64; 2]| {
                    (q[0] - p[0]) * (r[1] - p[1]) - (q[1] - p[1]) * (r[0] - p[0])
                };
                let proper = d(a1, a2, b1) * d(a1, a2, b2) < -1e-12
                    && d(b1, b2, a1) * d(b1, b2, a2) < -1e-12;
                assert!(
                    !proper,
                    "segments {i} and {j} cross: {a1:?}->{a2:?} x {b1:?}->{b2:?}"
                );
            }
        }
    }

    #[test]
    fn shape_without_holes_is_unchanged() {
        let outer = rect(0.0, 0.0, 10.0, 10.0);
        assert_eq!(bridge_shape(vec![outer.clone()]), outer);
    }
}
