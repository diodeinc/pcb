//! Source-independent electrical connectivity.
//!
//! The KiCad and Zener frontends independently reduce their source models to
//! the shared graph. Analysis can then compare graphs without either frontend
//! depending on the other source format.

pub(crate) mod kicad;
mod raw;
mod zener;

pub use kicad::{ConnectivityItemRef, PhysicalConnectivity, PhysicalIsland, PinVisibility};
pub(crate) use kicad::{PhysicalPinRef, reduce_with_provenance};
pub(crate) use zener::{named_connected_nets, not_connected_terminals};

pub use raw::{
    ComponentIdentity, ComponentNode, ComponentOrigin, ConnectionGroup, ConnectionOrigin,
    ConnectivityGraph, IslandRef, SymbolLocation, Terminal,
};

use pcb_sch::Schematic;

use crate::SchDocument;

impl ConnectivityGraph {
    pub fn from_zener(netlist: &Schematic) -> anyhow::Result<Self> {
        zener::reduce(netlist)
    }

    pub fn from_kicad(document: &SchDocument) -> anyhow::Result<Self> {
        // Standalone analysis includes hidden pins to match KiCad's implicit
        // hidden-power-pin connectivity. Reconciliation uses visible pins only.
        Ok(reduce_with_provenance(document, PinVisibility::IncludeHidden)?.graph)
    }
}

impl PhysicalConnectivity {
    pub fn from_kicad(
        document: &SchDocument,
        pin_visibility: PinVisibility,
    ) -> anyhow::Result<Self> {
        reduce_with_provenance(document, pin_visibility)
    }
}
