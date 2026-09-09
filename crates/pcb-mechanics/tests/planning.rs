use pcb_elastic::Tolerances;
use pcb_ir::geom::mesh::MeshOptions;
use pcb_ir::geom::{BBox, ContourSet, Point, Resolution};
use pcb_mechanics::planning::{LoadCase, Policy, Site, evaluate};

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> ContourSet {
    ContourSet::rectangle(
        BBox::new(Point::new(x0, y0), Point::new(x1, y1)),
        Resolution::default(),
    )
}

fn policy() -> Policy {
    Policy {
        bending: [[2.0, 0.2, 0.0], [0.2, 2.0, 0.0], [0.0, 0.0, 0.9]],
        connection_stiffness: [[20.0, 0.0, 0.0], [0.0, 10.0, 0.0], [0.0, 0.0, 10.0]],
        scales: [1.0, 0.2],
        mesh_options: MeshOptions {
            max_area_mm2: 0.3,
            min_angle_degrees: 20.0,
            max_additional_vertices: 500,
        },
        max_dofs: 10_000,
        max_subsets: 4,
        tolerances: Tolerances {
            rank_relative: 1e-10,
            rank_absolute: 1e-12,
            residual_relative: 1e-9,
            residual_absolute: 1e-10,
        },
        clamp_sides: [false, true, false, false],
    }
}

#[test]
fn clipped_cross_element_landings_and_asymmetric_resultant_evaluate() {
    let boards = [rect(0.0, 0.0, 2.0, 2.0)];
    let frame = rect(0.0, 0.0, 4.0, 2.0);
    let sites = [Site {
        id: 7,
        board: 0,
        // Neither footprint follows analysis element boundaries.
        board_landing: rect(0.35, 0.45, 1.65, 1.25),
        frame_landing: rect(2.35, 0.35, 3.45, 1.35),
        reference: [2.0, 1.0],
    }];
    let result = evaluate(
        &boards,
        &frame,
        &sites,
        &[],
        &[LoadCase {
            board_resultants: vec![[1.3, -0.4, 0.7]],
            compliance_limit: 1e9,
        }],
        &policy(),
    )
    .unwrap();
    assert!(result.dofs > 0);
    assert_eq!(result.mesh_quality.len(), 2);
    let selected = result
        .report
        .selected
        .as_ref()
        .expect("one coupled tab supports the board");
    assert_eq!(selected.ids, vec![7]);
    let [w, tx, ty] = result.board_responses[0][0];
    let work = 1.3 * w - 0.4 * tx + 0.7 * ty;
    assert!((work - selected.analyses[0].compliance).abs() < 1e-9);
    assert!(selected.analyses[0].relative_residual < 1e-9);
}

#[test]
fn uncovered_and_hole_landings_are_rejected_without_snapping() {
    let outer = rect(0.0, 0.0, 2.0, 2.0);
    let board = outer.difference(&rect(0.8, 0.8, 1.2, 1.2)).unwrap();
    let frame = rect(0.0, 0.0, 4.0, 2.0);
    let site = Site {
        id: 1,
        board: 0,
        board_landing: rect(0.5, 0.5, 1.5, 1.5), // covers the board's hole
        frame_landing: rect(2.5, 0.5, 3.5, 1.5),
        reference: [2.0, 1.0],
    };
    let error = evaluate(
        &[board],
        &frame,
        &[site],
        &[],
        &[LoadCase {
            board_resultants: vec![[1.0, 0.0, 0.0]],
            compliance_limit: 1.0,
        }],
        &policy(),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("not completely covered"),
        "{error}"
    );
}
