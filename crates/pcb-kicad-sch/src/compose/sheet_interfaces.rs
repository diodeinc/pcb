//! Keep each generated module sheet's pins in step with its module interface.
//!
//! A module page outlives changes to the module behind it: a swapped
//! implementation keeps its instance path, and an io passed `NotConnected()`
//! leaves the module in place. Its sheet must still expose exactly the
//! module's current ports, or KiCad reports hierarchical mismatches and the
//! repair has to bridge the new ports with global labels.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use pcb_sch::Schematic;

use super::{
    PinTarget, PlacedSymbol, SHEET_PIN_SPACING_MM, available_deterministic_id, canonical_target,
    connectivity_targets, linked_modules, module_ports, page_driver_contexts,
    page_has_hierarchical_label, place_context_label, points_coincide, upsert_contextual_driver,
};
use crate::{
    LabelKind, LabelShape, LabelSpin, Point, Rotation, SchDocument, SchItem, SchPage, SheetPin,
    SymbolSlotKey,
    connectivity::{
        ConnectionOrigin, ConnectivityItemRef, PhysicalConnectivity, PhysicalPinRef, PinVisibility,
        kicad::PagesByFile, named_connected_nets, reduce_with_provenance,
    },
    hierarchy, net_symbols,
    repair::point_on_segment,
};

/// Growing a full sheet is bounded so a degenerate sheet cannot loop forever.
const MAX_SHEET_GROWTH_STEPS: usize = 256;

struct GeneratedSheet {
    module_path: String,
    parent_page: usize,
    child_page: usize,
    sheet_id: String,
    ports: Vec<(String, String)>,
}

/// Reconcile the pins of every previously generated module sheet with its
/// module's ports. Only sheets whose UUID, file and child page UUID still
/// identify the generated module are touched; renamed or user-organized
/// sheets remain authoritative.
///
/// A pin stays while it is named after a port or still carries one of the
/// module's port nets to a pin inside the module, so a user's consistent
/// renaming of a pin and its hierarchical label survives. Other pins (ports
/// removed, renamed by a swapped implementation or explicitly not connected)
/// are removed together with the child's hierarchical labels they served and
/// the parent drivers that only served them. A pin stacked on another pin's
/// point is moved. Missing ports get a pin on a free point of the sheet edge
/// and drivers on both sides, exactly as when the sheet was first generated.
pub(super) fn sync_generated_sheet_interfaces(
    document: &mut SchDocument,
    netlist: &Schematic,
    placed: &BTreeMap<SymbolSlotKey, PlacedSymbol>,
    net_symbol_specs: &BTreeMap<String, net_symbols::NetSymbolSpec>,
    plan: &hierarchy::HierarchyPlan,
) -> Result<()> {
    let sheets = generated_sheets(document, netlist, plan)?;
    if sheets.is_empty() {
        return Ok(());
    }

    deduplicate_sheet_names(document, &sheets, plan);
    if sheets
        .iter()
        .all(|sheet| interface_matches(document, sheet))
    {
        return Ok(());
    }

    let all_nets = named_connected_nets(netlist)
        .map(|net| net.name.clone())
        .collect();
    let targets = connectivity_targets(netlist, placed, &all_nets)?;
    let carried = CarriedNets::new(document, &targets)?;
    let mut missing = Vec::new();
    for sheet in &sheets {
        let port_names = sheet
            .ports
            .iter()
            .map(|(_, port_name)| port_name.as_str())
            .collect::<BTreeSet<_>>();
        let port_nets = sheet
            .ports
            .iter()
            .map(|(net_name, _)| net_name.as_str())
            .collect::<BTreeSet<_>>();
        let page_id = document.pages[sheet.parent_page].id.clone();
        let pins = &mut sheet_mut(document, sheet).pins;
        let mut kept = Vec::<SheetPin>::new();
        let mut aliased_nets = BTreeSet::new();
        let mut removed = Vec::new();
        for pin in pins.drain(..) {
            // Coincident sheet pins short their nets together in KiCad.
            let stacked = kept
                .iter()
                .any(|other| other.name != pin.name && points_coincide(other.at, pin.at));
            if stacked {
                removed.push(pin.at);
                continue;
            }
            if !port_names.contains(pin.name.as_str()) {
                let alias_nets = carried
                    .through_pin(&page_id, &sheet.sheet_id, &pin.id, &sheet.module_path)
                    .intersection(&port_nets)
                    .copied()
                    .collect::<BTreeSet<_>>();
                if alias_nets.is_empty() {
                    removed.push(pin.at);
                    continue;
                }
                aliased_nets.extend(alias_nets);
            }
            kept.push(pin);
        }
        *pins = kept;
        let pin_names = pins
            .iter()
            .map(|pin| pin.name.clone())
            .collect::<BTreeSet<_>>();
        for point in removed {
            remove_dangling_drivers(document, sheet.parent_page, point)?;
        }
        // A hierarchical label is only reachable through its sheet pin.
        document.pages[sheet.child_page].items.retain(|item| {
            !matches!(item, SchItem::Label(label)
                if matches!(label.kind, LabelKind::Hierarchical { .. })
                    && !port_names.contains(label.text.as_str())
                    && !pin_names.contains(&label.text))
        });

        missing.extend(
            sheet
                .ports
                .iter()
                .filter(|(net_name, port_name)| {
                    !pin_names.contains(port_name) && !aliased_nets.contains(net_name.as_str())
                })
                .map(|(net_name, port_name)| (sheet, net_name.clone(), port_name.clone())),
        );
    }
    if missing.is_empty() {
        return Ok(());
    }

    let page_contexts = page_driver_contexts(document, netlist, placed, plan.root_page())?;
    for (sheet, net_name, port_name) in missing {
        let Some((at, rotation)) = free_pin_point(document, sheet)? else {
            // A sheet that cannot grow keeps its pins; the connectivity
            // repair still joins the port's net through labels.
            continue;
        };
        let id = available_deterministic_id(
            document,
            &format!("zener:module-sheet-pin:{}:{port_name}", sheet.module_path),
        );
        sheet_mut(document, sheet).pins.push(SheetPin {
            id,
            name: port_name.clone(),
            at,
            rotation,
            shape: LabelShape::Bidirectional,
            effects: crate::TextEffects::default(),
            unsupported: Vec::new(),
        });

        let key = format!("{}:{port_name}", sheet.module_path);
        let spin = if rotation == Rotation::Deg0 {
            LabelSpin::Right
        } else {
            LabelSpin::Left
        };
        upsert_contextual_driver(
            document,
            net_symbol_specs,
            &page_contexts,
            sheet.parent_page,
            &net_name,
            (at, spin),
            &format!("zener:module-parent-driver:{key}:{net_name}"),
        )?;
        if page_has_hierarchical_label(document, sheet.child_page, &port_name) {
            continue;
        }
        let child_anchor = match canonical_target(&targets, &net_name, sheet.child_page) {
            Some(target) => (target.point, target.spin),
            None => (
                place_context_label(document, sheet.child_page, &port_name)?,
                LabelSpin::Right,
            ),
        };
        upsert_contextual_driver(
            document,
            net_symbol_specs,
            &page_contexts,
            sheet.child_page,
            &net_name,
            child_anchor,
            &format!("zener:module-child-driver:{key}:{net_name}"),
        )?;
    }
    Ok(())
}

/// The netlist nets each KiCad net reaches through managed component pins,
/// keyed by those pins' component paths.
struct CarriedNets<'a> {
    observed: PhysicalConnectivity,
    pins: BTreeMap<PhysicalPinRef, (&'a str, &'a str)>,
}

impl<'a> CarriedNets<'a> {
    fn new(document: &SchDocument, targets: &'a BTreeMap<String, Vec<PinTarget>>) -> Result<Self> {
        let pins = targets
            .iter()
            .flat_map(|(net_name, targets)| {
                targets.iter().map(move |target| {
                    (
                        target.physical_pin(&document.pages[target.page_index].id),
                        (target.slot.component_path(), net_name.as_str()),
                    )
                })
            })
            .collect();
        Ok(Self {
            observed: reduce_with_provenance(document, PinVisibility::VisibleOnly)?,
            pins,
        })
    }

    /// Nets of the pins inside `module_path` that a sheet pin connects to.
    fn through_pin(
        &self,
        page_id: &str,
        sheet_id: &str,
        pin_id: &str,
        module_path: &str,
    ) -> BTreeSet<&'a str> {
        let pin = ConnectivityItemRef::SheetPin {
            page_id: page_id.to_string(),
            sheet_id: sheet_id.to_string(),
            pin_id: pin_id.to_string(),
        };
        let Some(island) = self
            .observed
            .islands
            .iter()
            .find_map(|(island, provenance)| provenance.items.contains(&pin).then_some(island))
        else {
            return BTreeSet::new();
        };
        let origin = ConnectionOrigin::KiCadIsland(island.clone());
        let Some(group) = self
            .observed
            .graph
            .groups
            .iter()
            .find(|group| group.origins.contains(&origin))
        else {
            return BTreeSet::new();
        };
        group
            .origins
            .iter()
            .filter_map(|origin| match origin {
                ConnectionOrigin::KiCadIsland(island) => self.observed.islands.get(island),
                _ => None,
            })
            .flat_map(|island| &island.pins)
            .filter_map(|pin| self.pins.get(pin))
            .filter(|(component_path, _)| hierarchy::is_descendant(component_path, module_path))
            .map(|(_, net_name)| *net_name)
            .collect()
    }
}

fn generated_sheets(
    document: &SchDocument,
    netlist: &Schematic,
    plan: &hierarchy::HierarchyPlan,
) -> Result<Vec<GeneratedSheet>> {
    let pages_by_file = PagesByFile::new(&document.pages);
    let mut sheets = Vec::new();
    for module in linked_modules(netlist)? {
        // A sheet generated by this application already has the right pins.
        if plan
            .sheets
            .iter()
            .any(|sheet| sheet.module_path == module.path)
        {
            continue;
        }
        let Some(child_page) = document
            .pages
            .iter()
            .position(|page| page.id == hierarchy::page_id(&module.path))
        else {
            continue;
        };
        let sheet_id = hierarchy::sheet_id(&module.path);
        let parent_page = document.pages.iter().position(|page| {
            page.items.iter().any(|item| {
                matches!(item, SchItem::Sheet(sheet)
                    if sheet.id == sheet_id
                        && sheet.placed
                        && sheet.at.is_some()
                        && sheet.size.is_some()
                        && pages_by_file.child(page, sheet) == Some(child_page))
            })
        });
        let Some(parent_page) = parent_page else {
            continue;
        };
        sheets.push(GeneratedSheet {
            ports: module_ports(netlist, &module.instance_ref)?,
            module_path: module.path,
            parent_page,
            child_page,
            sheet_id,
        });
    }
    Ok(sheets)
}

/// Whether the sheet has exactly one unstacked pin per port name and its
/// page no other hierarchical labels: the state generation leaves it in.
fn interface_matches(document: &SchDocument, generated: &GeneratedSheet) -> bool {
    let pins = &sheet(document, generated).pins;
    let mut names = pins.iter().map(|pin| pin.name.as_str()).collect::<Vec<_>>();
    let mut ports = generated
        .ports
        .iter()
        .map(|(_, port_name)| port_name.as_str())
        .collect::<Vec<_>>();
    names.sort_unstable();
    ports.sort_unstable();
    names == ports
        && pins.iter().enumerate().all(|(index, pin)| {
            pins[..index]
                .iter()
                .all(|other| !points_coincide(other.at, pin.at))
        })
        && document.pages[generated.child_page]
            .items
            .iter()
            .all(|item| {
                !matches!(item, SchItem::Label(label)
                    if matches!(label.kind, LabelKind::Hierarchical { .. })
                        && ports.binary_search(&label.text.as_str()).is_err())
            })
}

/// Earlier versions named each sheet after its leaf instance name, so
/// instances of one module nested in unlinked modules collided on one page.
/// KiCad requires unique sibling sheet names.
fn deduplicate_sheet_names(
    document: &mut SchDocument,
    sheets: &[GeneratedSheet],
    plan: &hierarchy::HierarchyPlan,
) {
    let duplicated = sheets
        .iter()
        .filter(|generated| {
            let Some(name) = &sheet(document, generated).name else {
                return false;
            };
            document.pages[generated.parent_page]
                .items
                .iter()
                .any(|item| {
                    matches!(item, SchItem::Sheet(other)
                    if other.id != generated.sheet_id
                        && other.name.as_ref().is_some_and(|field| field.value == name.value))
                })
        })
        .collect::<Vec<_>>();
    for sheet in duplicated {
        let name = plan.sheet_name(&sheet.module_path).to_string();
        if let Some(field) = sheet_mut(document, sheet).name.as_mut() {
            field.value = name;
        }
    }
}

fn sheet<'a>(document: &'a SchDocument, generated: &GeneratedSheet) -> &'a crate::Sheet {
    document.pages[generated.parent_page]
        .items
        .iter()
        .find_map(|item| match item {
            SchItem::Sheet(sheet) if sheet.id == generated.sheet_id => Some(sheet.as_ref()),
            _ => None,
        })
        .expect("generated sheet stays on its parent page")
}

fn sheet_mut<'a>(document: &'a mut SchDocument, sheet: &GeneratedSheet) -> &'a mut crate::Sheet {
    document.pages[sheet.parent_page]
        .items
        .iter_mut()
        .find_map(|item| match item {
            SchItem::Sheet(candidate) if candidate.id == sheet.sheet_id => Some(candidate.as_mut()),
            _ => None,
        })
        .expect("generated sheet stays on its parent page")
}

/// What touches a connection point on one page.
#[derive(Default)]
struct PointOccupancy {
    /// Labels and unmanaged single-pin symbols (power symbols) anchored here.
    drivers: BTreeSet<String>,
    /// Anything else that connects here: wires, junctions, no-connect
    /// markers, sheet pins and component pins.
    connections: usize,
}

fn point_occupancy(page: &SchPage, point: Point) -> Result<PointOccupancy> {
    let mut occupancy = PointOccupancy::default();
    for item in &page.items {
        match item {
            SchItem::Wire(wire) => {
                if point_on_segment(point, wire.a, wire.b) {
                    occupancy.connections += 1;
                }
            }
            SchItem::Junction(junction) => {
                if points_coincide(junction.at, point) {
                    occupancy.connections += 1;
                }
            }
            SchItem::NoConnect(no_connect) => {
                if points_coincide(no_connect.at, point) {
                    occupancy.connections += 1;
                }
            }
            SchItem::Label(label) => {
                if points_coincide(label.at, point) {
                    occupancy.drivers.insert(label.id.clone());
                }
            }
            SchItem::Sheet(sheet) => {
                occupancy.connections += sheet
                    .pins
                    .iter()
                    .filter(|pin| points_coincide(pin.at, point))
                    .count();
            }
            SchItem::Symbol(symbol) => {
                let Some(definition) = page.library.definitions.get(symbol.library_key()) else {
                    continue;
                };
                let pins = definition.placed_pins(symbol)?;
                if !pins.iter().any(|pin| points_coincide(pin.point, point)) {
                    continue;
                }
                if symbol.field_value("Path").is_none() && pins.len() == 1 {
                    occupancy.drivers.insert(symbol.id.clone());
                } else {
                    occupancy.connections += 1;
                }
            }
            SchItem::Graphic(_) | SchItem::Unsupported(_) => {}
        }
    }
    Ok(occupancy)
}

/// Remove labels and power symbols left on a removed sheet pin's point when
/// nothing else connects there. Alone they join no pins, so removing them
/// cannot change connectivity.
fn remove_dangling_drivers(
    document: &mut SchDocument,
    page_index: usize,
    point: Point,
) -> Result<()> {
    let page = &mut document.pages[page_index];
    let occupancy = point_occupancy(page, point)?;
    if occupancy.connections == 0 && !occupancy.drivers.is_empty() {
        page.items
            .retain(|item| item.id().is_none_or(|id| !occupancy.drivers.contains(id)));
    }
    Ok(())
}

/// The first free point on the sheet's left, then right, edge at the
/// generated pin pitch. A full sheet grows downward.
fn free_pin_point(
    document: &mut SchDocument,
    generated: &GeneratedSheet,
) -> Result<Option<(Point, Rotation)>> {
    for _ in 0..MAX_SHEET_GROWTH_STEPS {
        let page = &document.pages[generated.parent_page];
        let sheet = sheet(document, generated);
        let (Some(at), Some(size)) = (sheet.at, sheet.size) else {
            return Ok(None);
        };
        for (x, rotation) in [(at.x, Rotation::Deg180), (at.x + size.x, Rotation::Deg0)] {
            let mut offset = SHEET_PIN_SPACING_MM;
            while offset < size.y - crate::CONNECTION_GRID_MM * 0.5 {
                let point = Point::new(x, at.y + offset);
                let occupancy = point_occupancy(page, point)?;
                if occupancy.connections == 0 && occupancy.drivers.is_empty() {
                    return Ok(Some((point, rotation)));
                }
                offset += SHEET_PIN_SPACING_MM;
            }
        }
        // Growing would detach pins on the bottom edge.
        let bottom = at.y + size.y;
        if sheet
            .pins
            .iter()
            .any(|pin| (pin.at.y - bottom).abs() <= crate::GEOMETRY_EPS_MM)
        {
            return Ok(None);
        }
        let sheet = sheet_mut(document, generated);
        let size = sheet.size.as_mut().expect("sheet size checked above");
        size.y += SHEET_PIN_SPACING_MM;
        if sheet.file.at.y >= bottom - crate::GEOMETRY_EPS_MM {
            sheet.file.at.y += SHEET_PIN_SPACING_MM;
        }
    }
    Ok(None)
}
