//! Geometry primitives shared by the board and STEP stages.

use glam::{DMat4, DVec2, DVec3};

pub(crate) type Vec2 = DVec2;
pub(crate) type Vec3 = DVec3;

/// Rigid transform (rotation + translation) as a 4x4 matrix.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Transform(pub(crate) DMat4);

impl Transform {
    pub(crate) const IDENTITY: Self = Self(DMat4::IDENTITY);

    pub(crate) fn translation(v: Vec3) -> Self {
        Self(DMat4::from_translation(v))
    }

    pub(crate) fn rotation_x(radians: f64) -> Self {
        Self(DMat4::from_rotation_x(radians))
    }

    pub(crate) fn rotation_y(radians: f64) -> Self {
        Self(DMat4::from_rotation_y(radians))
    }

    pub(crate) fn rotation_z(radians: f64) -> Self {
        Self(DMat4::from_rotation_z(radians))
    }

    /// `self` followed by `next` in the local frame (`self * next`).
    pub(crate) fn then(&self, next: &Self) -> Self {
        Self(self.0 * next.0)
    }

    pub(crate) fn from_axes(origin: Vec3, x_axis: Vec3, z_axis: Vec3) -> Self {
        let y_axis = z_axis.cross(x_axis);
        Self(DMat4::from_cols(
            x_axis.extend(0.0),
            y_axis.extend(0.0),
            z_axis.extend(0.0),
            origin.extend(1.0),
        ))
    }

    pub(crate) fn inverse(&self) -> Self {
        Self(self.0.inverse())
    }

    pub(crate) fn point(&self, p: Vec3) -> Vec3 {
        self.0.transform_point3(p)
    }

    pub(crate) fn direction(&self, d: Vec3) -> Vec3 {
        self.0.transform_vector3(d).normalize_or_zero()
    }

    pub(crate) fn origin(&self) -> Vec3 {
        self.0.w_axis.truncate()
    }

    pub(crate) fn scale_translation(mut self, scale: f64) -> Self {
        self.0.w_axis.x *= scale;
        self.0.w_axis.y *= scale;
        self.0.w_axis.z *= scale;
        self
    }

    /// Bitwise total order, so emission order never depends on hash state.
    pub(crate) fn cmp_bits(&self, other: &Self) -> std::cmp::Ordering {
        let a = self.0.to_cols_array();
        let b = other.0.to_cols_array();
        a.iter()
            .zip(b.iter())
            .map(|(x, y)| x.to_bits().cmp(&y.to_bits()))
            .find(|o| *o != std::cmp::Ordering::Equal)
            .unwrap_or(std::cmp::Ordering::Equal)
    }
}

/// Rotate a point the way KiCad's `RotatePoint` does in its y-down board
/// frame: a positive angle turns counter-clockwise as drawn on screen.
pub(crate) fn rotate_kicad(p: Vec2, degrees: f64) -> Vec2 {
    let (s, c) = degrees.to_radians().sin_cos();
    Vec2::new(c * p.x + s * p.y, c * p.y - s * p.x)
}

pub(crate) fn signed_area(points: &[Vec2]) -> f64 {
    let mut twice = 0.0;
    for (i, p) in points.iter().enumerate() {
        let q = points[(i + 1) % points.len()];
        twice += p.x * q.y - q.x * p.y;
    }
    0.5 * twice
}

pub(crate) fn point_in_polygon(p: Vec2, polygon: &[Vec2]) -> bool {
    let mut inside = false;
    let n = polygon.len();
    for i in 0..n {
        let a = polygon[i];
        let b = polygon[(i + 1) % n];
        if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
            inside = !inside;
        }
    }
    inside
}

/// Whether segments `ab` and `cd` cross, ends included.
pub(crate) fn segments_cross(a: Vec2, b: Vec2, c: Vec2, d: Vec2) -> bool {
    let d1 = b - a;
    let d2 = d - c;
    let denom = d1.perp_dot(d2);
    if denom.abs() < 1e-12 {
        return false;
    }
    let w = c - a;
    let t = w.perp_dot(d2) / denom;
    let u = w.perp_dot(d1) / denom;
    (0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u)
}

/// Centre of the circle through three points, or `None` if collinear.
pub(crate) fn circle_center(a: Vec2, b: Vec2, c: Vec2) -> Option<Vec2> {
    let d = 2.0 * (a.x * (b.y - c.y) + b.x * (c.y - a.y) + c.x * (a.y - b.y));
    if d.abs() <= 1e-12 {
        return None;
    }
    let a2 = a.length_squared();
    let b2 = b.length_squared();
    let c2 = c.length_squared();
    Some(Vec2::new(
        (a2 * (b.y - c.y) + b2 * (c.y - a.y) + c2 * (a.y - b.y)) / d,
        (a2 * (c.x - b.x) + b2 * (a.x - c.x) + c2 * (b.x - a.x)) / d,
    ))
}

/// Counter-clockwise sweep from `from` to `to`, in `[0, 2π)`.
pub(crate) fn ccw_sweep(from: f64, to: f64) -> f64 {
    (to - from).rem_euclid(std::f64::consts::TAU)
}

/// Evaluate a cubic Bezier at `t`.
pub(crate) fn bezier_point(p: [Vec2; 4], t: f64) -> Vec2 {
    let u = 1.0 - t;
    p[0] * (u * u * u) + p[1] * (3.0 * u * u * t) + p[2] * (3.0 * u * t * t) + p[3] * (t * t * t)
}
