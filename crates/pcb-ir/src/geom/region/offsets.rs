//! Disk offsets, opening, and closing within the region's budget.

use super::simplification::{positive_winding, resolve_groups, untagged};
use super::{ContourSet, PreparedRegion, Ring, decimate_rings_inward, edges_of, signed_area_of};
use crate::geom::accuracy::numerical_error;
use crate::geom::{AccuracyError, Point};

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
        // A ring narrower than the disk on the side the offset shrinks
        // vanishes: an outer ring under erosion, whose component erodes on
        // its own, or a hole under dilation, which fills. Rings whose bounds
        // stay farther apart than the offset cannot meet in it, so each group
        // of the rest offsets, and if need be falls back, on its own.
        let kept = self
            .bounded_rings()
            .filter(|(ring, bounds)| {
                signed_area_of(ring) * offset > 0.0
                    || bounds.width().min(bounds.height()) > 2.0 * offset.abs()
            })
            .map(|(ring, _)| (ring.to_vec(), ()))
            .collect();
        let mut added: f64 = 0.0;
        let rings = resolve_groups(kept, offset.abs(), |group| {
            let (rings, sag) = offset_group(
                &untagged(group),
                offset,
                join_angle,
                remaining,
                numeric,
                self.resolution.tolerance_mm,
            );
            added = added.max(sag);
            rings
        });
        // Decimating within `numeric` moves the boundary by less than the
        // rounding the overlay already charged.
        Self::from_regularized(
            decimate_rings_inward(rings.iter().map(Vec::as_slice), numeric),
            self.resolution,
            inherited + added + numeric,
        )
        .checked()
    }
}

/// The offset of one group of rings and the error it carries beyond
/// rounding. Every ring winds once around what it offsets to and collapsed
/// pieces wind backwards, so positive winding is the offset region.
///
/// A crossing or direct join classifies a point differently from a path
/// through its corner only within `offset` of the corner, where an erosion
/// never reaches and a dilation always does. Distance to the boundary has no
/// minimum inside a piece, so a misclassified piece has an edge that close:
/// the shortfall bounds it like the joins' sag, and past the allowance the
/// group joins through its corners instead. Rings below significance leave
/// the result anyway.
fn offset_group(
    rings: &[Ring],
    offset: f64,
    join_angle: f64,
    allowance: f64,
    numeric: f64,
    tolerance: f64,
) -> (Vec<Ring>, f64) {
    let offset_rings = |direct: bool| {
        let (mut raw, mut sag, mut crossed) = (Vec::with_capacity(rings.len()), 0.0, false);
        for ring in rings {
            let (ring, ring_sag, ring_crossed) = raw_offset(ring, offset, join_angle, direct);
            raw.push(ring);
            sag = f64::max(sag, ring_sag);
            crossed |= ring_crossed;
        }
        (positive_winding(&raw), sag, crossed)
    };
    let (result, sag, crossed) = offset_rings(true);
    if !crossed {
        return (result, sag);
    }
    let boundary =
        PreparedRegion::from_segments(rings.iter().flat_map(|ring| edges_of(ring)).collect(), 0.0);
    let shortfall = result
        .iter()
        .filter(|ring| signed_area_of(ring).abs() > tolerance * tolerance)
        .flat_map(|ring| edges_of(ring))
        .filter_map(|(start, end)| boundary.segment_nearest_within(start, end, offset.abs()))
        .map(|nearest| offset.abs() - nearest.mm - 2.0 * numeric)
        .fold(sag, f64::max);
    if shortfall > allowance.max(sag) {
        let (result, sag, _) = offset_rings(false);
        (result, sag)
    } else {
        (result, shortfall)
    }
}

/// The sharpest turn a flattened curve takes at one vertex, even at a coarse
/// budget; sharper corners are polygon corners, few enough to join through.
const SMOOTH_TURN: f64 = std::f64::consts::FRAC_PI_8;

/// The raw offset curve of one ring, the sag of its round joins, and whether
/// it crossed or directly joined any corner: every edge moved `offset` to its
/// right, away from the material on its left, joined corner by corner.
///
/// Where the offset edges part, a round join of chords within `join_angle`
/// fills the gap. Where they overlap, they join through the corner; with
/// `direct`, a smooth turn of a flattened curve joins them where they cross,
/// as the curve's own offset would, or straight across where they cross
/// beyond either edge.
///
/// Direct joins exist for speed: offset edges at full length through every
/// corner of a flattened curve cross each other near its centre once the
/// offset nears its radius, and resolving those crossings costs the overlay
/// a hundred times what crossing joins do.
fn raw_offset(ring: &[[f64; 2]], offset: f64, join_angle: f64, direct: bool) -> (Ring, f64, bool) {
    let edges = edges_of(ring)
        .filter_map(|(start, end)| {
            let length = (end - start).length();
            (length > 0.0).then(|| (end, (end - start) / length, length))
        })
        .collect::<Vec<_>>();
    let mut raw = Vec::with_capacity(edges.len());
    let mut sag: f64 = 0.0;
    let mut crossed = false;
    let mut push = |point: Point| raw.push([point.x, point.y]);
    let right = |direction: Point| Point::new(direction.y, -direction.x) * offset;
    for (index, &(corner, direction, length)) in edges.iter().enumerate() {
        let (_, next, next_length) = edges[(index + 1) % edges.len()];
        let (normal, next_normal) = (right(direction), right(next));
        let cross = direction.x * next.y - direction.y * next.x;
        let dot = direction.x * next.x + direction.y * next.y;
        // A reversal parts the offset edges whichever way the offset goes.
        let turn = if cross == 0.0 && dot < 0.0 {
            std::f64::consts::PI.copysign(offset)
        } else {
            cross.atan2(dot)
        };
        if turn * offset > 0.0 {
            let steps = (turn.abs() / join_angle).ceil();
            sag = sag.max(2.0 * offset.abs() * (turn.abs() / steps / 4.0).sin().powi(2));
            push(corner + normal);
            for step in 1..steps as usize {
                let (sin, cos) = (turn * step as f64 / steps).sin_cos();
                push(
                    corner
                        + Point::new(
                            normal.x * cos - normal.y * sin,
                            normal.x * sin + normal.y * cos,
                        ),
                );
            }
            push(corner + next_normal);
        } else if direct && turn.abs() <= SMOOTH_TURN {
            crossed = true;
            if offset.abs() * (turn / 2.0).tan().abs() <= length.min(next_length) {
                push(corner + (normal + next_normal) / (1.0 + dot));
            } else {
                push(corner + normal);
                push(corner + next_normal);
            }
        } else {
            push(corner + normal);
            push(corner);
            push(corner + next_normal);
        }
    }
    (raw, sag, crossed)
}
