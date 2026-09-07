use super::*;
use crate::geom::BBox;
use crate::geom::attachment::{Decision, cutter_reachability};

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
fn straight_and_curved_retention_release_intrusion_and_cutter_access() -> Result<(), QueryError> {
    for curved in [false, true] {
        let (board, support, stock) = fixture(curved);
        let tab = tab(&board, &support, &stock).unwrap();
        assert_eq!(tab.npth.len(), 5);
        assert!(tab.minimum_ligament_mm > 0.25);
        assert!(tab.minimum_ligament_mm <= SparkFunShallow::NOMINAL_LIGAMENT_MM + 1e-6);
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
        assert!(tab.shoulders.area() > 0.05);
        // Both shoulder approaches must be reachable with a 1mm cutter, from
        // explicit side entries. Targets are deliberately off contact surfaces.
        let access = cutter_reachability(
            &rect(-12.0, -22.0, 24.0, 37.0),
            &tab.retained_substrate,
            0.5,
            &[Point::new(-11.0, 1.5), Point::new(11.0, 1.5)],
            &[Point::new(-1.6, 0.6), Point::new(1.6, 0.6)],
            TOL,
        )
        .unwrap();
        assert_eq!(
            access.targets,
            vec![Decision::Admissible, Decision::Admissible]
        );
        // A larger cutter must not inherit the small-cutter shoulder approval.
        let large = cutter_reachability(
            &rect(-12.0, -22.0, 24.0, 37.0),
            &tab.retained_substrate,
            0.8,
            &[Point::new(-11.0, 1.5), Point::new(11.0, 1.5)],
            &[Point::new(-1.6, 0.6), Point::new(1.6, 0.6)],
            TOL,
        )
        .unwrap();
        assert!(large.targets.iter().all(|d| *d != Decision::Admissible));
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
