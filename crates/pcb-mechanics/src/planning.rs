//! Pure numerical evaluation of an explicitly supplied board/frame plan.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt::{Display, Formatter};

use pcb_elastic::{DMatrix, DVector, Tolerances, selection};
use pcb_ir::geom::mesh::{AnalysisMesh, MeshOptions, MeshQuality, RefinementStatus};
use pcb_ir::geom::{BBox, ContourSet, FillRule, Point};

use crate::{Assembly, Connection, Dof, Patch, Plate};

pub struct Site {
    pub id: usize,
    pub board: usize,
    pub board_landing: ContourSet,
    pub frame_landing: ContourSet,
    pub reference: [f64; 2],
}

pub struct LoadCase {
    /// `[Fz, Mx, My]` at each corresponding board region bounding-box center.
    pub board_resultants: Vec<[f64; 3]>,
    pub compliance_limit: f64,
}

pub struct Policy {
    pub bending: [[f64; 3]; 3],
    pub connection_stiffness: [[f64; 3]; 3],
    pub scales: [f64; 2],
    pub mesh_options: MeshOptions,
    pub max_dofs: usize,
    pub max_subsets: usize,
    pub tolerances: Tolerances,
    /// Top, right, bottom, left sides of the outer frame bounding box.
    pub clamp_sides: [bool; 4],
}

#[derive(Debug)]
pub struct Evaluation {
    pub report: selection::Report,
    pub dofs: usize,
    /// Boards in input order, followed by the frame.
    pub mesh_quality: Vec<MeshQuality>,
    pub mesh_refinement: Vec<RefinementStatus>,
    /// Selected cases, then boards: whole-board fitted [w, theta_x, theta_y]
    /// at each board bbox center. Empty without a verified incumbent.
    pub board_responses: Vec<Vec<[f64; 3]>>,
}

#[derive(Debug)]
struct PlanningError(String);

impl Display for PlanningError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl Error for PlanningError {}

fn err(context: impl Into<String>) -> Box<dyn Error + Send + Sync> {
    Box::new(PlanningError(context.into()))
}

fn matrix(values: [[f64; 3]; 3]) -> DMatrix<f64> {
    DMatrix::from_fn(3, 3, |r, c| values[r][c])
}

fn barycentric(mesh: &AnalysisMesh, element: usize, point: Point) -> Option<[f64; 3]> {
    if let Some(attachment) = mesh.attach_to_element(point, element, 0.0) {
        return Some(attachment.weights);
    }
    let vertices = mesh
        .elements
        .get(element)?
        .vertices
        .map(|v| mesh.vertices[v]);
    let cross = |a: Point, b: Point| a.x * b.y - a.y * b.x;
    let denominator = cross(vertices[1] - vertices[0], vertices[2] - vertices[0]);
    let mut weights = [
        cross(vertices[1] - point, vertices[2] - point) / denominator,
        cross(vertices[2] - point, vertices[0] - point) / denominator,
        cross(vertices[0] - point, vertices[1] - point) / denominator,
    ];
    let scale = vertices
        .iter()
        .fold(1.0_f64, |s, p| s.max(p.x.abs()).max(p.y.abs()));
    let roundoff = 128.0 * f64::EPSILON * scale * scale / denominator.abs();
    if weights.iter().any(|w| !w.is_finite() || *w < -roundoff) {
        return None;
    }
    // This removes only the computed arithmetic residue at a clipping boundary;
    // no nonzero geometric distance is accepted.
    weights.iter_mut().for_each(|w| *w = w.max(0.0));
    let sum = weights.iter().sum::<f64>();
    Some(weights.map(|w| w / sum))
}

/// Partition `region` by mesh elements. Each positive-area intersection is
/// triangulated with its holes preserved, then attached to that chosen element
/// with zero snapping. Coverage is checked independently against the plate.
fn patches(
    region: &ContourSet,
    plate_region: &ContourSet,
    mesh: &AnalysisMesh,
    plate: usize,
    what: &str,
    options: MeshOptions,
) -> Result<Vec<Patch>, Box<dyn Error + Send + Sync>> {
    if region.is_empty() || !region.area().is_finite() || region.area() <= 0.0 {
        return Err(err(format!(
            "{what}: landing must have positive finite area"
        )));
    }
    let uncovered = region
        .difference(plate_region)
        .map_err(|e| err(format!("{what}: containment check failed: {e}")))?;
    if !uncovered.is_empty() || uncovered.area() != 0.0 {
        return Err(err(format!(
            "{what}: landing is not completely covered by its plate"
        )));
    }
    let mut out = Vec::new();
    let mut covered_area = 0.0;
    for (element, triangle) in mesh.elements.iter().enumerate() {
        let ring = triangle
            .vertices
            .iter()
            .map(|&v| {
                let p = mesh.vertices[v];
                [p.x, p.y]
            })
            .collect();
        let triangle_region =
            ContourSet::from_rings(vec![ring], FillRule::NonZero, region.resolution.strict())
                .map_err(|e| err(format!("{what}: element {element} region failed: {e}")))?;
        let clipped = region
            .intersection(&triangle_region)
            .map_err(|e| err(format!("{what}: clipping element {element} failed: {e}")))?;
        if clipped.is_empty() {
            continue;
        }
        let area = clipped.area();
        if !area.is_finite() || area <= 0.0 {
            return Err(err(format!(
                "{what}: invalid clipped area in element {element}"
            )));
        }
        let partition = AnalysisMesh::new(&clipped, options)
            .map_err(|e| err(format!("{what}: triangulating element {element}: {e}")))?;
        if partition.elements.is_empty() {
            return Err(err(format!(
                "{what}: positive clipped area produced no triangles in element {element}"
            )));
        }
        for part in &partition.elements {
            let mut vertices = [[0.0; 3]; 3];
            for (i, &v) in part.vertices.iter().enumerate() {
                let weights =
                    barycentric(mesh, element, partition.vertices[v]).ok_or_else(|| {
                        err(format!(
                            "{what}: clipped vertex is not in incident element {element}"
                        ))
                    })?;
                vertices[i] = weights;
            }
            out.push(Patch {
                plate,
                element,
                vertices,
            });
        }
        covered_area += area;
    }
    let allowance = region
        .tolerance()
        .powi(2)
        .max(f64::EPSILON * region.area().abs() * 64.0);
    if out.is_empty() || (covered_area - region.area()).abs() > allowance {
        return Err(err(format!(
            "{what}: mesh partition does not cover landing ({} of {})",
            covered_area,
            region.area()
        )));
    }
    Ok(out)
}

fn topology_dofs(meshes: &[AnalysisMesh]) -> usize {
    let mut keys = BTreeSet::new();
    for (plate, mesh) in meshes.iter().enumerate() {
        for element in &mesh.elements {
            for i in 0..3 {
                keys.insert(Dof::Vertex {
                    plate,
                    component: element.component,
                    vertex: element.vertices[i],
                });
                let mut edge = [element.vertices[i], element.vertices[(i + 1) % 3]];
                edge.sort();
                keys.insert(Dof::Slope {
                    plate,
                    component: element.component,
                    edge,
                });
            }
        }
    }
    keys.len()
}

pub fn evaluate(
    boards: &[ContourSet],
    frame: &ContourSet,
    sites: &[Site],
    conflicts: &[(usize, usize)],
    cases: &[LoadCase],
    policy: &Policy,
) -> Result<Evaluation, Box<dyn Error + Send + Sync>> {
    if boards.is_empty() || !policy.clamp_sides.iter().any(|&x| x) {
        return Err(err(
            "at least one board and one clamped frame side are required",
        ));
    }
    if sites.iter().enumerate().any(|(i, s)| {
        s.board >= boards.len()
            || !s.reference.iter().all(|x| x.is_finite())
            || sites[..i].iter().any(|p| p.id == s.id)
    }) {
        return Err(err("site IDs, board ownership, or references are invalid"));
    }
    let mut meshes = boards
        .iter()
        .enumerate()
        .map(|(i, r)| {
            AnalysisMesh::new(r, policy.mesh_options)
                .map_err(|e| err(format!("board {i} mesh: {e}")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let frame_mesh = AnalysisMesh::new(frame, policy.mesh_options)
        .map_err(|e| err(format!("frame mesh: {e}")))?;
    if frame_mesh.elements.is_empty() {
        return Err(err("frame mesh is empty"));
    }
    meshes.push(frame_mesh);
    let dofs = topology_dofs(&meshes);
    if dofs > policy.max_dofs {
        return Err(err(format!(
            "mesh has {dofs} DOFs, exceeding max_dofs {}",
            policy.max_dofs
        )));
    }

    let bending = matrix(policy.bending);
    let plates = meshes
        .iter()
        .map(|mesh| Plate {
            mesh,
            bending: bending.clone(),
            scales: policy.scales,
        })
        .collect::<Vec<_>>();
    let assembly =
        Assembly::new(&plates, policy.tolerances).map_err(|e| err(format!("assembly: {e}")))?;
    debug_assert_eq!(assembly.ndofs(), dofs);
    let frame_plate = boards.len();
    let stiffness = matrix(policy.connection_stiffness);
    let candidates = sites
        .iter()
        .map(|site| {
            let a = patches(
                &site.board_landing,
                &boards[site.board],
                &meshes[site.board],
                site.board,
                &format!("site {} board landing", site.id),
                policy.mesh_options,
            )?;
            let b = patches(
                &site.frame_landing,
                frame,
                &meshes[frame_plate],
                frame_plate,
                &format!("site {} frame landing", site.id),
                policy.mesh_options,
            )?;
            assembly
                .candidate(&Connection {
                    id: site.id,
                    a,
                    b,
                    reference: site.reference,
                    stiffness: stiffness.clone(),
                })
                .map_err(|e| err(format!("site {} stiffness/connection: {e}", site.id)))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let board_ports = boards
        .iter()
        .enumerate()
        .map(|(board, region)| {
            let whole = patches(
                region,
                region,
                &meshes[board],
                board,
                &format!("board {board} load region"),
                policy.mesh_options,
            )?;
            let bbox = region.bbox();
            assembly
                .port(
                    &whole,
                    [
                        (bbox.min.x + bbox.max.x) / 2.0,
                        (bbox.min.y + bbox.max.y) / 2.0,
                    ],
                )
                .map_err(|e| err(format!("board {board} port: {e}")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut numerical_cases = Vec::new();
    for (case_index, case) in cases.iter().enumerate() {
        if case.board_resultants.len() != boards.len() {
            return Err(err(format!(
                "load case {case_index}: expected {} board resultants",
                boards.len()
            )));
        }
        let mut loads = DVector::zeros(dofs);
        for (board, resultant) in case.board_resultants.iter().enumerate() {
            if !resultant.iter().all(|x| x.is_finite()) {
                return Err(err(format!(
                    "load case {case_index}, board {board}: non-finite resultant"
                )));
            }
            loads += board_ports[board].transpose() * DVector::from_column_slice(resultant);
        }
        numerical_cases.push(selection::LoadCase {
            loads: loads.as_slice().to_vec(),
            compliance_limit: case.compliance_limit,
        });
    }

    let bbox: BBox = frame.bbox();
    let fm = &meshes[frame_plate];
    let mut fixed = Vec::new();
    for edge in &fm.boundary {
        let [a, b] = edge.vertices.map(|v| fm.vertices[v]);
        let named = (policy.clamp_sides[0] && a.y == bbox.max.y && b.y == bbox.max.y)
            || (policy.clamp_sides[1] && a.x == bbox.max.x && b.x == bbox.max.x)
            || (policy.clamp_sides[2] && a.y == bbox.min.y && b.y == bbox.min.y)
            || (policy.clamp_sides[3] && a.x == bbox.min.x && b.x == bbox.min.x);
        if named {
            for vertex in edge.vertices {
                fixed.push(
                    assembly
                        .dof(Dof::Vertex {
                            plate: frame_plate,
                            component: edge.component,
                            vertex,
                        })
                        .map_err(|e| err(format!("frame clamp vertex: {e}")))?,
                );
            }
            let mut vertices = edge.vertices;
            vertices.sort();
            fixed.push(
                assembly
                    .dof(Dof::Slope {
                        plate: frame_plate,
                        component: edge.component,
                        edge: vertices,
                    })
                    .map_err(|e| err(format!("frame clamp slope: {e}")))?,
            );
        }
    }
    fixed.sort_unstable();
    fixed.dedup();
    if fixed.is_empty() {
        return Err(err("named clamp sides contain no exterior frame edges"));
    }
    let report = selection::select(
        &assembly.model,
        &candidates,
        conflicts,
        &numerical_cases,
        &fixed,
        policy.max_subsets,
    )
    .map_err(|e| err(format!("selection: {e}")))?;
    let board_responses = report
        .selected
        .as_ref()
        .map(|s| {
            s.analyses
                .iter()
                .map(|a| {
                    board_ports
                        .iter()
                        .map(|p| {
                            let response = p * &a.displacement;
                            [response[0], response[1], response[2]]
                        })
                        .collect()
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Evaluation {
        report,
        dofs,
        mesh_quality: meshes.iter().map(|m| m.quality).collect(),
        mesh_refinement: meshes.iter().map(|m| m.refinement).collect(),
        board_responses,
    })
}
