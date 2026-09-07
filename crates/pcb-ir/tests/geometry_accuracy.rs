//! Accuracy budgets are chosen once, at preparation, and travel with the
//! prepared region. These tests exercise that contract end to end.

use pcb_ir::geom::dfm::region_clearance;
use pcb_ir::geom::dist::point_segment;
use pcb_ir::geom::{
    Affine2, BBox, ContourBuf, ContourSet, FillRule, GeometryAccuracy, Paint, PathArena, Point,
    Resolution, shapes,
};

fn accuracy(mm: f64) -> GeometryAccuracy {
    GeometryAccuracy::new(mm).unwrap()
}
fn resolution(mm: f64) -> Resolution {
    Resolution::new(1e-6, accuracy(mm))
}
fn prepare(contours: &[ContourBuf], mm: f64) -> ContourSet {
    ContourSet::from_contours(contours, FillRule::EvenOdd, resolution(mm)).unwrap()
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
fn circles_converge_with_the_requested_budget() {
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
        assert_eq!(region.budget(), accuracy(budget));
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
fn ellipses_are_exact_sources_and_prepare_arbitrarily_fine() {
    let ellipse = shapes::ellipse(0.4, 0.2).unwrap();
    assert_eq!(ellipse.uncertainty_mm, 0.0);
    let coarse = prepare(std::slice::from_ref(&ellipse), 0.005);
    let fine = prepare(std::slice::from_ref(&ellipse), 0.000001);
    assert!(radial_error(&fine, 0.2, 0.1) < radial_error(&coarse, 0.2, 0.1));
    assert!(radial_error(&fine, 0.2, 0.1) <= fine.uncertainty_mm);
    // Commands that already carry approximation cannot be prepared below it.
    assert!(
        ContourSet::from_contours(
            &[ellipse.with_uncertainty(0.0001)],
            FillRule::NonZero,
            resolution(0.00001)
        )
        .is_err()
    );
}

#[test]
fn coarse_polygon_history_prevents_finer_preparation_and_offsets() {
    let circle = shapes::circle(0.2).unwrap();
    let coarse = ContourSet::from_contours(
        std::slice::from_ref(&circle),
        FillRule::NonZero,
        Resolution::new(1e-9, accuracy(0.01)),
    )
    .unwrap();
    assert!(coarse.uncertainty_mm > 0.0001);
    assert!(
        ContourSet::from_contours(
            &coarse.to_contours(),
            FillRule::NonZero,
            Resolution::new(1e-9, accuracy(0.0001))
        )
        .is_err()
    );
    // Tightening the budget below the recorded approximation is refused.
    assert!(coarse.clone().rebudget(accuracy(0.0001)).is_err());
    // Loosening always works, and derived regions inherit the budget.
    let loose = coarse.clone().rebudget(accuracy(0.05)).unwrap();
    assert_eq!(loose.disk_dilate(0.01).unwrap().budget(), accuracy(0.05));
    // Polygon round trips through an arena keep their history.
    let mut source = PathArena::default();
    let path = source.push_path(
        Paint::Fill {
            rule: FillRule::NonZero,
        },
        coarse.to_contours(),
    );
    let mut copy = PathArena::default();
    let copied = copy.append_path_from(&source, path, Affine2::IDENTITY);
    assert!(
        ContourSet::from_painted_paths(&copy, [copy.path(copied)], resolution(0.0001)).is_err()
    );
    let fine = prepare(&[circle], 0.0001);
    assert!(fine.uncertainty_mm < coarse.uncertainty_mm);
}

#[test]
fn explicit_offsets_converge_for_outside_and_concave_inside_rounds() {
    let rectangle = |budget| {
        ContourSet::rectangle(
            BBox::new(Point::ZERO, Point::new(2.0, 2.0)),
            Resolution::new(0.0, accuracy(budget)),
        )
    };
    assert!(rectangle(0.00001).disk_dilate(0.2).is_err());
    let concave = |budget| {
        prepare(
            &[shapes::closed_polygon(vec![
                Point::ZERO,
                Point::new(2.0, 0.0),
                Point::new(2.0, 1.0),
                Point::new(1.0, 1.0),
                Point::new(1.0, 2.0),
                Point::new(0.0, 2.0),
            ])
            .unwrap()],
            budget,
        )
    };
    for (inset, center) in [(false, Point::ZERO), (true, Point::new(1.0, 1.0))] {
        let mut previous = f64::INFINITY;
        for budget in [0.005, 0.001, 0.0001] {
            let result = if inset {
                concave(budget).disk_erode(0.2)
            } else {
                rectangle(budget).disk_dilate(0.2)
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
        .clone()
        .rebudget(accuracy(0.0005))
        .unwrap()
        .disk_erode(0.025)
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
        .clone()
        .rebudget(accuracy(0.0005))
        .unwrap()
        .disk_dilate(0.01)
        .unwrap();
    assert_eq!(grown.connected_components().len(), 2);
    assert_eq!(grown.rings.len(), 3);
    let parts = region.connected_components();
    let clearance = region_clearance(&parts[0], &parts[1]).unwrap();
    assert!((clearance.mm - 0.05).abs() <= clearance.uncertainty_mm);
}

#[test]
fn circles_and_ellipses_survive_arena_copies_and_affine_placement_exactly() {
    for source in [
        shapes::ellipse(0.4, 0.2).unwrap(),
        shapes::circle(0.4).unwrap(),
    ] {
        let mut arena = PathArena::default();
        let path = arena.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            [source.clone()],
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
        // The placed copy is still an exact source: no approximation yet.
        assert_eq!(copy.contours[0].uncertainty_mm, 0.0);
        let region = ContourSet::from_placed_painted_paths(
            &copy,
            [(copy.path(id), Affine2::IDENTITY)],
            Resolution::new(0.0, accuracy(0.000001)),
        )
        .unwrap();
        let (rx, ry) = (source.bbox.width() / 2.0, source.bbox.height() / 2.0);
        for i in 0..4096 {
            let angle = std::f64::consts::TAU * i as f64 / 4096.0;
            let point = placement.transform_point(Point::new(rx * angle.cos(), ry * angle.sin()));
            assert!(distance(&region, point) <= region.uncertainty_mm);
        }
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
        Resolution::new(0.0, accuracy(0.0002)),
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
            Resolution::new(0.0, accuracy(0.000001))
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
        let (layers, _) = artwork::compose_owner_regions(
            &doc,
            |_| Some(()),
            Resolution::new(0.0, accuracy(1e-6)),
        )
        .unwrap();
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
    let resolution = Resolution::default();
    let first = ContourSet::from_regularized(
        vec![
            vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            vec![[4.0, 4.0], [4.0001, 4.0], [4.0, 4.0001]],
        ],
        resolution,
        0.0,
    );
    let second = ContourSet::rectangle(
        BBox::new(Point::new(1.05, 0.0), Point::new(2.0, 1.0)),
        resolution,
    );
    let clipped = first.difference(&second).unwrap();
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
    let resolution = Resolution::new(0.0, accuracy(0.01));
    let direct = ContourSet::from_placed_painted_paths(
        &doc.arena,
        [(doc.arena.path(path), Affine2::IDENTITY)],
        resolution,
    )
    .unwrap();
    assert!((0.008..=0.01).contains(&direct.uncertainty_mm));
    let (layers, _) = artwork::compose_owner_regions(&doc, |_| Some(()), resolution).unwrap();
    assert!((0.008..=0.01).contains(&layers[0][0].1.uncertainty_mm));
}

#[test]
fn fine_geometry_reports_widths_inside_the_legacy_blind_band() {
    use pcb_ir::geom::dfm::{thin_features, thin_gaps};
    let rect = |x0, y0, x1, y1| {
        ContourSet::rectangle(
            BBox::new(Point::new(x0, y0), Point::new(x1, y1)),
            Resolution::new(1e-6, accuracy(0.01)),
        )
    };
    let feature = rect(0.0, 0.0, 1.0, 0.095);
    assert!(!thin_features(&feature, 0.1).unwrap().is_empty());
    let gap = rect(0.0, 0.0, 1.0, 1.0)
        .union(&rect(1.095, 0.0, 2.0, 1.0))
        .unwrap();
    assert!(!thin_gaps(&gap, 0.1).unwrap().is_empty());
}

#[test]
fn polygon_rounding_and_filled_union_respect_total_budget() {
    let polygon = ContourSet::from_rings(
        vec![vec![
            [1e9, 0.0],
            [1e9 + 1.0, 0.0],
            [1e9 + 1.0, 1.0],
            [1e9, 1.0],
        ]],
        FillRule::NonZero,
        Resolution::new(0.0, accuracy(0.01)),
    )
    .unwrap();
    assert!(polygon.uncertainty_mm > 0.0);
    assert!(
        polygon
            .clone()
            .rebudget(accuracy(polygon.uncertainty_mm / 2.0))
            .is_err()
    );
    let contours = polygon.to_contours();
    let prepared = ContourSet::from_contours(
        &contours,
        FillRule::EvenOdd,
        Resolution::new(0.0, accuracy(0.01)),
    )
    .unwrap();
    assert!(
        ContourSet::from_filled_contours(
            &contours,
            Resolution::new(0.0, accuracy(prepared.uncertainty_mm * 1.1))
        )
        .is_err()
    );
}
