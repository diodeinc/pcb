//! Conversion between curved contours and polygon rings.

use super::{ContourSet, Ring};
use crate::geom::path::{ContourBuf, PathCmd, PathOp};
use crate::geom::{AccuracyError, BBox, Point};

/// Flatten contours to polygon rings with every curve within `tolerance` of
/// its chords, returning the rings and the largest deviation incurred.
pub(super) fn flatten_contours(
    contours: &[ContourBuf],
    tolerance: f64,
) -> Result<(Vec<Ring>, f64), AccuracyError> {
    let mut rings = Vec::new();
    let mut ring = Ring::new();
    let mut deviation: f64 = 0.0;
    for contour in contours {
        let mut current = None;
        let mut first = None;
        for cmd in &contour.cmds {
            match cmd.op {
                PathOp::MoveTo => {
                    push_ring(&mut rings, &mut ring);
                    ring.push([cmd.p0.x, cmd.p0.y]);
                    first = Some(cmd.p0);
                    current = first;
                }
                PathOp::Close => {
                    push_ring(&mut rings, &mut ring);
                    // Drawing that continues after a close starts where the
                    // closed ring did.
                    ring.extend(first.map(|point| [point.x, point.y]));
                    current = first;
                }
                _ => {
                    let Some(segment) = cmd.segment_from(current) else {
                        continue;
                    };
                    if ring.is_empty() {
                        ring.push([segment.start().x, segment.start().y]);
                    }
                    let (points, strayed) = segment.chords(tolerance)?;
                    ring.extend(points.into_iter().map(|point| [point.x, point.y]));
                    deviation = deviation.max(strayed);
                    current = Some(segment.end());
                }
            }
        }
        push_ring(&mut rings, &mut ring);
    }
    Ok((rings, deviation))
}

/// Convert polygon rings back into closed line contours.
pub fn rings_to_contours(rings: Vec<Ring>) -> Vec<ContourBuf> {
    rings.into_iter().filter_map(ring_to_contour).collect()
}

impl ContourSet {
    pub fn to_contours(&self) -> Vec<ContourBuf> {
        rings_to_contours(self.to_rings())
            .into_iter()
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
