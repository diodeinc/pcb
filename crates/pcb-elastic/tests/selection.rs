use pcb_elastic::selection::{Candidate, Failure, LoadCase, Proof, select, select_covering};
use pcb_elastic::{Contribution, DMatrix, DVector, Error, Model, Status, Tolerances};

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
fn diagonal(id: usize, values: &[f64]) -> Candidate {
    Candidate {
        id,
        contributions: vec![block(DMatrix::from_diagonal(&DVector::from_column_slice(
            values,
        )))],
    }
}
fn case(loads: &[f64], compliance_limit: f64) -> LoadCase {
    LoadCase {
        loads: loads.to_vec(),
        compliance_limit,
    }
}

// Independent enumeration (bitmask, no cardinality iterator), assembly and SPD
// solve. Does not invoke select or Model for the reference answer.
fn oracle(
    base: &DMatrix<f64>,
    candidates: &[Candidate],
    conflicts: &[(usize, usize)],
    cases: &[LoadCase],
    fixed: &[usize],
) -> Option<(Vec<usize>, f64, Vec<DVector<f64>>)> {
    let mut best: Option<(Vec<usize>, f64, Vec<DVector<f64>>)> = None;
    for mask in 0usize..1 << candidates.len() {
        let mut ids: Vec<_> = candidates
            .iter()
            .enumerate()
            .filter(|(i, _)| mask & (1 << i) != 0)
            .map(|(_, c)| c.id)
            .collect();
        ids.sort();
        if conflicts
            .iter()
            .any(|(a, b)| ids.contains(a) && ids.contains(b))
        {
            continue;
        }
        let mut k = base.clone();
        for c in candidates.iter().filter(|c| ids.contains(&c.id)) {
            for s in &c.contributions {
                for i in 0..s.dofs.len() {
                    for j in 0..s.dofs.len() {
                        k[(s.dofs[i], s.dofs[j])] += s.stiffness[(i, j)];
                    }
                }
            }
        }
        // Eliminate homogeneous Dirichlet rows by replacing them with identity.
        for &i in fixed {
            k.row_mut(i).fill(0.0);
            k.column_mut(i).fill(0.0);
            k[(i, i)] = 1.0;
        }
        let factor = k.cholesky().unwrap();
        let mut responses = Vec::new();
        let mut objective = 0.0_f64;
        for c in cases {
            let mut f = DVector::from_column_slice(&c.loads);
            for &i in fixed {
                f[i] = 0.0;
            }
            let u = factor.solve(&f);
            objective = objective.max(f.dot(&u) / c.compliance_limit);
            responses.push(u);
        }
        if objective <= 1.0
            && best.as_ref().is_none_or(|(b, o, _)| {
                ids.len() < b.len()
                    || (ids.len() == b.len() && (objective < *o || (objective == *o && ids < *b)))
            })
        {
            best = Some((ids, objective, responses));
        }
    }
    best
}

fn random(seed: &mut u64) -> f64 {
    *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
    ((*seed >> 32) as u32 as f64) / u32::MAX as f64 - 0.5
}
fn synthetic(n: usize, m: usize, seed: &mut u64) -> (DMatrix<f64>, Vec<Candidate>, Vec<LoadCase>) {
    let a = DMatrix::from_fn(n, n, |_, _| random(seed));
    let base = a.transpose() * a + DMatrix::identity(n, n);
    let candidates = (0..m)
        .map(|id| {
            let v = DVector::from_fn(n, |_, _| random(seed));
            Candidate {
                id: 3 * id + 2,
                contributions: vec![block(&v * v.transpose() * 4.0)],
            }
        })
        .collect();
    let cases = (0..2)
        .map(|_| {
            let f = DVector::from_fn(n, |_, _| random(seed));
            let limit = f.dot(&base.clone().cholesky().unwrap().solve(&f)) * 0.85;
            case(f.as_slice(), limit)
        })
        .collect();
    (base, candidates, cases)
}

#[test]
fn exhaustive_independent_oracle_on_dense_coupled_systems() {
    let mut seed = 1435;
    let mut feasible = 0;
    for trial in 0..32 {
        let (base, mut candidates, cases) = synthetic(4, 8, &mut seed);
        // Distinct global DOF indexing and multiple blocks per candidate.
        candidates[0].contributions.push(Contribution {
            dofs: vec![3, 1],
            stiffness: DMatrix::from_row_slice(2, 2, &[0.2, -0.1, -0.1, 0.3]),
        });
        let conflicts = [(2, 5), (8, 14), (11, 20)];
        let fixed = if trial % 2 == 0 { vec![0] } else { vec![] };
        let expected = oracle(&base, &candidates, &conflicts, &cases, &fixed);
        candidates.reverse(); // input ordering must not choose tie/search order
        let model = Model::new(vec![1.0, 2.0, 0.5, 3.0], &[block(base)], tolerances()).unwrap();
        let result = select(&model, &candidates, &conflicts, &cases, &fixed, usize::MAX).unwrap();
        assert!(result.unresolved.is_empty());
        match expected {
            Some((ids, objective, responses)) => {
                feasible += 1;
                assert_eq!(result.proof, Proof::ExhaustiveOptimum);
                let selected = result.selected.unwrap();
                assert_eq!(selected.ids, ids, "trial {trial}");
                assert_eq!(result.count_lower_bound, ids.len());
                assert!((selected.objective - objective).abs() < 1e-10);
                for ((a, v), u) in selected
                    .analyses
                    .iter()
                    .zip(&selected.verification)
                    .zip(responses)
                {
                    assert!((&a.displacement - &u).norm() < 1e-10);
                    assert!((&v.displacement - &u).norm() < 1e-10);
                    assert!((a.compliance - v.compliance).abs() < 1e-10);
                }
            }
            None => {
                assert_eq!(result.proof, Proof::ExhaustiveInfeasible);
                assert!(result.selected.is_none());
            }
        }
    }
    assert!(feasible > 0);
}

#[test]
fn coupled_replacements_escape_single_exchange_trap_and_count_wins() {
    let model = Model::new(
        vec![1.0; 2],
        &[block(DMatrix::identity(2, 2))],
        tolerances(),
    )
    .unwrap();
    let candidates = [
        diagonal(0, &[2.0, 0.0]),
        diagonal(1, &[0.0, 2.0]),
        diagonal(2, &[4.0, 0.0]),
        diagonal(3, &[0.0, 4.0]),
    ];
    let cases = [case(&[1.0, 0.0], 0.4), case(&[0.0, 1.0], 0.4)];
    // {0,1} is feasible, every one-site exchange violates a conflict, while
    // the coupled move to {2,3} strictly improves both physical responses.
    let conflicts = [(0, 2), (0, 3), (1, 2), (1, 3)];
    let r = select(&model, &candidates, &conflicts, &cases, &[], 100).unwrap();
    assert_eq!(r.proof, Proof::ExhaustiveOptimum);
    assert_eq!(r.selected.unwrap().ids, [2, 3]);
    assert_eq!(r.visited_subsets, 11);
    let mut more = candidates.to_vec();
    more.push(diagonal(4, &[1.6, 1.6]));
    let r = select(&model, &more, &conflicts, &cases, &[], 100).unwrap();
    assert_eq!(r.selected.unwrap().ids, [4]); // worse response, fewer supports
}

#[test]
fn budgets_bounds_ties_and_infeasibility_are_distinct() {
    let model = Model::new(vec![1.0], &[block(DMatrix::identity(1, 1))], tolerances()).unwrap();
    let candidates = [diagonal(9, &[2.0]), diagonal(3, &[2.0])];
    let cases = [case(&[1.0], 0.5)];
    for budget in 0..=3 {
        let r = select(&model, &candidates, &[], &cases, &[], budget).unwrap();
        assert_eq!(r.visited_subsets, budget);
        assert_eq!(r.count_lower_bound, usize::from(budget > 0));
        assert_eq!(
            r.proof,
            if budget == 3 {
                Proof::ExhaustiveOptimum
            } else {
                Proof::BudgetExhausted
            }
        );
        assert_eq!(r.selected.map(|s| s.ids), (budget >= 2).then_some(vec![3]));
    }
    let r = select(&model, &candidates, &[], &[case(&[1.0], 0.1)], &[], 4).unwrap();
    assert_eq!(r.proof, Proof::ExhaustiveInfeasible);
    assert_eq!(r.count_lower_bound, 3);
    let r = select(&model, &[], &[], &[case(&[1.0], 1.0)], &[], 1).unwrap();
    assert_eq!(r.proof, Proof::ExhaustiveOptimum);
    assert_eq!(r.selected.unwrap().ids, []);
}

#[test]
fn singular_statuses_never_prove_exclusion_or_use_projected_compliance() {
    let model = Model::new(vec![1.0; 2], &[], tolerances()).unwrap();
    for loads in [&[0.0, 0.0][..], &[1.0, 0.0][..]] {
        let r = select(
            &model,
            &[diagonal(1, &[2.0, 2.0])],
            &[],
            &[case(loads, 1.0)],
            &[],
            10,
        )
        .unwrap();
        assert_eq!(r.proof, Proof::Unresolved);
        assert_eq!(r.count_lower_bound, 0);
        assert_eq!(r.selected.unwrap().ids, [1]);
        assert_eq!(r.unresolved.len(), 1);
        assert!(matches!(
            r.unresolved[0].failure,
            Failure::AnalysisStatus(Status::SingularCompatible | Status::SingularIncompatible)
        ));
    }
}

#[test]
fn numerical_failure_and_inaccuracy_are_not_infeasibility() {
    let model = Model::new(
        vec![1.0],
        &[block(DMatrix::from_element(1, 1, 4e307))],
        tolerances(),
    )
    .unwrap();
    let r = select_covering(
        &model,
        &[diagonal(1, &[4e307])],
        &[],
        &[case(&[1e308], 1.0)],
        &[],
        &[vec![1]],
        10,
    )
    .unwrap();
    assert_eq!(r.proof, Proof::Unresolved);
    assert!(r.selected.is_none());
    assert!(
        r.unresolved
            .iter()
            .any(|u| matches!(u.failure, Failure::AnalysisError(Error::NumericalFailure)))
    );

    let mut t = tolerances();
    t.residual_absolute = 0.0;
    t.residual_relative = 0.0;
    let model = Model::new(
        vec![1.0; 2],
        &[block(DMatrix::from_row_slice(2, 2, &[2.0, 0.3, 0.3, 1.0]))],
        t,
    )
    .unwrap();
    let r = select(&model, &[], &[], &[case(&[1.0, 0.7], 10.0)], &[], 1).unwrap();
    assert_eq!(r.proof, Proof::Unresolved);
    assert!(matches!(
        r.unresolved[0].failure,
        Failure::AnalysisStatus(Status::Inaccurate) | Failure::IndependentVerification
    ));
}

#[test]
fn invalid_inputs_are_rejected_even_with_zero_search_budget() {
    let model = Model::new(vec![1.0], &[], tolerances()).unwrap();
    for limit in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert!(select(&model, &[], &[], &[case(&[0.0], limit)], &[], 0).is_err());
    }
    let candidates = [diagonal(1, &[1.0]), diagonal(1, &[2.0])];
    assert!(select(&model, &candidates, &[], &[case(&[1.0], 1.0)], &[], 0).is_err());
    assert!(
        select(
            &model,
            &[diagonal(1, &[-1.0])],
            &[],
            &[case(&[1.0], 1.0)],
            &[],
            0
        )
        .is_err()
    );
    assert!(select(&model, &[], &[(1, 2)], &[case(&[1.0], 1.0)], &[], 0).is_err());
    assert!(select(&model, &[], &[], &[case(&[1.0], 1.0)], &[0, 0], 0).is_err());
    assert!(select(&model, &[], &[], &[], &[], 0).is_err());
}

#[test]
fn required_groups_validate_cover_and_overlap_without_charging_skips() {
    let model = Model::new(vec![1.0], &[block(DMatrix::identity(1, 1))], tolerances()).unwrap();
    let candidates = [
        diagonal(1, &[1.0]),
        diagonal(2, &[1.0]),
        diagonal(3, &[1.0]),
    ];
    let cases = [case(&[1.0], 1.0)];

    let report = select_covering(
        &model,
        &candidates,
        &[],
        &cases,
        &[],
        &[vec![1, 2], vec![2, 3]],
        1,
    )
    .unwrap();
    assert_eq!(report.selected.unwrap().ids, [2]);
    assert_eq!(report.visited_subsets, 1);

    assert!(select_covering(&model, &candidates, &[], &cases, &[], &[vec![99]], 0).is_err());
    let empty = select_covering(&model, &candidates, &[], &cases, &[], &[vec![]], 0).unwrap();
    assert_eq!(empty.proof, Proof::ExhaustiveInfeasible);
    assert_eq!(empty.visited_subsets, 0);
    assert_eq!(empty.count_lower_bound, candidates.len() + 1);

    let legacy = select(&model, &candidates, &[], &cases, &[], 2).unwrap();
    let explicit = select_covering(&model, &candidates, &[], &cases, &[], &[], 2).unwrap();
    assert_eq!(legacy.visited_subsets, explicit.visited_subsets);
    assert_eq!(legacy.selected.unwrap().ids, explicit.selected.unwrap().ids);
}

#[test]
fn covering_enumeration_matches_independent_overlapping_group_oracle() {
    let model = Model::new(vec![1.0], &[block(DMatrix::identity(1, 1))], tolerances()).unwrap();
    let candidates: Vec<_> = (0..5).map(|id| diagonal(id, &[1.0])).collect();
    let groups = [vec![0, 1, 2], vec![1, 3], vec![2, 3, 4]];
    let mut covering = Vec::new();
    for count in 0..=candidates.len() {
        for mask in 0usize..1 << candidates.len() {
            let ids: Vec<_> = (0..candidates.len())
                .filter(|candidate| mask & (1 << candidate) != 0)
                .collect();
            if ids.len() == count
                && groups
                    .iter()
                    .all(|group| group.iter().any(|id| ids.contains(id)))
            {
                covering.push(ids);
            }
        }
    }
    covering.sort_by_key(|ids| (ids.len(), ids.clone()));
    let minimum_count = covering[0].len();
    let minimum_layer: Vec<_> = covering
        .iter()
        .filter(|ids| ids.len() == minimum_count)
        .collect();

    let report = select_covering(
        &model,
        &candidates,
        &[],
        &[case(&[1.0], 1.0)],
        &[],
        &groups,
        usize::MAX,
    )
    .unwrap();
    assert_eq!(report.proof, Proof::ExhaustiveOptimum);
    assert_eq!(report.visited_subsets, minimum_layer.len());
    assert_eq!(report.selected.unwrap().ids, *minimum_layer[0]);
}

#[test]
fn many_disjoint_groups_reach_first_covering_subset_with_one_budget() {
    let model = Model::new(vec![1.0], &[block(DMatrix::identity(1, 1))], tolerances()).unwrap();
    let candidates: Vec<_> = (0..40).map(|id| diagonal(id, &[1.0])).collect();
    let groups: Vec<_> = (0..20)
        .map(|group| vec![2 * group, 2 * group + 1])
        .collect();
    let report = select_covering(
        &model,
        &candidates,
        &[],
        &[case(&[1.0], 1.0)],
        &[],
        &groups,
        1,
    )
    .unwrap();

    assert_eq!(report.proof, Proof::BudgetExhausted);
    assert_eq!(report.visited_subsets, 1);
    assert_eq!(report.analyses, 1);
    assert_eq!(report.count_lower_bound, 20);
    assert_eq!(
        report.selected.unwrap().ids,
        (0..20).map(|group| 2 * group).collect::<Vec<_>>()
    );
}

#[test]
fn invalid_conflicting_candidate_is_rejected_before_search() {
    let model = Model::new(vec![1.0], &[block(DMatrix::identity(1, 1))], tolerances()).unwrap();
    let candidates = [diagonal(1, &[2.0]), diagonal(2, &[-1.0])];
    assert!(matches!(
        select(&model, &candidates, &[(1, 2)], &[case(&[1.0], 0.5)], &[], 2,),
        Err(Error::Indefinite(_))
    ));
}

#[test]
fn selected_analysis_matches_public_evaluate() {
    let model = Model::new(vec![0.5, 2.0], &[], tolerances()).unwrap();
    let candidates = [Candidate {
        id: 7,
        contributions: vec![Contribution {
            dofs: vec![1, 0],
            stiffness: DMatrix::from_row_slice(2, 2, &[3.0, -0.4, -0.4, 2.0]),
        }],
    }];
    let loads = [0.7, -1.2];
    let report = select(
        &model,
        &candidates,
        &[],
        &[case(&loads, 10.0)],
        &[],
        usize::MAX,
    )
    .unwrap();
    let selected = report.selected.unwrap();
    let public = model
        .evaluate(&candidates[0].contributions, &loads, &[])
        .unwrap();
    let internal = &selected.analyses[0];
    assert_eq!(internal.status, public.status);
    assert_eq!(internal.displacement, public.displacement);
    assert_eq!(internal.compliance, public.compliance);
    assert_eq!(internal.strain_energy, public.strain_energy);
    assert_eq!(internal.scaled_residual_norm, public.scaled_residual_norm);
    assert_eq!(internal.relative_residual, public.relative_residual);
    assert_eq!(
        internal.unsupported_modes.len(),
        public.unsupported_modes.len()
    );
}

#[test]
fn prevalidated_candidates_still_check_accumulated_stiffness_before_fixtures() {
    let model = Model::new(vec![1.0; 3], &[], tolerances()).unwrap();
    let candidates = [
        diagonal(1, &[1.0, -6e-12, 0.0]),
        diagonal(2, &[0.0, -6e-12, 1.0]),
    ];
    let report = select_covering(
        &model,
        &candidates,
        &[],
        &[case(&[0.0; 3], 1.0)],
        &[0, 1, 2],
        &[vec![1], vec![2]],
        1,
    )
    .unwrap();
    assert!(report.selected.is_none());
    assert_eq!(report.proof, Proof::Unresolved);
    assert!(matches!(
        report.unresolved[0].failure,
        Failure::AnalysisError(Error::Indefinite(_))
    ));
}

#[test]
#[ignore = "release-mode synthetic performance benchmark; run explicitly"]
fn benchmark_synthetic_stiffness() {
    for (dofs, count, budget) in [(12, 20, 2000), (32, 32, 2000), (64, 48, 500)] {
        let (base, candidates, cases) = synthetic(dofs, count, &mut 1435);
        let model = Model::new(vec![1.0; dofs], &[block(base)], tolerances()).unwrap();
        let start = std::time::Instant::now();
        let report = select(&model, &candidates, &[], &cases, &[], budget).unwrap();
        eprintln!(
            "dofs={dofs} candidates={count} loads={} budget={budget} seconds={:.6} visited={} analyses={} proof={:?} lower_bound={} incumbent={:?} unresolved={}",
            cases.len(),
            start.elapsed().as_secs_f64(),
            report.visited_subsets,
            report.analyses,
            report.proof,
            report.count_lower_bound,
            report.selected.as_ref().map(|s| (&s.ids, s.objective)),
            report.unresolved.len()
        );
        assert!(report.unresolved.is_empty());
        assert!(report.visited_subsets <= budget);
        if let Some(s) = report.selected {
            assert!(s.objective <= 1.0);
            assert_eq!(s.verification.len(), cases.len());
        }
    }
}
