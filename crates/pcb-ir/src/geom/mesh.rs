//! Constrained, planar P1 analysis meshes of canonical regularized regions.
//!
//! Spade supplies constrained Delaunay triangulation and Delaunay refinement;
//! region topology belongs to `ContourSet`, not to this module. All distances
//! are mm. Constraints may split, but are never replaced by staircase cells.
//! This is a polygon mesh: refinement cannot recover curves or features lost
//! upstream during flattening/regularization. No constitutive law, mechanical
//! feasibility decision, or plate-element shape function is implied.
//!
//! Remeshing with a smaller `max_area_mm2` refines the same polygon domain;
//! compare downstream responses across such meshes separately from geometric
//! approximation error. IDs do not persist across remeshing or region booleans.
//! Consumers of nonconforming elements should use `attachments` or
//! `attach_to_element` to select a physical element side explicitly.

use std::collections::{BTreeMap, BTreeSet};

use spade::{
    AngleLimit, ConstrainedDelaunayTriangulation, Point2, RefinementParameters, Triangulation,
};

use super::{ContourSet, Point};

/// Explicit refinement targets. The vertex budget is shared by all components.
#[derive(Debug, Clone, Copy)]
pub struct MeshOptions {
    pub max_area_mm2: f64,
    /// Must be in [0, 30]. Input corners can prevent this target being met.
    pub min_angle_degrees: f64,
    pub max_additional_vertices: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefinementStatus {
    TargetsMet,
    /// A valid partial mesh, not evidence of physical infeasibility.
    VertexBudgetExhausted,
    /// Refinement stopped, but input constraints prevent the requested quality.
    QualityLimited,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MeshQuality {
    pub max_area_mm2: f64,
    pub min_angle_degrees: f64,
    pub max_edge_mm: f64,
    pub area_mm2: f64,
}

/// Approximation inherited from a region, not a topology or Hausdorff certificate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MeshApproximation {
    pub region_significance_mm: f64,
    /// Prepared boundary uncertainty, with the semantics of `ContourSet::uncertainty_mm`.
    pub region_boundary_uncertainty_mm: f64,
    /// Unknown: preparation accounting does not certify final Hausdorff error.
    pub source_curve_error_bound_mm: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeshElement {
    /// Counterclockwise vertex indices; no mechanical DOFs are prescribed.
    pub vertices: [usize; 3],
    /// Snapshot-local identity from the input region's ring components.
    pub component: usize,
}

/// One oriented subedge of an original input ring segment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MeshBoundaryEdge {
    pub vertices: [usize; 2],
    pub component: usize,
    /// Original index into `ContourSet::rings`, not a reordered ring index.
    pub ring: usize,
    pub segment: usize,
    /// Fractions along that original segment, in its original orientation.
    pub parameters: [f64; 2],
}

#[derive(Debug, Clone, PartialEq)]
pub struct AnalysisMesh {
    pub vertices: Vec<Point>,
    pub elements: Vec<MeshElement>,
    pub boundary: Vec<MeshBoundaryEdge>,
    pub quality: MeshQuality,
    pub approximation: MeshApproximation,
    pub refinement: RefinementStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeshError {
    InvalidOptions,
    InvalidRegion,
    /// Invalid or over-budget inherited boundary approximation, not physical infeasibility.
    Accuracy(String),
    Triangulation(String),
    /// Constraint topology could not be traced back to the input boundaries.
    BoundaryTopology,
    DegenerateElement,
}

impl std::fmt::Display for MeshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "analysis meshing failed: {self:?}")
    }
}

impl std::error::Error for MeshError {}

/// Location in the polygon mesh. Weights are linear scalar interpolation only,
/// not the shape functions of a bending element. IDs are mesh-snapshot-local.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MeshAttachment {
    pub element: usize,
    pub weights: [f64; 3],
    /// Distance from the query to the represented point (zero for interior).
    pub distance_mm: f64,
}

impl AnalysisMesh {
    /// Mesh each canonical material component independently, so even touching
    /// components do not share DOFs. Identical inputs/options reproduce ordering.
    /// The region must have a valid resolution and approximation history within
    /// its own budget. Meshing cannot repair exhausted source geometry accuracy.
    pub fn new(region: &ContourSet, options: MeshOptions) -> Result<Self, MeshError> {
        if !options.max_area_mm2.is_finite()
            || options.max_area_mm2 <= 0.0
            || !options.min_angle_degrees.is_finite()
            || !(0.0..=30.0).contains(&options.min_angle_degrees)
        {
            return Err(MeshError::InvalidOptions);
        }
        if !region.resolution.is_valid()
            || region
                .rings
                .iter()
                .any(|r| r.len() < 3 || r.iter().any(|p| !p[0].is_finite() || !p[1].is_finite()))
        {
            return Err(MeshError::InvalidRegion);
        }
        region
            .budget()
            .check(region.uncertainty_mm)
            .map_err(|error| MeshError::Accuracy(error.to_string()))?;
        let (components, outers) = region.ring_components();
        if components.contains(&usize::MAX) {
            return Err(MeshError::InvalidRegion);
        }
        let mut mesh = Self {
            vertices: Vec::new(),
            elements: Vec::new(),
            boundary: Vec::new(),
            quality: MeshQuality {
                max_area_mm2: 0.0,
                min_angle_degrees: 180.0,
                max_edge_mm: 0.0,
                area_mm2: 0.0,
            },
            approximation: MeshApproximation {
                region_significance_mm: region.tolerance(),
                region_boundary_uncertainty_mm: region.uncertainty_mm,
                source_curve_error_bound_mm: None,
            },
            refinement: RefinementStatus::TargetsMet,
        };
        let mut budget = options.max_additional_vertices;
        let mut budget_exhausted = false;
        for component in 0..outers.len() {
            let mut cdt = ConstrainedDelaunayTriangulation::<Point2<f64>>::new();
            let mut segments = Vec::new();
            let mut original = BTreeSet::new();
            for (ring, points) in region
                .rings
                .iter()
                .enumerate()
                .filter(|(r, _)| components[*r] == component)
            {
                let handles = points
                    .iter()
                    .map(|&[x, y]| cdt.insert(Point2::new(x, y)))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| MeshError::Triangulation(e.to_string()))?;
                for (segment, &a) in handles.iter().enumerate() {
                    let b = handles[(segment + 1) % handles.len()];
                    if a == b || !cdt.can_add_constraint(a, b) || cdt.exists_constraint(a, b) {
                        return Err(MeshError::InvalidRegion);
                    }
                    cdt.add_constraint(a, b);
                    original.insert(a.index());
                    segments.push((ring, segment, a.index(), b.index()));
                }
            }
            let initial = cdt.num_vertices();
            initial
                .checked_add(budget)
                .ok_or(MeshError::InvalidOptions)?;
            let result = cdt.refine(
                RefinementParameters::new()
                    .exclude_outer_faces(true)
                    .with_angle_limit(AngleLimit::from_deg(options.min_angle_degrees))
                    .with_max_allowed_area(options.max_area_mm2)
                    .with_max_additional_vertices(budget),
            );
            budget = budget.saturating_sub(cdt.num_vertices() - initial);
            budget_exhausted |= !result.refinement_complete;
            let excluded: BTreeSet<_> = result
                .excluded_faces
                .into_iter()
                .map(|f| f.index())
                .collect();
            let offset = mesh.vertices.len();
            mesh.vertices.extend(cdt.vertices().map(|v| {
                let p = v.position();
                Point::new(p.x, p.y)
            }));
            for face in cdt
                .inner_faces()
                .filter(|f| !excluded.contains(&f.fix().index()))
            {
                let vertices = face.vertices().map(|v| offset + v.fix().index());
                let [a, b, c] = vertices.map(|v| mesh.vertices[v]);
                let twice_area = cross(b - a, c - a);
                if !twice_area.is_finite() || twice_area <= 0.0 {
                    return Err(MeshError::DegenerateElement);
                }
                let lengths = [a.distance_to(b), b.distance_to(c), c.distance_to(a)];
                let angle = [(b - a, c - a), (a - b, c - b), (a - c, b - c)]
                    .into_iter()
                    .map(|(u, v)| cross(u, v).abs().atan2(dot(u, v)).to_degrees())
                    .fold(180.0, f64::min);
                mesh.quality.min_angle_degrees = mesh.quality.min_angle_degrees.min(angle);
                mesh.quality.max_area_mm2 = mesh.quality.max_area_mm2.max(twice_area / 2.0);
                mesh.quality.area_mm2 += twice_area / 2.0;
                mesh.quality.max_edge_mm =
                    lengths.into_iter().fold(mesh.quality.max_edge_mm, f64::max);
                mesh.elements.push(MeshElement {
                    vertices,
                    component,
                });
            }
            // Refinement inserts degree-two constraint vertices. Trace these
            // chains between original endpoints: identity is topological, not
            // inferred by nearest-boundary tolerances (which fail on thin webs).
            let mut adjacency: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
            for edge in cdt.undirected_edges().filter(|e| e.is_constraint_edge()) {
                let [a, b] = edge.vertices().map(|v| v.fix().index());
                adjacency.entry(a).or_default().push(b);
                adjacency.entry(b).or_default().push(a);
            }
            for (ring, segment, start, end) in segments {
                let mut found = None;
                for &next in adjacency.get(&start).ok_or(MeshError::BoundaryTopology)? {
                    let mut chain = vec![start, next];
                    while !original.contains(chain.last().unwrap()) {
                        let current = *chain.last().unwrap();
                        let neighbours =
                            adjacency.get(&current).ok_or(MeshError::BoundaryTopology)?;
                        if neighbours.len() != 2 || chain.len() > cdt.num_vertices() {
                            return Err(MeshError::BoundaryTopology);
                        }
                        let previous = chain[chain.len() - 2];
                        chain.push(
                            *neighbours
                                .iter()
                                .find(|&&v| v != previous)
                                .ok_or(MeshError::BoundaryTopology)?,
                        );
                    }
                    if chain.last() == Some(&end) {
                        if found.is_some() {
                            return Err(MeshError::BoundaryTopology);
                        }
                        found = Some(chain);
                    }
                }
                let chain = found.ok_or(MeshError::BoundaryTopology)?;
                let a = mesh.vertices[offset + start];
                let delta = mesh.vertices[offset + end] - a;
                for pair in chain.windows(2) {
                    let vertices = [offset + pair[0], offset + pair[1]];
                    let parameters =
                        vertices.map(|v| dot(mesh.vertices[v] - a, delta) / dot(delta, delta));
                    mesh.boundary.push(MeshBoundaryEdge {
                        vertices,
                        component,
                        ring,
                        segment,
                        parameters,
                    });
                }
            }
        }
        // Spade can hit its cap before checking an already satisfactory mesh.
        // Actual quality takes precedence over how the refinement loop stopped.
        mesh.refinement = if mesh.quality.max_area_mm2
            <= options.max_area_mm2 * (1.0 + 32.0 * f64::EPSILON)
            && mesh.quality.min_angle_degrees + 1e-10 >= options.min_angle_degrees
        {
            RefinementStatus::TargetsMet
        } else if budget_exhausted {
            RefinementStatus::VertexBudgetExhausted
        } else {
            RefinementStatus::QualityLimited
        };
        Ok(mesh)
    }

    /// Locate a point, optionally restricted to one component. Exact interior
    /// hits take precedence. For shared edges/vertices the lowest element index
    /// wins. Otherwise project to the nearest triangle within `tolerance_mm`;
    /// the returned distance exposes snapping, including across a polygon's
    /// approximation band. Zero tolerance uses robust orientation of the supplied
    /// floating-point coordinates, not a numerical snapping band. Invalid
    /// queries and points outside that band return None.
    pub fn attach(
        &self,
        point: Point,
        component: Option<usize>,
        tolerance_mm: f64,
    ) -> Option<MeshAttachment> {
        self.attachments(point, component, tolerance_mm)
            .min_by(|a, b| {
                a.distance_mm
                    .total_cmp(&b.distance_mm)
                    .then(a.element.cmp(&b.element))
            })
    }

    /// All incident elements (or elements within the explicit snapping band),
    /// ordered by element index. Nonconforming mechanical elements must choose
    /// a physical side rather than infer continuity from the default tie break.
    pub fn attachments(
        &self,
        point: Point,
        component: Option<usize>,
        tolerance_mm: f64,
    ) -> impl Iterator<Item = MeshAttachment> + '_ {
        self.elements
            .iter()
            .enumerate()
            .filter(move |(_, t)| component.is_none_or(|c| c == t.component))
            .filter_map(move |(element, _)| self.attach_to_element(point, element, tolerance_mm))
    }

    /// Validate a caller-selected element side and compute its interpolation.
    pub fn attach_to_element(
        &self,
        point: Point,
        element: usize,
        tolerance_mm: f64,
    ) -> Option<MeshAttachment> {
        if !point.is_finite() || !tolerance_mm.is_finite() || tolerance_mm < 0.0 {
            return None;
        }
        let triangle = self.elements.get(element)?;
        let mut nearest: Option<MeshAttachment> = None;
        let [a, b, c] = triangle.vertices.map(|v| self.vertices[v]);
        let coord = |p: Point| robust::Coord { x: p.x, y: p.y };
        // Use the triangulator's adaptive orientation predicate for exact signs
        // of represented inputs. An epsilon band would also accept exterior points.
        let areas = [
            robust::orient2d(coord(b), coord(c), coord(point)),
            robust::orient2d(coord(c), coord(a), coord(point)),
            robust::orient2d(coord(a), coord(b), coord(point)),
        ];
        let sum = areas.iter().sum::<f64>();
        if areas.iter().all(|w| w.is_finite() && *w >= 0.0) && sum.is_finite() && sum > 0.0 {
            return Some(MeshAttachment {
                element,
                weights: areas.map(|w| w / sum),
                distance_mm: 0.0,
            });
        }
        if tolerance_mm == 0.0 {
            return None;
        }
        for (i, j) in [(0, 1), (1, 2), (2, 0)] {
            let vertices = [a, b, c];
            let (distance, projection) =
                super::dist::point_segment(point, vertices[i], vertices[j]);
            if distance <= tolerance_mm && nearest.is_none_or(|n| distance < n.distance_mm) {
                let delta = vertices[j] - vertices[i];
                let t = (dot(projection - vertices[i], delta) / dot(delta, delta)).clamp(0.0, 1.0);
                let mut weights = [0.0; 3];
                weights[i] = 1.0 - t;
                weights[j] = t;
                nearest = Some(MeshAttachment {
                    element,
                    weights,
                    distance_mm: distance,
                });
            }
        }
        nearest
    }
}

fn cross(a: Point, b: Point) -> f64 {
    a.x * b.y - a.y * b.x
}
fn dot(a: Point, b: Point) -> f64 {
    a.x * b.x + a.y * b.y
}

#[cfg(test)]
mod tests;
