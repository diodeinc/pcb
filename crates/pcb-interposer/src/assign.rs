//! Min-cost assignment (Kuhn–Munkres) per kind.

use crate::types::{Assign, ContactId, DemandId, Kind, MatePinId, Problem, Shape, SlotId, dist};

const SCALE: f64 = 1000.0;

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

fn crossing_penalty(problem: &Problem, out: &Assign, did: DemandId, sid: SlotId) -> f64 {
    if out.contact_to_pin.is_empty() {
        return 0.0;
    }
    let demand = &problem.demands[&did];
    let slot = &problem.slots[&sid];
    let mut extra = 0.0;
    for (cid, pid) in demand.members.iter().zip(slot.pins.iter()) {
        let a1 = problem.contacts[cid].xy;
        let a2 = problem.pins[pid].xy;
        for (oc, op) in &out.contact_to_pin {
            let b1 = problem.contacts[oc].xy;
            let b2 = problem.pins[op].xy;
            if segs_cross(a1, a2, b1, b2) {
                extra += 8.0;
            }
        }
    }
    extra
}

/// Assign demands to slots. USB first in `Kind::ALL` order for a stable dump.
/// Later kinds pay a crossing penalty against already-bound segments.
pub fn assign(problem: &Problem) -> Assign {
    let mut out = Assign::default();
    let mut total = 0.0;
    let center = mate_center(problem);
    for kind in Kind::ALL {
        let demands = problem.demands_of(kind);
        let slots = problem.slots_of(kind);
        if demands.is_empty() {
            continue;
        }
        let n = demands.len().max(slots.len());
        let mut cost = vec![vec![1_000_000_000i64; n]; n];
        for (i, did) in demands.iter().enumerate() {
            for (j, sid) in slots.iter().enumerate() {
                let c = slot_cost(problem, center, *did, *sid)
                    + crossing_penalty(problem, &out, *did, *sid);
                cost[i][j] = (c * SCALE).round() as i64;
            }
        }
        let matching = hungarian(&cost);
        for (i, did) in demands.iter().enumerate() {
            let Some(j) = matching[i] else {
                continue;
            };
            if j >= slots.len() {
                continue;
            }
            let sid = slots[j];
            out.demand_to_slot.insert(*did, sid);
            total += slot_cost(problem, center, *did, sid);
            bind_pins(problem, *did, sid, &mut out);
        }
    }
    out.cost_mm = total;
    out
}

/// Center of the mate region: the bounding-box center of all mate pins.
/// Follows the pattern's folded orientation without extra plumbing.
fn mate_center(problem: &Problem) -> [f64; 2] {
    let (mut min, mut max) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
    for p in problem.pins.values() {
        for k in 0..2 {
            min[k] = min[k].min(p.xy[k]);
            max[k] = max[k].max(p.xy[k]);
        }
    }
    if min[0].is_finite() {
        [(min[0] + max[0]) / 2.0, (min[1] + max[1]) / 2.0]
    } else {
        [37.0, 52.5]
    }
}

fn angle_of(center: [f64; 2], p: [f64; 2]) -> f64 {
    (p[1] - center[1]).atan2(p[0] - center[0])
}

fn ang_delta(center: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let mut d = (angle_of(center, a) - angle_of(center, b)).abs();
    if d > std::f64::consts::PI {
        d = 2.0 * std::f64::consts::PI - d;
    }
    d
}

fn centroid(pts: &[[f64; 2]]) -> [f64; 2] {
    let n = pts.len().max(1) as f64;
    [
        pts.iter().map(|p| p[0]).sum::<f64>() / n,
        pts.iter().map(|p| p[1]).sum::<f64>() / n,
    ]
}

fn slot_cost(problem: &Problem, center: [f64; 2], did: DemandId, sid: SlotId) -> f64 {
    let demand = &problem.demands[&did];
    let slot = &problem.slots[&sid];
    let pin_xy: Vec<[f64; 2]> = slot.pins.iter().map(|p| problem.pins[p].xy).collect();
    let cxy: Vec<[f64; 2]> = demand
        .members
        .iter()
        .map(|c| problem.contacts[c].xy)
        .collect();
    // A pairing whose terminal via has no legal home cannot be routed at
    // all; make it a last resort.
    let via_locked = cxy
        .iter()
        .zip(pin_xy.iter())
        .any(|(c, p)| !crate::router::via_feasible(problem, demand.kind, *c, *p));
    if via_locked {
        return 10_000.0 + dist(cxy[0], pin_xy[0]);
    }
    // Angular term uses LS/USB interchangeability so the funnel does not cross.
    let ang = 30.0 * ang_delta(center, centroid(&cxy), centroid(&pin_xy));
    let geo = match slot.shape {
        Shape::Unit => dist(cxy[0], pin_xy[0]),
        Shape::Ordered { .. } => {
            // polarity fixed: member[i] → pin[i]
            let mut s = 0.0;
            for (a, b) in cxy.iter().zip(pin_xy.iter()) {
                s += dist(*a, *b);
            }
            if cxy.len() >= 2 && pin_xy.len() >= 2 {
                let l0 = dist(cxy[0], pin_xy[0]);
                let l1 = dist(cxy[1], pin_xy[1]);
                s += (l0 - l1).abs();
            }
            s
        }
        Shape::Unordered { .. } => min_unordered(&cxy, &pin_xy),
    };
    geo + ang
}

fn min_unordered(contacts: &[[f64; 2]], pins: &[[f64; 2]]) -> f64 {
    if contacts.len() == 1 {
        return pins
            .iter()
            .map(|p| dist(contacts[0], *p))
            .fold(f64::INFINITY, f64::min);
    }
    if contacts.len() == 2 && pins.len() == 2 {
        let a = dist(contacts[0], pins[0]) + dist(contacts[1], pins[1]);
        let b = dist(contacts[0], pins[1]) + dist(contacts[1], pins[0]);
        return a.min(b);
    }
    // brute force for n<=4
    let mut best = f64::INFINITY;
    let mut used = vec![false; pins.len()];
    fn rec(
        i: usize,
        contacts: &[[f64; 2]],
        pins: &[[f64; 2]],
        used: &mut [bool],
        acc: f64,
        best: &mut f64,
    ) {
        if i == contacts.len() {
            *best = acc.min(*best);
            return;
        }
        for j in 0..pins.len() {
            if used[j] {
                continue;
            }
            used[j] = true;
            rec(
                i + 1,
                contacts,
                pins,
                used,
                acc + dist(contacts[i], pins[j]),
                best,
            );
            used[j] = false;
        }
    }
    rec(0, contacts, pins, &mut used, 0.0, &mut best);
    best
}

fn bind_pins(problem: &Problem, did: DemandId, sid: SlotId, out: &mut Assign) {
    let demand = &problem.demands[&did];
    let slot = &problem.slots[&sid];
    match slot.shape {
        Shape::Unit | Shape::Ordered { .. } => {
            for (cid, pid) in demand.members.iter().zip(slot.pins.iter()) {
                out.contact_to_pin.insert(*cid, *pid);
            }
        }
        Shape::Unordered { .. } => {
            let cxy: Vec<(ContactId, [f64; 2])> = demand
                .members
                .iter()
                .map(|c| (*c, problem.contacts[c].xy))
                .collect();
            let pxy: Vec<(MatePinId, [f64; 2])> =
                slot.pins.iter().map(|p| (*p, problem.pins[p].xy)).collect();
            if cxy.len() == 1 {
                let mut best = pxy[0].0;
                let mut best_d = f64::INFINITY;
                for (pid, xy) in &pxy {
                    let d = dist(cxy[0].1, *xy);
                    if d < best_d {
                        best_d = d;
                        best = *pid;
                    }
                }
                out.contact_to_pin.insert(cxy[0].0, best);
                return;
            }
            if cxy.len() == 2 && pxy.len() == 2 {
                let a = dist(cxy[0].1, pxy[0].1) + dist(cxy[1].1, pxy[1].1);
                let b = dist(cxy[0].1, pxy[1].1) + dist(cxy[1].1, pxy[0].1);
                if a <= b {
                    out.contact_to_pin.insert(cxy[0].0, pxy[0].0);
                    out.contact_to_pin.insert(cxy[1].0, pxy[1].0);
                } else {
                    out.contact_to_pin.insert(cxy[0].0, pxy[1].0);
                    out.contact_to_pin.insert(cxy[1].0, pxy[0].0);
                }
            }
        }
    }
}

/// Square min-cost assignment. `matching[row] = Some(col)`.
pub fn hungarian(cost: &[Vec<i64>]) -> Vec<Option<usize>> {
    let n = cost.len();
    if n == 0 {
        return Vec::new();
    }
    let mut u = vec![0i64; n + 1];
    let mut v = vec![0i64; n + 1];
    let mut p = vec![0usize; n + 1];
    let mut way = vec![0usize; n + 1];
    for i in 1..=n {
        p[0] = i;
        let mut j0 = 0usize;
        let mut minv = vec![i64::MAX; n + 1];
        let mut used = vec![false; n + 1];
        loop {
            used[j0] = true;
            let i0 = p[j0];
            let mut delta = i64::MAX;
            let mut j1 = 0usize;
            for j in 1..=n {
                if used[j] {
                    continue;
                }
                let cur = cost[i0 - 1][j - 1] - u[i0] - v[j];
                if cur < minv[j] {
                    minv[j] = cur;
                    way[j] = j0;
                }
                if minv[j] < delta {
                    delta = minv[j];
                    j1 = j;
                }
            }
            for j in 0..=n {
                if used[j] {
                    u[p[j]] += delta;
                    v[j] -= delta;
                } else {
                    minv[j] -= delta;
                }
            }
            j0 = j1;
            if p[j0] == 0 {
                break;
            }
        }
        loop {
            let j1 = way[j0];
            p[j0] = p[j1];
            j0 = j1;
            if j0 == 0 {
                break;
            }
        }
    }
    let mut match_row = vec![None; n];
    for j in 1..=n {
        if p[j] != 0 {
            match_row[p[j] - 1] = Some(j - 1);
        }
    }
    match_row
}

#[cfg(test)]
mod hungarian_tests {
    use super::hungarian;

    #[test]
    fn identity_is_cheapest() {
        let cost = vec![vec![0, 9, 9], vec![9, 0, 9], vec![9, 9, 0]];
        assert_eq!(hungarian(&cost), vec![Some(0), Some(1), Some(2)]);
    }
}

#[cfg(test)]
mod assign_tests {
    use super::*;
    use crate::bundle::bundle;
    use crate::hall::hall;
    use crate::pattern::{PITCH_254, PatternKind, attach_pattern, generate_pattern};
    use crate::types::{BoardId, Contact, ContactId, Ict};

    fn c(id: u32, board: u32, xy: [f64; 2], ict: Ict) -> Contact {
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
    fn fitting_instance_is_injective_and_beta_lands_in_slot() {
        let cs = vec![
            c(0, 0, [40.0, 40.0], Ict::UsbDp),
            c(1, 0, [41.5, 40.0], Ict::UsbDm),
            c(2, 0, [40.0, 43.0], Ict::Vtarget),
            c(3, 0, [10.0, 10.0], Ict::Gnd),
            c(4, 0, [12.0, 10.0], Ict::Swdio),
        ];
        let mut p = bundle(cs).unwrap();
        attach_pattern(&mut p, &generate_pattern(PatternKind::S3, PITCH_254));
        hall(&p).unwrap();
        let a = assign(&p);
        let mut used_slots = std::collections::BTreeSet::new();
        for (did, sid) in &a.demand_to_slot {
            assert!(used_slots.insert(*sid), "α not injective");
            let d = &p.demands[did];
            let s = &p.slots[sid];
            assert_eq!(d.kind, s.kind);
            for cid in &d.members {
                let pid = a.contact_to_pin.get(cid).expect("β missing");
                assert!(s.pins.contains(pid), "β pin not in assigned slot");
            }
        }
        assert_eq!(a.contact_to_pin.len(), 5);
    }
}
