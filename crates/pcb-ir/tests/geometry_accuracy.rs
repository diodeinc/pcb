use pcb_ir::geom::dfm::region_clearance;
use pcb_ir::geom::dist::point_segment;
use pcb_ir::geom::{
    Affine2, BBox, ContourBuf, ContourSet, FillRule, GeometryAccuracy, Paint, PathArena, Point,
    shapes,
};

fn accuracy(mm: f64) -> GeometryAccuracy {
    GeometryAccuracy::new(mm).unwrap()
}
fn prepare(contours: &[ContourBuf], mm: f64) -> ContourSet {
    ContourSet::from_contours(contours, FillRule::EvenOdd, 1e-6, accuracy(mm)).unwrap()
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
        let measured = region
            .prepare_query()
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
        ContourSet::from_contours(
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
    let inset = fine.disk_erode(0.025, accuracy(0.0005)).unwrap();
    assert!(!inset.is_empty());
    assert!((inset.bbox.width() - 0.15).abs() < 0.001);
}

#[test]
fn coarse_polygon_history_prevents_finer_preparation_and_offsets() {
    let circle = shapes::circle(0.2).unwrap();
    let coarse = ContourSet::from_contours(
        std::slice::from_ref(&circle),
        FillRule::NonZero,
        1e-9,
        accuracy(0.01),
    )
    .unwrap();
    assert!(coarse.uncertainty_mm > 0.0001);
    assert!(
        ContourSet::from_contours(
            &coarse.to_contours(),
            FillRule::NonZero,
            1e-9,
            accuracy(0.0001)
        )
        .is_err()
    );
    assert!(coarse.disk_dilate(0.01, accuracy(0.0001)).is_err());
    let mut source = PathArena::default();
    let path = source.push_path(
        Paint::Fill {
            rule: FillRule::NonZero,
        },
        coarse.to_contours(),
    );
    assert!(
        PathArena::default()
            .append_path_from(&source, path, Affine2::IDENTITY, accuracy(0.0001))
            .is_err()
    );

    assert!(coarse.disk_erode(0.0, accuracy(0.0001)).is_err());
    let fine = prepare(&[circle], 0.0001);
    assert!(fine.uncertainty_mm < coarse.uncertainty_mm);
}

#[test]
fn explicit_offsets_converge_for_outside_and_concave_inside_rounds() {
    let rectangle = ContourSet::rectangle(BBox::new(Point::ZERO, Point::new(2.0, 2.0)), 0.0);
    assert!(rectangle.disk_dilate(0.2, accuracy(0.00001)).is_err());
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
                region.disk_erode(0.2, accuracy(budget))
            } else {
                region.disk_dilate(0.2, accuracy(budget))
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
        .transformed(
            Affine2::translation(Point::new(0.45, 0.0)),
            accuracy(0.00001),
        )
        .unwrap();
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
    let inset = region.disk_erode(0.025, accuracy(0.0005)).unwrap();
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
    let grown = region.disk_dilate(0.01, accuracy(0.0005)).unwrap();
    assert_eq!(grown.connected_components().len(), 2);
    assert_eq!(grown.rings.len(), 3);
    let parts = region.connected_components();
    let clearance = region_clearance(&parts[0], &parts[1]).unwrap();
    assert!((clearance.mm - 0.05).abs() <= clearance.uncertainty_mm);
}

#[test]
fn inset_preserves_input_error_and_handles_erasure() {
    let sharp =
        ContourSet::from_regularized(vec![vec![[0.0, 0.0], [10.0, 0.0], [0.1, 0.1]]], 0.0, 0.0001);
    let inset = sharp.disk_erode(0.025, accuracy(0.001)).unwrap();
    assert!(inset.uncertainty_mm >= sharp.uncertainty_mm);
    assert!(inset.uncertainty_mm <= 0.001);

    let circle = prepare(&[shapes::circle(0.2).unwrap()], 0.0001);
    assert!(circle.disk_erode(0.11, accuracy(0.001)).unwrap().is_empty());
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
    let id = copy
        .append_path_from(&arena, path, placement, accuracy(0.005))
        .unwrap();
    copy.compact(&[true]);
    let region = ContourSet::from_placed_painted_paths(
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
    let fine = ContourSet::from_placed_painted_paths(
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
        ContourSet::from_placed_painted_paths(
            &arena,
            [(arena.path(path), Affine2::IDENTITY)],
            0.0,
            accuracy(0.000001)
        )
        .is_err()
    );
}

#[test]
fn fine_artwork_budgets_reach_flashes_and_instanced_arcs() {
    use pcb_ir::dialects::{LayerRole, Side, artwork};
    use pcb_ir::geom::Polarity;
    for instance in [false, true] {
        let mut doc = artwork::Document::<(), ()>::new();
        let layer = doc.push_layer(artwork::Layer::new("F.Cu", LayerRole::Copper, Side::Top));
        let transform = Affine2 {
            m00: 2.0,
            m11: 0.5,
            ..Affine2::IDENTITY
        };
        let geometry = if instance {
            let block = doc.push_block();
            let path = doc.push_path(
                Paint::Fill {
                    rule: FillRule::NonZero,
                },
                vec![shapes::circle(2.0).unwrap()],
            );
            doc.push_block_object(
                block,
                artwork::Object::new(Polarity::Dark, artwork::Geometry::Region { path }),
            );
            artwork::Geometry::Instance { block, transform }
        } else {
            let aperture = doc.push_aperture(artwork::Aperture::circle(2.0));
            artwork::Geometry::Flash {
                aperture,
                transform,
            }
        };
        doc.push_object(layer, artwork::Object::new(Polarity::Dark, geometry));
        let (layers, _) =
            artwork::compose_owner_regions(&doc, |_| Some(()), 0.0, accuracy(1e-6)).unwrap();
        let region = &layers[0][0].1;
        assert!(region.uncertainty_mm <= 1e-6);
        assert!(radial_error(region, 2.0, 0.5) <= region.uncertainty_mm);
    }
}

#[test]
fn stroke_budget_reserves_and_records_coordinate_error() {
    use pcb_ir::geom::{
        LineCap, LineJoin, PathCmd,
        path::{StrokeToFillStyle, stroke_to_fill},
    };
    let line = ContourBuf::new(vec![
        PathCmd::move_to(Point::new(1e9, 0.0)),
        PathCmd::line_to(Point::new(1e9 + 1.0, 0.0)),
    ])
    .with_uncertainty(0.00005);
    let style = StrokeToFillStyle::new(0.2, LineCap::Round, LineJoin::Round);
    assert!(stroke_to_fill(std::slice::from_ref(&line), style, accuracy(0.0001)).is_err());
    let outlines = stroke_to_fill(&[line], style, accuracy(0.0002))
        .unwrap()
        .unwrap();
    for outline in outlines {
        assert!(outline.uncertainty_mm >= 0.00005 + 0.00004 + 64.0 * f64::EPSILON * 1e9);
        assert!(outline.uncertainty_mm <= 0.0002);
    }
}

#[test]
fn tiny_rings_do_not_suppress_clearance_findings() {
    let first = ContourSet::from_regularized(
        vec![
            vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            vec![[4.0, 4.0], [4.0001, 4.0], [4.0, 4.0001]],
        ],
        0.001,
        0.0,
    );
    let second = ContourSet::rectangle(
        BBox::new(Point::new(1.05, 0.0), Point::new(2.0, 1.0)),
        0.001,
    );
    let clipped = first.difference(&second);
    let clearance = region_clearance(&clipped, &second).unwrap();
    assert!(clearance.certainly_below(0.1));
}

#[test]
fn stroke_preparation_uses_the_total_inherited_error_budget() {
    use pcb_ir::dialects::{LayerRole, Side, artwork};
    use pcb_ir::geom::{LineCap, LineJoin, PathCmd, Polarity, StrokeStyle};
    let source = ContourBuf::new(vec![
        PathCmd::move_to(Point::ZERO),
        PathCmd::line_to(Point::new(1.0, 0.0)),
    ])
    .with_uncertainty(0.008);
    let paint = Paint::Stroke(StrokeStyle {
        join: LineJoin::Miter,
        ..StrokeStyle::new(0.2, LineCap::Butt)
    });
    let mut doc = artwork::Document::<(), ()>::new();
    let layer = doc.push_layer(artwork::Layer::new("F.Cu", LayerRole::Copper, Side::Top));
    let path = doc.push_path(paint, vec![source]);
    doc.push_object(
        layer,
        artwork::Object::new(Polarity::Dark, artwork::Geometry::Stroke { path }),
    );
    let direct = ContourSet::from_placed_painted_paths(
        &doc.arena,
        [(doc.arena.path(path), Affine2::IDENTITY)],
        0.0,
        accuracy(0.01),
    )
    .unwrap();
    assert!((0.008..=0.01).contains(&direct.uncertainty_mm));
    let (layers, _) =
        artwork::compose_owner_regions(&doc, |_| Some(()), 0.0, accuracy(0.01)).unwrap();
    assert!((0.008..=0.01).contains(&layers[0][0].1.uncertainty_mm));
}

#[test]
fn fine_geometry_reports_widths_inside_the_legacy_blind_band() {
    use pcb_ir::geom::dfm::{thin_features, thin_gaps};
    let rect = |x0, y0, x1, y1| {
        ContourSet::rectangle(BBox::new(Point::new(x0, y0), Point::new(x1, y1)), 1e-6)
    };
    let feature = rect(0.0, 0.0, 1.0, 0.095);
    assert!(
        !thin_features(&feature, 0.1, accuracy(0.01))
            .unwrap()
            .is_empty()
    );
    let gap = rect(0.0, 0.0, 1.0, 1.0).union(&rect(1.095, 0.0, 2.0, 1.0));
    assert!(!thin_gaps(&gap, 0.1, accuracy(0.01)).unwrap().is_empty());
}

#[test]
fn empty_paint_does_not_add_uncertainty() {
    use pcb_ir::geom::{Polarity, region::PaintComposer};
    let mut composer = PaintComposer::default();
    composer.push(
        Polarity::Clear,
        ContourSet::from_regularized(vec![], 0.0, 1.0),
    );
    composer.push(
        Polarity::Dark,
        ContourSet::rectangle(BBox::new(Point::ZERO, Point::new(1.0, 1.0)), 0.0),
    );
    let image = composer.finish(0.0);
    assert!(!image.is_empty());
    accuracy(0.000001).check(image.uncertainty_mm).unwrap();
}

#[test]
fn later_paint_does_not_change_an_earlier_runs_accuracy() {
    use pcb_ir::geom::{Polarity, region::PaintComposer};
    let mut paint = PaintComposer::default();
    paint.push(
        Polarity::Dark,
        ContourSet::rectangle(BBox::new(Point::new(-2.0, -1.0), Point::new(2.0, 0.0)), 0.0),
    );
    paint.push(
        Polarity::Dark,
        ContourSet::from_regularized(
            vec![vec![[-2.0, -0.02], [2.0, 0.02], [2.0, 1.0], [-2.0, 1.0]]],
            0.0,
            0.0,
        ),
    );
    let mut clear =
        ContourSet::rectangle(BBox::new(Point::new(3.0, 0.0), Point::new(4.0, 1.0)), 0.0);
    clear.uncertainty_mm = 0.001;
    paint.push(Polarity::Clear, clear);
    let result = paint.finish(0.0);
    assert!(!result.is_empty());
    accuracy(0.002).check(result.uncertainty_mm).unwrap();
}

#[test]
fn polygon_rounding_and_filled_union_respect_total_budget() {
    let polygon = ContourSet::new(
        vec![vec![
            [1e9, 0.0],
            [1e9 + 1.0, 0.0],
            [1e9 + 1.0, 1.0],
            [1e9, 1.0],
        ]],
        FillRule::NonZero,
        0.0,
    );
    assert!(polygon.uncertainty_mm > 0.0);
    assert!(
        polygon
            .disk_dilate(0.0, accuracy(polygon.uncertainty_mm / 2.0))
            .is_err()
    );
    let contours = polygon.to_contours();
    let prepared =
        ContourSet::from_contours(&contours, FillRule::EvenOdd, 0.0, accuracy(0.01)).unwrap();
    assert!(
        ContourSet::from_filled_contours(&contours, 0.0, accuracy(prepared.uncertainty_mm * 1.1))
            .is_err()
    );
}

#[test]
fn compound_offsets_check_final_rounding_and_noop_inputs() {
    let region = ContourSet::rectangle(
        BBox::new(Point::new(1e9, 0.0), Point::new(1e9 + 1.0, 1.0)),
        0.0,
    );
    let tight = accuracy(1e-6);
    for operation in [ContourSet::disk_open, ContourSet::disk_close] {
        assert!(operation(&region, 0.0, tight).is_err());
        assert!(operation(&region, -1.0, GeometryAccuracy::default()).is_err());
    }
}
