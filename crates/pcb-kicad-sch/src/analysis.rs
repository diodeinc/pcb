//! Comparison of independently reduced Zener and KiCad connectivity graphs.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Context;
use pcb_sch::Schematic;

use crate::{
    SchDocument, SchItem, SymbolSlotKey,
    connectivity::{
        ComponentIdentity, ComponentOrigin, ConnectionGroup, ConnectionOrigin, ConnectivityGraph,
        ConnectivityItemRef, IslandRef, PhysicalConnectivity, PhysicalIsland, PinVisibility,
        SymbolLocation, Terminal, not_connected_terminals, reduce_with_provenance,
    },
    symbol,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComponentPlacement {
    Missing,
    Placed(SymbolLocation),
    Duplicate(Vec<SymbolLocation>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentAnalysis {
    pub slot: SymbolSlotKey,
    pub placement: ComponentPlacement,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetAnalysis {
    pub name: String,
    pub expected_terminals: Vec<Terminal>,
    pub missing_terminals: Vec<Terminal>,
    pub islands: Vec<IslandRef>,
    pub connected_islands: Vec<Vec<IslandRef>>,
}

impl NetAnalysis {
    pub fn is_disconnected(&self) -> bool {
        self.connected_islands.len() > 1 || !self.missing_terminals.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchematicIssue {
    MissingSheet {
        page_id: String,
        sheet_id: String,
    },
    MissingSymbol {
        slot: SymbolSlotKey,
    },
    DuplicateSymbol {
        slot: SymbolSlotKey,
        locations: Vec<SymbolLocation>,
    },
    MismatchedSymbolId {
        slot: SymbolSlotKey,
        location: SymbolLocation,
        expected_symbol_id: String,
    },
    UnexpectedSymbol {
        slot: SymbolSlotKey,
        locations: Vec<SymbolLocation>,
    },
    UnboundSymbol {
        location: SymbolLocation,
    },
    DisconnectedNet {
        net_name: String,
        islands: Vec<IslandRef>,
        missing_terminals: Vec<Terminal>,
    },
    /// The net's pins all connect, but the interface port(s) the netlist
    /// declares for it have no hierarchical label on a top-level page, so
    /// the module no longer exposes the net to a consuming design.
    MissingPort {
        net_name: String,
        islands: Vec<IslandRef>,
        ports: Vec<String>,
    },
    UnexpectedNet {
        net_name: String,
        islands: Vec<IslandRef>,
    },
    UnexpectedConnection {
        islands: Vec<IslandRef>,
        terminals: Vec<Terminal>,
    },
    Shorted {
        islands: Vec<IslandRef>,
        net_names: BTreeSet<String>,
    },
}

impl SchematicIssue {
    /// Stable machine-readable category slug, for diagnostic kinds and
    /// suppression patterns.
    pub fn kind(&self) -> &'static str {
        match self {
            SchematicIssue::MissingSheet { .. } => "missing_sheet",
            SchematicIssue::MissingSymbol { .. } => "missing_symbol",
            SchematicIssue::DuplicateSymbol { .. } => "duplicate_symbol",
            SchematicIssue::MismatchedSymbolId { .. } => "mismatched_symbol_id",
            SchematicIssue::UnexpectedSymbol { .. } => "unexpected_symbol",
            SchematicIssue::UnboundSymbol { .. } => "unbound_symbol",
            SchematicIssue::DisconnectedNet { .. } => "disconnected_net",
            SchematicIssue::MissingPort { .. } => "missing_port",
            SchematicIssue::UnexpectedNet { .. } => "unexpected_net",
            SchematicIssue::UnexpectedConnection { .. } => "unexpected_connection",
            SchematicIssue::Shorted { .. } => "short",
        }
    }

    /// One-line human summary, for error messages and logs.
    pub fn summary(&self) -> String {
        match self {
            SchematicIssue::MissingSheet { page_id, sheet_id } => {
                format!("sheet '{sheet_id}' is not placed on page '{page_id}'")
            }
            SchematicIssue::MissingSymbol { slot } => {
                format!(
                    "component '{}' unit {} is not placed",
                    slot.component_path(),
                    slot.unit()
                )
            }
            SchematicIssue::DuplicateSymbol { slot, locations } => format!(
                "component '{}' unit {} is placed {} times",
                slot.component_path(),
                slot.unit(),
                locations.len()
            ),
            SchematicIssue::MismatchedSymbolId { slot, .. } => format!(
                "component '{}' unit {} uses the wrong symbol variant",
                slot.component_path(),
                slot.unit()
            ),
            SchematicIssue::UnexpectedSymbol { slot, .. } => format!(
                "component '{}' unit {} is not in the netlist",
                slot.component_path(),
                slot.unit()
            ),
            SchematicIssue::UnboundSymbol { location } => format!(
                "symbol '{}' is not bound to a component",
                location.symbol_id
            ),
            SchematicIssue::DisconnectedNet {
                net_name,
                islands,
                missing_terminals,
            } => {
                if missing_terminals.is_empty() {
                    format!(
                        "net '{net_name}' is wired in {} separate pieces",
                        islands.len()
                    )
                } else {
                    format!(
                        "net '{net_name}' is missing {} connection(s)",
                        missing_terminals.len()
                    )
                }
            }
            SchematicIssue::MissingPort {
                net_name, ports, ..
            } => format!(
                "net '{net_name}' does not expose interface port(s) {}",
                ports.join(", ")
            ),
            SchematicIssue::UnexpectedNet { net_name, .. } => {
                format!("net '{net_name}' is not in the netlist")
            }
            SchematicIssue::UnexpectedConnection { terminals, .. } => format!(
                "{} pins the netlist keeps apart are joined",
                terminals.len()
            ),
            SchematicIssue::Shorted { net_names, .. } => format!(
                "nets {} are shorted together",
                net_names
                    .iter()
                    .map(|name| format!("'{name}'"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

/// Stable semantic identity for one reported schematic discrepancy.
///
/// Physical issues include the UUID-addressed items that form their affected
/// islands instead of transient reduction indices, so the key is suitable for
/// retaining UI selection across repeated analysis.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum SchematicIssueKey {
    MissingSheet {
        page_id: String,
        sheet_id: String,
    },
    MissingSymbol(SymbolSlotKey),
    DuplicateSymbol(SymbolSlotKey),
    MismatchedSymbolId {
        slot: SymbolSlotKey,
        symbol_id: String,
    },
    UnexpectedSymbol(SymbolSlotKey),
    UnboundSymbol(SymbolLocation),
    DisconnectedNet(String),
    MissingPort(String),
    UnexpectedNet {
        net_name: String,
        items: BTreeSet<ConnectivityItemRef>,
    },
    UnexpectedConnection {
        terminals: Vec<Terminal>,
        items: BTreeSet<ConnectivityItemRef>,
    },
    Shorted {
        net_names: BTreeSet<String>,
        items: BTreeSet<ConnectivityItemRef>,
    },
}

/// One issue together with its stable key and exact physical provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchematicIssueContext {
    pub key: SchematicIssueKey,
    pub issue: SchematicIssue,
    pub items: BTreeSet<ConnectivityItemRef>,
}

/// A single-pass schematic analysis for UI and repair clients.
///
/// The physical graph is the same reduction used to produce `analysis` and
/// `issues`; clients do not need to maintain or recompute an electrical model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectivityInspection {
    pub analysis: ConnectivityAnalysis,
    /// The netlist-side graph the analysis compared against.
    pub expected: ConnectivityGraph,
    pub physical: PhysicalConnectivity,
    pub issues: Vec<SchematicIssueContext>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectivityAnalysis {
    pub components: BTreeMap<SymbolSlotKey, ComponentAnalysis>,
    pub nets: BTreeMap<String, NetAnalysis>,
    issues: Vec<SchematicIssue>,
}

impl ConnectivityAnalysis {
    pub fn issues(&self) -> &[SchematicIssue] {
        &self.issues
    }

    pub fn is_equivalent(&self) -> bool {
        self.issues.is_empty()
    }
}

/// Analyze a typed in-memory document and retain the physical provenance used
/// to identify and repair each issue.
pub fn inspect_schematic(
    document: &SchDocument,
    netlist: &Schematic,
) -> anyhow::Result<ConnectivityInspection> {
    let mut expected = expected_reconcilable_connectivity(document, netlist)?;
    apply_symbol_or_label_endpoint_requirements(document, netlist, &mut expected)?;
    let physical = observed_reconcilable_connectivity(document, netlist)?;
    let mut analysis = analyze_connectivity(&expected, &physical.graph);
    analysis.issues.splice(
        0..0,
        document.pages.iter().flat_map(|page| {
            page.items.iter().filter_map(|item| match item {
                SchItem::Sheet(sheet) if !sheet.placed => Some(SchematicIssue::MissingSheet {
                    page_id: page.id.clone(),
                    sheet_id: sheet.id.clone(),
                }),
                _ => None,
            })
        }),
    );
    let issues = analysis
        .issues()
        .iter()
        .cloned()
        .map(|issue| issue_context(issue, &physical.islands))
        .collect();
    Ok(ConnectivityInspection {
        analysis,
        expected,
        physical,
        issues,
    })
}

fn apply_symbol_or_label_endpoint_requirements(
    document: &SchDocument,
    netlist: &Schematic,
    expected: &mut ConnectivityGraph,
) -> anyhow::Result<()> {
    let Some(root) = &netlist.root_ref else {
        return Ok(());
    };
    let ports = crate::root_interface::symbol_ports_by_net(netlist, root)?;
    for (net_name, interface_names) in ports {
        let root_items = document
            .pages
            .iter()
            .filter(|page| document.root_page_ids.contains(&page.id))
            .flat_map(|page| &page.items)
            .collect::<Vec<_>>();
        let has_net_symbol = root_items.iter().any(|item| {
            matches!(item, SchItem::Symbol(symbol)
                if symbol.field_value("Path").is_none()
                    && symbol.field_value("Value") == Some(net_name.as_str()))
        });
        let Some(group) = expected
            .groups
            .iter_mut()
            .find(|group| logical_name(group) == Some(net_name.as_str()))
        else {
            continue;
        };
        for interface_name in interface_names {
            let has_hierarchical_label = root_items.iter().any(|item| {
                matches!(item, SchItem::Label(label)
                    if matches!(label.kind, crate::LabelKind::Hierarchical { .. })
                        && label.text == interface_name)
            });
            if !has_net_symbol || has_hierarchical_label {
                group.terminals.insert(Terminal::InterfacePort {
                    name: interface_name,
                });
            }
        }
    }
    Ok(())
}

pub(crate) fn issue_context(
    issue: SchematicIssue,
    islands: &BTreeMap<IslandRef, PhysicalIsland>,
) -> SchematicIssueContext {
    let island_items = |issue_islands: &[IslandRef]| {
        issue_islands
            .iter()
            .filter_map(|island| islands.get(island))
            .flat_map(|island| island.items.iter().cloned())
            .collect::<BTreeSet<_>>()
    };
    let (key, items) = match &issue {
        SchematicIssue::MissingSheet { page_id, sheet_id } => (
            SchematicIssueKey::MissingSheet {
                page_id: page_id.clone(),
                sheet_id: sheet_id.clone(),
            },
            BTreeSet::new(),
        ),
        SchematicIssue::MissingSymbol { slot } => (
            SchematicIssueKey::MissingSymbol(slot.clone()),
            BTreeSet::new(),
        ),
        SchematicIssue::DuplicateSymbol { slot, locations } => (
            SchematicIssueKey::DuplicateSymbol(slot.clone()),
            locations.iter().map(symbol_item).collect(),
        ),
        SchematicIssue::MismatchedSymbolId { slot, location, .. } => (
            SchematicIssueKey::MismatchedSymbolId {
                slot: slot.clone(),
                symbol_id: location.symbol_id.clone(),
            },
            BTreeSet::from([symbol_item(location)]),
        ),
        SchematicIssue::UnexpectedSymbol { slot, locations } => (
            SchematicIssueKey::UnexpectedSymbol(slot.clone()),
            locations.iter().map(symbol_item).collect(),
        ),
        SchematicIssue::UnboundSymbol { location } => (
            SchematicIssueKey::UnboundSymbol(location.clone()),
            BTreeSet::from([symbol_item(location)]),
        ),
        SchematicIssue::DisconnectedNet {
            net_name,
            islands: issue_islands,
            ..
        } => (
            SchematicIssueKey::DisconnectedNet(net_name.clone()),
            island_items(issue_islands),
        ),
        SchematicIssue::MissingPort {
            net_name,
            islands: issue_islands,
            ..
        } => (
            SchematicIssueKey::MissingPort(net_name.clone()),
            island_items(issue_islands),
        ),
        SchematicIssue::UnexpectedNet {
            net_name,
            islands: issue_islands,
        } => {
            let items = island_items(issue_islands);
            (
                SchematicIssueKey::UnexpectedNet {
                    net_name: net_name.clone(),
                    items: items.clone(),
                },
                items,
            )
        }
        SchematicIssue::UnexpectedConnection {
            terminals,
            islands: issue_islands,
        } => {
            let items = island_items(issue_islands);
            (
                SchematicIssueKey::UnexpectedConnection {
                    terminals: terminals.clone(),
                    items: items.clone(),
                },
                items,
            )
        }
        SchematicIssue::Shorted {
            net_names,
            islands: issue_islands,
        } => {
            let items = island_items(issue_islands);
            (
                SchematicIssueKey::Shorted {
                    net_names: net_names.clone(),
                    items: items.clone(),
                },
                items,
            )
        }
    };
    SchematicIssueContext { key, issue, items }
}

/// The key with volatile item fingerprints stripped, for before/after
/// identity comparisons.
pub(crate) fn coarse_key(key: &SchematicIssueKey) -> SchematicIssueKey {
    let mut key = key.clone();
    match &mut key {
        SchematicIssueKey::UnexpectedNet { items, .. }
        | SchematicIssueKey::UnexpectedConnection { items, .. }
        | SchematicIssueKey::Shorted { items, .. } => items.clear(),
        SchematicIssueKey::MissingSheet { .. }
        | SchematicIssueKey::MissingSymbol(_)
        | SchematicIssueKey::DuplicateSymbol(_)
        | SchematicIssueKey::MismatchedSymbolId { .. }
        | SchematicIssueKey::UnexpectedSymbol(_)
        | SchematicIssueKey::UnboundSymbol(_)
        | SchematicIssueKey::DisconnectedNet(_)
        | SchematicIssueKey::MissingPort(_) => {}
    }
    key
}

pub(crate) fn issue_summaries<'a>(issues: impl Iterator<Item = &'a SchematicIssue>) -> String {
    issues
        .map(|issue| issue.summary())
        .collect::<Vec<_>>()
        .join("; ")
}

/// Reject a repair that left one of the issues it set out to resolve.
pub(crate) fn ensure_issues_resolved(
    after: &ConnectivityInspection,
    keys: &BTreeSet<SchematicIssueKey>,
    action: &str,
) -> anyhow::Result<()> {
    for key in keys {
        if let Some(remaining) = after
            .issues
            .iter()
            .find(|issue| coarse_key(&issue.key) == coarse_key(key))
        {
            anyhow::bail!(
                "{action} did not resolve schematic issue: {}",
                remaining.issue.summary()
            );
        }
    }
    Ok(())
}

/// Reject a document change that introduced an issue absent before it.
pub(crate) fn ensure_no_new_issues(
    before: &ConnectivityInspection,
    after: &ConnectivityInspection,
    action: &str,
) -> anyhow::Result<()> {
    // Compare without item fingerprints: a pre-existing issue whose affected
    // islands shifted is still the same issue, not a new one this action
    // introduced.
    let before_keys = before
        .issues
        .iter()
        .map(|issue| coarse_key(&issue.key))
        .collect::<BTreeSet<_>>();
    let new_issues = after
        .issues
        .iter()
        .filter(|issue| !before_keys.contains(&coarse_key(&issue.key)))
        .collect::<Vec<_>>();
    if !new_issues.is_empty() {
        anyhow::bail!(
            "{action} would introduce unrelated issues: {}",
            issue_summaries(new_issues.iter().map(|context| &context.issue))
        );
    }
    Ok(())
}

fn symbol_item(location: &SymbolLocation) -> ConnectivityItemRef {
    ConnectivityItemRef::Symbol {
        page_id: location.page_id.clone(),
        id: location.symbol_id.clone(),
    }
}

pub(crate) fn observed_reconcilable_connectivity(
    document: &SchDocument,
    netlist: &Schematic,
) -> anyhow::Result<PhysicalConnectivity> {
    let mut observed = reduce_with_provenance(document, PinVisibility::VisibleOnly)?;
    let not_connected = not_connected_terminals(netlist);
    let islands = &observed.islands;
    observed
        .graph
        .groups
        .retain(|group| !is_open_not_connected_group(group, islands, &not_connected));
    Ok(observed)
}

fn is_open_not_connected_group(
    group: &ConnectionGroup,
    islands: &BTreeMap<IslandRef, PhysicalIsland>,
    not_connected: &BTreeSet<Terminal>,
) -> bool {
    if !group.names.is_empty() || group.terminals.len() != 1 {
        return false;
    }
    let terminal = group.terminals.first().expect("checked one terminal");
    if !not_connected
        .iter()
        .any(|candidate| candidate.matches(terminal))
    {
        return false;
    }
    let mut origins = group.origins.iter();
    let Some(ConnectionOrigin::KiCadIsland(island)) = origins.next() else {
        return false;
    };
    if origins.next().is_some() {
        return false;
    }
    islands.get(island).is_some_and(|provenance| {
        provenance
            .items
            .iter()
            .all(|item| matches!(item, ConnectivityItemRef::NoConnect { .. }))
    })
}

pub(crate) fn expected_reconcilable_connectivity(
    document: &SchDocument,
    netlist: &Schematic,
) -> anyhow::Result<ConnectivityGraph> {
    let (visible, hidden) = managed_terminals_by_visibility(document)?;
    let mut graph = ConnectivityGraph::from_zener(netlist)?;
    graph.groups.retain_mut(|group| {
        let original_len = group.terminals.len();
        group.terminals.retain(|terminal| {
            visible.iter().any(|candidate| terminal.matches(candidate))
                || !hidden.iter().any(|ignored| terminal.matches(ignored))
        });
        original_len == 0 || !group.terminals.is_empty()
    });
    Ok(graph)
}

fn managed_terminals_by_visibility(
    document: &SchDocument,
) -> anyhow::Result<(Vec<Terminal>, Vec<Terminal>)> {
    let mut visible = Vec::new();
    let mut hidden = Vec::new();
    for page in &document.pages {
        for placed in page.items.iter().filter_map(|item| match item {
            SchItem::Symbol(symbol) => Some(symbol),
            _ => None,
        }) {
            let Some(component_path) = placed.field_value("Path").filter(|path| !path.is_empty())
            else {
                continue;
            };
            let definition = page
                .library
                .definitions
                .get(&placed.lib_id)
                .with_context(|| {
                    format!(
                        "managed symbol {} has no cached definition {}",
                        placed.id, placed.lib_id
                    )
                })?;
            let parsed = symbol::ParsedSymbolDefinition::parse(definition)?;
            for pin in parsed.placed_pins(placed)? {
                let terminal = Terminal::ComponentPin {
                    component: ComponentIdentity::ManagedPath(component_path.to_string()),
                    pin_name: pin.name,
                    pin_numbers: pin.numbers,
                };
                if pin.hidden {
                    hidden.push(terminal);
                } else {
                    visible.push(terminal);
                }
            }
        }
    }
    Ok((visible, hidden))
}

/// Compare an expected logical graph with an observed physical graph.
pub fn analyze_connectivity(
    expected: &ConnectivityGraph,
    observed: &ConnectivityGraph,
) -> ConnectivityAnalysis {
    let components = analyze_components(expected, observed);
    let nets = analyze_nets(expected, observed);
    let issues = collect_issues(expected, observed, &components, &nets);
    ConnectivityAnalysis {
        components,
        nets,
        issues,
    }
}

fn analyze_components(
    expected: &ConnectivityGraph,
    observed: &ConnectivityGraph,
) -> BTreeMap<SymbolSlotKey, ComponentAnalysis> {
    let expected_slots = expected_component_slots(expected);
    let observed_locations = observed_component_locations(observed);
    expected_slots
        .into_iter()
        .map(|slot| {
            let locations = observed_locations.get(&slot).cloned().unwrap_or_default();
            let placement = match locations.as_slice() {
                [] => ComponentPlacement::Missing,
                [location] => ComponentPlacement::Placed(location.clone()),
                _ => ComponentPlacement::Duplicate(locations),
            };
            (slot.clone(), ComponentAnalysis { slot, placement })
        })
        .collect()
}

fn expected_component_slots(graph: &ConnectivityGraph) -> BTreeSet<SymbolSlotKey> {
    graph
        .components
        .iter()
        .filter(|component| component.origin == ComponentOrigin::Zener)
        .filter_map(|component| component.managed_slot.clone())
        .collect()
}

fn observed_component_locations(
    graph: &ConnectivityGraph,
) -> BTreeMap<SymbolSlotKey, Vec<SymbolLocation>> {
    let mut locations = BTreeMap::<SymbolSlotKey, Vec<SymbolLocation>>::new();
    for component in &graph.components {
        let ComponentOrigin::KiCad(location) = &component.origin else {
            continue;
        };
        let Some(slot) = &component.managed_slot else {
            continue;
        };
        locations
            .entry(slot.clone())
            .or_default()
            .push(location.clone());
    }
    for found in locations.values_mut() {
        found.sort();
    }
    locations
}

fn analyze_nets(
    expected: &ConnectivityGraph,
    observed: &ConnectivityGraph,
) -> BTreeMap<String, NetAnalysis> {
    expected
        .groups
        .iter()
        .filter_map(|expected_group| {
            let name = logical_name(expected_group)?;
            let matching_groups = observed
                .groups
                .iter()
                .filter(|observed_group| groups_match(expected_group, observed_group))
                .collect::<Vec<_>>();
            let missing_terminals = expected_group
                .terminals
                .iter()
                .filter(|expected_terminal| {
                    !matching_groups.iter().any(|observed_group| {
                        observed_group
                            .terminals
                            .iter()
                            .any(|observed_terminal| expected_terminal.matches(observed_terminal))
                    })
                })
                .cloned()
                .collect::<Vec<_>>();
            let connected_islands = matching_groups
                .iter()
                .map(|group| kicad_islands(group))
                .filter(|islands| !islands.is_empty())
                .collect::<Vec<_>>();
            let islands = connected_islands.iter().flatten().cloned().collect();
            Some((
                name.to_string(),
                NetAnalysis {
                    name: name.to_string(),
                    expected_terminals: expected_group.terminals.iter().cloned().collect(),
                    missing_terminals,
                    islands,
                    connected_islands,
                },
            ))
        })
        .collect()
}

fn groups_match(expected: &ConnectionGroup, observed: &ConnectionGroup) -> bool {
    !expected.names.is_disjoint(&observed.names)
        || expected.terminals.iter().any(|expected_terminal| {
            observed
                .terminals
                .iter()
                .any(|observed_terminal| expected_terminal.matches(observed_terminal))
        })
}

fn collect_issues(
    expected: &ConnectivityGraph,
    observed: &ConnectivityGraph,
    components: &BTreeMap<SymbolSlotKey, ComponentAnalysis>,
    nets: &BTreeMap<String, NetAnalysis>,
) -> Vec<SchematicIssue> {
    let mut issues = Vec::new();
    collect_component_issues(expected, observed, components, &mut issues);
    collect_connection_issues(expected, observed, nets, &mut issues);
    issues
}

fn collect_component_issues(
    expected: &ConnectivityGraph,
    observed: &ConnectivityGraph,
    components: &BTreeMap<SymbolSlotKey, ComponentAnalysis>,
    issues: &mut Vec<SchematicIssue>,
) {
    for component in components.values() {
        match &component.placement {
            ComponentPlacement::Missing => issues.push(SchematicIssue::MissingSymbol {
                slot: component.slot.clone(),
            }),
            ComponentPlacement::Duplicate(locations) => {
                issues.push(SchematicIssue::DuplicateSymbol {
                    slot: component.slot.clone(),
                    locations: locations.clone(),
                })
            }
            ComponentPlacement::Placed(_) => {}
        }
    }

    let expected_slots = expected_component_slots(expected);
    let observed_locations = observed_component_locations(observed);
    for (slot, locations) in observed_locations {
        if !expected_slots.contains(&slot) {
            issues.push(SchematicIssue::UnexpectedSymbol { slot, locations });
            continue;
        }
        let expected_symbol_id = slot.symbol_id();
        for location in locations {
            if location.symbol_id != expected_symbol_id {
                issues.push(SchematicIssue::MismatchedSymbolId {
                    slot: slot.clone(),
                    location,
                    expected_symbol_id: expected_symbol_id.clone(),
                });
            }
        }
    }

    for component in &observed.components {
        if component.managed_slot.is_none()
            && let ComponentOrigin::KiCad(location) = &component.origin
        {
            issues.push(SchematicIssue::UnboundSymbol {
                location: location.clone(),
            });
        }
    }
}

fn collect_connection_issues(
    expected: &ConnectivityGraph,
    observed: &ConnectivityGraph,
    nets: &BTreeMap<String, NetAnalysis>,
    issues: &mut Vec<SchematicIssue>,
) {
    for observed_group in &observed.groups {
        let matching_expected = expected
            .groups
            .iter()
            .filter(|expected_group| groups_match(expected_group, observed_group))
            .collect::<Vec<_>>();
        let matching_names = matching_expected
            .iter()
            .filter_map(|group| logical_name(group).map(str::to_string))
            .collect::<BTreeSet<_>>();
        if matching_names.len() > 1 {
            issues.push(SchematicIssue::Shorted {
                islands: kicad_islands(observed_group),
                net_names: matching_names,
            });
        }

        for name in &observed_group.names {
            let accepted = matching_expected.iter().any(|group| {
                group.names.contains(name)
                    || group.terminals.iter().any(|terminal| {
                        matches!(
                            terminal,
                            Terminal::InterfacePort { name: port_name } if port_name == name
                        )
                    })
            });
            if !accepted {
                issues.push(SchematicIssue::UnexpectedNet {
                    net_name: name.clone(),
                    islands: kicad_islands(observed_group),
                });
            }
        }

        let mut unexpected_terminals = observed_group
            .terminals
            .iter()
            .filter(|observed_terminal| {
                !matching_expected.iter().any(|expected_group| {
                    expected_group
                        .terminals
                        .iter()
                        .any(|expected_terminal| expected_terminal.matches(observed_terminal))
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        // A lone unmatched terminal is a standalone unmanaged pin, already
        // represented by its component issue — it cannot form a connection.
        // Two or more joined terminals the netlist keeps apart (including
        // wires between NotConnected pins) are a real unexpected connection.
        if matching_expected.is_empty() && observed_group.terminals.len() < 2 {
            unexpected_terminals.clear();
        }
        if !unexpected_terminals.is_empty() {
            issues.push(SchematicIssue::UnexpectedConnection {
                islands: kicad_islands(observed_group),
                terminals: unexpected_terminals,
            });
        }
    }

    for net in nets.values() {
        // Missing interface ports are their own issue: the placed geometry
        // can be fully connected while the module's port is unrepresented.
        let (ports, missing_terminals): (Vec<_>, Vec<_>) = net
            .missing_terminals
            .iter()
            .cloned()
            .partition(|terminal| matches!(terminal, Terminal::InterfacePort { .. }));
        if net.connected_islands.len() > 1 || !missing_terminals.is_empty() {
            issues.push(SchematicIssue::DisconnectedNet {
                net_name: net.name.clone(),
                islands: net.islands.clone(),
                missing_terminals,
            });
        }
        if !ports.is_empty() {
            issues.push(SchematicIssue::MissingPort {
                net_name: net.name.clone(),
                islands: net.islands.clone(),
                ports: ports
                    .into_iter()
                    .map(|terminal| match terminal {
                        Terminal::InterfacePort { name } => name,
                        Terminal::ComponentPin { .. } => unreachable!("partitioned above"),
                    })
                    .collect(),
            });
        }
    }
}

fn kicad_islands(group: &ConnectionGroup) -> Vec<IslandRef> {
    group
        .origins
        .iter()
        .filter_map(|origin| match origin {
            ConnectionOrigin::KiCadIsland(island) => Some(island.clone()),
            ConnectionOrigin::ZenerNet { .. } => None,
        })
        .collect()
}

pub(crate) fn logical_name(group: &ConnectionGroup) -> Option<&str> {
    group.origins.iter().find_map(|origin| match origin {
        ConnectionOrigin::ZenerNet { name } => Some(name.as_str()),
        ConnectionOrigin::KiCadIsland(_) => None,
    })
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, path::Path};

    use pcb_sch::{AttributeValue, Instance, InstanceRef, ModuleRef, Net};

    use super::*;
    use crate::{
        Label, LabelKind, LabelShape, Point, Rotation, SchItem, SchPage, Symbol, SymbolDefinition,
        SymbolField,
    };

    #[test]
    fn reports_missing_component_slot() {
        let module = ModuleRef::from_path(Path::new("/tmp/root.zen"), "root");
        let mut netlist = Schematic::new();
        netlist.add_instance(
            InstanceRef::new(module.clone(), vec!["U1".to_string()]),
            Instance::component(module),
        );
        let document = document_with_pages(vec![SchPage::new("page")]);

        let analysis = inspect_schematic(&document, &netlist).unwrap().analysis;

        assert!(matches!(
            analysis.issues(),
            [SchematicIssue::MissingSymbol { slot }] if slot.component_path() == "U1"
        ));
    }

    #[test]
    fn distinguishes_local_and_global_cross_page_connectivity() {
        let netlist = netlist_with_nets(&["N1", "GND"]);
        let mut pages = Vec::new();
        for index in 0..2 {
            let mut page = SchPage::new(format!("page-{index}"));
            page.items.push(SchItem::Label(Label::new(
                format!("local-{index}"),
                "N1",
                Point::new(index as f64, 0.0),
            )));
            let mut global = Label::new(
                format!("global-{index}"),
                "GND",
                Point::new(index as f64, 10.0),
            );
            global.kind = LabelKind::Global {
                shape: LabelShape::Bidirectional,
            };
            page.items.push(SchItem::Label(global));
            pages.push(page);
        }

        let analysis = inspect_schematic(&document_with_pages(pages), &netlist)
            .unwrap()
            .analysis;

        assert!(analysis.nets["N1"].is_disconnected());
        assert!(!analysis.nets["GND"].is_disconnected());
    }

    #[test]
    fn reports_shorts_and_unexpected_nets() {
        let netlist = netlist_with_nets(&["N1", "N2"]);
        let mut page = SchPage::new("page");
        page.items.extend([
            SchItem::Label(Label::new("a", "N1", Point::new(0.0, 0.0))),
            SchItem::Label(Label::new("b", "N2", Point::new(0.0, 0.0))),
            SchItem::Label(Label::new("c", "EXTRA", Point::new(10.0, 0.0))),
        ]);

        let analysis = inspect_schematic(&document_with_pages(vec![page]), &netlist)
            .unwrap()
            .analysis;

        assert!(
            analysis
                .issues()
                .iter()
                .any(|issue| matches!(issue, SchematicIssue::Shorted { .. }))
        );
        assert!(analysis.issues().iter().any(
            |issue| matches!(issue, SchematicIssue::UnexpectedNet { net_name, .. } if net_name == "EXTRA")
        ));
    }

    #[test]
    fn reports_an_extra_terminal_on_an_otherwise_matching_net() {
        let terminal = |path: &str| Terminal::ComponentPin {
            component: ComponentIdentity::ManagedPath(path.to_string()),
            pin_name: "1".to_string(),
            pin_numbers: BTreeSet::from(["1".to_string()]),
        };
        let expected = ConnectivityGraph {
            components: Vec::new(),
            groups: vec![ConnectionGroup {
                names: BTreeSet::from(["N".to_string()]),
                terminals: BTreeSet::from([terminal("U1")]),
                origins: BTreeSet::from([ConnectionOrigin::ZenerNet {
                    name: "N".to_string(),
                }]),
            }],
        };
        let extra = terminal("U2");
        let observed = ConnectivityGraph {
            components: Vec::new(),
            groups: vec![ConnectionGroup {
                names: BTreeSet::from(["N".to_string()]),
                terminals: BTreeSet::from([terminal("U1"), extra.clone()]),
                origins: BTreeSet::from([ConnectionOrigin::KiCadIsland(IslandRef {
                    page_id: "page".to_string(),
                    index: 0,
                })]),
            }],
        };

        let analysis = analyze_connectivity(&expected, &observed);

        assert!(matches!(
            analysis.issues(),
            [SchematicIssue::UnexpectedConnection { terminals, .. }] if terminals == &[extra]
        ));
    }

    #[test]
    fn component_pins_match_names_to_names_or_numbers_to_numbers() {
        let terminal = |name: &str, number: &str| Terminal::ComponentPin {
            component: ComponentIdentity::ManagedPath("U1".to_string()),
            pin_name: name.to_string(),
            pin_numbers: BTreeSet::from([number.to_string()]),
        };

        assert!(terminal("A", "1").matches(&terminal("A", "2")));
        assert!(terminal("A", "1").matches(&terminal("B", "1")));
        assert!(!terminal("A", "1").matches(&terminal("1", "2")));
    }

    #[test]
    fn not_connected_terminal_rejects_an_attached_wire() {
        let terminal = Terminal::ComponentPin {
            component: ComponentIdentity::ManagedPath("U1".to_string()),
            pin_name: "NC".to_string(),
            pin_numbers: BTreeSet::from(["1".to_string()]),
        };
        let island = IslandRef {
            page_id: "page".to_string(),
            index: 0,
        };
        let group = ConnectionGroup {
            names: BTreeSet::new(),
            terminals: BTreeSet::from([terminal.clone()]),
            origins: BTreeSet::from([ConnectionOrigin::KiCadIsland(island.clone())]),
        };
        let mut islands = BTreeMap::from([(island, PhysicalIsland::default())]);
        let not_connected = BTreeSet::from([terminal]);

        assert!(is_open_not_connected_group(
            &group,
            &islands,
            &not_connected
        ));
        islands
            .values_mut()
            .next()
            .unwrap()
            .items
            .insert(ConnectivityItemRef::Wire {
                page_id: "page".to_string(),
                id: "wire".to_string(),
            });
        assert!(!is_open_not_connected_group(
            &group,
            &islands,
            &not_connected
        ));
    }

    #[test]
    fn equivalent_project_has_no_issues() {
        let netlist = netlist_with_nets(&["N1"]);
        let mut page = SchPage::new("page");
        page.items.push(SchItem::Label(Label::new(
            "label",
            "N1",
            Point::new(0.0, 0.0),
        )));

        let analysis = inspect_schematic(&document_with_pages(vec![page]), &netlist)
            .unwrap()
            .analysis;

        assert!(analysis.is_equivalent(), "{:?}", analysis.issues());
    }

    #[test]
    fn root_interface_ports_map_to_their_shared_realized_net() {
        let mut netlist = netlist_with_nets(&["REALIZED_SIG"]);
        netlist.nets.get_mut("REALIZED_SIG").unwrap().id = 42;
        add_root_signature_io(&mut netlist, "SIG", "REALIZED_SIG", 42);
        add_root_signature_io(&mut netlist, "ALIAS", "REALIZED_SIG", 42);
        let mut page = SchPage::new("page");
        for (id, name) in [("port", "SIG"), ("alias", "ALIAS")] {
            let mut label = Label::new(id, name, Point::new(0.0, 0.0));
            label.kind = LabelKind::Hierarchical {
                shape: LabelShape::Bidirectional,
            };
            page.items.push(SchItem::Label(label));
        }

        let analysis = inspect_schematic(&document_with_pages(vec![page]), &netlist)
            .unwrap()
            .analysis;

        assert!(analysis.is_equivalent(), "{:?}", analysis.issues());
    }

    #[test]
    fn invalid_root_signature_net_reference_is_an_error() {
        let mut netlist = netlist_with_nets(&["N"]);
        add_root_signature_io(&mut netlist, "SIG", "N", 42);

        let error = inspect_schematic(&document_with_pages(vec![SchPage::new("page")]), &netlist)
            .unwrap_err();

        assert!(error.to_string().contains("references unknown net id 42"));
    }

    #[test]
    fn power_symbols_connect_globally_across_pages() {
        let netlist = netlist_with_nets(&["GND"]);
        let definition = SymbolDefinition::from_kicad_symbol_sexpr(
            r#"(symbol "power:GND"
              (power global)
              (symbol "GND_1_1"
                (pin power_in line (at 0 0 0) (length 2.54)
                  (name "GND") (number "1"))))"#,
        )
        .unwrap();
        let mut document = document_with_pages(vec![SchPage::new("a"), SchPage::new("b")]);
        for (index, page) in document.pages.iter_mut().enumerate() {
            page.library
                .definitions
                .insert(definition.lib_id.clone(), definition.clone());
            page.items.push(SchItem::Symbol(power_symbol(
                format!("power-{index}"),
                Point::new(index as f64, 0.0),
            )));
        }

        let analysis = inspect_schematic(&document, &netlist).unwrap().analysis;

        assert!(analysis.is_equivalent(), "{:?}", analysis.issues());
        assert_eq!(analysis.nets["GND"].connected_islands.len(), 1);
    }

    fn document_with_pages(pages: Vec<SchPage>) -> SchDocument {
        let root_page_ids = pages.iter().map(|page| page.id.clone()).collect();
        SchDocument {
            pages,
            root_page_ids,
        }
    }

    fn netlist_with_nets(names: &[&str]) -> Schematic {
        let mut netlist = Schematic::new();
        for (id, name) in names.iter().enumerate() {
            netlist.add_net(Net {
                kind: "Net".to_string(),
                id: id as u64,
                name: (*name).to_string(),
                ports: Vec::new(),
                properties: Default::default(),
            });
        }
        netlist
    }

    fn add_root_signature_io(netlist: &mut Schematic, io_name: &str, net_name: &str, id: u64) {
        let parameter = serde_json::json!({
            "name": io_name,
            "is_config": false,
            "value": { "Net": { "id": id, "name": net_name, "properties": {} } },
            "default_value": { "Net": { "id": id, "name": io_name, "properties": {} } }
        });
        if let Some(root_ref) = netlist.root_ref.clone() {
            let root = netlist.instances.get_mut(&root_ref).unwrap();
            let Some(AttributeValue::Json(signature)) = root.attributes.get_mut("__signature")
            else {
                panic!("root signature");
            };
            signature["parameters"]
                .as_array_mut()
                .unwrap()
                .push(parameter);
            return;
        }

        let module = ModuleRef::from_path(Path::new("/tmp/root.zen"), "root");
        let root_ref = InstanceRef::new(module.clone(), Vec::new());
        let mut root = Instance::module(module);
        root.attributes.insert(
            "__signature".to_string(),
            AttributeValue::Json(serde_json::json!({
                "parameters": [parameter]
            })),
        );
        netlist.root_ref = Some(root_ref.clone());
        netlist.add_instance(root_ref, root);
    }

    fn power_symbol(id: String, at: Point) -> Symbol {
        Symbol {
            id,
            lib_id: "power:GND".to_string(),
            unit: 1,
            body_style: 1,
            at,
            rotation: Rotation::Deg0,
            mirror: None,
            dnp: false,
            in_bom: true,
            on_board: true,
            in_pos_files: true,
            fields_autoplaced: false,
            fields: BTreeMap::from([
                (
                    "Reference".to_string(),
                    SymbolField::new("Reference", "#PWR", at),
                ),
                ("Value".to_string(), SymbolField::new("Value", "GND", at)),
            ]),
            pins: Vec::new(),
            unsupported: Vec::new(),
        }
    }
}
