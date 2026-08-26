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

impl Terminal {
    /// Whether two terminals name the same connection point. Sources differ in
    /// what they record for a pin (zener ports carry a pin name, KiCad symbols
    /// may leave names empty), so pins compare by component identity plus a
    /// shared pin number, or a shared non-empty pin name.
    pub fn matches(&self, other: &Terminal) -> bool {
        match (self, other) {
            (
                Terminal::ComponentPin {
                    component,
                    pin_name,
                    pin_numbers,
                },
                Terminal::ComponentPin {
                    component: other_component,
                    pin_name: other_name,
                    pin_numbers: other_numbers,
                },
            ) => {
                component == other_component
                    && ((!pin_name.is_empty() && !other_name.is_empty() && pin_name == other_name)
                        || !pin_numbers.is_disjoint(other_numbers))
            }
            (Terminal::InterfacePort { name }, Terminal::InterfacePort { name: other }) => {
                name == other
            }
            _ => false,
        }
    }
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
