//! The fixture plan: which boards one insertion tests, and which mate
//! land carries which contact.
//!
//! Demands per board: the USB pair is one unsplittable ordered demand
//! (`usb_dp` + `usb_dm` onto a kit's DP/DM lands), `vtarget` pads bank up
//! to two onto one kit's Vt column, `vusb` and low-speed contacts are unit
//! demands, and `gnd` is never assigned — it rides the pour. A panel
//! repeats one board design, so capacity is a per-kind count against
//! S11's fixed budget (8 kits, 48 LS lands); when the panel packs more
//! boards than fit, the tested subset is spread evenly across the sheet —
//! a clumped prefix concentrates every net far from half the perimeter.
//!
//! Assignment is per-kind Hungarian matching on demand-to-slot distance,
//! so the plan is deterministic and globally cheapest per kind.

use anyhow::{Result, bail};

use crate::contacts::PanelContact;
use crate::pattern::{Land, Role};

/// One assignment row: a contact bound to a land under a net name.
#[derive(Debug, Clone)]
pub struct Binding {
    /// Index into the plan's `contacts`.
    pub contact: usize,
    /// Index into the constellation's lands; `None` for `gnd` contacts,
    /// which the pour carries.
    pub land: Option<usize>,
    pub net: String,
}

/// The computed fixture plan for one panel.
#[derive(Debug)]
pub struct Plan {
    pub boards_total: u32,
    /// Board instance indices one fixture insertion tests.
    pub tested: Vec<u32>,
    pub contacts: Vec<PanelContact>,
    pub bindings: Vec<Binding>,
}

/// Slot kinds a demand can occupy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Pair,
    VtBank,
    Vusb,
    Ls,
}

/// A slot: one or two lands acting as a unit, with a centroid for costs.
struct Slot {
    kind: Kind,
    lands: Vec<usize>,
    at: [f64; 2],
}

/// A demand: one or two contacts of one board needing a slot of `kind`.
struct DemandRow {
    kind: Kind,
    contacts: Vec<usize>,
    at: [f64; 2],
}

/// Compute the plan for a panel's contacts against the S11 lands.
pub fn plan(contacts: Vec<PanelContact>, lands: &[Land]) -> Result<Plan> {
    if contacts.is_empty() {
        bail!("panel has no ICT contacts; mark test points with the TestPoint `ict` config");
    }
    let slots = derive_slots(lands);
    let capacity = |kind: Kind| slots.iter().filter(|slot| slot.kind == kind).count();

    // Per-board demand template — every instance repeats the same design,
    // so board 0's demands are everyone's.
    let boards_total = contacts.iter().map(|c| c.board).max().unwrap_or(0) + 1;
    let per_board = board_demands(&contacts, 0);
    let mut testable = boards_total as usize;
    for kind in [Kind::Pair, Kind::VtBank, Kind::Vusb, Kind::Ls] {
        let need = per_board
            .iter()
            .filter(|demand| demand.kind == kind)
            .count();
        if need > 0 {
            let cap = capacity(kind);
            if cap < need {
                bail!(
                    "one board needs {need} {kind:?} slots but the constellation has {cap}; \
                     the panel is untestable"
                );
            }
            testable = testable.min(cap / need);
        }
    }
    // Spread the tested subset across the sheet.
    let tested: Vec<u32> = (0..testable)
        .map(|i| (i * boards_total as usize / testable) as u32)
        .collect();

    // Demands for every tested board, then per-kind Hungarian.
    let mut demands: Vec<DemandRow> = Vec::new();
    for &board in &tested {
        demands.extend(board_demands(&contacts, board));
    }
    let mut bindings: Vec<Binding> = Vec::new();
    for kind in [Kind::Pair, Kind::VtBank, Kind::Vusb, Kind::Ls] {
        let kind_demands: Vec<&DemandRow> = demands.iter().filter(|d| d.kind == kind).collect();
        let kind_slots: Vec<&Slot> = slots.iter().filter(|s| s.kind == kind).collect();
        if kind_demands.is_empty() {
            continue;
        }
        // Square cost matrix: dummy demand rows fill unused slots at zero
        // cost, so real demands always win their cheapest real slot.
        let n = kind_slots.len();
        let cost: Vec<Vec<i64>> = (0..n)
            .map(|i| {
                (0..n)
                    .map(|j| match kind_demands.get(i) {
                        Some(demand) => {
                            let dx = demand.at[0] - kind_slots[j].at[0];
                            let dy = demand.at[1] - kind_slots[j].at[1];
                            ((dx * dx + dy * dy).sqrt() * 1000.0) as i64
                        }
                        None => 0,
                    })
                    .collect()
            })
            .collect();
        let matching = hungarian(&cost);
        for (i, demand) in kind_demands.iter().enumerate() {
            let slot = kind_slots[matching[i].expect("square matching is total")];
            for (member, contact_index) in demand.contacts.iter().enumerate() {
                let contact = &contacts[*contact_index];
                bindings.push(Binding {
                    contact: *contact_index,
                    land: Some(land_for(contact.role, member, slot, lands)),
                    net: net_name(contact),
                });
            }
        }
    }
    // GND contacts of tested boards ride the pour.
    for (index, contact) in contacts.iter().enumerate() {
        if contact.role == Role::Gnd && tested.contains(&contact.board) {
            bindings.push(Binding {
                contact: index,
                land: None,
                net: "GND".into(),
            });
        }
    }
    bindings.sort_by_key(|binding| binding.contact);

    Ok(Plan {
        boards_total,
        tested,
        contacts,
        bindings,
    })
}

fn net_name(contact: &PanelContact) -> String {
    let name = if contact.path.is_empty() {
        contact.refdes.clone()
    } else {
        contact.path.clone()
    };
    format!("B{}.{}", contact.board, name)
}

/// Pick the member's land inside a slot: DP/DM by role, bank members in
/// land order, units directly.
fn land_for(role: Role, member: usize, slot: &Slot, lands: &[Land]) -> usize {
    match role {
        Role::UsbDp | Role::UsbDm => *slot
            .lands
            .iter()
            .find(|&&land| lands[land].role == role)
            .expect("pair slot carries both polarities"),
        _ => slot.lands[member.min(slot.lands.len() - 1)],
    }
}

/// Group the constellation into slots by connector block.
fn derive_slots(lands: &[Land]) -> Vec<Slot> {
    let blocks = lands.iter().map(|land| land.block).max().unwrap_or(0) + 1;
    let mut slots = Vec::new();
    for block in 0..blocks {
        let members: Vec<usize> = (0..lands.len())
            .filter(|&i| lands[i].block == block)
            .collect();
        let of = |role: Role| -> Vec<usize> {
            members
                .iter()
                .copied()
                .filter(|&i| lands[i].role == role)
                .collect()
        };
        let centroid = |ids: &[usize]| -> [f64; 2] {
            let n = ids.len().max(1) as f64;
            [
                ids.iter().map(|&i| lands[i].xy[0]).sum::<f64>() / n,
                ids.iter().map(|&i| lands[i].xy[1]).sum::<f64>() / n,
            ]
        };
        let (dp, dm) = (of(Role::UsbDp), of(Role::UsbDm));
        if let (Some(&dp), Some(&dm)) = (dp.first(), dm.first()) {
            slots.push(Slot {
                kind: Kind::Pair,
                at: centroid(&[dp, dm]),
                lands: vec![dp, dm],
            });
        }
        let vt = of(Role::Vtarget);
        if !vt.is_empty() {
            slots.push(Slot {
                kind: Kind::VtBank,
                at: centroid(&vt),
                lands: vt,
            });
        }
        for land in of(Role::Vusb) {
            slots.push(Slot {
                kind: Kind::Vusb,
                at: lands[land].xy,
                lands: vec![land],
            });
        }
        for land in of(Role::Ls) {
            slots.push(Slot {
                kind: Kind::Ls,
                at: lands[land].xy,
                lands: vec![land],
            });
        }
    }
    slots
}

/// One board instance's demands, in a deterministic order.
fn board_demands(contacts: &[PanelContact], board: u32) -> Vec<DemandRow> {
    let mine: Vec<usize> = (0..contacts.len())
        .filter(|&i| contacts[i].board == board)
        .collect();
    let of = |role: Role| -> Vec<usize> {
        mine.iter()
            .copied()
            .filter(|&i| contacts[i].role == role)
            .collect()
    };
    let centroid = |ids: &[usize]| -> [f64; 2] {
        let n = ids.len().max(1) as f64;
        [
            ids.iter().map(|&i| contacts[i].xy[0]).sum::<f64>() / n,
            ids.iter().map(|&i| contacts[i].xy[1]).sum::<f64>() / n,
        ]
    };

    let mut demands = Vec::new();
    let (dp, dm) = (of(Role::UsbDp), of(Role::UsbDm));
    let pairs = dp.len().min(dm.len());
    for pair in 0..pairs {
        let members = vec![dp[pair], dm[pair]];
        demands.push(DemandRow {
            kind: Kind::Pair,
            at: centroid(&members),
            contacts: members,
        });
    }
    // Unpaired polarity pads degrade to low-speed contacts.
    let mut ls: Vec<usize> = of(Role::Ls);
    ls.extend(dp.into_iter().skip(pairs));
    ls.extend(dm.into_iter().skip(pairs));
    for chunk in of(Role::Vtarget).chunks(2) {
        demands.push(DemandRow {
            kind: Kind::VtBank,
            at: centroid(chunk),
            contacts: chunk.to_vec(),
        });
    }
    for contact in of(Role::Vusb) {
        demands.push(DemandRow {
            kind: Kind::Vusb,
            at: contacts[contact].xy,
            contacts: vec![contact],
        });
    }
    for contact in ls {
        demands.push(DemandRow {
            kind: Kind::Ls,
            at: contacts[contact].xy,
            contacts: vec![contact],
        });
    }
    demands
}

/// Square-matrix Hungarian algorithm (Jonker–Volgenant potentials);
/// returns each row's matched column. O(n³), exact.
fn hungarian(cost: &[Vec<i64>]) -> Vec<Option<usize>> {
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

/// Serialize a plan as the fixture-map JSON document.
pub fn to_json(plan: &Plan, panel: &crate::panel::Panel, lands: &[Land]) -> String {
    let bindings: Vec<serde_json::Value> = plan
        .bindings
        .iter()
        .map(|binding| {
            let contact = &plan.contacts[binding.contact];
            let land = binding.land.map(|index| {
                let land = &lands[index];
                serde_json::json!({
                    "index": index,
                    "x": land.xy[0],
                    "y": land.xy[1],
                    "role": land.role.name(),
                    "block": land.block,
                })
            });
            serde_json::json!({
                "board": contact.board,
                "refdes": contact.refdes,
                "path": contact.path,
                "role": contact.role.name(),
                "x": contact.xy[0],
                "y": contact.xy[1],
                "net": binding.net,
                "land": land,
            })
        })
        .collect();
    let doc = serde_json::json!({
        "panel": { "width": panel.width, "height": panel.height },
        "boards_total": plan.boards_total,
        "boards_tested": plan.tested,
        "bindings": bindings,
    });
    serde_json::to_string_pretty(&doc).expect("fixture map serializes") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_matching_is_cheapest() {
        let cost = vec![vec![0, 9, 9], vec![9, 0, 9], vec![9, 9, 0]];
        assert_eq!(hungarian(&cost), vec![Some(0), Some(1), Some(2)]);
    }

    #[test]
    fn s11_slot_budget() {
        let lands = crate::pattern::oriented_s11(74.0, 105.0);
        let slots = derive_slots(&lands);
        let count = |kind: Kind| slots.iter().filter(|slot| slot.kind == kind).count();
        assert_eq!(count(Kind::Pair), 8);
        assert_eq!(count(Kind::VtBank), 8);
        assert_eq!(count(Kind::Vusb), 8);
        assert_eq!(count(Kind::Ls), 48);
    }
}
