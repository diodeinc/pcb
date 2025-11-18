use std::path::Path;

use anyhow::Result;
use colored::Colorize;
use comfy_table::presets::UTF8_FULL_CONDENSED;
use comfy_table::{Cell, Color, Table};
use serde_json::json;

use crate::accessors::IpcAccessor;
use crate::utils::{file as file_utils, units};
use crate::{OutputFormat, UnitFormat};

pub fn execute(file: &Path, format: OutputFormat, units: UnitFormat) -> Result<()> {
    let content = file_utils::load_ipc_file(file)?;
    let ipc = ipc2581::Ipc2581::parse(&content)?;
    let accessor = IpcAccessor::new(&ipc);

    match format {
        OutputFormat::Text => output_text(&accessor, units),
        OutputFormat::Json => output_json(&accessor),
    }
}

fn output_text(accessor: &IpcAccessor, unit_format: UnitFormat) -> Result<()> {
    let mut summary_table = Table::new();
    summary_table.load_preset(UTF8_FULL_CONDENSED);
    summary_table.set_content_arrangement(comfy_table::ContentArrangement::Dynamic);

    // Board dimensions
    if let Some(dimensions) = accessor.board_dimensions() {
        summary_table.add_row(vec![
            Cell::new("Board Size").fg(Color::Cyan),
            Cell::new(units::format_board_size(
                dimensions.width_mm(),
                dimensions.height_mm(),
                unit_format,
            )),
        ]);
    }

    // Component statistics
    if let Some(components) = accessor.component_stats() {
        summary_table.add_row(vec![
            Cell::new("Components").fg(Color::Cyan),
            Cell::new(components.total.to_string()),
        ]);
    }

    // Net statistics
    if let Some(nets) = accessor.net_stats() {
        if nets.count > 0 {
            summary_table.add_row(vec![
                Cell::new("Nets").fg(Color::Cyan),
                Cell::new(nets.count.to_string()),
            ]);
        }
    }

    // Drill statistics
    if let Some(drills) = accessor.drill_stats() {
        if drills.total_holes > 0 {
            summary_table.add_row(vec![
                Cell::new("Drill Holes").fg(Color::Cyan),
                Cell::new(format!(
                    "{} ({} sizes)",
                    drills.total_holes, drills.unique_sizes
                )),
            ]);
        }
    }

    // Layer count
    if let Some(layers) = accessor.layer_stats() {
        summary_table.add_row(vec![
            Cell::new("Copper Layers").fg(Color::Cyan),
            Cell::new(layers.copper_count.to_string()),
        ]);
    }

    // Stackup thickness
    if let Some(stackup) = accessor.stackup_info() {
        if let Some(thickness) = stackup.overall_thickness_mm() {
            summary_table.add_row(vec![
                Cell::new("Board Thickness").fg(Color::Cyan),
                Cell::new(units::convert_mm(thickness, unit_format)),
            ]);
        }
    }

    println!("{summary_table}");

    // File metadata at the end (greyed out)
    let ipc = accessor.ipc();
    let content = ipc.content();
    let mode_str = if let Some(level) = content.function_mode.level {
        format!("{:?}/{:?}", content.function_mode.mode, level)
    } else {
        format!("{:?}", content.function_mode.mode)
    };

    println!(
        "{}",
        format!("IPC-2581 {} • {}", ipc.revision(), mode_str).dimmed()
    );

    Ok(())
}

fn output_json(accessor: &IpcAccessor) -> Result<()> {
    let ipc = accessor.ipc();
    let content = ipc.content();

    let mut info = json!({
        "revision": ipc.revision(),
        "mode": format!("{:?}", content.function_mode.mode),
        "level": content.function_mode.level.map(|l| format!("{:?}", l)),
    });

    // Board dimensions
    if let Some(dimensions) = accessor.board_dimensions() {
        info["board_dimensions"] = json!({
            "width_mm": dimensions.width_mm(),
            "height_mm": dimensions.height_mm(),
            "width_inch": dimensions.width_inch(),
            "height_inch": dimensions.height_inch(),
        });
    }

    // Component statistics
    if let Some(components) = accessor.component_stats() {
        info["components"] = json!({
            "total": components.total,
            "smt": components.smt,
            "tht": components.tht,
            "other": components.other,
        });
    }

    // Drill statistics
    if let Some(drills) = accessor.drill_stats() {
        if drills.total_holes > 0 {
            info["drills"] = json!({
                "total_holes": drills.total_holes,
                "unique_sizes": drills.unique_sizes,
            });
        }
    }

    // Net statistics
    if let Some(nets) = accessor.net_stats() {
        info["nets"] = json!({
            "count": nets.count,
        });
    }

    // Layer statistics
    if let Some(layers) = accessor.layer_stats() {
        info["layers"] = json!({
            "copper": layers.copper_count,
            "total": layers.total_count,
        });
    }

    // Stackup
    if let Some(stackup) = accessor.stackup_info() {
        info["stackup"] = json!({
            "overall_thickness_mm": stackup.overall_thickness_mm(),
            "layer_count": stackup.layer_count,
        });
    }

    println!("{}", serde_json::to_string_pretty(&info)?);
    Ok(())
}
