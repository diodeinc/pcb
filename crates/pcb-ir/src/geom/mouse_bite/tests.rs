use super::*;
use crate::geom::BBox;
use crate::geom::attachment::{Decision, cutter_reachability};

const TOL: QueryTolerance = QueryTolerance {
    boundary_mm: 0.0,
    numerical_mm: 1e-7,
};

fn rect(x: f64, y: f64, w: f64, h: f64) -> ContourSet {
    ContourSet::rectangle(BBox::new(Point::new(x, y), Point::new(x + w, y + h)), 0.0)
}

fn fixture(curved: bool) -> (ContourSet, ContourSet, ContourSet) {
    let board = if curved {
        transform_region(
            &ContourSet::from_filled_contours(&[super::super::shapes::circle(20.0).unwrap()], 0.0),
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
fn straight_and_curved_retention_release_intrusion_and_cutter_access() {
    for curved in [false, true] {
        let (board, support, stock) = fixture(curved);
        let tab = tab(&board, &support, &stock).unwrap();
        assert_eq!(tab.npth.len(), 5);
        assert!(tab.minimum_ligament_mm > 0.25);
        assert!(tab.minimum_ligament_mm <= SparkFunShallow::NOMINAL_LIGAMENT_MM + 1e-6);
        assert!(
            tab.retained_substrate
                .intersection(&tab.routed_removal)
                .is_empty()
        );
        assert!(
            tab.retained_substrate
                .intersection(&tab.perforations)
                .is_empty()
        );
        // Repeated overlays on curved polygons can leave sub-micron slivers.
        // Check both area and penetration, not exact empty-set equality.
        let overlap = tab.routed_removal.intersection(&board.union(&support));
        assert!(overlap.area() < 1e-6);
        assert!(
            overlap
                .intersection(&board.union(&support).disk_erode(1e-6))
                .is_empty()
        );
        assert!(
            stock
                .difference(
                    &tab.retained_substrate
                        .union(&tab.routed_removal)
                        .union(&tab.perforations)
                )
                .area()
                < 1e-6
        );
        assert!(!tab.perforations.intersection(&board).is_empty());
        // Full-mask intrusion check, not merely drill-center placement.
        assert!(
            tab.perforations
                .intersection(&board.disk_erode(SparkFunShallow::NOMINAL_INTRUSION_MM + 0.015))
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
}

#[test]
fn missing_ligament_and_bad_stock_are_not_successful_tabs() {
    let (board, support, stock) = fixture(false);
    assert!(tab(&board, &support, &board).is_err());
    let missing = stock.difference(&rect(-0.4, 0.0, 0.2, 3.0));
    assert!(tab(&board, &support, &missing).is_err());
    let tab = tab(&board, &support, &stock).unwrap();
    assert!(tab.after_break(f64::NAN, &[], TOL).is_err());
    let unknown = tab
        .after_break(0.01, &[Point::new(100.0, 100.0), Point::new(0.0, 5.0)], TOL)
        .unwrap();
    assert_eq!(unknown.connected(0, 1).unwrap(), None);
}
