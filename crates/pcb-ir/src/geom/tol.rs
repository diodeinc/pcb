//! Canonical geometric tolerances.
//!
//! All pcb-ir geometry is in millimeters; these constants document the
//! precision assumptions shared by flattening, boolean composition, and
//! validation. Pass an explicit tolerance where one is semantic (region
//! significance, relief construction); use these as the defaults.

/// Coincidence threshold for points and angles.
pub const EPSILON_MM: f64 = 1e-9;

/// Chord tolerance when flattening curves to polygon rings for boolean
/// composition.
pub const FLATTEN_MM: f64 = 0.005;

/// Chord tolerance for stroke-outline expansion.
pub const STROKE_OUTLINE_MM: f64 = 0.01;

/// Default minimum significant feature size for regularized regions;
/// contours whose area is below `REGION_MM²` are discarded.
pub const REGION_MM: f64 = 0.001;

/// Absolute slack when checking that arc start/end radii describe the same
/// circle, sized for source-format coordinate precision noise.
pub const ARC_RADIUS_MM: f64 = 1e-4;
