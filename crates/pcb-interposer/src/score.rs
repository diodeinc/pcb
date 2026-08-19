//! G0 / G1 scores from the exploration note.

use crate::pattern::Pattern;
use crate::route::{RouteResult, TwoPinNet};
use crate::types::{Assign, Kind, Problem, dist};

/// Routed vs failed for one electrical kind. GND uses `poured` instead of maze `routed`.
#[derive(Debug, Clone, Copy, Default)]
pub struct KindCov {
    pub total: usize,
    pub routed: usize,
    pub failed: usize,
    pub poured: usize,
}

impl KindCov {
    pub fn fmt_maze(self) -> String {
        format!("{}/{}", self.routed, self.total)
    }

    pub fn fmt_gnd(self) -> String {
        format!("{}/{}", self.poured, self.total)
    }
}

#[derive(Debug, Clone, Default)]
pub struct Score {
    pub hall_ok: bool,
    pub a_mm: f64,
    pub x_cross: usize,
    pub l_max: f64,
    pub u_delta: f64,
    pub u_mean: f64,
    pub p_mean: f64,
    pub n_arr: usize,
    pub w_unused: usize,
    pub v_vias: usize,
    pub l_routed: f64,
    /// Maze-attempted nets only (USB / power / LS). GND pours are not included.
    pub nets_total: usize,
    pub nets_routed: usize,
    pub boards_total: usize,
    pub boards_complete: usize,
    pub usb: KindCov,
    pub power: KindCov,
    pub ls: KindCov,
    pub gnd: KindCov,
}

pub fn crossing_count(nets: &[TwoPinNet]) -> usize {
    let mut n = 0;
    for i in 0..nets.len() {
        for j in i + 1..nets.len() {
            if segs_cross(nets[i].src, nets[i].dst, nets[j].src, nets[j].dst) {
                n += 1;
            }
        }
    }
    n
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

pub fn score_g0(
    problem: &Problem,
    assign: &Assign,
    pattern: &Pattern,
    nets: &[TwoPinNet],
) -> Score {
    let mut s = Score {
        hall_ok: true,
        a_mm: assign.cost_mm,
        x_cross: crossing_count(nets),
        n_arr: pattern.n_arrays,
        w_unused: pattern.unused_pins,
        nets_total: nets.iter().filter(|n| n.kind != Kind::Gnd).count(),
        boards_total: problem
            .contacts
            .values()
            .map(|c| c.board)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        ..Score::default()
    };
    if !nets.is_empty() {
        s.l_max = nets.iter().map(|n| dist(n.src, n.dst)).fold(0.0, f64::max);
    }
    let mut usb_lens = Vec::new();
    let mut pwr = Vec::new();
    for n in nets {
        let l = dist(n.src, n.dst);
        match n.kind {
            Kind::UsbHs => usb_lens.push(l),
            Kind::Vtarget | Kind::Vusb => pwr.push(l),
            _ => {}
        }
    }
    if usb_lens.len() >= 2 {
        // pair consecutive USB nets from same board after sort
        let mut by_board: std::collections::BTreeMap<_, Vec<f64>> =
            std::collections::BTreeMap::new();
        for n in nets.iter().filter(|n| n.kind == Kind::UsbHs) {
            by_board
                .entry(n.board)
                .or_default()
                .push(dist(n.src, n.dst));
        }
        let mut dmax: f64 = 0.0;
        let mut sum = 0.0;
        let mut cnt = 0;
        for lens in by_board.values() {
            if lens.len() >= 2 {
                dmax = dmax.max((lens[0] - lens[1]).abs());
                sum += (lens[0] + lens[1]) / 2.0;
                cnt += 1;
            }
        }
        s.u_delta = dmax;
        if cnt > 0 {
            s.u_mean = sum / cnt as f64;
        }
    }
    if !pwr.is_empty() {
        s.p_mean = pwr.iter().sum::<f64>() / pwr.len() as f64;
    }
    s
}

fn fill_kind(nets: &[TwoPinNet], route: &RouteResult, pred: impl Fn(Kind) -> bool) -> KindCov {
    let mut c = KindCov::default();
    let routed: std::collections::HashSet<_> = route
        .traces
        .iter()
        .filter(|t| pred(t.kind))
        .map(|t| t.contact)
        .collect();
    let poured: std::collections::HashSet<_> = route.poured.iter().copied().collect();
    let failed: std::collections::HashSet<_> = route
        .failed
        .iter()
        .filter(|n| pred(n.kind))
        .map(|n| n.contact)
        .collect();
    for n in nets.iter().filter(|n| pred(n.kind)) {
        c.total += 1;
        if routed.contains(&n.contact) {
            c.routed += 1;
        } else if poured.contains(&n.contact) {
            c.poured += 1;
        } else if failed.contains(&n.contact) {
            c.failed += 1;
        } else {
            c.failed += 1;
        }
    }
    c
}

pub fn score_g1(mut g0: Score, route: &RouteResult, nets: &[TwoPinNet]) -> Score {
    g0.v_vias = 0;
    g0.l_routed = route.maze_traces().map(|t| t.length_mm).sum();
    g0.usb = fill_kind(nets, route, |k| k == Kind::UsbHs);
    g0.power = fill_kind(nets, route, |k| matches!(k, Kind::Vtarget | Kind::Vusb));
    g0.ls = fill_kind(nets, route, |k| k == Kind::Ls);
    g0.gnd = fill_kind(nets, route, |k| k == Kind::Gnd);
    g0.nets_total = g0.usb.total + g0.power.total + g0.ls.total;
    g0.nets_routed = g0.usb.routed + g0.power.routed + g0.ls.routed;
    g0.boards_complete = route.boards_complete(nets);
    g0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::route::{RouterKind, TwoPinNet, route};
    use crate::types::{BoardId, ContactId, MatePinId};

    fn net(kind: Kind, src: [f64; 2], dst: [f64; 2], id: u32) -> TwoPinNet {
        TwoPinNet {
            contact: ContactId(id),
            pin: MatePinId(id),
            kind,
            board: BoardId(0),
            src,
            dst,
            width: 0.2,
        }
    }

    #[test]
    fn gnd_pour_does_not_count_as_maze_routed() {
        let usb = net(Kind::UsbHs, [12.0, 20.0], [30.0, 20.0], 0);
        let gnd = net(Kind::Gnd, [12.0, 24.0], [30.0, 24.0], 1);
        let ls = net(Kind::Ls, [12.0, 28.0], [200.0, 28.0], 2); // off-sheet → fail
        let nets = vec![usb, gnd, ls];
        let r = route(RouterKind::R1, 50.0, 50.0, &nets);
        assert_eq!(r.poured.len(), 1);
        assert!(r.traces.iter().all(|t| t.kind != Kind::Gnd));
        let s = score_g1(Score::default(), &r, &nets);
        assert_eq!(s.gnd.poured, 1);
        assert_eq!(s.gnd.routed, 0);
        assert_eq!(s.usb.routed, 1);
        assert_eq!(s.usb.total, 1);
        assert_eq!(s.ls.failed, 1);
        assert_eq!(s.ls.total, 1);
        assert_eq!(s.nets_routed, 1, "GND pour must not inflate maze routed");
        assert_eq!(s.nets_total, 2);
    }
}

/// Min-max hat then weighted S0 / S1 as in the note. `rows` are one per
/// (strategy × sheet). Lower is better.
pub fn rank_s1(rows: &[(String, Score)]) -> Vec<(String, f64, f64)> {
    if rows.is_empty() {
        return Vec::new();
    }
    fn hat(xs: &[f64], x: f64) -> f64 {
        let min = xs.iter().copied().fold(f64::INFINITY, f64::min);
        let max = xs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        if (max - min).abs() < 1e-9 {
            0.0
        } else {
            (x - min) / (max - min)
        }
    }
    let a: Vec<f64> = rows.iter().map(|(_, s)| s.a_mm).collect();
    let u: Vec<f64> = rows.iter().map(|(_, s)| s.u_delta).collect();
    let lmax: Vec<f64> = rows.iter().map(|(_, s)| s.l_max).collect();
    let narr: Vec<f64> = rows.iter().map(|(_, s)| s.n_arr as f64).collect();
    let x: Vec<f64> = rows.iter().map(|(_, s)| s.x_cross as f64).collect();
    let w: Vec<f64> = rows.iter().map(|(_, s)| s.w_unused as f64).collect();
    let v: Vec<f64> = rows.iter().map(|(_, s)| s.v_vias as f64).collect();
    let l: Vec<f64> = rows.iter().map(|(_, s)| s.l_routed).collect();
    let um: Vec<f64> = rows.iter().map(|(_, s)| s.u_mean).collect();
    let p: Vec<f64> = rows.iter().map(|(_, s)| s.p_mean).collect();
    let mut out = Vec::new();
    for (name, s) in rows {
        let s0 = 3.0 * hat(&a, s.a_mm)
            + 4.0 * hat(&u, s.u_delta)
            + 2.0 * hat(&lmax, s.l_max)
            + hat(&narr, s.n_arr as f64)
            + hat(&x, s.x_cross as f64)
            + 0.5 * hat(&w, s.w_unused as f64);
        let s1 = s0
            + 3.0 * hat(&v, s.v_vias as f64)
            + 2.0 * hat(&l, s.l_routed)
            + 2.0 * hat(&um, s.u_mean)
            + hat(&p, s.p_mean);
        out.push((name.clone(), s0, s1));
    }
    out.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal));
    out
}
