//! Single-board interposers: one singulated DUT in a printed nest instead
//! of a panel.
//!
//! The board step alone fixes the contact pattern — a board-array export's
//! boards are copies of it — so the input is a board export or a panel
//! export alike. The generator places the board on the bed itself. It
//! lays the board's probed face down, mirroring a board probed on its top
//! face, tries the four quarter turns, and slides it over the bed in
//! [`STEP_MM`] steps. A placement fits when every pogo sits inside the
//! bed's envelope across the pogo rows and clear of the bed's alignment
//! features and fiducials; along the rows the board may overhang the bed,
//! and the interposer and plate grow to carry it and its nest. Among the
//! placements that fit, it takes the one with the shortest total distance
//! from each contact to the nearest bed land of its role, then the one
//! nearest the bed's center.

use anyhow::{Context, Result, bail};
use ipc2581::Ipc2581;
use ipc2581::types::Side;

use crate::bed::Bed;
use crate::contacts::{Footprints, PanelContact, bom_roles, component_side, parse_role};
use crate::panel::{Outline, Panel};
use crate::pattern::{A7_H, A7_W, Role};

/// Placement search resolution.
pub const STEP_MM: f64 = 0.5;

/// How the board's own frame maps onto the bed's sheet frame (Y down):
/// `sheet = R(quarter_turns · 90°) · M · (x, -y) + offset`, where `(x, y)`
/// is a point in the board's IPC-2581 frame (Y up) and `M` negates x when
/// the board lies face down on its top face.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub quarter_turns: u8,
    pub mirrored: bool,
    pub offset: [f64; 2],
}

impl Placement {
    /// Map a board-frame (Y-up) point into the sheet frame.
    pub fn apply(&self, [x, y]: [f64; 2]) -> [f64; 2] {
        let [u, v] = turn(local([x, y], self.mirrored), self.quarter_turns);
        [u + self.offset[0], v + self.offset[1]]
    }
}

/// A single board placed on its bed.
#[derive(Debug)]
pub struct Placed {
    /// The interposer's fixed features: the bed's envelope, grown along
    /// the pogo rows to carry the board and its nest.
    pub panel: Panel,
    pub contacts: Vec<PanelContact>,
    /// The board's bounding box in the sheet frame.
    pub board: [f64; 4],
    pub placement: Placement,
}

/// Board frame → Y-down frame, face down when `mirrored`.
fn local([x, y]: [f64; 2], mirrored: bool) -> [f64; 2] {
    [if mirrored { -x } else { x }, -y]
}

/// Rotate a Y-down point by quarter turns.
fn turn([u, v]: [f64; 2], quarter_turns: u8) -> [f64; 2] {
    match quarter_turns % 4 {
        0 => [u, v],
        1 => [-v, u],
        2 => [-u, -v],
        _ => [v, -u],
    }
}

fn distance(a: [f64; 2], b: [f64; 2]) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

/// Place the board step of `ipc` on `bed`, probing the test points
/// `footprints` admits.
pub fn place(ipc: &Ipc2581, bed: Bed, footprints: Footprints) -> Result<Placed> {
    let Some([ex0, ey0, ex1, ey1]) = bed.envelope() else {
        bail!("the {bed:?} bed has no envelope to place a single board in");
    };
    let ecad = ipc.ecad().context("board has no ECAD section")?;
    let steps = &ecad.cad_data.steps;
    let primary = pcb_ir::import::ipc2581::primary_step(ipc, steps).context("board has no step")?;
    let board = if primary.components.is_empty() {
        steps
            .iter()
            .find(|step| !step.components.is_empty() && step.profile.is_some())
            .context("no board step with components")?
    } else {
        primary
    };
    let profile = board
        .profile
        .as_ref()
        .context("board step has no profile")?;
    let corners: Vec<[f64; 2]> = {
        let points = profile.polygon.points();
        let x0 = points.iter().map(|p| p.x).fold(f64::MAX, f64::min);
        let x1 = points.iter().map(|p| p.x).fold(f64::MIN, f64::max);
        let y0 = points.iter().map(|p| p.y).fold(f64::MAX, f64::min);
        let y1 = points.iter().map(|p| p.y).fold(f64::MIN, f64::max);
        vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]]
    };

    let roles = bom_roles(ipc, footprints);
    let mut found: Vec<(PanelContact, [f64; 2])> = Vec::new();
    let mut sides: Vec<(String, Option<Side>)> = Vec::new();
    for component in &board.components {
        let Some(ref_des) = component.ref_des else {
            continue;
        };
        let refdes = ipc.resolve(ref_des).to_string();
        let Some((ict, path)) = roles.get(&refdes) else {
            continue;
        };
        let Some(role) = parse_role(ict) else {
            eprintln!("warning: skipped {refdes}: unrecognized ict role {ict}");
            continue;
        };
        sides.push((refdes.clone(), component_side(ipc, component)));
        found.push((
            PanelContact {
                board: 0,
                refdes,
                path: path.clone(),
                role,
                ict: ict.clone(),
                xy: [0.0, 0.0],
            },
            [component.location.x, component.location.y],
        ));
    }
    if found.is_empty() {
        bail!("board has no ICT contacts; mark test points with the TestPoint `ict` config");
    }
    let on = |side: Side| -> Vec<&str> {
        sides
            .iter()
            .filter(|(_, s)| *s == Some(side))
            .map(|(refdes, _)| refdes.as_str())
            .collect()
    };
    let (top, bottom) = (on(Side::Top), on(Side::Bottom));
    if !top.is_empty() && !bottom.is_empty() {
        bail!(
            "the board has ICT contacts on both faces (top: {}; bottom: {}); a fixture probes one",
            top.join(", "),
            bottom.join(", ")
        );
    }
    let mirrored = !top.is_empty();

    let lands = bed.lands(A7_H, A7_W)?;
    let r = crate::pogo::COURTYARD_RADIUS_MM;
    let alignment = bed.alignment();
    // The bed's fiducials, as `pogo_keepouts` grows them.
    let fiducials: Vec<([f64; 2], f64)> = bed
        .fiducials()
        .into_iter()
        .map(|at| (at, 1.0 + r + 0.5))
        .collect();
    let center = [(ex0 + ex1) / 2.0, (ey0 + ey1) / 2.0];
    let um = |d: f64| (d * 1000.0).round() as i64;

    let mut best: Option<((i64, i64), Placement)> = None;
    for quarter_turns in 0..4u8 {
        let pts: Vec<[f64; 2]> = found
            .iter()
            .map(|(_, xy)| turn(local(*xy, mirrored), quarter_turns))
            .collect();
        let (lo_x, hi_x, lo_y, hi_y) = pts.iter().fold(
            (f64::MAX, f64::MIN, f64::MAX, f64::MIN),
            |(a, b, c, d), p| (a.min(p[0]), b.max(p[0]), c.min(p[1]), d.max(p[1])),
        );
        let mid_x = (lo_x + hi_x) / 2.0;
        // Pogos across the rows stay inside the envelope; along them the
        // contacts' middle stays over the bed.
        let (oy0, oy1) = (ey0 + r - lo_y, ey1 - r - hi_y);
        let (ox0, ox1) = (ex0 - mid_x, ex1 - mid_x);
        let ny = ((oy1 - oy0) / STEP_MM).floor();
        if ny < 0.0 {
            continue;
        }
        let nx = ((ox1 - ox0) / STEP_MM).floor() as i64;
        for i in 0..=nx {
            for j in 0..=ny as i64 {
                let offset = [ox0 + i as f64 * STEP_MM, oy0 + j as f64 * STEP_MM];
                let at = |p: [f64; 2]| [p[0] + offset[0], p[1] + offset[1]];
                let fits = pts.iter().all(|&p| {
                    let p = at(p);
                    alignment
                        .iter()
                        .all(|a| distance(p, a.at) >= a.keepout_dia / 2.0 + r)
                        && fiducials.iter().all(|(f, reach)| distance(p, *f) >= *reach)
                });
                if !fits {
                    continue;
                }
                let wire: f64 = found
                    .iter()
                    .zip(&pts)
                    .filter(|((contact, _), _)| contact.role != Role::Gnd)
                    .filter_map(|((contact, _), &p)| {
                        let p = at(p);
                        lands
                            .iter()
                            .filter(|land| land.role == contact.role)
                            .map(|land| distance(p, land.xy))
                            .min_by(f64::total_cmp)
                    })
                    .sum();
                let score = (
                    um(wire),
                    um(distance(at([mid_x, (lo_y + hi_y) / 2.0]), center)),
                );
                if best.as_ref().is_none_or(|(s, _)| score < *s) {
                    best = Some((
                        score,
                        Placement {
                            quarter_turns,
                            mirrored,
                            offset,
                        },
                    ));
                }
            }
        }
    }
    let Some((_, placement)) = best else {
        bail!(
            "no placement fits the board's contacts on the {bed:?} bed: across its pogo rows \
             they need to fit y {:.1}–{:.1} mm, clear of its alignment features and fiducials",
            ey0 + r,
            ey1 - r
        );
    };

    let contacts: Vec<PanelContact> = found
        .into_iter()
        .map(|(mut contact, xy)| {
            contact.xy = placement.apply(xy);
            contact
        })
        .collect();
    let placed: Vec<[f64; 2]> = corners.iter().map(|&c| placement.apply(c)).collect();
    let board = placed.iter().fold(
        [f64::MAX, f64::MAX, f64::MIN, f64::MIN],
        |[a, b, c, d], p| [a.min(p[0]), b.min(p[1]), c.max(p[0]), d.max(p[1])],
    );
    let reach = crate::shroud::nest_reach();
    let envelope = [
        ex0.min(board[0] - reach),
        ey0,
        ex1.max(board[2] + reach),
        ey1,
    ];
    let [x0, y0, x1, y1] = envelope;
    let outline = [[x0, y0], [x1, y0], [x1, y1], [x0, y1]];
    let panel = Panel {
        width: A7_H,
        height: A7_W,
        outline: (0..4)
            .map(|i| Outline::Line {
                start: outline[i],
                end: outline[(i + 1) % 4],
            })
            .collect(),
        holes: Vec::new(),
        fids_top: bed.fiducials(),
        fids_bottom: Vec::new(),
        mouse_bites: Vec::new(),
        envelope: Some(envelope),
    };
    Ok(Placed {
        panel,
        contacts,
        board,
        placement,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_maps_board_points() {
        let p = Placement {
            quarter_turns: 1,
            mirrored: true,
            offset: [10.0, 20.0],
        };
        // (2, 3) Y-up → face down (-2, -3) → quarter turn (3, -2) → offset.
        assert_eq!(p.apply([2.0, 3.0]), [13.0, 18.0]);
    }
}
