use std::collections::HashSet;
use std::path::Path;

use anyhow::Result;
use colored::Colorize;
use comfy_table::presets::UTF8_FULL_CONDENSED;
use comfy_table::{Cell, Color, Table};
use ipc2581::types::{Ecad, LayerFunction, Step};
use serde_json::json;

use crate::utils::{file as file_utils, units};
use crate::{OutputFormat, UnitFormat};

pub fn execute(file: &Path, format: OutputFormat, units: UnitFormat) -> Result<()> {
    let content = file_utils::load_ipc_file(file)?;
    let ipc = ipc2581::Ipc2581::parse(&content)?;

    match format {
        OutputFormat::Text => output_text(&ipc, units),
        OutputFormat::Json => output_json(&ipc),
    }
}

fn output_text(ipc: &ipc2581::Ipc2581, unit_format: UnitFormat) -> Result<()> {
    // ECAD section
    if let Some(ecad) = ipc.ecad() {
        output_ecad_info(ecad, unit_format)?;
    }

    // File metadata at the end (greyed out)
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

fn output_ecad_info(ecad: &Ecad, unit_format: UnitFormat) -> Result<()> {
    let mut summary_table = Table::new();
    summary_table.load_preset(UTF8_FULL_CONDENSED);
    summary_table.set_content_arrangement(comfy_table::ContentArrangement::Dynamic);

    // Get first step for analysis
    if let Some(step) = ecad.cad_data.steps.first() {
        // Board dimensions (from profile)
        if let Some((width, height)) = calculate_board_dimensions(step) {
            summary_table.add_row(vec![
                Cell::new("Board Size").fg(Color::Cyan),
                Cell::new(units::format_board_size(width, height, unit_format)),
            ]);
        }

        // Component statistics
        let (total_components, _, _, _) = count_components_by_mount_type(step);
        summary_table.add_row(vec![
            Cell::new("Components").fg(Color::Cyan),
            Cell::new(total_components.to_string()),
        ]);

        // Net statistics
        if !step.logical_nets.is_empty() {
            summary_table.add_row(vec![
                Cell::new("Nets").fg(Color::Cyan),
                Cell::new(step.logical_nets.len().to_string()),
            ]);
        }

        // Drill statistics
        let (total_holes, unique_sizes) = count_drill_info(step);
        if total_holes > 0 {
            summary_table.add_row(vec![
                Cell::new("Drill Holes").fg(Color::Cyan),
                Cell::new(format!("{} ({} sizes)", total_holes, unique_sizes)),
            ]);
        }
    }

    // Layer count
    let copper_count = count_layers_by_function(
        &ecad.cad_data.layers,
        &[
            LayerFunction::Conductor,
            LayerFunction::Signal,
            LayerFunction::Plane,
        ],
    );

    summary_table.add_row(vec![
        Cell::new("Copper Layers").fg(Color::Cyan),
        Cell::new(copper_count.to_string()),
    ]);

    // Stackup thickness
    if let Some(stackup) = ecad.cad_data.stackups.first() {
        if let Some(thickness) = stackup.overall_thickness {
            summary_table.add_row(vec![
                Cell::new("Board Thickness").fg(Color::Cyan),
                Cell::new(units::convert_mm(thickness, unit_format)),
            ]);
        }
    }

    println!("{summary_table}");

    Ok(())
}

fn calculate_board_dimensions(step: &Step) -> Option<(f64, f64)> {
    // Calculate bounding box from profile
    // TODO: Make arc-aware - currently only checks arc endpoints, not the actual arc path
    // For curves, we should calculate the true bounding box including arc bulge
    if let Some(profile) = &step.profile {
        let mut min_x = f64::MAX;
        let mut max_x = f64::MIN;
        let mut min_y = f64::MAX;
        let mut max_y = f64::MIN;

        // Check main outline
        min_x = min_x.min(profile.polygon.begin.x);
        max_x = max_x.max(profile.polygon.begin.x);
        min_y = min_y.min(profile.polygon.begin.y);
        max_y = max_y.max(profile.polygon.begin.y);

        for step in &profile.polygon.steps {
            match step {
                ipc2581::types::PolyStep::Segment(seg) => {
                    min_x = min_x.min(seg.x);
                    max_x = max_x.max(seg.x);
                    min_y = min_y.min(seg.y);
                    max_y = max_y.max(seg.y);
                }
                ipc2581::types::PolyStep::Curve(curve) => {
                    // TODO: Calculate actual arc bounding box, not just endpoints
                    min_x = min_x.min(curve.x);
                    max_x = max_x.max(curve.x);
                    min_y = min_y.min(curve.y);
                    max_y = max_y.max(curve.y);
                }
            }
        }

        let width = max_x - min_x;
        let height = max_y - min_y;

        if width > 0.0 && height > 0.0 {
            return Some((width, height));
        }
    }

    None
}

fn count_components_by_mount_type(step: &Step) -> (usize, usize, usize, usize) {
    let mut smt_count = 0;
    let mut tht_count = 0;
    let mut other_count = 0;

    for component in &step.components {
        match component.mount_type {
            Some(ref mt) => match mt {
                ipc2581::types::MountType::Smt => smt_count += 1,
                ipc2581::types::MountType::Tht => tht_count += 1,
                _ => other_count += 1,
            },
            None => other_count += 1,
        }
    }

    let total = smt_count + tht_count + other_count;
    (total, smt_count, tht_count, other_count)
}

fn count_drill_info(step: &Step) -> (usize, usize) {
    let mut total_holes = 0;
    let mut unique_diameters = HashSet::new();

    for layer_feature in &step.layer_features {
        for set in &layer_feature.sets {
            for hole in &set.holes {
                total_holes += 1;
                // Convert diameter to integer mils to avoid floating point comparison issues
                let diameter_mils = (hole.diameter * 39370.0) as i32;
                unique_diameters.insert(diameter_mils);
            }
        }
    }

    (total_holes, unique_diameters.len())
}

fn count_layers_by_function(
    layers: &[ipc2581::types::Layer],
    functions: &[LayerFunction],
) -> usize {
    layers
        .iter()
        .filter(|layer| functions.contains(&layer.layer_function))
        .count()
}

fn output_json(ipc: &ipc2581::Ipc2581) -> Result<()> {
    let content = ipc.content();
    let mut info = json!({
        "revision": ipc.revision(),
        "mode": format!("{:?}", content.function_mode.mode),
        "level": content.function_mode.level.map(|l| format!("{:?}", l)),
    });

    if let Some(ecad) = ipc.ecad() {
        if let Some(step) = ecad.cad_data.steps.first() {
            // Board dimensions
            if let Some((width, height)) = calculate_board_dimensions(step) {
                info["board_dimensions"] = json!({
                    "width_mm": width,
                    "height_mm": height,
                    "width_inch": width / 25.4,
                    "height_inch": height / 25.4,
                });
            }

            // Component statistics
            let (total, smt, tht, other) = count_components_by_mount_type(step);
            info["components"] = json!({
                "total": total,
                "smt": smt,
                "tht": tht,
                "other": other,
            });

            // Drill statistics
            let (total_holes, unique_sizes) = count_drill_info(step);
            if total_holes > 0 {
                info["drills"] = json!({
                    "total_holes": total_holes,
                    "unique_sizes": unique_sizes,
                });
            }

            // Net statistics
            info["nets"] = json!({
                "count": step.logical_nets.len(),
            });
        }

        // Layer statistics
        let copper_count = count_layers_by_function(
            &ecad.cad_data.layers,
            &[
                LayerFunction::Conductor,
                LayerFunction::Signal,
                LayerFunction::Plane,
            ],
        );

        info["layers"] = json!({
            "copper": copper_count,
            "total": ecad.cad_data.layers.len(),
        });

        // Stackup
        if let Some(stackup) = ecad.cad_data.stackups.first() {
            info["stackup"] = json!({
                "overall_thickness_mm": stackup.overall_thickness,
                "layer_count": stackup.layers.len(),
            });
        }
    }

    println!("{}", serde_json::to_string_pretty(&info)?);
    Ok(())
}
