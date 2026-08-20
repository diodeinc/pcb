//! Read an assembly panel's fixed features from its IPC-2581 file.
//!
//! The interposer stacks under the assembly panel on the same tooling
//! pins, so its outline, tooling holes, and global fiducials are
//! *inherited* from the generated panel (`pcbc ipc2581 board-array
//! create` output) rather than recomputed — they cannot drift. On top of
//! the panel's holes we add the folded A7 tile's corner tooling, the
//! fixture connector plate's registration; exactly coincident holes
//! collapse, so holes either overlap perfectly or not at all.
//!
//! IPC-2581 is Y-up while KiCad boards are Y-down; everything returned
//! here is already flipped into the KiCad sheet frame.

use anyhow::{Context, Result, bail};
use ipc2581::Ipc2581;
use ipc2581::types::{
    FiducialKind, LayerFunction, PlatingStatus, PolyStep, SetFeature, Side, Step,
};

use crate::pattern::mate_dims;

/// Corner tooling matches the board-array generator's spec.
const CORNER_TOOLING_DIA_MM: f64 = 2.1;
const CORNER_TOOLING_INSET_MM: f64 = 3.0;
const EPS: f64 = 1e-6;

/// The panel's fixed features, in the KiCad sheet frame.
pub struct Panel {
    pub width: f64,
    pub height: f64,
    /// Board outline as drawable primitives.
    pub outline: Vec<Outline>,
    /// NPTH tooling holes: (center, drill diameter).
    pub holes: Vec<([f64; 2], f64)>,
    /// Global fiducial centers per face.
    pub fids_top: Vec<[f64; 2]>,
    pub fids_bottom: Vec<[f64; 2]>,
}

/// One outline primitive. Arcs carry a KiCad-style mid point.
#[derive(Debug, Clone, Copy)]
pub enum Outline {
    Line {
        start: [f64; 2],
        end: [f64; 2],
    },
    Arc {
        start: [f64; 2],
        mid: [f64; 2],
        end: [f64; 2],
    },
}

/// Extract the panel features from a parsed IPC-2581 file.
pub fn extract(ipc: &Ipc2581) -> Result<Panel> {
    let ecad = ipc.ecad().context("panel has no ECAD section")?;
    let step = primary_step(ipc, &ecad.cad_data.steps).context("panel has no step")?;
    let profile = step
        .profile
        .as_ref()
        .context("panel step has no board profile")?;

    // Sheet dimensions from the profile bounding box; the generator anchors
    // the array at the origin.
    let mut points: Vec<[f64; 2]> = vec![[profile.polygon.begin.x, profile.polygon.begin.y]];
    for poly_step in &profile.polygon.steps {
        match poly_step {
            PolyStep::Segment(segment) => points.push([segment.point.x, segment.point.y]),
            PolyStep::Curve(curve) => points.push([curve.point.x, curve.point.y]),
        }
    }
    let (x0, y0, x1, y1) = points.iter().fold(
        (f64::MAX, f64::MAX, f64::MIN, f64::MIN),
        |(x0, y0, x1, y1), p| (x0.min(p[0]), y0.min(p[1]), x1.max(p[0]), y1.max(p[1])),
    );
    if x0.abs() > 0.1 || y0.abs() > 0.1 {
        bail!("panel profile does not start at the origin (found {x0}, {y0})");
    }
    let (width, height) = (x1, y1);
    // Y-up IPC frame → Y-down KiCad frame.
    let flip = |p: [f64; 2]| [p[0], height - p[1]];

    let mut outline = Vec::new();
    let mut cursor = [profile.polygon.begin.x, profile.polygon.begin.y];
    for poly_step in &profile.polygon.steps {
        match poly_step {
            PolyStep::Segment(segment) => {
                let end = [segment.point.x, segment.point.y];
                outline.push(Outline::Line {
                    start: flip(cursor),
                    end: flip(end),
                });
                cursor = end;
            }
            PolyStep::Curve(curve) => {
                let end = [curve.point.x, curve.point.y];
                let center = [curve.center.x, curve.center.y];
                // Mid point of the sweep in the source frame; a pure Y flip
                // then carries all three points into the KiCad frame (the
                // three-point arc needs no direction flag).
                let a0 = (cursor[1] - center[1]).atan2(cursor[0] - center[0]);
                let a1 = (end[1] - center[1]).atan2(end[0] - center[0]);
                let radius =
                    ((cursor[0] - center[0]).powi(2) + (cursor[1] - center[1]).powi(2)).sqrt();
                let sweep = if curve.clockwise {
                    // Decreasing angle in the Y-up frame.
                    let mut sweep = a1 - a0;
                    if sweep > -EPS {
                        sweep -= std::f64::consts::TAU;
                    }
                    sweep
                } else {
                    let mut sweep = a1 - a0;
                    if sweep < EPS {
                        sweep += std::f64::consts::TAU;
                    }
                    sweep
                };
                let am = a0 + sweep / 2.0;
                let mid = [center[0] + radius * am.cos(), center[1] + radius * am.sin()];
                outline.push(Outline::Arc {
                    start: flip(cursor),
                    mid: flip(mid),
                    end: flip(end),
                });
                cursor = end;
            }
        }
    }

    // Tooling holes and global fiducials from the array step's features.
    let drill_layers: Vec<_> = ecad
        .cad_data
        .layers
        .iter()
        .filter(|layer| layer.layer_function == LayerFunction::Drill)
        .map(|layer| layer.name)
        .collect();
    let copper_side = |name: ipc2581::Symbol| -> Option<Side> {
        ecad.cad_data
            .layers
            .iter()
            .find(|layer| layer.name == name && layer.layer_function == LayerFunction::Conductor)
            .and_then(|layer| layer.side)
    };

    let mut holes: Vec<([f64; 2], f64)> = Vec::new();
    let mut fids_top = Vec::new();
    let mut fids_bottom = Vec::new();
    for layer_feature in &step.layer_features {
        let on_drill = drill_layers.contains(&layer_feature.layer_ref);
        let side = copper_side(layer_feature.layer_ref);
        for set in &layer_feature.sets {
            for feature in &set.features {
                match feature {
                    SetFeature::Hole(hole)
                        if on_drill && hole.plating_status == PlatingStatus::NonPlated =>
                    {
                        holes.push((flip([hole.x, hole.y]), hole.diameter));
                    }
                    SetFeature::Fiducial(fiducial) if fiducial.kind == FiducialKind::Global => {
                        let at = flip([fiducial.location.x, fiducial.location.y]);
                        match side {
                            Some(Side::Top) => fids_top.push(at),
                            Some(Side::Bottom) => fids_bottom.push(at),
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    if holes.is_empty() {
        bail!("panel has no non-plated tooling holes; is this a board-array file?");
    }

    // The folded A7 tile's corner tooling (fixture connector plate
    // registration); exactly coincident holes collapse.
    let (mate_w, mate_h) = mate_dims(width, height);
    let inset = CORNER_TOOLING_INSET_MM;
    for xy in [
        [inset, inset],
        [mate_w - inset, inset],
        [mate_w - inset, mate_h - inset],
        [inset, mate_h - inset],
    ] {
        if !holes
            .iter()
            .any(|(hole, _)| (hole[0] - xy[0]).abs() < EPS && (hole[1] - xy[1]).abs() < EPS)
        {
            holes.push((xy, CORNER_TOOLING_DIA_MM));
        }
    }

    Ok(Panel {
        width,
        height,
        outline,
        holes,
        fids_top,
        fids_bottom,
    })
}

/// The step the file is about: the Content section's first step reference,
/// falling back to document order.
fn primary_step<'a>(ipc: &Ipc2581, steps: &'a [Step]) -> Option<&'a Step> {
    ipc.content()
        .step_refs
        .first()
        .and_then(|step_ref| steps.iter().find(|step| step.name == *step_ref))
        .or_else(|| steps.first())
}
