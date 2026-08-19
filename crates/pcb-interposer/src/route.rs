//! Single-layer PTH routers: R1 maze, R2 street-pattern+maze, R3 board-planarize+river.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};

use crate::types::{Assign, BoardId, ContactId, Kind, MatePinId, Problem};

const PITCH: f64 = 0.4;
const STREET: f64 = 2.54;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RouterKind {
    /// Sequential A* maze.
    R1,
    /// L/Z / street-jog, then A*.
    R2,
    /// Drop up to 2 boards to uncross, river-order, then R2.
    R3,
    /// 2-layer preferred-direction A* with vias, USB pair bias, fat power.
    R4,
}

impl RouterKind {
    pub fn all() -> [RouterKind; 4] {
        [Self::R1, Self::R2, Self::R3, Self::R4]
    }

    /// Primary eval set: 2-layer first, keep R2 as a clean single-layer control.
    pub fn eval() -> [RouterKind; 2] {
        [Self::R4, Self::R2]
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::R1 => "R1",
            Self::R2 => "R2",
            Self::R3 => "R3",
            Self::R4 => "R4",
        }
    }
}

#[derive(Debug, Clone)]
pub struct TwoPinNet {
    pub contact: ContactId,
    pub pin: MatePinId,
    pub kind: Kind,
    pub board: BoardId,
    pub src: [f64; 2],
    pub dst: [f64; 2],
    pub width: f64,
}

#[derive(Debug, Clone)]
pub struct Trace {
    pub contact: ContactId,
    pub kind: Kind,
    pub board: BoardId,
    pub points: Vec<[f64; 2]>,
    pub length_mm: f64,
    pub via_pattern: bool,
    /// Layer index per point (0 = bottom, 1 = top). Empty means all bottom.
    pub layer_of: Vec<u8>,
    pub vias: Vec<[f64; 2]>,
}

#[derive(Debug, Clone, Default)]
pub struct RouteResult {
    pub traces: Vec<Trace>,
    pub failed: Vec<TwoPinNet>,
    pub order: Vec<Kind>,
    pub dropped_boards: Vec<BoardId>,
    pub router: String,
    /// GND contacts kept via pour (not maze traces).
    pub poured: Vec<ContactId>,
}

impl RouteResult {
    pub fn routed_of(&self, kind: Kind) -> usize {
        self.traces.iter().filter(|t| t.kind == kind).count()
    }

    pub fn maze_traces(&self) -> impl Iterator<Item = &Trace> {
        self.traces.iter().filter(|t| t.kind != Kind::Gnd)
    }

    pub fn boards_complete(&self, nets: &[TwoPinNet]) -> usize {
        let mut need: HashMap<BoardId, usize> = HashMap::new();
        let mut got: HashMap<BoardId, usize> = HashMap::new();
        let poured: HashSet<ContactId> = self.poured.iter().copied().collect();
        for n in nets {
            *need.entry(n.board).or_default() += 1;
        }
        for t in &self.traces {
            *got.entry(t.board).or_default() += 1;
        }
        for n in nets {
            if n.kind == Kind::Gnd && poured.contains(&n.contact) {
                *got.entry(n.board).or_default() += 1;
            }
        }
        need.iter()
            .filter(|(b, n)| got.get(b).copied().unwrap_or(0) == **n)
            .count()
    }
}

pub fn nets_from_assign(problem: &Problem, assign: &Assign) -> Vec<TwoPinNet> {
    let mut nets = Vec::new();
    for (cid, pid) in &assign.contact_to_pin {
        let c = &problem.contacts[cid];
        let p = &problem.pins[pid];
        let Some(kind) = c.ict.kind() else {
            continue;
        };
        let width = match kind {
            Kind::UsbHs => 0.20,
            Kind::Vtarget | Kind::Vusb => 0.60,
            Kind::Gnd => 0.30,
            Kind::Ls => 0.18,
        };
        nets.push(TwoPinNet {
            contact: *cid,
            pin: *pid,
            kind,
            board: c.board,
            src: c.xy,
            dst: p.xy,
            width,
        });
    }
    nets.sort_by_key(|n| kind_pri(n.kind));
    nets
}

fn kind_pri(k: Kind) -> u8 {
    match k {
        Kind::UsbHs => 0,
        Kind::Vtarget => 1,
        Kind::Vusb => 2,
        Kind::Gnd => 3,
        Kind::Ls => 4,
    }
}

struct Grid {
    nx: i32,
    ny: i32,
    occ: Vec<u8>,
}

impl Grid {
    fn new(w: f64, h: f64) -> Self {
        let nx = (w / PITCH).ceil() as i32 + 3;
        let ny = (h / PITCH).ceil() as i32 + 3;
        Self {
            nx,
            ny,
            occ: vec![0; (nx * ny) as usize],
        }
    }

    fn idx(&self, x: i32, y: i32) -> Option<usize> {
        if x < 0 || y < 0 || x >= self.nx || y >= self.ny {
            None
        } else {
            Some((y * self.nx + x) as usize)
        }
    }

    fn cell(p: [f64; 2]) -> (i32, i32) {
        ((p[0] / PITCH).round() as i32, (p[1] / PITCH).round() as i32)
    }

    fn stamp_disk(&mut self, p: [f64; 2], r_mm: f64, val: u8) {
        let (cx, cy) = Self::cell(p);
        let rad = (r_mm / PITCH).ceil() as i32;
        for dy in -rad..=rad {
            for dx in -rad..=rad {
                if dx * dx + dy * dy <= rad * rad {
                    if let Some(i) = self.idx(cx + dx, cy + dy) {
                        self.occ[i] = val;
                    }
                }
            }
        }
    }

    fn stamp_path(&mut self, cells: &[(i32, i32)], half: i32) {
        for &(x, y) in cells {
            for dy in -half..=half {
                for dx in -half..=half {
                    if let Some(i) = self.idx(x + dx, y + dy) {
                        self.occ[i] = 1;
                    }
                }
            }
        }
    }
}

#[derive(Copy, Clone, Eq, PartialEq)]
struct Node {
    f: i32,
    g: i32,
    x: i32,
    y: i32,
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

fn half_w(width: f64) -> i32 {
    let r = width / 2.0 + 0.15;
    ((r / PITCH).ceil() as i32 - 1).max(0)
}

fn astar(grid: &Grid, src: [f64; 2], dst: [f64; 2], half: i32) -> Option<Vec<(i32, i32)>> {
    let s = Grid::cell(src);
    let t = Grid::cell(dst);
    let free = |x: i32, y: i32| -> bool {
        for dy in -half..=half {
            for dx in -half..=half {
                match grid.idx(x + dx, y + dy) {
                    None => return false,
                    Some(i)
                        if grid.occ[i] != 0 && (x + dx, y + dy) != s && (x + dx, y + dy) != t =>
                    {
                        return false;
                    }
                    _ => {}
                }
            }
        }
        true
    };
    if !free(s.0, s.1) || !free(t.0, t.1) {
        // still try; terminals may sit on their own stamp
    }
    let mut open = BinaryHeap::new();
    let h0 = (s.0 - t.0).abs() + (s.1 - t.1).abs();
    open.push(Node {
        f: h0,
        g: 0,
        x: s.0,
        y: s.1,
    });
    let mut gscore: HashMap<(i32, i32), i32> = HashMap::new();
    gscore.insert(s, 0);
    let mut came: HashMap<(i32, i32), (i32, i32)> = HashMap::new();
    let mut closed = std::collections::HashSet::new();
    const NEI: [(i32, i32); 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];
    let mut expanded = 0u32;
    while let Some(Node { g, x, y, .. }) = open.pop() {
        let u = (x, y);
        if !closed.insert(u) {
            continue;
        }
        expanded += 1;
        if expanded > 80_000 {
            return None;
        }
        if u == t {
            let mut path = vec![u];
            while let Some(p) = came.get(path.last().unwrap()) {
                path.push(*p);
            }
            path.reverse();
            return Some(path);
        }
        for (dx, dy) in NEI {
            let v = (x + dx, y + dy);
            if closed.contains(&v) || !free(v.0, v.1) {
                continue;
            }
            let ng = g + 1;
            if ng < *gscore.get(&v).unwrap_or(&i32::MAX) {
                gscore.insert(v, ng);
                came.insert(v, u);
                let h = (v.0 - t.0).abs() + (v.1 - t.1).abs();
                open.push(Node {
                    f: ng + h,
                    g: ng,
                    x: v.0,
                    y: v.1,
                });
            }
        }
    }
    None
}

fn walk_orth(a: (i32, i32), b: (i32, i32), horiz_first: bool) -> Vec<(i32, i32)> {
    let mut cells = vec![a];
    let (mut x, mut y) = a;
    let step = |from: i32, to: i32| -> i32 { if to > from { 1 } else { -1 } };
    if horiz_first {
        while x != b.0 {
            x += step(x, b.0);
            cells.push((x, y));
        }
        while y != b.1 {
            y += step(y, b.1);
            cells.push((x, y));
        }
    } else {
        while y != b.1 {
            y += step(y, b.1);
            cells.push((x, y));
        }
        while x != b.0 {
            x += step(x, b.0);
            cells.push((x, y));
        }
    }
    cells
}

fn concat_walk(pts: &[(i32, i32)]) -> Vec<(i32, i32)> {
    let mut out = Vec::new();
    for w in pts.windows(2) {
        let mut seg = walk_orth(w[0], w[1], true);
        if !out.is_empty() {
            seg.remove(0);
        }
        out.extend(seg);
    }
    out
}

fn corridor_free(
    grid: &Grid,
    cells: &[(i32, i32)],
    half: i32,
    s: (i32, i32),
    t: (i32, i32),
) -> bool {
    for &(x, y) in cells {
        for dy in -half..=half {
            for dx in -half..=half {
                let xx = x + dx;
                let yy = y + dy;
                match grid.idx(xx, yy) {
                    None => return false,
                    Some(i) if grid.occ[i] != 0 && (xx, yy) != s && (xx, yy) != t => {
                        return false;
                    }
                    _ => {}
                }
            }
        }
    }
    true
}

/// L, Z, then a street-jog snapped onto a 2.54 mm channel through the dest pad.
fn pattern_path(grid: &Grid, src: [f64; 2], dst: [f64; 2], half: i32) -> Option<Vec<(i32, i32)>> {
    let s = Grid::cell(src);
    let t = Grid::cell(dst);
    let street = (STREET / PITCH).round() as i32;
    let mx = (s.0 + t.0) / 2;
    let my = (s.1 + t.1) / 2;
    // Snap dest onto the lattice street just outside the pad.
    let jog = if (t.0 - s.0).abs() >= (t.1 - s.1).abs() {
        street
    } else {
        -street
    };
    let cands = [
        walk_orth(s, t, true),
        walk_orth(s, t, false),
        concat_walk(&[s, (mx, s.1), (mx, t.1), t]),
        concat_walk(&[s, (s.0, my), (t.0, my), t]),
        concat_walk(&[s, (t.0, s.1 + jog), (t.0, t.1), t]),
        concat_walk(&[s, (s.0 + jog, t.1), (t.0, t.1), t]),
    ];
    cands
        .into_iter()
        .find(|c| !c.is_empty() && corridor_free(grid, c, half, s, t))
}

fn segs_cross(a1: [f64; 2], a2: [f64; 2], b1: [f64; 2], b2: [f64; 2]) -> bool {
    fn orient(p: [f64; 2], q: [f64; 2], r: [f64; 2]) -> f64 {
        (q[0] - p[0]) * (r[1] - p[1]) - (q[1] - p[1]) * (r[0] - p[0])
    }
    let o1 = orient(a1, a2, b1);
    let o2 = orient(a1, a2, b2);
    let o3 = orient(b1, b2, a1);
    let o4 = orient(b1, b2, a2);
    o1 * o2 < -1e-9 && o3 * o4 < -1e-9
}

fn crossings_per_net(nets: &[&TwoPinNet]) -> Vec<usize> {
    let mut c = vec![0; nets.len()];
    for i in 0..nets.len() {
        for j in i + 1..nets.len() {
            if segs_cross(nets[i].src, nets[i].dst, nets[j].src, nets[j].dst) {
                c[i] += 1;
                c[j] += 1;
            }
        }
    }
    c
}

fn board_radius(nets: &[TwoPinNet], board: BoardId) -> f64 {
    let pts: Vec<[f64; 2]> = nets
        .iter()
        .filter(|n| n.board == board)
        .map(|n| n.src)
        .collect();
    if pts.is_empty() {
        return 0.0;
    }
    let cx = pts.iter().map(|p| p[0]).sum::<f64>() / pts.len() as f64;
    let cy = pts.iter().map(|p| p[1]).sum::<f64>() / pts.len() as f64;
    (cx * cx + cy * cy).sqrt()
}

/// Drop whole boards (not nets) until the sketch is planar or only `keep` remain.
pub fn drop_boards_until_planar(nets: &[TwoPinNet], keep: usize) -> (Vec<TwoPinNet>, Vec<BoardId>) {
    let mut alive: HashSet<BoardId> = nets.iter().map(|n| n.board).collect();
    let mut dropped = Vec::new();
    loop {
        if alive.len() <= keep.max(1) {
            break;
        }
        let kept: Vec<&TwoPinNet> = nets.iter().filter(|n| alive.contains(&n.board)).collect();
        let xs = crossings_per_net(&kept);
        if xs.iter().sum::<usize>() == 0 {
            break;
        }
        let mut score: HashMap<BoardId, usize> = HashMap::new();
        for (n, x) in kept.iter().zip(xs.iter()) {
            *score.entry(n.board).or_default() += *x;
        }
        let Some((&worst, &sc)) = score.iter().max_by_key(|(b, s)| (*s, b.0)) else {
            break;
        };
        if sc == 0 {
            break;
        }
        alive.remove(&worst);
        dropped.push(worst);
    }
    let kept = nets
        .iter()
        .filter(|n| alive.contains(&n.board))
        .cloned()
        .collect();
    (kept, dropped)
}

struct RouteOpts {
    use_pattern: bool,
    river_order: bool,
}

fn route_engine(sheet_w: f64, sheet_h: f64, nets: &[TwoPinNet], opts: RouteOpts) -> RouteResult {
    let mut grid = Grid::new(sheet_w, sheet_h);
    let mut order: Vec<&TwoPinNet> = nets.iter().filter(|n| n.kind != Kind::Gnd).collect();
    if opts.river_order {
        order.sort_by(|a, b| {
            kind_pri(a.kind).cmp(&kind_pri(b.kind)).then_with(|| {
                let aa = (a.src[1] - 50.0).atan2(a.src[0] - 32.0);
                let bb = (b.src[1] - 50.0).atan2(b.src[0] - 32.0);
                aa.partial_cmp(&bb).unwrap_or(Ordering::Equal)
            })
        });
    } else {
        order.sort_by_key(|n| (kind_pri(n.kind), n.board.0, n.contact.0));
    }
    let mut out = RouteResult {
        order: order.iter().map(|n| n.kind).collect(),
        ..RouteResult::default()
    };
    for n in nets {
        if n.kind == Kind::Gnd {
            // Poured return: not a maze trace, so it must not inflate nets_routed.
            out.poured.push(n.contact);
            continue;
        }
        grid.stamp_disk(n.src, 0.55, 1);
        grid.stamp_disk(n.dst, 0.55, 1);
    }
    for (hx, hy) in [(2.5, 2.5), (71.5, 2.5), (2.5, 102.5), (71.5, 102.5)] {
        grid.stamp_disk([hx, hy], 1.6, 1);
    }

    for net in order {
        grid.stamp_disk(net.src, 0.55, 0);
        grid.stamp_disk(net.dst, 0.55, 0);
        let hw = half_w(net.width);
        let mut via_pattern = false;
        let mut cells = None;
        if opts.use_pattern {
            if let Some(p) = pattern_path(&grid, net.src, net.dst, hw) {
                via_pattern = true;
                cells = Some(p);
            }
        }
        if cells.is_none() {
            cells = astar(&grid, net.src, net.dst, hw);
        }
        match cells {
            Some(cells) => {
                grid.stamp_path(&cells, hw + 1);
                let points: Vec<[f64; 2]> = cells
                    .iter()
                    .map(|&(x, y)| [x as f64 * PITCH, y as f64 * PITCH])
                    .collect();
                let length_mm = (cells.len().saturating_sub(1) as f64) * PITCH;
                let npts = points.len();
                out.traces.push(Trace {
                    contact: net.contact,
                    kind: net.kind,
                    board: net.board,
                    points,
                    length_mm,
                    via_pattern,
                    layer_of: vec![0; npts],
                    vias: vec![],
                });
            }
            None => {
                grid.stamp_disk(net.src, 0.55, 1);
                grid.stamp_disk(net.dst, 0.55, 1);
                out.failed.push(net.clone());
            }
        }
    }
    out
}

/// Route two-pin nets. GND is poured. USB/power before LS. Fail-open.
pub fn route(kind: RouterKind, sheet_w: f64, sheet_h: f64, nets: &[TwoPinNet]) -> RouteResult {
    match kind {
        RouterKind::R1 => {
            let mut r = route_engine(
                sheet_w,
                sheet_h,
                nets,
                RouteOpts {
                    use_pattern: false,
                    river_order: false,
                },
            );
            r.router = "R1".into();
            r
        }
        RouterKind::R2 => {
            let mut r = route_engine(
                sheet_w,
                sheet_h,
                nets,
                RouteOpts {
                    use_pattern: true,
                    river_order: false,
                },
            );
            r.router = "R2".into();
            r
        }
        RouterKind::R3 => {
            // Uncross by sketch first, then drop incomplete boards until the
            // leftover set fully routes (6-of-8, or fewer if the funnel is worse).
            let boards: HashSet<BoardId> = nets.iter().map(|n| n.board).collect();
            let keep = boards.len().saturating_sub(2).max(1);
            let (mut kept, mut dropped) = drop_boards_until_planar(nets, keep);
            let mut r;
            loop {
                r = route_engine(
                    sheet_w,
                    sheet_h,
                    &kept,
                    RouteOpts {
                        use_pattern: true,
                        river_order: true,
                    },
                );
                let failed_boards: HashSet<BoardId> = r.failed.iter().map(|n| n.board).collect();
                if failed_boards.is_empty() {
                    break;
                }
                let alive: HashSet<BoardId> = kept.iter().map(|n| n.board).collect();
                if alive.len() <= 1 {
                    break;
                }
                // Drop the incomplete board farthest from the A7 origin (funnel tail).
                let worst = *failed_boards
                    .iter()
                    .max_by(|a, b| {
                        let da = board_radius(nets, **a);
                        let db = board_radius(nets, **b);
                        da.partial_cmp(&db)
                            .unwrap_or(Ordering::Equal)
                            .then(a.0.cmp(&b.0))
                    })
                    .unwrap();
                dropped.push(worst);
                kept.retain(|n| n.board != worst);
            }
            let kept_ids: HashSet<ContactId> = kept.iter().map(|n| n.contact).collect();
            for n in nets {
                if !kept_ids.contains(&n.contact) && n.kind != Kind::Gnd {
                    if !r.failed.iter().any(|f| f.contact == n.contact) {
                        r.failed.push(n.clone());
                    }
                }
            }
            r.dropped_boards = dropped;
            r.router = "R3".into();
            r
        }
        RouterKind::R4 => crate::route_ml::route_r4(sheet_w, sheet_h, nets),
    }
}

pub fn route_r1(sheet_w: f64, sheet_h: f64, nets: &[TwoPinNet]) -> RouteResult {
    route(RouterKind::R1, sheet_w, sheet_h, nets)
}

pub fn route_r2(sheet_w: f64, sheet_h: f64, nets: &[TwoPinNet]) -> RouteResult {
    route(RouterKind::R2, sheet_w, sheet_h, nets)
}

pub fn route_r3(sheet_w: f64, sheet_h: f64, nets: &[TwoPinNet]) -> RouteResult {
    route(RouterKind::R3, sheet_w, sheet_h, nets)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::BoardId;

    fn net(kind: Kind, src: [f64; 2], dst: [f64; 2], id: u32) -> TwoPinNet {
        TwoPinNet {
            contact: ContactId(id),
            pin: MatePinId(id),
            kind,
            board: BoardId(0),
            src,
            dst,
            width: if matches!(kind, Kind::Vtarget) {
                0.6
            } else {
                0.2
            },
        }
    }

    #[test]
    fn usb_before_ls_and_blocked_ls_is_omitted() {
        let usb = net(Kind::UsbHs, [12.0, 20.0], [30.0, 20.0], 0);
        let ls_ok = net(Kind::Ls, [12.0, 26.0], [30.0, 26.0], 1);
        let ls_block = net(Kind::Ls, [12.0, 32.0], [30.0, 32.0], 2);
        let mut nets = vec![ls_ok.clone(), usb.clone(), ls_block.clone()];
        // paint a wall that only the last LS must cross, by putting a dummy routed barrier:
        // we simulate by placing src/dst of ls_block on opposite sides of a dense pad field
        // created by extra reserved terminals along y=14.
        let mut extras = Vec::new();
        for i in 0..12 {
            extras.push(net(
                Kind::Ls,
                [4.0 + i as f64 * 1.2, 14.0],
                [4.0 + i as f64 * 1.2, 14.0],
                10 + i,
            ));
        }
        // extras are degenerate (src==dst) — skip them as real nets; stamp via a wall net
        // Simpler: route USB+LS on an open sheet, then a net whose dst is outside the sheet.
        let outside = net(Kind::Ls, [12.0, 32.0], [200.0, 32.0], 2);
        nets = vec![ls_ok.clone(), usb.clone(), outside];
        let r = route_r1(50.0, 50.0, &nets);
        assert!(
            r.order.iter().position(|&k| k == Kind::UsbHs).unwrap()
                < r.order.iter().position(|&k| k == Kind::Ls).unwrap()
        );
        assert!(r.traces.iter().any(|t| t.kind == Kind::UsbHs));
        assert!(r.failed.iter().any(|n| n.contact.0 == 2));
        assert!(
            r.traces
                .iter()
                .all(|t| t.points.iter().all(|p| p.len() == 2))
        );
        assert!(!r.traces.is_empty());
    }

    #[test]
    fn open_net_routes_on_one_layer() {
        let n = net(Kind::Ls, [12.0, 20.0], [28.0, 28.0], 0);
        let r = route_r1(40.0, 40.0, &[n]);
        assert_eq!(r.traces.len(), 1);
        assert!(r.traces[0].length_mm > 0.0);
        assert!(r.failed.is_empty());
    }

    #[test]
    fn r2_uses_pattern_on_open_l() {
        let n = net(Kind::UsbHs, [12.0, 20.0], [36.0, 20.0], 0);
        let r = route_r2(50.0, 50.0, &[n]);
        assert_eq!(r.traces.len(), 1);
        assert!(r.traces[0].via_pattern, "open L should accept a pattern");
        assert!(r.failed.is_empty());
        assert_eq!(r.router, "R2");
    }

    #[test]
    fn r3_drops_a_crossing_board_not_the_panel() {
        let a = TwoPinNet {
            contact: ContactId(0),
            pin: MatePinId(0),
            kind: Kind::Ls,
            board: BoardId(0),
            src: [12.0, 12.0],
            dst: [40.0, 40.0],
            width: 0.2,
        };
        let b = TwoPinNet {
            contact: ContactId(1),
            pin: MatePinId(1),
            kind: Kind::Ls,
            board: BoardId(1),
            src: [12.0, 40.0],
            dst: [40.0, 12.0],
            width: 0.2,
        };
        let r = route_r3(60.0, 60.0, &[a, b]);
        assert_eq!(r.dropped_boards.len(), 1);
        assert_eq!(r.traces.len(), 1);
        assert_eq!(r.failed.len(), 1);
        assert_ne!(r.traces[0].board, r.failed[0].board);
        assert_eq!(r.router, "R3");
    }

    #[test]
    fn r2_still_attempts_usb_before_ls_and_fail_opens() {
        let usb = net(Kind::UsbHs, [12.0, 20.0], [30.0, 20.0], 0);
        let ls_ok = net(Kind::Ls, [12.0, 26.0], [30.0, 26.0], 1);
        let outside = net(Kind::Ls, [12.0, 32.0], [200.0, 32.0], 2);
        let r = route_r2(50.0, 50.0, &[ls_ok, usb, outside]);
        assert!(
            r.order.iter().position(|&k| k == Kind::UsbHs).unwrap()
                < r.order.iter().position(|&k| k == Kind::Ls).unwrap()
        );
        assert!(r.traces.iter().any(|t| t.kind == Kind::UsbHs));
        assert!(r.failed.iter().any(|n| n.contact.0 == 2));
    }
}
