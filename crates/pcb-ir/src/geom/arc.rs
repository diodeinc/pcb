use crate::geom::affine::Affine2;
use crate::geom::bbox::BBox;
use crate::geom::point::Point;
use crate::geom::tol;

/// A circular arc from `start` to `end` around `center`.
///
/// A zero-length chord with a positive radius denotes a full circle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Arc {
    pub start: Point,
    pub end: Point,
    pub center: Point,
    pub clockwise: bool,
}

impl Arc {
    pub fn new(start: Point, end: Point, center: Point, clockwise: bool) -> Self {
        Self {
            start,
            end,
            center,
            clockwise,
        }
    }

    pub fn radius(&self) -> f64 {
        self.start.distance_to(self.center)
    }

    pub fn is_full_circle(&self) -> bool {
        self.start.distance_to(self.end) <= 1e-9 && self.radius() > 1e-9
    }

    /// Arc sweep in `[0, 2π]`, measured along the arc direction.
    pub fn sweep_radians(&self) -> f64 {
        if self.is_full_circle() {
            return std::f64::consts::TAU;
        }

        let start_angle = self.start.angle_from(self.center);
        let end_angle = self.end.angle_from(self.center);
        if self.clockwise {
            normalize_angle(start_angle - end_angle)
        } else {
            normalize_angle(end_angle - start_angle)
        }
    }

    /// Tight bounding box of the arc, using the larger of the two endpoint
    /// radii for axis extremes so slightly non-circular source data stays
    /// covered.
    pub fn bbox(&self) -> BBox {
        let mut bbox = BBox::from_point(self.start);
        bbox.include_point(self.end);

        let radius = self
            .start
            .distance_to(self.center)
            .max(self.end.distance_to(self.center));
        if radius <= 0.0 {
            return bbox;
        }

        let start_angle = self.start.angle_from(self.center);
        let end_angle = self.end.angle_from(self.center);
        for angle in [
            0.0,
            std::f64::consts::FRAC_PI_2,
            std::f64::consts::PI,
            std::f64::consts::PI * 1.5,
        ] {
            if angle_is_on_arc(start_angle, end_angle, angle, self.clockwise) {
                bbox.include_point(Point::new(
                    self.center.x + radius * angle.cos(),
                    self.center.y + radius * angle.sin(),
                ));
            }
        }
        bbox
    }

    pub fn reversed(&self) -> Self {
        Self {
            start: self.end,
            end: self.start,
            center: self.center,
            clockwise: !self.clockwise,
        }
    }

    pub fn point_at(&self, angle: f64) -> Point {
        let radius = self.radius();
        Point::new(
            self.center.x + radius * angle.cos(),
            self.center.y + radius * angle.sin(),
        )
    }

    /// The same arc as an elliptical arc with circular axes, the form every
    /// affine transform preserves.
    pub fn to_elliptical(&self) -> EllipticalArc {
        let radius = self.radius();
        EllipticalArc {
            start: self.start,
            end: self.end,
            center: self.center,
            x_axis: Point::new(radius, 0.0),
            y_axis: Point::new(0.0, radius),
            clockwise: self.clockwise,
        }
    }
}

/// An arc of the affine image of the unit circle: the points
/// `center + x_axis·cos θ + y_axis·sin θ` between `start` and `end`.
///
/// The axes are the images of the unit x and y vectors and need not be
/// orthogonal; every affine transform of an arc is again an arc of this
/// form, which keeps the IR closed under placement. `clockwise` is the
/// geometric winding of travel, as for [`Arc`]. A zero-length chord denotes
/// the full ellipse.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EllipticalArc {
    pub start: Point,
    pub end: Point,
    pub center: Point,
    pub x_axis: Point,
    pub y_axis: Point,
    pub clockwise: bool,
}

impl EllipticalArc {
    /// Signed area of the parallelogram the axes span; its sign says whether
    /// increasing the parameter travels counter-clockwise.
    fn basis_orientation(&self) -> f64 {
        self.x_axis.x * self.y_axis.y - self.x_axis.y * self.y_axis.x
    }

    /// Whether increasing the parameter travels in this arc's direction.
    fn parameter_increases(&self) -> bool {
        (self.basis_orientation() >= 0.0) != self.clockwise
    }

    /// Whether the axes span no area at coincidence scale.
    pub fn is_degenerate(&self) -> bool {
        self.basis_orientation().abs() <= tol::EPSILON_MM * tol::EPSILON_MM
    }

    pub fn is_full_ellipse(&self) -> bool {
        self.start.distance_to(self.end) <= tol::EPSILON_MM && !self.is_degenerate()
    }

    /// The parameter of a point on the ellipse.
    pub fn angle_of(&self, point: Point) -> f64 {
        let delta = point - self.center;
        if self.is_degenerate() {
            return 0.0;
        }
        let det = self.basis_orientation();
        // Solve [x_axis y_axis]·(cos θ, sin θ) = delta.
        let cos = (delta.x * self.y_axis.y - delta.y * self.y_axis.x) / det;
        let sin = (self.x_axis.x * delta.y - self.x_axis.y * delta.x) / det;
        sin.atan2(cos)
    }

    pub fn start_angle(&self) -> f64 {
        self.angle_of(self.start)
    }

    /// Parametric sweep in `[0, 2π]`, measured along the arc direction.
    pub fn sweep_radians(&self) -> f64 {
        if self.is_full_ellipse() {
            return std::f64::consts::TAU;
        }
        let start = self.start_angle();
        let end = self.angle_of(self.end);
        if self.parameter_increases() {
            normalize_angle(end - start)
        } else {
            normalize_angle(start - end)
        }
    }

    /// Parametric sweep with the sign of parameter travel.
    pub fn signed_sweep_radians(&self) -> f64 {
        if self.parameter_increases() {
            self.sweep_radians()
        } else {
            -self.sweep_radians()
        }
    }

    pub fn point_at(&self, angle: f64) -> Point {
        self.center + self.x_axis * angle.cos() + self.y_axis * angle.sin()
    }

    /// Largest singular value of the axis basis: the factor that scales a
    /// unit-circle approximation error onto this ellipse.
    pub fn max_scale(&self) -> f64 {
        Affine2 {
            m00: self.x_axis.x,
            m01: self.y_axis.x,
            m02: 0.0,
            m10: self.x_axis.y,
            m11: self.y_axis.y,
            m12: 0.0,
        }
        .max_scale()
    }

    /// Tight bounding box: the endpoints plus every axis extreme the arc
    /// passes through.
    pub fn bbox(&self) -> BBox {
        let mut bbox = BBox::from_point(self.start);
        bbox.include_point(self.end);
        if self.is_degenerate() {
            return bbox;
        }
        let start = self.start_angle();
        let sweep = self.signed_sweep_radians();
        // x(θ) is extreme where −x_axis.x·sin θ + y_axis.x·cos θ = 0, and
        // likewise for y; each gives two antipodal parameters.
        for (a, b) in [
            (self.x_axis.x, self.y_axis.x),
            (self.x_axis.y, self.y_axis.y),
        ] {
            let base = b.atan2(a);
            for angle in [base, base + std::f64::consts::PI] {
                let offset = if sweep >= 0.0 {
                    normalize_angle(angle - start)
                } else {
                    normalize_angle(start - angle)
                };
                if offset <= sweep.abs() + 1e-12 {
                    bbox.include_point(self.point_at(angle));
                }
            }
        }
        bbox
    }

    /// Principal semi-axes and the rotation of the major axis, for formats
    /// that describe ellipses by radii and rotation.
    pub fn principal_axes(&self) -> (f64, f64, f64) {
        let (a, b, c, d) = (self.x_axis.x, self.y_axis.x, self.x_axis.y, self.y_axis.y);
        let e = (a + d) / 2.0;
        let f = (a - d) / 2.0;
        let g = (c + b) / 2.0;
        let h = (c - b) / 2.0;
        let q = e.hypot(h);
        let r = f.hypot(g);
        let major = q + r;
        let minor = (q - r).abs();
        let rotation = (g.atan2(f) + h.atan2(e)) / 2.0;
        (major, minor, rotation)
    }

    /// Exact image under an affine transform.
    pub fn transformed(&self, transform: Affine2) -> Self {
        let linear = |p: Point| {
            Point::new(
                transform.m00 * p.x + transform.m01 * p.y,
                transform.m10 * p.x + transform.m11 * p.y,
            )
        };
        Self {
            start: transform.transform_point(self.start),
            end: transform.transform_point(self.end),
            center: transform.transform_point(self.center),
            x_axis: linear(self.x_axis),
            y_axis: linear(self.y_axis),
            clockwise: self.clockwise != (transform.determinant() < 0.0),
        }
    }
}

fn angle_is_on_arc(start: f64, end: f64, angle: f64, clockwise: bool) -> bool {
    if normalize_angle(end - start) <= 1e-12 {
        return true;
    }

    if clockwise {
        normalize_angle(start - angle) <= normalize_angle(start - end) + 1e-12
    } else {
        normalize_angle(angle - start) <= normalize_angle(end - start) + 1e-12
    }
}

pub(crate) fn normalize_angle(angle: f64) -> f64 {
    angle.rem_euclid(std::f64::consts::TAU)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elliptical_arc_matches_its_circular_source_under_scaling() {
        let arc = Arc::new(
            Point::new(1.0, 0.0),
            Point::new(0.0, 1.0),
            Point::ZERO,
            false,
        );
        let scaled = arc.to_elliptical().transformed(Affine2 {
            m00: 2.0,
            m11: 0.5,
            ..Affine2::IDENTITY
        });
        assert!((scaled.sweep_radians() - std::f64::consts::FRAC_PI_2).abs() < 1e-12);
        let mid = scaled.point_at(scaled.start_angle() + scaled.signed_sweep_radians() / 2.0);
        assert!((mid.x - 2.0 * 0.5_f64.sqrt()).abs() < 1e-12);
        assert!((mid.y - 0.5 * 0.5_f64.sqrt()).abs() < 1e-12);
        let bbox = scaled.bbox();
        assert!((bbox.max.x - 2.0).abs() < 1e-12 && (bbox.max.y - 0.5).abs() < 1e-12);
        let (major, minor, _) = scaled.principal_axes();
        assert!((major - 2.0).abs() < 1e-12 && (minor - 0.5).abs() < 1e-12);
    }

    #[test]
    fn mirroring_flips_geometric_winding_but_keeps_the_arc() {
        let arc = Arc::new(
            Point::new(1.0, 0.0),
            Point::new(0.0, 1.0),
            Point::ZERO,
            false,
        )
        .to_elliptical();
        let mirrored = arc.transformed(Affine2 {
            m00: -1.0,
            ..Affine2::IDENTITY
        });
        assert!(mirrored.clockwise);
        assert!((mirrored.sweep_radians() - std::f64::consts::FRAC_PI_2).abs() < 1e-12);
        let mid = mirrored.point_at(mirrored.start_angle() + mirrored.signed_sweep_radians() / 2.0);
        assert!((mid.x + 0.5_f64.sqrt()).abs() < 1e-12);
        assert!((mid.y - 0.5_f64.sqrt()).abs() < 1e-12);
    }
}
