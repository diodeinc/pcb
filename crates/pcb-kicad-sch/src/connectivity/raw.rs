use std::collections::{BTreeMap, BTreeSet};

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

/// An index of direct terminal matches, not equivalence classes: matching by
/// name or number is nontransitive, so aliases must never be unioned together.
#[derive(Default)]
pub(crate) struct TerminalIndex<'a> {
    points: BTreeMap<ConnectionPoint<'a>, Vec<usize>>,
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum ConnectionPoint<'a> {
    PinName(&'a ComponentIdentity, &'a str),
    PinNumber(&'a ComponentIdentity, &'a str),
    InterfacePort(&'a str),
}

fn connection_points(terminal: &Terminal) -> impl Iterator<Item = ConnectionPoint<'_>> {
    let (name, numbers) = match terminal {
        Terminal::ComponentPin {
            component,
            pin_name,
            pin_numbers,
        } => (
            (!pin_name.is_empty()).then_some(ConnectionPoint::PinName(component, pin_name)),
            Some((component, pin_numbers)),
        ),
        Terminal::InterfacePort { name } => (Some(ConnectionPoint::InterfacePort(name)), None),
    };
    name.into_iter()
        .chain(numbers.into_iter().flat_map(|(component, numbers)| {
            numbers
                .iter()
                .map(move |number| ConnectionPoint::PinNumber(component, number))
        }))
}

impl<'a> TerminalIndex<'a> {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn insert(&mut self, terminal: &'a Terminal, index: usize) {
        for point in connection_points(terminal) {
            self.points.entry(point).or_default().push(index);
        }
    }

    /// Return payloads for direct matches. A payload may occur more than once
    /// when terminals share multiple keys or multiple insertions use it.
    pub(crate) fn matching<'b>(
        &'b self,
        terminal: &'b Terminal,
    ) -> impl Iterator<Item = usize> + 'b {
        connection_points(terminal)
            .filter_map(|point| self.points.get(&point))
            .flatten()
            .copied()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_index_matches_brute_force() {
        let mut terminals = Vec::new();
        for component in [
            ComponentIdentity::ManagedPath("U1".into()),
            ComponentIdentity::ManagedPath("U2".into()),
            ComponentIdentity::KiCadSymbol(SymbolLocation {
                page_id: "page-a".into(),
                symbol_id: "U1".into(),
            }),
            ComponentIdentity::KiCadSymbol(SymbolLocation {
                page_id: "page-b".into(),
                symbol_id: "U1".into(),
            }),
            ComponentIdentity::KiCadSymbol(SymbolLocation {
                page_id: "page-a".into(),
                symbol_id: "U2".into(),
            }),
        ] {
            for name in ["", "A", "B", "1"] {
                for numbers in [vec![], vec![""], vec!["1"], vec!["2"], vec!["1", "2"]] {
                    terminals.push(Terminal::ComponentPin {
                        component: component.clone(),
                        pin_name: name.into(),
                        pin_numbers: numbers.into_iter().map(str::to_string).collect(),
                    });
                }
            }
        }
        for name in ["", "A", "B", "1"] {
            terminals.push(Terminal::InterfacePort { name: name.into() });
        }
        let mut index = TerminalIndex::default();
        for (id, terminal) in terminals.iter().enumerate() {
            index.insert(terminal, id);
        }
        for query in &terminals {
            let brute_force = terminals
                .iter()
                .enumerate()
                .filter_map(|(id, terminal)| terminal.matches(query).then_some(id))
                .collect::<BTreeSet<_>>();
            assert_eq!(
                index.matching(query).collect::<BTreeSet<_>>(),
                brute_force,
                "query: {query:?}"
            );
        }
    }

    #[test]
    fn terminal_index_keeps_aliases_nontransitive_and_allows_duplicate_payloads() {
        let pin = |name: &str, numbers: &[&str]| Terminal::ComponentPin {
            component: ComponentIdentity::ManagedPath("U1".into()),
            pin_name: name.into(),
            pin_numbers: numbers.iter().map(|number| (*number).into()).collect(),
        };
        let terminals = [
            pin("A", &["1"]),
            pin("A", &["2", "3"]),
            pin("B", &["2", "3"]),
        ];
        assert!(terminals[0].matches(&terminals[1]));
        assert!(terminals[1].matches(&terminals[2]));
        assert!(!terminals[0].matches(&terminals[2]));
        let mut index = TerminalIndex::new();
        for (id, terminal) in terminals.iter().enumerate() {
            index.insert(terminal, id);
        }
        assert_eq!(
            index.matching(&terminals[0]).collect::<BTreeSet<_>>(),
            BTreeSet::from([0, 1])
        );
        // Name plus two overlapping numbers yields three hits, not one.
        assert_eq!(
            index.matching(&terminals[1]).filter(|&id| id == 1).count(),
            3
        );
        index.insert(&terminals[2], 1);
        assert_eq!(
            index.matching(&terminals[1]).filter(|&id| id == 1).count(),
            5
        );
        // Inserting another alias must not make the unrelated endpoint match.
        assert!(!index.matching(&terminals[0]).any(|id| id == 2));
    }
}
