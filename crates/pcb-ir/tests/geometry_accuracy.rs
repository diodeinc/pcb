use pcb_ir::geom::dfm::{RegionBoundaryIndex, region_clearance};
use pcb_ir::geom::dist::point_segment;
use pcb_ir::geom::{
    Affine2, BBox, ContourBuf, ContourSet, FillRule, GeometryAccuracy, Paint, PathArena, Point,
    shapes,
};

fn accuracy(mm: f64) -> GeometryAccuracy {
    GeometryAccuracy::new(mm).unwrap()
}
fn prepare(contours: &[ContourBuf], mm: f64) -> ContourSet {
    ContourSet::from_contours_with_accuracy(contours, FillRule::EvenOdd, 1e-6, accuracy(mm))
        .unwrap()
}
fn distance(region: &ContourSet, p: Point) -> f64 {
    region
        .rings
        .iter()
        .flat_map(|ring| {
            ring.iter()
                .zip(ring.iter().cycle().skip(1))
                .take(ring.len())
        })
        .map(|(&a, &b)| point_segment(p, Point::new(a[0], a[1]), Point::new(b[0], b[1])).0)
        .fold(f64::INFINITY, f64::min)
}
fn radial_error(region: &ContourSet, rx: f64, ry: f64) -> f64 {
    (0..4096)
        .map(|i| {
            let angle = std::f64::consts::TAU * i as f64 / 4096.0;
            distance(region, Point::new(rx * angle.cos(), ry * angle.sin()))
        })
        .fold(0.0, f64::max)
}

#[test]
fn circles_converge_below_the_old_cubic_floor() {
    let circle = shapes::circle(2.0).unwrap();
    let mut previous = f64::INFINITY;
    for budget in [0.005, 0.001, 0.0001, 0.00001] {
        let region = prepare(std::slice::from_ref(&circle), budget);
        let error = radial_error(&region, 1.0, 1.0);
        assert!(
            error <= region.uncertainty_mm,
            "{error} > {}",
            region.uncertainty_mm
        );
        assert!(region.uncertainty_mm <= budget);
        assert!(error < previous);
        previous = error;
        let measured = RegionBoundaryIndex::new(&region, 2.0)
            .nearest_within(Point::ZERO, 2.0)
            .unwrap();
        assert_eq!(measured.uncertainty_mm, region.uncertainty_mm);
        assert!((measured.mm - 1.0).abs() <= measured.uncertainty_mm);
    }
}

#[test]
fn ellipses_reprepare_analytic_source_and_report_untracked_cubic_floors() {
    let ellipse = shapes::ellipse(0.4, 0.2).unwrap();
    let coarse = prepare(std::slice::from_ref(&ellipse), 0.005);
    let fine = prepare(std::slice::from_ref(&ellipse), 0.000001);
    assert!(radial_error(&fine, 0.2, 0.1) < radial_error(&coarse, 0.2, 0.1));
    assert!(radial_error(&fine, 0.2, 0.1) <= fine.uncertainty_mm);
    assert!(
        ContourSet::from_contours_with_accuracy(
            &[ellipse.clone().with_uncertainty(ellipse.uncertainty_mm)],
            FillRule::NonZero,
            0.0,
            accuracy(0.00001)
        )
        .is_err()
    );
}

#[test]
fn selectively_rounded_rectangle_keeps_square_corners_and_narrow_material() {
    let source = shapes::rounded_rect(0.2, 0.6, 0.08, [true, false, true, false], true).unwrap();
    let expected_area = 0.2 * 0.6 - 2.0 * (1.0 - std::f64::consts::PI / 4.0) * 0.08_f64.powi(2);
    let coarse = prepare(std::slice::from_ref(&source), 0.005);
    let fine = prepare(std::slice::from_ref(&source), 0.0001);
    assert!((fine.area() - expected_area).abs() < (coarse.area() - expected_area).abs());
    for point in [Point::new(0.1, -0.3), Point::new(-0.1, 0.3)] {
        assert!(distance(&fine, point) < 1e-7);
    }
    for segment in source.segments() {
        let mut points = Vec::new();
        segment.sample_points(100, &mut points);
        assert!(
            points
                .into_iter()
                .all(|point| distance(&fine, point) <= fine.uncertainty_mm)
        );
    }
    let inset = fine
        .disk_erode_with_accuracy(0.025, accuracy(0.0005))
        .unwrap();
    assert!(!inset.is_empty());
    assert!((inset.bbox.width() - 0.15).abs() < 0.001);
}

#[test]
fn coarse_polygon_history_prevents_finer_preparation_and_offsets() {
    let circle = shapes::circle(0.2).unwrap();
    let coarse = ContourSet::from_contours(&[circle.clone()], FillRule::NonZero, 1e-9);
    assert!(coarse.uncertainty_mm >= 0.005);
    assert!(
        ContourSet::from_contours_with_accuracy(
            &coarse.to_contours(),
            FillRule::NonZero,
            1e-9,
            accuracy(0.0001)
        )
        .is_err()
    );
    assert!(
        coarse
            .disk_dilate_with_accuracy(0.01, accuracy(0.0001))
            .is_err()
    );
    assert!(
        coarse
            .disk_erode_with_accuracy(0.0, accuracy(0.0001))
            .is_err()
    );
    let fine = prepare(&[circle], 0.0001);
    assert!(fine.uncertainty_mm < coarse.uncertainty_mm);
}

#[test]
fn explicit_offsets_converge_for_outside_and_concave_inside_rounds() {
    let rectangle = ContourSet::rectangle(BBox::new(Point::ZERO, Point::new(2.0, 2.0)), 0.0);
    assert!(
        rectangle
            .disk_dilate_with_accuracy(0.2, accuracy(0.00001))
            .is_err()
    );
    let concave = prepare(
        &[shapes::closed_polygon(vec![
            Point::ZERO,
            Point::new(2.0, 0.0),
            Point::new(2.0, 1.0),
            Point::new(1.0, 1.0),
            Point::new(1.0, 2.0),
            Point::new(0.0, 2.0),
        ])
        .unwrap()],
        0.00001,
    );
    for (region, inset, center) in [
        (&rectangle, false, Point::ZERO),
        (&concave, true, Point::new(1.0, 1.0)),
    ] {
        let mut previous = f64::INFINITY;
        for budget in [0.005, 0.001, 0.0001] {
            let result = if inset {
                region.disk_erode_with_accuracy(0.2, accuracy(budget))
            } else {
                region.disk_dilate_with_accuracy(0.2, accuracy(budget))
            }
            .unwrap();
            let max_error = (0..1024)
                .map(|i| {
                    let angle =
                        std::f64::consts::PI + std::f64::consts::FRAC_PI_2 * i as f64 / 1023.0;
                    distance(&result, center + Point::new(angle.cos(), angle.sin()) * 0.2)
                })
                .fold(0.0, f64::max);
            assert!(
                max_error <= result.uncertainty_mm + 1e-10,
                "{max_error} > {}",
                result.uncertainty_mm
            );
            assert!(max_error < previous);
            previous = max_error;
        }
    }
}

#[test]
fn holes_islands_and_fifty_micron_gaps_survive_fine_preparation() {
    let outer = shapes::circle(0.6).unwrap();
    let hole = shapes::circle(0.2).unwrap();
    let island = shapes::circle(0.2)
        .unwrap()
        .transformed(Affine2::translation(Point::new(0.45, 0.0)));
    let region = prepare(&[outer, hole, island], 0.0001);
    assert_eq!(region.connected_components().len(), 2);
    assert!(!region.contains_point(Point::ZERO));
    assert!(region.contains_point(Point::new(0.2, 0.0)));
    assert!(!region.contains_point(Point::new(0.325, 0.0)));
    assert_eq!(
        region
            .segment_spans(Point::new(-1.0, 0.0), Point::new(1.0, 0.0))
            .len(),
        3
    );
    let inset = region
        .disk_erode_with_accuracy(0.025, accuracy(0.0005))
        .unwrap();
    assert_eq!(inset.connected_components().len(), 2);
    assert_eq!(inset.rings.len(), 3);
    assert!(!inset.contains_point(Point::new(0.11, 0.0)));
    for (center, radius) in [
        (Point::ZERO, 0.275),
        (Point::ZERO, 0.125),
        (Point::new(0.45, 0.0), 0.075),
    ] {
        for i in 0..512 {
            let angle = std::f64::consts::TAU * i as f64 / 512.0;
            assert!(
                distance(
                    &inset,
                    center + Point::new(angle.cos(), angle.sin()) * radius
                ) <= inset.uncertainty_mm
            );
        }
    }
    let grown = region
        .disk_dilate_with_accuracy(0.01, accuracy(0.0005))
        .unwrap();
    assert_eq!(grown.connected_components().len(), 2);
    assert_eq!(grown.rings.len(), 3);
    let parts = region.connected_components();
    let clearance = region_clearance(&parts[0], &parts[1]).unwrap();
    assert!((clearance.mm - 0.05).abs() <= clearance.uncertainty_mm);
}

#[test]
fn artwork_significance_is_separate_and_cannot_hide_an_unmet_budget() {
    use pcb_ir::dialects::{LayerRole, Side, artwork};
    use pcb_ir::geom::{Polarity, tol};

    let mut doc = artwork::Document::<(), ()>::new();
    let layer = doc.push_layer(artwork::Layer::new("F.Cu", LayerRole::Copper, Side::Top));
    let path = doc.push_path(
        Paint::Fill {
            rule: FillRule::NonZero,
        },
        vec![shapes::rect(0.0005, 0.0005).unwrap()],
    );
    doc.push_object(
        layer,
        artwork::Object::new(Polarity::Dark, artwork::Geometry::Region { path }),
    );

    let (legacy, _) = artwork::compose_selected_owners(&doc, |_| Some(()));
    assert_eq!(legacy[0].len(), 1);
    let (fine, _) =
        artwork::compose_owner_regions(&doc, |_| Some(()), 0.0, Some(accuracy(1e-6))).unwrap();
    assert_eq!(fine[0].len(), 1);
    assert!(
        artwork::compose_owner_regions(&doc, |_| Some(()), tol::REGION_MM, Some(accuracy(1e-6)))
            .is_err()
    );
}

#[test]
fn inset_reports_miter_amplification_and_uncertain_topology_loss() {
    let sharp = ContourSet::from_regularized_with_uncertainty(
        vec![vec![[0.0, 0.0], [10.0, 0.0], [0.1, 0.1]]],
        0.0,
        0.0001,
    );
    assert!(
        sharp
            .disk_erode_with_accuracy(0.025, accuracy(0.001))
            .is_err()
    );
    let inset = sharp
        .disk_erode_with_accuracy(0.025, accuracy(0.03))
        .unwrap();
    assert!(inset.uncertainty_mm > 0.01);

    let circle = prepare(&[shapes::circle(0.2).unwrap()], 0.0001);
    assert!(
        circle
            .disk_erode_with_accuracy(0.11, accuracy(0.001))
            .is_err()
    );
    assert!(circle.disk_erode(0.11).uncertainty_mm.is_infinite());
}

#[test]
fn analytic_ellipse_survives_arena_copies_and_affine_placement() {
    let mut arena = PathArena::default();
    let path = arena.push_path(
        Paint::Fill {
            rule: FillRule::NonZero,
        },
        [shapes::ellipse(0.4, 0.2).unwrap()],
    );
    let placement = Affine2 {
        m00: -2.0,
        m01: 0.3,
        m02: 5.0,
        m10: 0.2,
        m11: 1.0,
        m12: 7.0,
    };
    let mut copy = PathArena::default();
    let id = copy.append_path_from(&arena, path, placement);
    copy.compact(&[true]);
    let region = ContourSet::from_placed_painted_paths_with_accuracy(
        &copy,
        [(copy.path(id), Affine2::IDENTITY)],
        0.0,
        accuracy(0.000001),
    )
    .unwrap();
    for i in 0..4096 {
        let angle = std::f64::consts::TAU * i as f64 / 4096.0;
        let point = placement.transform_point(Point::new(0.2 * angle.cos(), 0.1 * angle.sin()));
        assert!(distance(&region, point) <= region.uncertainty_mm);
    }
}

#[test]
fn stroke_expansion_carries_its_own_round_cap_floor() {
    use pcb_ir::geom::{PathCmd, StrokeStyle};
    let line = ContourBuf::new(vec![
        PathCmd::move_to(Point::ZERO),
        PathCmd::line_to(Point::new(1.0, 0.0)),
    ]);
    let mut arena = PathArena::default();
    let path = arena.push_path(Paint::Stroke(StrokeStyle::round(0.2)), [line]);
    let fine = ContourSet::from_placed_painted_paths_with_accuracy(
        &arena,
        [(arena.path(path), Affine2::IDENTITY)],
        0.0,
        accuracy(0.0002),
    )
    .unwrap();
    for i in 0..1024 {
        let angle = std::f64::consts::FRAC_PI_2 + std::f64::consts::PI * i as f64 / 1023.0;
        assert!(distance(&fine, Point::new(angle.cos(), angle.sin()) * 0.1) <= fine.uncertainty_mm);
    }
    assert!(
        ContourSet::from_placed_painted_paths_with_accuracy(
            &arena,
            [(arena.path(path), Affine2::IDENTITY)],
            0.0,
            accuracy(0.000001)
        )
        .is_err()
    );
}
