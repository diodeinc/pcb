//! STEP → triangle mesh via foxtrot's `triangulate` crate.

use anyhow::{Result, anyhow};

/// Tessellated STEP mesh: flat vertices (3*N f64) and triangle indices
/// (3*M u32). Kept dense and flat for fast transform and rasterization.
#[derive(Debug, Clone)]
pub struct MeshData {
    pub vertices: Vec<f64>,
    pub faces: Vec<u32>,
}

impl MeshData {
    pub fn vertex(&self, i: usize) -> [f64; 3] {
        [
            self.vertices[i * 3],
            self.vertices[i * 3 + 1],
            self.vertices[i * 3 + 2],
        ]
    }
    pub fn num_vertices(&self) -> usize {
        self.vertices.len() / 3
    }
    pub fn num_faces(&self) -> usize {
        self.faces.len() / 3
    }
}

/// Tessellate a STEP blob into a triangle soup via foxtrot.
///
/// Foxtrot's `tessellate_step_bytes` uses a fixed internal resolution;
/// there is no caller-tunable tolerance knob at this time.
pub fn tessellate(step_bytes: &[u8]) -> Result<MeshData> {
    let mesh = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        triangulate::colored_mesh::tessellate_step_bytes(step_bytes)
    }))
    .map_err(|p| {
        let msg = p
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_else(|| "unknown panic".into());
        anyhow!("tessellation panicked: {msg}")
    })?
    .map_err(|e| anyhow!("tessellation failed: {e}"))?
    .0;

    Ok(to_mesh_data(&mesh))
}

fn to_mesh_data(mesh: &triangulate::colored_mesh::TessellatedMesh) -> MeshData {
    // Foxtrot returns per-submesh positions+indices (indices are local to each
    // submesh). Concatenate with an index-base offset so the pose solver sees
    // a single indexed triangle soup.
    let mut vertices: Vec<f64> = Vec::new();
    let mut faces: Vec<u32> = Vec::new();
    for sm in &mesh.submeshes {
        let base = (vertices.len() / 3) as u32;
        for p in &sm.positions {
            vertices.push(p[0] as f64);
            vertices.push(p[1] as f64);
            vertices.push(p[2] as f64);
        }
        for &idx in &sm.indices {
            faces.push(idx + base);
        }
    }
    MeshData { vertices, faces }
}
