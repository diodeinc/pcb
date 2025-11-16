use super::resolved_feature::*;
use super::{BoardContext, Result};
use crate::{Ipc2581, Pad, Polarity, Symbol, Trace};
use std::collections::HashMap;

/// Stage 1: Hierarchy & Transformation Resolution
///
/// Flattens the Layer → LayerFeature → Set → Features hierarchy and applies
/// all Location offsets and Xform transformations.
///
/// Returns per-layer resolved features ready for geometry conversion.
pub fn resolve_features(
    doc: &Ipc2581,
    context: &BoardContext,
    layer_filter: Option<&[String]>,
) -> Result<HashMap<String, LayerResolution>> {
    let ecad = doc
        .ecad()
        .ok_or(crate::Ipc2581Error::MissingElement("Ecad"))?;
    let step = ecad
        .cad_data
        .steps
        .first()
        .ok_or(crate::Ipc2581Error::MissingElement("Step"))?;

    let mut layer_resolutions: HashMap<String, LayerResolution> = HashMap::new();

    // Filter layers if requested
    let layer_filter_set: Option<std::collections::HashSet<&str>> =
        layer_filter.map(|layers| layers.iter().map(|s| s.as_str()).collect());

    // Process each LayerFeature
    for layer_feature in &step.layer_features {
        let layer_name = doc.resolve(layer_feature.layer_ref).to_string();

        // Apply layer filter
        if let Some(ref filter) = layer_filter_set {
            if !filter.contains(layer_name.as_str()) {
                continue;
            }
        }

        // Look up layer definition to get default polarity
        let layer_default_polarity = ecad
            .cad_data
            .layers
            .iter()
            .find(|l| l.name == layer_feature.layer_ref)
            .and_then(|l| l.polarity)
            .unwrap_or(Polarity::Positive);

        let mut features = Vec::new();
        let mut bbox = BoundingBox::empty();
        let mut stats = LayerStats::new();

        // Process each Set in this LayerFeature
        for set in &layer_feature.sets {
            let net_sym = set.net; // Net is already a Symbol (or None)

            // Get set-level polarity (defaults to layer polarity)
            let set_polarity = set.polarity.unwrap_or(layer_default_polarity);

            // Resolve pads
            for pad in &set.pads {
                if let Some(resolved) =
                    resolve_pad(doc, context, pad, &layer_name, net_sym, set_polarity)
                {
                    stats.record(resolved.bucket);
                    bbox = bbox.union(&resolved.bbox);
                    features.push(resolved);
                }
            }

            // Resolve traces
            for trace in &set.traces {
                if let Some(resolved) = resolve_trace(doc, context, trace, net_sym, set_polarity) {
                    stats.record(resolved.bucket);
                    bbox = bbox.union(&resolved.bbox);
                    features.push(resolved);
                }
            }

            // Resolve polygons (copper pours)
            for polygon in &set.polygons {
                let resolved = resolve_polygon(polygon, net_sym, set_polarity);
                stats.record(resolved.bucket);
                bbox = bbox.union(&resolved.bbox);
                features.push(resolved);
            }

            // Resolve lines
            for line in &set.lines {
                let resolved = resolve_line(line, net_sym, set_polarity);
                stats.record(resolved.bucket);
                bbox = bbox.union(&resolved.bbox);
                features.push(resolved);
            }

            // Resolve arcs (curved trace segments)
            for arc in &set.arcs {
                if let Some(resolved) = resolve_arc(doc, context, arc, net_sym, set_polarity) {
                    stats.record(resolved.bucket);
                    bbox = bbox.union(&resolved.bbox);
                    features.push(resolved);
                }
            }
        }

        layer_resolutions.insert(
            layer_name.clone(),
            LayerResolution {
                layer_name, // Already a String, no need to clone again
                features,
                bbox,
                stats,
            },
        );
    }

    Ok(layer_resolutions)
}

/// Resolve a pad with all transformations applied
fn resolve_pad(
    doc: &Ipc2581,
    context: &BoardContext,
    pad: &Pad,
    layer_name: &str,
    net: Option<Symbol>,
    polarity: Polarity,
) -> Option<ResolvedFeature> {
    let x = pad.x?;
    let y = pad.y?;

    // Get Xform (defaults if not present)
    let xform = pad.xform.unwrap_or_default();

    // NOTE: x, y are ALREADY in mm (parser converted them)
    // Transform the offset vector by scale → mirror → rotate before adding to position
    // (IPC-2581 spec: transforms apply to local coordinates, including offset)
    let transformed_offset = Point::new(xform.x_offset, xform.y_offset)
        .scale(xform.scale)
        .mirror_if(xform.mirror)
        .rotate(xform.rotation);

    let center = Point::new(x, y).translate(transformed_offset.x, transformed_offset.y);

    // The rotation/mirror/scale transformations are applied to the pad geometry in Stage 2

    // Get padstack reference - if missing, skip this pad
    let padstack_ref = pad.padstack_def_ref?;
    let padstack_name = doc.resolve(padstack_ref).to_string();

    // Determine bucket based on plating status
    let bucket = if let Some(psd) = context.padstack_defs.get(&padstack_ref) {
        if let Some(ref hole_def) = psd.hole_def {
            use crate::PlatingStatus;
            match hole_def.plating_status {
                PlatingStatus::Via => FeatureBucket::Via,
                PlatingStatus::Plated => FeatureBucket::Pth,
                // Skip NPTH pads - they're soldermask openings, not copper/electrical features
                // (They appear on mask layers as HOLLOW circles for clearance)
                PlatingStatus::NonPlated => return None,
            }
        } else {
            FeatureBucket::Smd // No hole = SMD
        }
    } else {
        // Padstack not found - this is a data integrity error
        eprintln!(
            "WARNING: Pad references non-existent padstack '{}' at ({:.3}, {:.3}) - skipping",
            padstack_name, center.x, center.y
        );
        return None;
    };

    let geometry = ResolvedGeometry::PadstackRef {
        padstack_name,
        center,
        rotation: xform.rotation,
        mirror: xform.mirror,
        scale: xform.scale,
        layer: layer_name.to_string(),
        inline_standard_primitive: pad
            .standard_primitive_ref
            .map(|s| doc.resolve(s).to_string()),
        inline_user_primitive: pad.user_primitive_ref.map(|s| doc.resolve(s).to_string()),
    };

    // Placeholder bbox (will be replaced in Stage 2 with actual pad shape bbox)
    // Use a nominal 1mm diameter for now - this is just for Stage 1 validation
    let placeholder_radius = 0.5;
    let bbox = BoundingBox {
        min_x: center.x - placeholder_radius,
        min_y: center.y - placeholder_radius,
        max_x: center.x + placeholder_radius,
        max_y: center.y + placeholder_radius,
    };

    Some(ResolvedFeature {
        bucket,
        net,
        polarity,
        geometry,
        bbox,
    })
}

/// Resolve a trace (Polyline) with transformations
fn resolve_trace(
    doc: &Ipc2581,
    context: &BoardContext,
    trace: &Trace,
    net: Option<Symbol>,
    polarity: Polarity,
) -> Option<ResolvedFeature> {
    if trace.points.is_empty() {
        return None;
    }

    // NOTE: Trace points are ALREADY in mm (parser converted them)
    let points: Vec<Point> = trace.points.iter().map(|p| Point::new(p.x, p.y)).collect();

    // Calculate bounding box
    let mut bbox = BoundingBox::from_point(points[0]);
    for p in &points[1..] {
        bbox.expand_to_point(*p);
    }

    // Get line width and line end from LineDescRef - REQUIRED for manufacturing accuracy
    let line_desc_sym = match trace.line_desc_ref {
        Some(sym) => sym,
        None => {
            eprintln!("WARNING: Trace missing LineDescRef (required for line width) - skipping");
            return None;
        }
    };

    let line_desc = match context.line_descriptors.get(&line_desc_sym) {
        Some(desc) => desc,
        None => {
            eprintln!(
                "WARNING: Trace references non-existent LineDesc '{}' - skipping",
                doc.resolve(line_desc_sym)
            );
            return None;
        }
    };
    // NOTE: line_width is ALREADY in mm (parser converted it)
    let line_width = line_desc.line_width;
    let line_end = match line_desc.line_end {
        crate::LineEnd::Round => LineEndStyle::Round,
        crate::LineEnd::Square => LineEndStyle::Square,
        crate::LineEnd::Flat => LineEndStyle::None,
    };

    // Expand bbox by line width
    let half_width = line_width / 2.0;
    bbox = BoundingBox {
        min_x: bbox.min_x - half_width,
        min_y: bbox.min_y - half_width,
        max_x: bbox.max_x + half_width,
        max_y: bbox.max_y + half_width,
    };

    let num_points = points.len();
    let geometry = ResolvedGeometry::Polyline {
        points,
        arc_segments: vec![None; num_points], // Traces have no arc segments
        line_width,
        line_end,
    };

    Some(ResolvedFeature {
        bucket: FeatureBucket::Trace,
        net,
        polarity,
        geometry,
        bbox,
    })
}

/// Resolve a polygon (filled copper pour)
fn resolve_polygon(
    polygon: &crate::Polygon,
    net: Option<Symbol>,
    polarity: Polarity,
) -> ResolvedFeature {
    // Convert polygon points and preserve arc data
    let mut points = vec![Point::new(polygon.begin.x, polygon.begin.y)];
    let mut arc_segments = vec![None]; // First point has no arc (starting point)

    for step in &polygon.steps {
        match step {
            crate::PolyStep::Segment(seg) => {
                points.push(Point::new(seg.x, seg.y));
                arc_segments.push(None); // Straight line
            }
            crate::PolyStep::Curve(curve) => {
                // Preserve arc data for Stage 3 rendering with Skia
                points.push(Point::new(curve.x, curve.y));
                arc_segments.push(Some(super::resolved_feature::ArcSegment {
                    center: Point::new(curve.center_x, curve.center_y),
                    clockwise: curve.clockwise,
                }));
            }
        }
    }

    // Calculate bbox
    let mut bbox = BoundingBox::from_point(points[0]);
    for p in &points[1..] {
        bbox.expand_to_point(*p);
    }

    let geometry = ResolvedGeometry::Polygon {
        points,
        arc_segments,
        cutouts: vec![],
        cutout_arcs: vec![],
    };

    ResolvedFeature {
        bucket: FeatureBucket::Fill,
        net,
        polarity,
        geometry,
        bbox,
    }
}

/// Resolve a line (straight trace segment from Features > UserSpecial > Line)
fn resolve_line(
    line: &crate::ecad::Line,
    net: Option<Symbol>,
    polarity: Polarity,
) -> ResolvedFeature {
    let points = vec![
        Point::new(line.start_x, line.start_y),
        Point::new(line.end_x, line.end_y),
    ];

    // Line already has width in mm (parser applied units::to_mm)
    let line_width = line.line_width;
    let half_width = line_width / 2.0;

    let mut bbox = BoundingBox::from_point(points[0]);
    bbox.expand_to_point(points[1]);
    bbox = BoundingBox {
        min_x: bbox.min_x - half_width,
        min_y: bbox.min_y - half_width,
        max_x: bbox.max_x + half_width,
        max_y: bbox.max_y + half_width,
    };

    let line_end = line
        .line_end
        .map(|le| match le {
            crate::LineEnd::Round => LineEndStyle::Round,
            crate::LineEnd::Square => LineEndStyle::Square,
            crate::LineEnd::Flat => LineEndStyle::None,
        })
        .unwrap_or(LineEndStyle::Round);

    ResolvedFeature {
        bucket: FeatureBucket::Trace,
        net,
        polarity,
        geometry: ResolvedGeometry::Polyline {
            arc_segments: vec![None; points.len()], // Lines have no arc segments
            points,
            line_width,
            line_end,
        },
        bbox,
    }
}

/// Resolve an arc (curved trace segment from Features > Arc)
fn resolve_arc(
    _doc: &crate::Ipc2581,
    _context: &BoardContext,
    arc: &crate::Arc,
    net: Option<Symbol>,
    polarity: Polarity,
) -> Option<ResolvedFeature> {
    // Arc has start, end, and center points
    let points = vec![
        Point::new(arc.start_x, arc.start_y),
        Point::new(arc.end_x, arc.end_y),
    ];

    // Arc segments: first point has no arc (starting point), second point has the arc to it
    let arc_segments = vec![
        None, // First point has no incoming arc
        Some(ArcSegment {
            center: Point::new(arc.center_x, arc.center_y),
            clockwise: arc.clockwise,
        }),
    ];

    // Line width and end are stored directly in Arc (already in mm)
    let line_width = arc.line_width;
    let line_end = arc
        .line_end
        .map(|le| match le {
            crate::LineEnd::Round => LineEndStyle::Round,
            crate::LineEnd::Square => LineEndStyle::Square,
            crate::LineEnd::Flat => LineEndStyle::None,
        })
        .unwrap_or(LineEndStyle::Round);

    let half_width = line_width / 2.0;

    // Calculate bounding box considering arc curvature
    // For now, use a conservative bounding box from start/end points + center
    let mut bbox = BoundingBox::from_point(points[0]);
    bbox.expand_to_point(points[1]);
    bbox.expand_to_point(Point::new(arc.center_x, arc.center_y));
    bbox = BoundingBox {
        min_x: bbox.min_x - half_width,
        min_y: bbox.min_y - half_width,
        max_x: bbox.max_x + half_width,
        max_y: bbox.max_y + half_width,
    };

    Some(ResolvedFeature {
        bucket: FeatureBucket::Trace,
        net,
        polarity,
        geometry: ResolvedGeometry::Polyline {
            points,
            arc_segments,
            line_width,
            line_end,
        },
        bbox,
    })
}
