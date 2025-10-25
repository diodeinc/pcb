use crate::types::Units;
use crate::{
    board_outline::{render_board_outline_svg, PadShape},
    geometry, Ipc2581, LayerFunction, PlatingStatus, Side, StandardPrimitive, UserPrimitive,
    UserShapeType,
};
use base64::Engine;
use minijinja::{context, Environment};
use serde::Serialize;

#[derive(Serialize)]
struct BoardSummary {
    design_name: String,
    board_dimensions: Option<BoardDimensions>,
    copper_layers: usize,
    components: ComponentCounts,
    board_thickness: Option<BoardThickness>,
    drill_info: Option<DrillSummary>,
}

#[derive(Serialize)]
struct BoardDimensions {
    width_in: f64,
    height_in: f64,
    width_mm: f64,
    height_mm: f64,
}

#[derive(Serialize)]
struct ComponentCounts {
    total: usize,
    smt: usize,
    tht: usize,
    other: usize,
}

#[derive(Serialize)]
struct BoardThickness {
    mils: f64,
    mm: f64,
}

#[derive(Serialize)]
struct DrillSummary {
    total_holes: usize,
    unique_sizes: usize,
}

#[derive(Serialize)]
struct FileInfo {
    revision: String,
    mode: String,
    source_units: Option<String>,
    created: Option<String>,
    last_modified: Option<String>,
    software: Option<String>,
    author: Option<String>,
    enterprise: Option<String>,
}

#[derive(Serialize)]
struct Downloads {
    filename: String,
    xml_size_mb: f64,
    compressed_size_mb: f64,
}

#[derive(Serialize)]
struct StackupLayer {
    number: usize,
    name: String,
    layer_type: String,
    thickness_mils: Option<f64>,
    material: String,
    dk: Option<f64>,
    loss_tangent: Option<f64>,
    is_copper: bool,
    is_dielectric: bool,
}

#[derive(Serialize)]
struct StackupInfo {
    layers: Vec<StackupLayer>,
    where_measured: Option<String>,
    overall_thickness_mils: Option<f64>,
    overall_thickness_mm: Option<f64>,
}

#[derive(Serialize)]
struct BoardFinishes {
    top_soldermask: Option<FinishInfo>,
    bottom_soldermask: Option<FinishInfo>,
    top_silkscreen: Option<FinishInfo>,
    bottom_silkscreen: Option<FinishInfo>,
}

#[derive(Serialize)]
struct FinishInfo {
    color: Option<String>,
    color_hex: Option<String>, // Hex code for swatch
    thickness_mils: Option<f64>,
}

fn get_color_hex(color_name: &str) -> Option<String> {
    // Check if it's already a hex code
    let trimmed = color_name.trim();
    if trimmed.starts_with('#') && trimmed.len() == 7 {
        return Some(trimmed.to_string());
    }
    // Handle RGBA hex codes (8 characters: # + 6 hex + 2 alpha)
    if trimmed.starts_with('#') && trimmed.len() == 9 {
        return Some(trimmed[0..7].to_string());
    }
    if trimmed.len() == 6 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        return Some(format!("#{}", trimmed));
    }
    // Handle RGBA without # (8 hex characters)
    if trimmed.len() == 8 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        return Some(format!("#{}", &trimmed[0..6]));
    }

    // Map common color names to hex
    match color_name.to_lowercase().as_str() {
        "black" => Some("#000000".to_string()),
        "white" => Some("#FFFFFF".to_string()),
        "green" => Some("#006400".to_string()),
        "red" => Some("#8B0000".to_string()),
        "blue" => Some("#00008B".to_string()),
        "yellow" => Some("#FFD700".to_string()),
        _ => None, // Unknown color
    }
}

#[derive(Serialize)]
struct ComponentInfo {
    package_defs: usize,
    component_instances: usize,
    dnp_count: usize,
}

#[derive(Serialize)]
struct ConnectivityInfo {
    logical_nets: usize,
    total_pins: usize,
}

#[derive(Serialize)]
struct DrillStats {
    total_holes: usize,
    via_count: usize,
    via_pct: usize,
    pth_count: usize,
    pth_pct: usize,
    npth_count: usize,
    npth_pct: usize,
    unique_sizes: usize,
    range_mils: String,
}

#[derive(Serialize)]
struct DrillSize {
    diameter_mils: f64,
    diameter_mm: f64,
    count: usize,
    drill_type: String,
}

#[derive(Serialize)]
struct DrillHistogram {
    via_sizes: Vec<DrillSize>,
    pth_sizes: Vec<DrillSize>,
    npth_sizes: Vec<DrillSize>,
    max_count: usize,
    bar_width: f64,
}

#[derive(Serialize)]
struct DrillInfo {
    stats: DrillStats,
    histogram: Option<DrillHistogram>,
    details: Vec<DrillSize>,
}

#[derive(Serialize)]
struct BomItem {
    number: usize,
    part_number: String,
    value: String,
    manufacturer: String,
    mpn: String,
    category: String,
    quantity: String,
    dnp: bool,
    ref_designators: String,
}

#[derive(Serialize)]
struct Bom {
    total_items: usize,
    items: Vec<BomItem>,
}

pub fn generate_html(doc: &Ipc2581, xml: &str, filename: &str) -> String {
    let mut env = Environment::new();
    env.add_template("base", include_str!("html_template.html"))
        .expect("Failed to add template");

    let template = env.get_template("base").expect("Failed to get template");

    let xml_size = xml.len();
    let compressed_xml_b64 = compress_xml(xml);
    let compressed_size = (compressed_xml_b64.len() as f64 * 0.75) as usize;

    let board_outline_svg = extract_board_outline(doc);
    let stackup = extract_stackup(doc);
    let board_finishes = extract_board_finishes(doc);
    let drill = extract_drill_info(doc);
    let bom = extract_bom(doc);

    template
        .render(context! {
            board_summary => extract_board_summary(doc),
            file_info => extract_file_info(doc),
            downloads => Downloads {
                filename: filename.to_string(),
                xml_size_mb: xml_size as f64 / 1_000_000.0,
                compressed_size_mb: compressed_size as f64 / 1_000_000.0,
            },
            board_outline_svg,
            stackup,
            board_finishes,
            component_info => extract_component_info(doc),
            connectivity_info => extract_connectivity_info(doc),
            drill,
            bom,
            compressed_xml => compressed_xml_b64,
            fzstd_js => include_str!("fzstd.min.js"),
        })
        .expect("Failed to render template")
}

fn extract_file_info(doc: &Ipc2581) -> FileInfo {
    let content = doc.content();
    let history = doc.history_record();
    let header = doc.logistic_header();

    // Extract source units from CadHeader if ECAD section exists
    let source_units = doc
        .ecad()
        .map(|ecad| format!("{:?}", ecad.cad_header.units));

    // Build software string with version if available
    let software = history.and_then(|h| {
        let base_software = h.software.map(|s| doc.resolve(s).to_string());

        // Try to get more detailed software info from FileRevision/SoftwarePackage
        if let Some(file_rev) = &h.file_revision {
            if let Some(sw_pkg) = &file_rev.software_package {
                let name = doc.resolve(sw_pkg.name);
                let version = sw_pkg.revision.map(|r| doc.resolve(r).to_string());
                let vendor = sw_pkg.vendor.map(|v| doc.resolve(v).to_string());

                // Format as "Name version (Vendor)" or just use what we have
                return Some(match (version, vendor) {
                    (Some(v), Some(vendor_str)) => format!("{} {} ({})", name, v, vendor_str),
                    (Some(v), None) => format!("{} {}", name, v),
                    (None, Some(vendor_str)) => format!("{} ({})", name, vendor_str),
                    (None, None) => name.to_string(),
                });
            }
        }

        base_software
    });

    FileInfo {
        revision: doc.revision().to_string(),
        mode: format!("{:?}", content.function_mode.mode),
        source_units,
        created: history.map(|h| doc.resolve(h.origination).to_string()),
        last_modified: history.map(|h| doc.resolve(h.last_change).to_string()),
        software,
        author: header.and_then(|h| h.persons.first().map(|p| doc.resolve(p.name).to_string())),
        enterprise: header.and_then(|h| {
            h.enterprises
                .first()
                .and_then(|e| e.name.map(|n| doc.resolve(n).to_string()))
        }),
    }
}

fn extract_board_summary(doc: &Ipc2581) -> Option<BoardSummary> {
    let ecad = doc.ecad()?;
    let step = &ecad.cad_data.steps[0];

    let board_dimensions = step.profile.as_ref().map(|profile| {
        // Use the same slot collection and arc-aware bounding box calculation as the SVG rendering
        let slots = collect_slots(step);
        let (width_mm, height_mm) = geometry::calculate_board_outline_dimensions(
            &profile.polygon,
            &profile.cutouts,
            &slots,
        );

        BoardDimensions {
            width_in: crate::units::from_mm(width_mm, Units::Inch),
            height_in: crate::units::from_mm(height_mm, Units::Inch),
            width_mm,
            height_mm,
        }
    });

    let (plane, conductor, signal) = count_layer_types(&ecad.cad_data.layers);
    let copper_layers = plane + conductor + signal;

    let (smt, tht, other) = count_component_types(&step.components);
    let components = ComponentCounts {
        total: step.components.len(),
        smt,
        tht,
        other,
    };

    let board_thickness = ecad.cad_data.stackup.as_ref().and_then(|stackup| {
        stackup.overall_thickness.map(|thickness_mm| {
            // Note: thickness is in mm after parsing, convert to mils
            BoardThickness {
                mils: crate::units::from_mm(thickness_mm, Units::Mils),
                mm: thickness_mm,
            }
        })
    });

    let (total_holes, unique_sizes) = count_drill_info(doc, &ecad.cad_data.layers, step);
    let drill_info = if total_holes > 0 {
        Some(DrillSummary {
            total_holes,
            unique_sizes,
        })
    } else {
        None
    };

    Some(BoardSummary {
        design_name: doc.resolve(step.name).to_string(),
        board_dimensions,
        copper_layers,
        components,
        board_thickness,
        drill_info,
    })
}

/// Extract pad shape from a StandardPrimitive for accurate rendering
fn extract_pad_shape(prim: &StandardPrimitive) -> Option<PadShape> {
    use crate::types::primitives::*;
    match prim {
        StandardPrimitive::Circle(c) => Some(PadShape::Circle {
            diameter: c.diameter,
        }),
        StandardPrimitive::RectCenter(r) => Some(PadShape::Rect {
            width: r.width,
            height: r.height,
        }),
        StandardPrimitive::Oval(o) => Some(PadShape::Oval {
            width: o.width,
            height: o.height,
        }),
        _ => None,
    }
}

/// Extract pad shape from a UserPrimitive for accurate rendering
fn extract_user_pad_shape(user_prim: &UserPrimitive) -> Option<PadShape> {
    let UserPrimitive::UserSpecial(special) = user_prim;

    // Collect all shapes from the UserPrimitive
    let shapes: Vec<PadShape> = special
        .shapes
        .iter()
        .map(|shape| match &shape.shape {
            UserShapeType::Circle(c) => PadShape::Circle {
                diameter: c.diameter,
            },
            UserShapeType::RectCenter(r) => PadShape::Rect {
                width: r.width,
                height: r.height,
            },
            UserShapeType::Oval(o) => PadShape::Oval {
                width: o.width,
                height: o.height,
            },
            UserShapeType::Polygon(p) => PadShape::Polygon { polygon: p.clone() },
        })
        .collect();

    // Return composite if multiple shapes, single shape if one, or None if empty
    match shapes.len() {
        0 => None,
        1 => shapes.into_iter().next(),
        _ => Some(PadShape::Composite { shapes }),
    }
}

/// Helper to collect slots from layer features
fn collect_slots(step: &crate::Step) -> Vec<(crate::Polygon, f64, f64)> {
    let mut slots = Vec::new();
    for layer_feature in &step.layer_features {
        for feature_set in &layer_feature.sets {
            for slot in &feature_set.slots {
                slots.push((slot.outline.clone(), slot.x, slot.y));
            }
        }
    }
    slots
}

fn extract_board_outline(doc: &Ipc2581) -> Option<String> {
    use crate::board_outline::BoardOutlineData;
    use crate::PadUse;
    use std::collections::HashMap;

    let ecad = doc.ecad()?;
    let step = &ecad.cad_data.steps[0];
    let profile = step.profile.as_ref()?;

    // Build a map of standard primitives for quick lookup
    let standard_primitives: HashMap<_, _> = doc
        .content()
        .dictionary_standard
        .entries
        .iter()
        .map(|entry| (entry.id, &entry.primitive))
        .collect();

    // Build a map of user primitives for quick lookup
    let user_primitives: HashMap<_, _> = doc
        .content()
        .dictionary_user
        .entries
        .iter()
        .map(|entry| (entry.id, &entry.primitive))
        .collect();

    // Build a map of padstack definitions for quick lookup
    let padstack_defs: HashMap<_, _> = step
        .padstack_defs
        .iter()
        .map(|def| (def.name, def))
        .collect();

    // Get TOP conductor layer name for pad lookups
    let top_layer_name = ecad
        .cad_data
        .layers
        .iter()
        .find(|l| {
            l.side == Some(Side::Top)
                && matches!(
                    l.layer_function,
                    LayerFunction::Conductor
                        | LayerFunction::Signal
                        | LayerFunction::Plane
                        | LayerFunction::Mixed
                )
        })
        .map(|l| l.name)?;

    // Get drill layers
    let drill_layers: Vec<_> = ecad
        .cad_data
        .layers
        .iter()
        .filter(|l| matches!(l.layer_function, LayerFunction::Drill))
        .map(|l| doc.resolve(l.name))
        .collect();

    // Collect slots from layer features (mechanical features like mounting slots)
    let slots = collect_slots(step);
    // Collect NPTHs (non-plated through holes)
    let mut npths = Vec::new();
    // Collect PTHs (plated through holes) with pad geometry
    let mut pths = Vec::new();

    for layer_feature in &step.layer_features {
        let layer_name = doc.resolve(layer_feature.layer_ref);
        let is_drill_layer = drill_layers.contains(&layer_name);

        for feature_set in &layer_feature.sets {
            // Slots already collected above

            // Collect NPTHs and PTHs from drill layers
            if is_drill_layer {
                // Get the geometry (padstack) reference for this feature set
                let padstack_def = feature_set
                    .geometry
                    .and_then(|geom_ref| padstack_defs.get(&geom_ref));

                for hole in &feature_set.holes {
                    if hole.plating_status == PlatingStatus::NonPlated {
                        npths.push((hole.x, hole.y, hole.diameter));
                    } else if hole.plating_status == PlatingStatus::Plated {
                        // For PTHs, look up the pad diameter from the padstack definition
                        if let Some(padstack_def) = padstack_def {
                            // Find the TOP layer pad with REGULAR use
                            let pad_def_option = padstack_def.pad_defs.iter().find(|pad| {
                                pad.layer_ref == top_layer_name && pad.pad_use == PadUse::Regular
                            });

                            // Extract pad shape from either StandardPrimitive or UserPrimitive
                            let pad_shape = pad_def_option.and_then(|pad| {
                                // Try StandardPrimitiveRef first
                                if let Some(prim_ref) = pad.standard_primitive_ref {
                                    if let Some(prim) = standard_primitives.get(&prim_ref) {
                                        return extract_pad_shape(prim);
                                    }
                                }

                                // Fall back to UserPrimitiveRef
                                if let Some(user_prim_ref) = pad.user_primitive_ref {
                                    if let Some(user_prim) = user_primitives.get(&user_prim_ref) {
                                        return extract_user_pad_shape(user_prim);
                                    }
                                }

                                None
                            });

                            if let Some(shape) = pad_shape {
                                pths.push((hole.x, hole.y, hole.diameter, shape));
                            }
                        }
                    }
                }
            }
        }
    }

    let board_data = BoardOutlineData {
        outline: &profile.polygon,
        cutouts: &profile.cutouts,
        slots: &slots,
        npths: &npths,
        pths: &pths,
    };

    Some(render_board_outline_svg(board_data))
}

fn extract_stackup(doc: &Ipc2581) -> Option<StackupInfo> {
    let ecad = doc.ecad()?;
    let stackup = ecad.cad_data.stackup.as_ref()?;

    if stackup.layers.is_empty() {
        return None;
    }

    let mut layer_map = std::collections::HashMap::new();
    for layer in &ecad.cad_data.layers {
        layer_map.insert(doc.resolve(layer.name), layer);
    }

    // Filter to only show conductor and dielectric layers (normalized view)
    let layers: Vec<StackupLayer> = stackup
        .layers
        .iter()
        .enumerate()
        .filter_map(|(idx, stackup_layer)| {
            let layer_ref = doc.resolve(stackup_layer.layer_ref);
            let layer_def = layer_map.get(layer_ref)?;

            // Only include conductor and dielectric layers in the stackup table
            let is_copper = is_copper_layer(layer_def.layer_function);
            let is_dielectric = is_dielectric_layer(layer_def.layer_function);

            if !is_copper && !is_dielectric {
                return None;
            }

            Some(StackupLayer {
                number: idx + 1,
                name: layer_ref.to_string(),
                layer_type: format_layer_function(layer_def.layer_function).to_string(),
                // Note: thickness is in mm after parsing, convert to mils
                thickness_mils: stackup_layer
                    .thickness
                    .map(|t_mm| crate::units::from_mm(t_mm, Units::Mils)),
                material: stackup_layer
                    .material
                    .map(|m| doc.resolve(m).to_string())
                    .unwrap_or_else(|| "—".to_string()),
                // Only include Dk and loss tangent for dielectric layers
                dk: if is_dielectric {
                    stackup_layer.dielectric_constant
                } else {
                    None
                },
                loss_tangent: if is_dielectric {
                    stackup_layer.loss_tangent
                } else {
                    None
                },
                is_copper,
                is_dielectric,
            })
        })
        .collect();

    if layers.is_empty() {
        return None;
    }

    let where_measured = stackup.where_measured.map(|wm| {
        use crate::WhereMeasured;
        match wm {
            WhereMeasured::Metal => "Metal".to_string(),
            WhereMeasured::Mask => "Mask".to_string(),
            WhereMeasured::Laminate => "Laminate".to_string(),
            WhereMeasured::Other => "Other".to_string(),
        }
    });

    let overall_thickness_mils = stackup
        .overall_thickness
        .map(|t_mm| crate::units::from_mm(t_mm, Units::Mils));

    let overall_thickness_mm = stackup.overall_thickness;

    Some(StackupInfo {
        layers,
        where_measured,
        overall_thickness_mils,
        overall_thickness_mm,
    })
}

fn extract_board_finishes(doc: &Ipc2581) -> Option<BoardFinishes> {
    let ecad = doc.ecad()?;
    let stackup = ecad.cad_data.stackup.as_ref();

    // Build layer map
    let mut layer_map = std::collections::HashMap::new();
    for layer in &ecad.cad_data.layers {
        layer_map.insert(doc.resolve(layer.name), layer);
    }

    let mut top_soldermask = None;
    let mut bottom_soldermask = None;
    let mut top_silkscreen = None;
    let mut bottom_silkscreen = None;

    // Strategy 1: Check stackup layers (KiCad approach - includes mask/silk in stackup)
    if let Some(s) = stackup {
        for stackup_layer in &s.layers {
            let layer_ref = doc.resolve(stackup_layer.layer_ref);
            if let Some(layer_def) = layer_map.get(layer_ref) {
                let thickness_mils = stackup_layer
                    .thickness
                    .map(|t_mm| crate::units::from_mm(t_mm, Units::Mils));

                // Extract color from spec if available
                let color = stackup_layer.spec_ref.and_then(|spec_name| {
                    ecad.cad_header.specs.get(&spec_name).and_then(|spec| {
                        // Look for "Color : X" in properties
                        spec.properties.iter().find_map(|prop| {
                            if prop.starts_with("Color :") || prop.starts_with("Color:") {
                                Some(prop.split(':').nth(1)?.trim().to_string())
                            } else {
                                None
                            }
                        })
                    })
                });

                match layer_def.layer_function {
                    LayerFunction::Soldermask if layer_def.side == Some(Side::Top) => {
                        let color_hex = color.as_ref().and_then(|c| get_color_hex(c));
                        top_soldermask = Some(FinishInfo {
                            color: color.clone(),
                            color_hex,
                            thickness_mils,
                        });
                    }
                    LayerFunction::Soldermask if layer_def.side == Some(Side::Bottom) => {
                        let color_hex = color.as_ref().and_then(|c| get_color_hex(c));
                        bottom_soldermask = Some(FinishInfo {
                            color: color.clone(),
                            color_hex,
                            thickness_mils,
                        });
                    }
                    LayerFunction::Silkscreen if layer_def.side == Some(Side::Top) => {
                        let color_hex = color.as_ref().and_then(|c| get_color_hex(c));
                        top_silkscreen = Some(FinishInfo {
                            color: color.clone(),
                            color_hex,
                            thickness_mils,
                        });
                    }
                    LayerFunction::Silkscreen if layer_def.side == Some(Side::Bottom) => {
                        let color_hex = color.as_ref().and_then(|c| get_color_hex(c));
                        bottom_silkscreen = Some(FinishInfo {
                            color: color.clone(),
                            color_hex,
                            thickness_mils,
                        });
                    }
                    _ => {}
                }
            }
        }
    }

    // Strategy 2: Check layer definitions directly (Allegro approach - mask/silk not in stackup)
    for layer in &ecad.cad_data.layers {
        let layer_name = doc.resolve(layer.name);

        // Extract color from spec properties
        let color = layer_name.split('.').next().and_then(|_base_name| {
            // Try to find spec with this layer name
            for spec in ecad.cad_header.specs.values() {
                let spec_name = doc.resolve(spec.name);
                if spec_name.contains(layer_name) || layer_name.contains(spec_name) {
                    // Look for "Color : X" in properties
                    if let Some(color_val) = spec.properties.iter().find_map(|prop| {
                        if prop.starts_with("Color :") || prop.starts_with("Color:") {
                            Some(prop.split(':').nth(1)?.trim().to_string())
                        } else {
                            None
                        }
                    }) {
                        return Some(color_val);
                    }
                }
            }
            None
        });

        match layer.layer_function {
            LayerFunction::Soldermask
                if layer.side == Some(Side::Top) && top_soldermask.is_none() =>
            {
                let color_hex = color.as_ref().and_then(|c| get_color_hex(c));
                top_soldermask = Some(FinishInfo {
                    color,
                    color_hex,
                    thickness_mils: None,
                });
            }
            LayerFunction::Soldermask
                if layer.side == Some(Side::Bottom) && bottom_soldermask.is_none() =>
            {
                let color_hex = color.as_ref().and_then(|c| get_color_hex(c));
                bottom_soldermask = Some(FinishInfo {
                    color,
                    color_hex,
                    thickness_mils: None,
                });
            }
            LayerFunction::Silkscreen
                if layer.side == Some(Side::Top) && top_silkscreen.is_none() =>
            {
                let color_hex = color.as_ref().and_then(|c| get_color_hex(c));
                top_silkscreen = Some(FinishInfo {
                    color,
                    color_hex,
                    thickness_mils: None,
                });
            }
            LayerFunction::Silkscreen
                if layer.side == Some(Side::Bottom) && bottom_silkscreen.is_none() =>
            {
                let color_hex = color.as_ref().and_then(|c| get_color_hex(c));
                bottom_silkscreen = Some(FinishInfo {
                    color,
                    color_hex,
                    thickness_mils: None,
                });
            }
            _ => {}
        }
    }

    // Return Some only if we found at least one finish
    if top_soldermask.is_some()
        || bottom_soldermask.is_some()
        || top_silkscreen.is_some()
        || bottom_silkscreen.is_some()
    {
        Some(BoardFinishes {
            top_soldermask,
            bottom_soldermask,
            top_silkscreen,
            bottom_silkscreen,
        })
    } else {
        None
    }
}

fn extract_component_info(doc: &Ipc2581) -> Option<ComponentInfo> {
    let ecad = doc.ecad()?;
    let step = &ecad.cad_data.steps[0];

    // Count total DNP components from BOM (sum of quantities for DNP entries)
    let mut dnp_count: usize = 0;
    if let Some(bom) = doc.bom() {
        for item in &bom.items {
            // Check if any RefDes has populate=false (DNP)
            if item.ref_des_list.iter().any(|rd| !rd.populate) {
                dnp_count += item.quantity.unwrap_or(0) as usize;
            }
        }
    }

    Some(ComponentInfo {
        package_defs: step.packages.len(),
        component_instances: step.components.len(),
        dnp_count,
    })
}

fn extract_connectivity_info(doc: &Ipc2581) -> Option<ConnectivityInfo> {
    let ecad = doc.ecad()?;
    let step = &ecad.cad_data.steps[0];
    let total_pins: usize = step.logical_nets.iter().map(|net| net.pin_refs.len()).sum();

    Some(ConnectivityInfo {
        logical_nets: step.logical_nets.len(),
        total_pins,
    })
}

fn extract_drill_info(doc: &Ipc2581) -> Option<DrillInfo> {
    use crate::PlatingStatus;
    use std::collections::{HashMap, HashSet};

    let ecad = doc.ecad()?;
    let step = &ecad.cad_data.steps[0];

    let drill_layers: Vec<_> = ecad
        .cad_data
        .layers
        .iter()
        .filter(|l| matches!(l.layer_function, LayerFunction::Drill))
        .collect();

    if drill_layers.is_empty() {
        return None;
    }

    let mut total_holes = 0usize;
    let mut via_count = 0usize;
    let mut pth_count = 0usize;
    let mut npth_count = 0usize;

    let mut via_counts: HashMap<String, usize> = HashMap::new();
    let mut pth_counts: HashMap<String, usize> = HashMap::new();
    let mut npth_counts: HashMap<String, usize> = HashMap::new();

    let mut unique_diams: HashSet<String> = HashSet::new();
    let mut min_diam_mils: Option<f64> = None;
    let mut max_diam_mils: Option<f64> = None;

    for feature in &step.layer_features {
        let layer_name = doc.resolve(feature.layer_ref);
        let is_drill_layer = drill_layers
            .iter()
            .any(|l| doc.resolve(l.name) == layer_name);
        if !is_drill_layer {
            continue;
        }

        for set in &feature.sets {
            for hole in &set.holes {
                total_holes += 1;

                // Note: hole.diameter is in mm after parsing, convert to inches for grouping
                let diameter_in = crate::units::from_mm(hole.diameter, Units::Inch);
                let key_in = format!("{:.4}", diameter_in);
                unique_diams.insert(key_in.clone());

                let mils = crate::units::from_mm(hole.diameter, Units::Mils);
                min_diam_mils = Some(min_diam_mils.map_or(mils, |m| m.min(mils)));
                max_diam_mils = Some(max_diam_mils.map_or(mils, |m| m.max(mils)));

                match hole.plating_status {
                    PlatingStatus::Via => {
                        via_count += 1;
                        *via_counts.entry(key_in).or_insert(0) += 1;
                    }
                    PlatingStatus::Plated => {
                        pth_count += 1;
                        *pth_counts.entry(key_in).or_insert(0) += 1;
                    }
                    PlatingStatus::NonPlated => {
                        npth_count += 1;
                        *npth_counts.entry(key_in).or_insert(0) += 1;
                    }
                }
            }
        }
    }

    if total_holes == 0 {
        return None;
    }

    let via_pct = ((via_count as f64 / total_holes as f64) * 100.0).round() as usize;
    let pth_pct = ((pth_count as f64 / total_holes as f64) * 100.0).round() as usize;
    let npth_pct = 100 - via_pct - pth_pct;

    let range_mils = if let (Some(minm), Some(maxm)) = (min_diam_mils, max_diam_mils) {
        format!("{:.1} – {:.1}", minm, maxm)
    } else {
        "—".to_string()
    };

    let stats = DrillStats {
        total_holes,
        via_count,
        via_pct,
        pth_count,
        pth_pct,
        npth_count,
        npth_pct,
        unique_sizes: unique_diams.len(),
        range_mils,
    };

    let mut via_vec: Vec<(f64, usize)> = via_counts
        .iter()
        .filter_map(|(k, v)| k.parse::<f64>().ok().map(|d| (d, *v)))
        .collect();
    via_vec.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());

    let mut pth_vec: Vec<(f64, usize)> = pth_counts
        .iter()
        .filter_map(|(k, v)| k.parse::<f64>().ok().map(|d| (d, *v)))
        .collect();
    pth_vec.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());

    let mut npth_vec: Vec<(f64, usize)> = npth_counts
        .iter()
        .filter_map(|(k, v)| k.parse::<f64>().ok().map(|d| (d, *v)))
        .collect();
    npth_vec.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());

    let max_count = via_vec
        .iter()
        .chain(pth_vec.iter())
        .chain(npth_vec.iter())
        .map(|(_, c)| *c)
        .max()
        .unwrap_or(1);

    let histogram = if max_count > 0 {
        let total_bars = via_vec.len() + pth_vec.len() + npth_vec.len();
        let bar_width = if total_bars > 30 { 40.0 } else { 50.0 };

        // Note: d is in inches (from the key parsing above)
        let to_drill_size = |d: f64, c: usize, drill_type: &str| DrillSize {
            diameter_mils: d * 1000.0,
            diameter_mm: d * 25.4,
            count: c,
            drill_type: drill_type.to_string(),
        };

        Some(DrillHistogram {
            via_sizes: via_vec
                .iter()
                .map(|(d, c)| to_drill_size(*d, *c, "Via"))
                .collect(),
            pth_sizes: pth_vec
                .iter()
                .map(|(d, c)| to_drill_size(*d, *c, "PTH"))
                .collect(),
            npth_sizes: npth_vec
                .iter()
                .map(|(d, c)| to_drill_size(*d, *c, "NPTH"))
                .collect(),
            max_count,
            bar_width,
        })
    } else {
        None
    };

    // Note: d is in inches (from the key parsing above)
    let to_drill_size = |d: f64, c: usize, drill_type: &str| DrillSize {
        diameter_mils: d * 1000.0,
        diameter_mm: d * 25.4,
        count: c,
        drill_type: drill_type.to_string(),
    };

    let details: Vec<DrillSize> = via_vec
        .iter()
        .map(|(d, c)| to_drill_size(*d, *c, "Via"))
        .chain(pth_vec.iter().map(|(d, c)| to_drill_size(*d, *c, "PTH")))
        .chain(npth_vec.iter().map(|(d, c)| to_drill_size(*d, *c, "NPTH")))
        .collect();

    Some(DrillInfo {
        stats,
        histogram,
        details,
    })
}
fn extract_bom(doc: &Ipc2581) -> Option<Bom> {
    let bom = doc.bom()?;

    if bom.items.is_empty() {
        return None;
    }

    let items = bom
        .items
        .iter()
        .enumerate()
        .map(|(idx, item)| {
            let part_number = doc.resolve(item.oem_design_number_ref).to_string();
            let ref_des_list: Vec<_> = item
                .ref_des_list
                .iter()
                .map(|rd| doc.resolve(rd.name).to_string())
                .collect();
            let ref_designators = ref_des_list.join(", ");

            let quantity = item
                .quantity
                .map(|q| q.to_string())
                .unwrap_or_else(|| "—".to_string());

            let category = item
                .category
                .map(|c| match c {
                    crate::BomCategory::Electrical => "Electrical",
                    crate::BomCategory::Mechanical => "Mechanical",
                })
                .unwrap_or("—")
                .to_string();

            // Extract value, manufacturer and MPN from characteristics
            let mut value = String::from("—");
            let mut manufacturer = String::from("—");
            let mut mpn = String::from("—");

            if let Some(ref chars) = item.characteristics {
                for textual in &chars.textuals {
                    if let Some(ref name) = textual.name {
                        if let Some(ref val) = textual.value {
                            let name_lower = name.to_lowercase();
                            // Support both KiCad (Manufacturer, Mpn) and Allegro (VENDOR, VENDOR_PN) naming
                            if name_lower == "manufacturer" || name_lower == "vendor" {
                                manufacturer = val.clone();
                            } else if name_lower == "mpn" || name_lower == "vendor_pn" {
                                mpn = val.clone();
                            } else if name_lower == "value" {
                                value = val.clone();
                            } else if value == "—" {
                                // Fallback: use Capacitance, Resistance, or DEVICE_TYPE if Value not set
                                if name_lower == "capacitance"
                                    || name_lower == "resistance"
                                    || name_lower == "device_type"
                                {
                                    value = val.clone();
                                }
                            }
                        }
                    }
                }
            }

            // Check if any RefDes has populate=false (DNP)
            let dnp = item.ref_des_list.iter().any(|rd| !rd.populate);

            // If we have MPN, don't show generic value (MPN is more specific)
            if mpn != "—" {
                value = String::from("—");
            }

            BomItem {
                number: idx + 1,
                part_number,
                value,
                manufacturer,
                mpn,
                category,
                quantity,
                dnp,
                ref_designators,
            }
        })
        .collect();

    Some(Bom {
        total_items: bom.items.len(),
        items,
    })
}

fn compress_xml(xml: &str) -> String {
    let compressed = zstd::encode_all(xml.as_bytes(), 3).unwrap();
    base64::engine::general_purpose::STANDARD.encode(&compressed)
}

// Helper functions

fn count_layer_types(layers: &[crate::Layer]) -> (usize, usize, usize) {
    let plane = layers
        .iter()
        .filter(|l| l.layer_function == LayerFunction::Plane)
        .count();
    let conductor = layers
        .iter()
        .filter(|l| l.layer_function == LayerFunction::Conductor)
        .count();
    let signal = layers
        .iter()
        .filter(|l| l.layer_function == LayerFunction::Signal)
        .count();
    (plane, conductor, signal)
}

fn count_component_types(components: &[crate::Component]) -> (usize, usize, usize) {
    let smt = components
        .iter()
        .filter(|c| matches!(c.mount_type, Some(crate::MountType::Smt)))
        .count();
    let tht = components
        .iter()
        .filter(|c| matches!(c.mount_type, Some(crate::MountType::Tht)))
        .count();
    let other = components
        .iter()
        .filter(|c| matches!(c.mount_type, Some(crate::MountType::Other) | None))
        .count();
    (smt, tht, other)
}

fn count_drill_info(doc: &Ipc2581, layers: &[crate::Layer], step: &crate::Step) -> (usize, usize) {
    use std::collections::HashSet;

    let drill_layers: Vec<_> = layers
        .iter()
        .filter(|l| matches!(l.layer_function, LayerFunction::Drill))
        .collect();

    if drill_layers.is_empty() {
        return (0, 0);
    }

    let mut total_holes = 0usize;
    let mut unique_diams: HashSet<String> = HashSet::new();

    for feature in &step.layer_features {
        let layer_name = doc.resolve(feature.layer_ref);
        let is_drill_layer = drill_layers
            .iter()
            .any(|l| doc.resolve(l.name) == layer_name);
        if !is_drill_layer {
            continue;
        }

        for set in &feature.sets {
            for hole in &set.holes {
                total_holes += 1;
                // Note: hole.diameter is in mm after parsing, convert to inches for grouping
                let key = format!("{:.4}", crate::units::from_mm(hole.diameter, Units::Inch));
                unique_diams.insert(key);
            }
        }
    }

    (total_holes, unique_diams.len())
}

fn format_layer_function(func: LayerFunction) -> &'static str {
    match func {
        LayerFunction::Plane => "Plane",
        LayerFunction::Conductor => "Conductor",
        LayerFunction::Signal => "Signal",
        LayerFunction::CondFilm => "Cu Film",
        LayerFunction::CondFoil => "Cu Foil",
        LayerFunction::DielBase => "Diel Base",
        LayerFunction::DielCore => "Core",
        LayerFunction::DielPreg => "Prepreg",
        LayerFunction::Soldermask => "Soldermask",
        LayerFunction::Solderpaste => "Paste",
        LayerFunction::Silkscreen => "Silkscreen",
        LayerFunction::Legend => "Legend",
        LayerFunction::Drill => "Drill",
        LayerFunction::Rout => "Route",
        LayerFunction::VCut => "V-Cut",
        LayerFunction::Document => "Document",
        LayerFunction::Graphic => "Graphic",
        _ => "Other",
    }
}

fn is_copper_layer(func: LayerFunction) -> bool {
    matches!(
        func,
        LayerFunction::Plane
            | LayerFunction::Conductor
            | LayerFunction::Signal
            | LayerFunction::CondFilm
            | LayerFunction::CondFoil
    )
}

fn is_dielectric_layer(func: LayerFunction) -> bool {
    matches!(
        func,
        LayerFunction::DielBase | LayerFunction::DielCore | LayerFunction::DielPreg
    )
}
