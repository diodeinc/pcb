//! In-memory model from the exploration note: Contact / Demand / Slot / MatePin.

use std::collections::BTreeMap;
use std::str::FromStr;

use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContactId(pub u32);
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DemandId(pub u32);
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SlotId(pub u32);
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MatePinId(pub u32);
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BoardId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ict {
    Swdio,
    Swclk,
    Nrst,
    Swo,
    Gnd,
    Vusb,
    Vtarget,
    UsbDp,
    UsbDm,
    Ls,
    Swd,
}

impl Ict {
    pub fn parse_name(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "swdio" => Some(Self::Swdio),
            "swclk" => Some(Self::Swclk),
            "nrst" => Some(Self::Nrst),
            "swo" => Some(Self::Swo),
            "gnd" => Some(Self::Gnd),
            "vusb" => Some(Self::Vusb),
            "vtarget" => Some(Self::Vtarget),
            "usb_dp" => Some(Self::UsbDp),
            "usb_dm" => Some(Self::UsbDm),
            "ls" => Some(Self::Ls),
            "swd" => Some(Self::Swd),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Swdio => "swdio",
            Self::Swclk => "swclk",
            Self::Nrst => "nrst",
            Self::Swo => "swo",
            Self::Gnd => "gnd",
            Self::Vusb => "vusb",
            Self::Vtarget => "vtarget",
            Self::UsbDp => "usb_dp",
            Self::UsbDm => "usb_dm",
            Self::Ls => "ls",
            Self::Swd => "swd",
        }
    }

    pub fn kind(self) -> Option<Kind> {
        Some(match self {
            Self::Swdio | Self::Swclk | Self::Nrst | Self::Swo | Self::Ls => Kind::Ls,
            Self::Gnd => Kind::Gnd,
            Self::Vusb => Kind::Vusb,
            Self::Vtarget => Kind::Vtarget,
            Self::UsbDp | Self::UsbDm => Kind::UsbHs,
            Self::Swd => return None,
        })
    }
}

impl FromStr for Ict {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse_name(s).ok_or(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Ls,
    Gnd,
    Vusb,
    Vtarget,
    UsbHs,
}

impl Kind {
    pub const ALL: [Kind; 5] = [Kind::UsbHs, Kind::Vtarget, Kind::Vusb, Kind::Gnd, Kind::Ls];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ls => "ls",
            Self::Gnd => "gnd",
            Self::Vusb => "vusb",
            Self::Vtarget => "vtarget",
            Self::UsbHs => "usb_hs",
        }
    }

    pub fn is_unit(self) -> bool {
        matches!(self, Self::Ls | Self::Gnd | Self::Vusb)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Unit,
    Unordered { n: u8 },
    Ordered { n: u8 },
}

impl Shape {
    pub fn capacity(self) -> usize {
        match self {
            Self::Unit => 1,
            Self::Unordered { n } | Self::Ordered { n } => n as usize,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Contact {
    pub id: ContactId,
    pub board: BoardId,
    pub xy: [f64; 2],
    pub ict: Ict,
    pub path: String,
    pub package: String,
    pub side: String,
}

#[derive(Debug, Clone)]
pub struct Demand {
    pub id: DemandId,
    pub kind: Kind,
    pub board: BoardId,
    pub members: Vec<ContactId>,
}

#[derive(Debug, Clone)]
pub struct MatePin {
    pub id: MatePinId,
    pub xy: [f64; 2],
}

#[derive(Debug, Clone)]
pub struct Slot {
    pub id: SlotId,
    pub kind: Kind,
    pub shape: Shape,
    pub pins: Vec<MatePinId>,
}

/// Panel-level fixed features the interposer inherits from the assembly
/// panel (plus the folded A7 tile's tooling): NPTH tooling holes and
/// two-sided global fiducials. Fixed obstacles for routing, emitted as
/// real footprints.
#[derive(Debug, Clone, Default)]
pub struct PanelSpec {
    /// NPTH tooling holes: (center, drill diameter).
    pub holes: Vec<([f64; 2], f64)>,
    /// Global fiducial centers (Ø1 copper dot, Ø2 mask opening) per face.
    pub fids_top: Vec<[f64; 2]>,
    pub fids_bottom: Vec<[f64; 2]>,
}

#[derive(Debug, Clone, Default)]
pub struct Problem {
    pub contacts: BTreeMap<ContactId, Contact>,
    pub demands: BTreeMap<DemandId, Demand>,
    pub pins: BTreeMap<MatePinId, MatePin>,
    pub slots: BTreeMap<SlotId, Slot>,
    pub panel: PanelSpec,
}

impl Problem {
    pub fn demands_of(&self, kind: Kind) -> Vec<DemandId> {
        self.demands
            .values()
            .filter(|d| d.kind == kind)
            .map(|d| d.id)
            .collect()
    }

    pub fn slots_of(&self, kind: Kind) -> Vec<SlotId> {
        self.slots
            .values()
            .filter(|s| s.kind == kind)
            .map(|s| s.id)
            .collect()
    }
}

#[derive(Debug, Clone, Default)]
pub struct Assign {
    pub demand_to_slot: BTreeMap<DemandId, SlotId>,
    pub contact_to_pin: BTreeMap<ContactId, MatePinId>,
    pub cost_mm: f64,
}

#[derive(Debug, Error)]
pub enum InterposerError {
    #[error("unpaired USB on board {board:?}: dp={dp} dm={dm}")]
    UnpairedUsb {
        board: BoardId,
        dp: usize,
        dm: usize,
    },
    #[error("board {board:?} has {count} vtarget pads; max is 2")]
    TooManyVtarget { board: BoardId, count: usize },
    #[error(
        "Hall failed for {kind:?}: {demands} demands need {need} capacity, {slots} slots have {have}"
    )]
    Hall {
        kind: Kind,
        demands: usize,
        need: usize,
        slots: usize,
        have: usize,
    },
    #[error("{0}")]
    Other(String),
}

pub fn dist(a: [f64; 2], b: [f64; 2]) -> f64 {
    let dx = a[0] - b[0];
    let dy = a[1] - b[1];
    (dx * dx + dy * dy).sqrt()
}
