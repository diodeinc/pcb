use crate::geom::bbox::BBox;
use crate::geom::point::Point;

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
