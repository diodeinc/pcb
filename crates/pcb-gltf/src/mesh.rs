//! Indexed triangle meshes in millimetres.

use glam::DVec3;
use meshopt_rs::simplify::{SimplificationOptions, simplify};
use meshopt_rs::vertex::Vertex;
use meshopt_rs::vertex::cache::optimize_vertex_cache;
use meshopt_rs::vertex::fetch::optimize_vertex_fetch;
use rustc_hash::FxHashMap;

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
    /// boundaries do too.
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
        self.indices.resize(welded.len(), 0);
        let count = simplify(
            &mut self.indices,
            &welded,
            &unique,
            0,
            error,
            SimplificationOptions::SimplifyErrorAbsolute
                | SimplificationOptions::SimplifyLockBorder,
            None,
        );
        self.indices.truncate(count);
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
