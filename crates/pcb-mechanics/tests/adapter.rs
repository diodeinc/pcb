use pcb_elastic::{
    DMatrix, DVector, Status, Tolerances,
    selection::{self, LoadCase, Proof},
};
use pcb_ir::geom::{
    BBox, ContourSet, Point, Resolution,
    mesh::{AnalysisMesh, MeshOptions},
};
use pcb_mechanics::{Assembly, Connection, Dof, Patch, Plate};

fn tolerances() -> Tolerances {
    Tolerances {
        rank_relative: 1e-10,
        rank_absolute: 1e-12,
        residual_relative: 1e-9,
        residual_absolute: 1e-10,
    }
}

fn mesh(x: f64, area: f64) -> AnalysisMesh {
    AnalysisMesh::new(
        &ContourSet::rectangle(
            BBox::new(Point::new(x, -0.3), Point::new(x + 2.4, 1.0)),
            Resolution::default(),
        ),
        MeshOptions {
            max_area_mm2: area,
            min_angle_degrees: 25.0,
            max_additional_vertices: 1000,
        },
    )
    .unwrap()
}

fn bending() -> DMatrix<f64> {
    DMatrix::from_row_slice(3, 3, &[2.0, 0.3, 0.0, 0.3, 2.0, 0.0, 0.0, 0.0, 0.85])
}

fn assembly(meshes: &[AnalysisMesh]) -> Assembly {
    Assembly::new(
        &meshes
            .iter()
            .map(|mesh| Plate {
                mesh,
                bending: bending(),
                scales: [0.7, 0.4],
            })
            .collect::<Vec<_>>(),
        tolerances(),
    )
    .unwrap()
}

fn footprint(plate: usize, mesh: &AnalysisMesh) -> Vec<Patch> {
    (0..mesh.elements.len())
        .map(|element| Patch {
            plate,
            element,
            vertices: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        })
        .collect()
}

fn stiffness() -> DMatrix<f64> {
    DMatrix::from_row_slice(3, 3, &[4.0, 0.3, -0.7, 0.3, 2.0, 0.2, -0.7, 0.2, 3.0])
}

fn connection(meshes: &[AnalysisMesh], id: usize, factor: f64) -> Connection {
    Connection {
        id,
        a: footprint(0, &meshes[0]),
        b: footprint(1, &meshes[1]),
        reference: [2.7, 0.15],
        stiffness: stiffness() * factor,
    }
}

fn close(a: &DMatrix<f64>, b: &DMatrix<f64>, tolerance: f64) {
    assert!(
        (a - b).norm() < tolerance * (1.0 + b.norm()),
        "error {}",
        (a - b).norm()
    );
}

// Independent physical polynomial DOFs, including canonical midpoint normals.
fn polynomial(a: &Assembly, meshes: &[AnalysisMesh], c: [f64; 6]) -> DVector<f64> {
    let mut u = DVector::zeros(a.ndofs());
    for (plate, mesh) in meshes.iter().enumerate() {
        for e in &mesh.elements {
            for i in 0..3 {
                let v = e.vertices[i];
                let p = mesh.vertices[v];
                u[a.dof(Dof::Vertex {
                    plate,
                    component: e.component,
                    vertex: v,
                })
                .unwrap()] = c[0]
                    + c[1] * p.x
                    + c[2] * p.y
                    + c[3] * p.x * p.x
                    + c[4] * p.x * p.y
                    + c[5] * p.y * p.y;
                let mut edge = [v, e.vertices[(i + 1) % 3]];
                edge.sort();
                let p = mesh.vertices[edge[0]];
                let q = mesh.vertices[edge[1]];
                let (x, y) = ((p.x + q.x) / 2.0, (p.y + q.y) / 2.0);
                let length = p.distance_to(q);
                u[a.dof(Dof::Slope {
                    plate,
                    component: e.component,
                    edge,
                })
                .unwrap()] = ((q.y - p.y) * (c[1] + 2.0 * c[3] * x + c[4] * y)
                    - (q.x - p.x) * (c[2] + c[4] * x + 2.0 * c[5] * y))
                    / length;
            }
        }
    }
    u
}

#[test]
fn finite_area_projection_and_virtual_work_under_refinement() {
    // Exact rectangular L² projection of a quadratic. Centroid/point evaluation
    // gives a different translation; averaging gradients is not this operator.
    for area in [2.0, 0.3, 0.08] {
        let meshes = [mesh(0.2, area)];
        let a = assembly(&meshes);
        let c = [0.8, -0.4, 0.7, 0.9, -0.6, 0.2];
        let u = polynomial(&a, &meshes, c);
        let reference = [-0.7, 1.4];
        let p = a.port(&footprint(0, &meshes[0]), reference).unwrap();
        let (x, y) = (1.4, 0.35);
        let gx = c[1] + 2.0 * c[3] * x + c[4] * y;
        let gy = c[2] + c[4] * x + 2.0 * c[5] * y;
        let mean = c[0]
            + c[1] * x
            + c[2] * y
            + c[3] * (x * x + 2.4_f64.powi(2) / 12.0)
            + c[4] * x * y
            + c[5] * (y * y + 1.3_f64.powi(2) / 12.0);
        let expected = DVector::from_vec(vec![
            mean + gx * (reference[0] - x) + gy * (reference[1] - y),
            gy,
            -gx,
        ]);
        assert!((&p * &u - &expected).norm() < 1e-10);
        let f = DVector::from_vec(vec![1.2, -0.8, 0.35]);
        assert!((u.dot(&(p.transpose() * &f)) - expected.dot(&f)).abs() < 1e-10);
        // Force/moment resultants against three independent rigid test fields.
        for c in [
            [1.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0, 0.0, 0.0],
        ] {
            let rigid = polynomial(&a, &meshes, c);
            let expected = f[0] * (c[0] + c[1] * reference[0] + c[2] * reference[1]) + f[1] * c[2]
                - f[2] * c[1];
            assert!((rigid.dot(&(p.transpose() * &f)) - expected).abs() < 1e-10);
        }
    }
}

#[test]
fn common_motion_energy_and_rotation_covariance() {
    let meshes = [mesh(0.2, 2.0), mesh(3.1, 2.0)];
    let a = assembly(&meshes);
    let c = connection(&meshes, 7, 1.0);
    let candidate = a.candidate(&c).unwrap();
    let k = &candidate.contributions[0].stiffness;
    let u = polynomial(&a, &meshes, [0.7, -0.4, 1.3, 0.0, 0.0, 0.0]);
    assert!(
        (k * &u).norm() < 1e-9,
        "separated footprints must share a reference"
    );
    let u = DVector::from_iterator(a.ndofs(), (0..a.ndofs()).map(|i| (i as f64 * 1.7).sin()));
    let delta = (a.port(&c.a, c.reference).unwrap() - a.port(&c.b, c.reference).unwrap()) * &u;
    assert!((u.dot(&(k * &u)) - delta.dot(&(&c.stiffness * &delta))).abs() < 1e-9);
    let angle: f64 = 0.63;
    let (s, co) = angle.sin_cos();
    let rotate = |p: [f64; 2]| [co * p[0] - s * p[1], s * p[0] + co * p[1]];
    let mut rotated = meshes.clone();
    for mesh in &mut rotated {
        for p in &mut mesh.vertices {
            let q = rotate([p.x, p.y]);
            *p = Point::new(q[0], q[1]);
        }
    }
    let ar = assembly(&rotated);
    let t = DMatrix::from_row_slice(3, 3, &[1.0, 0.0, 0.0, 0.0, co, -s, 0.0, s, co]);
    let mut cr = connection(&rotated, 7, 1.0);
    cr.reference = rotate(c.reference);
    cr.stiffness = &t * &c.stiffness * t.transpose();
    // Canonical slopes are scalar normal derivatives: simultaneous mesh/normal
    // rotation leaves the global DOF coordinates unchanged.
    close(
        k,
        &ar.candidate(&cr).unwrap().contributions[0].stiffness,
        1e-10,
    );
    let fixed: Vec<_> = (0..a.ndofs()).map(|i| (i, u[i])).collect();
    let e = a
        .model
        .evaluate(&[], &vec![0.0; a.ndofs()], &fixed)
        .unwrap()
        .strain_energy;
    let er = ar
        .model
        .evaluate(&[], &vec![0.0; a.ndofs()], &fixed)
        .unwrap()
        .strain_energy;
    assert!((e - er).abs() < 1e-9);
}

// An independent complete-model path: physical-coordinate Vandermonde inverse,
// positive 3x3 Gauss/Duffy integration, global assembly, and direct Cholesky.
// It does not call MorleyTriangle, Assembly::port, or candidate composition.
fn independent(
    meshes: &[AnalysisMesh],
    a: &Assembly,
    reference: [f64; 2],
) -> (DMatrix<f64>, Vec<DMatrix<f64>>) {
    let mut k = DMatrix::zeros(a.ndofs(), a.ndofs());
    let mut ports = Vec::new();
    for (plate, mesh) in meshes.iter().enumerate() {
        let mut gram = DMatrix::zeros(3, 3);
        let mut rhs = DMatrix::zeros(3, a.ndofs());
        for (element, e) in mesh.elements.iter().enumerate() {
            let points = e.vertices.map(|v| mesh.vertices[v]);
            let mut v = DMatrix::zeros(6, 6);
            for i in 0..3 {
                let p = points[i];
                v.row_mut(i).copy_from(
                    &DMatrix::from_row_slice(
                        1,
                        6,
                        &[1.0, p.x, p.y, p.x * p.x, p.x * p.y, p.y * p.y],
                    )
                    .row(0),
                );
                let j = (i + 1) % 3;
                let (p, q) = if e.vertices[i] < e.vertices[j] {
                    (points[i], points[j])
                } else {
                    (points[j], points[i])
                };
                let (nx, ny) = (
                    (q.y - p.y) / p.distance_to(q),
                    (p.x - q.x) / p.distance_to(q),
                );
                let (x, y) = ((p.x + q.x) / 2.0, (p.y + q.y) / 2.0);
                v.row_mut(3 + i).copy_from(
                    &DMatrix::from_row_slice(
                        1,
                        6,
                        &[0.0, nx, ny, 2.0 * x * nx, y * nx + x * ny, 2.0 * y * ny],
                    )
                    .row(0),
                );
            }
            let inv = v.try_inverse().unwrap();
            let hessian = DMatrix::from_row_slice(
                3,
                6,
                &[
                    0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 0.0,
                    2.0, 0.0,
                ],
            ) * &inv;
            let [p, q, r] = points;
            let jac = (q.x - p.x) * (r.y - p.y) - (q.y - p.y) * (r.x - p.x);
            let ke = hessian.transpose() * bending() * hessian * (jac / 2.0);
            let indices = a.element_dofs(plate, element).unwrap();
            for i in 0..6 {
                for j in 0..6 {
                    k[(indices[i], indices[j])] += ke[(i, j)];
                }
            }
            let rule = [
                (0.5 - (0.6_f64).sqrt() / 2.0, 5.0 / 18.0),
                (0.5, 4.0 / 9.0),
                (0.5 + (0.6_f64).sqrt() / 2.0, 5.0 / 18.0),
            ];
            for (u, wu) in rule {
                for (v, wv) in rule {
                    let x = p.x + u * (q.x - p.x) + (1.0 - u) * v * (r.x - p.x);
                    let y = p.y + u * (q.y - p.y) + (1.0 - u) * v * (r.y - p.y);
                    let w = jac * (1.0 - u) * wu * wv;
                    let plane = DVector::from_vec(vec![1.0, y - reference[1], reference[0] - x]);
                    gram += &plane * plane.transpose() * w;
                    let shape =
                        DMatrix::from_row_slice(1, 6, &[1.0, x, y, x * x, x * y, y * y]) * &inv;
                    for i in 0..3 {
                        for j in 0..6 {
                            rhs[(i, indices[j])] += plane[i] * shape[(0, j)] * w;
                        }
                    }
                }
            }
        }
        ports.push(gram.lu().solve(&rhs).unwrap());
    }
    (k, ports)
}

fn clamp_frame(a: &Assembly, frame: &AnalysisMesh) -> Vec<usize> {
    let mut fixed = Vec::new();
    for edge in &frame.boundary {
        if edge
            .vertices
            .iter()
            .all(|&v| (frame.vertices[v].x - 3.1).abs() < 1e-10)
        {
            for &vertex in &edge.vertices {
                fixed.push(
                    a.dof(Dof::Vertex {
                        plate: 1,
                        component: 0,
                        vertex,
                    })
                    .unwrap(),
                );
            }
            let mut vertices = edge.vertices;
            vertices.sort();
            fixed.push(
                a.dof(Dof::Slope {
                    plate: 1,
                    component: 0,
                    edge: vertices,
                })
                .unwrap(),
            );
        }
    }
    fixed.sort();
    fixed.dedup();
    assert!(fixed.len() >= 3);
    fixed
}

#[test]
fn selection_matches_independent_complete_frame_model_and_does_not_accumulate() {
    let meshes = [mesh(0.2, 1.0), mesh(3.1, 1.0)];
    let a = assembly(&meshes);
    let c = connection(&meshes, 19, 1.0);
    let (base, ports) = independent(&meshes, &a, c.reference);
    let f = ports[0].transpose() * DVector::from_vec(vec![0.4, -0.15, 0.2]);
    // Clamp one end of the actual flexible frame; board is not fixed.
    let fixed = clamp_frame(&a, &meshes[1]);
    let prescribed: Vec<_> = fixed.iter().map(|&d| (d, 0.0)).collect();
    let empty = a.model.evaluate(&[], f.as_slice(), &prescribed).unwrap();
    assert_eq!(empty.status, Status::SingularIncompatible);
    assert!(!empty.unsupported_modes.is_empty());
    let candidates = [
        a.candidate(&c).unwrap(),
        a.candidate(&connection(&meshes, 3, 0.3)).unwrap(),
    ];
    let h = &ports[0] - &ports[1];
    let full = base + h.transpose() * stiffness() * h;
    let free: Vec<_> = (0..a.ndofs()).filter(|d| !fixed.contains(d)).collect();
    let kff = DMatrix::from_fn(free.len(), free.len(), |i, j| full[(free[i], free[j])]);
    let ff = DVector::from_iterator(free.len(), free.iter().map(|&d| f[d]));
    let u = kff.cholesky().unwrap().solve(&ff);
    let compliance = ff.dot(&u);
    let cases = [LoadCase {
        loads: f.as_slice().to_vec(),
        compliance_limit: compliance * 1.1,
    }];
    let report = selection::select(&a.model, &candidates, &[], &cases, &fixed, 4).unwrap();
    let selected = report.selected.unwrap();
    assert_eq!(selected.ids, vec![19]);
    assert_eq!(report.proof, Proof::Unresolved); // empty disconnected set is unresolved
    for (i, &d) in free.iter().enumerate() {
        assert!((selected.analyses[0].displacement[d] - u[i]).abs() < 1e-7);
    }
    assert!((selected.analyses[0].compliance - compliance).abs() < 1e-8);
    let frame_response = &ports[1] * &selected.analyses[0].displacement;
    assert!(
        frame_response.norm() > 1e-3,
        "frame must flex, not act as ground"
    );
    let again = a.model.evaluate(&[], f.as_slice(), &prescribed).unwrap();
    assert_eq!(again.status, empty.status);
    assert!((again.displacement - empty.displacement).norm() < 1e-12);
    let budget = selection::select(&a.model, &candidates, &[], &cases, &fixed, 1).unwrap();
    assert_eq!(budget.proof, Proof::BudgetExhausted);
    assert!(budget.selected.is_none());
    assert_eq!(budget.unresolved.len(), 1);
    let unsupported = a
        .model
        .evaluate(&candidates[0].contributions, f.as_slice(), &[])
        .unwrap();
    assert_eq!(unsupported.status, Status::SingularIncompatible);
}

#[test]
fn finite_footprint_response_converges_to_uniform_cantilever_solution() {
    // ν=0 makes the rectangular Kirchhoff plate's uniform-y solution exactly
    // the 1D cantilever solution D w''''=q. No beam element is used here.
    // The port load at the frame centroid is uniform pressure. The floating
    // board undergoes rigid motion; its extra compliance is exactly F²/Cww.
    // Integrating w=q*x²*(6L²-4Lx+x²)/(24D) gives F² L³/(20 D width).
    let exact = 1.0 / 4.0 + 2.4_f64.powi(3) / (20.0 * 2.0 * 1.3);
    let mut errors = Vec::new();
    for area in [0.5, 0.125, 0.03125] {
        let meshes = [mesh(0.2, 2.0), mesh(3.1, area)];
        let a = Assembly::new(
            &meshes
                .iter()
                .map(|mesh| Plate {
                    mesh,
                    bending: DMatrix::from_diagonal(&DVector::from_vec(vec![2.0, 2.0, 1.0])),
                    scales: [0.7, 0.4],
                })
                .collect::<Vec<_>>(),
            tolerances(),
        )
        .unwrap();
        let mut c = connection(&meshes, 1, 1.0);
        c.reference = [4.3, 0.35];
        c.stiffness = DMatrix::from_diagonal(&DVector::from_vec(vec![4.0, 2.0, 3.0]));
        let f = a.port(&c.a, c.reference).unwrap().row(0).transpose();
        let fixed: Vec<_> = clamp_frame(&a, &meshes[1])
            .into_iter()
            .map(|d| (d, 0.0))
            .collect();
        let response = a
            .model
            .evaluate(
                &a.candidate(&c).unwrap().contributions,
                f.as_slice(),
                &fixed,
            )
            .unwrap();
        assert_eq!(response.status, Status::Stable);
        errors.push((response.compliance - exact).abs());
        eprintln!(
            "area={area}, dofs={}, compliance={}, exact={exact}, error={}",
            a.ndofs(),
            response.compliance,
            errors.last().unwrap()
        );
    }
    assert!(errors.windows(2).all(|e| e[1] < 0.7 * e[0]), "{errors:?}");
    assert!(errors[2] < 0.01 * exact, "{errors:?}");
}

#[test]
fn partial_patch_partition_and_reference_transport() {
    let meshes = [mesh(0.2, 2.0)];
    let a = assembly(&meshes);
    let vertices = [[0.8, 0.1, 0.1], [0.1, 0.7, 0.2], [0.2, 0.2, 0.6]];
    let patch = Patch {
        plate: 0,
        element: 0,
        vertices,
    };
    let center = [0, 1, 2].map(|i| vertices.iter().map(|v| v[i]).sum::<f64>() / 3.0);
    let pieces: Vec<_> = (0..3)
        .map(|i| Patch {
            vertices: [vertices[i], vertices[(i + 1) % 3], center],
            ..patch.clone()
        })
        .collect();
    let reference = [0.4, 0.2];
    let p = a.port(&[patch], reference).unwrap();
    close(&p, &a.port(&pieces, reference).unwrap(), 1e-11);
    let far = [1e8, -2e8];
    let t = DMatrix::from_row_slice(
        3,
        3,
        &[
            1.0,
            far[1] - reference[1],
            reference[0] - far[0],
            0.0,
            1.0,
            0.0,
            0.0,
            0.0,
            1.0,
        ],
    );
    close(&a.port(&pieces, far).unwrap(), &(t * p), 1e-11);
}

#[test]
fn invalid_footprints_and_hidden_negative_stiffness_are_rejected() {
    let meshes = [mesh(0.2, 2.0), mesh(3.1, 2.0)];
    let a = assembly(&meshes);
    let mut c = connection(&meshes, 1, 1.0);
    assert!(a.port(&[], [0.0, 0.0]).is_err());
    let mut bad = c.a[0].clone();
    bad.vertices = [[1.0, 0.0, 0.0]; 3];
    assert!(a.port(&[bad], [0.0, 0.0]).is_err());
    let mut mixed = c.a.clone();
    mixed.push(c.b[0].clone());
    assert!(a.port(&mixed, [0.0, 0.0]).is_err());
    c.b = c.a.clone();
    c.stiffness = -DMatrix::identity(3, 3);
    assert!(
        a.candidate(&c).is_err(),
        "zero pullback must not hide negative material stiffness"
    );
}
