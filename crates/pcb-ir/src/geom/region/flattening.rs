//! Conversion between curved contours and polygon rings.

use super::{ContourSet, Ring, simplify_shapes};
use crate::geom::accuracy::{ErrorAllocation, allocate_error};
use crate::geom::path::{ContourBuf, PathCmd};
use crate::geom::{BBox, FillRule, Point};

pub(super) fn flatten_contours(contours: &[ContourBuf], accuracy: f64) -> (Vec<Ring>, f64) {
    // Arc-to-cubic conversion is cheap in error and reported exactly, so it
    // gets a small target and chord flattening takes the rest. Source error
    // the conversion reports, such as a mismatched arc radius, is charged on
    // top rather than squeezed out of the chord tolerance, so an inconsistent
    // arc fails its budget instead of being flattened without bound.
    let conversion_target = allocate_error(accuracy, ErrorAllocation::CurveConversion);
    let (bez_path, conversion_error) =
        crate::geom::path::contours_to_kurbo(contours, conversion_target);
    let curved = bez_path
        .elements()
        .iter()
        .any(|el| matches!(el, kurbo::PathEl::CurveTo(..) | kurbo::PathEl::QuadTo(..)));
    let flatten_error = if curved {
        accuracy - conversion_target
    } else {
        0.0
    };
    let mut rings = Vec::new();
    let mut current = Vec::new();
    crate::geom::path::flatten_path(bez_path, flatten_error.max(f64::MIN_POSITIVE), |element| {
        match element {
            kurbo::PathEl::MoveTo(point) => {
                push_ring(&mut rings, &mut current);
                current.push([point.x, point.y]);
            }
            kurbo::PathEl::LineTo(point) => current.push([point.x, point.y]),
            kurbo::PathEl::ClosePath => push_ring(&mut rings, &mut current),
            kurbo::PathEl::QuadTo(..) | kurbo::PathEl::CurveTo(..) => {
                unreachable!("kurbo::flatten emits lines")
            }
        }
    });
    push_ring(&mut rings, &mut current);
    (rings, conversion_error + flatten_error)
}

/// Convert polygon rings back into closed line contours.
pub fn rings_to_contours(rings: Vec<Ring>) -> Vec<ContourBuf> {
    rings.into_iter().filter_map(ring_to_contour).collect()
}

impl ContourSet {
    pub fn to_contours(&self) -> Vec<ContourBuf> {
        rings_to_contours(self.rings.clone())
            .into_iter()
            .map(|c| c.with_uncertainty(self.uncertainty_mm))
            .collect()
    }

    /// Convert each connected component to one positive contour.
    ///
    /// Hole rings are connected to their outer ring with zero-width bridges,
    /// allowing formats without compound-polygon holes to carry the same
    /// local positive geometry without layer-wide clear features.
    pub fn to_bridged_contours(&self) -> Vec<ContourBuf> {
        simplify_shapes(self.rings.clone(), FillRule::NonZero)
            .into_iter()
            .map(crate::geom::bridge::bridge_shape)
            .filter(|ring| ring.len() >= 3)
            .filter_map(ring_to_contour)
            .map(|c| c.with_uncertainty(self.uncertainty_mm))
            .collect()
    }
}

fn push_ring(out: &mut Vec<Ring>, ring: &mut Ring) {
    // Flattening emits an explicit corner point at every join; drop the
    // zero-length edges that would otherwise reach the writers.
    ring.dedup();
    if ring.first() == ring.last() {
        ring.pop();
    }
    if ring.len() >= 3 {
        out.push(std::mem::take(ring));
    } else {
        ring.clear();
    }
}

fn ring_to_contour(ring: Ring) -> Option<ContourBuf> {
    if ring.len() < 3 {
        return None;
    }
    let mut bbox = BBox::empty();
    let mut cmds = Vec::with_capacity(ring.len() + 1);
    for (index, [x, y]) in ring.into_iter().enumerate() {
        let point = Point::new(x, y);
        bbox.include_point(point);
        if index == 0 {
            cmds.push(PathCmd::move_to(point));
        } else {
            cmds.push(PathCmd::line_to(point));
        }
    }
    cmds.push(PathCmd::close());
    Some(ContourBuf::from_parts(bbox, cmds))
}
