//! Exhaustive, budgeted support selection. See the crate README for proof scope.
use crate::{Analysis, Contribution, DMatrix, DVector, Error, Model, Status};

#[derive(Clone, Debug)]
pub struct Candidate {
    pub id: usize,
    pub contributions: Vec<Contribution>,
}

#[derive(Clone, Debug)]
pub struct LoadCase {
    pub loads: Vec<f64>,
    /// Strictly positive, finite, in the same units as fᵀu. No default limit.
    pub compliance_limit: f64,
}

#[derive(Debug)]
pub struct Selection {
    pub ids: Vec<usize>,
    /// Minimize max(case compliance / case limit), after support count.
    pub objective: f64,
    pub analyses: Vec<Analysis>,
    /// Independently assembled full-system Cholesky responses, not condensation.
    pub verification: Vec<VerifiedResponse>,
}

#[derive(Debug)]
pub struct VerifiedResponse {
    pub displacement: DVector<f64>,
    pub compliance: f64,
    pub scaled_residual_norm: f64,
}

#[derive(Debug)]
pub enum Failure {
    AnalysisStatus(Status),
    AnalysisError(Error),
    IndependentVerification,
}

#[derive(Debug)]
pub struct Unresolved {
    pub ids: Vec<usize>,
    pub load_case: usize,
    pub failure: Failure,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Proof {
    /// Exhaustive optimum of the stated numerical predicate, not real arithmetic.
    ExhaustiveOptimum,
    ExhaustiveInfeasible,
    /// Relevant subsets could not be classified numerically.
    Unresolved,
    BudgetExhausted,
}

#[derive(Debug)]
pub struct Report {
    /// None means no verified feasible set found, not necessarily infeasibility.
    pub selected: Option<Selection>,
    pub proof: Proof,
    /// Includes conflict-rejected subsets; independent of wall-clock speed.
    pub visited_subsets: usize,
    /// Counts all full spectral load-case evaluations (not independent checks).
    pub analyses: usize,
    /// Every smaller cardinality was completely classified infeasible.
    /// Can be n+1 when no subset is feasible. No response lower bound is claimed.
    pub count_lower_bound: usize,
    pub unresolved: Vec<Unresolved>,
}

/// Enumerate increasing cardinalities, then lexicographic sorted IDs. Complete
/// the first feasible layer before stopping. Conflicts are unordered ID pairs.
/// `fixed` prescribes zero displacement; moving fixtures are deliberately outside
/// this compliance objective. Only stable, unique equilibrium is admissible.
/// Nonstable analyses remain unresolved (even compatible singular ones); they
/// cannot certify exclusion or optimality. `max_subsets` is an explicit work cap,
/// including conflict checks, not a hidden convergence heuristic.
pub fn select(
    model: &Model,
    candidates: &[Candidate],
    conflicts: &[(usize, usize)],
    cases: &[LoadCase],
    fixed: &[usize],
    max_subsets: usize,
) -> Result<Report, Error> {
    select_covering(model, candidates, conflicts, cases, fixed, &[], max_subsets)
}

/// Like [`select`], but each required group must contribute at least one
/// candidate ID. Subsets that do not cover every group consume no budget and
/// are not numerically evaluated.
pub fn select_covering(
    model: &Model,
    candidates: &[Candidate],
    conflicts: &[(usize, usize)],
    cases: &[LoadCase],
    fixed: &[usize],
    required_groups: &[Vec<usize>],
    max_subsets: usize,
) -> Result<Report, Error> {
    let n = model.scales.len();
    let mut candidates: Vec<_> = candidates.iter().collect();
    candidates.sort_by_key(|c| c.id);
    if candidates.windows(2).any(|c| c[0].id == c[1].id)
        || cases.is_empty()
        || cases.iter().any(|c| {
            !c.compliance_limit.is_finite()
                || c.compliance_limit <= 0.0
                || c.loads.len() != n
                || c.loads.iter().any(|f| !f.is_finite())
        })
        || fixed
            .iter()
            .enumerate()
            .any(|(i, d)| *d >= n || fixed[..i].contains(d))
        || conflicts.iter().any(|(a, b)| {
            a == b
                || !candidates.iter().any(|c| c.id == *a)
                || !candidates.iter().any(|c| c.id == *b)
        })
        || required_groups
            .iter()
            .flatten()
            .any(|id| !candidates.iter().any(|c| c.id == *id))
    {
        return Err(Error::InvalidInput);
    }
    // Validate even candidates never reached by a short budget or conflicts.
    // Failure here invalidates the input, rather than proving any set infeasible.
    for c in &candidates {
        Model::new(
            model.scales.as_slice().to_vec(),
            &c.contributions,
            model.tolerances,
        )?;
    }
    let prescribed: Vec<_> = fixed.iter().map(|&d| (d, 0.0)).collect();
    let mut report = Report {
        selected: None,
        proof: Proof::BudgetExhausted,
        visited_subsets: 0,
        analyses: 0,
        count_lower_bound: 0,
        unresolved: Vec::new(),
    };
    if required_groups.iter().any(Vec::is_empty) {
        report.proof = Proof::ExhaustiveInfeasible;
        report.count_lower_bound = candidates.len() + 1;
        return Ok(report);
    }
    let group_memberships: Vec<Vec<usize>> = candidates
        .iter()
        .map(|candidate| {
            required_groups
                .iter()
                .enumerate()
                .filter_map(|(group, ids)| ids.contains(&candidate.id).then_some(group))
                .collect()
        })
        .collect();
    for count in 0..=candidates.len() {
        let complete = for_each_covering_combination(
            candidates.len(),
            count,
            &group_memberships,
            required_groups.len(),
            &mut |subset| {
                let ids: Vec<_> = subset.iter().map(|&i| candidates[i].id).collect();
                if report.visited_subsets == max_subsets {
                    return false;
                }
                report.visited_subsets += 1;
                if !conflicts
                    .iter()
                    .any(|(a, b)| ids.contains(a) && ids.contains(b))
                {
                    let supports: Vec<_> = subset
                        .iter()
                        .flat_map(|&i| candidates[i].contributions.iter().cloned())
                        .collect();
                    let mut analyses = Vec::new();
                    let mut verification = Vec::new();
                    let mut objective = f64::NEG_INFINITY;
                    let mut infeasible = false;
                    let mut unresolved = Vec::new();
                    for (load_case, case) in cases.iter().enumerate() {
                        report.analyses += 1;
                        let result = model.evaluate_validated(&supports, &case.loads, &prescribed);
                        match result {
                            Ok(a) if a.status == Status::Stable => {
                                let ratio = a.compliance / case.compliance_limit;
                                if a.compliance > case.compliance_limit {
                                    infeasible = true;
                                } else {
                                    match verify(model, &supports, case, fixed) {
                                        Some(v) => verification.push(v),
                                        None => unresolved.push(Unresolved {
                                            ids: ids.clone(),
                                            load_case,
                                            failure: Failure::IndependentVerification,
                                        }),
                                    }
                                }
                                objective = objective.max(ratio);
                                analyses.push(a);
                            }
                            other => unresolved.push(Unresolved {
                                ids: ids.clone(),
                                load_case,
                                failure: match other {
                                    Ok(a) => Failure::AnalysisStatus(a.status),
                                    Err(e) => Failure::AnalysisError(e),
                                },
                            }),
                        }
                    }
                    // Retain every diagnostic, even if another case proves violation.
                    // Conservatively withhold proof whenever any relevant solve failed.
                    let resolved = unresolved.is_empty();
                    report.unresolved.extend(unresolved);
                    if !infeasible
                        && resolved
                        && report
                            .selected
                            .as_ref()
                            .is_none_or(|s| objective < s.objective)
                    {
                        report.selected = Some(Selection {
                            ids,
                            objective,
                            analyses,
                            verification,
                        });
                    }
                }
                true
            },
        );
        if !complete {
            return Ok(report);
        }
        if report.selected.is_some() {
            report.proof = if report.unresolved.is_empty() {
                Proof::ExhaustiveOptimum
            } else {
                Proof::Unresolved
            };
            return Ok(report);
        }
        if report.unresolved.is_empty() {
            report.count_lower_bound = count + 1;
        }
    }
    report.proof = if report.unresolved.is_empty() {
        Proof::ExhaustiveInfeasible
    } else {
        Proof::Unresolved
    };
    Ok(report)
}

fn for_each_covering_combination(
    candidate_count: usize,
    count: usize,
    memberships: &[Vec<usize>],
    group_count: usize,
    visit: &mut impl FnMut(&[usize]) -> bool,
) -> bool {
    fn recurse(
        candidate_count: usize,
        start: usize,
        slots: usize,
        memberships: &[Vec<usize>],
        covered: &mut [bool],
        subset: &mut Vec<usize>,
        visit: &mut impl FnMut(&[usize]) -> bool,
    ) -> bool {
        if slots == 0 {
            return !covered.iter().all(|covered| *covered) || visit(subset);
        }
        if candidate_count - start < slots {
            return true;
        }

        let missing = covered.iter().filter(|covered| !**covered).count();
        if missing != 0 {
            let max_coverage = (start..candidate_count)
                .map(|candidate| {
                    memberships[candidate]
                        .iter()
                        .filter(|group| !covered[**group])
                        .count()
                })
                .max()
                .unwrap_or(0);
            if max_coverage == 0 || missing.div_ceil(max_coverage) > slots {
                return true;
            }
            if covered.iter().enumerate().any(|(group, is_covered)| {
                !is_covered
                    && !(start..candidate_count)
                        .any(|candidate| memberships[candidate].contains(&group))
            }) {
                return true;
            }
        }

        for candidate in start..=candidate_count - slots {
            let newly_covered: Vec<_> = memberships[candidate]
                .iter()
                .copied()
                .filter(|group| !covered[*group])
                .collect();
            for &group in &newly_covered {
                covered[group] = true;
            }
            subset.push(candidate);
            if !recurse(
                candidate_count,
                candidate + 1,
                slots - 1,
                memberships,
                covered,
                subset,
                visit,
            ) {
                return false;
            }
            subset.pop();
            for group in newly_covered {
                covered[group] = false;
            }
        }
        true
    }

    recurse(
        candidate_count,
        0,
        count,
        memberships,
        &mut vec![false; group_count],
        &mut Vec::with_capacity(count),
        visit,
    )
}

// Separate assembly and direct SPD factorization, without Model::evaluate,
// spectrum, or cached/reduced support operators. Homogeneous Dirichlet only.
fn verify(
    model: &Model,
    supports: &[Contribution],
    case: &LoadCase,
    fixed: &[usize],
) -> Option<VerifiedResponse> {
    let mut k = model.stiffness.clone();
    for s in supports {
        for (i, &a) in s.dofs.iter().enumerate() {
            for (j, &b) in s.dofs.iter().enumerate() {
                k[(a, b)] += 0.5 * (s.stiffness[(i, j)] + s.stiffness[(j, i)]);
            }
        }
    }
    let free: Vec<_> = (0..k.nrows()).filter(|d| !fixed.contains(d)).collect();
    let a = DMatrix::from_fn(free.len(), free.len(), |i, j| {
        k[(free[i], free[j])] * model.scales[free[i]] * model.scales[free[j]]
    });
    let b = DVector::from_iterator(
        free.len(),
        free.iter().map(|&d| case.loads[d] * model.scales[d]),
    );
    let q = if free.is_empty() {
        DVector::zeros(0)
    } else {
        a.clone().cholesky()?.solve(&b)
    };
    let residual = (&a * &q - &b).norm();
    let denominator = a.norm() * q.norm() + b.norm();
    let mut displacement = DVector::zeros(k.nrows());
    for (i, &d) in free.iter().enumerate() {
        displacement[d] = q[i] * model.scales[d];
    }
    let compliance = DVector::from_column_slice(&case.loads).dot(&displacement);
    (displacement.iter().all(|x| x.is_finite())
        && compliance.is_finite()
        && residual.is_finite()
        && denominator.is_finite()
        && residual
            <= model.tolerances.residual_absolute
                + model.tolerances.residual_relative * denominator
        && compliance <= case.compliance_limit)
        .then_some(VerifiedResponse {
            displacement,
            compliance,
            scaled_residual_norm: residual,
        })
}
