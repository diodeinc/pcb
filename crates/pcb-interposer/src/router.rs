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

const PITCH: f64 = 0.4;
const PAD_R: f64 = 0.5;
const HOLE_R: f64 = 1.6;
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
const TURN_45: i32 = 6;
const TURN_90: i32 = 30;
const EXPAND_CAP: u32 = 500_000;
/// Extra stamp radius: grid segments run between cell centers, so a chord can
/// sag up to ~half a diagonal below the sampled circle.
const GRID_SLOP: f64 = 0.08;
/// Length of the reserved entry corridor in front of each USB kit.
const GATEWAY_LEN: f64 = 3.0;

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

/// Gateway of a USB pad pair: (midpoint, outward unit normal). Outward is
/// the side of the pair axis facing away from the A7 interior.
fn gateway(a: P, b: P) -> (P, P) {
    let m = mid(a, b);
    let n = perp(unit(geom::sub(a, m)));
    let outward = geom::sub(m, [crate::pattern::A7_W / 2.0, crate::pattern::A7_H / 2.0]);
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
    dynamic: Vec<[Vec<bool>; 2]>,
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
                .map(|_| [vec![false; n], vec![false; n]])
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

    /// Stamp a disk obstacle of copper radius `r` for every class map.
    fn stamp_disk_all(&mut self, c: P, r: f64, layers: u8, dynamic: bool) {
        for class in CLASSES {
            let rr = r + class.clear() + class.half() + GRID_SLOP;
            self.stamp_disk(class, c, rr, layers, dynamic);
        }
    }

    fn stamp_disk(&mut self, class: Class, c: P, r_mm: f64, layers: u8, dynamic: bool) {
        // Distances are measured from the true center, not the rounded cell,
        // so protection is not eroded by up to half a diagonal.
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
                if let Some(i) = self.idx(cx + dx, cy + dy) {
                    for z in 0..2u8 {
                        if layers & (1 << z) != 0 {
                            if dynamic {
                                self.dynamic[class.idx()][z as usize][i] = true;
                            } else {
                                self.stat[class.idx()][z as usize][i] = true;
                            }
                        }
                    }
                }
            }
        }
    }

    /// Commit a routed centerline on `layer` so later nets keep clearance.
    fn stamp_route(&mut self, class_a: Class, layer: u8, cells: &[(i32, i32)]) {
        for probe in CLASSES {
            let r = class_a.half() + class_a.clear().max(probe.clear()) + probe.half() + GRID_SLOP;
            let rad = (r / PITCH).ceil() as i32;
            let r2 = (r / PITCH) * (r / PITCH);
            for &(x, y) in cells {
                for dy in -rad..=rad {
                    for dx in -rad..=rad {
                        if (dx * dx + dy * dy) as f64 > r2 {
                            continue;
                        }
                        if let Some(i) = self.idx(x + dx, y + dy) {
                            self.dynamic[probe.idx()][layer as usize][i] = true;
                        }
                    }
                }
            }
        }
    }

    /// Commit an off-grid segment (e.g. a pair tail or gateway) as blockage.
    fn stamp_seg(&mut self, class_a: Class, a: P, b: P, layers: u8, dynamic: bool) {
        let steps = (geom::dist(a, b) / (PITCH * 0.5)).ceil().max(1.0) as usize;
        for probe in CLASSES {
            let r = class_a.half() + class_a.clear().max(probe.clear()) + probe.half() + GRID_SLOP;
            for i in 0..=steps {
                let t = i as f64 / steps as f64;
                let p = [a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1])];
                self.stamp_disk(probe, p, r, layers, dynamic);
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
            if self.dynamic[class.idx()][layer as usize][idx] {
                return false;
            }
            if self.stat[class.idx()][layer as usize][idx] && !exempt.contains(cx, cy, layer) {
                return false;
            }
        }
        true
    }

    /// Is committed copper already too close for a terminal via at `xy`?
    /// A free cell in the dilated dynamic map proves copper is at least
    /// (their_half + clear + class.half()) away from that cell center; a via
    /// barrel needs (their_half + clear + via_pad_r) from `xy`, so probe the
    /// cells within the extra ring — and only those.
    fn via_dyn_ok(&self, class: Class, xy: P) -> bool {
        let extra = (class.via_pad_r() - class.half()).max(0.0) + PITCH * 0.72;
        let (cx, cy) = Self::cell(xy);
        let rad = (extra / PITCH).ceil() as i32;
        for z in 0..2usize {
            for dy in -rad..=rad {
                for dx in -rad..=rad {
                    let px = (cx + dx) as f64 * PITCH - xy[0];
                    let py = (cy + dy) as f64 * PITCH - xy[1];
                    if px * px + py * py > extra * extra {
                        continue;
                    }
                    if let Some(i) = self.idx(cx + dx, cy + dy)
                        && self.dynamic[class.idx()][z][i]
                    {
                        return false;
                    }
                }
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
            Some(i) => !dynamic[i] && (!stat[i] || exempt.contains(x, y, layer)),
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
        if x == t.0 && y == t.1 && end_dir.is_none_or(|e| dir != NO_DIR && dir_delta(dir, e) <= 2) {
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
            let ng = g + step + turn;
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

/// Greedy line-of-sight shortcutting of one polyline on one layer.
fn shortcut_run(world: &World, pts: &[P], half: f64, clear: f64, layers: u8, owner: u32) -> Vec<P> {
    if pts.len() <= 2 {
        return pts.to_vec();
    }
    let mut out = vec![pts[0]];
    let mut i = 0usize;
    while i + 1 < pts.len() {
        let mut j = pts.len() - 1;
        while j > i + 1 {
            if world.seg_clear(pts[i], pts[j], half, layers, clear, owner) {
                break;
            }
            j -= 1;
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
// Length matching
// ---------------------------------------------------------------------------

/// Insert one 45°-chamfered trapezoid meander (KiCad's chamfer style) into a
/// pair rail, adding `extra` mm of length. The bump points away from the
/// ribbon centerline and its segments are clearance-checked. Returns true
/// when a bump fit.
fn add_bump(
    world: &World,
    owner: u32,
    line: &mut Vec<P>,
    center: &[P],
    extra: f64,
    layer: u8,
) -> bool {
    if line.len() < 4 {
        return false;
    }
    // 45° ramps: amplitude from required extra length, 2·A·(√2−1) per bump.
    let amp = extra / (2.0 * (std::f64::consts::SQRT_2 - 1.0));
    if amp > 4.0 {
        return false;
    }
    let mut segs: Vec<usize> = (1..line.len() - 2).collect();
    segs.sort_by(|&i, &j| {
        dist(line[j], line[j + 1])
            .partial_cmp(&dist(line[i], line[i + 1]))
            .unwrap_or(Ordering::Equal)
    });
    for &i in segs.iter().take(10) {
        let a = line[i];
        let b = line[i + 1];
        let l = dist(a, b);
        let w = (l * 0.8).min(2.0 * amp + 2.0);
        let top = w - 2.0 * amp;
        if top < 1.0 {
            continue;
        }
        let m = mid(a, b);
        let dir = unit(geom::sub(b, a));
        let near = center
            .iter()
            .min_by(|x, y| {
                dist(**x, m)
                    .partial_cmp(&dist(**y, m))
                    .unwrap_or(Ordering::Equal)
            })
            .copied()
            .unwrap_or(m);
        let n0 = perp(dir);
        let to_out = geom::sub(m, near);
        let out_dir = if n0[0] * to_out[0] + n0[1] * to_out[1] >= 0.0 {
            n0
        } else {
            [-n0[0], -n0[1]]
        };
        let p1 = add_scaled(m, dir, -w / 2.0);
        let p2 = add_scaled(m, dir, w / 2.0);
        let t1 = add_scaled(add_scaled(p1, dir, amp), out_dir, amp);
        let t2 = add_scaled(t1, dir, top);
        if world.seg_clear(p1, t1, USB_W / 2.0, 1 << layer, 0.25, owner)
            && world.seg_clear(t1, t2, USB_W / 2.0, 1 << layer, 0.25, owner)
            && world.seg_clear(t2, p2, USB_W / 2.0, 1 << layer, 0.25, owner)
        {
            line.splice(i + 1..i + 1, [p1, t1, t2, p2]);
            return true;
        }
    }
    false
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

fn tooling_holes(w: f64, h: f64) -> Vec<P> {
    let mut holes = vec![
        // A7 mate tooling on the bottom mate, origin corner.
        [2.5, 2.5],
        [71.5, 2.5],
        [2.5, 102.5],
        [71.5, 102.5],
    ];
    if w > 80.0 || h > 110.0 {
        // Panel tooling at the sheet corners for larger sheets.
        holes.extend([
            [3.0, 3.0],
            [w - 3.0, 3.0],
            [3.0, h - 3.0],
            [w - 3.0, h - 3.0],
        ]);
    }
    holes
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

/// One full greedy grid pass in the given order. Returns raw centerlines,
/// the routables that found no path, and how many pairs went loose.
#[allow(clippy::too_many_arguments)]
fn grid_pass(
    sheet_w: f64,
    sheet_h: f64,
    problem: &Problem,
    routables: &[Routable],
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
    for hxy in tooling_holes(sheet_w, sheet_h) {
        maps.stamp_disk_all(hxy, HOLE_R, 0b11, false);
    }
    // Pogo pads are SMT on top; mate lands are SMT on bottom.
    for c in problem.contacts.values() {
        maps.stamp_disk_all(c.xy, PAD_R, 1 << TOP, false);
    }
    for p in problem.pins.values() {
        maps.stamp_disk_all(p.xy, PAD_R, 1 << BOT, false);
    }
    // Reserve the via spot above every *assigned* land: a top run must be
    // able to dive there no matter what routes earlier. (The owner's own
    // exemption disks cover its own reservation.)
    for r in routables {
        for m in &r.members {
            maps.stamp_disk_all(m.3, r.class.via_pad_r(), 1 << TOP, false);
        }
    }
    // Reserve a gateway in front of every USB kit on both layers so no net
    // parks across a pair's forced entry corridor.
    for slot in problem.slots.values() {
        if slot.kind != Kind::UsbHs || slot.pins.len() != 2 {
            continue;
        }
        let a = problem.pins[&slot.pins[0]].xy;
        let b = problem.pins[&slot.pins[1]].xy;
        let (m, n) = gateway(a, b);
        maps.stamp_seg(Class::Pair, m, add_scaled(m, n, GATEWAY_LEN), 0b11, false);
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

    // A terminal via is legal when no *foreign* pad on the via's blind side
    // sits within pad + clearance of the barrel.
    let via_legal = |member: &(ContactId, MatePinId, P, P), class: Class, layer: u8| -> bool {
        // The barrel exists on both layers: keep clearance from every
        // foreign pad regardless of side.
        let xy = term_via_xy(member, layer);
        let need = class.via_pad_r() + class.clear() + PAD_R;
        all_pads
            .iter()
            .filter(|(p, _)| dist(*p, member.2) > 1e-6 && dist(*p, member.3) > 1e-6)
            .all(|(p, _)| dist(*p, xy) >= need)
    };

    // Commit the terminal vias of a routed net: the barrel blocks both layers.
    let stamp_vias = |maps: &mut Maps, r: &Routable, members: &[usize], layer: u8| {
        for &k in members {
            let xy = term_via_xy(&r.members[k], layer);
            maps.stamp_disk_all(xy, r.class.via_pad_r(), 0b11, true);
        }
    };

    let mut raw: Vec<RawPath> = Vec::new();
    let mut failed = Vec::new();
    let mut loose = 0usize;
    for &ri in order {
        let r = &routables[ri];
        let own: Vec<(P, u8)> = r
            .members
            .iter()
            .flat_map(|(_, _, c, p)| [(*c, 1 << TOP), (*p, 1 << BOT)])
            .collect();
        // A net's own terminal vias also live in its exemption: the via pad
        // exists on both layers at the pogo and land.
        let own_both: Vec<(P, u8)> = r
            .members
            .iter()
            .flat_map(|(_, _, c, p)| [(*c, 0b11u8), (*p, 0b11u8)])
            .collect();
        let mut exempt = make_exempt(&own_both, r.class);
        let _ = &own;

        // Candidates: (layer, trunk src, trunk dst, pair entry stubs).
        type Stubs = [[[P; 3]; 2]; 2];
        type Cand = (u8, P, P, Option<Stubs>);
        let mut candidates: Vec<Cand> = Vec::new();
        match r.class {
            Class::Pair => {
                let u_s = unit(geom::sub(r.members[0].2, r.src));
                let u_d = unit(geom::sub(r.members[0].3, r.dst));
                let (_, n_d_out) = gateway(r.members[0].3, r.members[1].3);
                // The pair may use its own reserved gateway, and — as a
                // fallback — the mirrored inward corridor through the band.
                for i in -6..=6i32 {
                    let p = add_scaled(r.dst, n_d_out, GATEWAY_LEN * i as f64 / 6.0);
                    exempt.disks.push((p, 1.0, 0b11));
                }
                for layer in [TOP, BOT] {
                    let ok = r.members.iter().all(|m| {
                        via_legal(m, Class::Pair, layer)
                            && maps.via_dyn_ok(Class::Pair, term_via_xy(m, layer))
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
                        // Side of DP at the dst end (travel direction is -n_d).
                        let side_d = cross([-n_d[0], -n_d[1]], u_d) > 0.0;
                        let n_s_raw = perp(u_s);
                        // Source waypoint side keeps DP on the same side.
                        let n_s = if (cross(n_s_raw, u_s) > 0.0) == side_d {
                            n_s_raw
                        } else {
                            [-n_s_raw[0], -n_s_raw[1]]
                        };
                        let (w_s, stubs_s) = pair_entry([r.members[0].2, r.members[1].2], n_s);
                        let (w_d, stubs_d) = pair_entry([r.members[0].3, r.members[1].3], n_d);
                        candidates.push((layer, w_s, w_d, Some([stubs_s, stubs_d])));
                    }
                }
            }
            Class::Power | Class::Ls => {
                for layer in [TOP, BOT] {
                    if via_legal(&r.members[0], r.class, layer)
                        && maps.via_dyn_ok(r.class, term_via_xy(&r.members[0], layer))
                    {
                        candidates.push((layer, r.src, r.dst, None));
                    }
                }
            }
        }

        // Route the best legal candidate: the cheaper layer wins after a
        // handicap. Pairs and power prefer the top (the bottom GND pour
        // stays whole); LS follows an H/V discipline — without mid-route
        // vias a long trace is a wall, so horizontal-dominant nets lean
        // top and vertical-dominant nets lean bottom, and crossing nets
        // land on opposite layers.
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
        // Score every candidate (all pair waypoint variants included — the
        // first workable one is not necessarily the clean one) and keep the
        // cheapest.
        type Found = (u8, Vec<(i32, i32)>, Option<Stubs>);
        let mut found: Option<Found> = None;
        let mut best_cost = i32::MAX;
        for cand in &candidates {
            let (layer, src, dst, stubs) = cand;
            let (layer, src, dst) = (*layer, *src, *dst);
            // Pair entry stubs are fixed geometry: reject candidates whose
            // stubs collide before spending an A* on them. The trunk must
            // also depart/arrive without kinking against the stubs.
            let mut dirs = (None, None);
            if let Some(stubs) = stubs {
                let clear_stubs = stubs.iter().all(|end| {
                    end.iter().all(|st| {
                        maps.seg_free(Class::Ls, layer, st[0], st[1], &exempt)
                            && maps.seg_free(Class::Ls, layer, st[1], st[2], &exempt)
                    })
                });
                if !clear_stubs {
                    continue;
                }
                dirs = (
                    Some(snap_dir(unit(geom::sub(src, r.src)))),
                    Some(snap_dir(unit(geom::sub(r.dst, dst)))),
                );
            }
            if let Some((cells, cost)) = astar(
                &maps, search, r.class, layer, src, dst, &exempt, dirs.0, dirs.1,
            ) {
                let eff = cost + handicap(layer);
                if eff < best_cost {
                    best_cost = eff;
                    found = Some((layer, cells, *stubs));
                }
            }
        }
        match found {
            Some((layer, cells, stubs)) => {
                maps.stamp_route(r.class, layer, &cells);
                let pts = collapse(&cells);
                if let Some(stubs) = stubs {
                    // The entry stubs are fixed off-grid copper: stamp each
                    // rail's two stub segments so later nets keep out.
                    for end in &stubs {
                        for st in end {
                            maps.stamp_seg(Class::Ls, st[0], st[1], 1 << layer, true);
                            maps.stamp_seg(Class::Ls, st[1], st[2], 1 << layer, true);
                        }
                    }
                    stamp_vias(&mut maps, r, &[0, 1], layer);
                } else {
                    stamp_vias(&mut maps, r, &[0], layer);
                }
                raw.push((ri, None, layer, pts));
            }
            None if r.class == Class::Pair => {
                // Loose fallback: no legal ribbon exists. Route DP and DM as
                // two individual traces (either layer) and report the pair
                // as loose.
                let mut halves = Vec::new();
                for (k, m) in r.members.iter().enumerate() {
                    let (_, _, cxy, pxy) = m;
                    let ex = make_exempt(&[(*cxy, 0b11), (*pxy, 0b11)], Class::Ls);
                    let mut got = None;
                    for layer in [TOP, BOT] {
                        if !via_legal(m, Class::Ls, layer)
                            || !maps.via_dyn_ok(Class::Ls, term_via_xy(m, layer))
                        {
                            continue;
                        }
                        if let Some((cells, _)) =
                            astar(&maps, search, Class::Ls, layer, *cxy, *pxy, &ex, None, None)
                        {
                            maps.stamp_route(Class::Ls, layer, &cells);
                            maps.stamp_disk_all(term_via_xy(m, layer), 0.3, 0b11, true);
                            got = Some((ri, Some(k), layer, collapse(&cells)));
                            break;
                        }
                    }
                    match got {
                        Some(h) => halves.push(h),
                        None => break,
                    }
                }
                if halves.len() == 2 {
                    loose += 1;
                    raw.extend(halves);
                } else {
                    // A committed first half stays stamped; harmless and rare.
                    failed.push(ri);
                }
            }
            None => {
                if std::env::var_os("INTERPOSER_DRC_DEBUG").is_some() {
                    let m = &r.members[0];
                    eprintln!(
                        "FAIL {:?} net {} src ({:.1},{:.1}) dst ({:.1},{:.1}) cands {} viaT {}/{} viaB {}/{}",
                        r.kind,
                        r.id,
                        r.src[0],
                        r.src[1],
                        r.dst[0],
                        r.dst[1],
                        candidates.len(),
                        via_legal(m, r.class, TOP),
                        maps.via_dyn_ok(r.class, term_via_xy(m, TOP)),
                        via_legal(m, r.class, BOT),
                        maps.via_dyn_ok(r.class, term_via_xy(m, BOT)),
                    );
                }
                failed.push(ri)
            }
        }
    }
    (raw, failed, loose)
}

pub fn route_r5(sheet_w: f64, sheet_h: f64, problem: &Problem, assign: &Assign) -> RouteResult {
    let (routables, poured) = build_routables(problem, assign);

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
    let mut best: Option<(Vec<RawPath>, Vec<usize>, usize)> = None;
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
            &routables,
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
            Some((_, bf, bl)) => (n_failed, loose) < (bf.len(), *bl),
        };
        if better {
            best = Some((raw, failed, loose));
        }
        if n_failed == 0 || !grew {
            break;
        }
    }
    let (raw, failed_ris, loose_pairs) = best.unwrap_or_default();
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

    // ---------------- smoothing world ----------------
    let mut world = World::new(sheet_w, sheet_h, EDGE_MARGIN);
    for hxy in tooling_holes(sheet_w, sheet_h) {
        world.add(Obstacle::disk(hxy, HOLE_R, 0b11, u32::MAX));
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
            let short = if is_pair && pts.len() >= 4 {
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
                // DP is on the left of travel iff its src pad is.
                let d0 = unit(geom::sub(center[1], center[0]));
                let to_dp = geom::sub(r.members[0].2, center[0]);
                let dp_left = cross(d0, to_dp) > 0.0;
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
                // Intra-pair length matching: bump the shorter rail. When one
                // big bump does not fit, spread it over smaller ones.
                let owner = owner_of(r.id, None);
                let mut remaining = geom::polyline_len(&fulls[0]) - geom::polyline_len(&fulls[1]);
                let mut bumped = false;
                let mut attempts = 0;
                // Match to a 0.25 mm budget with as few meanders as possible
                // (the intra-pair spec budget is far looser than that).
                while remaining.abs() > 0.25 && attempts < 3 {
                    attempts += 1;
                    let shorter = if remaining > 0.0 { 1 } else { 0 };
                    let mut chunk = remaining.abs();
                    let mut ok = false;
                    while chunk > 0.25 {
                        if add_bump(&world, owner, &mut fulls[shorter], &center, chunk, *layer) {
                            ok = true;
                            bumped = true;
                            break;
                        }
                        chunk /= 2.0;
                    }
                    if !ok {
                        break;
                    }
                    remaining = geom::polyline_len(&fulls[0]) - geom::polyline_len(&fulls[1]);
                }
                if bumped {
                    // Keep later checks honest: the bumps are new copper.
                    for line in &fulls {
                        for w in line.windows(2) {
                            world.add(Obstacle::capsule(
                                w[0],
                                w[1],
                                USB_W / 2.0,
                                1 << *layer,
                                owner,
                            ));
                        }
                    }
                }
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
        assert!(mismatch < 0.5, "pair mismatch {mismatch}");
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
