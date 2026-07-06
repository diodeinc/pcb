use crate::geom::point::Point;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BBox {
    pub min: Point,
    pub max: Point,
}

impl BBox {
    pub fn empty() -> Self {
        Self {
            min: Point::new(f64::INFINITY, f64::INFINITY),
            max: Point::new(f64::NEG_INFINITY, f64::NEG_INFINITY),
        }
    }

    pub fn new(min: Point, max: Point) -> Self {
        Self { min, max }
    }

    pub fn from_point(p: Point) -> Self {
        Self { min: p, max: p }
    }

    pub fn include_point(&mut self, p: Point) {
        self.min.x = self.min.x.min(p.x);
        self.min.y = self.min.y.min(p.y);
        self.max.x = self.max.x.max(p.x);
        self.max.y = self.max.y.max(p.y);
    }

    pub fn union(mut self, other: BBox) -> Self {
        if other.is_empty() {
            return self;
        }
        self.include_point(other.min);
        self.include_point(other.max);
        self
    }

    pub fn intersects(self, other: BBox) -> bool {
        !self.is_empty()
            && !other.is_empty()
            && self.min.x <= other.max.x
            && self.max.x >= other.min.x
            && self.min.y <= other.max.y
            && self.max.y >= other.min.y
    }

    pub fn expand(self, amount: f64) -> Self {
        if self.is_empty() {
            return self;
        }
        Self {
            min: Point::new(self.min.x - amount, self.min.y - amount),
            max: Point::new(self.max.x + amount, self.max.y + amount),
        }
    }

    pub fn width(&self) -> f64 {
        self.max.x - self.min.x
    }

    pub fn height(&self) -> f64 {
        self.max.y - self.min.y
    }

    pub fn center(&self) -> Point {
        self.min.midpoint(self.max)
    }

    pub fn is_empty(&self) -> bool {
        self.min.x.is_infinite() || self.min.y.is_infinite()
    }

    pub fn is_valid(&self) -> bool {
        self.is_empty()
            || (self.min.is_finite()
                && self.max.is_finite()
                && self.min.x <= self.max.x
                && self.min.y <= self.max.y)
    }
}

impl Default for BBox {
    fn default() -> Self {
        Self::empty()
    }
}
