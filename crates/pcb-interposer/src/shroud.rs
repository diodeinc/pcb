//! The upper shroud: the stiff, panel-specific print between the
//! interposer and the DUT, for Switchyard's rev-1 bench stack.
//!
//! Bottom to top the stack is the 14 mm backplate, Switchyard (1.602 mm),
//! the 4.28 mm lower shroud, the interposer flat on that stop, this
//! shroud, and the DUT (Switchyard `mechanical/README.md` at f90ab8c).
//! The shroud's top face is the DUT stop, so its thickness is the DUT
//! pogo's free height less its working travel. It carries:
//!
//! - a clearance hole for every DUT pogo, merged into a slot where two
//!   are too close for a printable web;
//! - blind sockets on its underside for the two Ø3 m6 stack dowels: DW1
//!   round, DW2 a slot along X. The dowels stand 10 mm above the
//!   backplate, so their engagement follows from the stack;
//! - the five M3 clamp screws (MH1–MH4, MH7), from the DUT side, their
//!   socket heads sunk in equal-height counterbores below the DUT stop.
//!   The screw is the shortest stock length that reaches its insert from
//!   that seat; the bench's screw holes are clear behind, so a longer
//!   screw goes in further. The corner screws sit 3.2 mm in from the
//!   interposer's edges, so the shroud grows past them ([`footprint`])
//!   to keep a wall around their counterbores;
//! - a pocket through the shroud under every part on the DUT's probed
//!   face, since the file gives no part heights;
//! - the DUT's locators: two Ø2 m6 pins pressed into the shroud at two
//!   of the panel's own tooling holes, reaching down into reliefs drilled
//!   through the interposer; or, for a single board, a nest rim.
//!
//! Every printed opening is drawn [`PRINT_ALLOWANCE_MM`] over its finished
//! size (PETG prints holes small); solids are drawn as finished. The
//! shroud is emitted as OpenSCAD source in its assembled orientation
//! (DUT face up, X along the 105 mm edge, `y_scad = 74 − y` so the part
//! is right-handed). `part = "assembly"` draws the whole bench stack
//! around it instead.
//!
//! The lower shroud is Switchyard's; this module reports which features
//! reach below the interposer and would need a relief in its top face
//! ([`relief_map`]), and the hardware it all takes ([`bom`]).

use std::fmt::Write as _;

use anyhow::{Result, bail};

use crate::bed::{Alignment, Bed};
use crate::panel::Panel;
use crate::parts::{Part, signed_distance};
use crate::pattern::Land;
use crate::plan::Plan;
use crate::pogo;

/// How much a printed opening comes out under its drawn width.
pub const PRINT_ALLOWANCE_MM: f64 = 0.5;
/// Clearance hole around each pogo's land and solder fillet.
const POGO_HOLE_DIA_MM: f64 = pogo::LAND_DIA_MM + 0.5 + PRINT_ALLOWANCE_MM;
/// The thinnest wall that prints between two openings; closer ones merge.
const MIN_WEB_MM: f64 = 0.8;
/// A DUT part's pocket clears its package outline by this much.
const PART_CLEARANCE_MM: f64 = 0.5;

/// The bench stack under the interposer (Switchyard spec, "Board outline
/// and mechanical stack").
pub const SWITCHYARD_MM: f64 = 1.602;
pub const LOWER_STOP_MM: f64 = 4.28;
const LOWER_STOP_TOL_MM: f64 = 0.10;
/// The lower shroud's roof over its component pockets.
const LOWER_ROOF_MM: f64 = 1.28;
/// Solid the lower shroud keeps under any relief cut into its top face.
const LOWER_MIN_ROOF_MM: f64 = 0.30;

/// Stack dowels: Ø3 m6 × 16, 6 mm in the backplate, 10 mm proud of it.
const DOWEL_PROUD_MM: f64 = 10.0;
const SOCKET_DEPTH_MM: f64 = 3.20;
const SOCKET_DEPTH_TOL_MM: f64 = 0.10;
const MIN_ENGAGEMENT_MM: f64 = 2.00;
const MIN_TIP_CLEARANCE_MM: f64 = 0.30;
const MIN_ROOF_MM: f64 = 1.00;
const MIN_SOCKET_THICKNESS_MM: f64 = 4.30;
/// Finished socket openings: DW1 round, DW2 a slot, long axis along X.
const SOCKET_ROUND_MM: f64 = 3.15;
const SOCKET_SLOT_MM: [f64; 2] = [4.05, 3.15];

/// M3 socket-head clamp screws.
const SCREW_HOLE_MM: f64 = 3.40;
const SCREW_HEAD_DIA_MM: f64 = 5.5;
const SCREW_HEAD_H_MM: f64 = 3.0;
/// The counterbore clears the head by this much, finished.
const HEAD_CLEARANCE_MM: f64 = 0.5;
/// The head stays this far below the DUT stop.
const HEAD_HEADROOM_MM: f64 = 0.3;
/// Under-head length = head seat above Switchyard's top + this, for 5 mm
/// in the backplate insert with the tip 1 mm inside the base.
const SCREW_REACH_MM: f64 = 14.6;
const SCREW_ENGAGEMENT_MM: f64 = 5.0;
const SCREW_TIP_INSIDE_BASE_MM: f64 = 1.0;
const INSERT_LENGTH_MM: f64 = 5.7;
/// Stock lengths and McMaster numbers.
const SCREWS: [(f64, &str); 3] = [
    (25.0, "91290A125"),
    (30.0, "91290A130"),
    (35.0, "91290A135"),
];

/// DUT locating pins: Ø2 m6 (+0.002/+0.008).
const PIN_DIA_MM: f64 = 2.0;
const PIN_MAX_MM: f64 = 2.008;
const PINS: [(f64, &str); 2] = [(8.0, "91595A023"), (10.0, "91595A026")];
/// A panel hole takes the pin when its smallest size (−0.05) still slips
/// over the largest pin, and holds it to ±0.15 at its largest.
const HOLE_TOL_MM: f64 = 0.05;
const MIN_SLIP_MM: f64 = 0.01;
const MAX_PLAY_MM: f64 = 0.25;
/// The pin presses into a printed hole drawn this much under its
/// diameter plus the allowance: about 0.1 mm interference, ream to fit.
const PIN_PRESS_MM: f64 = 0.1;
/// The pin's bottom stays this far above the interposer's underside.
const PIN_RECESS_MM: f64 = 0.3;
/// Interposer relief for a pin: drilled, so no allowance.
pub const PIN_RELIEF_MM: f64 = 2.2;
/// At first pogo contact the pin is already this deep in the DUT hole.
const PIN_LEAD_MM: f64 = 1.0;
/// Pins this far apart or more register rotation well enough.
const MIN_PIN_SPAN_MM: f64 = 30.0;

/// Nest rim for a single board.
const NEST_GAP_MM: f64 = 0.15 + PRINT_ALLOWANCE_MM / 2.0;
const NEST_WALL_MM: f64 = 2.0;
const NEST_HEIGHT_MM: f64 = 1.2;
const NEST_MARGIN_MM: f64 = 1.0;
const FINGER_NOTCH_MM: f64 = 15.0;

/// How far past the board's outline the shroud must reach to carry a
/// nest's rim.
pub fn nest_reach() -> f64 {
    NEST_GAP_MM + NEST_WALL_MM + NEST_MARGIN_MM
}

/// The shroud's thickness: the DUT pogo's tip height at working travel
/// (half its stroke, the nominal the Switchyard bed uses for its own).
pub fn thickness() -> f64 {
    pogo::FREE_HEIGHT_MM - travel()
}

fn travel() -> f64 {
    pogo::STROKE_MM / 2.0
}

/// A DUT locating pin at one of the panel's tooling holes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pin {
    pub at: [f64; 2],
    /// The panel hole it enters.
    pub hole_dia: f64,
}

/// How the shroud locates the DUT.
#[derive(Debug, Clone, PartialEq)]
pub enum Locating {
    Pins([Pin; 2]),
    /// A single board's nest rim.
    Nest,
    /// No valid pair: why, for the operator choosing tooling positions.
    Missing(Vec<String>),
}

/// What the stack and the shroud's features come to.
#[derive(Debug, Clone)]
pub struct Stack {
    pub interposer: f64,
    pub shroud: f64,
    /// Stack dowel engagement in its socket and tip clearance, nominal
    /// and at the tolerance extremes (stop ±0.10, socket depth ±0.10).
    pub engagement: [f64; 3],
    pub tip_clearance: [f64; 3],
    pub socket_roof: f64,
    /// Head seat above the interposer's top, and the head's top below
    /// the DUT stop.
    pub seat: f64,
    pub head_below_dut: f64,
    pub screw: (f64, &'static str),
    /// Thread in the insert, and how far the tip passes the base's
    /// underside (negative: still inside).
    pub screw_engagement: f64,
    pub screw_tip_past_base: f64,
    /// Pin length chosen for the DUT, its projection above the DUT stop,
    /// and how far it stands proud of the clamped DUT's far face.
    pub pin: Option<((f64, &'static str), f64, f64)>,
}

/// The stack for a `interposer`-thick board under a `dut`-thick DUT.
pub fn stack(interposer: f64, dut: f64) -> Result<Stack> {
    let shroud = thickness();
    let proud = |stop: f64| DOWEL_PROUD_MM - SWITCHYARD_MM - stop - interposer;
    let engagement = [
        proud(LOWER_STOP_MM),
        proud(LOWER_STOP_MM + LOWER_STOP_TOL_MM),
        proud(LOWER_STOP_MM - LOWER_STOP_TOL_MM),
    ];
    let tip_clearance = [
        SOCKET_DEPTH_MM - engagement[0],
        SOCKET_DEPTH_MM - SOCKET_DEPTH_TOL_MM - engagement[2],
        SOCKET_DEPTH_MM + SOCKET_DEPTH_TOL_MM - engagement[1],
    ];
    let socket_roof = shroud - SOCKET_DEPTH_MM - SOCKET_DEPTH_TOL_MM;
    if shroud < MIN_SOCKET_THICKNESS_MM || socket_roof < MIN_ROOF_MM {
        bail!(
            "a {shroud:.2} mm shroud leaves {socket_roof:.2} mm over the stack-dowel sockets; \
             it needs {MIN_SOCKET_THICKNESS_MM} mm and a {MIN_ROOF_MM} mm roof"
        );
    }
    if engagement[1] < MIN_ENGAGEMENT_MM || tip_clearance[1] < MIN_TIP_CLEARANCE_MM {
        bail!(
            "a {interposer:.3} mm interposer gives the stack dowels {:.3} mm engagement and \
             {:.3} mm tip clearance at worst; they need {MIN_ENGAGEMENT_MM} and \
             {MIN_TIP_CLEARANCE_MM}",
            engagement[1],
            tip_clearance[1]
        );
    }
    let seat = shroud - HEAD_HEADROOM_MM - SCREW_HEAD_H_MM;
    let needed = seat + interposer + LOWER_STOP_MM + SCREW_REACH_MM;
    let Some(screw) = SCREWS
        .iter()
        .copied()
        .find(|(length, _)| *length >= needed - 1e-9)
    else {
        bail!("the clamp screws need {needed:.2} mm under the head; stock stops at 35 mm");
    };
    let extra = screw.0 - needed;
    let pin = {
        // The pin's bottom sits just above the interposer's underside.
        let embedded = shroud + interposer - PIN_RECESS_MM;
        PINS.iter().copied().find_map(|pin| {
            let projection = pin.0 - embedded;
            (projection >= travel() + PIN_LEAD_MM).then_some((pin, projection, projection - dut))
        })
    };
    Ok(Stack {
        interposer,
        shroud,
        engagement,
        tip_clearance,
        socket_roof,
        seat,
        head_below_dut: HEAD_HEADROOM_MM,
        screw,
        screw_engagement: (SCREW_ENGAGEMENT_MM + extra).min(INSERT_LENGTH_MM),
        screw_tip_past_base: extra - SCREW_TIP_INSIDE_BASE_MM,
        pin,
    })
}

fn distance(a: [f64; 2], b: [f64; 2]) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

/// A feature's reach from its center: the slot's half-length along X for
/// DW2, the radius otherwise.
fn socket_reach(feature: &Alignment) -> f64 {
    if feature.drill[0] > feature.drill[1] {
        SOCKET_SLOT_MM[0] / 2.0
    } else {
        SOCKET_ROUND_MM / 2.0
    }
}

fn counterbore_radius() -> f64 {
    (SCREW_HEAD_DIA_MM + HEAD_CLEARANCE_MM) / 2.0
}

/// The wall a screw-head counterbore keeps to the shroud's outside.
const COUNTERBORE_WALL_MM: f64 = 1.2;

/// The shroud's footprint `[x0, y0, x1, y1]`: the interposer's, grown on
/// any side where a screw-head counterbore would break out of it (the
/// bench's corner screws sit 3.2 mm in from the zone's edges), and along
/// the pogo rows to carry a single board's `nest` (its outline) past the
/// interposer's ends. Across the rows the shroud keeps to the bed.
pub fn footprint(panel: &Panel, bed: Bed, nest: Option<[f64; 4]>) -> [f64; 4] {
    let [mut x0, mut y0, mut x1, mut y1] =
        panel
            .envelope
            .unwrap_or([0.0, 0.0, panel.width, panel.height]);
    if let Some([nx0, _, nx1, _]) = nest {
        x0 = x0.min(nx0 - nest_reach());
        x1 = x1.max(nx1 + nest_reach());
    }
    let reach = counterbore_radius() + PRINT_ALLOWANCE_MM / 2.0 + COUNTERBORE_WALL_MM;
    for screw in bed.alignment().iter().filter(|a| a.clamps) {
        let [x, y] = screw.at;
        x0 = x0.min(x - reach);
        y0 = y0.min(y - reach);
        x1 = x1.max(x + reach);
        y1 = y1.max(y + reach);
    }
    [x0, y0, x1, y1]
}

/// Choose the DUT's two locating pins among the panel's tooling holes:
/// holes a Ø2 m6 pin fits, clear of the shroud's edge, the screw heads,
/// the dowel sockets and their keep-outs, the pogos, the DUT's parts and
/// the bed's lands and fiducials, as far apart as possible.
pub fn choose_pins(
    holes: &[([f64; 2], f64)],
    shroud: [f64; 4],
    bed: Bed,
    pogos: &[[f64; 2]],
    parts: &[Part],
    lands: &[Land],
) -> Locating {
    let r = PIN_DIA_MM / 2.0;
    let mut rejected = Vec::new();
    let mut fit: Vec<Pin> = Vec::new();
    if holes.is_empty() {
        return Locating::Missing(vec![
            "the panel file carries no non-plated tooling holes".into(),
        ]);
    }
    for &(at, dia) in holes {
        let why =
            |reason: String| format!("Ø{dia:.2} hole at ({:.2}, {:.2}): {reason}", at[0], at[1]);
        if dia - HOLE_TOL_MM < PIN_MAX_MM + MIN_SLIP_MM {
            rejected.push(why(format!(
                "too small for a Ø2 m6 pin (needs ≥ Ø{:.2})",
                PIN_MAX_MM + MIN_SLIP_MM + HOLE_TOL_MM
            )));
            continue;
        }
        if dia + HOLE_TOL_MM - PIN_DIA_MM > MAX_PLAY_MM {
            rejected.push(why(format!(
                "too loose on a Ø2 pin (needs ≤ Ø{:.2})",
                PIN_DIA_MM + MAX_PLAY_MM - HOLE_TOL_MM
            )));
            continue;
        }
        let press = (PIN_DIA_MM + PRINT_ALLOWANCE_MM - PIN_PRESS_MM) / 2.0;
        let [x0, y0, x1, y1] = shroud;
        let edge = (at[0] - x0).min(x1 - at[0]).min(at[1] - y0).min(y1 - at[1]);
        if edge < press + MIN_WEB_MM {
            rejected.push(why(format!("{edge:.2} mm from the shroud's edge")));
            continue;
        }
        let mut blocked = None;
        for feature in bed.alignment() {
            let gap = distance(at, feature.at);
            let need = if feature.clamps {
                counterbore_radius() + PRINT_ALLOWANCE_MM / 2.0 + press + MIN_WEB_MM
            } else {
                socket_reach(&feature) + PRINT_ALLOWANCE_MM / 2.0 + press + MIN_WEB_MM
            }
            .max(feature.keepout_dia / 2.0 + r);
            if gap < need {
                blocked = Some(format!(
                    "{gap:.2} mm from {} (needs {need:.2})",
                    feature.name
                ));
                break;
            }
        }
        if blocked.is_none()
            && let Some(p) = pogos
                .iter()
                .find(|&&p| distance(p, at) < POGO_HOLE_DIA_MM / 2.0 + press + MIN_WEB_MM)
        {
            blocked = Some(format!(
                "too close to the pogo at ({:.2}, {:.2})",
                p[0], p[1]
            ));
        }
        if blocked.is_none()
            && let Some(part) = parts.iter().find(|part| {
                signed_distance(&part.outline, at)
                    < PART_CLEARANCE_MM + PRINT_ALLOWANCE_MM / 2.0 + press + MIN_WEB_MM
            })
        {
            blocked = Some(format!(
                "too close to DUT part B{}.{}",
                part.board, part.refdes
            ));
        }
        if blocked.is_none()
            && let Some(land) = lands.iter().find(|land| {
                distance(land.xy, at) < PIN_RELIEF_MM / 2.0 + bed.land_dia() / 2.0 + 0.6
            })
        {
            blocked = Some(format!(
                "its interposer relief is too close to bed land {}",
                land.label
            ));
        }
        match blocked {
            Some(reason) => rejected.push(why(reason)),
            None => fit.push(Pin { at, hole_dia: dia }),
        }
    }
    let mut best: Option<(f64, [Pin; 2])> = None;
    for (i, a) in fit.iter().enumerate() {
        for b in &fit[i + 1..] {
            let span = distance(a.at, b.at);
            if best.as_ref().is_none_or(|(s, _)| span > *s) {
                best = Some((span, [*a, *b]));
            }
        }
    }
    match best {
        Some((span, pins)) if span >= MIN_PIN_SPAN_MM => Locating::Pins(pins),
        Some((span, _)) => {
            rejected.push(format!(
                "the two usable holes are only {span:.1} mm apart; rotation needs ≥ {MIN_PIN_SPAN_MM} mm"
            ));
            Locating::Missing(rejected)
        }
        None => {
            if fit.len() == 1 {
                rejected.push(format!(
                    "only the hole at ({:.2}, {:.2}) takes a pin",
                    fit[0].at[0], fit[0].at[1]
                ));
            }
            Locating::Missing(rejected)
        }
    }
}

/// DUT parts that collide with the shroud's fixed features: over a screw
/// head, a dowel socket's roof, or inside a CR-17 Ø6 keep-out.
pub fn part_conflicts(bed: Bed, parts: &[Part]) -> Vec<String> {
    let mut issues = Vec::new();
    for feature in bed.alignment() {
        let reach = if feature.clamps {
            counterbore_radius() + PRINT_ALLOWANCE_MM / 2.0 + MIN_WEB_MM
        } else {
            socket_reach(&feature) + PRINT_ALLOWANCE_MM / 2.0 + MIN_WEB_MM
        }
        .max(feature.keepout_dia / 2.0);
        for part in parts {
            let gap = signed_distance(&part.outline, feature.at);
            if gap < reach + PART_CLEARANCE_MM {
                issues.push(format!(
                    "DUT part B{}.{} reaches within {:.2} mm of {} at ({:.2}, {:.2}); {} needs {:.2} mm",
                    part.board,
                    part.refdes,
                    gap.max(0.0),
                    feature.name,
                    feature.at[0],
                    feature.at[1],
                    if feature.clamps {
                        "its screw head"
                    } else {
                        "its dowel socket's roof"
                    },
                    reach + PART_CLEARANCE_MM
                ));
            }
        }
    }
    issues
}

/// Everything the shroud's source needs.
pub struct Shroud<'a> {
    pub panel: &'a Panel,
    pub bed: Bed,
    pub plan: &'a Plan,
    /// Board outlines in the sheet frame (for a nest).
    pub boards: &'a [[f64; 4]],
    pub parts: &'a [Part],
    pub locating: &'a Locating,
    pub stack: &'a Stack,
    pub dut: f64,
}

impl Shroud<'_> {
    /// The board outline a nest rims, for a single board.
    fn nest(&self) -> Option<[f64; 4]> {
        (*self.locating == Locating::Nest)
            .then(|| self.boards.first().copied())
            .flatten()
    }
}

/// The shroud's OpenSCAD source.
pub fn scad(s: &Shroud) -> Result<String> {
    if s.bed.alignment().is_empty() {
        bail!(
            "the {:?} bed has no stack dowels to seat an upper shroud on",
            s.bed
        );
    }
    let st = s.stack;
    let t = st.shroud;
    let [x0, y0, x1, y1] = footprint(s.panel, s.bed, s.nest());
    let h = s.panel.height;
    let up = |p: [f64; 2]| [p[0], h - p[1]];
    let a = |v: f64| v + PRINT_ALLOWANCE_MM;

    let mut out = String::new();
    let w = &mut out;
    writeln!(w, "// Upper shroud, generated by pcb-interposer.").unwrap();
    writeln!(
        w,
        "// Switchyard rev-1 bench stack. Frame: x along the 105 mm edge from the\n\
         // joint-edge corner, y_scad = {h} - Switchyard y, z up from the interposer's top.\n\
         // Printed openings are drawn {PRINT_ALLOWANCE_MM} mm over finished size.\n\
         // part = \"shroud\" for the print (print it DUT face up), \"assembly\" for the stack."
    )
    .unwrap();
    writeln!(
        w,
        "part = \"shroud\";\n$fn = 48;\nthickness = {t:.3}; // DUT stop: {:.2} mm free height - {:.2} mm travel\n\
         interposer = {:.3};\nlower_stop = {LOWER_STOP_MM};\nswitchyard = {SWITCHYARD_MM};\n\
         socket_depth = {SOCKET_DEPTH_MM};\nseat = {:.3}; // screw-head seats above the interposer's top\n\
         dut = {:.3};\n",
        pogo::FREE_HEIGHT_MM,
        travel(),
        st.interposer,
        st.seat,
        s.dut
    )
    .unwrap();

    // Body.
    writeln!(w, "module body() {{").unwrap();
    let [bx, by] = up([x0, y1]);
    writeln!(
        w,
        "  translate([{bx:.3}, {by:.3}, 0]) cube([{:.3}, {:.3}, thickness]);",
        x1 - x0,
        y1 - y0
    )
    .unwrap();
    if *s.locating == Locating::Nest {
        nest(w, s, up)?;
    }
    writeln!(w, "}}\n").unwrap();

    // Cuts.
    writeln!(w, "module cuts() {{").unwrap();
    let pogos: Vec<[f64; 2]> = s
        .plan
        .bindings
        .iter()
        .map(|binding| up(s.plan.contacts[binding.contact].xy))
        .collect();
    let hole = |[x, y]: [f64; 2]| {
        format!(
            "translate([{x:.3}, {y:.3}, -1]) cylinder(h = thickness + 2, d = {POGO_HOLE_DIA_MM:.2});"
        )
    };
    let mut merged = vec![false; pogos.len()];
    writeln!(w, "  // DUT pogo clearance holes.").unwrap();
    for (i, &p) in pogos.iter().enumerate() {
        for (j, &q) in pogos.iter().enumerate().skip(i + 1) {
            if distance(p, q) < POGO_HOLE_DIA_MM + MIN_WEB_MM {
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
    writeln!(
        w,
        "  // Stack-dowel sockets, blind from the underside, {SOCKET_DEPTH_MM} mm deep."
    )
    .unwrap();
    let head_d = a(SCREW_HEAD_DIA_MM + HEAD_CLEARANCE_MM);
    for feature in s.bed.alignment() {
        let [x, y] = up(feature.at);
        if feature.clamps {
            writeln!(
                w,
                "  translate([{x:.3}, {y:.3}, -1]) cylinder(h = thickness + 2, d = {:.2}); // {} M3 clearance\n  \
                 translate([{x:.3}, {y:.3}, seat]) cylinder(h = thickness, d = {head_d:.2}); // {} head",
                a(SCREW_HOLE_MM),
                feature.name,
                feature.name
            )
            .unwrap();
            // A pogo closer than a printable wall: the counterbore takes
            // in the top of its hole.
            for &p in &pogos {
                if distance(p, [x, y]) < (head_d + POGO_HOLE_DIA_MM) / 2.0 + MIN_WEB_MM {
                    writeln!(
                        w,
                        "  hull() {{ translate([{x:.3}, {y:.3}, seat]) cylinder(h = thickness, d = {head_d:.2}); \
                         translate([{:.3}, {:.3}, seat]) cylinder(h = thickness, d = {POGO_HOLE_DIA_MM:.2}); }}",
                        p[0], p[1]
                    )
                    .unwrap();
                }
            }
        } else if feature.drill[0] > feature.drill[1] {
            let [l, wd] = [a(SOCKET_SLOT_MM[0]), a(SOCKET_SLOT_MM[1])];
            writeln!(
                w,
                "  translate([{x:.3}, {y:.3}, -1]) hull() {{ // {} slot, {:.2} x {:.2} along X\n    \
                 translate([{:.3}, 0, 0]) cylinder(h = socket_depth + 1, d = {wd:.2});\n    \
                 translate([{:.3}, 0, 0]) cylinder(h = socket_depth + 1, d = {wd:.2});\n  }}",
                feature.name,
                l,
                wd,
                -(l - wd) / 2.0,
                (l - wd) / 2.0
            )
            .unwrap();
        } else {
            writeln!(
                w,
                "  translate([{x:.3}, {y:.3}, -1]) cylinder(h = socket_depth + 1, d = {:.2}); // {} round",
                a(SOCKET_ROUND_MM),
                feature.name
            )
            .unwrap();
        }
    }
    if !s.parts.is_empty() {
        writeln!(
            w,
            "  // Pockets through the shroud under the DUT's probed-face parts (no heights in the file)."
        )
        .unwrap();
        for part in s.parts {
            let points: Vec<String> = part
                .outline
                .iter()
                .map(|&p| {
                    let [x, y] = up(p);
                    format!("[{x:.3}, {y:.3}]")
                })
                .collect();
            writeln!(
                w,
                "  translate([0, 0, -1]) linear_extrude(thickness + 2) offset(r = {:.2}) polygon([{}]); // B{}.{}",
                PART_CLEARANCE_MM + PRINT_ALLOWANCE_MM / 2.0,
                points.join(", "),
                part.board,
                part.refdes
            )
            .unwrap();
        }
    }
    if let Locating::Pins(pins) = s.locating {
        writeln!(
            w,
            "  // DUT locating pins, pressed through: drawn {PIN_PRESS_MM} mm under Ø2 + allowance; ream to fit."
        )
        .unwrap();
        for pin in pins {
            let [x, y] = up(pin.at);
            writeln!(
                w,
                "  translate([{x:.3}, {y:.3}, -1]) cylinder(h = thickness + 2, d = {:.2});",
                PIN_DIA_MM + PRINT_ALLOWANCE_MM - PIN_PRESS_MM
            )
            .unwrap();
        }
    }
    writeln!(w, "}}\n").unwrap();

    writeln!(w, "module shroud() difference() {{ body(); cuts(); }}\n").unwrap();
    assembly(w, s, up);
    writeln!(w, "if (part == \"assembly\") assembly(); else shroud();").unwrap();
    Ok(out)
}

fn nest(w: &mut String, s: &Shroud, up: impl Fn([f64; 2]) -> [f64; 2]) -> Result<()> {
    let Some(&[nx0, ny0, nx1, ny1]) = s.boards.first() else {
        bail!("a nest needs the board's outline");
    };
    let [x0, y0, x1, y1] = footprint(s.panel, s.bed, s.nest());
    let [a, b] = up([x0, y1]);
    let g = NEST_GAP_MM;
    let wall = NEST_WALL_MM;
    let h = NEST_HEIGHT_MM;
    let [ix, iy] = up([nx0 - g, ny1 + g]);
    let (iw, ih) = (nx1 - nx0 + 2.0 * g, ny1 - ny0 + 2.0 * g);
    let notch = if iw >= ih {
        format!(
            "translate([{:.3}, {:.3}, thickness - 1]) cube([{FINGER_NOTCH_MM}, {:.3}, {:.3}]);",
            ix + iw / 2.0 - FINGER_NOTCH_MM / 2.0,
            iy - wall - 1.0,
            ih + 2.0 * wall + 2.0,
            h + 2.0
        )
    } else {
        format!(
            "translate([{:.3}, {:.3}, thickness - 1]) cube([{:.3}, {FINGER_NOTCH_MM}, {:.3}]);",
            ix - wall - 1.0,
            iy + ih / 2.0 - FINGER_NOTCH_MM / 2.0,
            iw + 2.0 * wall + 2.0,
            h + 2.0
        )
    };
    writeln!(
        w,
        "  // Nest: a rim around the board, notched for fingers on its long sides.\n  \
         intersection() {{\n    \
         translate([{a:.3}, {b:.3}, thickness]) cube([{:.3}, {:.3}, {h}]);\n    \
         difference() {{\n      \
         translate([{:.3}, {:.3}, thickness]) cube([{:.3}, {:.3}, {h}]);\n      \
         translate([{ix:.3}, {iy:.3}, thickness - 1]) cube([{iw:.3}, {ih:.3}, {:.3}]);\n      \
         {notch}\n    \
         }}\n  \
         }}",
        x1 - x0,
        y1 - y0,
        ix - wall,
        iy - wall,
        iw + 2.0 * wall,
        ih + 2.0 * wall,
        h + 2.0,
    )
    .unwrap();
    Ok(())
}

/// The bench stack around the shroud, for the assembled view.
fn assembly(w: &mut String, s: &Shroud, up: impl Fn([f64; 2]) -> [f64; 2]) {
    let st = s.stack;
    let [x0, y0, x1, y1] = s
        .panel
        .envelope
        .unwrap_or([0.0, 0.0, s.panel.width, s.panel.height]);
    let [bx, by] = up([x0, y1]);
    let (ww, hh) = (x1 - x0, y1 - y0);
    // Switchyard's bench zone and the backplate under it.
    let [zx, zy] = up([0.0, 74.0]);
    let base = -(st.interposer + LOWER_STOP_MM + SWITCHYARD_MM);
    writeln!(w, "module assembly() {{").unwrap();
    writeln!(
        w,
        "  color(\"darkslategray\") translate([{zx:.3}, {zy:.3}, {:.3}]) cube([105, 74, 14]);\n  \
         color(\"darkgreen\") translate([{zx:.3}, {zy:.3}, {base:.3}]) cube([105, 74, switchyard]);\n  \
         color(\"orange\", 0.5) translate([{zx:.3}, {zy:.3}, -interposer - lower_stop]) cube([105, 74, lower_stop]);\n  \
         color(\"seagreen\") translate([{bx:.3}, {by:.3}, -interposer]) cube([{ww:.3}, {hh:.3}, interposer]);\n  \
         color(\"gold\") shroud();",
        base - 14.0
    )
    .unwrap();
    for feature in s.bed.alignment() {
        let [x, y] = up(feature.at);
        if feature.clamps {
            let tip = st.seat - st.screw.0;
            writeln!(
                w,
                "  color(\"silver\") {{ translate([{x:.3}, {y:.3}, seat]) cylinder(h = {SCREW_HEAD_H_MM}, d = {SCREW_HEAD_DIA_MM}); \
                 translate([{x:.3}, {y:.3}, {tip:.3}]) cylinder(h = {:.3}, d = 3); }} // {} M3 x {}",
                st.screw.0, feature.name, st.screw.0
            )
            .unwrap();
        } else {
            writeln!(
                w,
                "  color(\"lightsteelblue\") translate([{x:.3}, {y:.3}, {:.3}]) cylinder(h = 16, d = 3); // {} stack dowel",
                base - 6.0,
                feature.name
            )
            .unwrap();
        }
    }
    if let (Locating::Pins(pins), Some(((length, _), projection, _))) = (s.locating, st.pin) {
        for pin in pins {
            let [x, y] = up(pin.at);
            writeln!(
                w,
                "  color(\"red\") translate([{x:.3}, {y:.3}, thickness + {projection:.3} - {length}]) cylinder(h = {length}, d = 2);"
            )
            .unwrap();
        }
    }
    for &[px0, py0, px1, py1] in s.boards {
        let [ux, uy] = up([px0, py1]);
        writeln!(
            w,
            "  color(\"royalblue\", 0.35) translate([{ux:.3}, {uy:.3}, thickness]) cube([{:.3}, {:.3}, dut]);",
            px1 - px0,
            py1 - py0
        )
        .unwrap();
    }
    writeln!(w, "}}\n").unwrap();
}

/// The lower-shroud relief map, as JSON: every feature that reaches below
/// the interposer's underside (the 4.28 mm stop plane), with the relief
/// it needs in the lower shroud's top face, checked against both lower
/// shrouds.
pub fn relief_map(stack: &Stack, locating: &Locating) -> String {
    let mut entries = Vec::new();
    if let (Locating::Pins(pins), Some(((length, _), projection, _))) = (locating, stack.pin) {
        for pin in pins {
            // The pin's bottom relative to the stop plane: negative is
            // above it, inside the interposer's relief.
            let bottom = stack.shroud + projection - length + stack.interposer;
            let protrusion = -bottom;
            let depth = if protrusion > 0.0 {
                protrusion + 0.2
            } else {
                0.0
            };
            let verdict = |roof: f64| {
                if depth == 0.0 {
                    "no relief needed".to_string()
                } else if depth <= roof - LOWER_MIN_ROOF_MM {
                    format!("{depth:.2} mm pocket fits under the {roof} mm roof")
                } else {
                    format!(
                        "{depth:.2} mm is deeper than the {roof} mm roof allows; needs an opening"
                    )
                }
            };
            entries.push(serde_json::json!({
                "feature": "dut_locating_pin",
                "at": [round(pin.at[0]), round(pin.at[1])],
                "clearance_dia_mm": PIN_DIA_MM + PRINT_ALLOWANCE_MM,
                "protrusion_mm": round(protrusion),
                "relief_depth_mm": round(depth),
                "closed_roof": verdict(LOWER_ROOF_MM),
                "open_roof_27": verdict(LOWER_ROOF_MM),
            }));
        }
    }
    let doc = serde_json::json!({
        "frame": "Switchyard top view, mm: origin at the joint-edge corner, x along the 105 mm edge, y toward the station connectors",
        "stop_plane": "the interposer's underside, seated on the 4.28 mm lower shroud",
        "dut_pogo_tails": {
            "part": "Mill-Max 806-22-001-30-012191",
            "mount": "SMD",
            "count": 0,
            "note": "surface-mount DUT pogos have no tail below the interposer",
        },
        "features": entries,
        "reliefs_required": entries.iter().filter(|e| e["relief_depth_mm"].as_f64() > Some(0.0)).count(),
    });
    serde_json::to_string_pretty(&doc).expect("relief map serializes")
}

fn round(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

/// The fixture's hardware and the stack's numbers, as Markdown.
pub fn bom(stack: &Stack, locating: &Locating, pogos: usize, issues: &[String]) -> String {
    let st = stack;
    let mut w = String::new();
    writeln!(w, "# Fixture BOM — Switchyard rev-1 bench stack\n").unwrap();
    writeln!(w, "| Item | Qty | Part | Notes |\n|---|---|---|---|").unwrap();
    writeln!(
        w,
        "| Interposer PCB, {:.2} mm | 1 | generated | Gerbers from the interposer IPC-2581 |",
        st.interposer
    )
    .unwrap();
    writeln!(
        w,
        "| DUT pogo, Mill-Max 806-22-001-30-012191 | {pogos} | Mill-Max | SMD, on the interposer's top face |"
    )
    .unwrap();
    writeln!(
        w,
        "| Upper shroud, PETG, {:.2} mm | 1 | printed | solid, DUT face up |",
        st.shroud
    )
    .unwrap();
    writeln!(
        w,
        "| Stack dowel, Ø3 m6 × 16 mm | 2 | [McMaster 91595A116](https://www.mcmaster.com/91595A116/) | 6 mm in the backplate, {:.3} mm into the shroud's sockets |",
        st.engagement[0]
    )
    .unwrap();
    writeln!(
        w,
        "| Clamp screw, M3 × 0.5 × {} mm socket head | 5 | [McMaster {}](https://www.mcmaster.com/{}/) | MH1–MH4, MH7; head seats {:.2} mm above the interposer |",
        st.screw.0, st.screw.1, st.screw.1, st.seat
    )
    .unwrap();
    writeln!(
        w,
        "| Heat-set insert, M3 × 0.5 × 5.7 mm | 5 | [McMaster 94459A140](https://www.mcmaster.com/product/94459A140/) | in the backplate |"
    )
    .unwrap();
    match (locating, st.pin) {
        (Locating::Pins(_), Some(((length, part), projection, above))) => writeln!(
            w,
            "| DUT locating pin, Ø2 m6 × {length} mm | 2 | [McMaster {part}](https://www.mcmaster.com/{part}/) | {projection:.2} mm above the DUT stop, {above:.2} mm through the clamped DUT |"
        )
        .unwrap(),
        (Locating::Nest, _) => writeln!(w, "| DUT nest rim | — | printed with the shroud | single board |").unwrap(),
        _ => writeln!(w, "| DUT locating pins | 2 | not chosen | see below |").unwrap(),
    }

    writeln!(w, "\n## Stack\n").unwrap();
    writeln!(
        w,
        "- Interposer {:.3} mm on the {LOWER_STOP_MM} mm stop over Switchyard ({SWITCHYARD_MM} mm).",
        st.interposer
    )
    .unwrap();
    writeln!(
        w,
        "- Stack dowels: engagement {:.3} mm nominal ({:.3}–{:.3} with the stop at ±{LOWER_STOP_TOL_MM}); tip clearance {:.3} mm nominal, {:.3} mm worst (socket {SOCKET_DEPTH_MM} ± {SOCKET_DEPTH_TOL_MM} mm). Needs ≥ {MIN_ENGAGEMENT_MM} and ≥ {MIN_TIP_CLEARANCE_MM}.",
        st.engagement[0], st.engagement[1], st.engagement[2], st.tip_clearance[0], st.tip_clearance[1]
    )
    .unwrap();
    writeln!(
        w,
        "- Socket roof: {:.3} mm minimum (shroud {:.3} mm, socket {:.2} mm deep at most). Needs ≥ {MIN_ROOF_MM}.",
        st.socket_roof,
        st.shroud,
        SOCKET_DEPTH_MM + SOCKET_DEPTH_TOL_MM
    )
    .unwrap();
    writeln!(
        w,
        "- Clamp screws: M3 × {} at all five, heads seated {:.2} mm above the interposer, {:.2} mm below the DUT stop. Thread in the insert {:.2} mm; the tip ends {}.",
        st.screw.0,
        st.seat,
        st.head_below_dut,
        st.screw_engagement,
        if st.screw_tip_past_base > 0.0 {
            format!("{:.2} mm past the backplate's underside (the holes are clear behind)", st.screw_tip_past_base)
        } else {
            format!("{:.2} mm inside the backplate", -st.screw_tip_past_base)
        }
    )
    .unwrap();
    match (locating, st.pin) {
        (Locating::Pins(pins), Some(((length, _), projection, above))) => {
            writeln!(
                w,
                "- DUT pins: Ø2 m6 × {length} mm, pressed through the shroud until {:.2} mm stands below its underside, in Ø{PIN_RELIEF_MM} interposer reliefs; {projection:.2} mm above the DUT stop, {:.2} mm into the DUT at first pogo contact, {above:.2} mm through the clamped DUT.",
                st.interposer - PIN_RECESS_MM,
                projection - travel(),
            )
            .unwrap();
            for pin in pins {
                writeln!(
                    w,
                    "  - ({:.3}, {:.3}) in the panel's Ø{:.2} tooling hole",
                    pin.at[0], pin.at[1], pin.hole_dia
                )
                .unwrap();
            }
        }
        (Locating::Nest, _) => writeln!(w, "- DUT: single board in a printed nest rim.").unwrap(),
        (Locating::Missing(_), _) | (Locating::Pins(_), None) => {
            writeln!(w, "- DUT pins: no valid pair; see below.").unwrap()
        }
    }
    if !issues.is_empty() {
        writeln!(w, "\n## Needs attention\n").unwrap();
        for issue in issues {
            writeln!(w, "- {issue}").unwrap();
        }
    }
    writeln!(
        w,
        "\n## Physical fit to check on the print\n\n\
         - Finished socket depth ({SOCKET_DEPTH_MM} ± {SOCKET_DEPTH_TOL_MM}) and the {LOWER_STOP_MM} mm stop; ream the sockets and pin holes to fit.\n\
         - DUT part heights: the file gives none, so every probed-face part has a pocket through the shroud; a part taller than {:.2} mm would reach the interposer.\n\
         - The shroud seats flat on the interposer before either stack dowel bottoms out.\n\
         - Screw heads clear the DUT through the full stroke, and the screw tips clear whatever is under the backplate.",
        st.shroud
    )
    .unwrap();
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stack_matches_the_bench_contract() {
        let st = stack(1.60, 1.6).unwrap();
        assert!((st.shroud - 5.78).abs() < 1e-9);
        // README: 2.518 mm engagement and 0.682 mm tip clearance with a
        // 1.60 mm interposer.
        assert!((st.engagement[0] - 2.518).abs() < 1e-9);
        assert!((st.tip_clearance[0] - 0.682).abs() < 1e-9);
        assert!(st.socket_roof >= MIN_ROOF_MM);
        // Heads sunk 0.3 below the DUT stop need 22.96 mm; 25 mm is the
        // shortest stock and goes 2.04 mm further.
        assert_eq!(st.screw.0, 25.0);
        assert!((st.seat - 2.48).abs() < 1e-9);
        assert!((st.screw_tip_past_base - 1.04).abs() < 1e-9);
        assert!((st.screw_engagement - INSERT_LENGTH_MM).abs() < 1e-9);
        // An 8 mm pin cannot reach the DUT before the pogos; 10 mm does.
        let ((length, part), projection, _) = st.pin.unwrap();
        assert_eq!((length, part), (10.0, "91595A026"));
        assert!((projection - 2.92).abs() < 1e-9);
    }

    #[test]
    fn pins_take_holes_that_fit_far_apart() {
        let holes = [
            ([20.0, 40.0], 2.1),
            ([90.0, 40.0], 2.1),
            ([60.0, 40.0], 2.0),
            ([3.0, 3.0], 2.1),
        ];
        let locating = choose_pins(
            &holes,
            [0.0, 0.0, 105.0, 74.0],
            Bed::SwitchyardRev1,
            &[],
            &[],
            &[],
        );
        let Locating::Pins([a, b]) = locating else {
            panic!("{locating:?}");
        };
        assert_eq!([a.at, b.at], [[20.0, 40.0], [90.0, 40.0]]);
        let Locating::Missing(why) = choose_pins(
            &holes[2..],
            [0.0, 0.0, 105.0, 74.0],
            Bed::SwitchyardRev1,
            &[],
            &[],
            &[],
        ) else {
            panic!();
        };
        assert!(why[0].contains("too small"), "{why:?}");
        assert!(why[1].contains("MH1"), "{why:?}");
    }
}
