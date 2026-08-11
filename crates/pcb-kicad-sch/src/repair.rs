//! Pure planning for KiCad connectivity repairs.

use std::collections::BTreeSet;

use anyhow::{Result, bail};
use pcb_sch::Schematic;

use crate::{
    SchDocument, SchItem, SchPage,
    analysis::{
        SchematicIssue, analyze_connectivity, expected_reconcilable_connectivity, logical_name,
        observed_reconcilable_connectivity, terminals_match,
    },
    connectivity::{ConnectivityGraph, ConnectivityItemRef, IslandProvenance, Terminal},
};

/// A deterministic, UUID-addressed connectivity repair decision.
///
/// Planning does not mutate the input document. Callers can inspect the plan,
/// apply it to a clone, and verify the result before persisting any changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectivityRepairPlan {
    removals: BTreeSet<ConnectivityItemRef>,
    reconnect_nets: BTreeSet<String>,
}

impl ConnectivityRepairPlan {
    pub fn removals(&self) -> &BTreeSet<ConnectivityItemRef> {
        &self.removals
    }

    pub fn reconnect_nets(&self) -> &BTreeSet<String> {
        &self.reconnect_nets
    }
}

/// Plan the smallest supported unambiguous repair for an existing schematic.
///
/// Unexpected net names have an exact label-driver repair. A physical short
/// must have one uniquely proven wire or junction removal; more destructive or
/// multi-item choices are reported instead of guessed.
pub fn plan_connectivity_repair(
    document: &SchDocument,
    netlist: &Schematic,
) -> Result<ConnectivityRepairPlan> {
    plan_connectivity_repair_with(document, netlist, false)
}

pub(crate) fn plan_connectivity_repair_with(
    document: &SchDocument,
    netlist: &Schematic,
    initialize_all_nets: bool,
) -> Result<ConnectivityRepairPlan> {
    let expected = expected_reconcilable_connectivity(document, netlist)?;
    let observed = observed_reconcilable_connectivity(document, netlist)?;
    let analysis = analyze_connectivity(&expected, &observed.graph);
    let mut removals = BTreeSet::new();
    let mut reconnect_nets = if initialize_all_nets {
        netlist
            .nets
            .values()
            .filter(|net| net.kind != "NotConnected" && !net.name.is_empty())
            .map(|net| net.name.clone())
            .collect()
    } else {
        BTreeSet::new()
    };

    for issue in analysis.issues() {
        match issue {
            SchematicIssue::DisconnectedNet { net_name, .. } => {
                reconnect_nets.insert(net_name.clone());
            }
            SchematicIssue::UnexpectedNet { net_name, islands } => {
                let mut drivers = BTreeSet::new();
                for island in islands {
                    if let Some(provenance) = observed.islands.get(island) {
                        if let Some(items) = provenance.named_drivers.get(net_name) {
                            drivers.extend(items.iter().filter(|item| item.is_label()).cloned());
                        }
                        reconnect_nets.extend(expected_names_for_island(&expected, provenance));
                    }
                }
                if drivers.is_empty() {
                    bail!("unexpected KiCad net '{net_name}' is not driven by a removable label");
                }
                removals.extend(drivers);
            }
            SchematicIssue::Shorted { islands, net_names } => {
                reconnect_nets.extend(net_names.iter().cloned());
                for island in islands {
                    if let Some(provenance) = observed.islands.get(island) {
                        reconnect_nets.extend(expected_names_for_island(&expected, provenance));
                    }
                }
            }
            SchematicIssue::UnexpectedConnection { islands, .. } => {
                for island in islands {
                    if let Some(provenance) = observed.islands.get(island) {
                        reconnect_nets.extend(expected_names_for_island(&expected, provenance));
                    }
                }
            }
            SchematicIssue::UnboundSymbol { .. }
            | SchematicIssue::MissingSymbol { .. }
            | SchematicIssue::DuplicateSymbol { .. }
            | SchematicIssue::MismatchedSymbolId { .. }
            | SchematicIssue::UnexpectedSymbol { .. } => {
                bail!("component reconciliation did not resolve schematic issue: {issue:?}")
            }
        }
    }

    // Remove exact unexpected name drivers first. This can also resolve a
    // short without requiring a physical topology edit.
    let mut simulated = document.clone();
    remove_items(&mut simulated, &removals)?;

    loop {
        let current_observed = observed_reconcilable_connectivity(&simulated, netlist)?;
        let current_analysis = analyze_connectivity(&expected, &current_observed.graph);
        let current_problems = repair_problems(current_analysis.issues());
        let Some(issue) = current_analysis.issues().iter().find(|issue| {
            matches!(
                issue,
                SchematicIssue::Shorted { .. } | SchematicIssue::UnexpectedConnection { .. }
            )
        }) else {
            break;
        };

        let candidates = repair_candidates(issue, &expected, &current_observed.islands);
        let mut valid = Vec::new();
        for candidate in candidates {
            let mut next = simulated.clone();
            remove_items(&mut next, &BTreeSet::from([candidate.clone()]))?;
            let next_observed = observed_reconcilable_connectivity(&next, netlist)?;
            let next_analysis = analyze_connectivity(&expected, &next_observed.graph);
            let next_problems = repair_problems(next_analysis.issues());
            if next_problems.is_subset(&current_problems)
                && next_problems.len() < current_problems.len()
            {
                valid.push(candidate);
            }
        }

        let candidate = match valid.as_slice() {
            [candidate] => candidate.clone(),
            [] => return Err(unrepairable_issue(document, issue)),
            _ => bail!(
                "KiCad connectivity has multiple equally minimal repairs: {}; no changes were planned",
                valid
                    .iter()
                    .map(|item| format!("{item:?}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
        remove_items(&mut simulated, &BTreeSet::from([candidate.clone()]))?;
        removals.insert(candidate);
    }

    let after_removal = analyze_connectivity(
        &expected,
        &observed_reconcilable_connectivity(&simulated, netlist)?.graph,
    );
    for issue in after_removal.issues() {
        match issue {
            SchematicIssue::DisconnectedNet { net_name, .. } => {
                reconnect_nets.insert(net_name.clone());
            }
            SchematicIssue::UnexpectedNet { net_name, .. } => {
                bail!("removing unexpected net drivers left unexpected KiCad net '{net_name}'")
            }
            SchematicIssue::Shorted { .. } | SchematicIssue::UnexpectedConnection { .. } => {
                unreachable!("repair loop exits only after physical connectivity issues are gone")
            }
            SchematicIssue::UnboundSymbol { .. }
            | SchematicIssue::MissingSymbol { .. }
            | SchematicIssue::DuplicateSymbol { .. }
            | SchematicIssue::MismatchedSymbolId { .. }
            | SchematicIssue::UnexpectedSymbol { .. } => {
                bail!("component reconciliation did not resolve schematic issue: {issue:?}")
            }
        }
    }

    Ok(ConnectivityRepairPlan {
        removals,
        reconnect_nets,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum RepairProblem {
    UnexpectedNet(String),
    Shorted(String, String),
    UnexpectedConnection(Terminal),
}

fn repair_problems(issues: &[SchematicIssue]) -> BTreeSet<RepairProblem> {
    let mut result = BTreeSet::new();
    for issue in issues {
        match issue {
            SchematicIssue::UnexpectedNet { net_name, .. } => {
                result.insert(RepairProblem::UnexpectedNet(net_name.clone()));
            }
            SchematicIssue::Shorted { net_names, .. } => {
                let names = net_names.iter().collect::<Vec<_>>();
                for (index, left) in names.iter().enumerate() {
                    for right in &names[index + 1..] {
                        result.insert(RepairProblem::Shorted((*left).clone(), (*right).clone()));
                    }
                }
            }
            SchematicIssue::UnexpectedConnection { terminals, .. } => {
                result.extend(
                    terminals
                        .iter()
                        .cloned()
                        .map(RepairProblem::UnexpectedConnection),
                );
            }
            SchematicIssue::DisconnectedNet { .. }
            | SchematicIssue::UnboundSymbol { .. }
            | SchematicIssue::MissingSymbol { .. }
            | SchematicIssue::DuplicateSymbol { .. }
            | SchematicIssue::MismatchedSymbolId { .. }
            | SchematicIssue::UnexpectedSymbol { .. } => {}
        }
    }
    result
}

fn repair_candidates(
    issue: &SchematicIssue,
    expected: &ConnectivityGraph,
    islands: &std::collections::BTreeMap<crate::connectivity::IslandRef, IslandProvenance>,
) -> BTreeSet<ConnectivityItemRef> {
    let issue_islands = match issue {
        SchematicIssue::Shorted { islands, .. }
        | SchematicIssue::UnexpectedConnection { islands, .. } => islands,
        _ => return BTreeSet::new(),
    };
    issue_islands
        .iter()
        .filter_map(|island| islands.get(island))
        .filter(|island| repair_island(issue, expected, island))
        .flat_map(|island| island.items.iter())
        .filter(|item| item.is_physical_connector())
        .cloned()
        .collect()
}

fn repair_island(
    issue: &SchematicIssue,
    expected: &ConnectivityGraph,
    island: &IslandProvenance,
) -> bool {
    match issue {
        SchematicIssue::Shorted { net_names, .. } => expected_names_for_island(expected, island)
            .intersection(net_names)
            .nth(1)
            .is_some(),
        SchematicIssue::UnexpectedConnection { terminals, .. } => terminals
            .iter()
            .any(|terminal| island.terminals.contains(terminal)),
        _ => false,
    }
}

fn unrepairable_issue(document: &SchDocument, issue: &SchematicIssue) -> anyhow::Error {
    match issue {
        SchematicIssue::UnexpectedConnection { terminals, .. } => {
            let descriptions = terminals
                .iter()
                .map(|terminal| describe_terminal(document, terminal))
                .collect::<Vec<_>>()
                .join(", ");
            anyhow::anyhow!(
                "KiCad directly connects component terminals that should be separate: {descriptions}. pcb apply cannot identify one wire or junction whose removal repairs the connection"
            )
        }
        SchematicIssue::Shorted { net_names, .. } => anyhow::anyhow!(
            "KiCad shorts nets {} but no single wire or junction removal repairs the short",
            net_names.iter().cloned().collect::<Vec<_>>().join(", ")
        ),
        _ => anyhow::anyhow!("KiCad connectivity issue cannot be repaired: {issue:?}"),
    }
}

fn describe_terminal(document: &SchDocument, terminal: &Terminal) -> String {
    match terminal {
        Terminal::InterfacePort { name } => format!("interface port {name}"),
        Terminal::ComponentPin {
            component,
            pin_name,
            pin_numbers,
        } => {
            let pin = if pin_name.trim().is_empty() {
                format!(
                    "pin {}",
                    pin_numbers.iter().cloned().collect::<Vec<_>>().join("/")
                )
            } else if pin_numbers.is_empty() {
                format!("pin {pin_name}")
            } else {
                format!(
                    "pin {pin_name} ({})",
                    pin_numbers.iter().cloned().collect::<Vec<_>>().join("/")
                )
            };
            match component {
                crate::connectivity::ComponentIdentity::ManagedPath(path) => {
                    format!("{path} {pin}")
                }
                crate::connectivity::ComponentIdentity::KiCadSymbol(location) => {
                    let symbol = document
                        .pages
                        .iter()
                        .find(|page| page.id == location.page_id)
                        .and_then(|page| {
                            page.items.iter().find_map(|item| match item {
                                SchItem::Symbol(symbol) if symbol.id == location.symbol_id => {
                                    Some(symbol)
                                }
                                _ => None,
                            })
                        });
                    let name = symbol
                        .and_then(crate::Symbol::reference)
                        .filter(|reference| !reference.trim().is_empty())
                        .or_else(|| symbol.map(|symbol| symbol.lib_id.as_str()))
                        .unwrap_or("unnamed symbol");
                    format!("{name} {pin} (UUID {})", location.symbol_id)
                }
            }
        }
    }
}

fn expected_names_for_island(
    expected: &ConnectivityGraph,
    island: &IslandProvenance,
) -> BTreeSet<String> {
    expected
        .groups
        .iter()
        .filter(|group| {
            !group.names.is_disjoint(&island.names)
                || group.terminals.iter().any(|expected_terminal| {
                    island.terminals.iter().any(|observed_terminal| {
                        terminals_match(expected_terminal, observed_terminal)
                    })
                })
        })
        .filter_map(logical_name)
        .map(str::to_string)
        .collect()
}

pub(crate) fn remove_items(
    document: &mut SchDocument,
    removals: &BTreeSet<ConnectivityItemRef>,
) -> Result<()> {
    for removal in removals {
        let count = document
            .pages
            .iter()
            .map(|page| matching_item_count(page, removal))
            .sum::<usize>();
        if count != 1 {
            bail!("connectivity item {removal:?} matched {count} loaded schematic items");
        }
    }
    for page in &mut document.pages {
        page.items.retain_mut(|item| {
            for removal in removals {
                if let ConnectivityItemRef::SheetPin {
                    page_id,
                    sheet_id,
                    pin_id,
                } = removal
                    && page_id == &page.id
                    && let SchItem::Sheet(sheet) = item
                    && &sheet.id == sheet_id
                {
                    sheet.pins.retain(|pin| &pin.id != pin_id);
                    return true;
                }
                if item_matches(&page.id, item, removal) {
                    return false;
                }
            }
            true
        });
    }
    Ok(())
}

fn matching_item_count(page: &SchPage, item_ref: &ConnectivityItemRef) -> usize {
    if let ConnectivityItemRef::SheetPin {
        page_id,
        sheet_id,
        pin_id,
    } = item_ref
    {
        if page_id != &page.id {
            return 0;
        }
        return page
            .items
            .iter()
            .filter_map(|item| match item {
                SchItem::Sheet(sheet) if &sheet.id == sheet_id => Some(sheet),
                _ => None,
            })
            .flat_map(|sheet| &sheet.pins)
            .filter(|pin| &pin.id == pin_id)
            .count();
    }
    page.items
        .iter()
        .filter(|item| item_matches(&page.id, item, item_ref))
        .count()
}

fn item_matches(page_id: &str, item: &SchItem, item_ref: &ConnectivityItemRef) -> bool {
    match (item, item_ref) {
        (SchItem::Symbol(item), ConnectivityItemRef::Symbol { page_id: page, id }) => {
            page == page_id && &item.id == id
        }
        (SchItem::Wire(item), ConnectivityItemRef::Wire { page_id: page, id }) => {
            page == page_id && &item.id == id
        }
        (SchItem::Junction(item), ConnectivityItemRef::Junction { page_id: page, id }) => {
            page == page_id && &item.id == id
        }
        (SchItem::NoConnect(item), ConnectivityItemRef::NoConnect { page_id: page, id }) => {
            page == page_id && &item.id == id
        }
        (SchItem::Label(item), ConnectivityItemRef::Label { page_id: page, id }) => {
            page == page_id && &item.id == id
        }
        _ => false,
    }
}

impl ConnectivityItemRef {
    fn is_label(&self) -> bool {
        matches!(self, Self::Label { .. })
    }

    fn is_physical_connector(&self) -> bool {
        matches!(self, Self::Wire { .. } | Self::Junction { .. })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::connectivity::{ConnectionGroup, ConnectionOrigin, IslandRef};

    #[test]
    fn short_candidates_exclude_uninvolved_globally_merged_islands() {
        let expected = expected_graph(&["A", "B"]);
        let causal = island(0);
        let uninvolved = island(1);
        let islands = BTreeMap::from([
            (causal.clone(), provenance(&["A", "B"], wire("bridge"))),
            (uninvolved.clone(), provenance(&["A"], wire("unrelated"))),
        ]);
        let issue = SchematicIssue::Shorted {
            islands: vec![causal, uninvolved],
            net_names: BTreeSet::from(["A".to_string(), "B".to_string()]),
        };

        assert_eq!(
            repair_candidates(&issue, &expected, &islands),
            BTreeSet::from([wire("bridge")])
        );
    }

    #[test]
    fn unexpected_connection_candidates_stay_with_the_unexpected_terminal() {
        let causal = island(0);
        let uninvolved = island(1);
        let islands = BTreeMap::from([
            (
                causal.clone(),
                provenance(&["EXTRA"], wire("unexpected-connection")),
            ),
            (uninvolved.clone(), provenance(&["A"], wire("unrelated"))),
        ]);
        let issue = SchematicIssue::UnexpectedConnection {
            islands: vec![causal, uninvolved],
            terminals: vec![terminal("EXTRA")],
        };

        assert_eq!(
            repair_candidates(&issue, &ConnectivityGraph::default(), &islands),
            BTreeSet::from([wire("unexpected-connection")])
        );
    }

    fn expected_graph(names: &[&str]) -> ConnectivityGraph {
        ConnectivityGraph {
            components: Vec::new(),
            groups: names
                .iter()
                .map(|name| ConnectionGroup {
                    names: BTreeSet::new(),
                    terminals: BTreeSet::from([terminal(name)]),
                    origins: BTreeSet::from([ConnectionOrigin::ZenerNet {
                        name: (*name).to_string(),
                    }]),
                })
                .collect(),
        }
    }

    fn provenance(terminals: &[&str], item: ConnectivityItemRef) -> IslandProvenance {
        IslandProvenance {
            items: BTreeSet::from([item]),
            terminals: terminals.iter().map(|name| terminal(name)).collect(),
            ..IslandProvenance::default()
        }
    }

    fn terminal(name: &str) -> Terminal {
        Terminal::InterfacePort {
            name: name.to_string(),
        }
    }

    fn island(index: usize) -> IslandRef {
        IslandRef {
            page_id: "page".to_string(),
            index,
        }
    }

    fn wire(id: &str) -> ConnectivityItemRef {
        ConnectivityItemRef::Wire {
            page_id: "page".to_string(),
            id: id.to_string(),
        }
    }
}
