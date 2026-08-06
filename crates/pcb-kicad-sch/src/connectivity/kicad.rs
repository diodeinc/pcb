use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
};

use super::{
    ComponentIdentity, ComponentNode, ComponentOrigin, ConnectionGroup, ConnectionOrigin,
    ConnectivityGraph, IslandRef, SymbolLocation, Terminal,
};
use crate::{
    Label, LabelKind, Point, SchDocument, SchItem, SchPage, Symbol, SymbolSlotKey,
    net_symbols::{self, PowerScope},
    symbol,
};

const CONNECT_EPS_MM: f64 = 1.0e-4;

pub(super) fn reduce(document: &SchDocument) -> ConnectivityGraph {
    let mut components = Vec::new();
    let mut groups = Vec::new();
    for instance in page_instances(document) {
        let reduced = reduce_page(document, &instance);
        components.extend(reduced.components);
        groups.extend(reduced.groups);
    }
    components.sort();
    ConnectivityGraph {
        components,
        groups: merge_scoped_groups(groups),
    }
}

struct PageInstance<'a> {
    page: &'a SchPage,
    id: String,
    child_ids: BTreeMap<String, String>,
}

fn page_instances(document: &SchDocument) -> Vec<PageInstance<'_>> {
    let by_file = document
        .pages
        .iter()
        .filter_map(|page| Some((normalize_file_name(page.file_name.as_deref()?), page)))
        .collect::<BTreeMap<_, _>>();
    let referenced_files = document
        .pages
        .iter()
        .flat_map(|page| {
            page.items.iter().filter_map(|item| match item {
                SchItem::Sheet(sheet) => Some(resolve_file_name(page, &sheet.file_name)),
                _ => None,
            })
        })
        .collect::<BTreeSet<_>>();
    let roots = document
        .pages
        .iter()
        .filter(|page| {
            page.file_name
                .as_deref()
                .is_none_or(|name| !referenced_files.contains(&normalize_file_name(name)))
        })
        .collect::<Vec<_>>();

    let mut instances = Vec::new();
    for root in roots {
        collect_page_instances(
            root,
            root.id.clone(),
            &by_file,
            &mut BTreeSet::new(),
            &mut instances,
        );
    }
    instances
}

fn collect_page_instances<'a>(
    page: &'a SchPage,
    id: String,
    by_file: &BTreeMap<String, &'a SchPage>,
    active_files: &mut BTreeSet<String>,
    instances: &mut Vec<PageInstance<'a>>,
) {
    let file_name = page.file_name.as_deref().map(normalize_file_name);
    if let Some(file_name) = &file_name
        && !active_files.insert(file_name.clone())
    {
        return;
    }

    let mut child_ids = BTreeMap::new();
    let children = page
        .items
        .iter()
        .filter_map(|item| match item {
            SchItem::Sheet(sheet) => Some(sheet),
            _ => None,
        })
        .filter_map(|sheet| {
            let child_page = by_file.get(&resolve_file_name(page, &sheet.file_name))?;
            let child_id = format!("{id}/{}", sheet.id);
            child_ids.insert(sheet.id.clone(), child_id.clone());
            Some((*child_page, child_id))
        })
        .collect::<Vec<_>>();
    instances.push(PageInstance {
        page,
        id,
        child_ids,
    });
    for (child, child_id) in children {
        collect_page_instances(child, child_id, by_file, active_files, instances);
    }
    if let Some(file_name) = file_name {
        active_files.remove(&file_name);
    }
}

fn normalize_file_name(name: &str) -> String {
    name.trim_start_matches("./").replace('\\', "/")
}

fn resolve_file_name(parent: &SchPage, child: &str) -> String {
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
    normalize_path(&path).to_string_lossy().replace('\\', "/")
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

struct ReducedPage {
    components: Vec<ComponentNode>,
    groups: Vec<ScopedConnectionGroup>,
}

fn reduce_page(document: &SchDocument, instance: &PageInstance<'_>) -> ReducedPage {
    let page = instance.page;
    let mut components = Vec::new();
    let mut connectables = Vec::new();
    for item in &page.items {
        match item {
            SchItem::Symbol(placed) => collect_symbol(
                document,
                &instance.id,
                placed,
                &mut components,
                &mut connectables,
            ),
            SchItem::Wire(wire) => connectables.push(Connectable {
                geometry: Geometry::Segment(Segment {
                    a: wire.a,
                    b: wire.b,
                }),
                driver: None,
                terminal: None,
                hierarchy: None,
                internal_links: BTreeSet::new(),
            }),
            SchItem::Label(label) => {
                collect_label(label, instance.id == page.id, &mut connectables)
            }
            SchItem::Junction(junction) => connectables.push(Connectable {
                geometry: Geometry::Point {
                    at: junction.at,
                    connects_to_segment_interior: true,
                },
                driver: None,
                terminal: None,
                hierarchy: None,
                internal_links: BTreeSet::new(),
            }),
            SchItem::NoConnect(no_connect) => connectables.push(Connectable {
                geometry: Geometry::Point {
                    at: no_connect.at,
                    connects_to_segment_interior: false,
                },
                driver: None,
                terminal: None,
                hierarchy: None,
                internal_links: BTreeSet::new(),
            }),
            SchItem::Sheet(sheet) => {
                let Some(child_id) = instance.child_ids.get(&sheet.id) else {
                    continue;
                };
                for pin in &sheet.pins {
                    connectables.push(Connectable {
                        geometry: Geometry::Point {
                            at: pin.at,
                            connects_to_segment_interior: false,
                        },
                        driver: Some(NameDriver {
                            name: pin.name.clone(),
                            kind: DriverKind::Hierarchical,
                            merge_by_name: false,
                        }),
                        terminal: None,
                        hierarchy: Some(HierarchyEndpoint::Child {
                            instance_id: child_id.clone(),
                            name: pin.name.clone(),
                        }),
                        internal_links: BTreeSet::new(),
                    });
                }
            }
            SchItem::Unsupported(_) => {}
        }
    }

    let mut union_find = UnionFind::new(connectables.len());
    union_same_page_drivers(&connectables, &mut union_find);
    union_internal_connections(&connectables, &mut union_find);
    union_touching(&connectables, &mut union_find);
    ReducedPage {
        components,
        groups: connection_groups(&instance.id, connectables, union_find),
    }
}

fn collect_symbol(
    document: &SchDocument,
    page_instance_id: &str,
    placed: &Symbol,
    components: &mut Vec<ComponentNode>,
    connectables: &mut Vec<Connectable>,
) {
    let definition = document.library.definitions.get(&placed.lib_id);
    if let Some((scope, net_name)) =
        definition.and_then(|definition| net_symbols::symbol_power_driver(placed, definition))
    {
        for pin in definition
            .into_iter()
            .flat_map(|definition| symbol::placed_pins(definition, placed, true))
        {
            connectables.push(Connectable {
                geometry: Geometry::Point {
                    at: pin.point,
                    connects_to_segment_interior: false,
                },
                driver: Some(NameDriver {
                    name: net_name.clone(),
                    kind: match scope {
                        PowerScope::Local => DriverKind::Local,
                        PowerScope::Global => DriverKind::Global,
                    },
                    merge_by_name: true,
                }),
                terminal: None,
                hierarchy: None,
                internal_links: BTreeSet::new(),
            });
        }
        return;
    }

    let component_path = placed
        .field_value("Path")
        .map(str::trim)
        .filter(|path| !path.is_empty());
    let slot = component_path.and_then(|path| SymbolSlotKey::new(path, placed.unit));
    let location = SymbolLocation {
        page_id: page_instance_id.to_string(),
        symbol_id: placed.id.clone(),
    };
    components.push(ComponentNode {
        managed_slot: slot,
        origin: ComponentOrigin::KiCad(location.clone()),
    });

    let Some(definition) = definition else {
        return;
    };
    let component = component_path.map_or_else(
        || ComponentIdentity::KiCadSymbol(location),
        |path| ComponentIdentity::ManagedPath(path.to_string()),
    );
    let duplicate_numbers = symbol::duplicate_pin_numbers_are_jumpers(definition);
    let jumper_groups = symbol::jumper_pin_groups(definition);
    for pin in symbol::placed_pins(definition, placed, true) {
        if pin.is_no_connect() {
            continue;
        }
        let mut pin_keys = BTreeSet::from([pin.number.clone()]);
        if !pin.name.trim().is_empty() {
            pin_keys.insert(pin.name.clone());
        }
        connectables.push(Connectable {
            geometry: Geometry::Point {
                at: pin.point,
                connects_to_segment_interior: false,
            },
            driver: pin.is_hidden_power_input().then(|| NameDriver {
                name: pin.name.clone(),
                kind: DriverKind::Global,
                merge_by_name: true,
            }),
            terminal: Some(Terminal::ComponentPin {
                component: component.clone(),
                pin_name: pin.name,
                pin_keys,
            }),
            hierarchy: None,
            internal_links: internal_link_keys(
                placed,
                &pin.number,
                duplicate_numbers,
                &jumper_groups,
            ),
        });
    }
}

fn collect_label(
    label: &Label,
    expose_hierarchical_terminal: bool,
    connectables: &mut Vec<Connectable>,
) {
    let name = label.text.trim();
    if name.is_empty() {
        return;
    }
    let (kind, terminal) = match label.kind {
        LabelKind::Local => (DriverKind::Local, None),
        LabelKind::Global { .. } => (DriverKind::Global, None),
        LabelKind::Hierarchical { .. } => (
            DriverKind::Hierarchical,
            expose_hierarchical_terminal.then(|| Terminal::HierarchicalPort {
                label_text: name.to_string(),
            }),
        ),
    };
    connectables.push(Connectable {
        geometry: Geometry::Point {
            at: label.at,
            connects_to_segment_interior: true,
        },
        driver: Some(NameDriver {
            name: name.to_string(),
            kind,
            merge_by_name: true,
        }),
        terminal,
        hierarchy: matches!(label.kind, LabelKind::Hierarchical { .. }).then(|| {
            HierarchyEndpoint::Parent {
                name: name.to_string(),
            }
        }),
        internal_links: BTreeSet::new(),
    });
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Geometry {
    Point {
        at: Point,
        connects_to_segment_interior: bool,
    },
    Segment(Segment),
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Segment {
    a: Point,
    b: Point,
}

#[derive(Debug, Clone)]
struct Connectable {
    geometry: Geometry,
    driver: Option<NameDriver>,
    terminal: Option<Terminal>,
    hierarchy: Option<HierarchyEndpoint>,
    internal_links: BTreeSet<String>,
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
    merge_by_name: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum DriverKind {
    Local,
    Hierarchical,
    Global,
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
    pin_number: &str,
    duplicate_numbers: bool,
    jumper_groups: &[BTreeSet<String>],
) -> BTreeSet<String> {
    let mut links = BTreeSet::new();
    if duplicate_numbers {
        links.insert(format!("{}:number:{pin_number}", symbol.id));
    }
    for (index, group) in jumper_groups.iter().enumerate() {
        if group.contains(pin_number) {
            links.insert(format!("{}:jumper:{index}", symbol.id));
        }
    }
    links
}

fn union_touching(connectables: &[Connectable], union_find: &mut UnionFind) {
    for a in 0..connectables.len() {
        for b in (a + 1)..connectables.len() {
            if geometries_touch(connectables[a].geometry, connectables[b].geometry) {
                union_find.union(a, b);
            }
        }
    }
}

fn geometries_touch(a: Geometry, b: Geometry) -> bool {
    match (a, b) {
        (Geometry::Point { at: a, .. }, Geometry::Point { at: b, .. }) => points_equal(a, b),
        (
            Geometry::Point {
                at,
                connects_to_segment_interior,
            },
            Geometry::Segment(segment),
        )
        | (
            Geometry::Segment(segment),
            Geometry::Point {
                at,
                connects_to_segment_interior,
            },
        ) => {
            points_equal(at, segment.a)
                || points_equal(at, segment.b)
                || (connects_to_segment_interior && point_on_segment(at, segment))
        }
        (Geometry::Segment(a), Geometry::Segment(b)) => {
            points_equal(a.a, b.a)
                || points_equal(a.a, b.b)
                || points_equal(a.b, b.a)
                || points_equal(a.b, b.b)
        }
    }
}

fn points_equal(a: Point, b: Point) -> bool {
    distance2(a, b) <= CONNECT_EPS_MM.powi(2)
}

fn point_on_segment(point: Point, segment: Segment) -> bool {
    distance2(point, closest_point(point, segment)) <= CONNECT_EPS_MM.powi(2)
}

fn closest_point(point: Point, segment: Segment) -> Point {
    let dx = segment.b.x - segment.a.x;
    let dy = segment.b.y - segment.a.y;
    let length2 = dx * dx + dy * dy;
    if length2 <= CONNECT_EPS_MM.powi(2) {
        return segment.a;
    }
    let t =
        (((point.x - segment.a.x) * dx + (point.y - segment.a.y) * dy) / length2).clamp(0.0, 1.0);
    Point::new(segment.a.x + t * dx, segment.a.y + t * dy)
}

fn distance2(a: Point, b: Point) -> f64 {
    (a.x - b.x).powi(2) + (a.y - b.y).powi(2)
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
            for item in items {
                if let Some(driver) = item.driver {
                    names.insert(driver.name.clone());
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
            if names.is_empty() && terminals.is_empty() {
                return None;
            }
            Some(ScopedConnectionGroup {
                global_names,
                page_instance_id: page_instance_id.to_string(),
                hierarchical_ports,
                child_pins,
                group: ConnectionGroup {
                    names,
                    terminals,
                    origins: BTreeSet::from([ConnectionOrigin::KiCadIsland(IslandRef {
                        page_id: page_instance_id.to_string(),
                        index,
                    })]),
                },
            })
        })
        .collect()
}

struct ScopedConnectionGroup {
    group: ConnectionGroup,
    global_names: BTreeSet<String>,
    page_instance_id: String,
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
                .entry((&group.page_instance_id, name))
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
        let entry = merged
            .entry(union_find.find(index))
            .or_insert_with(empty_connection_group);
        entry.names.extend(group.group.names);
        entry.terminals.extend(group.group.terminals);
        entry.origins.extend(group.group.origins);
    }
    merged.into_values().collect()
}

fn empty_connection_group() -> ConnectionGroup {
    ConnectionGroup {
        names: BTreeSet::new(),
        terminals: BTreeSet::new(),
        origins: BTreeSet::new(),
    }
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
