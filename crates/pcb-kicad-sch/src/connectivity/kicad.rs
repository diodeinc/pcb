use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};

use super::{
    ComponentIdentity, ComponentNode, ComponentOrigin, ConnectionGroup, ConnectionOrigin,
    ConnectivityGraph, IslandRef, SymbolLocation, Terminal,
};
use crate::{
    Label, LabelKind, Point, SchDocument, SchItem, SchPage, Symbol, SymbolSlotKey,
    symbol::{self, PowerScope},
};

const SCH_IU_PER_MM: f64 = 10_000.0;

pub(super) fn reduce(document: &SchDocument) -> Result<ConnectivityGraph> {
    let mut components = Vec::new();
    let mut groups = Vec::new();
    let instances = page_instances(document)?;
    let instance_counts = instances
        .iter()
        .fold(BTreeMap::new(), |mut counts, instance| {
            *counts.entry(instance.page.id.as_str()).or_insert(0usize) += 1;
            counts
        });
    for instance in instances {
        let reduced = reduce_page(&instance, instance_counts[instance.page.id.as_str()] > 1)?;
        components.extend(reduced.components);
        groups.extend(reduced.groups);
    }
    components.sort();
    Ok(ConnectivityGraph {
        components,
        groups: merge_scoped_groups(groups),
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
    normalize_path(Path::new(name))
        .to_string_lossy()
        .replace('\\', "/")
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
                if matches!(
                    normalized.components().next_back(),
                    Some(Component::Normal(_))
                ) {
                    normalized.pop();
                } else if !path.is_absolute() {
                    normalized.push("..");
                }
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

fn reduce_page(instance: &PageInstance<'_>, repeated_page: bool) -> Result<ReducedPage> {
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
                collect_symbol(
                    page,
                    &instance.id,
                    repeated_page,
                    placed,
                    &mut components,
                    &mut connectables,
                )?;
            }
            SchItem::Wire(wire) => connectables.push(Connectable {
                geometry: Geometry::Segment(Segment {
                    a: wire.a.into(),
                    b: wire.b.into(),
                }),
                driver: None,
                terminal: None,
                hierarchy: None,
                internal_links: BTreeSet::new(),
            }),
            SchItem::Label(label) => {
                collect_label(label, instance.id == page.id, &mut connectables)?
            }
            SchItem::Junction(junction) => connectables.push(Connectable {
                geometry: Geometry::Point {
                    at: junction.at.into(),
                    segment_interior_tolerance: Some(0),
                },
                driver: None,
                terminal: None,
                hierarchy: None,
                internal_links: BTreeSet::new(),
            }),
            SchItem::NoConnect(no_connect) => connectables.push(Connectable {
                geometry: Geometry::Point {
                    at: no_connect.at.into(),
                    segment_interior_tolerance: None,
                },
                driver: None,
                terminal: None,
                hierarchy: None,
                internal_links: BTreeSet::new(),
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
                            merge_by_name: false,
                        }),
                        terminal: None,
                        hierarchy: Some(HierarchyEndpoint::Child {
                            instance_id: child_id.clone(),
                            name,
                        }),
                        internal_links: BTreeSet::new(),
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

fn collect_symbol(
    page: &SchPage,
    page_instance_id: &str,
    repeated_page: bool,
    placed: &Symbol,
    components: &mut Vec<ComponentNode>,
    connectables: &mut Vec<Connectable>,
) -> Result<()> {
    let definition = page.library.definitions.get(&placed.lib_id);
    let definition = definition.with_context(|| {
        format!(
            "symbol {} on page instance {page_instance_id} has no cached definition {}",
            placed.id, placed.lib_id
        )
    })?;
    let pins = symbol::placed_pins(definition, placed, true).with_context(|| {
        format!(
            "failed to resolve symbol {} on page instance {page_instance_id}",
            placed.id
        )
    })?;
    let power_scope = symbol::power_scope(definition)?;
    if let Some(power_scope) = power_scope {
        let net_name = placed
            .field_value("Value")
            .filter(|value| !value.is_empty())
            .with_context(|| format!("power symbol {} has no Value", placed.id))?;
        let net_name = static_net_text("power symbol value", net_name)?;
        for pin in pins.into_iter().filter(|pin| pin.is_power_input()) {
            connectables.push(Connectable {
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
                    merge_by_name: true,
                }),
                terminal: None,
                hierarchy: None,
                internal_links: BTreeSet::new(),
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
    let location = SymbolLocation {
        page_id: page_instance_id.to_string(),
        symbol_id: placed.id.clone(),
    };
    components.push(ComponentNode {
        managed_slot: slot,
        origin: ComponentOrigin::KiCad(location.clone()),
    });

    let component = component_path.map_or_else(
        || ComponentIdentity::KiCadSymbol(location),
        |path| ComponentIdentity::ManagedPath(path.to_string()),
    );
    let duplicate_numbers = symbol::duplicate_pin_numbers_are_jumpers(definition)?;
    let jumper_groups = symbol::jumper_pin_groups(definition)?;
    for pin in pins {
        let pin_name = static_net_text("symbol pin name", &pin.name)?;
        let mut pin_keys = pin.numbers.clone();
        if !pin_name.is_empty() {
            pin_keys.insert(pin_name.clone());
        }
        connectables.push(Connectable {
            geometry: Geometry::Point {
                at: pin.point.into(),
                segment_interior_tolerance: None,
            },
            driver: pin.is_hidden_power_input().then(|| NameDriver {
                name: pin_name.clone(),
                kind: DriverKind::LegacyGlobal,
                merge_by_name: true,
            }),
            terminal: Some(Terminal::ComponentPin {
                component: component.clone(),
                pin_name,
                pin_keys,
            }),
            hierarchy: None,
            internal_links: internal_link_keys(
                placed,
                &pin.numbers,
                duplicate_numbers,
                &jumper_groups,
            ),
        });
    }
    Ok(())
}

fn collect_label(
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
            hierarchy: None,
            internal_links: BTreeSet::new(),
        });
        return Ok(());
    }
    let name = static_net_text("label", &label.text)?;
    if name.is_empty() {
        return Ok(());
    }
    let (kind, terminal) = match label.kind {
        LabelKind::Local => (DriverKind::Local, None),
        LabelKind::Global { .. } => (DriverKind::Global, None),
        LabelKind::Hierarchical { .. } => (
            DriverKind::Local,
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
            merge_by_name: true,
        }),
        terminal,
        hierarchy: matches!(label.kind, LabelKind::Hierarchical { .. })
            .then(|| HierarchyEndpoint::Parent { name }),
        internal_links: BTreeSet::new(),
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
