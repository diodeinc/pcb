//! Canonical geometric tolerances.
//!
//! All pcb-ir geometry is in millimeters; these constants document the
//! coincidence and source validation thresholds. Numerical approximation
//! uses GeometryAccuracy.

/// Coincidence threshold for points and angles.
pub const EPSILON_MM: f64 = 1e-9;

/// Default minimum significant feature size for regularized regions;
/// used for feature significance and containment slack.
pub const REGION_MM: f64 = 0.001;

/// Absolute slack when checking that arc start/end radii describe the same
/// circle, sized for source-format coordinate precision noise.
pub const ARC_RADIUS_MM: f64 = 1e-4;

/// Coordinate grid of PCB source data: KiCad computes in whole nanometres
/// and the files it writes carry them. Geometry closer than its rounding is
/// coincident, like points within [`EPSILON_MM`]; it spends no accuracy.
pub const SOURCE_GRID_MM: f64 = 1e-6;
