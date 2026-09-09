//! Small, dense, linear elastic systems. No geometry, fixture or material policy.
//!
//! All quantities use caller-consistent units. With N and mm, displacements are
//! mm, slopes dimensionless, and compliance is N·mm. Mixed DOFs MUST be scaled
//! by explicit characteristic displacements for meaningful rank detection.
//! Singular solutions are minimum-norm representatives in those scaled
//! coordinates, not physically restrained solutions. Inspect status and modes.

pub mod elements;
pub mod selection;
pub use nalgebra::{DMatrix, DVector};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid dimensions, indices, finite values, symmetry, or solver options")]
    InvalidInput,
    #[error("stiffness is not positive semidefinite (scaled eigenvalue {0})")]
    Indefinite(f64),
    #[error("eigendecomposition did not converge or arithmetic overflowed")]
    NumericalFailure,
}

/// An element or optional support in global DOF indices. Repeated indices
/// within one block are invalid; separate blocks may share any DOFs.
#[derive(Clone, Debug)]
pub struct Contribution {
    pub dofs: Vec<usize>,
    pub stiffness: DMatrix<f64>,
}

/// Explicit numerical (not manufacturing) tolerances in scaled coordinates.
#[derive(Clone, Copy, Debug)]
pub struct Tolerances {
    /// Relative eigenvalue cutoff, also used to validate matrix symmetry/PSD.
    pub rank_relative: f64,
    /// Absolute eigenvalue cutoff; zero is allowed. No stiffness is added.
    pub rank_absolute: f64,
    /// Relative/absolute free equilibrium residual acceptance.
    pub residual_relative: f64,
    pub residual_absolute: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Stable,
    SingularCompatible,
    SingularIncompatible,
    /// Requested equilibrium accuracy was not achieved. Inspect modes too.
    Inaccurate,
}

#[derive(Debug)]
pub struct Mode {
    /// Global displacement vector, unit norm before characteristic scaling.
    pub displacement: DVector<f64>,
    pub eigenvalue: f64,
    /// Work of the effective load against this mode.
    pub load_projection: f64,
}

#[derive(Debug)]
pub struct Analysis {
    pub status: Status,
    pub displacement: DVector<f64>,
    /// fᵀu (not twice stored energy when prescribed displacement is nonzero).
    pub compliance: f64,
    pub strain_energy: f64,
    /// Ku-f at prescribed DOFs, zero at all free DOFs.
    pub reactions: DVector<f64>,
    /// Restoring forces -K_support u, in each optional contribution's local
    /// DOF order, in the same order as the input supports.
    pub support_reactions: Vec<DVector<f64>>,
    /// Ku-f at free DOFs, zero at prescribed DOFs, in original units.
    pub residual: DVector<f64>,
    pub scaled_residual_norm: f64,
    pub relative_residual: f64,
    pub unsupported_modes: Vec<Mode>,
}

/// Immutable assembled base; each evaluation assembles only the additional
/// supports then performs a full dense solve. O(n²) storage / O(n³) solve;
/// deliberately no condensation, caching of singular inverses, or hidden pins.
pub struct Model {
    stiffness: DMatrix<f64>,
    scales: DVector<f64>,
    tolerances: Tolerances,
}

impl Model {
    pub fn new(
        scales: Vec<f64>,
        base: &[Contribution],
        tolerances: Tolerances,
    ) -> Result<Self, Error> {
        let t = tolerances;
        if scales.is_empty()
            || scales.iter().any(|x| !x.is_finite() || *x <= 0.0)
            || [
                t.rank_relative,
                t.rank_absolute,
                t.residual_relative,
                t.residual_absolute,
            ]
            .iter()
            .any(|x| !x.is_finite() || *x < 0.0)
            || t.rank_relative <= 0.0
            || t.rank_relative >= 1.0
        {
            return Err(Error::InvalidInput);
        }
        let mut model = Self {
            stiffness: DMatrix::zeros(scales.len(), scales.len()),
            scales: DVector::from_vec(scales),
            tolerances,
        };
        assemble(&mut model.stiffness, base, &model.scales, t)?;
        Ok(model)
    }

    /// Prescribed displacement pairs are physical Dirichlet conditions.
    /// Optional supports are elastic contributions, not implicit constraints.
    pub fn evaluate(
        &self,
        supports: &[Contribution],
        loads: &[f64],
        prescribed: &[(usize, f64)],
    ) -> Result<Analysis, Error> {
        let n = self.scales.len();
        if loads.len() != n || loads.iter().any(|x| !x.is_finite()) {
            return Err(Error::InvalidInput);
        }
        let mut k = self.stiffness.clone();
        assemble(&mut k, supports, &self.scales, self.tolerances)?;
        let mut fixed = vec![false; n];
        let mut u = DVector::zeros(n);
        for &(i, value) in prescribed {
            if i >= n || fixed[i] || !value.is_finite() {
                return Err(Error::InvalidInput);
            }
            fixed[i] = true;
            u[i] = value;
        }
        let free: Vec<_> = (0..n).filter(|&i| !fixed[i]).collect();
        let f = DVector::from_column_slice(loads);
        let effective = &f - &k * &u;
        let a = DMatrix::from_fn(free.len(), free.len(), |i, j| {
            k[(free[i], free[j])] * self.scales[free[i]] * self.scales[free[j]]
        });
        let b = DVector::from_iterator(
            free.len(),
            free.iter().map(|&i| effective[i] * self.scales[i]),
        );
        let mut modes = Vec::new();
        let mut q = DVector::zeros(free.len());
        if !free.is_empty() {
            let eigen = spectrum(a.clone())?;
            let cutoff = cutoff(eigen.eigenvalues.amax(), self.tolerances)?;
            for j in 0..free.len() {
                let value = eigen.eigenvalues[j];
                let v = eigen.eigenvectors.column(j);
                let projection = v.dot(&b);
                if value < -cutoff {
                    return Err(Error::Indefinite(value));
                }
                if value <= cutoff {
                    let mut displacement = DVector::zeros(n);
                    for (i, &dof) in free.iter().enumerate() {
                        displacement[dof] = v[i] * self.scales[dof];
                    }
                    modes.push(Mode {
                        displacement,
                        eigenvalue: value,
                        load_projection: projection,
                    });
                } else {
                    q.axpy(projection / value, &v, 1.0);
                }
            }
        }
        for (i, &dof) in free.iter().enumerate() {
            u[dof] = q[i] * self.scales[dof];
        }
        let imbalance = &k * &u - &f;
        let reactions = DVector::from_fn(n, |i, _| if fixed[i] { imbalance[i] } else { 0.0 });
        let residual = DVector::from_fn(n, |i, _| if fixed[i] { 0.0 } else { imbalance[i] });
        let scaled_residual_norm = (&a * &q - &b).norm();
        let denominator = a.norm() * q.norm() + b.norm();
        let relative_residual = if denominator == 0.0 {
            0.0
        } else {
            scaled_residual_norm / denominator
        };
        let accurate = scaled_residual_norm
            <= tolerance_limit(
                self.tolerances.residual_absolute,
                self.tolerances.residual_relative,
                denominator,
            )?;
        // Compatibility uses load scale, not ||A||||q||: a soft supported
        // direction must not hide a finite load on an unsupported direction.
        let unsupported_load =
            DVector::from_iterator(modes.len(), modes.iter().map(|m| m.load_projection)).norm();
        let compatible = unsupported_load
            <= tolerance_limit(
                self.tolerances.residual_absolute,
                self.tolerances.residual_relative,
                b.norm(),
            )?;
        let status = match (modes.is_empty(), compatible, accurate) {
            (false, false, _) => Status::SingularIncompatible,
            (_, _, false) => Status::Inaccurate,
            (true, _, true) => Status::Stable,
            (false, true, true) => Status::SingularCompatible,
        };
        let support_reactions: Vec<_> = supports
            .iter()
            .map(|s| {
                let local = DVector::from_iterator(s.dofs.len(), s.dofs.iter().map(|&i| u[i]));
                -((&s.stiffness + s.stiffness.transpose()) * 0.5) * local
            })
            .collect();
        let compliance = f.dot(&u);
        let strain_energy = 0.5 * u.dot(&(&k * &u));
        if u.iter()
            .chain(reactions.iter())
            .chain(residual.iter())
            .chain(support_reactions.iter().flat_map(|r| r.iter()))
            .any(|x| !x.is_finite())
            || [
                compliance,
                strain_energy,
                scaled_residual_norm,
                relative_residual,
                denominator,
            ]
            .iter()
            .any(|x| !x.is_finite())
        {
            return Err(Error::NumericalFailure);
        }
        Ok(Analysis {
            status,
            displacement: u,
            compliance,
            strain_energy,
            reactions,
            support_reactions,
            residual,
            scaled_residual_norm,
            relative_residual,
            unsupported_modes: modes,
        })
    }
}

fn spectrum(
    a: DMatrix<f64>,
) -> Result<nalgebra::linalg::SymmetricEigen<f64, nalgebra::Dyn>, Error> {
    if a.iter().any(|x| !x.is_finite()) {
        return Err(Error::NumericalFailure);
    }
    // Optional connections leave many DOFs identically uncoupled. Separate
    // this exact zero block before tridiagonalization: its eigenpairs are known,
    // and feeding the padded low-rank matrix to QR can fail to converge. This
    // removes no small stiffness and supplies no hidden physical constraint.
    let active = (0..a.nrows())
        .filter(|&i| a.row(i).iter().any(|&v| v != 0.0))
        .collect::<Vec<_>>();
    if active.len() < a.nrows() {
        let n = a.nrows();
        let mut result = nalgebra::linalg::SymmetricEigen {
            eigenvalues: DVector::zeros(n),
            eigenvectors: DMatrix::zeros(n, n),
        };
        if !active.is_empty() {
            let reduced = spectrum(DMatrix::from_fn(active.len(), active.len(), |i, j| {
                a[(active[i], active[j])]
            }))?;
            for (j, &value) in reduced.eigenvalues.iter().enumerate() {
                result.eigenvalues[j] = value;
                for (i, &row) in active.iter().enumerate() {
                    result.eigenvectors[(row, j)] = reduced.eigenvectors[(i, j)];
                }
            }
        }
        for (j, row) in (0..n).filter(|i| !active.contains(i)).enumerate() {
            result.eigenvectors[(row, active.len() + j)] = 1.0;
        }
        return Ok(result);
    }
    nalgebra::linalg::SymmetricEigen::try_new(a, f64::EPSILON, 100_000)
        .filter(|e| {
            e.eigenvalues
                .iter()
                .chain(e.eigenvectors.iter())
                .all(|x| x.is_finite())
        })
        .ok_or(Error::NumericalFailure)
}

fn tolerance_limit(absolute: f64, relative: f64, scale: f64) -> Result<f64, Error> {
    let limit = absolute + relative * scale;
    if limit.is_finite() {
        Ok(limit)
    } else {
        Err(Error::NumericalFailure)
    }
}

fn cutoff(scale: f64, t: Tolerances) -> Result<f64, Error> {
    tolerance_limit(t.rank_absolute, t.rank_relative, scale)
}

fn validate_psd(matrix: DMatrix<f64>, t: Tolerances) -> Result<(), Error> {
    let eigen = spectrum(matrix)?;
    let min = eigen.eigenvalues.min();
    if min < -cutoff(eigen.eigenvalues.amax(), t)? {
        Err(Error::Indefinite(min))
    } else {
        Ok(())
    }
}

fn assemble(
    k: &mut DMatrix<f64>,
    blocks: &[Contribution],
    scales: &DVector<f64>,
    t: Tolerances,
) -> Result<(), Error> {
    for block in blocks {
        let m = block.dofs.len();
        if m == 0
            || block.stiffness.shape() != (m, m)
            || block
                .dofs
                .iter()
                .enumerate()
                .any(|(i, &d)| d >= k.nrows() || block.dofs[..i].contains(&d))
            || block.stiffness.iter().any(|x| !x.is_finite())
        {
            return Err(Error::InvalidInput);
        }
        let scaled = DMatrix::from_fn(m, m, |i, j| {
            block.stiffness[(i, j)] * scales[block.dofs[i]] * scales[block.dofs[j]]
        });
        if (&scaled - scaled.transpose()).amax() > cutoff(scaled.amax(), t)? {
            return Err(Error::InvalidInput);
        }
        validate_psd((&scaled + scaled.transpose()) * 0.5, t)?;
        for (i, &di) in block.dofs.iter().enumerate() {
            for (j, &dj) in block.dofs.iter().enumerate() {
                k[(di, dj)] += (block.stiffness[(i, j)] + block.stiffness[(j, i)]) * 0.5;
            }
        }
    }
    if k.iter().any(|x| !x.is_finite()) {
        return Err(Error::NumericalFailure);
    }
    // Block-local roundoff allowances can accumulate beyond the global
    // tolerance. Validate before imposing any physical constraints, including
    // all-fixed evaluations. An unchanged, already validated base needs no work.
    if !blocks.is_empty() {
        validate_psd(
            DMatrix::from_fn(k.nrows(), k.ncols(), |i, j| {
                k[(i, j)] * scales[i] * scales[j]
            }),
            t,
        )?;
    }
    Ok(())
}
