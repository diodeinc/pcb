//! Set operations and ordered dark/clear paint composition.

use super::simplification::{
    ON_EDGE_MM, bounds_connected_groups, overlay, overlay_rules, resolve_groups, untagged,
};
use super::{ContourSet, PreparedRegion, Ring, Shape, flatten_shapes, rings_bbox, simplify_shapes};
use crate::geom::accuracy::numerical_error;
use crate::geom::{AccuracyError, BBox, FillRule, Polarity, Resolution};
use i_overlay::core::fill_rule::FillRule as OverlayFillRule;
use i_overlay::core::overlay_rule::OverlayRule;
use rustc_hash::FxHashMap;

/// Difference keeping the connected-shape structure of the result.
pub(crate) fn difference_shapes(subject: Vec<Ring>, cutters: Vec<Ring>) -> Vec<Shape> {
    overlay(
        &subject,
        &cutters,
        OverlayRule::Difference,
        OverlayFillRule::NonZero,
    )
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
    resolve_groups(rings, 0.0, |group| {
        let source = group[0].1;
        let regular = group.iter().all(|(_, other)| *other == source);
        let rings = untagged(group);
        if regular {
            rings
        } else {
            flatten_shapes(overlay(
                &rings,
                &[],
                OverlayRule::Subject,
                OverlayFillRule::NonZero,
            ))
        }
    })
}

/// Difference or intersection of two regularized operands. A group holding
/// only one operand needs no overlay: subject rings no clip ring reaches
/// survive a difference verbatim, and everything else contributes nothing.
fn overlay_rings(subject: Vec<Ring>, clip: Vec<Ring>, rule: OverlayRule) -> Vec<Ring> {
    let rings = sourced(subject, 0).chain(sourced(clip, 1)).collect();
    resolve_groups(rings, 0.0, |group| {
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
            flatten_shapes(overlay(&subject, &clip, rule, OverlayFillRule::NonZero))
        }
    })
}

/// Intersection and difference of two regularized operands, each resolved
/// group by group exactly as [`overlay_rings`] resolves it alone. The
/// difference groups every `subject` ring; the intersection groups only
/// `reaching`, when that is not all of them. A group holding the same
/// operands under both groupings is overlaid once for both rules, which is
/// every group when the whole subject is within the clip's reach.
fn split_rings(
    subject: Vec<Ring>,
    reaching: Option<Vec<Ring>>,
    clip: Vec<Ring>,
) -> (Vec<Ring>, Vec<Ring>) {
    let groups = |subject: Vec<Ring>, clip: Vec<Ring>| {
        // Clip rings carry their index, so a group can be found again under
        // the other grouping through its first clip ring.
        let rings = sourced(subject, u32::MAX)
            .chain(clip.into_iter().zip(0..))
            .collect();
        bounds_connected_groups(rings, 0.0)
            .into_iter()
            .map(|group| {
                let (subject, clip): (Vec<_>, Vec<_>) =
                    group.into_iter().partition(|(_, index)| *index == u32::MAX);
                let first = clip.first().map(|(_, index)| *index);
                (untagged(subject), untagged(clip), first)
            })
            .collect::<Vec<_>>()
    };
    let overlay_one = |subject: &[Ring], clip: &[Ring], rule| {
        flatten_shapes(overlay(subject, clip, rule, OverlayFillRule::NonZero))
    };
    let overlay_both = |subject: &[Ring], clip: &[Ring]| {
        overlay_rules(
            subject,
            clip,
            [OverlayRule::Intersect, OverlayRule::Difference],
            OverlayFillRule::NonZero,
        )
        .map(flatten_shapes)
    };

    // Only groups holding both operands contribute to the intersection.
    let intersected = reaching.map(|reaching| {
        groups(reaching, clip.clone())
            .into_iter()
            .filter(|(subject, clip, _)| !subject.is_empty() && !clip.is_empty())
            .collect::<Vec<_>>()
    });
    let slots = intersected.iter().flatten().enumerate();
    let slot_of = slots
        .map(|(slot, (_, _, first))| (first.unwrap(), slot))
        .collect::<FxHashMap<_, _>>();
    let mut inside = Vec::new();
    let mut shared = vec![None; slot_of.len()];
    let mut outside = Vec::new();
    for (subject, clip, first) in groups(subject, clip) {
        let Some(first) = first else {
            outside.extend(subject);
            continue;
        };
        if subject.is_empty() {
            continue;
        }
        let Some(intersected) = &intersected else {
            let [intersection, difference] = overlay_both(&subject, &clip);
            inside.extend(intersection);
            outside.extend(difference);
            continue;
        };
        match slot_of.get(&first) {
            Some(&slot) if intersected[slot].0 == subject && intersected[slot].1 == clip => {
                let [intersection, difference] = overlay_both(&subject, &clip);
                shared[slot] = Some(intersection);
                outside.extend(difference);
            }
            _ => outside.extend(overlay_one(&subject, &clip, OverlayRule::Difference)),
        }
    }
    for ((subject, clip, _), shared) in intersected.into_iter().flatten().zip(shared) {
        inside
            .extend(shared.unwrap_or_else(|| overlay_one(&subject, &clip, OverlayRule::Intersect)));
    }
    (inside, outside)
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

    /// The parts of `self` inside and outside `clip`: exactly
    /// `(self.intersection(clip)?, self.difference(clip)?)`, with each group
    /// of rings the two share overlaid once for both. A ring of `self` that
    /// stays clear of `clip`'s bounds but shares a group with rings that
    /// reach them leaves the two with different groups, overlaid apart.
    pub fn intersection_and_difference(&self, clip: &Self) -> Result<(Self, Self), AccuracyError> {
        let numeric = numerical_error(self.bbox.union(clip.bbox));
        let reaching = self.reaching(clip.bbox.expand(numeric));
        let reaching = (reaching.rings.len() < self.rings.len()).then(|| reaching.into_rings());
        let (inside, outside) = split_rings(
            self.to_rings(),
            reaching,
            clip.reaching(self.bbox.expand(numeric)).into_rings(),
        );
        let resolution = self.resolution.meet(clip.resolution);
        let uncertainty = self.uncertainty_mm.max(clip.uncertainty_mm) + numeric;
        Ok((
            Self::from_regularized(inside, resolution, uncertainty).checked()?,
            Self::from_regularized(outside, resolution, uncertainty).checked()?,
        ))
    }

    /// Whether `self` lies wholly inside (`Some(true)`) or wholly outside
    /// (`Some(false)`) the region `boundary` was prepared from. `None` when
    /// a boundary passes close enough to the bounds of `self` that only a
    /// boolean can decide. An empty region is outside.
    pub fn side_of(&self, boundary: &PreparedRegion) -> Option<bool> {
        if self.is_empty() {
            return Some(false);
        }
        let numeric = numerical_error(self.bbox.union(boundary.bounds()));
        // With no boundary in the expanded box, winding is constant there.
        boundary
            .segments_meeting(self.bbox.expand(ON_EDGE_MM + 2.0 * numeric))
            .next()
            .is_none()
            .then(|| boundary.winding(self.bbox.center()) != 0)
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

    /// Connected components, each retaining its own hole rings, in the same
    /// order every time for the same region.
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
