//! How a board packs into each automatic board array sheet, and which sheet
//! an order of a given quantity should use. Analysis only: no array is made.

use anyhow::{Context, Result};
use ipc2581::Ipc2581;
use pcb_ir::geom::Resolution;
use serde::Serialize;

use super::{BoardMarginMm, auto_board_margin, eligibility, import_design, primary_board_layout};
use crate::commands::board_array_auto::{AUTO_SHEETS, Order, SheetOrder, plan_for_sheet};

#[derive(Debug, Clone, Serialize)]
pub struct PlanReport {
    pub board: PlanBoard,
    /// The sheets the board fits, smallest first.
    pub sheets: Vec<SheetRow>,
    pub order: Option<Order>,
    /// The sheet to pass to `board-array create --sheet`.
    pub recommended: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanBoard {
    pub width_mm: f64,
    pub height_mm: f64,
    /// Board material: the outline less its cutouts.
    pub outline_area_mm2: f64,
    pub margin_mm: BoardMarginMm,
}

#[derive(Debug, Clone, Serialize)]
pub struct SheetRow {
    pub sheet: &'static str,
    /// The array as `create --sheet` makes it, in its chosen orientation.
    pub width_mm: f64,
    pub height_mm: f64,
    pub columns: u32,
    pub rows: u32,
    pub boards: u32,
    /// Board material over array area.
    pub utilization: f64,
    pub area_per_board_mm2: f64,
    /// The per-axis grid limit, not the sheet, held the count down.
    pub grid_limited: bool,
    pub edge_rail_mm: BoardMarginMm,
    pub order: Option<SheetOrder>,
}

pub fn plan(xml: &str, order: Option<Order>, resolution: Resolution) -> Result<PlanReport> {
    let ipc = Ipc2581::parse(xml).context("Failed to parse IPC-2581 input")?;
    let board = primary_board_layout(&ipc)?;
    let imported = import_design(&ipc, resolution)?;
    let margin = auto_board_margin(&ipc, &imported, board.bbox)?;
    let outline_area = eligibility::board_substrate(&imported, resolution)?.area();
    let (width, height) = (board.bbox.width(), board.bbox.height());

    let plans: Vec<_> = AUTO_SHEETS
        .into_iter()
        .filter_map(|sheet| plan_for_sheet(sheet, width, height, margin))
        .collect();
    let sheets = plans
        .iter()
        .map(|plan| SheetRow {
            sheet: plan.sheet.name(),
            width_mm: plan.target.width,
            height_mm: plan.target.height,
            columns: plan.columns,
            rows: plan.rows,
            boards: plan.boards(),
            utilization: outline_area * plan.boards() as f64 / plan.area_mm2(),
            area_per_board_mm2: plan.area_mm2() / plan.boards() as f64,
            grid_limited: plan.grid_limited,
            edge_rail_mm: plan.edge_rail_mm,
            order: order.map(|order| order.on(plan)),
        })
        .collect();

    Ok(PlanReport {
        board: PlanBoard {
            width_mm: width,
            height_mm: height,
            outline_area_mm2: outline_area,
            margin_mm: margin,
        },
        sheets,
        order,
        recommended: order
            .and_then(|order| order.recommend(&plans))
            .map(|plan| plan.sheet.name()),
    })
}

#[cfg(feature = "cli")]
pub fn execute(
    input: &std::path::Path,
    format: crate::OutputFormat,
    order: Option<Order>,
    resolution: Resolution,
) -> Result<()> {
    let xml = crate::utils::file::load_ipc_file(input)?;
    let report = plan(&xml, order, resolution)?;
    match format {
        crate::OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&report)?),
        crate::OutputFormat::Text => print_text(&report),
    }
    Ok(())
}

#[cfg(feature = "cli")]
fn print_text(report: &PlanReport) {
    use crate::utils::format::fmt_num;
    use colored::Colorize;
    use comfy_table::{Cell, CellAlignment, Color, Table, presets::UTF8_FULL_CONDENSED};

    fn right(value: impl std::fmt::Display) -> Cell {
        Cell::new(value).set_alignment(CellAlignment::Right)
    }

    let board = &report.board;
    let margin = board
        .margin_mm
        .sides()
        .map(|(_, mm)| fmt_num(mm))
        .join(" / ");
    println!(
        "{} {} × {} mm, outline {} mm², margin {margin} mm",
        "Board".bold(),
        fmt_num(board.width_mm),
        fmt_num(board.height_mm),
        fmt_num(board.outline_area_mm2.round()),
    );
    if report.sheets.is_empty() {
        println!(
            "No sheet fits the board; `board-array create --auto` makes a single-board array."
        );
        return;
    }
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
            Cell::new(format!("{}×{}", row.columns, row.rows)),
            right(row.boards),
            right(format!("{:.1}%", row.utilization * 100.0)),
            right(format!("{:.0}", row.area_per_board_mm2)),
        ];
        if let Some(order) = row.order {
            cells.extend([
                right(order.arrays),
                right(order.boards_ordered),
                right(order.surplus),
                right(format!("{:.0}", order.ordered_area_mm2)),
            ]);
        }
        let notes = [
            row.grid_limited.then_some("grid limit"),
            picked.then_some("recommended"),
        ];
        cells.push(Cell::new(
            notes.into_iter().flatten().collect::<Vec<_>>().join(", "),
        ));
        table.add_row(cells);
    }
    println!("{table}");

    if let Some(sheet) = report.recommended {
        println!(
            "Recommended: {sheet} (board-array create --sheet {})",
            sheet.to_lowercase()
        );
    }
}
