use crate::geom::point::{Mirror, Point};

/// Row-major 2D affine transform:
///
/// ```text
/// | m00 m01 m02 |   | x |
/// | m10 m11 m12 | * | y |
///                   | 1 |
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Affine2 {
    pub m00: f64,
    pub m01: f64,
    pub m02: f64,
    pub m10: f64,
    pub m11: f64,
    pub m12: f64,
}

impl Affine2 {
    pub const IDENTITY: Self = Self {
        m00: 1.0,
        m01: 0.0,
        m02: 0.0,
        m10: 0.0,
        m11: 1.0,
        m12: 0.0,
    };

    pub fn identity() -> Self {
        Self::IDENTITY
    }

    pub fn translation(offset: Point) -> Self {
        Self {
            m02: offset.x,
            m12: offset.y,
            ..Self::IDENTITY
        }
    }

    /// Placement transform: mirror, then uniformly scale, then rotate, then
    /// translate to `center`. This is the standard component/step placement
    /// used by IPC-2581 `Xform` and Gerber load-state commands.
    pub fn placement(center: Point, rotation_degrees: f64, mirror: Mirror, scale: f64) -> Self {
        let sx = if mirror.x { -scale } else { scale };
        let sy = if mirror.y { -scale } else { scale };
        let radians = rotation_degrees.to_radians();
        let cos = radians.cos();
        let sin = radians.sin();

        Self {
            m00: cos * sx,
            m01: -sin * sy,
            m02: center.x,
            m10: sin * sx,
            m11: cos * sy,
            m12: center.y,
        }
    }

    pub fn transform_point(&self, p: Point) -> Point {
        Point::new(
            self.m00 * p.x + self.m01 * p.y + self.m02,
            self.m10 * p.x + self.m11 * p.y + self.m12,
        )
    }

    /// Apply only the linear part of the transform (no translation).
    pub fn transform_vector(&self, v: Point) -> Point {
        Point::new(
            self.m00 * v.x + self.m01 * v.y,
            self.m10 * v.x + self.m11 * v.y,
        )
    }

    pub fn concat(&self, child: Self) -> Self {
        Self {
            m00: self.m00 * child.m00 + self.m01 * child.m10,
            m01: self.m00 * child.m01 + self.m01 * child.m11,
            m02: self.m00 * child.m02 + self.m01 * child.m12 + self.m02,
            m10: self.m10 * child.m00 + self.m11 * child.m10,
            m11: self.m10 * child.m01 + self.m11 * child.m11,
            m12: self.m10 * child.m02 + self.m11 * child.m12 + self.m12,
        }
    }

    pub fn determinant(&self) -> f64 {
        self.m00 * self.m11 - self.m01 * self.m10
    }

    pub fn inverse(&self) -> Option<Self> {
        let det = self.determinant();
        if det == 0.0 || !det.is_finite() {
            return None;
        }
        let inv = 1.0 / det;
        Some(Self {
            m00: self.m11 * inv,
            m01: -self.m01 * inv,
            m02: (self.m01 * self.m12 - self.m11 * self.m02) * inv,
            m10: -self.m10 * inv,
            m11: self.m00 * inv,
            m12: (self.m10 * self.m02 - self.m00 * self.m12) * inv,
        })
    }

    pub fn is_identity(&self) -> bool {
        *self == Self::IDENTITY
    }

    pub fn is_translation(&self) -> bool {
        self.m00 == 1.0 && self.m01 == 0.0 && self.m10 == 0.0 && self.m11 == 1.0
    }

    /// True when the linear part is a similarity (uniform scale + rotation,
    /// possibly mirrored), i.e. circles map to circles.
    pub fn preserves_circles(&self, epsilon: f64) -> bool {
        let col0 = self.m00 * self.m00 + self.m10 * self.m10;
        let col1 = self.m01 * self.m01 + self.m11 * self.m11;
        let dot = self.m00 * self.m01 + self.m10 * self.m11;
        (col0 - col1).abs() <= epsilon && dot.abs() <= epsilon
    }
}
