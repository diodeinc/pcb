use std::ops::{Add, Div, Mul, Neg, Sub};

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub const ZERO: Self = Self { x: 0.0, y: 0.0 };

    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }

    pub fn distance_to(self, other: Point) -> f64 {
        (self - other).length()
    }

    pub fn length(self) -> f64 {
        self.x.hypot(self.y)
    }

    pub fn angle_from(self, center: Point) -> f64 {
        (self.y - center.y).atan2(self.x - center.x)
    }

    pub fn midpoint(self, other: Point) -> Point {
        Point::new((self.x + other.x) / 2.0, (self.y + other.y) / 2.0)
    }
}

impl Add for Point {
    type Output = Point;

    fn add(self, rhs: Point) -> Point {
        Point::new(self.x + rhs.x, self.y + rhs.y)
    }
}

impl Sub for Point {
    type Output = Point;

    fn sub(self, rhs: Point) -> Point {
        Point::new(self.x - rhs.x, self.y - rhs.y)
    }
}

impl Neg for Point {
    type Output = Point;

    fn neg(self) -> Point {
        Point::new(-self.x, -self.y)
    }
}

impl Mul<f64> for Point {
    type Output = Point;

    fn mul(self, rhs: f64) -> Point {
        Point::new(self.x * rhs, self.y * rhs)
    }
}

impl Div<f64> for Point {
    type Output = Point;

    fn div(self, rhs: f64) -> Point {
        Point::new(self.x / rhs, self.y / rhs)
    }
}

/// Axis mirroring applied before rotation in a placement transform.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Mirror {
    pub x: bool,
    pub y: bool,
}

impl Mirror {
    pub const NONE: Self = Self { x: false, y: false };
    pub const X: Self = Self { x: true, y: false };
    pub const Y: Self = Self { x: false, y: true };
    pub const XY: Self = Self { x: true, y: true };

    /// The conventional single-axis mirror used by placements: mirror across
    /// the Y axis (negate X) when `mirrored` is set.
    pub fn across_y(mirrored: bool) -> Self {
        Self {
            x: mirrored,
            y: false,
        }
    }

    pub fn any(self) -> bool {
        self.x || self.y
    }
}
