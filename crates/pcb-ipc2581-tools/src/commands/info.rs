use std::path::Path;

use anyhow::Result;
use colored::Colorize;
use comfy_table::presets::UTF8_FULL_CONDENSED;
use comfy_table::{Cell, Color, Table};
use serde_json::json;

use crate::accessors::{ColorInfo, IpcAccessor};
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

/// Format color with unicode block swatch
fn format_color_with_swatch(color: &ColorInfo) -> String {
    use colored::{ColoredString, Colorize};

    // Map color name to colored swatch
    let swatch: ColoredString = if let Some(name) = &color.name {
        match name.to_lowercase().as_str() {
            "black" => "■".black(),
            "white" => "■".white(),
            "red" => "■".red(),
            "green" => "■".green(),
            "blue" => "■".blue(),
            "yellow" => "■".yellow(),
            "magenta" | "purple" => "■".magenta(),
            "cyan" => "■".cyan(),
            _ => "■".normal(),
        }
    } else {
        "■".normal()
    };

    if let Some(name) = &color.name {
        format!("{} {}", swatch, name)
    } else {
        swatch.to_string()
    }
}

fn output_text(accessor: &IpcAccessor, unit_format: UnitFormat) -> Result<()> {
    // Board Summary header
    println!("{}", "Board Summary".bold());

    let mut summary_table = Table::new();
    summary_table.load_preset(UTF8_FULL_CONDENSED);
    summary_table.set_content_arrangement(comfy_table::ContentArrangement::Dynamic);

    // Design name
    if let Some(step) = accessor.first_step() {
        let design_name = accessor.ipc().resolve(step.name);
        summary_table.add_row(vec![
            Cell::new("Design").fg(Color::Cyan),
            Cell::new(design_name),
        ]);
    }

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

    // Stackup table
    if let Some(stackup) = accessor.stackup_details() {
        println!();
        println!("{}", "Stackup".bold());

        // Summary stackup table
        let mut summary_stackup = Table::new();
        summary_stackup.load_preset(UTF8_FULL_CONDENSED);
        summary_stackup.set_content_arrangement(comfy_table::ContentArrangement::Dynamic);

        // Stackup name
        summary_stackup.add_row(vec![
            Cell::new("Stackup Name").fg(Color::Cyan),
            Cell::new(&stackup.name),
        ]);

        // Total thickness
        if let Some(thickness_mm) = stackup.overall_thickness_mm {
            let thickness_mils = thickness_mm / 0.0254;
            summary_stackup.add_row(vec![
                Cell::new("Total Thickness").fg(Color::Cyan),
                Cell::new(format!(
                    "{:.2} mm ({:.1} mil)",
                    thickness_mm, thickness_mils
                )),
            ]);
        }

        // Copper layers
        let copper_count = stackup
            .layers
            .iter()
            .filter(|l| l.layer_type == "Conductor")
            .count();
        summary_stackup.add_row(vec![
            Cell::new("Copper Layers").fg(Color::Cyan),
            Cell::new(copper_count.to_string()),
        ]);

        // Calculate copper weights (1 oz/ft² = 0.0348 mm)
        let outer_layers: Vec<_> = stackup
            .layers
            .iter()
            .filter(|l| {
                l.layer_type == "Conductor" && (l.name.contains("F.Cu") || l.name.contains("B.Cu"))
            })
            .collect();
        let inner_layers: Vec<_> = stackup
            .layers
            .iter()
            .filter(|l| l.layer_type == "Conductor" && l.name.contains("In"))
            .collect();

        // Helper to format copper weight
        let format_copper_weight = |thickness_mm: f64| -> String {
            let oz = thickness_mm / 0.0348;
            let standard_oz = if oz < 0.75 {
                0.5
            } else if oz < 1.25 {
                1.0
            } else if oz < 1.75 {
                1.5
            } else {
                2.0
            };
            format!("{:.2} oz (~{} oz)", oz, standard_oz)
        };

        // Outer copper weight (if consistent)
        if let Some(first_outer) = outer_layers.first() {
            if let Some(thickness) = first_outer.thickness_mm {
                let all_same = outer_layers.iter().all(|l| {
                    l.thickness_mm
                        .map(|t| (t - thickness).abs() < 0.001)
                        .unwrap_or(false)
                });
                if all_same {
                    summary_stackup.add_row(vec![
                        Cell::new("Outer Copper").fg(Color::Cyan),
                        Cell::new(format_copper_weight(thickness)),
                    ]);
                }
            }
        }

        // Inner copper weight (if consistent)
        if let Some(first_inner) = inner_layers.first() {
            if let Some(thickness) = first_inner.thickness_mm {
                let all_same = inner_layers.iter().all(|l| {
                    l.thickness_mm
                        .map(|t| (t - thickness).abs() < 0.001)
                        .unwrap_or(false)
                });
                if all_same {
                    summary_stackup.add_row(vec![
                        Cell::new("Inner Copper").fg(Color::Cyan),
                        Cell::new(format_copper_weight(thickness)),
                    ]);
                }
            }
        }

        // Soldermask color (only show if we have color info)
        if let Some(color) = &stackup.soldermask_color {
            if color.name.is_some() || color.rgb.is_some() {
                let color_display = format_color_with_swatch(color);
                summary_stackup.add_row(vec![
                    Cell::new("Soldermask").fg(Color::Cyan),
                    Cell::new(color_display),
                ]);
            }
        }

        // Silkscreen color (only show if we have color info)
        if let Some(color) = &stackup.silkscreen_color {
            if color.name.is_some() || color.rgb.is_some() {
                let color_display = format_color_with_swatch(color);
                summary_stackup.add_row(vec![
                    Cell::new("Silkscreen").fg(Color::Cyan),
                    Cell::new(color_display),
                ]);
            }
        }

        println!("{summary_stackup}");

        let mut stackup_table = Table::new();
        stackup_table.load_preset(UTF8_FULL_CONDENSED);
        stackup_table.set_content_arrangement(comfy_table::ContentArrangement::Dynamic);

        // Header row
        stackup_table.set_header(vec![
            Cell::new("#"),
            Cell::new("Layer Name"),
            Cell::new("Type"),
            Cell::new("Thickness"),
            Cell::new("Material"),
            Cell::new("Dk"),
            Cell::new("Loss Tan"),
        ]);

        for layer in &stackup.layers {
            let layer_num = layer.layer_number.unwrap_or(0);
            let material = layer.material.as_deref().unwrap_or("");
            let dk = layer
                .dielectric_constant
                .map(|d| format!("{:.1}", d))
                .unwrap_or_default();
            let loss_tan = layer
                .loss_tangent
                .map(|l| format!("{:.2}", l))
                .unwrap_or_default();

            // Determine layer type display
            let type_str = if let Some(ref dt) = layer.dielectric_type {
                dt.clone()
            } else {
                layer.layer_type.clone()
            };

            // Format thickness based on layer type
            let (name_cell, type_cell, thickness_cell) = match layer.layer_type.as_str() {
                "Conductor" => {
                    let thickness = if let Some(t) = layer.thickness_mm {
                        format!("{:.4}mm ({:.1} mils)", t, t / 0.0254)
                    } else {
                        String::new()
                    };
                    (
                        Cell::new(&layer.name).fg(Color::Rgb {
                            r: 255,
                            g: 140,
                            b: 0,
                        }), // Orange
                        Cell::new(&type_str),
                        Cell::new(thickness),
                    )
                }
                "Dielectric" => {
                    let thickness = if let Some(t) = layer.thickness_mm {
                        format!("{:.4}mm ({:.1} mils)", t, t / 0.0254)
                    } else {
                        String::new()
                    };
                    (
                        Cell::new(&layer.name).fg(Color::Grey),
                        Cell::new(&type_str).fg(Color::Grey),
                        Cell::new(thickness).fg(Color::Grey),
                    )
                }
                "Soldermask" => {
                    let thickness = if let Some(t) = layer.thickness_mm {
                        format!("{:.4}mm ({:.1} mils)", t, t / 0.0254)
                    } else {
                        String::new()
                    };
                    (
                        Cell::new(&layer.name).fg(Color::Grey),
                        Cell::new(&type_str).fg(Color::Grey),
                        Cell::new(thickness).fg(Color::Grey),
                    )
                }
                _ => {
                    // Don't show thickness for paste, silkscreen, etc.
                    (Cell::new(&layer.name), Cell::new(&type_str), Cell::new(""))
                }
            };

            stackup_table.add_row(vec![
                Cell::new(layer_num.to_string()),
                name_cell,
                type_cell,
                thickness_cell,
                Cell::new(material),
                Cell::new(dk),
                Cell::new(loss_tan),
            ]);
        }

        println!("{stackup_table}");
        println!();
    }

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

    // Additional metadata (greyed out)
    if let Some(metadata) = accessor.file_metadata() {
        if let Some(units) = &metadata.source_units {
            println!("{}", format!("Source Units: {}", units).dimmed());
        }
        if let Some(created) = &metadata.created {
            println!("{}", format!("Created: {}", created).dimmed());
        }
        if let Some(modified) = &metadata.last_modified {
            println!("{}", format!("Last Modified: {}", modified).dimmed());
        }
        if let Some(software) = &metadata.software {
            if let Some(formatted) = software.format() {
                println!("{}", format!("Software: {}", formatted).dimmed());
            }
        }
    }

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

    // File metadata
    if let Some(metadata) = accessor.file_metadata() {
        info["source_units"] = json!(metadata.source_units);
        info["created"] = json!(metadata.created);
        info["last_modified"] = json!(metadata.last_modified);
        if let Some(software) = &metadata.software {
            info["software"] = json!({
                "name": software.name,
                "package_name": software.package_name,
                "package_revision": software.package_revision,
                "vendor": software.vendor,
                "formatted": software.format(),
            });
        }
    }

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
