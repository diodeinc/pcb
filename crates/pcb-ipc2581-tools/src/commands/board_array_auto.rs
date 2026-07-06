use std::cmp::Ordering;

use clap::ValueEnum;

use super::board_array::BoardMarginMm;
use crate::utils::format::fmt_num;

const AUTO_SHEETS: [AutoSheetSize; 4] = [
    AutoSheetSize::A7,
    AutoSheetSize::A6,
    AutoSheetSize::A5,
    AutoSheetSize::A4,
];
const AUTO_MIN_EDGE_RAIL_MM: f64 = 5.0;
const AUTO_MAX_GRID_COUNT: u32 = 10;

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoSheetSize {
    A7,
    A6,
    A5,
    A4,
}

impl AutoSheetSize {
    pub fn name(self) -> &'static str {
        match self {
            Self::A7 => "A7",
            Self::A6 => "A6",
            Self::A5 => "A5",
            Self::A4 => "A4",
        }
    }

    fn dimensions_mm(self) -> (f64, f64) {
        match self {
            Self::A7 => (74.0, 105.0),
            Self::A6 => (105.0, 148.0),
            Self::A5 => (148.0, 210.0),
            Self::A4 => (210.0, 297.0),
        }
    }

    fn targets_mm(self) -> [TargetSizeMm; 2] {
        let (short, long) = self.dimensions_mm();
        [
            TargetSizeMm {
                width: long,
                height: short,
            },
            TargetSizeMm {
                width: short,
                height: long,
            },
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TargetSizeMm {
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AutoBoardArrayPlan {
    pub sheet: AutoSheetSize,
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
    board_margin_mm: BoardMarginMm,
    sheet: AutoSheetSize,
}

impl std::fmt::Display for AutoBoardArrayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "board bbox {} x {} mm cannot fit in {} with board margins {} and 5 mm edge rails",
            fmt_num(self.board_width_mm),
            fmt_num(self.board_height_mm),
            self.sheet.name(),
            fmt_margin(self.board_margin_mm)
        )
    }
}

impl std::error::Error for AutoBoardArrayError {}

pub fn auto_board_array_plan(
    board_width_mm: f64,
    board_height_mm: f64,
    board_margin_mm: BoardMarginMm,
) -> Result<AutoBoardArrayPlan, AutoBoardArrayError> {
    // Try sheets in ascending A-series size and keep the first sheet that fits:
    //
    //   sheets = [A7, A6, A5, A4]
    //   targets(sheet) = [(long, short), (short, long)]
    //
    // For each target T = (W, H), board bbox B = (w, h), side margins
    // M = (mt, mr, mb, ml), and minimum rail r:
    //
    //   C = (w + ml + mr, h + mb + mt)
    //   N = (floor((W - 2r) / Cx), floor((H - 2r) / Cy)), clamped to <= 10
    //   R = ((W - Nx * Cx) / 2, (H - Ny * Cy) / 2)
    //
    // A valid plan has Nx, Ny >= 1. The final array dimensions are exactly
    // T because the leftover span is assigned back to the two edge rails.
    for sheet in AUTO_SHEETS {
        if let Some(plan) = plan_for_sheet(sheet, board_width_mm, board_height_mm, board_margin_mm)
        {
            return Ok(plan);
        }
    }

    Err(AutoBoardArrayError {
        board_width_mm,
        board_height_mm,
        board_margin_mm,
        sheet: AutoSheetSize::A4,
    })
}

pub fn auto_board_array_plan_for_sheet(
    board_width_mm: f64,
    board_height_mm: f64,
    board_margin_mm: BoardMarginMm,
    sheet: AutoSheetSize,
) -> Result<AutoBoardArrayPlan, AutoBoardArrayError> {
    plan_for_sheet(sheet, board_width_mm, board_height_mm, board_margin_mm).ok_or(
        AutoBoardArrayError {
            board_width_mm,
            board_height_mm,
            board_margin_mm,
            sheet,
        },
    )
}

fn plan_for_sheet(
    sheet: AutoSheetSize,
    board_width_mm: f64,
    board_height_mm: f64,
    board_margin_mm: BoardMarginMm,
) -> Option<AutoBoardArrayPlan> {
    sheet
        .targets_mm()
        .into_iter()
        .filter_map(|target| {
            plan_for_target(
                sheet,
                target,
                board_width_mm,
                board_height_mm,
                board_margin_mm,
            )
        })
        .max_by(compare_auto_plan)
}

fn plan_for_target(
    sheet: AutoSheetSize,
    target: TargetSizeMm,
    board_width_mm: f64,
    board_height_mm: f64,
    board_margin_mm: BoardMarginMm,
) -> Option<AutoBoardArrayPlan> {
    if !board_width_mm.is_finite()
        || !board_height_mm.is_finite()
        || board_width_mm <= 0.0
        || board_height_mm <= 0.0
        || !valid_margin(board_margin_mm)
    {
        return None;
    }

    let cell_width = board_width_mm + board_margin_mm.left + board_margin_mm.right;
    let cell_height = board_height_mm + board_margin_mm.bottom + board_margin_mm.top;
    let usable_width = target.width - 2.0 * AUTO_MIN_EDGE_RAIL_MM;
    let usable_height = target.height - 2.0 * AUTO_MIN_EDGE_RAIL_MM;
    let columns = axis_count(usable_width, cell_width)?;
    let rows = axis_count(usable_height, cell_height)?;
    let rail_x = (target.width - columns as f64 * cell_width) / 2.0;
    let rail_y = (target.height - rows as f64 * cell_height) / 2.0;

    Some(AutoBoardArrayPlan {
        sheet,
        target,
        columns,
        rows,
        board_margin_mm,
        edge_rail_mm: BoardMarginMm {
            top: rail_y,
            right: rail_x,
            bottom: rail_y,
            left: rail_x,
        },
    })
}

fn valid_margin(margin: BoardMarginMm) -> bool {
    [margin.top, margin.right, margin.bottom, margin.left]
        .into_iter()
        .all(|value| value.is_finite() && value >= 0.0)
}

fn axis_count(usable_span: f64, cell_span: f64) -> Option<u32> {
    if usable_span < 0.0 || cell_span <= 0.0 {
        return None;
    }

    let count = ((usable_span / cell_span).floor() as u32).min(AUTO_MAX_GRID_COUNT);
    (count >= 1).then_some(count)
}

/// Rank plans by board count, then by more balanced edge rails, then by
/// preferring a landscape sheet.
fn compare_auto_plan(a: &AutoBoardArrayPlan, b: &AutoBoardArrayPlan) -> Ordering {
    let count = |plan: &AutoBoardArrayPlan| plan.columns * plan.rows;
    let imbalance =
        |plan: &AutoBoardArrayPlan| (plan.edge_rail_mm.right - plan.edge_rail_mm.top).abs();
    let landscape = |plan: &AutoBoardArrayPlan| plan.target.width > plan.target.height;

    count(a)
        .cmp(&count(b))
        .then_with(|| imbalance(b).total_cmp(&imbalance(a)))
        .then_with(|| landscape(a).cmp(&landscape(b)))
}

fn fmt_margin(margin: BoardMarginMm) -> String {
    format!(
        "{} top / {} right / {} bottom / {} left",
        fmt_num(margin.top),
        fmt_num(margin.right),
        fmt_num(margin.bottom),
        fmt_num(margin.left)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const AUTO_BOARD_MARGIN_MM: f64 = 5.0;

    fn auto_plan(board_width_mm: f64, board_height_mm: f64) -> AutoBoardArrayPlan {
        auto_board_array_plan(
            board_width_mm,
            board_height_mm,
            BoardMarginMm::all(AUTO_BOARD_MARGIN_MM),
        )
        .unwrap()
    }

    #[test]
    fn projects_board_bbox_to_maximal_a7_grid() {
        let plan = auto_plan(20.0, 10.0);

        assert_eq!(plan.sheet, AutoSheetSize::A7);
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
    fn projects_board_bbox_to_requested_sheet() {
        let plan = auto_board_array_plan_for_sheet(
            20.0,
            10.0,
            BoardMarginMm::all(AUTO_BOARD_MARGIN_MM),
            AutoSheetSize::A5,
        )
        .unwrap();

        assert_eq!(plan.sheet, AutoSheetSize::A5);
        assert_eq!(
            plan.target,
            TargetSizeMm {
                width: 148.0,
                height: 210.0
            }
        );
        assert_eq!((plan.columns, plan.rows), (4, 10));
        assert_close(finished_width(20.0, &plan), plan.target.width);
        assert_close(finished_height(10.0, &plan), plan.target.height);
    }

    #[test]
    fn chooses_rotated_a7_when_it_fits_more_boards() {
        let plan = auto_plan(40.0, 20.0);

        assert_eq!(plan.sheet, AutoSheetSize::A7);
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
    fn promotes_to_a6_when_board_cannot_fit_a7() {
        let plan = auto_plan(70.0, 58.0);

        assert_eq!(plan.sheet, AutoSheetSize::A6);
        assert_eq!(
            plan.target,
            TargetSizeMm {
                width: 105.0,
                height: 148.0
            }
        );
        assert_eq!((plan.columns, plan.rows), (1, 2));
        assert_close(finished_width(70.0, &plan), 105.0);
        assert_close(finished_height(58.0, &plan), 148.0);
    }

    #[test]
    fn promotes_to_a5_when_board_cannot_fit_a6() {
        let plan = auto_plan(120.0, 90.0);

        assert_eq!(plan.sheet, AutoSheetSize::A5);
        assert_eq!(
            plan.target,
            TargetSizeMm {
                width: 148.0,
                height: 210.0
            }
        );
        assert_eq!((plan.columns, plan.rows), (1, 2));
        assert_close(finished_width(120.0, &plan), 148.0);
        assert_close(finished_height(90.0, &plan), 210.0);
    }

    #[test]
    fn promotes_to_a4_when_board_cannot_fit_a5() {
        let plan = auto_plan(190.0, 250.0);

        assert_eq!(plan.sheet, AutoSheetSize::A4);
        assert_eq!(
            plan.target,
            TargetSizeMm {
                width: 210.0,
                height: 297.0
            }
        );
        assert_eq!((plan.columns, plan.rows), (1, 1));
        assert_close(finished_width(190.0, &plan), 210.0);
        assert_close(finished_height(250.0, &plan), 297.0);
    }

    #[test]
    fn rejects_board_that_cannot_fit_a4() {
        let error = auto_board_array_plan(278.0, 278.0, BoardMarginMm::all(AUTO_BOARD_MARGIN_MM))
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("cannot fit in A4 with board margins 5 top / 5 right / 5 bottom / 5 left and 5 mm edge rails")
        );
    }

    #[test]
    fn keeps_grid_axes_within_limit() {
        let plan = auto_plan(1.0, 1.0);
        assert!(plan.columns <= AUTO_MAX_GRID_COUNT);
        assert!(plan.rows <= AUTO_MAX_GRID_COUNT);
        assert_close(finished_width(1.0, &plan), plan.target.width);
        assert_close(finished_height(1.0, &plan), plan.target.height);
    }

    #[test]
    fn uses_asymmetric_board_margins_as_cell_size() {
        let margin = BoardMarginMm {
            top: 6.0,
            right: 7.0,
            bottom: 8.0,
            left: 9.0,
        };
        let plan = auto_board_array_plan(20.0, 10.0, margin).unwrap();

        assert_eq!(plan.board_margin_mm, margin);
        assert_close(finished_width(20.0, &plan), plan.target.width);
        assert_close(finished_height(10.0, &plan), plan.target.height);
    }

    fn finished_width(board_width: f64, plan: &AutoBoardArrayPlan) -> f64 {
        let cell_width = board_width + plan.board_margin_mm.left + plan.board_margin_mm.right;
        plan.columns as f64 * cell_width + plan.edge_rail_mm.left + plan.edge_rail_mm.right
    }

    fn finished_height(board_height: f64, plan: &AutoBoardArrayPlan) -> f64 {
        let cell_height = board_height + plan.board_margin_mm.bottom + plan.board_margin_mm.top;
        plan.rows as f64 * cell_height + plan.edge_rail_mm.bottom + plan.edge_rail_mm.top
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-9,
            "expected {expected}, got {actual}"
        );
    }
}
