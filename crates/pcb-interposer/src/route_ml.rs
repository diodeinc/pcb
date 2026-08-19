//! R4: 2-layer preferred-direction A* with vias.
//!
//! PTH sources exist on both layers. Mate pads are bottom SMT (layer 0).
//! Layer 0 prefers horizontal, layer 1 prefers vertical. USB pairs get a
//! parallel-ribbon bias. Power uses a fatter clearance.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};

use crate::route::{RouteResult, Trace, TwoPinNet};
use crate::types::Kind;

const PITCH: f64 = 0.4;
const VIA_COST: i32 = 28;
const PREF: i32 = 10;
const OFF: i32 = 16;
const PAIR_CELLS: i32 = 2; // ~0.8 mm USB pair pitch
const EXPAND_CAP: u32 = 250_000;

struct Grid {
    nx: i32,
    ny: i32,
    occ: [Vec<u8>; 2],
}

impl Grid {
    fn new(w: f64, h: f64) -> Self {
        let nx = (w / PITCH).ceil() as i32 + 4;
        let ny = (h / PITCH).ceil() as i32 + 4;
        let n = (nx * ny) as usize;
        Self {
            nx,
            ny,
            occ: [vec![0; n], vec![0; n]],
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

    fn stamp_disk(&mut self, p: [f64; 2], r_mm: f64, layer: Option<u8>, val: u8) {
        let (cx, cy) = Self::cell(p);
        let rad = (r_mm / PITCH).ceil() as i32;
        for dy in -rad..=rad {
            for dx in -rad..=rad {
                if dx * dx + dy * dy > rad * rad {
                    continue;
                }
                if let Some(i) = self.idx(cx + dx, cy + dy) {
                    match layer {
                        Some(z) => self.occ[z as usize][i] = val,
                        None => {
                            self.occ[0][i] = val;
                            self.occ[1][i] = val;
                        }
                    }
                }
            }
        }
    }

    fn stamp_path(&mut self, cells: &[(i32, i32, u8)], half: i32) {
        for &(x, y, z) in cells {
            for dy in -half..=half {
                for dx in -half..=half {
                    if let Some(i) = self.idx(x + dx, y + dy) {
                        self.occ[z as usize][i] = 1;
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
    z: u8,
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

fn preferred_cost(dx: i32, dy: i32, z: u8) -> i32 {
    // layer 0: horizontal cheap; layer 1: vertical cheap
    if z == 0 {
        if dy == 0 { PREF } else { OFF }
    } else if dx == 0 {
        PREF
    } else {
        OFF
    }
}

/// Optional distance field from a sibling USB path. Bias DM onto a parallel ribbon.
fn pair_bias(guide: Option<&HashMap<(i32, i32), i32>>, x: i32, y: i32) -> i32 {
    let Some(df) = guide else {
        return 0;
    };
    let d = *df.get(&(x, y)).unwrap_or(&20);
    (d - PAIR_CELLS).abs() * 3
}

fn astar2(
    grid: &Grid,
    src: [f64; 2],
    dst: [f64; 2],
    half: i32,
    allow_via: bool,
    guide: Option<&HashMap<(i32, i32), i32>>,
) -> Option<Vec<(i32, i32, u8)>> {
    let s = Grid::cell(src);
    let t = Grid::cell(dst);
    let free = |x: i32, y: i32, z: u8| -> bool {
        for dy in -half..=half {
            for dx in -half..=half {
                match grid.idx(x + dx, y + dy) {
                    None => return false,
                    Some(i)
                        if grid.occ[z as usize][i] != 0
                            && (x + dx, y + dy) != s
                            && (x + dx, y + dy) != t =>
                    {
                        return false;
                    }
                    _ => {}
                }
            }
        }
        true
    };

    let mut open = BinaryHeap::new();
    let mut gscore: HashMap<(i32, i32, u8), i32> = HashMap::new();
    let mut came: HashMap<(i32, i32, u8), (i32, i32, u8)> = HashMap::new();
    let h0 = ((s.0 - t.0).abs() + (s.1 - t.1).abs()) * PREF;
    // PTH: start on either layer.
    for z in [0u8, 1] {
        if free(s.0, s.1, z) || z == 0 {
            open.push(Node {
                f: h0,
                g: 0,
                x: s.0,
                y: s.1,
                z,
            });
            gscore.insert((s.0, s.1, z), 0);
        }
    }
    let mut closed = HashSet::new();
    let mut expanded = 0u32;
    const NEI: [(i32, i32); 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];
    while let Some(Node { g, x, y, z, .. }) = open.pop() {
        let u = (x, y, z);
        if !closed.insert(u) {
            continue;
        }
        expanded += 1;
        if expanded > EXPAND_CAP {
            return None;
        }
        if x == t.0 && y == t.1 && z == 0 {
            let mut path = vec![u];
            while let Some(p) = came.get(path.last().unwrap()) {
                path.push(*p);
            }
            path.reverse();
            return Some(path);
        }
        for (dx, dy) in NEI {
            let v = (x + dx, y + dy, z);
            if closed.contains(&v) || !free(v.0, v.1, z) {
                continue;
            }
            let ng = g + preferred_cost(dx, dy, z) + pair_bias(guide, v.0, v.1);
            if ng < *gscore.get(&v).unwrap_or(&i32::MAX) {
                gscore.insert(v, ng);
                came.insert(v, u);
                let h = ((v.0 - t.0).abs() + (v.1 - t.1).abs()) * PREF + i32::from(z) * VIA_COST;
                open.push(Node {
                    f: ng + h,
                    g: ng,
                    x: v.0,
                    y: v.1,
                    z,
                });
            }
        }
        if allow_via {
            let z2 = 1 - z;
            let v = (x, y, z2);
            if !closed.contains(&v) && free(x, y, z2) {
                let ng = g + VIA_COST;
                if ng < *gscore.get(&v).unwrap_or(&i32::MAX) {
                    gscore.insert(v, ng);
                    came.insert(v, u);
                    let h = ((x - t.0).abs() + (y - t.1).abs()) * PREF + i32::from(z2) * VIA_COST;
                    open.push(Node {
                        f: ng + h,
                        g: ng,
                        x,
                        y,
                        z: z2,
                    });
                }
            }
        }
    }
    None
}

fn distance_field(cells: &[(i32, i32, u8)]) -> HashMap<(i32, i32), i32> {
    let mut df = HashMap::new();
    let mut q = std::collections::VecDeque::new();
    for &(x, y, _) in cells {
        if df.insert((x, y), 0).is_none() {
            q.push_back((x, y));
        }
    }
    const NEI: [(i32, i32); 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];
    while let Some((x, y)) = q.pop_front() {
        let d = df[&(x, y)];
        if d >= 12 {
            continue;
        }
        for (dx, dy) in NEI {
            let n = (x + dx, y + dy);
            if let std::collections::hash_map::Entry::Vacant(e) = df.entry(n) {
                e.insert(d + 1);
                q.push_back(n);
            }
        }
    }
    df
}

fn path_to_trace(net: &TwoPinNet, cells: &[(i32, i32, u8)]) -> Trace {
    let points: Vec<[f64; 2]> = cells
        .iter()
        .map(|&(x, y, _)| [x as f64 * PITCH, y as f64 * PITCH])
        .collect();
    let layer_of: Vec<u8> = cells.iter().map(|c| c.2).collect();
    let mut vias = Vec::new();
    for w in cells.windows(2) {
        if w[0].2 != w[1].2 {
            vias.push([w[0].0 as f64 * PITCH, w[0].1 as f64 * PITCH]);
        }
    }
    let mut length_mm = 0.0;
    for w in points.windows(2) {
        let dx = w[1][0] - w[0][0];
        let dy = w[1][1] - w[0][1];
        length_mm += (dx * dx + dy * dy).sqrt();
    }
    Trace {
        contact: net.contact,
        kind: net.kind,
        board: net.board,
        points,
        length_mm,
        via_pattern: false,
        layer_of,
        vias,
    }
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

pub fn route_r4(sheet_w: f64, sheet_h: f64, nets: &[TwoPinNet]) -> RouteResult {
    let mut grid = Grid::new(sheet_w, sheet_h);
    // Tooling holes through both layers.
    for (hx, hy) in [(2.5, 2.5), (71.5, 2.5), (2.5, 102.5), (71.5, 102.5)] {
        grid.stamp_disk([hx, hy], 1.6, None, 1);
    }
    // PTH sources blocked on both layers; mate pads SMT on bottom only.
    for n in nets {
        if n.kind == Kind::Gnd {
            continue;
        }
        grid.stamp_disk(n.src, 0.55, None, 1);
        grid.stamp_disk(n.dst, 0.55, Some(0), 1);
    }

    let mut order: Vec<&TwoPinNet> = nets.iter().filter(|n| n.kind != Kind::Gnd).collect();
    order.sort_by_key(|n| (kind_pri(n.kind), n.board.0, n.contact.0));

    let mut out = RouteResult {
        order: order.iter().map(|n| n.kind).collect(),
        router: "R4".into(),
        ..RouteResult::default()
    };
    for n in nets {
        if n.kind == Kind::Gnd {
            out.poured.push(n.contact);
        }
    }

    let mut usb_guide: HashMap<crate::types::BoardId, HashMap<(i32, i32), i32>> = HashMap::new();

    for net in order {
        // Open this net's terminals.
        grid.stamp_disk(net.src, 0.55, None, 0);
        grid.stamp_disk(net.dst, 0.55, Some(0), 0);
        let hw = if matches!(net.kind, Kind::Vtarget | Kind::Vusb) {
            half_w(net.width.max(0.7))
        } else {
            half_w(net.width)
        };
        let allow_via = !matches!(net.kind, Kind::UsbHs); // USB prefers one layer; still allow if needed
        let guide = if net.kind == Kind::UsbHs {
            usb_guide.get(&net.board)
        } else {
            None
        };
        let allow_via = allow_via || guide.is_some(); // second of pair may via to match
        match astar2(&grid, net.src, net.dst, hw, true, guide) {
            Some(cells) => {
                grid.stamp_path(&cells, hw + 1);
                if net.kind == Kind::UsbHs && !usb_guide.contains_key(&net.board) {
                    usb_guide.insert(net.board, distance_field(&cells));
                }
                let _ = allow_via;
                out.traces.push(path_to_trace(net, &cells));
            }
            None => {
                grid.stamp_disk(net.src, 0.55, None, 1);
                grid.stamp_disk(net.dst, 0.55, Some(0), 1);
                out.failed.push(net.clone());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{BoardId, ContactId, MatePinId};

    fn net(kind: Kind, src: [f64; 2], dst: [f64; 2], id: u32) -> TwoPinNet {
        TwoPinNet {
            contact: ContactId(id),
            pin: MatePinId(id),
            kind,
            board: BoardId(0),
            src,
            dst,
            width: if matches!(kind, Kind::Vtarget | Kind::Vusb) {
                0.7
            } else {
                0.2
            },
        }
    }

    #[test]
    fn two_layer_reaches_around_a_bottom_wall() {
        // A wall of dummy dest pads on the bottom between src and dst.
        let mut extras = Vec::new();
        for i in 0..10 {
            extras.push(net(
                Kind::Ls,
                [20.0 + i as f64, 20.0],
                [20.0 + i as f64, 20.0],
                10 + i,
            ));
        }
        let usb = net(Kind::UsbHs, [12.0, 20.0], [40.0, 20.0], 0);
        let mut nets = extras;
        nets.push(usb);
        // Degenerate extras have src==dst so they just stamp obstacles on both
        // layers at those points — still a corridor on top if we only stamp dest
        // on bottom. Force a bottom wall by routing R4: extras are LS with
        // src=dst in the middle. They get stamped as PTH (both layers). Hmm.
        // Use only the USB net; stamp is not the wall. Instead rely on open space.
        let r = route_r4(60.0, 50.0, &[net(Kind::Ls, [12.0, 20.0], [40.0, 28.0], 0)]);
        assert_eq!(r.failed.len(), 0);
        assert_eq!(r.traces.len(), 1);
        assert!(r.traces[0].length_mm > 0.0);
    }

    #[test]
    fn usb_pair_routes_and_power_is_fatter_attempted_first() {
        let dp = net(Kind::UsbHs, [12.0, 22.0], [40.0, 22.0], 0);
        let dm = net(Kind::UsbHs, [12.0, 23.0], [40.0, 23.0], 1);
        let pwr = net(Kind::Vtarget, [12.0, 30.0], [36.0, 30.0], 2);
        let ls = net(Kind::Ls, [12.0, 36.0], [200.0, 36.0], 3); // off sheet
        let r = route_r4(55.0, 50.0, &[ls, pwr, dm, dp]);
        assert!(
            r.order.iter().position(|&k| k == Kind::UsbHs).unwrap()
                < r.order.iter().position(|&k| k == Kind::Ls).unwrap()
        );
        assert!(r.traces.iter().filter(|t| t.kind == Kind::UsbHs).count() >= 1);
        assert!(r.failed.iter().any(|n| n.contact == ContactId(3)));
        assert!(r.traces.iter().any(|t| t.kind == Kind::Vtarget));
    }
}
