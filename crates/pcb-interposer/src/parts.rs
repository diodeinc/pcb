//! The DUT's parts on its probed face, as outlines in the sheet frame.
//!
//! The probed face lies on the upper shroud, so every part on it hangs
//! into the shroud's top face and needs a pocket there ([`crate::shroud`]).
//! The outline is the part's IPC-2581 package outline, placed the way
//! IPC-2581C places a component: rotate counterclockwise, then mirror X
//! for a flipped part, then move to its location. KiCad releases that
//! wrote flipped rotations in a non-standard form are corrected first.
//! Test pads and fiducials stand no taller than their copper, so they get
//! no pocket.

use anyhow::{Context, Result};
use ipc2581::Ipc2581;
use ipc2581::types::{Component, PolyStep, Polygon, Side, Step};
use pcb_ir::import::ipc2581::FlippedRotationDefect;

use crate::contacts::component_side;

/// One part on the probed face of one board instance.
#[derive(Debug, Clone)]
pub struct Part {
    pub board: u32,
    pub refdes: String,
    /// Closed outline in the Y-down sheet frame, without the repeated
    /// first point.
    pub outline: Vec<[f64; 2]>,
}

/// Packages that are flat copper on the board: no pocket.
fn flat(package: &str) -> bool {
    ["TestPoint", "Fiducial", "MountingHole"]
        .iter()
        .any(|prefix| package.starts_with(prefix))
}

/// Maps a board-frame (Y-up) point to the sheet frame.
pub type ToSheet<'a> = &'a dyn Fn([f64; 2]) -> [f64; 2];

/// The step's parts on `side`, placed on each board instance by
/// `instances`: (board index, its placement in the sheet frame).
pub fn probed_parts(
    ipc: &Ipc2581,
    step: &Step,
    side: Side,
    instances: &[(u32, ToSheet)],
) -> Result<Vec<Part>> {
    let defect = FlippedRotationDefect::of(ipc);
    let mut parts = Vec::new();
    for component in &step.components {
        if component_side(ipc, component) != Some(side) {
            continue;
        }
        let Some(package_ref) = component.package_ref else {
            continue;
        };
        let package_name = ipc.resolve(package_ref);
        if flat(package_name) {
            continue;
        }
        let Some(package) = step.packages.iter().find(|p| p.name == package_ref) else {
            continue;
        };
        let Some(outline) = &package.outline else {
            continue;
        };
        let refdes = component
            .ref_des
            .map(|r| ipc.resolve(r).to_string())
            .unwrap_or_default();
        let local = flatten(&outline.polygon);
        let local: Vec<[f64; 2]> = match &outline.polygon_xform {
            Some(xform) => local
                .iter()
                .map(|&p| {
                    let [x, y] = rotate(p, xform.rotation);
                    let x = if xform.mirror { -x } else { x };
                    [x + xform.x_offset, y + xform.y_offset]
                })
                .collect(),
            None => local,
        };
        let board_frame: Vec<[f64; 2]> = local
            .iter()
            .map(|&p| place(component, p, defect.as_ref()))
            .collect();
        for (board, to_sheet) in instances {
            parts.push(Part {
                board: *board,
                refdes: refdes.clone(),
                outline: board_frame.iter().map(|&p| to_sheet(p)).collect(),
            });
        }
    }
    Ok(parts)
}

/// A component-frame point in the board's Y-up frame.
fn place(component: &Component, p: [f64; 2], defect: Option<&FlippedRotationDefect>) -> [f64; 2] {
    let xform = component.xform.unwrap_or_default();
    let rotation = match defect {
        Some(defect) if xform.mirror => defect.standard_rotation(xform.rotation),
        _ => xform.rotation,
    };
    let [x, y] = rotate(p, rotation);
    let x = if xform.mirror { -x } else { x };
    [x + component.location.x, y + component.location.y]
}

fn rotate([x, y]: [f64; 2], degrees: f64) -> [f64; 2] {
    let (s, c) = degrees.to_radians().sin_cos();
    [x * c - y * s, x * s + y * c]
}

/// The polygon's vertices with each arc cut into short chords.
fn flatten(polygon: &Polygon) -> Vec<[f64; 2]> {
    let begin = polygon.begin();
    let mut out = vec![[begin.x, begin.y]];
    let mut last = [begin.x, begin.y];
    for step in polygon.steps() {
        match step {
            PolyStep::Segment(segment) => {
                last = [segment.point.x, segment.point.y];
                out.push(last);
            }
            PolyStep::Curve(curve) => {
                let center = [curve.center.x, curve.center.y];
                let end = [curve.point.x, curve.point.y];
                let radius = (last[0] - center[0]).hypot(last[1] - center[1]);
                let a0 = (last[1] - center[1]).atan2(last[0] - center[0]);
                let a1 = (end[1] - center[1]).atan2(end[0] - center[0]);
                let tau = std::f64::consts::TAU;
                let mut sweep = if curve.clockwise { a0 - a1 } else { a1 - a0 };
                sweep = sweep.rem_euclid(tau);
                if sweep < 1e-9 {
                    sweep = tau;
                }
                let n = ((sweep / tau) * 32.0).ceil().max(2.0) as usize;
                for k in 1..=n {
                    let t = k as f64 / n as f64;
                    let a = if curve.clockwise {
                        a0 - sweep * t
                    } else {
                        a0 + sweep * t
                    };
                    out.push([center[0] + radius * a.cos(), center[1] + radius * a.sin()]);
                }
                last = end;
            }
        }
    }
    if out.len() > 1 && out.first() == out.last() {
        out.pop();
    }
    out
}

/// The step that carries the board's components, as `crate::single` and
/// `crate::contacts` find it.
pub fn component_step(ipc: &Ipc2581) -> Result<&Step> {
    let ecad = ipc.ecad().context("board has no ECAD section")?;
    let steps = &ecad.cad_data.steps;
    let primary = pcb_ir::import::ipc2581::primary_step(ipc, steps).context("board has no step")?;
    if !primary.components.is_empty() {
        return Ok(primary);
    }
    steps
        .iter()
        .find(|step| !step.components.is_empty())
        .context("no step with components")
}

/// Distance from `p` to the closed outline's boundary, negative inside.
pub fn signed_distance(outline: &[[f64; 2]], p: [f64; 2]) -> f64 {
    let n = outline.len();
    let mut nearest = f64::MAX;
    let mut inside = false;
    for i in 0..n {
        let a = outline[i];
        let b = outline[(i + 1) % n];
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let len2 = dx * dx + dy * dy;
        let t = if len2 > 0.0 {
            (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        nearest = nearest.min((a[0] + t * dx - p[0]).hypot(a[1] + t * dy - p[1]));
        if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < a[0] + (p[1] - a[1]) / dy * dx {
            inside = !inside;
        }
    }
    if inside { -nearest } else { nearest }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_distance_is_negative_inside() {
        let square = [[0.0, 0.0], [2.0, 0.0], [2.0, 2.0], [0.0, 2.0]];
        assert!((signed_distance(&square, [1.0, 1.0]) + 1.0).abs() < 1e-9);
        assert!((signed_distance(&square, [3.0, 1.0]) - 1.0).abs() < 1e-9);
    }
}
