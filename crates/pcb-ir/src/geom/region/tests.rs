use super::*;
use crate::geom::{Affine2, ContourBuf, Paint, PathArena, PathCmd, shapes};

pub(super) fn res(tolerance_mm: f64) -> Resolution {
    Resolution::default().with_tolerance(tolerance_mm)
}

#[test]
fn width_requires_a_disk_that_survives_boundary_uncertainty() {
    let mut region = ContourSet::rectangle(rect(0.0, 0.0, 1.0, 0.003), res(1e-6));
    assert!(
        !crate::geom::dfm::thin_features(&region, 0.127)
            .unwrap()
            .is_empty()
    );
    region.uncertainty_mm = 0.002;
    assert!(
        crate::geom::dfm::thin_features(&region, 0.127)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn erosion_of_a_convex_polygon_does_not_spend_round_join_error() {
    let accuracy = GeometryAccuracy::new(1e-6).unwrap();
    let region = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), Resolution::new(0.0, accuracy));
    let inset = region.disk_erode(1.0).unwrap();
    assert!((inset.area() - 64.0).abs() < 1e-8);
    assert!(inset.uncertainty_mm < 1e-9);
    assert!(region.disk_dilate(1.0).is_err());
}

#[test]
fn fixed_grid_regularization_removes_sub_grid_geometry() {
    let shapes = simplify_shapes_on_grid(
        vec![
            vec![
                [0.0004, 0.0004],
                [1.0004, 0.0004],
                [1.00049, 0.00049],
                [1.0004, 1.0004],
                [0.0004, 1.0004],
            ],
            vec![
                [2.0001, 0.0],
                [2.0004, 0.0],
                [2.0004, 0.0003],
                [2.0001, 0.0003],
            ],
        ],
        FillRule::NonZero,
        0.001,
    );

    assert_eq!(shapes.len(), 1);
    assert!((rings_area(&shapes[0]) - 1.0).abs() < 1e-9);
    for ring in &shapes[0] {
        for point in ring {
            assert!((point[0] * 1000.0 - (point[0] * 1000.0).round()).abs() < 1e-9);
            assert!((point[1] * 1000.0 - (point[1] * 1000.0).round()).abs() < 1e-9);
        }
        for (start, end) in ring
            .iter()
            .zip(ring.iter().cycle().skip(1))
            .take(ring.len())
        {
            assert!((start[0] - end[0]).hypot(start[1] - end[1]) >= 0.001);
        }
    }
}

/// Partial cells are the whole point: a sampled estimate would round each
/// of these to nothing or to everything.
#[test]
fn grid_coverage_measures_partly_covered_cells_exactly() {
    let square = ContourSet::rectangle(rect(0.5, 0.5, 2.5, 2.5), res(tol::REGION_MM));

    let coverage = square.grid_coverage(rect(0.0, 0.0, 3.0, 3.0), 3, 3);

    #[rustfmt::skip]
        let expected = [
            0.25, 0.5, 0.25,
            0.5,  1.0, 0.5,
            0.25, 0.5, 0.25,
        ];
    for (measured, expected) in coverage.iter().zip(expected) {
        assert!(
            (measured - expected).abs() < 1e-12,
            "{measured} != {expected}"
        );
    }
}

/// Holes are separate rings wound against their outer, and the cell they
/// fall in has to see that sign.
#[test]
fn grid_coverage_subtracts_holes() {
    let ring = ContourSet::rectangle(rect(0.0, 0.0, 4.0, 4.0), res(tol::REGION_MM))
        .difference(&ContourSet::rectangle(
            rect(1.0, 1.0, 2.0, 3.0),
            res(tol::REGION_MM),
        ))
        .unwrap();

    let coverage = ring.grid_coverage(rect(0.0, 0.0, 4.0, 4.0), 1, 1);

    assert!((coverage[0] - 14.0 / 16.0).abs() < 1e-12, "{coverage:?}");
}

#[test]
fn contour_set_composes_region_operations() {
    let outer = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), res(tol::REGION_MM));
    let inner = ContourSet::rectangle(rect(3.0, 3.0, 7.0, 7.0), res(tol::REGION_MM));
    let clip = ContourSet::rectangle(rect(5.0, 0.0, 10.0, 10.0), res(tol::REGION_MM));

    let ring = outer.difference(&inner).unwrap();
    let clipped = ring.intersection(&clip).unwrap();
    let expanded = clipped.disk_dilate(0.5).unwrap();

    assert!(!expanded.is_empty());
    assert!((expanded.bbox.min.x - 4.5).abs() <= 1e-9);
    assert!((expanded.bbox.max.x - 10.5).abs() <= 1e-9);
}

#[test]
fn filled_contour_region_is_winding_insensitive() {
    let clockwise = rectangle_contour(0.0, 0.0, 10.0, 5.0);
    let counter_clockwise = ContourBuf::new(vec![
        PathCmd::move_to(Point::new(0.0, 5.0)),
        PathCmd::line_to(Point::new(10.0, 5.0)),
        PathCmd::line_to(Point::new(10.0, 0.0)),
        PathCmd::line_to(Point::new(0.0, 0.0)),
        PathCmd::close(),
    ]);

    let a = ContourSet::from_filled_contours(std::slice::from_ref(&clockwise), res(tol::REGION_MM))
        .unwrap();
    let b = ContourSet::from_filled_contours(
        std::slice::from_ref(&counter_clockwise),
        res(tol::REGION_MM),
    )
    .unwrap();
    let unioned =
        ContourSet::from_filled_contours(&[clockwise, counter_clockwise], res(tol::REGION_MM))
            .unwrap();

    assert!(!a.is_empty());
    assert!((a.area() - b.area()).abs() <= 1e-9);
    assert!((unioned.area() - 50.0).abs() <= 1e-6);
}

#[test]
fn area_subtracts_holes() {
    let outer = ContourSet::rectangle(rect(0.0, 0.0, 4.0, 4.0), res(tol::REGION_MM));
    let inner = ContourSet::rectangle(rect(1.0, 1.0, 3.0, 3.0), res(tol::REGION_MM));

    let ring = outer.difference(&inner).unwrap();

    assert!((ring.area() - 12.0).abs() <= 1e-6);
}

#[test]
fn containment_observes_boundaries_and_holes() {
    let outer = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), res(tol::REGION_MM));
    let hole = ContourSet::rectangle(rect(4.0, 4.0, 6.0, 6.0), res(tol::REGION_MM));
    let region = outer.difference(&hole).unwrap();

    assert!(region.contains_point(Point::new(2.0, 2.0)));
    assert!(region.contains_point(Point::new(0.0, 5.0)));
    assert!(!region.contains_point(Point::new(5.0, 5.0)));
    assert!(region.contains_disk(Point::new(2.0, 2.0), 2.0));
    assert!(!region.contains_disk(Point::new(2.0, 2.0), 2.01));
    assert!(!region.contains_disk(Point::new(3.5, 5.0), 0.6));
}

type ExpectedSpan = ((f64, f64), (f64, f64));

fn assert_spans(actual: Vec<(Point, Point)>, expected: &[ExpectedSpan]) {
    assert_eq!(actual.len(), expected.len(), "{actual:?}");
    for ((start, end), &(from, to)) in actual.iter().zip(expected) {
        assert!(
            start.distance_to(Point::new(from.0, from.1)) <= 1e-8,
            "{actual:?}"
        );
        assert!(
            end.distance_to(Point::new(to.0, to.1)) <= 1e-8,
            "{actual:?}"
        );
    }
}

#[test]
fn segment_spans_preserve_holes_and_clip_to_the_query() {
    let outer = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), res(tol::REGION_MM));
    let hole = ContourSet::rectangle(rect(4.0, 2.0, 6.0, 8.0), res(tol::REGION_MM));
    let ring = outer.difference(&hole).unwrap();

    assert_spans(
        ring.segment_spans(Point::new(-2.0, 5.0), Point::new(12.0, 5.0)),
        &[((0.0, 5.0), (4.0, 5.0)), ((6.0, 5.0), (10.0, 5.0))],
    );
    assert_spans(
        ring.segment_spans(Point::new(2.0, 5.0), Point::new(9.0, 5.0)),
        &[((2.0, 5.0), (4.0, 5.0)), ((6.0, 5.0), (9.0, 5.0))],
    );
}

#[test]
fn segment_spans_preserve_disconnected_and_concave_regions() {
    let left = ContourSet::rectangle(rect(0.0, 0.0, 2.0, 2.0), res(tol::REGION_MM));
    let concave = ContourSet::from_rings(
        vec![vec![
            [4.0, 0.0],
            [8.0, 0.0],
            [8.0, 1.0],
            [5.0, 1.0],
            [5.0, 2.0],
            [4.0, 2.0],
        ]],
        FillRule::NonZero,
        res(tol::REGION_MM),
    )
    .unwrap();
    assert_spans(
        left.union(&concave)
            .unwrap()
            .segment_spans(Point::new(-1.0, 1.5), Point::new(9.0, 1.5)),
        &[((0.0, 1.5), (2.0, 1.5)), ((4.0, 1.5), (5.0, 1.5))],
    );
}

#[test]
fn segment_spans_follow_reversed_arbitrary_direction() {
    let square = ContourSet::rectangle(rect(0.0, 0.0, 4.0, 4.0), res(tol::REGION_MM));
    assert_spans(
        square.segment_spans(Point::new(6.0, 6.0), Point::new(-2.0, -2.0)),
        &[((4.0, 4.0), (0.0, 0.0))],
    );
}

#[test]
fn segment_spans_include_boundary_but_not_tangencies() {
    let square = ContourSet::rectangle(rect(0.0, 0.0, 4.0, 4.0), res(tol::REGION_MM));
    assert_spans(
        square.segment_spans(Point::new(-1.0, 0.0), Point::new(3.0, 0.0)),
        &[((0.0, 0.0), (3.0, 0.0))],
    );
    assert!(
        square
            .segment_spans(Point::new(-1.0, 1.0), Point::new(1.0, -1.0))
            .is_empty()
    );
}

#[test]
fn segment_spans_omit_degenerate_and_sub_tolerance_intervals() {
    let square = ContourSet::rectangle(rect(0.0, 0.0, 4.0, 4.0), res(tol::EPSILON_MM));
    assert!(
        square
            .segment_spans(Point::new(1.0, 1.0), Point::new(1.0, 1.0))
            .is_empty()
    );
    assert!(
        square
            .segment_spans(Point::new(-1e-10, 2.0), Point::new(0.0, 2.0))
            .is_empty()
    );
    assert_spans(
        square.segment_spans(Point::new(-1e-5, 2.0), Point::new(1e-5, 2.0)),
        &[((0.0, 2.0), (1e-5, 2.0))],
    );
}

#[test]
fn bridged_contour_preserves_local_holes_without_clear_polarity() {
    let outer = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), res(tol::REGION_MM));
    let circle = shapes::circle(2.0).unwrap();
    let circle = circle.transformed(crate::geom::Affine2::translation(Point::new(5.0, 5.0)));
    let hole = ContourSet::from_filled_contours(&[circle], res(tol::REGION_MM)).unwrap();
    let region = outer.difference(&hole).unwrap();

    let contours = region.to_bridged_contours();
    let round_trip =
        ContourSet::from_contours(&contours, FillRule::NonZero, res(tol::REGION_MM)).unwrap();

    assert_eq!(contours.len(), 1);
    assert!(
        (round_trip.area() - region.area()).abs() <= 0.01,
        "bridged area {}, source area {}",
        round_trip.area(),
        region.area()
    );
    assert!(!round_trip.contains_point(Point::new(5.0, 5.0)));
}

#[test]
fn erodes_outer_boundaries_and_expands_holes() {
    let outer = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), res(tol::REGION_MM));
    let hole = ContourSet::rectangle(rect(4.0, 4.0, 6.0, 6.0), res(tol::REGION_MM));

    let eroded = outer.difference(&hole).unwrap().disk_erode(0.5).unwrap();

    assert!((eroded.bbox.min.x - 0.5).abs() <= 1e-9);
    assert!((eroded.bbox.min.y - 0.5).abs() <= 1e-9);
    assert!((eroded.bbox.max.x - 9.5).abs() <= 1e-9);
    assert!((eroded.bbox.max.y - 9.5).abs() <= 1e-9);
    let area = eroded.area();
    assert!(
        (area - 72.214601837).abs() <= 2e-2,
        "unexpected eroded area {area}"
    );
}

#[test]
fn erosion_can_remove_an_entire_region() {
    let region = ContourSet::rectangle(rect(0.0, 0.0, 0.5, 0.5), res(tol::REGION_MM));

    let eroded = region.disk_erode(0.5).unwrap();

    assert!(eroded.is_empty());
}

#[test]
fn disk_opening_rounds_corners_and_stays_inside_source() {
    let region = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), res(tol::REGION_MM));

    let opened = region.disk_open(0.5).unwrap();

    assert!(opened.difference(&region).unwrap().is_empty());
    assert!((opened.bbox.min.x - 0.0).abs() <= 1e-9);
    assert!((opened.bbox.min.y - 0.0).abs() <= 1e-9);
    assert!((opened.bbox.max.x - 10.0).abs() <= 1e-9);
    assert!((opened.bbox.max.y - 10.0).abs() <= 1e-9);
    assert!((opened.area() - (99.0 + std::f64::consts::PI / 4.0)).abs() <= 2e-2);
}

#[test]
fn disk_opening_removes_sub_diameter_slivers_and_small_islands() {
    let body = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), res(tol::REGION_MM));
    let sliver = ContourSet::rectangle(rect(12.0, 0.0, 20.0, 0.8), res(tol::REGION_MM));
    let island = ContourSet::rectangle(rect(22.0, 0.0, 22.8, 0.8), res(tol::REGION_MM));
    let region = body.union(&sliver).unwrap().union(&island).unwrap();

    let opened = region.disk_open(0.5).unwrap();

    assert_eq!(opened.connected_components().len(), 1);
    assert!(opened.intersection(&body).unwrap().area() > 99.0);
    assert!(opened.intersection(&sliver).unwrap().is_empty());
    assert!(opened.intersection(&island).unwrap().is_empty());
}

#[test]
fn disk_opening_is_idempotent_within_offset_tolerance() {
    let outer = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), res(tol::REGION_MM));
    let notch = ContourSet::rectangle(rect(4.0, 8.0, 6.0, 10.0), res(tol::REGION_MM));
    let region = outer.difference(&notch).unwrap();

    let once = region.disk_open(0.5).unwrap();
    let twice = once.disk_open(0.5).unwrap();
    let symmetric_difference = once
        .difference(&twice)
        .unwrap()
        .union(&twice.difference(&once).unwrap())
        .unwrap();

    assert!(
        symmetric_difference.area() <= 2e-2,
        "opening changed by {:.9} mm² on repetition",
        symmetric_difference.area()
    );
}

#[test]
fn disk_closing_fills_sub_diameter_gaps_and_stays_outside_source() {
    let left = ContourSet::rectangle(rect(0.0, 0.0, 4.0, 10.0), res(tol::REGION_MM));
    let right = ContourSet::rectangle(rect(4.8, 0.0, 10.0, 10.0), res(tol::REGION_MM));
    let region = left.union(&right).unwrap();

    let closed = region.disk_close(0.5).unwrap();
    let middle = ContourSet::rectangle(rect(4.0, 1.0, 4.8, 9.0), res(tol::REGION_MM));

    assert!(region.difference(&closed).unwrap().is_empty());
    assert!(closed.intersection(&middle).unwrap().area() > 6.3);
}

#[test]
fn disk_closing_preserves_wide_gaps() {
    let left = ContourSet::rectangle(rect(0.0, 0.0, 4.0, 10.0), res(tol::REGION_MM));
    let right = ContourSet::rectangle(rect(5.2, 0.0, 10.0, 10.0), res(tol::REGION_MM));
    let region = left.union(&right).unwrap();

    let closed = region.disk_close(0.5).unwrap();
    let middle = ContourSet::rectangle(rect(4.0, 1.0, 5.2, 9.0), res(tol::REGION_MM));

    assert!(closed.intersection(&middle).unwrap().is_empty());
}

#[test]
fn disk_gap_violations_report_close_distinct_components() {
    let left = ContourSet::rectangle(rect(0.0, 0.0, 4.0, 10.0), res(tol::REGION_MM));
    let close = ContourSet::rectangle(rect(4.8, 0.0, 10.0, 10.0), res(tol::REGION_MM));
    let wide = ContourSet::rectangle(rect(5.2, 0.0, 10.0, 10.0), res(tol::REGION_MM));

    let close_violations = left
        .union(&close)
        .unwrap()
        .disk_gap_violations(0.5)
        .unwrap();
    let wide_violations = left.union(&wide).unwrap().disk_gap_violations(0.5).unwrap();

    assert!(close_violations.area() > 1.5);
    assert!(wide_violations.is_empty());
    assert!(left.disk_gap_violations(0.5).unwrap().is_empty());
}

#[test]
fn disk_gap_regularization_sweeps_a_void_thinner_than_the_axis_stroke() {
    // A 3 µm gap is two-sided but too thin to carry a medial-axis stroke;
    // the whole-component sweep must still make progress instead of
    // stalling into an error.
    let left = ContourSet::rectangle(rect(0.0, 0.0, 5.0, 6.0), res(tol::REGION_MM));
    let right = ContourSet::rectangle(rect(5.003, 0.0, 10.0, 6.0), res(tol::REGION_MM));
    let region = left.union(&right).unwrap();
    assert!(!region.disk_gap_violations(0.5).unwrap().is_empty());

    let regularization = region.disk_regularize_gaps(0.5, 0.5, 0.025).unwrap();

    assert!(
        regularization
            .kept
            .disk_gap_violations(0.5)
            .unwrap()
            .is_empty()
    );
    assert!(regularization.removed.area() > 0.0);
}

#[test]
fn disk_gap_violations_exclude_isolated_void_corners() {
    let outer = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), res(tol::REGION_MM));
    let wide_hole = ContourSet::rectangle(rect(3.0, 3.0, 7.0, 7.0), res(tol::REGION_MM));
    let region = outer.difference(&wide_hole).unwrap();
    let raw_closing_residual = region.disk_close(0.5).unwrap().difference(&region).unwrap();

    assert!(raw_closing_residual.area() > 0.1);
    assert!(region.disk_gap_violations(0.5).unwrap().is_empty());
}

#[test]
fn disk_gap_regularization_rejects_invalid_scales() {
    let region = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), res(tol::REGION_MM));

    assert!(region.disk_regularize_gaps(0.0, 0.5, 0.025).is_err());
    assert!(region.disk_regularize_gaps(0.5, f64::NAN, 0.025).is_err());
    assert!(region.disk_regularize_gaps(0.5, 0.5, -0.025).is_err());
}

#[test]
fn disk_gap_regularization_widens_a_gap_thinner_than_the_guard() {
    let left = ContourSet::rectangle(rect(0.0, 0.0, 4.0, 10.0), res(tol::REGION_MM));
    let right = ContourSet::rectangle(rect(4.01, 0.0, 10.0, 10.0), res(tol::REGION_MM));
    let region = left.union(&right).unwrap();

    let before = region.disk_gap_violations(0.5).unwrap();
    let result = region.disk_regularize_gaps(0.5, 0.5, 0.025).unwrap();

    assert!(before.area() > 0.05);
    assert!(before.disk_open(0.025).unwrap().is_empty());
    assert!(result.removed.area() > 0.0);
    assert!(result.kept.disk_gap_violations(0.5).unwrap().is_empty());
}

#[test]
fn disk_gap_regularization_trims_a_close_pair_locally() {
    let left = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), res(tol::REGION_MM));
    let right = ContourSet::rectangle(rect(10.8, 0.0, 20.8, 10.0), res(tol::REGION_MM));
    let distant = ContourSet::rectangle(rect(24.0, 0.0, 30.0, 10.0), res(tol::REGION_MM));
    let region = left.union(&right).unwrap().union(&distant).unwrap();

    let result = region.disk_regularize_gaps(0.5, 0.5, 0.025).unwrap();

    assert_eq!(result.kept.connected_components().len(), 3);
    assert!(result.kept.difference(&region).unwrap().is_empty());
    assert!(
        result.removed.area() < 4.0,
        "removed {:.9} mm²",
        result.removed.area()
    );
    assert!(result.removed.intersection(&distant).unwrap().area() <= 0.25);
    assert!(result.kept.disk_gap_violations(0.5).unwrap().is_empty());
}

#[test]
fn disk_gap_regularization_is_symmetric_at_a_three_way_conflict() {
    let lower_left = ContourSet::rectangle(rect(0.0, 0.0, 4.0, 4.0), res(tol::REGION_MM));
    let lower_right = ContourSet::rectangle(rect(4.8, 0.0, 8.8, 4.0), res(tol::REGION_MM));
    let upper = ContourSet::rectangle(rect(2.4, 4.8, 6.4, 8.8), res(tol::REGION_MM));
    let region = lower_left
        .union(&lower_right)
        .unwrap()
        .union(&upper)
        .unwrap();

    let result = region.disk_regularize_gaps(0.5, 0.5, 0.025).unwrap();

    assert_eq!(result.kept.connected_components().len(), 3);
    for source in [&lower_left, &lower_right, &upper] {
        assert!(result.kept.intersection(source).unwrap().area() > 13.0);
    }
    let violations = result.kept.disk_gap_violations(0.5).unwrap();
    assert!(
        violations.is_empty(),
        "remaining void-gap violation area {:.9} mm²",
        violations.area(),
    );
}

#[test]
fn disk_gap_regularization_widens_a_same_component_hairpin() {
    let outer = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), res(tol::REGION_MM));
    let narrow_notch = ContourSet::rectangle(rect(4.6, 3.0, 5.4, 10.0), res(tol::REGION_MM));
    let hairpin = outer.difference(&narrow_notch).unwrap();

    let before = hairpin.disk_gap_violations(0.5).unwrap();
    let result = hairpin.disk_regularize_gaps(0.5, 0.5, 0.025).unwrap();
    let after = result.kept.disk_gap_violations(0.5).unwrap();

    assert_eq!(hairpin.connected_components().len(), 1);
    assert!(before.area() > 1.0);
    assert!(result.removed.area() > 1.0);
    assert!(
        after.is_empty(),
        "remaining hairpin gap {:.9} mm²",
        after.area()
    );
}

#[test]
fn disk_gap_regularization_widens_a_narrow_internal_void() {
    let outer = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), res(tol::REGION_MM));
    let narrow_hole = ContourSet::rectangle(rect(4.6, 3.0, 5.4, 7.0), res(tol::REGION_MM));
    let region = outer.difference(&narrow_hole).unwrap();

    let before = region.disk_gap_violations(0.5).unwrap();
    let result = region.disk_regularize_gaps(0.5, 0.5, 0.025).unwrap();
    let after = result.kept.disk_gap_violations(0.5).unwrap();

    assert!(before.area() > 3.0);
    assert!(result.removed.area() > 1.0);
    assert!(result.kept.contains_point(Point::new(2.0, 5.0)));
    assert!(
        after.is_empty(),
        "remaining internal-void gap {:.9} mm²",
        after.area(),
    );
}

#[test]
fn dilation_shrinks_but_preserves_a_large_hole() {
    let outer = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), res(tol::REGION_MM));
    let hole = ContourSet::rectangle(rect(3.0, 3.0, 7.0, 7.0), res(tol::REGION_MM));

    let dilated = outer.difference(&hole).unwrap().disk_dilate(0.5).unwrap();
    let expected_hole = ContourSet::rectangle(rect(3.5, 3.5, 6.5, 6.5), res(tol::REGION_MM));

    assert!(dilated.intersection(&expected_hole).unwrap().is_empty());
    let area = dilated.area();
    assert!(
        (area - 111.785398163).abs() <= 2e-2,
        "unexpected dilated area {area}"
    );
}

#[test]
fn union_contains_both_regions_when_a_hole_overlaps_filled_material() {
    let outer = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), res(tol::REGION_MM));
    let hole = ContourSet::rectangle(rect(3.0, 3.0, 7.0, 7.0), res(tol::REGION_MM));
    let frame = outer.difference(&hole).unwrap();
    let plug = ContourSet::rectangle(rect(4.0, 4.0, 6.0, 6.0), res(tol::REGION_MM));

    let union = frame.union(&plug).unwrap();

    assert!(frame.difference(&union).unwrap().is_empty());
    assert!(plug.difference(&union).unwrap().is_empty());
    assert!((union.area() - 88.0).abs() <= 1e-6);
}

/// Reduced chain of narrow complement pockets from the ControlHub A5
/// rounded-board/V-score corner. A positive offset must contain every
/// source point even when all of these holes collapse.
#[test]
fn dilation_is_monotone_for_a5_corner_hole_chain() {
    let outer = ContourSet::rectangle(
        rect(22.8473, 110.5175, 27.8973, 115.5675),
        res(tol::REGION_MM),
    );
    let holes = ContourSet::from_filled_contours(
        &[
            contour_from_vertices(&[
                [22.947299957275405, 111.0119310617447],
                [22.947299957275405, 111.0245145559311],
                [22.95425641536714, 111.06290817260744],
            ]),
            contour_from_vertices(&[
                [23.024561643600478, 111.42717397212984],
                [23.034708142280593, 111.47974538803102],
                [23.044173359870925, 111.51395440101625],
            ]),
            contour_from_vertices(&[
                [23.121961593627944, 111.79509580135347],
                [23.14569699764253, 111.88088023662569],
                [23.17426168918611, 111.95898854732515],
            ]),
            contour_from_vertices(&[
                [23.255928039550795, 112.18230032920839],
                [23.288409471511855, 112.27111899852754],
                [23.340039849281325, 112.38374710083009],
            ]),
            contour_from_vertices(&[
                [23.42500483989717, 112.56909239292146],
                [23.46326601505281, 112.65255665779115],
                [23.54809403419496, 112.80427730083467],
            ]),
            contour_from_vertices(&[
                [23.626791238784804, 112.94503259658815],
                [23.66619455814363, 113.01550805568696],
                [23.791095137596145, 113.20204389095308],
            ]),
            contour_from_vertices(&[
                [23.864429831504836, 113.31156766414644],
                [23.89930665493013, 113.36365520954134],
                [24.080685615539565, 113.59284710884096],
            ]),
            contour_from_vertices(&[
                [24.131775259971633, 113.65740430355073],
                [24.158974766731276, 113.69177389144899],
                [24.444096326828017, 113.99821996688844],
                [24.75268936157228, 114.28101646900178],
                [25.08276188373567, 114.53819620609285],
                [25.115122795104995, 114.55951189994813],
                [24.76722896099092, 114.29204106330873],
                [24.431027054786696, 113.98377299308778],
            ]),
            contour_from_vertices(&[
                [25.20675671100618, 114.61986958980562],
                [25.432661533355727, 114.76866948604585],
                [25.4813165664673, 114.795392036438],
            ]),
            contour_from_vertices(&[
                [25.624097824096694, 114.87381088733675],
                [25.797136902809157, 114.96884799003602],
                [25.857525467872634, 114.99598038196565],
            ]),
            contour_from_vertices(&[
                [26.056700825691237, 115.08546924591066],
                [26.179885625839248, 115.1408157348633],
                [26.24467504024507, 115.16395568847658],
            ]),
            contour_from_vertices(&[
                [26.486665248870864, 115.25038421154024],
                [26.5711922645569, 115.2805736064911],
                [26.634812474250808, 115.29765975475313],
            ]),
            contour_from_vertices(&[
                [26.93655109405519, 115.37869584560396],
                [26.973154783248916, 115.38852632045747],
                [27.011330246925368, 115.39559543132783],
            ]),
            contour_from_vertices(&[
                [27.390587329864516, 115.46582400798799],
                [27.396905422210708, 115.46750009059907],
                [27.402869582176223, 115.46750009059907],
            ]),
        ],
        res(tol::REGION_MM),
    )
    .unwrap();
    let source = outer.difference(&holes).unwrap();

    let dilated = source.disk_dilate(0.525).unwrap();
    let removed_source = source.difference(&dilated).unwrap();

    assert!(
        removed_source.is_empty(),
        "dilation removed {:.9} mm² from its source",
        removed_source.area()
    );
}

#[test]
fn painted_path_region_unions_fills_and_native_strokes() {
    let mut arena = PathArena::default();
    let filled = arena.push_path(
        Paint::Fill {
            rule: FillRule::EvenOdd,
        },
        [rectangle_contour(0.0, 0.0, 1.0, 1.0)],
    );
    let stroked = arena.push_path(
        Paint::Stroke(crate::geom::StrokeStyle::round(1.0)),
        [ContourBuf::new(vec![
            PathCmd::move_to(Point::new(2.0, 0.5)),
            PathCmd::line_to(Point::new(4.0, 0.5)),
        ])],
    );
    let unpainted = arena.push_path(Paint::None, [rectangle_contour(10.0, 10.0, 20.0, 20.0)]);

    let region = ContourSet::from_painted_paths(
        &arena,
        [filled, stroked, unpainted]
            .iter()
            .map(|&index| arena.path(index)),
        res(tol::REGION_MM),
    )
    .unwrap();

    assert!((region.bbox.min.x - 0.0).abs() <= 1e-9);
    assert!((region.bbox.min.y - 0.0).abs() <= 1e-9);
    assert!((region.bbox.max.x - 4.5).abs() <= 1e-9);
    assert!((region.bbox.max.y - 1.0).abs() <= 1e-9);
    assert!(region.area() > 3.5);
    assert!(region.area() < 4.0);
}

pub(super) fn rect(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> BBox {
    BBox::new(Point::new(min_x, min_y), Point::new(max_x, max_y))
}

fn rectangle_contour(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> ContourBuf {
    ContourBuf::new(vec![
        PathCmd::move_to(Point::new(min_x, min_y)),
        PathCmd::line_to(Point::new(max_x, min_y)),
        PathCmd::line_to(Point::new(max_x, max_y)),
        PathCmd::line_to(Point::new(min_x, max_y)),
        PathCmd::close(),
    ])
}

fn contour_from_vertices(vertices: &[[f64; 2]]) -> ContourBuf {
    let mut cmds = Vec::with_capacity(vertices.len() + 1);
    for (index, &[x, y]) in vertices.iter().enumerate() {
        let point = Point::new(x, y);
        cmds.push(if index == 0 {
            PathCmd::move_to(point)
        } else {
            PathCmd::line_to(point)
        });
    }
    cmds.push(PathCmd::close());
    ContourBuf::new(cmds)
}

/// Regression: V-score relief tool-center region from a real board whose
/// boolean output carried sub-micrometer float-debris segments. Dilation
/// must handle it without panicking or losing the region.
#[test]
fn dilates_boolean_debris_with_submicron_segments() {
    let contour = contour_from_vertices(&[
        [38.0, 160.0],
        [38.0, 156.894598],
        [38.0171578, 156.764663],
        [38.0503974, 156.684556],
        [38.0504384, 156.684832],
        [38.1270673, 157.070071],
        [38.1389899, 157.117669],
        [38.2530098, 157.493541],
        [38.2695398, 157.539739],
        [38.419852, 157.902626],
        [38.4408314, 157.946984],
        [38.6259894, 158.29339],
        [38.6512156, 158.335477],
        [38.8694365, 158.662066],
        [38.8986657, 158.701478],
        [39.1478457, 159.005105],
        [39.1807983, 159.041462],
        [39.4585402, 159.319203],
        [39.4948957, 159.352154],
        [39.7985227, 159.601335],
        [39.8379354, 159.630565],
        [40.1645255, 159.848785],
        [40.2066116, 159.874011],
        [40.5530176, 160.059169],
        [40.5973749, 160.080148],
        [40.9602618, 160.23046],
        [41.0064602, 160.24699],
        [41.3823323, 160.36101],
        [41.4299297, 160.372933],
        [41.8151686, 160.449562],
        [41.8154452, 160.449603],
        [41.735338, 160.482842],
        [41.6054032, 160.5],
        [38.5, 160.5],
        [38.3675704, 160.482272],
        [38.2503393, 160.433321],
        [38.1464467, 160.353553],
        [38.0666795, 160.249661],
        [38.0177281, 160.13243],
    ]);
    let region = ContourSet::from_filled_contours(&[contour], res(tol::REGION_MM)).unwrap();

    let grown = region.disk_dilate(0.5).unwrap();

    assert!(grown.area() > region.area());
}

/// Regression: minimal boundary fragment from a real board that crashed
/// an arc-preserving offset library's slice stitching when grown by the
/// route-tool radius.
#[test]
fn dilates_relief_boundary_fragment() {
    let contour = contour_from_vertices(&[
        [31.901232957840, 63.057707951027],
        [31.859204053879, 63.115636036354],
        [31.806460976601, 63.248603985268],
        [31.793315052986, 63.391045973259],
        [32.526947975159, 63.811510965782],
        [32.643206000328, 63.728166029411],
        [32.689244031906, 63.673370048958],
        [33.861821055412, 62.191123053986],
    ]);
    let region = ContourSet::from_filled_contours(&[contour], res(tol::REGION_MM)).unwrap();

    let grown = region.disk_dilate(0.5).unwrap();

    assert!(grown.area() > region.area());
}
#[test]
fn mismatched_arc_radii_fail_the_budget_instead_of_flattening_without_bound() {
    // Start radius 1, end radius 1.5: the source arc is inconsistent by
    // far more than the budget allows.
    let contour = ContourBuf::new(vec![
        PathCmd::move_to(Point::new(1.0, 0.0)),
        PathCmd::arc_to(Point::new(0.0, 1.5), Point::ZERO, false),
        PathCmd::close(),
    ]);
    let error = ContourSet::from_contours(&[contour], FillRule::NonZero, res(0.0)).unwrap_err();
    assert!(matches!(error, AccuracyError::BudgetExceeded { .. }));
}

#[test]
fn cancelled_overlap_edges_leave_one_exact_rectangle() {
    let left = ContourSet::rectangle(rect(0.0, 0.0, 2.0, 1.0), res(0.0));
    let right = ContourSet::rectangle(rect(1.0, 0.0, 3.0, 1.0), res(0.0));
    let whole = ContourSet::rectangle(rect(0.0, 0.0, 3.0, 1.0), res(0.0));

    let union = left.union(&right).unwrap();

    assert_eq!(union.rings.len(), 1);
    assert!((union.area() - 3.0).abs() < 1e-9);
    assert!(union.difference(&whole).unwrap().is_empty());
    assert!(whole.difference(&union).unwrap().is_empty());
    assert!(union.contains_point(Point::new(1.5, 0.5)));
    let inside = union
        .prepare_query()
        .signed_distance(Point::new(1.5, 0.5))
        .unwrap();
    assert!((inside.mm + 0.5).abs() < 1e-9);
    assert!(union.uncertainty_mm < 1e-9);
}

#[test]
fn adjoining_rectangles_equal_one_rectangle() {
    let left = ContourSet::rectangle(rect(0.0, 0.0, 1.0, 1.0), res(0.0));
    let right = ContourSet::rectangle(rect(1.0, 0.0, 2.0, 1.0), res(0.0));
    let whole = ContourSet::rectangle(rect(0.0, 0.0, 2.0, 1.0), res(0.0));

    let union = left.union(&right).unwrap();

    assert_eq!(union.rings.len(), 1);
    assert!((union.area() - whole.area()).abs() < 1e-9);
    for x in [0.5, 1.0, 1.5] {
        let point = Point::new(x, 0.5);
        assert_eq!(union.contains_point(point), whole.contains_point(point));
        let seamed = union.prepare_query().signed_distance(point).unwrap().mm;
        let exact = whole.prepare_query().signed_distance(point).unwrap().mm;
        assert!((seamed - exact).abs() < 1e-9, "x={x}: {seamed} vs {exact}");
    }
}

#[test]
fn mixed_budget_booleans_fail_when_the_coarser_history_does_not_fit() {
    let coarse_resolution = Resolution::new(0.0, GeometryAccuracy::new(0.05).unwrap());
    let fine_resolution = Resolution::new(0.0, GeometryAccuracy::new(0.001).unwrap());
    let coarse = ContourSet::from_contours(
        &[shapes::circle(4.0).unwrap()],
        FillRule::NonZero,
        coarse_resolution,
    )
    .unwrap();
    let fine = ContourSet::rectangle(rect(0.0, 0.0, 1.0, 1.0), fine_resolution);
    assert!(coarse.uncertainty_mm > fine_resolution.accuracy.max_error_mm());

    assert!(matches!(
        coarse.union(&fine),
        Err(AccuracyError::BudgetExceeded { .. })
    ));
    assert!(matches!(
        fine.difference(&coarse),
        Err(AccuracyError::BudgetExceeded { .. })
    ));
    assert!(matches!(
        fine.intersection(&coarse),
        Err(AccuracyError::BudgetExceeded { .. })
    ));

    // The same operands fit the coarser budget, and the result keeps it
    // together with the coarse history.
    let loose = ContourSet::rectangle(rect(3.0, 0.0, 4.0, 1.0), coarse_resolution);
    let union = coarse.union(&loose).unwrap();
    assert_eq!(union.budget(), coarse_resolution.accuracy);
    assert!(union.uncertainty_mm >= coarse.uncertainty_mm);
    assert!(union.contains_point(Point::new(3.5, 0.5)));
    assert!((union.area() - coarse.area() - 1.0).abs() < 1e-6);
}

#[test]
fn polygon_preparation_checks_coordinate_rounding_against_the_budget() {
    let far = vec![[0.0, 0.0], [1000.0, 0.0], [1000.0, 1000.0], [0.0, 1000.0]];
    let resolution = Resolution::new(0.0, GeometryAccuracy::new(1e-15).unwrap());
    assert!(matches!(
        ContourSet::from_rings(vec![far.clone()], FillRule::NonZero, resolution),
        Err(AccuracyError::BudgetExceeded { .. })
    ));
    assert!(ContourSet::from_rings(vec![far], FillRule::NonZero, res(0.0)).is_ok());
}

#[test]
fn regularization_drops_rings_below_significance() {
    let square = vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
    let sliver = vec![[2.0, 0.0], [12.0, 0.0], [12.0, 5e-6], [2.0, 5e-6]];
    let rings = || vec![square.clone(), sliver.clone()];

    let significant = ContourSet::from_rings(rings(), FillRule::NonZero, res(0.01)).unwrap();
    assert_eq!(significant.rings.len(), 1);
    assert!((significant.area() - 1.0).abs() < 1e-9);

    let exact = ContourSet::from_rings(rings(), FillRule::NonZero, res(0.0)).unwrap();
    assert_eq!(exact.rings.len(), 2);
}

#[test]
fn preparation_scales_to_panel_sized_curve_sets() {
    // A flattened panel legitimately needs millions of chord vertices;
    // the subdivision guard must only reject absurd budgets.
    let circles = (0..125)
        .flat_map(|row| (0..120).map(move |column| (row, column)))
        .map(|(row, column)| {
            crate::geom::shapes::circle(1.0)
                .unwrap()
                .transformed(Affine2::translation(Point::new(
                    2.0 * column as f64,
                    2.0 * row as f64,
                )))
        })
        .collect::<Vec<_>>();

    let region =
        ContourSet::from_contours(&circles, FillRule::NonZero, Resolution::default()).unwrap();

    assert_eq!(region.rings.len(), circles.len());
}
