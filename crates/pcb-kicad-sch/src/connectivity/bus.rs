//! Read-only bus reduction. Bus geometry joins bundles, never scalar nets.
//! KiCad's SCH_CONNECTION::ConfigureFromLabel and CONNECTION_GRAPH::matchBusMember
//! define the expansion and member alignment used here (KiCad 10.0).
use pcb_sexpr::{Sexpr, find_child_list};

use super::*;
use crate::connectivity::bus_name::{self, BusSchema};

#[derive(Default)]
pub(super) struct PageBuses {
    segments: Vec<Segment>,
    entries: Vec<Segment>,
    label_ids: BTreeSet<String>,
    pin_ids: BTreeSet<(String, String)>,
    members: BTreeMap<String, BTreeSet<String>>,
    implicit_members: BTreeMap<String, DriverKind>,
    ports: Vec<(String, String)>,
}

impl PageBuses {
    pub(super) fn contains(&self, point: Point) -> bool {
        self.on_bus(point.into())
    }

    fn on_bus(&self, point: GridPoint) -> bool {
        self.segments
            .iter()
            .any(|segment| point_near_segment(point, *segment, 0))
    }

    pub(super) fn is_label(&self, id: &str) -> bool {
        self.label_ids.contains(id)
    }

    pub(super) fn is_pin(&self, sheet: &str, pin: &str) -> bool {
        self.pin_ids.contains(&(sheet.to_owned(), pin.to_owned()))
    }

    pub(super) fn connect(&self, connectables: &mut Vec<Connectable>) {
        // Bus labels also join identically named members of otherwise separate
        // bundles. This carries signals through intermediate sheets with no
        // scalar breakout. Sheet pins alone must not make these connections.
        for (name, kind) in &self.implicit_members {
            let mut item = empty(Geometry::Contacts([None, None]));
            item.internal_links = self.members[name].clone();
            // Global bus members also meet explicit global scalar labels on
            // sheets with no bus. Local labels on those sheets remain local.
            item.driver = Some(NameDriver {
                name: name.clone(),
                kind: *kind,
                role: DriverNameRole::HierarchyAlias,
                merge_by_name: true,
            });
            connectables.push(item);
        }
        // Named scalar drivers join member slots, even without a drawn entry.
        // In particular, do not publish sheet-pin member names as parent-sheet
        // labels: two separate instances can legitimately use the same bus name.
        for item in connectables.iter_mut() {
            if let Some(driver) = &item.driver
                && driver.merge_by_name
                // Legacy pins become drivers only after physical connectivity
                // is resolved. Same-page name merging will bind them afterward.
                && driver.kind != DriverKind::LegacyGlobal
                && let Some(links) = self.members.get(&driver.name)
            {
                item.internal_links.extend(links.iter().cloned());
            }
        }
        for (name, link) in &self.ports {
            let mut item = empty(Geometry::Contacts([None, None]));
            item.terminal = Some(Terminal::InterfacePort { name: name.clone() });
            item.internal_links.insert(link.clone());
            connectables.push(item);
        }
        // An entry has ports, not a conductive diagonal body. Neither its bus
        // end nor another entry touching that end can short member wires.
        for entry in &self.entries {
            let contacts = [entry.a, entry.b].map(|at| (!self.on_bus(at)).then_some(at));
            connectables.push(empty(Geometry::Contacts(contacts)));
        }
    }
}

struct Claim {
    page: usize,
    node: usize,
    name: String,
    schema: BusSchema,
    child: Option<String>,
    hierarchical: bool,
    global: bool,
    root_port: bool,
}

pub(super) fn collect(instances: &[PageInstance<'_>]) -> Result<Vec<PageBuses>> {
    let mut aliases = BTreeMap::new();
    for instance in instances {
        for item in &instance.page.items {
            let Some(list) = raw(item, "bus_alias") else {
                continue;
            };
            let name = list
                .get(1)
                .and_then(Sexpr::as_atom)
                .context("bus_alias missing name")?;
            let members = find_child_list(list, "members").context("bus_alias missing members")?;
            let members = members[1..]
                .iter()
                .map(|member| {
                    static_net_text(
                        "bus alias member",
                        member.as_atom().context("invalid bus alias member")?,
                    )
                })
                .collect::<Result<Vec<_>>>()?;
            let name = static_net_text("bus alias", name)?;
            if let Some(previous) = aliases.insert(name.clone(), members.clone())
                && previous != members
            {
                bail!("conflicting KiCad bus alias definitions for {name}");
            }
        }
    }

    let mut pages = Vec::new();
    let mut claims = Vec::new();
    let mut clusters = Vec::new();
    for (page_index, instance) in instances.iter().enumerate() {
        let mut page = PageBuses::default();
        for item in &instance.page.items {
            if let Some(segment) = bus_segment(item)? {
                page.segments.push(segment);
            } else if let Some(list) = raw(item, "bus_entry") {
                let at = xy(find_child_list(list, "at").context("bus_entry missing at")?)?;
                let size = xy(find_child_list(list, "size").context("bus_entry missing size")?)?;
                page.entries.push(Segment {
                    a: at.into(),
                    b: Point::new(at.x + size.x, at.y + size.y).into(),
                });
            }
        }
        let mut nodes = page
            .segments
            .iter()
            .map(|segment| empty(Geometry::Segment(*segment)))
            .collect::<Vec<_>>();
        let first_claim = claims.len();
        for item in &instance.page.items {
            match item {
                SchItem::Label(label) if !matches!(label.kind, LabelKind::Directive { .. }) => {
                    let name = static_net_text("bus label", &label.text)?;
                    if let Some(schema) = bus_name::parse(&name, &aliases)? {
                        page.label_ids.insert(label.id.clone());
                        claims.push(Claim {
                            page: page_index,
                            node: nodes.len(),
                            name,
                            schema,
                            child: None,
                            hierarchical: matches!(label.kind, LabelKind::Hierarchical { .. }),
                            global: matches!(label.kind, LabelKind::Global { .. }),
                            root_port: instance.id == instance.page.id
                                && matches!(label.kind, LabelKind::Hierarchical { .. }),
                        });
                        nodes.push(anchor(label.at, 1));
                    }
                }
                SchItem::Sheet(sheet) if sheet.placed => {
                    for pin in &sheet.pins {
                        let name = static_net_text("bus sheet pin", &pin.name)?;
                        if let Some(schema) = bus_name::parse(&name, &aliases)? {
                            page.pin_ids.insert((sheet.id.clone(), pin.id.clone()));
                            claims.push(Claim {
                                page: page_index,
                                node: nodes.len(),
                                name,
                                schema,
                                child: Some(instance.child_ids[&sheet.id].clone()),
                                hierarchical: false,
                                global: false,
                                root_port: false,
                            });
                            nodes.push(empty(Geometry::Point {
                                at: pin.at.into(),
                                segment_interior_tolerance: None,
                            }));
                        }
                    }
                }
                SchItem::Junction(junction) if page.contains(junction.at) => {
                    nodes.push(anchor(junction.at, 0))
                }
                SchItem::Label(label) if page.contains(label.at) => nodes.push(anchor(label.at, 1)),
                _ => {}
            }
        }
        let mut uf = UnionFind::new(nodes.len());
        union_touching(&nodes, &mut uf);
        for claim in &claims[first_claim..] {
            clusters.push((page_index, uf.find(claim.node)));
        }
        pages.push(page);
    }

    let mut bundles = UnionFind::new(claims.len());
    let mut physical = BTreeMap::new();
    let mut local = BTreeMap::new();
    let mut global = BTreeMap::new();
    let mut ports = BTreeMap::<(&str, &str), Vec<usize>>::new();
    for (index, claim) in claims.iter().enumerate() {
        if let Some(other) = physical.insert(clusters[index], index) {
            bundles.union(index, other);
        }
        if claim.child.is_none()
            && let Some(other) = local.insert((claim.page, claim.name.as_str()), index)
        {
            bundles.union(index, other);
        }
        if claim.global
            && let Some(other) = global.insert(claim.name.as_str(), index)
        {
            bundles.union(index, other);
        }
        if claim.hierarchical {
            ports
                .entry((&instances[claim.page].id, &claim.name))
                .or_default()
                .push(index);
        }
    }
    for (index, claim) in claims.iter().enumerate() {
        if let Some(child) = &claim.child
            && let Some(others) = ports.get(&(child.as_str(), claim.name.as_str()))
        {
            for other in others {
                bundles.union(index, *other);
            }
        }
    }
    let mut by_bundle = BTreeMap::<usize, Vec<usize>>::new();
    for index in 0..claims.len() {
        by_bundle
            .entry(bundles.find(index))
            .or_default()
            .push(index);
    }
    for (bundle, mut indices) in by_bundle {
        // Stable driver preference: global, local, hierarchical, sheet pin.
        indices.sort_by_key(|&index| {
            let claim = &claims[index];
            (
                !claim.global,
                claim.child.is_some(),
                claim.hierarchical,
                &claim.name,
                claim.page,
            )
        });
        let canonical = &claims[indices[0]].schema;
        let mut slots = canonical
            .members
            .iter()
            .map(|member| member.local_name.clone())
            .collect::<Vec<_>>();
        for index in indices {
            let claim = &claims[index];
            let mut used = BTreeSet::new();
            for (ordinal, member) in claim.schema.members.iter().enumerate() {
                let slot = if canonical.group && claim.schema.group {
                    slots
                        .iter()
                        .enumerate()
                        .find(|(slot, name)| !used.contains(slot) && **name == member.local_name)
                        .map(|(slot, _)| slot)
                } else {
                    (ordinal < slots.len()).then_some(ordinal)
                }
                .unwrap_or_else(|| {
                    slots.push(member.local_name.clone());
                    slots.len() - 1
                });
                used.insert(slot);
                let link = format!("bus:{bundle}:{slot}");
                pages[claim.page]
                    .members
                    .entry(member.name.clone())
                    .or_default()
                    .insert(link.clone());
                if claim.child.is_none() {
                    let kind = if claim.global {
                        DriverKind::Global
                    } else {
                        DriverKind::Local
                    };
                    pages[claim.page]
                        .implicit_members
                        .entry(member.name.clone())
                        .and_modify(|previous| *previous = (*previous).max(kind))
                        .or_insert(kind);
                }
                if claim.root_port {
                    pages[claim.page].ports.push((member.name.clone(), link));
                }
            }
        }
    }
    Ok(pages)
}

fn empty(geometry: Geometry) -> Connectable {
    Connectable {
        geometry,
        driver: None,
        terminal: None,
        pin: None,
        hierarchy: None,
        internal_links: BTreeSet::new(),
        source: None,
    }
}

fn anchor(at: Point, tolerance: i64) -> Connectable {
    empty(Geometry::Point {
        at: at.into(),
        segment_interior_tolerance: Some(tolerance),
    })
}

/// Repair cleanup must not remove a bus junction after cutting a crossing
/// scalar wire. The document has already passed connectivity validation.
pub(crate) fn is_bus_junction(page: &SchPage, at: Point) -> bool {
    page.items.iter().any(|item| {
        bus_segment(item)
            .ok()
            .flatten()
            .is_some_and(|segment| point_near_segment(at.into(), segment, 0))
    })
}

fn bus_segment(item: &SchItem) -> Result<Option<Segment>> {
    let Some(list) = raw(item, "bus") else {
        return Ok(None);
    };
    let pts = find_child_list(list, "pts").context("KiCad bus missing pts")?;
    anyhow::ensure!(pts.len() == 3, "KiCad bus requires two endpoints");
    Ok(Some(Segment {
        a: xy(pts[1].as_list().context("invalid bus endpoint")?)?.into(),
        b: xy(pts[2].as_list().context("invalid bus endpoint")?)?.into(),
    }))
}

fn raw<'a>(item: &'a SchItem, tag: &str) -> Option<&'a [Sexpr]> {
    let SchItem::Unsupported(expr) = item else {
        return None;
    };
    let list = expr.as_list()?;
    (list.first()?.as_sym() == Some(tag)).then_some(list)
}

fn xy(list: &[Sexpr]) -> Result<Point> {
    let number = |index| {
        list.get(index)
            .and_then(|value: &Sexpr| {
                value
                    .as_float()
                    .or_else(|| value.as_int().map(|n| n as f64))
            })
            .filter(|n| n.is_finite())
            .context("invalid KiCad bus coordinate")
    };
    Ok(Point::new(number(1)?, number(2)?))
}
