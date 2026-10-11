//! Indexed triangle meshes in millimetres.

use glam::{DVec3, Vec3};
use meshopt_rs::simplify::{SimplificationOptions, simplify};
use meshopt_rs::vertex::Vertex;
use meshopt_rs::vertex::cache::optimize_vertex_cache;
use meshopt_rs::vertex::fetch::optimize_vertex_fetch;
use rayon::prelude::*;
use rustc_hash::FxHashMap;

/// The most triangles one simplification takes. A larger primitive is
/// simplified in spatially compact chunks on the rayon pool; the edges
/// between chunks are borders and stay put, so the mesh stays closed.
const SIMPLIFY_CHUNK: usize = 1 << 17;

/// Triangles of one material.
#[derive(Default)]
pub(crate) struct Primitive {
    pub(crate) positions: Vec<[f32; 3]>,
    pub(crate) normals: Vec<[f32; 3]>,
    pub(crate) indices: Vec<u32>,
}

/// A position and normal, as meshoptimizer simplifies and reorders them.
#[derive(Clone, Copy, Default)]
#[repr(C)]
struct Corner {
    position: [f32; 3],
    normal: [f32; 3],
}

impl Vertex for Corner {
    fn pos(&self) -> [f32; 3] {
        self.position
    }
}

impl Primitive {
    pub(crate) fn vertex(&mut self, p: DVec3, n: DVec3) -> u32 {
        let index = self.positions.len() as u32;
        self.positions.push(p.as_vec3().to_array());
        self.normals.push(n.as_vec3().to_array());
        index
    }

    /// The vertex at `p` with normal `n`, shared with the last few vertices
    /// if one matches. Consecutive quads along a curved wall share an edge.
    fn recent_vertex(&mut self, p: DVec3, n: DVec3) -> u32 {
        let (p32, n32) = (p.as_vec3().to_array(), n.as_vec3().to_array());
        let recent = self.positions.len().saturating_sub(4)..self.positions.len();
        recent
            .rev()
            .find(|&i| self.positions[i] == p32 && self.normals[i] == n32)
            .map_or_else(|| self.vertex(p, n), |i| i as u32)
    }

    /// A quad `a b c d` facing along `normal`, whatever its winding.
    pub(crate) fn quad(&mut self, corners: [(DVec3, DVec3); 4], normal: DVec3) {
        let [a, b, c, d] = corners;
        let facing = (b.0 - a.0).cross(c.0 - a.0) + (c.0 - a.0).cross(d.0 - a.0);
        let i = corners.map(|(p, n)| self.recent_vertex(p, n));
        if facing.dot(normal) >= 0.0 {
            self.indices.extend([i[0], i[1], i[2], i[0], i[2], i[3]]);
        } else {
            self.indices.extend([i[0], i[2], i[1], i[0], i[3], i[2]]);
        }
    }

    pub(crate) fn append(&mut self, other: Primitive) {
        let base = self.positions.len() as u32;
        self.positions.extend(other.positions);
        self.normals.extend(other.normals);
        self.indices
            .extend(other.indices.into_iter().map(|i| i + base));
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    /// Weld vertices with the same position and normal, then simplify
    /// within `error` millimetres. Open borders stay put, so colour
    /// boundaries do too, and so do the seams between chunks of a large
    /// primitive.
    pub(crate) fn simplify(&mut self, error: f32) {
        let mut ids = FxHashMap::with_capacity_and_hasher(self.positions.len(), Default::default());
        let mut unique = Vec::new();
        let remap: Vec<u32> = self
            .corners()
            .map(|corner| {
                let key = [corner.position, corner.normal].map(|v| v.map(f32::to_bits));
                *ids.entry(key).or_insert_with(|| {
                    unique.push(corner);
                    unique.len() as u32 - 1
                })
            })
            .collect();
        let welded: Vec<u32> = self.indices.iter().map(|&i| remap[i as usize]).collect();
        let triangles = welded.len() / 3;
        self.indices = if triangles <= SIMPLIFY_CHUNK {
            simplify_locked(&welded, &unique, error)
        } else {
            chunks(&welded, &unique, triangles.div_ceil(SIMPLIFY_CHUNK))
                .par_iter()
                .map(|chunk| simplify_chunk(chunk, &unique, error))
                .flatten_iter()
                .collect()
        };
        self.set_corners(&unique);
    }

    /// Order the triangles for the vertex cache and the vertices by first
    /// use, dropping unused ones. This is also the order the meshopt codec
    /// compresses best.
    pub(crate) fn optimize(&mut self) {
        let corners: Vec<Corner> = self.corners().collect();
        let mut indices = vec![0; self.indices.len()];
        optimize_vertex_cache(&mut indices, &self.indices, corners.len());
        let mut ordered = vec![Corner::default(); corners.len()];
        let used = optimize_vertex_fetch(&mut ordered, &mut indices, &corners);
        ordered.truncate(used);
        self.indices = indices;
        self.set_corners(&ordered);
    }

    fn corners(&self) -> impl Iterator<Item = Corner> {
        self.positions
            .iter()
            .zip(&self.normals)
            .map(|(&position, &normal)| Corner { position, normal })
    }

    fn set_corners(&mut self, corners: &[Corner]) {
        self.positions = corners.iter().map(|c| c.position).collect();
        self.normals = corners.iter().map(|c| c.normal).collect();
    }
}

/// `indices` simplified within `error` millimetres, borders locked.
fn simplify_locked(indices: &[u32], corners: &[Corner], error: f32) -> Vec<u32> {
    let mut out = vec![0; indices.len()];
    let count = simplify(
        &mut out,
        indices,
        corners,
        0,
        error,
        SimplificationOptions::SimplifyErrorAbsolute | SimplificationOptions::SimplifyLockBorder,
        None,
    );
    out.truncate(count);
    out
}

/// One chunk simplified on just the corners it uses, so each job's cost
/// follows its own size.
fn simplify_chunk(indices: &[u32], corners: &[Corner], error: f32) -> Vec<u32> {
    let mut local_of = FxHashMap::with_capacity_and_hasher(indices.len(), Default::default());
    let mut global = Vec::new();
    let mut local_corners = Vec::new();
    let local: Vec<u32> = indices
        .iter()
        .map(|&i| {
            *local_of.entry(i).or_insert_with(|| {
                global.push(i);
                local_corners.push(corners[i as usize]);
                global.len() as u32 - 1
            })
        })
        .collect();
    simplify_locked(&local, &local_corners, error)
        .into_iter()
        .map(|l| global[l as usize])
        .collect()
}

/// The triangles of `indices` in `count` runs of nearly equal size along
/// the Morton order of their centroids, so each run is spatially compact.
fn chunks(indices: &[u32], corners: &[Corner], count: usize) -> Vec<Vec<u32>> {
    let triangles = indices.as_chunks::<3>().0;
    let centroid = |t: &[u32; 3]| {
        t.iter()
            .map(|&i| Vec3::from(corners[i as usize].position))
            .sum::<Vec3>()
            / 3.0
    };
    let (lo, hi) = triangles
        .iter()
        .map(centroid)
        .fold((Vec3::INFINITY, Vec3::NEG_INFINITY), |(lo, hi), c| {
            (lo.min(c), hi.max(c))
        });
    let scale = 1023.0 / (hi - lo).max_element().max(f32::MIN_POSITIVE);
    let morton = |c: Vec3| {
        // Spread the low 10 bits of v to every third bit.
        let spread = |v: u32| {
            let mut x = v & 0x3ff;
            x = (x | (x << 16)) & 0x0300_00ff;
            x = (x | (x << 8)) & 0x0300_f00f;
            x = (x | (x << 4)) & 0x030c_30c3;
            (x | (x << 2)) & 0x0924_9249
        };
        let q = ((c - lo) * scale).as_uvec3();
        spread(q.x) | (spread(q.y) << 1) | (spread(q.z) << 2)
    };
    let mut order: Vec<(u32, u32)> = triangles
        .iter()
        .enumerate()
        .map(|(i, t)| (morton(centroid(t)), i as u32))
        .collect();
    order.sort_unstable();
    order
        .chunks(triangles.len().div_ceil(count))
        .map(|run| {
            run.iter()
                .flat_map(|&(_, t)| triangles[t as usize])
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A primitive large enough to be simplified in chunks stays closed:
    /// every edge still has a triangle on each side.
    #[test]
    fn chunked_simplification_keeps_a_closed_mesh_closed() {
        let (rings, segments) = (260, 520);
        let mut sphere = Primitive::default();
        for i in 0..=rings {
            for j in 0..segments {
                let (theta, phi) = (
                    std::f64::consts::PI * i as f64 / rings as f64,
                    std::f64::consts::TAU * j as f64 / segments as f64,
                );
                let n = DVec3::new(
                    theta.sin() * phi.cos(),
                    theta.sin() * phi.sin(),
                    theta.cos(),
                );
                sphere.vertex(n * 10.0, n);
            }
        }
        let at = |i: u32, j: u32| i * segments + j % segments;
        for i in 0..rings {
            for j in 0..segments {
                let (a, b, c, d) = (at(i, j), at(i, j + 1), at(i + 1, j + 1), at(i + 1, j));
                sphere.indices.extend([a, b, c, a, c, d]);
            }
        }
        // The pole rings collapse to one point each; weld them so the
        // input is closed too.
        for (k, p) in sphere.positions.clone().iter().enumerate() {
            if k % segments as usize != 0 && (p[2].abs() - 10.0).abs() < 1e-4 {
                sphere.positions[k] = sphere.positions[k - k % segments as usize];
                sphere.normals[k] = sphere.normals[k - k % segments as usize];
            }
        }
        assert!(sphere.indices.len() / 3 > SIMPLIFY_CHUNK);

        sphere.simplify(0.01);
        sphere.optimize();
        assert!(sphere.indices.len() / 3 < SIMPLIFY_CHUNK);

        let mut uses: FxHashMap<([u32; 3], [u32; 3]), i32> = FxHashMap::default();
        let key = |i: u32| sphere.positions[i as usize].map(f32::to_bits);
        for t in sphere.indices.as_chunks::<3>().0 {
            for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                let (a, b) = (key(a), key(b));
                if a == b {
                    continue;
                }
                *uses.entry((a.min(b), a.max(b))).or_default() += if a < b { 1 } else { -1 };
            }
        }
        let open = uses.values().filter(|&&n| n != 0).count();
        assert_eq!(open, 0, "{open} edges without a triangle on each side");
    }
}
