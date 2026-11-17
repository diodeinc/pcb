//! The DUT plate: a printed slab on the interposer's top face that fills
//! the space up to the DUT panel and stops it at the pogos' working
//! travel.
//!
//! Its top face is the DUT stop, so its thickness is the pogo's free
//! height less the working travel (half the stroke, the nominal the
//! Switchyard bed uses for its own pins). It drops onto the bed's
//! alignment dowels through the interposer — a round hole on D1, a slot
//! on D2 — clears the M3, and gives every pogo a clearance hole. The DUT
//! panel's tooling holes sit in its rails, outside the envelope, so the
//! last millimeters of DUT alignment come from locators standing up from
//! the top face into the routed channels around the boards, against the
//! board edges and clear of the break-away tabs: printed tapered nubs, or
//! short M2 dowel pins pressed into printed holes for interposers that
//! see more insertions.
//!
//! The plate is printed, and printed holes come out small, so every
//! clearance hole is drawn [`PRINT_ALLOWANCE_MM`] over what it must
//! clear. Pogos too close for a printable web between their holes share
//! one slot. Four screw ears off the plate's sides fasten the plate, and
//! the interposer under it, to the bed, and the bed's clamping feature
//! (Switchyard's M3) holds its middle down against the bed pogos, which a
//! printed plate held at its ends alone would bow under; where the bed's screws land is
//! not settled, so the ears' positions and heights are variables at the
//! top of the file.
//!
//! The plate is emitted as OpenSCAD source in its print orientation
//! (top face up, Y up), so the part stays a readable, adjustable text
//! artifact; `openscad -o plate.stl plate.scad` renders it.

use std::fmt::Write as _;

use anyhow::{Result, bail};

use crate::bed::Bed;
use crate::panel::Panel;
use crate::plan::Plan;
use crate::pogo;

/// How the plate locates the DUT panel.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Locators {
    /// Printed tapered nubs.
    #[default]
    Nubs,
    /// Ø2 mm dowel pins pressed into printed holes.
    Dowels,
}

impl std::str::FromStr for Locators {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "nubs" => Ok(Self::Nubs),
            "dowels" => Ok(Self::Dowels),
            _ => Err("expected nubs or dowels".into()),
        }
    }
}

/// How much a printed hole comes out under its drawn diameter.
pub const PRINT_ALLOWANCE_MM: f64 = 0.5;
/// Clearance hole around each pogo's land and solder fillet.
const POGO_HOLE_DIA_MM: f64 = pogo::LAND_DIA_MM + 0.5 + PRINT_ALLOWANCE_MM;
/// The thinnest wall that prints between two holes; closer holes merge.
const MIN_WEB_MM: f64 = 0.8;
/// Screws: M3 clearance, and a counterbore that sinks an M3 socket head
/// below the top face, under the DUT panel; the ears put a boss around
/// it.
const SCREW_DIA_MM: f64 = 3.4;
const SCREW_HEAD_DIA_MM: f64 = 5.5;
const SCREW_HEAD_HEIGHT_MM: f64 = 3.0;
const EAR_DIA_MM: f64 = 9.0;
/// Ear centers sit this far out from the plate's sides, this far in
/// from its ends.
const EAR_OUT_MM: f64 = 6.0;
const EAR_IN_MM: f64 = 10.0;
/// The interposer's thickness, which the ears reach down past to seat
/// on the bed, less a gap that leaves the screws clamping the board.
const BOARD_THICKNESS_MM: f64 = 1.6;
const CLAMP_GAP_MM: f64 = 0.2;
/// Locator diameter (a nub's base, or the dowel pin), and how far it
/// stands above the top face: below the DUT panel's thickness.
const LOCATOR_DIA_MM: f64 = 2.0;
const LOCATOR_HEIGHT_MM: f64 = 1.2;
/// A nub's tip diameter: the taper is the lead-in.
const NUB_TIP_DIA_MM: f64 = 1.2;
/// Gap between a locator and the board edge it registers.
const EDGE_GAP_MM: f64 = 0.15;
/// A locator keeps this far from the plate edge, a tab's mouse bites, and
/// a pogo hole.
const PLATE_MARGIN_MM: f64 = 1.0;
const TAB_MARGIN_MM: f64 = 1.0;
const HOLE_MARGIN_MM: f64 = 0.4;
/// Dowel holes print under the pin's diameter, to be reamed or bonded,
/// and keep a floor under the pin.
const DOWEL_HOLE_DIA_MM: f64 = 1.9;
const DOWEL_FLOOR_MM: f64 = 0.8;
const DOWEL_LENGTHS_MM: [f64; 5] = [4.0, 5.0, 6.0, 8.0, 10.0];

/// The plate's thickness: the pogo tip height at working travel.
pub fn thickness() -> f64 {
    pogo::FREE_HEIGHT_MM - pogo::STROKE_MM / 2.0
}

/// A locator's spot: its center and the board edge it pushes against.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Locator {
    pub at: [f64; 2],
    /// Which side of its board it stands on: `-x`, `+x`, `-y` or `+y`.
    pub side: &'static str,
}

/// Up to two locators per side of the boards, as far apart as the
/// channels allow, in the Y-down sheet frame.
pub fn locators(panel: &Panel, bed: Bed, plan: &Plan, boards: &[[f64; 4]]) -> Vec<Locator> {
    let r = LOCATOR_DIA_MM / 2.0;
    let reach = r + EDGE_GAP_MM;
    let [ex0, ey0, ex1, ey1] = bed
        .envelope()
        .unwrap_or([0.0, 0.0, panel.width, panel.height]);
    let pogos: Vec<[f64; 2]> = plan
        .bindings
        .iter()
        .map(|binding| plan.contacts[binding.contact].xy)
        .collect();
    let alignment = bed.alignment();
    let clear = |at: [f64; 2]| {
        let inside = at[0] - r >= ex0 + PLATE_MARGIN_MM
            && at[0] + r <= ex1 - PLATE_MARGIN_MM
            && at[1] - r >= ey0 + PLATE_MARGIN_MM
            && at[1] + r <= ey1 - PLATE_MARGIN_MM;
        let off_boards = boards.iter().all(|&[x0, y0, x1, y1]| {
            let dx = (x0 - at[0]).max(at[0] - x1).max(0.0);
            let dy = (y0 - at[1]).max(at[1] - y1).max(0.0);
            dx.hypot(dy) >= reach - 1e-6
        });
        let distance = |p: [f64; 2]| (p[0] - at[0]).hypot(p[1] - at[1]);
        inside
            && off_boards
            && panel
                .mouse_bites
                .iter()
                .all(|&p| distance(p) >= r + TAB_MARGIN_MM)
            && pogos
                .iter()
                .all(|&p| distance(p) >= r + POGO_HOLE_DIA_MM / 2.0 + HOLE_MARGIN_MM)
            && alignment
                .iter()
                .all(|a| distance(a.at) >= r + a.keepout_dia / 2.0)
    };
    let mut candidates: Vec<Locator> = Vec::new();
    for &[x0, y0, x1, y1] in boards {
        for k in 2..=8 {
            let t = k as f64 / 10.0;
            let (x, y) = (x0 + t * (x1 - x0), y0 + t * (y1 - y0));
            for locator in [
                Locator {
                    at: [x0 - reach, y],
                    side: "-x",
                },
                Locator {
                    at: [x1 + reach, y],
                    side: "+x",
                },
                Locator {
                    at: [x, y0 - reach],
                    side: "-y",
                },
                Locator {
                    at: [x, y1 + reach],
                    side: "+y",
                },
            ] {
                if clear(locator.at) {
                    candidates.push(locator);
                }
            }
        }
    }
    let mut chosen = Vec::new();
    for side in ["-x", "+x", "-y", "+y"] {
        let along = usize::from(side.ends_with('x'));
        let mut on_side: Vec<Locator> = candidates
            .iter()
            .copied()
            .filter(|l| l.side == side)
            .collect();
        on_side.sort_by(|a, b| a.at[along].total_cmp(&b.at[along]));
        match on_side.as_slice() {
            [] => {}
            [only] => chosen.push(*only),
            [first, .., last] => chosen.extend([*first, *last]),
        }
    }
    chosen
}

/// The plate's OpenSCAD source.
pub fn plate(
    panel: &Panel,
    bed: Bed,
    plan: &Plan,
    boards: &[[f64; 4]],
    style: Locators,
) -> Result<String> {
    if bed.alignment().is_empty() {
        bail!("the {bed:?} bed has no alignment dowels to seat a DUT plate on");
    }
    let t = thickness();
    let [x0, y0, x1, y1] = bed
        .envelope()
        .unwrap_or([0.0, 0.0, panel.width, panel.height]);
    // Print orientation: Y up.
    let up = |p: [f64; 2]| [p[0], panel.height - p[1]];
    let located = locators(panel, bed, plan, boards);
    let mut sides: Vec<&str> = located.iter().map(|l| l.side).collect();
    sides.dedup();

    let head_dia = SCREW_HEAD_DIA_MM + PRINT_ALLOWANCE_MM;

    let mut out = String::new();
    let w = &mut out;
    writeln!(w, "// DUT plate, generated by pcb-interposer.").unwrap();
    writeln!(
        w,
        "// Top face at the pogos' working travel: {:.2} mm free height - {:.2} mm.",
        pogo::FREE_HEIGHT_MM,
        pogo::STROKE_MM / 2.0
    )
    .unwrap();
    if sides.len() < 4 {
        writeln!(
            w,
            "// Warning: locators on {} side(s) only: {}.",
            sides.len(),
            sides.join(" ")
        )
        .unwrap();
    }
    writeln!(w, "$fn = 48;\nthickness = {t:.3};\n").unwrap();
    let [a, b] = up([x0, y1]);
    let [c, d] = up([x1, y0]);
    let ears = [
        [a - EAR_OUT_MM, b + EAR_IN_MM],
        [a - EAR_OUT_MM, d - EAR_IN_MM],
        [c + EAR_OUT_MM, b + EAR_IN_MM],
        [c + EAR_OUT_MM, d - EAR_IN_MM],
    ];
    writeln!(
        w,
        "// Screw ears fastening the plate and interposer to the bed. Placeholder\n\
         // positions: set them, and the heights, to the bed's screws.\n\
         ears = [{}];\n\
         ear_d = {EAR_DIA_MM};\n\
         // Ear bottom relative to the plate's bottom face (the interposer's top):\n\
         // down past the board to the bed, less a gap so the screw clamps the board.\n\
         ear_bottom = {:.2};\n\
         ear_top = thickness;\n\
         screw_d = {:.2}; // M3 clearance + print allowance\n\
         head_d = {head_dia:.2};\n\
         head_h = {SCREW_HEAD_HEIGHT_MM};\n",
        ears.iter()
            .map(|[x, y]| format!("[{x:.3}, {y:.3}]"))
            .collect::<Vec<_>>()
            .join(", "),
        -(BOARD_THICKNESS_MM - CLAMP_GAP_MM),
        SCREW_DIA_MM + PRINT_ALLOWANCE_MM,
    )
    .unwrap();
    writeln!(w, "difference() {{\n  union() {{").unwrap();
    writeln!(
        w,
        "    translate([{a:.3}, {b:.3}, 0]) cube([{:.3}, {:.3}, thickness]);",
        x1 - x0,
        y1 - y0
    )
    .unwrap();
    // Each ear is joined to the plate's side by a web of the plate's own
    // thickness, and its boss reaches down to ear_bottom.
    writeln!(
        w,
        "    for (e = ears) {{\n      \
         hull() {{\n        \
         translate([e[0], e[1], 0]) cylinder(h = thickness, d = ear_d);\n        \
         translate([min(max(e[0], {a:.3}), {c:.3}), min(max(e[1], {b:.3}), {d:.3}), thickness / 2]) cube([0.01, ear_d, thickness], center = true);\n      \
         }}\n      \
         translate([e[0], e[1], ear_bottom]) cylinder(h = ear_top - ear_bottom, d = ear_d);\n    \
         }}"
    )
    .unwrap();
    if style == Locators::Nubs {
        for locator in &located {
            let [x, y] = up(locator.at);
            writeln!(
                w,
                "    translate([{x:.3}, {y:.3}, thickness]) cylinder(h = {LOCATOR_HEIGHT_MM}, d1 = {LOCATOR_DIA_MM}, d2 = {NUB_TIP_DIA_MM}); // {}",
                locator.side
            )
            .unwrap();
        }
    }
    writeln!(w, "  }}").unwrap();
    writeln!(
        w,
        "  for (e = ears) {{\n    \
         translate([e[0], e[1], ear_bottom - 1]) cylinder(h = ear_top - ear_bottom + 2, d = screw_d);\n    \
         translate([e[0], e[1], ear_top - head_h]) cylinder(h = head_h + 1, d = head_d);\n  \
         }}"
    )
    .unwrap();
    // The ears' bosses reach below the plate; keep them off the
    // interposer by cutting its outline (plus allowance) out of them.
    writeln!(
        w,
        "  translate([{:.3}, {:.3}, ear_bottom - 1]) cube([{:.3}, {:.3}, -ear_bottom + 1]);",
        a - PRINT_ALLOWANCE_MM / 2.0,
        b - PRINT_ALLOWANCE_MM / 2.0,
        x1 - x0 + PRINT_ALLOWANCE_MM,
        y1 - y0 + PRINT_ALLOWANCE_MM,
    )
    .unwrap();
    let pogos: Vec<[f64; 2]> = plan
        .bindings
        .iter()
        .map(|binding| up(plan.contacts[binding.contact].xy))
        .collect();
    let hole = |[x, y]: [f64; 2]| {
        format!(
            "translate([{x:.3}, {y:.3}, -1]) cylinder(h = thickness + 2, d = {POGO_HOLE_DIA_MM:.2});"
        )
    };
    let mut merged = vec![false; pogos.len()];
    for (i, &p) in pogos.iter().enumerate() {
        for (j, &q) in pogos.iter().enumerate().skip(i + 1) {
            if (p[0] - q[0]).hypot(p[1] - q[1]) < POGO_HOLE_DIA_MM + MIN_WEB_MM {
                writeln!(w, "  hull() {{ {} {} }}", hole(p), hole(q)).unwrap();
                merged[i] = true;
                merged[j] = true;
            }
        }
    }
    for (&p, merged) in pogos.iter().zip(&merged) {
        if !merged {
            writeln!(w, "  {}", hole(p)).unwrap();
        }
    }
    for alignment in bed.alignment() {
        let [x, y] = up(alignment.at);
        let [dx, dy] = alignment.drill;
        let [dx, dy] = [dx + PRINT_ALLOWANCE_MM, dy + PRINT_ALLOWANCE_MM];
        let d = dx.min(dy);
        writeln!(
            w,
            "  translate([{x:.3}, {y:.3}, -1]) hull() {{ // {}\n    translate([{:.3}, {:.3}, 0]) cylinder(h = thickness + 2, d = {d});\n    translate([{:.3}, {:.3}, 0]) cylinder(h = thickness + 2, d = {d});\n  }}",
            alignment.name,
            -(dx - d) / 2.0,
            -(dy - d) / 2.0,
            (dx - d) / 2.0,
            (dy - d) / 2.0,
        )
        .unwrap();
    }
    // A pogo may stand closer to a clamp than a printable wall between
    // its hole and the counterbore allows: the counterbore takes in the
    // top of that hole.
    for alignment in bed.alignment().iter().filter(|a| a.clamps) {
        let [x, y] = up(alignment.at);
        let counterbore = format!(
            "translate([{x:.3}, {y:.3}, thickness - head_h]) cylinder(h = head_h + 1, d = head_d);"
        );
        writeln!(w, "  {counterbore} // {} clamp", alignment.name).unwrap();
        for &[px, py] in &pogos {
            if (px - x).hypot(py - y) < (head_dia + POGO_HOLE_DIA_MM) / 2.0 + MIN_WEB_MM {
                writeln!(
                    w,
                    "  hull() {{ {counterbore} translate([{px:.3}, {py:.3}, thickness - head_h]) cylinder(h = head_h + 1, d = {POGO_HOLE_DIA_MM:.2}); }}"
                )
                .unwrap();
            }
        }
    }
    if style == Locators::Dowels {
        let length = DOWEL_LENGTHS_MM
            .iter()
            .copied()
            .rfind(|l| l - LOCATOR_HEIGHT_MM <= t - DOWEL_FLOOR_MM);
        let Some(length) = length else {
            bail!("a {t:.2} mm plate is too thin to seat a dowel pin");
        };
        let depth = length - LOCATOR_HEIGHT_MM;
        writeln!(
            w,
            "  // Ø{LOCATOR_DIA_MM} × {length} mm dowel pins, {LOCATOR_HEIGHT_MM} mm proud."
        )
        .unwrap();
        for locator in &located {
            let [x, y] = up(locator.at);
            writeln!(
                w,
                "  translate([{x:.3}, {y:.3}, thickness - {depth:.2}]) cylinder(h = {:.2}, d = {DOWEL_HOLE_DIA_MM}); // {}",
                depth + 1.0,
                locator.side
            )
            .unwrap();
        }
    }
    writeln!(w, "}}").unwrap();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bed::Bed;

    fn panel() -> Panel {
        Panel {
            width: 105.0,
            height: 74.0,
            outline: Vec::new(),
            holes: Vec::new(),
            fids_top: Vec::new(),
            fids_bottom: Vec::new(),
            mouse_bites: vec![[24.0, 20.0]],
        }
    }

    fn plan() -> Plan {
        Plan {
            boards_total: 2,
            tested: vec![0, 1],
            cells: Vec::new(),
            contacts: Vec::new(),
            bindings: Vec::new(),
            dropped: Vec::new(),
        }
    }

    #[test]
    fn locators_register_board_edges_clear_of_tabs() {
        // Two boards with a 5 mm channel between them at x 22–27.
        let boards = [[10.0, 12.0, 22.0, 30.0], [27.0, 12.0, 39.0, 30.0]];
        let located = locators(&panel(), Bed::SwitchyardRev1, &plan(), &boards);
        for side in ["-x", "+x", "-y", "+y"] {
            assert!(located.iter().any(|l| l.side == side), "{side}");
        }
        let reach = LOCATOR_DIA_MM / 2.0 + EDGE_GAP_MM;
        for locator in &located {
            let [x, y] = locator.at;
            // Off every board by exactly the edge gap or more, and off
            // the tab's mouse bites.
            assert!(boards.iter().all(|&[x0, y0, x1, y1]| {
                (x0 - x)
                    .max(x - x1)
                    .max(0.0)
                    .hypot((y0 - y).max(y - y1).max(0.0))
                    >= reach - 1e-9
            }));
            assert!((x - 24.0).hypot(y - 20.0) >= LOCATOR_DIA_MM / 2.0 + TAB_MARGIN_MM);
        }
    }

    #[test]
    fn plate_stops_the_dut_at_working_travel() {
        assert!((thickness() - 5.78).abs() < 1e-9);
        let boards = [[10.0, 12.0, 22.0, 30.0]];
        let nubs = plate(
            &panel(),
            Bed::SwitchyardRev1,
            &plan(),
            &boards,
            Locators::Nubs,
        )
        .unwrap();
        assert!(nubs.contains("thickness = 5.780;"));
        assert!(nubs.contains("d1 = 2, d2 = 1.2"));
        // Printed holes drawn over size; four screw ears off the sides,
        // their webs clamped onto the plate's sides.
        assert!(nubs.contains("d = 3.55"));
        assert!(nubs.contains(
            "ears = [[-1.000, 15.000], [-1.000, 59.000], [106.000, 15.000], [106.000, 59.000]];"
        ));
        assert!(nubs.contains("min(max(e[0], 5.000), 100.000), min(max(e[1], 5.000), 69.000)"));
        // D1, D2 and the M3.
        assert_eq!(nubs.matches("hull() { //").count(), 3);
        // The M3 clamps the plate's middle; D1 and D2 do not.
        assert_eq!(nubs.matches("d = head_d); //").count(), 1);
        assert!(nubs.contains("// M3 clamp"));
        let dowels = plate(
            &panel(),
            Bed::SwitchyardRev1,
            &plan(),
            &boards,
            Locators::Dowels,
        )
        .unwrap();
        assert!(dowels.contains("Ø2 × 6 mm dowel pins"));
        assert!(!dowels.contains("d1 ="));
        assert!(plate(&panel(), Bed::S13, &plan(), &boards, Locators::Nubs).is_err());
        assert_eq!("dowels".parse::<Locators>(), Ok(Locators::Dowels));
    }
}
