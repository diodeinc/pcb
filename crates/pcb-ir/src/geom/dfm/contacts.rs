//! Clearance at intentional contacts is measured between exposed edge pairs,
//! not the global set distance (which is necessarily zero). Only the exposed
//! source boundaries participate: clipping must not create new copper edges.

use super::{BBoxIndex, ClearanceSite, Distance};
use crate::geom::region::ring_edges;
use crate::geom::{AccuracyError, BBox, ContourSet, Point, dist, tol};

/// Find undeclared contacts and gaps between two copper regions
/// with intentional, location-specific contacts. Each location must identify
/// exactly one positive-area intersection component. No clearance radius is
/// exempted and other intersection components remain zero-clearance sites.
///
/// An edge pair meeting at an authorized contact is exempt, not a whole
/// connected pair of nets. Separate edge pairs bounding open air remain
/// checked, including sloping slots draining into the contact. This can be
/// conservative for segmented boundaries near a contact.
///
/// Inputs must be unfiltered. `approximation_bounds` must cover where source
/// approximation could change the pair's intersection near its contacts.
/// Expanded bounds of every approximated paint operand (dark and clear) are
/// sufficient; callers may tighten them using guaranteed/possible material.
/// Only contacts outside those bounds can be authorized; other distances
/// retain the regions' uncertainty.
pub fn region_clearance_sites_except_contacts(
    first: &ContourSet,
    second: &ContourSet,
    locations: &[Point],
    location_tolerance_mm: f64,
    approximation_bounds: &[BBox],
    minimum_mm: f64,
) -> Result<Vec<ClearanceSite>, AccuracyError> {
    let uncertainty = first.uncertainty_mm + second.uncertainty_mm;
    if first.resolution.tolerance_mm != 0.0 || second.resolution.tolerance_mm != 0.0 {
        return Err(AccuracyError::InvalidGeometry(
            "NetShort has uncertain contact topology",
        ));
    }
    let overlaps = first.intersection(second)?.connected_components();
    let overlap_queries = overlaps
        .iter()
        .map(ContourSet::prepare_query)
        .collect::<Vec<_>>();
    let mut authorized = vec![false; overlaps.len()];
    for &location in locations {
        let matches = overlap_queries
            .iter()
            .enumerate()
            .filter_map(|(i, query)| {
                query
                    .signed_distance(location)
                    .is_some_and(|d| d.mm <= location_tolerance_mm)
                    .then_some(i)
            })
            .collect::<Vec<_>>();
        let [index] = matches.as_slice() else {
            return Err(AccuracyError::InvalidGeometry(
                "NetShort must identify exactly one positive-area contact; edge/point-only contacts are unsupported",
            ));
        };
        if approximation_bounds
            .iter()
            .any(|bounds| bounds.intersects(overlaps[*index].bbox.expand(tol::EPSILON_MM)))
        {
            return Err(AccuracyError::InvalidGeometry(
                "NetShort has uncertain contact topology: declared contact adjoins approximated copper",
            ));
        }
        authorized[*index] = true;
    }
    let mut sites = overlaps
        .iter()
        .zip(&authorized)
        .filter_map(|(overlap, &allowed)| {
            if allowed {
                return None;
            }
            let [x, y] = overlap.rings[0][0];
            let point = Point::new(x, y);
            Some(ClearanceSite {
                distance: Distance::with_uncertainty(0.0, point, point, uncertainty),
                bbox: overlap.bbox,
                first_paths: Vec::new(),
                second_paths: Vec::new(),
                overlap: overlap.clone(),
            })
        })
        .collect::<Vec<_>>();

    let first_query = first.prepare_query();
    let second_query = second.prepare_query();
    let exposed = |source: &ContourSet, query: &crate::geom::region::PreparedRegion| {
        let mut edges = Vec::new();
        for (start, end) in source.rings.iter().flat_map(ring_edges) {
            let mut stations = vec![0.0, 1.0];
            for (low, high) in query.interior_intervals(start, end) {
                stations.extend([low, high]);
            }
            stations.sort_by(f64::total_cmp);
            stations.dedup();
            for pair in stations.windows(2) {
                let a = start + (end - start) * pair[0];
                let b = start + (end - start) * pair[1];
                if a.distance_to(b) > tol::EPSILON_MM
                    && query
                        .signed_distance(a.midpoint(b))
                        .is_some_and(|d| d.mm >= -tol::EPSILON_MM)
                {
                    edges.push((a, b));
                }
            }
        }
        edges
    };
    let left = exposed(first, &second_query);
    let right = exposed(second, &first_query);
    let bounds = |&(a, b): &(Point, Point)| BBox::spanning(a, b);
    let right_index = BBoxIndex::new(right.iter().map(bounds).collect());
    let in_overlap = |point: Point| {
        overlap_queries.iter().any(|q| {
            q.signed_distance(point)
                .is_some_and(|d| d.mm <= tol::EPSILON_MM)
        })
    };

    for &(a, b) in &left {
        for id in right_index.query(BBox::spanning(a, b).expand(minimum_mm)) {
            let (c, d) = right[id];
            for (mm, p, q) in closest(a, b, c, d) {
                if mm >= minimum_mm {
                    continue;
                }
                if mm <= tol::EPSILON_MM {
                    // Area contacts already have their own sites. In particular,
                    // never discard a second overlap merely because this pair
                    // of nets also has an authorized contact somewhere else.
                    if in_overlap(p) {
                        continue;
                    }
                } else {
                    // Regularized rings keep material on the left. Both
                    // edges must face the gap, even when its limiting chord
                    // follows copper at a slot's closed end. This excludes
                    // the exterior corner of a protruding pad without
                    // discarding a tilted slot as a monotone opening.
                    let faces = |start: Point, end: Point, toward: Point| {
                        let edge = end - start;
                        (edge.y * toward.x - edge.x * toward.y) / edge.length() > tol::EPSILON_MM
                    };
                    if !faces(a, b, q - p) || !faces(c, d, p - q) {
                        continue;
                    }
                    // Test each open interval, not just the chord midpoint: a
                    // chord may cross a thin island or a hole near either end.
                    if [&first_query, &second_query].iter().any(|query| {
                        query.interior_intervals(p, q).iter().any(|&(low, high)| {
                            query
                                .signed_distance(p + (q - p) * ((low + high) / 2.0))
                                .is_some_and(|distance| distance.mm < -tol::EPSILON_MM)
                        })
                    }) {
                        continue;
                    }
                }
                if sites.iter().any(|site| {
                    site.distance.first.distance_to(p) <= tol::EPSILON_MM
                        && site.distance.second.distance_to(q) <= tol::EPSILON_MM
                }) {
                    continue;
                }
                sites.push(ClearanceSite {
                    distance: Distance::with_uncertainty(mm, p, q, uncertainty),
                    bbox: BBox::spanning(p, q),
                    // Witnesses are edge-pair minima, not a claim that each whole
                    // source edge falls below the limit.
                    first_paths: Vec::new(),
                    second_paths: Vec::new(),
                    overlap: ContourSet::empty(first.resolution),
                });
            }
        }
    }
    Ok(sites)
}

fn dot(a: Point, b: Point) -> f64 {
    a.x * b.x + a.y * b.y
}

// Parallel edges have a flat set of minima. Pick its interior so a reflex
// endpoint cannot discard an otherwise valid parallel gap. Zero-length gap
// intervals also need both ends, lest an allowed endpoint hide an undeclared
// collinear contact farther along the same edges.
fn closest(a: Point, b: Point, c: Point, d: Point) -> Vec<(f64, Point, Point)> {
    let u = (b - a) / b.distance_to(a);
    let v = (d - c) / d.distance_to(c);
    if (u.x * v.y - u.y * v.x).abs() <= f64::EPSILON * 16.0 {
        let low = dot(c - a, u).min(dot(d - a, u)).max(0.0);
        let high = dot(c - a, u).max(dot(d - a, u)).min(b.distance_to(a));
        if high > low {
            let p = a + u * ((low + high) / 2.0);
            let (mm, q) = dist::point_segment(p, c, d);
            if mm > tol::EPSILON_MM {
                return vec![(mm, p, q)];
            }
            return [low, (low + high) / 2.0, high]
                .map(|t| {
                    let p = a + u * t;
                    let (mm, q) = dist::point_segment(p, c, d);
                    (mm, p, q)
                })
                .to_vec();
        }
    }
    vec![dist::segments(a, b, c, d)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::{
        Affine2, FillRule, Resolution,
        path::{ContourBuf, PathCmd},
    };

    fn polygon(points: &[[f64; 2]]) -> ContourSet {
        let mut commands = vec![PathCmd::move_to(Point::new(points[0][0], points[0][1]))];
        commands.extend(
            points[1..]
                .iter()
                .map(|p| PathCmd::line_to(Point::new(p[0], p[1]))),
        );
        commands.push(PathCmd::close());
        ContourSet::from_contours(
            &[ContourBuf::new(commands)],
            FillRule::NonZero,
            Resolution::default().strict(),
        )
        .unwrap()
    }

    fn rect(x: f64, y: f64, right: f64, top: f64) -> ContourSet {
        polygon(&[[x, y], [right, y], [right, top], [x, top]])
    }

    fn sites(a: &ContourSet, b: &ContourSet, contacts: &[Point]) -> Vec<ClearanceSite> {
        region_clearance_sites_except_contacts(a, b, contacts, 0.000002, &[], 0.15).unwrap()
    }

    #[test]
    fn protruding_pad_does_not_create_clearance_against_its_connected_arm() {
        let pad = rect(0.0, -0.3, 0.5, 0.3);
        let arm = rect(-5.0, -0.25, 5.0, 0.25);
        for (a, b) in [(&pad, &arm), (&arm, &pad)] {
            assert!(sites(a, b, &[Point::new(0.2, 0.0)]).is_empty());
        }
    }

    #[test]
    fn a_slot_does_not_disappear_when_its_wall_is_tilted() {
        let ground = rect(0.0, 0.0, 2.0, 1.0);
        for tilt in [-0.003, -3e-5, -3e-8, 0.0, 3e-8, 3e-5, 0.003] {
            let feed = rect(-2.0, 0.4, 0.5, 0.6)
                .union(&polygon(&[
                    [-0.3, 0.5],
                    [-0.05, 0.5],
                    [-0.05, 0.6],
                    [-0.05 - tilt, 0.9],
                    [-0.3, 0.9],
                ]))
                .unwrap();
            for (a, b) in [(&feed, &ground), (&ground, &feed)] {
                let found = sites(a, b, &[Point::new(0.2, 0.5)]);
                let expected = 0.05 + tilt.min(0.0);
                assert!(
                    found
                        .iter()
                        .any(|s| (s.distance.mm - expected).abs() < 1e-9),
                    "tilt {tilt}: {found:?}"
                );
            }
        }
    }

    #[test]
    fn location_rounding_never_creates_or_merges_contacts() {
        let ground = rect(0.0, 0.0, 2.0, 1.0);
        let feed = rect(-2.0, 0.4, 0.5, 0.6);
        let check = |feed: &ContourSet, point| {
            region_clearance_sites_except_contacts(feed, &ground, &[point], 0.000002, &[], 0.15)
        };
        assert!(
            check(&feed, Point::new(-0.0000005, 0.4))
                .unwrap()
                .is_empty()
        );
        assert!(check(&feed, Point::new(-0.000003, 0.4)).is_err());
        // Nearby but disjoint copper is not a contact, even within the
        // declaration's coordinate-rounding allowance.
        assert!(check(&rect(-2.0, 0.4, -0.000001, 0.6), Point::new(0.0, 0.5)).is_err());
        let two = rect(-1.0, 0.0, 1.0, 0.4)
            .union(&rect(-1.0, 0.400001, 1.0, 0.8))
            .unwrap()
            .union(&rect(-1.0, 0.0, -0.5, 1.0))
            .unwrap();
        assert!(check(&two, Point::new(0.0, 0.4000005)).is_err());
    }

    #[test]
    fn same_connected_pair_retains_parallel_gap_and_second_contact() {
        let ground = rect(0.0, 0.0, 2.0, 1.0);
        let contact = Point::new(0.2, 0.5);
        let feed = rect(-2.0, 0.4, 0.5, 0.6)
            .union(&rect(-2.0, 0.4, -1.8, 1.3))
            .unwrap()
            .union(&rect(-2.0, 1.05, 2.0, 1.3))
            .unwrap();
        assert_eq!(feed.connected_components().len(), 1);
        for (a, b) in [(&feed, &ground), (&ground, &feed)] {
            let gaps = sites(a, b, &[contact]);
            assert!(
                gaps.iter().any(|s| (s.distance.mm - 0.05).abs() < 1e-9),
                "{gaps:?}"
            );
            assert!(gaps.iter().all(|s| s.distance.mm > 0.0));
        }
        for angle in [0.0, 37.0, 90.0, 179.0] {
            for mirror in [crate::geom::Mirror::NONE, crate::geom::Mirror::X] {
                let transform =
                    Affine2::placement(Point::new(168.9, -100.439392), angle, mirror, 1.0);
                let placed = |region: &ContourSet| {
                    ContourSet::from_contours(
                        &region
                            .to_contours()
                            .into_iter()
                            .map(|c| c.transformed(transform))
                            .collect::<Vec<_>>(),
                        FillRule::NonZero,
                        Resolution::default().strict(),
                    )
                    .unwrap()
                };
                let found = sites(
                    &placed(&feed),
                    &placed(&ground),
                    &[transform.transform_point(contact)],
                );
                assert!(
                    found.iter().any(|s| (s.distance.mm - 0.05).abs() < 1e-9),
                    "rotation {angle}, {mirror:?}: {found:?}"
                );
            }
        }
        let feed = feed.union(&rect(1.6, 0.5, 1.8, 1.3)).unwrap();
        let one = sites(&feed, &ground, &[contact]);
        assert_eq!(one.iter().filter(|s| !s.overlap.is_empty()).count(), 1);
        let both = sites(&feed, &ground, &[contact, Point::new(1.7, 0.6)]);
        assert!(both.iter().all(|s| s.distance.mm > 0.0));
        assert!(both.iter().any(|s| (s.distance.mm - 0.05).abs() < 1e-9));
    }

    #[test]
    fn parallel_gap_limit_is_not_an_exemption_radius() {
        let ground = rect(0.0, 0.0, 2.0, 1.0);
        for gap in [0.149, 0.151] {
            let feed = rect(-2.0, 0.4, 0.5, 0.6)
                .union(&rect(-2.0, 0.4, -1.8, 1.4))
                .unwrap()
                .union(&rect(-2.0, 1.0 + gap, 2.0, 1.4))
                .unwrap();
            let found = sites(&feed, &ground, &[Point::new(0.2, 0.5)]);
            if gap < 0.15 {
                assert!(found.iter().any(|s| (s.distance.mm - gap).abs() < 1e-9));
            } else {
                assert!(found.is_empty(), "{found:?}");
            }
        }
    }

    #[test]
    fn clipped_endpoint_preserves_a_narrowing_gap_next_to_the_contact() {
        let ground = polygon(&[
            [0.0, 0.0],
            [2.0, 0.0],
            [2.0, 1.0],
            [-1.0, 1.0],
            [-1.0, 0.7],
            [0.35, 0.7],
        ]);
        let feed = polygon(&[[-1.0, 0.4], [0.5, 0.4], [0.5, 0.65], [-1.0, 0.5]]);
        // The exposed feed edge y = 0.6 + 0.1x ends at ground's
        // x = y/2 wall: y = 12/19. The slot ceiling is y = 0.7.
        let expected = 0.7 - 12.0 / 19.0;
        for (a, b) in [(&feed, &ground), (&ground, &feed)] {
            let found = sites(a, b, &[Point::new(0.4, 0.5)]);
            assert!(
                found
                    .iter()
                    .any(|s| (s.distance.mm - expected).abs() < 1e-9),
                "{found:?}"
            );
        }
    }

    #[test]
    fn undeclared_edge_contact_and_tiny_second_overlap_are_not_dropped() {
        let ground = rect(0.0, 0.0, 2.0, 1.0);
        for bottom in [1.0, 0.99999] {
            let feed = rect(-2.0, 0.4, 0.5, 0.6)
                .union(&rect(-2.0, 0.4, -1.8, 2.0))
                .unwrap()
                .union(&rect(-2.0, 1.8, 2.0, 2.0))
                .unwrap()
                .union(&rect(1.6, bottom, 1.8, 2.0))
                .unwrap();
            let found = sites(&feed, &ground, &[Point::new(0.2, 0.5)]);
            assert!(
                found
                    .iter()
                    .any(|s| s.distance.mm <= tol::EPSILON_MM && s.distance.first.x >= 1.6),
                "{found:?}"
            );
        }
    }
}
