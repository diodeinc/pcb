//! The fixture plan: which boards one insertion tests, and which mate
//! land carries which contact. [`plan`] is a pure function of the
//! contact list and the constellation — same inputs, same plan, no IO.
//!
//! **Slots** are the supply side, derived from the constellation by
//! connector block. A USB block yields one `Pair` slot (its DP+DM
//! lands, unsplittable); a PWR block yields one `VtBank` slot (its two
//! Vt lands) and one `Vusb` unit slot; each low-speed land is its own
//! `Ls` unit slot. Detect and ground lands are not slots. S13 therefore
//! supplies 8 pair, 8 bank, 8 vusb, and 48 LS slots.
//!
//! **Demands** are the ask side, built per board instance from its
//! contacts: `usb_dp`+`usb_dm` pair up into one `Pair` demand (an
//! unpaired polarity pad degrades to a plain LS demand); `vtarget` pads
//! chunk into `VtBank` demands of up to two; `vusb` and low-speed
//! contacts are unit demands; `gnd` is never a demand — it rides the
//! bottom pour. A panel repeats one board design, so board 0's demand
//! counts are every board's, and the number of testable boards per
//! insertion is `min(slot count ÷ per-board count)` over the kinds.
//! When the panel packs more boards than that, the tested subset is
//! spread evenly across the sheet — a clumped prefix concentrates every
//! net far from half the perimeter and routes badly. A board whose
//! contact would put a pogo pad inside a fixed-feature keep-out (the A7
//! tile's corner tooling can sit under the board field on large sheets)
//! is not selectable.
//!
//! **Assignment** runs per kind: demands and slots of one kind form a
//! square cost matrix (cost = centroid distance, dummy zero-cost rows
//! filling unused slots) solved exactly by the Hungarian algorithm, so
//! each kind's total wire length is minimal. Within a matched slot,
//! pair members take the land of their own polarity and bank members
//! take lands in order. Every binding gets a stable net name,
//! `B{board}.{path}`.

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

/// Compute the plan for a panel's contacts against the S13 lands.
/// `keepouts` are fixed-feature disks (center, radius) a pogo pad must
/// stay out of.
pub fn plan(
    contacts: Vec<PanelContact>,
    lands: &[Land],
    keepouts: &[([f64; 2], f64)],
) -> Result<Plan> {
    if contacts.is_empty() {
        bail!("panel has no ICT contacts; mark test points with the TestPoint `ict` config");
    }
    let slots = derive_slots(lands);
    let capacity = |kind: Kind| slots.iter().filter(|slot| slot.kind == kind).count();

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
    let valid: Vec<u32> = (0..boards_total)
        .filter(|&board| {
            contacts
                .iter()
                .filter(|contact| contact.board == board)
                .all(|contact| {
                    keepouts.iter().all(|(center, radius)| {
                        let dx = contact.xy[0] - center[0];
                        let dy = contact.xy[1] - center[1];
                        (dx * dx + dy * dy).sqrt() >= *radius
                    })
                })
        })
        .collect();
    if valid.is_empty() {
        bail!("every board instance has a contact inside a fixed-feature keep-out");
    }
    let testable = testable.min(valid.len());
    let tested: Vec<u32> = (0..testable)
        .map(|i| valid[i * valid.len() / testable])
        .collect();

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

/// Pick the member's land inside a slot: by polarity in pair slots, in
/// member order everywhere else.
fn land_for(role: Role, member: usize, slot: &Slot, lands: &[Land]) -> usize {
    if slot.kind == Kind::Pair {
        return *slot
            .lands
            .iter()
            .find(|&&land| lands[land].role == role)
            .expect("pair slot carries both polarities");
    }
    slot.lands[member.min(slot.lands.len() - 1)]
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
    fn unpaired_polarity_degrades_to_ls() {
        use crate::contacts::PanelContact;
        let lands = crate::pattern::oriented_s13(74.0, 105.0);
        // One board with a lone usb_dp and no usb_dm.
        let contacts = vec![
            PanelContact {
                board: 0,
                refdes: "TP1".into(),
                path: "TP_DP.TP".into(),
                role: Role::UsbDp,
                xy: [30.0, 50.0],
            },
            PanelContact {
                board: 0,
                refdes: "TP2".into(),
                path: "TP_SWDIO.TP".into(),
                role: Role::Ls,
                xy: [32.0, 50.0],
            },
        ];
        let plan = plan(contacts, &lands, &[]).expect("plans without panicking");
        assert_eq!(plan.bindings.len(), 2);
        for binding in &plan.bindings {
            // Both bind to ordinary LS lands.
            assert_eq!(lands[binding.land.unwrap()].role, Role::Ls);
        }
    }

    #[test]
    fn s13_slot_budget() {
        let lands = crate::pattern::oriented_s13(74.0, 105.0);
        let slots = derive_slots(&lands);
        let count = |kind: Kind| slots.iter().filter(|slot| slot.kind == kind).count();
        assert_eq!(count(Kind::Pair), 8);
        assert_eq!(count(Kind::VtBank), 8);
        assert_eq!(count(Kind::Vusb), 8);
        assert_eq!(count(Kind::Ls), 48);
    }
}
