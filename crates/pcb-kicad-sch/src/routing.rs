//! Pure orthogonal routing over PCB's authoritative physical connectivity.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use ortho_router::{
    ConnDirFlags, Connector, ExistingRouteSegment, Obstacle, OrthoRouter, Point as RouterPoint,
    Port, Rect, RouterInput,
};
use pcb_sch::Schematic;

use crate::{
    Bounds, LabelSpin, Point, SchDocument, SchItem, SchPage, Wire,
    analysis::{ConnectivityInspection, inspect_schematic},
    connectivity::{ConnectivityItemRef, IslandRef, PhysicalIsland},
    deterministic_uuid,
    reconcile::{DocumentEdit, DocumentPatch, coarse_issue_key},
    wire_junctions::{PointKey, reconcile_page_wires},
};

const ROUTER_COORD_SCALE: f64 = 10.0;
const PAGE_ATLAS_STRIDE: f64 = 1_000_000.0;
const CONNECT_EPS_MM: f64 = 1.0e-6;
const LABEL_OBSTACLE_SIZE_MM: f64 = 0.2;

/// Geometry-only routing limits shared by reconciliation and editors.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RoutingPolicy {
    pub max_wire_span_mm: f64,
}

impl Default for RoutingPolicy {
    fn default() -> Self {
        Self {
            max_wire_span_mm: 50.0,
        }
    }
}

/// Purely reroute an explicit editor selection without invoking semantic
/// reconciliation or label fallback.
///
/// Selected wires are replaced; selecting another item routes its physically
/// attached nets. The result is returned only when it introduces no issue that
/// was absent before the user edit.
pub fn plan_wire_reroute(
    document: &SchDocument,
    netlist: &Schematic,
    page_id: &str,
    selected_item_ids: &BTreeSet<String>,
    policy: RoutingPolicy,
) -> Result<Option<DocumentPatch>> {
    let original_inspection = inspect_schematic(document, netlist)?;
    let net_names = selected_net_names(&original_inspection, page_id, selected_item_ids);
    if net_names.is_empty() {
        return Ok(None);
    }
    let page_index = document
        .pages
        .iter()
        .position(|page| page.id == page_id)
        .with_context(|| format!("schematic route page '{page_id}' is absent"))?;
    let selected_wire_ids = document.pages[page_index]
        .items
        .iter()
        .filter_map(|item| match item {
            SchItem::Wire(wire) if selected_item_ids.contains(&wire.id) => Some(wire.id.clone()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let required_replacements = selected_wire_ids
        .iter()
        .flat_map(|wire_id| wire_net_names(&original_inspection, page_id, wire_id))
        .collect::<BTreeSet<_>>();

    let mut stripped = document.clone();
    let affected_junctions =
        affected_junction_points(&stripped.pages[page_index], &selected_wire_ids);
    stripped.pages[page_index].items.retain(
        |item| !matches!(item, SchItem::Wire(wire) if selected_wire_ids.contains(&wire.id)),
    );
    let stripped_inspection = inspect_schematic(&stripped, netlist)?;
    let outcome = route_candidate(
        &stripped,
        netlist,
        &stripped_inspection,
        &net_names,
        Some(&BTreeSet::from([page_index])),
        &BTreeMap::from([(page_index, affected_junctions)]),
        policy,
    )?;
    let Some(outcome) = outcome else {
        return Ok(None);
    };
    if !required_replacements.is_subset(&outcome.added_nets) {
        return Ok(None);
    }
    let after_inspection = inspect_schematic(&outcome.document, netlist)?;
    let original_keys = original_inspection
        .issues
        .iter()
        .map(|issue| coarse_issue_key(&issue.key))
        .collect::<BTreeSet<_>>();
    if after_inspection
        .issues
        .iter()
        .any(|issue| !original_keys.contains(&coarse_issue_key(&issue.key)))
    {
        return Ok(None);
    }
    let before_page = document.pages[page_index].clone();
    let after_page = outcome.document.pages[page_index].clone();
    if before_page == after_page {
        return Ok(None);
    }
    Ok(Some(DocumentPatch::new(vec![DocumentEdit::ReplacePage {
        index: page_index,
        before: before_page,
        after: after_page,
    }])))
}

pub(crate) fn route_disconnected_nets(
    document: &mut SchDocument,
    netlist: &Schematic,
    target_nets: &BTreeSet<String>,
) -> Result<bool> {
    if target_nets.is_empty() {
        return Ok(false);
    }
    let inspection = inspect_schematic(document, netlist)?;
    let outcome = route_candidate(
        document,
        netlist,
        &inspection,
        target_nets,
        None,
        &BTreeMap::new(),
        RoutingPolicy::default(),
    )?;
    let Some(outcome) = outcome else {
        return Ok(false);
    };
    *document = outcome.document;
    Ok(true)
}

struct RouteOutcome {
    document: SchDocument,
    added_nets: BTreeSet<String>,
}

fn route_candidate(
    document: &SchDocument,
    netlist: &Schematic,
    inspection: &ConnectivityInspection,
    target_nets: &BTreeSet<String>,
    target_pages: Option<&BTreeSet<usize>>,
    affected_junctions: &BTreeMap<usize, BTreeSet<PointKey>>,
    policy: RoutingPolicy,
) -> Result<Option<RouteOutcome>> {
    let mut input = RouterInput::new();
    let mut ports = BTreeMap::<String, Port>::new();
    let mut connector_specs = BTreeMap::new();
    let mut pages_with_routes = BTreeSet::new();

    for (page_index, page) in document.pages.iter().enumerate() {
        if target_pages.is_some_and(|pages| !pages.contains(&page_index)) {
            continue;
        }
        let model = PageRouteModel::new(page, page_index, inspection);
        let mut page_connectors = Vec::new();
        for net_name in target_nets {
            let Some(net) = inspection.analysis.nets.get(net_name) else {
                continue;
            };
            let groups = net
                .connected_islands
                .iter()
                .filter_map(|islands| {
                    let group = model.route_group(net_name, islands);
                    (!group.attachments.is_empty()).then_some(group)
                })
                .collect::<Vec<_>>();
            page_connectors.extend(plan_net_connections(
                page_index, &page.id, net_name, &groups, policy,
            ));
        }
        if page_connectors.is_empty() {
            continue;
        }
        pages_with_routes.insert(page_index);
        for obstacle in model.obstacles()? {
            input.add_obstacle(obstacle);
        }
        for segment in model.existing_segments() {
            input.add_existing_segment(segment);
        }
        for planned in page_connectors {
            for port in [&planned.source, &planned.target] {
                if let Some(existing) = ports.get(&port.id) {
                    if !router_points_coincide(existing.position, port.position)
                        || existing.visibility != port.visibility
                        || existing.obstacle_id != port.obstacle_id
                    {
                        anyhow::bail!("route port '{}' has conflicting geometry", port.id);
                    }
                } else {
                    ports.insert(port.id.clone(), port.clone());
                }
            }
            input.add_connector(Connector::with_net(
                &planned.id,
                &planned.source.id,
                &planned.target.id,
                route_net_id(page_index, &planned.net_name),
            ));
            connector_specs.insert(planned.id.clone(), planned);
        }
    }
    if connector_specs.is_empty() {
        return Ok(None);
    }
    for port in ports.into_values() {
        input.add_port(port);
    }

    let output = OrthoRouter::with_defaults().route(&input);
    let mut additions = BTreeMap::<usize, Vec<(String, Wire)>>::new();
    for path in output.paths {
        let Some(spec) = connector_specs.get(&path.connector_id) else {
            continue;
        };
        let mut points = path.points;
        let source = spec.source.position;
        let target = spec.target.position;
        if points
            .first()
            .is_none_or(|point| !router_points_coincide(*point, source))
        {
            points.insert(0, source);
        }
        if points
            .last()
            .is_none_or(|point| !router_points_coincide(*point, target))
        {
            points.push(target);
        }
        if !router_path_is_orthogonal(&points) {
            continue;
        }
        for (segment_index, points) in points.windows(2).enumerate() {
            let a = from_router_point(spec.page_index, points[0]);
            let b = from_router_point(spec.page_index, points[1]);
            if points_coincide(a, b) {
                continue;
            }
            let key = SegmentKey::new(a, b);
            additions.entry(spec.page_index).or_default().push((
                spec.net_name.clone(),
                Wire {
                    id: deterministic_uuid(format!(
                        "pcb-kicad-sch:route:{}:{segment_index}:{}:{}:{}:{}",
                        spec.id, key.0.x, key.0.y, key.1.x, key.1.y
                    )),
                    a,
                    b,
                    unsupported: Vec::new(),
                },
            ));
        }
    }

    let mut candidate = document.clone();
    let mut added_nets = BTreeSet::new();
    let mut changed = false;
    let no_affected_junctions = BTreeSet::new();
    for page_index in pages_with_routes {
        let page = &mut candidate.pages[page_index];
        let mut wires = page
            .items
            .iter()
            .filter_map(|item| match item {
                SchItem::Wire(wire) => Some(wire.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let mut seen = wires
            .iter()
            .map(|wire| SegmentKey::new(wire.a, wire.b))
            .collect::<BTreeSet<_>>();
        let mut candidates = BTreeSet::new();
        for (net_name, wire) in additions.remove(&page_index).unwrap_or_default() {
            let key = SegmentKey::new(wire.a, wire.b);
            if seen.insert(key) {
                candidates.extend([PointKey::from_point(wire.a), PointKey::from_point(wire.b)]);
                wires.push(wire);
                added_nets.insert(net_name);
            }
        }
        changed |= reconcile_page_wires(
            page,
            wires,
            affected_junctions
                .get(&page_index)
                .unwrap_or(&no_affected_junctions),
            &candidates,
        );
    }
    if !changed {
        return Ok(None);
    }

    let baseline = inspection
        .issues
        .iter()
        .map(|issue| coarse_issue_key(&issue.key))
        .collect::<BTreeSet<_>>();
    let after = inspect_schematic(&candidate, netlist)?;
    if after
        .issues
        .iter()
        .any(|issue| !baseline.contains(&coarse_issue_key(&issue.key)))
    {
        return Ok(None);
    }
    Ok(Some(RouteOutcome {
        document: candidate,
        added_nets,
    }))
}

#[derive(Debug, Clone)]
struct PlannedConnector {
    id: String,
    net_name: String,
    page_index: usize,
    source: Port,
    target: Port,
}

#[derive(Debug, Clone)]
struct RouteGroup {
    attachments: Vec<AttachmentGeometry>,
    has_driver: bool,
}

#[derive(Debug, Clone)]
enum AttachmentGeometry {
    Point(RouteAttachment),
    Segment { a: Point, b: Point },
}

#[derive(Debug, Clone)]
struct RouteAttachment {
    point: Point,
    kind: RouteAttachmentKind,
}

#[derive(Debug, Clone)]
enum RouteAttachmentKind {
    Free,
    Obstacle {
        id: String,
        visibility: Option<ConnDirFlags>,
    },
}

struct PageRouteModel<'a> {
    page: &'a SchPage,
    page_index: usize,
    inspection: &'a ConnectivityInspection,
    wire_nets: BTreeMap<String, String>,
}

impl<'a> PageRouteModel<'a> {
    fn new(page: &'a SchPage, page_index: usize, inspection: &'a ConnectivityInspection) -> Self {
        let mut assignments = BTreeMap::<String, BTreeSet<String>>::new();
        for net in inspection.analysis.nets.values() {
            for island_ref in &net.islands {
                let Some(island) = inspection.physical.islands.get(island_ref) else {
                    continue;
                };
                for item in &island.items {
                    if let ConnectivityItemRef::Wire { page_id, id } = item
                        && page_id == &page.id
                    {
                        assignments
                            .entry(id.clone())
                            .or_default()
                            .insert(net.name.clone());
                    }
                }
            }
        }
        let wire_nets = assignments
            .into_iter()
            .filter_map(|(id, names)| {
                (names.len() == 1).then(|| (id, names.into_iter().next().unwrap()))
            })
            .collect();
        Self {
            page,
            page_index,
            inspection,
            wire_nets,
        }
    }

    fn route_group(&self, net_name: &str, island_refs: &[IslandRef]) -> RouteGroup {
        let mut attachments = Vec::new();
        let mut has_driver = false;
        for island_ref in island_refs {
            let Some(island) = self.inspection.physical.islands.get(island_ref) else {
                continue;
            };
            has_driver |= island.named_drivers.contains_key(net_name);
            attachments.extend(self.attachments(island));
        }
        attachments.sort_by_key(AttachmentGeometry::sort_key);
        attachments.dedup_by_key(|attachment| attachment.sort_key());
        RouteGroup {
            attachments,
            has_driver,
        }
    }

    fn attachments(&self, island: &PhysicalIsland) -> Vec<AttachmentGeometry> {
        let mut attachments = island
            .symbol_pins
            .iter()
            .filter(|pin| pin.page_id() == self.page.id)
            .map(|pin| {
                AttachmentGeometry::Point(RouteAttachment {
                    point: pin.point(),
                    kind: RouteAttachmentKind::Obstacle {
                        id: symbol_obstacle_id(self.page_index, pin.symbol_id()),
                        visibility: Some(visibility_for_spin(pin.outward_spin())),
                    },
                })
            })
            .collect::<Vec<_>>();
        for item_ref in &island.items {
            match item_ref {
                ConnectivityItemRef::Wire { page_id, id } if page_id == &self.page.id => {
                    attachments.extend(self.page.items.iter().filter_map(|item| match item {
                        SchItem::Wire(wire) if &wire.id == id => {
                            Some(AttachmentGeometry::Segment {
                                a: wire.a,
                                b: wire.b,
                            })
                        }
                        _ => None,
                    }));
                }
                ConnectivityItemRef::Label { page_id, id } if page_id == &self.page.id => {
                    attachments.extend(self.page.items.iter().filter_map(|item| match item {
                        SchItem::Label(label) if &label.id == id => {
                            Some(AttachmentGeometry::Point(RouteAttachment {
                                point: label.at,
                                kind: RouteAttachmentKind::Obstacle {
                                    id: label_obstacle_id(self.page_index, &label.id),
                                    visibility: None,
                                },
                            }))
                        }
                        _ => None,
                    }));
                }
                ConnectivityItemRef::Junction { page_id, id } if page_id == &self.page.id => {
                    attachments.extend(self.page.items.iter().filter_map(|item| match item {
                        SchItem::Junction(junction) if &junction.id == id => Some(
                            AttachmentGeometry::Point(RouteAttachment::free(junction.at)),
                        ),
                        _ => None,
                    }));
                }
                ConnectivityItemRef::SheetPin {
                    page_id,
                    sheet_id,
                    pin_id,
                } if page_id == &self.page.id => {
                    attachments.extend(self.page.items.iter().filter_map(|item| match item {
                        SchItem::Sheet(sheet) if &sheet.id == sheet_id => {
                            sheet.pins.iter().find(|pin| &pin.id == pin_id).map(|pin| {
                                AttachmentGeometry::Point(RouteAttachment {
                                    point: pin.at,
                                    kind: RouteAttachmentKind::Obstacle {
                                        id: sheet_obstacle_id(self.page_index, &sheet.id),
                                        visibility: Some(visibility_for_rotation(pin.rotation)),
                                    },
                                })
                            })
                        }
                        _ => None,
                    }));
                }
                ConnectivityItemRef::Symbol { .. }
                | ConnectivityItemRef::Wire { .. }
                | ConnectivityItemRef::Junction { .. }
                | ConnectivityItemRef::NoConnect { .. }
                | ConnectivityItemRef::Label { .. }
                | ConnectivityItemRef::SheetPin { .. } => {}
            }
        }
        attachments
    }

    fn obstacles(&self) -> Result<Vec<Obstacle>> {
        let mut obstacles = Vec::new();
        for item in &self.page.items {
            match item {
                SchItem::Symbol(symbol) => {
                    let Some(definition) = self.page.library.definitions.get(&symbol.lib_id) else {
                        continue;
                    };
                    let Some(bounds) = symbol.visual_bounds(definition)? else {
                        continue;
                    };
                    obstacles.push(obstacle_from_bounds(
                        symbol_obstacle_id(self.page_index, &symbol.id),
                        self.page_index,
                        bounds,
                    ));
                }
                SchItem::Label(label) if !label.text.trim().is_empty() => {
                    let half = LABEL_OBSTACLE_SIZE_MM / 2.0;
                    obstacles.push(obstacle_from_bounds(
                        label_obstacle_id(self.page_index, &label.id),
                        self.page_index,
                        Bounds {
                            min_x: label.at.x - half,
                            min_y: label.at.y - half,
                            max_x: label.at.x + half,
                            max_y: label.at.y + half,
                        },
                    ));
                }
                SchItem::Sheet(sheet) => {
                    let Some((min, max)) = sheet.bounds() else {
                        continue;
                    };
                    obstacles.push(obstacle_from_bounds(
                        sheet_obstacle_id(self.page_index, &sheet.id),
                        self.page_index,
                        Bounds {
                            min_x: min.x,
                            min_y: min.y,
                            max_x: max.x,
                            max_y: max.y,
                        },
                    ));
                }
                _ => {}
            }
        }
        Ok(obstacles)
    }

    fn existing_segments(&self) -> Vec<ExistingRouteSegment> {
        self.page
            .items
            .iter()
            .filter_map(|item| {
                let SchItem::Wire(wire) = item else {
                    return None;
                };
                let net_name = self.wire_nets.get(&wire.id)?;
                Some(ExistingRouteSegment::new(
                    format!("page:{}:wire:{}", self.page_index, wire.id),
                    to_router_point(self.page_index, wire.a),
                    to_router_point(self.page_index, wire.b),
                    route_net_id(self.page_index, net_name),
                ))
            })
            .collect()
    }
}

impl RouteAttachment {
    fn free(point: Point) -> Self {
        Self {
            point,
            kind: RouteAttachmentKind::Free,
        }
    }
}

impl AttachmentGeometry {
    fn sort_key(&self) -> (PointKey, PointKey, u8) {
        match self {
            Self::Point(point) => {
                let key = PointKey::from_point(point.point);
                (key, key, 0)
            }
            Self::Segment { a, b } => {
                let key = SegmentKey::new(*a, *b);
                (key.0, key.1, 1)
            }
        }
    }
}

fn plan_net_connections(
    page_index: usize,
    page_id: &str,
    net_name: &str,
    groups: &[RouteGroup],
    policy: RoutingPolicy,
) -> Vec<PlannedConnector> {
    let mut candidates = Vec::new();
    for left in 0..groups.len() {
        for right in (left + 1)..groups.len() {
            if groups[left].has_driver && groups[right].has_driver {
                continue;
            }
            let Some(closest) = closest_between_groups(&groups[left], &groups[right]) else {
                continue;
            };
            if closest.distance2 > policy.max_wire_span_mm * policy.max_wire_span_mm {
                continue;
            }
            candidates.push((left, right, closest));
        }
    }
    candidates.sort_by(|left, right| {
        left.2
            .distance2
            .total_cmp(&right.2.distance2)
            .then_with(|| left.0.cmp(&right.0))
            .then_with(|| left.1.cmp(&right.1))
    });
    let mut union_find = UnionFind::new(groups.iter().map(|group| group.has_driver));
    let mut connectors = Vec::new();
    for (left, right, closest) in candidates {
        if !union_find.union(left, right) {
            continue;
        }
        let id = format!("route:{page_id}:{net_name}:{left}:{right}");
        let source_point = PointKey::from_point(closest.from.point);
        let target_point = PointKey::from_point(closest.to.point);
        let source = route_port(
            page_index,
            format!(
                "route-port:{page_id}:{net_name}:{left}:{}:{}",
                source_point.x, source_point.y
            ),
            &closest.from,
            closest.to.point,
        );
        let target = route_port(
            page_index,
            format!(
                "route-port:{page_id}:{net_name}:{right}:{}:{}",
                target_point.x, target_point.y
            ),
            &closest.to,
            closest.from.point,
        );
        connectors.push(PlannedConnector {
            id,
            net_name: net_name.to_string(),
            page_index,
            source,
            target,
        });
    }
    connectors
}

struct ClosestPair {
    from: RouteAttachment,
    to: RouteAttachment,
    distance2: f64,
}

fn closest_between_groups(left: &RouteGroup, right: &RouteGroup) -> Option<ClosestPair> {
    left.attachments
        .iter()
        .flat_map(|left| {
            right.attachments.iter().filter_map(move |right| {
                let closest = closest_between_geometries(left, right);
                (!attachments_share_obstacle(&closest.from, &closest.to)).then_some(closest)
            })
        })
        .min_by(|left, right| left.distance2.total_cmp(&right.distance2))
}

fn attachments_share_obstacle(left: &RouteAttachment, right: &RouteAttachment) -> bool {
    matches!(
        (&left.kind, &right.kind),
        (
            RouteAttachmentKind::Obstacle { id: left, .. },
            RouteAttachmentKind::Obstacle { id: right, .. }
        ) if left == right
    )
}

fn closest_between_geometries(
    left: &AttachmentGeometry,
    right: &AttachmentGeometry,
) -> ClosestPair {
    let candidates = match (left, right) {
        (AttachmentGeometry::Point(left), AttachmentGeometry::Point(right)) => vec![ClosestPair {
            from: left.clone(),
            to: right.clone(),
            distance2: distance2(left.point, right.point),
        }],
        (AttachmentGeometry::Point(left), AttachmentGeometry::Segment { a, b }) => {
            let point = closest_point_on_segment(left.point, *a, *b);
            vec![ClosestPair {
                from: left.clone(),
                to: RouteAttachment::free(point),
                distance2: distance2(left.point, point),
            }]
        }
        (AttachmentGeometry::Segment { a, b }, AttachmentGeometry::Point(right)) => {
            let point = closest_point_on_segment(right.point, *a, *b);
            vec![ClosestPair {
                from: RouteAttachment::free(point),
                to: right.clone(),
                distance2: distance2(point, right.point),
            }]
        }
        (
            AttachmentGeometry::Segment { a: a1, b: a2 },
            AttachmentGeometry::Segment { a: b1, b: b2 },
        ) => {
            let mut candidates = Vec::new();
            for point in [*a1, *a2] {
                let other = closest_point_on_segment(point, *b1, *b2);
                candidates.push(ClosestPair {
                    from: RouteAttachment::free(point),
                    to: RouteAttachment::free(other),
                    distance2: distance2(point, other),
                });
            }
            for point in [*b1, *b2] {
                let other = closest_point_on_segment(point, *a1, *a2);
                candidates.push(ClosestPair {
                    from: RouteAttachment::free(other),
                    to: RouteAttachment::free(point),
                    distance2: distance2(point, other),
                });
            }
            candidates
        }
    };
    candidates
        .into_iter()
        .min_by(|left, right| left.distance2.total_cmp(&right.distance2))
        .expect("each geometry pair has a closest-point candidate")
}

fn route_port(page_index: usize, id: String, attachment: &RouteAttachment, toward: Point) -> Port {
    let visibility = match &attachment.kind {
        RouteAttachmentKind::Free => ConnDirFlags::ALL,
        RouteAttachmentKind::Obstacle {
            visibility: Some(visibility),
            ..
        } => *visibility,
        RouteAttachmentKind::Obstacle {
            visibility: None, ..
        } => visibility_toward(attachment.point, toward),
    };
    match &attachment.kind {
        RouteAttachmentKind::Free => Port::new(
            id,
            to_router_point(page_index, attachment.point),
            visibility,
        ),
        RouteAttachmentKind::Obstacle {
            id: obstacle_id, ..
        } => Port::on_obstacle(
            id,
            to_router_point(page_index, attachment.point),
            visibility,
            obstacle_id,
        ),
    }
}

fn selected_net_names(
    inspection: &ConnectivityInspection,
    page_id: &str,
    selected_item_ids: &BTreeSet<String>,
) -> BTreeSet<String> {
    inspection
        .analysis
        .nets
        .values()
        .filter(|net| {
            net.islands.iter().any(|island_ref| {
                inspection
                    .physical
                    .islands
                    .get(island_ref)
                    .is_some_and(|island| {
                        island.items.iter().any(|item| {
                            item_page_and_id(item).is_some_and(|(item_page, item_id)| {
                                item_page == page_id && selected_item_ids.contains(item_id)
                            })
                        }) || island.symbol_pins.iter().any(|pin| {
                            pin.page_id() == page_id && selected_item_ids.contains(pin.symbol_id())
                        })
                    })
            })
        })
        .map(|net| net.name.clone())
        .collect()
}

fn wire_net_names(
    inspection: &ConnectivityInspection,
    page_id: &str,
    wire_id: &str,
) -> BTreeSet<String> {
    inspection
        .analysis
        .nets
        .values()
        .filter(|net| {
            net.islands.iter().any(|island_ref| {
                inspection
                    .physical
                    .islands
                    .get(island_ref)
                    .is_some_and(|island| {
                        island.items.contains(&ConnectivityItemRef::Wire {
                            page_id: page_id.to_string(),
                            id: wire_id.to_string(),
                        })
                    })
            })
        })
        .map(|net| net.name.clone())
        .collect()
}

fn affected_junction_points(page: &SchPage, removed: &BTreeSet<String>) -> BTreeSet<PointKey> {
    let removed_wires = page
        .items
        .iter()
        .filter_map(|item| match item {
            SchItem::Wire(wire) if removed.contains(&wire.id) => Some(wire),
            _ => None,
        })
        .collect::<Vec<_>>();
    page.items
        .iter()
        .filter_map(|item| match item {
            SchItem::Junction(junction)
                if removed_wires
                    .iter()
                    .any(|wire| point_on_segment(junction.at, wire.a, wire.b)) =>
            {
                Some(PointKey::from_point(junction.at))
            }
            _ => None,
        })
        .chain(
            removed_wires
                .iter()
                .flat_map(|wire| [PointKey::from_point(wire.a), PointKey::from_point(wire.b)]),
        )
        .collect()
}

fn item_page_and_id(item: &ConnectivityItemRef) -> Option<(&str, &str)> {
    match item {
        ConnectivityItemRef::Symbol { page_id, id }
        | ConnectivityItemRef::Wire { page_id, id }
        | ConnectivityItemRef::Junction { page_id, id }
        | ConnectivityItemRef::NoConnect { page_id, id }
        | ConnectivityItemRef::Label { page_id, id } => Some((page_id, id)),
        ConnectivityItemRef::SheetPin {
            page_id, pin_id, ..
        } => Some((page_id, pin_id)),
    }
}

fn obstacle_from_bounds(id: String, page_index: usize, bounds: Bounds) -> Obstacle {
    let min = to_router_point(page_index, Point::new(bounds.min_x, bounds.min_y));
    let max = to_router_point(page_index, Point::new(bounds.max_x, bounds.max_y));
    Obstacle::new(id, Rect::new(min.x, min.y, max.x, max.y))
}

fn symbol_obstacle_id(page_index: usize, symbol_id: &str) -> String {
    format!("page:{page_index}:symbol:{symbol_id}")
}

fn label_obstacle_id(page_index: usize, label_id: &str) -> String {
    format!("page:{page_index}:label:{label_id}")
}

fn sheet_obstacle_id(page_index: usize, sheet_id: &str) -> String {
    format!("page:{page_index}:sheet:{sheet_id}")
}

fn route_net_id(page_index: usize, net_name: &str) -> String {
    format!("page:{page_index}:net:{net_name}")
}

fn visibility_for_spin(spin: LabelSpin) -> ConnDirFlags {
    match spin {
        LabelSpin::Left => ConnDirFlags::LEFT,
        LabelSpin::Up => ConnDirFlags::UP,
        LabelSpin::Right => ConnDirFlags::RIGHT,
        LabelSpin::Bottom => ConnDirFlags::DOWN,
    }
}

fn visibility_for_rotation(rotation: crate::Rotation) -> ConnDirFlags {
    match rotation {
        crate::Rotation::Deg0 => ConnDirFlags::RIGHT,
        crate::Rotation::Deg90 => ConnDirFlags::UP,
        crate::Rotation::Deg180 => ConnDirFlags::LEFT,
        crate::Rotation::Deg270 => ConnDirFlags::DOWN,
    }
}

fn visibility_toward(from: Point, to: Point) -> ConnDirFlags {
    let dx = to.x - from.x;
    let dy = to.y - from.y;
    if dx.abs() >= dy.abs() {
        if dx >= 0.0 {
            ConnDirFlags::RIGHT
        } else {
            ConnDirFlags::LEFT
        }
    } else if dy >= 0.0 {
        ConnDirFlags::DOWN
    } else {
        ConnDirFlags::UP
    }
}

fn to_router_point(page_index: usize, point: Point) -> RouterPoint {
    RouterPoint::new(
        page_index as f64 * PAGE_ATLAS_STRIDE + point.x * ROUTER_COORD_SCALE,
        point.y * ROUTER_COORD_SCALE,
    )
}

fn from_router_point(page_index: usize, point: RouterPoint) -> Point {
    Point::new(
        (point.x - page_index as f64 * PAGE_ATLAS_STRIDE) / ROUTER_COORD_SCALE,
        point.y / ROUTER_COORD_SCALE,
    )
}

fn router_points_coincide(left: RouterPoint, right: RouterPoint) -> bool {
    (left.x - right.x).abs() <= CONNECT_EPS_MM * ROUTER_COORD_SCALE
        && (left.y - right.y).abs() <= CONNECT_EPS_MM * ROUTER_COORD_SCALE
}

fn router_path_is_orthogonal(points: &[RouterPoint]) -> bool {
    points.windows(2).all(|points| {
        (points[0].x - points[1].x).abs() <= CONNECT_EPS_MM * ROUTER_COORD_SCALE
            || (points[0].y - points[1].y).abs() <= CONNECT_EPS_MM * ROUTER_COORD_SCALE
    })
}

fn points_coincide(left: Point, right: Point) -> bool {
    distance2(left, right) <= CONNECT_EPS_MM * CONNECT_EPS_MM
}

fn distance2(left: Point, right: Point) -> f64 {
    (left.x - right.x).powi(2) + (left.y - right.y).powi(2)
}

fn closest_point_on_segment(point: Point, a: Point, b: Point) -> Point {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let length2 = dx * dx + dy * dy;
    if length2 <= CONNECT_EPS_MM * CONNECT_EPS_MM {
        return a;
    }
    let position = (((point.x - a.x) * dx + (point.y - a.y) * dy) / length2).clamp(0.0, 1.0);
    Point::new(a.x + position * dx, a.y + position * dy)
}

fn point_on_segment(point: Point, a: Point, b: Point) -> bool {
    let cross = (b.x - a.x) * (point.y - a.y) - (b.y - a.y) * (point.x - a.x);
    cross.abs() <= CONNECT_EPS_MM
        && point.x >= a.x.min(b.x) - CONNECT_EPS_MM
        && point.x <= a.x.max(b.x) + CONNECT_EPS_MM
        && point.y >= a.y.min(b.y) - CONNECT_EPS_MM
        && point.y <= a.y.max(b.y) + CONNECT_EPS_MM
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct SegmentKey(PointKey, PointKey);

impl SegmentKey {
    fn new(a: Point, b: Point) -> Self {
        let a = PointKey::from_point(a);
        let b = PointKey::from_point(b);
        if a <= b { Self(a, b) } else { Self(b, a) }
    }
}

struct UnionFind {
    parent: Vec<usize>,
    has_driver: Vec<bool>,
}

impl UnionFind {
    fn new(has_driver: impl IntoIterator<Item = bool>) -> Self {
        let has_driver = has_driver.into_iter().collect::<Vec<_>>();
        Self {
            parent: (0..has_driver.len()).collect(),
            has_driver,
        }
    }

    fn root(&mut self, value: usize) -> usize {
        if self.parent[value] != value {
            self.parent[value] = self.root(self.parent[value]);
        }
        self.parent[value]
    }

    fn union(&mut self, left: usize, right: usize) -> bool {
        let left = self.root(left);
        let right = self.root(right);
        if left == right {
            return false;
        }
        if self.has_driver[left] && self.has_driver[right] {
            return false;
        }
        self.parent[right] = left;
        self.has_driver[left] |= self.has_driver[right];
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point_group(x: f64, has_driver: bool) -> RouteGroup {
        RouteGroup {
            attachments: vec![AttachmentGeometry::Point(RouteAttachment::free(
                Point::new(x, 0.0),
            ))],
            has_driver,
        }
    }

    #[test]
    fn net_connections_form_a_sparse_minimum_spanning_tree() {
        let groups = [
            point_group(0.0, false),
            point_group(10.0, false),
            point_group(20.0, false),
            point_group(30.0, false),
        ];
        let connectors = plan_net_connections(0, "page", "NET", &groups, RoutingPolicy::default());
        assert_eq!(connectors.len(), groups.len() - 1);
        assert_eq!(connectors[0].target.id, connectors[1].source.id);
    }

    #[test]
    fn net_connections_never_join_two_driver_trees() {
        let groups = [
            point_group(0.0, true),
            point_group(10.0, false),
            point_group(20.0, true),
        ];
        let connectors = plan_net_connections(0, "page", "NET", &groups, RoutingPolicy::default());
        assert_eq!(connectors.len(), 1);
    }

    #[test]
    fn net_connections_do_not_route_between_pins_on_one_obstacle() {
        let attachment = |point| {
            AttachmentGeometry::Point(RouteAttachment {
                point,
                kind: RouteAttachmentKind::Obstacle {
                    id: "same-symbol".to_string(),
                    visibility: Some(ConnDirFlags::RIGHT),
                },
            })
        };
        let groups = [
            RouteGroup {
                attachments: vec![attachment(Point::new(0.0, 0.0))],
                has_driver: false,
            },
            RouteGroup {
                attachments: vec![attachment(Point::new(10.0, 0.0))],
                has_driver: false,
            },
        ];
        assert!(
            plan_net_connections(0, "page", "NET", &groups, RoutingPolicy::default()).is_empty()
        );
    }
}
