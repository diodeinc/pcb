//! Pure planning for KiCad connectivity repairs.

use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
};

use anyhow::{Result, bail};
use pcb_sch::Schematic;

use crate::{
    GEOMETRY_EPS_MM, Point, SchDocument, SchItem, SchPage, Symbol,
    analysis::{
        ConnectivityAnalysis, ConnectivityInspection, NoConnectTarget, SchematicIssue,
        SchematicIssueKey, analyze_connectivity, ensure_issues_resolved, ensure_no_new_issues,
        has_no_connect, inspect_no_connects, inspect_schematic, issue_context, logical_name,
        observed_reconcilable_connectivity,
    },
    compose,
    connectivity::{
        ComponentIdentity, ConnectivityGraph, ConnectivityItemRef, CutNode, PhysicalConnectivity,
        PhysicalIsland, PinVisibility, SymbolLocation, Terminal, TerminalIndex, cut_graph,
        points_connect, reduce_with_provenance,
    },
    cut,
    net_symbols::NetSymbolSpec,
};

/// A deterministic, UUID-addressed connectivity repair decision.
///
/// The intent says what must change: exact items to remove, symbols that must
/// move, nets whose connectivity a realizer rebuilds, the name driver each net
/// needs, and exact native no-connect marker targets. Planning does not mutate
/// the input document. A realizer applies the intent and then
/// [`verify_connectivity_repair`] judges the result.
#[derive(Debug, Clone, PartialEq)]
pub struct ConnectivityRepairIntent {
    pub(crate) selected_keys: BTreeSet<SchematicIssueKey>,
    pub(crate) removals: BTreeSet<ConnectivityItemRef>,
    pub(crate) relocated_symbols: BTreeSet<SymbolLocation>,
    pub(crate) reconnect_nets: BTreeSet<String>,
    pub(crate) drivers: BTreeMap<String, BTreeMap<String, NetDriverKind>>,
    pub(crate) no_connect_additions: Vec<NoConnectTarget>,
}

impl ConnectivityRepairIntent {
    /// An intent that changes nothing. Verifying a document against it means
    /// the realizer may only have added items.
    pub fn additions_only() -> Self {
        Self {
            selected_keys: BTreeSet::new(),
            removals: BTreeSet::new(),
            relocated_symbols: BTreeSet::new(),
            reconnect_nets: BTreeSet::new(),
            drivers: BTreeMap::new(),
            no_connect_additions: Vec::new(),
        }
    }

    /// The issues this intent resolves.
    pub fn selected_keys(&self) -> &BTreeSet<SchematicIssueKey> {
        &self.selected_keys
    }

    /// Existing connectivity items that the repair will remove. Includes the
    /// caller's forced removals and any junction they leave behind.
    pub fn removals(&self) -> &BTreeSet<ConnectivityItemRef> {
        &self.removals
    }

    /// Component symbols that the repair will move away from an invalid overlap.
    pub fn relocated_symbols(&self) -> &BTreeSet<SymbolLocation> {
        &self.relocated_symbols
    }

    /// Nets whose expected connectivity the repair will regenerate.
    pub fn reconnect_nets(&self) -> &BTreeSet<String> {
        &self.reconnect_nets
    }

    /// The name driver each reconnected net needs, by net and then page id. A
    /// page appears only where the net has a visible pin or, on the root page,
    /// an interface port.
    pub fn drivers(&self) -> &BTreeMap<String, BTreeMap<String, NetDriverKind>> {
        &self.drivers
    }

    /// The driver kind for one net on one page, when the net is reconnected.
    pub fn driver_kind(&self, net_name: &str, page_id: &str) -> Option<&NetDriverKind> {
        self.drivers.get(net_name)?.get(page_id)
    }

    /// Native KiCad no-connect markers the realizer must add.
    pub fn no_connect_additions(&self) -> &[NoConnectTarget] {
        &self.no_connect_additions
    }

    /// Apply the removals and symbol relocations of this intent: everything
    /// PCB decided must change, with no new geometry. A consumer's realizer
    /// starts from this document and adds connections and no-connect markers.
    pub fn apply_edits(&self, document: &SchDocument) -> Result<SchDocument> {
        let mut repaired = document.clone();
        remove_items(&mut repaired, &self.removals)?;
        compose::relocate_symbols(&mut repaired, &self.relocated_symbols)?;
        Ok(repaired)
    }
}

/// How a realizer must name a reconnected net on a page when its wiring alone
/// cannot express the connection. Label scope is connectivity semantics, so
/// PCB decides it; the realizer only chooses where the driver sits.
#[derive(Debug, Clone, PartialEq)]
pub enum NetDriverKind {
    /// The netlist specifies a KiCad power symbol for this net.
    NetSymbol(NetSymbolSpec),
    /// A hierarchical label carrying one of the page's interface names.
    Hierarchical { names: BTreeSet<String> },
    /// A global label: the net's pins span pages no interface bridges.
    Global,
    /// A local label.
    Local,
}

/// Plans the pure connectivity-recovery intent for a set of inspected issues.
///
/// `forced_removals` are items the caller has decided to remove regardless of
/// analysis (an explicit rip-up). They join the intent's removals, the nets
/// they carried are queued for reconnection, and any conflict touching the
/// selection is still cut minimally on top of them.
pub fn plan_connectivity_repair(
    document: &SchDocument,
    netlist: &Schematic,
    inspection: &ConnectivityInspection,
    selected_keys: &BTreeSet<SchematicIssueKey>,
    forced_removals: &BTreeSet<ConnectivityItemRef>,
) -> Result<ConnectivityRepairIntent> {
    let mut intent = plan_connectivity_repair_core(
        document,
        netlist,
        inspection,
        selected_keys,
        forced_removals,
    )?;
    intent.drivers = compose::plan_net_driver_kinds(document, netlist, &intent.reconnect_nets)?;
    Ok(intent)
}

/// The intent without driver kinds. PCB's own realizer derives drivers with
/// the same rule while it places them, so it never reads the map.
pub(crate) fn plan_connectivity_repair_core(
    document: &SchDocument,
    netlist: &Schematic,
    inspection: &ConnectivityInspection,
    selected_keys: &BTreeSet<SchematicIssueKey>,
    forced_removals: &BTreeSet<ConnectivityItemRef>,
) -> Result<ConnectivityRepairIntent> {
    let expected = &inspection.expected;
    let observed = &inspection.physical;
    let expected_nets = ExpectedNets::new(expected);
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
    let selected_problems = selected
        .iter()
        .flat_map(|context| repair_problems(&context.issue))
        .collect::<BTreeSet<_>>();
    let mut reconnect_nets = BTreeSet::new();
    let mut selected_no_connects = BTreeSet::new();

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
                        reconnect_nets.extend(expected_nets.for_island(provenance));
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
                        reconnect_nets.extend(expected_nets.for_island(provenance));
                    }
                }
            }
            SchematicIssue::UnexpectedConnection { islands, .. } => {
                for island in islands {
                    if let Some(provenance) = observed.islands.get(island) {
                        reconnect_nets.extend(expected_nets.for_island(provenance));
                    }
                }
            }
            SchematicIssue::MissingNoConnect {
                page_id,
                symbol_id,
                pin_number,
            } => {
                selected_no_connects.insert((
                    page_id.clone(),
                    symbol_id.clone(),
                    pin_number.clone(),
                ));
            }
            SchematicIssue::UnexpectedNoConnect { page_id, id } => {
                removals.insert(ConnectivityItemRef::NoConnect {
                    page_id: page_id.clone(),
                    id: id.clone(),
                });
            }
            SchematicIssue::MissingSheet { .. }
            | SchematicIssue::UnboundSymbol { .. }
            | SchematicIssue::MissingSymbol { .. }
            | SchematicIssue::DuplicateSymbol { .. }
            | SchematicIssue::MismatchedSymbolId { .. }
            | SchematicIssue::UnexpectedSymbol { .. } => {}
        }
    }

    for item in forced_removals {
        if !item.is_removable() {
            bail!("connectivity item {item:?} cannot be removed by a repair");
        }
        // A symbol is removable only as a name driver (a power symbol). A
        // component symbol is design content: deleting it leaves a missing
        // symbol that no verification can accept.
        if item.is_symbol()
            && !observed.islands.values().any(|island| {
                island
                    .named_drivers
                    .values()
                    .any(|drivers| drivers.contains(item))
            })
        {
            bail!("connectivity item {item:?} is not a removable driver symbol");
        }
        removals.insert(item.clone());
        for provenance in observed
            .islands
            .values()
            .filter(|island| island.items.contains(item))
        {
            reconnect_nets.extend(expected_nets.for_island(provenance));
        }
    }

    // Remove exact unexpected name drivers and forced items first. This can
    // also resolve a short without requiring a physical topology edit.
    let mut simulated = document.clone();
    remove_items(&mut simulated, &removals)?;
    let mut current_observed = Cow::Borrowed(observed);
    let mut current_analysis = Cow::Borrowed(&inspection.analysis);
    if !removals.is_empty() {
        current_observed = Cow::Owned(observed_reconcilable_connectivity(&simulated, netlist)?);
        current_analysis = Cow::Owned(analyze_connectivity(expected, &current_observed.graph));
    }

    loop {
        let current_problems = repair_problem_counts(current_analysis.issues());
        let participates = |issue: &SchematicIssue| {
            if !matches!(
                issue,
                SchematicIssue::Shorted { .. } | SchematicIssue::UnexpectedConnection { .. }
            ) {
                return false;
            }
            let context = issue_context(issue.clone(), &current_observed.islands);
            selected.iter().any(|selected| &selected.issue == issue)
                || !context.items.is_disjoint(&selected_items)
                || !repair_problems(issue).is_disjoint(&selected_problems)
        };
        let Some(issue) = current_analysis
            .issues()
            .iter()
            .find(|issue| participates(issue))
        else {
            break;
        };

        if matches!(issue, SchematicIssue::UnexpectedConnection { .. }) {
            let unexpected = current_analysis
                .issues()
                .iter()
                .filter(|issue| {
                    matches!(issue, SchematicIssue::UnexpectedConnection { .. })
                        && participates(issue)
                })
                .collect::<Vec<_>>();
            if let Some(batch) = verified_unexpected_connection_cuts(
                &mut simulated,
                netlist,
                expected,
                &current_observed.islands,
                &current_problems,
                &unexpected,
            )? {
                current_observed = Cow::Owned(batch.observed);
                current_analysis = Cow::Owned(batch.analysis);
                removals.extend(batch.removals);
                reconnect_nets.extend(batch.reconnect_nets);
                continue;
            }
        }

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
                    reconnect_nets.extend(expected_nets.for_island(provenance));
                }
            }
        }

        let candidates = repair_candidates(issue, &expected_nets, &current_observed.islands);
        if let Some(cut) = minimum_verified_cut(
            &mut simulated,
            netlist,
            expected,
            &current_problems,
            issue,
            &candidates,
        )? {
            current_observed = Cow::Owned(cut.observed);
            current_analysis = Cow::Owned(cut.analysis);
            removals.extend(cut.removals);
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
                    // Relocation detaches every pin, not just the pin in the
                    // short. Rebuild the other nets that were valid before
                    // repair as well, using the original physical provenance.
                    for island in observed
                        .islands
                        .values()
                        .filter(|island| island.pins.iter().any(|pin| pin.is_on_symbol(&location)))
                    {
                        reconnect_nets.extend(expected_nets.for_island(island));
                    }
                    remove_items(
                        &mut simulated,
                        &BTreeSet::from([ConnectivityItemRef::Symbol {
                            page_id: location.page_id,
                            id: location.symbol_id,
                        }]),
                    )?;
                }
            }
        } else {
            remove_items(&mut simulated, &fallback)?;
            removals.extend(fallback);
        }
        current_observed = Cow::Owned(observed_reconcilable_connectivity(&simulated, netlist)?);
        current_analysis = Cow::Owned(analyze_connectivity(expected, &current_observed.graph));
    }

    removals.extend(orphaned_junctions(document, &simulated, &removals));
    removals.extend(no_connects_on_relocated_symbols(
        document,
        netlist,
        &relocate_symbols,
    )?);

    // Additions are coordinates in the document after this intent's edits.
    // Relocation leaves pin-attached annotations behind, so remove those and
    // regenerate NC markers on the moved pins along with explicitly selected
    // missing-marker issues.
    let mut post_edits = document.clone();
    remove_items(&mut post_edits, &removals)?;
    compose::relocate_symbols(&mut post_edits, &relocate_symbols)?;
    let required_no_connects = inspect_no_connects(&post_edits, netlist)?
        .0
        .into_iter()
        .filter(|target| {
            !has_no_connect(&post_edits, target)
                && (selected_no_connects.contains(&(
                    target.page_id.clone(),
                    target.symbol_id.clone(),
                    target.pin_number.clone(),
                )) || relocate_symbols.contains(&SymbolLocation {
                    page_id: target.page_id.clone(),
                    symbol_id: target.symbol_id.clone(),
                }))
        })
        .collect::<Vec<_>>();
    // Every physical pin requirement participates in issue selection above.
    // One native marker satisfies all pins that deliberately share a point.
    let mut no_connect_additions = Vec::<NoConnectTarget>::new();
    for target in required_no_connects {
        if !no_connect_additions.iter().any(|existing| {
            existing.page_id == target.page_id && points_connect(existing.at, target.at)
        }) {
            no_connect_additions.push(target);
        }
    }
    Ok(ConnectivityRepairIntent {
        selected_keys: selected_keys.clone(),
        removals,
        relocated_symbols: relocate_symbols,
        reconnect_nets,
        drivers: BTreeMap::new(),
        no_connect_additions,
    })
}

fn no_connects_on_relocated_symbols(
    document: &SchDocument,
    netlist: &Schematic,
    locations: &BTreeSet<SymbolLocation>,
) -> Result<BTreeSet<ConnectivityItemRef>> {
    if locations.is_empty() {
        return Ok(BTreeSet::new());
    }
    let physical = reduce_with_provenance(document, PinVisibility::IncludeHidden)?;
    let pins = physical
        .islands
        .values()
        .flat_map(|island| island.pin_terminals.iter())
        .map(|(pin, terminal)| (pin, terminal, pin.no_connect_target()))
        .collect::<Vec<_>>();
    let expected_no_connects = crate::connectivity::not_connected_terminals(netlist);
    Ok(document
        .pages
        .iter()
        .flat_map(|page| {
            let pins = &pins;
            let expected_no_connects = &expected_no_connects;
            page.items.iter().filter_map(move |item| match item {
                SchItem::NoConnect(marker)
                    if pins.iter().any(|(pin, _, target)| {
                        locations.iter().any(|location| pin.is_on_symbol(location))
                            && target.page_id == page.id
                            && points_connect(target.at, marker.at)
                    }) && !pins.iter().any(|(pin, terminal, target)| {
                        !locations.iter().any(|location| pin.is_on_symbol(location))
                            && expected_no_connects
                                .iter()
                                .any(|expected| terminal.matches(expected))
                            && target.page_id == page.id
                            && points_connect(target.at, marker.at)
                    }) =>
                {
                    Some(ConnectivityItemRef::NoConnect {
                        page_id: page.id.clone(),
                        id: marker.id.clone(),
                    })
                }
                _ => None,
            })
        })
        .collect())
}

/// Verify a realized repair against the inspection it was planned from.
///
/// The realizer may be PCB's own driver placement or a consumer's router; both
/// must meet the same postcondition: every selected issue is gone, no issue
/// appeared, and nothing outside the intent's removals and relocations
/// changed. Returns the inspection of the repaired document.
pub fn verify_connectivity_repair(
    before: &SchDocument,
    before_inspection: &ConnectivityInspection,
    netlist: &Schematic,
    intent: &ConnectivityRepairIntent,
    after: &SchDocument,
) -> Result<ConnectivityInspection> {
    let after_inspection = inspect_schematic(after, netlist)?;
    ensure_issues_resolved(&after_inspection, &intent.selected_keys, "repair")?;
    ensure_no_new_issues(before_inspection, &after_inspection, "repair")?;
    ensure_items_preserved(before, after, intent)?;
    Ok(after_inspection)
}

fn ensure_items_preserved(
    before: &SchDocument,
    after: &SchDocument,
    intent: &ConnectivityRepairIntent,
) -> Result<()> {
    if before.root_page_ids != after.root_page_ids {
        bail!("repair changed the schematic root pages");
    }
    ensure_pages_preserved(before, after)?;
    for (page, after_page) in before.pages.iter().zip(&after.pages) {
        if page.file_name != after_page.file_name || page.paper != after_page.paper {
            bail!(
                "repair changed the file name or paper of schematic page '{}'",
                page.id
            );
        }
        ensure_library_preserved(page, after_page)?;
        ensure_opaque_items_preserved(page, after_page)?;
        for item in &page.items {
            let Some(id) = item.id() else {
                continue;
            };
            let after_item = after_page
                .items
                .iter()
                .find(|candidate| candidate.id() == Some(id));
            // An item the intent removes may be gone, or kept exactly as it
            // was: a realizer that re-establishes a tee where PCB dropped an
            // orphaned junction has changed nothing the issue checks do not
            // already judge.
            if let Some(item_ref) = intent
                .removals
                .iter()
                .find(|removal| item_matches(&page.id, item, removal))
            {
                if after_item.is_some_and(|after_item| after_item != item) {
                    bail!("repair modified {item_ref:?}, which the intent removes");
                }
                continue;
            }
            let Some(after_item) = after_item else {
                bail!(
                    "repair removed item '{id}' on page '{}' outside the intent",
                    page.id
                );
            };
            let expected = match item {
                SchItem::Symbol(symbol)
                    if intent.relocated_symbols.contains(&SymbolLocation {
                        page_id: page.id.clone(),
                        symbol_id: id.to_string(),
                    }) =>
                {
                    let relocated = matches!(after_item, SchItem::Symbol(after_symbol)
                        if symbol_differs_only_by_relocation(symbol, after_symbol));
                    if !relocated {
                        bail!(
                            "repair changed relocated symbol '{id}' on page '{}' beyond its position",
                            page.id
                        );
                    }
                    continue;
                }
                SchItem::Sheet(sheet) => {
                    let mut sheet = sheet.clone();
                    sheet.pins.retain(|pin| {
                        !intent.removals.contains(&ConnectivityItemRef::SheetPin {
                            page_id: page.id.clone(),
                            sheet_id: sheet.id.clone(),
                            pin_id: pin.id.clone(),
                        })
                    });
                    SchItem::Sheet(sheet)
                }
                _ => item.clone(),
            };
            if *after_item != expected {
                bail!(
                    "repair modified item '{id}' on page '{}' outside the intent",
                    page.id
                );
            }
        }
    }
    Ok(())
}

/// The repaired document carries the same pages in the same order.
fn ensure_pages_preserved(before: &SchDocument, after: &SchDocument) -> Result<()> {
    for page in &after.pages {
        if !before.pages.iter().any(|candidate| candidate.id == page.id) {
            bail!("repair added schematic page '{}'", page.id);
        }
    }
    for page in &before.pages {
        if !after.pages.iter().any(|candidate| candidate.id == page.id) {
            bail!("repair removed schematic page '{}'", page.id);
        }
    }
    if before
        .pages
        .iter()
        .map(|page| &page.id)
        .ne(after.pages.iter().map(|page| &page.id))
    {
        bail!("repair reordered the schematic pages");
    }
    Ok(())
}

/// Existing symbol definitions stay as they were; a realizer may add the
/// definitions of the net symbols it places.
fn ensure_library_preserved(page: &SchPage, after_page: &SchPage) -> Result<()> {
    for (lib_id, definition) in &page.library.definitions {
        match after_page.library.definitions.get(lib_id) {
            Some(after_definition) if after_definition == definition => {}
            Some(_) => bail!(
                "repair modified symbol definition '{lib_id}' on page '{}'",
                page.id
            ),
            None => bail!(
                "repair removed symbol definition '{lib_id}' on page '{}'",
                page.id
            ),
        }
    }
    if page.library.unsupported != after_page.library.unsupported {
        bail!(
            "repair changed uninterpreted symbol library content on page '{}'",
            page.id
        );
    }
    Ok(())
}

/// Items without a typed identity can only be matched by content, so the
/// repaired page must carry exactly the same multiset of them.
fn ensure_opaque_items_preserved(page: &SchPage, after_page: &SchPage) -> Result<()> {
    let mut remaining = after_page
        .items
        .iter()
        .filter_map(|item| match item {
            SchItem::Unsupported(sexpr) => Some(sexpr),
            _ => None,
        })
        .collect::<Vec<_>>();
    for item in &page.items {
        let SchItem::Unsupported(sexpr) = item else {
            continue;
        };
        let Some(position) = remaining.iter().position(|candidate| *candidate == sexpr) else {
            bail!(
                "repair removed or modified an uninterpreted item on page '{}'",
                page.id
            );
        };
        remaining.swap_remove(position);
    }
    if !remaining.is_empty() {
        bail!("repair added an uninterpreted item on page '{}'", page.id);
    }
    Ok(())
}

/// A relocated symbol may sit elsewhere, with its fields carried along by the
/// same offset; everything else about it must be exactly as it was.
fn symbol_differs_only_by_relocation(before: &Symbol, after: &Symbol) -> bool {
    let delta = Point::new(after.at.x - before.at.x, after.at.y - before.at.y);
    let mut normalized = after.clone();
    normalized.at = before.at;
    for (name, field) in &mut normalized.fields {
        let Some(original) = before.fields.get(name) else {
            return false;
        };
        let carried = Point::new(field.at.x - delta.x, field.at.y - delta.y);
        if (carried.x - original.at.x).abs() > GEOMETRY_EPS_MM
            || (carried.y - original.at.y).abs() > GEOMETRY_EPS_MM
        {
            return false;
        }
        field.at = original.at;
    }
    normalized == *before
}

/// Junctions left with fewer than two contacts once the removals are applied,
/// restricted to junctions that sat on a removed wire so unrelated litter
/// stays exactly as it was. Wires, labels, no-connects, sheet pins, and symbol
/// pins at the junction point all count as contacts.
fn orphaned_junctions(
    original: &SchDocument,
    remaining: &SchDocument,
    removals: &BTreeSet<ConnectivityItemRef>,
) -> BTreeSet<ConnectivityItemRef> {
    let mut orphaned = BTreeSet::new();
    for page in &original.pages {
        let removed_segments = page
            .items
            .iter()
            .filter_map(|item| match item {
                SchItem::Wire(wire)
                    if removals.contains(&ConnectivityItemRef::Wire {
                        page_id: page.id.clone(),
                        id: wire.id.clone(),
                    }) =>
                {
                    Some((wire.a, wire.b))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        if removed_segments.is_empty() {
            continue;
        }
        let Some(remaining_page) = remaining
            .pages
            .iter()
            .find(|candidate| candidate.id == page.id)
        else {
            continue;
        };
        let mut remaining_wires = Vec::new();
        let mut remaining_points = Vec::new();
        for item in &remaining_page.items {
            match item {
                SchItem::Wire(wire) => remaining_wires.push((wire.a, wire.b)),
                SchItem::Label(label) => remaining_points.push(label.at),
                SchItem::NoConnect(no_connect) => remaining_points.push(no_connect.at),
                SchItem::Symbol(symbol) => {
                    if let Some(definition) = remaining_page.library.definitions.get(&symbol.lib_id)
                        && let Ok(pins) = definition.placed_pins(symbol)
                    {
                        remaining_points.extend(pins.into_iter().map(|pin| pin.point));
                    }
                }
                SchItem::Sheet(sheet) => {
                    remaining_points.extend(sheet.pins.iter().map(|pin| pin.at));
                }
                SchItem::Junction(_) | SchItem::Graphic(_) | SchItem::Unsupported(_) => {}
            }
        }
        for junction in page.items.iter().filter_map(|item| match item {
            SchItem::Junction(junction) => Some(junction),
            _ => None,
        }) {
            let junction_ref = ConnectivityItemRef::Junction {
                page_id: page.id.clone(),
                id: junction.id.clone(),
            };
            if removals.contains(&junction_ref)
                || !removed_segments
                    .iter()
                    .any(|(a, b)| point_on_segment(junction.at, *a, *b))
            {
                continue;
            }
            let contacts = remaining_wires
                .iter()
                .filter(|(a, b)| point_on_segment(junction.at, *a, *b))
                .count()
                + remaining_points
                    .iter()
                    .filter(|point| point_on_segment(**point, junction.at, junction.at))
                    .count();
            if contacts < 2 {
                orphaned.insert(junction_ref);
            }
        }
    }
    orphaned
}

/// The least-cost set of wires and junctions whose removal separates the
/// conflicting sides of an issue, verified through the reducer.
///
/// For a short, one net's pins and drivers are separated from the other
/// shorted nets'. For an unexpected connection, the offending pin is separated
/// from everything else it touches. `None` when the geometric model has no
/// finite cut (the sides meet only through labels or coincident pins) or the
/// reducer does not confirm the cut, so the caller falls back to teardown.
fn minimum_verified_cut(
    document: &mut SchDocument,
    netlist: &Schematic,
    expected: &ConnectivityGraph,
    current_problems: &BTreeMap<RepairProblem, usize>,
    issue: &SchematicIssue,
    candidates: &BTreeSet<ConnectivityItemRef>,
) -> Result<Option<VerifiedCuts>> {
    if candidates.is_empty() {
        return Ok(None);
    }
    let graph = cut_graph(document, PinVisibility::VisibleOnly)?;
    let node_nets = node_expected_nets(expected, &graph.nodes);
    let nodes_with = |predicate: &dyn Fn(usize, &CutNode) -> bool| {
        graph
            .nodes
            .iter()
            .enumerate()
            .filter(|(index, node)| predicate(*index, node))
            .map(|(index, _)| index)
            .collect::<BTreeSet<_>>()
    };
    let (sources, sinks) = match issue {
        SchematicIssue::Shorted { net_names, .. } => {
            let Some(first) = net_names.iter().next() else {
                return Ok(None);
            };
            let sources = nodes_with(&|index, _| node_nets[index].contains(first));
            let sinks = nodes_with(&|index, _| {
                node_nets[index]
                    .iter()
                    .any(|name| name != first && net_names.contains(name))
            });
            (sources, sinks)
        }
        SchematicIssue::UnexpectedConnection { terminals, .. } => {
            let matches_terminal = |node: &CutNode, terminal: &Terminal| {
                node.terminals
                    .iter()
                    .any(|observed| terminal.matches(observed) || observed.matches(terminal))
            };
            let Some(first) = terminals.first() else {
                return Ok(None);
            };
            let sources = nodes_with(&|_, node| matches_terminal(node, first));
            // The pin's own connection point is uncuttable and sits on its
            // side; every other point, pin, and driver is what it must leave.
            let mut adjacent = BTreeSet::new();
            for (a, b) in &graph.edges {
                if sources.contains(a) {
                    adjacent.insert(*b);
                }
                if sources.contains(b) {
                    adjacent.insert(*a);
                }
            }
            // Nothing on the offending pin's own net is a sink; the cut must
            // not sever the pin from its label just to satisfy the flow.
            let source_nets = sources
                .iter()
                .flat_map(|index| node_nets[*index].iter().cloned())
                .collect::<BTreeSet<_>>();
            let sinks = nodes_with(&|index, node| {
                !sources.contains(&index)
                    && !adjacent.contains(&index)
                    && node_nets[index].is_disjoint(&source_nets)
                    && !node
                        .driver_names
                        .iter()
                        .any(|name| source_nets.contains(name))
                    && (node.item.is_none()
                        || !node_nets[index].is_empty()
                        || !node.driver_names.is_empty()
                        || terminals[1..]
                            .iter()
                            .any(|terminal| matches_terminal(node, terminal)))
            });
            (sources, sinks)
        }
        _ => return Ok(None),
    };
    let Some(cut) = cut::minimum_node_cut(&graph, &sources, &sinks, |index| {
        graph.nodes[index]
            .item
            .as_ref()
            .filter(|item| candidates.contains(item))
            .map(connectivity_item_cut_cost)
    }) else {
        return Ok(None);
    };
    let cut = cut
        .into_iter()
        .filter_map(|index| graph.nodes[index].item.clone())
        .collect::<BTreeSet<_>>();
    if cut.is_empty() {
        return Ok(None);
    }
    Ok(
        verify_cuts(document, netlist, expected, current_problems, &cut)?.map(
            |(observed, analysis)| VerifiedCuts {
                observed,
                analysis,
                removals: cut,
                reconnect_nets: BTreeSet::new(),
            },
        ),
    )
}

/// Expected nets a node belongs to, through its terminals or the names it
/// drives.
fn node_expected_nets(expected: &ConnectivityGraph, nodes: &[CutNode]) -> Vec<BTreeSet<String>> {
    let mut terminals = TerminalIndex::new();
    let names = expected
        .groups
        .iter()
        .filter_map(logical_name)
        .collect::<BTreeSet<_>>();
    for (index, group) in expected.groups.iter().enumerate() {
        for terminal in &group.terminals {
            terminals.insert(terminal, index);
        }
    }
    nodes
        .iter()
        .map(|node| {
            node.terminals
                .iter()
                .flat_map(|terminal| terminals.matching(terminal))
                .filter_map(|index| logical_name(&expected.groups[index]))
                .chain(
                    node.driver_names
                        .iter()
                        .map(String::as_str)
                        .filter(|name| names.contains(name)),
                )
                .map(str::to_string)
                .collect()
        })
        .collect()
}

struct VerifiedCuts {
    observed: PhysicalConnectivity,
    analysis: ConnectivityAnalysis,
    removals: BTreeSet<ConnectivityItemRef>,
    reconnect_nets: BTreeSet<String>,
}

/// Isolate unexpected pins on a deletion-only graph, then verify the entire
/// batch through the real reducer. Wires/junctions cannot merge islands, so
/// other independent conflicts do not need to be reanalyzed after every cut.
/// The cut graph approximates legacy power semantics: if verification rejects
/// the batch, the caller retains the individually verified repair path.
fn verified_unexpected_connection_cuts(
    document: &mut SchDocument,
    netlist: &Schematic,
    expected: &ConnectivityGraph,
    islands: &BTreeMap<crate::connectivity::IslandRef, PhysicalIsland>,
    current_problems: &BTreeMap<RepairProblem, usize>,
    issues: &[&SchematicIssue],
) -> Result<Option<VerifiedCuts>> {
    let expected_nets = ExpectedNets::new(expected);
    let issues = issues
        .iter()
        .filter_map(|issue| {
            let candidates = repair_candidates(issue, &expected_nets, islands);
            (!candidates.is_empty()).then_some((*issue, candidates))
        })
        .collect::<Vec<_>>();
    if issues.is_empty() {
        return Ok(None);
    }
    let graph = cut_graph(document, PinVisibility::VisibleOnly)?;
    let node_nets = node_expected_nets(expected, &graph.nodes);
    let expected_names = expected
        .groups
        .iter()
        .flat_map(|group| &group.names)
        .collect::<BTreeSet<_>>();
    let mut expected_terminals = TerminalIndex::new();
    for group in &expected.groups {
        for terminal in &group.terminals {
            expected_terminals.insert(terminal, 0);
        }
    }
    let matches_expected = graph
        .nodes
        .iter()
        .map(|node| {
            node.driver_names
                .iter()
                .any(|name| expected_names.contains(name))
                || node
                    .terminals
                    .iter()
                    .any(|terminal| expected_terminals.matching(terminal).next().is_some())
        })
        .collect::<Vec<_>>();
    let mut terminals = TerminalIndex::new();
    let mut item_nodes = BTreeMap::<&ConnectivityItemRef, Vec<usize>>::new();
    let mut adjacency = vec![Vec::new(); graph.nodes.len()];
    for (index, node) in graph.nodes.iter().enumerate() {
        if let Some(item) = &node.item {
            item_nodes.entry(item).or_default().push(index);
        }
        for terminal in &node.terminals {
            terminals.insert(terminal, index);
        }
    }
    for &(a, b) in &graph.edges {
        adjacency[a].push(b);
        adjacency[b].push(a);
    }
    let mut active = vec![true; graph.nodes.len()];
    let mut removals = BTreeSet::new();
    let mut reconnect_nets = BTreeSet::new();
    for (issue, candidates) in issues {
        let SchematicIssue::UnexpectedConnection {
            terminals: unexpected,
            islands: issue_islands,
        } = issue
        else {
            continue;
        };
        let unexpected_nodes = unexpected
            .iter()
            .flat_map(|terminal| terminals.matching(terminal))
            .collect::<BTreeSet<_>>();
        let mut changed = false;
        for terminal in unexpected {
            let sources = terminals
                .matching(terminal)
                .filter(|&index| active[index])
                .collect::<BTreeSet<_>>();
            let mut reachable = vec![false; graph.nodes.len()];
            let mut pending = sources.iter().copied().collect::<Vec<_>>();
            let mut region = Vec::new();
            let mut connected_terminals = BTreeSet::new();
            let mut connected_to_expected = false;
            while let Some(node) = pending.pop() {
                if reachable[node] || !active[node] {
                    continue;
                }
                reachable[node] = true;
                region.push(node);
                connected_terminals.extend(graph.nodes[node].terminals.iter());
                connected_to_expected |= matches_expected[node];
                pending.extend(adjacency[node].iter().copied());
            }
            if connected_terminals.len() < 2 && !connected_to_expected {
                continue;
            }
            region.sort_unstable();
            let adjacent = sources
                .iter()
                .flat_map(|&node| adjacency[node].iter().copied())
                .collect::<BTreeSet<_>>();
            let source_nets = sources
                .iter()
                .flat_map(|&index| node_nets[index].iter().cloned())
                .collect::<BTreeSet<_>>();
            let sinks = region
                .iter()
                .copied()
                .filter(|index| {
                    let node = &graph.nodes[*index];
                    !sources.contains(index)
                        && !adjacent.contains(index)
                        && node_nets[*index].is_disjoint(&source_nets)
                        && !node
                            .driver_names
                            .iter()
                            .any(|name| source_nets.contains(name))
                        && (node.item.is_none()
                            || !node_nets[*index].is_empty()
                            || !node.driver_names.is_empty()
                            || unexpected_nodes.contains(index))
                })
                .collect::<BTreeSet<_>>();
            let Some(cut) =
                cut::minimum_node_cut_in_region(&graph, &sources, &sinks, &region, |index| {
                    graph.nodes[index]
                        .item
                        .as_ref()
                        .filter(|item| candidates.contains(item))
                        .map(connectivity_item_cut_cost)
                })
            else {
                continue;
            };
            let cut = cut
                .into_iter()
                .filter_map(|index| graph.nodes[index].item.clone())
                .collect::<BTreeSet<_>>();
            // One physical item can occur in several instances of a sheet.
            for item in &cut {
                for &index in &item_nodes[item] {
                    active[index] = false;
                }
            }
            removals.extend(cut);
            changed = true;
        }
        if changed {
            for island in issue_islands {
                if let Some(island) = islands.get(island) {
                    reconnect_nets.extend(expected_nets.for_island(island));
                }
            }
        }
    }
    if removals.is_empty() {
        return Ok(None);
    }
    let Some((observed, analysis)) =
        verify_cuts(document, netlist, expected, current_problems, &removals)?
    else {
        return Ok(None);
    };
    Ok(Some(VerifiedCuts {
        observed,
        analysis,
        removals,
        reconnect_nets,
    }))
}

/// Speculate on the working document, retaining only the removed items for
/// rollback rather than cloning all page libraries. Cuts never edit sheet
/// pins or other retained items. Restore exact order on rejection or error.
fn verify_cuts(
    document: &mut SchDocument,
    netlist: &Schematic,
    expected: &ConnectivityGraph,
    current_problems: &BTreeMap<RepairProblem, usize>,
    removals: &BTreeSet<ConnectivityItemRef>,
) -> Result<Option<(PhysicalConnectivity, ConnectivityAnalysis)>> {
    let by_page = index_removals(removals);
    let mut saved = Vec::new();
    for (page_index, page) in document.pages.iter().enumerate() {
        let Some(by_id) = by_page.get(page.id.as_str()) else {
            continue;
        };
        for (item_index, item) in page.items.iter().enumerate() {
            if item
                .id()
                .and_then(|id| by_id.get(id))
                .is_some_and(|candidates| {
                    candidates
                        .iter()
                        .any(|removal| item_matches(&page.id, item, removal))
                })
            {
                saved.push((page_index, item_index, item.clone()));
            }
        }
    }
    remove_items(document, removals)?;
    let result = observed_reconcilable_connectivity(document, netlist).map(|observed| {
        let analysis = analyze_connectivity(expected, &observed.graph);
        // Disconnected nets are rebuilt by the realizer. Only conflicts must
        // decrease, and no existing conflict may get worse.
        strictly_reduces_problems(current_problems, &repair_problem_counts(analysis.issues()))
            .then_some((observed, analysis))
    });
    if !matches!(&result, Ok(Some(_))) {
        for (page, index, item) in saved {
            document.pages[page].items.insert(index, item);
        }
    }
    result
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

/// Teardown fallback: every removable item across the issue's islands, for
/// an issue whose physical graph has no finite cut (a short through labels,
/// pins that coincide). The affected nets are queued for driver
/// reconnection, which rebuilds correct connectivity from the component pins.
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
        for problem in repair_problems(issue) {
            *result.entry(problem).or_default() += 1;
        }
    }
    result
}

fn repair_problems(issue: &SchematicIssue) -> BTreeSet<RepairProblem> {
    match issue {
        SchematicIssue::UnexpectedNet { net_name, .. } => {
            BTreeSet::from([RepairProblem::UnexpectedNet(net_name.clone())])
        }
        SchematicIssue::Shorted { net_names, .. } => {
            let names = net_names.iter().collect::<Vec<_>>();
            let mut result = BTreeSet::new();
            for (index, left) in names.iter().enumerate() {
                for right in &names[index + 1..] {
                    result.insert(RepairProblem::Shorted((*left).clone(), (*right).clone()));
                }
            }
            result
        }
        SchematicIssue::UnexpectedConnection { terminals, .. } => terminals
            .iter()
            .cloned()
            .map(RepairProblem::UnexpectedConnection)
            .collect(),
        SchematicIssue::MissingSheet { .. }
        | SchematicIssue::DisconnectedNet { .. }
        | SchematicIssue::MissingPort { .. }
        | SchematicIssue::UnboundSymbol { .. }
        | SchematicIssue::MissingSymbol { .. }
        | SchematicIssue::DuplicateSymbol { .. }
        | SchematicIssue::MismatchedSymbolId { .. }
        | SchematicIssue::UnexpectedSymbol { .. }
        | SchematicIssue::MissingNoConnect { .. }
        | SchematicIssue::UnexpectedNoConnect { .. } => BTreeSet::new(),
    }
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

/// Removing a wire is the cheapest repair, a junction breaks a tee, and a
/// power symbol is a named driver worth keeping. Only these three kinds are
/// ever cut candidates.
fn connectivity_item_cut_cost(item: &ConnectivityItemRef) -> u64 {
    match item {
        ConnectivityItemRef::Wire { .. } => 1,
        ConnectivityItemRef::Junction { .. } => 2,
        ConnectivityItemRef::Symbol { .. } => 8,
        ConnectivityItemRef::NoConnect { .. }
        | ConnectivityItemRef::Label { .. }
        | ConnectivityItemRef::SheetPin { .. } => u64::MAX / 4,
    }
}

fn repair_candidates(
    issue: &SchematicIssue,
    expected: &ExpectedNets<'_>,
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

/// Whether `point` lies on the closed segment `a`-`b`.
pub(crate) fn point_on_segment(point: Point, a: Point, b: Point) -> bool {
    let cross = (point.x - a.x) * (b.y - a.y) - (point.y - a.y) * (b.x - a.x);
    cross.abs() <= GEOMETRY_EPS_MM
        && point.x >= a.x.min(b.x) - GEOMETRY_EPS_MM
        && point.x <= a.x.max(b.x) + GEOMETRY_EPS_MM
        && point.y >= a.y.min(b.y) - GEOMETRY_EPS_MM
        && point.y <= a.y.max(b.y) + GEOMETRY_EPS_MM
}

fn repair_island(
    issue: &SchematicIssue,
    expected: &ExpectedNets<'_>,
    island: &PhysicalIsland,
) -> bool {
    match issue {
        SchematicIssue::Shorted { net_names, .. } => expected
            .for_island(island)
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
                "KiCad directly connects component terminals that should be separate: {descriptions}. no set of wires or junctions separates these terminals"
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

struct ExpectedNets<'a> {
    graph: &'a ConnectivityGraph,
    terminals: TerminalIndex<'a>,
    names: BTreeMap<&'a str, Vec<usize>>,
}

impl<'a> ExpectedNets<'a> {
    fn new(graph: &'a ConnectivityGraph) -> Self {
        let mut terminals = TerminalIndex::new();
        let mut names = BTreeMap::<&str, Vec<usize>>::new();
        for (index, group) in graph.groups.iter().enumerate() {
            for terminal in &group.terminals {
                terminals.insert(terminal, index);
            }
            for name in &group.names {
                names.entry(name).or_default().push(index);
            }
        }
        Self {
            graph,
            terminals,
            names,
        }
    }

    fn for_island(&self, island: &PhysicalIsland) -> BTreeSet<String> {
        island
            .terminals
            .iter()
            .flat_map(|terminal| self.terminals.matching(terminal))
            .chain(
                island
                    .names
                    .iter()
                    .filter_map(|name| self.names.get(name.as_str()))
                    .flatten()
                    .copied(),
            )
            .filter_map(|index| logical_name(&self.graph.groups[index]))
            .map(str::to_string)
            .collect()
    }
}

type RemovalIndex<'a> = BTreeMap<&'a str, BTreeMap<&'a str, Vec<&'a ConnectivityItemRef>>>;

fn index_removals(removals: &BTreeSet<ConnectivityItemRef>) -> RemovalIndex<'_> {
    let mut by_page = RemovalIndex::new();
    for removal in removals {
        let (page, id) = removal.page_and_item_id();
        by_page
            .entry(page)
            .or_default()
            .entry(id)
            .or_default()
            .push(removal);
    }
    by_page
}

pub(crate) fn remove_items(
    document: &mut SchDocument,
    removals: &BTreeSet<ConnectivityItemRef>,
) -> Result<()> {
    if removals.is_empty() {
        return Ok(());
    }
    let by_page = index_removals(removals);
    let mut counts = removals
        .iter()
        .map(|removal| (removal, 0usize))
        .collect::<BTreeMap<_, _>>();
    // Validate every address before changing anything, including duplicate
    // IDs and item-kind mismatches. Indexing limits comparisons to that ID.
    for page in &document.pages {
        let Some(by_id) = by_page.get(page.id.as_str()) else {
            continue;
        };
        for item in &page.items {
            let Some(candidates) = item.id().and_then(|id| by_id.get(id)) else {
                continue;
            };
            for &removal in candidates {
                let count = if let ConnectivityItemRef::SheetPin { pin_id, .. } = removal
                    && let SchItem::Sheet(sheet) = item
                {
                    sheet.pins.iter().filter(|pin| &pin.id == pin_id).count()
                } else {
                    usize::from(item_matches(&page.id, item, removal))
                };
                *counts.get_mut(removal).expect("indexed removal") += count;
            }
        }
    }
    for removal in removals {
        let count = counts[removal];
        if count != 1 {
            bail!("connectivity item {removal:?} matched {count} loaded schematic items");
        }
    }
    for page in &mut document.pages {
        let Some(by_id) = by_page.get(page.id.as_str()) else {
            continue;
        };
        page.items.retain_mut(|item| {
            let Some(candidates) = item.id().and_then(|id| by_id.get(id)) else {
                return true;
            };
            for &removal in candidates {
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

pub(crate) fn item_matches(page_id: &str, item: &SchItem, item_ref: &ConnectivityItemRef) -> bool {
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
    fn page_and_item_id(&self) -> (&str, &str) {
        match self {
            Self::Symbol { page_id, id }
            | Self::Wire { page_id, id }
            | Self::Junction { page_id, id }
            | Self::NoConnect { page_id, id }
            | Self::Label { page_id, id } => (page_id, id),
            Self::SheetPin {
                page_id, sheet_id, ..
            } => (page_id, sheet_id),
        }
    }

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

    use pcb_sexpr::Sexpr;

    use super::*;
    use crate::{
        connectivity::{ConnectionGroup, ConnectionOrigin, IslandRef},
        model::{
            Junction, LabelShape, Paper, Rotation, Sheet, SheetPin, SymbolDefinition, SymbolField,
            Wire,
        },
    };

    fn page_with(id: &str, items: Vec<SchItem>) -> SchPage {
        let mut page = SchPage::new(id);
        page.items = items;
        page
    }

    fn document(pages: Vec<SchPage>) -> SchDocument {
        SchDocument {
            root_page_ids: vec![pages[0].id.clone()],
            pages,
        }
    }

    fn note() -> SchItem {
        SchItem::Unsupported(Sexpr::list(vec![
            Sexpr::symbol("text"),
            Sexpr::string("keep me"),
        ]))
    }

    fn part(at: Point, field_at: Point) -> Symbol {
        Symbol {
            id: "part".to_string(),
            lib_id: "Test:Part".to_string(),
            unit: 1,
            body_style: 1,
            at,
            rotation: Rotation::default(),
            mirror: None,
            dnp: false,
            in_bom: true,
            on_board: true,
            in_pos_files: true,
            fields_autoplaced: false,
            fields: BTreeMap::from([(
                "Reference".to_string(),
                SymbolField::new("Reference", "R1", field_at),
            )]),
            pins: Vec::new(),
            unsupported: Vec::new(),
        }
    }

    fn unmanaged_chains(chains: usize, pins: usize) -> SchDocument {
        let definition = SymbolDefinition::from_kicad_symbol_sexpr(
            r#"(symbol "Test:Part" (symbol "Part_1_1"
              (pin passive line (at 0 0 0) (length 2.54) (name "1") (number "1"))))"#,
        )
        .unwrap();
        let mut page = SchPage::new("chains");
        page.library
            .definitions
            .insert(definition.lib_id.clone(), definition);
        for chain in 0..chains {
            for pin in 0..pins {
                let at = Point::new(pin as f64 * 9.0, chain as f64 * 10.0);
                let mut symbol = part(at, at);
                symbol.id = format!("part-{chain}-{pin}");
                page.items.push(SchItem::Symbol(symbol));
                if pin > 0 {
                    for segment in 0..3 {
                        page.items.push(SchItem::Wire(Wire {
                            id: format!("wire-{chain}-{pin}-{segment}"),
                            a: Point::new(at.x - 9.0 + segment as f64 * 3.0, at.y),
                            b: Point::new(at.x - 6.0 + segment as f64 * 3.0, at.y),
                            unsupported: Vec::new(),
                        }));
                    }
                }
            }
        }
        document(vec![page])
    }

    #[test]
    fn batches_only_selected_conflicts_and_preserves_wire_stubs() {
        let document = unmanaged_chains(32, 2);
        let original = document.clone();
        let netlist = Schematic::new();
        let inspection = inspect_schematic(&document, &netlist).unwrap();
        let conflicts = inspection
            .issues
            .iter()
            .filter(|issue| matches!(issue.issue, SchematicIssue::UnexpectedConnection { .. }))
            .collect::<Vec<_>>();
        assert_eq!(conflicts.len(), 32);
        let selected = conflicts
            .iter()
            .step_by(2)
            .map(|issue| issue.key.clone())
            .collect();
        let intent = plan_connectivity_repair(
            &document,
            &netlist,
            &inspection,
            &selected,
            &BTreeSet::new(),
        )
        .unwrap();
        assert_eq!(
            intent.removals.len(),
            16,
            "one cut per selected three-segment path"
        );
        assert!(intent.relocated_symbols.is_empty());
        assert!(intent.reconnect_nets.is_empty());
        let after = intent.apply_edits(&document).unwrap();
        let verified =
            verify_connectivity_repair(&document, &inspection, &netlist, &intent, &after).unwrap();
        for issue in conflicts.iter().skip(1).step_by(2) {
            assert!(
                verified
                    .issues
                    .iter()
                    .any(|remaining| remaining.key == issue.key),
                "unselected conflict changed"
            );
        }
        assert_eq!(document, original, "planning is pure");
        assert_eq!(
            intent,
            plan_connectivity_repair(
                &document,
                &netlist,
                &inspection,
                &selected,
                &BTreeSet::new()
            )
            .unwrap()
        );
    }

    #[test]
    fn batch_verification_rejects_an_unverified_cut() {
        let mut document = unmanaged_chains(2, 2);
        let original = document.clone();
        let netlist = Schematic::new();
        let inspection = inspect_schematic(&document, &netlist).unwrap();
        let issues = inspection
            .issues
            .iter()
            .map(|issue| &issue.issue)
            .filter(|issue| matches!(issue, SchematicIssue::UnexpectedConnection { .. }))
            .collect::<Vec<_>>();
        // A stale/overly optimistic conflict count cannot authorize edits.
        // The real reducer must reject the batch even though graph cuts exist.
        assert!(
            verified_unexpected_connection_cuts(
                &mut document,
                &netlist,
                &inspection.expected,
                &inspection.physical.islands,
                &BTreeMap::new(),
                &issues
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(document, original);
    }

    #[test]
    fn failed_cut_inspection_restores_exact_item_order() {
        let mut document = unmanaged_chains(2, 2);
        document.pages[0].library.definitions.clear();
        let original = document.clone();
        let cuts = BTreeSet::from([
            ConnectivityItemRef::Wire {
                page_id: "chains".into(),
                id: "wire-0-1-0".into(),
            },
            ConnectivityItemRef::Wire {
                page_id: "chains".into(),
                id: "wire-1-1-2".into(),
            },
        ]);
        assert!(
            verify_cuts(
                &mut document,
                &Schematic::new(),
                &ConnectivityGraph::default(),
                &BTreeMap::new(),
                &cuts
            )
            .is_err()
        );
        assert_eq!(document, original);
    }

    #[test]
    fn indexed_removals_validate_kind_page_and_duplicates_before_mutation() {
        let mut document = unmanaged_chains(1, 2);
        let wire = document.pages[0]
            .items
            .iter()
            .find(|item| matches!(item, SchItem::Wire(_)))
            .unwrap()
            .clone();
        let id = wire.id().unwrap().to_string();
        document.pages.push(page_with("other", vec![wire.clone()]));
        let original = document.clone();
        let selected = ConnectivityItemRef::Wire {
            page_id: "chains".into(),
            id: id.clone(),
        };
        let wrong_kind = ConnectivityItemRef::Symbol {
            page_id: "chains".into(),
            id: id.clone(),
        };
        assert!(
            remove_items(
                &mut document,
                &BTreeSet::from([selected.clone(), wrong_kind])
            )
            .is_err()
        );
        assert_eq!(document, original);
        document.pages[0].items.push(wire.clone());
        let duplicated = document.clone();
        assert!(remove_items(&mut document, &BTreeSet::from([selected.clone()])).is_err());
        assert_eq!(document, duplicated);
        document.pages[0].items.pop();
        remove_items(&mut document, &BTreeSet::from([selected])).unwrap();
        assert_eq!(document.pages[1].items, vec![wire]);
        assert!(
            !document.pages[0]
                .items
                .iter()
                .any(|item| item.id() == Some(id.as_str()))
        );
    }

    #[test]
    #[ignore = "release-mode synthetic performance benchmark; run explicitly"]
    fn benchmark_unexpected_connection_repairs() {
        for (chains, pins) in [(64, 2), (256, 2), (1024, 2), (1, 256)] {
            let document = unmanaged_chains(chains, pins);
            let netlist = Schematic::new();
            let inspection = inspect_schematic(&document, &netlist).unwrap();
            let selected = inspection
                .issues
                .iter()
                .filter(|issue| matches!(issue.issue, SchematicIssue::UnexpectedConnection { .. }))
                .map(|issue| issue.key.clone())
                .collect();
            let start = std::time::Instant::now();
            let intent = plan_connectivity_repair(
                &document,
                &netlist,
                &inspection,
                &selected,
                &BTreeSet::new(),
            )
            .unwrap();
            eprintln!(
                "chains={chains} pins={pins} planning={:?} removals={}",
                start.elapsed(),
                intent.removals.len()
            );
            if pins == 2 {
                assert_eq!(intent.removals.len(), chains);
            }
            let after = intent.apply_edits(&document).unwrap();
            verify_connectivity_repair(&document, &inspection, &netlist, &intent, &after).unwrap();
        }
    }

    fn preservation_error(before: &SchDocument, after: &SchDocument) -> String {
        ensure_items_preserved(before, after, &ConnectivityRepairIntent::additions_only())
            .expect_err("the change lies outside the intent")
            .to_string()
    }

    #[test]
    fn verification_rejects_document_changes_outside_the_intent() {
        let definition = SymbolDefinition::from_kicad_symbol_sexpr(
            r#"(symbol "Test:Part" (symbol "Part_1_1"
              (pin passive line (at 0 0 0) (length 2.54) (name "1") (number "1"))))"#,
        )
        .unwrap();
        let mut before = document(vec![page_with("page", vec![note()])]);
        before.pages[0]
            .library
            .definitions
            .insert(definition.lib_id.clone(), definition.clone());
        assert!(
            ensure_items_preserved(
                &before,
                &before,
                &ConnectivityRepairIntent::additions_only()
            )
            .is_ok()
        );

        let mut added_page = before.clone();
        added_page.pages.push(SchPage::new("extra"));
        assert!(preservation_error(&before, &added_page).contains("added schematic page"));

        let mut resized = before.clone();
        resized.pages[0].paper = Paper::Named {
            name: "A3".to_string(),
            portrait: false,
        };
        assert!(preservation_error(&before, &resized).contains("paper"));

        let mut renamed = before.clone();
        renamed.pages[0].file_name = Some("moved.kicad_sch".to_string());
        assert!(preservation_error(&before, &renamed).contains("file name"));

        let mut dropped_note = before.clone();
        dropped_note.pages[0].items.clear();
        assert!(preservation_error(&before, &dropped_note).contains("uninterpreted item"));

        let mut dropped_definition = before.clone();
        dropped_definition.pages[0].library.definitions.clear();
        assert!(
            preservation_error(&before, &dropped_definition).contains("removed symbol definition")
        );

        // Realizers add the definitions of the net symbols they place.
        let mut extended_library = before.clone();
        let mut power = definition.clone();
        power.lib_id = "power:GND".to_string();
        extended_library.pages[0]
            .library
            .definitions
            .insert(power.lib_id.clone(), power);
        assert!(
            ensure_items_preserved(
                &before,
                &extended_library,
                &ConnectivityRepairIntent::additions_only()
            )
            .is_ok()
        );
    }

    #[test]
    fn relocated_symbols_may_only_move_with_their_fields() {
        let before = document(vec![page_with(
            "page",
            vec![SchItem::Symbol(part(
                Point::new(0.0, 0.0),
                Point::new(1.0, 1.0),
            ))],
        )]);
        let mut intent = ConnectivityRepairIntent::additions_only();
        intent.relocated_symbols.insert(SymbolLocation {
            page_id: "page".to_string(),
            symbol_id: "part".to_string(),
        });

        let moved = document(vec![page_with(
            "page",
            vec![SchItem::Symbol(part(
                Point::new(10.0, 20.0),
                Point::new(11.0, 21.0),
            ))],
        )]);
        assert!(ensure_items_preserved(&before, &moved, &intent).is_ok());

        let mut rotated = moved.clone();
        let SchItem::Symbol(symbol) = &mut rotated.pages[0].items[0] else {
            unreachable!();
        };
        symbol.rotation = Rotation::Deg90;
        assert!(
            ensure_items_preserved(&before, &rotated, &intent)
                .unwrap_err()
                .to_string()
                .contains("beyond its position")
        );

        let field_left_behind = document(vec![page_with(
            "page",
            vec![SchItem::Symbol(part(
                Point::new(10.0, 20.0),
                Point::new(1.0, 1.0),
            ))],
        )]);
        assert!(
            ensure_items_preserved(&before, &field_left_behind, &intent)
                .unwrap_err()
                .to_string()
                .contains("beyond its position")
        );
    }

    #[test]
    fn junction_joining_a_sheet_pin_to_a_through_wire_is_not_orphaned() {
        let removed = SchItem::Wire(Wire {
            id: "removed".to_string(),
            a: Point::new(0.0, 0.0),
            b: Point::new(10.0, 0.0),
            unsupported: Vec::new(),
        });
        let through = SchItem::Wire(Wire {
            id: "through".to_string(),
            a: Point::new(10.0, -10.0),
            b: Point::new(10.0, 10.0),
            unsupported: Vec::new(),
        });
        let junction = SchItem::Junction(Junction {
            id: "junction".to_string(),
            at: Point::new(10.0, 0.0),
            unsupported: Vec::new(),
        });
        let sheet = SchItem::Sheet(Box::new(Sheet {
            id: "sheet".to_string(),
            placed: true,
            at: Some(Point::new(10.0, 0.0)),
            size: Some(Point::new(20.0, 20.0)),
            name: None,
            file: SymbolField::new("Sheetfile", "child.kicad_sch", Point::default()),
            pins: vec![SheetPin {
                id: "pin".to_string(),
                name: "NET".to_string(),
                at: Point::new(10.0, 0.0),
                rotation: Rotation::default(),
                shape: LabelShape::Bidirectional,
                unsupported: Vec::new(),
            }],
            unsupported: Vec::new(),
        }));
        let removals = BTreeSet::from([ConnectivityItemRef::Wire {
            page_id: "page".to_string(),
            id: "removed".to_string(),
        }]);
        let without = |original: &SchDocument| {
            let mut remaining = original.clone();
            remaining.pages[0]
                .items
                .retain(|item| item.id() != Some("removed"));
            remaining
        };

        let with_sheet = document(vec![page_with(
            "page",
            vec![removed.clone(), through.clone(), junction.clone(), sheet],
        )]);
        assert!(orphaned_junctions(&with_sheet, &without(&with_sheet), &removals).is_empty());

        let without_sheet = document(vec![page_with("page", vec![removed, through, junction])]);
        assert_eq!(
            orphaned_junctions(&without_sheet, &without(&without_sheet), &removals),
            BTreeSet::from([ConnectivityItemRef::Junction {
                page_id: "page".to_string(),
                id: "junction".to_string(),
            }])
        );
    }

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
            repair_candidates(&issue, &ExpectedNets::new(&expected), &islands),
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
            repair_candidates(
                &issue,
                &ExpectedNets::new(&ConnectivityGraph::default()),
                &islands
            ),
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
