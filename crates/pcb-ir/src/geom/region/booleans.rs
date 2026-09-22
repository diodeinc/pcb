//! Set operations and ordered dark/clear paint composition.

use super::simplification::{resolve_groups, untagged};
use super::{ContourSet, Ring, Shape, flatten_shapes, rings_bbox, simplify_shapes};
use crate::geom::accuracy::numerical_error;
use crate::geom::{AccuracyError, BBox, FillRule, Polarity, Resolution};
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

/// Rings tagged with the regularized source they belong to.
fn sourced(rings: Vec<Ring>, source: u32) -> impl Iterator<Item = (Ring, u32)> {
    rings.into_iter().map(move |ring| (ring, source))
}

/// Union of regularized sources. Each source winds once over its own interior
/// and not at all outside, so their sum is nonzero exactly on the union and
/// one simplification replaces the pairwise overlay. A group drawn from a
/// single source is already regular and passes through untouched, so adding
/// to a large image costs only the neighbourhoods the addition reaches.
fn union_rings(rings: Vec<(Ring, u32)>) -> Vec<Ring> {
    resolve_groups(rings, |group| {
        let source = group[0].1;
        let regular = group.iter().all(|(_, other)| *other == source);
        let rings = untagged(group);
        if regular {
            rings
        } else {
            flatten_shapes(rings.simplify_shape_as::<i64>(OverlayFillRule::NonZero))
        }
    })
}

/// Difference or intersection of two regularized operands. A group holding
/// only one operand needs no overlay: subject rings no clip ring reaches
/// survive a difference verbatim, and everything else contributes nothing.
fn overlay_rings(subject: Vec<Ring>, clip: Vec<Ring>, rule: OverlayRule) -> Vec<Ring> {
    let rings = sourced(subject, 0).chain(sourced(clip, 1)).collect();
    resolve_groups(rings, |group| {
        let (subject, clip): (Vec<_>, Vec<_>) =
            group.into_iter().partition(|(_, source)| *source == 0);
        let (subject, clip) = (untagged(subject), untagged(clip));
        if subject.is_empty() {
            Vec::new()
        } else if clip.is_empty() {
            match rule {
                OverlayRule::Difference => subject,
                _ => Vec::new(),
            }
        } else {
            flatten_shapes(subject.overlay_as::<i64>(&clip, rule, OverlayFillRule::NonZero))
        }
    })
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
        // An empty operand that lost nothing on its way here leaves the
        // other's rings as they are, at the resolution the two share.
        for (empty, kept) in [(self, other), (other, self)] {
            if empty.is_empty() && empty.uncertainty_mm == 0.0 {
                return Self::from_regularized(
                    kept.to_rings(),
                    self.resolution.meet(other.resolution),
                    kept.uncertainty_mm,
                )
                .checked();
            }
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
        let numeric = numerical_error(self.bbox.union(other.bbox));
        // Only rings that reach the other operand can change the result, so
        // cutting a small subject out of a panel-sized region never copies
        // the panel.
        let rings = match rule {
            OverlayRule::Union => union_rings(
                sourced(self.to_rings(), 0)
                    .chain(sourced(other.to_rings(), 1))
                    .collect(),
            ),
            OverlayRule::Difference => overlay_rings(
                self.to_rings(),
                other.reaching(self.bbox.expand(numeric)).into_rings(),
                rule,
            ),
            _ => overlay_rings(
                self.reaching(other.bbox.expand(numeric)).into_rings(),
                other.reaching(self.bbox.expand(numeric)).into_rings(),
                rule,
            ),
        };
        let uncertainty = self.uncertainty_mm.max(other.uncertainty_mm) + numeric;
        Self::from_regularized(rings, self.resolution.meet(other.resolution), uncertainty).checked()
    }
    /// The rings whose bounds reach `window`: everything that can matter to
    /// an operation or query confined to it. A hole lies within its outer
    /// ring's bounds, so a kept hole always keeps the material around it.
    pub fn reaching(&self, window: BBox) -> Self {
        let rings = self
            .bounded_rings()
            .filter(|(_, bounds)| bounds.intersects(window))
            .map(|(ring, _)| ring.to_vec())
            .collect();
        Self::from_regularized(rings, self.resolution, self.uncertainty_mm)
    }

    /// Connected components, each retaining its own hole rings.
    pub fn connected_components(&self) -> Vec<Self> {
        simplify_shapes(self.to_rings(), FillRule::NonZero)
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
    /// The pending same-polarity run, each ring tagged with the pushed
    /// region it came from; the image itself is source zero.
    run: Vec<(Ring, u32)>,
    run_sources: u32,
    run_polarity: Option<Polarity>,
    resolution: Resolution,
    uncertainty_mm: f64,
}

impl PaintComposer {
    pub fn new(resolution: Resolution) -> Self {
        Self {
            image: Vec::new(),
            run: Vec::new(),
            run_sources: 0,
            run_polarity: None,
            resolution,
            uncertainty_mm: 0.0,
        }
    }

    pub fn push(&mut self, polarity: Polarity, region: ContourSet) {
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
        self.run_sources += 1;
        self.run
            .extend(sourced(region.into_rings(), self.run_sources));
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
        let run = std::mem::take(&mut self.run);
        let image = std::mem::take(&mut self.image);
        self.run_sources = 0;
        self.uncertainty_mm += numerical_error(
            run.iter()
                .map(|(ring, _)| rings_bbox(std::slice::from_ref(ring)))
                .fold(rings_bbox(&image), |bbox, ring| bbox.union(ring)),
        );
        self.image = match polarity {
            Polarity::Dark => union_rings(sourced(image, 0).chain(run).collect()),
            Polarity::Clear => overlay_rings(image, untagged(run), OverlayRule::Difference),
        };
    }
}
