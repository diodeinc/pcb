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
    repair::{ConnectivityRepairPlan, plan_connectivity_repair, remove_items},
    root_interface, root_page_id, symbol,
};

const DEFAULT_TITLE_BLOCK_WIDTH_MM: f64 = 110.0;
const DEFAULT_TITLE_BLOCK_HEIGHT_MM: f64 = 34.0;
const PACKING_MARGIN_CELLS: i32 = 5;
const PACKING_CLEARANCE_CELLS: i32 = 2;
const LABEL_SHAPE_LENGTH_MM: f64 = 2.54;
const NET_SYMBOL_OFFSET_CELLS: f64 = 4.0;
const NET_SYMBOL_STAIR_CELLS: f64 = 2.0;
const ESTIMATED_LABEL_WIDTH_EM: f64 = 0.8;
const SHEET_PIN_SPACING_MM: f64 = 5.08;
const SHEET_MIN_WIDTH_MM: f64 = 50.8;
const SHEET_MIN_HEIGHT_MM: f64 = 20.32;

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
    let instances = component_instances(netlist)?;
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
        let mut plan = plan_connectivity_repair(&document, netlist, &current, &repair_keys)?;
        if creating {
            plan.reconnect_nets
                .extend(named_connected_nets(netlist).map(|net| net.name.clone()));
        }
        apply_connectivity_repair(
            &mut document,
            netlist,
            &mut placed,
            &net_symbol_specs,
            default_page,
            plan,
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
        fields_autoplaced: false,
        fields: BTreeMap::new(),
        pins: Vec::new(),
        unsupported: Vec::new(),
    };
    let pins = symbol::ParsedSymbolDefinition::parse(definition)?.placed_pins(&unplaced)?;
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

    let rotations = [
        Rotation::Deg0,
        Rotation::Deg90,
        Rotation::Deg180,
        Rotation::Deg270,
    ];
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
    Ok(if best.next().is_none() {
        rotation
    } else {
        Rotation::default()
    })
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

fn component_instances(netlist: &Schematic) -> Result<BTreeMap<String, &Instance>> {
    let mut result = BTreeMap::new();
    for (instance_ref, instance) in &netlist.instances {
        if instance.kind != InstanceKind::Component {
            continue;
        }
        let path = crate::canonical_component_path(&instance_ref.instance_path)
            .context("component instance has no canonical path")?;
        if result.insert(path.clone(), instance).is_some() {
            bail!("netlist contains duplicate component path '{path}'");
        }
    }
    Ok(result)
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
        let height = SHEET_MIN_HEIGHT_MM.max((ports.len() as f64 + 1.0) * SHEET_PIN_SPACING_MM);
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
                at: Point::new(at.x, at.y + (index as f64 + 1.0) * SHEET_PIN_SPACING_MM),
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
        let mut packer = GridPacker::for_page(&document.pages[page_index].paper)?;
        let relocatable_ids = relocatable
            .iter()
            .map(|slot| slot.symbol_id())
            .collect::<BTreeSet<_>>();
        occupy_page_items_except(&mut packer, &document.pages[page_index], &relocatable_ids)?;

        let mut items = relocatable
            .into_iter()
            .map(|slot| {
                let bounds = GridRect::from_bounds(component_envelope(
                    &placed[&slot],
                    &targets,
                    net_symbol_specs,
                    &net_symbol_runs,
                )?);
                Ok((slot, bounds))
            })
            .collect::<Result<Vec<_>>>()?;
        items.sort_by(|(left_slot, left), (right_slot, right)| {
            right
                .area()
                .cmp(&left.area())
                .then_with(|| left_slot.cmp(right_slot))
        });
        for (slot, relative_bounds) in items {
            let anchor = packer.place(relative_bounds);
            move_placed_symbol(document, placed, &slot, anchor.to_point())?;
        }
    }
    Ok(())
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
                    packer.occupy(GridRect::from_bounds(bounds));
                }
            }
            SchItem::Sheet(sheet) => {
                if let Some((min, max)) = sheet.bounds() {
                    let bounds = field_autoplace::Bounds::from_points([min, max])
                        .expect("sheet bounds have two corners");
                    packer.occupy(GridRect::from_bounds(bounds));
                }
            }
            SchItem::Label(label) => packer.occupy(point_rect(label.at)),
            SchItem::Wire(wire) => {
                let bounds = field_autoplace::Bounds::from_points([wire.a, wire.b])
                    .expect("wire has two endpoints");
                packer.occupy(GridRect::from_bounds(bounds));
            }
            SchItem::Junction(junction) => packer.occupy(point_rect(junction.at)),
            SchItem::NoConnect(no_connect) => packer.occupy(point_rect(no_connect.at)),
            SchItem::Symbol(_) | SchItem::Unsupported(_) => {}
        }
    }
    Ok(())
}

fn point_rect(point: Point) -> GridRect {
    GridRect {
        min_x: grid_floor(point.x),
        min_y: grid_floor(point.y),
        max_x: grid_ceil(point.x).max(grid_floor(point.x) + 1),
        max_y: grid_ceil(point.y).max(grid_floor(point.y) + 1),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GridPoint {
    x: i32,
    y: i32,
}

impl GridPoint {
    fn to_point(self) -> Point {
        Point::new(
            self.x as f64 * CONNECTION_GRID_MM,
            self.y as f64 * CONNECTION_GRID_MM,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GridRect {
    min_x: i32,
    min_y: i32,
    max_x: i32,
    max_y: i32,
}

impl GridRect {
    fn from_bounds(bounds: field_autoplace::Bounds) -> Self {
        let min_x = grid_floor(bounds.min_x);
        let min_y = grid_floor(bounds.min_y);
        Self {
            min_x,
            min_y,
            max_x: grid_ceil(bounds.max_x).max(min_x + 1),
            max_y: grid_ceil(bounds.max_y).max(min_y + 1),
        }
    }

    fn translated(self, point: GridPoint) -> Self {
        Self {
            min_x: self.min_x + point.x,
            min_y: self.min_y + point.y,
            max_x: self.max_x + point.x,
            max_y: self.max_y + point.y,
        }
    }

    fn expanded(self, amount: i32) -> Self {
        Self {
            min_x: self.min_x - amount,
            min_y: self.min_y - amount,
            max_x: self.max_x + amount,
            max_y: self.max_y + amount,
        }
    }

    fn area(self) -> i64 {
        i64::from(self.max_x - self.min_x) * i64::from(self.max_y - self.min_y)
    }
}

struct GridPacker {
    usable: GridRect,
    width: usize,
    occupied: Vec<bool>,
}

impl GridPacker {
    fn for_page(paper: &Paper) -> Result<Self> {
        let (width_mm, height_mm) = paper_dimensions(paper)?;
        let usable = GridRect {
            min_x: PACKING_MARGIN_CELLS,
            min_y: PACKING_MARGIN_CELLS,
            max_x: grid_floor(width_mm) - PACKING_MARGIN_CELLS,
            max_y: grid_floor(height_mm) - PACKING_MARGIN_CELLS,
        };
        if usable.max_x <= usable.min_x || usable.max_y <= usable.min_y {
            bail!("schematic page is too small for automatic placement");
        }
        let width = (usable.max_x - usable.min_x) as usize;
        let height = (usable.max_y - usable.min_y) as usize;
        let mut packer = Self {
            usable,
            width,
            occupied: vec![false; width * height],
        };
        packer.occupy(GridRect::from_bounds(
            field_autoplace::Bounds::from_points([
                Point::new(
                    width_mm - DEFAULT_TITLE_BLOCK_WIDTH_MM,
                    height_mm - DEFAULT_TITLE_BLOCK_HEIGHT_MM,
                ),
                Point::new(width_mm, height_mm),
            ])
            .expect("title block has two corners"),
        ));
        Ok(packer)
    }

    fn occupy(&mut self, rect: GridRect) {
        let rect = rect.expanded(PACKING_CLEARANCE_CELLS);
        let min_x = rect.min_x.max(self.usable.min_x);
        let min_y = rect.min_y.max(self.usable.min_y);
        let max_x = rect.max_x.min(self.usable.max_x);
        let max_y = rect.max_y.min(self.usable.max_y);
        for y in min_y..max_y {
            for x in min_x..max_x {
                let index = self.index(x, y);
                self.occupied[index] = true;
            }
        }
    }

    fn place(&mut self, relative: GridRect) -> GridPoint {
        let min_anchor_x = self.usable.min_x - relative.min_x;
        let min_anchor_y = self.usable.min_y - relative.min_y;
        let max_anchor_x = self.usable.max_x - relative.max_x;
        let max_anchor_y = self.usable.max_y - relative.max_y;
        if max_anchor_x < min_anchor_x || max_anchor_y < min_anchor_y {
            let anchor = self.centered_anchor(relative);
            self.occupy(relative.translated(anchor));
            return anchor;
        }
        let occupied = self.occupancy_prefix();
        let mut best = None;
        for y in min_anchor_y..=max_anchor_y {
            for x in min_anchor_x..=max_anchor_x {
                let anchor = GridPoint { x, y };
                let candidate = relative.translated(anchor);
                let overlap = self.occupied_cells(candidate, &occupied);
                let rank = (overlap, self.distance_from_center_squared(candidate), y, x);
                if best.is_none_or(|(best_rank, _)| rank < best_rank) {
                    best = Some((rank, anchor));
                }
            }
        }
        let anchor = best.expect("a non-empty anchor range has a candidate").1;
        self.occupy(relative.translated(anchor));
        anchor
    }

    fn centered_anchor(&self, relative: GridRect) -> GridPoint {
        let x2 = i64::from(self.usable.min_x) + i64::from(self.usable.max_x)
            - i64::from(relative.min_x)
            - i64::from(relative.max_x);
        let y2 = i64::from(self.usable.min_y) + i64::from(self.usable.max_y)
            - i64::from(relative.min_y)
            - i64::from(relative.max_y);
        GridPoint {
            x: x2.div_euclid(2) as i32,
            y: y2.div_euclid(2) as i32,
        }
    }

    fn distance_from_center_squared(&self, rect: GridRect) -> i64 {
        let dx = i64::from(rect.min_x) + i64::from(rect.max_x)
            - i64::from(self.usable.min_x)
            - i64::from(self.usable.max_x);
        let dy = i64::from(rect.min_y) + i64::from(rect.max_y)
            - i64::from(self.usable.min_y)
            - i64::from(self.usable.max_y);
        dx * dx + dy * dy
    }

    fn occupancy_prefix(&self) -> Vec<u32> {
        let height = self.occupied.len() / self.width;
        let stride = self.width + 1;
        let mut prefix = vec![0; stride * (height + 1)];
        for y in 0..height {
            let mut row = 0;
            for x in 0..self.width {
                row += u32::from(self.occupied[y * self.width + x]);
                prefix[(y + 1) * stride + x + 1] = prefix[y * stride + x + 1] + row;
            }
        }
        prefix
    }

    fn occupied_cells(&self, rect: GridRect, prefix: &[u32]) -> u32 {
        let x0 = (rect.min_x - self.usable.min_x) as usize;
        let y0 = (rect.min_y - self.usable.min_y) as usize;
        let x1 = (rect.max_x - self.usable.min_x) as usize;
        let y1 = (rect.max_y - self.usable.min_y) as usize;
        let stride = self.width + 1;
        prefix[y1 * stride + x1] + prefix[y0 * stride + x0]
            - prefix[y0 * stride + x1]
            - prefix[y1 * stride + x0]
    }

    fn index(&self, x: i32, y: i32) -> usize {
        (y - self.usable.min_y) as usize * self.width + (x - self.usable.min_x) as usize
    }
}

fn grid_floor(value: f64) -> i32 {
    ((value + GEOMETRY_EPS_MM) / CONNECTION_GRID_MM).floor() as i32
}

fn grid_ceil(value: f64) -> i32 {
    ((value - GEOMETRY_EPS_MM) / CONNECTION_GRID_MM).ceil() as i32
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
                let bend = net_symbol_wire_bend(target.point, connection, spec.pin_outward_spin);
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
    let text_height = crate::TextEffects::default().font_size.y.abs();
    let length = estimated_label_width(net_name);
    let half_height = text_height * 0.5;
    // Extend `length` along the spin direction, +/- half a text height on the
    // perpendicular axis.
    let (dx, dy): (f64, f64) = match target.spin {
        LabelSpin::Left => (-1.0, 0.0),
        LabelSpin::Right => (1.0, 0.0),
        LabelSpin::Up => (0.0, -1.0),
        LabelSpin::Bottom => (0.0, 1.0),
    };
    let (px, py) = (dy.abs() * half_height, dx.abs() * half_height);
    field_autoplace::Bounds::from_points([
        Point::new(target.point.x - px, target.point.y - py),
        Point::new(
            target.point.x + dx * length + px,
            target.point.y + dy * length + py,
        ),
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
    plan: ConnectivityRepairPlan,
) -> Result<()> {
    remove_items(document, &plan.removals)?;
    for location in &plan.relocate_symbols {
        relocate_symbol(document, placed, location)?;
    }
    add_connectivity_drivers(
        document,
        netlist,
        placed,
        net_symbol_specs,
        root_page,
        &plan.reconnect_nets,
    )?;
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
    let new_at = packer.place(relative).to_point();
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
    )
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

    for run in net_symbol_runs {
        let spec = &net_symbol_specs[&run.net_name];
        let target = &run.targets[0];
        let connection_point = net_symbol_connection_point(
            &placed[&run.slot],
            &run.placement_target(),
            spec.pin_outward_spin,
            run.stair_index,
        )?;
        let id = available_deterministic_id(document, &target.net_symbol_key(&run.net_name));
        let symbol = build_net_symbol(spec, &run.net_name, id, connection_point)?;
        let symbol_id = symbol.id.clone();
        insert_net_symbol(document, run.page_index, symbol, &spec.definition)?;
        for member in run.targets {
            insert_connection_wires(
                document,
                member.page_index,
                &symbol_id,
                member.point,
                connection_point,
                spec.pin_outward_spin,
            );
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

fn adjacent_target_runs<'a>(
    placed: &PlacedSymbol,
    mut targets: Vec<&'a PinTarget>,
) -> Result<Vec<Vec<&'a PinTarget>>> {
    let Some(side) = targets.first().map(|target| target.spin) else {
        return Ok(Vec::new());
    };
    debug_assert!(targets.iter().all(|target| target.spin == side));
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
        .map(|target| {
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
            Ok((target, *rank, stacked))
        })
        .collect::<Result<Vec<_>>>()?;

    let mut runs = Vec::<Vec<&PinTarget>>::new();
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
    arrange_net_symbol_runs(missing, net_symbol_specs, placed)
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
    arrange_net_symbol_runs(missing, net_symbol_specs, placed)
}

fn arrange_net_symbol_runs<'a>(
    missing: impl IntoIterator<Item = (&'a String, &'a PinTarget)>,
    net_symbol_specs: &BTreeMap<String, net_symbols::NetSymbolSpec>,
    placed: &BTreeMap<SymbolSlotKey, PlacedSymbol>,
) -> Result<Vec<NetSymbolRun>> {
    type Slot = (usize, SymbolSlotKey, LabelSpin, LabelSpin);
    type GroupKey = (String, usize, SymbolSlotKey, LabelSpin, LabelSpin);

    let mut groups = BTreeMap::<GroupKey, Vec<&PinTarget>>::new();
    for (net_name, target) in missing {
        let spec = &net_symbol_specs[net_name];
        groups
            .entry((
                net_name.clone(),
                target.page_index,
                target.slot.clone(),
                target.spin,
                spec.pin_outward_spin,
            ))
            .or_default()
            .push(target);
    }

    let mut runs = Vec::<NetSymbolRun>::new();
    let mut slots = BTreeMap::<Slot, Vec<usize>>::new();
    for ((net_name, page_index, slot, target_side, symbol_side), members) in groups {
        for members in adjacent_target_runs(&placed[&slot], members)? {
            let run_index = runs.len();
            runs.push(NetSymbolRun {
                net_name: net_name.clone(),
                page_index,
                slot: slot.clone(),
                targets: members.into_iter().cloned().collect(),
                stair_index: 0,
            });
            slots
                .entry((page_index, slot.clone(), target_side, symbol_side))
                .or_default()
                .push(run_index);
        }
    }

    for ((_, _, target_side, _), run_indices) in &mut slots {
        run_indices.sort_by(|left, right| {
            let left_point = runs[*left].placement_target().point;
            let right_point = runs[*right].placement_target().point;
            let primary = if matches!(*target_side, LabelSpin::Left | LabelSpin::Right) {
                left_point.y.total_cmp(&right_point.y)
            } else {
                left_point.x.total_cmp(&right_point.x)
            };
            primary
                .then_with(|| left_point.x.total_cmp(&right_point.x))
                .then_with(|| left_point.y.total_cmp(&right_point.y))
                .then_with(|| runs[*left].net_name.cmp(&runs[*right].net_name))
        });
        for (stair_index, run_index) in run_indices.iter().enumerate() {
            runs[*run_index].stair_index = stair_index;
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
        LabelSpin::Up => Point::new(target.point.x + stair, bounds.min_y - offset),
        LabelSpin::Bottom => Point::new(target.point.x + stair, bounds.max_y + offset),
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
    symbol_pin_spin: LabelSpin,
) {
    let bend = net_symbol_wire_bend(target, connection, symbol_pin_spin);
    for (index, (a, b)) in [(target, bend), (bend, connection)].into_iter().enumerate() {
        if points_coincide(a, b) {
            continue;
        }
        let id = available_deterministic_id(
            document,
            &format!("zener:net-symbol-wire:{symbol_id}:{index}"),
        );
        document.pages[page_index].items.push(SchItem::Wire(Wire {
            id,
            a,
            b,
            unsupported: Vec::new(),
        }));
    }
}

fn net_symbol_wire_bend(target: Point, connection: Point, symbol_pin_spin: LabelSpin) -> Point {
    if symbol_pin_spin.is_vertical() {
        Point::new(connection.x, target.y)
    } else {
        Point::new(target.x, connection.y)
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
        for (net_name, interface_names) in context {
            if !target_nets.contains(net_name) || net_symbol_specs.contains_key(net_name) {
                continue;
            }
            // Anchor at a pin, else at a sheet pin carrying the interface
            // (the net reaches this page only through a subsheet), else at a
            // free spot.
            let (anchor, spin) = match canonical_target(targets_by_net, net_name, page_index)
                .map(|target| (target.point, target.spin))
                .or_else(|| sheet_pin_anchor(document, page_index, interface_names))
            {
                Some(anchor) => anchor,
                None => (
                    place_context_label(
                        document,
                        page_index,
                        interface_names.first().map_or(net_name, String::as_str),
                    )?,
                    LabelSpin::Right,
                ),
            };
            for interface_name in interface_names {
                if page_has_hierarchical_label(document, page_index, interface_name) {
                    continue;
                }
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

fn estimated_label_width(text: &str) -> f64 {
    let font_height = crate::TextEffects::default().font_size.y.abs();
    text.chars().count().max(1) as f64 * font_height * ESTIMATED_LABEL_WIDTH_EM
}

fn estimated_shaped_label_width(text: &str) -> f64 {
    estimated_label_width(text) + LABEL_SHAPE_LENGTH_MM
}

fn paper_dimensions(paper: &Paper) -> Result<(f64, f64)> {
    let (mut width, mut height) = match paper {
        Paper::Custom {
            width_mm,
            height_mm,
        } => (*width_mm, *height_mm),
        Paper::Named { name, .. } => match name.as_str() {
            "A0" => (1189.0, 841.0),
            "A1" => (841.0, 594.0),
            "A2" => (594.0, 420.0),
            "A3" => (420.0, 297.0),
            "A4" => (297.0, 210.0),
            "A5" => (210.0, 148.0),
            "A" | "USLetter" => (279.4, 215.9),
            "B" | "USLedger" => (431.8, 279.4),
            "C" => (558.8, 431.8),
            "D" => (863.6, 558.8),
            "E" => (1117.6, 863.6),
            "USLegal" => (355.6, 215.9),
            "GERBER" => (812.8, 812.8),
            _ => bail!("unsupported KiCad paper size '{name}' for interface placement"),
        },
    };
    if matches!(paper, Paper::Named { portrait: true, .. }) {
        std::mem::swap(&mut width, &mut height);
    }
    Ok((width, height))
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

        assert!((second.x - first.x - 2.0 * CONNECTION_GRID_MM).abs() < GEOMETRY_EPS_MM);
        assert!((second.y - first.y - 2.0 * CONNECTION_GRID_MM).abs() < GEOMETRY_EPS_MM);
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
        let bend = net_symbol_wire_bend(second.point, connection, LabelSpin::Up);
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

        let split = adjacent_target_runs(item, vec![&first, &last]).unwrap();
        assert_eq!(split.len(), 2);

        let shared = adjacent_target_runs(item, vec![&first, &intervening, &last]).unwrap();
        assert_eq!(shared.len(), 1);
        assert_eq!(shared[0].len(), 3);
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

    #[test]
    fn grid_packer_prefers_the_page_center() {
        let mut packer = GridPacker::for_page(&Paper::default()).unwrap();
        let relative = GridRect {
            min_x: -2,
            min_y: -2,
            max_x: 2,
            max_y: 2,
        };

        let placed = relative.translated(packer.place(relative));

        let center_dx = placed.min_x + placed.max_x - packer.usable.min_x - packer.usable.max_x;
        let center_dy = placed.min_y + placed.max_y - packer.usable.min_y - packer.usable.max_y;
        assert!(center_dx.abs() <= 1);
        assert!(center_dy.abs() <= 1);
    }
}
