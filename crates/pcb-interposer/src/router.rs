//! R5, terminal-via edition.
//!
//! Physical model: pogo receptacles are SMT pads on the **top** copper; mate
//! lands are SMT pads on the **bottom**. Copper changes layers exactly once
//! per net, through an optional via placed **in one of the net's own pads**
//! (via-in-pad at the mate land for a top run, or in the pogo pad for a
//! bottom run). There are no free mid-route vias, so every net is a
//! single-layer path and the router's job collapses to: pick the layer,
//! route in 2-D, keep it legal, make it pretty.
//!
//! - real net classes (LS 0.25 mm, power 0.9 mm for 2 A, USB pair ribbon)
//! - octilinear A* with direction state and turn penalties (no z, no vias)
//! - USB pairs routed as a single centerline "ribbon" then offset into two
//!   parallel rails (structural length match, no polarity twist)
//! - gridless post-route smoothing: line-of-sight shortcutting against an
//!   exact capsule world, so traces become a few long straight segments
//! - self-DRC over the final geometry so the score is honest
//!
//! GND is poured on the bottom (each GND pogo takes a via-in-pad into the
//! pour); routing everything else top-first leaves that pour nearly solid —
//! a proper return plane under the USB ribbons.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

use crate::geom::{self, Obstacle, P, World};
use crate::route::{RouteResult, Trace, TwoPinNet, kind_pri};
use crate::types::{Assign, BoardId, ContactId, Kind, MatePinId, Problem, dist};

const PITCH: f64 = 0.2;
const PAD_R: f64 = 0.5;
/// A fiducial blocks copper out to its Ø2 mask opening.
const FID_KEEPOUT_R: f64 = 1.0;
const EDGE_MARGIN: f64 = 0.5;
/// Trace width / gap of one side of a USB pair.
pub const USB_W: f64 = 0.2;
pub const USB_GAP: f64 = 0.15;
/// 2 A at 1 oz outer copper, ~10 °C rise (IPC-2221) is ≈0.8 mm; 0.9 adds margin.
pub const POWER_W: f64 = 0.9;
pub const LS_W: f64 = 0.25;

const BOT: u8 = 0;
const TOP: u8 = 1;

const STEP: i32 = 10;
const STEP_DIAG: i32 = 14;
const TURN_45: i32 = 10;
const TURN_90: i32 = 30;
const EXPAND_CAP: u32 = 2_000_000;
/// Extra stamp radius: grid segments run between cell centers, so a chord can
/// sag up to ~half a diagonal below the sampled circle.
const GRID_SLOP: f64 = 0.04;
/// Length of the reserved entry corridor in front of each USB kit.
const GATEWAY_LEN: f64 = 3.0;
/// Soft-mode surcharge for entering a cell occupied by committed copper —
/// used only to identify which nets to shove aside.
const SOFT_CELL: i32 = 400;

/// True when a contact–land pairing can host its terminal via somewhere: in
/// the land (mode T) or in the pogo pad (mode B). A via barrel exists on
/// both layers, so it must keep clearance from every foreign pad regardless
/// of side. The matcher uses this so interchangeable kinds never bind a
/// via-locked pairing.
pub fn via_feasible(problem: &Problem, kind: Kind, c_xy: P, p_xy: P) -> bool {
    let need = Class::of(kind).via_pad_r() + Class::of(kind).clear() + PAD_R;
    let ok_at = |xy: P| {
        problem
            .contacts
            .values()
            .all(|c| dist(c.xy, c_xy) < 1e-6 || dist(c.xy, xy) >= need)
            && problem
                .pins
                .values()
                .all(|p| dist(p.xy, p_xy) < 1e-6 || dist(p.xy, xy) >= need)
    };
    ok_at(p_xy) || ok_at(c_xy)
}

/// Both rails of a pair leave their pads straight along the gateway normal
/// by this much before converging (KiCad's "fan" distance).
const PAIR_FAN: f64 = 0.9;
/// The pair trunk leaves/enters its gateway anchor with a fixed straight
/// lead along the snapped normal, so rail offsetting near the junction is
/// a clean parallel translate instead of a cramped first grid step.
const PAIR_LEAD: f64 = 1.2;
/// Rail center-to-center spacing of the coupled pair.
const RAIL_GAP: f64 = USB_W + USB_GAP;

/// KiCad-style two-stage pair entry: each rail runs `[pad, knee, anchor]` —
/// straight out along `n` by the fan distance, then one slanted segment that
/// converges the separation from the pad pitch down to RAIL_GAP. The
/// waypoint (trunk endpoint) sits between the anchors; every corner is
/// obtuse by construction.
fn pair_entry(pads: [P; 2], n: P) -> (P, [[P; 3]; 2]) {
    let m = mid(pads[0], pads[1]);
    let pad_dist = dist(pads[0], pads[1]);
    let delta = ((pad_dist - RAIL_GAP) / 2.0).max(0.0);
    let standoff = PAIR_FAN + delta.max(0.6);
    let w = add_scaled(m, n, standoff);
    let stubs = [0usize, 1].map(|k| {
        let u = unit(geom::sub(pads[k], m));
        let anchor = add_scaled(w, u, RAIL_GAP / 2.0);
        [pads[k], add_scaled(pads[k], n, PAIR_FAN), anchor]
    });
    (w, stubs)
}

/// A pair whose run is shorter than its own entry geometry routes as two
/// plain traces: the gateways would interlock and force the trunk across
/// its own entry stubs, and coupling over a couple of millimetres is
/// electrically irrelevant.
fn pair_direct(r: &Routable) -> bool {
    let sd = |a: P, b: P| PAIR_FAN + ((dist(a, b) - RAIL_GAP) / 2.0).max(0.6);
    dist(r.src, r.dst)
        < sd(r.members[0].2, r.members[1].2)
            + sd(r.members[0].3, r.members[1].3)
            + 2.0 * PAIR_LEAD
            + 1.0
}

/// Gateway of a USB pad pair: (midpoint, outward unit normal). Outward is
/// the side of the pair axis facing away from the mate's interior; the
/// mate center follows the sheet's folded orientation.
fn gateway(a: P, b: P, mate_center: P) -> (P, P) {
    let m = mid(a, b);
    let n = perp(unit(geom::sub(a, m)));
    let outward = geom::sub(m, mate_center);
    if n[0] * outward[0] + n[1] * outward[1] >= 0.0 {
        (m, n)
    } else {
        (m, [-n[0], -n[1]])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Ls,
    Power,
    Pair,
}

impl Class {
    fn of(kind: Kind) -> Class {
        match kind {
            Kind::UsbHs => Class::Pair,
            Kind::Vtarget | Kind::Vusb => Class::Power,
            _ => Class::Ls,
        }
    }

    fn half(self) -> f64 {
        match self {
            Class::Ls => LS_W / 2.0,
            Class::Power => POWER_W / 2.0,
            Class::Pair => USB_W + USB_GAP / 2.0, // ribbon envelope half-width
        }
    }

    /// One clearance for everything: asymmetric per-class clearances make
    /// net-to-net spacing depend on routing order, which is a bug factory.
    fn clear(self) -> f64 {
        0.25
    }

    /// Terminal via pad radius. Power gets a bigger barrel for the 2 A.
    fn via_pad_r(self) -> f64 {
        match self {
            Class::Power => 0.4,
            _ => 0.3,
        }
    }

    fn idx(self) -> usize {
        match self {
            Class::Ls => 0,
            Class::Power => 1,
            Class::Pair => 2,
        }
    }

    fn width(self) -> f64 {
        match self {
            Class::Pair => USB_W,
            _ => 2.0 * self.half(),
        }
    }
}

const CLASSES: [Class; 3] = [Class::Ls, Class::Power, Class::Pair];

/// One routable: a single net, or a USB pair collapsed to its centerline.
#[derive(Debug, Clone)]
struct Routable {
    id: u32,
    kind: Kind,
    board: BoardId,
    /// (contact, mate pin, pogo xy, land xy) per member; pairs have two in
    /// (dp, dm) order.
    members: Vec<(ContactId, MatePinId, P, P)>,
    /// Centerline endpoints the A* connects.
    src: P,
    dst: P,
    class: Class,
}

// ---------------------------------------------------------------------------
// Grid
// ---------------------------------------------------------------------------

struct Maps {
    nx: i32,
    ny: i32,
    /// Static blockage (pads, holes, sheet edge): exemptable near own pads.
    stat: Vec<[Vec<bool>; 2]>,
    /// Dynamic blockage (committed routes and vias): never exempt.
    /// Counted, so a net can be ripped up (-1) and rerouted for quality.
    dynamic: Vec<[Vec<u16>; 2]>,
}

impl Maps {
    fn new(w: f64, h: f64) -> Self {
        let nx = (w / PITCH).ceil() as i32 + 2;
        let ny = (h / PITCH).ceil() as i32 + 2;
        let n = (nx * ny) as usize;
        Maps {
            nx,
            ny,
            stat: CLASSES
                .iter()
                .map(|_| [vec![false; n], vec![false; n]])
                .collect(),
            dynamic: CLASSES
                .iter()
                .map(|_| [vec![0u16; n], vec![0u16; n]])
                .collect(),
        }
    }

    fn cell(p: P) -> (i32, i32) {
        ((p[0] / PITCH).round() as i32, (p[1] / PITCH).round() as i32)
    }

    fn idx(&self, x: i32, y: i32) -> Option<usize> {
        if x < 0 || y < 0 || x >= self.nx || y >= self.ny {
            None
        } else {
            Some((y * self.nx + x) as usize)
        }
    }

    /// Stamp a static disk obstacle of copper radius `r` for every class map.
    fn stamp_disk_all(&mut self, c: P, r: f64, layers: u8) {
        for class in CLASSES {
            let rr = r + class.clear() + class.half() + GRID_SLOP;
            self.stamp_disk(class, c, rr, layers);
        }
    }

    fn stamp_disk(&mut self, class: Class, c: P, r_mm: f64, layers: u8) {
        Self::disk_cells(self.nx, self.ny, c, r_mm, |i| {
            for z in 0..2u8 {
                if layers & (1 << z) != 0 {
                    self.stat[class.idx()][z as usize][i] = true;
                }
            }
        });
    }

    /// Distances are measured from the true center, not the rounded cell,
    /// so protection is not eroded by up to half a diagonal.
    fn disk_cells(nx: i32, ny: i32, c: P, r_mm: f64, mut f: impl FnMut(usize)) {
        let (cx, cy) = Self::cell(c);
        let rad = (r_mm / PITCH).ceil() as i32 + 1;
        let r2 = r_mm * r_mm;
        for dy in -rad..=rad {
            for dx in -rad..=rad {
                let px = (cx + dx) as f64 * PITCH - c[0];
                let py = (cy + dy) as f64 * PITCH - c[1];
                if px * px + py * py > r2 {
                    continue;
                }
                let (x, y) = (cx + dx, cy + dy);
                if x >= 0 && y >= 0 && x < nx && y < ny {
                    f((y * nx + x) as usize);
                }
            }
        }
    }

    fn bump(map: &mut [u16], i: usize, delta: i32) {
        map[i] = (map[i] as i32 + delta).max(0) as u16;
    }

    /// Commit (+1) or rip up (-1) a dynamic disk (a via barrel).
    fn bump_disk_all(&mut self, c: P, r: f64, layers: u8, delta: i32) {
        let (nx, ny) = (self.nx, self.ny);
        for class in CLASSES {
            let rr = r + class.clear() + class.half() + GRID_SLOP;
            let dynamic = &mut self.dynamic[class.idx()];
            Self::disk_cells(nx, ny, c, rr, |i| {
                for (z, layer_map) in dynamic.iter_mut().enumerate() {
                    if layers & (1 << z) != 0 {
                        Self::bump(layer_map, i, delta);
                    }
                }
            });
        }
    }

    /// Commit (+1) or rip up (-1) a routed centerline on `layer`.
    fn bump_route(&mut self, class_a: Class, layer: u8, cells: &[(i32, i32)], delta: i32) {
        let (nx, ny) = (self.nx, self.ny);
        for probe in CLASSES {
            let r = class_a.half() + class_a.clear().max(probe.clear()) + probe.half() + GRID_SLOP;
            let rad = (r / PITCH).ceil() as i32;
            let r2 = (r / PITCH) * (r / PITCH);
            let dynamic = &mut self.dynamic[probe.idx()][layer as usize];
            for &(x, y) in cells {
                for dy in -rad..=rad {
                    for dx in -rad..=rad {
                        if (dx * dx + dy * dy) as f64 > r2 {
                            continue;
                        }
                        let (px, py) = (x + dx, y + dy);
                        if px >= 0 && py >= 0 && px < nx && py < ny {
                            Self::bump(dynamic, (py * nx + px) as usize, delta);
                        }
                    }
                }
            }
        }
    }

    /// Commit (+1) or rip up (-1) an off-grid segment (a pair stub).
    fn bump_seg(&mut self, class_a: Class, a: P, b: P, layers: u8, delta: i32) {
        let steps = (geom::dist(a, b) / (PITCH * 0.5)).ceil().max(1.0) as usize;
        let (nx, ny) = (self.nx, self.ny);
        for probe in CLASSES {
            let r = class_a.half() + class_a.clear().max(probe.clear()) + probe.half() + GRID_SLOP;
            let dynamic = &mut self.dynamic[probe.idx()];
            for i in 0..=steps {
                let t = i as f64 / steps as f64;
                let p = [a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1])];
                Self::disk_cells(nx, ny, p, r, |i| {
                    for (z, layer_map) in dynamic.iter_mut().enumerate() {
                        if layers & (1 << z) != 0 {
                            Self::bump(layer_map, i, delta);
                        }
                    }
                });
            }
        }
    }

    /// Stamp a static off-grid segment (a gateway reservation).
    fn stamp_seg(&mut self, class_a: Class, a: P, b: P, layers: u8) {
        let steps = (geom::dist(a, b) / (PITCH * 0.5)).ceil().max(1.0) as usize;
        for probe in CLASSES {
            let r = class_a.half() + class_a.clear().max(probe.clear()) + probe.half() + GRID_SLOP;
            for i in 0..=steps {
                let t = i as f64 / steps as f64;
                let p = [a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1])];
                self.stamp_disk(probe, p, r, layers);
            }
        }
    }

    /// True when the off-grid segment `ab` on `layer` is passable for `class`
    /// under the given exemptions. Used to validate fixed pair tails.
    fn seg_free(&self, class: Class, layer: u8, a: P, b: P, exempt: &Exempt) -> bool {
        let steps = (geom::dist(a, b) / (PITCH * 0.5)).ceil().max(1.0) as usize;
        for i in 0..=steps {
            let t = i as f64 / steps as f64;
            let p = [a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1])];
            let (cx, cy) = Self::cell(p);
            let Some(idx) = self.idx(cx, cy) else {
                return false;
            };
            if self.dynamic[class.idx()][layer as usize][idx] > 0 {
                return false;
            }
            if self.stat[class.idx()][layer as usize][idx] && !exempt.contains(cx, cy, layer) {
                return false;
            }
        }
        true
    }
}

// ---------------------------------------------------------------------------
// A* (single layer, direction state)
// ---------------------------------------------------------------------------

const DIRS: [(i32, i32); 8] = [
    (1, 0),
    (1, 1),
    (0, 1),
    (-1, 1),
    (-1, 0),
    (-1, -1),
    (0, -1),
    (1, -1),
];
const NO_DIR: u8 = 8;

#[derive(Copy, Clone, Eq, PartialEq)]
struct Node {
    f: i32,
    g: i32,
    x: i32,
    y: i32,
    dir: u8,
}

impl Ord for Node {
    fn cmp(&self, other: &Self) -> Ordering {
        other.f.cmp(&self.f).then_with(|| other.g.cmp(&self.g))
    }
}
impl PartialOrd for Node {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

struct Search {
    /// gscore per (cell, dir), generation-stamped so we can reuse it.
    g: Vec<i32>,
    generation: Vec<u32>,
    current: u32,
    came: HashMap<u32, u32>,
}

impl Search {
    fn new() -> Self {
        Search {
            g: Vec::new(),
            generation: Vec::new(),
            current: 0,
            came: HashMap::new(),
        }
    }

    fn reset(&mut self, states: usize) {
        if self.g.len() < states {
            self.g = vec![i32::MAX; states];
            self.generation = vec![0; states];
        }
        self.current += 1;
        self.came.clear();
    }

    fn get(&self, s: usize) -> i32 {
        if self.generation[s] == self.current {
            self.g[s]
        } else {
            i32::MAX
        }
    }

    fn set(&mut self, s: usize, v: i32) {
        self.generation[s] = self.current;
        self.g[s] = v;
    }
}

/// Exemption disks around a routable's own terminals, minus deny disks for
/// foreign pads that overlap them (their static blockage must stay).
struct Exempt {
    disks: Vec<(P, f64, u8)>,
    deny: Vec<(P, f64, u8)>,
}

impl Exempt {
    fn contains(&self, x: i32, y: i32, z: u8) -> bool {
        let px = x as f64 * PITCH;
        let py = y as f64 * PITCH;
        let inside = |set: &[(P, f64, u8)]| {
            set.iter().any(|(c, r, layers)| {
                layers & (1 << z) != 0 && {
                    let dx = px - c[0];
                    let dy = py - c[1];
                    dx * dx + dy * dy <= r * r
                }
            })
        };
        inside(&self.disks) && !inside(&self.deny)
    }
}

fn octile(dx: i32, dy: i32) -> i32 {
    let a = dx.abs();
    let b = dy.abs();
    STEP * a.max(b) + (STEP_DIAG - STEP) * a.min(b)
}

/// Octilinear direction index closest to vector `v`.
fn snap_dir(v: P) -> u8 {
    let mut best = 0usize;
    let mut best_dot = f64::NEG_INFINITY;
    let l = geom::norm(v).max(1e-12);
    for (i, (dx, dy)) in DIRS.iter().enumerate() {
        let dl = ((dx * dx + dy * dy) as f64).sqrt();
        let dot = (v[0] * *dx as f64 + v[1] * *dy as f64) / (l * dl);
        if dot > best_dot {
            best_dot = dot;
            best = i;
        }
    }
    best as u8
}

fn dir_delta(a: u8, b: u8) -> i32 {
    (a as i32 - b as i32)
        .rem_euclid(8)
        .min((b as i32 - a as i32).rem_euclid(8))
}

/// A* between `src` and `dst` on one layer. `start_dir` seeds the incoming
/// direction (a pair trunk must leave its gateway without kinking against
/// the entry stubs); `end_dir` restricts the arrival direction likewise.
#[allow(clippy::too_many_arguments)]
fn astar(
    maps: &Maps,
    search: &mut Search,
    class: Class,
    layer: u8,
    src: P,
    dst: P,
    exempt: &Exempt,
    start_dir: Option<u8>,
    end_dir: Option<u8>,
    soft: bool,
) -> Option<(Vec<(i32, i32)>, i32)> {
    let s = Maps::cell(src);
    let t = Maps::cell(dst);
    let nx = maps.nx;
    let states = (nx * maps.ny) as usize * 9;
    search.reset(states);
    let stat = &maps.stat[class.idx()][layer as usize];
    let dynamic = &maps.dynamic[class.idx()][layer as usize];
    let free = |x: i32, y: i32| -> bool {
        match maps.idx(x, y) {
            None => false,
            Some(i) => (soft || dynamic[i] == 0) && (!stat[i] || exempt.contains(x, y, layer)),
        }
    };
    let soft_cost = |x: i32, y: i32| -> i32 {
        if !soft {
            return 0;
        }
        match maps.idx(x, y) {
            Some(i) if dynamic[i] > 0 => SOFT_CELL,
            _ => 0,
        }
    };
    let enc = |x: i32, y: i32, dir: u8| -> usize { (y * nx + x) as usize * 9 + dir as usize };

    let mut open = BinaryHeap::new();
    if free(s.0, s.1) {
        let d0 = start_dir.unwrap_or(NO_DIR);
        let st = enc(s.0, s.1, d0);
        search.set(st, 0);
        open.push(Node {
            f: octile(s.0 - t.0, s.1 - t.1),
            g: 0,
            x: s.0,
            y: s.1,
            dir: d0,
        });
    }

    let mut expanded = 0u32;
    while let Some(Node { g, x, y, dir, .. }) = open.pop() {
        let u = enc(x, y, dir);
        if g > search.get(u) {
            continue;
        }
        expanded += 1;
        if expanded > EXPAND_CAP {
            return None;
        }
        if x == t.0 && y == t.1 && end_dir.is_none_or(|e| dir != NO_DIR && dir_delta(dir, e) <= 1) {
            let mut path = Vec::new();
            let mut cur = u as u32;
            loop {
                let cell = (cur as usize) / 9;
                path.push((cell as i32 % nx, cell as i32 / nx));
                match search.came.get(&cur) {
                    Some(p) => cur = *p,
                    None => break,
                }
            }
            path.reverse();
            path.dedup();
            return Some((path, g));
        }
        for (nd, (dx, dy)) in DIRS.iter().enumerate() {
            let nxp = x + dx;
            let nyp = y + dy;
            if !free(nxp, nyp) {
                continue;
            }
            // No corner cutting on diagonals.
            if dx * dy != 0 && (!free(x + dx, y) || !free(x, y + dy)) {
                continue;
            }
            let step = if dx * dy != 0 { STEP_DIAG } else { STEP };
            let turn = if dir == NO_DIR {
                0
            } else {
                match (dir as i32 - nd as i32)
                    .rem_euclid(8)
                    .min((nd as i32 - dir as i32).rem_euclid(8))
                {
                    0 => 0,
                    1 => TURN_45,
                    2 => TURN_90,
                    _ => continue, // >90° turns are never worth it
                }
            };
            let ng = g + step + turn + soft_cost(nxp, nyp);
            let v = enc(nxp, nyp, nd as u8);
            if ng < search.get(v) {
                search.set(v, ng);
                search.came.insert(v as u32, u as u32);
                open.push(Node {
                    f: ng + octile(nxp - t.0, nyp - t.1),
                    g: ng,
                    x: nxp,
                    y: nyp,
                    dir: nd as u8,
                });
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Path post-processing
// ---------------------------------------------------------------------------

/// Collapse runs of collinear grid steps into vertices.
fn collapse(cells: &[(i32, i32)]) -> Vec<P> {
    let mut out: Vec<P> = Vec::new();
    for (i, &(x, y)) in cells.iter().enumerate() {
        let p = [x as f64 * PITCH, y as f64 * PITCH];
        if i >= 2 && out.len() >= 2 {
            let prev = out[out.len() - 1];
            let pprev = out[out.len() - 2];
            let d1 = [prev[0] - pprev[0], prev[1] - pprev[1]];
            let d2 = [p[0] - prev[0], p[1] - prev[1]];
            if (d1[0] * d2[1] - d1[1] * d2[0]).abs() < 1e-9 && d1[0] * d2[0] + d1[1] * d2[1] > 0.0 {
                let last = out.len() - 1;
                out[last] = p;
                continue;
            }
        }
        out.push(p);
    }
    out
}

/// Minimum distance between two polylines (segmentwise).
fn poly_min_dist(a: &[P], b: &[P]) -> f64 {
    let mut best = f64::INFINITY;
    if a.len() < 2 || b.len() < 2 {
        return best;
    }
    for wa in a.windows(2) {
        for wb in b.windows(2) {
            best = best.min(geom::dist_seg_seg(wa[0], wa[1], wb[0], wb[1]));
        }
    }
    best
}

/// The two canonical two-segment octilinear connections between `a` and
/// `b` (KiCad's BuildInitialTrace): one diagonal leg + one straight leg,
/// in either order. Returns the corner points.
fn bypass_corners(a: P, b: P) -> [P; 2] {
    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    let d = dx.abs().min(dy.abs());
    let (sx, sy) = (dx.signum(), dy.signum());
    // Diagonal-first corner and straight-first corner.
    [
        [a[0] + sx * d, a[1] + sy * d],
        [b[0] - sx * d, b[1] - sy * d],
    ]
}

/// Greedy line-of-sight shortcutting of one polyline on one layer. When the
/// straight shortcut is blocked, try the two canonical two-segment 45°
/// bypasses (KiCad's mergeStep) before shrinking the span — this collapses
/// staircases that a straight line cannot.
fn shortcut_run(world: &World, pts: &[P], half: f64, clear: f64, layers: u8, owner: u32) -> Vec<P> {
    if pts.len() <= 2 {
        return pts.to_vec();
    }
    let mut out = vec![pts[0]];
    let mut i = 0usize;
    while i + 1 < pts.len() {
        let mut j = pts.len() - 1;
        let mut corner: Option<P> = None;
        while j > i + 1 {
            if world.seg_clear(pts[i], pts[j], half, layers, clear, owner) {
                break;
            }
            corner = bypass_corners(pts[i], pts[j]).into_iter().find(|c| {
                geom::dist(*c, pts[i]) > 0.2
                    && geom::dist(*c, pts[j]) > 0.2
                    && world.seg_clear(pts[i], *c, half, layers, clear, owner)
                    && world.seg_clear(*c, pts[j], half, layers, clear, owner)
            });
            if corner.is_some() {
                break;
            }
            j -= 1;
        }
        if let Some(c) = corner {
            out.push(c);
        }
        out.push(pts[j]);
        i = j;
    }
    out
}

/// Vertices where the direction changes by more than ~30° / ~60°.
pub fn count_bends(pts: &[P]) -> (usize, usize) {
    let mut bends = 0;
    let mut sharp = 0;
    for w in pts.windows(3) {
        let d1 = geom::sub(w[1], w[0]);
        let d2 = geom::sub(w[2], w[1]);
        let l1 = geom::norm(d1);
        let l2 = geom::norm(d2);
        if l1 < 1e-9 || l2 < 1e-9 {
            continue;
        }
        let cosang = (d1[0] * d2[0] + d1[1] * d2[1]) / (l1 * l2);
        if cosang < (30f64).to_radians().cos() {
            bends += 1;
        }
        if cosang < (60f64).to_radians().cos() {
            sharp += 1;
        }
    }
    (bends, sharp)
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

fn perp(v: P) -> P {
    [-v[1], v[0]]
}

fn unit(v: P) -> P {
    let l = geom::norm(v).max(1e-12);
    [v[0] / l, v[1] / l]
}

fn cross(a: P, b: P) -> f64 {
    a[0] * b[1] - a[1] * b[0]
}

fn mid(a: P, b: P) -> P {
    [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0]
}

fn add_scaled(p: P, v: P, k: f64) -> P {
    [p[0] + k * v[0], p[1] + k * v[1]]
}

/// Build routables from the assignment: one per contact, except USB demands
/// which collapse to a centerline pair.
fn build_routables(problem: &Problem, assign: &Assign) -> (Vec<Routable>, Vec<ContactId>) {
    let mut out = Vec::new();
    let mut poured = Vec::new();
    let mut next = 0u32;
    let mut in_pair: std::collections::HashSet<ContactId> = Default::default();
    for (did, sid) in &assign.demand_to_slot {
        let d = &problem.demands[did];
        if d.kind != Kind::UsbHs {
            continue;
        }
        let slot = &problem.slots[sid];
        let members: Vec<(ContactId, MatePinId, P, P)> = d
            .members
            .iter()
            .zip(slot.pins.iter())
            .map(|(c, p)| (*c, *p, problem.contacts[c].xy, problem.pins[p].xy))
            .collect();
        if members.len() != 2 {
            continue;
        }
        for (c, ..) in &members {
            in_pair.insert(*c);
        }
        let src = mid(members[0].2, members[1].2);
        let dst = mid(members[0].3, members[1].3);
        out.push(Routable {
            id: next,
            kind: Kind::UsbHs,
            board: d.board,
            members,
            src,
            dst,
            class: Class::Pair,
        });
        next += 1;
    }
    for (cid, pid) in &assign.contact_to_pin {
        let c = &problem.contacts[cid];
        let Some(kind) = c.ict.kind() else { continue };
        if kind == Kind::Gnd {
            poured.push(*cid);
            continue;
        }
        if in_pair.contains(cid) {
            continue;
        }
        let p = &problem.pins[pid];
        out.push(Routable {
            id: next,
            kind,
            board: c.board,
            members: vec![(*cid, *pid, c.xy, p.xy)],
            src: c.xy,
            dst: p.xy,
            class: Class::of(kind),
        });
        next += 1;
    }
    (out, poured)
}

/// One raw routed centerline: routable index, member index for a loosely
/// routed pair half (None = whole routable), the layer, and the path.
type RawPath = (usize, Option<usize>, u8, Vec<P>);

fn owner_of(id: u32, member: Option<usize>) -> u32 {
    id * 2 + member.unwrap_or(0) as u32
}

/// Where the terminal via of a net on `layer` sits: a top run dives at the
/// mate land; a bottom run rises at the pogo pad.
fn term_via_xy(member: &(ContactId, MatePinId, P, P), layer: u8) -> P {
    if layer == TOP { member.3 } else { member.2 }
}

/// A routed solution for one routable (or one loose pair half).
#[derive(Clone)]
struct Solution {
    layer: u8,
    class: Class,
    cells: Vec<(i32, i32)>,
    pts: Vec<P>,
    stubs: Option<Stubs>,
    vias: Vec<(P, f64)>,
    cost: i32,
    /// Pair only: the rails terminate on each other's assigned lands (the
    /// polarity untwist — DP/DM lands are interchangeable by reassignment).
    swap_dst: bool,
}

type Stubs = [[[P; 3]; 2]; 2];

/// Registry of committed copper for exact via-legality checks. Entries are
/// slots so a ripped-up net can withdraw its own.
#[derive(Default)]
struct Registry {
    items: Vec<Option<(f64, Vec<P>)>>,
}

impl Registry {
    fn add(&mut self, half: f64, pts: Vec<P>) -> usize {
        self.items.push(Some((half, pts)));
        self.items.len() - 1
    }

    fn remove(&mut self, ids: &[usize]) {
        for &i in ids {
            self.items[i] = None;
        }
    }

    /// May a via barrel of `class` sit at `xy` given committed copper?
    fn via_ok(&self, class: Class, xy: P) -> bool {
        let need = |half: f64| class.via_pad_r() + class.clear() + half;
        self.items.iter().flatten().all(|(half, pts)| {
            if pts.len() == 1 {
                dist(pts[0], xy) >= need(*half) - 1e-9
            } else {
                pts.windows(2)
                    .all(|w| geom::dist_point_seg(xy, w[0], w[1]) >= need(*half) - 1e-9)
            }
        })
    }
}

/// One full grid pass: greedy route in the given order, then quality
/// sweeps that rip each net up and reroute it in the finished context —
/// unlucky-order detours straighten out once the world is known.
#[allow(clippy::too_many_arguments)]
fn grid_pass(
    sheet_w: f64,
    sheet_h: f64,
    problem: &Problem,
    routables: &mut [Routable],
    order: &[usize],
    all_pads: &[(P, u8)],
    search: &mut Search,
) -> (Vec<RawPath>, Vec<usize>, usize) {
    let mut maps = Maps::new(sheet_w, sheet_h);
    // Sheet edge.
    let (nx, ny) = (maps.nx, maps.ny);
    for class in CLASSES {
        let m = ((EDGE_MARGIN + class.clear() + class.half()) / PITCH).ceil() as i32;
        let max_x = ((sheet_w / PITCH).floor() as i32 - m).max(0);
        let max_y = ((sheet_h / PITCH).floor() as i32 - m).max(0);
        for y in 0..ny {
            for x in 0..nx {
                if x < m || y < m || x > max_x || y > max_y {
                    let i = (y * nx + x) as usize;
                    maps.stat[class.idx()][0][i] = true;
                    maps.stat[class.idx()][1][i] = true;
                }
            }
        }
    }
    // Panel tooling holes and fiducials are fixed features. A fiducial's
    // keepout is its mask opening: copper inside the aperture would sit
    // exposed next to the dot.
    for (hxy, dia) in &problem.panel.holes {
        maps.stamp_disk_all(*hxy, dia / 2.0, 0b11);
    }
    for fxy in &problem.panel.fids_top {
        maps.stamp_disk_all(*fxy, FID_KEEPOUT_R, 1 << TOP);
    }
    for fxy in &problem.panel.fids_bottom {
        maps.stamp_disk_all(*fxy, FID_KEEPOUT_R, 1 << BOT);
    }
    // Pogo pads are SMT on top; mate lands are SMT on bottom.
    for c in problem.contacts.values() {
        maps.stamp_disk_all(c.xy, PAD_R, 1 << TOP);
    }
    for p in problem.pins.values() {
        maps.stamp_disk_all(p.xy, PAD_R, 1 << BOT);
    }
    // Reserve the via spot above every *assigned* land while its net is
    // still unrouted: a top run must be able to dive there no matter what
    // routes earlier. Reservations are dynamic and lapse once the owner
    // commits (its own copper takes over), so they never over-protect.
    for r in routables.iter() {
        for m in &r.members {
            maps.bump_disk_all(m.3, r.class.via_pad_r(), 1 << TOP, 1);
        }
    }
    let mut reserved: Vec<bool> = vec![true; routables.len()];
    // Reserve a gateway in front of every USB kit on both layers so no net
    // parks across a pair's forced entry corridor.
    let mate_center = {
        let (mw, mh) = crate::pattern::mate_dims(sheet_w, sheet_h);
        [mw / 2.0, mh / 2.0]
    };
    for slot in problem.slots.values() {
        if slot.kind != Kind::UsbHs || slot.pins.len() != 2 {
            continue;
        }
        let a = problem.pins[&slot.pins[0]].xy;
        let b = problem.pins[&slot.pins[1]].xy;
        let (m, n) = gateway(a, b, mate_center);
        maps.stamp_seg(Class::Pair, m, add_scaled(m, n, GATEWAY_LEN), 0b11);
    }

    // Exemption around a net's own pads, with deny disks so foreign pads
    // whose static stamp reaches into an exemption disk keep their blockage.
    let make_exempt = |own: &[(P, u8)], class: Class| -> Exempt {
        let rad = PAD_R + class.clear() + class.half() + PITCH;
        let mut e = Exempt {
            disks: own.iter().map(|(p, l)| (*p, rad, *l)).collect(),
            deny: Vec::new(),
        };
        for (pad, layers) in all_pads {
            if own.iter().any(|(o, _)| dist(*o, *pad) < 1e-6) {
                continue;
            }
            if own.iter().any(|(o, _)| dist(*o, *pad) < 2.0 * rad + 1.0) {
                e.deny.push((
                    *pad,
                    PAD_R + class.clear() + class.half() + GRID_SLOP,
                    *layers,
                ));
            }
        }
        e
    };

    // A terminal via is legal when no *foreign* pad sits within pad +
    // clearance of the barrel — the barrel exists on both layers.
    let via_legal = |member: &(ContactId, MatePinId, P, P), class: Class, layer: u8| -> bool {
        let xy = term_via_xy(member, layer);
        let need = class.via_pad_r() + class.clear() + PAD_R;
        all_pads
            .iter()
            .filter(|(p, _)| dist(*p, member.2) > 1e-6 && dist(*p, member.3) > 1e-6)
            .all(|(p, _)| dist(*p, xy) >= need)
    };

    // Route one routable in the current context. Scores every candidate
    // (layers × pair waypoint variants) and returns the cheapest.
    let route_one = |maps: &Maps,
                     search: &mut Search,
                     registry: &Registry,
                     r: &Routable|
     -> Option<Solution> {
        let own_both: Vec<(P, u8)> = r
            .members
            .iter()
            .flat_map(|(_, _, c, p)| [(*c, 0b11u8), (*p, 0b11u8)])
            .collect();
        let mut exempt = make_exempt(&own_both, r.class);
        // The pair trunk never touches its own pads (the stubs do), so
        // its A* gets a corridor-only exemption — own pads stay solid
        // walls and the trunk can never cut back across its own entry.
        let mut trunk_exempt = Exempt {
            disks: Vec::new(),
            deny: exempt.deny.clone(),
        };

        // (layer, trunk ends a/b, A* ends one lead deeper, entry dirs,
        // pair stubs, dst-land swap)
        type Cand = (u8, P, P, P, P, Option<(u8, u8)>, Option<Stubs>, bool);
        let mut candidates: Vec<Cand> = Vec::new();
        match r.class {
            Class::Pair if pair_direct(r) => {
                // No ribbon candidates: the caller falls back to two
                // direct traces (not reported as a loose pair).
            }
            Class::Pair => {
                let u_s = unit(geom::sub(r.members[0].2, r.src));
                let u_d = unit(geom::sub(r.members[0].3, r.dst));
                let (_, n_d_out) = gateway(r.members[0].3, r.members[1].3, mate_center);
                // The pair may use its own reserved gateway, and — as a
                // fallback — the mirrored inward corridor through the band.
                // Corridor half-width just over the trunk envelope: a
                // wider exemption would let the trunk ride over its own
                // lands' terminal vias.
                for i in -12..=12i32 {
                    let p = add_scaled(r.dst, n_d_out, GATEWAY_LEN * i as f64 / 12.0);
                    exempt.disks.push((p, 0.6, 0b11));
                    trunk_exempt.disks.push((p, 0.6, 0b11));
                }
                for layer in [TOP, BOT] {
                    let ok = r.members.iter().all(|m| {
                        via_legal(m, Class::Pair, layer)
                            && registry.via_ok(Class::Pair, term_via_xy(m, layer))
                    });
                    if !ok {
                        continue;
                    }
                    for flip_d in [false, true] {
                        let n_d = if flip_d {
                            [-n_d_out[0], -n_d_out[1]]
                        } else {
                            n_d_out
                        };
                        // Side of DP at the dst end (travel direction
                        // -n_d) when it lands on its own assigned pad.
                        let side_d = cross([-n_d[0], -n_d[1]], u_d) > 0.0;
                        let n_s_raw = perp(u_s);
                        // Both departure directions are legal: a polarity
                        // mismatch is untwisted by terminating each rail
                        // on the peer's land, never by forcing the trunk
                        // into a hairpin.
                        for flip_s in [false, true] {
                            let n_s = if flip_s {
                                [-n_s_raw[0], -n_s_raw[1]]
                            } else {
                                n_s_raw
                            };
                            let side_s = cross(n_s, u_s) > 0.0;
                            let swap = side_s != side_d;
                            let dst = if swap {
                                [r.members[1].3, r.members[0].3]
                            } else {
                                [r.members[0].3, r.members[1].3]
                            };
                            let (w_s, stubs_s) = pair_entry([r.members[0].2, r.members[1].2], n_s);
                            let (w_d, stubs_d) = pair_entry(dst, n_d);
                            let uv = |d: u8| -> P {
                                let (dx, dy) = DIRS[d as usize];
                                let l = ((dx * dx + dy * dy) as f64).sqrt();
                                [dx as f64 / l, dy as f64 / l]
                            };
                            let ds = snap_dir(n_s);
                            let a2 = add_scaled(w_s, uv(ds), PAIR_LEAD);
                            let dd = snap_dir(n_d);
                            let b2 = add_scaled(w_d, uv(dd), PAIR_LEAD);
                            let de = snap_dir(unit(geom::sub(w_d, b2)));
                            candidates.push((
                                layer,
                                w_s,
                                w_d,
                                a2,
                                b2,
                                Some((ds, de)),
                                Some([stubs_s, stubs_d]),
                                swap,
                            ));
                        }
                    }
                }
            }
            Class::Power | Class::Ls => {
                for layer in [TOP, BOT] {
                    if via_legal(&r.members[0], r.class, layer)
                        && registry.via_ok(r.class, term_via_xy(&r.members[0], layer))
                    {
                        candidates.push((layer, r.src, r.dst, r.src, r.dst, None, None, false));
                    }
                }
            }
        }

        let handicap = |layer: u8| -> i32 {
            match r.class {
                Class::Pair | Class::Power => {
                    if layer == BOT {
                        60
                    } else {
                        0
                    }
                }
                Class::Ls => {
                    let d = geom::sub(r.dst, r.src);
                    let preferred = if d[0].abs() >= d[1].abs() { TOP } else { BOT };
                    if layer == preferred { 0 } else { 150 }
                }
            }
        };
        let mut best: Option<Solution> = None;
        for (layer, a, b, a2, b2, dirs, stubs, swap) in &candidates {
            let (layer, a, b, a2, b2) = (*layer, *a, *b, *a2, *b2);
            if let Some(stubs) = stubs {
                // Pair entry stubs and trunk leads are fixed geometry:
                // reject candidates that collide before spending an A*.
                let clear_stubs = stubs.iter().all(|end| {
                    end.iter().all(|st| {
                        maps.seg_free(Class::Ls, layer, st[0], st[1], &exempt)
                            && maps.seg_free(Class::Ls, layer, st[1], st[2], &exempt)
                    })
                }) && maps.seg_free(Class::Pair, layer, a, a2, &trunk_exempt)
                    && maps.seg_free(Class::Pair, layer, b2, b, &trunk_exempt);
                if !clear_stubs {
                    continue;
                }
            }
            let (d_start, d_end) = dirs
                .map(|(x, y)| (Some(x), Some(y)))
                .unwrap_or((None, None));
            let ex = if stubs.is_some() {
                &trunk_exempt
            } else {
                &exempt
            };
            if let Some((cells, cost)) = astar(
                maps, search, r.class, layer, a2, b2, ex, d_start, d_end, false,
            ) {
                let eff = cost + handicap(layer);
                if best.as_ref().is_none_or(|q| eff < q.cost) {
                    let mut pts = collapse(&cells);
                    if let Some(stubs) = stubs {
                        // Fixed straight leads keep the junctions clean.
                        pts.insert(0, a);
                        pts.push(b);
                        // The trunk's own stubs are not in the maps (they
                        // commit together), so keep the ribbon envelope
                        // off its own pad→knee copper explicitly.
                        let need = Class::Pair.half() + Class::Pair.clear() + USB_W / 2.0 - 1e-9;
                        let ok = stubs.iter().all(|end| {
                            end.iter().all(|st| {
                                pts.windows(2)
                                    .all(|w| geom::dist_seg_seg(w[0], w[1], st[0], st[1]) >= need)
                            })
                        });
                        if !ok {
                            continue;
                        }
                    }
                    let vias: Vec<(P, f64)> = if stubs.is_some() {
                        (0..2)
                            .map(|k| {
                                // Rail k terminates on the effective
                                // (possibly swapped) land.
                                let j = if *swap { 1 - k } else { k };
                                let m = (
                                    r.members[k].0,
                                    r.members[j].1,
                                    r.members[k].2,
                                    r.members[j].3,
                                );
                                (term_via_xy(&m, layer), r.class.via_pad_r())
                            })
                            .collect()
                    } else {
                        vec![(term_via_xy(&r.members[0], layer), r.class.via_pad_r())]
                    };
                    best = Some(Solution {
                        layer,
                        class: r.class,
                        cells,
                        pts,
                        stubs: *stubs,
                        vias,
                        cost: eff,
                        swap_dst: *swap,
                    });
                }
            }
        }
        best
    };

    let commit = |maps: &mut Maps, registry: &mut Registry, sol: &Solution| -> Vec<usize> {
        let mut ids = Vec::new();
        maps.bump_route(sol.class, sol.layer, &sol.cells, 1);
        ids.push(registry.add(sol.class.half(), sol.pts.clone()));
        if let Some(stubs) = &sol.stubs {
            let n = sol.pts.len();
            maps.bump_seg(sol.class, sol.pts[0], sol.pts[1], 1 << sol.layer, 1);
            maps.bump_seg(sol.class, sol.pts[n - 2], sol.pts[n - 1], 1 << sol.layer, 1);
            for end in stubs {
                for st in end {
                    maps.bump_seg(Class::Ls, st[0], st[1], 1 << sol.layer, 1);
                    maps.bump_seg(Class::Ls, st[1], st[2], 1 << sol.layer, 1);
                    ids.push(registry.add(USB_W / 2.0, st.to_vec()));
                }
            }
        }
        for (xy, r) in &sol.vias {
            maps.bump_disk_all(*xy, *r, 0b11, 1);
            ids.push(registry.add(*r, vec![*xy]));
        }
        ids
    };
    let uncommit = |maps: &mut Maps, registry: &mut Registry, sol: &Solution, ids: &[usize]| {
        maps.bump_route(sol.class, sol.layer, &sol.cells, -1);
        if let Some(stubs) = &sol.stubs {
            let n = sol.pts.len();
            maps.bump_seg(sol.class, sol.pts[0], sol.pts[1], 1 << sol.layer, -1);
            maps.bump_seg(
                sol.class,
                sol.pts[n - 2],
                sol.pts[n - 1],
                1 << sol.layer,
                -1,
            );
            for end in stubs {
                for st in end {
                    maps.bump_seg(Class::Ls, st[0], st[1], 1 << sol.layer, -1);
                    maps.bump_seg(Class::Ls, st[1], st[2], 1 << sol.layer, -1);
                }
            }
        }
        for (xy, r) in &sol.vias {
            maps.bump_disk_all(*xy, *r, 0b11, -1);
        }
        registry.remove(ids);
    };

    fn set_reserved(maps: &mut Maps, reserved: &mut [bool], r: &Routable, ri: usize, want: bool) {
        if reserved[ri] == want {
            return;
        }
        let delta = if want { 1 } else { -1 };
        for m in &r.members {
            maps.bump_disk_all(m.3, r.class.via_pad_r(), 1 << TOP, delta);
        }
        reserved[ri] = want;
    }

    // ---- greedy pass ----
    let mut registry = Registry::default();
    let mut sols: HashMap<usize, (Solution, Vec<usize>)> = HashMap::new();
    let mut halves: Vec<(usize, usize, Solution)> = Vec::new();
    let mut failed = Vec::new();
    let mut loose = 0usize;
    for &ri in order {
        let r = routables[ri].clone();
        set_reserved(&mut maps, &mut reserved, &r, ri, false);
        match route_one(&maps, search, &registry, &r) {
            Some(sol) => {
                let ids = commit(&mut maps, &mut registry, &sol);
                sols.insert(ri, (sol, ids));
            }
            None if r.class == Class::Pair => {
                // Loose fallback: no legal ribbon exists. Route DP and DM as
                // two individual traces and report the pair as loose.
                let mut got = Vec::new();
                for (k, m) in r.members.iter().enumerate() {
                    let half_r = Routable {
                        id: r.id,
                        kind: r.kind,
                        board: r.board,
                        members: vec![*m],
                        src: m.2,
                        dst: m.3,
                        class: Class::Ls,
                    };
                    match route_one(&maps, search, &registry, &half_r) {
                        Some(sol) => {
                            commit(&mut maps, &mut registry, &sol);
                            got.push((ri, k, sol));
                        }
                        None => break,
                    }
                }
                if got.len() == 2 {
                    if !pair_direct(&r) {
                        loose += 1;
                    }
                    halves.extend(got);
                } else {
                    set_reserved(&mut maps, &mut reserved, &r, ri, true);
                    failed.push(ri);
                }
            }
            None => {
                if std::env::var_os("INTERPOSER_DRC_DEBUG").is_some() {
                    eprintln!(
                        "FAIL {:?} net {} src ({:.1},{:.1}) dst ({:.1},{:.1})",
                        r.kind, r.id, r.src[0], r.src[1], r.dst[0], r.dst[1],
                    );
                }
                set_reserved(&mut maps, &mut reserved, &r, ri, true);
                failed.push(ri);
            }
        }
    }

    // ---- quality sweeps: rip up and reroute each net in final context ----
    // Strict improvement only, worst-cost first, stop when a sweep accepts
    // nothing (gains are front-loaded; Freerouting-style).
    for _sweep in 0..4 {
        let mut accepted = 0usize;
        // Failed nets get first claim on any space the sweeps free up. A
        // unit-kind net (LS, VUSB) whose assigned land is unreachable may
        // also renegotiate: interchangeability means any free land of the
        // same kind will do.
        let still_failed: Vec<usize> = std::mem::take(&mut failed);
        for ri in still_failed {
            {
                let r = routables[ri].clone();
                set_reserved(&mut maps, &mut reserved, &r, ri, false);
            }
            if let Some(sol) = route_one(&maps, search, &registry, &routables[ri]) {
                let ids = commit(&mut maps, &mut registry, &sol);
                sols.insert(ri, (sol, ids));
                accepted += 1;
                continue;
            }
            let r = routables[ri].clone();
            let mut adopted = false;
            if matches!(r.kind, Kind::Ls | Kind::Vusb) {
                let used: std::collections::HashSet<MatePinId> = routables
                    .iter()
                    .flat_map(|q| q.members.iter().map(|m| m.1))
                    .collect();
                let mut frees: Vec<(f64, MatePinId, P)> = problem
                    .slots
                    .values()
                    .filter(|sl| sl.kind == r.kind && sl.pins.len() == 1)
                    .filter(|sl| !used.contains(&sl.pins[0]))
                    .map(|sl| {
                        let xy = problem.pins[&sl.pins[0]].xy;
                        (dist(r.src, xy), sl.pins[0], xy)
                    })
                    .collect();
                frees.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(Ordering::Equal));
                for (_, pin, xy) in frees.into_iter().take(8) {
                    let mut alt = r.clone();
                    alt.members[0].1 = pin;
                    alt.members[0].3 = xy;
                    alt.dst = xy;
                    if let Some(sol) = route_one(&maps, search, &registry, &alt) {
                        let ids = commit(&mut maps, &mut registry, &sol);
                        sols.insert(ri, (sol, ids));
                        routables[ri] = alt;
                        accepted += 1;
                        adopted = true;
                        break;
                    }
                }
            }
            // A full-pool unit kind (every land in use) can still swap
            // lands with a routed peer: rip the peer, exchange, reroute
            // both, revert atomically if either loses.
            if !adopted && matches!(r.kind, Kind::Ls | Kind::Vusb) {
                let mut peers: Vec<(f64, usize)> = sols
                    .keys()
                    .filter(|ri2| {
                        let q = &routables[**ri2];
                        q.kind == r.kind && q.members.len() == 1
                    })
                    .map(|ri2| (dist(r.src, routables[*ri2].members[0].3), *ri2))
                    .collect();
                peers.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(Ordering::Equal));
                for (_, ri2) in peers.into_iter().take(10) {
                    let (sol2, ids2) = sols.remove(&ri2).unwrap();
                    uncommit(&mut maps, &mut registry, &sol2, &ids2);
                    let peer = routables[ri2].clone();
                    let mut alt_x = r.clone();
                    alt_x.members[0].1 = peer.members[0].1;
                    alt_x.members[0].3 = peer.members[0].3;
                    alt_x.dst = peer.members[0].3;
                    let mut alt_y = peer.clone();
                    alt_y.members[0].1 = r.members[0].1;
                    alt_y.members[0].3 = r.members[0].3;
                    alt_y.dst = r.members[0].3;
                    if let Some(sx) = route_one(&maps, search, &registry, &alt_x) {
                        let sx_ids = commit(&mut maps, &mut registry, &sx);
                        if let Some(sy) = route_one(&maps, search, &registry, &alt_y) {
                            let sy_ids = commit(&mut maps, &mut registry, &sy);
                            sols.insert(ri, (sx, sx_ids));
                            sols.insert(ri2, (sy, sy_ids));
                            routables[ri] = alt_x;
                            routables[ri2] = alt_y;
                            accepted += 1;
                            adopted = true;
                            break;
                        }
                        uncommit(&mut maps, &mut registry, &sx, &sx_ids);
                    }
                    let ids2 = commit(&mut maps, &mut registry, &sol2);
                    sols.insert(ri2, (sol2, ids2));
                }
            }
            // Last resort for walled-in singles: shove. Find the cheapest
            // soft path (occupied cells cost extra instead of blocking),
            // rip up just the nets it crosses, route, and put them back —
            // reverting atomically if anyone ends up worse off.
            if !adopted && r.class != Class::Pair {
                'layers: for layer in [TOP, BOT] {
                    let m = &r.members[0];
                    if !via_legal(m, r.class, layer)
                        || !registry.via_ok(r.class, term_via_xy(m, layer))
                    {
                        continue;
                    }
                    let own_both: Vec<(P, u8)> = r
                        .members
                        .iter()
                        .flat_map(|(_, _, c, p)| [(*c, 0b11u8), (*p, 0b11u8)])
                        .collect();
                    let exempt = make_exempt(&own_both, r.class);
                    let Some((cells, _)) = astar(
                        &maps, search, r.class, layer, r.src, r.dst, &exempt, None, None, true,
                    ) else {
                        continue;
                    };
                    let path = collapse(&cells);
                    let blockers: Vec<usize> = sols
                        .iter()
                        .filter(|(_, (sol, _))| {
                            sol.layer == layer
                                && poly_min_dist(&sol.pts, &path)
                                    < sol.class.half() + sol.class.clear() + r.class.half()
                        })
                        .map(|(ri2, _)| *ri2)
                        .collect();
                    if blockers.is_empty() || blockers.len() > 6 {
                        continue;
                    }
                    // Rip the blockers out.
                    let mut saved = Vec::new();
                    for b in &blockers {
                        let (sol, ids) = sols.remove(b).unwrap();
                        uncommit(&mut maps, &mut registry, &sol, &ids);
                        saved.push((*b, sol));
                    }
                    let restore =
                        |maps: &mut Maps,
                         registry: &mut Registry,
                         sols: &mut HashMap<usize, (Solution, Vec<usize>)>,
                         saved: Vec<(usize, Solution)>| {
                            for (b, sol) in saved {
                                let ids = commit(maps, registry, &sol);
                                sols.insert(b, (sol, ids));
                            }
                        };
                    let Some(self_sol) = route_one(&maps, search, &registry, &r) else {
                        restore(&mut maps, &mut registry, &mut sols, saved);
                        continue;
                    };
                    let self_ids = commit(&mut maps, &mut registry, &self_sol);
                    let mut rerouted = Vec::new();
                    let mut all_ok = true;
                    for (b, _) in &saved {
                        match route_one(&maps, search, &registry, &routables[*b]) {
                            Some(sol) => {
                                let ids = commit(&mut maps, &mut registry, &sol);
                                rerouted.push((*b, sol, ids));
                            }
                            None => {
                                all_ok = false;
                                break;
                            }
                        }
                    }
                    if all_ok {
                        sols.insert(ri, (self_sol, self_ids));
                        for (b, sol, ids) in rerouted {
                            sols.insert(b, (sol, ids));
                        }
                        accepted += 1;
                        adopted = true;
                        break 'layers;
                    }
                    // Revert everything.
                    for (b, sol, ids) in rerouted {
                        uncommit(&mut maps, &mut registry, &sol, &ids);
                        let _ = b;
                    }
                    uncommit(&mut maps, &mut registry, &self_sol, &self_ids);
                    restore(&mut maps, &mut registry, &mut sols, saved);
                }
            }
            if !adopted {
                let r = routables[ri].clone();
                set_reserved(&mut maps, &mut reserved, &r, ri, true);
                failed.push(ri);
            }
        }
        let mut idx: Vec<usize> = sols.keys().copied().collect();
        idx.sort_by_key(|ri| std::cmp::Reverse(sols[ri].0.cost));
        for ri in idx {
            let (old, old_ids) = sols.remove(&ri).unwrap();
            uncommit(&mut maps, &mut registry, &old, &old_ids);
            let better = match route_one(&maps, search, &registry, &routables[ri]) {
                Some(new) if new.cost + 5 < old.cost => {
                    accepted += 1;
                    new
                }
                _ => old,
            };
            let ids = commit(&mut maps, &mut registry, &better);
            sols.insert(ri, (better, ids));
        }
        if accepted == 0 {
            break;
        }
    }

    let mut raw: Vec<RawPath> = Vec::new();
    for &ri in order {
        if let Some((sol, _)) = sols.get(&ri) {
            raw.push((ri, None, sol.layer, sol.pts.clone()));
            if sol.swap_dst {
                // Land-swap polarity untwist: make the assignment record
                // what the copper actually does.
                let m = &mut routables[ri].members;
                let (a1, a3) = (m[0].1, m[0].3);
                (m[0].1, m[0].3) = (m[1].1, m[1].3);
                (m[1].1, m[1].3) = (a1, a3);
            }
        }
    }
    for (ri, k, sol) in halves {
        raw.push((ri, Some(k), sol.layer, sol.pts));
    }
    (raw, failed, loose)
}

pub fn route_r5(sheet_w: f64, sheet_h: f64, problem: &Problem, assign: &Assign) -> RouteResult {
    let (mut routables, poured) = build_routables(problem, assign);

    // All pads, for exemption deny lookups near a routable's own terminals.
    let all_pads: Vec<(P, u8)> = problem
        .contacts
        .values()
        .map(|c| (c.xy, 1u8 << TOP))
        .chain(problem.pins.values().map(|p| (p.xy, 1u8 << BOT)))
        .collect();

    let mut out = RouteResult {
        router: "R5".into(),
        order: routables.iter().map(|r| r.kind).collect(),
        poured,
        ..RouteResult::default()
    };

    let mut search = Search::new();

    // Rip-up by reordering: greedy passes where previously failed nets are
    // promoted to the front of their class group. Keep the best attempt.
    let mut boost: std::collections::HashSet<u32> = Default::default();
    let mut best: Option<(Vec<RawPath>, Vec<usize>, usize, Vec<Routable>)> = None;
    for attempt in 0..5 {
        let mut order: Vec<usize> = (0..routables.len()).collect();
        order.sort_by(|&a, &b| {
            let ra = &routables[a];
            let rb = &routables[b];
            // From the third attempt on, stuck nets route before everyone —
            // walls form early, so a within-class promotion is not enough.
            let global = |r: &Routable| attempt >= 2 && boost.contains(&r.id);
            global(rb)
                .cmp(&global(ra))
                .then(kind_pri(ra.kind).cmp(&kind_pri(rb.kind)))
                .then_with(|| boost.contains(&rb.id).cmp(&boost.contains(&ra.id)))
                .then_with(|| {
                    // Longest first: long funnel nets claim corridors early.
                    dist(rb.src, rb.dst)
                        .partial_cmp(&dist(ra.src, ra.dst))
                        .unwrap_or(Ordering::Equal)
                })
                .then(ra.id.cmp(&rb.id))
        });
        let (raw, failed, loose) = grid_pass(
            sheet_w,
            sheet_h,
            problem,
            &mut routables,
            &order,
            &all_pads,
            &mut search,
        );
        let n_failed = failed.len();
        let before = boost.len();
        for &ri in &failed {
            boost.insert(routables[ri].id);
        }
        let grew = boost.len() > before;
        let better = match &best {
            None => true,
            Some((_, bf, bl, _)) => (n_failed, loose) < (bf.len(), *bl),
        };
        if better {
            // Snapshot the members alongside the paths: later attempts keep
            // reassigning and swapping lands, and the emitted geometry must
            // agree with the member state it was routed against.
            best = Some((raw, failed, loose, routables.clone()));
        }
        if n_failed == 0 || !grew {
            break;
        }
    }
    let (raw, failed_ris, loose_pairs, routables) = best.unwrap_or_default();
    out.loose_pairs = loose_pairs;
    for (ri, member, _, _) in &raw {
        if member.is_some() {
            out.loose_contacts
                .push(routables[*ri].members[member.unwrap()].0);
        }
    }
    for &ri in &failed_ris {
        let r = &routables[ri];
        for (cid, pid, cxy, pxy) in &r.members {
            out.failed.push(TwoPinNet {
                contact: *cid,
                pin: *pid,
                kind: r.kind,
                board: r.board,
                src: *cxy,
                dst: *pxy,
                width: r.class.width(),
            });
        }
    }

    // ---------------- GND stitching ----------------
    // Every poured pogo needs a via into the bottom pour. Prefer the via in
    // the pad itself; when something sits underneath (an unassigned land,
    // someone's trace), walk outward for the nearest legal spot and add a
    // short top stub to it.
    {
        let gnd_lands: std::collections::HashSet<crate::types::MatePinId> = problem
            .slots
            .values()
            .filter(|sl| sl.kind == Kind::Gnd)
            .flat_map(|sl| sl.pins.iter().copied())
            .collect();
        let gnd_contacts: std::collections::HashSet<ContactId> =
            out.poured.iter().copied().collect();
        // Fixed copper the stitch must respect.
        let mut vias: Vec<P> = Vec::new();
        for (ri, member, layer, _) in &raw {
            let r = &routables[*ri];
            let ks: Vec<usize> = match member {
                Some(k) => vec![*k],
                None => (0..r.members.len()).collect(),
            };
            for k in ks {
                vias.push(term_via_xy(&r.members[k], *layer));
            }
        }
        let mut traces: Vec<(u8, f64, Vec<P>)> = raw
            .iter()
            .map(|(ri, member, layer, pts)| {
                let half = if member.is_some() {
                    USB_W / 2.0
                } else {
                    routables[*ri].class.half()
                };
                (*layer, half, pts.clone())
            })
            .collect();
        // Pair entry stubs are copper too — they reach well outside the
        // trunk's coupled envelope.
        for (ri, member, layer, pts) in &raw {
            let r = &routables[*ri];
            if member.is_some() || r.class != Class::Pair || pts.len() < 2 {
                continue;
            }
            let n_s = unit(geom::sub(pts[0], r.src));
            let n_d = unit(geom::sub(pts[pts.len() - 1], r.dst));
            let (_, stubs_s) = pair_entry([r.members[0].2, r.members[1].2], n_s);
            let (_, stubs_d) = pair_entry([r.members[0].3, r.members[1].3], n_d);
            for end in [stubs_s, stubs_d] {
                for st in end {
                    traces.push((*layer, USB_W / 2.0, st.to_vec()));
                }
            }
        }
        let seg_min = |pts: &[P], xy: P| -> f64 {
            pts.windows(2)
                .map(|w| geom::dist_point_seg(xy, w[0], w[1]))
                .fold(f64::INFINITY, f64::min)
        };
        // Tooling holes and fiducials are fixed features every stitch must
        // clear: via barrels stay off hole walls and outside fiducial mask
        // openings; stubs likewise.
        let panel = &problem.panel;
        let fixed_via_ok = |xy: P| -> bool {
            panel
                .holes
                .iter()
                .all(|(h, d)| dist(*h, xy) >= d / 2.0 + 0.65)
                && panel
                    .fids_top
                    .iter()
                    .chain(panel.fids_bottom.iter())
                    .all(|f| dist(*f, xy) >= FID_KEEPOUT_R + 0.55)
        };
        let fixed_stub_ok = |a: P, b: P| -> bool {
            panel
                .holes
                .iter()
                .all(|(h, d)| geom::dist_point_seg(*h, a, b) >= d / 2.0 + 0.4)
                && panel
                    .fids_top
                    .iter()
                    .chain(panel.fids_bottom.iter())
                    .all(|f| geom::dist_point_seg(*f, a, b) >= FID_KEEPOUT_R + 0.4)
        };
        let poured: Vec<ContactId> = out.poured.clone();
        let mut stitches: Vec<(ContactId, Vec<P>, P)> = Vec::new();
        for cid in poured {
            let c = &problem.contacts[&cid];
            let via_ok = |xy: P, stitches: &[(ContactId, Vec<P>, P)]| -> bool {
                let m = EDGE_MARGIN + 0.3;
                if xy[0] < m || xy[1] < m || xy[0] > sheet_w - m || xy[1] > sheet_h - m {
                    return false;
                }
                // Foreign pads on either side (GND copper is the same net).
                let ok_pads = problem
                    .contacts
                    .values()
                    .all(|o| gnd_contacts.contains(&o.id) || dist(o.xy, xy) >= 1.05)
                    && problem
                        .pins
                        .values()
                        .all(|p| gnd_lands.contains(&p.id) || dist(p.xy, xy) >= 1.05);
                let ok_traces = traces
                    .iter()
                    .all(|(_, half, pts)| seg_min(pts, xy) >= 0.3 + 0.25 + half);
                let ok_vias = vias.iter().all(|v| dist(*v, xy) >= 0.85);
                let ok_stitch = stitches.iter().all(|(_, _, v)| dist(*v, xy) >= 0.85);
                ok_pads && ok_traces && ok_vias && ok_stitch && fixed_via_ok(xy)
            };
            let stub_ok = |a: P, b: P, stitches: &[(ContactId, Vec<P>, P)]| -> bool {
                let need_pad = 0.125 + 0.25 + 0.5;
                let ok_pads = problem.contacts.values().all(|o| {
                    o.id == cid
                        || gnd_contacts.contains(&o.id)
                        || geom::dist_point_seg(o.xy, a, b) >= need_pad
                });
                let ok_traces = traces.iter().all(|(layer, half, pts)| {
                    *layer != TOP || {
                        pts.windows(2)
                            .all(|w| geom::dist_seg_seg(a, b, w[0], w[1]) >= 0.125 + 0.25 + half)
                    }
                });
                let ok_stubs = stitches.iter().all(|(_, stub, _)| {
                    stub.len() < 2 || geom::dist_seg_seg(a, b, stub[0], stub[1]) >= 0.5
                });
                ok_pads && ok_traces && ok_stubs && fixed_stub_ok(a, b)
            };
            let mut found = None;
            if via_ok(c.xy, &stitches) {
                found = Some((Vec::new(), c.xy));
            } else {
                'search: for ring in 1..=14 {
                    let rad = ring as f64 * 0.25;
                    for k in 0..16 {
                        let a = std::f64::consts::TAU * k as f64 / 16.0;
                        let xy = [c.xy[0] + rad * a.cos(), c.xy[1] + rad * a.sin()];
                        if via_ok(xy, &stitches) && stub_ok(c.xy, xy, &stitches) {
                            found = Some((vec![c.xy, xy], xy));
                            break 'search;
                        }
                    }
                }
            }
            if let Some((stub, xy)) = found {
                stitches.push((cid, stub, xy));
            } else if std::env::var_os("INTERPOSER_DRC_DEBUG").is_some() {
                eprintln!("GND stitch failed at ({:.1},{:.1})", c.xy[0], c.xy[1]);
            }
        }
        out.gnd_stitches = stitches
            .into_iter()
            .map(|(c, stub, xy)| (c, stub, xy))
            .collect();

        // Every GND land also gets a via into the top pour: the bottom fill
        // fragments around dense trace fields, and each fragment must reach
        // the rest of the net through the other layer.
        let mut land_stitches: Vec<(crate::types::MatePinId, Vec<P>, P)> = Vec::new();
        for (pid, p) in &problem.pins {
            if !gnd_lands.contains(pid) {
                continue;
            }
            let via_ok = |xy: P, lst: &[(crate::types::MatePinId, Vec<P>, P)]| -> bool {
                let m = EDGE_MARGIN + 0.3;
                if xy[0] < m || xy[1] < m || xy[0] > sheet_w - m || xy[1] > sheet_h - m {
                    return false;
                }
                let ok_pads = problem
                    .contacts
                    .values()
                    .all(|o| gnd_contacts.contains(&o.id) || dist(o.xy, xy) >= 1.05)
                    && problem
                        .pins
                        .values()
                        .all(|q| gnd_lands.contains(&q.id) || dist(q.xy, xy) >= 1.05);
                let ok_traces = traces
                    .iter()
                    .all(|(_, half, pts)| seg_min(pts, xy) >= 0.3 + 0.25 + half);
                let ok_vias = vias.iter().all(|v| dist(*v, xy) >= 0.85);
                let ok_stitch = out
                    .gnd_stitches
                    .iter()
                    .all(|(_, _, v)| dist(*v, xy) >= 0.85)
                    && lst.iter().all(|(_, _, v)| dist(*v, xy) >= 0.85);
                ok_pads && ok_traces && ok_vias && ok_stitch && fixed_via_ok(xy)
            };
            let stub_ok = |a: P, b: P, lst: &[(crate::types::MatePinId, Vec<P>, P)]| -> bool {
                let need_pad = 0.125 + 0.25 + 0.5;
                let ok_pads = problem.pins.values().all(|q| {
                    q.id == *pid
                        || gnd_lands.contains(&q.id)
                        || geom::dist_point_seg(q.xy, a, b) >= need_pad
                });
                let ok_traces = traces.iter().all(|(layer, half, pts)| {
                    *layer != BOT
                        || pts
                            .windows(2)
                            .all(|w| geom::dist_seg_seg(a, b, w[0], w[1]) >= 0.125 + 0.25 + half)
                });
                let ok_stubs = lst.iter().all(|(_, stub, _)| {
                    stub.len() < 2 || geom::dist_seg_seg(a, b, stub[0], stub[1]) >= 0.5
                });
                ok_pads && ok_traces && ok_stubs && fixed_stub_ok(a, b)
            };
            let mut found = None;
            if via_ok(p.xy, &land_stitches) {
                found = Some((Vec::new(), p.xy));
            } else {
                'search: for ring in 1..=14 {
                    let rad = ring as f64 * 0.25;
                    for k in 0..16 {
                        let a = std::f64::consts::TAU * k as f64 / 16.0;
                        let xy = [p.xy[0] + rad * a.cos(), p.xy[1] + rad * a.sin()];
                        if via_ok(xy, &land_stitches) && stub_ok(p.xy, xy, &land_stitches) {
                            found = Some((vec![p.xy, xy], xy));
                            break 'search;
                        }
                    }
                }
            }
            if let Some((stub, xy)) = found {
                land_stitches.push((*pid, stub, xy));
            } else if std::env::var_os("INTERPOSER_DRC_DEBUG").is_some() {
                eprintln!("GND land stitch failed at ({:.1},{:.1})", p.xy[0], p.xy[1]);
            }
        }
        out.gnd_land_stitches = land_stitches;

        // Field stitching: a sparse via grid over the sheet ties the top
        // and bottom pours into one net even when dense trace fields
        // fragment them. A blocked grid point ring-searches its cell for
        // the nearest legal spot, so every cell that has one gets a via —
        // dense regions are exactly where the pours need the tie.
        let mut field: Vec<P> = Vec::new();
        let step = 12.0;
        let ok_at = |xy: P, field: &[P]| -> bool {
            let m = EDGE_MARGIN + 0.3;
            xy[0] >= m
                && xy[1] >= m
                && xy[0] <= sheet_w - m
                && xy[1] <= sheet_h - m
                && problem.contacts.values().all(|o| dist(o.xy, xy) >= 1.05)
                && problem.pins.values().all(|q| dist(q.xy, xy) >= 1.05)
                && traces
                    .iter()
                    .all(|(_, half, pts)| seg_min(pts, xy) >= 0.3 + 0.25 + half)
                && vias.iter().all(|v| dist(*v, xy) >= 0.85)
                && out
                    .gnd_stitches
                    .iter()
                    .all(|(_, _, v)| dist(*v, xy) >= 0.85)
                && out
                    .gnd_land_stitches
                    .iter()
                    .all(|(_, _, v)| dist(*v, xy) >= 0.85)
                && field.iter().all(|v| dist(*v, xy) >= 0.85)
                && fixed_via_ok(xy)
        };
        let mut y = 6.0;
        while y < sheet_h - 6.0 + 1e-9 {
            let mut x = 6.0;
            while x < sheet_w - 6.0 + 1e-9 {
                let c = [x, y];
                if ok_at(c, &field) {
                    field.push(c);
                } else {
                    'ring: for ring in 1..=11 {
                        let rad = ring as f64 * 0.5;
                        for k in 0..16 {
                            let a = std::f64::consts::TAU * k as f64 / 16.0;
                            let xy = [c[0] + rad * a.cos(), c[1] + rad * a.sin()];
                            if ok_at(xy, &field) {
                                field.push(xy);
                                break 'ring;
                            }
                        }
                    }
                }
                x += step;
            }
            y += step;
        }
        out.gnd_field_stitches = field;
    }

    // ---------------- smoothing world ----------------
    let mut world = World::new(sheet_w, sheet_h, EDGE_MARGIN);
    for (hxy, dia) in &problem.panel.holes {
        world.add(Obstacle::disk(*hxy, dia / 2.0, 0b11, u32::MAX));
    }
    for fxy in &problem.panel.fids_top {
        world.add(Obstacle::disk(*fxy, FID_KEEPOUT_R, 1 << TOP, u32::MAX));
    }
    for fxy in &problem.panel.fids_bottom {
        world.add(Obstacle::disk(*fxy, FID_KEEPOUT_R, 1 << BOT, u32::MAX));
    }
    for c in problem.contacts.values() {
        world.add(Obstacle::disk(c.xy, PAD_R, 1 << TOP, u32::MAX));
    }
    for p in problem.pins.values() {
        world.add(Obstacle::disk(p.xy, PAD_R, 1 << BOT, u32::MAX));
    }
    // Terminal vias of routed nets exist on both layers.
    for (ri, member, layer, _) in &raw {
        let r = &routables[*ri];
        let ks: Vec<usize> = match member {
            Some(k) => vec![*k],
            None => (0..r.members.len()).collect(),
        };
        for k in ks {
            let xy = term_via_xy(&r.members[k], *layer);
            world.add(Obstacle::disk(
                xy,
                r.class.via_pad_r(),
                0b11,
                owner_of(r.id, *member),
            ));
        }
    }
    // GND stitches are fixed copper.
    for (_, stub, xy) in &out.gnd_stitches {
        world.add(Obstacle::disk(*xy, 0.3, 0b11, u32::MAX));
        if stub.len() == 2 {
            world.add(Obstacle::capsule(
                stub[0],
                stub[1],
                0.125,
                1 << TOP,
                u32::MAX,
            ));
        }
    }
    for (_, stub, xy) in &out.gnd_land_stitches {
        world.add(Obstacle::disk(*xy, 0.3, 0b11, u32::MAX));
        if stub.len() == 2 {
            world.add(Obstacle::capsule(
                stub[0],
                stub[1],
                0.125,
                1 << BOT,
                u32::MAX,
            ));
        }
    }
    for xy in &out.gnd_field_stitches {
        world.add(Obstacle::disk(*xy, 0.3, 0b11, u32::MAX));
    }
    // Pair entry stubs are fixed copper: put each rail's stubs in the world
    // so every later clearance check sees them.
    for (ri, member, layer, pts) in &raw {
        let r = &routables[*ri];
        if member.is_some() || r.class != Class::Pair || pts.len() < 2 {
            continue;
        }
        let owner = owner_of(r.id, None);
        let n_s = unit(geom::sub(pts[0], r.src));
        let n_d = unit(geom::sub(pts[pts.len() - 1], r.dst));
        let (_, stubs_s) = pair_entry([r.members[0].2, r.members[1].2], n_s);
        let (_, stubs_d) = pair_entry([r.members[0].3, r.members[1].3], n_d);
        for end in [stubs_s, stubs_d] {
            for st in end {
                world.add(Obstacle::capsule(
                    st[0],
                    st[1],
                    USB_W / 2.0,
                    1 << *layer,
                    owner,
                ));
                world.add(Obstacle::capsule(
                    st[1],
                    st[2],
                    USB_W / 2.0,
                    1 << *layer,
                    owner,
                ));
            }
        }
    }
    let mut net_obstacles: HashMap<u32, Vec<u32>> = HashMap::new();
    let add_net = |world: &mut World,
                   net_obstacles: &mut HashMap<u32, Vec<u32>>,
                   owner: u32,
                   half: f64,
                   layer: u8,
                   pts: &[P]| {
        let mut ids = Vec::new();
        for w in pts.windows(2) {
            ids.push(world.add(Obstacle::capsule(w[0], w[1], half, 1 << layer, owner)));
        }
        net_obstacles.insert(owner, ids);
    };
    // Per raw entry: (owner id, copper half-width, clearance). Loose pair
    // halves are individual thin traces, not ribbons.
    let params = |ri: usize, member: Option<usize>| -> (u32, f64, f64) {
        let r = &routables[ri];
        if member.is_some() {
            (owner_of(r.id, member), USB_W / 2.0, Class::Ls.clear())
        } else {
            (owner_of(r.id, None), r.class.half(), r.class.clear())
        }
    };
    for (ri, member, layer, pts) in &raw {
        let (owner, half, _) = params(*ri, *member);
        add_net(&mut world, &mut net_obstacles, owner, half, *layer, pts);
    }

    // Two global smoothing rounds: the second sees everyone smoothed.
    type SmoothKey = (usize, Option<usize>);
    let mut smoothed: HashMap<SmoothKey, Vec<P>> = raw
        .iter()
        .map(|(ri, m, _, pts)| ((*ri, *m), pts.clone()))
        .collect();
    for _round in 0..2 {
        for (ri, member, layer, _) in &raw {
            let (owner, half, clear) = params(*ri, *member);
            let pts = smoothed[&(*ri, *member)].clone();
            // Deactivate own geometry while smoothing.
            if let Some(ids) = net_obstacles.get(&owner) {
                for &id in ids {
                    world.obstacles[id as usize].layers = 0;
                }
            }
            // Pair trunks keep their first and last segments: the entry
            // stubs depend on the trunk's departure directions.
            let is_pair = member.is_none() && routables[*ri].class == Class::Pair;
            let mut short = if is_pair && pts.len() >= 4 {
                let inner = shortcut_run(
                    &world,
                    &pts[1..pts.len() - 1],
                    half,
                    clear,
                    1 << *layer,
                    owner,
                );
                let mut v = vec![pts[0]];
                v.extend(inner);
                v.push(pts[pts.len() - 1]);
                v
            } else {
                shortcut_run(&world, &pts, half, clear, 1 << *layer, owner)
            };
            // A* only ever turns ≤90°, which is what keeps the offset rails
            // inside the trunk envelope and un-crossed. Shortcutting can
            // manufacture sharper corners (even full reversals into the
            // fixed leads) — and, because the trunk's own stubs share its
            // owner, it can also slide the trunk over its own entry copper.
            // A pair trunk that lost either invariant keeps its raw path.
            if is_pair {
                let r = &routables[*ri];
                let turns_ok = |v: &[P]| {
                    v.windows(3).all(|w| {
                        let v1 = geom::sub(w[1], w[0]);
                        let v2 = geom::sub(w[2], w[1]);
                        v1[0] * v2[0] + v1[1] * v2[1] >= -1e-9
                    })
                };
                let stubs_ok = |v: &[P]| {
                    let n_s = unit(geom::sub(v[0], r.src));
                    let n_d = unit(geom::sub(v[v.len() - 1], r.dst));
                    let (_, ss) = pair_entry([r.members[0].2, r.members[1].2], n_s);
                    let (_, sd) = pair_entry([r.members[0].3, r.members[1].3], n_d);
                    let need = Class::Pair.half() + Class::Pair.clear() + USB_W / 2.0;
                    ss.iter().chain(sd.iter()).all(|st| {
                        v.windows(2)
                            .all(|w| geom::dist_seg_seg(w[0], w[1], st[0], st[1]) >= need - 1e-9)
                    })
                };
                if !turns_ok(&short) || !stubs_ok(&short) {
                    short = pts.clone();
                }
            }
            add_net(&mut world, &mut net_obstacles, owner, half, *layer, &short);
            smoothed.insert((*ri, *member), short);
        }
    }

    // ---------------- emit traces ----------------
    for (ri, member, layer, _) in &raw {
        let r = &routables[*ri];
        let pts = &smoothed[&(*ri, *member)];
        if let Some(k) = member {
            // Loose pair half: an ordinary thin trace.
            let m = &r.members[*k];
            out.traces.push(Trace {
                contact: m.0,
                kind: r.kind,
                board: r.board,
                length_mm: geom::polyline_len(pts),
                points: pts.clone(),
                via_pattern: false,
                layer_of: vec![*layer; pts.len()],
                vias: Vec::new(),
                term_via: Some(term_via_xy(m, *layer)),
                width: USB_W,
            });
            continue;
        }
        match r.class {
            Class::Pair => {
                let center = pts.clone();
                // Reconstruct the entry stubs the trunk was routed against.
                let n_s = unit(geom::sub(center[0], r.src));
                let n_d = unit(geom::sub(center[center.len() - 1], r.dst));
                let (_, stubs_s) = pair_entry([r.members[0].2, r.members[1].2], n_s);
                let (_, stubs_d) = pair_entry([r.members[0].3, r.members[1].3], n_d);
                let off = RAIL_GAP / 2.0;
                let left = geom::offset_polyline(&center, off);
                let right = geom::offset_polyline(&center, -off);
                // DP is on the left of travel iff its src pad sits left of
                // the gateway normal (the same side test the candidate
                // selection used — never the snapped first grid segment,
                // whose angle error can flip the sign).
                let u_dp = unit(geom::sub(r.members[0].2, r.src));
                let dp_left = cross(n_s, u_dp) > 0.0;
                let (dp_line, dm_line) = if dp_left {
                    (left, right)
                } else {
                    (right, left)
                };
                // Rail = pad → knee (straight out) → offset trunk (its first
                // point plays the anchor) → knee → pad.
                let mut fulls: Vec<Vec<P>> = Vec::new();
                for (k, line) in [(0usize, &dp_line), (1, &dm_line)] {
                    let mut full = Vec::with_capacity(line.len() + 4);
                    full.push(stubs_s[k][0]);
                    full.push(stubs_s[k][1]);
                    full.extend(line.iter().copied());
                    full.push(stubs_d[k][1]);
                    full.push(stubs_d[k][0]);
                    fulls.push(full);
                }
                // No length tuning: the rails are parallel by construction,
                // so the residual skew is a couple of corner miters. It is
                // measured and reported (UΔ), not meandered away.
                for (k, full) in fulls.into_iter().enumerate() {
                    let m = &r.members[k];
                    out.traces.push(Trace {
                        contact: m.0,
                        kind: r.kind,
                        board: r.board,
                        length_mm: geom::polyline_len(&full),
                        layer_of: vec![*layer; full.len()],
                        points: full,
                        via_pattern: false,
                        vias: Vec::new(),
                        term_via: Some(term_via_xy(m, *layer)),
                        width: USB_W,
                    });
                }
            }
            _ => {
                let m = &r.members[0];
                out.traces.push(Trace {
                    contact: m.0,
                    kind: r.kind,
                    board: r.board,
                    length_mm: geom::polyline_len(pts),
                    points: pts.clone(),
                    via_pattern: false,
                    layer_of: vec![*layer; pts.len()],
                    vias: Vec::new(),
                    term_via: Some(term_via_xy(m, *layer)),
                    width: r.class.width(),
                });
            }
        }
    }

    // ---------------- self-DRC ----------------
    let mut drc = 0usize;
    let mut min_gap = f64::INFINITY;
    for (ri, member, layer, _) in &raw {
        let r = &routables[*ri];
        let (owner, half, clear) = params(*ri, *member);
        let pts = &smoothed[&(*ri, *member)];
        if let Some(ids) = net_obstacles.get(&owner) {
            for &id in ids {
                world.obstacles[id as usize].layers = 0;
            }
        }
        let term_pads: Vec<(P, P)> = match member {
            Some(k) => vec![(r.members[*k].2, r.members[*k].3)],
            None => r.members.iter().map(|(_, _, c, p)| (*c, *p)).collect(),
        };
        for w in pts.windows(2) {
            let (gap, hit) = world.seg_min_gap_with(w[0], w[1], half, 1 << *layer, owner, 5.0);
            // Own terminals legitimately touch their pads; ignore near-terminal hits.
            let near_term = term_pads.iter().any(|(c, p)| {
                geom::dist_point_seg(*c, w[0], w[1]) < PAD_R + half + clear
                    || geom::dist_point_seg(*p, w[0], w[1]) < PAD_R + half + clear
            });
            if gap < clear - 1e-6 && !near_term {
                drc += 1;
                if std::env::var_os("INTERPOSER_DRC_DEBUG").is_some() {
                    let vs = hit
                        .map(|id| {
                            let ob = &world.obstacles[id as usize];
                            format!(
                                "owner {} r {:.2} at ({:.1},{:.1})→({:.1},{:.1})",
                                ob.owner, ob.r, ob.a[0], ob.a[1], ob.b[0], ob.b[1]
                            )
                        })
                        .unwrap_or_default();
                    eprintln!(
                        "DRC {:?} net {} z{layer} seg ({:.1},{:.1})→({:.1},{:.1}) gap {:.3} vs {vs}",
                        r.kind, r.id, w[0][0], w[0][1], w[1][0], w[1][1], gap
                    );
                }
            }
            if !near_term && gap < min_gap {
                min_gap = gap;
            }
        }
        add_net(&mut world, &mut net_obstacles, owner, half, *layer, pts);
    }
    out.drc_violations = drc;
    out.min_gap_mm = if min_gap.is_finite() { min_gap } else { 0.0 };
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundle::bundle;
    use crate::pattern::{PITCH_254, PatternKind, attach_pattern, generate_pattern};
    use crate::types::{BoardId, Contact, ContactId, Ict};

    fn c(id: u32, board: u32, xy: P, ict: Ict) -> Contact {
        Contact {
            id: ContactId(id),
            board: BoardId(board),
            xy,
            ict,
            path: format!("c{id}"),
            package: "TestPoint_Pad_D1.0mm".into(),
            side: "bottom".into(),
        }
    }

    #[test]
    fn routes_a_small_panel_cleanly() {
        let cs = vec![
            c(0, 0, [40.0, 60.0], Ict::UsbDp),
            c(1, 0, [41.5, 60.0], Ict::UsbDm),
            c(2, 0, [40.0, 63.0], Ict::Vtarget),
            c(3, 0, [30.0, 70.0], Ict::Gnd),
            c(4, 0, [32.0, 70.0], Ict::Swdio),
        ];
        let mut p = bundle(cs).unwrap();
        attach_pattern(&mut p, &generate_pattern(PatternKind::S10, PITCH_254));
        crate::hall(&p).unwrap();
        let a = crate::assign(&p);
        let r = route_r5(74.0, 105.0, &p, &a);
        assert!(r.failed.is_empty(), "failed: {:?}", r.failed);
        // USB pair became two traces of near-equal length.
        let usb: Vec<_> = r.traces.iter().filter(|t| t.kind == Kind::UsbHs).collect();
        assert_eq!(usb.len(), 2);
        let mismatch = (usb[0].length_mm - usb[1].length_mm).abs();
        assert!(mismatch < 1.5, "pair mismatch {mismatch}");
        // Power trace is fat.
        assert!(
            r.traces
                .iter()
                .any(|t| t.kind == Kind::Vtarget && t.width >= POWER_W - 1e-9)
        );
        // No mid-route vias anywhere; every trace has exactly one terminal via.
        assert!(r.traces.iter().all(|t| t.vias.is_empty()));
        assert!(r.traces.iter().all(|t| t.term_via.is_some()));
        // Each trace lives on exactly one layer.
        for t in &r.traces {
            assert!(t.layer_of.windows(2).all(|w| w[0] == w[1]));
        }
        assert_eq!(r.drc_violations, 0, "DRC violations");
    }
}
