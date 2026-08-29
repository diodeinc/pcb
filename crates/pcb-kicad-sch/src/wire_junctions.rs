use std::collections::{BTreeMap, BTreeSet};

use crate::{Junction, Point, SchItem, SchPage, Wire, deterministic_uuid};

const IU_PER_MM: f64 = 10_000.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PointKey {
    pub x: i64,
    pub y: i64,
}

impl PointKey {
    pub fn from_point(point: Point) -> Self {
        Self {
            x: (point.x * IU_PER_MM).round() as i64,
            y: (point.y * IU_PER_MM).round() as i64,
        }
    }

    pub fn to_point(self) -> Point {
        Point::new(self.x as f64 / IU_PER_MM, self.y as f64 / IU_PER_MM)
    }
}

/// Recompute explicit KiCad junctions after replacing a page's wire geometry.
pub fn reconcile_page_wires(
    page: &mut SchPage,
    wires: Vec<Wire>,
    affected_original_points: &BTreeSet<PointKey>,
    additional_candidates: &BTreeSet<PointKey>,
) -> bool {
    let original = page
        .items
        .iter()
        .filter_map(|item| match item {
            SchItem::Junction(junction) => {
                Some((PointKey::from_point(junction.at), junction.clone()))
            }
            _ => None,
        })
        .collect::<BTreeMap<_, _>>();
    let junctions = derive_junctions(
        &page.id,
        &wires,
        &original,
        affected_original_points,
        additional_candidates,
    );
    replace_page_wires_and_junctions(page, wires, junctions)
}

/// Recompute junctions for the page's existing wires without changing them.
pub fn reconcile_page_junctions(page: &mut SchPage) -> bool {
    let wires = page
        .items
        .iter()
        .filter_map(|item| match item {
            SchItem::Wire(wire) => Some(wire.clone()),
            _ => None,
        })
        .collect();
    reconcile_page_wires(page, wires, &BTreeSet::new(), &BTreeSet::new())
}

pub fn derive_junctions(
    page_id: &str,
    wires: &[Wire],
    original: &BTreeMap<PointKey, Junction>,
    affected_original_points: &BTreeSet<PointKey>,
    additional_candidates: &BTreeSet<PointKey>,
) -> Vec<Junction> {
    let mut junction_points = branch_points_with_candidates(
        wires,
        original
            .keys()
            .copied()
            .chain(additional_candidates.iter().copied()),
    );
    junction_points.extend(
        original
            .keys()
            .filter(|point| !affected_original_points.contains(point))
            .copied(),
    );
    junction_points
        .into_iter()
        .map(|point| {
            original.get(&point).cloned().unwrap_or_else(|| Junction {
                id: deterministic_uuid(format!(
                    "pcb-kicad-sch:junction:{page_id}:{}:{}",
                    point.x, point.y
                )),
                at: point.to_point(),
                unsupported: Vec::new(),
            })
        })
        .collect()
}

pub fn branch_points_with_candidates(
    wires: &[Wire],
    additional_candidates: impl IntoIterator<Item = PointKey>,
) -> BTreeSet<PointKey> {
    wires
        .iter()
        .flat_map(|wire| [PointKey::from_point(wire.a), PointKey::from_point(wire.b)])
        .chain(additional_candidates)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|point| incident_directions(wires, *point).len() >= 3)
        .collect()
}

fn incident_directions(wires: &[Wire], point: PointKey) -> BTreeSet<(i8, i8)> {
    let mut directions = BTreeSet::new();
    for wire in wires {
        let a = PointKey::from_point(wire.a);
        let b = PointKey::from_point(wire.b);
        if !point_on_segment(point, a, b) {
            continue;
        }
        for endpoint in [a, b] {
            if endpoint == point {
                continue;
            }
            directions.insert((
                (endpoint.x - point.x).signum() as i8,
                (endpoint.y - point.y).signum() as i8,
            ));
        }
    }
    directions
}

pub fn replace_page_wires_and_junctions(
    page: &mut SchPage,
    wires: Vec<Wire>,
    junctions: Vec<Junction>,
) -> bool {
    let wire_index = page
        .items
        .iter()
        .position(|item| matches!(item, SchItem::Wire(_)));
    let junction_index = page
        .items
        .iter()
        .position(|item| matches!(item, SchItem::Junction(_)))
        .or(wire_index);
    let mut wires = Some(wires);
    let mut junctions = Some(junctions);
    let mut replacement = Vec::with_capacity(page.items.len());
    for (index, item) in page.items.iter().enumerate() {
        if junction_index == Some(index) {
            replacement.extend(
                junctions
                    .take()
                    .unwrap_or_default()
                    .into_iter()
                    .map(SchItem::Junction),
            );
        }
        if wire_index == Some(index) {
            replacement.extend(
                wires
                    .take()
                    .unwrap_or_default()
                    .into_iter()
                    .map(SchItem::Wire),
            );
        }
        if !matches!(item, SchItem::Wire(_) | SchItem::Junction(_)) {
            replacement.push(item.clone());
        }
    }
    replacement.extend(
        junctions
            .unwrap_or_default()
            .into_iter()
            .map(SchItem::Junction),
    );
    replacement.extend(wires.unwrap_or_default().into_iter().map(SchItem::Wire));
    if replacement == page.items {
        return false;
    }
    page.items = replacement;
    true
}

pub fn point_on_segment(point: PointKey, a: PointKey, b: PointKey) -> bool {
    let cross = (b.x - a.x) * (point.y - a.y) - (b.y - a.y) * (point.x - a.x);
    cross == 0
        && point.x >= a.x.min(b.x)
        && point.x <= a.x.max(b.x)
        && point.y >= a.y.min(b.y)
        && point.y <= a.y.max(b.y)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire(id: &str, a: (f64, f64), b: (f64, f64)) -> Wire {
        Wire {
            id: id.to_string(),
            a: Point::new(a.0, a.1),
            b: Point::new(b.0, b.1),
            unsupported: Vec::new(),
        }
    }

    #[test]
    fn materializes_endpoint_to_segment_branch() {
        let mut page = SchPage::new("page");
        let wires = vec![
            wire("vertical", (10.0, 0.0), (10.0, 20.0)),
            wire("branch", (0.0, 5.0), (10.0, 5.0)),
        ];
        assert!(reconcile_page_wires(
            &mut page,
            wires.clone(),
            &BTreeSet::new(),
            &BTreeSet::new()
        ));
        let junction = page
            .items
            .iter()
            .find_map(|item| match item {
                SchItem::Junction(junction) if junction.at == Point::new(10.0, 5.0) => {
                    Some(junction)
                }
                _ => None,
            })
            .expect("endpoint-to-segment branch gets an explicit junction");
        assert_eq!(
            junction.id,
            deterministic_uuid("pcb-kicad-sch:junction:page:100000:50000")
        );
        assert!(!reconcile_page_wires(
            &mut page,
            wires,
            &BTreeSet::new(),
            &BTreeSet::new()
        ));
    }
}
