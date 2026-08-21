use std::collections::BTreeSet;

use crate::SymbolSlotKey;

/// The source-independent electrical model used by schematic analysis.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConnectivityGraph {
    pub components: Vec<ComponentNode>,
    pub groups: Vec<ConnectionGroup>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ComponentNode {
    /// Managed component identity supplied by the source, when available.
    pub managed_slot: Option<SymbolSlotKey>,
    pub origin: ComponentOrigin,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum ComponentOrigin {
    Zener,
    KiCad(SymbolLocation),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SymbolLocation {
    pub page_id: String,
    pub symbol_id: String,
}

/// Identity used to associate a pin terminal with its component.
///
/// Managed paths can be compared across source formats. A KiCad-local identity
/// preserves connectivity for ordinary symbols that do not carry that metadata.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum ComponentIdentity {
    ManagedPath(String),
    KiCadSymbol(SymbolLocation),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConnectionGroup {
    /// Logical labels and power-symbol names that identify this connection.
    /// KiCad sheet-pin and hierarchical-label aliases are topology only and
    /// do not appear here.
    pub names: BTreeSet<String>,
    pub terminals: BTreeSet<Terminal>,
    pub origins: BTreeSet<ConnectionOrigin>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Terminal {
    ComponentPin {
        component: ComponentIdentity,
        pin_name: String,
        pin_numbers: BTreeSet<String>,
    },
    /// A named port at the design boundary, independent of source format.
    InterfacePort { name: String },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConnectionOrigin {
    ZenerNet { name: String },
    KiCadIsland(IslandRef),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct IslandRef {
    pub page_id: String,
    pub index: usize,
}
