use super::*;
use crate::geom::{
    BBox, ContourBuf, FillRule, GeometryAccuracy, LineCap, Mirror, PathCmd, Resolution, StrokeStyle,
};

const RESOLUTION: Resolution = Resolution {
    tolerance_mm: 0.0,
    accuracy: GeometryAccuracy::micrometres(10),
};

const TOL: QueryTolerance = QueryTolerance {
    boundary_mm: 0.0,
    numerical_mm: 1e-8,
};

fn rect(x: f64, y: f64, w: f64, h: f64) -> ContourSet {
    ContourSet::rectangle(
        BBox::new(Point::new(x, y), Point::new(x + w, y + h)),
        RESOLUTION,
    )
}

/// One footprint checked against one obstacle.
fn check<'a>(
    footprint: &ContourSet,
    obstacle: &'a ContourSet,
    clearance_mm: f64,
    tolerance: QueryTolerance,
) -> FootprintCheck<'a> {
    let obstacles = [Obstacle {
        id: "obstacle",
        region: obstacle,
    }];
    check_footprint(footprint, &obstacles, clearance_mm, tolerance)
        .unwrap()
        .remove(0)
}

fn close(a: f64, b: f64, tolerance: f64) {
    assert!((a - b).abs() < tolerance, "{a} != {b}");
}

#[test]
fn cyclic_projection_and_material_normals_preserve_hole_identity() {
    let material = rect(0.0, 0.0, 10.0, 10.0)
        .difference(&rect(3.0, 3.0, 4.0, 4.0))
        .unwrap()
        .union(&rect(20.0, 0.0, 2.0, 2.0))
        .unwrap();
    let query = BoundaryQuery::new(&material, TOL).unwrap();
    let ids = query.boundaries().collect::<Vec<_>>();
    assert_eq!(ids.len(), 3);
    let mut components = ids.iter().map(|id| id.component).collect::<Vec<_>>();
    components.sort();
    components.dedup();
    assert_eq!(components.len(), 2);
    for id in ids {
        let perimeter = query.perimeter(id).unwrap();
        let first = query.site(id, 0.5).unwrap();
        assert_eq!(first, query.site(id, perimeter + 0.5).unwrap());
        assert_eq!(first, query.site(id, 0.5 - perimeter).unwrap());
        close(first.tangent.length(), 1.0, 1e-10);
        let outside = first.point + first.outward_normal * 0.1;
        let inside = first.point - first.outward_normal * 0.1;
        assert!(!material.contains_point(outside));
        assert!(material.contains_point(inside));
        let projected = query.project(id, outside).unwrap();
        close(projected.distance.mm, 0.1, 1e-8);
        close(projected.site.point.distance_to(first.point), 0.0, 1e-8);
        assert_eq!(projected.site.boundary, id);
    }
}

#[test]
fn curved_contours_use_explicit_polygon_model() {
    let circle = ContourBuf::new(vec![
        PathCmd::move_to(Point::new(5.0, 0.0)),
        PathCmd::arc_to(Point::new(-5.0, 0.0), Point::ZERO, false),
        PathCmd::arc_to(Point::new(5.0, 0.0), Point::ZERO, false),
        PathCmd::close(),
    ]);
    let region = ContourSet::from_contours(&[circle], FillRule::NonZero, RESOLUTION).unwrap();
    let tolerance = QueryTolerance {
        boundary_mm: 0.0,
        ..TOL
    };
    let query = BoundaryQuery::new(&region, tolerance).unwrap();
    let id = query.boundaries().next().unwrap();
    close(
        query.perimeter(id).unwrap(),
        10.0 * std::f64::consts::PI,
        0.04,
    );
    let p = query.project(id, Point::new(6.0, 1.0)).unwrap();
    close(p.site.point.length(), 5.0, 0.006);
    close(
        p.site.tangent.x * p.site.outward_normal.x + p.site.tangent.y * p.site.outward_normal.y,
        0.0,
        1e-10,
    );
    assert!(p.distance.uncertainty_mm > region.uncertainty_mm);
}

#[test]
fn concavity_and_affine_reflection_keep_outward_normals_and_clearance() {
    let material = rect(0.0, 0.0, 6.0, 2.0)
        .union(&rect(0.0, 0.0, 2.0, 6.0))
        .unwrap();
    let transform = Affine2::placement(Point::new(30.0, -12.0), 37.0, Mirror::X, 2.0);
    let moved = transform_region(&material, transform).unwrap();
    let query = BoundaryQuery::new(&moved, TOL).unwrap();
    let id = query.boundaries().next().unwrap();
    close(query.perimeter(id).unwrap(), 48.0, 1e-6);
    for (a, b) in ring_edges(&moved.rings[id.ring]) {
        let midpoint = a.midpoint(b);
        let site = query.project(id, midpoint).unwrap().site;
        assert!(!moved.contains_point(site.point + site.outward_normal * 0.01));
        assert!(moved.contains_point(site.point - site.outward_normal * 0.01));
    }
    let attachment = transform_region(&rect(3.0, 3.0, 1.0, 1.0), transform).unwrap();
    let concave = check(&attachment, &moved, 1.5, TOL);
    assert_eq!(concave.decision, Decision::Admissible);
    close(concave.boundary_distance.unwrap().mm, 2.0, 1e-6);
}

#[test]
fn complete_footprints_detect_crossings_containment_and_holes() {
    let attachment = rect(-3.0, -0.25, 6.0, 0.5);
    // Crossing is far from attachment center; no attachment vertex is inside.
    let overhang = rect(1.0, -2.0, 0.5, 4.0);
    let clear = rect(3.2, 0.5, 0.2, 0.2);
    let hole = rect(-2.0, -0.1, 0.1, 0.1);
    let checks = check_footprint(
        &attachment,
        &[
            Obstacle {
                id: "overhang",
                region: &overhang,
            },
            Obstacle {
                id: "clear",
                region: &clear,
            },
            Obstacle {
                id: "existing hole",
                region: &hole,
            },
        ],
        0.0,
        TOL,
    )
    .unwrap();
    assert_eq!(checks.len(), 3);
    for index in [0, 2] {
        assert_eq!(
            checks[index].decision,
            Decision::Rejected(GeometricRejection::FootprintOverlap)
        );
        assert!(checks[index].overlap.area() > 0.0);
    }
    assert_eq!(checks[1].obstacle, "clear");
    assert_eq!(checks[1].decision, Decision::Admissible);
    let enclosing = rect(-10.0, -10.0, 20.0, 20.0);
    assert_eq!(
        check(&attachment, &enclosing, 0.0, TOL).decision,
        Decision::Rejected(GeometricRejection::FootprintOverlap)
    );
    // A hole in an obstacle is free space.
    let annulus = enclosing.difference(&rect(-5.0, -5.0, 10.0, 10.0)).unwrap();
    assert_eq!(
        check(&attachment, &annulus, 0.5, TOL).decision,
        Decision::Admissible
    );
}

#[test]
fn tolerance_ambiguity_is_not_geometric_rejection() {
    let attachment = rect(0.0, 0.0, 1.0, 1.0);
    let obstacle = rect(1.1, 0.0, 1.0, 1.0);
    let tolerance = QueryTolerance {
        boundary_mm: 0.01,
        ..TOL
    };
    let decision = |clearance| check(&attachment, &obstacle, clearance, tolerance).decision;
    assert_eq!(decision(0.07), Decision::Admissible);
    assert!(matches!(decision(0.1), Decision::Unresolved(_)));
    assert!(matches!(
        decision(0.13),
        Decision::Rejected(GeometricRejection::InsufficientClearance { .. })
    ));
    assert!(matches!(
        check(&attachment, &attachment, 0.0, tolerance).decision,
        Decision::Unresolved(_)
    ));
}

#[test]
fn supplied_break_sweep_separates_only_when_last_ligament_is_removed() {
    let material = rect(0.0, 0.0, 10.0, 4.0)
        .difference(&rect(4.5, 1.0, 1.0, 2.0))
        .unwrap();
    let points = [
        Point::new(1.0, 2.0),
        Point::new(9.0, 2.0),
        Point::new(5.0, 2.0),
    ];
    let before =
        material_after_break(&material, &ContourSet::empty(RESOLUTION), &points, TOL).unwrap();
    assert_eq!(before.connected(0, 1).unwrap(), Some(true));
    assert_eq!(before.connected(0, 2).unwrap(), None);
    let partial =
        material_after_break(&material, &rect(4.8, -1.0, 0.4, 4.9999), &points, TOL).unwrap();
    assert_eq!(partial.connected(0, 1).unwrap(), Some(true));
    let path = ContourBuf::new(vec![
        PathCmd::move_to(Point::new(5.0, -1.0)),
        PathCmd::line_to(Point::new(5.0, 5.0)),
    ]);
    let sweep = crate::geom::path::stroke_to_fill(
        &[path],
        StrokeStyle::new(0.4, LineCap::Round),
        RESOLUTION.accuracy,
    )
    .unwrap()
    .unwrap();
    let removal = ContourSet::from_contours(&sweep, FillRule::NonZero, RESOLUTION).unwrap();
    let after = material_after_break(&material, &removal, &points, TOL).unwrap();
    assert_eq!(after.components.len(), 2);
    assert_eq!(after.connected(0, 1).unwrap(), Some(false));
    assert_eq!(after.connected(0, 2).unwrap(), None);
    let transform = Affine2::placement(Point::new(-8.0, 7.0), 22.0, Mirror::Y, 1.0);
    let transformed = material_after_break(
        &transform_region(&material, transform).unwrap(),
        &transform_region(&removal, transform).unwrap(),
        &points.map(|p| transform.transform_point(p)),
        TOL,
    )
    .unwrap();
    assert_eq!(transformed.connected(0, 1).unwrap(), Some(false));
}

#[test]
fn boundary_witnesses_are_unresolved_not_separation() {
    let region = rect(0.0, 0.0, 4.0, 4.0);
    let empty = ContourSet::empty(RESOLUTION);
    let points = [
        Point::new(2.0, 2.0),
        Point::new(-1.0, 2.0),
        Point::new(0.0, 2.0),
    ];
    let topology = material_after_break(&region, &empty, &points, TOL).unwrap();
    assert!(matches!(
        topology.witnesses[0],
        RegionMembership::Component(_)
    ));
    assert_eq!(topology.witnesses[1], RegionMembership::Outside);
    assert_eq!(topology.witnesses[2], RegionMembership::BoundaryBand);
    assert_eq!(topology.connected(0, 1).unwrap(), None);
    assert_eq!(topology.connected(0, 2).unwrap(), None);
}

#[test]
fn preparation_uncertainty_and_budget_survive_queries_and_transforms() {
    let original = rect(0.0, 0.0, 1.0, 1.0);
    let prepared = ContourSet::from_regularized(original.rings, RESOLUTION, 0.002);
    let transformed = transform_region(
        &prepared,
        Affine2::placement(Point::ZERO, 17.0, Mirror::X, 2.0),
    )
    .unwrap();
    assert!(transformed.uncertainty_mm >= 0.004);
    assert_eq!(transformed.resolution, RESOLUTION);
    let obstacle = rect(1.003, 0.0, 1.0, 1.0);
    let near = check(&prepared, &obstacle, 0.002, TOL);
    assert!(matches!(near.decision, Decision::Unresolved(_)));
    assert!(near.boundary_distance.unwrap().uncertainty_mm >= 0.002);
    assert!(matches!(
        transform_region(
            &prepared,
            Affine2::placement(Point::ZERO, 0.0, Mirror::NONE, 10.0)
        ),
        Err(QueryError::Accuracy(AccuracyError::BudgetExceeded { .. }))
    ));
}

#[test]
fn transforms_map_vertices_and_keep_each_ring_winding() {
    let region = rect(0.0, 0.0, 4.0, 2.0)
        .difference(&rect(1.0, 0.5, 1.0, 1.0))
        .unwrap();
    assert_eq!(region.rings.len(), 2);
    // A translation is the same polygon, vertex for vertex.
    let moved = transform_region(&region, Affine2::translation(Point::new(10.0, -3.0))).unwrap();
    for (ring, moved) in region.rings.iter().zip(&moved.rings) {
        let expected = ring
            .iter()
            .map(|p| [p[0] + 10.0, p[1] - 3.0])
            .collect::<Vec<_>>();
        assert_eq!(*moved, expected);
    }
    // A reflection keeps material outside counter-clockwise and holes
    // clockwise without being regularized again.
    let mirrored = transform_region(
        &region,
        Affine2::placement(Point::new(5.0, 1.0), 30.0, Mirror::X, 1.0),
    )
    .unwrap();
    for (ring, mirrored) in region.rings.iter().zip(&mirrored.rings) {
        close(
            crate::geom::region::ring_signed_area(mirrored),
            crate::geom::region::ring_signed_area(ring),
            1e-9,
        );
    }
    close(mirrored.area(), region.area(), 1e-9);
    assert!(
        mirrored.contains_point(
            Affine2::placement(Point::new(5.0, 1.0), 30.0, Mirror::X, 1.0)
                .transform_point(Point::new(3.5, 1.0))
        )
    );
}

#[test]
fn empty_transform_keeps_scaled_uncertainty_and_budget() {
    let empty = ContourSet::from_regularized(vec![], RESOLUTION, 0.002);
    let transform = Affine2::placement(Point::new(1.0, 2.0), 30.0, Mirror::X, 2.0);
    let result = transform_region(&empty, transform).unwrap();
    assert!(result.is_empty());
    close(result.uncertainty_mm, 0.004, 1e-12);
    assert_eq!(result.resolution, RESOLUTION);
    assert!(matches!(
        transform_region(
            &empty,
            Affine2::placement(Point::ZERO, 0.0, Mirror::NONE, 10.0)
        ),
        Err(QueryError::Accuracy(AccuracyError::BudgetExceeded { .. }))
    ));
}

#[test]
fn stored_membership_uncertainty_keeps_additive_numerical_guard() {
    let material = ContourSet::from_regularized(rect(0.0, 0.0, 4.0, 4.0).rings, RESOLUTION, 0.002);
    let empty = ContourSet::empty(RESOLUTION);
    let tolerance = QueryTolerance {
        boundary_mm: 0.0,
        numerical_mm: 0.001,
    };
    let topology =
        material_after_break(&material, &empty, &[Point::new(0.0025, 2.0)], tolerance).unwrap();
    assert_eq!(topology.witnesses, vec![RegionMembership::BoundaryBand]);
}

#[test]
fn numerical_overlap_requires_a_deep_interior_witness() {
    let footprint = rect(0.0, 0.0, 1.0, 1.0);
    let tolerance = QueryTolerance {
        numerical_mm: 0.001,
        ..TOL
    };
    for (depth, rejected) in [(0.0005, false), (0.001, false), (0.01, true)] {
        let obstacle = rect(1.0 - depth, 0.0, 1.0, 1.0);
        let decision = check(&footprint, &obstacle, 0.0, tolerance).decision;
        if rejected {
            assert_eq!(
                decision,
                Decision::Rejected(GeometricRejection::FootprintOverlap)
            );
        } else {
            assert!(matches!(decision, Decision::Unresolved(_)));
        }
    }
}

#[test]
fn transform_preserves_nonzero_significance_for_following_operations() {
    let resolution = RESOLUTION.with_tolerance(0.1);
    let region = ContourSet::rectangle(BBox::new(Point::ZERO, Point::new(2.0, 2.0)), resolution);
    let moved = transform_region(&region, Affine2::IDENTITY).unwrap();
    assert_eq!(moved.resolution, resolution);
    assert!(
        moved
            .intersection(&rect(0.0, 0.0, 0.05, 0.05))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn invalid_queries_are_errors_not_geometric_rejections() {
    let region = rect(0.0, 0.0, 1.0, 1.0);
    assert!(
        BoundaryQuery::new(
            &region,
            QueryTolerance {
                boundary_mm: f64::NAN,
                ..TOL
            }
        )
        .is_err()
    );
    let query = BoundaryQuery::new(&region, TOL).unwrap();
    let id = query.boundaries().next().unwrap();
    assert!(query.site(id, f64::INFINITY).is_err());
    assert!(query.project(id, Point::new(f64::NAN, 0.0)).is_err());
    assert!(
        query
            .site(
                BoundaryId {
                    component: 99,
                    ..id
                },
                0.0
            )
            .is_err()
    );
    assert!(
        transform_region(
            &region,
            Affine2 {
                m00: 0.0,
                ..Affine2::IDENTITY
            }
        )
        .is_err()
    );
}
