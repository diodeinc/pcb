//! Pure planning for KiCad connectivity repairs.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail};
use pcb_sch::Schematic;

use crate::{
    SchDocument, SchItem, SchPage,
    analysis::{
        ConnectivityInspection, SchematicIssue, SchematicIssueKey, analyze_connectivity,
        issue_context, logical_name, observed_reconcilable_connectivity,
    },
    connectivity::{
        ComponentIdentity, ConnectivityGraph, ConnectivityItemRef, PhysicalIsland, SymbolLocation,
        Terminal,
    },
};

/// A deterministic, UUID-addressed connectivity repair decision.
///
/// Planning does not mutate the input document. Callers can inspect the plan,
/// apply it to a clone, and verify the result before persisting any changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectivityRepairIntent {
    pub(crate) removals: BTreeSet<ConnectivityItemRef>,
    pub(crate) relocate_symbols: BTreeSet<SymbolLocation>,
    pub(crate) reconnect_nets: BTreeSet<String>,
}

impl ConnectivityRepairIntent {
    /// Existing connectivity items that the repair will remove.
    pub fn removals(&self) -> &BTreeSet<ConnectivityItemRef> {
        &self.removals
    }

    /// Component symbols that the repair will move away from an invalid overlap.
    pub fn relocated_symbols(&self) -> &BTreeSet<SymbolLocation> {
        &self.relocate_symbols
    }

    /// Nets whose expected connectivity the repair will regenerate.
    pub fn reconnect_nets(&self) -> &BTreeSet<String> {
        &self.reconnect_nets
    }
}

/// Plans the pure connectivity-recovery intent for a set of inspected issues.
pub fn plan_connectivity_repair(
    document: &SchDocument,
    netlist: &Schematic,
    inspection: &ConnectivityInspection,
    selected_keys: &BTreeSet<SchematicIssueKey>,
) -> Result<ConnectivityRepairIntent> {
    let expected = &inspection.expected;
    let observed = &inspection.physical;
    let mut removals = BTreeSet::new();
    let mut relocate_symbols = BTreeSet::new();
    let selected = inspection
        .issues
        .iter()
        .filter(|issue| selected_keys.contains(&issue.key))
        .collect::<Vec<_>>();
    if selected.len() != selected_keys.len() {
        let found = selected
            .iter()
            .map(|issue| issue.key.clone())
            .collect::<BTreeSet<_>>();
        let missing = selected_keys.difference(&found).collect::<Vec<_>>();
        bail!("schematic issues are not present: {missing:?}");
    }
    let selected_items = selected
        .iter()
        .flat_map(|issue| issue.items.iter().cloned())
        .collect::<BTreeSet<_>>();
    let mut reconnect_nets = BTreeSet::new();

    for context in &selected {
        match &context.issue {
            SchematicIssue::DisconnectedNet { net_name, .. }
            | SchematicIssue::MissingPort { net_name, .. } => {
                reconnect_nets.insert(net_name.clone());
            }
            SchematicIssue::UnexpectedNet { net_name, islands } => {
                let mut drivers = BTreeSet::new();
                for island in islands {
                    if let Some(provenance) = observed.islands.get(island) {
                        if let Some(items) = provenance.named_drivers.get(net_name) {
                            drivers.extend(
                                items
                                    .iter()
                                    .filter(|item| item.is_removable_name_driver())
                                    .cloned(),
                            );
                        }
                        reconnect_nets.extend(expected_names_for_island(expected, provenance));
                    }
                }
                if drivers.is_empty() {
                    bail!(
                        "unexpected KiCad net '{net_name}' is not driven by a removable label or power symbol"
                    );
                }
                removals.extend(drivers);
            }
            SchematicIssue::Shorted { islands, net_names } => {
                reconnect_nets.extend(net_names.iter().cloned());
                for island in islands {
                    if let Some(provenance) = observed.islands.get(island) {
                        reconnect_nets.extend(expected_names_for_island(expected, provenance));
                    }
                }
            }
            SchematicIssue::UnexpectedConnection { islands, .. } => {
                for island in islands {
                    if let Some(provenance) = observed.islands.get(island) {
                        reconnect_nets.extend(expected_names_for_island(expected, provenance));
                    }
                }
            }
            SchematicIssue::UnboundSymbol { .. }
            | SchematicIssue::MissingSymbol { .. }
            | SchematicIssue::DuplicateSymbol { .. }
            | SchematicIssue::MismatchedSymbolId { .. }
            | SchematicIssue::UnexpectedSymbol { .. } => {}
        }
    }

    // Remove exact unexpected name drivers first. This can also resolve a
    // short without requiring a physical topology edit.
    let mut simulated = document.clone();
    remove_items(&mut simulated, &removals)?;

    loop {
        let current_observed = observed_reconcilable_connectivity(&simulated, netlist)?;
        let current_analysis = analyze_connectivity(expected, &current_observed.graph);
        let current_problems = repair_problem_counts(current_analysis.issues());
        let Some(issue) = current_analysis.issues().iter().find(|issue| {
            if !matches!(
                issue,
                SchematicIssue::Shorted { .. } | SchematicIssue::UnexpectedConnection { .. }
            ) {
                return false;
            }
            let context = issue_context((*issue).clone(), &current_observed.islands);
            selected.iter().any(|selected| &selected.issue == *issue)
                || !context.items.is_disjoint(&selected_items)
        }) else {
            break;
        };

        // Whatever gets dismantled, its nets get rebuilt — including issues
        // repaired only because they overlap the selection.
        if let SchematicIssue::Shorted { net_names, .. } = issue {
            reconnect_nets.extend(net_names.iter().cloned());
        }
        if let SchematicIssue::Shorted { islands, .. }
        | SchematicIssue::UnexpectedConnection { islands, .. } = issue
        {
            for island in islands {
                if let Some(provenance) = current_observed.islands.get(island) {
                    reconnect_nets.extend(expected_names_for_island(expected, provenance));
                }
            }
        }

        let candidates = repair_candidates(issue, expected, &current_observed.islands);
        if let Some(cut) = minimum_verified_cut(
            &simulated,
            netlist,
            expected,
            &current_problems,
            &candidates,
        )? {
            remove_items(&mut simulated, &cut)?;
            removals.extend(cut);
            continue;
        }

        let fallback = repair_region_items(issue, &current_observed.islands);
        if fallback.is_empty() {
            let locations = relocation_candidates(document, issue, &current_observed.islands);
            if locations.is_empty() {
                return Err(unrepairable_issue(document, issue));
            }
            for location in locations {
                if relocate_symbols.insert(location.clone()) {
                    remove_items(
                        &mut simulated,
                        &BTreeSet::from([ConnectivityItemRef::Symbol {
                            page_id: location.page_id,
                            id: location.symbol_id,
                        }]),
                    )?;
                }
            }
            continue;
        }
        remove_items(&mut simulated, &fallback)?;
        removals.extend(fallback);
    }

    Ok(ConnectivityRepairIntent {
        removals,
        relocate_symbols,
        reconnect_nets,
    })
}

fn relocation_candidates(
    document: &SchDocument,
    issue: &SchematicIssue,
    islands: &std::collections::BTreeMap<crate::connectivity::IslandRef, PhysicalIsland>,
) -> BTreeSet<SymbolLocation> {
    let mut candidates = BTreeSet::new();
    let issue_islands = match issue {
        SchematicIssue::Shorted { islands, .. }
        | SchematicIssue::UnexpectedConnection { islands, .. } => islands,
        _ => return BTreeSet::new(),
    };
    for island in issue_islands
        .iter()
        .filter_map(|island| islands.get(island))
    {
        for terminal in &island.terminals {
            collect_terminal_symbols(document, terminal, &mut candidates);
        }
    }
    candidates
}

fn collect_terminal_symbols(
    document: &SchDocument,
    terminal: &Terminal,
    candidates: &mut BTreeSet<SymbolLocation>,
) {
    let Terminal::ComponentPin { component, .. } = terminal else {
        return;
    };
    match component {
        ComponentIdentity::KiCadSymbol(location) => {
            candidates.insert(location.clone());
        }
        ComponentIdentity::ManagedPath(path) => {
            for page in &document.pages {
                for symbol in page.items.iter().filter_map(|item| match item {
                    SchItem::Symbol(symbol) => Some(symbol),
                    _ => None,
                }) {
                    if symbol.field_value("Path") == Some(path) {
                        candidates.insert(SymbolLocation {
                            page_id: page.id.clone(),
                            symbol_id: symbol.id.clone(),
                        });
                    }
                }
            }
        }
    }
}

/// Teardown fallback: every removable item across the issue's islands. This
/// is deliberately ungated — the affected nets are queued for driver
/// reconnection, which rebuilds correct connectivity from the component pins.
/// Not minimal, but it makes every wiring-caused short repairable, including
/// shorts created by mislabeled hierarchical labels that no single-item
/// search can attribute to one island.
fn repair_region_items(
    issue: &SchematicIssue,
    islands: &std::collections::BTreeMap<crate::connectivity::IslandRef, PhysicalIsland>,
) -> BTreeSet<ConnectivityItemRef> {
    let issue_islands = match issue {
        SchematicIssue::Shorted { islands, .. }
        | SchematicIssue::UnexpectedConnection { islands, .. } => islands,
        _ => return BTreeSet::new(),
    };
    issue_islands
        .iter()
        .filter_map(|island| islands.get(island))
        .flat_map(|island| island.items.iter())
        .filter(|item| item.is_removable())
        .cloned()
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum RepairProblem {
    UnexpectedNet(String),
    Shorted(String, String),
    UnexpectedConnection(Terminal),
}

fn repair_problem_counts(issues: &[SchematicIssue]) -> BTreeMap<RepairProblem, usize> {
    let mut result = BTreeMap::new();
    for issue in issues {
        match issue {
            SchematicIssue::UnexpectedNet { net_name, .. } => {
                *result
                    .entry(RepairProblem::UnexpectedNet(net_name.clone()))
                    .or_default() += 1;
            }
            SchematicIssue::Shorted { net_names, .. } => {
                let names = net_names.iter().collect::<Vec<_>>();
                for (index, left) in names.iter().enumerate() {
                    for right in &names[index + 1..] {
                        *result
                            .entry(RepairProblem::Shorted((*left).clone(), (*right).clone()))
                            .or_default() += 1;
                    }
                }
            }
            SchematicIssue::UnexpectedConnection { terminals, .. } => {
                for terminal in terminals {
                    *result
                        .entry(RepairProblem::UnexpectedConnection(terminal.clone()))
                        .or_default() += 1;
                }
            }
            SchematicIssue::DisconnectedNet { .. }
            | SchematicIssue::MissingPort { .. }
            | SchematicIssue::UnboundSymbol { .. }
            | SchematicIssue::MissingSymbol { .. }
            | SchematicIssue::DuplicateSymbol { .. }
            | SchematicIssue::MismatchedSymbolId { .. }
            | SchematicIssue::UnexpectedSymbol { .. } => {}
        }
    }
    result
}

fn strictly_reduces_problems(
    before: &BTreeMap<RepairProblem, usize>,
    after: &BTreeMap<RepairProblem, usize>,
) -> bool {
    after
        .iter()
        .all(|(problem, count)| count <= before.get(problem).unwrap_or(&0))
        && after.values().sum::<usize>() < before.values().sum()
}

const MAX_EXACT_CUT_CANDIDATES: usize = 16;
const MAX_EXACT_CUT_ITEMS: usize = 4;

/// Finds the minimum-cost removable-item cut and verifies it through the same
/// semantic reducer used by inspection. This is a bounded exact multiway cut:
/// schematic labels and hierarchical drivers make a geometric two-terminal
/// max-flow graph insufficient on its own.
fn minimum_verified_cut(
    document: &SchDocument,
    netlist: &Schematic,
    expected: &ConnectivityGraph,
    current_problems: &BTreeMap<RepairProblem, usize>,
    candidates: &BTreeSet<ConnectivityItemRef>,
) -> Result<Option<BTreeSet<ConnectivityItemRef>>> {
    if candidates.is_empty() {
        return Ok(None);
    }

    let candidates = candidates.iter().cloned().collect::<Vec<_>>();
    let mut best = None;
    let max_items = if candidates.len() > MAX_EXACT_CUT_CANDIDATES {
        1
    } else {
        MAX_EXACT_CUT_ITEMS.min(candidates.len())
    };
    for size in 1..=max_items {
        visit_cut_combinations(
            document,
            netlist,
            expected,
            current_problems,
            &candidates,
            size,
            0,
            &mut BTreeSet::new(),
            &mut best,
        )?;
        if best
            .as_ref()
            .is_some_and(|best| cut_cost(best) <= size.saturating_add(1))
        {
            break;
        }
    }
    Ok(best)
}

#[allow(clippy::too_many_arguments)]
fn visit_cut_combinations(
    document: &SchDocument,
    netlist: &Schematic,
    expected: &ConnectivityGraph,
    current_problems: &BTreeMap<RepairProblem, usize>,
    candidates: &[ConnectivityItemRef],
    remaining: usize,
    start: usize,
    current: &mut BTreeSet<ConnectivityItemRef>,
    best: &mut Option<BTreeSet<ConnectivityItemRef>>,
) -> Result<()> {
    if remaining == 0 {
        if best
            .as_ref()
            .is_some_and(|best| cut_sort_key(current) >= cut_sort_key(best))
        {
            return Ok(());
        }
        let mut next = document.clone();
        remove_items(&mut next, current)?;
        let observed = observed_reconcilable_connectivity(&next, netlist)?;
        let analysis = analyze_connectivity(expected, &observed.graph);
        let problems = repair_problem_counts(analysis.issues());
        if strictly_reduces_problems(current_problems, &problems) {
            *best = Some(current.clone());
        }
        return Ok(());
    }

    let last_start = candidates.len() - remaining;
    for index in start..=last_start {
        let candidate = candidates[index].clone();
        current.insert(candidate.clone());
        visit_cut_combinations(
            document,
            netlist,
            expected,
            current_problems,
            candidates,
            remaining - 1,
            index + 1,
            current,
            best,
        )?;
        current.remove(&candidate);
    }
    Ok(())
}

fn cut_sort_key(
    items: &BTreeSet<ConnectivityItemRef>,
) -> (usize, usize, &BTreeSet<ConnectivityItemRef>) {
    (cut_cost(items), items.len(), items)
}

fn cut_cost(items: &BTreeSet<ConnectivityItemRef>) -> usize {
    items.iter().map(connectivity_item_cut_cost).sum()
}

fn connectivity_item_cut_cost(item: &ConnectivityItemRef) -> usize {
    match item {
        ConnectivityItemRef::Wire { .. } => 1,
        ConnectivityItemRef::Junction { .. } => 2,
        ConnectivityItemRef::NoConnect { .. } => 3,
        ConnectivityItemRef::Label { .. } => 4,
        ConnectivityItemRef::Symbol { .. } => 8,
        ConnectivityItemRef::SheetPin { .. } => usize::MAX,
    }
}

fn repair_candidates(
    issue: &SchematicIssue,
    expected: &ConnectivityGraph,
    islands: &std::collections::BTreeMap<crate::connectivity::IslandRef, PhysicalIsland>,
) -> BTreeSet<ConnectivityItemRef> {
    let issue_islands = match issue {
        SchematicIssue::Shorted { islands, .. }
        | SchematicIssue::UnexpectedConnection { islands, .. } => islands,
        _ => return BTreeSet::new(),
    };
    let mut candidates = BTreeSet::new();
    for island in issue_islands
        .iter()
        .filter_map(|island| islands.get(island))
        .filter(|island| repair_island(issue, expected, island))
    {
        candidates.extend(
            island
                .items
                .iter()
                .filter(|item| item.is_physical_connector())
                .cloned(),
        );
        if matches!(issue, SchematicIssue::Shorted { .. }) {
            candidates.extend(
                island
                    .named_drivers
                    .values()
                    .flatten()
                    .filter(|item| item.is_symbol())
                    .cloned(),
            );
        }
    }
    candidates
}

fn repair_island(
    issue: &SchematicIssue,
    expected: &ConnectivityGraph,
    island: &PhysicalIsland,
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
            "KiCad shorts nets {} without any removable wire, junction, label, or power symbol joining them",
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
    island: &PhysicalIsland,
) -> BTreeSet<String> {
    expected
        .groups
        .iter()
        .filter(|group| {
            !group.names.is_disjoint(&island.names)
                || group.terminals.iter().any(|expected_terminal| {
                    island
                        .terminals
                        .iter()
                        .any(|observed_terminal| expected_terminal.matches(observed_terminal))
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
        (SchItem::Symbol(_), ConnectivityItemRef::Symbol { page_id: page, id })
        | (SchItem::Wire(_), ConnectivityItemRef::Wire { page_id: page, id })
        | (SchItem::Junction(_), ConnectivityItemRef::Junction { page_id: page, id })
        | (SchItem::NoConnect(_), ConnectivityItemRef::NoConnect { page_id: page, id })
        | (SchItem::Label(_), ConnectivityItemRef::Label { page_id: page, id }) => {
            page == page_id && item.id() == Some(id.as_str())
        }
        _ => false,
    }
}

impl ConnectivityItemRef {
    fn is_removable_name_driver(&self) -> bool {
        matches!(self, Self::Label { .. } | Self::Symbol { .. })
    }

    /// Anything the repair teardown may delete: labels of any kind, wires,
    /// junctions, and driver symbols. Component symbols never appear as
    /// connectivity items, and sheet pins belong to their sheet's structure.
    fn is_removable(&self) -> bool {
        matches!(
            self,
            Self::Label { .. } | Self::Wire { .. } | Self::Junction { .. } | Self::Symbol { .. }
        )
    }

    fn is_symbol(&self) -> bool {
        matches!(self, Self::Symbol { .. })
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

    fn provenance(terminals: &[&str], item: ConnectivityItemRef) -> PhysicalIsland {
        PhysicalIsland {
            items: BTreeSet::from([item]),
            terminals: terminals.iter().map(|name| terminal(name)).collect(),
            ..PhysicalIsland::default()
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
