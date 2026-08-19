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
    /// Direction changes >30° / >60° over all routed traces.
    pub bends: usize,
    pub bends90: usize,
    /// Routed length over the Euclidean lower bound (1.0 = perfect).
    pub detour: f64,
    /// Max routed USB pair length mismatch (mm).
    pub u_delta_routed: f64,
    /// Self-DRC clearance violations on the final geometry.
    pub drc_violations: usize,
    /// Smallest observed copper gap (mm).
    pub min_gap_mm: f64,
    /// USB pairs routed as two individual traces instead of a ribbon.
    pub loose_pairs: usize,
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
    for n in nets.iter().filter(|n| pred(n.kind)) {
        c.total += 1;
        if routed.contains(&n.contact) {
            c.routed += 1;
        } else if poured.contains(&n.contact) {
            c.poured += 1;
        } else {
            // Unassigned and failed alike: the contact has no copper.
            c.failed += 1;
        }
    }
    c
}

pub fn score_g1(mut g0: Score, route: &RouteResult, nets: &[TwoPinNet]) -> Score {
    g0.v_vias = route.maze_traces().map(|t| t.vias.len()).sum();
    g0.l_routed = route.maze_traces().map(|t| t.length_mm).sum();
    let mut euclid = 0.0;
    for t in route.maze_traces() {
        let (b, s) = crate::router::count_bends(&t.points);
        g0.bends += b;
        g0.bends90 += s;
        if let (Some(a), Some(z)) = (t.points.first(), t.points.last()) {
            euclid += dist(*a, *z);
        }
    }
    if euclid > 1e-9 {
        g0.detour = g0.l_routed / euclid;
    }
    let loose: std::collections::HashSet<_> = route.loose_contacts.iter().copied().collect();
    let mut by_board: std::collections::BTreeMap<_, Vec<f64>> = Default::default();
    for t in route
        .maze_traces()
        .filter(|t| t.kind == Kind::UsbHs && !loose.contains(&t.contact))
    {
        by_board.entry(t.board).or_default().push(t.length_mm);
    }
    for lens in by_board.values() {
        if lens.len() == 2 {
            g0.u_delta_routed = g0.u_delta_routed.max((lens[0] - lens[1]).abs());
        }
    }
    g0.drc_violations = route.drc_violations;
    g0.min_gap_mm = route.min_gap_mm;
    g0.loose_pairs = route.loose_pairs;
    g0.usb = fill_kind(nets, route, |k| k == Kind::UsbHs);
    g0.power = fill_kind(nets, route, |k| matches!(k, Kind::Vtarget | Kind::Vusb));
    g0.ls = fill_kind(nets, route, |k| k == Kind::Ls);
    g0.gnd = fill_kind(nets, route, |k| k == Kind::Gnd);
    g0.nets_total = g0.usb.total + g0.power.total + g0.ls.total;
    g0.nets_routed = g0.usb.routed + g0.power.routed + g0.ls.routed;
    g0.boards_complete = route.boards_complete(nets);
    g0
}

/// Honest single-number quality score (lower is better). Completion is the
/// gate: an incomplete board costs more than any polish metric can recover,
/// so failed nets can never help a strategy win. Then legality, then USB
/// integrity, then cleanliness (vias, bends, detour) normalized per net.
pub fn quality_score(s: &Score) -> f64 {
    let frac = if s.boards_total == 0 {
        0.0
    } else {
        s.boards_complete as f64 / s.boards_total as f64
    };
    let per_net = |x: usize| x as f64 / s.nets_total.max(1) as f64;
    1000.0 * (1.0 - frac)
        + 200.0 * s.drc_violations as f64
        + 30.0 * s.loose_pairs as f64
        + 8.0 * s.u_delta_routed
        + 25.0 * (s.detour - 1.0).max(0.0)
        + 12.0 * per_net(s.v_vias)
        + 6.0 * per_net(s.bends90)
        + 2.0 * per_net(s.bends)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::route::{RouteResult, Trace, TwoPinNet};
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
        let ls = net(Kind::Ls, [12.0, 28.0], [200.0, 28.0], 2); // unroutable
        let nets = vec![usb.clone(), gnd, ls.clone()];
        let r = RouteResult {
            traces: vec![Trace {
                contact: usb.contact,
                kind: usb.kind,
                board: usb.board,
                points: vec![usb.src, usb.dst],
                length_mm: 18.0,
                via_pattern: false,
                layer_of: vec![0, 0],
                vias: vec![],
                term_via: None,
                width: 0.2,
            }],
            failed: vec![ls],
            poured: vec![ContactId(1)],
            ..RouteResult::default()
        };
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

    #[test]
    fn completion_dominates_polish() {
        let mut complete = Score {
            boards_total: 8,
            boards_complete: 8,
            nets_total: 40,
            v_vias: 40,
            bends: 120,
            bends90: 30,
            detour: 1.3,
            u_delta_routed: 2.0,
            ..Score::default()
        };
        let incomplete = Score {
            boards_total: 8,
            boards_complete: 7,
            nets_total: 40,
            detour: 1.0,
            ..Score::default()
        };
        assert!(quality_score(&complete) < quality_score(&incomplete));
        complete.drc_violations = 1;
        assert!(
            quality_score(&complete) > 100.0,
            "DRC violations must be expensive"
        );
    }
}
