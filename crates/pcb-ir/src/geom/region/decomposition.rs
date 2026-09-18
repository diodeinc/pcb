//! An additive, simple-polygon cover of a quantized filled set.

use std::collections::{HashMap, HashSet};

use anyhow::{Result, ensure};
use i_overlay::core::overlay::IntOverlayOptions;
use i_overlay::i_float::int::point::IntPoint;
use i_triangle::int::{triangulation::IntTriangulation, unchecked::IntUncheckedTriangulatable};

use super::{Ring, simplification::integer_shapes_on_grid};
use crate::geom::FillRule;

type Vertex = IntPoint<i64>;
type Boundary = HashMap<Vertex, Vertex>;

/// Quantize and regularize a filled set, then cover it with CCW simple polygons.
///
/// The polygons have disjoint interiors and at most `max_vertices` vertices.
/// Their union is exactly the integer overlay's filled set, including when
/// quantization closes a neck or creates touching holes. Decomposition only
/// removes internal mesh edges: it never nudges coordinates, inserts bridge
/// intersections, or emits negative/clear pieces. Separate polygons may touch.
///
/// This guarantees topology and coverage on the chosen grid, not preservation
/// of sub-grid features or robustness to a downstream consumer's coarser grid.
pub fn decompose_on_grid(
    rings: Vec<Ring>,
    fill_rule: FillRule,
    grid: f64,
    max_vertices: usize,
) -> Result<Vec<Ring>> {
    ensure!(grid.is_finite() && grid > 0.0, "invalid polygon grid");
    ensure!(
        max_vertices >= 3,
        "polygon vertex limit must be at least three"
    );
    // Keep integer coordinates exactly representable in f64, with ample room
    // for differences and cross products in i128.
    ensure!(
        rings.iter().flatten().flatten().all(|v| {
            let scaled = v / grid;
            scaled.is_finite() && scaled.abs() <= (1_u64 << 50) as f64
        }),
        "polygon coordinates exceed the supported grid range"
    );
    // Triangulation needs graph-generated contact vertices even when they are
    // collinear. This is the normal triangulation API's normalization policy;
    // do it once here, on the output grid and with the caller's fill rule.
    let shapes = integer_shapes_on_grid(
        rings,
        fill_rule,
        grid,
        IntOverlayOptions::keep_output_points(),
    );
    let mesh: IntTriangulation<i64, usize> = shapes.uncheck_triangulate().into_triangulation();
    let triangles: Vec<_> = mesh.triangles().collect();
    let mut coverage = Coverage::default();
    for ring in shapes.iter().flatten() {
        coverage.ring(ring, 1);
    }
    for triangle in &triangles {
        ensure!(
            cross(triangle[0], triangle[1], triangle[2]) > 0,
            "polygon triangulation produced a non-positive triangle"
        );
        coverage.ring(triangle, -1);
    }
    // Positive triangles with the same directed boundary have winding one
    // exactly on the source material. Exact edge pairs also preserve the
    // boundary subdivisions required by the conforming mesh coalescer.
    ensure!(
        coverage.0.is_empty(),
        "polygon triangulation changed the filled boundary"
    );

    Ok(coalesce(&triangles, max_vertices)
        .into_iter()
        .map(|ring| {
            ring.into_iter()
                .map(|p| [p.x as f64 * grid, p.y as f64 * grid])
                .collect()
        })
        .collect())
}

fn cross(a: Vertex, b: Vertex, c: Vertex) -> i128 {
    (b.x as i128 - a.x as i128) * (c.y as i128 - a.y as i128)
        - (b.y as i128 - a.y as i128) * (c.x as i128 - a.x as i128)
}

/// Signed endpoint-pair counts. Unlike area or supporting-line comparisons,
/// these reject losing a contact vertex in the middle of a boundary edge.
#[derive(Default)]
struct Coverage(HashMap<(Vertex, Vertex), i64>);

impl Coverage {
    fn ring(&mut self, ring: &[Vertex], sign: i64) {
        for (&a, &b) in ring.iter().zip(ring.iter().cycle().skip(1)) {
            let (key, delta) = if (a.x, a.y) < (b.x, b.y) {
                ((a, b), sign)
            } else {
                ((b, a), -sign)
            };
            let count = self.0.entry(key).or_default();
            *count += delta;
            if *count == 0 {
                self.0.remove(&key);
            }
        }
    }
}

struct Face {
    parent: usize,
    // Counts triangles, not boundary vertices: every union changes this value.
    weight: usize,
    boundary: Boundary,
}

fn root(faces: &mut [Face], mut index: usize) -> usize {
    while faces[index].parent != index {
        faces[index].parent = faces[faces[index].parent].parent;
        index = faces[index].parent;
    }
    index
}

fn coalesce(triangles: &[[Vertex; 3]], max_vertices: usize) -> Vec<Vec<Vertex>> {
    let mut faces = Vec::with_capacity(triangles.len());
    let mut owners = HashMap::new();
    let mut adjacency = Vec::new();
    for (index, triangle) in triangles.iter().enumerate() {
        let mut boundary = Boundary::with_capacity(3);
        for i in 0..3 {
            let (a, b) = (triangle[i], triangle[(i + 1) % 3]);
            boundary.insert(a, b);
            if let Some(other) = owners.remove(&(b, a)) {
                adjacency.push((other, index));
            } else {
                owners.insert((a, b), index);
            }
        }
        faces.push(Face {
            parent: index,
            weight: 1,
            boundary,
        });
    }
    // Original mesh edges give a deterministic merge order, independent of
    // hash iteration. Avoid rescanning a rejected face pair along every edge.
    let mut rejected = HashSet::new();
    for (a, b) in adjacency {
        let (mut a, mut b) = (root(&mut faces, a), root(&mut faces, b));
        if a == b {
            continue;
        }
        if (faces[a].boundary.len(), a) < (faces[b].boundary.len(), b) {
            std::mem::swap(&mut a, &mut b);
        }
        if !rejected.insert((a, b, faces[a].weight, faces[b].weight)) {
            continue;
        }
        let (large, small) = (&faces[a].boundary, &faces[b].boundary);
        let common = small.keys().filter(|p| large.contains_key(p)).count();
        let shared = small
            .iter()
            .filter(|(u, v)| large.get(v) == Some(u))
            .count();
        // Two simple faces of a planar mesh may join only along one boundary
        // path: k shared edges have k+1 shared vertices. Extra contacts or
        // disjoint shared paths would create a self-touch or close a hole.
        if shared == 0
            || common != shared + 1
            || large.len() + small.len() - 2 * shared > max_vertices
        {
            continue;
        }
        let small = std::mem::take(&mut faces[b].boundary);
        let mut additions = Vec::with_capacity(small.len() - shared);
        for (u, v) in small {
            if faces[a].boundary.get(&v) == Some(&u) {
                faces[a].boundary.remove(&v);
            } else {
                additions.push((u, v));
            }
        }
        faces[a].boundary.extend(additions);
        faces[b].parent = a;
        faces[a].weight += faces[b].weight;
    }
    faces
        .into_iter()
        .filter(|f| !f.boundary.is_empty())
        .map(|face| {
            let start = *face.boundary.keys().min_by_key(|p| (p.x, p.y)).unwrap();
            let mut ring = Vec::with_capacity(face.boundary.len());
            let mut point = start;
            loop {
                ring.push(point);
                point = face.boundary[&point];
                if point == start {
                    break;
                }
            }
            debug_assert_eq!(ring.len(), face.boundary.len());
            ring
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vertex([x, y]: [i64; 2]) -> Vertex {
        Vertex::new(x, y)
    }

    fn on_segment(a: Vertex, b: Vertex, p: Vertex) -> bool {
        cross(a, b, p) == 0
            && (a.x.min(b.x)..=a.x.max(b.x)).contains(&p.x)
            && (a.y.min(b.y)..=a.y.max(b.y)).contains(&p.y)
    }

    fn assert_simple(ring: &[Vertex]) {
        assert_eq!(ring.iter().collect::<HashSet<_>>().len(), ring.len());
        for i in 0..ring.len() {
            let (a, b) = (ring[i], ring[(i + 1) % ring.len()]);
            assert_ne!(a, b);
            for j in i + 1..ring.len() {
                if j == i + 1 || (i == 0 && j == ring.len() - 1) {
                    continue;
                }
                let (c, d) = (ring[j], ring[(j + 1) % ring.len()]);
                assert!(
                    !on_segment(a, b, c)
                        && !on_segment(a, b, d)
                        && !on_segment(c, d, a)
                        && !on_segment(c, d, b)
                );
                assert!(
                    !(cross(a, b, c).signum() * cross(a, b, d).signum() == -1
                        && cross(c, d, a).signum() * cross(c, d, b).signum() == -1)
                );
            }
        }
    }

    #[test]
    fn touching_boundaries_keep_material_without_cut_ins() {
        let outer = vec![[0., 0.], [20., 0.], [20., 15.], [0., 15.]];
        for (holes, expected_area) in [
            // Hole/hole contact, outer/hole contact, and a pinched hole.
            (
                vec![
                    vec![[2., 2.], [6., 2.], [6., 6.], [2., 6.]],
                    vec![[6., 6.], [9., 6.], [9., 10.], [6., 10.]],
                ],
                272.,
            ),
            (vec![vec![[0., 4.], [3., 2.], [3., 6.]]], 294.),
            (
                vec![vec![
                    [2., 2.],
                    [6., 2.],
                    [4.0004, 4.],
                    [6., 6.],
                    [2., 6.],
                    [3.9996, 4.],
                ]],
                292.,
            ),
        ] {
            let mut rings = vec![outer.clone()];
            rings.extend(holes);
            for reverse in [false, true] {
                let mut input = rings.clone();
                if reverse {
                    input.reverse();
                    for ring in &mut input {
                        ring.reverse();
                    }
                }
                for limit in [3, 5, 5000] {
                    let output =
                        decompose_on_grid(input.clone(), FillRule::EvenOdd, 0.001, limit).unwrap();
                    assert_eq!(
                        output,
                        decompose_on_grid(input.clone(), FillRule::EvenOdd, 0.001, limit).unwrap(),
                        "hash seeds must not change output"
                    );
                    let mut area2 = 0_i128;
                    for ring in output {
                        assert!(ring.len() <= limit);
                        let integer: Vec<_> = ring
                            .iter()
                            .map(|p| {
                                vertex([
                                    (p[0] * 1000.).round() as i64,
                                    (p[1] * 1000.).round() as i64,
                                ])
                            })
                            .collect();
                        assert_simple(&integer);
                        let signed: i128 = (1..integer.len() - 1)
                            .map(|i| cross(integer[0], integer[i], integer[i + 1]))
                            .sum();
                        assert!(signed > 0);
                        area2 += signed;
                    }
                    assert_eq!(area2, (expected_area * 2_000_000.) as i128);
                }
            }
        }
    }

    #[test]
    fn coverage_checks_directed_boundaries_not_just_area() {
        let square = [[0, 0], [8, 0], [8, 5], [0, 5]].map(vertex);
        let first = [square[0], square[1], square[2]];
        let second = [square[0], square[2], square[3]];
        let mut coverage = Coverage::default();
        coverage.ring(&square, 1);
        coverage.ring(&first, -1);
        assert!(!coverage.0.is_empty(), "missing triangle");
        coverage.ring(&second, -1);
        assert!(coverage.0.is_empty());
        coverage.ring(&first, -1);
        assert!(!coverage.0.is_empty(), "duplicate triangle");

        let mut coverage = Coverage::default();
        coverage.ring(&square, 1);
        coverage.ring(&[[0, 0], [3, 0], [8, 0], [8, 5], [0, 5]].map(vertex), -1);
        assert!(!coverage.0.is_empty(), "lost collinear contact vertex");
        let mut coverage = Coverage::default();
        coverage.ring(&square, 1);
        coverage.ring(&[[1, 0], [9, 0], [9, 5], [1, 5]].map(vertex), -1);
        assert!(!coverage.0.is_empty(), "equal area with displaced boundary");
    }

    #[test]
    fn coalescing_rejects_point_contacts_and_hole_closure() {
        let points = [
            [0, 0],
            [10, 0],
            [10, 9],
            [0, 9],
            [2, 3],
            [7, 3],
            [7, 6],
            [2, 6],
        ]
        .map(vertex);
        let triangles: Vec<_> = [
            [0, 1, 5],
            [0, 5, 4],
            [1, 2, 6],
            [1, 6, 5],
            [2, 3, 7],
            [2, 7, 6],
            [3, 0, 4],
            [3, 4, 7],
        ]
        .map(|indices| indices.map(|i| points[i]))
        .to_vec();
        let polygons = coalesce(&triangles, 5000);
        assert!(
            polygons.len() >= 2,
            "a hole cannot fit in one simple additive polygon"
        );
        let mut coverage = Coverage::default();
        for triangle in &triangles {
            coverage.ring(triangle, 1);
        }
        for polygon in &polygons {
            assert_simple(polygon);
            coverage.ring(polygon, -1);
        }
        assert!(coverage.0.is_empty());

        let touching =
            [[[0, 0], [3, 0], [0, 2]], [[0, 0], [-4, 0], [0, -2]]].map(|t| t.map(vertex));
        assert_eq!(coalesce(&touching, 5000).len(), 2);
    }
}
