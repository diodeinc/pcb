use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::Result;
use colored::Colorize;
use comfy_table::presets::UTF8_FULL_CONDENSED;
use comfy_table::{Cell, Color, Table};
use serde::Serialize;
use serde_json::json;

use crate::accessors::{
    ColorInfo, DrillHoleType, IpcAccessor, StackupLayerType, SurfaceFinishInfo,
};
use crate::utils::{file as file_utils, units};
use crate::{OutputFormat, UnitFormat};

/// Format a drill diameter in both mm and mils
fn format_diameter(mm: f64) -> String {
    let mils = mm / 0.0254;
    format!("{:.3}mm ({:.1} mil)", mm, mils)
}

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
    use colored::Colorize;

    let swatch = if let Some((r, g, b)) = color.rgb_color() {
        "■".truecolor(r, g, b)
    } else {
        "■".normal()
    };

    if let Some(name) = &color.name {
        format!("{} {}", swatch, name)
    } else {
        swatch.to_string()
    }
}

/// Format surface finish with color swatch for well-known finishes
fn format_surface_finish_with_swatch(finish: &SurfaceFinishInfo) -> String {
    use colored::Colorize;
    let (r, g, b) = finish.rgb_color();
    let swatch = "■".truecolor(r, g, b);
    format!("{} {}", swatch, finish.name)
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum ComponentMountType {
    Smt,
    Tht,
    Other,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum ComponentSide {
    Top,
    Bottom,
    Internal,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum SoldermaskKind {
    Black,
    Green,
    Other,
}

fn canonical_mount_type(mount_type: Option<ComponentMountType>) -> ComponentMountType {
    mount_type.unwrap_or(ComponentMountType::Unknown)
}

fn map_layer_side(side: Option<ipc2581::types::Side>) -> ComponentSide {
    match side {
        Some(ipc2581::types::Side::Top) => ComponentSide::Top,
        Some(ipc2581::types::Side::Bottom) => ComponentSide::Bottom,
        Some(ipc2581::types::Side::Internal) => ComponentSide::Internal,
        Some(ipc2581::types::Side::Both)
        | Some(ipc2581::types::Side::All)
        | Some(ipc2581::types::Side::None)
        | None => ComponentSide::Unknown,
    }
}

fn canonical_soldermask_kind(color: Option<&ColorInfo>) -> SoldermaskKind {
    let Some(color) = color else {
        return SoldermaskKind::Other;
    };

    if color
        .name
        .as_ref()
        .is_some_and(|name| name.eq_ignore_ascii_case("black"))
    {
        return SoldermaskKind::Black;
    }

    if color
        .rgb_color()
        .is_some_and(|(r, g, b)| r == 0x00 && g == 0x00 && b == 0x00)
    {
        return SoldermaskKind::Black;
    }

    if color
        .name
        .as_ref()
        .is_some_and(|name| name.eq_ignore_ascii_case("green"))
    {
        return SoldermaskKind::Green;
    }

    if color
        .rgb_color()
        .is_some_and(|(r, g, b)| r == 0x00 && g == 0x64 && b == 0x00)
    {
        return SoldermaskKind::Green;
    }

    SoldermaskKind::Other
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
    if let Some(nets) = accessor.net_stats()
        && nets.count > 0
    {
        summary_table.add_row(vec![
            Cell::new("Nets").fg(Color::Cyan),
            Cell::new(nets.count.to_string()),
        ]);
    }

    // Drill statistics (summary)
    if let Some(drills) = accessor.drill_stats()
        && drills.total_holes > 0
    {
        summary_table.add_row(vec![
            Cell::new("Drill Holes").fg(Color::Cyan),
            Cell::new(format!(
                "{} ({} sizes)",
                drills.total_holes, drills.unique_sizes
            )),
        ]);
    }

    // Layer count
    if let Some(layers) = accessor.layer_stats() {
        summary_table.add_row(vec![
            Cell::new("Copper Layers").fg(Color::Cyan),
            Cell::new(layers.copper_count.to_string()),
        ]);
    }

    // Stackup thickness
    if let Some(stackup) = accessor.stackup_info()
        && let Some(thickness) = stackup.overall_thickness_mm()
    {
        summary_table.add_row(vec![
            Cell::new("Board Thickness").fg(Color::Cyan),
            Cell::new(units::convert_mm(thickness, unit_format)),
        ]);
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
            .filter(|l| l.layer_type == StackupLayerType::Conductor)
            .count();
        summary_stackup.add_row(vec![
            Cell::new("Copper Layers").fg(Color::Cyan),
            Cell::new(copper_count.to_string()),
        ]);

        // Outer copper weight (if consistent)
        if let Some(outer_weight) = stackup.outer_copper_weight() {
            summary_stackup.add_row(vec![
                Cell::new("Outer Copper").fg(Color::Cyan),
                Cell::new(outer_weight),
            ]);
        }

        // Inner copper weight (if consistent)
        if let Some(inner_weight) = stackup.inner_copper_weight() {
            summary_stackup.add_row(vec![
                Cell::new("Inner Copper").fg(Color::Cyan),
                Cell::new(inner_weight),
            ]);
        }

        // Soldermask color (only show if we have color info)
        if let Some(color) = &stackup.soldermask_color
            && (color.name.is_some() || color.rgb.is_some())
        {
            let color_display = format_color_with_swatch(color);
            summary_stackup.add_row(vec![
                Cell::new("Soldermask").fg(Color::Cyan),
                Cell::new(color_display),
            ]);
        }

        // Silkscreen color (only show if we have color info)
        if let Some(color) = &stackup.silkscreen_color
            && (color.name.is_some() || color.rgb.is_some())
        {
            let color_display = format_color_with_swatch(color);
            summary_stackup.add_row(vec![
                Cell::new("Silkscreen").fg(Color::Cyan),
                Cell::new(color_display),
            ]);
        }

        // Surface finish (with swatch for well-known finishes)
        if let Some(finish) = &stackup.surface_finish {
            let finish_display = format_surface_finish_with_swatch(finish);
            summary_stackup.add_row(vec![
                Cell::new("Surface Finish").fg(Color::Cyan),
                Cell::new(finish_display),
            ]);
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

        // Filter out "Other" layers (silkscreen, paste, etc.) - only show physical stackup
        for layer in stackup
            .layers
            .iter()
            .filter(|l| l.layer_type != StackupLayerType::Other)
        {
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
            let type_str = layer.layer_type.as_str();

            // Format thickness based on layer type
            let (name_cell, type_cell, thickness_cell) = match layer.layer_type {
                StackupLayerType::Conductor => {
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
                        Cell::new(type_str),
                        Cell::new(thickness),
                    )
                }
                StackupLayerType::DielectricCore
                | StackupLayerType::DielectricPrepreg
                | StackupLayerType::DielectricOther => {
                    let thickness = if let Some(t) = layer.thickness_mm {
                        format!("{:.4}mm ({:.1} mils)", t, t / 0.0254)
                    } else {
                        String::new()
                    };
                    (
                        Cell::new(&layer.name).fg(Color::Grey),
                        Cell::new(type_str).fg(Color::Grey),
                        Cell::new(thickness).fg(Color::Grey),
                    )
                }
                StackupLayerType::Soldermask => {
                    let thickness = if let Some(t) = layer.thickness_mm {
                        format!("{:.4}mm ({:.1} mils)", t, t / 0.0254)
                    } else {
                        String::new()
                    };
                    (
                        Cell::new(&layer.name).fg(Color::Grey),
                        Cell::new(type_str).fg(Color::Grey),
                        Cell::new(thickness).fg(Color::Grey),
                    )
                }
                StackupLayerType::Other => {
                    // Don't show thickness for paste, silkscreen, etc.
                    (Cell::new(&layer.name), Cell::new(type_str), Cell::new(""))
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

        // Material summary
        if let Some(materials) = accessor.material_info() {
            println!();
            println!("{}", "Materials".bold());
            let mut mat_table = Table::new();
            mat_table.load_preset(UTF8_FULL_CONDENSED);
            mat_table.set_content_arrangement(comfy_table::ContentArrangement::Dynamic);
            mat_table.add_row(vec![
                Cell::new("Dielectric").fg(Color::Cyan),
                Cell::new(materials.dielectric.join(", ")),
            ]);
            println!("{mat_table}");
        }

        // Impedance control
        if let Some(imp) = accessor.impedance_control_info()
            && imp.is_impedance_controlled()
        {
            println!();
            println!("{}", "Impedance Control".bold());
            let mut imp_table = Table::new();
            imp_table.load_preset(UTF8_FULL_CONDENSED);
            imp_table.set_content_arrangement(comfy_table::ContentArrangement::Dynamic);
            imp_table.add_row(vec![
                Cell::new("Controlled").fg(Color::Cyan),
                Cell::new("Yes"),
            ]);
            if !imp.dielectric_constants.is_empty() {
                let dk_str: Vec<String> = imp
                    .dielectric_constants
                    .iter()
                    .map(|v| format!("{:.2}", v))
                    .collect();
                imp_table.add_row(vec![
                    Cell::new("Dk").fg(Color::Cyan),
                    Cell::new(dk_str.join(", ")),
                ]);
            }
            if !imp.loss_tangents.is_empty() {
                let df_str: Vec<String> = imp
                    .loss_tangents
                    .iter()
                    .map(|v| format!("{:.4}", v))
                    .collect();
                imp_table.add_row(vec![
                    Cell::new("Df").fg(Color::Cyan),
                    Cell::new(df_str.join(", ")),
                ]);
            }
            println!("{imp_table}");
        }

        println!();
    }

    // Drill distribution
    if let Some(drills) = accessor.drill_stats()
        && !drills.distribution.is_empty()
    {
        println!("{}", "Drill Distribution".bold());
        let mut drill_table = Table::new();
        drill_table.load_preset(UTF8_FULL_CONDENSED);
        drill_table.set_content_arrangement(comfy_table::ContentArrangement::Dynamic);
        drill_table.set_header(vec![
            Cell::new("Type"),
            Cell::new("Diameter"),
            Cell::new("Count"),
        ]);

        for dist in &drills.distribution {
            for (i, size) in dist.sizes.iter().enumerate() {
                let type_cell = if i == 0 {
                    Cell::new(dist.hole_type.as_str()).fg(Color::Cyan)
                } else {
                    Cell::new("")
                };
                drill_table.add_row(vec![
                    type_cell,
                    Cell::new(format_diameter(size.diameter_mm)),
                    Cell::new(size.count.to_string()),
                ]);
            }
        }

        println!("{drill_table}");
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
        if let Some(software) = &metadata.software
            && let Some(formatted) = software.format()
        {
            println!("{}", format!("Software: {}", formatted).dimmed());
        }
    }

    Ok(())
}

fn output_json(accessor: &IpcAccessor) -> Result<()> {
    let ipc = accessor.ipc();
    let content = ipc.content();
    let layer_side_map: BTreeMap<_, _> = ipc
        .ecad()
        .map(|ecad| {
            ecad.cad_data
                .layers
                .iter()
                .map(|layer| (ipc.resolve(layer.name).to_string(), layer.side))
                .collect()
        })
        .unwrap_or_default();

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

    // Drill statistics with distribution
    if let Some(drills) = accessor.drill_stats()
        && drills.total_holes > 0
    {
        let min_via_hole_mm = drills
            .distribution
            .iter()
            .filter(|dist| dist.hole_type == DrillHoleType::Via)
            .flat_map(|dist| dist.sizes.iter().map(|size| size.diameter_mm))
            .min_by(|a, b| a.total_cmp(b));

        let distribution: Vec<_> = drills
            .distribution
            .iter()
            .map(|dist| {
                let sizes: Vec<_> = dist
                    .sizes
                    .iter()
                    .map(|s| {
                        json!({
                            "diameter_mm": s.diameter_mm,
                            "count": s.count,
                        })
                    })
                    .collect();
                json!({
                    "type": format!("{:?}", dist.hole_type),
                    "total": dist.total,
                    "sizes": sizes,
                })
            })
            .collect();
        info["drills"] = json!({
            "total_holes": drills.total_holes,
            "unique_sizes": drills.unique_sizes,
            "distribution": distribution,
            "via_min_diameter_mm": min_via_hole_mm,
        });
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

    // Materials
    if let Some(materials) = accessor.material_info() {
        info["materials"] = json!({
            "dielectric": materials.dielectric,
        });
    }

    // Impedance control
    if let Some(imp) = accessor.impedance_control_info() {
        info["impedance_control"] = json!({
            "controlled": imp.is_impedance_controlled(),
            "dielectric_constants": imp.dielectric_constants,
            "loss_tangents": imp.loss_tangents,
        });
    }

    let stackup_details = accessor.stackup_details();
    if let Some(stackup_details) = &stackup_details {
        info["stackup_details"] = json!({
            "surface_finish_name": stackup_details.surface_finish.as_ref().map(|f| f.name.clone()),
            "surface_finish_category": stackup_details.surface_finish.as_ref().map(|f| f.category),
            "soldermask_name": stackup_details
                .soldermask_color
                .as_ref()
                .and_then(|c| c.name.clone()),
            "soldermask_kind": canonical_soldermask_kind(stackup_details.soldermask_color.as_ref()),
            "outer_copper_oz": stackup_details.outer_copper_oz(),
            "inner_copper_oz": stackup_details.inner_copper_oz(),
        });
    }

    let component_map: BTreeMap<
        String,
        (String, String, Option<ComponentMountType>, Option<String>),
    > = accessor
        .first_step()
        .map(|step| {
            step.components
                .iter()
                .filter_map(|component| {
                    let designator = ipc.resolve(component.ref_des).to_string();
                    if designator.is_empty() {
                        return None;
                    }

                    let package = ipc.resolve(component.package_ref).to_string();
                    let layer_ref = ipc.resolve(component.layer_ref).to_string();
                    let mount_type = component.mount_type.map(|mt| match mt {
                        ipc2581::types::MountType::Smt => ComponentMountType::Smt,
                        ipc2581::types::MountType::Tht => ComponentMountType::Tht,
                        ipc2581::types::MountType::Other => ComponentMountType::Other,
                    });
                    let part_mpn = component
                        .part
                        .map(|sym| ipc.resolve(sym).to_string())
                        .filter(|v| !v.is_empty());

                    Some((designator, (package, layer_ref, mount_type, part_mpn)))
                })
                .collect()
        })
        .unwrap_or_default();

    let mut seen_designators = BTreeSet::new();
    let mut component_placements = Vec::new();

    if let Some(bom_section) = ipc.bom() {
        for item in &bom_section.items {
            if matches!(item.category, Some(ipc2581::types::BomCategory::Document)) {
                continue;
            }

            let avl_lookup = accessor.lookup_avl(item.oem_design_number_ref);

            for ref_des in &item.ref_des_list {
                let designator = ipc.resolve(ref_des.name).to_string();
                if designator.is_empty() {
                    continue;
                }

                let fallback = component_map.get(&designator);
                let bom_package = ipc.resolve(ref_des.package_ref).to_string();
                let bom_layer = ipc.resolve(ref_des.layer_ref).to_string();

                let package = if !bom_package.is_empty() {
                    bom_package
                } else {
                    fallback.map(|v| v.0.clone()).unwrap_or_default()
                };
                let layer_ref = if !bom_layer.is_empty() {
                    bom_layer
                } else {
                    fallback.map(|v| v.1.clone()).unwrap_or_default()
                };
                let mount_type = fallback.and_then(|v| v.2);
                let mpn = avl_lookup
                    .primary_mpn
                    .clone()
                    .or_else(|| fallback.and_then(|v| v.3.clone()));
                let canonical_mount_type = canonical_mount_type(mount_type);
                let side = map_layer_side(layer_side_map.get(&layer_ref).copied().flatten());

                component_placements.push(json!({
                    "designator": designator,
                    "package": package,
                    "mpn": mpn,
                    "dnp": !ref_des.populate,
                    "layer_ref": layer_ref,
                    "mount_type": canonical_mount_type,
                    "side": side,
                    "pin_count": item.pin_count,
                }));
                seen_designators.insert(ipc.resolve(ref_des.name).to_string());
            }
        }
    }

    for (designator, (package, layer_ref, mount_type, part_mpn)) in component_map {
        if seen_designators.contains(&designator) {
            continue;
        }

        let canonical_mount_type = canonical_mount_type(mount_type);
        let side = map_layer_side(layer_side_map.get(&layer_ref).copied().flatten());

        component_placements.push(json!({
            "designator": designator,
            "package": package,
            "mpn": part_mpn,
            "dnp": false,
            "layer_ref": layer_ref,
            "mount_type": canonical_mount_type,
            "side": side,
            "pin_count": serde_json::Value::Null,
        }));
    }

    info["component_placements"] = json!(component_placements);

    println!("{}", serde_json::to_string_pretty(&info)?);
    Ok(())
}
