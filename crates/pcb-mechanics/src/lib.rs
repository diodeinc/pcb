#![doc = include_str!("../README.md")]
pub mod planning;

use std::collections::BTreeMap;

use pcb_elastic::{Contribution, DMatrix, Error, Model, Tolerances, elements::MorleyTriangle};
use pcb_ir::geom::mesh::AnalysisMesh;

/// All coordinates are mm. Bending tensor acts on [w,xx, w,yy, 2w,xy], in N·mm,
/// in the mesh's global axes (rotate orthotropic tensors before supplying them).
pub struct Plate<'a> {
    pub mesh: &'a AnalysisMesh,
    pub bending: DMatrix<f64>,
    /// Characteristic [w (mm), normal slope] for numerical scaling, not restraints.
    pub scales: [f64; 2],
}

/// Snapshot-local, component-separated DOF identity. Edge endpoints are sorted;
/// their canonical normal is (dy,-dx)/length from the smaller to larger index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Dof {
    Vertex {
        plate: usize,
        component: usize,
        vertex: usize,
    },
    Slope {
        plate: usize,
        component: usize,
        edge: [usize; 2],
    },
}

/// A positive-area triangular part of an attachment, wholly inside one named
/// element. Vertices are barycentric locations, not bending shape weights.
/// Patches must partition the physical footprint without overlap; no snapping,
/// clipping, union, or incident-side choice is performed by this adapter.
#[derive(Clone, Debug)]
pub struct Patch {
    pub plate: usize,
    pub element: usize,
    pub vertices: [[f64; 3]; 3],
}

/// One candidate, counted once by the existing selector. Neither side is ground.
pub struct Connection {
    pub id: usize,
    pub a: Vec<Patch>,
    pub b: Vec<Patch>,
    /// Shared global reference for both fitted rigid planes, in mm.
    pub reference: [f64; 2],
    /// Symmetric PSD stiffness on relative [w, theta_x, theta_y]. Energy is
    /// 1/2 dᵀ C d. Units: Cww N/mm, Cwθ N, Cθθ N·mm. Explicit calibration input.
    pub stiffness: DMatrix<f64>,
}

struct Element {
    triangle: MorleyTriangle,
    vertices: [[f64; 2]; 3],
    dofs: [usize; 6],
    component: usize,
}

/// Immutable plate base and snapshot-local mappings. Loads and fixtures use
/// `dof`/`element_dofs`; footprint resultants use `port(...).transpose() * f`.
/// Feed `model`, `candidate` results and numerical load cases to
/// `pcb_elastic::selection::select`; its report retains all budget/failure status.
pub struct Assembly {
    pub model: Model,
    elements: Vec<Vec<Element>>,
    dofs: BTreeMap<Dof, usize>,
    tolerances: Tolerances,
}

impl Assembly {
    pub fn new(plates: &[Plate<'_>], tolerances: Tolerances) -> Result<Self, Error> {
        let mut dofs = BTreeMap::new();
        let mut scales = Vec::new();
        let mut base = Vec::new();
        let mut elements = Vec::new();
        for (plate, p) in plates.iter().enumerate() {
            if p.mesh.elements.is_empty() || p.scales.iter().any(|s| !s.is_finite() || *s <= 0.0) {
                return Err(Error::InvalidInput);
            }
            let mut local = Vec::new();
            for e in &p.mesh.elements {
                let mut vertices = [[0.0; 2]; 3];
                let mut indices = [0; 6];
                let mut signs = [0.0; 3];
                for i in 0..3 {
                    let v = e.vertices[i];
                    let point = p.mesh.vertices.get(v).ok_or(Error::InvalidInput)?;
                    vertices[i] = [point.x, point.y];
                    let mut edge = [v, e.vertices[(i + 1) % 3]];
                    signs[i] = if edge[0] < edge[1] { 1.0 } else { -1.0 };
                    edge.sort();
                    for (slot, key, scale) in [
                        (
                            i,
                            Dof::Vertex {
                                plate,
                                component: e.component,
                                vertex: v,
                            },
                            p.scales[0],
                        ),
                        (
                            3 + i,
                            Dof::Slope {
                                plate,
                                component: e.component,
                                edge,
                            },
                            p.scales[1],
                        ),
                    ] {
                        indices[slot] = *dofs.entry(key).or_insert_with(|| {
                            scales.push(scale);
                            scales.len() - 1
                        });
                    }
                }
                let triangle = MorleyTriangle::new(vertices, signs)?;
                base.push(Contribution {
                    dofs: indices.to_vec(),
                    stiffness: triangle.stiffness(&p.bending)?,
                });
                local.push(Element {
                    triangle,
                    vertices,
                    dofs: indices,
                    component: e.component,
                });
            }
            elements.push(local);
        }
        Ok(Self {
            model: Model::new(scales, &base, tolerances)?,
            elements,
            dofs,
            tolerances,
        })
    }

    pub fn dof(&self, key: Dof) -> Result<usize, Error> {
        self.dofs.get(&key).copied().ok_or(Error::InvalidInput)
    }

    pub fn ndofs(&self) -> usize {
        self.dofs.len()
    }

    pub fn element_dofs(&self, plate: usize, element: usize) -> Result<[usize; 6], Error> {
        Ok(self.element(plate, element)?.dofs)
    }

    fn element(&self, plate: usize, element: usize) -> Result<&Element, Error> {
        self.elements
            .get(plate)
            .and_then(|p| p.get(element))
            .ok_or(Error::InvalidInput)
    }

    /// Area L² projection of the element-sided quadratic w onto the rigid plane
    /// w(x,y) = z + theta_x (y-ref_y) - theta_y (x-ref_x).
    /// Returns global rows for [z, theta_x, theta_y]. Integrals are exact for P2
    /// fields; rotations come from the fitted displacement, NOT averaged gradients.
    /// Each footprint must belong to one physical plate/component. Non-affine
    /// warping orthogonal to this fit is unrestrained by the connection.
    pub fn port(&self, patches: &[Patch], reference: [f64; 2]) -> Result<DMatrix<f64>, Error> {
        if patches.is_empty() || reference.iter().any(|x| !x.is_finite()) {
            return Err(Error::InvalidInput);
        }
        let mut gram = DMatrix::<f64>::zeros(3, 3);
        let mut rhs = DMatrix::<f64>::zeros(3, self.ndofs());
        let mut owner = None;
        let origin = self.element(patches[0].plate, patches[0].element)?.vertices[0];
        let mut prepared = Vec::new();
        let mut total_area = 0.0;
        let mut centroid = [0.0; 2];
        for patch in patches {
            let e = self.element(patch.plate, patch.element)?;
            let identity = (patch.plate, e.component);
            if owner.is_some_and(|o| o != identity) {
                return Err(Error::InvalidInput);
            }
            owner = Some(identity);
            let mut points = [[0.0; 2]; 3];
            for (i, bary) in patch.vertices.iter().enumerate() {
                e.triangle.attachment(*bary)?; // validate each patch vertex
                points[i] = [0, 1].map(|axis| {
                    (0..3)
                        .map(|j| bary[j] * (e.vertices[j][axis] - origin[axis]))
                        .sum()
                });
            }
            let [a, b, c] = points;
            let area = ((b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])).abs() / 2.0;
            if !area.is_finite() || area <= 0.0 {
                return Err(Error::InvalidInput);
            }
            total_area += area;
            for axis in 0..2 {
                centroid[axis] += area * points.iter().map(|p| p[axis]).sum::<f64>() / 3.0;
            }
            prepared.push((patch, e, points, area));
        }
        if !total_area.is_finite() {
            return Err(Error::NumericalFailure);
        }
        let length = total_area.sqrt();
        centroid.iter_mut().for_each(|x| *x /= total_area);
        for (patch, e, points, area) in prepared {
            // Degree-three triangle rule: exact for plane × quadratic shape.
            for (q, weight) in [
                ([1.0 / 3.0; 3], -27.0 / 48.0),
                ([0.6, 0.2, 0.2], 25.0 / 48.0),
                ([0.2, 0.6, 0.2], 25.0 / 48.0),
                ([0.2, 0.2, 0.6], 25.0 / 48.0),
            ] {
                let bary = [0, 1, 2].map(|i| (0..3).map(|j| q[j] * patch.vertices[j][i]).sum());
                let point: [f64; 2] = [0, 1].map(|i| (0..3).map(|j| q[j] * points[j][i]).sum());
                // Fit near the footprint, in dimensionless coordinates. Changing
                // the requested reference then transports a plane, rather than
                // solving nearly dependent columns for a small distant footprint.
                let plane = [
                    1.0,
                    (point[1] - centroid[1]) / length,
                    (centroid[0] - point[0]) / length,
                ];
                let shape = e.triangle.attachment(bary)?.rows;
                for i in 0..3 {
                    for j in 0..3 {
                        gram[(i, j)] += area * weight * plane[i] * plane[j];
                    }
                    for j in 0..6 {
                        rhs[(i, e.dofs[j])] += area * weight * plane[i] * shape[(0, j)];
                    }
                }
            }
        }
        let fit = gram.cholesky().ok_or(Error::NumericalFailure)?.solve(&rhs);
        let transport = DMatrix::from_row_slice(
            3,
            3,
            &[
                1.0,
                (reference[1] - origin[1] - centroid[1]) / length,
                (origin[0] - reference[0] + centroid[0]) / length,
                0.0,
                1.0 / length,
                0.0,
                0.0,
                0.0,
                1.0 / length,
            ],
        );
        let rows = transport * fit;
        if rows.iter().any(|x| !x.is_finite()) {
            return Err(Error::NumericalFailure);
        }
        Ok(rows)
    }

    pub fn candidate(
        &self,
        connection: &Connection,
    ) -> Result<pcb_elastic::selection::Candidate, Error> {
        let c = &connection.stiffness;
        if c.shape() != (3, 3) || c.iter().any(|x| !x.is_finite()) {
            return Err(Error::InvalidInput);
        }
        // Validate C itself, not just its pullback (a null H can hide negative C).
        // Channel validation uses unit mm/radian scales and the supplied solver
        // tolerances. The selector additionally validates the scaled global K.
        Model::new(
            vec![1.0; 3],
            &[Contribution {
                dofs: vec![0, 1, 2],
                stiffness: c.clone(),
            }],
            self.tolerances,
        )?;
        let h = self.port(&connection.a, connection.reference)?
            - self.port(&connection.b, connection.reference)?;
        let stiffness = h.transpose() * c * h;
        if stiffness.iter().any(|x| !x.is_finite()) {
            return Err(Error::NumericalFailure);
        }
        Ok(pcb_elastic::selection::Candidate {
            id: connection.id,
            contributions: vec![Contribution {
                dofs: (0..self.ndofs()).collect(),
                stiffness: (&stiffness + stiffness.transpose()) * 0.5,
            }],
        })
    }
}
