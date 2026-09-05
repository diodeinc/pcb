//! Public prepared-region API, including the headless placed-path consumer.

use pcb_ir::geom::GeometryAccuracy;
use pcb_ir::geom::dist::{self, Distance};
use pcb_ir::geom::region::{ring_edges, rings_to_contours};
use pcb_ir::geom::{
    Affine2, BBox, ContourSet, FillRule, Mirror, Paint, PathArena, Point, PreparedRegion, Ring,
    shapes, tol,
};

fn rectangle(x0: f64, y0: f64, x1: f64, y1: f64) -> Ring {
    vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]]
}

fn near(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-7,
        "expected {expected}, got {actual}"
    );
}

fn query(prepared: &PreparedRegion, point: Point, expected: f64) -> Distance {
    let distance = prepared.signed_distance(point).unwrap();
    near(distance.mm, expected);
    assert_eq!(distance.first, point);
    near(distance.first.distance_to(distance.second), expected.abs());
    distance
}

#[test]
fn boundary_and_near_boundary_points_keep_their_signed_distance() {
    let region =
        ContourSet::rectangle(BBox::new(Point::ZERO, Point::new(4.0, 4.0)), tol::REGION_MM);
    let prepared = region.prepare_query();
    for point in [
        Point::ZERO,
        Point::new(4.0, 4.0),
        Point::new(0.0, 2.0),
        Point::new(4.0, 2.0),
        Point::new(2.0, 0.0),
        Point::new(2.0, 4.0),
    ] {
        let distance = query(&prepared, point, 0.0);
        assert_eq!(distance.second, point);
    }
    // Much closer than the region's containment/significance tolerance.
    for x in [-5e-7, -1e-10, 1e-10, 5e-7] {
        let distance = query(&prepared, Point::new(x, 2.0), -x);
        assert_eq!(distance.mm.is_sign_negative(), x > 0.0);
    }
    query(&prepared, Point::new(2.0, 2.0), -2.0);
    query(&prepared, Point::new(1e6, 2.0), 1e6 - 4.0);
}

#[test]
fn concave_corners_return_a_euclidean_boundary_witness() {
    let region = ContourSet::new(
        vec![vec![
            [0.0, 0.0],
            [6.0, 0.0],
            [6.0, 2.0],
            [2.0, 2.0],
            [2.0, 6.0],
            [0.0, 6.0],
        ]],
        FillRule::NonZero,
        tol::REGION_MM,
    );
    let prepared = region.prepare_query();
    for (point, expected, witness) in [
        (Point::new(3.0, 3.5), 1.0, Point::new(2.0, 3.5)),
        (Point::new(1.7, 1.6), -0.5, Point::new(2.0, 2.0)),
        (Point::new(1.7, 3.0), -0.3, Point::new(2.0, 3.0)),
        (Point::new(-1.0, -1.0), 2.0_f64.sqrt(), Point::ZERO),
    ] {
        let distance = query(&prepared, point, expected);
        near(distance.second.distance_to(witness), 0.0);
    }
}

fn nested_rings() -> Vec<Ring> {
    vec![
        rectangle(0.0, 0.0, 10.0, 10.0),
        rectangle(2.0, 2.0, 8.0, 8.0),
        rectangle(4.0, 4.0, 6.0, 6.0),
        rectangle(20.0, 0.0, 22.0, 2.0),
    ]
}

#[test]
fn holes_nested_islands_and_separate_islands_follow_the_fill_rule() {
    let region = ContourSet::new(nested_rings(), FillRule::EvenOdd, tol::REGION_MM);
    let prepared = region.prepare_query();
    for (point, expected) in [
        (Point::new(1.0, 5.0), -1.0),
        (Point::new(3.0, 5.0), 1.0),
        (Point::new(5.0, 5.0), -1.0),
        (Point::new(21.0, 1.0), -1.0),
        (Point::new(12.0, 1.0), 2.0),
        (Point::new(2.0, 5.0), 0.0),
    ] {
        query(&prepared, point, expected);
    }
    let filled = ContourSet::new(nested_rings(), FillRule::NonZero, tol::REGION_MM);
    query(&filled.prepare_query(), Point::new(3.0, 5.0), -3.0);
}

#[test]
fn headless_placements_transform_holes_distances_and_witnesses() {
    let accuracy = GeometryAccuracy::default();

    let mut arena = PathArena::default();
    let id = arena.push_path(
        Paint::Fill {
            rule: FillRule::EvenOdd,
        },
        rings_to_contours(nested_rings()),
    );
    for mirror in [Mirror::NONE, Mirror::X] {
        for rotation in [0.0, 37.0, 90.0] {
            let scale = 2.5;
            let transform = Affine2::placement(Point::new(17.0, -11.0), rotation, mirror, scale);
            let region = ContourSet::from_placed_painted_paths(
                &arena,
                [(arena.path(id), transform)],
                tol::REGION_MM,
                accuracy,
            )
            .unwrap();
            let prepared = region.prepare_query();
            for (point, expected, witness) in [
                (Point::new(3.0, 2.5), 0.5, Point::new(3.0, 2.0)),
                (Point::new(0.5, 5.0), -0.5, Point::new(0.0, 5.0)),
                (Point::new(21.5, 1.0), -0.5, Point::new(22.0, 1.0)),
            ] {
                let distance = query(
                    &prepared,
                    transform.transform_point(point),
                    expected * scale,
                );
                near(
                    distance
                        .second
                        .distance_to(transform.transform_point(witness)),
                    0.0,
                );
            }
        }
    }
}

#[test]
fn curve_distance_bands_cover_the_source_circle() {
    let accuracy = GeometryAccuracy::default();

    let radius = 2.0;
    let region = ContourSet::from_contours(
        &[shapes::circle(2.0 * radius).unwrap()],
        FillRule::NonZero,
        tol::REGION_MM,
        accuracy,
    )
    .unwrap();
    let prepared = region.prepare_query();
    for index in 0..128 {
        let angle = (index as f64 + 0.37) * std::f64::consts::TAU / 128.0;
        for offset in [-0.5, -0.001, 0.0, 0.001, 0.5] {
            let point = Point::new(angle.cos(), angle.sin()) * (radius + offset);
            let distance = prepared.signed_distance(point).unwrap();
            assert!(distance.uncertainty_mm.is_finite());
            assert!(
                (distance.mm - offset).abs() <= distance.uncertainty_mm,
                "angle={angle}, offset={offset}, distance={distance:?}"
            );
            assert!((distance.second.length() - radius).abs() <= distance.uncertainty_mm);
        }
    }
}

#[test]
fn batches_match_exhaustive_measurements_and_repeated_queries() {
    let mut rings = nested_rings();
    for index in 0..64 {
        let x = 30.0 + (index % 8) as f64 * 4.0;
        let y = (index / 8) as f64 * 4.0;
        rings.push(vec![[x, y], [x + 2.0, y], [x + 1.0, y + 3.0]]);
    }
    let region = ContourSet::new(rings, FillRule::EvenOdd, tol::REGION_MM);
    let prepared = region.prepare_query();
    let points = (0..51)
        .flat_map(|x| {
            (0..31).map(move |y| Point::new(x as f64 * 1.37 - 4.0, y as f64 * 1.29 - 4.0))
        })
        .collect::<Vec<_>>();
    let inside = region.contains_points_batch(&points);
    let distances = prepared.signed_distances(&points);
    for ((&point, distance), inside) in points.iter().zip(&distances).zip(inside) {
        assert_eq!(*distance, prepared.signed_distance(point));
        let distance = distance.unwrap();
        let expected = region
            .rings
            .iter()
            .flat_map(ring_edges)
            .map(|(start, end)| dist::point_segment(point, start, end).0)
            .fold(f64::INFINITY, f64::min);
        near(distance.mm, if inside { -expected } else { expected });
        near(distance.first.distance_to(distance.second), expected);
        let witness_distance = region
            .rings
            .iter()
            .flat_map(ring_edges)
            .map(|(start, end)| dist::point_segment(distance.second, start, end).0)
            .fold(f64::INFINITY, f64::min);
        near(witness_distance, 0.0);
        let end = point + Point::new(13.1, -9.7);
        let segment_distance = region
            .rings
            .iter()
            .flat_map(ring_edges)
            .map(|(a, b)| dist::segments(point, end, a, b).0)
            .fold(f64::INFINITY, f64::min);
        for limit in [0.0, 0.3, 2.0] {
            let nearest = prepared.nearest_within(point, limit);
            assert_eq!(nearest.is_some(), expected <= limit + tol::EPSILON_MM);
            if let Some(nearest) = nearest {
                near(nearest.mm, expected);
            }
            let nearest = prepared.segment_nearest_within(point, end, limit);
            assert_eq!(
                nearest.is_some(),
                segment_distance <= limit + tol::EPSILON_MM
            );
            if let Some(nearest) = nearest {
                near(nearest.mm, segment_distance);
                near(nearest.first.distance_to(nearest.second), segment_distance);
            }
        }
        let bounds = BBox::spanning(point, end);
        assert_eq!(
            prepared.segments_meeting(bounds).collect::<Vec<_>>(),
            region
                .rings
                .iter()
                .flat_map(ring_edges)
                .filter(|&(a, b)| BBox::spanning(a, b).intersects(bounds))
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn empty_degenerate_and_invalid_queries_have_no_witness() {
    for region in [
        ContourSet::empty(0.0),
        ContourSet::from_regularized(
            vec![
                vec![],
                vec![[1.0, 1.0]],
                vec![[0.0, 0.0], [1.0, 1.0], [2.0, 2.0]],
            ],
            0.0,
            0.0,
        ),
    ] {
        let prepared = region.prepare_query();
        assert_eq!(prepared.signed_distance(Point::ZERO), None);
    }
    let prepared = ContourSet::new(
        vec![rectangle(0.0, 0.0, 2.0, 2.0)],
        FillRule::NonZero,
        tol::REGION_MM,
    )
    .prepare_query();
    let points = [
        Point::new(f64::NAN, 1.0),
        Point::new(1.0, f64::INFINITY),
        Point::new(f64::NEG_INFINITY, 1.0),
        Point::new(1.0, 1.0),
    ];
    let distances = prepared.signed_distances(&points);
    assert_eq!(&distances[..3], &[None, None, None]);
    near(distances[3].unwrap().mm, -1.0);
}

#[test]
fn short_nonzero_edges_remain_segments() {
    let prepared = ContourSet::from_regularized(vec![rectangle(0.0, 0.0, 1e-8, 1e-8)], 0.0, 0.0)
        .prepare_query();
    let distance = prepared.signed_distance(Point::new(1e-9, 5e-9)).unwrap();
    assert!((distance.mm + 1e-9).abs() < 1e-20, "{distance:?}");
    assert!(distance.second.distance_to(Point::new(0.0, 5e-9)) < 1e-20);
}

#[test]
fn far_translated_rings_keep_area_holes_and_distances() {
    for origin in [1e9, -1e9] {
        let outer = rectangle(origin, origin, origin + 4.0, origin + 4.0);
        let mut hole = rectangle(origin + 1.0, origin + 1.0, origin + 3.0, origin + 3.0);
        hole.reverse();
        assert_eq!(pcb_ir::geom::region::ring_signed_area(&outer), 16.0);
        assert_eq!(pcb_ir::geom::region::ring_signed_area(&hole), -4.0);
        for tolerance in [0.0, tol::REGION_MM] {
            let region =
                ContourSet::from_regularized(vec![outer.clone(), hole.clone()], tolerance, 0.0);
            assert_eq!(region.rings.len(), 2);
            let prepared = region.prepare_query();
            for (offset, expected) in [(0.5, -0.5), (2.0, 1.0), (5.0, 1.0)] {
                query(
                    &prepared,
                    Point::new(origin + offset, origin + 2.0),
                    expected,
                );
            }
        }
    }
}
