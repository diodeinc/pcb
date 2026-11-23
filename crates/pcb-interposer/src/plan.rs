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
//! tile's corner tooling can sit under the board field on large sheets),
//! or outside the bed's envelope, is not selectable.
//!
//! **Assignment** runs per kind: demands and slots of one kind form a
//! square cost matrix (cost = centroid distance, dummy zero-cost rows
//! filling unused slots) solved exactly by the Hungarian algorithm, so
//! each kind's total wire length is minimal. Within a matched slot,
//! pair members take the land of their own polarity and bank members
//! take lands in order. `analog` contacts ride S13's generic LS lands.
//!
//! The **Switchyard** bed instead gives each tested board whole per-DUT
//! cells: as many as its signal contacts (USB, digital, analog) need
//! against one cell's lanes, so a wide board halves (or worse) the
//! boards one insertion tests. Board copies are matched to cells by the
//! same Hungarian solve over centroid distances. The board's supplies
//! and SWD use only its primary cell, the one nearest those contacts: a
//! DUT's VTarget or VUSB pads are one DUT net, and binding them to two
//! cells would tie two tester supplies together. Each USB pair (a DP and
//! its nearest DM) stays inside one cell. Every contact takes a land of
//! its own role — digital roles never reach the analog-only lands — and
//! a contact with no free land of its role is dropped, not failed. A
//! cell's VTarget lands are one bed net, so a board's VTarget contacts
//! and lands form one net, `B{board}.VTARGET`.
//!
//! Every binding gets a stable net name, `B{board}.{path}`.

use anyhow::{Result, bail};

use crate::bed::Bed;
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
    /// Per tested board, the bed cells it spans, primary first; empty on
    /// beds without cells.
    pub cells: Vec<Vec<u32>>,
    pub contacts: Vec<PanelContact>,
    pub bindings: Vec<Binding>,
    /// Contacts of tested boards that found no land (indices into
    /// `contacts`).
    pub dropped: Vec<usize>,
    /// Lands bound to a net with no contact of their own: every VTarget
    /// land of a block one of the board's VTarget contacts uses, so the
    /// board draws its current through all of them in parallel
    /// ([`parallel_vtarget`]). (land index, net name).
    pub paralleled: Vec<(usize, String)>,
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

/// Compute the plan for a panel's contacts against a bed's lands.
/// `keepouts` are fixed-feature disks (center, radius) a pogo pad must
/// stay out of.
pub fn plan(
    contacts: Vec<PanelContact>,
    lands: &[Land],
    keepouts: &[([f64; 2], f64)],
    envelope: Option<[f64; 4]>,
    bed: Bed,
) -> Result<Plan> {
    if contacts.is_empty() {
        bail!("panel has no ICT contacts; mark test points with the TestPoint `ict` config");
    }
    // A pogo over the bed's dowels or screws cannot be built at all, on
    // any board instance: fail rather than leave the board untested.
    let pogo = crate::pogo::COURTYARD_RADIUS_MM;
    let blocked: Vec<String> = contacts
        .iter()
        .flat_map(|contact| {
            bed.alignment().into_iter().filter_map(move |feature| {
                let reach = feature.keepout_dia / 2.0 + pogo;
                let gap = distance(contact.xy, feature.at);
                (gap < reach).then(|| {
                    format!(
                        "board {} {} ({}) at ({:.2}, {:.2}) is {gap:.2} mm from the bed's {} {} \
                         hole at ({:.2}, {:.2}); its pogo needs {reach:.2} mm",
                        contact.board,
                        contact.refdes,
                        contact.path,
                        contact.xy[0],
                        contact.xy[1],
                        feature.name,
                        if feature.clamps { "screw" } else { "alignment" },
                        feature.at[0],
                        feature.at[1],
                    )
                })
            })
        })
        .collect();
    if !blocked.is_empty() {
        bail!(
            "{} contact{} would put a pogo in a bed alignment or screw keep-out:\n  {}",
            blocked.len(),
            if blocked.len() == 1 { "" } else { "s" },
            blocked.join("\n  ")
        );
    }
    let boards_total = contacts.iter().map(|c| c.board).max().unwrap_or(0) + 1;
    // A pogo's whole courtyard must sit inside the bed's envelope.
    let inside = |xy: [f64; 2]| {
        envelope.is_none_or(|[x0, y0, x1, y1]| {
            let r = crate::pogo::COURTYARD_RADIUS_MM;
            xy[0] - r >= x0 && xy[0] + r <= x1 && xy[1] - r >= y0 && xy[1] + r <= y1
        })
    };
    let valid: Vec<u32> = (0..boards_total)
        .filter(|&board| {
            contacts
                .iter()
                .filter(|contact| contact.board == board)
                .all(|contact| {
                    inside(contact.xy)
                        && keepouts
                            .iter()
                            .all(|(center, radius)| distance(contact.xy, *center) >= *radius)
                })
        })
        .collect();
    if valid.is_empty() {
        bail!(
            "every board instance has a contact inside a fixed-feature keep-out or outside the \
             bed's envelope"
        );
    }

    let (tested, cells, mut bindings, dropped) = match bed {
        Bed::S13 => {
            let (tested, bindings, dropped) = by_kind(&contacts, lands, &valid)?;
            (tested, Vec::new(), bindings, dropped)
        }
        Bed::SwitchyardRev1 => by_cell(&contacts, lands, &valid),
    };
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
    let paralleled = parallel_vtarget(&contacts, &bindings, lands);

    Ok(Plan {
        boards_total,
        tested,
        cells,
        contacts,
        bindings,
        dropped,
        paralleled,
    })
}

/// The VTarget lands no contact took, in each block where one did, bound
/// to that land's net. A block's VTarget lands are one supply on the bed
/// (a Switchyard cell's two VT pogos, an S13 PWR bank), so a board with a
/// single VTarget test point still feeds through both pogos.
fn parallel_vtarget(
    contacts: &[PanelContact],
    bindings: &[Binding],
    lands: &[Land],
) -> Vec<(usize, String)> {
    let mut used: Vec<(u32, &str)> = Vec::new();
    for binding in bindings {
        if contacts[binding.contact].role != Role::Vtarget {
            continue;
        }
        if let Some(land) = binding.land
            && !used.iter().any(|&(block, _)| block == lands[land].block)
        {
            used.push((lands[land].block, binding.net.as_str()));
        }
    }
    let bound: Vec<usize> = bindings.iter().filter_map(|binding| binding.land).collect();
    (0..lands.len())
        .filter(|&land| lands[land].role == Role::Vtarget && !bound.contains(&land))
        .filter_map(|land| {
            used.iter()
                .find(|&&(block, _)| block == lands[land].block)
                .map(|&(_, net)| (land, net.to_string()))
        })
        .collect()
}

/// `testable` of the valid boards, spread evenly across the sheet.
fn spread(valid: &[u32], testable: usize) -> Vec<u32> {
    (0..testable)
        .map(|i| valid[i * valid.len() / testable])
        .collect()
}

/// S13: slots and demands matched per kind across the whole
/// constellation. Returns the tested boards and their non-GND bindings.
fn by_kind(
    contacts: &[PanelContact],
    lands: &[Land],
    valid: &[u32],
) -> Result<(Vec<u32>, Vec<Binding>, Vec<usize>)> {
    let slots = derive_slots(lands);
    let capacity = |kind: Kind| slots.iter().filter(|slot| slot.kind == kind).count();

    let per_board = board_demands(contacts, 0);
    let mut testable = valid.len();
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
    let tested = spread(valid, testable);

    let mut demands: Vec<DemandRow> = Vec::new();
    for &board in &tested {
        demands.extend(board_demands(contacts, board));
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
                        Some(demand) => cost_of(demand.at, kind_slots[j].at),
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
    Ok((tested, bindings, Vec::new()))
}

/// Switchyard: each tested board takes `span` whole cells (blocks with
/// Vtarget lands), as many as its signal contacts need, board copies
/// matched to cells by centroid distance. The board's supplies and SWD
/// use only its primary cell, the one nearest those contacts, so the
/// DUT never ties two cells' supplies together; a USB pair stays inside
/// one cell. Every contact takes a land of its own role, SWCLK and SWDIO
/// the primary's S0 and S1; a contact whose role has no free land left
/// is dropped. Returns the tested boards, their cells (primary first),
/// their non-GND bindings, and the dropped contacts.
fn by_cell(
    contacts: &[PanelContact],
    lands: &[Land],
    valid: &[u32],
) -> (Vec<u32>, Vec<Vec<u32>>, Vec<Binding>, Vec<usize>) {
    let mut cells: Vec<u32> = lands
        .iter()
        .filter(|land| land.role == Role::Vtarget)
        .map(|land| land.block)
        .collect();
    cells.sort_unstable();
    cells.dedup();
    let n = cells.len();

    let cell_lands = |cell: u32| -> Vec<usize> {
        (0..lands.len())
            .filter(|&i| lands[i].block == cell)
            .collect()
    };
    let board_contacts = |board: u32| -> Vec<usize> {
        (0..contacts.len())
            .filter(|&i| contacts[i].board == board)
            .collect()
    };
    let board_at = |board: u32| centroid(board_contacts(board).iter().map(|&c| contacts[c].xy));
    let supply = |role: Role| matches!(role, Role::Vtarget | Role::Vusb);

    // The cells are alike and the boards one design, so one board's
    // signal counts against one cell's lanes size every board's span.
    let span = match (valid.first(), cells.first()) {
        (Some(&board), Some(&cell)) => {
            let lanes = cell_lands(cell);
            [Role::UsbDp, Role::UsbDm, Role::Ls, Role::Analog]
                .into_iter()
                .map(|role| {
                    let need = board_contacts(board)
                        .iter()
                        .filter(|&&c| contacts[c].role == role)
                        .count();
                    let cap = lanes.iter().filter(|&&l| lands[l].role == role).count();
                    if cap == 0 { 1 } else { need.div_ceil(cap) }
                })
                .max()
                .unwrap_or(1)
                .clamp(1, n)
        }
        _ => 1,
    };
    let tested = spread(valid, (n / span).min(valid.len()));

    // One row per board copy; zero-cost dummies square the matrix.
    let cell_at: Vec<[f64; 2]> = cells
        .iter()
        .map(|&cell| centroid(cell_lands(cell).iter().map(|&i| lands[i].xy)))
        .collect();
    let rows: Vec<[f64; 2]> = tested
        .iter()
        .flat_map(|&board| std::iter::repeat_n(board_at(board), span))
        .collect();
    let cost: Vec<Vec<i64>> = (0..n)
        .map(|i| {
            (0..n)
                .map(|j| rows.get(i).map_or(0, |&at| cost_of(at, cell_at[j])))
                .collect()
        })
        .collect();
    let matching = hungarian(&cost);

    let mut board_cells = Vec::new();
    let mut bindings = Vec::new();
    let mut dropped = Vec::new();
    for (i, &board) in tested.iter().enumerate() {
        let mut mine: Vec<usize> = board_contacts(board)
            .into_iter()
            .filter(|&c| contacts[c].role != Role::Gnd)
            .collect();
        let mut own: Vec<usize> = (i * span..(i + 1) * span)
            .map(|row| matching[row].expect("square matching is total"))
            .collect();
        // The primary is the cell whose own lands the anchors (supplies
        // and SWD) reach most cheaply, each in its track's copper: a
        // VTarget contact runs to the middle of the cell's VTarget lands
        // (it feeds both) in the wide power track, VLogic to the cell's
        // VL land, SWCLK and SWDIO to its S0 and S1.
        let anchor_cost = |j: usize| -> i64 {
            let cell = cells[j];
            let at = |pick: &dyn Fn(&Land) -> bool| -> Option<[f64; 2]> {
                let points: Vec<[f64; 2]> = cell_lands(cell)
                    .into_iter()
                    .filter(|&l| pick(&lands[l]))
                    .map(|l| lands[l].xy)
                    .collect();
                (!points.is_empty()).then(|| centroid(points.into_iter()))
            };
            mine.iter()
                .filter_map(|&c| {
                    let contact = &contacts[c];
                    let (target, weight) = match (contact.role, contact.ict.as_str()) {
                        (Role::Vtarget, _) => (at(&|l| l.role == Role::Vtarget), VTARGET_WEIGHT),
                        (Role::Vusb, _) => (at(&|l| l.role == Role::Vusb), 1.0),
                        (_, "swclk") => (at(&|l| l.label == "S0"), 1.0),
                        (_, "swdio") => (at(&|l| l.label == "S1"), 1.0),
                        _ => return None,
                    };
                    target.map(|t| (weight * cost_of(contact.xy, t) as f64) as i64)
                })
                .sum()
        };
        own.sort_by_key(|&j| {
            (
                anchor_cost(j),
                cost_of(board_at(board), cell_at[j]),
                cells[j],
            )
        });
        let own: Vec<u32> = own.into_iter().map(|j| cells[j]).collect();
        let primary = own[0];
        let mut free: Vec<usize> = own
            .iter()
            .flat_map(|&cell| cell_lands(cell))
            .filter(|&l| lands[l].block == primary || !supply(lands[l].role))
            .collect();

        // The cell's VTarget lands are one bed net, so the board's
        // VTarget contacts share one interposer net too: any pogo can feed
        // any land, and the board's current splits over every path.
        let mut bind = |contact: usize, land: usize| {
            let net = match contacts[contact].role {
                Role::Vtarget => format!("B{board}.VTARGET"),
                _ => net_name(&contacts[contact]),
            };
            bindings.push(Binding {
                contact,
                land: Some(land),
                net,
            });
        };
        let take = |list: &mut Vec<usize>, item: usize| {
            let at = list.iter().position(|&x| x == item).expect("listed");
            list.remove(at)
        };
        for (ict, label) in [("swclk", "S0"), ("swdio", "S1")] {
            let contact = mine.iter().copied().find(|&c| contacts[c].ict == ict);
            let land = free
                .iter()
                .copied()
                .find(|&l| lands[l].block == primary && lands[l].label == label);
            if let (Some(contact), Some(land)) = (contact, land) {
                bind(take(&mut mine, contact), take(&mut free, land));
            }
        }

        // USB pairs, each DP with its nearest DM, matched whole to cells.
        let mut dms: Vec<usize> = mine
            .iter()
            .copied()
            .filter(|&c| contacts[c].role == Role::UsbDm)
            .collect();
        let mut pairs: Vec<(usize, usize)> = Vec::new();
        for dp in mine
            .iter()
            .copied()
            .filter(|&c| contacts[c].role == Role::UsbDp)
        {
            if let Some(dm) = dms
                .iter()
                .copied()
                .min_by_key(|&dm| cost_of(contacts[dp].xy, contacts[dm].xy))
            {
                take(&mut dms, dm);
                pairs.push((dp, dm));
            }
        }
        let ports: Vec<(usize, usize)> = own
            .iter()
            .filter_map(|&cell| {
                let find = |role: Role| {
                    free.iter()
                        .copied()
                        .find(|&l| lands[l].block == cell && lands[l].role == role)
                };
                Some((find(Role::UsbDp)?, find(Role::UsbDm)?))
            })
            .collect();
        if !pairs.is_empty() && !ports.is_empty() {
            let m = pairs.len().max(ports.len());
            let cost: Vec<Vec<i64>> = (0..m)
                .map(|i| {
                    (0..m)
                        .map(|j| match (pairs.get(i), ports.get(j)) {
                            (Some(&(dp, dm)), Some(&(p, q))) => {
                                cost_of(contacts[dp].xy, lands[p].xy)
                                    + cost_of(contacts[dm].xy, lands[q].xy)
                            }
                            _ => 0,
                        })
                        .collect()
                })
                .collect();
            let matching = hungarian(&cost);
            for (i, &(dp, dm)) in pairs.iter().enumerate() {
                if let Some(&(p, q)) = ports.get(matching[i].expect("square matching is total")) {
                    bind(take(&mut mine, dp), take(&mut free, p));
                    bind(take(&mut mine, dm), take(&mut free, q));
                }
            }
        }

        for role in [
            Role::UsbDp,
            Role::UsbDm,
            Role::Vtarget,
            Role::Vusb,
            Role::Ls,
            Role::Analog,
        ] {
            let want: Vec<usize> = mine
                .iter()
                .copied()
                .filter(|&c| contacts[c].role == role)
                .collect();
            if want.is_empty() {
                continue;
            }
            let have: Vec<usize> = free
                .iter()
                .copied()
                .filter(|&l| lands[l].role == role)
                .collect();
            // Square with zero-cost dummies; a contact matched to a dummy
            // land has nowhere to go.
            let m = want.len().max(have.len());
            let cost: Vec<Vec<i64>> = (0..m)
                .map(|i| {
                    (0..m)
                        .map(|j| match (want.get(i), have.get(j)) {
                            (Some(&c), Some(&l)) => cost_of(contacts[c].xy, lands[l].xy),
                            _ => 0,
                        })
                        .collect()
                })
                .collect();
            let matching = hungarian(&cost);
            for (i, &contact) in want.iter().enumerate() {
                match have.get(matching[i].expect("square matching is total")) {
                    Some(&land) => bind(contact, land),
                    None => dropped.push(contact),
                }
            }
        }
        board_cells.push(own);
    }
    dropped.sort_unstable();
    (tested, board_cells, bindings, dropped)
}

fn distance(a: [f64; 2], b: [f64; 2]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

/// How much more a millimetre of VTarget track costs than one of signal
/// track when choosing a board's primary cell: their width ratio, since
/// the wide track takes that much more routing room.
const VTARGET_WEIGHT: f64 = crate::emit::POWER_TRACK_MM / crate::emit::SIGNAL_TRACK_MM;

/// Integer matching cost: distance in µm.
fn cost_of(a: [f64; 2], b: [f64; 2]) -> i64 {
    (distance(a, b) * 1000.0) as i64
}

fn centroid(points: impl Iterator<Item = [f64; 2]>) -> [f64; 2] {
    let (mut sum, mut n) = ([0.0, 0.0], 0.0);
    for p in points {
        sum[0] += p[0];
        sum[1] += p[1];
        n += 1.0;
    }
    let n = f64::max(n, 1.0);
    [sum[0] / n, sum[1] / n]
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
    ls.extend(of(Role::Analog));
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
pub fn to_json(
    plan: &Plan,
    panel: &crate::panel::Panel,
    lands: &[Land],
    placement: Option<&crate::single::Placement>,
) -> String {
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
        "cells": plan.cells,
        // A single board's frame on the sheet: sheet = R(quarter_turns ·
        // 90°) · M · (x, -y) + offset, M negating x face down.
        "placement": placement.map(|p| serde_json::json!({
            "quarter_turns": p.quarter_turns,
            "mirrored": p.mirrored,
            "offset": p.offset,
        })),
        "bindings": bindings,
        // VTarget lands fed in parallel with a bound one, no contact of
        // their own.
        "paralleled": plan.paralleled.iter().map(|(index, net)| {
            let land = &lands[*index];
            serde_json::json!({
                "net": net,
                "land": {
                    "index": index,
                    "x": land.xy[0],
                    "y": land.xy[1],
                    "role": land.role.name(),
                    "block": land.block,
                },
            })
        }).collect::<Vec<_>>(),
        "dropped": plan.dropped.iter().map(|&contact| {
            let contact = &plan.contacts[contact];
            serde_json::json!({
                "board": contact.board,
                "refdes": contact.refdes,
                "path": contact.path,
                "role": contact.ict,
            })
        }).collect::<Vec<_>>(),
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
                ict: "usb_dp".into(),
                xy: [30.0, 50.0],
            },
            PanelContact {
                board: 0,
                refdes: "TP2".into(),
                path: "TP_SWDIO.TP".into(),
                role: Role::Ls,
                ict: "swdio".into(),
                xy: [32.0, 50.0],
            },
        ];
        let plan = plan(contacts, &lands, &[], None, Bed::S13).expect("plans without panicking");
        assert_eq!(plan.bindings.len(), 2);
        for binding in &plan.bindings {
            // Both bind to ordinary LS lands.
            assert_eq!(lands[binding.land.unwrap()].role, Role::Ls);
        }
    }

    #[test]
    fn pogo_in_an_alignment_or_screw_keepout_fails() {
        let lands = Bed::SwitchyardRev1.lands(105.0, 74.0).unwrap();
        let contact = |xy| PanelContact {
            board: 2,
            refdes: "TP7".into(),
            path: "TP_SWDIO.TP".into(),
            role: Role::Ls,
            ict: "swdio".into(),
            xy,
        };
        // The MH7 screw at (52.5, 31.25): Ø6 keep-out plus the pogo's
        // 1.5 mm courtyard.
        let err = plan(
            vec![contact([52.5, 35.7])],
            &lands,
            &[],
            Bed::SwitchyardRev1.envelope(),
            Bed::SwitchyardRev1,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("board 2 TP7 (TP_SWDIO.TP)"), "{err}");
        assert!(err.contains("MH7 screw hole"), "{err}");
        let err = plan(
            vec![contact([28.5, 28.0])],
            &lands,
            &[],
            Bed::SwitchyardRev1.envelope(),
            Bed::SwitchyardRev1,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("DW1 alignment hole"), "{err}");
        // Clear of every feature: plans (the contact is just untested
        // where no cell takes it).
        assert!(
            plan(
                vec![contact([52.5, 35.8])],
                &lands,
                &[],
                Bed::SwitchyardRev1.envelope(),
                Bed::SwitchyardRev1
            )
            .is_ok()
        );
    }

    #[test]
    fn switchyard_cells_bind_by_role() {
        let lands = Bed::SwitchyardRev1.lands(105.0, 74.0).unwrap();
        // One board over cell 0 (column R1, inner rows): the rev-1 contact
        // set plus a third VTarget, which has no VT land left; supplies
        // never claim a second cell.
        let roles = [
            ("vtarget", Role::Vtarget),
            ("vtarget", Role::Vtarget),
            ("vtarget", Role::Vtarget),
            ("vusb", Role::Vusb),
            ("gnd", Role::Gnd),
            ("usb_dp", Role::UsbDp),
            ("usb_dm", Role::UsbDm),
            ("swdio", Role::Ls),
            ("swclk", Role::Ls),
            ("nrst", Role::Ls),
            ("ls", Role::Ls),
            ("analog", Role::Analog),
            ("analog", Role::Analog),
        ];
        let contacts: Vec<PanelContact> = roles
            .iter()
            .enumerate()
            .map(|(i, (ict, role))| PanelContact {
                board: 0,
                refdes: format!("TP{}", i + 1),
                path: format!("TP{}.TP", i + 1),
                role: *role,
                ict: (*ict).into(),
                xy: [80.0 + i as f64, 50.0],
            })
            .collect();
        let plan = plan(
            contacts,
            &lands,
            &[],
            Bed::SwitchyardRev1.envelope(),
            Bed::SwitchyardRev1,
        )
        .unwrap();
        assert_eq!(plan.tested, vec![0]);
        assert_eq!(plan.cells, vec![vec![0]]);
        assert_eq!(plan.dropped.len(), 1);
        assert_eq!(plan.contacts[plan.dropped[0]].role, Role::Vtarget);
        let land_of = |ict: &str| {
            plan.bindings
                .iter()
                .find(|b| plan.contacts[b.contact].ict == ict)
                .and_then(|b| b.land)
                .map(|land| lands[land])
                .unwrap()
        };
        // VTarget contacts share one net.
        let vt: Vec<&str> = plan
            .bindings
            .iter()
            .filter(|b| plan.contacts[b.contact].role == Role::Vtarget)
            .map(|b| b.net.as_str())
            .collect();
        assert_eq!(vt, ["B0.VTARGET", "B0.VTARGET"]);
        // Nearest cell, SWD on its default lanes.
        assert_eq!(land_of("swclk").block, 0);
        assert_eq!(land_of("swclk").label, "S0");
        assert_eq!(land_of("swdio").label, "S1");
        for binding in &plan.bindings {
            let contact = &plan.contacts[binding.contact];
            match binding.land {
                None => assert_eq!(contact.role, Role::Gnd),
                // Role-faithful: digital never lands on S4/S5.
                Some(land) => assert_eq!(lands[land].role, contact.role),
            }
        }
    }

    /// fynkit-label: one VTarget test point 7 mm from cell 6's VT lands,
    /// SWD 5–6 mm from cell 4's S0/S1, five digital contacts so the board
    /// spans both. The primary follows the wide VTarget track to cell 6,
    /// and its single VTarget contact feeds both of the cell's VT lands.
    #[test]
    fn switchyard_primary_follows_vtarget_and_parallels_both_lands() {
        let lands = Bed::SwitchyardRev1.lands(105.0, 74.0).unwrap();
        let contacts: Vec<PanelContact> = [
            ("vtarget", Role::Vtarget, [13.75, 62.2]),
            ("gnd", Role::Gnd, [13.75, 58.8]),
            ("swclk", Role::Ls, [42.85, 41.0]),
            ("swdio", Role::Ls, [43.65, 45.4]),
            ("nrst", Role::Ls, [40.45, 45.5]),
            ("ls", Role::Ls, [29.05, 49.9]),
            ("ls", Role::Ls, [50.45, 54.7]),
        ]
        .iter()
        .enumerate()
        .map(|(i, &(ict, role, xy))| PanelContact {
            board: 0,
            refdes: format!("TP{}", i + 1),
            path: format!("TP{}.TP", i + 1),
            role,
            ict: ict.into(),
            xy,
        })
        .collect();
        let plan = plan(
            contacts,
            &lands,
            &[],
            Bed::SwitchyardRev1.envelope(),
            Bed::SwitchyardRev1,
        )
        .unwrap();
        assert_eq!(plan.cells, vec![vec![6, 4]]);
        let vt: Vec<usize> = plan
            .bindings
            .iter()
            .filter(|b| plan.contacts[b.contact].role == Role::Vtarget)
            .filter_map(|b| b.land)
            .chain(plan.paralleled.iter().map(|(land, _)| *land))
            .collect();
        assert_eq!(vt.len(), 2);
        assert!(vt.iter().all(|&l| lands[l].block == 6 && lands[l].role == Role::Vtarget));
        assert!(plan.paralleled.iter().all(|(_, net)| net == "B0.VTARGET"));
        assert!(plan.dropped.is_empty());
    }

    /// A board with more signals than one cell's lanes, repeated on a
    /// panel with more boards than cells.
    fn wide_board(boards: u32) -> Vec<PanelContact> {
        let roles = [
            ("vtarget", Role::Vtarget),
            ("vusb", Role::Vusb),
            ("gnd", Role::Gnd),
            ("usb_dp", Role::UsbDp),
            ("usb_dm", Role::UsbDm),
            ("usb_dp", Role::UsbDp),
            ("usb_dm", Role::UsbDm),
            ("swclk", Role::Ls),
            ("swdio", Role::Ls),
            ("ls", Role::Ls),
            ("ls", Role::Ls),
            ("ls", Role::Ls),
            ("analog", Role::Analog),
        ];
        (0..boards)
            .flat_map(|board| {
                roles
                    .iter()
                    .enumerate()
                    .map(move |(i, (ict, role))| PanelContact {
                        board,
                        refdes: format!("TP{}", i + 1),
                        path: format!("TP{}.TP", i + 1),
                        role: *role,
                        ict: (*ict).into(),
                        // Two USB ports far apart on each board: DP/DM at
                        // i = 3/4 and 5/6.
                        xy: [
                            10.0 + 22.0 * (board % 4) as f64
                                + if i == 5 || i == 6 { 15.0 } else { i as f64 },
                            20.0 + 30.0 * (board / 4) as f64
                                + if i == 4 || i == 6 { 1.0 } else { 0.0 },
                        ],
                    })
            })
            .collect()
    }

    #[test]
    fn switchyard_board_spans_cells() {
        let lands = Bed::SwitchyardRev1.lands(105.0, 74.0).unwrap();
        let plan = plan(
            wide_board(8),
            &lands,
            &[],
            Bed::SwitchyardRev1.envelope(),
            Bed::SwitchyardRev1,
        )
        .unwrap();
        // Two USB pairs and five digital lines need two cells a board, so
        // eight cells test four of the eight boards.
        assert_eq!(plan.tested.len(), 4);
        assert!(plan.dropped.is_empty(), "{:?}", plan.dropped);
        let mut all: Vec<u32> = plan.cells.iter().flatten().copied().collect();
        all.sort_unstable();
        assert_eq!(all, (0..8).collect::<Vec<_>>());
        for (&board, own) in plan.tested.iter().zip(&plan.cells) {
            assert_eq!(own.len(), 2);
            let land_of = |contact: usize| {
                plan.bindings
                    .iter()
                    .find(|b| b.contact == contact)
                    .and_then(|b| b.land)
                    .map(|land| lands[land])
            };
            let mine = |role: Role| -> Vec<usize> {
                (0..plan.contacts.len())
                    .filter(|&c| plan.contacts[c].board == board && plan.contacts[c].role == role)
                    .collect()
            };
            // Supplies and SWD on the primary cell only.
            for role in [Role::Vtarget, Role::Vusb] {
                for c in mine(role) {
                    assert_eq!(land_of(c).unwrap().block, own[0]);
                }
            }
            for c in mine(Role::Ls) {
                let land = land_of(c).unwrap();
                match plan.contacts[c].ict.as_str() {
                    "swclk" => assert_eq!((land.block, land.label), (own[0], "S0")),
                    "swdio" => assert_eq!((land.block, land.label), (own[0], "S1")),
                    _ => {}
                }
            }
            // Each USB pair shares one cell, the two pairs different ones.
            let dp = mine(Role::UsbDp);
            let dm = mine(Role::UsbDm);
            for (&p, &m) in dp.iter().zip(&dm) {
                assert_eq!(land_of(p).unwrap().block, land_of(m).unwrap().block);
            }
            assert_ne!(land_of(dp[0]).unwrap().block, land_of(dp[1]).unwrap().block);
            // No secondary-cell supply land carries anything.
            for binding in &plan.bindings {
                if let Some(land) = binding.land
                    && lands[land].block == own[1]
                {
                    assert!(!matches!(lands[land].role, Role::Vtarget | Role::Vusb));
                }
            }
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
