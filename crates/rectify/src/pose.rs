//! 3D rotation bookkeeping: 24 axis-aligned poses, KiCad import basis, and
//! the stored `rotate (xyz ...)` → rotation-matrix mapping.
//!
//! Naming mirrors `solver.py` so the two implementations can be diffed.

use glam::{DMat3, DVec3};
use serde::{Deserialize, Serialize};

/// Integer Euler angles in degrees (each multiple of 90, range `(-180, 180]`).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct EulerPose {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl EulerPose {
    pub fn new(x: i32, y: i32, z: i32) -> Self {
        Self {
            x: normalize_deg(x),
            y: normalize_deg(y),
            z: normalize_deg(z),
        }
    }

    #[allow(dead_code)] // Useful public API.
    pub fn as_tuple(self) -> (i32, i32, i32) {
        (self.x, self.y, self.z)
    }
}

fn normalize_deg(a: i32) -> i32 {
    let mut v = a.rem_euclid(360);
    if v > 180 {
        v -= 360;
    }
    v
}

/// 3x3 rotation matrix. `glam` stores matrices column-major and applies them
/// to column vectors (`m * v`), matching the algebra used by the solver.
pub type Mat3 = DMat3;

pub fn ident() -> Mat3 {
    Mat3::IDENTITY
}

fn rot_axis(angle_deg: i32, axis: char) -> Mat3 {
    let a = (angle_deg as f64).to_radians();
    // Python solver rounds cos/sin to keep the 24-pose search exactly
    // axis-aligned; mirror that to stay bit-compatible on the rotation keys.
    let c = a.cos().round();
    let s = a.sin().round();
    match axis {
        'x' => Mat3::from_cols_array(&[1.0, 0.0, 0.0, 0.0, c, s, 0.0, -s, c]),
        'y' => Mat3::from_cols_array(&[c, 0.0, -s, 0.0, 1.0, 0.0, s, 0.0, c]),
        _ => Mat3::from_cols_array(&[c, s, 0.0, -s, c, 0.0, 0.0, 0.0, 1.0]),
    }
}

/// KiCad stored `(rotate (xyz ...))` interpretation in the imported model
/// frame. Mirrors `rotation_matrix_kicad_model_frame` in the Python solver:
/// axis map `y -> z`, `z -> y`; signs `(-1, 1, -1)`.
pub fn rotation_matrix_kicad(pose: EulerPose) -> Mat3 {
    let angles = [("x", -pose.x), ("y", pose.y), ("z", -pose.z)];
    let kicad_axis = |a: &str| match a {
        "x" => 'x',
        "y" => 'z',
        _ => 'y',
    };
    let mut m = ident();
    // rotation_order = "xyz" in Python; apply x, then y, then z.
    for (axis, angle) in angles {
        m = rot_axis(angle, kicad_axis(axis)) * m;
    }
    m
}

/// KiCad's raw STEP import basis: `X' = X, Y' = Z, Z' = -Y`.
pub const KICAD_IMPORT_BASIS: Mat3 =
    Mat3::from_cols_array(&[1.0, 0.0, 0.0, 0.0, 0.0, -1.0, 0.0, 1.0, 0.0]);

/// Python solver constants: `KICAD_ROTATION_ORDER = "xyz"`,
/// `KICAD_ROTATION_SIGNS = (-1, 1, -1)`. Re-exposed here so callers that
/// classify poses against `(rotate ...)` use the same basis as Python's
/// `matrix_key_for_pose`.
pub const KICAD_ROTATION_ORDER: [char; 3] = ['x', 'y', 'z'];
pub const KICAD_ROTATION_SIGNS: [i32; 3] = [-1, 1, -1];

/// Rotation matrix from an `EulerPose` under a configurable axis order + signs.
/// Mirrors the Python solver's `rotation_matrix_from_pose`.
pub fn rotation_matrix_from_pose(pose: EulerPose, order: [char; 3], signs: [i32; 3]) -> Mat3 {
    let angles = [
        ('x', pose.x * signs[0]),
        ('y', pose.y * signs[1]),
        ('z', pose.z * signs[2]),
    ];
    let kicad_axis = |a: char| match a {
        'x' => 'x',
        'y' => 'z',
        _ => 'y',
    };
    let mut m = ident();
    for axis in order {
        let angle = angles
            .iter()
            .find(|(k, _)| *k == axis)
            .map(|(_, v)| *v)
            .unwrap_or(0);
        m = rot_axis(angle, kicad_axis(axis)) * m;
    }
    m
}

/// Integer-rounded rotation-matrix key, matching Python's
/// `matrix_key_for_pose`: `np.rint(matrix).astype(np.int8).tobytes()`.
pub fn matrix_key_for_pose(pose: EulerPose, order: [char; 3], signs: [i32; 3]) -> [i8; 9] {
    let m = rotation_matrix_from_pose(pose, order, signs);
    matrix_key(&m)
}

pub fn apply_mat(m: &Mat3, v: [f64; 3]) -> [f64; 3] {
    (*m * DVec3::from_array(v)).to_array()
}

/// The 24 unique axis-aligned rigid rotations, each expressed as an
/// `EulerPose` whose stored `(x, y, z)` KiCad tuple yields that rotation.
pub fn candidate_poses() -> Vec<EulerPose> {
    let mut out = Vec::with_capacity(24);
    let mut seen: std::collections::HashSet<[i8; 9]> = std::collections::HashSet::new();
    for x in [0, 90, 180, 270] {
        for y in [0, 90, 180, 270] {
            for z in [0, 90, 180, 270] {
                let pose = EulerPose::new(x, y, z);
                let m = rotation_matrix_kicad(pose);
                let key = matrix_key(&m);
                if seen.insert(key) {
                    out.push(pose);
                }
            }
        }
    }
    debug_assert_eq!(out.len(), 24, "expected 24 axis-aligned rotations");
    out
}

fn matrix_key(m: &Mat3) -> [i8; 9] {
    let cols = m.to_cols_array();
    let mut k = [0i8; 9];
    for row in 0..3 {
        for col in 0..3 {
            k[row * 3 + col] = cols[col * 3 + row].round() as i8;
        }
    }
    k
}

/// Classification of how two rotations relate under Z-rotation equivalence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotationMatch {
    /// Identical rotation matrices.
    Exact,
    /// Same physical orientation up to a Z-axis rotation (90°, 180°, 270°).
    ZRotation,
    /// Completely different orientations.
    Mismatch,
}

impl RotationMatch {
    /// True when the rotations are equivalent (either exact or Z-rotation).
    pub fn is_equivalent(self) -> bool {
        matches!(self, RotationMatch::Exact | RotationMatch::ZRotation)
    }
}

/// Classify how `predicted` relates to `repo` under KiCad's rotation
/// conventions, allowing Z-rotation equivalence. Used by bench, audit,
/// and solver to avoid duplicating the same logic.
pub fn classify_rotation(predicted: EulerPose, repo: EulerPose) -> RotationMatch {
    let predicted_key = matrix_key_for_pose(predicted, KICAD_ROTATION_ORDER, KICAD_ROTATION_SIGNS);
    let repo_key = matrix_key_for_pose(repo, KICAD_ROTATION_ORDER, KICAD_ROTATION_SIGNS);
    if predicted_key == repo_key {
        return RotationMatch::Exact;
    }
    for &delta_z in &[90, 180, 270] {
        let rotated = EulerPose::new(repo.x, repo.y, repo.z + delta_z);
        let rotated_key = matrix_key_for_pose(rotated, KICAD_ROTATION_ORDER, KICAD_ROTATION_SIGNS);
        if predicted_key == rotated_key {
            return RotationMatch::ZRotation;
        }
    }
    RotationMatch::Mismatch
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exactly_24_unique_poses() {
        assert_eq!(candidate_poses().len(), 24);
    }

    #[test]
    fn identity_pose_is_identity_matrix() {
        let m = rotation_matrix_kicad(EulerPose::new(0, 0, 0));
        assert_eq!(m, ident());
    }

    #[test]
    fn classify_rotation_exact_match() {
        let pose = EulerPose::new(90, 0, 0);
        assert_eq!(classify_rotation(pose, pose), RotationMatch::Exact);
    }

    #[test]
    fn classify_rotation_z_equivalence() {
        // Rotating only around Z should be equivalent.
        let a = EulerPose::new(90, 0, 0);
        let b = EulerPose::new(90, 0, 180);
        assert_eq!(classify_rotation(a, b), RotationMatch::ZRotation);
        assert!(classify_rotation(a, b).is_equivalent());
    }

    #[test]
    fn classify_rotation_mismatch() {
        let a = EulerPose::new(90, 0, 0);
        let b = EulerPose::new(0, 90, 0);
        assert_eq!(classify_rotation(a, b), RotationMatch::Mismatch);
        assert!(!classify_rotation(a, b).is_equivalent());
    }
}
