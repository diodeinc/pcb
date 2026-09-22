use super::*;
use crate::geom::BBox;
use crate::geom::attachment::transform_region;

const TOL: QueryTolerance = QueryTolerance {
    boundary_mm: 0.0,
    numerical_mm: 1e-7,
};

fn rect(x: f64, y: f64, w: f64, h: f64) -> ContourSet {
    ContourSet::rectangle(
        BBox::new(Point::new(x, y), Point::new(x + w, y + h)),
        Resolution::default().strict(),
    )
}

fn fixture(curved: bool) -> (ContourSet, ContourSet, ContourSet) {
    let board = if curved {
        transform_region(
            &ContourSet::from_filled_contours(
                &[super::super::shapes::circle(20.0).unwrap()],
                Resolution::default().strict(),
            )
            .unwrap(),
            Affine2::translation(Point::new(0.0, -10.0)),
        )
        .unwrap()
    } else {
        rect(-10.0, -20.0, 20.0, 20.0)
    };
    (
        board,
        rect(-10.0, 3.0, 20.0, 10.0),
        rect(-10.0, -20.0, 20.0, 33.0),
    )
}

fn tab(
    board: &ContourSet,
    support: &ContourSet,
    stock: &ContourSet,
) -> Result<TabGeometry, QueryError> {
    let query = BoundaryQuery::new(board, TOL)?;
    let boundary = query.boundaries().next().unwrap();
    let site = query.project(boundary, Point::ZERO)?.site;
    build(Attachment {
        board,
        support,
        stock,
        boundary,
        station_mm: site.station_mm,
        support_anchor: Point::new(0.0, 5.0),
        board_witness: Point::new(0.0, -5.0),
        tolerance: TOL,
    })
}

#[test]
fn straight_and_curved_retention_release_intrusion_and_cutter_fit() -> Result<(), QueryError> {
    for curved in [false, true] {
        let (board, support, stock) = fixture(curved);
        let tab = tab(&board, &support, &stock).unwrap();
        assert_eq!(tab.npth.len(), 5);
        // Consecutive centers are one pitch apart in a straight line on the
        // curve as on the straight edge, so the web is exactly nominal.
        for pair in tab.npth.windows(2) {
            let chord = pair[0].center.distance_to(pair[1].center);
            assert!((chord - SparkFunShallow::PITCH_MM).abs() < 1e-9, "{chord}");
        }
        assert!((tab.minimum_ligament_mm - SparkFunShallow::NOMINAL_LIGAMENT_MM).abs() < 1e-9);
        // Repeated overlays on curved polygons can leave sub-micron slivers.
        // Check both area and penetration for every material/removal pair,
        // including drill boundaries whose last-bit rounding varies by platform.
        let protected = board.union(&support)?;
        for (label, material, removal) in [
            ("route", &tab.retained_substrate, &tab.routed_removal),
            ("drill", &tab.retained_substrate, &tab.perforations),
            ("protected", &protected, &tab.routed_removal),
        ] {
            let overlap = material.intersection(removal)?;
            assert!(
                overlap.area() < 1e-6,
                "{label} curved={curved}: overlap area {}",
                overlap.area()
            );
            assert!(
                overlap.intersection(&removal.disk_erode(1e-6)?)?.is_empty(),
                "{label} curved={curved}: overlap exceeds 1nm penetration"
            );
        }
        assert!(
            stock
                .difference(
                    &tab.retained_substrate
                        .union(&tab.routed_removal)?
                        .union(&tab.perforations)?
                )?
                .area()
                < 1e-6
        );
        assert!(!tab.perforations.intersection(&board)?.is_empty());
        // Full-mask intrusion check, not merely drill-center placement.
        assert!(
            tab.perforations
                .intersection(&board.disk_erode(SparkFunShallow::NOMINAL_INTRUSION_MM + 0.015)?)?
                .is_empty()
        );
        let query = BoundaryQuery::new(&board, TOL).unwrap();
        let id = query.boundaries().next().unwrap();
        for drill in &tab.npth {
            let offset = query.project(id, drill.center).unwrap().distance.mm;
            assert!((offset - SparkFunShallow::OUTWARD_OFFSET_MM).abs() < 0.006);
        }
        let witnesses = [Point::new(0.0, -5.0), Point::new(0.0, 5.0)];
        for width in [0.002, 0.01, 0.02] {
            let after = tab.after_break(width, &witnesses, TOL).unwrap();
            assert_eq!(
                after.connected(0, 1).unwrap(),
                Some(false),
                "curved={curved}, width={width}"
            );
            assert_eq!(after.components.len(), 2, "no loose shoulder islands");
        }
        // The router leaves rounded shoulders where the neck meets the walls.
        let shoulders = stock
            .difference(&tab.routed_removal)?
            .difference(&protected.union(&tab.neck)?)?;
        assert!(shoulders.area() > 0.05);
        // The cutter that made them fits everything it is said to remove
        // around the neck, to within the flattening of its own arcs; a larger
        // one does not inherit that.
        let around_neck = rect(-3.0, -1.0, 6.0, 5.0);
        let unreached = |radius: f64| -> Result<f64, QueryError> {
            Ok(tab
                .routed_removal
                .difference(&tab.routed_removal.disk_open(radius)?)?
                .intersection(&around_neck)?
                .area())
        };
        assert!(unreached(SparkFunShallow::CUTTER_RADIUS_MM)? < 0.005);
        assert!(unreached(0.8)? > 0.05);
    }
    Ok(())
}

#[test]
fn missing_ligament_and_bad_stock_are_not_successful_tabs() {
    let (board, support, stock) = fixture(false);
    assert!(tab(&board, &support, &board).is_err());
    let missing = stock.difference(&rect(-0.4, 0.0, 0.2, 3.0)).unwrap();
    assert!(tab(&board, &support, &missing).is_err());
    let tab = tab(&board, &support, &stock).unwrap();
    assert!(tab.after_break(f64::NAN, &[], TOL).is_err());
    let unknown = tab
        .after_break(0.01, &[Point::new(100.0, 100.0), Point::new(0.0, 5.0)], TOL)
        .unwrap();
    assert_eq!(unknown.connected(0, 1).unwrap(), None);
}

#[test]
fn support_flush_with_the_stock_edge_is_inside_it() {
    // A frame clipped to its cell shares the cell's edge, and the booleans
    // that clip it may leave that edge a rounding step outside. Anything
    // thicker than the query tolerance is still outside.
    let (board, _, stock) = fixture(false);
    let flush = |beyond: f64| {
        ContourSet::from_regularized(
            vec![vec![
                [-10.0, 3.0],
                [10.0, 3.0],
                [10.0, 13.0 + beyond],
                [-10.0, 13.0 + beyond],
            ]],
            Resolution::default().strict(),
            0.0,
        )
    };
    assert!(tab(&board, &flush(7e-15), &stock).is_ok());
    assert!(matches!(
        tab(&board, &flush(1e-6), &stock),
        Err(QueryError::InvalidInput(
            "expected board and support inside stock"
        ))
    ));
}

#[test]
fn witnesses_must_be_interior_to_the_original_regions() {
    let (board, support, stock) = fixture(false);
    let query = BoundaryQuery::new(&board, TOL).unwrap();
    let boundary = query.boundaries().next().unwrap();
    let station_mm = query
        .project(boundary, Point::ZERO)
        .unwrap()
        .site
        .station_mm;
    let anchor = "expected the support anchor inside support";
    let witness = "expected the board witness inside the board";
    for (support_anchor, board_witness, expected) in [
        (Point::new(0.0, 3.0), Point::new(0.0, -5.0), anchor),
        (Point::new(0.0, 3.0 - 1e-7), Point::new(0.0, -5.0), anchor),
        (Point::new(0.0, 5.0), Point::ZERO, witness),
        (Point::new(0.0, 5.0), Point::new(0.0, 1e-7), witness),
    ] {
        assert!(matches!(
            build(Attachment {
                board: &board,
                support: &support,
                stock: &stock,
                boundary,
                station_mm,
                support_anchor,
                board_witness,
                tolerance: TOL,
            }),
            Err(QueryError::InvalidInput(message)) if message == expected
        ));
    }
}

#[test]
fn perforations_require_resolved_clearance_from_support() {
    let (board, _, stock) = fixture(false);
    let drill_top = SparkFunShallow::OUTWARD_OFFSET_MM + SparkFunShallow::HOLE_DIAMETER_MM / 2.0;
    // Overlap, tangency, and a positive gap smaller than stored uncertainty
    // must all fail before drilling support. A resolved gap remains supported.
    for bottom in [0.2, drill_top, drill_top + 0.001] {
        let support = rect(-10.0, bottom, 20.0, 10.0);
        assert!(
            matches!(
                tab(&board, &support, &stock),
                Err(QueryError::InvalidInput(
                    "perforations overlap support or clearance is unresolved"
                ))
            ),
            "support bottom={bottom}"
        );
    }
    let support = rect(-10.0, 3.0, 20.0, 10.0);
    let result = tab(&board, &support, &stock).unwrap();
    assert!(
        result
            .perforations
            .intersection(&support)
            .unwrap()
            .is_empty()
    );
    assert!(
        support
            .difference(&result.retained_substrate)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn input_accuracy_is_preserved_and_failures_propagate() {
    let (board, support, stock) = fixture(true);
    let result = tab(&board, &support, &stock).unwrap();
    assert!(result.break_path.uncertainty_mm >= board.uncertainty_mm);
    assert!(result.perforations.uncertainty_mm >= result.break_path.uncertainty_mm);
    assert_eq!(result.retained_substrate.budget(), board.budget());
    let exhausted = ContourSet::from_regularized(
        board.rings.clone(),
        board.resolution,
        board.budget().max_error_mm(),
    );
    assert!(matches!(
        tab(&exhausted, &support, &stock),
        Err(QueryError::Accuracy(_))
    ));
}
