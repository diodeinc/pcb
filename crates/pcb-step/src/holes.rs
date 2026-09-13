//! Round holes as radius profiles.
//!
//! A round hole through the body is described by its radius as a function
//! of depth: a list of `(z, r)` points from the top down. Equal radii make a
//! cylinder, equal heights a flat shoulder, and both changing a cone. That
//! one description covers plain drills, blind vias, backdrills,
//! counterbores and countersinks, and each becomes exact analytic faces.
//!
//! The body is dielectric plus via fill, so a filled or capped via is not
//! cut, and mask features (tenting, covering, plugging) leave it alone.
//! Machining depths are measured from the outer copper surface, as KiCad
//! measures them.

use crate::board::{Backdrill, Machining, Mouth, Physical, Via};
use crate::geom::Vec2;

/// A round hole: its centre and radius profile, top down. A radius of zero
/// at either end is the flat floor of a pocket.
pub(crate) struct RoundHole {
    pub(crate) center: Vec2,
    pub(crate) profile: Vec<(f64, f64)>,
    /// Radius of the plain drill to fall back to when the machined shape
    /// cannot be placed; `None` when there is no through drill.
    pub(crate) fallback: Option<f64>,
}

impl RoundHole {
    pub(crate) fn max_radius(&self) -> f64 {
        self.profile.iter().map(|p| p.1).fold(0.0, f64::max)
    }

    /// Whether the profile is a plain cylinder through the body.
    pub(crate) fn is_plain(&self) -> bool {
        self.profile.len() == 2 && self.profile[0].1 == self.profile[1].1
    }
}

/// Where a drill starts and ends in z, and what is machined around it.
pub(crate) struct DrillSpec {
    pub(crate) r: f64,
    pub(crate) z_top: f64,
    pub(crate) z_bottom: f64,
    pub(crate) machining: Machining,
}

const EPS: f64 = 1e-9;

pub(crate) fn pad_hole(
    center: Vec2,
    r: f64,
    machining: Machining,
    physical: &Physical,
) -> Option<RoundHole> {
    profile(
        center,
        DrillSpec {
            r,
            z_top: physical.body_top,
            z_bottom: 0.0,
            machining,
        },
        physical,
        &mut Vec::new(),
    )
}

/// The hole a via cuts, or `None` when it cuts nothing.
pub(crate) fn via_hole(
    center: Vec2,
    via: &Via,
    physical: &Physical,
    warnings: &mut Vec<String>,
) -> Option<RoundHole> {
    let last = physical.copper_z.len().saturating_sub(1) as u32;
    if via.filled {
        // Only a backdrill removes material from a filled via.
        let mut hole: Option<RoundHole> = None;
        for backdrill in via.machining.backdrills.iter().flatten() {
            let Some(pocket) = backdrill_pocket(center, *backdrill, physical, last) else {
                continue;
            };
            if hole.is_some() {
                warnings.push(format!(
                    "via at ({:.3}, {:.3}) mm: only one backdrill is cut on a filled via",
                    center.x, -center.y
                ));
                break;
            }
            hole = Some(pocket);
        }
        return hole;
    }
    let (z_top, z_bottom) = if via.top == 0 && via.bottom == last {
        (physical.body_top, 0.0)
    } else if via.top == 0 {
        (physical.body_top, physical.copper_z[via.bottom as usize].0)
    } else if via.bottom == last {
        (physical.copper_z[via.top as usize].1, 0.0)
    } else {
        warnings.push(format!(
            "skipped buried via at ({:.3}, {:.3}) mm; the body has no face to cut it from",
            center.x, -center.y
        ));
        return None;
    };
    profile(
        center,
        DrillSpec {
            r: via.drill * 0.5,
            z_top,
            z_bottom,
            machining: via.machining,
        },
        physical,
        warnings,
    )
}

/// The pocket a backdrill leaves on its own, with no drill through it.
fn backdrill_pocket(
    center: Vec2,
    backdrill: Backdrill,
    physical: &Physical,
    last: u32,
) -> Option<RoundHole> {
    let z_end = |layer: u32| physical.copper_z.get(layer as usize).copied();
    let (floor, from_top) = if backdrill.start == 0 {
        (z_end(backdrill.end)?.0, true)
    } else if backdrill.start == last {
        (z_end(backdrill.end)?.1, false)
    } else {
        return None;
    };
    let floor = floor.clamp(0.0, physical.body_top);
    let r = backdrill.r;
    let profile = if from_top {
        if floor <= EPS {
            vec![(physical.body_top, r), (0.0, r)]
        } else {
            vec![(physical.body_top, r), (floor, r), (floor, 0.0)]
        }
    } else if floor >= physical.body_top - EPS {
        vec![(physical.body_top, r), (0.0, r)]
    } else {
        vec![(floor, 0.0), (floor, r), (0.0, r)]
    };
    Some(RoundHole {
        center,
        profile,
        fallback: None,
    })
}

/// Assemble a drill's profile: what is machined at the top face, the drill
/// itself, and what is machined at the bottom face.
fn profile(
    center: Vec2,
    spec: DrillSpec,
    physical: &Physical,
    warnings: &mut Vec<String>,
) -> Option<RoundHole> {
    let body_top = physical.body_top;
    let last = physical.copper_z.len().saturating_sub(1) as u32;
    let reaches_top = spec.z_top >= body_top - EPS;
    let reaches_bottom = spec.z_bottom <= EPS;
    let mut dropped = |what: &str| {
        warnings.push(format!(
            "drill at ({:.3}, {:.3}) mm: {what}",
            center.x, -center.y
        ))
    };

    // Top part, ending at the drill radius.
    let mut top: Vec<(f64, f64)> = Vec::new();
    let front_backdrill = spec
        .machining
        .backdrills
        .iter()
        .flatten()
        .find(|b| b.start == 0)
        .copied();
    if reaches_top {
        if let Some(mouth) = spec.machining.front {
            top = mouth_from_top(mouth, spec.r, body_top, physical.front_copper, &mut dropped);
        } else if let Some(backdrill) = front_backdrill {
            top = backdrill_from_top(backdrill, spec.r, body_top, physical, &mut dropped);
        }
        if top.is_empty() {
            top.push((body_top, spec.r));
        }
    } else {
        top.push((spec.z_top, 0.0));
        top.push((spec.z_top, spec.r));
    }

    // Bottom part, starting at the drill radius.
    let mut bottom: Vec<(f64, f64)> = Vec::new();
    let back_backdrill = spec
        .machining
        .backdrills
        .iter()
        .flatten()
        .find(|b| b.start == last && last != 0)
        .copied();
    if reaches_bottom {
        if let Some(mouth) = spec.machining.back {
            bottom = mouth_from_bottom(mouth, spec.r, physical.back_copper, &mut dropped);
        } else if let Some(backdrill) = back_backdrill {
            bottom = backdrill_from_bottom(backdrill, spec.r, physical, &mut dropped);
        }
        if bottom.is_empty() {
            bottom.push((0.0, spec.r));
        }
    } else {
        bottom.push((spec.z_bottom, spec.r));
        bottom.push((spec.z_bottom, 0.0));
    }

    if top.last().unwrap().0 < bottom[0].0 + EPS {
        dropped("machining from both faces overlaps; using the plain drill");
        top = vec![(spec.z_top, spec.r)];
        bottom = vec![(spec.z_bottom, spec.r)];
        if !reaches_top {
            top.insert(0, (spec.z_top, 0.0));
        }
        if !reaches_bottom {
            bottom.push((spec.z_bottom, 0.0));
        }
    }
    let mut profile = top;
    for point in bottom {
        if profile
            .last()
            .is_none_or(|p| (p.0 - point.0).abs() > EPS || (p.1 - point.1).abs() > EPS)
        {
            profile.push(point);
        }
    }
    Some(RoundHole {
        center,
        profile,
        fallback: (reaches_top && reaches_bottom).then_some(spec.r),
    })
}

/// Points from the top face down to where the drill radius takes over.
fn mouth_from_top(
    mouth: Mouth,
    r: f64,
    body_top: f64,
    copper: f64,
    dropped: &mut impl FnMut(&str),
) -> Vec<(f64, f64)> {
    let surface = body_top + copper;
    match mouth {
        Mouth::Counterbore { r: bore, depth } => {
            let floor = surface - depth;
            if bore <= r + EPS {
                dropped("counterbore is no wider than the drill; ignored");
                return Vec::new();
            }
            if floor >= body_top - EPS {
                return Vec::new();
            }
            if floor <= EPS {
                return vec![(body_top, bore), (0.0, bore), (0.0, r)];
            }
            vec![(body_top, bore), (floor, bore), (floor, r)]
        }
        Mouth::Countersink {
            r: sink,
            depth,
            half_angle,
        } => {
            let slope = half_angle.tan();
            let r_top = sink - copper * slope;
            if r_top <= r + EPS {
                return Vec::new();
            }
            let depth = depth.unwrap_or(sink / slope);
            let z_floor = surface - depth;
            let r_floor = (sink - depth * slope).max(0.0);
            if r_floor >= r {
                // The cone ends above the drill radius on a flat shoulder.
                if z_floor <= EPS {
                    let r_at_zero = sink - surface * slope;
                    return vec![(body_top, r_top), (0.0, r_at_zero), (0.0, r)];
                }
                let mut points = vec![(body_top, r_top), (z_floor, r_floor)];
                if r_floor > r + EPS {
                    points.push((z_floor, r));
                }
                points
            } else {
                let z_meet = surface - (sink - r) / slope;
                if z_meet <= EPS {
                    let r_at_zero = sink - surface * slope;
                    return vec![(body_top, r_top), (0.0, r_at_zero.max(r))];
                }
                vec![(body_top, r_top), (z_meet, r)]
            }
        }
    }
}

/// Points from where the drill radius ends down to the bottom face.
fn mouth_from_bottom(
    mouth: Mouth,
    r: f64,
    copper: f64,
    dropped: &mut impl FnMut(&str),
) -> Vec<(f64, f64)> {
    let surface = -copper;
    match mouth {
        Mouth::Counterbore { r: bore, depth } => {
            let ceiling = surface + depth;
            if bore <= r + EPS {
                dropped("counterbore is no wider than the drill; ignored");
                return Vec::new();
            }
            if ceiling <= EPS {
                return Vec::new();
            }
            vec![(ceiling, r), (ceiling, bore), (0.0, bore)]
        }
        Mouth::Countersink {
            r: sink,
            depth,
            half_angle,
        } => {
            let slope = half_angle.tan();
            let r_bottom = sink - copper * slope;
            if r_bottom <= r + EPS {
                return Vec::new();
            }
            let depth = depth.unwrap_or(sink / slope);
            let z_ceiling = surface + depth;
            let r_ceiling = (sink - depth * slope).max(0.0);
            if r_ceiling >= r {
                let mut points = Vec::new();
                if r_ceiling > r + EPS {
                    points.push((z_ceiling, r));
                }
                points.push((z_ceiling, r_ceiling));
                points.push((0.0, r_bottom));
                points
            } else {
                let z_meet = surface + (sink - r) / slope;
                vec![(z_meet, r), (0.0, r_bottom)]
            }
        }
    }
}

fn backdrill_from_top(
    backdrill: Backdrill,
    r: f64,
    body_top: f64,
    physical: &Physical,
    dropped: &mut impl FnMut(&str),
) -> Vec<(f64, f64)> {
    let Some(layer) = physical.copper_z.get(backdrill.end as usize) else {
        return Vec::new();
    };
    if backdrill.r <= r + EPS {
        dropped("backdrill is no wider than the drill; ignored");
        return Vec::new();
    }
    let floor = layer.0.clamp(0.0, body_top);
    if floor <= EPS {
        return vec![(body_top, backdrill.r), (0.0, backdrill.r), (0.0, r)];
    }
    vec![(body_top, backdrill.r), (floor, backdrill.r), (floor, r)]
}

fn backdrill_from_bottom(
    backdrill: Backdrill,
    r: f64,
    physical: &Physical,
    dropped: &mut impl FnMut(&str),
) -> Vec<(f64, f64)> {
    let Some(layer) = physical.copper_z.get(backdrill.end as usize) else {
        return Vec::new();
    };
    if backdrill.r <= r + EPS {
        dropped("backdrill is no wider than the drill; ignored");
        return Vec::new();
    }
    let ceiling = layer.1.clamp(0.0, physical.body_top);
    vec![(ceiling, r), (ceiling, backdrill.r), (0.0, backdrill.r)]
}
