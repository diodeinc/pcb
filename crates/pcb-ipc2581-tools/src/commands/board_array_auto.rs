use std::cmp::Ordering;

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

#[cfg_attr(feature = "cli", derive(clap::ValueEnum))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    AUTO_SHEETS
        .into_iter()
        .find_map(|sheet| plan_for_sheet(sheet, board_width_mm, board_height_mm, board_margin_mm))
        .ok_or(AutoBoardArrayError {
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
        || !board_margin_mm
            .sides()
            .iter()
            .all(|(_, value)| value.is_finite() && *value >= 0.0)
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
    margin
        .sides()
        .map(|(side, value)| format!("{} {side}", fmt_num(value)))
        .join(" / ")
}

#[cfg(test)]
mod tests {
    use super::AutoSheetSize::{A4, A5, A6, A7};
    use super::*;

    const MARGIN: BoardMarginMm = BoardMarginMm::all(5.0);

    /// The leftover span goes to the rails, so the array is exactly its sheet.
    fn assert_fills_target(board: (f64, f64), plan: &AutoBoardArrayPlan) {
        let (margin, rail) = (plan.board_margin_mm, plan.edge_rail_mm);
        let width = plan.columns as f64 * (board.0 + margin.left + margin.right);
        let height = plan.rows as f64 * (board.1 + margin.bottom + margin.top);
        assert!((width + rail.left + rail.right - plan.target.width).abs() < 1e-9);
        assert!((height + rail.bottom + rail.top - plan.target.height).abs() < 1e-9);
    }

    #[test]
    fn picks_the_smallest_sheet_and_the_orientation_that_fits_most_boards() {
        for (board, sheet, target, grid) in [
            ((20.0, 10.0), A7, (105.0, 74.0), (3, 3)),
            // Rotated, one column of three beats one row of two.
            ((40.0, 20.0), A7, (74.0, 105.0), (1, 3)),
            ((70.0, 58.0), A6, (105.0, 148.0), (1, 2)),
            ((120.0, 90.0), A5, (148.0, 210.0), (1, 2)),
            ((190.0, 250.0), A4, (210.0, 297.0), (1, 1)),
        ] {
            let plan = auto_board_array_plan(board.0, board.1, MARGIN).unwrap();
            assert_eq!(plan.sheet, sheet);
            assert_eq!((plan.target.width, plan.target.height), target);
            assert_eq!((plan.columns, plan.rows), grid);
            assert_eq!(plan.board_margin_mm, MARGIN);
            assert_fills_target(board, &plan);
        }

        let error = auto_board_array_plan(278.0, 278.0, MARGIN).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("cannot fit in A4 with board margins 5 top / 5 right / 5 bottom / 5 left and 5 mm edge rails")
        );
    }

    #[test]
    fn a_requested_sheet_is_filled_up_to_the_grid_limit() {
        let plan = auto_board_array_plan_for_sheet(20.0, 10.0, MARGIN, A5).unwrap();
        assert_eq!((plan.sheet, plan.columns, plan.rows), (A5, 4, 10));
        assert_eq!((plan.target.width, plan.target.height), (148.0, 210.0));
        assert_fills_target((20.0, 10.0), &plan);

        let plan = auto_board_array_plan_for_sheet(1.0, 1.0, MARGIN, A4).unwrap();
        assert_eq!(
            (plan.columns, plan.rows),
            (AUTO_MAX_GRID_COUNT, AUTO_MAX_GRID_COUNT)
        );
        assert_fills_target((1.0, 1.0), &plan);
    }

    #[test]
    fn uses_asymmetric_board_margins_as_cell_size() {
        let margin = BoardMarginMm::new(6.0, 7.0, 8.0, 9.0);
        let plan = auto_board_array_plan(20.0, 10.0, margin).unwrap();

        assert_eq!(plan.board_margin_mm, margin);
        assert_fills_target((20.0, 10.0), &plan);
    }
}
