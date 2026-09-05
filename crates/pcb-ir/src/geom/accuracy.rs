//! Approximation budgets, independent of coincidence and feature significance.

use std::fmt;

/// Budget for accumulated numerical approximation error, in millimetres.
///
/// This accounting does not certify topology or final Hausdorff distance.
/// Independent of feature significance and coincidence. Unmet budgets return
/// an error, including when earlier approximation cannot be refined.
///
/// ```
/// use pcb_ir::geom::{ContourSet, FillRule, GeometryAccuracy, shapes};
/// let source = shapes::circle(0.2).unwrap();
/// let region = ContourSet::from_contours(
///     &[source], FillRule::NonZero, 0.000001, GeometryAccuracy::new(0.0001)?,
/// )?;
/// let inset = region.disk_erode(0.025, GeometryAccuracy::new(0.0005)?)?;
/// assert!(inset.uncertainty_mm <= 0.0005);
/// # Ok::<(), pcb_ir::geom::AccuracyError>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GeometryAccuracy(f64);

impl Default for GeometryAccuracy {
    fn default() -> Self {
        Self(0.01)
    }
}

impl GeometryAccuracy {
    pub fn new(max_error_mm: f64) -> Result<Self, AccuracyError> {
        if !max_error_mm.is_finite() || max_error_mm <= 0.0 {
            return Err(AccuracyError::InvalidBudget(max_error_mm));
        }
        Ok(Self(max_error_mm))
    }

    pub fn max_error_mm(self) -> f64 {
        self.0
    }

    pub(crate) fn remaining(self, uncertainty_mm: f64) -> Result<f64, AccuracyError> {
        let remaining = self.0 - uncertainty_mm;
        if remaining > 0.0 && uncertainty_mm >= 0.0 {
            Ok(remaining)
        } else {
            Err(AccuracyError::BudgetExceeded {
                requested_mm: self.0,
                uncertainty_mm,
            })
        }
    }

    pub(crate) fn allowance(self, uncertainty_mm: f64) -> Result<f64, AccuracyError> {
        Ok(self.remaining(uncertainty_mm)? / 4.0)
    }

    pub fn check(self, uncertainty_mm: f64) -> Result<(), AccuracyError> {
        if uncertainty_mm >= 0.0 && uncertainty_mm <= self.0 {
            Ok(())
        } else {
            Err(AccuracyError::BudgetExceeded {
                requested_mm: self.0,
                uncertainty_mm,
            })
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum AccuracyError {
    InvalidBudget(f64),
    BudgetExceeded {
        requested_mm: f64,
        uncertainty_mm: f64,
    },
    InvalidGeometry(&'static str),
    SubdivisionLimit,
}

impl fmt::Display for AccuracyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBudget(mm) => write!(
                f,
                "geometry accuracy must be finite and positive, got {mm} mm"
            ),
            Self::BudgetExceeded {
                requested_mm,
                uncertainty_mm,
            } => write!(
                f,
                "accuracy budget {requested_mm} mm cannot be met (uncertainty: {uncertainty_mm} mm)"
            ),
            Self::InvalidGeometry(reason) => write!(f, "cannot prepare geometry: {reason}"),
            Self::SubdivisionLimit => {
                f.write_str("requested geometry accuracy exceeds the subdivision limit")
            }
        }
    }
}

impl std::error::Error for AccuracyError {}

/// Floating arithmetic allowance. Overlay uses an automatic integer grid;
/// the i64 adapter retains the floating point coordinate precision.
pub(crate) fn numerical_error(bbox: super::BBox) -> f64 {
    if bbox.is_empty() {
        return 0.0;
    }
    let extent = (bbox.max.x - bbox.min.x).max(bbox.max.y - bbox.min.y);
    let magnitude = [bbox.min.x, bbox.min.y, bbox.max.x, bbox.max.y]
        .into_iter()
        .map(f64::abs)
        .fold(1.0, f64::max);
    16.0 * extent / (1_u64 << 52) as f64 + 64.0 * f64::EPSILON * magnitude
}
