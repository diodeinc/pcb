//! Set operations and ordered dark/clear paint composition.

use super::{ContourSet, Ring, Shape, flatten_shapes, rings_bbox, simplify_shapes};
use crate::geom::accuracy::numerical_error;
use crate::geom::{AccuracyError, FillRule, Polarity, Resolution};
use i_overlay::core::fill_rule::FillRule as OverlayFillRule;
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::float::simplify::SimplifyShape;
use i_overlay::float::single::SingleFloatOverlay;

/// Difference keeping the connected-shape structure of the result.
pub(crate) fn difference_shapes(subject: Vec<Ring>, cutters: Vec<Ring>) -> Vec<Shape> {
    if subject.is_empty() || cutters.is_empty() {
        return subject.simplify_shape_as::<i64>(OverlayFillRule::NonZero);
    }
    subject.overlay_as::<i64>(&cutters, OverlayRule::Difference, OverlayFillRule::NonZero)
}

impl ContourSet {
    /// Regularized union of many regions.
    pub fn union_all(
        resolution: Resolution,
        regions: impl IntoIterator<Item = Self>,
    ) -> Result<Self, AccuracyError> {
        let mut composer = PaintComposer::new(resolution);
        for region in regions {
            composer.push(Polarity::Dark, region);
        }
        composer.finish()
    }

    /// Regularized union: `self ∪ other`.
    pub fn union(&self, other: &Self) -> Result<Self, AccuracyError> {
        let budget = self.resolution.meet(other.resolution).accuracy;
        if self.is_empty() && self.uncertainty_mm == 0.0 {
            return other.clone().rebudget(budget);
        }
        if other.is_empty() && other.uncertainty_mm == 0.0 {
            return self.clone().rebudget(budget);
        }
        self.boolean(other, OverlayRule::Union)
    }

    pub fn union_assign(&mut self, other: &Self) -> Result<(), AccuracyError> {
        *self = self.union(other)?;
        Ok(())
    }

    /// Regularized difference: `self \ cutters`.
    pub fn difference(&self, cutters: &Self) -> Result<Self, AccuracyError> {
        self.boolean(cutters, OverlayRule::Difference)
    }

    /// Regularized intersection: `self ∩ clip`.
    pub fn intersection(&self, clip: &Self) -> Result<Self, AccuracyError> {
        self.boolean(clip, OverlayRule::Intersect)
    }

    /// Every point of the result's boundary is a point of an operand's
    /// boundary or a crossing of two, so it stays within the larger operand
    /// uncertainty of the source geometry; the overlay adds coordinate
    /// rounding. The result takes the tighter budget and fails when the
    /// operands' history does not fit it.
    fn boolean(&self, other: &Self, rule: OverlayRule) -> Result<Self, AccuracyError> {
        let rings = flatten_shapes(self.rings.overlay_as::<i64>(
            &other.rings,
            rule,
            OverlayFillRule::NonZero,
        ));
        let uncertainty = self.uncertainty_mm.max(other.uncertainty_mm)
            + numerical_error(self.bbox.union(other.bbox));
        Self::from_regularized(rings, self.resolution.meet(other.resolution), uncertainty).checked()
    }
    /// Connected components, each retaining its own hole rings.
    pub fn connected_components(&self) -> Vec<Self> {
        simplify_shapes(self.rings.clone(), FillRule::NonZero)
            .into_iter()
            .map(|shape| Self::from_regularized(shape, self.resolution, self.uncertainty_mm))
            .collect()
    }
}

/// Compose an ordered dark/clear paint stream into a final positive image.
///
/// Consecutive same-polarity pushes are batched into one boolean operation.
/// The image is prepared at the composer's resolution, tightened to the
/// budget of any input.
#[derive(Debug)]
pub struct PaintComposer {
    image: Vec<Ring>,
    run: Vec<Ring>,
    run_polarity: Option<Polarity>,
    resolution: Resolution,
    uncertainty_mm: f64,
}

impl PaintComposer {
    pub fn new(resolution: Resolution) -> Self {
        Self {
            image: Vec::new(),
            run: Vec::new(),
            run_polarity: None,
            resolution,
            uncertainty_mm: 0.0,
        }
    }

    pub fn push(&mut self, polarity: Polarity, mut region: ContourSet) {
        // An empty input still contributes its history: material within its
        // uncertainty band may have been lost before it arrived here.
        self.uncertainty_mm = self.uncertainty_mm.max(region.uncertainty_mm);
        self.resolution = self.resolution.meet(region.resolution);
        if region.is_empty() {
            return;
        }
        if self.run_polarity != Some(polarity) {
            self.flush_run();
            self.run_polarity = Some(polarity);
        }
        self.run.append(&mut region.rings);
    }

    /// The composed image, checked against its budget.
    pub fn finish(mut self) -> Result<ContourSet, AccuracyError> {
        self.flush_run();
        ContourSet::from_regularized(self.image, self.resolution, self.uncertainty_mm).checked()
    }

    fn flush_run(&mut self) {
        let Some(polarity) = self.run_polarity.take() else {
            return;
        };
        let rule = match polarity {
            Polarity::Dark => OverlayRule::Union,
            Polarity::Clear => OverlayRule::Difference,
        };
        let rings = flatten_shapes(self.image.overlay_as::<i64>(
            &self.run,
            rule,
            OverlayFillRule::NonZero,
        ));
        self.uncertainty_mm +=
            numerical_error(rings_bbox(&self.image).union(rings_bbox(&self.run)));
        self.image = rings;
        self.run.clear();
    }
}
