use pcb_elastic::{Contribution, DMatrix, DVector, Error, Model, Status, Tolerances, elements};

fn tolerances() -> Tolerances {
    Tolerances {
        rank_relative: 1e-11,
        rank_absolute: 0.0,
        residual_relative: 1e-11,
        residual_absolute: 1e-12,
    }
}
fn block(k: DMatrix<f64>) -> Contribution {
    Contribution {
        dofs: (0..k.nrows()).collect(),
        stiffness: k,
    }
}
fn close(a: f64, b: f64, tolerance: f64) {
    assert!(
        (a - b).abs() < tolerance,
        "{a} != {b}; error {}",
        (a - b).abs()
    );
}
fn isotropic() -> DMatrix<f64> {
    DMatrix::from_row_slice(3, 3, &[1.0, 0.3, 0.0, 0.3, 1.0, 0.0, 0.0, 0.0, 0.35])
}

#[test]
fn beam_cantilever_tip_load_and_rigid_modes() {
    let k = elements::beam(3.0, 7.0).unwrap();
    for v in [[1.0, 0.0, 1.0, 0.0], [0.0, 1.0, 3.0, 1.0]] {
        assert!((&k * DVector::from_column_slice(&v)).norm() < 1e-13);
    }
    let model = Model::new(vec![3.0, 1.0, 3.0, 1.0], &[block(k)], tolerances()).unwrap();
    let result = model
        .evaluate(&[], &[0.0, 0.0, 2.0, 0.0], &[(0, 0.0), (1, 0.0)])
        .unwrap();
    assert_eq!(result.status, Status::Stable);
    close(result.displacement[2], 2.0 * 27.0 / 21.0, 1e-12);
    close(result.displacement[3], 2.0 * 9.0 / 14.0, 1e-12);
    close(result.reactions[0], -2.0, 1e-12);
    close(result.reactions[1], -6.0, 1e-12);
    close(result.compliance, 2.0 * result.strain_energy, 1e-12);
    assert!(result.relative_residual < 1e-14);
    let free = model.evaluate(&[], &[0.0; 4], &[]).unwrap();
    assert_eq!(free.status, Status::SingularCompatible);
    assert_eq!(free.unsupported_modes.len(), 2);
}

#[test]
fn beam_uniform_load_energy_converges_to_analytical_solution() {
    let mut previous = f64::INFINITY;
    for count in [1, 2, 4, 8] {
        let l = 1.0 / count as f64;
        let mut blocks = Vec::new();
        let mut loads = vec![0.0; 2 * (count + 1)];
        for i in 0..count {
            blocks.push(Contribution {
                dofs: (2 * i..2 * i + 4).collect(),
                stiffness: elements::beam(l, 1.0).unwrap(),
            });
            for (j, v) in [l / 2.0, l * l / 12.0, l / 2.0, -l * l / 12.0]
                .iter()
                .enumerate()
            {
                loads[2 * i + j] += v;
            }
        }
        let model = Model::new(vec![1.0; loads.len()], &blocks, tolerances()).unwrap();
        let r = model.evaluate(&[], &loads, &[(0, 0.0), (1, 0.0)]).unwrap();
        assert_eq!(r.status, Status::Stable);
        let error = (r.compliance - 1.0 / 20.0).abs();
        eprintln!("beam n={count}: compliance={} error={error}", r.compliance);
        assert!(error < previous / 10.0);
        previous = error;
        close(r.displacement[2 * count], 1.0 / 8.0, 2e-10);
    }
    assert!(previous < 4e-7);
}

#[test]
fn optional_supports_match_independent_full_cholesky_and_do_not_accumulate() {
    let base = block(DMatrix::from_row_slice(
        3,
        3,
        &[2.0, -2.0, 0.0, -2.0, 5.0, -3.0, 0.0, -3.0, 3.0],
    ));
    let model = Model::new(vec![1.0; 3], std::slice::from_ref(&base), tolerances()).unwrap();
    let supports = [
        Contribution {
            dofs: vec![0],
            stiffness: DMatrix::from_element(1, 1, 4.0),
        },
        Contribution {
            dofs: vec![2],
            stiffness: DMatrix::from_element(1, 1, 7.0),
        },
    ];
    let load = [1.0, -2.0, 3.0];
    for mask in [1, 2, 3, 1, 3, 2] {
        let chosen: Vec<_> = supports
            .iter()
            .enumerate()
            .filter(|(i, _)| mask & (1 << i) != 0)
            .map(|(_, s)| s.clone())
            .collect();
        let mut full = base.stiffness.clone();
        if mask & 1 != 0 {
            full[(0, 0)] += 4.0;
        }
        if mask & 2 != 0 {
            full[(2, 2)] += 7.0;
        }
        let independent = full
            .cholesky()
            .unwrap()
            .solve(&DVector::from_column_slice(&load));
        let r = model.evaluate(&chosen, &load, &[]).unwrap();
        assert_eq!(r.status, Status::Stable);
        assert!((r.displacement - independent).norm() < 1e-12);
        close(r.support_reactions.iter().map(|f| f[0]).sum(), -2.0, 1e-12);
    }
    let r = model.evaluate(&[], &load, &[]).unwrap();
    assert_eq!(r.status, Status::SingularIncompatible);
    assert_eq!(r.unsupported_modes.len(), 1);
    assert!(r.unsupported_modes[0].load_projection.abs() > 1.0);
    assert!(r.residual.norm() > 1.0);
    assert_eq!(r.reactions.norm(), 0.0);
    let balanced = model.evaluate(&[], &[1.0, 0.0, -1.0], &[]).unwrap();
    assert_eq!(balanced.status, Status::SingularCompatible);
    assert!(balanced.residual.norm() < 1e-12);
}

#[test]
fn prescribed_motion_reactions_and_disconnected_modes() {
    let k = DMatrix::from_row_slice(3, 3, &[2.0, -2.0, 0.0, -2.0, 2.0, 0.0, 0.0, 0.0, 0.0]);
    let model = Model::new(vec![1.0; 3], &[block(k)], tolerances()).unwrap();
    let r = model.evaluate(&[], &[0.0, 3.0, 0.0], &[(0, 4.0)]).unwrap();
    close(r.displacement[1], 5.5, 1e-12);
    close(r.reactions[0], -3.0, 1e-12);
    assert_eq!(r.status, Status::SingularCompatible);
    assert_eq!(r.unsupported_modes.len(), 1);
    let r = model
        .evaluate(&[], &[1.0, 0.0, 0.0], &[(0, 1.0), (1, 0.0), (2, 0.0)])
        .unwrap();
    assert_eq!(r.status, Status::Stable);
    close(r.reactions[0], 1.0, 1e-12);
    close(r.reactions[1], -2.0, 1e-12);
    assert_eq!(r.residual.norm(), 0.0);
}

#[test]
fn rejects_invalid_and_indefinite_inputs_without_regularization() {
    let negative = block(DMatrix::from_element(1, 1, -1.0));
    assert!(matches!(
        Model::new(vec![1.0], &[negative], tolerances()),
        Err(Error::Indefinite(_))
    ));
    let asym = block(DMatrix::from_row_slice(2, 2, &[1.0, 0.1, 0.0, 1.0]));
    assert!(matches!(
        Model::new(vec![1.0; 2], &[asym], tolerances()),
        Err(Error::InvalidInput)
    ));
    let model = Model::new(vec![1.0; 2], &[], tolerances()).unwrap();
    assert!(matches!(
        model.evaluate(&[], &[0.0; 2], &[(0, 0.0), (0, 1.0)]),
        Err(Error::InvalidInput)
    ));
    assert!(model.evaluate(&[], &[f64::NAN, 0.0], &[]).is_err());
    assert!(elements::beam(0.0, 1.0).is_err());
    assert!(elements::MorleyTriangle::new([[0.0; 2]; 3], [1.0; 3]).is_err());
    let r = model.evaluate(&[], &[1.0, 0.0], &[]).unwrap();
    assert_eq!(r.status, Status::SingularIncompatible);
    assert_eq!(r.unsupported_modes.len(), 2);
    assert_eq!(r.displacement.norm(), 0.0);
    close(r.scaled_residual_norm, 1.0, 1e-15);
}

#[test]
fn characteristic_scaling_preserves_solution_across_unit_changes() {
    let k = elements::beam(3.0, 7.0).unwrap();
    let f = DVector::from_vec(vec![0.0, 0.0, 2.0, 0.0]);
    let r = Model::new(vec![3.0, 1.0, 3.0, 1.0], &[block(k.clone())], tolerances())
        .unwrap()
        .evaluate(&[], f.as_slice(), &[(0, 0.0), (1, 0.0)])
        .unwrap();
    // u_original = S*u_new, energy invariant. New translations in metres.
    let s = DMatrix::from_diagonal(&DVector::from_vec(vec![1000.0, 1.0, 1000.0, 1.0]));
    let transformed = &s * &k * &s;
    let new_f = &s * f;
    let r2 = Model::new(
        vec![0.003, 1.0, 0.003, 1.0],
        &[block(transformed)],
        tolerances(),
    )
    .unwrap()
    .evaluate(&[], new_f.as_slice(), &[(0, 0.0), (1, 0.0)])
    .unwrap();
    assert!((r.displacement - &s * r2.displacement).norm() < 1e-12);
    close(r.compliance, r2.compliance, 1e-12);
}

fn interpolate(
    vertices: [[f64; 2]; 3],
    signs: [f64; 3],
    field: impl Fn(f64, f64) -> [f64; 3],
) -> DVector<f64> {
    let mut u = DVector::zeros(6);
    for i in 0..3 {
        let [x, y] = vertices[i];
        u[i] = field(x, y)[0];
        let [xx, yy] = vertices[(i + 1) % 3];
        let length = (xx - x).hypot(yy - y);
        let v = field((x + xx) / 2.0, (y + yy) / 2.0);
        u[3 + i] = signs[i] * (v[1] * (yy - y) - v[2] * (xx - x)) / length;
    }
    u
}

#[test]
fn morley_rigid_modes_quadratic_patch_and_attachment_virtual_work() {
    let vertices = [[1.0, 2.0], [3.0, 2.5], [1.5, 5.0]];
    let signs = [1.0, -1.0, 1.0];
    let triangle = elements::MorleyTriangle::new(vertices, signs).unwrap();
    let k = triangle.stiffness(&isotropic()).unwrap();
    let f = triangle.pressure_load(|_| 5.0).unwrap();
    let area = 2.875;
    for (a, b, c) in [(1.0, 0.0, 0.0), (0.0, 1.0, 0.0), (0.0, 0.0, 1.0)] {
        let u = interpolate(vertices, signs, |x, y| [a + b * x + c * y, b, c]);
        assert!((&k * &u).norm() < 1e-12);
        close(
            f.dot(&u),
            5.0 * area * (a + b * 5.5 / 3.0 + c * 9.5 / 3.0),
            1e-12,
        );
    }
    let field = |x: f64, y: f64| {
        [
            x * x + 2.0 * y * y + 3.0 * x * y,
            2.0 * x + 3.0 * y,
            4.0 * y + 3.0 * x,
        ]
    };
    let u = interpolate(vertices, signs, field);
    let curvature = DVector::from_vec(vec![2.0, 4.0, 6.0]);
    close(
        u.dot(&(&k * &u)),
        area * curvature.dot(&(isotropic() * curvature.clone())),
        1e-10,
    );
    let rows = triangle.attachment([0.2, 0.3, 0.5]).unwrap().rows;
    let expected = field(0.2 + 0.9 + 0.75, 0.4 + 0.75 + 2.5);
    assert!((&rows * &u - DVector::from_column_slice(&expected)).norm() < 1e-12);
    let generalized_load = DVector::from_vec(vec![2.0, 3.0, -4.0]);
    close(
        (&rows.transpose() * &generalized_load).dot(&u),
        generalized_load.dot(&DVector::from_column_slice(&expected)),
        1e-12,
    );
    let support =
        rows.transpose() * DMatrix::from_diagonal(&DVector::from_vec(vec![2.0, 3.0, 4.0])) * &rows;
    assert!(u.dot(&(&support * &u)) > 0.0);
    let model = Model::new(vec![1.0; 6], &[block(k.clone())], tolerances()).unwrap();
    let free = model.evaluate(&[], &[0.0; 6], &[]).unwrap();
    assert_eq!(free.unsupported_modes.len(), 3);
    for mode in free.unsupported_modes {
        assert!((&k * mode.displacement).norm() < 1e-12);
    }
    assert_eq!(
        model
            .evaluate(&[block(support)], &[0.0; 6], &[])
            .unwrap()
            .status,
        Status::Stable
    );
}

// Deterministic hand-built square triangulation, with perturbed interior nodes.
// No mesher/importer and no production geometry types or policies.
type PlateFixture = (Model, Vec<f64>, Vec<(usize, f64)>, Vec<[f64; 2]>);

fn plate_mesh(n: usize, thickness: f64) -> PlateFixture {
    use std::collections::BTreeMap;
    let mut points = Vec::new();
    for y in 0..=n {
        for x in 0..=n {
            let perturb = if x > 0 && x < n && y > 0 && y < n {
                0.13 * ((x + y) % 2) as f64
            } else {
                0.0
            };
            points.push([
                (x as f64 + perturb) / n as f64,
                (y as f64 - perturb) / n as f64,
            ]);
        }
    }
    let nv = points.len();
    let mut edges = BTreeMap::new();
    let mut elements = Vec::new();
    for y in 0..n {
        for x in 0..n {
            let a = y * (n + 1) + x;
            for vertices in [[a, a + 1, a + n + 2], [a, a + n + 2, a + n + 1]] {
                let mut dofs = vertices.to_vec();
                let mut signs = [0.0; 3];
                for i in 0..3 {
                    let (a, b) = (vertices[i], vertices[(i + 1) % 3]);
                    let next = nv + edges.len();
                    dofs.push(*edges.entry((a.min(b), a.max(b))).or_insert(next));
                    signs[i] = if a < b { 1.0 } else { -1.0 };
                }
                let triangle =
                    elements::MorleyTriangle::new(vertices.map(|i| points[i]), signs).unwrap();
                elements.push((dofs, triangle));
            }
        }
    }
    let mut loads = vec![0.0; nv + edges.len()];
    let mut blocks = Vec::new();
    for (dofs, triangle) in elements {
        // Exact clamped solution w=g(x)g(y), g=x²(1-x)², q=Δ²w.
        let force = triangle
            .pressure_load(|[x, y]| {
                let g = |t: f64| t * t * (1.0 - t).powi(2);
                let ddg = |t: f64| 2.0 - 12.0 * t + 12.0 * t * t;
                (24.0 * (g(x) + g(y)) + 2.0 * ddg(x) * ddg(y)) * thickness.powi(3)
            })
            .unwrap();
        for (i, &d) in dofs.iter().enumerate() {
            loads[d] += force[i];
        }
        blocks.push(Contribution {
            dofs,
            stiffness: triangle
                .stiffness(&(isotropic() * thickness.powi(3)))
                .unwrap(),
        });
    }
    let mut prescribed = Vec::new();
    for (i, &[x, y]) in points.iter().enumerate() {
        if x == 0.0 || x == 1.0 || y == 0.0 || y == 1.0 {
            prescribed.push((i, 0.0));
        }
    }
    for ((a, b), dof) in edges {
        if (0..2).any(|axis| {
            points[a][axis] == points[b][axis] && (points[a][axis] == 0.0 || points[a][axis] == 1.0)
        }) {
            prescribed.push((dof, 0.0));
        }
    }
    let model = Model::new(vec![1.0; loads.len()], &blocks, tolerances()).unwrap();
    (model, loads, prescribed, points)
}

#[test]
fn morley_clamped_manufactured_plate_refinement_and_thin_limit() {
    // ∫g²=1/630, ∫(g″)²=4/5, ∫(g′)²=2/105.
    // ∫w Δ²w = 2(4/5)(1/630)+2(2/105)².
    let exact = 8.0 / 3150.0 + 8.0 / 11025.0;
    let mut previous = f64::INFINITY;
    for n in [2, 4, 8, 16] {
        let (model, loads, prescribed, points) = plate_mesh(n, 1.0);
        let r = model.evaluate(&[], &loads, &prescribed).unwrap();
        assert_eq!(r.status, Status::Stable);
        let error = (r.compliance - exact).abs();
        let nodal_error = points
            .iter()
            .enumerate()
            .map(|(i, &[x, y])| {
                (r.displacement[i] - x * x * (1.0 - x).powi(2) * y * y * (1.0 - y).powi(2)).powi(2)
            })
            .sum::<f64>()
            .sqrt()
            / (points.len() as f64).sqrt();
        eprintln!(
            "Morley n={n}: compliance={} error={error}, nodal RMS={nodal_error}",
            r.compliance
        );
        assert!(error < previous / 2.0);
        previous = error;
        if n == 16 {
            assert!(nodal_error < 2e-4);
        }
    }
    assert!(previous < 4e-4);
    // No parasitic shear term: normalized response is independent of t³.
    let (model, loads, bc, _) = plate_mesh(2, 1.0);
    let reference = model.evaluate(&[], &loads, &bc).unwrap();
    for t in [0.1, 0.001, 0.00001] {
        let (model, loads, bc, _) = plate_mesh(2, t);
        let r = model.evaluate(&[], &loads, &bc).unwrap();
        assert_eq!(r.status, Status::Stable);
        assert!((&r.displacement - &reference.displacement).norm() < 1e-12);
        close(r.compliance / t.powi(3), reference.compliance, 1e-12);
    }
}

#[test]
fn morley_two_triangle_patch_shares_oriented_edge_and_has_only_three_rigid_modes() {
    let points = [[0.0, 0.0], [2.0, 0.0], [1.7, 1.2], [0.0, 1.0]];
    let field = |x: f64, y: f64| {
        [
            x * x + 2.0 * y * y + 3.0 * x * y,
            2.0 * x + 3.0 * y,
            4.0 * y + 3.0 * x,
        ]
    };
    let mut exact = DVector::zeros(9);
    let mut blocks = Vec::new();
    for (ids, signs, dofs) in [
        ([0, 1, 2], [1.0, 1.0, -1.0], vec![0, 1, 2, 4, 5, 6]),
        ([0, 2, 3], [1.0, 1.0, -1.0], vec![0, 2, 3, 6, 7, 8]),
    ] {
        let vertices = ids.map(|i| points[i]);
        let triangle = elements::MorleyTriangle::new(vertices, signs).unwrap();
        let u = interpolate(vertices, signs, field);
        for (j, &d) in dofs.iter().enumerate() {
            exact[d] = u[j];
        }
        blocks.push(Contribution {
            dofs,
            stiffness: triangle.stiffness(&isotropic()).unwrap(),
        });
    }
    let model = Model::new(vec![1.0; 9], &blocks, tolerances()).unwrap();
    let free = model.evaluate(&[], &[0.0; 9], &[]).unwrap();
    assert_eq!(free.unsupported_modes.len(), 3);
    // Prescribe quadratic field on outer boundary, leave diagonal normal
    // derivative free. Constant moments balance across that internal edge.
    let prescribed: Vec<_> = (0..9).filter(|&i| i != 6).map(|i| (i, exact[i])).collect();
    let r = model.evaluate(&[], &[0.0; 9], &prescribed).unwrap();
    assert_eq!(r.status, Status::Stable);
    assert!((&r.displacement - &exact).norm() < 1e-12);
    close(r.residual[6], 0.0, 1e-12);
}

#[test]
fn rank_cutoff_reports_soft_modes_and_never_masks_unsupported_load() {
    let k = DMatrix::from_diagonal(&DVector::from_vec(vec![1.0, 1e-10, 0.0]));
    let model = Model::new(vec![1.0; 3], &[block(k.clone())], tolerances()).unwrap();
    let r = model.evaluate(&[], &[0.0, 1.0, 0.1], &[]).unwrap();
    assert_eq!(r.status, Status::SingularIncompatible);
    close(r.displacement[1], 1e10, 1e-4);
    close(r.residual[2], -0.1, 1e-15);
    assert_eq!(r.unsupported_modes.len(), 1);
    let mut t = tolerances();
    t.rank_relative = 1e-9;
    let model = Model::new(vec![1.0; 3], &[block(k)], t).unwrap();
    let r = model.evaluate(&[], &[0.0, 1.0, 0.0], &[]).unwrap();
    assert_eq!(r.unsupported_modes.len(), 2);
    assert_eq!(r.status, Status::SingularIncompatible);
    assert_eq!(r.displacement.norm(), 0.0);
    close(r.scaled_residual_norm, 1.0, 1e-15);
}

#[test]
fn accumulated_negative_stiffness_is_rejected_before_constraints() {
    let first = block(DMatrix::from_diagonal(&DVector::from_vec(vec![
        1.0, -6e-12, 0.0,
    ])));
    let second = block(DMatrix::from_diagonal(&DVector::from_vec(vec![
        0.0, -6e-12, 1.0,
    ])));
    // Each negative direction is within the explicit 1e-11 relative cutoff,
    // but their sum exceeds the cutoff of the complete assembled matrix.
    let model = Model::new(vec![1.0; 3], std::slice::from_ref(&first), tolerances()).unwrap();
    Model::new(vec![1.0; 3], std::slice::from_ref(&second), tolerances()).unwrap();
    assert!(matches!(
        Model::new(vec![1.0; 3], &[first, second.clone()], tolerances()),
        Err(Error::Indefinite(_))
    ));
    for prescribed in [vec![], vec![(0, 0.0), (1, 0.0), (2, 0.0)]] {
        assert!(matches!(
            model.evaluate(std::slice::from_ref(&second), &[0.0; 3], &prescribed),
            Err(Error::Indefinite(_))
        ));
    }
}

#[test]
fn finite_tolerance_overflow_is_a_numerical_failure() {
    let mut t = tolerances();
    t.rank_absolute = f64::MAX;
    t.rank_relative = 0.75;
    let k = block(DMatrix::from_element(1, 1, 1e307));
    assert!(matches!(
        Model::new(vec![1.0], std::slice::from_ref(&k), t),
        Err(Error::NumericalFailure)
    ));
    let model = Model::new(vec![1.0], &[], t).unwrap();
    assert!(matches!(
        model.evaluate(&[k], &[0.0], &[(0, 0.0)]),
        Err(Error::NumericalFailure)
    ));

    let mut t = tolerances();
    t.residual_absolute = f64::MAX;
    t.residual_relative = f64::MAX;
    let model = Model::new(vec![1.0], &[block(DMatrix::identity(1, 1))], t).unwrap();
    assert!(matches!(
        model.evaluate(&[], &[1.0], &[]),
        Err(Error::NumericalFailure)
    ));
}

#[test]
fn exact_zero_padding_preserves_eigenmodes_and_does_not_ground_unused_dofs() {
    let model = Model::new(
        vec![1.0; 81],
        &[Contribution {
            dofs: vec![7, 35],
            stiffness: DMatrix::from_row_slice(2, 2, &[4.0, 1.0, 1.0, 2.0]),
        }],
        tolerances(),
    )
    .unwrap();
    let mut loads = vec![0.0; 81];
    loads[7] = 3.0;
    loads[35] = -2.0;
    let result = model.evaluate(&[], &loads, &[]).unwrap();
    assert_eq!(result.status, Status::SingularCompatible);
    assert_eq!(result.unsupported_modes.len(), 79);
    close(result.displacement[7], 8.0 / 7.0, 1e-12);
    close(result.displacement[35], -11.0 / 7.0, 1e-12);
    for mode in &result.unsupported_modes {
        close(mode.displacement[7], 0.0, 1e-12);
        close(mode.displacement[35], 0.0, 1e-12);
        close(mode.displacement.norm(), 1.0, 1e-12);
    }
    loads[60] = 1.0;
    let loaded = model.evaluate(&[], &loads, &[]).unwrap();
    assert_eq!(loaded.status, Status::SingularIncompatible);
    assert!(
        loaded
            .unsupported_modes
            .iter()
            .any(|m| m.load_projection.abs() > 0.9)
    );
}
