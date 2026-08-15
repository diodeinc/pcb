use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use pcb_sch::{ATTR_SCHEMATIC_PATH, Instance, InstanceKind, Schematic};
use pcb_sexpr::Sexpr;

use crate::{
    CONNECTION_GRID_MM, GEOMETRY_EPS_MM, Label, LabelKind, LabelShape, LabelSpin, Paper, Point,
    Rotation, SchDocument, SchItem, SchPage, Sheet, SheetPin, Symbol, SymbolDefinition,
    SymbolField, SymbolSlotKey, Wire,
    analysis::{analyze_schematic, terminals_match},
    component_slots,
    connectivity::{
        ComponentIdentity, ConnectivityItemRef, PinVisibility, Terminal, named_connected_nets,
        reduce_with_provenance,
    },
    deterministic_uuid, field_autoplace, hierarchy, net_symbols,
    repair::{RepairScope, plan_connectivity_repair_with, remove_items},
    root_interface, root_page_id, symbol,
};

const INTERFACE_STUB_LENGTH_MM: f64 = 5.08;
const INTERFACE_STUB_CELLS: i32 = 4;
const DEFAULT_TITLE_BLOCK_WIDTH_MM: f64 = 110.0;
const DEFAULT_TITLE_BLOCK_HEIGHT_MM: f64 = 34.0;
const PACKING_MARGIN_CELLS: i32 = 5;
const PACKING_CLEARANCE_CELLS: i32 = 2;
const LABEL_SHAPE_LENGTH_MM: f64 = 2.54;
const ESTIMATED_LABEL_WIDTH_EM: f64 = 0.8;
const SHEET_PIN_SPACING_MM: f64 = 5.08;
const SHEET_MIN_WIDTH_MM: f64 = 50.8;
const SHEET_MIN_HEIGHT_MM: f64 = 20.32;

pub(crate) fn reconcile_document(
    existing: Option<&SchDocument>,
    netlist: &Schematic,
    root_file_name: &str,
) -> Result<SchDocument> {
    // The generated document is not a canonical representation. Any existing
    // KiCad organization is valid when its connectivity matches the netlist;
    // managed symbol identities are the only reconciliation metadata we may
    // rely on. See ../DESIGN.md.
    let creating = existing.is_none();
    let mut document = existing.cloned().unwrap_or_else(|| SchDocument {
        pages: vec![SchPage {
            file_name: Some(root_file_name.to_string()),
            ..SchPage::new(root_page_id())
        }],
        root_page_ids: vec![root_page_id()],
    });
    if document.pages.is_empty() {
        bail!("KiCad schematic project has no pages");
    }
    let default_page = document
        .root_page_ids
        .iter()
        .find_map(|id| document.pages.iter().position(|page| &page.id == id))
        .context("KiCad schematic project has no loaded root page")?;

    let existing_slots = existing_slot_locations(&document);
    let instances = component_instances(netlist)?;
    let slots = component_slots::component_symbol_slots(netlist)?;
    let net_symbol_specs = net_symbols::specs(netlist)?;
    let mut selected_existing = BTreeMap::new();
    for slot in &slots {
        if let Some(location) = select_existing_symbol(slot, existing_slots.get(slot)) {
            selected_existing.insert(slot.clone(), location);
        }
    }
    let relocatable_slots = slots
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
    let hierarchy = hierarchy::plan(
        linked_modules(netlist)?,
        existing_component_pages,
        default_page,
        document.pages.len(),
    )?;
    materialize_hierarchy(&mut document, netlist, &hierarchy)?;

    let retained_power_symbols = power_symbol_locations(&document)?;
    retain_power_symbol_definitions(&mut document, &retained_power_symbols);

    let mut placed = BTreeMap::new();
    let mut retained_symbol_locations = retained_power_symbols;
    for slot in &slots {
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
        let (page_index, previous) = match selected_existing.get(slot) {
            Some(location) => (
                location.page_index,
                Some(&existing_slots[slot][location.candidate_index].symbol),
            ),
            None => (
                hierarchy.page_for_new_component(slot.component_path())?,
                None,
            ),
        };
        let at = previous.map(|symbol| symbol.at).unwrap_or_default();
        let rotation = previous.map(|symbol| symbol.rotation).unwrap_or_default();
        let mirror = previous.and_then(|symbol| symbol.mirror);
        let symbol =
            build_component_symbol(instance, slot, &definition, at, rotation, mirror, previous)?;

        let page = &mut document.pages[page_index];
        match page.library.definitions.get(&definition.lib_id) {
            Some(found) if found != &definition => bail!(
                "page '{}' has conflicting definitions for library symbol '{}'",
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
        let item_index = selected_existing
            .get(slot)
            .map(|location| existing_slots[slot][location.candidate_index].item_index)
            .unwrap_or_else(|| {
                page.items.push(SchItem::Symbol(symbol.clone()));
                page.items.len() - 1
            });
        if selected_existing.contains_key(slot) {
            page.items[item_index] = SchItem::Symbol(symbol.clone());
        }
        retained_symbol_locations.insert((page_index, item_index));
        placed.insert(
            slot.clone(),
            PlacedSymbol {
                page_index,
                symbol,
                definition,
            },
        );
    }
    retain_projected_symbols(&mut document, &retained_symbol_locations);

    pack_generated_symbols(&mut document, netlist, &mut placed, &relocatable_slots)?;

    // Generated labels, wires, and hierarchy are an initialization policy,
    // not managed reconciliation state. If the projected symbols already form
    // the expected connectivity, preserve every KiCad-authored electrical item
    // exactly as found regardless of whether its UUID was once generated here.
    if !creating
        && hierarchy.sheets.is_empty()
        && analyze_schematic(&document, netlist)?.is_equivalent()
    {
        return Ok(document);
    }

    add_hierarchy_connectivity(&mut document, netlist, &placed, &hierarchy)?;

    refresh_generated_presentation(
        &mut document,
        netlist,
        &placed,
        &net_symbol_specs,
        default_page,
    )?;

    if !creating && analyze_schematic(&document, netlist)?.is_equivalent() {
        return Ok(document);
    }

    let repair_scope = if creating {
        RepairScope::InitializeAllNets
    } else {
        RepairScope::ExistingIssues
    };
    repair_connectivity(
        &mut document,
        netlist,
        &placed,
        &net_symbol_specs,
        default_page,
        repair_scope,
    )?;
    Ok(document)
}

/// Component symbols are projected from Zener. Explicit KiCad power symbols
/// are semantic net-name drivers, so they remain available for connectivity
/// analysis and minimally destructive repair.
fn power_symbol_locations(document: &SchDocument) -> Result<BTreeSet<(usize, usize)>> {
    let mut locations = BTreeSet::new();
    for (page_index, page) in document.pages.iter().enumerate() {
        for (item_index, item) in page.items.iter().enumerate() {
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
                locations.insert((page_index, item_index));
            }
        }
    }
    Ok(locations)
}

fn retain_power_symbol_definitions(
    document: &mut SchDocument,
    retained_symbols: &BTreeSet<(usize, usize)>,
) {
    for (page_index, page) in document.pages.iter_mut().enumerate() {
        let retained_definitions = page
            .items
            .iter()
            .enumerate()
            .filter(|(item_index, _)| retained_symbols.contains(&(page_index, *item_index)))
            .filter_map(|(_, item)| match item {
                SchItem::Symbol(symbol) => Some(symbol.lib_id.clone()),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        page.library
            .definitions
            .retain(|lib_id, _| retained_definitions.contains(lib_id));
    }
}

fn retain_projected_symbols(document: &mut SchDocument, retained: &BTreeSet<(usize, usize)>) {
    for (page_index, page) in document.pages.iter_mut().enumerate() {
        page.items = page
            .items
            .drain(..)
            .enumerate()
            .filter_map(|(item_index, item)| {
                (!matches!(item, SchItem::Symbol(_))
                    || retained.contains(&(page_index, item_index)))
                .then_some(item)
            })
            .collect();
    }
}

#[derive(Clone)]
struct ExistingSymbol {
    page_index: usize,
    item_index: usize,
    symbol: Symbol,
}

#[derive(Clone, Copy)]
struct ExistingSelection {
    page_index: usize,
    candidate_index: usize,
}

fn existing_slot_locations(document: &SchDocument) -> BTreeMap<SymbolSlotKey, Vec<ExistingSymbol>> {
    let mut locations = BTreeMap::<SymbolSlotKey, Vec<ExistingSymbol>>::new();
    for (page_index, page) in document.pages.iter().enumerate() {
        for (item_index, symbol) in
            page.items
                .iter()
                .enumerate()
                .filter_map(|(index, item)| match item {
                    SchItem::Symbol(symbol) => Some((index, symbol)),
                    _ => None,
                })
        {
            let Some(path) = symbol.field_value("Path") else {
                continue;
            };
            let Some(slot) = SymbolSlotKey::new(path, symbol.unit) else {
                continue;
            };
            locations.entry(slot).or_default().push(ExistingSymbol {
                page_index,
                item_index,
                symbol: symbol.clone(),
            });
        }
    }
    locations
}

fn select_existing_symbol(
    slot: &SymbolSlotKey,
    candidates: Option<&Vec<ExistingSymbol>>,
) -> Option<ExistingSelection> {
    let candidates = candidates?;
    let exact = candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| candidate.symbol.id == slot.symbol_id())
        .collect::<Vec<_>>();
    let (candidate_index, candidate) = match exact.as_slice() {
        [(index, candidate)] => (*index, *candidate),
        [] if candidates.len() == 1 => (0, &candidates[0]),
        _ => return None,
    };
    Some(ExistingSelection {
        page_index: candidate.page_index,
        candidate_index,
    })
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
        if !component_paths.iter().any(|component_path| {
            component_path
                .strip_prefix(&path)
                .is_some_and(|suffix| suffix.starts_with('.'))
        }) {
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
        let mut file_field = SymbolField::new("Sheetfile", &sheet_plan.file_name, at);
        file_field.at.y += height + 0.7112;
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
        child.file_name = Some(sheet_plan.file_name.clone());
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
) -> Result<()> {
    if relocatable_slots.is_empty() {
        return Ok(());
    }
    let all_nets = named_connected_nets(netlist)
        .map(|net| net.name.clone())
        .collect();
    let targets = connectivity_targets(netlist, placed, &all_nets)?;

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
        occupy_page_items_except(&mut packer, &document.pages[page_index], &relocatable)?;

        let mut items = relocatable
            .into_iter()
            .map(|slot| {
                let bounds = GridRect::from_bounds(component_envelope(&placed[&slot], &targets)?);
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
    excluded_slots: &BTreeSet<SymbolSlotKey>,
) -> Result<()> {
    for item in &page.items {
        match item {
            SchItem::Symbol(symbol)
                if !excluded_slots
                    .iter()
                    .any(|slot| slot.symbol_id() == symbol.id) =>
            {
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
            _ => {}
        }
    }
    Ok(())
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
            let anchor = GridPoint {
                x: min_anchor_x,
                y: min_anchor_y,
            };
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
                if overlap == 0 {
                    self.occupy(candidate);
                    return anchor;
                }
                if best.is_none_or(|(best_overlap, _)| overlap < best_overlap) {
                    best = Some((overlap, anchor));
                }
            }
        }
        let anchor = best.expect("a non-empty anchor range has a candidate").1;
        self.occupy(relative.translated(anchor));
        anchor
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
) -> Result<field_autoplace::Bounds> {
    let mut bounds = field_autoplace::symbol_visual_bounds(&placed.symbol, &placed.definition)?
        .unwrap_or_else(|| {
            field_autoplace::Bounds::from_points([placed.symbol.at])
                .expect("one point defines bounds")
        });
    for (net_name, net_targets) in targets {
        for target in net_targets
            .iter()
            .filter(|target| !target.hidden && target.slot.symbol_id() == placed.symbol.id)
        {
            bounds.union(net_label_bounds(net_name, target));
        }
    }
    Ok(bounds.translated(-placed.symbol.at.x, -placed.symbol.at.y))
}

fn net_label_bounds(net_name: &str, target: &PinTarget) -> field_autoplace::Bounds {
    let text_height = crate::TextEffects::default().font_size.y.abs();
    let length = estimated_label_width(net_name);
    let half_height = text_height * 0.5;
    let (min, max) = match target.spin {
        LabelSpin::Left => (
            Point::new(target.point.x - length, target.point.y - half_height),
            Point::new(target.point.x, target.point.y + half_height),
        ),
        LabelSpin::Right => (
            Point::new(target.point.x, target.point.y - half_height),
            Point::new(target.point.x + length, target.point.y + half_height),
        ),
        LabelSpin::Up => (
            Point::new(target.point.x - half_height, target.point.y - length),
            Point::new(target.point.x + half_height, target.point.y),
        ),
        LabelSpin::Bottom => (
            Point::new(target.point.x - half_height, target.point.y),
            Point::new(target.point.x + half_height, target.point.y + length),
        ),
    };
    field_autoplace::Bounds::from_points([min, max]).expect("two points define label bounds")
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
    pin_name: String,
    number: String,
    pin_numbers: BTreeSet<String>,
    point: Point,
    spin: LabelSpin,
    hidden: bool,
}

impl PinTarget {
    fn terminal(&self) -> Terminal {
        Terminal::ComponentPin {
            component: ComponentIdentity::ManagedPath(self.slot.component_path().to_string()),
            pin_name: self.pin_name.clone(),
            pin_numbers: self.pin_numbers.clone(),
        }
    }

    fn label_id(&self, net_name: &str) -> String {
        deterministic_uuid(format!(
            "zener:net-label:{net_name}:{}:{}",
            self.slot.symbol_id(),
            self.number
        ))
    }

    fn legacy_label_id(&self, net_name: &str) -> String {
        deterministic_uuid(format!(
            "zener:global-label:{net_name}:{}:{}",
            self.slot.symbol_id(),
            self.number
        ))
    }

    fn net_symbol_id(&self, net_name: &str) -> String {
        deterministic_uuid(format!(
            "zener:net-symbol:{net_name}:{}:{}:{:.4}:{:.4}",
            self.slot.symbol_id(),
            self.number,
            self.point.x,
            self.point.y
        ))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ConnectivityUpdate {
    InsertMissing,
    ExistingOnly,
}

fn refresh_generated_presentation(
    document: &mut SchDocument,
    netlist: &Schematic,
    placed: &BTreeMap<SymbolSlotKey, PlacedSymbol>,
    net_symbol_specs: &BTreeMap<String, net_symbols::NetSymbolSpec>,
    root_page: usize,
) -> Result<()> {
    let all_nets = named_connected_nets(netlist)
        .map(|net| net.name.clone())
        .collect();
    add_connectivity_drivers(
        document,
        netlist,
        placed,
        net_symbol_specs,
        root_page,
        &all_nets,
        ConnectivityUpdate::ExistingOnly,
    )
}

fn repair_connectivity(
    document: &mut SchDocument,
    netlist: &Schematic,
    placed: &BTreeMap<SymbolSlotKey, PlacedSymbol>,
    net_symbol_specs: &BTreeMap<String, net_symbols::NetSymbolSpec>,
    root_page: usize,
    scope: RepairScope,
) -> Result<()> {
    let plan = plan_connectivity_repair_with(document, netlist, scope)?;
    remove_items(document, plan.removals())?;
    add_connectivity_drivers(
        document,
        netlist,
        placed,
        net_symbol_specs,
        root_page,
        plan.reconnect_nets(),
        ConnectivityUpdate::InsertMissing,
    )?;
    prune_unused_symbol_definitions(document);
    Ok(())
}

fn add_hierarchy_connectivity(
    document: &mut SchDocument,
    netlist: &Schematic,
    placed: &BTreeMap<SymbolSlotKey, PlacedSymbol>,
    plan: &hierarchy::HierarchyPlan,
) -> Result<()> {
    if plan.sheets.is_empty() {
        return Ok(());
    }
    let all_nets = named_connected_nets(netlist)
        .map(|net| net.name.clone())
        .collect();
    let targets = connectivity_targets(netlist, placed, &all_nets)?;

    for sheet_plan in &plan.sheets {
        let ports = module_ports(netlist, &sheet_plan.instance_ref)?;
        let child_anchors =
            interface_anchors(document, placed, &targets, sheet_plan.child_page, &ports)?;
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

        for (((net_name, port_name), (child_net, child_port)), parent_pin) in
            ports.into_iter().zip(child_anchors).zip(parent_anchors)
        {
            let key = format!("{}:{port_name}", sheet_plan.module_path);
            let parent_net = Point::new(parent_pin.x - INTERFACE_STUB_LENGTH_MM, parent_pin.y);
            let mut parent_label = Label::new(
                deterministic_uuid(format!("zener:module-parent-net:{key}:{net_name}")),
                &net_name,
                parent_net,
            );
            parent_label.spin = LabelSpin::Left;
            upsert_label(document, sheet_plan.parent_page, parent_label)?;
            upsert_wire(
                document,
                sheet_plan.parent_page,
                Wire {
                    id: deterministic_uuid(format!("zener:module-parent-link:{key}")),
                    a: parent_net,
                    b: parent_pin,
                    unsupported: Vec::new(),
                },
            )?;

            let mut child_net_label = Label::new(
                deterministic_uuid(format!("zener:module-child-net:{key}:{net_name}")),
                &net_name,
                child_net,
            );
            child_net_label.spin = LabelSpin::Left;
            upsert_label(document, sheet_plan.child_page, child_net_label)?;
            upsert_wire(
                document,
                sheet_plan.child_page,
                Wire {
                    id: deterministic_uuid(format!("zener:module-child-link:{key}")),
                    a: child_net,
                    b: child_port,
                    unsupported: Vec::new(),
                },
            )?;
            let mut child_label = Label::new(
                deterministic_uuid(format!("zener:module-child-port:{key}")),
                port_name,
                child_port,
            );
            child_label.kind = LabelKind::Hierarchical {
                shape: LabelShape::Bidirectional,
            };
            child_label.spin = LabelSpin::Right;
            upsert_label(document, sheet_plan.child_page, child_label)?;
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
    update: ConnectivityUpdate,
) -> Result<()> {
    let anchors_by_net = connectivity_targets(netlist, placed, target_nets)?;
    sync_net_drivers(document, &anchors_by_net, net_symbol_specs, update)?;

    let interface_ports = match &netlist.root_ref {
        Some(root) => root_interface::ports_by_net(netlist, root)?,
        None => BTreeMap::new(),
    }
    .into_iter()
    .filter(|(net_name, _)| target_nets.contains(net_name))
    .flat_map(|(net_name, port_names)| {
        port_names
            .into_iter()
            .map(move |port_name| (net_name.clone(), port_name))
    })
    .collect::<Vec<_>>();
    if update == ConnectivityUpdate::ExistingOnly
        && !interface_ports.iter().any(|(net_name, port_name)| {
            [
                deterministic_uuid(format!("zener:interface-port:{port_name}")),
                deterministic_uuid(format!("zener:interface-net:{net_name}:{port_name}")),
                deterministic_uuid(format!("zener:interface-global:{net_name}:{port_name}")),
                deterministic_uuid(format!("zener:interface-link:{net_name}:{port_name}")),
            ]
            .iter()
            .any(|id| contains_id(document, id))
        })
    {
        return Ok(());
    }
    let interface_anchors = interface_anchors(
        document,
        placed,
        &anchors_by_net,
        root_page,
        &interface_ports,
    )?;
    for ((net_name, port_name), (net_anchor, hierarchical_anchor)) in
        interface_ports.into_iter().zip(interface_anchors)
    {
        let hierarchical_id = deterministic_uuid(format!("zener:interface-port:{port_name}"));
        let net_label_id =
            deterministic_uuid(format!("zener:interface-net:{net_name}:{port_name}"));
        let legacy_global_id =
            deterministic_uuid(format!("zener:interface-global:{net_name}:{port_name}"));
        let wire_id = deterministic_uuid(format!("zener:interface-link:{net_name}:{port_name}"));
        let migrate_existing = [
            hierarchical_id.as_str(),
            net_label_id.as_str(),
            legacy_global_id.as_str(),
            wire_id.as_str(),
        ]
        .into_iter()
        .any(|id| contains_id(document, id));
        if update == ConnectivityUpdate::ExistingOnly && !migrate_existing {
            continue;
        }
        let mut net_label = Label::new(net_label_id, &net_name, net_anchor);
        net_label.spin = LabelSpin::Left;
        upsert_label(document, root_page, net_label)?;
        remove_label_by_id(document, &legacy_global_id);
        upsert_wire(
            document,
            root_page,
            Wire {
                id: wire_id,
                a: net_anchor,
                b: hierarchical_anchor,
                unsupported: Vec::new(),
            },
        )?;
        let mut label = Label::new(hierarchical_id, port_name, hierarchical_anchor);
        label.kind = LabelKind::Hierarchical {
            shape: LabelShape::Bidirectional,
        };
        label.spin = LabelSpin::Right;
        upsert_label(document, root_page, label)?;

        remove_label_by_id(
            document,
            &deterministic_uuid(format!("zener:interface-net:{net_name}")),
        );
    }
    Ok(())
}

fn sync_net_drivers(
    document: &mut SchDocument,
    targets_by_net: &BTreeMap<String, Vec<PinTarget>>,
    net_symbol_specs: &BTreeMap<String, net_symbols::NetSymbolSpec>,
    update: ConnectivityUpdate,
) -> Result<()> {
    // Adopt any exact existing power symbol or user label. For an unnamed
    // island, create the Zener net's preferred power symbol or a local label.
    let generated_ids = targets_by_net
        .iter()
        .flat_map(|(net_name, targets)| {
            targets.iter().flat_map(move |target| {
                [target.label_id(net_name), target.legacy_label_id(net_name)]
            })
        })
        .collect::<BTreeSet<_>>();
    let observed = reduce_with_provenance(document, PinVisibility::VisibleOnly)?;

    for (net_name, targets) in targets_by_net {
        for target in targets.iter().filter(|target| target.hidden) {
            remove_label_by_id(document, &target.label_id(net_name));
            remove_label_by_id(document, &target.legacy_label_id(net_name));
        }

        let mut targets_by_island = BTreeMap::<_, Vec<usize>>::new();
        for (target_index, target) in targets.iter().enumerate() {
            if target.hidden {
                continue;
            }
            let terminal = target.terminal();
            let islands = observed
                .islands
                .iter()
                .filter(|(_, provenance)| {
                    provenance
                        .terminals
                        .iter()
                        .any(|found| terminals_match(&terminal, found))
                })
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

        for (island, target_indices) in targets_by_island {
            let provenance = &observed.islands[&island];
            let has_user_driver = provenance
                .named_drivers
                .get(net_name)
                .is_some_and(|drivers| {
                    drivers.iter().any(|driver| match driver {
                        ConnectivityItemRef::Label { id, .. } => !generated_ids.contains(id),
                        _ => true,
                    })
                });
            let canonical = *target_indices
                .iter()
                .min_by_key(|index| (&targets[**index].slot, &targets[**index].number))
                .expect("a terminal island has a target");
            let had_generated_label = target_indices.iter().any(|index| {
                let target = &targets[*index];
                contains_id(document, &target.label_id(net_name))
                    || contains_id(document, &target.legacy_label_id(net_name))
            });

            for target_index in target_indices {
                let target = &targets[target_index];
                if has_user_driver || target_index != canonical {
                    remove_label_by_id(document, &target.label_id(net_name));
                }
                remove_label_by_id(document, &target.legacy_label_id(net_name));
            }

            if !has_user_driver
                && (update == ConnectivityUpdate::InsertMissing || had_generated_label)
            {
                let target = &targets[canonical];
                if let Some(spec) = net_symbol_specs.get(net_name) {
                    remove_label_by_id(document, &target.label_id(net_name));
                    let symbol = build_net_symbol(
                        spec,
                        net_name,
                        target.net_symbol_id(net_name),
                        target.point,
                    )?;
                    insert_net_symbol(document, target.page_index, symbol, &spec.definition)?;
                } else {
                    let mut label = Label::new(target.label_id(net_name), net_name, target.point);
                    label.spin = target.spin;
                    upsert_label(document, target.page_index, label)?;
                }
            }
        }
    }

    Ok(())
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

fn interface_anchors(
    document: &SchDocument,
    placed: &BTreeMap<SymbolSlotKey, PlacedSymbol>,
    anchors_by_net: &BTreeMap<String, Vec<PinTarget>>,
    root_page: usize,
    ports: &[(String, String)],
) -> Result<Vec<(Point, Point)>> {
    if ports.is_empty() {
        return Ok(Vec::new());
    }
    let mut packer = GridPacker::for_page(&document.pages[root_page].paper)?;
    occupy_page_items(&mut packer, &document.pages[root_page])?;
    for item in placed
        .values()
        .filter(|placed| placed.page_index == root_page)
    {
        let envelope = component_envelope(item, anchors_by_net)?
            .translated(item.symbol.at.x, item.symbol.at.y);
        packer.occupy(GridRect::from_bounds(envelope));
    }

    let label_height = crate::TextEffects::default().font_size.y.abs();
    let mut result = Vec::with_capacity(ports.len());
    for (net_name, port_name) in ports {
        let net_label_width = estimated_label_width(net_name);
        let hierarchical_width = estimated_shaped_label_width(port_name);
        let relative = GridRect::from_bounds(
            field_autoplace::Bounds::from_points([
                Point::new(-net_label_width, -label_height * 0.5),
                Point::new(
                    INTERFACE_STUB_LENGTH_MM + hierarchical_width,
                    label_height * 0.5,
                ),
            ])
            .expect("interface label pair has two corners"),
        );
        let anchor = packer.place(relative);
        let net_anchor = anchor.to_point();
        let hierarchical_anchor = GridPoint {
            x: anchor.x + INTERFACE_STUB_CELLS,
            y: anchor.y,
        }
        .to_point();
        result.push((net_anchor, hierarchical_anchor));
    }
    Ok(result)
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

fn remove_label_by_id(document: &mut SchDocument, id: &str) {
    for page in &mut document.pages {
        page.items
            .retain(|item| !matches!(item, SchItem::Label(label) if label.id == id));
    }
}

fn upsert_label(document: &mut SchDocument, page_index: usize, label: Label) -> Result<()> {
    let mut found = Vec::new();
    for (found_page, page) in document.pages.iter().enumerate() {
        for (item_index, item) in page.items.iter().enumerate() {
            if item.id() == Some(label.id.as_str()) {
                found.push((found_page, item_index, matches!(item, SchItem::Label(_))));
            }
        }
    }
    match found.as_slice() {
        [] => {}
        [(found_page, item_index, true)] => {
            let SchItem::Label(mut existing) =
                document.pages[*found_page].items.remove(*item_index)
            else {
                unreachable!("matched label changed before update")
            };
            if *found_page == page_index
                && existing.text == label.text
                && existing.at == label.at
                && existing.kind == label.kind
                && existing.spin == label.spin
            {
                document.pages[*found_page]
                    .items
                    .insert(*item_index, SchItem::Label(existing));
                return Ok(());
            }
            existing.text = label.text;
            existing.at = label.at;
            existing.kind = label.kind;
            existing.spin = label.spin;
            document.pages[page_index]
                .items
                .push(SchItem::Label(existing));
            return Ok(());
        }
        [(_, _, false)] => bail!(
            "generated label UUID '{}' is already used by another schematic item",
            label.id
        ),
        _ => bail!(
            "generated label UUID '{}' occurs more than once in the schematic",
            label.id
        ),
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

fn upsert_wire(document: &mut SchDocument, page_index: usize, wire: Wire) -> Result<()> {
    let mut found = Vec::new();
    for (found_page, page) in document.pages.iter().enumerate() {
        for (item_index, item) in page.items.iter().enumerate() {
            if item.id() == Some(wire.id.as_str()) {
                found.push((found_page, item_index, matches!(item, SchItem::Wire(_))));
            }
        }
    }
    match found.as_slice() {
        [] => {}
        [(found_page, item_index, true)] => {
            let SchItem::Wire(mut existing) = document.pages[*found_page].items.remove(*item_index)
            else {
                unreachable!("matched wire changed before update")
            };
            if *found_page == page_index && existing.a == wire.a && existing.b == wire.b {
                document.pages[*found_page]
                    .items
                    .insert(*item_index, SchItem::Wire(existing));
                return Ok(());
            }
            existing.a = wire.a;
            existing.b = wire.b;
            document.pages[page_index]
                .items
                .push(SchItem::Wire(existing));
            return Ok(());
        }
        [(_, _, false)] => bail!(
            "generated wire UUID '{}' is already used by another schematic item",
            wire.id
        ),
        _ => bail!(
            "generated wire UUID '{}' occurs more than once in the schematic",
            wire.id
        ),
    }
    document.pages[page_index].items.push(SchItem::Wire(wire));
    Ok(())
}

fn resolve_pin_targets(
    placed: &BTreeMap<SymbolSlotKey, PlacedSymbol>,
    component_path: &str,
    pin_name: &str,
    pin_numbers: &BTreeSet<String>,
) -> Result<Vec<PinTarget>> {
    let mut matches = Vec::new();
    for (slot, placed) in placed
        .iter()
        .filter(|(slot, _)| slot.component_path() == component_path)
    {
        let parsed = symbol::ParsedSymbolDefinition::parse(&placed.definition)?;
        for pin in parsed.placed_pins(&placed.symbol)? {
            let matches_name = !pin_name.is_empty() && !pin.name.is_empty() && pin.name == pin_name;
            if matches_name || !pin.numbers.is_disjoint(pin_numbers) {
                matches.push(PinTarget {
                    page_index: placed.page_index,
                    slot: slot.clone(),
                    pin_name: pin.name,
                    number: pin.number,
                    pin_numbers: pin.numbers,
                    point: pin.point,
                    spin: pin.outward_spin,
                    hidden: pin.hidden,
                });
            }
        }
    }
    if matches.is_empty() {
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
        _ => bail!(
            "netlist terminal '{}.{}' matches more than one KiCad symbol pin",
            component_path,
            pin_name
        ),
    }
}
