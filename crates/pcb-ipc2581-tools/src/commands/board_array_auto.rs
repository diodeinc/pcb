use std::cmp::Ordering;

use super::board_array::BoardMarginMm;

const A7_TARGETS_MM: [TargetSizeMm; 2] = [
    TargetSizeMm {
        width: 105.0,
        height: 74.0,
    },
    TargetSizeMm {
        width: 74.0,
        height: 105.0,
    },
];
const AUTO_BOARD_MARGIN_MM: f64 = 5.0;
const AUTO_MIN_EDGE_RAIL_MM: f64 = 5.0;
const AUTO_MAX_GRID_COUNT: u32 = 10;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TargetSizeMm {
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AutoBoardArrayPlan {
    pub target: TargetSizeMm,
    pub columns: u32,
    pub rows: u32,
    pub board_margin_mm: BoardMarginMm,
    pub edge_rail_mm: BoardMarginMm,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AutoBoardArrayError {
    board_width_mm: f64,
    board_height_mm: f64,
}

impl std::fmt::Display for AutoBoardArrayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "board bbox {} x {} mm cannot fit in A7 with 5 mm board margins and 5 mm edge rails",
            fmt_num(self.board_width_mm),
            fmt_num(self.board_height_mm)
        )
    }
}

impl std::error::Error for AutoBoardArrayError {}

pub fn auto_a7_board_array_plan(
    board_width_mm: f64,
    board_height_mm: f64,
) -> Result<AutoBoardArrayPlan, AutoBoardArrayError> {
    // For each target T = (W, H), board bbox B = (w, h), board margin m,
    // and minimum rail r:
    //
    //   C = (w + 2m, h + 2m)
    //   N = (floor((W - 2r) / Cx), floor((H - 2r) / Cy)), clamped to <= 10
    //   R = ((W - Nx * Cx) / 2, (H - Ny * Cy) / 2)
    //
    // A valid plan has Nx, Ny >= 1. The final array dimensions are exactly
    // T because the leftover span is assigned back to the two edge rails.
    A7_TARGETS_MM
        .into_iter()
        .filter_map(|target| plan_for_target(target, board_width_mm, board_height_mm))
        .max_by(compare_auto_plan)
        .ok_or(AutoBoardArrayError {
            board_width_mm,
            board_height_mm,
        })
}

fn plan_for_target(
    target: TargetSizeMm,
    board_width_mm: f64,
    board_height_mm: f64,
) -> Option<AutoBoardArrayPlan> {
    if !board_width_mm.is_finite()
        || !board_height_mm.is_finite()
        || board_width_mm <= 0.0
        || board_height_mm <= 0.0
    {
        return None;
    }

    let cell_width = board_width_mm + 2.0 * AUTO_BOARD_MARGIN_MM;
    let cell_height = board_height_mm + 2.0 * AUTO_BOARD_MARGIN_MM;
    let usable_width = target.width - 2.0 * AUTO_MIN_EDGE_RAIL_MM;
    let usable_height = target.height - 2.0 * AUTO_MIN_EDGE_RAIL_MM;
    let columns = axis_count(usable_width, cell_width)?;
    let rows = axis_count(usable_height, cell_height)?;
    let rail_x = (target.width - columns as f64 * cell_width) / 2.0;
    let rail_y = (target.height - rows as f64 * cell_height) / 2.0;

    Some(AutoBoardArrayPlan {
        target,
        columns,
        rows,
        board_margin_mm: BoardMarginMm::all(AUTO_BOARD_MARGIN_MM),
        edge_rail_mm: BoardMarginMm {
            top: rail_y,
            right: rail_x,
            bottom: rail_y,
            left: rail_x,
        },
    })
}

fn axis_count(usable_span: f64, cell_span: f64) -> Option<u32> {
    if usable_span < 0.0 || cell_span <= 0.0 {
        return None;
    }

    let count = ((usable_span / cell_span).floor() as u32).min(AUTO_MAX_GRID_COUNT);
    (count >= 1).then_some(count)
}

fn compare_auto_plan(a: &AutoBoardArrayPlan, b: &AutoBoardArrayPlan) -> Ordering {
    board_count(a)
        .cmp(&board_count(b))
        .then_with(|| {
            rail_imbalance(b)
                .partial_cmp(&rail_imbalance(a))
                .unwrap_or(Ordering::Equal)
        })
        .then_with(|| orientation_priority(a).cmp(&orientation_priority(b)))
}

fn board_count(plan: &AutoBoardArrayPlan) -> u32 {
    plan.columns * plan.rows
}

fn rail_imbalance(plan: &AutoBoardArrayPlan) -> f64 {
    (plan.edge_rail_mm.right - plan.edge_rail_mm.top).abs()
}

fn orientation_priority(plan: &AutoBoardArrayPlan) -> u8 {
    if plan.target.width > plan.target.height {
        1
    } else {
        0
    }
}

fn fmt_num(value: f64) -> String {
    let text = format!("{value:.3}");
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projects_board_bbox_to_maximal_a7_grid() {
        let plan = auto_a7_board_array_plan(20.0, 10.0).unwrap();

        assert_eq!(
            plan.target,
            TargetSizeMm {
                width: 105.0,
                height: 74.0
            }
        );
        assert_eq!((plan.columns, plan.rows), (3, 3));
        assert_eq!(plan.board_margin_mm, BoardMarginMm::all(5.0));
        assert_close(plan.edge_rail_mm.left, 7.5);
        assert_close(plan.edge_rail_mm.right, 7.5);
        assert_close(plan.edge_rail_mm.bottom, 7.0);
        assert_close(plan.edge_rail_mm.top, 7.0);
        assert_close(finished_width(20.0, &plan), plan.target.width);
        assert_close(finished_height(10.0, &plan), plan.target.height);
    }

    #[test]
    fn chooses_rotated_a7_when_it_fits_more_boards() {
        let plan = auto_a7_board_array_plan(40.0, 20.0).unwrap();

        assert_eq!(
            plan.target,
            TargetSizeMm {
                width: 74.0,
                height: 105.0
            }
        );
        assert_eq!((plan.columns, plan.rows), (1, 3));
        assert_close(finished_width(40.0, &plan), 74.0);
        assert_close(finished_height(20.0, &plan), 105.0);
    }

    #[test]
    fn rejects_board_that_cannot_fit_a7() {
        let error = auto_a7_board_array_plan(100.0, 80.0).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("cannot fit in A7 with 5 mm board margins and 5 mm edge rails")
        );
    }

    #[test]
    fn keeps_grid_axes_within_limit() {
        let plan = auto_a7_board_array_plan(1.0, 1.0).unwrap();
        assert!(plan.columns <= AUTO_MAX_GRID_COUNT);
        assert!(plan.rows <= AUTO_MAX_GRID_COUNT);
        assert_close(finished_width(1.0, &plan), plan.target.width);
        assert_close(finished_height(1.0, &plan), plan.target.height);
    }

    fn finished_width(board_width: f64, plan: &AutoBoardArrayPlan) -> f64 {
        let cell_width = board_width + 2.0 * AUTO_BOARD_MARGIN_MM;
        plan.columns as f64 * cell_width + plan.edge_rail_mm.left + plan.edge_rail_mm.right
    }

    fn finished_height(board_height: f64, plan: &AutoBoardArrayPlan) -> f64 {
        let cell_height = board_height + 2.0 * AUTO_BOARD_MARGIN_MM;
        plan.rows as f64 * cell_height + plan.edge_rail_mm.bottom + plan.edge_rail_mm.top
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-9,
            "expected {expected}, got {actual}"
        );
    }
}
