//! How a board packs into each automatic board array sheet, and which sheet
//! an order of a given quantity should use. Analysis only: no array is made.

use anyhow::{Context, Result};
use ipc2581::Ipc2581;
use pcb_ir::geom::Resolution;
use serde::Serialize;

use super::{BoardMarginMm, auto_board_margin_for, eligibility, primary_board_layout};
use crate::commands::board_array_auto::{
    AUTO_SHEETS, AutoBoardArrayPlan, SheetOrder, auto_board_array_plan_for_sheet, recommend_sheet,
    sheet_order,
};

/// Boards wanted, and the fewest arrays the fabricator accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct OrderRequest {
    pub quantity: u32,
    pub moq: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanReport {
    pub board: PlanBoard,
    /// Every automatic sheet, smallest first.
    pub sheets: Vec<SheetRow>,
    pub order: Option<OrderRequest>,
    /// The sheet to pass to `board-array create --sheet`; set only for an
    /// order that some sheet fits.
    pub recommended: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanBoard {
    pub width_mm: f64,
    pub height_mm: f64,
    /// Board material: the outline less its cutouts.
    pub outline_area_mm2: f64,
    pub margin_mm: Margin,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Margin {
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    pub left: f64,
}

impl From<BoardMarginMm> for Margin {
    fn from(m: BoardMarginMm) -> Self {
        Self {
            top: m.top,
            right: m.right,
            bottom: m.bottom,
            left: m.left,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SheetRow {
    pub sheet: &'static str,
    pub fits: bool,
    /// The array as `create --sheet` makes it, in its chosen orientation.
    pub width_mm: f64,
    pub height_mm: f64,
    pub columns: u32,
    pub rows: u32,
    pub boards: u32,
    /// Board material over array area.
    pub utilization: Option<f64>,
    pub area_per_board_mm2: Option<f64>,
    /// The per-axis grid limit, not the sheet, held the count down.
    pub grid_limited: bool,
    pub edge_rail_mm: Option<Margin>,
    pub order: Option<SheetOrderRow>,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct SheetOrderRow {
    pub arrays: u32,
    pub boards_ordered: u32,
    pub surplus: u32,
    pub ordered_area_mm2: f64,
}

impl From<SheetOrder> for SheetOrderRow {
    fn from(o: SheetOrder) -> Self {
        Self {
            arrays: o.arrays,
            boards_ordered: o.boards_ordered,
            surplus: o.surplus,
            ordered_area_mm2: o.ordered_area_mm2,
        }
    }
}

pub fn plan(xml: &str, order: Option<OrderRequest>, resolution: Resolution) -> Result<PlanReport> {
    let ipc = Ipc2581::parse(xml).context("Failed to parse IPC-2581 input")?;
    let board = primary_board_layout(&ipc)?;
    let imported = pcb_ir::import::ipc2581::import_design(&ipc, resolution)?;
    let margin = auto_board_margin_for(&ipc, &imported, board.bbox)?;
    let outline_area = eligibility::board_substrate(&imported, resolution)?.area();
    let (width, height) = (board.bbox.width(), board.bbox.height());

    let plans: Vec<AutoBoardArrayPlan> = AUTO_SHEETS
        .into_iter()
        .filter_map(|sheet| auto_board_array_plan_for_sheet(width, height, margin, sheet).ok())
        .collect();
    let sheets = AUTO_SHEETS
        .into_iter()
        .map(
            |sheet| match plans.iter().find(|plan| plan.sheet == sheet) {
                Some(plan) => SheetRow {
                    sheet: sheet.name(),
                    fits: true,
                    width_mm: plan.target.width,
                    height_mm: plan.target.height,
                    columns: plan.columns,
                    rows: plan.rows,
                    boards: plan.boards(),
                    utilization: Some(outline_area * plan.boards() as f64 / plan.area_mm2()),
                    area_per_board_mm2: Some(plan.area_mm2() / plan.boards() as f64),
                    grid_limited: plan.grid_limited,
                    edge_rail_mm: Some(plan.edge_rail_mm.into()),
                    order: order.map(|o| sheet_order(plan, o.quantity, o.moq).into()),
                },
                None => {
                    let (short, long) = sheet.dimensions_mm();
                    SheetRow {
                        sheet: sheet.name(),
                        fits: false,
                        width_mm: long,
                        height_mm: short,
                        columns: 0,
                        rows: 0,
                        boards: 0,
                        utilization: None,
                        area_per_board_mm2: None,
                        grid_limited: false,
                        edge_rail_mm: None,
                        order: None,
                    }
                }
            },
        )
        .collect();
    let recommended = order
        .and_then(|o| recommend_sheet(&plans, o.quantity, o.moq))
        .map(|plan| plan.sheet.name());

    Ok(PlanReport {
        board: PlanBoard {
            width_mm: width,
            height_mm: height,
            outline_area_mm2: outline_area,
            margin_mm: margin.into(),
        },
        sheets,
        order,
        recommended,
    })
}

#[cfg(feature = "cli")]
pub fn execute(
    input: &std::path::Path,
    format: crate::OutputFormat,
    order: Option<OrderRequest>,
    resolution: Resolution,
) -> Result<()> {
    let xml = crate::utils::file::load_ipc_file(input)?;
    let report = plan(&xml, order, resolution)?;
    match format {
        crate::OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        crate::OutputFormat::Text => print_text(&report),
    }
    Ok(())
}

#[cfg(feature = "cli")]
fn print_text(report: &PlanReport) {
    use crate::utils::format::fmt_num;
    use colored::Colorize;
    use comfy_table::{Cell, CellAlignment, Color, Table, presets::UTF8_FULL_CONDENSED};

    let board = &report.board;
    let m = board.margin_mm;
    println!(
        "{} {} × {} mm, outline {} mm², margin {} / {} / {} / {} mm",
        "Board".bold(),
        fmt_num(board.width_mm),
        fmt_num(board.height_mm),
        fmt_num(board.outline_area_mm2.round()),
        fmt_num(m.top),
        fmt_num(m.right),
        fmt_num(m.bottom),
        fmt_num(m.left),
    );
    if let Some(order) = report.order {
        let moq = match order.moq {
            1 => String::new(),
            moq => format!(", minimum {moq} arrays"),
        };
        println!("{} {} boards{moq}", "Order".bold(), order.quantity);
    }

    let mut table = Table::new();
    table.load_style(UTF8_FULL_CONDENSED);
    let mut header = vec![
        "Sheet",
        "Array mm",
        "Grid",
        "Boards",
        "Utilization",
        "mm²/board",
    ];
    if report.order.is_some() {
        header.extend(["Arrays", "Ordered", "Surplus", "Ordered mm²"]);
    }
    header.push("Notes");
    table.set_header(header);

    let right = |text: String| Cell::new(text).set_alignment(CellAlignment::Right);
    for row in &report.sheets {
        let picked = report.recommended == Some(row.sheet);
        let mut sheet = Cell::new(row.sheet);
        if picked {
            sheet = sheet.fg(Color::Green);
        }
        let mut cells = vec![
            sheet,
            Cell::new(format!(
                "{}×{}",
                fmt_num(row.width_mm),
                fmt_num(row.height_mm)
            )),
        ];
        if row.fits {
            cells.extend([
                Cell::new(format!("{}×{}", row.columns, row.rows)),
                right(row.boards.to_string()),
                right(format!("{:.1}%", row.utilization.unwrap_or(0.0) * 100.0)),
                right(format!("{:.0}", row.area_per_board_mm2.unwrap_or(0.0))),
            ]);
        } else {
            cells.extend(["–", "0", "–", "–"].map(right_str));
        }
        if report.order.is_some() {
            match row.order {
                Some(o) => cells.extend([
                    right(o.arrays.to_string()),
                    right(o.boards_ordered.to_string()),
                    right(o.surplus.to_string()),
                    right(format!("{:.0}", o.ordered_area_mm2)),
                ]),
                None => cells.extend(["–", "–", "–", "–"].map(right_str)),
            }
        }
        let notes = [
            (!row.fits).then_some("does not fit"),
            row.grid_limited.then_some("grid limit"),
            picked.then_some("recommended"),
        ];
        cells.push(Cell::new(
            notes.into_iter().flatten().collect::<Vec<_>>().join(", "),
        ));
        table.add_row(cells);
    }
    println!("{table}");

    if report.sheets.iter().all(|row| !row.fits) {
        println!(
            "No sheet fits the board; `board-array create --auto` makes a single-board array."
        );
    } else if let Some(sheet) = report.recommended {
        println!(
            "Recommended: {sheet} (board-array create --sheet {})",
            sheet.to_lowercase()
        );
    }
}

#[cfg(feature = "cli")]
fn right_str(text: &str) -> comfy_table::Cell {
    comfy_table::Cell::new(text).set_alignment(comfy_table::CellAlignment::Right)
}
