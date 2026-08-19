//! Shared routing types: two-pin nets, emitted traces, and route results.
//!
//! The router itself lives in [`crate::router`] (R5). The earlier maze
//! routers (R1–R4) proved connectivity and were deleted once R5 matched
//! their coverage with legal, clean copper.

use std::collections::{HashMap, HashSet};

use crate::types::{Assign, BoardId, ContactId, Kind, MatePinId, Problem};

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
    /// Mid-route vias. Always empty under the terminal-via model.
    pub vias: Vec<[f64; 2]>,
    /// The net's single layer-change via, placed in one of its own pads.
    pub term_via: Option<[f64; 2]>,
    /// Copper width of the emitted trace (mm).
    pub width: f64,
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
    /// Self-DRC clearance violations over the final geometry.
    pub drc_violations: usize,
    /// Smallest observed copper-to-copper gap (mm).
    pub min_gap_mm: f64,
    /// USB pairs that could not be routed as a parallel ribbon and fell back
    /// to two individual traces.
    pub loose_pairs: usize,
    /// Contacts belonging to loose pairs (excluded from ribbon UΔ).
    pub loose_contacts: Vec<ContactId>,
    /// GND stitching: per poured contact, an optional top stub from the pogo
    /// pad and the via position dropping into the bottom pour.
    pub gnd_stitches: Vec<(ContactId, Vec<[f64; 2]>, [f64; 2])>,
    /// GND land stitching: per GND mate land, an optional bottom stub and
    /// the via position joining the bottom fragment to the top pour.
    pub gnd_land_stitches: Vec<(MatePinId, Vec<[f64; 2]>, [f64; 2])>,
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

/// One two-pin net per assigned contact, with the class width the router
/// will use. Only used for scoring and visualization; the router derives its
/// own routables (pairs collapse to a centerline) from the assignment.
pub fn nets_from_assign(problem: &Problem, assign: &Assign) -> Vec<TwoPinNet> {
    let mut nets = Vec::new();
    for (cid, pid) in &assign.contact_to_pin {
        let c = &problem.contacts[cid];
        let p = &problem.pins[pid];
        let Some(kind) = c.ict.kind() else {
            continue;
        };
        let width = match kind {
            Kind::UsbHs => crate::router::USB_W,
            Kind::Vtarget | Kind::Vusb => crate::router::POWER_W,
            Kind::Gnd => 0.3,
            Kind::Ls => crate::router::LS_W,
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

pub(crate) fn kind_pri(k: Kind) -> u8 {
    match k {
        Kind::UsbHs => 0,
        Kind::Vtarget => 1,
        Kind::Vusb => 2,
        Kind::Gnd => 3,
        Kind::Ls => 4,
    }
}
