//! Experimental, pure PCB-to-mechanics translation. Units: N, mm, radians.
//! No manufacturing acceptance, implicit FR4, clamps, or load magnitude.
//! See README.md for the physical contract and outstanding validation.

use std::collections::{BTreeMap, BTreeSet};

use pcb_elastic::{Contribution, DMatrix, elements::MorleyTriangle};
use pcb_ir::geom::mesh::{AnalysisMesh, MeshError, MeshOptions, RefinementStatus};
use pcb_ir::geom::mouse_bite::TabGeometry;
use pcb_ir::geom::{Affine2, ContourSet, Point, attachment::transform_region};
use pcb_ir::import::physical::BoardPhysicalView;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("unsupported or invalid mechanical input: {0}")]
    Input(&'static str),
    #[error(transparent)]
    Mesh(#[from] MeshError),
    #[error(transparent)]
    Elastic(#[from] pcb_elastic::Error),
    #[error("refinement incomplete: {0:?}")]
    Refinement(RefinementStatus),
    #[error("dense model requires {required} DOFs, budget is {limit}; no model assembled")]
    ResourceLimit { required: usize, limit: usize },
}

/// Explicit equivalent homogeneous symmetric laminate. Q is the effective
/// plane-stress tensor in PANEL axes for [epsilon_x, epsilon_y, gamma_xy],
/// N/mm². D = Q t³/12. Unsymmetric laminates with membrane/bending coupling
/// are unsupported. This is not a ply-stack homogenizer or material database.
pub struct Laminate {
    pub thickness_mm: f64,
    pub plane_stress: DMatrix<f64>,
    /// Datasheet/coupon provenance, temperature, direction, and lot if known.
    pub evidence: Option<String>,
}

impl Laminate {
    fn bending(&self) -> Result<DMatrix<f64>, Error> {
        let q = &self.plane_stress;
        if !self.thickness_mm.is_finite()
            || self.thickness_mm <= 0.0
            || q.shape() != (3, 3)
            || q.iter().any(|x| !x.is_finite())
            || q != &q.transpose()
            || q.clone().cholesky().is_none()
        {
            return Err(Error::Input(
                "positive thickness and symmetric positive definite Q required",
            ));
        }
        Ok(q * (self.thickness_mm.powi(3) / 12.0))
    }
}

pub struct BoardInstance<'a> {
    pub board: &'a BoardPhysicalView,
    pub placement: Affine2,
}

/// Final polygon domain, counted exactly once. All members use a single
/// panel-axis equivalent laminate; rotating board artwork does not rotate
/// the panel stock's material axes. Tab inputs are already in panel space.
pub struct Panel {
    pub substrate: ContourSet,
    pub diagnostics: Vec<String>,
}

impl Panel {
    /// Headless physical-view adapter; preserves missing/ambiguous metadata
    /// as diagnostics. Does not turn component bodies/courtyards into stiffness.
    pub fn from_physical(
        boards: &[BoardInstance<'_>],
        frame: &ContourSet,
        tabs: &[TabGeometry],
    ) -> Result<Self, Error> {
        let mut regions = Vec::new();
        let mut diagnostics = Vec::new();
        for (i, instance) in boards.iter().enumerate() {
            let t = instance.placement;
            if !t.preserves_circles(1e-12) || (t.determinant().abs() - 1.0).abs() > 1e-12 {
                return Err(Error::Input(
                    "board placement must be rigid, not scaled or sheared",
                ));
            }
            regions.push(
                transform_region(&instance.board.substrate, t)
                    .map_err(|_| Error::Input("invalid board placement"))?,
            );
            diagnostics.push(format!(
                "board {i}: source thickness {:?}; physical {:?}; metadata {:?}; geometry {:?}",
                instance.board.metadata.overall_thickness_mm,
                instance.board.diagnostics,
                instance.board.metadata.diagnostics,
                instance.board.source_diagnostics
            ));
        }
        let mut panel = Self::from_regions(&regions, frame, tabs)?;
        panel.diagnostics.extend(diagnostics);
        Ok(panel)
    }

    /// Canonical region entry point for synthetic models. Boards and retained
    /// frame must not overlap. Each tab retains full local stock: only its
    /// material outside nominal board/frame is added. All perforations are
    /// subtracted LAST, including their intrusion into nominal board material.
    /// Router voids from one single-tab construction must not cut another tab.
    pub fn from_regions(
        boards: &[ContourSet],
        frame: &ContourSet,
        tabs: &[TabGeometry],
    ) -> Result<Self, Error> {
        let mut nominal = frame.clone();
        for board in boards {
            if board.is_empty() || !nominal.intersection(board).is_empty() {
                return Err(Error::Input("empty or overlapping nominal board/frame"));
            }
            nominal.union_assign(board);
        }
        let mut substrate = nominal.clone();
        let mut holes = ContourSet::empty(nominal.tolerance);
        for tab in tabs {
            substrate.union_assign(&tab.retained_substrate.difference(&nominal));
            holes.union_assign(&tab.perforations);
        }
        substrate = substrate.difference(&holes);
        if substrate.is_empty() {
            return Err(Error::Input("empty panel"));
        }
        Ok(Self { substrate, diagnostics: vec![
            "EXPERIMENTAL / UNVALIDATED: no qualified laminate range, process limits, or physical tab/coupon validation".into(),
            "Omitted: copper stiffening, source drills/slots, components, fracture, local stress concentrations, transverse shear, membrane coupling, and beam torsion; continuum plate twisting IS included".into(),
            "Tab pattern is SparkFunShallow: diameter .381, pitch .635, nominal ligament .254, outward .127 / intrusion .0635 mm; offset is not SparkFun-tested".into(),
        ] })
    }

    /// Shared CDT only. Reject incomplete refinement and excessive DOFs before
    /// dense assembly. A small budget is a resource limit, not infeasibility.
    pub fn discretize(
        &self,
        laminate: &Laminate,
        options: MeshOptions,
        max_dofs: usize,
    ) -> Result<Discretization, Error> {
        let bending = laminate.bending()?;
        let mesh = AnalysisMesh::new(&self.substrate, options)?;
        if mesh.refinement != RefinementStatus::TargetsMet {
            return Err(Error::Refinement(mesh.refinement));
        }
        let mut edges = BTreeMap::new();
        for element in &mesh.elements {
            for j in 0..3 {
                let key = edge(element.vertices[j], element.vertices[(j + 1) % 3]);
                let next = mesh.vertices.len() + edges.len();
                edges.entry(key).or_insert(next);
            }
        }
        let n = mesh.vertices.len() + edges.len();
        if n > max_dofs {
            return Err(Error::ResourceLimit {
                required: n,
                limit: max_dofs,
            });
        }
        let mut triangles = Vec::new();
        let mut contributions = Vec::new();
        for element in &mesh.elements {
            let v = element.vertices;
            let signs = [0, 1, 2].map(|j| if v[j] < v[(j + 1) % 3] { 1.0 } else { -1.0 });
            let triangle =
                MorleyTriangle::new(v.map(|i| [mesh.vertices[i].x, mesh.vertices[i].y]), signs)?;
            let mut dofs = v.to_vec();
            dofs.extend((0..3).map(|j| edges[&edge(v[j], v[(j + 1) % 3])]));
            contributions.push(Contribution {
                dofs,
                stiffness: triangle.stiffness(&bending)?,
            });
            triangles.push(triangle);
        }
        // Unit w characteristic is 1 mm; slopes use 1 mm / mesh edge length.
        // This numerical nondimensionalization is not a physical restraint.
        let mut scales = vec![1.0; mesh.vertices.len()];
        scales.resize(n, 1.0 / mesh.quality.max_edge_mm);
        let mut diagnostics = self.diagnostics.clone();
        diagnostics.push(format!("laminate thickness {} mm; evidence {:?}; supplied Q overrides source metadata, reconcile explicitly", laminate.thickness_mm, laminate.evidence));
        diagnostics.push(format!(
            "mesh {:?}; approximation {:?}; dense storage O(n²), solve O(n³), {n} DOFs",
            mesh.quality, mesh.approximation
        ));
        if laminate
            .evidence
            .as_deref()
            .is_none_or(|s| s.trim().is_empty())
        {
            diagnostics.push("Missing measured/datasheet laminate evidence".into());
        }
        Ok(Discretization {
            mesh,
            triangles,
            edges,
            contributions,
            scales,
            diagnostics,
        })
    }
}

fn edge(a: usize, b: usize) -> (usize, usize) {
    (a.min(b), a.max(b))
}

pub struct Discretization {
    mesh: AnalysisMesh,
    triangles: Vec<MorleyTriangle>,
    edges: BTreeMap<(usize, usize), usize>,
    contributions: Vec<Contribution>,
    scales: Vec<f64>,
    pub diagnostics: Vec<String>,
}

#[derive(Clone, Copy)]
pub enum Restraint {
    SimplySupported,
    Clamped,
}

pub struct RailFixture {
    /// Exact final-mesh boundary subedge indices, not every board-cell edge.
    pub boundary_edges: Vec<usize>,
    pub restraint: Restraint,
}

/// Explicit physical element side. No nearest-node/P1 attachment or snapping.
#[derive(Clone, Copy)]
pub struct Site {
    pub element: usize,
    pub point: Point,
}

pub enum Load {
    /// Uniform transverse pressure over the complete retained panel, N/mm².
    Pressure(f64),
    /// Constant pressure on selected whole triangles. Contact patches must
    /// be resolved by the mesh; partial-element pressure is not approximated.
    ElementPressure { element: usize, pressure: f64 },
    /// Virtual work Fz*w + Mx*dw/dy - My*dw/dx. Units N and N·mm.
    Point {
        site: Site,
        force_z: f64,
        moment_xy: [f64; 2],
    },
}

/// Bilateral, linear normal tooling spring. No unilateral contact inference.
pub struct ToolingSpring {
    pub site: Site,
    pub stiffness_n_mm: f64,
}

pub struct ProcessCase {
    pub name: String,
    pub loads: Vec<Load>,
    pub rails: Vec<RailFixture>,
    pub tooling: Vec<ToolingSpring>,
    /// Measured force/contact/fixture provenance; None is explicitly unvalidated.
    pub evidence: Option<String>,
}

/// Standalone numerical input. Construct pcb_elastic::Model with these scales,
/// contributions and explicit numerical tolerances; evaluate forces/prescribed.
/// No optimizer, solver invocation, export, or manufacturing pass/fail here.
pub struct Problem {
    pub contributions: Vec<Contribution>,
    pub scales: Vec<f64>,
    pub forces: Vec<f64>,
    pub prescribed: Vec<(usize, f64)>,
    pub diagnostics: Vec<String>,
}

impl Discretization {
    /// Immutable geometry snapshot for selecting physical element sides and
    /// boundary subedges. Resolve all indices again after refinement.
    pub fn mesh(&self) -> &AnalysisMesh {
        &self.mesh
    }

    /// Element-local P2 rows [w, dw/dx, dw/dy] and their global DOF indices,
    /// also suitable for user-defined displacement/rotation observations.
    pub fn observation(&self, site: Site) -> Result<(Vec<usize>, DMatrix<f64>), Error> {
        let a = self
            .mesh
            .attach_to_element(site.point, site.element, 0.0)
            .ok_or(Error::Input(
                "attachment must lie on explicitly selected element",
            ))?;
        Ok((
            self.contributions[a.element].dofs.clone(),
            self.triangles[a.element].attachment(a.weights)?.rows,
        ))
    }

    pub fn problem(&self, case: &ProcessCase) -> Result<Problem, Error> {
        let mut contributions = self.contributions.clone();
        let mut forces = vec![0.0; self.scales.len()];
        let mut fixed = BTreeSet::new();
        for fixture in &case.rails {
            if fixture.boundary_edges.is_empty() {
                return Err(Error::Input("empty rail fixture"));
            }
            for &index in &fixture.boundary_edges {
                let boundary = self
                    .mesh
                    .boundary
                    .get(index)
                    .ok_or(Error::Input("unknown boundary edge"))?;
                fixed.extend(boundary.vertices);
                if matches!(fixture.restraint, Restraint::Clamped) {
                    fixed.insert(self.edges[&edge(boundary.vertices[0], boundary.vertices[1])]);
                }
            }
        }
        for spring in &case.tooling {
            if !spring.stiffness_n_mm.is_finite() || spring.stiffness_n_mm <= 0.0 {
                return Err(Error::Input(
                    "tooling stiffness must be positive and finite",
                ));
            }
            let (dofs, rows) = self.observation(spring.site)?;
            let w = rows.row(0);
            let stiffness = w.transpose() * w * spring.stiffness_n_mm;
            if stiffness.iter().any(|x| !x.is_finite()) {
                return Err(Error::Input("tooling stiffness overflow"));
            }
            contributions.push(Contribution { dofs, stiffness });
        }
        for load in &case.loads {
            match load {
                Load::Pressure(p) => {
                    for i in 0..self.triangles.len() {
                        self.add_pressure(i, *p, &mut forces)?;
                    }
                }
                Load::ElementPressure { element, pressure } => {
                    self.add_pressure(*element, *pressure, &mut forces)?
                }
                Load::Point {
                    site,
                    force_z,
                    moment_xy: [mx, my],
                } => {
                    if [*force_z, *mx, *my].iter().any(|x| !x.is_finite()) {
                        return Err(Error::Input("nonfinite point load"));
                    }
                    let (dofs, rows) = self.observation(*site)?;
                    for (j, d) in dofs.into_iter().enumerate() {
                        forces[d] += force_z * rows[(0, j)] - my * rows[(1, j)] + mx * rows[(2, j)];
                    }
                }
            }
        }
        if forces.iter().any(|x| !x.is_finite()) {
            return Err(Error::Input("load accumulation overflow"));
        }
        let mut diagnostics = self.diagnostics.clone();
        diagnostics.push(format!("process {:?}; evidence {:?}; no qualified deformation/compliance limit (0.05 N·mm is NOT an acceptance criterion)", case.name, case.evidence));
        if case.evidence.as_deref().is_none_or(|s| s.trim().is_empty()) {
            diagnostics
                .push("Missing measured process loads and actual fixture characterization".into());
        }
        if case.loads.is_empty() {
            diagnostics.push("No manufacturing load cases supplied".into());
        }
        if case.rails.is_empty() && case.tooling.is_empty() {
            diagnostics.push("No fixtures: expect unsupported rigid modes".into());
        }
        if !case.tooling.is_empty() {
            diagnostics.push("Tooling springs assume maintained bilateral contact; preload, lift-off and compliance need measurement".into());
        }
        Ok(Problem {
            contributions,
            scales: self.scales.clone(),
            forces,
            prescribed: fixed.into_iter().map(|d| (d, 0.0)).collect(),
            diagnostics,
        })
    }

    fn add_pressure(&self, element: usize, p: f64, forces: &mut [f64]) -> Result<(), Error> {
        let triangle = self
            .triangles
            .get(element)
            .ok_or(Error::Input("unknown pressure element"))?;
        let f = triangle.pressure_load(|_| p)?;
        for (j, &d) in self.contributions[element].dofs.iter().enumerate() {
            forces[d] += f[j];
        }
        Ok(())
    }
}
