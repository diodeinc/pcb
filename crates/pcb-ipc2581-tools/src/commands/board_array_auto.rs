use std::cmp::Ordering;

use super::board_array::BoardMarginMm;
use crate::utils::format::fmt_num;

pub const AUTO_SHEETS: [AutoSheetSize; 5] = [
    AutoSheetSize::A7,
    AutoSheetSize::A6,
    AutoSheetSize::A5,
    AutoSheetSize::A4,
    AutoSheetSize::A3,
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
    A3,
}

impl AutoSheetSize {
    pub fn name(self) -> &'static str {
        match self {
            Self::A7 => "A7",
            Self::A6 => "A6",
            Self::A5 => "A5",
            Self::A4 => "A4",
            Self::A3 => "A3",
        }
    }

    fn dimensions_mm(self) -> (f64, f64) {
        match self {
            Self::A7 => (74.0, 105.0),
            Self::A6 => (105.0, 148.0),
            Self::A5 => (148.0, 210.0),
            Self::A4 => (210.0, 297.0),
            Self::A3 => (297.0, 420.0),
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
    /// The per-axis grid limit, not the sheet, stopped a larger grid.
    pub grid_limited: bool,
}

impl AutoBoardArrayPlan {
    pub fn boards(&self) -> u32 {
        self.columns * self.rows
    }

    pub fn area_mm2(&self) -> f64 {
        self.target.width * self.target.height
    }
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
    //   sheets = [A7, A6, A5, A4, A3]
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
            sheet: AutoSheetSize::A3,
        })
}

/// Boards wanted, and the fewest arrays the fabricator accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Order {
    pub quantity: u32,
    pub moq: u32,
}

/// What an [`Order`] takes on one sheet.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct SheetOrder {
    pub arrays: u64,
    pub boards_ordered: u64,
    pub surplus: u64,
    pub ordered_area_mm2: f64,
}

impl Order {
    pub fn on(self, plan: &AutoBoardArrayPlan) -> SheetOrder {
        // Counts widen to u64: up to 100 boards an array, times any u32
        // quantity or MOQ.
        let (quantity, boards) = (u64::from(self.quantity), u64::from(plan.boards()));
        let arrays = quantity.div_ceil(boards).max(self.moq.into());
        let boards_ordered = arrays * boards;
        SheetOrder {
            arrays,
            boards_ordered,
            surplus: boards_ordered - quantity,
            ordered_area_mm2: arrays as f64 * plan.area_mm2(),
        }
    }

    /// The plan that orders the least array area. `min_by` keeps the first of
    /// equal plans, so with plans in ascending sheet size a larger sheet wins
    /// only by packing better.
    pub fn recommend(self, plans: &[AutoBoardArrayPlan]) -> Option<&AutoBoardArrayPlan> {
        plans.iter().min_by(|a, b| {
            self.on(a)
                .ordered_area_mm2
                .total_cmp(&self.on(b).ordered_area_mm2)
        })
    }
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

pub fn plan_for_sheet(
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
    let (columns, columns_limited) = axis_count(usable_width, cell_width)?;
    let (rows, rows_limited) = axis_count(usable_height, cell_height)?;
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
        grid_limited: columns_limited || rows_limited,
    })
}

/// How many cells fit along an axis, and whether the grid limit cut it.
fn axis_count(usable_span: f64, cell_span: f64) -> Option<(u32, bool)> {
    if usable_span < 0.0 || cell_span <= 0.0 {
        return None;
    }

    let fit = (usable_span / cell_span).floor() as u32;
    let count = fit.min(AUTO_MAX_GRID_COUNT);
    (count >= 1).then_some((count, fit > count))
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
    use super::AutoSheetSize::{A3, A4, A5, A6, A7};
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
            ((190.0, 330.0), A3, (297.0, 420.0), (1, 1)),
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
                .contains("cannot fit in A3 with board margins 5 top / 5 right / 5 bottom / 5 left and 5 mm edge rails")
        );
    }

    #[test]
    fn a_requested_sheet_is_filled_up_to_the_grid_limit() {
        let plan = auto_board_array_plan_for_sheet(20.0, 10.0, MARGIN, A5).unwrap();
        assert_eq!((plan.sheet, plan.columns, plan.rows), (A5, 4, 10));
        // Ten rows fill the sheet exactly: the limit cut nothing.
        assert!(!plan.grid_limited);
        assert_eq!((plan.target.width, plan.target.height), (148.0, 210.0));
        assert_fills_target((20.0, 10.0), &plan);

        let plan = auto_board_array_plan_for_sheet(1.0, 1.0, MARGIN, A4).unwrap();
        assert_eq!(
            (plan.columns, plan.rows),
            (AUTO_MAX_GRID_COUNT, AUTO_MAX_GRID_COUNT)
        );
        assert!(plan.grid_limited);
        assert_fills_target((1.0, 1.0), &plan);
    }

    #[test]
    fn recommends_the_sheet_that_orders_the_least_array_area() {
        // 50 x 40 mm cells: A7 holds 2, A6 4, A5 12, A4 28, A3 56. A7 and A6
        // pack equally well, so A6 never wins; A3 only ties A4.
        let plans: Vec<_> = AUTO_SHEETS
            .into_iter()
            .map(|sheet| auto_board_array_plan_for_sheet(40.0, 30.0, MARGIN, sheet).unwrap())
            .collect();
        assert_eq!(
            plans.iter().map(|p| p.boards()).collect::<Vec<_>>(),
            [2, 4, 12, 28, 56]
        );

        let order = |quantity, moq| Order { quantity, moq };
        for (quantity, moq, sheet) in [
            // The minimum order fills more than enough small arrays.
            (5, 5, A7),
            (4, 1, A7),
            (50, 5, A5),
            (500, 1, A4),
        ] {
            let pick = order(quantity, moq).recommend(&plans).unwrap();
            assert_eq!(pick.sheet, sheet, "quantity {quantity}, moq {moq}");
        }

        let a5 = order(50, 5).on(&plans[2]);
        assert_eq!((a5.arrays, a5.boards_ordered, a5.surplus), (5, 60, 10));
        assert_eq!(a5.ordered_area_mm2, 5.0 * 148.0 * 210.0);

        // Neither the largest quantity nor the largest MOQ overflows.
        let a7 = order(u32::MAX, 1).on(&plans[0]);
        assert_eq!((a7.arrays, a7.surplus), (1 << 31, 1));
        let a4 = order(1, u32::MAX).on(&plans[3]);
        assert_eq!(a4.boards_ordered, u64::from(u32::MAX) * 28);
    }

    #[test]
    fn uses_asymmetric_board_margins_as_cell_size() {
        let margin = BoardMarginMm::new(6.0, 7.0, 8.0, 9.0);
        let plan = auto_board_array_plan(20.0, 10.0, margin).unwrap();

        assert_eq!(plan.board_margin_mm, margin);
        assert_fills_target((20.0, 10.0), &plan);
    }
}
