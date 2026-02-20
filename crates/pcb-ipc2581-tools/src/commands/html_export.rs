use std::path::Path;

use anyhow::{Context, Result};
use minijinja::{Environment, context};
use serde::Serialize;

use crate::UnitFormat;
use crate::accessors::{ColorInfo, IpcAccessor, StackupLayerType, SurfaceFinishInfo};
use crate::utils::file as file_utils;

pub fn execute(
    input_file: &Path,
    output_file: Option<&Path>,
    unit_format: UnitFormat,
) -> Result<()> {
    // Load and parse IPC-2581 file
    let content = file_utils::load_ipc_file(input_file)?;
    let ipc = ipc2581::Ipc2581::parse(&content)?;
    let accessor = IpcAccessor::new(&ipc);

    // Generate HTML
    let html = generate_html(&accessor, unit_format)?;

    // Determine output path
    let output_path = match output_file {
        Some(path) => path.to_path_buf(),
        None => {
            let mut path = input_file.to_path_buf();
            path.set_extension("html");
            path
        }
    };

    // Write HTML to file
    std::fs::write(&output_path, html)
        .with_context(|| format!("Failed to write HTML to {}", output_path.display()))?;

    println!("✓ HTML exported to {}", output_path.display());

    Ok(())
}

pub fn generate_html(accessor: &IpcAccessor, unit_format: UnitFormat) -> Result<String> {
    let mut env = Environment::new();
    env.add_template("html", HTML_TEMPLATE)
        .context("Failed to add HTML template")?;

    let template = env.get_template("html")?;

    // Extract data
    let board_summary = extract_board_summary(accessor, unit_format);
    let stackup = extract_stackup_data(accessor, unit_format);
    let version = env!("CARGO_PKG_VERSION");

    // Extract file metadata
    let ipc = accessor.ipc();
    let content = ipc.content();
    let ipc_revision = ipc.revision();
    let mode_str = if let Some(level) = content.function_mode.level {
        format!("{:?}/{:?}", content.function_mode.mode, level)
    } else {
        format!("{:?}", content.function_mode.mode)
    };
    let file_metadata = accessor.file_metadata();

    // Format software string in Rust
    let software_str = file_metadata
        .as_ref()
        .and_then(|m| m.software.as_ref())
        .and_then(|s| s.format());
    let source_units = file_metadata.as_ref().and_then(|m| m.source_units.clone());
    let created = file_metadata.as_ref().and_then(|m| m.created.clone());
    let last_modified = file_metadata.as_ref().and_then(|m| m.last_modified.clone());

    let html = template
        .render(context! {
            board_summary,
            stackup,
            css_styles => CSS_STYLES,
            version,
            ipc_revision,
            mode_str,
            source_units,
            created,
            last_modified,
            software_str,
        })
        .context("Failed to render HTML template")?;

    Ok(html)
}

#[derive(Serialize)]
struct BoardSummary {
    design_name: Option<String>,
    width: Option<String>,
    height: Option<String>,
    thickness: Option<String>,
    copper_layers: Option<usize>,
    components: Option<usize>,
    nets: Option<usize>,
    drill_holes: Option<String>,
}

#[derive(Serialize)]
struct StackupData {
    name: String,
    overall_thickness: Option<String>,
    layers: Vec<StackupLayer>,
    soldermask_color: Option<Color>,
    silkscreen_color: Option<Color>,
    surface_finish: Option<SurfaceFinish>,
    outer_copper: Option<String>,
    inner_copper: Option<String>,
}

#[derive(Serialize)]
struct SurfaceFinish {
    name: String,
    hex: String,
    is_standard: bool,
}

#[derive(Serialize)]
struct StackupLayer {
    number: String,
    name: String,
    layer_type: String,
    thickness_mm: Option<String>,
    thickness_mil: Option<String>,
    material: Option<String>,
    dk: Option<String>,
    loss_tangent: Option<String>,
    is_conductor: bool,
    is_dielectric: bool,
}

#[derive(Serialize)]
struct Color {
    name: String,
    hex: String,
}

fn extract_board_summary(accessor: &IpcAccessor, unit_format: UnitFormat) -> BoardSummary {
    let design_name = accessor
        .first_step()
        .map(|step| accessor.ipc().resolve(step.name).to_string());

    let (width, height) = if let Some(dims) = accessor.board_dimensions() {
        let (w, h) = match unit_format {
            UnitFormat::Mm => (
                format!("{:.2} mm", dims.width_mm()),
                format!("{:.2} mm", dims.height_mm()),
            ),
            UnitFormat::Mil => (
                format!("{:.1} mil", dims.width_mm() / 0.0254),
                format!("{:.1} mil", dims.height_mm() / 0.0254),
            ),
            UnitFormat::Inch => (
                format!("{:.3} in", dims.width_mm() / 25.4),
                format!("{:.3} in", dims.height_mm() / 25.4),
            ),
        };
        (Some(w), Some(h))
    } else {
        (None, None)
    };

    let thickness = if let Some(stackup) = accessor.stackup_details() {
        stackup.overall_thickness_mm.map(|t| match unit_format {
            UnitFormat::Mm => format!("{:.2} mm", t),
            UnitFormat::Mil => format!("{:.1} mil", t / 0.0254),
            UnitFormat::Inch => format!("{:.4} in", t / 25.4),
        })
    } else {
        None
    };

    let copper_layers = accessor.stackup_details().map(|s| {
        s.layers
            .iter()
            .filter(|l| l.layer_type == StackupLayerType::Conductor)
            .count()
    });

    let components = accessor.component_stats().map(|stats| stats.total);
    let nets = accessor.net_stats().map(|stats| stats.count);
    let drill_holes = accessor.drill_stats().and_then(|drills| {
        if drills.total_holes > 0 {
            Some(format!(
                "{} total ({} sizes)",
                drills.total_holes, drills.unique_sizes
            ))
        } else {
            None
        }
    });

    BoardSummary {
        design_name,
        width,
        height,
        thickness,
        copper_layers,
        components,
        nets,
        drill_holes,
    }
}

/// Format a decimal number with engineering precision:
/// - Removes unnecessary trailing zeros
/// - Maintains minimum decimal places
fn format_decimal(value: f64, min_decimals: usize, max_decimals: usize) -> String {
    let formatted = format!("{:.prec$}", value, prec = max_decimals);
    let trimmed = formatted.trim_end_matches('0');

    // Ensure minimum decimal places
    if let Some(dot_pos) = trimmed.find('.') {
        let current_decimals = trimmed.len() - dot_pos - 1;
        if current_decimals < min_decimals {
            let zeros_needed = min_decimals - current_decimals;
            format!("{}{}", trimmed, "0".repeat(zeros_needed))
        } else {
            trimmed.to_string()
        }
    } else {
        // No decimal point, add it with min decimals
        format!("{}.{}", trimmed, "0".repeat(min_decimals))
    }
}

fn extract_stackup_data(accessor: &IpcAccessor, unit_format: UnitFormat) -> Option<StackupData> {
    let stackup = accessor.stackup_details()?;

    // Format total thickness like: "1.61 mm (63.2 mil)"
    let overall_thickness = stackup.overall_thickness_mm.map(|t| match unit_format {
        UnitFormat::Mm => {
            let mm = format_decimal(t, 2, 2);
            let mil = format_decimal(t / 0.0254, 1, 1);
            format!("{} mm ({} mil)", mm, mil)
        }
        UnitFormat::Mil => {
            let mil = format_decimal(t / 0.0254, 1, 2);
            let mm = format_decimal(t, 2, 2);
            format!("{} mil ({} mm)", mil, mm)
        }
        UnitFormat::Inch => {
            let inch = format_decimal(t / 25.4, 3, 4);
            let mm = format_decimal(t, 2, 2);
            format!("{} in ({} mm)", inch, mm)
        }
    });

    let layers = stackup
        .layers
        .iter()
        .filter(|layer| {
            // Only include physical stackup layers (conductor, dielectric, soldermask)
            // Filter out "Other" layers (silkscreen, paste, etc.) as they're not part of the board structure
            layer.layer_type != StackupLayerType::Other
        })
        .map(|layer| {
            let is_conductor = layer.layer_type == StackupLayerType::Conductor;
            let is_dielectric = layer.layer_type.is_dielectric();
            let is_soldermask = layer.layer_type == StackupLayerType::Soldermask;

            // Only show thickness for conductor, dielectric, and soldermask layers
            let (thickness_mm, thickness_mil) = if is_conductor || is_dielectric || is_soldermask {
                (
                    layer.thickness_mm.map(|t| format_decimal(t, 2, 4)),
                    layer.thickness_mm.map(|t| format_decimal(t / 0.0254, 1, 2)),
                )
            } else {
                (None, None)
            };

            StackupLayer {
                number: layer.layer_number.unwrap_or(0).to_string(),
                name: layer.name.clone(),
                layer_type: layer.layer_type.as_str().to_string(),
                thickness_mm,
                thickness_mil,
                material: layer.material.clone(),
                dk: layer.dielectric_constant.map(|dk| format_decimal(dk, 1, 2)),
                loss_tangent: layer.loss_tangent.map(|lt| format_decimal(lt, 3, 4)),
                is_conductor,
                is_dielectric,
            }
        })
        .collect();

    // Calculate copper weights using helper methods
    let outer_copper = stackup.outer_copper_weight();
    let inner_copper = stackup.inner_copper_weight();

    let soldermask_color = stackup.soldermask_color.as_ref().and_then(color_to_html);
    let silkscreen_color = stackup.silkscreen_color.as_ref().and_then(color_to_html);
    let surface_finish = stackup.surface_finish.as_ref().map(surface_finish_to_html);

    Some(StackupData {
        name: stackup.name,
        overall_thickness,
        layers,
        soldermask_color,
        silkscreen_color,
        surface_finish,
        outer_copper,
        inner_copper,
    })
}

fn color_to_html(color: &ColorInfo) -> Option<Color> {
    let name = color.name.clone()?;
    let hex = color.hex_color()?;
    Some(Color { name, hex })
}

fn surface_finish_to_html(finish: &SurfaceFinishInfo) -> SurfaceFinish {
    SurfaceFinish {
        name: finish.name.clone(),
        hex: finish.hex_color(),
        is_standard: finish.is_standard, // Track but don't render
    }
}

const HTML_TEMPLATE: &str = include_str!("html_template.html.jinja");
const CSS_STYLES: &str = include_str!("style.css");
