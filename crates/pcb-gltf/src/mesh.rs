//! Indexed triangle meshes in millimetres.

use glam::DVec3;

/// Triangles of one material.
#[derive(Default)]
pub(crate) struct Primitive {
    pub(crate) positions: Vec<[f32; 3]>,
    pub(crate) normals: Vec<[f32; 3]>,
    pub(crate) indices: Vec<u32>,
}

impl Primitive {
    pub(crate) fn vertex(&mut self, p: DVec3, n: DVec3) -> u32 {
        let index = self.positions.len() as u32;
        self.positions.push(p.as_vec3().to_array());
        self.normals.push(n.as_vec3().to_array());
        index
    }

    /// A quad `a b c d` facing along `normal`, whatever its winding.
    pub(crate) fn quad(&mut self, corners: [(DVec3, DVec3); 4], normal: DVec3) {
        let [a, b, c, d] = corners;
        let facing = (b.0 - a.0).cross(c.0 - a.0) + (c.0 - a.0).cross(d.0 - a.0);
        let i = corners.map(|(p, n)| self.vertex(p, n));
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
}
