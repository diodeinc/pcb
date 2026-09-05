use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use pcb_sch::{ATTR_SCHEMATIC_PATH, Instance, InstanceKind, Schematic};
use pcb_sexpr::Sexpr;

use crate::{
    CONNECTION_GRID_MM, GEOMETRY_EPS_MM, Label, LabelKind, LabelShape, LabelSpin, Paper, Point,
    Rotation, SchDocument, SchItem, SchPage, Sheet, SheetPin, Symbol, SymbolDefinition,
    SymbolField, SymbolSlotKey, Wire,
    analysis::{ConnectivityInspection, SchematicIssue, SchematicIssueKey},
    component_slots,
    connectivity::{
        ConnectionOrigin, ConnectivityItemRef, IslandRef, PhysicalConnectivity, PhysicalIsland,
        PhysicalPinRef, PinVisibility, SymbolLocation, named_connected_nets,
        reduce_with_provenance,
    },
    deterministic_uuid, field_autoplace, hierarchy, net_symbols,
    placement::{GridPacker, GridPoint, GridRect, point_rect},
    repair::{
        ConnectivityRepairIntent, NetDriverKind, item_matches, plan_connectivity_repair_core,
        point_on_segment, remove_items,
    },
    root_interface, root_page_id, symbol,
};

const LABEL_SHAPE_LENGTH_MM: f64 = 2.54;
const NET_SYMBOL_OFFSET_CELLS: f64 = 4.0;
const NET_SYMBOL_STAIR_CELLS: f64 = 4.0;
const NET_SYMBOL_STAIR_RETRIES: usize = 8;
const ESTIMATED_LABEL_WIDTH_EM: f64 = 0.8;
const SHEET_PIN_SPACING_MM: f64 = 5.08;
const SHEET_MIN_WIDTH_MM: f64 = 50.8;
const SHEET_MIN_HEIGHT_MM: f64 = 20.32;
const PLACEMENT_BLOCK_GAP_CELLS: i32 = 4;
const CONTEXT_LABEL_GAP_CELLS: i32 = 1;
const CAPACITOR_BANK_BUS_OFFSET_CELLS: f64 = 2.0;

pub(crate) fn reconcile_document(
    existing: Option<&SchDocument>,
    netlist: &Schematic,
    root_file_name: Option<&str>,
    issue_selection: Option<&BTreeSet<SchematicIssueKey>>,
    placement_page_id: Option<&str>,
    inspection_before: Option<&ConnectivityInspection>,
) -> Result<SchDocument> {
    if issue_selection.is_some_and(BTreeSet::is_empty) {
        return existing
            .cloned()
            .context("repairing selected issues requires an existing document");
    }
    let complete = issue_selection.is_none();
    // The generated document is not a canonical representation. Any existing
    // KiCad organization is valid when its connectivity matches the netlist;
    // managed symbol identities are the only reconciliation metadata we may
    // rely on. See ../DESIGN.md.
    let creating = existing.is_none();
    let mut document = match existing {
        Some(existing) => existing.clone(),
        None => SchDocument {
            pages: vec![SchPage {
                file_name: Some(
                    root_file_name
                        .context("initializing a schematic requires a root filename")?
                        .to_string(),
                ),
                ..SchPage::new(root_page_id())
            }],
            root_page_ids: vec![root_page_id()],
        },
    };
    if document.pages.is_empty() {
        bail!("KiCad schematic project has no pages");
    }
    let preserved_page_count = if creating { 0 } else { document.pages.len() };
    let default_page = document
        .root_page_ids
        .iter()
        .find_map(|id| document.pages.iter().position(|page| &page.id == id))
        .context("KiCad schematic project has no loaded root page")?;

    let existing_slots = existing_slot_locations(&document);
    let expected_slots = component_slots::component_symbol_slots(netlist)?
        .into_iter()
        .collect::<BTreeSet<_>>();
    let RepairTargets {
        project_slots,
        remove_slots,
        remove_locations,
        connectivity: selected_connectivity,
    } = repair_targets(issue_selection, inspection_before, &expected_slots)?;
    let instances = component_slots::component_instances(netlist)?;
    let mut selected_existing = BTreeMap::new();
    for slot in &project_slots {
        if let Some(symbol) = select_existing_symbol(slot, existing_slots.get(slot)) {
            selected_existing.insert(slot.clone(), symbol);
        }
    }
    let relocatable_slots = project_slots
        .iter()
        .filter(|slot| !selected_existing.contains_key(*slot))
        .cloned()
        .collect::<BTreeSet<_>>();

    // Hierarchy ownership comes from every managed symbol in the document,
    // including symbols that no longer exist in the target netlist. A stale
    // symbol still proves that its module already has a schematic page.
    let existing_component_pages = existing_slots.iter().fold(
        BTreeMap::<String, BTreeSet<usize>>::new(),
        |mut pages, (slot, candidates)| {
            pages
                .entry(slot.component_path().to_string())
                .or_default()
                .extend(candidates.iter().map(|candidate| candidate.page_index));
            pages
        },
    );
    let linked_modules = linked_modules(netlist)?;
    let linked_modules = if complete {
        linked_modules
    } else {
        linked_modules
            .into_iter()
            .filter(|module| {
                project_slots
                    .iter()
                    .any(|slot| hierarchy::is_descendant(slot.component_path(), &module.path))
            })
            .collect()
    };
    let existing_page_ids = document
        .pages
        .iter()
        .map(|page| page.id.clone())
        .collect::<Vec<_>>();
    let hierarchy = hierarchy::plan(
        linked_modules,
        existing_component_pages,
        &existing_page_ids,
        default_page,
        document.pages.len(),
    )?;
    materialize_hierarchy(&mut document, netlist, &hierarchy)?;

    let retained_power_symbols = if complete {
        power_symbol_locations(&document)?
    } else {
        BTreeSet::new()
    };
    for slot in &remove_slots {
        remove_component_slot(&mut document, slot);
    }
    for location in &remove_locations {
        remove_symbol_location(&mut document, location);
    }
    let placement_page = placement_page_id
        .map(|page_id| {
            document
                .pages
                .iter()
                .position(|page| page.id == page_id)
                .with_context(|| format!("placement page '{page_id}' is absent"))
        })
        .transpose()?;
    let net_symbol_specs = net_symbols::specs(netlist)?;
    for slot in &project_slots {
        let selected = selected_existing.get(slot);
        let page_index = if let Some(selected) = selected {
            selected.page_index
        } else if let Some(placement_page) = placement_page {
            placement_page
        } else {
            hierarchy.page_for_new_component(slot.component_path())?
        };
        project_component_slot(
            &mut document,
            netlist,
            &instances,
            slot,
            selected,
            page_index,
            &net_symbol_specs,
        )?;
    }
    if complete {
        retain_expected_and_power_symbols(&mut document, &expected_slots, &retained_power_symbols);
    }

    let mut placed = placed_symbols_from_document(&document, &expected_slots)?;
    pack_generated_symbols(
        &mut document,
        netlist,
        &mut placed,
        &relocatable_slots,
        &net_symbol_specs,
        preserved_page_count,
    )?;

    add_hierarchy_connectivity(
        &mut document,
        netlist,
        &placed,
        &net_symbol_specs,
        &hierarchy,
    )?;

    // A projected component's expected connectivity is already known from the
    // netlist. Attach its pins now, after placement, instead of rediscovering
    // their disconnections as a second set of schematic issues.
    let all_nets = named_connected_nets(netlist)
        .map(|net| net.name.clone())
        .collect::<BTreeSet<_>>();
    let projected_nets = connectivity_targets(netlist, &placed, &all_nets)?
        .into_iter()
        .filter(|(_, targets)| {
            targets
                .iter()
                .any(|target| relocatable_slots.contains(&target.slot))
        })
        .map(|(net_name, _)| net_name)
        .collect::<BTreeSet<_>>();
    if !projected_nets.is_empty() {
        add_connectivity_drivers(
            &mut document,
            netlist,
            &placed,
            &net_symbol_specs,
            default_page,
            &projected_nets,
        )?;
    }

    let current = crate::analysis::inspect_schematic(&document, netlist)?;
    let repair_keys = if complete {
        current
            .issues
            .iter()
            .map(|issue| issue.key.clone())
            .collect::<BTreeSet<_>>()
    } else {
        let before_keys = inspection_before
            .into_iter()
            .flat_map(|inspection| &inspection.issues)
            .map(|issue| issue.key.clone())
            .collect::<BTreeSet<_>>();
        // Any selected mutation (projection or removal) shifts issue keys, so
        // connectivity issues that appear only after the mutation belong to
        // this repair's scope.
        let document_mutated =
            !project_slots.is_empty() || !remove_slots.is_empty() || !remove_locations.is_empty();
        current
            .issues
            .iter()
            .filter(|issue| {
                selected_connectivity.contains(&issue.key)
                    || (document_mutated
                        && !before_keys.contains(&issue.key)
                        && is_connectivity_issue(&issue.issue))
            })
            .map(|issue| issue.key.clone())
            .collect::<BTreeSet<_>>()
    };
    if creating || !repair_keys.is_empty() {
        let intent = plan_connectivity_repair_core(
            &document,
            netlist,
            &current,
            &repair_keys,
            &BTreeSet::new(),
        )?;
        let mut reconnect_nets = intent.reconnect_nets.clone();
        if creating {
            reconnect_nets.extend(named_connected_nets(netlist).map(|net| net.name.clone()));
        }
        apply_connectivity_repair(
            &mut document,
            netlist,
            &mut placed,
            &net_symbol_specs,
            default_page,
            &intent,
            &reconnect_nets,
        )?;
    }

    // Library cleanup is a whole-document concern; a scoped repair must not
    // touch pages outside its selection.
    if complete {
        prune_unused_symbol_definitions(&mut document);
    }
    Ok(document)
}

fn is_connectivity_issue(issue: &SchematicIssue) -> bool {
    matches!(
        issue,
        SchematicIssue::DisconnectedNet { .. }
            | SchematicIssue::MissingPort { .. }
            | SchematicIssue::UnexpectedNet { .. }
            | SchematicIssue::UnexpectedConnection { .. }
            | SchematicIssue::Shorted { .. }
    )
}

struct RepairTargets {
    project_slots: BTreeSet<SymbolSlotKey>,
    remove_slots: BTreeSet<SymbolSlotKey>,
    remove_locations: BTreeSet<SymbolLocation>,
    connectivity: BTreeSet<SchematicIssueKey>,
}

fn repair_targets(
    issue_selection: Option<&BTreeSet<SchematicIssueKey>>,
    inspection: Option<&ConnectivityInspection>,
    expected_slots: &BTreeSet<SymbolSlotKey>,
) -> Result<RepairTargets> {
    let mut project_slots = if issue_selection.is_none() {
        expected_slots.clone()
    } else {
        BTreeSet::new()
    };
    let mut remove_slots = BTreeSet::new();
    let mut remove_locations = BTreeSet::new();
    let mut selected_connectivity = BTreeSet::new();
    if let Some(inspection) = inspection {
        let selected = match issue_selection {
            Some(keys) => keys
                .iter()
                .map(|key| {
                    inspection
                        .issues
                        .iter()
                        .find(|issue| &issue.key == key)
                        .with_context(|| format!("schematic issue {key:?} is not present"))
                })
                .collect::<Result<Vec<_>>>()?,
            None => inspection.issues.iter().collect(),
        };
        for context in selected {
            match &context.issue {
                SchematicIssue::MissingSymbol { slot }
                | SchematicIssue::DuplicateSymbol { slot, .. }
                | SchematicIssue::MismatchedSymbolId { slot, .. } => {
                    project_slots.insert(slot.clone());
                }
                SchematicIssue::UnexpectedSymbol { slot, .. } => {
                    remove_slots.insert(slot.clone());
                }
                SchematicIssue::UnboundSymbol { location } => {
                    remove_locations.insert(location.clone());
                }
                SchematicIssue::DisconnectedNet { .. }
                | SchematicIssue::MissingPort { .. }
                | SchematicIssue::UnexpectedNet { .. }
                | SchematicIssue::UnexpectedConnection { .. }
                | SchematicIssue::Shorted { .. } => {
                    selected_connectivity.insert(context.key.clone());
                }
            }
        }
    } else if issue_selection.is_some() {
        bail!("repairing selected issues requires a valid inspection");
    }
    Ok(RepairTargets {
        project_slots,
        remove_slots,
        remove_locations,
        connectivity: selected_connectivity,
    })
}

fn project_component_slot(
    document: &mut SchDocument,
    netlist: &Schematic,
    instances: &BTreeMap<String, &Instance>,
    slot: &SymbolSlotKey,
    selected: Option<&ExistingSymbol>,
    page_index: usize,
    net_symbol_specs: &BTreeMap<String, net_symbols::NetSymbolSpec>,
) -> Result<()> {
    let instance = instances.get(slot.component_path()).with_context(|| {
        format!(
            "component '{}' is absent from the netlist",
            slot.component_path()
        )
    })?;
    let definition = component_slots::component_symbol_definition(netlist, instance)?
        .with_context(|| {
            format!(
                "component '{}' has no KiCad symbol definition",
                slot.component_path()
            )
        })?;

    if document.pages.iter().any(|page| {
        page.items.iter().any(|item| {
            item.id() == Some(slot.symbol_id().as_str())
                && !matches!(item, SchItem::Symbol(symbol) if symbol.field_value("Path") == Some(slot.component_path()) && symbol.unit == slot.unit())
        })
    }) {
        bail!(
            "managed symbol UUID '{}' is already used by another schematic item",
            slot.symbol_id()
        );
    }

    let previous = selected.map(|selected| &selected.symbol);
    let at = previous.map(|symbol| symbol.at).unwrap_or_default();
    let rotation = match previous {
        Some(symbol) => symbol.rotation,
        None => initial_component_rotation(netlist, slot, &definition, net_symbol_specs)?,
    };
    let mirror = previous.and_then(|symbol| symbol.mirror);
    let symbol =
        build_component_symbol(instance, slot, &definition, at, rotation, mirror, previous)?;
    document.pages[page_index]
        .library
        .definitions
        .insert(definition.lib_id.clone(), definition);
    if let Some(selected) = selected {
        let selected_page_id = document.pages[selected.page_index].id.clone();
        let mut replacement = Some(symbol);
        for page in &mut document.pages {
            page.items.retain_mut(|item| {
                let matches_slot = matches!(
                    item,
                    SchItem::Symbol(symbol)
                        if symbol.field_value("Path") == Some(slot.component_path())
                            && symbol.unit == slot.unit()
                );
                if !matches_slot {
                    return true;
                }
                if replacement.is_some()
                    && page.id == selected_page_id
                    && item.id() == Some(selected.symbol.id.as_str())
                {
                    *item = SchItem::Symbol(replacement.take().expect("checked replacement"));
                    true
                } else {
                    false
                }
            });
        }
        if replacement.is_some() {
            bail!("selected managed symbol '{}' is absent", selected.symbol.id);
        }
    } else {
        document.pages[page_index]
            .items
            .push(SchItem::Symbol(symbol));
    }
    Ok(())
}

fn initial_component_rotation(
    netlist: &Schematic,
    slot: &SymbolSlotKey,
    definition: &SymbolDefinition,
    net_symbol_specs: &BTreeMap<String, net_symbols::NetSymbolSpec>,
) -> Result<Rotation> {
    let unplaced = Symbol {
        id: String::new(),
        lib_id: definition.lib_id.clone(),
        unit: slot.unit(),
        body_style: 1,
        at: Point::default(),
        rotation: Rotation::default(),
        mirror: None,
        dnp: false,
        in_bom: true,
        on_board: true,
        in_pos_files: true,
        fields_autoplaced: false,
        fields: BTreeMap::new(),
        pins: Vec::new(),
        unsupported: Vec::new(),
    };
    let pins = symbol::ParsedSymbolDefinition::parse(definition)?.placed_pins(&unplaced)?;
    // Orientation inference is useful for simple passives, but rotating a
    // larger authored symbol to favor its generated net symbols makes the
    // whole schematic harder to scan.
    if pins.len() != 2 {
        return Ok(Rotation::default());
    }
    let mut constraints = Vec::new();
    for net in named_connected_nets(netlist) {
        let Some(spec) = net_symbol_specs.get(&net.name) else {
            continue;
        };
        for port in &net.ports {
            let Some((component_ref, pin_name)) = netlist.component_ref_and_pin_for_port(port)
            else {
                continue;
            };
            if crate::canonical_component_path(&component_ref.instance_path).as_deref()
                != Some(slot.component_path())
            {
                continue;
            }
            let pin_numbers = component_slots::port_pad_numbers(netlist, port);
            let mut matches = pins.iter().filter(|pin| {
                !pin.hidden
                    && ((!pin_name.is_empty() && !pin.name.is_empty() && pin.name == pin_name)
                        || !pin.numbers.is_disjoint(&pin_numbers))
            });
            if let Some(pin) = matches.next()
                && matches.next().is_none()
            {
                constraints.push((pin.outward_spin, spec.pin_outward_spin));
            }
        }
    }

    Ok(preferred_two_pin_rotation(&constraints))
}

fn preferred_two_pin_rotation(constraints: &[(LabelSpin, LabelSpin)]) -> Rotation {
    let rotations = [
        Rotation::Deg0,
        Rotation::Deg90,
        Rotation::Deg180,
        Rotation::Deg270,
    ];

    // Two same-facing net symbols cannot both sit directly beyond opposite
    // component pins. Keep the component's terminal axis horizontal so the
    // two drivers can use parallel, easy-to-scan escape routes.
    if constraints.len() == 2
        && constraints[0].1 == constraints[1].1
        && let Some(rotation) = rotations.into_iter().find(|rotation| {
            constraints
                .iter()
                .all(|(pin_spin, _)| !rotate_spin(*pin_spin, *rotation).is_vertical())
        })
    {
        return rotation;
    }

    // Opposite outward directions put the component and net-symbol bodies on
    // opposite sides of their shared connection point.
    let scores = rotations.map(|rotation| {
        let score = constraints
            .iter()
            .filter(|(pin_spin, net_spin)| {
                rotate_spin(*pin_spin, rotation) == opposite_spin(*net_spin)
            })
            .count();
        (rotation, score)
    });

    let best_score = scores
        .iter()
        .map(|(_, score)| *score)
        .max()
        .expect("four candidate rotations");
    let mut best = scores
        .into_iter()
        .filter(|(_, score)| *score == best_score)
        .map(|(rotation, _)| rotation);
    let rotation = best.next().expect("at least one best rotation");
    // A non-unique optimum is less informative than the stable default.
    if best.next().is_none() {
        rotation
    } else {
        Rotation::default()
    }
}

fn rotate_spin(mut spin: LabelSpin, rotation: Rotation) -> LabelSpin {
    for _ in 0..rotation.degrees() / 90 {
        spin = match spin {
            LabelSpin::Left => LabelSpin::Bottom,
            LabelSpin::Up => LabelSpin::Left,
            LabelSpin::Right => LabelSpin::Up,
            LabelSpin::Bottom => LabelSpin::Right,
        };
    }
    spin
}

fn opposite_spin(spin: LabelSpin) -> LabelSpin {
    match spin {
        LabelSpin::Left => LabelSpin::Right,
        LabelSpin::Up => LabelSpin::Bottom,
        LabelSpin::Right => LabelSpin::Left,
        LabelSpin::Bottom => LabelSpin::Up,
    }
}

fn remove_component_slot(document: &mut SchDocument, slot: &SymbolSlotKey) {
    for page in &mut document.pages {
        page.items.retain(|item| {
            !matches!(
                item,
                SchItem::Symbol(symbol)
                    if symbol.field_value("Path") == Some(slot.component_path())
                        && symbol.unit == slot.unit()
            )
        });
    }
}

fn remove_symbol_location(document: &mut SchDocument, location: &SymbolLocation) {
    if let Some(page) = document
        .pages
        .iter_mut()
        .find(|page| page.id == location.page_id)
    {
        page.items.retain(
            |item| !matches!(item, SchItem::Symbol(symbol) if symbol.id == location.symbol_id),
        );
    }
}

fn placed_symbols_from_document(
    document: &SchDocument,
    expected_slots: &BTreeSet<SymbolSlotKey>,
) -> Result<BTreeMap<SymbolSlotKey, PlacedSymbol>> {
    let locations = existing_slot_locations(document);
    let mut placed = BTreeMap::new();
    for slot in expected_slots {
        let Some(candidates) = locations.get(slot) else {
            continue;
        };
        let [candidate] = candidates.as_slice() else {
            continue;
        };
        let definition = document.pages[candidate.page_index]
            .library
            .definitions
            .get(&candidate.symbol.lib_id)
            .with_context(|| {
                format!(
                    "managed symbol {} has no cached definition {}",
                    candidate.symbol.id, candidate.symbol.lib_id
                )
            })?
            .clone();
        placed.insert(
            slot.clone(),
            PlacedSymbol {
                page_index: candidate.page_index,
                symbol: candidate.symbol.clone(),
                definition,
            },
        );
    }
    Ok(placed)
}

/// Component symbols are projected from Zener. Explicit KiCad power symbols
/// are semantic net-name drivers, so they remain available for connectivity
/// analysis and minimally destructive repair.
fn power_symbol_locations(document: &SchDocument) -> Result<BTreeSet<SymbolLocation>> {
    let mut locations = BTreeSet::new();
    for page in &document.pages {
        for item in &page.items {
            let SchItem::Symbol(symbol) = item else {
                continue;
            };
            let Some(definition) = page.library.definitions.get(&symbol.lib_id) else {
                continue;
            };
            if symbol::ParsedSymbolDefinition::parse(definition)?
                .power_scope()
                .is_some()
            {
                locations.insert(SymbolLocation {
                    page_id: page.id.clone(),
                    symbol_id: symbol.id.clone(),
                });
            }
        }
    }
    Ok(locations)
}

fn retain_expected_and_power_symbols(
    document: &mut SchDocument,
    expected_slots: &BTreeSet<SymbolSlotKey>,
    retained_power_symbols: &BTreeSet<SymbolLocation>,
) {
    for page in &mut document.pages {
        let page_id = page.id.clone();
        page.items.retain(|item| {
            let SchItem::Symbol(symbol) = item else {
                return true;
            };
            if retained_power_symbols.contains(&SymbolLocation {
                page_id: page_id.clone(),
                symbol_id: symbol.id.clone(),
            }) {
                return true;
            }
            let Some(slot) = symbol
                .field_value("Path")
                .and_then(|path| SymbolSlotKey::new(path, symbol.unit))
            else {
                return false;
            };
            expected_slots.contains(&slot) && symbol.id == slot.symbol_id()
        });
    }
}

#[derive(Clone)]
struct ExistingSymbol {
    page_index: usize,
    symbol: Symbol,
}

fn existing_slot_locations(document: &SchDocument) -> BTreeMap<SymbolSlotKey, Vec<ExistingSymbol>> {
    let mut locations = BTreeMap::<SymbolSlotKey, Vec<ExistingSymbol>>::new();
    for (page_index, page) in document.pages.iter().enumerate() {
        for symbol in page.items.iter().filter_map(|item| match item {
            SchItem::Symbol(symbol) => Some(symbol),
            _ => None,
        }) {
            let Some(path) = symbol.field_value("Path") else {
                continue;
            };
            let Some(slot) = SymbolSlotKey::new(path, symbol.unit) else {
                continue;
            };
            locations.entry(slot).or_default().push(ExistingSymbol {
                page_index,
                symbol: symbol.clone(),
            });
        }
    }
    locations
}

fn select_existing_symbol(
    slot: &SymbolSlotKey,
    candidates: Option<&Vec<ExistingSymbol>>,
) -> Option<ExistingSymbol> {
    let candidates = candidates?;
    candidates
        .iter()
        .find(|candidate| candidate.symbol.id == slot.symbol_id())
        .or_else(|| candidates.first())
        .cloned()
}

fn linked_modules(netlist: &Schematic) -> Result<Vec<hierarchy::LinkedModule>> {
    let component_paths = netlist
        .instances
        .iter()
        .filter(|(_, instance)| instance.kind == InstanceKind::Component)
        .filter_map(|(instance_ref, _)| {
            crate::canonical_component_path(&instance_ref.instance_path)
        })
        .collect::<Vec<_>>();
    let mut modules = Vec::new();
    for (instance_ref, instance) in &netlist.instances {
        if instance.kind != InstanceKind::Module
            || !instance.attributes.contains_key(ATTR_SCHEMATIC_PATH)
            || netlist.root_ref.as_ref() == Some(instance_ref)
        {
            continue;
        }
        let path = crate::canonical_component_path(&instance_ref.instance_path)
            .context("linked module instance has no canonical path")?;
        if !component_paths
            .iter()
            .any(|component_path| hierarchy::is_descendant(component_path, &path))
        {
            continue;
        }
        if path.contains(['/', '\\']) || path.chars().any(char::is_control) {
            bail!("linked module path '{path}' cannot be used as a KiCad schematic filename");
        }
        modules.push(hierarchy::LinkedModule {
            path,
            instance_ref: instance_ref.clone(),
        });
    }
    Ok(modules)
}

fn materialize_hierarchy(
    document: &mut SchDocument,
    netlist: &Schematic,
    plan: &hierarchy::HierarchyPlan,
) -> Result<()> {
    for sheet_plan in &plan.sheets {
        if document.pages.len() != sheet_plan.child_page {
            bail!("hierarchy planner produced a non-contiguous child page index");
        }
        let ports = module_ports(netlist, &sheet_plan.instance_ref)?;
        let height = SHEET_MIN_HEIGHT_MM.max((ports.len() as f64 + 2.0) * SHEET_PIN_SPACING_MM);
        let size = Point::new(SHEET_MIN_WIDTH_MM, height);
        let at = place_sheet(document, sheet_plan.parent_page, size)?;
        let name = sheet_plan
            .module_path
            .rsplit('.')
            .next()
            .expect("canonical module path is non-empty");
        let pins = ports
            .iter()
            .enumerate()
            .map(|(index, (_, port_name))| SheetPin {
                id: deterministic_uuid(format!(
                    "zener:module-sheet-pin:{}:{port_name}",
                    sheet_plan.module_path
                )),
                name: port_name.clone(),
                at: Point::new(at.x, at.y + (index as f64 + 2.0) * SHEET_PIN_SPACING_MM),
                rotation: Rotation::Deg180,
                shape: LabelShape::Bidirectional,
                unsupported: Vec::new(),
            })
            .collect();
        let mut name_field = SymbolField::new("Sheetname", name, at);
        name_field.at.y -= 0.7112;
        // The sheet reference is parent-relative (KiCad resolves Sheetfile
        // against the referencing page's directory), so the new page lives in
        // the parent page's directory and the reference stays the bare name.
        let mut file_field = SymbolField::new("Sheetfile", &sheet_plan.file_name, at);
        file_field.at.y += height + 0.7112;
        let child_file_name = match document.pages[sheet_plan.parent_page]
            .file_name
            .as_deref()
            .and_then(|file_name| std::path::Path::new(file_name).parent())
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            Some(parent) => format!(
                "{}/{}",
                parent.to_string_lossy().replace('\\', "/"),
                sheet_plan.file_name
            ),
            None => sheet_plan.file_name.clone(),
        };
        let sheet = Sheet {
            id: hierarchy::sheet_id(&sheet_plan.module_path),
            at: Some(at),
            size: Some(size),
            name: Some(name_field),
            file: file_field,
            pins,
            unsupported: generated_sheet_style(),
        };
        document.pages[sheet_plan.parent_page]
            .items
            .push(SchItem::Sheet(Box::new(sheet)));

        let mut child = SchPage::new(hierarchy::page_id(&sheet_plan.module_path));
        child.file_name = Some(child_file_name);
        child.paper = document.pages[sheet_plan.parent_page].paper.clone();
        document.pages.push(child);
    }
    Ok(())
}

fn generated_sheet_style() -> Vec<Sexpr> {
    vec![
        Sexpr::list(vec![Sexpr::symbol("exclude_from_sim"), Sexpr::symbol("no")]),
        Sexpr::list(vec![Sexpr::symbol("in_bom"), Sexpr::symbol("yes")]),
        Sexpr::list(vec![Sexpr::symbol("on_board"), Sexpr::symbol("yes")]),
        Sexpr::list(vec![Sexpr::symbol("dnp"), Sexpr::symbol("no")]),
        Sexpr::list(vec![
            Sexpr::symbol("stroke"),
            Sexpr::list(vec![Sexpr::symbol("width"), Sexpr::float(0.1524)]),
            Sexpr::list(vec![Sexpr::symbol("type"), Sexpr::symbol("solid")]),
        ]),
        Sexpr::list(vec![
            Sexpr::symbol("fill"),
            Sexpr::list(vec![
                Sexpr::symbol("color"),
                Sexpr::int(0),
                Sexpr::int(0),
                Sexpr::int(0),
                Sexpr::int(0),
            ]),
        ]),
    ]
}

fn module_ports(
    netlist: &Schematic,
    module_ref: &pcb_sch::InstanceRef,
) -> Result<Vec<(String, String)>> {
    Ok(root_interface::ports_by_net(netlist, module_ref)?
        .into_iter()
        .flat_map(|(net_name, port_names)| {
            port_names
                .into_iter()
                .map(move |port_name| (net_name.clone(), port_name))
        })
        .collect())
}

fn place_sheet(document: &SchDocument, page_index: usize, size: Point) -> Result<Point> {
    let page = document
        .pages
        .get(page_index)
        .context("planned sheet parent page is absent")?;
    let mut packer = GridPacker::for_page(&page.paper)?;
    occupy_page_items(&mut packer, page)?;
    let relative = GridRect::from_bounds(
        field_autoplace::Bounds::from_points([Point::default(), size])
            .expect("sheet size defines bounds"),
    );
    Ok(packer.place(relative).to_point())
}

fn pack_generated_symbols(
    document: &mut SchDocument,
    netlist: &Schematic,
    placed: &mut BTreeMap<SymbolSlotKey, PlacedSymbol>,
    relocatable_slots: &BTreeSet<SymbolSlotKey>,
    net_symbol_specs: &BTreeMap<String, net_symbols::NetSymbolSpec>,
    preserved_page_count: usize,
) -> Result<()> {
    if relocatable_slots.is_empty() {
        return Ok(());
    }
    let all_nets = named_connected_nets(netlist)
        .map(|net| net.name.clone())
        .collect();
    let targets = connectivity_targets(netlist, placed, &all_nets)?;
    let net_symbol_runs =
        plan_projected_net_symbol_runs(&targets, relocatable_slots, net_symbol_specs, placed)?;

    let bounds_by_slot = relocatable_slots
        .iter()
        .map(|slot| {
            Ok((
                slot.clone(),
                GridRect::from_bounds(component_envelope(
                    &placed[slot],
                    &targets,
                    net_symbol_specs,
                    &net_symbol_runs,
                )?),
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let blocks = placement_blocks(
        netlist,
        placed,
        relocatable_slots,
        &targets,
        &bounds_by_slot,
    )?;

    for page_index in 0..document.pages.len() {
        let relocatable = relocatable_slots
            .iter()
            .filter(|slot| {
                placed
                    .get(*slot)
                    .is_some_and(|item| item.page_index == page_index)
            })
            .cloned()
            .collect::<BTreeSet<_>>();
        if relocatable.is_empty() {
            continue;
        }
        let page_blocks = blocks
            .iter()
            .filter(|block| block.page_index == page_index)
            .collect::<Vec<_>>();
        // Fit and placement share one occupancy: the title block, hierarchy
        // sheets, and preserved items with known bounds. If no grid fits,
        // placement spills outside the page rather than covering that work.
        let relocatable_ids = relocatable
            .iter()
            .map(|slot| slot.symbol_id())
            .collect::<BTreeSet<_>>();
        let (mut packer, grid_bounds, block_offsets) = if page_index >= preserved_page_count {
            arrange_new_page_blocks(
                &mut document.pages[page_index],
                &page_blocks,
                &relocatable_ids,
            )?
        } else {
            arrange_existing_page_blocks(
                &document.pages[page_index],
                &page_blocks,
                &relocatable_ids,
            )?
        };
        let grid_origin = packer.place_anchored(grid_bounds);
        for (block, block_offset) in page_blocks.into_iter().zip(block_offsets) {
            let block_origin = grid_origin.translated(block_offset);
            for member in &block.members {
                let anchor = block_origin.translated(member.offset);
                move_placed_symbol(document, placed, &member.slot, anchor.to_point())?;
            }
        }
    }

    add_capacitor_bank_wires(document, netlist, placed, &blocks)?;
    Ok(())
}

fn arrange_new_page_blocks(
    page: &mut SchPage,
    blocks: &[&PlacementBlock],
    excluded_symbol_ids: &BTreeSet<String>,
) -> Result<(GridPacker, GridRect, Vec<GridPoint>)> {
    for paper in placement_paper_candidates(&page.paper) {
        let packer = occupied_page_packer(page, &paper, excluded_symbol_ids)?;
        if let Some((bounds, offsets)) = arrange_placement_blocks(blocks, &packer)
            && packer.can_place_without_overlap(bounds)
        {
            page.paper = paper;
            return Ok((packer, bounds, offsets));
        }
    }
    arrange_existing_page_blocks(page, blocks, excluded_symbol_ids)
}

fn arrange_existing_page_blocks(
    page: &SchPage,
    blocks: &[&PlacementBlock],
    excluded_symbol_ids: &BTreeSet<String>,
) -> Result<(GridPacker, GridRect, Vec<GridPoint>)> {
    let packer = occupied_page_packer(page, &page.paper, excluded_symbol_ids)?;
    let (bounds, offsets) =
        arrange_placement_blocks(blocks, &packer).expect("a generated cohort has blocks");
    Ok((packer, bounds, offsets))
}

/// A packer for `paper` with the title block and known item bounds occupied,
/// except excluded symbols: fit checks and final placement share this occupancy.
fn occupied_page_packer(
    page: &SchPage,
    paper: &Paper,
    excluded_symbol_ids: &BTreeSet<String>,
) -> Result<GridPacker> {
    let mut packer = GridPacker::for_page(paper)?;
    occupy_page_items_except(&mut packer, page, excluded_symbol_ids)?;
    Ok(packer)
}

fn placement_paper_candidates(current: &Paper) -> Vec<Paper> {
    let mut candidates = vec![current.clone()];
    let Paper::Named { name, portrait } = current else {
        return candidates;
    };
    let larger: &[&str] = match name.as_str() {
        "A5" => &["A4", "A3", "A2", "A1", "A0"],
        "A4" => &["A3", "A2", "A1", "A0"],
        "A3" => &["A2", "A1", "A0"],
        "A2" => &["A1", "A0"],
        "A1" => &["A0"],
        "A" | "USLetter" => &["B", "C", "D", "E"],
        "B" | "USLedger" => &["C", "D", "E"],
        "C" => &["D", "E"],
        "D" => &["E"],
        _ => return candidates,
    };
    candidates.extend(larger.iter().map(|name| Paper::Named {
        name: (*name).to_string(),
        portrait: *portrait,
    }));
    candidates
}

#[derive(Debug)]
struct PlacementMember {
    slot: SymbolSlotKey,
    offset: GridPoint,
}

#[derive(Debug)]
struct CapacitorBank {
    nets: [String; 2],
}

#[derive(Debug)]
struct PlacementBlock {
    key: String,
    page_index: usize,
    members: Vec<PlacementMember>,
    bounds: GridRect,
    capacitor_bank: Option<CapacitorBank>,
}

fn arrange_placement_blocks(
    blocks: &[&PlacementBlock],
    packer: &GridPacker,
) -> Option<(GridRect, Vec<GridPoint>)> {
    // Pick one page-shaped shelf grid for the whole generated cohort, among
    // the column counts, preferring those the page still has room for.
    // Placing the grid as a unit avoids the radial pattern produced by
    // repeatedly extending a centered cluster one block at a time.
    let usable = packer.usable_bounds();
    (1..=blocks.len())
        .map(|columns| {
            let (bounds, offsets) = placement_grid_with_columns(blocks, columns);
            (columns, bounds, offsets)
        })
        .min_by_key(|(columns, bounds, _)| {
            let aspect_error = (i64::from(bounds.width()) * i64::from(usable.height())
                - i64::from(bounds.height()) * i64::from(usable.width()))
            .abs();
            (
                !packer.can_place_without_overlap(*bounds),
                aspect_error,
                bounds.area(),
                *columns,
            )
        })
        .map(|(_, bounds, offsets)| (bounds, offsets))
}

fn placement_grid_with_columns(
    blocks: &[&PlacementBlock],
    columns: usize,
) -> (GridRect, Vec<GridPoint>) {
    debug_assert!(!blocks.is_empty());
    debug_assert!((1..=blocks.len()).contains(&columns));
    let rows = blocks.len().div_ceil(columns);
    let mut row_min = vec![i32::MAX; rows];
    let mut row_max = vec![i32::MIN; rows];
    for (index, block) in blocks.iter().enumerate() {
        let row = index / columns;
        row_min[row] = row_min[row].min(block.bounds.min_y);
        row_max[row] = row_max[row].max(block.bounds.max_y);
    }

    let mut row_origins = vec![0; rows];
    for row in 1..rows {
        row_origins[row] =
            row_origins[row - 1] + row_max[row - 1] - row_min[row] + PLACEMENT_BLOCK_GAP_CELLS;
    }

    let mut offsets: Vec<GridPoint> = Vec::with_capacity(blocks.len());
    for (index, block) in blocks.iter().enumerate() {
        let row = index / columns;
        let x = if index % columns == 0 {
            0
        } else {
            let previous = blocks[index - 1];
            offsets[index - 1].x + previous.bounds.max_x - block.bounds.min_x
                + PLACEMENT_BLOCK_GAP_CELLS
        };
        offsets.push(GridPoint {
            x,
            y: row_origins[row],
        });
    }
    let bounds = blocks
        .iter()
        .zip(&offsets)
        .map(|(block, offset)| block.bounds.translated(*offset))
        .reduce(GridRect::union)
        .expect("a placement grid has blocks");
    (bounds, offsets)
}

fn placement_blocks(
    netlist: &Schematic,
    placed: &BTreeMap<SymbolSlotKey, PlacedSymbol>,
    relocatable_slots: &BTreeSet<SymbolSlotKey>,
    targets: &BTreeMap<String, Vec<PinTarget>>,
    bounds_by_slot: &BTreeMap<SymbolSlotKey, GridRect>,
) -> Result<Vec<PlacementBlock>> {
    let instances = component_slots::component_instances(netlist)?;
    let slots_by_component = relocatable_slots.iter().cloned().fold(
        BTreeMap::<String, Vec<SymbolSlotKey>>::new(),
        |mut components, slot| {
            components
                .entry(slot.component_path().to_string())
                .or_default()
                .push(slot);
            components
        },
    );

    // Recognize topology, not names: two or more simple capacitors spanning
    // the same pair of nets form one visual bank. Requiring every symbol slot
    // of a component to be new prevents a repair from reorganizing user work.
    let mut bank_candidates = BTreeMap::<(usize, String, String), Vec<SymbolSlotKey>>::new();
    for (component_path, slots) in &slots_by_component {
        let Some(instance) = instances.get(component_path) else {
            continue;
        };
        if instance.component_type().as_deref() != Some("capacitor") || slots.len() != 1 {
            continue;
        }
        let all_slots_are_relocatable = placed
            .keys()
            .filter(|slot| slot.component_path() == component_path)
            .all(|slot| relocatable_slots.contains(slot));
        if !all_slots_are_relocatable {
            continue;
        }
        let connected_nets = targets
            .iter()
            .filter(|(_, net_targets)| {
                net_targets
                    .iter()
                    .any(|target| !target.hidden && target.slot.component_path() == component_path)
            })
            .map(|(net_name, _)| net_name.clone())
            .collect::<BTreeSet<_>>();
        let mut connected_nets = connected_nets.into_iter();
        let (Some(first), Some(second), None) = (
            connected_nets.next(),
            connected_nets.next(),
            connected_nets.next(),
        ) else {
            continue;
        };
        let page_index = placed[&slots[0]].page_index;
        bank_candidates
            .entry((page_index, first, second))
            .or_default()
            .push(slots[0].clone());
    }

    let mut claimed = BTreeSet::new();
    let mut blocks = Vec::new();
    for ((page_index, first, second), mut slots) in bank_candidates {
        if slots.len() < 2 {
            continue;
        }
        slots.sort();
        claimed.extend(slots.iter().cloned());
        let key = format!("capacitor-bank:{page_index}:{first}:{second}");
        let mut block = placement_block(key, page_index, slots, bounds_by_slot, usize::MAX);
        // Shared rail buses sit just outside the component pins. The projected
        // net-symbol envelopes generally reserve more room, but keep the block
        // contract explicit for nets without symbols as well.
        block.bounds = block
            .bounds
            .expanded(CAPACITOR_BANK_BUS_OFFSET_CELLS as i32);
        block.capacitor_bank = Some(CapacitorBank {
            nets: [first, second],
        });
        blocks.push(block);
    }

    // The fallback groups all units of one component into a small regular
    // block. Most components have one unit, so these become singleton blocks.
    for (component_path, mut slots) in slots_by_component {
        slots.retain(|slot| !claimed.contains(slot));
        if slots.is_empty() {
            continue;
        }
        slots.sort();
        let page_index = placed[&slots[0]].page_index;
        let columns = (1..)
            .find(|columns| columns * columns >= slots.len())
            .expect("a finite component has a square grid");
        blocks.push(placement_block(
            format!("component:{component_path}"),
            page_index,
            slots,
            bounds_by_slot,
            columns,
        ));
    }

    blocks.sort_by(|left, right| {
        right
            .bounds
            .area()
            .cmp(&left.bounds.area())
            .then_with(|| left.key.cmp(&right.key))
    });
    Ok(blocks)
}

fn placement_block(
    key: String,
    page_index: usize,
    slots: Vec<SymbolSlotKey>,
    bounds_by_slot: &BTreeMap<SymbolSlotKey, GridRect>,
    max_columns: usize,
) -> PlacementBlock {
    let columns = max_columns.min(slots.len());
    let rows = slots.len().div_ceil(columns);
    let mut row_min = vec![i32::MAX; rows];
    let mut row_max = vec![i32::MIN; rows];
    for (index, slot) in slots.iter().enumerate() {
        let row = index / columns;
        row_min[row] = row_min[row].min(bounds_by_slot[slot].min_y);
        row_max[row] = row_max[row].max(bounds_by_slot[slot].max_y);
    }
    let mut row_origins = vec![0; rows];
    for row in 1..rows {
        row_origins[row] =
            row_origins[row - 1] + row_max[row - 1] - row_min[row] + PLACEMENT_BLOCK_GAP_CELLS;
    }

    let mut block_bounds = None;
    let mut members: Vec<PlacementMember> = Vec::with_capacity(slots.len());
    for (index, slot) in slots.into_iter().enumerate() {
        let bounds = bounds_by_slot[&slot];
        let row = index / columns;
        let x = if index % columns == 0 {
            0
        } else {
            let previous = &members[index - 1];
            previous.offset.x + bounds_by_slot[&previous.slot].max_x - bounds.min_x
                + PLACEMENT_BLOCK_GAP_CELLS
        };
        let offset = GridPoint {
            x,
            y: row_origins[row],
        };
        let member_bounds = bounds.translated(offset);
        block_bounds =
            Some(block_bounds.map_or(member_bounds, |block: GridRect| block.union(member_bounds)));
        members.push(PlacementMember { slot, offset });
    }
    PlacementBlock {
        key,
        page_index,
        members,
        bounds: block_bounds.expect("placement block has bounds"),
        capacitor_bank: None,
    }
}

fn add_capacitor_bank_wires(
    document: &mut SchDocument,
    netlist: &Schematic,
    placed: &BTreeMap<SymbolSlotKey, PlacedSymbol>,
    blocks: &[PlacementBlock],
) -> Result<()> {
    let bank_nets = blocks
        .iter()
        .filter_map(|block| block.capacitor_bank.as_ref())
        .flat_map(|bank| bank.nets.iter().cloned())
        .collect::<BTreeSet<_>>();
    if bank_nets.is_empty() {
        return Ok(());
    }
    let targets = connectivity_targets(netlist, placed, &bank_nets)?;
    for block in blocks {
        let Some(bank) = &block.capacitor_bank else {
            continue;
        };
        let member_slots = block
            .members
            .iter()
            .map(|member| member.slot.clone())
            .collect::<BTreeSet<_>>();
        let rail_targets = bank
            .nets
            .iter()
            .map(|net_name| {
                let selected = targets[net_name]
                    .iter()
                    .filter(|target| !target.hidden && member_slots.contains(&target.slot))
                    .collect::<Vec<_>>();
                let one_per_member = selected.len() == member_slots.len()
                    && member_slots.iter().all(|slot| {
                        selected
                            .iter()
                            .filter(|target| &target.slot == slot)
                            .count()
                            == 1
                    });
                one_per_member.then_some(selected)
            })
            .collect::<Option<Vec<_>>>();
        let Some(rail_targets) = rail_targets else {
            continue;
        };
        let sides = rail_targets
            .iter()
            .map(|targets| {
                let side = targets[0].spin;
                targets
                    .iter()
                    .all(|target| target.spin == side)
                    .then_some(side)
            })
            .collect::<Option<Vec<_>>>();
        let Some(sides) = sides else {
            continue;
        };
        // A horizontal capacitor row has safe, conventional shared buses only
        // when the two terminals point vertically in opposite directions.
        if !sides[0].is_vertical() || sides[1] != opposite_spin(sides[0]) {
            continue;
        }
        for ((net_name, targets), side) in bank.nets.iter().zip(rail_targets).zip(sides) {
            for (index, (a, b)) in capacitor_bank_wire_segments(&targets, side)
                .into_iter()
                .enumerate()
            {
                if points_coincide(a, b) {
                    continue;
                }
                let id = available_deterministic_id(
                    document,
                    &format!("zener:{}:{net_name}:wire:{index}", block.key),
                );
                document.pages[block.page_index]
                    .items
                    .push(SchItem::Wire(Wire {
                        id,
                        a,
                        b,
                        unsupported: Vec::new(),
                    }));
            }
        }
    }
    Ok(())
}

fn capacitor_bank_wire_segments(targets: &[&PinTarget], side: LabelSpin) -> Vec<(Point, Point)> {
    debug_assert!(side.is_vertical());
    let bus_y = if side == LabelSpin::Up {
        targets
            .iter()
            .map(|target| target.point.y)
            .min_by(f64::total_cmp)
            .expect("capacitor bank rail has targets")
            - CAPACITOR_BANK_BUS_OFFSET_CELLS * CONNECTION_GRID_MM
    } else {
        targets
            .iter()
            .map(|target| target.point.y)
            .max_by(f64::total_cmp)
            .expect("capacitor bank rail has targets")
            + CAPACITOR_BANK_BUS_OFFSET_CELLS * CONNECTION_GRID_MM
    };
    let mut bus_points = targets
        .iter()
        .map(|target| Point::new(target.point.x, bus_y))
        .collect::<Vec<_>>();
    bus_points.sort_by(|left, right| left.x.total_cmp(&right.x));
    targets
        .iter()
        .map(|target| (target.point, Point::new(target.point.x, bus_y)))
        // Split the bus at every branch. KiCad connectivity treats endpoints
        // as joins without needing generated junction markers.
        .chain(bus_points.windows(2).map(|points| (points[0], points[1])))
        .collect()
}

fn occupy_page_items(packer: &mut GridPacker, page: &SchPage) -> Result<()> {
    occupy_page_items_except(packer, page, &BTreeSet::new())
}

fn occupy_page_items_except(
    packer: &mut GridPacker,
    page: &SchPage,
    excluded_symbol_ids: &BTreeSet<String>,
) -> Result<()> {
    for item in &page.items {
        match item {
            SchItem::Symbol(symbol) if !excluded_symbol_ids.contains(&symbol.id) => {
                let Some(definition) = page.library.definitions.get(&symbol.lib_id) else {
                    continue;
                };
                if let Some(bounds) = field_autoplace::symbol_visual_bounds(symbol, definition)? {
                    let bounds = GridRect::from_bounds(bounds);
                    if symbol.field_value("Path").is_some() {
                        packer.occupy_anchored(bounds, symbol.at);
                    } else {
                        packer.occupy(bounds);
                    }
                }
            }
            SchItem::Sheet(sheet) => {
                if let Some((min, max)) = sheet.bounds() {
                    let bounds = field_autoplace::Bounds::from_points([min, max])
                        .expect("sheet bounds have two corners");
                    packer.occupy(GridRect::from_bounds(bounds));
                }
            }
            SchItem::Label(label) => {
                packer.occupy(GridRect::from_bounds(label_visual_bounds(label)))
            }
            SchItem::Wire(wire) => {
                let bounds = field_autoplace::Bounds::from_points([wire.a, wire.b])
                    .expect("wire has two endpoints");
                packer.occupy(GridRect::from_bounds(bounds));
            }
            SchItem::Junction(junction) => packer.occupy(point_rect(junction.at)),
            SchItem::NoConnect(no_connect) => packer.occupy(point_rect(no_connect.at)),
            SchItem::Unsupported(item) => {
                if let Some(bounds) = field_autoplace::page_graphic_bounds(item) {
                    packer.occupy(GridRect::from_bounds(bounds));
                }
            }
            SchItem::Symbol(_) => {}
        }
    }
    Ok(())
}

fn component_envelope(
    placed: &PlacedSymbol,
    targets: &BTreeMap<String, Vec<PinTarget>>,
    net_symbol_specs: &BTreeMap<String, net_symbols::NetSymbolSpec>,
    net_symbol_runs: &[NetSymbolRun],
) -> Result<field_autoplace::Bounds> {
    let mut bounds = field_autoplace::symbol_visual_bounds(&placed.symbol, &placed.definition)?
        .unwrap_or_else(|| {
            field_autoplace::Bounds::from_points([placed.symbol.at])
                .expect("one point defines bounds")
        });
    for (net_name, net_targets) in targets {
        let component_targets = net_targets
            .iter()
            .filter(|target| !target.hidden && target.slot.symbol_id() == placed.symbol.id)
            .collect::<Vec<_>>();
        for target in &component_targets {
            bounds.union(net_label_bounds(net_name, target));
        }

        for run in net_symbol_runs.iter().filter(|run| {
            run.net_name == *net_name
                && run.page_index == placed.page_index
                && run.slot.symbol_id() == placed.symbol.id
        }) {
            let spec = &net_symbol_specs[net_name];
            let placement_target = run.placement_target();
            let connection = net_symbol_connection_point(
                placed,
                &placement_target,
                spec.pin_outward_spin,
                run.stair_index,
            )?;
            let symbol = build_net_symbol(spec, net_name, String::new(), connection)?;
            if let Some(symbol_bounds) =
                field_autoplace::symbol_visual_bounds(&symbol, &spec.definition)?
            {
                bounds.union(symbol_bounds);
            }
            for target in &run.targets {
                let bend = net_symbol_wire_bend(target.point, connection, target.spin);
                bounds.union(
                    field_autoplace::Bounds::from_points([target.point, bend, connection])
                        .expect("net-symbol route has at least two points"),
                );
            }
        }
    }
    Ok(bounds.translated(-placed.symbol.at.x, -placed.symbol.at.y))
}

fn net_label_bounds(net_name: &str, target: &PinTarget) -> field_autoplace::Bounds {
    estimated_label_bounds(target.point, target.spin, estimated_label_width(net_name))
}

fn label_visual_bounds(label: &Label) -> field_autoplace::Bounds {
    let length = match label.kind {
        LabelKind::Local => estimated_label_width(&label.text),
        LabelKind::Global { .. } | LabelKind::Hierarchical { .. } | LabelKind::Directive { .. } => {
            estimated_shaped_label_width(&label.text)
        }
    };
    estimated_label_bounds(label.at, label.spin, length)
}

fn estimated_label_bounds(at: Point, spin: LabelSpin, length: f64) -> field_autoplace::Bounds {
    let text_height = crate::TextEffects::default().font_size.y.abs();
    let half_height = text_height * 0.5;
    // Extend `length` along the spin direction, +/- half a text height on the
    // perpendicular axis.
    let (dx, dy): (f64, f64) = match spin {
        LabelSpin::Left => (-1.0, 0.0),
        LabelSpin::Right => (1.0, 0.0),
        LabelSpin::Up => (0.0, -1.0),
        LabelSpin::Bottom => (0.0, 1.0),
    };
    let (px, py) = (dy.abs() * half_height, dx.abs() * half_height);
    field_autoplace::Bounds::from_points([
        Point::new(at.x - px, at.y - py),
        Point::new(at.x + dx * length + px, at.y + dy * length + py),
    ])
    .expect("two points define label bounds")
}

fn move_placed_symbol(
    document: &mut SchDocument,
    placed: &mut BTreeMap<SymbolSlotKey, PlacedSymbol>,
    slot: &SymbolSlotKey,
    new_at: Point,
) -> Result<()> {
    let item = placed
        .get_mut(slot)
        .with_context(|| format!("missing placed symbol for '{}'", slot.component_path()))?;
    let delta = Point::new(new_at.x - item.symbol.at.x, new_at.y - item.symbol.at.y);
    item.symbol.at = new_at;
    for field in item.symbol.fields.values_mut() {
        field.at = Point::new(field.at.x + delta.x, field.at.y + delta.y);
    }
    replace_placed_symbol(document, item)
}

fn replace_placed_symbol(document: &mut SchDocument, item: &PlacedSymbol) -> Result<()> {
    let symbol_id = item.symbol.id.clone();
    let symbol = item.symbol.clone();
    let mut found = None;
    for page_item in &mut document.pages[item.page_index].items {
        let SchItem::Symbol(candidate) = page_item else {
            continue;
        };
        if candidate.id != symbol_id {
            continue;
        }
        if found.is_some() {
            bail!("placed symbol UUID '{symbol_id}' is not unique on its page");
        }
        found = Some(candidate);
    }
    *found
        .with_context(|| format!("placed symbol UUID '{symbol_id}' is absent from its page"))? =
        symbol;
    Ok(())
}

fn build_component_symbol(
    instance: &Instance,
    slot: &SymbolSlotKey,
    definition: &SymbolDefinition,
    at: Point,
    rotation: Rotation,
    mirror: Option<crate::MirrorAxis>,
    previous: Option<&Symbol>,
) -> Result<Symbol> {
    let mut fields = previous
        .map(|symbol| symbol.fields.clone())
        .unwrap_or_default();
    for next in component_fields(instance, slot, &definition.lib_id, at)? {
        match fields.get_mut(&next.name) {
            Some(existing) => {
                existing.value = next.value;
            }
            None => {
                fields.insert(next.name.clone(), next);
            }
        }
    }

    let previous_pins = previous
        .filter(|symbol| symbol.id == slot.symbol_id())
        .map(|symbol| symbol.pins.as_slice())
        .unwrap_or_default();
    let mut symbol = Symbol {
        id: slot.symbol_id(),
        lib_id: definition.lib_id.clone(),
        unit: slot.unit(),
        body_style: previous.map(|symbol| symbol.body_style).unwrap_or(1),
        at,
        rotation,
        mirror,
        dnp: false,
        in_bom: true,
        on_board: true,
        in_pos_files: true,
        fields_autoplaced: previous
            .map(|symbol| symbol.fields_autoplaced)
            .unwrap_or(true),
        fields,
        pins: Vec::new(),
        unsupported: previous
            .filter(|symbol| symbol.id == slot.symbol_id())
            .map(|symbol| symbol.unsupported.clone())
            .unwrap_or_default(),
    };
    component_slots::sync_netlist_derived_symbol_properties(&mut symbol, instance);
    reconcile_pin_instances(&mut symbol, definition, previous_pins)?;
    if previous.is_none() {
        field_autoplace::apply_definition_field_styles(&mut symbol, definition)?;
        field_autoplace::autoplace_symbol_fields(&mut symbol, definition)?;
    }
    Ok(symbol)
}

fn reconcile_pin_instances(
    symbol: &mut Symbol,
    definition: &SymbolDefinition,
    previous: &[crate::PinInstance],
) -> Result<()> {
    let parsed = symbol::ParsedSymbolDefinition::parse(definition)?;
    let definition_pins = parsed.placed_pins(symbol)?;
    let mut pins_by_number = BTreeMap::<String, Vec<_>>::new();
    for pin in definition_pins {
        if !pin.number.is_empty() {
            pins_by_number
                .entry(pin.number.clone())
                .or_default()
                .push(pin);
        }
    }

    let mut previous_by_number = BTreeMap::new();
    for pin in previous {
        if previous_by_number
            .insert(pin.number.clone(), pin.clone())
            .is_some()
        {
            bail!(
                "symbol {} has multiple placed pin instances numbered {}",
                symbol.id,
                pin.number
            );
        }
    }

    symbol.pins = pins_by_number
        .into_iter()
        .map(|(number, definition_pins)| {
            let Some(mut pin) = previous_by_number.remove(&number) else {
                return crate::PinInstance {
                    id: deterministic_uuid(format!("{}:pin:{number}", symbol.id)),
                    number,
                    alternate: None,
                    unsupported: Vec::new(),
                };
            };
            pin.alternate = pin.alternate.filter(|alternate| {
                definition_pins.len() == 1 && definition_pins[0].supports_alternate(alternate)
            });
            pin
        })
        .collect();
    Ok(())
}

fn component_fields(
    instance: &Instance,
    slot: &SymbolSlotKey,
    lib_id: &str,
    at: Point,
) -> Result<Vec<SymbolField>> {
    let reference = instance
        .reference_designator
        .clone()
        .filter(|value| !value.trim().is_empty())
        .with_context(|| {
            format!(
                "component '{}' has no assigned reference designator",
                slot.component_path()
            )
        })?;
    let value = first_attribute(instance, &["Value", "value"])?
        .unwrap_or(lib_id)
        .to_string();
    let mut fields = vec![
        SymbolField::new("Reference", reference, at),
        SymbolField::new("Value", value, at),
        SymbolField::new("Path", slot.component_path(), at).with_hidden(true),
    ];
    if let Some(footprint) = first_attribute(instance, &["footprint"])? {
        fields.push(SymbolField::new("Footprint", footprint, at).with_hidden(true));
    }
    if let Some(description) = first_attribute(instance, &["Description", "description"])? {
        fields.push(SymbolField::new("Description", description, at).with_hidden(true));
    }
    Ok(fields)
}

fn first_attribute<'a>(instance: &'a Instance, keys: &[&str]) -> Result<Option<&'a str>> {
    for key in keys {
        if let Some(value) = component_slots::attribute_string(instance, key)?
            && !value.trim().is_empty()
        {
            return Ok(Some(value));
        }
    }
    Ok(None)
}

#[derive(Clone)]
struct PlacedSymbol {
    page_index: usize,
    symbol: Symbol,
    definition: SymbolDefinition,
}

#[derive(Clone)]
struct PinTarget {
    page_index: usize,
    slot: SymbolSlotKey,
    symbol_id: String,
    number: String,
    point: Point,
    spin: LabelSpin,
    hidden: bool,
}

impl PinTarget {
    fn physical_pin(&self, page_id: &str) -> PhysicalPinRef {
        PhysicalPinRef::new(page_id, &self.symbol_id, &self.number, self.point)
    }

    fn label_key(&self, net_name: &str) -> String {
        format!(
            "zener:net-label:{net_name}:{}:{}",
            self.slot.symbol_id(),
            self.number
        )
    }

    fn net_symbol_key(&self, net_name: &str) -> String {
        format!(
            "zener:net-symbol:{net_name}:{}:{}:{:.4}:{:.4}",
            self.slot.symbol_id(),
            self.number,
            self.point.x,
            self.point.y
        )
    }
}

#[derive(Clone)]
struct NetSymbolRun {
    net_name: String,
    page_index: usize,
    slot: SymbolSlotKey,
    targets: Vec<PinTarget>,
    stair_index: usize,
}

impl NetSymbolRun {
    fn placement_target(&self) -> PinTarget {
        let mut target = self.targets[0].clone();
        let count = self.targets.len() as f64;
        target.point = Point::new(
            self.targets
                .iter()
                .map(|target| target.point.x)
                .sum::<f64>()
                / count,
            self.targets
                .iter()
                .map(|target| target.point.y)
                .sum::<f64>()
                / count,
        );
        target
    }
}

struct NetDriverIsland {
    island: IslandRef,
    canonical_target: usize,
    needs_global: bool,
}

fn net_driver_islands(
    document: &SchDocument,
    targets: &[PinTarget],
    observed: &PhysicalConnectivity,
) -> Result<Vec<NetDriverIsland>> {
    let mut targets_by_island = BTreeMap::<IslandRef, Vec<usize>>::new();
    for (target_index, target) in targets.iter().enumerate() {
        if target.hidden {
            continue;
        }
        let pin = target.physical_pin(&document.pages[target.page_index].id);
        let islands = observed
            .islands
            .iter()
            .filter(|(_, provenance)| provenance.pins.contains(&pin))
            .map(|(island, _)| island.clone())
            .collect::<Vec<_>>();
        let [island] = islands.as_slice() else {
            bail!(
                "visible terminal '{}.{}' belongs to {} physical schematic islands",
                target.slot.component_path(),
                target.number,
                islands.len()
            );
        };
        targets_by_island
            .entry(island.clone())
            .or_default()
            .push(target_index);
    }

    // A page-scoped driver does not serve an island that remains disconnected
    // from the same net on another page; that island requires a global driver.
    let island_group = |island: &IslandRef| {
        observed.graph.groups.iter().position(|group| {
            group
                .origins
                .contains(&ConnectionOrigin::KiCadIsland(island.clone()))
        })
    };
    let island_groups = targets_by_island
        .keys()
        .map(|island| (island.clone(), island_group(island)))
        .collect::<BTreeMap<_, _>>();
    let net_page_count = targets
        .iter()
        .map(|target| target.page_index)
        .collect::<BTreeSet<_>>()
        .len();

    Ok(targets_by_island
        .into_iter()
        .map(|(island, target_indices)| {
            let canonical_target = *target_indices
                .iter()
                .min_by_key(|index| (&targets[**index].slot, &targets[**index].number))
                .expect("a terminal island has a target");
            let needs_global = net_page_count > 1
                && island_groups.iter().any(|(other, group)| {
                    other.page_id != island.page_id && group != &island_groups[&island]
                });
            NetDriverIsland {
                island,
                canonical_target,
                needs_global,
            }
        })
        .collect())
}

fn apply_connectivity_repair(
    document: &mut SchDocument,
    netlist: &Schematic,
    placed: &mut BTreeMap<SymbolSlotKey, PlacedSymbol>,
    net_symbol_specs: &BTreeMap<String, net_symbols::NetSymbolSpec>,
    root_page: usize,
    intent: &ConnectivityRepairIntent,
    reconnect_nets: &BTreeSet<String>,
) -> Result<()> {
    remove_items(document, &intent.removals)?;
    for location in &intent.relocated_symbols {
        relocate_symbol(document, placed, location)?;
    }
    add_connectivity_drivers(
        document,
        netlist,
        placed,
        net_symbol_specs,
        root_page,
        reconnect_nets,
    )?;
    Ok(())
}

/// The name driver a realizer must use for each reconnected net on each page
/// where the net has visible pins or an interface port. Power-symbol nets use
/// their netlist symbol; an island cut off from the same net on another page
/// needs a global label; a page whose module interface exposes the net uses a
/// hierarchical label; a net whose pins span pages without such an interface
/// needs a global label; everything else is a local label.
pub(crate) fn plan_net_driver_kinds(
    document: &SchDocument,
    netlist: &Schematic,
    nets: &BTreeSet<String>,
) -> Result<BTreeMap<String, BTreeMap<String, NetDriverKind>>> {
    if nets.is_empty() {
        return Ok(BTreeMap::new());
    }
    let root_page = document
        .root_page_ids
        .iter()
        .find_map(|id| document.pages.iter().position(|page| &page.id == id))
        .context("KiCad schematic project has no loaded root page")?;
    let expected_slots = component_slots::component_symbol_slots(netlist)?
        .into_iter()
        .collect::<BTreeSet<_>>();
    let placed = placed_symbols_from_document(document, &expected_slots)?;
    let targets = connectivity_targets(netlist, &placed, nets)?;
    let contexts = page_driver_contexts(document, netlist, root_page)?;
    let specs = net_symbols::specs(netlist)?;
    let root_ports = netlist
        .root_ref
        .as_ref()
        .map(|root| root_interface::ports_by_net(netlist, root))
        .transpose()?
        .unwrap_or_default();

    let observed = reduce_with_provenance(document, PinVisibility::VisibleOnly)?;

    let mut kinds = BTreeMap::new();
    for net_name in nets {
        let net_targets = targets.get(net_name).map(Vec::as_slice).unwrap_or(&[]);
        let mut pages = net_targets
            .iter()
            .filter(|target| !target.hidden)
            .map(|target| target.page_index)
            .collect::<BTreeSet<_>>();
        // An island cut off from the same net on another page needs a global
        // driver whatever the page's interface says. This is the realizer's
        // own rule, so a consumer following the intent lands on the same
        // label kind and the pages actually join.
        let global_pages = if specs.contains_key(net_name) {
            BTreeSet::new()
        } else {
            net_driver_islands(document, net_targets, &observed)?
                .into_iter()
                .filter(|island| island.needs_global)
                .map(|island| net_targets[island.canonical_target].page_index)
                .collect::<BTreeSet<_>>()
        };
        if root_ports.contains_key(net_name) {
            pages.insert(root_page);
        }
        // A page whose module interface exposes the net owes it a
        // hierarchical endpoint even when none of the net's pins sit there.
        if !specs.contains_key(net_name) {
            pages.extend(
                contexts
                    .iter()
                    .filter(|(_, context)| context.contains_key(net_name))
                    .map(|(page_index, _)| *page_index),
            );
        }
        let spans_pages = pages.len() > 1;
        let per_page = pages
            .into_iter()
            .map(|page_index| {
                let kind = if let Some(spec) = specs.get(net_name) {
                    NetDriverKind::NetSymbol(spec.clone())
                } else if global_pages.contains(&page_index) {
                    NetDriverKind::Global
                } else if let Some(names) = contexts
                    .get(&page_index)
                    .and_then(|context| context.get(net_name))
                {
                    NetDriverKind::Hierarchical {
                        names: names.clone(),
                    }
                } else if spans_pages {
                    NetDriverKind::Global
                } else {
                    NetDriverKind::Local
                };
                (document.pages[page_index].id.clone(), kind)
            })
            .collect();
        kinds.insert(net_name.clone(), per_page);
    }
    Ok(kinds)
}

/// Move each symbol off its invalid overlap, changing nothing else.
pub(crate) fn relocate_symbols(
    document: &mut SchDocument,
    locations: &BTreeSet<SymbolLocation>,
) -> Result<()> {
    let mut placed = BTreeMap::new();
    for location in locations {
        relocate_symbol(document, &mut placed, location)?;
    }
    Ok(())
}

fn relocate_symbol(
    document: &mut SchDocument,
    placed: &mut BTreeMap<SymbolSlotKey, PlacedSymbol>,
    location: &SymbolLocation,
) -> Result<()> {
    let page_index = document
        .pages
        .iter()
        .position(|page| page.id == location.page_id)
        .with_context(|| format!("schematic page '{}' is absent", location.page_id))?;
    let mut symbol = document.pages[page_index]
        .items
        .iter()
        .find_map(|item| match item {
            SchItem::Symbol(symbol) if symbol.id == location.symbol_id => Some(symbol.clone()),
            _ => None,
        })
        .with_context(|| format!("schematic symbol '{}' is absent", location.symbol_id))?;
    let definition = document.pages[page_index]
        .library
        .definitions
        .get(&symbol.lib_id)
        .with_context(|| {
            format!(
                "schematic symbol '{}' has no cached definition '{}'",
                symbol.id, symbol.lib_id
            )
        })?;
    let mut packer = GridPacker::for_page(&document.pages[page_index].paper)?;
    occupy_page_items_except(
        &mut packer,
        &document.pages[page_index],
        &BTreeSet::from([symbol.id.clone()]),
    )?;
    let relative = GridRect::from_bounds(
        field_autoplace::symbol_visual_bounds(&symbol, definition)?
            .unwrap_or_else(|| {
                field_autoplace::Bounds::from_points([symbol.at]).expect("one point defines bounds")
            })
            .translated(-symbol.at.x, -symbol.at.y),
    );
    let new_at = packer.place_anchored(relative).to_point();
    let delta = Point::new(new_at.x - symbol.at.x, new_at.y - symbol.at.y);
    symbol.at = new_at;
    for field in symbol.fields.values_mut() {
        field.at = Point::new(field.at.x + delta.x, field.at.y + delta.y);
    }
    let item = document.pages[page_index]
        .items
        .iter_mut()
        .find(
            |item| matches!(item, SchItem::Symbol(candidate) if candidate.id == location.symbol_id),
        )
        .expect("located symbol remains present");
    *item = SchItem::Symbol(symbol.clone());

    if let Some(slot) = symbol
        .field_value("Path")
        .and_then(|path| SymbolSlotKey::new(path, symbol.unit))
        && let Some(placed) = placed.get_mut(&slot)
    {
        placed.symbol = symbol;
        placed.page_index = page_index;
    }
    Ok(())
}

fn add_hierarchy_connectivity(
    document: &mut SchDocument,
    netlist: &Schematic,
    placed: &BTreeMap<SymbolSlotKey, PlacedSymbol>,
    net_symbol_specs: &BTreeMap<String, net_symbols::NetSymbolSpec>,
    plan: &hierarchy::HierarchyPlan,
) -> Result<()> {
    if plan.sheets.is_empty() {
        return Ok(());
    }
    let all_nets = named_connected_nets(netlist)
        .map(|net| net.name.clone())
        .collect();
    let targets = connectivity_targets(netlist, placed, &all_nets)?;
    let page_contexts = page_driver_contexts(document, netlist, plan.root_page())?;

    for sheet_plan in &plan.sheets {
        let ports = module_ports(netlist, &sheet_plan.instance_ref)?;
        let sheet = document.pages[sheet_plan.parent_page]
            .items
            .iter()
            .find_map(|item| match item {
                SchItem::Sheet(sheet)
                    if sheet.id == hierarchy::sheet_id(&sheet_plan.module_path) =>
                {
                    Some(sheet)
                }
                _ => None,
            })
            .context("materialized hierarchy sheet is absent from its parent page")?;
        let parent_anchors = sheet.pins.iter().map(|pin| pin.at).collect::<Vec<_>>();

        for ((net_name, port_name), parent_pin) in ports.into_iter().zip(parent_anchors) {
            let key = format!("{}:{port_name}", sheet_plan.module_path);
            upsert_contextual_driver(
                document,
                net_symbol_specs,
                &page_contexts,
                sheet_plan.parent_page,
                &net_name,
                (parent_pin, LabelSpin::Left),
                &format!("zener:module-parent-driver:{key}:{net_name}"),
            )?;

            let child_anchor = canonical_target(&targets, &net_name, sheet_plan.child_page)
                .map(|target| (target.point, target.spin))
                .unwrap_or((
                    place_context_label(document, sheet_plan.child_page, &port_name)?,
                    LabelSpin::Right,
                ));
            upsert_contextual_driver(
                document,
                net_symbol_specs,
                &page_contexts,
                sheet_plan.child_page,
                &net_name,
                child_anchor,
                &format!("zener:module-child-driver:{key}:{net_name}"),
            )?;
        }
    }
    Ok(())
}

fn add_connectivity_drivers(
    document: &mut SchDocument,
    netlist: &Schematic,
    placed: &BTreeMap<SymbolSlotKey, PlacedSymbol>,
    net_symbol_specs: &BTreeMap<String, net_symbols::NetSymbolSpec>,
    root_page: usize,
    target_nets: &BTreeSet<String>,
) -> Result<()> {
    let anchors_by_net = connectivity_targets(netlist, placed, target_nets)?;
    let page_contexts = page_driver_contexts(document, netlist, root_page)?;
    ensure_context_endpoints(
        document,
        &anchors_by_net,
        net_symbol_specs,
        &page_contexts,
        target_nets,
    )?;
    sync_net_drivers(
        document,
        &anchors_by_net,
        placed,
        net_symbol_specs,
        &page_contexts,
    )?;
    ensure_symbol_root_endpoints(
        document,
        netlist,
        &anchors_by_net,
        net_symbol_specs,
        root_page,
        target_nets,
    )
}

fn ensure_symbol_root_endpoints(
    document: &mut SchDocument,
    netlist: &Schematic,
    targets_by_net: &BTreeMap<String, Vec<PinTarget>>,
    specs: &BTreeMap<String, net_symbols::NetSymbolSpec>,
    root_page: usize,
    target_nets: &BTreeSet<String>,
) -> Result<()> {
    let Some(root) = &netlist.root_ref else {
        return Ok(());
    };
    for (net_name, interface_names) in root_interface::symbol_ports_by_net(netlist, root)? {
        if !target_nets.contains(&net_name) {
            continue;
        }
        let Some(spec) = specs.get(&net_name) else {
            continue;
        };
        let has_net_symbol = document.pages[root_page].items.iter().any(|item| {
            matches!(item, SchItem::Symbol(symbol)
                    if symbol.field_value("Path").is_none()
                        && symbol.field_value("Value") == Some(net_name.as_str()))
        });
        let has_all_hierarchical_labels = interface_names.iter().all(|interface_name| {
            document.pages[root_page].items.iter().any(|item| {
                matches!(item, SchItem::Label(label)
                    if matches!(label.kind, LabelKind::Hierarchical { .. })
                        && label.text == *interface_name)
            })
        });
        if has_net_symbol || has_all_hierarchical_labels {
            continue;
        }
        let at = canonical_target(targets_by_net, &net_name, root_page)
            .map(|target| target.point)
            .unwrap_or(place_context_label(document, root_page, &net_name)?);
        let page_id = document.pages[root_page].id.clone();
        let symbol = build_net_symbol(
            spec,
            &net_name,
            deterministic_uuid(format!("zener:root-symbol-endpoint:{page_id}:{net_name}")),
            at,
        )?;
        insert_net_symbol(document, root_page, symbol, &spec.definition)?;
    }
    Ok(())
}

fn sync_net_drivers(
    document: &mut SchDocument,
    targets_by_net: &BTreeMap<String, Vec<PinTarget>>,
    placed: &BTreeMap<SymbolSlotKey, PlacedSymbol>,
    net_symbol_specs: &BTreeMap<String, net_symbols::NetSymbolSpec>,
    page_contexts: &PageDriverContexts,
) -> Result<()> {
    // Adopt any exact existing contextual driver. For an unnamed island, add
    // a power symbol, current-sheet hierarchical endpoint, or local label in
    // that order without treating a deterministic UUID as ownership.
    let net_symbol_runs = plan_net_symbol_runs(document, targets_by_net, net_symbol_specs, placed)?;
    let observed = reduce_with_provenance(document, PinVisibility::VisibleOnly)?;
    // Drivers this pass adds, by net: the snapshot above predates them, and
    // meeting a net's own new symbol or stub is not a collision.
    let mut added_by_net = BTreeMap::<String, BTreeSet<ConnectivityItemRef>>::new();

    for run in net_symbol_runs {
        let spec = &net_symbol_specs[&run.net_name];
        let target = &run.targets[0];
        // Geometry the new driver may touch: every island that already
        // belongs to this net, whether named or reached through one of the
        // net's pins. Meeting it never shorts and may well reconnect it.
        let net_targets = targets_by_net
            .get(&run.net_name)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let net_pins = net_targets
            .iter()
            .map(|member| member.physical_pin(&document.pages[member.page_index].id))
            .collect::<BTreeSet<_>>();
        let mut own_items = observed
            .islands
            .values()
            .filter(|island| {
                island.names.contains(&run.net_name) || !island.pins.is_disjoint(&net_pins)
            })
            .flat_map(|island| island.items.iter().cloned())
            .collect::<BTreeSet<_>>();
        if let Some(added) = added_by_net.get(&run.net_name) {
            own_items.extend(added.iter().cloned());
        }
        let page_id = document.pages[run.page_index].id.clone();
        // Only this page's pins are safe to meet: a coordinate that merely
        // matches a pin on another page is foreign geometry here.
        let own_points = net_targets
            .iter()
            .filter(|member| member.page_index == run.page_index)
            .map(|member| member.point)
            .collect::<Vec<_>>();
        let placement_target = run.placement_target();
        let mut connection_point = None;
        for extra in 0..NET_SYMBOL_STAIR_RETRIES {
            let candidate = net_symbol_connection_point(
                &placed[&run.slot],
                &placement_target,
                spec.pin_outward_spin,
                run.stair_index + extra,
            )?;
            if !net_symbol_stub_collides(
                &document.pages[run.page_index],
                &run.targets,
                &own_points,
                &own_items,
                candidate,
            )? {
                connection_point = Some(candidate);
                break;
            }
        }
        let Some(connection_point) = connection_point else {
            // Dense wiring leaves the symbol no clear spot. A label at the
            // pin names the island with no new geometry, in the scope the
            // power symbol itself has: global, so the island joins the net's
            // symbols on every page, or local for a local power symbol.
            let spans_pages = matches!(
                symbol::ParsedSymbolDefinition::parse(&spec.definition)?.power_scope(),
                Some(symbol::PowerScope::Global)
            );
            for member in &run.targets {
                let id = available_deterministic_id(document, &member.label_key(&run.net_name));
                let mut label = driver_label(&run.net_name, None, spans_pages, id, member.point);
                label.spin = member.spin;
                added_by_net
                    .entry(run.net_name.clone())
                    .or_default()
                    .insert(ConnectivityItemRef::Label {
                        page_id: page_id.clone(),
                        id: label.id.clone(),
                    });
                document.pages[member.page_index]
                    .items
                    .push(SchItem::Label(label));
            }
            continue;
        };
        let id = available_deterministic_id(document, &target.net_symbol_key(&run.net_name));
        let symbol = build_net_symbol(spec, &run.net_name, id, connection_point)?;
        let symbol_id = symbol.id.clone();
        insert_net_symbol(document, run.page_index, symbol, &spec.definition)?;
        let added = added_by_net.entry(run.net_name.clone()).or_default();
        added.insert(ConnectivityItemRef::Symbol {
            page_id: page_id.clone(),
            id: symbol_id.clone(),
        });
        for member in run.targets {
            let wire_ids = insert_connection_wires(
                document,
                member.page_index,
                &symbol_id,
                member.point,
                connection_point,
                member.spin,
            );
            added.extend(wire_ids.into_iter().map(|id| ConnectivityItemRef::Wire {
                page_id: page_id.clone(),
                id,
            }));
        }
    }

    for (net_name, targets) in targets_by_net {
        if net_symbol_specs.contains_key(net_name) {
            continue;
        }
        for driver_island in net_driver_islands(document, targets, &observed)? {
            let target = &targets[driver_island.canonical_target];
            let provenance = &observed.islands[&driver_island.island];
            let interface_names = page_contexts
                .get(&target.page_index)
                .and_then(|context| context.get(net_name));
            let has_driver = if driver_island.needs_global {
                island_has_global_driver(document, target.page_index, provenance, net_name)
            } else if let Some(interface_names) = interface_names {
                island_has_hierarchical_driver(
                    document,
                    target.page_index,
                    provenance,
                    interface_names,
                )
            } else {
                has_named_driver(provenance, net_name)
            };
            if has_driver {
                continue;
            }
            let id = available_deterministic_id(document, &target.label_key(net_name));
            let mut label = driver_label(
                net_name,
                if driver_island.needs_global {
                    None
                } else {
                    interface_names
                },
                driver_island.needs_global,
                id,
                target.point,
            );
            label.spin = target.spin;
            document.pages[target.page_index]
                .items
                .push(SchItem::Label(label));
        }
    }

    Ok(())
}

fn adjacent_target_runs<'a, T>(
    placed: &PlacedSymbol,
    mut targets: Vec<T>,
    pin_target: impl Fn(&T) -> &'a PinTarget,
) -> Result<Vec<Vec<T>>> {
    let Some(side) = targets.first().map(|target| pin_target(target).spin) else {
        return Ok(Vec::new());
    };
    debug_assert!(targets.iter().all(|target| pin_target(target).spin == side));
    let compare_points = |left: Point, right: Point| {
        let primary = if matches!(side, LabelSpin::Left | LabelSpin::Right) {
            left.y.total_cmp(&right.y)
        } else {
            left.x.total_cmp(&right.x)
        };
        primary
            .then_with(|| left.x.total_cmp(&right.x))
            .then_with(|| left.y.total_cmp(&right.y))
    };
    targets.sort_by(|left, right| {
        let left = pin_target(left);
        let right = pin_target(right);
        compare_points(left.point, right.point).then_with(|| left.number.cmp(&right.number))
    });

    let mut visible_pins = placed
        .definition
        .placed_pins(&placed.symbol)?
        .into_iter()
        .filter(|pin| !pin.hidden && pin.outward_spin == side)
        .collect::<Vec<_>>();
    visible_pins.sort_by(|left, right| {
        compare_points(left.point, right.point).then_with(|| left.number.cmp(&right.number))
    });

    let ranked = targets
        .into_iter()
        .map(|item| {
            let target = pin_target(&item);
            let matches = visible_pins
                .iter()
                .enumerate()
                .filter(|(_, pin)| pin.number == target.number && pin.point == target.point)
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            let [rank] = matches.as_slice() else {
                bail!(
                    "schematic pin '{}:{}' does not uniquely identify a visible component pin",
                    target.slot.component_path(),
                    target.number
                );
            };
            let stacked = visible_pins
                .iter()
                .filter(|pin| pin.point == target.point)
                .count()
                > 1;
            Ok((item, *rank, stacked))
        })
        .collect::<Result<Vec<_>>>()?;

    let mut runs = Vec::<Vec<T>>::new();
    let mut previous: Option<(usize, bool)> = None;
    for (target, rank, stacked) in ranked {
        let joins_previous = previous.is_some_and(|(previous_rank, previous_stacked)| {
            !stacked && !previous_stacked && rank == previous_rank + 1
        });
        if joins_previous {
            runs.last_mut().expect("a previous run exists").push(target);
        } else {
            runs.push(vec![target]);
        }
        previous = Some((rank, stacked));
    }
    Ok(runs)
}

fn plan_net_symbol_runs(
    document: &SchDocument,
    targets_by_net: &BTreeMap<String, Vec<PinTarget>>,
    net_symbol_specs: &BTreeMap<String, net_symbols::NetSymbolSpec>,
    placed: &BTreeMap<SymbolSlotKey, PlacedSymbol>,
) -> Result<Vec<NetSymbolRun>> {
    let observed = reduce_with_provenance(document, PinVisibility::VisibleOnly)?;
    let mut missing = Vec::new();
    for (net_name, targets) in targets_by_net {
        if !net_symbol_specs.contains_key(net_name) {
            continue;
        }

        for driver_island in net_driver_islands(document, targets, &observed)? {
            let target = &targets[driver_island.canonical_target];
            let provenance = &observed.islands[&driver_island.island];
            let has_driver = if driver_island.needs_global {
                island_has_global_driver(document, target.page_index, provenance, net_name)
            } else {
                has_named_driver(provenance, net_name)
            };
            if has_driver {
                continue;
            }
            missing.push((net_name, target));
        }
    }
    arrange_net_symbol_runs(missing, placed, net_symbol_specs)
}

fn plan_projected_net_symbol_runs(
    targets_by_net: &BTreeMap<String, Vec<PinTarget>>,
    projected_slots: &BTreeSet<SymbolSlotKey>,
    net_symbol_specs: &BTreeMap<String, net_symbols::NetSymbolSpec>,
    placed: &BTreeMap<SymbolSlotKey, PlacedSymbol>,
) -> Result<Vec<NetSymbolRun>> {
    let missing = targets_by_net
        .iter()
        .filter(|(net_name, _)| net_symbol_specs.contains_key(*net_name))
        .flat_map(|(net_name, targets)| {
            targets
                .iter()
                .filter(|target| !target.hidden && projected_slots.contains(&target.slot))
                .map(move |target| (net_name, target))
        });
    arrange_net_symbol_runs(missing, placed, net_symbol_specs)
}

fn arrange_net_symbol_runs<'a>(
    missing: impl IntoIterator<Item = (&'a String, &'a PinTarget)>,
    placed: &BTreeMap<SymbolSlotKey, PlacedSymbol>,
    net_symbol_specs: &BTreeMap<String, net_symbols::NetSymbolSpec>,
) -> Result<Vec<NetSymbolRun>> {
    type Slot = (usize, SymbolSlotKey, LabelSpin);
    type GroupKey = (String, usize, SymbolSlotKey, LabelSpin);

    let mut groups = BTreeMap::<GroupKey, Vec<&PinTarget>>::new();
    for (net_name, target) in missing {
        groups
            .entry((
                net_name.clone(),
                target.page_index,
                target.slot.clone(),
                target.spin,
            ))
            .or_default()
            .push(target);
    }

    let mut runs = Vec::<NetSymbolRun>::new();
    let mut slots = BTreeMap::<Slot, Vec<usize>>::new();
    for ((net_name, page_index, slot, target_side), members) in groups {
        for members in adjacent_target_runs(&placed[&slot], members, |target| *target)? {
            let run_index = runs.len();
            runs.push(NetSymbolRun {
                net_name: net_name.clone(),
                page_index,
                slot: slot.clone(),
                targets: members.into_iter().cloned().collect(),
                stair_index: 0,
            });
            slots
                .entry((page_index, slot.clone(), target_side))
                .or_default()
                .push(run_index);
        }
    }

    for ((_, slot, target_side), run_indices) in &slots {
        let banks = {
            let indexed_targets = run_indices
                .iter()
                .flat_map(|run_index| {
                    runs[*run_index]
                        .targets
                        .iter()
                        .map(move |target| (*run_index, target))
                })
                .collect::<Vec<_>>();
            adjacent_target_runs(&placed[slot], indexed_targets, |(_, target)| *target)?
                .into_iter()
                .map(|bank| {
                    let mut bank_runs = Vec::new();
                    for (run_index, _) in bank {
                        if !bank_runs.contains(&run_index) {
                            bank_runs.push(run_index);
                        }
                    }
                    bank_runs
                })
                .collect::<Vec<_>>()
        };
        // A stair that follows its pin (top or bottom pins, or side pins
        // with a sideways symbol) restarts per bank: distinct pins already
        // give distinct points. A side pin with an upright symbol lands at
        // the body corner whatever the pin, so those stairs number the whole
        // side, or two banks' symbols would stack and short their nets.
        let mut corner_stair = 0;
        for bank_runs in banks {
            let mut bank_stair = 0;
            for run_index in bank_runs {
                let run = &mut runs[run_index];
                let follows_pin = target_side.is_vertical()
                    || net_symbol_specs
                        .get(&run.net_name)
                        .is_some_and(|spec| !spec.pin_outward_spin.is_vertical());
                if follows_pin {
                    run.stair_index = bank_stair;
                    bank_stair += 1;
                } else {
                    run.stair_index = corner_stair;
                    corner_stair += 1;
                }
            }
        }
    }
    Ok(runs)
}

fn net_symbol_connection_point(
    placed: &PlacedSymbol,
    target: &PinTarget,
    symbol_pin_spin: LabelSpin,
    stair_index: usize,
) -> Result<Point> {
    let bounds = field_autoplace::symbol_geometry_bounds(&placed.symbol, &placed.definition)?
        .unwrap_or_else(|| {
            field_autoplace::Bounds::from_points([placed.symbol.at])
                .expect("one point defines bounds")
        });
    let offset = NET_SYMBOL_OFFSET_CELLS * CONNECTION_GRID_MM;
    let stair = stair_index as f64 * NET_SYMBOL_STAIR_CELLS * CONNECTION_GRID_MM;
    let point = match target.spin {
        LabelSpin::Left | LabelSpin::Right => {
            let x = if target.spin == LabelSpin::Left {
                bounds.min_x - offset - stair
            } else {
                bounds.max_x + offset + stair
            };
            let y = match symbol_pin_spin {
                LabelSpin::Up => bounds.max_y + offset + stair,
                LabelSpin::Bottom => bounds.min_y - offset - stair,
                LabelSpin::Left | LabelSpin::Right => target.point.y + stair,
            };
            Point::new(x, y)
        }
        LabelSpin::Up => Point::new(target.point.x + stair, bounds.min_y - offset - stair),
        LabelSpin::Bottom => Point::new(target.point.x + stair, bounds.max_y + offset + stair),
    };
    Ok(Point::new(
        snap_connection_grid(point.x),
        snap_connection_grid(point.y),
    ))
}

fn snap_connection_grid(value: f64) -> f64 {
    (value / CONNECTION_GRID_MM).round() * CONNECTION_GRID_MM
}

fn insert_connection_wires(
    document: &mut SchDocument,
    page_index: usize,
    symbol_id: &str,
    target: Point,
    connection: Point,
    target_spin: LabelSpin,
) -> Vec<String> {
    let bend = net_symbol_wire_bend(target, connection, target_spin);
    let mut ids = Vec::new();
    for (index, (a, b)) in [(target, bend), (bend, connection)].into_iter().enumerate() {
        if points_coincide(a, b) {
            continue;
        }
        let id = available_deterministic_id(
            document,
            &format!("zener:net-symbol-wire:{symbol_id}:{index}"),
        );
        ids.push(id.clone());
        document.pages[page_index].items.push(SchItem::Wire(Wire {
            id,
            a,
            b,
            unsupported: Vec::new(),
        }));
    }
    ids
}

/// Whether a net symbol at `connection`, with stubs from each target pin,
/// would touch geometry that is not already part of the targets' islands.
/// Meeting a wire end, junction, label, or pin would connect the new driver to
/// whatever sits there; landing on a wire's interior is at best ambiguous.
fn net_symbol_stub_collides(
    page: &SchPage,
    targets: &[PinTarget],
    own_points: &[Point],
    own_items: &BTreeSet<ConnectivityItemRef>,
    connection: Point,
) -> Result<bool> {
    let is_own_point = |point: Point| own_points.iter().any(|own| points_coincide(*own, point));
    let mut new_segments = Vec::new();
    for target in targets {
        let bend = net_symbol_wire_bend(target.point, connection, target.spin);
        for (a, b) in [(target.point, bend), (bend, connection)] {
            if !points_coincide(a, b) {
                new_segments.push((a, b));
            }
        }
    }
    let mut new_points = vec![connection];
    for (a, b) in &new_segments {
        new_points.push(*a);
        new_points.push(*b);
    }
    new_points.retain(|point| !is_own_point(*point));

    let mut existing_points = Vec::new();
    let mut existing_segments = Vec::new();
    for item in &page.items {
        if own_items
            .iter()
            .any(|item_ref| item_matches(&page.id, item, item_ref))
        {
            continue;
        }
        match item {
            SchItem::Wire(wire) => {
                existing_points.extend([wire.a, wire.b]);
                existing_segments.push((wire.a, wire.b));
            }
            SchItem::Junction(junction) => existing_points.push(junction.at),
            SchItem::Label(label) => existing_points.push(label.at),
            SchItem::NoConnect(no_connect) => existing_points.push(no_connect.at),
            SchItem::Sheet(sheet) => existing_points.extend(sheet.pins.iter().map(|pin| pin.at)),
            SchItem::Symbol(symbol) => {
                if let Some(definition) = page.library.definitions.get(&symbol.lib_id) {
                    existing_points.extend(
                        definition
                            .placed_pins(symbol)?
                            .into_iter()
                            .map(|pin| pin.point),
                    );
                }
            }
            SchItem::Unsupported(_) => {}
        }
    }
    existing_points.retain(|point| !is_own_point(*point));

    Ok(new_points.iter().any(|point| {
        existing_points
            .iter()
            .any(|existing| points_coincide(*existing, *point))
            || existing_segments
                .iter()
                .any(|(a, b)| point_on_segment(*point, *a, *b))
    }) || existing_points.iter().any(|point| {
        new_segments
            .iter()
            .any(|(a, b)| point_on_segment(*point, *a, *b))
    }))
}

fn net_symbol_wire_bend(target: Point, connection: Point, target_spin: LabelSpin) -> Point {
    if target_spin.is_vertical() {
        Point::new(target.x, connection.y)
    } else {
        Point::new(connection.x, target.y)
    }
}

fn points_coincide(left: Point, right: Point) -> bool {
    (left.x - right.x).abs() <= GEOMETRY_EPS_MM && (left.y - right.y).abs() <= GEOMETRY_EPS_MM
}

type PageDriverContexts = BTreeMap<usize, BTreeMap<String, BTreeSet<String>>>;

fn page_driver_contexts(
    document: &SchDocument,
    netlist: &Schematic,
    root_page: usize,
) -> Result<PageDriverContexts> {
    // A page's interface names come from its identity: the root page speaks
    // the root module's interface, and a generated module page (deterministic
    // page id) speaks that module's interface. Pages the user created or
    // reorganized carry no interface names, so their drivers fall back to
    // local labels and net symbols — any KiCad organization stays valid.
    let modules = linked_modules(netlist)?;
    let mut module_by_page = BTreeMap::<usize, &pcb_sch::InstanceRef>::new();
    if let Some(root) = netlist.root_ref.as_ref() {
        module_by_page.insert(root_page, root);
    }
    for module in &modules {
        if let Some(page_index) = document
            .pages
            .iter()
            .position(|page| page.id == hierarchy::page_id(&module.path))
        {
            module_by_page.insert(page_index, &module.instance_ref);
        }
    }

    let mut contexts = module_by_page
        .into_iter()
        .map(|(page_index, module_ref)| {
            Ok((
                page_index,
                root_interface::ports_by_net(netlist, module_ref)?,
            ))
        })
        .collect::<Result<PageDriverContexts>>()?;

    // A page the user created (subsheet extraction, reorganization) has no
    // module identity of its own: it belongs to the module of the page
    // instantiating it, so it inherits that page's interface names and its
    // drivers can bridge through the sheet pins. Contexts flow down the
    // sheet hierarchy until nothing changes.
    let page_by_file: BTreeMap<String, usize> = document
        .pages
        .iter()
        .enumerate()
        .filter_map(|(index, page)| {
            let file_name = page.file_name.as_deref()?;
            Some((
                crate::normalize_schematic_path(std::path::Path::new(file_name))
                    .to_string_lossy()
                    .replace('\\', "/"),
                index,
            ))
        })
        .collect();
    loop {
        let mut changed = false;
        for parent_index in 0..document.pages.len() {
            let Some(parent_context) = contexts.get(&parent_index).cloned() else {
                continue;
            };
            for item in &document.pages[parent_index].items {
                let SchItem::Sheet(sheet) = item else {
                    continue;
                };
                let child_file = crate::connectivity::kicad::resolve_file_name(
                    &document.pages[parent_index],
                    sheet.file_name(),
                );
                let Some(&child_index) = page_by_file.get(&child_file) else {
                    continue;
                };
                if child_index == parent_index || contexts.contains_key(&child_index) {
                    continue;
                }
                contexts.insert(child_index, parent_context.clone());
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    Ok(contexts)
}

fn ensure_context_endpoints(
    document: &mut SchDocument,
    targets_by_net: &BTreeMap<String, Vec<PinTarget>>,
    net_symbol_specs: &BTreeMap<String, net_symbols::NetSymbolSpec>,
    page_contexts: &PageDriverContexts,
    target_nets: &BTreeSet<String>,
) -> Result<()> {
    for (&page_index, context) in page_contexts {
        let page_id = document.pages[page_index].id.clone();
        let mut free_labels = Vec::new();
        for (net_name, interface_names) in context {
            if !target_nets.contains(net_name) || net_symbol_specs.contains_key(net_name) {
                continue;
            }
            // Anchor at a pin, else at a sheet pin carrying the interface
            // (the net reaches this page only through a subsheet). Collect
            // interface-only endpoints and place them as one regular block.
            let attached = canonical_target(targets_by_net, net_name, page_index)
                .map(|target| (target.point, target.spin))
                .or_else(|| sheet_pin_anchor(document, page_index, interface_names));
            for interface_name in interface_names {
                if page_has_hierarchical_label(document, page_index, interface_name) {
                    continue;
                }
                let Some((anchor, spin)) = attached else {
                    free_labels.push((net_name, interface_name));
                    continue;
                };
                let mut label = Label::new(
                    deterministic_uuid(format!(
                        "zener:context-endpoint:{page_id}:{net_name}:{interface_name}"
                    )),
                    interface_name,
                    anchor,
                );
                label.kind = LabelKind::Hierarchical {
                    shape: LabelShape::Bidirectional,
                };
                label.spin = spin;
                upsert_label(document, page_index, label)?;
            }
        }

        let texts = free_labels
            .iter()
            .map(|(_, interface_name)| interface_name.as_str())
            .collect::<Vec<_>>();
        let anchors = place_context_label_group(document, page_index, &texts)?;
        let placed = free_labels.into_iter().zip(anchors).collect::<Vec<_>>();
        for ((net_name, interface_name), anchor) in &placed {
            let mut label = Label::new(
                deterministic_uuid(format!(
                    "zener:context-endpoint:{page_id}:{net_name}:{interface_name}"
                )),
                *interface_name,
                *anchor,
            );
            label.kind = LabelKind::Hierarchical {
                shape: LabelShape::Bidirectional,
            };
            label.spin = LabelSpin::Right;
            upsert_label(document, page_index, label)?;
        }
        // Hierarchical labels merge by text, so a net exposed under several
        // interface names would otherwise split into one island per label.
        // A wire through the group's consecutive rows makes them one.
        for pair in placed.windows(2) {
            let ((net_name, interface_name), anchor) = &pair[0];
            let ((next_net_name, next_interface_name), next_anchor) = &pair[1];
            if net_name != next_net_name {
                continue;
            }
            upsert_wire(
                document,
                page_index,
                Wire {
                    id: deterministic_uuid(format!(
                        "zener:context-endpoint-wire:{page_id}:{net_name}:{interface_name}:{next_interface_name}"
                    )),
                    a: *anchor,
                    b: *next_anchor,
                    unsupported: Vec::new(),
                },
            );
        }
    }
    Ok(())
}

/// A sheet pin on this page named after one of the net's interface names:
/// the attachment point for the page's context label when the net has no
/// component pins here.
fn sheet_pin_anchor(
    document: &SchDocument,
    page_index: usize,
    interface_names: &BTreeSet<String>,
) -> Option<(Point, LabelSpin)> {
    document.pages[page_index].items.iter().find_map(|item| {
        let SchItem::Sheet(sheet) = item else {
            return None;
        };
        sheet
            .pins
            .iter()
            .find(|pin| interface_names.contains(&pin.name))
            .map(|pin| {
                let spin = match pin.rotation {
                    Rotation::Deg0 => LabelSpin::Right,
                    Rotation::Deg90 => LabelSpin::Up,
                    Rotation::Deg180 => LabelSpin::Left,
                    Rotation::Deg270 => LabelSpin::Bottom,
                };
                (pin.at, spin)
            })
    })
}

fn canonical_target<'a>(
    targets_by_net: &'a BTreeMap<String, Vec<PinTarget>>,
    net_name: &str,
    page_index: usize,
) -> Option<&'a PinTarget> {
    targets_by_net
        .get(net_name)?
        .iter()
        .filter(|target| target.page_index == page_index && !target.hidden)
        .min_by_key(|target| (&target.slot, &target.number))
}

fn upsert_contextual_driver(
    document: &mut SchDocument,
    net_symbol_specs: &BTreeMap<String, net_symbols::NetSymbolSpec>,
    page_contexts: &PageDriverContexts,
    page_index: usize,
    net_name: &str,
    anchor: (Point, LabelSpin),
    key: &str,
) -> Result<()> {
    let (at, spin) = anchor;
    if let Some(spec) = net_symbol_specs.get(net_name) {
        let symbol = build_net_symbol(spec, net_name, deterministic_uuid(key), at)?;
        return insert_net_symbol(document, page_index, symbol, &spec.definition);
    }
    let interface_names = page_contexts
        .get(&page_index)
        .and_then(|context| context.get(net_name));
    // Hierarchy drivers bridge pages through their sheet pins, so they never
    // need a global label.
    let mut label = driver_label(
        net_name,
        interface_names,
        false,
        deterministic_uuid(key),
        at,
    );
    label.spin = spin;
    upsert_label(document, page_index, label)
}

/// The driver label for a net on one page: hierarchical when the page's
/// interface exposes the net, global when the net spans pages that no
/// interface path connects, local otherwise.
fn driver_label(
    net_name: &str,
    interface_names: Option<&BTreeSet<String>>,
    spans_unbridged_pages: bool,
    id: String,
    at: Point,
) -> Label {
    let text = interface_names
        .and_then(|names| names.first())
        .map_or(net_name, String::as_str);
    let mut label = Label::new(id, text, at);
    if interface_names.is_some() {
        label.kind = LabelKind::Hierarchical {
            shape: LabelShape::Bidirectional,
        };
    } else if spans_unbridged_pages {
        label.kind = LabelKind::Global {
            shape: LabelShape::Bidirectional,
        };
    }
    label
}

fn has_named_driver(provenance: &PhysicalIsland, net_name: &str) -> bool {
    provenance
        .named_drivers
        .get(net_name)
        .is_some_and(|drivers| !drivers.is_empty())
}

/// Whether the island carries a driver that merges by name across pages: a
/// global label with the net's name, or a power symbol (power symbols merge
/// globally through their hidden power-input pins).
fn island_has_global_driver(
    document: &SchDocument,
    page_index: usize,
    provenance: &PhysicalIsland,
    net_name: &str,
) -> bool {
    provenance
        .named_drivers
        .get(net_name)
        .is_some_and(|drivers| {
            drivers.iter().any(|driver| match driver {
                ConnectivityItemRef::Symbol { .. } => true,
                ConnectivityItemRef::Label { id, .. } => {
                    document.pages[page_index].items.iter().any(|item| {
                        matches!(item, SchItem::Label(label)
                        if label.id == *id && matches!(label.kind, LabelKind::Global { .. }))
                    })
                }
                _ => false,
            })
        })
}

fn island_has_hierarchical_driver(
    document: &SchDocument,
    page_index: usize,
    provenance: &PhysicalIsland,
    interface_names: &BTreeSet<String>,
) -> bool {
    provenance.items.iter().any(|item| {
        let ConnectivityItemRef::Label { id, .. } = item else {
            return false;
        };
        document.pages[page_index].items.iter().any(|item| {
            matches!(item, SchItem::Label(label)
                if label.id == *id
                    && matches!(label.kind, LabelKind::Hierarchical { .. })
                    && interface_names.contains(&label.text))
        })
    })
}

fn page_has_hierarchical_label(
    document: &SchDocument,
    page_index: usize,
    interface_name: &str,
) -> bool {
    document.pages[page_index].items.iter().any(|item| {
        matches!(item, SchItem::Label(label)
            if matches!(label.kind, LabelKind::Hierarchical { .. })
                && label.text == interface_name)
    })
}

fn build_net_symbol(
    spec: &net_symbols::NetSymbolSpec,
    net_name: &str,
    id: String,
    connection_point: Point,
) -> Result<Symbol> {
    let mut fields = spec.definition.default_fields()?;
    let reference = format!(
        "#PWR{}",
        id.chars().filter(|c| *c != '-').collect::<String>()
    );
    fields
        .entry("Reference".to_string())
        .or_insert_with(|| SymbolField::new("Reference", &reference, Point::default()))
        .value = reference.clone();
    fields
        .entry("Value".to_string())
        .or_insert_with(|| SymbolField::new("Value", net_name, Point::default()))
        .value = net_name.to_string();
    for field in fields.values_mut() {
        field.at = Point::default();
    }

    let mut symbol = Symbol {
        id,
        lib_id: spec.definition.lib_id.clone(),
        unit: spec.unit,
        body_style: 1,
        at: Point::default(),
        rotation: Rotation::default(),
        mirror: None,
        dnp: false,
        in_bom: true,
        on_board: true,
        in_pos_files: true,
        fields_autoplaced: true,
        fields,
        pins: Vec::new(),
        unsupported: Vec::new(),
    };
    reconcile_pin_instances(&mut symbol, &spec.definition, &[])?;
    symbol.at = Point::new(
        connection_point.x - spec.pin_offset.x,
        connection_point.y - spec.pin_offset.y,
    );
    for field in symbol.fields.values_mut() {
        field.at = symbol.at;
    }
    field_autoplace::apply_definition_field_styles(&mut symbol, &spec.definition)?;
    field_autoplace::autoplace_symbol_fields(&mut symbol, &spec.definition)?;
    Ok(symbol)
}

fn connectivity_targets(
    netlist: &Schematic,
    placed: &BTreeMap<SymbolSlotKey, PlacedSymbol>,
    target_nets: &BTreeSet<String>,
) -> Result<BTreeMap<String, Vec<PinTarget>>> {
    let mut anchors_by_net = BTreeMap::<String, Vec<PinTarget>>::new();
    let mut nets = named_connected_nets(netlist).collect::<Vec<_>>();
    nets.sort_by(|a, b| a.name.cmp(&b.name));
    for net in &nets {
        if !target_nets.contains(&net.name) {
            continue;
        }
        for port in &net.ports {
            let Some((component_ref, pin_name)) = netlist.component_ref_and_pin_for_port(port)
            else {
                continue;
            };
            let component_path = crate::canonical_component_path(&component_ref.instance_path)
                .context("net terminal component has no canonical path")?;
            let pin_numbers = component_slots::port_pad_numbers(netlist, port);
            let targets = resolve_pin_targets(placed, &component_path, &pin_name, &pin_numbers)?;
            anchors_by_net
                .entry(net.name.clone())
                .or_default()
                .extend(targets);
        }
    }
    Ok(anchors_by_net)
}

fn place_context_label(document: &SchDocument, page_index: usize, text: &str) -> Result<Point> {
    let mut packer = GridPacker::for_page(&document.pages[page_index].paper)?;
    occupy_page_items(&mut packer, &document.pages[page_index])?;
    let label_height = crate::TextEffects::default().font_size.y.abs();
    let label_width = estimated_shaped_label_width(text);
    let relative = GridRect::from_bounds(
        field_autoplace::Bounds::from_points([
            Point::new(-label_width * 0.5, -label_height * 0.5),
            Point::new(label_width * 0.5, label_height * 0.5),
        ])
        .expect("interface label has two corners"),
    );
    Ok(packer.place(relative).to_point())
}

fn place_context_label_group(
    document: &SchDocument,
    page_index: usize,
    texts: &[&str],
) -> Result<Vec<Point>> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    let bounds = texts
        .iter()
        .map(|text| {
            GridRect::from_bounds(estimated_label_bounds(
                Point::default(),
                LabelSpin::Right,
                estimated_shaped_label_width(text),
            ))
        })
        .collect::<Vec<_>>();
    let row_height = bounds
        .iter()
        .map(|bounds| bounds.height())
        .max()
        .expect("label group is non-empty")
        + CONTEXT_LABEL_GAP_CELLS;
    let offsets = (0..texts.len())
        .map(|index| GridPoint {
            x: 0,
            y: index as i32 * row_height,
        })
        .collect::<Vec<_>>();
    let group_bounds = bounds
        .iter()
        .zip(&offsets)
        .map(|(bounds, offset)| bounds.translated(*offset))
        .reduce(GridRect::union)
        .expect("label group is non-empty");
    let mut packer = GridPacker::for_page(&document.pages[page_index].paper)?;
    occupy_page_items(&mut packer, &document.pages[page_index])?;
    let group_anchor = packer.place(group_bounds);
    Ok(offsets
        .into_iter()
        .map(|offset| offset.translated(group_anchor).to_point())
        .collect())
}

fn estimated_label_width(text: &str) -> f64 {
    let font_height = crate::TextEffects::default().font_size.y.abs();
    text.chars().count().max(1) as f64 * font_height * ESTIMATED_LABEL_WIDTH_EM
}

fn estimated_shaped_label_width(text: &str) -> f64 {
    estimated_label_width(text) + LABEL_SHAPE_LENGTH_MM
}

fn contains_id(document: &SchDocument, id: &str) -> bool {
    document
        .pages
        .iter()
        .any(|page| page.items.iter().any(|item| item.id() == Some(id)))
}

fn available_deterministic_id(document: &SchDocument, key: &str) -> String {
    (0_u64..)
        .map(|index| {
            deterministic_uuid(if index == 0 {
                key.to_string()
            } else {
                format!("{key}:{index}")
            })
        })
        .find(|id| !contains_id(document, id))
        .expect("a finite schematic cannot exhaust deterministic UUIDs")
}

fn upsert_label(document: &mut SchDocument, page_index: usize, mut label: Label) -> Result<()> {
    // Refresh our previously-composed label in place when it still lives on
    // the target page.
    let existing = document.pages[page_index]
        .items
        .iter_mut()
        .find_map(|item| match item {
            SchItem::Label(existing) if existing.id == label.id => Some(existing),
            _ => None,
        });
    if let Some(existing) = existing {
        existing.text = label.text;
        existing.at = label.at;
        existing.kind = label.kind;
        existing.spin = label.spin;
        return Ok(());
    }
    // The deterministic id may belong to an item the editor moved elsewhere
    // (or reused). That item is not ours to relocate — mint a fresh id.
    if contains_id(document, &label.id) {
        label.id = available_deterministic_id(document, &label.id);
    }
    document.pages[page_index].items.push(SchItem::Label(label));
    Ok(())
}

fn upsert_wire(document: &mut SchDocument, page_index: usize, mut wire: Wire) {
    let existing = document.pages[page_index]
        .items
        .iter_mut()
        .find_map(|item| match item {
            SchItem::Wire(existing) if existing.id == wire.id => Some(existing),
            _ => None,
        });
    if let Some(existing) = existing {
        existing.a = wire.a;
        existing.b = wire.b;
        return;
    }
    if contains_id(document, &wire.id) {
        wire.id = available_deterministic_id(document, &wire.id);
    }
    document.pages[page_index].items.push(SchItem::Wire(wire));
}

fn insert_net_symbol(
    document: &mut SchDocument,
    page_index: usize,
    symbol: Symbol,
    definition: &SymbolDefinition,
) -> Result<()> {
    if contains_id(document, &symbol.id) {
        bail!(
            "generated net symbol UUID '{}' is already used by another schematic item",
            symbol.id
        );
    }
    let page = &mut document.pages[page_index];
    match page.library.definitions.get(&definition.lib_id) {
        Some(existing) if existing != definition => bail!(
            "page '{}' already has a different definition for net symbol '{}'",
            page.id,
            definition.lib_id
        ),
        Some(_) => {}
        None => {
            page.library
                .definitions
                .insert(definition.lib_id.clone(), definition.clone());
        }
    }
    page.items.push(SchItem::Symbol(symbol));
    Ok(())
}

fn prune_unused_symbol_definitions(document: &mut SchDocument) {
    for page in &mut document.pages {
        let used = page
            .items
            .iter()
            .filter_map(|item| match item {
                SchItem::Symbol(symbol) => Some(symbol.lib_id.as_str()),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        page.library
            .definitions
            .retain(|lib_id, _| used.contains(lib_id.as_str()));
    }
}

fn resolve_pin_targets(
    placed: &BTreeMap<SymbolSlotKey, PlacedSymbol>,
    component_path: &str,
    pin_name: &str,
    pin_numbers: &BTreeSet<String>,
) -> Result<Vec<PinTarget>> {
    let mut component_placed = false;
    let mut number_matches = Vec::new();
    let mut name_matches = Vec::new();
    for (slot, placed) in placed
        .iter()
        .filter(|(slot, _)| slot.component_path() == component_path)
    {
        component_placed = true;
        let parsed = symbol::ParsedSymbolDefinition::parse(&placed.definition)?;
        for pin in parsed.placed_pins(&placed.symbol)? {
            let matches_name = !pin_name.is_empty() && !pin.name.is_empty() && pin.name == pin_name;
            let matches_number = !pin.numbers.is_disjoint(pin_numbers);
            if matches_name || matches_number {
                let target = PinTarget {
                    page_index: placed.page_index,
                    slot: slot.clone(),
                    symbol_id: placed.symbol.id.clone(),
                    number: pin.number,
                    point: pin.point,
                    spin: pin.outward_spin,
                    hidden: pin.hidden,
                };
                if matches_number {
                    number_matches.push(target);
                } else {
                    name_matches.push(target);
                }
            }
        }
    }
    // One Zener terminal can represent multiple physical pads. Prefer the
    // exact pad numbers and retain every matching KiCad pin in that case;
    // names are a fallback for symbols whose number metadata is unavailable.
    let (matches, exact_numbers) = if number_matches.is_empty() {
        (name_matches, false)
    } else {
        (number_matches, true)
    };
    if matches.is_empty() {
        // A component that is not placed at all contributes no anchors; its
        // absence is already reported as a missing-symbol issue, and partial
        // repairs must stay plannable while other components remain unplaced.
        if !component_placed {
            return Ok(Vec::new());
        }
        bail!(
            "netlist terminal '{}.{}' does not match a KiCad symbol pin",
            component_path,
            pin_name
        );
    }
    let visible = matches
        .iter()
        .filter(|target| !target.hidden)
        .cloned()
        .collect::<Vec<_>>();
    match visible.as_slice() {
        [target] => Ok(vec![target.clone()]),
        [] => Ok(matches),
        _ if exact_numbers => Ok(visible),
        _ => bail!(
            "netlist terminal '{}.{}' matches more than one KiCad symbol pin",
            component_path,
            pin_name
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_facing_net_symbols_make_two_pin_terminal_axis_horizontal() {
        assert_eq!(
            preferred_two_pin_rotation(&[
                (LabelSpin::Up, LabelSpin::Up),
                (LabelSpin::Bottom, LabelSpin::Up),
            ]),
            Rotation::Deg90
        );
        assert_eq!(
            preferred_two_pin_rotation(&[
                (LabelSpin::Left, LabelSpin::Bottom),
                (LabelSpin::Right, LabelSpin::Bottom),
            ]),
            Rotation::Deg0
        );
    }

    fn test_placement_block(key: &str, bounds: GridRect) -> PlacementBlock {
        PlacementBlock {
            key: key.to_string(),
            page_index: 0,
            members: Vec::new(),
            bounds,
            capacitor_bank: None,
        }
    }

    #[test]
    fn placement_block_separates_opposing_asymmetric_envelopes() {
        let slots = (0..4)
            .map(|index| SymbolSlotKey::new(format!("U{index}"), 1).unwrap())
            .collect::<Vec<_>>();
        let bounds_by_slot = [
            GridRect {
                min_x: -1,
                min_y: -1,
                max_x: 8,
                max_y: 8,
            },
            GridRect {
                min_x: -8,
                min_y: -1,
                max_x: 1,
                max_y: 8,
            },
            GridRect {
                min_x: -1,
                min_y: -8,
                max_x: 8,
                max_y: 1,
            },
            GridRect {
                min_x: -8,
                min_y: -8,
                max_x: 1,
                max_y: 1,
            },
        ]
        .into_iter()
        .enumerate()
        .map(|(index, bounds)| (slots[index].clone(), bounds))
        .collect::<BTreeMap<_, _>>();

        let block = placement_block("component".to_string(), 0, slots, &bounds_by_slot, 2);
        let member_bounds = block
            .members
            .iter()
            .map(|member| bounds_by_slot[&member.slot].translated(member.offset))
            .collect::<Vec<_>>();

        assert_eq!(
            member_bounds[1].min_x - member_bounds[0].max_x,
            PLACEMENT_BLOCK_GAP_CELLS
        );
        assert_eq!(
            member_bounds[2].min_y - member_bounds[0].max_y,
            PLACEMENT_BLOCK_GAP_CELLS
        );
    }

    #[test]
    fn placement_grid_aligns_block_origins_in_rows() {
        let blocks = [
            test_placement_block(
                "a",
                GridRect {
                    min_x: -2,
                    min_y: -8,
                    max_x: 3,
                    max_y: 2,
                },
            ),
            test_placement_block(
                "b",
                GridRect {
                    min_x: -4,
                    min_y: -2,
                    max_x: 6,
                    max_y: 7,
                },
            ),
            test_placement_block(
                "c",
                GridRect {
                    min_x: -1,
                    min_y: -3,
                    max_x: 2,
                    max_y: 4,
                },
            ),
            test_placement_block(
                "d",
                GridRect {
                    min_x: -3,
                    min_y: -1,
                    max_x: 5,
                    max_y: 3,
                },
            ),
        ];
        let blocks = blocks.iter().collect::<Vec<_>>();

        let (_, offsets) = placement_grid_with_columns(&blocks, 2);

        assert_eq!(offsets[0].y, offsets[1].y);
        assert_eq!(offsets[2].y, offsets[3].y);
        assert_eq!(offsets[0].x, offsets[2].x);
        assert_ne!(
            offsets[1].x, offsets[3].x,
            "each row should use its members' actual widths"
        );
    }

    #[test]
    fn placement_grid_chooses_a_page_shaped_matrix() {
        let blocks = (0..6)
            .map(|index| {
                test_placement_block(
                    &index.to_string(),
                    GridRect {
                        min_x: -2,
                        min_y: -2,
                        max_x: 2,
                        max_y: 2,
                    },
                )
            })
            .collect::<Vec<_>>();
        let blocks = blocks.iter().collect::<Vec<_>>();
        // A page whose usable area is 200 x 140 cells.
        let packer = GridPacker::for_page(&Paper::Custom {
            width_mm: 267.0,
            height_mm: 191.0,
        })
        .unwrap();
        assert_eq!(packer.usable_bounds().width(), 200);
        assert_eq!(packer.usable_bounds().height(), 140);

        let (_, offsets) = arrange_placement_blocks(&blocks, &packer).unwrap();

        assert_eq!(
            offsets
                .iter()
                .map(|offset| offset.x)
                .collect::<BTreeSet<_>>()
                .len(),
            3
        );
        assert_eq!(
            offsets
                .iter()
                .map(|offset| offset.y)
                .collect::<BTreeSet<_>>()
                .len(),
            2
        );
    }

    #[test]
    fn new_page_grows_to_fit_grid_without_covering_title_block() {
        let default_usable = GridPacker::for_page(&Paper::default())
            .unwrap()
            .usable_bounds();
        let cases = [
            vec![
                test_placement_block(
                    "a",
                    GridRect {
                        min_x: 0,
                        min_y: 0,
                        max_x: 180,
                        max_y: 100,
                    },
                ),
                test_placement_block(
                    "b",
                    GridRect {
                        min_x: 0,
                        min_y: 0,
                        max_x: 180,
                        max_y: 100,
                    },
                ),
            ],
            vec![test_placement_block(
                "full-page",
                GridRect {
                    min_x: 0,
                    min_y: 0,
                    max_x: default_usable.width(),
                    max_y: default_usable.height(),
                },
            )],
        ];

        for blocks in cases {
            let blocks = blocks.iter().collect::<Vec<_>>();
            let mut page = SchPage::new("large");
            let (_, bounds, _) =
                arrange_new_page_blocks(&mut page, &blocks, &BTreeSet::new()).unwrap();

            assert_eq!(
                page.paper,
                Paper::Named {
                    name: "A3".to_string(),
                    portrait: false,
                }
            );
            let packer = GridPacker::for_page(&page.paper).unwrap();
            assert!(packer.can_place_without_overlap(bounds));
        }
    }

    /// A hierarchy sheet covering most of an A4 page, as a page keeps
    /// between incremental applies.
    fn test_page_with_sheet(id: &str) -> SchPage {
        let mut page = SchPage::new(id);
        page.items.push(SchItem::Sheet(Box::new(Sheet {
            id: format!("{id}-sheet"),
            at: Some(Point::new(10.0, 10.0)),
            size: Some(Point::new(200.0, 180.0)),
            name: None,
            file: SymbolField::new("Sheetfile", "child.kicad_sch", Point::default()),
            pins: Vec::new(),
            unsupported: Vec::new(),
        })));
        page
    }

    #[test]
    fn new_page_fit_counts_preserved_items() {
        let block = test_placement_block(
            "a",
            GridRect {
                min_x: 0,
                min_y: 0,
                max_x: 100,
                max_y: 60,
            },
        );
        let blocks = vec![&block];

        let mut empty = SchPage::new("empty");
        arrange_new_page_blocks(&mut empty, &blocks, &BTreeSet::new()).unwrap();
        assert_eq!(empty.paper, Paper::default());

        let mut occupied = test_page_with_sheet("occupied");
        let (packer, bounds, _) =
            arrange_new_page_blocks(&mut occupied, &blocks, &BTreeSet::new()).unwrap();
        assert_eq!(
            occupied.paper,
            Paper::Named {
                name: "A3".to_string(),
                portrait: false,
            }
        );
        assert!(packer.can_place_without_overlap(bounds));
    }

    #[test]
    fn fit_tries_every_grid_shape_around_preserved_items() {
        // Two 50-cell blocks: side by side they are 104 cells wide and cannot
        // pass the sheet on an A4 page, stacked they fit the strip beside it.
        let blocks = ["a", "b"].map(|key| {
            test_placement_block(
                key,
                GridRect {
                    min_x: 0,
                    min_y: 0,
                    max_x: 50,
                    max_y: 50,
                },
            )
        });
        let blocks = blocks.iter().collect::<Vec<_>>();

        let (packer, bounds, offsets) = arrange_existing_page_blocks(
            &test_page_with_sheet("occupied"),
            &blocks,
            &BTreeSet::new(),
        )
        .unwrap();
        assert!(packer.can_place_without_overlap(bounds));
        assert_eq!(offsets[0].x, offsets[1].x);
        assert_ne!(offsets[0].y, offsets[1].y);

        let mut page = test_page_with_sheet("new");
        arrange_new_page_blocks(&mut page, &blocks, &BTreeSet::new()).unwrap();
        assert_eq!(page.paper, Paper::default());
    }

    #[test]
    fn existing_page_fit_counts_preserved_items() {
        let block = test_placement_block(
            "a",
            GridRect {
                min_x: 0,
                min_y: 0,
                max_x: 100,
                max_y: 60,
            },
        );
        let blocks = vec![&block];

        assert!(
            arrange_existing_page_blocks(&SchPage::new("empty"), &blocks, &BTreeSet::new()).is_ok()
        );
        let page = test_page_with_sheet("occupied");
        let (mut packer, bounds, _) =
            arrange_existing_page_blocks(&page, &blocks, &BTreeSet::new()).unwrap();
        assert!(!packer.can_place_without_overlap(bounds));
        let placed = bounds.translated(packer.place_anchored(bounds));
        assert!(placed.min_x > packer.usable_bounds().max_x);
        assert_eq!(page.paper, Paper::default());
    }

    #[test]
    fn shallow_page_arc_does_not_force_overflow() {
        let mut page = SchPage::new("shallow-arc");
        page.items.push(SchItem::Unsupported(
            pcb_sexpr::parse("(arc (start 10 10) (mid 20 9.9) (end 30 10))").unwrap(),
        ));
        let block = test_placement_block(
            "new",
            GridRect {
                min_x: 0,
                min_y: 0,
                max_x: 100,
                max_y: 60,
            },
        );
        let (mut packer, bounds, _) =
            arrange_existing_page_blocks(&page, &[&block], &BTreeSet::new()).unwrap();
        assert!(packer.can_place_without_overlap(bounds));
        let placed = bounds.translated(packer.place_anchored(bounds));
        let usable = packer.usable_bounds();
        assert!(placed.min_x >= usable.min_x && placed.max_x <= usable.max_x);
        assert!(placed.min_y >= usable.min_y && placed.max_y <= usable.max_y);
    }

    #[test]
    fn overflow_avoids_opaque_page_text_and_graphics() {
        for (source, right_edge) in [
            (
                r#"(text "off-page note\nsecond line" (at 500 10 90)
                (effects (font (size 2 2)) (justify left bottom)))"#,
                532.0,
            ),
            ("(text_box \"note\" (at 500 10 90) (size 100 20))", 620.0),
            (
                "(rectangle (start 500 10) (end 600 100) (stroke (width 10)))",
                605.0,
            ),
            ("(polyline (pts (xy 500 10) (xy 600 100)))", 600.0),
            ("(bezier (pts (xy 500 10) (xy 700 20) (xy 600 100)))", 700.0),
            ("(circle (center 500 10) (radius 50))", 550.0),
            // A major arc whose rightmost point is not a control point.
            ("(arc (start 470 40) (mid 540 30) (end 470 -40))", 550.0),
        ] {
            let mut page = test_page_with_sheet("opaque");
            let opaque = SchItem::Unsupported(pcb_sexpr::parse(source).unwrap());
            page.items.push(opaque.clone());
            let block = test_placement_block(
                "new",
                GridRect {
                    min_x: 0,
                    min_y: 0,
                    max_x: 100,
                    max_y: 60,
                },
            );
            let (mut packer, bounds, _) =
                arrange_existing_page_blocks(&page, &[&block], &BTreeSet::new()).unwrap();
            let placed = bounds.translated(packer.place_anchored(bounds));
            assert!(
                f64::from(placed.min_x) * CONNECTION_GRID_MM > right_edge,
                "{source}"
            );
            assert_eq!(page.items.last(), Some(&opaque));
        }
    }

    #[test]
    fn oversized_batches_spill_outside_new_and_existing_pages() {
        let block = test_placement_block(
            "oversized",
            GridRect {
                min_x: -10,
                min_y: -20,
                max_x: 2000,
                max_y: 2000,
            },
        );
        for paper in [
            Paper::default(),
            Paper::Custom {
                width_mm: 5.0,
                height_mm: 5.0,
            },
        ] {
            let mut page = SchPage::new("overflow");
            page.paper = paper.clone();
            for new_page in [false, true] {
                let (mut packer, bounds, offsets) = if new_page {
                    arrange_new_page_blocks(&mut page, &[&block], &BTreeSet::new())
                } else {
                    arrange_existing_page_blocks(&page, &[&block], &BTreeSet::new())
                }
                .unwrap();
                let placed = bounds.translated(packer.place_anchored(bounds));
                assert!(placed.min_x > packer.usable_bounds().max_x);
                assert_eq!(offsets.len(), 1);
                assert_eq!(page.paper, paper);
            }
        }
    }

    fn multi_pad_symbol() -> BTreeMap<SymbolSlotKey, PlacedSymbol> {
        let definition = SymbolDefinition::from_kicad_symbol_sexpr(
            r#"(symbol "Test:MultiPad"
              (symbol "MultiPad_1_1"
                (pin input line (at -2.54 2.54 0) (length 2.54)
                  (name "A") (number "1"))
                (pin input line (at -2.54 -2.54 0) (length 2.54)
                  (name "A") (number "3"))
                (pin input line (at 2.54 0 180) (length 2.54)
                  (name "B") (number "4"))
                (pin input line (at 2.54 -1.27 180) (length 2.54)
                  (name "X") (number "6"))
                (pin input line (at 2.54 -2.54 180) (length 2.54)
                  (name "C") (number "5"))))"#,
        )
        .unwrap();
        let slot = SymbolSlotKey::new("MQ-7.MQ-7", 1).unwrap();
        let symbol = Symbol {
            id: slot.symbol_id(),
            lib_id: definition.lib_id.clone(),
            unit: 1,
            body_style: 1,
            at: Point::default(),
            rotation: Rotation::default(),
            mirror: None,
            dnp: false,
            in_bom: true,
            on_board: true,
            in_pos_files: true,
            fields_autoplaced: true,
            fields: BTreeMap::new(),
            pins: Vec::new(),
            unsupported: Vec::new(),
        };
        BTreeMap::from([(
            slot,
            PlacedSymbol {
                page_index: 0,
                symbol,
                definition,
            },
        )])
    }

    fn ground_net_symbol_spec() -> net_symbols::NetSymbolSpec {
        net_symbols::NetSymbolSpec {
            definition: SymbolDefinition::from_kicad_symbol_sexpr(
                r#"(symbol "power:GND"
                  (power)
                  (symbol "GND_1_1"
                    (pin power_in line (at 0 0 270) (length 2.54)
                      (name "GND") (number "1"))))"#,
            )
            .unwrap(),
            unit: 1,
            pin_offset: Point::default(),
            pin_outward_spin: LabelSpin::Up,
        }
    }

    #[test]
    fn logical_terminal_resolves_all_of_its_physical_pins() {
        let targets = resolve_pin_targets(
            &multi_pad_symbol(),
            "MQ-7.MQ-7",
            "A",
            &BTreeSet::from(["1".to_string(), "3".to_string()]),
        )
        .unwrap();

        assert_eq!(
            targets
                .into_iter()
                .map(|target| target.number)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["1".to_string(), "3".to_string()])
        );
    }

    #[test]
    fn pin_numbers_take_precedence_over_a_repeated_pin_name() {
        let targets = resolve_pin_targets(
            &multi_pad_symbol(),
            "MQ-7.MQ-7",
            "A",
            &BTreeSet::from(["1".to_string()]),
        )
        .unwrap();

        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].number, "1");
    }

    #[test]
    fn right_side_net_symbols_stair_outward_and_below() {
        let placed = multi_pad_symbol();
        let target = resolve_pin_targets(
            &placed,
            "MQ-7.MQ-7",
            "B",
            &BTreeSet::from(["4".to_string()]),
        )
        .unwrap()
        .remove(0);
        let item = &placed[&target.slot];

        let first = net_symbol_connection_point(item, &target, LabelSpin::Up, 0).unwrap();
        let second = net_symbol_connection_point(item, &target, LabelSpin::Up, 1).unwrap();
        let bounds = field_autoplace::symbol_geometry_bounds(&item.symbol, &item.definition)
            .unwrap()
            .unwrap();

        assert!(
            (second.x - first.x - NET_SYMBOL_STAIR_CELLS * CONNECTION_GRID_MM).abs()
                < GEOMETRY_EPS_MM
        );
        assert!(
            (second.y - first.y - NET_SYMBOL_STAIR_CELLS * CONNECTION_GRID_MM).abs()
                < GEOMETRY_EPS_MM
        );
        assert!((first.x - bounds.max_x - 4.0 * CONNECTION_GRID_MM).abs() < GEOMETRY_EPS_MM);
        assert!(first.x > target.point.x);
        assert!(first.y > target.point.y);
    }

    #[test]
    fn component_envelope_reserves_net_symbol_stairs_and_wires() {
        let placed = multi_pad_symbol();
        let item = placed.values().next().unwrap();
        let first = resolve_pin_targets(
            &placed,
            "MQ-7.MQ-7",
            "B",
            &BTreeSet::from(["4".to_string()]),
        )
        .unwrap()
        .remove(0);
        let second = resolve_pin_targets(
            &placed,
            "MQ-7.MQ-7",
            "C",
            &BTreeSet::from(["5".to_string()]),
        )
        .unwrap()
        .remove(0);
        let spec = ground_net_symbol_spec();
        let targets = BTreeMap::from([
            ("FIRST".to_string(), vec![first.clone()]),
            ("SECOND".to_string(), vec![second.clone()]),
        ]);
        let specs = BTreeMap::from([
            ("FIRST".to_string(), spec.clone()),
            ("SECOND".to_string(), spec.clone()),
        ]);
        let runs = vec![
            NetSymbolRun {
                net_name: "FIRST".to_string(),
                page_index: 0,
                slot: first.slot.clone(),
                targets: vec![first.clone()],
                stair_index: 0,
            },
            NetSymbolRun {
                net_name: "SECOND".to_string(),
                page_index: 0,
                slot: second.slot.clone(),
                targets: vec![second.clone()],
                stair_index: 1,
            },
        ];

        let envelope = component_envelope(item, &targets, &specs, &runs).unwrap();
        let connection = net_symbol_connection_point(item, &second, LabelSpin::Up, 1).unwrap();
        let bend = net_symbol_wire_bend(second.point, connection, second.spin);
        let symbol = build_net_symbol(&spec, "SECOND", String::new(), connection).unwrap();
        let symbol_bounds = field_autoplace::symbol_visual_bounds(&symbol, &spec.definition)
            .unwrap()
            .unwrap();

        assert!(envelope.min_x <= symbol_bounds.min_x);
        assert!(envelope.min_y <= symbol_bounds.min_y);
        assert!(envelope.max_x >= symbol_bounds.max_x);
        assert!(envelope.max_y >= symbol_bounds.max_y);
        assert!(envelope.min_x <= bend.x && bend.x <= envelope.max_x);
        assert!(envelope.min_y <= bend.y && bend.y <= envelope.max_y);
        assert!(envelope.min_x <= connection.x && connection.x <= envelope.max_x);
        assert!(envelope.min_y <= connection.y && connection.y <= envelope.max_y);
    }

    #[test]
    fn net_symbols_share_only_consecutive_visible_pins() {
        let placed = multi_pad_symbol();
        let item = placed.values().next().unwrap();
        let target = |name: &str, number: &str| {
            resolve_pin_targets(
                &placed,
                "MQ-7.MQ-7",
                name,
                &BTreeSet::from([number.to_string()]),
            )
            .unwrap()
            .remove(0)
        };
        let first = target("B", "4");
        let intervening = target("X", "6");
        let last = target("C", "5");

        let split = adjacent_target_runs(item, vec![&first, &last], |target| *target).unwrap();
        assert_eq!(split.len(), 2);

        let shared =
            adjacent_target_runs(item, vec![&first, &intervening, &last], |target| *target)
                .unwrap();
        assert_eq!(shared.len(), 1);
        assert_eq!(shared[0].len(), 3);
    }

    #[test]
    fn upright_side_net_symbols_stair_uniquely_across_pin_banks() {
        // B/4 and C/5 are separate right-side banks (X/6 sits between them).
        // An upright symbol lands at the body corner whatever the pin, so
        // both banks at stair 0 would stack two power nets on one point.
        let placed = multi_pad_symbol();
        let slot = placed.keys().next().unwrap().clone();
        let item = placed.values().next().unwrap();
        let target = |name: &str, number: &str| {
            resolve_pin_targets(
                &placed,
                "MQ-7.MQ-7",
                name,
                &BTreeSet::from([number.to_string()]),
            )
            .unwrap()
            .remove(0)
        };
        let spec = ground_net_symbol_spec();
        let targets = BTreeMap::from([
            ("FIRST".to_string(), vec![target("B", "4")]),
            ("LAST".to_string(), vec![target("C", "5")]),
        ]);
        let specs = BTreeMap::from([
            ("FIRST".to_string(), spec.clone()),
            ("LAST".to_string(), spec.clone()),
        ]);

        let runs =
            plan_projected_net_symbol_runs(&targets, &BTreeSet::from([slot]), &specs, &placed)
                .unwrap();

        assert_eq!(runs.len(), 2);
        assert_eq!(
            runs.iter()
                .map(|run| run.stair_index)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([0, 1])
        );
        let points = runs
            .iter()
            .map(|run| {
                net_symbol_connection_point(
                    item,
                    &run.placement_target(),
                    spec.pin_outward_spin,
                    run.stair_index,
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        assert!(!points_coincide(points[0], points[1]));
    }

    #[test]
    fn sideways_side_net_symbols_restart_stairs_per_pin_bank() {
        // A sideways symbol follows its pin row, so separate banks start at
        // stair 0 each and still land apart.
        let placed = multi_pad_symbol();
        let slot = placed.keys().next().unwrap().clone();
        let item = placed.values().next().unwrap();
        let target = |name: &str, number: &str| {
            resolve_pin_targets(
                &placed,
                "MQ-7.MQ-7",
                name,
                &BTreeSet::from([number.to_string()]),
            )
            .unwrap()
            .remove(0)
        };
        let mut spec = ground_net_symbol_spec();
        spec.pin_outward_spin = LabelSpin::Right;
        let targets = BTreeMap::from([
            ("FIRST".to_string(), vec![target("B", "4")]),
            ("LAST".to_string(), vec![target("C", "5")]),
        ]);
        let specs = BTreeMap::from([
            ("FIRST".to_string(), spec.clone()),
            ("LAST".to_string(), spec.clone()),
        ]);

        let runs =
            plan_projected_net_symbol_runs(&targets, &BTreeSet::from([slot]), &specs, &placed)
                .unwrap();

        assert_eq!(runs.len(), 2);
        assert!(runs.iter().all(|run| run.stair_index == 0));
        let points = runs
            .iter()
            .map(|run| {
                net_symbol_connection_point(
                    item,
                    &run.placement_target(),
                    spec.pin_outward_spin,
                    run.stair_index,
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        assert!(!points_coincide(points[0], points[1]));
    }

    #[test]
    fn stub_collisions_ignore_same_net_pins_on_other_pages() {
        let mut placed = multi_pad_symbol();
        let first_slot = placed.keys().next().unwrap().clone();
        let first_symbol = placed[&first_slot].symbol.clone();
        let definition = placed[&first_slot].definition.clone();
        let target = resolve_pin_targets(
            &placed,
            "MQ-7.MQ-7",
            "B",
            &BTreeSet::from(["4".to_string()]),
        )
        .unwrap()
        .remove(0);
        let spec = ground_net_symbol_spec();
        let crowded =
            net_symbol_connection_point(&placed[&first_slot], &target, spec.pin_outward_spin, 0)
                .unwrap();

        // A same-net symbol on another page whose pin B/4 happens to sit at
        // the first symbol's stair-0 point.
        let second_slot = SymbolSlotKey::new("MQ-8.MQ-8", 1).unwrap();
        let mut second_symbol = first_symbol.clone();
        second_symbol.id = second_slot.symbol_id();
        second_symbol.at = Point::new(crowded.x - target.point.x, crowded.y - target.point.y);
        placed.insert(
            second_slot,
            PlacedSymbol {
                page_index: 1,
                symbol: second_symbol.clone(),
                definition: definition.clone(),
            },
        );
        let other = resolve_pin_targets(
            &placed,
            "MQ-8.MQ-8",
            "B",
            &BTreeSet::from(["4".to_string()]),
        )
        .unwrap()
        .remove(0);
        assert!(points_coincide(other.point, crowded));

        let mut page = SchPage::new("page");
        page.library
            .definitions
            .insert(definition.lib_id.clone(), definition.clone());
        page.items.push(SchItem::Symbol(first_symbol));
        // Foreign wire ending exactly on this page's stair-0 point.
        page.items.push(SchItem::Wire(Wire {
            id: "foreign".to_string(),
            a: crowded,
            b: Point::new(crowded.x, crowded.y + 2.54),
            unsupported: Vec::new(),
        }));
        let mut other_page = SchPage::new("other");
        other_page
            .library
            .definitions
            .insert(definition.lib_id.clone(), definition);
        other_page.items.push(SchItem::Symbol(second_symbol));
        let mut document = SchDocument {
            pages: vec![page, other_page],
            root_page_ids: vec!["page".to_string(), "other".to_string()],
        };
        let targets_by_net = BTreeMap::from([("GND".to_string(), vec![target, other])]);
        let specs = BTreeMap::from([("GND".to_string(), spec)]);

        sync_net_drivers(
            &mut document,
            &targets_by_net,
            &placed,
            &specs,
            &BTreeMap::new(),
        )
        .unwrap();

        // This page's net symbol stepped past the foreign wire endpoint, and
        // none of its stubs end on it.
        let symbols = document.pages[0]
            .items
            .iter()
            .filter_map(|item| match item {
                SchItem::Symbol(symbol) if symbol.lib_id == "power:GND" => Some(symbol.at),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(symbols.len(), 1);
        assert!(!points_coincide(symbols[0], crowded), "{:?}", symbols[0]);
        assert!(document.pages[0].items.iter().all(|item| !matches!(
            item,
            SchItem::Wire(wire)
                if wire.id != "foreign"
                    && (points_coincide(wire.a, crowded) || points_coincide(wire.b, crowded))
        )));
    }

    #[test]
    fn crowded_net_symbol_falls_back_to_a_label_in_the_symbol_scope() {
        let placed = multi_pad_symbol();
        let item = placed.values().next().unwrap();
        let target = resolve_pin_targets(
            &placed,
            "MQ-7.MQ-7",
            "B",
            &BTreeSet::from(["4".to_string()]),
        )
        .unwrap()
        .remove(0);
        let spec = ground_net_symbol_spec();
        let mut page = SchPage::new("page");
        page.library
            .definitions
            .insert(item.definition.lib_id.clone(), item.definition.clone());
        page.items.push(SchItem::Symbol(item.symbol.clone()));
        // Foreign wires end on every stair candidate the symbol could take.
        for extra in 0..NET_SYMBOL_STAIR_RETRIES {
            let candidate =
                net_symbol_connection_point(item, &target, spec.pin_outward_spin, extra).unwrap();
            page.items.push(SchItem::Wire(Wire {
                id: format!("blocker-{extra}"),
                a: candidate,
                b: Point::new(candidate.x, candidate.y + 2.54),
                unsupported: Vec::new(),
            }));
        }
        let mut document = SchDocument {
            pages: vec![page],
            root_page_ids: vec!["page".to_string()],
        };
        let targets_by_net = BTreeMap::from([("GND".to_string(), vec![target.clone()])]);
        let specs = BTreeMap::from([("GND".to_string(), spec)]);

        sync_net_drivers(
            &mut document,
            &targets_by_net,
            &placed,
            &specs,
            &BTreeMap::new(),
        )
        .unwrap();

        let labels = document.pages[0]
            .items
            .iter()
            .filter_map(|item| match item {
                SchItem::Label(label) => Some(label),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(labels.len(), 1);
        assert_eq!(labels[0].text, "GND");
        assert!(points_coincide(labels[0].at, target.point));
        // Power symbols merge globally, so the stand-in label must too.
        assert!(
            matches!(labels[0].kind, LabelKind::Global { .. }),
            "{:?}",
            labels[0].kind
        );
        assert!(
            document.pages[0].items.iter().all(
                |item| !matches!(item, SchItem::Symbol(symbol) if symbol.lib_id == "power:GND")
            )
        );
    }

    #[test]
    fn opposite_net_symbols_share_stair_indices_on_one_pin_bank() {
        let placed = multi_pad_symbol();
        let slot = placed.keys().next().unwrap().clone();
        let target = |name: &str, number: &str| {
            resolve_pin_targets(
                &placed,
                "MQ-7.MQ-7",
                name,
                &BTreeSet::from([number.to_string()]),
            )
            .unwrap()
            .remove(0)
        };
        let upper = target("B", "4");
        let lower = target("X", "6");
        let ground = ground_net_symbol_spec();
        let mut power = ground.clone();
        power.pin_outward_spin = LabelSpin::Bottom;
        let targets = BTreeMap::from([
            ("GROUND".to_string(), vec![lower]),
            ("POWER".to_string(), vec![upper]),
        ]);
        let specs = BTreeMap::from([("GROUND".to_string(), ground), ("POWER".to_string(), power)]);

        let runs =
            plan_projected_net_symbol_runs(&targets, &BTreeSet::from([slot]), &specs, &placed)
                .unwrap();

        assert_eq!(runs.len(), 2);
        assert_eq!(
            runs.iter()
                .map(|run| run.stair_index)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([0, 1])
        );
    }

    #[test]
    fn vertical_pin_routes_escape_before_stairing_sideways() {
        let target = Point::new(10.0, 10.0);
        let connection = Point::new(15.0, 0.0);

        assert_eq!(
            net_symbol_wire_bend(target, connection, LabelSpin::Up),
            Point::new(10.0, 0.0)
        );
    }

    #[test]
    fn projected_net_symbol_runs_keep_coincident_components_distinct() {
        let mut placed = multi_pad_symbol();
        let first = resolve_pin_targets(
            &placed,
            "MQ-7.MQ-7",
            "B",
            &BTreeSet::from(["4".to_string()]),
        )
        .unwrap()
        .remove(0);
        let second_slot = SymbolSlotKey::new("OTHER", 1).unwrap();
        let mut second_item = placed[&first.slot].clone();
        second_item.symbol.id = second_slot.symbol_id();
        placed.insert(second_slot.clone(), second_item);
        let mut second = first.clone();
        second.slot = second_slot.clone();
        second.symbol_id = second_slot.symbol_id();

        let specs = BTreeMap::from([("GROUND".to_string(), ground_net_symbol_spec())]);
        let targets = BTreeMap::from([("GROUND".to_string(), vec![first.clone(), second])]);
        let projected = BTreeSet::from([first.slot, second_slot]);

        let runs = plan_projected_net_symbol_runs(&targets, &projected, &specs, &placed).unwrap();

        assert_eq!(runs.len(), 2);
        assert_eq!(
            runs.into_iter()
                .map(|run| run.slot)
                .collect::<BTreeSet<_>>(),
            projected
        );
    }

    #[test]
    fn unplaced_component_terminals_are_skipped_not_errors() {
        // Partial repairs must stay plannable while other components remain
        // unplaced; their absence is reported as a missing-symbol issue.
        let placed = BTreeMap::new();
        let targets =
            resolve_pin_targets(&placed, "R_EN.R", "2", &BTreeSet::from(["2".to_string()]))
                .expect("unplaced component contributes no anchors");
        assert!(targets.is_empty());
    }
}
