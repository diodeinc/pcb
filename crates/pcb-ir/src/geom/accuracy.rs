//! How geometry is prepared: the feature size worth keeping and the total
//! boundary approximation allowed.
//!
//! Three thresholds govern prepared geometry and they are independent:
//!
//! - [`tol::EPSILON_MM`](crate::geom::tol::EPSILON_MM) is numerical
//!   coincidence.
//! - [`Resolution::tolerance_mm`] is feature significance and containment
//!   slack.
//! - [`GeometryAccuracy`] bounds accumulated approximation. Contours and
//!   regions carry `uncertainty_mm`, the approximation already present;
//!   every operation that approximates spends from the region's budget and
//!   fails when the total would exceed it.
//!
//! A budget is chosen once, where source curves are prepared into polygons,
//! and inherited by everything derived from that preparation. Polygon round
//! trips cannot recover lost precision, so a coarse preparation is rejected
//! by a finer request instead of being silently reused.
//!
//! # Why significance is not a fraction of accuracy
//!
//! The default significance is 1 µm and the default approximation budget is
//! 10 µm, but their ratio is not a geometric invariant. Significance drops
//! rings whose absolute area is at most `tolerance_mm²`, even for exact
//! polygons with zero uncertainty; that deliberate removal is not charged
//! to the approximation budget. For example, an exact 2 µm square survives
//! a 1 µm tolerance but disappears at 3 µm. Deriving significance as one
//! tenth of accuracy would therefore delete it merely by loosening the
//! budget from 10 µm to 30 µm, with no approximation involved. Dropping a
//! hole ring likewise fills a hole. Containment slack also changes query
//! answers independently of how accurately the boundary was prepared.
//!
//! Existing callers need different policies even with the same budget:
//! Gerber composition uses zero significance to avoid filtering rings;
//! V-score relief uses `DEFAULT_RELIEF_TOLERANCE_MM` (0.01 mm) for relief
//! significance and boundary validation; copper-balance void verification
//! uses 1e-5 mm. The latter is distinct from its 1e-5 mm² mismatch-area
//! limit. Relief lowering and void verification retain these significance
//! tolerances while preserving the caller's accuracy budget.
//! Export's zero significance does not imply zero approximation uncertainty.
//!
//! Neither the allowed budget nor the accumulated uncertainty identifies
//! which small rings are intentional features and which are approximation
//! artefacts. An area cutoff cannot certify topology within an uncertainty
//! band. Keep [`Resolution`] as the pair of caller policy and error budget:
//! deriving one from the other would change semantics, not just simplify
//! error accounting.
//!
//! # What `uncertainty_mm` certifies
//!
//! Every point of a prepared region's boundary lies within `uncertainty_mm`
//! of the boundary of the source geometry that produced it. Flattening and
//! offsets record the chord or join error they introduce; a boolean's
//! boundary consists of pieces of its operands' boundaries and their
//! crossings, so it inherits the larger operand uncertainty plus coordinate
//! rounding. Every operation checks the accumulated total against the
//! region's budget and fails rather than return a region it cannot certify;
//! combining regions prepared at different budgets takes the tighter one.
//!
//! The band is about the *source* boundary, not the boundary of the exactly
//! composed set. Two source features that adjoin or overlap along a curve
//! within their uncertainty can leave a seam or a sliver in the prepared
//! result that the exact composition would not have. Such artefacts only add
//! boundary: distances measured to a prepared region are never larger than
//! the distance to the exact composition, so clearance and width findings
//! derived from them remain conservative. Consumers that need the exact
//! composed topology, rather than distances to source boundaries, must
//! prepare their inputs exactly (zero uncertainty).

use std::fmt;

/// The preparation resolution of a region: significance and accuracy
/// together, chosen once at a preparation entry point and carried by every
/// region derived from it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Resolution {
    /// Minimum significant feature size and containment slack, in
    /// millimetres. Zero keeps every ring and tests containment exactly.
    pub tolerance_mm: f64,
    /// Total accumulated approximation budget.
    pub accuracy: GeometryAccuracy,
}

impl Default for Resolution {
    /// Sub-micrometre significance with the default budget.
    fn default() -> Self {
        Self {
            tolerance_mm: super::tol::REGION_MM,
            accuracy: GeometryAccuracy::default(),
        }
    }
}

impl Resolution {
    pub fn new(tolerance_mm: f64, accuracy: GeometryAccuracy) -> Self {
        Self {
            tolerance_mm,
            accuracy,
        }
    }

    /// The same budget with a different significance tolerance.
    pub fn with_tolerance(self, tolerance_mm: f64) -> Self {
        Self {
            tolerance_mm,
            ..self
        }
    }

    /// The same significance with a different budget.
    pub fn with_accuracy(self, accuracy: GeometryAccuracy) -> Self {
        Self { accuracy, ..self }
    }

    /// Zero significance: every ring survives and containment is exact.
    /// Intermediate compositions use this so significance is applied once,
    /// to the final image.
    pub fn strict(self) -> Self {
        self.with_tolerance(0.0)
    }

    /// The resolution two regions share: this tolerance, the tighter budget.
    pub(crate) fn meet(self, other: Self) -> Self {
        Self {
            tolerance_mm: self.tolerance_mm,
            accuracy: self.accuracy.min(other.accuracy),
        }
    }

    pub(crate) fn is_valid(self) -> bool {
        self.tolerance_mm.is_finite() && self.tolerance_mm >= 0.0
    }
}

/// Budget for accumulated numerical approximation error, in millimetres.
///
/// This accounting does not certify topology or final Hausdorff distance.
/// Independent of feature significance and coincidence. Unmet budgets return
/// an error, including when earlier approximation cannot be refined.
///
/// ```
/// use pcb_ir::geom::{ContourSet, FillRule, GeometryAccuracy, Resolution, shapes};
/// let source = shapes::circle(0.2).unwrap();
/// let resolution = Resolution::new(0.000001, GeometryAccuracy::new(0.0001)?);
/// let region = ContourSet::from_contours(&[source], FillRule::NonZero, resolution)?;
/// let inset = region.disk_erode(0.025)?;
/// assert!(inset.uncertainty_mm <= 0.0005);
/// # Ok::<(), pcb_ir::geom::AccuracyError>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GeometryAccuracy(f64);

impl Default for GeometryAccuracy {
    fn default() -> Self {
        Self::micrometres(10)
    }
}

impl GeometryAccuracy {
    pub fn new(max_error_mm: f64) -> Result<Self, AccuracyError> {
        if !max_error_mm.is_finite() || max_error_mm <= 0.0 {
            return Err(AccuracyError::InvalidBudget(max_error_mm));
        }
        Ok(Self(max_error_mm))
    }

    /// A whole-micrometre budget, usable in constants.
    pub const fn micrometres(max_error_um: u32) -> Self {
        assert!(max_error_um > 0, "an accuracy budget must be positive");
        Self(max_error_um as f64 / 1000.0)
    }

    pub fn max_error_mm(self) -> f64 {
        self.0
    }

    /// The tighter of two budgets.
    pub fn min(self, other: Self) -> Self {
        Self(self.0.min(other.0))
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
        Ok(allocate_error(
            self.remaining(uncertainty_mm)?,
            ErrorAllocation::Operation,
        ))
    }

    pub(crate) fn before_transform(
        self,
        bbox: super::BBox,
        transform: super::Affine2,
    ) -> Result<Self, AccuracyError> {
        let scale = transform.max_scale();
        if !scale.is_finite()
            || scale <= 0.0
            || transform.determinant() == 0.0
            || !transform.m02.is_finite()
            || !transform.m12.is_finite()
        {
            return Err(AccuracyError::InvalidGeometry(
                "singular or non-finite transform",
            ));
        }
        Self::new(self.remaining(numerical_error(bbox.transformed(transform)))? / scale)
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

pub(crate) enum ErrorAllocation {
    Operation,
    CurveConversion,
    ConstructionGuard,
}

/// Allocate approximation targets and construction separation together.
///
/// An operation receives a quarter of the remaining budget, leaving room for
/// later composition and certification. Curve conversion receives an eighth
/// of that operation allowance; chord flattening gets the rest. The balancing
/// construction guard is half the total budget: its two construction offsets
/// each target at most a quarter, separating construction from nominal checks.
/// These fractions interact; increasing the operation share alone can break
/// balancing certification.
///
/// These are targets, not recorded uncertainty. Callers still charge actual
/// conversion/join error and coordinate rounding and check the accumulated
/// total. Spending the full remainder is only appropriate for an explicitly
/// terminal operation after reserving all of its numerical error; it must not
/// replace the allowance used by intermediate or certification operations.
pub(crate) fn allocate_error(budget_mm: f64, allocation: ErrorAllocation) -> f64 {
    match allocation {
        ErrorAllocation::Operation => budget_mm / 4.0,
        ErrorAllocation::CurveConversion => budget_mm / 8.0,
        ErrorAllocation::ConstructionGuard => budget_mm / 2.0,
    }
}

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
