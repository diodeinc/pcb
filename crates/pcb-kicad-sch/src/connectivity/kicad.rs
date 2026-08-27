use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use anyhow::{Context, Result, anyhow, bail};

use super::{
    ComponentIdentity, ComponentNode, ComponentOrigin, ConnectionGroup, ConnectionOrigin,
    ConnectivityGraph, IslandRef, SymbolLocation, Terminal,
};
use crate::{
    Label, LabelKind, Point, SchDocument, SchItem, SchPage, Symbol, SymbolSlotKey,
    identity::normalize_schematic_path,
    symbol::{self, PowerScope},
};

const SCH_IU_PER_MM: f64 = 10_000.0;

/// Controls whether invisible KiCad symbol pins participate in reduction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinVisibility {
    IncludeHidden,
    VisibleOnly,
}

impl PinVisibility {
    fn includes(self, hidden: bool) -> bool {
        self == Self::IncludeHidden || !hidden
    }
}

/// KiCad connectivity plus the physical items that formed each island.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhysicalConnectivity {
    pub graph: ConnectivityGraph,
    pub islands: BTreeMap<IslandRef, PhysicalIsland>,
}

/// Physical provenance for one connected KiCad geometry island.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PhysicalIsland {
    pub items: BTreeSet<ConnectivityItemRef>,
    pub named_drivers: BTreeMap<String, BTreeSet<ConnectivityItemRef>>,
    pub names: BTreeSet<String>,
    pub terminals: BTreeSet<Terminal>,
    pub(crate) pins: BTreeSet<PhysicalPinRef>,
}

/// Exact identity of one placed KiCad symbol pin.
///
/// This stays separate from [`Terminal`]: terminals describe semantic
/// equivalence, while reconciliation needs to distinguish physical pins that
/// share one logical name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct PhysicalPinRef {
    page_id: String,
    symbol_id: String,
    number: String,
    point: GridPoint,
}

impl PhysicalPinRef {
    pub(crate) fn new(
        page_id: impl Into<String>,
        symbol_id: impl Into<String>,
        number: impl Into<String>,
        point: Point,
    ) -> Self {
        Self {
            page_id: page_id.into(),
            symbol_id: symbol_id.into(),
            number: number.into(),
            point: point.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConnectivityItemRef {
    Symbol {
        page_id: String,
        id: String,
    },
    Wire {
        page_id: String,
        id: String,
    },
    Junction {
        page_id: String,
        id: String,
    },
    NoConnect {
        page_id: String,
        id: String,
    },
    Label {
        page_id: String,
        id: String,
    },
    SheetPin {
        page_id: String,
        sheet_id: String,
        pin_id: String,
    },
}

pub(crate) fn reduce_with_provenance(
    document: &SchDocument,
    pin_visibility: PinVisibility,
) -> Result<PhysicalConnectivity> {
    let mut components = Vec::new();
    let mut groups = Vec::new();
    let mut islands = BTreeMap::new();
    let instances = page_instances(document)?;
    let instance_counts = instances
        .iter()
        .fold(BTreeMap::new(), |mut counts, instance| {
            *counts.entry(instance.page.id.as_str()).or_insert(0usize) += 1;
            counts
        });
    let mut parsed_definitions =
        BTreeMap::<String, BTreeMap<String, symbol::ParsedSymbolDefinition>>::new();
    for instance in instances {
        let definitions = parsed_definitions
            .entry(instance.page.id.clone())
            .or_default();
        let reduced = reduce_page(
            &instance,
            instance_counts[instance.page.id.as_str()] > 1,
            definitions,
            pin_visibility,
        )?;
        components.extend(reduced.components);
        for group in &reduced.groups {
            islands.insert(group.island.clone(), group.provenance.clone());
        }
        groups.extend(reduced.groups);
    }
    components.sort();
    Ok(PhysicalConnectivity {
        graph: ConnectivityGraph {
            components,
            groups: merge_scoped_groups(groups),
        },
        islands,
    })
}

struct PageInstance<'a> {
    page: &'a SchPage,
    id: String,
    child_ids: BTreeMap<String, String>,
}

fn page_instances(document: &SchDocument) -> Result<Vec<PageInstance<'_>>> {
    if document.root_page_ids.is_empty() {
        bail!("schematic document has no explicit root pages");
    }
    let mut by_file = BTreeMap::new();
    for page in &document.pages {
        let Some(file_name) = page.file_name.as_deref() else {
            continue;
        };
        let file_name = normalize_file_name(file_name);
        if by_file.insert(file_name.clone(), page).is_some() {
            bail!("multiple schematic pages use file name {file_name}");
        }
    }
    let mut by_id = BTreeMap::new();
    for page in &document.pages {
        if by_id.insert(page.id.as_str(), page).is_some() {
            bail!("multiple schematic pages use id {}", page.id);
        }
    }

    let mut instances = Vec::new();
    let mut root_ids = BTreeSet::new();
    for root_id in &document.root_page_ids {
        if !root_ids.insert(root_id) {
            bail!("root page {root_id} is listed more than once");
        }
        let root = by_id
            .get(root_id.as_str())
            .copied()
            .with_context(|| format!("root page {root_id} is not present in the document"))?;
        collect_page_instances(
            root,
            root.id.clone(),
            &by_file,
            &mut BTreeSet::new(),
            &mut instances,
        )?;
    }
    Ok(instances)
}

fn collect_page_instances<'a>(
    page: &'a SchPage,
    id: String,
    by_file: &BTreeMap<String, &'a SchPage>,
    active_files: &mut BTreeSet<String>,
    instances: &mut Vec<PageInstance<'a>>,
) -> Result<()> {
    let file_name = page.file_name.as_deref().map(normalize_file_name);
    if let Some(file_name) = &file_name
        && !active_files.insert(file_name.clone())
    {
        bail!("recursive schematic hierarchy through {file_name}");
    }

    let mut child_ids = BTreeMap::new();
    let children = page
        .items
        .iter()
        .filter_map(|item| match item {
            SchItem::Sheet(sheet) => Some(sheet),
            _ => None,
        })
        .map(|sheet| {
            let child_file = resolve_file_name(page, sheet.file_name());
            let child_page = by_file.get(&child_file).copied().ok_or_else(|| {
                anyhow!(
                    "sheet {} on page {} references missing page {child_file}",
                    sheet.id,
                    page.id
                )
            })?;
            let child_id = format!("{id}/{}", sheet.id);
            if child_ids
                .insert(sheet.id.clone(), child_id.clone())
                .is_some()
            {
                bail!("page {} contains duplicate sheet id {}", page.id, sheet.id);
            }
            Ok((child_page, child_id))
        })
        .collect::<Result<Vec<_>>>()?;
    instances.push(PageInstance {
        page,
        id,
        child_ids,
    });
    for (child, child_id) in children {
        collect_page_instances(child, child_id, by_file, active_files, instances)?;
    }
    if let Some(file_name) = file_name {
        active_files.remove(&file_name);
    }
    Ok(())
}

fn normalize_file_name(name: &str) -> String {
    normalize_schematic_path(Path::new(name))
        .to_string_lossy()
        .replace('\\', "/")
}

pub(crate) fn resolve_file_name(parent: &SchPage, child: &str) -> String {
    let child = Path::new(child);
    let path = if child.is_absolute() {
        child.to_path_buf()
    } else {
        parent
            .file_name
            .as_deref()
            .and_then(|name| Path::new(name).parent())
            .unwrap_or_else(|| Path::new(""))
            .join(child)
    };
    normalize_schematic_path(&path)
        .to_string_lossy()
        .replace('\\', "/")
}

struct ReducedPage {
    components: Vec<ComponentNode>,
    groups: Vec<ScopedConnectionGroup>,
}

fn reduce_page(
    instance: &PageInstance<'_>,
    repeated_page: bool,
    symbol_definitions: &mut BTreeMap<String, symbol::ParsedSymbolDefinition>,
    pin_visibility: PinVisibility,
) -> Result<ReducedPage> {
    let page = instance.page;
    let mut components = Vec::new();
    let mut connectables = Vec::new();
    let mut symbol_ids = BTreeSet::new();
    for item in &page.items {
        match item {
            SchItem::Symbol(placed) => {
                if !symbol_ids.insert(&placed.id) {
                    bail!(
                        "page {} contains duplicate symbol id {}",
                        page.id,
                        placed.id
                    );
                }
                let definition =
                    cached_symbol_definition(page, &instance.id, placed, symbol_definitions)?;
                collect_symbol(
                    page,
                    &instance.id,
                    repeated_page,
                    placed,
                    definition,
                    pin_visibility,
                    &mut SymbolConnectivity {
                        components: &mut components,
                        connectables: &mut connectables,
                    },
                )?;
            }
            SchItem::Wire(wire) => connectables.push(Connectable {
                geometry: Geometry::Segment(Segment {
                    a: wire.a.into(),
                    b: wire.b.into(),
                }),
                driver: None,
                terminal: None,
                pin: None,
                hierarchy: None,
                internal_links: BTreeSet::new(),
                source: Some(ConnectivityItemRef::Wire {
                    page_id: page.id.clone(),
                    id: wire.id.clone(),
                }),
            }),
            SchItem::Label(label) => {
                collect_label(&page.id, label, instance.id == page.id, &mut connectables)?
            }
            SchItem::Junction(junction) => connectables.push(Connectable {
                geometry: Geometry::Point {
                    at: junction.at.into(),
                    segment_interior_tolerance: Some(0),
                },
                driver: None,
                terminal: None,
                pin: None,
                hierarchy: None,
                internal_links: BTreeSet::new(),
                source: Some(ConnectivityItemRef::Junction {
                    page_id: page.id.clone(),
                    id: junction.id.clone(),
                }),
            }),
            SchItem::NoConnect(no_connect) => connectables.push(Connectable {
                geometry: Geometry::Point {
                    at: no_connect.at.into(),
                    segment_interior_tolerance: None,
                },
                driver: None,
                terminal: None,
                pin: None,
                hierarchy: None,
                internal_links: BTreeSet::new(),
                source: Some(ConnectivityItemRef::NoConnect {
                    page_id: page.id.clone(),
                    id: no_connect.id.clone(),
                }),
            }),
            SchItem::Sheet(sheet) => {
                let child_id = instance.child_ids.get(&sheet.id).with_context(|| {
                    format!(
                        "sheet {} on page {} has no hierarchy instance",
                        sheet.id, page.id
                    )
                })?;
                for pin in &sheet.pins {
                    let name = static_net_text("sheet pin", &pin.name)?;
                    connectables.push(Connectable {
                        geometry: Geometry::Point {
                            at: pin.at.into(),
                            segment_interior_tolerance: None,
                        },
                        driver: Some(NameDriver {
                            name: name.clone(),
                            kind: DriverKind::Local,
                            role: DriverNameRole::HierarchyAlias,
                            merge_by_name: false,
                        }),
                        terminal: None,
                        pin: None,
                        hierarchy: Some(HierarchyEndpoint::Child {
                            instance_id: child_id.clone(),
                            name,
                        }),
                        internal_links: BTreeSet::new(),
                        source: Some(ConnectivityItemRef::SheetPin {
                            page_id: page.id.clone(),
                            sheet_id: sheet.id.clone(),
                            pin_id: pin.id.clone(),
                        }),
                    });
                }
            }
            SchItem::Unsupported(sexpr) => {
                let tag = sexpr
                    .as_list()
                    .and_then(|items| items.first())
                    .and_then(pcb_sexpr::Sexpr::as_sym);
                if matches!(tag, Some("bus" | "bus_entry" | "bus_alias")) {
                    bail!("KiCad bus connectivity is not supported");
                }
            }
        }
    }

    let mut union_find = UnionFind::new(connectables.len());
    union_internal_connections(&connectables, &mut union_find);
    union_touching(&connectables, &mut union_find);
    resolve_legacy_power_drivers(&mut connectables, &mut union_find);
    union_same_page_drivers(&connectables, &mut union_find);
    Ok(ReducedPage {
        components,
        groups: connection_groups(&instance.id, connectables, union_find),
    })
}

fn cached_symbol_definition<'a>(
    page: &SchPage,
    page_instance_id: &str,
    placed: &Symbol,
    parsed: &'a mut BTreeMap<String, symbol::ParsedSymbolDefinition>,
) -> Result<&'a symbol::ParsedSymbolDefinition> {
    match parsed.entry(placed.lib_id.clone()) {
        std::collections::btree_map::Entry::Occupied(entry) => Ok(entry.into_mut()),
        std::collections::btree_map::Entry::Vacant(entry) => {
            let definition = page
                .library
                .definitions
                .get(&placed.lib_id)
                .with_context(|| {
                    format!(
                        "symbol {} on page instance {page_instance_id} has no cached definition {}",
                        placed.id, placed.lib_id
                    )
                })?;
            Ok(entry.insert(symbol::ParsedSymbolDefinition::parse(definition)?))
        }
    }
}

fn collect_symbol(
    page: &SchPage,
    page_instance_id: &str,
    repeated_page: bool,
    placed: &Symbol,
    definition: &symbol::ParsedSymbolDefinition,
    pin_visibility: PinVisibility,
    output: &mut SymbolConnectivity<'_>,
) -> Result<()> {
    let pins = definition.placed_pins(placed).with_context(|| {
        format!(
            "failed to resolve symbol {} on page instance {page_instance_id}",
            placed.id
        )
    })?;
    if let Some(power_scope) = definition.power_scope() {
        let net_name = placed
            .field_value("Value")
            .filter(|value| !value.is_empty())
            .with_context(|| format!("power symbol {} has no Value", placed.id))?;
        let net_name = static_net_text("power symbol value", net_name)?;
        for pin in pins
            .into_iter()
            .filter(|pin| pin_visibility.includes(pin.hidden))
            .filter(|pin| pin.is_power_input())
        {
            output.connectables.push(Connectable {
                geometry: Geometry::Point {
                    at: pin.point.into(),
                    segment_interior_tolerance: None,
                },
                driver: Some(NameDriver {
                    name: net_name.clone(),
                    kind: match power_scope {
                        PowerScope::Local => DriverKind::Local,
                        PowerScope::Global => DriverKind::Global,
                    },
                    role: DriverNameRole::NetName,
                    merge_by_name: true,
                }),
                terminal: None,
                pin: None,
                hierarchy: None,
                internal_links: BTreeSet::new(),
                source: Some(ConnectivityItemRef::Symbol {
                    page_id: page.id.clone(),
                    id: placed.id.clone(),
                }),
            });
        }
        return Ok(());
    }

    let component_path = placed.field_value("Path").filter(|path| !path.is_empty());
    let slot = component_path.and_then(|path| SymbolSlotKey::new(path, placed.unit));
    if repeated_page && component_path.is_some() {
        bail!(
            "managed symbol {} on repeated page {} has one Path value for multiple sheet instances",
            placed.id,
            page.id
        );
    }
    // Locations address the symbol in the file, so they use the page's own id
    // like every other ConnectivityItemRef — never the page-instance id.
    // (An unmanaged symbol on a repeated page therefore shares one identity
    // across instances, matching how wires and labels are already keyed.)
    let location = SymbolLocation {
        page_id: page.id.clone(),
        symbol_id: placed.id.clone(),
    };
    output.components.push(ComponentNode {
        managed_slot: slot,
        origin: ComponentOrigin::KiCad(location.clone()),
    });

    let component = component_path.map_or_else(
        || ComponentIdentity::KiCadSymbol(location),
        |path| ComponentIdentity::ManagedPath(path.to_string()),
    );
    let duplicate_numbers = definition.duplicate_pin_numbers_are_jumpers();
    let jumper_groups = definition.jumper_pin_groups();
    for pin in pins
        .into_iter()
        .filter(|pin| pin_visibility.includes(pin.hidden))
    {
        let pin_name = static_net_text("symbol pin name", &pin.name)?;
        output.connectables.push(Connectable {
            geometry: Geometry::Point {
                at: pin.point.into(),
                segment_interior_tolerance: None,
            },
            driver: pin.is_hidden_power_input().then(|| NameDriver {
                name: pin_name.clone(),
                kind: DriverKind::LegacyGlobal,
                role: DriverNameRole::NetName,
                merge_by_name: true,
            }),
            terminal: Some(Terminal::ComponentPin {
                component: component.clone(),
                pin_name,
                pin_numbers: pin.numbers.clone(),
            }),
            pin: Some(PhysicalPinRef::new(
                &page.id,
                &placed.id,
                &pin.number,
                pin.point,
            )),
            hierarchy: None,
            internal_links: internal_link_keys(
                placed,
                &pin.numbers,
                duplicate_numbers,
                jumper_groups,
            ),
            source: None,
        });
    }
    Ok(())
}

struct SymbolConnectivity<'a> {
    components: &'a mut Vec<ComponentNode>,
    connectables: &'a mut Vec<Connectable>,
}

fn collect_label(
    page_id: &str,
    label: &Label,
    expose_hierarchical_terminal: bool,
    connectables: &mut Vec<Connectable>,
) -> Result<()> {
    if matches!(label.kind, LabelKind::Directive { .. }) {
        connectables.push(Connectable {
            geometry: Geometry::Point {
                at: label.at.into(),
                segment_interior_tolerance: Some(1),
            },
            driver: None,
            terminal: None,
            pin: None,
            hierarchy: None,
            internal_links: BTreeSet::new(),
            source: Some(ConnectivityItemRef::Label {
                page_id: page_id.to_string(),
                id: label.id.clone(),
            }),
        });
        return Ok(());
    }
    let name = static_net_text("label", &label.text)?;
    if name.is_empty() {
        return Ok(());
    }
    let (kind, role, terminal) = match label.kind {
        LabelKind::Local => (DriverKind::Local, DriverNameRole::NetName, None),
        LabelKind::Global { .. } => (DriverKind::Global, DriverNameRole::NetName, None),
        LabelKind::Hierarchical { .. } => (
            DriverKind::Local,
            DriverNameRole::HierarchyAlias,
            expose_hierarchical_terminal.then(|| Terminal::InterfacePort { name: name.clone() }),
        ),
        LabelKind::Directive { .. } => unreachable!(),
    };
    connectables.push(Connectable {
        geometry: Geometry::Point {
            at: label.at.into(),
            // KiCad's label dangling-state test uses TestSegmentHit with a
            // one-IU tolerance to account for coordinate rounding.
            segment_interior_tolerance: Some(1),
        },
        driver: Some(NameDriver {
            name: name.clone(),
            kind,
            role,
            merge_by_name: true,
        }),
        terminal,
        pin: None,
        hierarchy: matches!(label.kind, LabelKind::Hierarchical { .. })
            .then(|| HierarchyEndpoint::Parent { name }),
        internal_links: BTreeSet::new(),
        source: Some(ConnectivityItemRef::Label {
            page_id: page_id.to_string(),
            id: label.id.clone(),
        }),
    });
    Ok(())
}

fn static_net_text(field: &str, value: &str) -> Result<String> {
    let value = crate::kicad::unescape_text(value);
    let characters = value.chars().collect::<Vec<_>>();
    let mut output = String::with_capacity(value.len());
    let mut index = 0;
    while index < characters.len() {
        let is_expression =
            matches!(characters[index], '$' | '@') && characters.get(index + 1) == Some(&'{');
        let is_escaped_expression = characters[index] == '\\'
            && matches!(characters.get(index + 1), Some('$' | '@'))
            && characters.get(index + 2) == Some(&'{');
        if is_expression {
            bail!("{field} uses an unsupported KiCad text expression: {value}");
        }
        if is_escaped_expression {
            output.push(characters[index + 1]);
            output.push('{');
            index += 3;
            let mut depth = 1;
            while index < characters.len() && depth > 0 {
                match characters[index] {
                    '{' => depth += 1,
                    '}' => depth -= 1,
                    _ => {}
                }
                output.push(characters[index]);
                index += 1;
            }
            continue;
        }
        if !matches!(characters[index], '\n' | '\r') {
            output.push(characters[index]);
        }
        index += 1;
    }
    Ok(output)
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Geometry {
    Point {
        at: GridPoint,
        segment_interior_tolerance: Option<i64>,
    },
    Segment(Segment),
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Segment {
    a: GridPoint,
    b: GridPoint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct GridPoint {
    x: i64,
    y: i64,
}

impl From<Point> for GridPoint {
    fn from(point: Point) -> Self {
        Self {
            x: (point.x * SCH_IU_PER_MM).round() as i64,
            y: (point.y * SCH_IU_PER_MM).round() as i64,
        }
    }
}

#[derive(Debug, Clone)]
struct Connectable {
    geometry: Geometry,
    driver: Option<NameDriver>,
    terminal: Option<Terminal>,
    pin: Option<PhysicalPinRef>,
    hierarchy: Option<HierarchyEndpoint>,
    internal_links: BTreeSet<String>,
    source: Option<ConnectivityItemRef>,
}

#[derive(Debug, Clone)]
enum HierarchyEndpoint {
    Parent { name: String },
    Child { instance_id: String, name: String },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct NameDriver {
    name: String,
    kind: DriverKind,
    role: DriverNameRole,
    merge_by_name: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum DriverNameRole {
    NetName,
    HierarchyAlias,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum DriverKind {
    Local,
    Global,
    LegacyGlobal,
}

fn resolve_legacy_power_drivers(connectables: &mut [Connectable], union_find: &mut UnionFind) {
    // KiCad's generateGlobalPowerPinSubGraphs() suppresses the implicit global
    // connection for an invisible power-input pin on a non-power symbol when
    // ConnectedItems() is non-empty; ERC reports the wired legacy pin instead.
    // Modern power symbols take the separate PowerScope path above and always
    // retain their declared local or global drive.
    let mut group_sizes = BTreeMap::new();
    for index in 0..connectables.len() {
        *group_sizes.entry(union_find.find(index)).or_insert(0usize) += 1;
    }
    for (index, item) in connectables.iter_mut().enumerate() {
        let Some(driver) = &mut item.driver else {
            continue;
        };
        if driver.kind != DriverKind::LegacyGlobal {
            continue;
        }
        if group_sizes[&union_find.find(index)] == 1 {
            driver.kind = DriverKind::Global;
        } else {
            item.driver = None;
        }
    }
}

fn union_same_page_drivers(connectables: &[Connectable], union_find: &mut UnionFind) {
    let mut first = BTreeMap::<&str, usize>::new();
    for (index, item) in connectables.iter().enumerate() {
        let Some(driver) = &item.driver else {
            continue;
        };
        if !driver.merge_by_name {
            continue;
        }
        if let Some(previous) = first.insert(&driver.name, index) {
            union_find.union(previous, index);
        }
    }
}

fn union_internal_connections(connectables: &[Connectable], union_find: &mut UnionFind) {
    let mut first = BTreeMap::<&str, usize>::new();
    for (index, item) in connectables.iter().enumerate() {
        for link in &item.internal_links {
            if let Some(previous) = first.insert(link, index) {
                union_find.union(previous, index);
            }
        }
    }
}

fn internal_link_keys(
    symbol: &Symbol,
    pin_numbers: &BTreeSet<String>,
    duplicate_numbers: bool,
    jumper_groups: &[BTreeSet<String>],
) -> BTreeSet<String> {
    let mut links = BTreeSet::new();
    if duplicate_numbers {
        links.extend(
            pin_numbers
                .iter()
                .map(|number| format!("{}:number:{number}", symbol.id)),
        );
    }
    for (index, group) in jumper_groups.iter().enumerate() {
        if !group.is_disjoint(pin_numbers) {
            links.insert(format!("{}:jumper:{index}", symbol.id));
        }
    }
    links
}

fn union_touching(connectables: &[Connectable], union_find: &mut UnionFind) {
    let mut at_point = BTreeMap::<GridPoint, Vec<usize>>::new();
    let mut segments = Vec::new();
    for (index, item) in connectables.iter().enumerate() {
        match item.geometry {
            Geometry::Point { at, .. } => at_point.entry(at).or_default().push(index),
            Geometry::Segment(segment) => {
                at_point.entry(segment.a).or_default().push(index);
                at_point.entry(segment.b).or_default().push(index);
                segments.push((index, segment));
            }
        }
    }
    for indices in at_point.values() {
        if let Some((first, rest)) = indices.split_first() {
            for index in rest {
                union_find.union(*first, *index);
            }
        }
    }

    for (point_index, item) in connectables.iter().enumerate() {
        let Geometry::Point {
            at,
            segment_interior_tolerance: Some(tolerance),
        } = item.geometry
        else {
            continue;
        };
        for (segment_index, segment) in &segments {
            if point_near_segment(at, *segment, tolerance) {
                union_find.union(point_index, *segment_index);
            }
        }
    }
}

fn points_equal(a: GridPoint, b: GridPoint) -> bool {
    a == b
}

fn point_near_segment(point: GridPoint, segment: Segment, tolerance: i64) -> bool {
    let ab_x = i128::from(segment.b.x) - i128::from(segment.a.x);
    let ab_y = i128::from(segment.b.y) - i128::from(segment.a.y);
    let ap_x = i128::from(point.x) - i128::from(segment.a.x);
    let ap_y = i128::from(point.y) - i128::from(segment.a.y);
    if point.x < segment.a.x.min(segment.b.x) - tolerance
        || point.x > segment.a.x.max(segment.b.x) + tolerance
        || point.y < segment.a.y.min(segment.b.y) - tolerance
        || point.y > segment.a.y.max(segment.b.y) + tolerance
    {
        return false;
    }
    let length_squared = ab_x * ab_x + ab_y * ab_y;
    if length_squared == 0 {
        return points_equal(point, segment.a);
    }
    let projection = ap_x * ab_x + ap_y * ab_y;
    if projection < 0 || projection > length_squared {
        return false;
    }
    let cross = ab_x * ap_y - ab_y * ap_x;
    let threshold = i128::from(tolerance) + 1;
    cross * cross < threshold * threshold * length_squared
}

fn connection_groups(
    page_instance_id: &str,
    connectables: Vec<Connectable>,
    mut union_find: UnionFind,
) -> Vec<ScopedConnectionGroup> {
    let mut groups = BTreeMap::<usize, Vec<Connectable>>::new();
    for (index, item) in connectables.into_iter().enumerate() {
        groups.entry(union_find.find(index)).or_default().push(item);
    }
    groups
        .into_values()
        .enumerate()
        .filter_map(|(index, items)| {
            let mut names = BTreeSet::new();
            let mut global_names = BTreeSet::new();
            let mut terminals = BTreeSet::new();
            let mut hierarchical_ports = BTreeSet::new();
            let mut child_pins = BTreeSet::new();
            let mut provenance = PhysicalIsland::default();
            for item in items {
                if let Some(source) = &item.source {
                    provenance.items.insert(source.clone());
                }
                provenance.pins.extend(item.pin);
                if let Some(driver) = item.driver {
                    if driver.role == DriverNameRole::NetName {
                        names.insert(driver.name.clone());
                        if let Some(source) = &item.source {
                            provenance
                                .named_drivers
                                .entry(driver.name.clone())
                                .or_default()
                                .insert(source.clone());
                        }
                    }
                    if driver.kind == DriverKind::Global {
                        global_names.insert(driver.name);
                    }
                }
                terminals.extend(item.terminal);
                match item.hierarchy {
                    Some(HierarchyEndpoint::Parent { name }) => {
                        hierarchical_ports.insert(name);
                    }
                    Some(HierarchyEndpoint::Child { instance_id, name }) => {
                        child_pins.insert(SheetPinLink { instance_id, name });
                    }
                    None => {}
                }
            }
            if names.is_empty()
                && terminals.is_empty()
                && hierarchical_ports.is_empty()
                && child_pins.is_empty()
            {
                return None;
            }
            provenance.names = names.clone();
            provenance.terminals = terminals.clone();
            let island = IslandRef {
                page_id: page_instance_id.to_string(),
                index,
            };
            Some(ScopedConnectionGroup {
                island: island.clone(),
                provenance,
                global_names,
                hierarchical_ports,
                child_pins,
                group: ConnectionGroup {
                    names,
                    terminals,
                    origins: BTreeSet::from([ConnectionOrigin::KiCadIsland(island)]),
                },
            })
        })
        .collect()
}

struct ScopedConnectionGroup {
    island: IslandRef,
    provenance: PhysicalIsland,
    group: ConnectionGroup,
    global_names: BTreeSet<String>,
    hierarchical_ports: BTreeSet<String>,
    child_pins: BTreeSet<SheetPinLink>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SheetPinLink {
    instance_id: String,
    name: String,
}

fn merge_scoped_groups(groups: Vec<ScopedConnectionGroup>) -> Vec<ConnectionGroup> {
    let mut union_find = UnionFind::new(groups.len());
    let mut first_by_name = BTreeMap::<&str, usize>::new();
    for (index, group) in groups.iter().enumerate() {
        for name in &group.global_names {
            if let Some(previous) = first_by_name.insert(name, index) {
                union_find.union(previous, index);
            }
        }
    }
    let mut ports = BTreeMap::<(&str, &str), Vec<usize>>::new();
    for (index, group) in groups.iter().enumerate() {
        for name in &group.hierarchical_ports {
            ports
                .entry((group.island.page_id.as_str(), name))
                .or_default()
                .push(index);
        }
    }
    for (index, group) in groups.iter().enumerate() {
        for pin in &group.child_pins {
            if let Some(children) = ports.get(&(pin.instance_id.as_str(), pin.name.as_str())) {
                for child in children {
                    union_find.union(index, *child);
                }
            }
        }
    }
    let mut merged = BTreeMap::<usize, ConnectionGroup>::new();
    for (index, group) in groups.into_iter().enumerate() {
        let entry = merged.entry(union_find.find(index)).or_default();
        entry.names.extend(group.group.names);
        entry.terminals.extend(group.group.terminals);
        entry.origins.extend(group.group.origins);
    }
    merged
        .into_values()
        .filter(|group| !group.names.is_empty() || !group.terminals.is_empty())
        .collect()
}

struct UnionFind {
    parents: Vec<usize>,
    ranks: Vec<u8>,
}

impl UnionFind {
    fn new(size: usize) -> Self {
        Self {
            parents: (0..size).collect(),
            ranks: vec![0; size],
        }
    }

    fn find(&mut self, value: usize) -> usize {
        if self.parents[value] != value {
            self.parents[value] = self.find(self.parents[value]);
        }
        self.parents[value]
    }

    fn union(&mut self, a: usize, b: usize) {
        let mut a = self.find(a);
        let mut b = self.find(b);
        if a == b {
            return;
        }
        if self.ranks[a] < self.ranks[b] {
            std::mem::swap(&mut a, &mut b);
        }
        self.parents[b] = a;
        if self.ranks[a] == self.ranks[b] {
            self.ranks[a] += 1;
        }
    }
}
