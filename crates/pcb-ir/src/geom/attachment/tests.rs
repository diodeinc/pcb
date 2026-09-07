use super::*;
use crate::geom::{
    BBox, ContourBuf, GeometryAccuracy, LineCap, LineJoin, Mirror, PathCmd, Resolution,
    StrokeToFillStyle,
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
fn intervals_retain_every_narrow_window_and_hole_split() {
    let material = rect(0.0, 0.0, 10.0, 3.0);
    let query = BoundaryQuery::new(&material, TOL).unwrap();
    let id = query.boundaries().next().unwrap();
    // More windows than a typical candidate cap, each much smaller than a
    // plausible sampling step. The hole removes a central part of one window.
    let windows = (0..80).fold(ContourSet::empty(RESOLUTION), |r, i| {
        r.union(&rect(0.1 + i as f64 * 0.1, -0.1, 0.0001, 0.2))
            .unwrap()
    });
    let windows = windows
        .difference(&rect(0.10004, -0.05, 0.00002, 0.1))
        .unwrap();
    let intervals = query.usable_intervals(id, &windows).unwrap();
    assert_eq!(intervals.len(), 81);
    close(
        intervals.iter().map(|i| i.end_mm - i.start_mm).sum(),
        0.00798,
        1e-6,
    );
    for interval in intervals {
        assert!(windows.contains_point(query.interval_site(interval).unwrap().point));
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
    let intervals = query
        .usable_intervals(id, &rect(4.0, -1.0, 2.0, 2.0))
        .unwrap();
    assert!(!intervals.is_empty());
    for interval in intervals {
        assert!(query.interval_site(interval).unwrap().point.x > 4.0);
    }
    let cubic = ContourBuf::new(vec![
        PathCmd::move_to(Point::ZERO),
        PathCmd::cubic_to(
            Point::new(0.0, 4.0),
            Point::new(4.0, 4.0),
            Point::new(4.0, 0.0),
        ),
        PathCmd::close(),
    ]);
    let region = ContourSet::from_contours(&[cubic], FillRule::EvenOdd, RESOLUTION).unwrap();
    let query = BoundaryQuery::new(&region, tolerance).unwrap();
    let id = query.boundaries().next().unwrap();
    close(
        query.project(id, Point::new(2.0, 4.0)).unwrap().distance.mm,
        1.0,
        0.006,
    );
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
    let checks = check_footprints(
        &attachment,
        &ContourSet::empty(RESOLUTION),
        &[Obstacle {
            id: "concave board",
            region: &moved,
        }],
        1.5,
        TOL,
    )
    .unwrap();
    assert_eq!(checks[0].decision, Decision::Admissible);
    close(checks[0].boundary_distance.unwrap().mm, 2.0, 1e-6);
}

#[test]
fn complete_footprints_detect_crossings_containment_shoulders_and_holes() {
    let attachment = rect(-3.0, -0.25, 6.0, 0.5);
    let shoulder = rect(2.5, -1.0, 1.0, 2.0);
    // Crossing is far from attachment center; no attachment vertex is inside.
    let overhang = rect(1.0, -2.0, 0.5, 4.0);
    let shoulder_obstacle = rect(3.2, 0.5, 0.2, 0.2);
    let hole = rect(-2.0, -0.1, 0.1, 0.1);
    let checks = check_footprints(
        &attachment,
        &shoulder,
        &[
            Obstacle {
                id: "overhang",
                region: &overhang,
            },
            Obstacle {
                id: "shoulder only",
                region: &shoulder_obstacle,
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
    assert_eq!(checks.len(), 6);
    for index in [0, 3, 4] {
        assert_eq!(
            checks[index].decision,
            Decision::Rejected(GeometricRejection::FootprintOverlap)
        );
        assert!(checks[index].overlap.area() > 0.0);
    }
    assert_eq!(checks[3].part, FootprintPart::RouterShoulder);
    assert_eq!(checks[3].obstacle, "shoulder only");
    let enclosing = rect(-10.0, -10.0, 20.0, 20.0);
    assert_eq!(
        check_footprints(
            &attachment,
            &shoulder,
            &[Obstacle {
                id: "enclosing",
                region: &enclosing
            }],
            0.0,
            TOL
        )
        .unwrap()[0]
            .decision,
        Decision::Rejected(GeometricRejection::FootprintOverlap)
    );
    let annulus = enclosing.difference(&rect(-5.0, -5.0, 10.0, 10.0)).unwrap();
    assert_eq!(
        check_footprints(
            &attachment,
            &shoulder,
            &[Obstacle {
                id: "free hole",
                region: &annulus
            }],
            0.5,
            TOL
        )
        .unwrap()[0]
            .decision,
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
    let check = |clearance| {
        check_footprints(
            &attachment,
            &ContourSet::empty(RESOLUTION),
            &[Obstacle {
                id: "near",
                region: &obstacle,
            }],
            clearance,
            tolerance,
        )
        .unwrap()[0]
            .decision
            .clone()
    };
    assert_eq!(check(0.07), Decision::Admissible);
    assert!(matches!(check(0.1), Decision::Unresolved(_)));
    assert!(matches!(
        check(0.13),
        Decision::Rejected(GeometricRejection::InsufficientClearance { .. })
    ));
    assert!(matches!(
        check_footprints(
            &attachment,
            &attachment,
            &[Obstacle {
                id: "same",
                region: &attachment
            }],
            0.0,
            tolerance
        )
        .unwrap()[0]
            .decision,
        Decision::Unresolved(_)
    ));
}

#[test]
fn cutter_reachability_detects_closed_holes_and_radius_limited_necks() {
    let workspace = rect(-5.0, -5.0, 10.0, 10.0);
    let ring = rect(-3.0, -3.0, 6.0, 6.0)
        .difference(&rect(-1.0, -1.0, 2.0, 2.0))
        .unwrap();
    let reach = cutter_reachability(
        &workspace,
        &ring,
        0.2,
        &[Point::new(-4.0, 0.0)],
        &[Point::new(4.0, 0.0), Point::ZERO],
        TOL,
    )
    .unwrap();
    assert_eq!(reach.targets[0], Decision::Admissible);
    assert_eq!(
        reach.targets[1],
        Decision::Rejected(GeometricRejection::Unreachable { target: 1 })
    );
    let free = rect(0.0, 0.0, 3.0, 4.0)
        .union(&rect(3.0, 1.7, 4.0, 0.6))
        .unwrap()
        .union(&rect(7.0, 0.0, 3.0, 4.0))
        .unwrap();
    let empty = ContourSet::empty(RESOLUTION);
    for (radius, reachable) in [(0.2, true), (0.4, false)] {
        let result = cutter_reachability(
            &free,
            &empty,
            radius,
            &[Point::new(1.0, 2.0)],
            &[Point::new(9.0, 2.0)],
            TOL,
        )
        .unwrap();
        assert_eq!(result.targets[0] == Decision::Admissible, reachable);
    }
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
        StrokeToFillStyle::new(0.4, LineCap::Round, LineJoin::Round),
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
fn boundary_witnesses_are_unresolved_not_access_or_separation() {
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
    let tolerance = QueryTolerance {
        boundary_mm: 0.001,
        ..TOL
    };
    let reach = cutter_reachability(
        &region,
        &empty,
        0.5,
        &[Point::new(0.5, 2.0)],
        &points,
        tolerance,
    )
    .unwrap();
    assert_eq!(reach.entries, vec![RegionMembership::BoundaryBand]);
    assert!(matches!(reach.targets[0], Decision::Unresolved(_)));
    assert_eq!(
        reach.targets[1],
        Decision::Rejected(GeometricRejection::OutsideCutterSpace { point: 1 })
    );
    let multiple_entries = cutter_reachability(
        &region,
        &empty,
        0.5,
        &[Point::new(0.5, 2.0), points[0]],
        &points[..1],
        tolerance,
    )
    .unwrap();
    assert_eq!(multiple_entries.targets[0], Decision::Admissible);
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
    let checks = check_footprints(
        &prepared,
        &ContourSet::empty(RESOLUTION),
        &[Obstacle {
            id: "near",
            region: &obstacle,
        }],
        0.002,
        TOL,
    )
    .unwrap();
    assert!(matches!(checks[0].decision, Decision::Unresolved(_)));
    assert!(checks[0].boundary_distance.unwrap().uncertainty_mm >= 0.002);
    assert!(matches!(
        transform_region(
            &prepared,
            Affine2::placement(Point::ZERO, 0.0, Mirror::NONE, 10.0)
        ),
        Err(QueryError::Accuracy(AccuracyError::BudgetExceeded { .. }))
    ));
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
    assert!(cutter_reachability(&region, &region, -1.0, &[Point::ZERO], &[], TOL).is_err());
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
