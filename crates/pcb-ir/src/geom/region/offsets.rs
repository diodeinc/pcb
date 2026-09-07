//! Disk offsets, opening, and closing within the region's budget.

use super::{ContourSet, decimate_rings_inward, flatten_shapes, ring_edges, simplify_rings};
use crate::geom::accuracy::numerical_error;
use crate::geom::{AccuracyError, FillRule};
use i_overlay::mesh::outline::offset::OutlineOffset;
use i_overlay::mesh::style::{LineJoin as OutlineLineJoin, OutlineStyle};

impl ContourSet {
    /// Morphological opening by a disk: `(self ⊖ D_radius) ⊕ D_radius`.
    ///
    /// Equivalently, this is the union of every radius-sized disk contained in
    /// the source region. It is therefore a subset of the source that removes
    /// tips and islands too small to accommodate the disk while rounding the
    /// surviving outward corners.
    pub fn disk_open(&self, radius: f64) -> Result<Self, AccuracyError> {
        self.disk_erode(radius)?
            .disk_dilate(radius)?
            .intersection(self)
    }

    /// Morphological closing by a disk: `(self ⊕ D_radius) ⊖ D_radius`.
    ///
    /// Equivalently, the complement of the result is the union of every
    /// radius-sized disk contained in the source complement. Closing therefore
    /// fills void tips and gaps too small to accommodate the disk while
    /// rounding the surviving inward corners.
    pub fn disk_close(&self, radius: f64) -> Result<Self, AccuracyError> {
        self.disk_dilate(radius)?.disk_erode(radius)?.union(self)
    }
    /// Disk dilation within the region's budget, input uncertainty included.
    pub fn disk_dilate(&self, radius: f64) -> Result<Self, AccuracyError> {
        if radius < 0.0 {
            return Err(AccuracyError::InvalidGeometry("negative disk radius"));
        }
        self.disk_offset(radius)
    }

    /// Disk erosion within the region's budget, input uncertainty included.
    pub fn disk_erode(&self, radius: f64) -> Result<Self, AccuracyError> {
        if radius < 0.0 {
            return Err(AccuracyError::InvalidGeometry("negative disk radius"));
        }
        self.disk_offset(-radius)
    }

    fn disk_offset(&self, offset: f64) -> Result<Self, AccuracyError> {
        if !offset.is_finite() {
            return Err(AccuracyError::InvalidGeometry("non-finite disk radius"));
        }
        let accuracy = self.budget();
        accuracy.check(self.uncertainty_mm)?;
        if self.is_empty() || offset == 0.0 {
            return Ok(self.clone());
        }
        let numeric = numerical_error(self.bbox.expand(offset.abs()));
        let inherited = self.uncertainty_mm + numeric;
        let remaining = accuracy.allowance(inherited)?;
        let join_angle = (2.0 * (remaining / (2.0 * offset.abs())).min(1.0).sqrt().asin())
            .clamp(0.01 * std::f64::consts::PI, std::f64::consts::FRAC_PI_4);
        let style = OutlineStyle::new(offset).line_join(OutlineLineJoin::Round(join_angle));
        let rings = flatten_shapes(self.rings.outline_as::<i64>(&style));
        // Only rounded corners incur chord error; inward corners intersect straight edges.
        // The backend emits floor(sweep / join_angle) chords at each rounded corner.
        let added = self
            .rings
            .iter()
            .flat_map(|ring| {
                let directions = ring_edges(ring)
                    .filter_map(|(a, b)| {
                        let d = b - a;
                        (d.length() > 0.0).then(|| d / d.length())
                    })
                    .collect::<Vec<_>>();
                directions
                    .iter()
                    .zip(directions.iter().cycle().skip(1))
                    .take(directions.len())
                    .filter(|(a, b)| (a.x * b.y - a.y * b.x) * offset > 0.0)
                    .map(|(a, b)| {
                        let sweep = (a.x * b.x + a.y * b.y).clamp(-1.0, 1.0).acos();
                        let steps = (sweep / join_angle).floor().max(1.0);
                        2.0 * offset.abs() * (sweep / steps / 4.0).sin().powi(2)
                    })
                    .collect::<Vec<_>>()
            })
            .fold(0.0, f64::max);
        let simplified = decimate_rings_inward(&rings, numeric);
        Self::from_regularized(
            simplify_rings(simplified, FillRule::NonZero),
            self.resolution,
            inherited + added + numeric,
        )
        .checked()
    }
}
