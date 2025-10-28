/// Stage 4.5: Drill Mask Subtraction
///
/// Processes DRILL layers to extract holes and slots, then subtracts them from
/// copper layers to show correct annular rings (PTH/Via as donuts with holes).
///
/// This mirrors the manufacturing process: copper is etched first, then holes
/// are drilled through the copper layers.

use super::resolved_feature::Point;
use super::stage4::FlattenedLayer;
use super::Result;
use crate::{Ipc2581, LayerFunction, PlatingStatus};
use skia_safe::{Path, PathOp};
use std::collections::HashMap;

/// Drill hole or slot that affects copper layers
#[derive(Debug, Clone)]
struct DrillFeature {
    center: Point,
    path: Path,
    start_layer: String,
    end_layer: String,
    is_circular: bool, // true = hole, false = slot
}

/// Drill mask information for a specific layer
#[derive(Debug, Clone)]
pub struct LayerDrillMask {
    pub mask: Path,
    pub hole_count: usize,
    pub slot_count: usize,
}

/// Apply drill mask subtraction to flattened copper layers
///
/// Returns drill masks per layer for optional visualization
pub fn subtract_drill_mask(
    doc: &Ipc2581,
    flattened_layers: &mut HashMap<String, FlattenedLayer>,
) -> Result<HashMap<String, LayerDrillMask>> {
    println!("Stage 4.5: Drill Mask Subtraction");

    let ecad = doc
        .ecad()
        .ok_or(crate::Ipc2581Error::MissingElement("Ecad"))?;
    let step = ecad
        .cad_data
        .steps
        .first()
        .ok_or(crate::Ipc2581Error::MissingElement("Step"))?;

    // Extract drill features from DRILL layers
    let drill_features = extract_drill_features(doc, step)?;

    if drill_features.is_empty() {
        println!("  No drill features found, skipping");
        return Ok(HashMap::new());
    }

    let hole_count = drill_features.iter().filter(|d| d.is_circular).count();
    let slot_count = drill_features.len() - hole_count;

    println!(
        "  Extracted {} drill features ({} holes, {} slots)",
        drill_features.len(),
        hole_count,
        slot_count
    );

    // Get all layer names in stackup order
    let layer_names: Vec<String> = ecad
        .cad_data
        .layers
        .iter()
        .filter(|l| matches!(l.layer_function, LayerFunction::Conductor | LayerFunction::Plane))
        .map(|l| doc.resolve(l.name).to_string())
        .collect();

    let mut drill_masks = HashMap::new();

    // For each copper layer, subtract applicable drill holes
    for (layer_name, flattened_layer) in flattened_layers.iter_mut() {
        // Find drill features that span this layer
        let applicable_drills: Vec<&DrillFeature> = drill_features
            .iter()
            .filter(|d| drill_spans_layer(d, layer_name, &layer_names))
            .collect();

        if applicable_drills.is_empty() {
            continue;
        }

        println!(
            "  Subtracting {} drills from {}",
            applicable_drills.len(),
            layer_name
        );

        // Union all drill features into a single mask
        let drill_mask = union_drill_features(&applicable_drills)?;

        // Save mask for visualization
        let drill_hole_count = applicable_drills.iter().filter(|d| d.is_circular).count();
        let drill_slot_count = applicable_drills.len() - drill_hole_count;
        drill_masks.insert(
            layer_name.clone(),
            LayerDrillMask {
                mask: drill_mask.clone(),
                hole_count: drill_hole_count,
                slot_count: drill_slot_count,
            },
        );

        // Subtract drill mask from each bucket
        for (bucket, copper_path) in flattened_layer.buckets.iter_mut() {
            match copper_path.op(&drill_mask, PathOp::Difference) {
                Some(result) => {
                    let before_vertices = copper_path.count_points();
                    let after_vertices = result.count_points();
                    *copper_path = result;
                    println!(
                        "    {:?}: {} → {} vertices",
                        bucket, before_vertices, after_vertices
                    );
                }
                None => {
                    eprintln!(
                        "    WARNING: Drill mask subtraction failed for {:?}, keeping original",
                        bucket
                    );
                }
            }
        }
    }

    println!();
    Ok(drill_masks)
}

/// Extract all drill features (holes and slots) from DRILL layers
fn extract_drill_features(
    doc: &Ipc2581,
    step: &crate::Step,
) -> Result<Vec<DrillFeature>> {
    let mut features = Vec::new();

    for layer_feature in &step.layer_features {
        let layer_name = doc.resolve(layer_feature.layer_ref);

        // Check if this is a DRILL layer
        let ecad = doc.ecad().unwrap();
        let is_drill_layer = ecad
            .cad_data
            .layers
            .iter()
            .any(|l| doc.resolve(l.name) == layer_name && l.layer_function == LayerFunction::Drill);

        // Parse layer span from layer name
        let (start_layer, end_layer) = parse_drill_layer_span(layer_name);

        // Extract holes and slots from ALL layers (slots can be on non-DRILL layers too)
        for set in &layer_feature.sets {
            // Process circular holes (only from DRILL layers)
            if is_drill_layer {
                for hole in &set.holes {
                    let path = create_circle_path(Point::new(hole.x, hole.y), hole.diameter);
                    features.push(DrillFeature {
                        center: Point::new(hole.x, hole.y),
                        path,
                        start_layer: start_layer.clone(),
                        end_layer: end_layer.clone(),
                        is_circular: true,
                    });
                }
            }

            // Process slotted holes (can be on ANY layer)
            for slot in &set.slots {
                // DEBUG: Print slot geometry
                println!("  DEBUG SLOT:");
                println!("    Position: ({}, {})", slot.x, slot.y);
                println!("    Plating: {:?}", slot.plating_status);
                println!("    Outline: begin=({}, {})", slot.outline.begin.x, slot.outline.begin.y);
                for (i, step) in slot.outline.steps.iter().enumerate() {
                    match step {
                        crate::PolyStep::Segment(s) => println!("      [{}] Line to ({}, {})", i, s.x, s.y),
                        crate::PolyStep::Curve(c) => println!("      [{}] Curve to ({}, {})", i, c.x, c.y),
                    }
                }

                // Slots have an outline polygon - convert to path
                let path = polygon_to_path(&slot.outline);
                let bbox = path.bounds();
                println!("    Path bbox: ({}, {}) to ({}, {}) => size {}×{}",
                    bbox.left, bbox.top, bbox.right, bbox.bottom,
                    bbox.width(), bbox.height());

                let center = Point::new(slot.x, slot.y);
                features.push(DrillFeature {
                    center,
                    path,
                    start_layer: start_layer.clone(),
                    end_layer: end_layer.clone(),
                    is_circular: false,
                });
            }
        }
    }

    Ok(features)
}

/// Parse drill layer span from layer name (e.g., "DRILL_1-12" → ("TOP", "BOTTOM"))
fn parse_drill_layer_span(layer_name: &str) -> (String, String) {
    // For through-hole drills, assume they span all layers
    // TODO: Parse actual layer range from name if needed
    ("TOP".to_string(), "BOTTOM".to_string())
}

/// Check if a drill feature spans a given copper layer
fn drill_spans_layer(drill: &DrillFeature, layer_name: &str, all_layers: &[String]) -> bool {
    // For now, assume all drills span all layers (through-holes)
    // TODO: Implement proper layer range checking for blind/buried vias

    // Simplified: drills affect all conductor layers
    true
}

/// Union multiple drill features into a single mask
fn union_drill_features(drills: &[&DrillFeature]) -> Result<Path> {
    if drills.is_empty() {
        return Ok(Path::new());
    }

    if drills.len() == 1 {
        return Ok(drills[0].path.clone());
    }

    // Union all drill paths
    let mut result = drills[0].path.clone();
    for drill in &drills[1..] {
        match result.op(&drill.path, PathOp::Union) {
            Some(unioned) => result = unioned,
            None => {
                eprintln!("WARNING: Drill union failed, continuing with partial mask");
            }
        }
    }

    Ok(result)
}

/// Create a circular path for a drill hole using cubic beziers
fn create_circle_path(center: Point, diameter: f64) -> Path {
    let mut path = Path::new();
    let radius = (diameter / 2.0) as f32;
    super::primitives::add_circle_as_cubics(&mut path, (center.x as f32, center.y as f32), radius);
    path
}

/// Convert a polygon to a Skia path
fn polygon_to_path(polygon: &crate::Polygon) -> Path {
    let mut path = Path::new();

    // Move to start point
    path.move_to((polygon.begin.x as f32, polygon.begin.y as f32));

    // Add segments
    for step in &polygon.steps {
        match step {
            crate::PolyStep::Segment(seg) => {
                path.line_to((seg.x as f32, seg.y as f32));
            }
            crate::PolyStep::Curve(curve) => {
                // For slots, curves are typically arcs
                // Approximate with arc_to or line segments
                path.line_to((curve.x as f32, curve.y as f32));
            }
        }
    }

    path.close();
    path
}
