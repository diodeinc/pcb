use super::*;
use crate::geom::{FillRule, GeometryAccuracy, Mirror, Resolution};

fn resolution() -> Resolution {
    Resolution::new(0.0, GeometryAccuracy::new(1e-5).unwrap())
}

fn rectangle(x0: f64, y0: f64, x1: f64, y1: f64) -> ContourSet {
    ContourSet::rectangle(
        BBox::new(Point::new(x0, y0), Point::new(x1, y1)),
        resolution(),
    )
}

fn footprint() -> OutlineFootprint {
    OutlineFootprint {
        width_mm: 2.0,
        inward_mm: 0.5,
        outward_mm: 2.0,
    }
}

fn tolerance() -> QueryTolerance {
    QueryTolerance {
        boundary_mm: 0.0,
        numerical_mm: 1e-8,
    }
}

fn classify(
    board: &ContourSet,
    obstacle: &ContourSet,
    footprint: OutlineFootprint,
) -> Vec<OutlineInterval> {
    eligible_outline(
        board,
        &[OutlineObstacle {
            id: "part",
            region: Some(obstacle),
        }],
        footprint,
        tolerance(),
    )
    .unwrap()
}

fn bottom_state(intervals: &[OutlineInterval], x: f64) -> OutlineState {
    intervals
        .iter()
        .find(|i| i.start.y.abs() < 1e-8 && i.end.y.abs() < 1e-8 && i.start.x < x && x < i.end.x)
        .unwrap()
        .state
}

#[test]
fn continuous_full_footprint_not_centre_or_overhang_only() {
    let board = rectangle(0.0, 0.0, 20.0, 10.0);
    let original = board.rings.clone();
    // Entirely outside the board and not touching its outline. A 2 mm-deep
    // tab still intersects it. Nominal blocked centre interval is (7,12).
    let obstacle = rectangle(8.0, -1.8, 11.0, -0.2);
    let intervals = classify(&board, &obstacle, footprint());
    assert_eq!(bottom_state(&intervals, 6.0), OutlineState::Eligible);
    assert_eq!(bottom_state(&intervals, 7.5), OutlineState::Blocked);
    assert_eq!(bottom_state(&intervals, 12.5), OutlineState::Eligible);
    assert_eq!(bottom_state(&intervals, 7.0), OutlineState::Unknown);
    assert_eq!(board.rings, original);
    let length: f64 = intervals.iter().map(|i| i.end_mm - i.start_mm).sum();
    assert!((length - 60.0).abs() < 1e-8);
    let blocked: f64 = intervals
        .iter()
        .filter(|i| i.start.y == 0.0 && i.end.y == 0.0 && i.state == OutlineState::Blocked)
        .map(|i| i.end_mm - i.start_mm)
        .sum();
    assert!((blocked - 5.0).abs() < 0.0001);
    // An entirely interior component is also relevant to the landing depth.
    let inside = rectangle(8.0, 0.1, 11.0, 0.4);
    assert_eq!(
        bottom_state(&classify(&board, &inside, footprint()), 9.0),
        OutlineState::Blocked
    );
}

#[test]
fn clipping_does_not_discard_a_new_narrow_collision_by_significance() {
    let board = rectangle(0.0, 0.0, 20.0, 10.0);
    let mut obstacle = rectangle(8.0, 0.4999, 11.0, 5.0);
    obstacle.resolution = obstacle.resolution.with_tolerance(1.0);
    // The obstacle survives its source significance, but the thin strip
    // intersection must not be filtered again and silently called clear.
    assert_eq!(
        bottom_state(&classify(&board, &obstacle, footprint()), 9.0),
        OutlineState::Blocked
    );
}

#[test]
fn curved_source_retains_preparation_uncertainty() {
    let board = rectangle(0.0, 0.0, 20.0, 10.0);
    let circle = crate::geom::shapes::circle(2.0)
        .unwrap()
        .transformed(Affine2::translation(Point::new(10.0, -1.0)));
    let obstacle = ContourSet::from_filled_contours(&[circle], resolution()).unwrap();
    assert!(obstacle.uncertainty_mm > 0.0);
    let result = classify(&board, &obstacle, footprint());
    assert_eq!(bottom_state(&result, 7.5), OutlineState::Eligible);
    assert_eq!(bottom_state(&result, 8.0), OutlineState::Unknown);
    assert_eq!(bottom_state(&result, 8.5), OutlineState::Blocked);
    assert!(
        result
            .iter()
            .all(|i| i.uncertainty_mm >= obstacle.uncertainty_mm)
    );
}

#[test]
fn hole_and_concavity_do_not_become_a_bounding_box() {
    let board = rectangle(0.0, 0.0, 20.0, 10.0);
    let obstacle = rectangle(3.0, -5.0, 17.0, 5.0)
        .difference(&rectangle(6.0, -3.0, 14.0, 3.0))
        .unwrap();
    let intervals = classify(&board, &obstacle, footprint());
    assert_eq!(bottom_state(&intervals, 4.0), OutlineState::Blocked);
    assert_eq!(bottom_state(&intervals, 10.0), OutlineState::Eligible);
    assert_eq!(bottom_state(&intervals, 16.0), OutlineState::Blocked);
    let concave = obstacle
        .difference(&rectangle(0.0, 2.0, 20.0, 6.0))
        .unwrap();
    assert_eq!(
        bottom_state(&classify(&board, &concave, footprint()), 10.0),
        OutlineState::Eligible
    );
}

#[test]
fn missing_empty_invalid_and_complete_blockage_are_distinct() {
    let board = rectangle(0.0, 0.0, 20.0, 10.0);
    let empty = ContourSet::empty(resolution());
    for region in [None, Some(&empty)] {
        let result = eligible_outline(
            &board,
            &[OutlineObstacle {
                id: "missing",
                region,
            }],
            footprint(),
            tolerance(),
        )
        .unwrap();
        assert!(
            result
                .iter()
                .all(|i| i.state == OutlineState::Unknown && i.obstacles == [0])
        );
    }
    assert!(eligible_outline(&empty, &[], footprint(), tolerance()).is_err());
    assert!(
        eligible_outline(
            &board,
            &[],
            OutlineFootprint {
                width_mm: f64::NAN,
                ..footprint()
            },
            tolerance()
        )
        .is_err()
    );
    let cover = rectangle(-5.0, -5.0, 25.0, 15.0);
    let result = eligible_outline(
        &board,
        &[
            OutlineObstacle {
                id: "missing",
                region: None,
            },
            OutlineObstacle {
                id: "cover",
                region: Some(&cover),
            },
        ],
        footprint(),
        tolerance(),
    )
    .unwrap();
    assert!(
        result
            .iter()
            .all(|i| i.state == OutlineState::Blocked && i.obstacles == [0, 1])
    );
}

#[test]
fn larger_footprints_and_uncertainty_cannot_create_eligible_space() {
    let board = rectangle(0.0, 0.0, 20.0, 10.0);
    let obstacle = rectangle(8.0, -1.8, 11.0, -0.2);
    let small = classify(&board, &obstacle, footprint());
    let large = classify(
        &board,
        &obstacle,
        OutlineFootprint {
            width_mm: 4.0,
            inward_mm: 1.0,
            outward_mm: 3.0,
        },
    );
    let uncertain = eligible_outline(
        &board,
        &[OutlineObstacle {
            id: "part",
            region: Some(&obstacle),
        }],
        footprint(),
        QueryTolerance {
            boundary_mm: 0.1,
            ..tolerance()
        },
    )
    .unwrap();
    for x in [6.5, 7.5, 8.5, 10.5, 11.5, 12.5] {
        if bottom_state(&small, x) != OutlineState::Eligible {
            assert_ne!(bottom_state(&large, x), OutlineState::Eligible);
            assert_ne!(bottom_state(&uncertain, x), OutlineState::Eligible);
        }
    }
    assert_eq!(bottom_state(&uncertain, 7.1), OutlineState::Unknown);
}

#[test]
fn tangency_is_unknown_and_corners_and_hole_rings_remain_identifiable() {
    let board = rectangle(0.0, 0.0, 20.0, 10.0)
        .difference(&rectangle(6.0, 3.0, 14.0, 7.0))
        .unwrap();
    let obstacle = rectangle(8.0, -3.0, 11.0, -2.0);
    let intervals = classify(&board, &obstacle, footprint());
    assert_eq!(bottom_state(&intervals, 9.0), OutlineState::Unknown);
    assert!(intervals.iter().any(|i| i.boundary.ring == 1));
    let query = BoundaryQuery::new(&board, tolerance()).unwrap();
    for interval in &intervals {
        assert_eq!(interval.boundary.component, 0);
        assert!(interval.end_mm <= query.perimeter(interval.boundary).unwrap());
    }
}

#[test]
fn rigid_transforms_preserve_eligible_lengths_and_full_footprint_checks() {
    let board = rectangle(0.0, 0.0, 20.0, 10.0);
    let obstacle = rectangle(8.0, -1.8, 11.0, -0.2);
    let original = classify(&board, &obstacle, footprint());
    let sum = |intervals: &[OutlineInterval], state| {
        intervals
            .iter()
            .filter(|i| i.state == state)
            .map(|i| i.end_mm - i.start_mm)
            .sum::<f64>()
    };
    for mirror in [Mirror::NONE, Mirror::across_y(true)] {
        let transform = Affine2::placement(Point::new(100.0, -30.0), 37.0, mirror, 1.0);
        let moved = classify(
            &transform_region(&board, transform).unwrap(),
            &transform_region(&obstacle, transform).unwrap(),
            footprint(),
        );
        for state in [
            OutlineState::Eligible,
            OutlineState::Blocked,
            OutlineState::Unknown,
        ] {
            assert!((sum(&moved, state) - sum(&original, state)).abs() < 1e-5);
        }
    }
    // Independent filled-region query on interior stations of each output.
    for interval in original.iter().filter(|i| i.state != OutlineState::Unknown) {
        let site = BoundaryQuery::new(&board, tolerance())
            .unwrap()
            .site(
                interval.boundary,
                (interval.start_mm + interval.end_mm) / 2.0,
            )
            .unwrap();
        let t = site.tangent * (footprint().width_mm / 2.0);
        let n = site.outward_normal;
        let corners = [
            site.point - t - n * footprint().inward_mm,
            site.point + t - n * footprint().inward_mm,
            site.point + t + n * footprint().outward_mm,
            site.point - t + n * footprint().outward_mm,
        ];
        let tab = ContourSet::from_rings(
            vec![corners.map(|p| [p.x, p.y]).to_vec()],
            FillRule::EvenOdd,
            resolution(),
        )
        .unwrap();
        let overlap = tab.intersection(&obstacle).unwrap();
        assert_eq!(overlap.is_empty(), interval.state == OutlineState::Eligible);
    }
}
