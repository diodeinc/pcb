//! Source-independent electrical connectivity.
//!
//! The KiCad and Zener frontends independently reduce their source models to
//! the shared graph. Analysis can then compare graphs without either frontend
//! depending on the other source format.

mod kicad;
mod raw;
mod zener;

pub use kicad::ConnectivityItemRef;
pub(crate) use kicad::{IslandProvenance, reduce_visible_with_provenance};

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
        kicad::reduce(document)
    }
}
