/// Complete copper layer rendering with board outline, holes, and copper features
///
/// Generates full SVG documents matching the style and dimensions of board_outline.rs

use crate::{
    geometry, LayerFeature, LayerFunction, StandardPrimitive, FeatureSet, Polarity,
    PadStackDef, Symbol, Polygon, PolyStep, LineDesc, PadUse,
};
use geo::{Polygon as GeoPolygon, LineString, Coord, BooleanOps, MultiPolygon};
use std::collections::HashMap;
use svg::node::element::{path::Data, Path, Circle, Group, Mask, Rectangle, Definitions};
use svg::Document;
use lyon_path::{Path as LyonPath, math::Point as LyonPoint};
use lyon_tessellation::{StrokeOptions, StrokeTessellator, BuffersBuilder, VertexBuffers, StrokeVertex, LineCap, LineJoin};

pub struct BoardGeometry<'a> {
    pub outline: &'a Polygon,
    pub cutouts: &'a [Polygon],
    pub slots: &'a [(Polygon, f64, f64)],
    pub npths: &'a [(f64, f64, f64)],
    pub pths: &'a [(f64, f64, f64, crate::board_outline::PadShape)],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerSide {
    Top,
    Bottom,
    Inner,
}

/// Render a complete copper layer SVG with board outline and copper features
pub fn render_copper_layer_svg(
    layer_feature: &LayerFeature,
    layer_function: LayerFunction,
    layer_polarity: Option<Polarity>,
    layer_ref: Symbol,
    layer_side: LayerSide,
    padstack_defs: &HashMap<Symbol, PadStackDef>,
    standard_primitives: &HashMap<Symbol, StandardPrimitive>,
    line_descs: &HashMap<Symbol, LineDesc>,
    board_geom: &BoardGeometry,
) -> Option<String> {
    // Only process copper layers
    if !matches!(
        layer_function,
        LayerFunction::Conductor | LayerFunction::Signal | LayerFunction::Mixed | LayerFunction::Plane
    ) {
        return None;
    }

    // Default polarity: Plane layers are Negative, others are Positive
    let default_polarity = match layer_function {
        LayerFunction::Plane => Polarity::Negative,
        _ => Polarity::Positive,
    };
    let layer_default_polarity = layer_polarity.unwrap_or(default_polarity);

    // Extract copper geometry
    let mut positive_geometry: Vec<GeoPolygon<f64>> = Vec::new();
    let mut negative_geometry: Vec<GeoPolygon<f64>> = Vec::new();

    for set in &layer_feature.sets {
        let set_polarity = set.polarity.unwrap_or(layer_default_polarity);
        let (pos_polys, neg_polys) = extract_set_geometry(
            set,
            layer_ref,
            padstack_defs,
            standard_primitives,
            line_descs,
        );
        
        match set_polarity {
            Polarity::Positive => {
                positive_geometry.extend(pos_polys);
                negative_geometry.extend(neg_polys);
            }
            Polarity::Negative => {
                positive_geometry.extend(neg_polys);
                negative_geometry.extend(pos_polys);
            }
        }
    }

    // PTH pads will be rendered separately AFTER holes (for z-order)

    // FAST PATH: Skip expensive boolean ops, use SVG masks instead
    // Positive geometry uses nonzero fill-rule (overlaps auto-union)
    // Negative geometry rendered as black in a mask (subtracts)

    // Calculate dimensions from board outline
    let (width, height) = geometry::calculate_board_outline_dimensions(
        board_geom.outline,
        board_geom.cutouts,
        board_geom.slots,
    );

    let (min_x, min_y, _, _) = geometry::polygon_bounding_box(board_geom.outline, 0.0, 0.0);

    // Scale to 800px display (2x for better debugging)
    let scale = 1600.0 / width.max(height);
    let padding = 20.0; // Minimal padding for copper layers
    let svg_width = (width * scale).round() + padding * 2.0;
    let svg_height = (height * scale).round() + padding * 2.0;
    let display_width = svg_width / 2.0;
    let display_height = svg_height / 2.0;
    let offset_x = padding;
    let offset_y = padding;

    // Helper to convert polygon to SVG path data
    let add_polygon =
        |mut data: Data, poly: &Polygon, x_offset: f64, y_offset: f64, close: bool| -> Data {
            let mut current_x = poly.begin.x + x_offset;
            let mut current_y = poly.begin.y + y_offset;
            data = data.move_to((
                (current_x - min_x) * scale + offset_x,
                (current_y - min_y) * scale + offset_y,
            ));

            for poly_step in &poly.steps {
                match poly_step {
                    PolyStep::Segment(s) => {
                        current_x = s.x + x_offset;
                        current_y = s.y + y_offset;
                        data = data.line_to((
                            (current_x - min_x) * scale + offset_x,
                            (current_y - min_y) * scale + offset_y,
                        ));
                    }
                    PolyStep::Curve(c) => {
                        let arc = geometry::create_arc(
                            current_x,
                            current_y,
                            c.x + x_offset,
                            c.y + y_offset,
                            c.center_x + x_offset,
                            c.center_y + y_offset,
                            c.clockwise,
                        );

                        // Convert arc to cubic beziers with tight tolerance
                        let mut beziers = Vec::new();
                        arc.to_cubic_beziers(0.01, |p1, p2, p3| {
                            beziers.push((p1, p2, p3));
                        });

                        for (p1, p2, p3) in beziers {
                            data = data.cubic_curve_to((
                                (p1.x - min_x) * scale + offset_x,
                                (p1.y - min_y) * scale + offset_y,
                                (p2.x - min_x) * scale + offset_x,
                                (p2.y - min_y) * scale + offset_y,
                                (p3.x - min_x) * scale + offset_x,
                                (p3.y - min_y) * scale + offset_y,
                            ));
                        }

                        current_x = c.x + x_offset;
                        current_y = c.y + y_offset;
                    }
                }
            }

            if close {
                data.close()
            } else {
                data
            }
        };

    // Build board outline path
    let mut board_path_data = Data::new();
    board_path_data = add_polygon(board_path_data, board_geom.outline, 0.0, 0.0, true);
    for cutout in board_geom.cutouts {
        board_path_data = add_polygon(board_path_data, cutout, 0.0, 0.0, true);
    }

    let board_outline = Path::new()
        .set("fill", "none")
        .set("stroke", "#333")
        .set("stroke-width", 2)
        .set("stroke-linejoin", "round")
        .set("stroke-linecap", "round")
        .set("fill-rule", "evenodd")
        .set("d", board_path_data);

    // Build positive copper path (nonzero fill-rule makes overlaps union)
    let pos_data = geo_polygons_to_svg_data(&positive_geometry, min_x, min_y, scale, offset_x, offset_y);
    
    let copper_path = Path::new()
        .set("fill", "#C87533")
        .set("fill-rule", "nonzero")
        .set("stroke", "none")
        .set("d", pos_data);

    // Build definitions with mask for negatives
    let mut defs = Definitions::new();
    
    if !negative_geometry.is_empty() {
        let neg_data = geo_polygons_to_svg_data(&negative_geometry, min_x, min_y, scale, offset_x, offset_y);
        
        // Mask: white background (allow all) + black negatives (cut out)
        let mask_rect = Rectangle::new()
            .set("x", 0)
            .set("y", 0)
            .set("width", svg_width)
            .set("height", svg_height)
            .set("fill", "white");
        
        let mask_cutout = Path::new()
            .set("fill", "black")
            .set("fill-rule", "nonzero")
            .set("d", neg_data);
        
        let mask = Mask::new()
            .set("id", "neg-mask")
            .set("maskUnits", "userSpaceOnUse")
            .set("maskContentUnits", "userSpaceOnUse")
            .add(mask_rect)
            .add(mask_cutout);
        
        defs = defs.add(mask);
    }

    // Copper group with opacity and optional mask
    let mut copper_group = Group::new().set("opacity", 0.8);
    if !negative_geometry.is_empty() {
        copper_group = copper_group.set("mask", "url(#neg-mask)");
    }
    copper_group = copper_group.add(copper_path);

    let mut document = Document::new()
        .set("viewBox", (0, 0, svg_width, svg_height))
        .set("width", display_width)
        .set("height", display_height)
        .add(defs)
        .add(copper_group)
        .add(board_outline);

    // Add NPTHs
    for (x, y, diameter) in board_geom.npths {
        let cx = (x - min_x) * scale + offset_x;
        let cy = (y - min_y) * scale + offset_y;
        let radius = (diameter / 2.0) * scale;

        let circle = Circle::new()
            .set("cx", cx)
            .set("cy", cy)
            .set("r", radius)
            .set("fill", "white")
            .set("stroke", "#999")
            .set("stroke-width", 1);
        document = document.add(circle);
    }

    // Add slots
    for (slot_outline, x_offset_slot, y_offset_slot) in board_geom.slots {
        let slot_path_data = add_polygon(Data::new(), slot_outline, *x_offset_slot, *y_offset_slot, true);
        let slot_path = Path::new()
            .set("fill", "white")
            .set("stroke", "#999")
            .set("stroke-width", 1)
            .set("d", slot_path_data);
        document = document.add(slot_path);
    }

    // Add PTHs (render hole + annular ring on top/bottom layers)
    for (x, y, hole_diameter, pad_shape) in board_geom.pths {
        let cx = (x - min_x) * scale + offset_x;
        let cy = (y - min_y) * scale + offset_y;
        let hole_radius = (hole_diameter / 2.0) * scale;

        // Render annular ring (PTH pad) on top/bottom layers FIRST
        if matches!(layer_side, LayerSide::Top | LayerSide::Bottom) {
            if let Some(pad_poly) = padshape_to_polygon(pad_shape, *x, *y) {
                let pad_data = geo_polygons_to_svg_data(&[pad_poly], min_x, min_y, scale, offset_x, offset_y);
                let pad_path = Path::new()
                    .set("fill", "#C87533")
                    .set("fill-rule", "nonzero")
                    .set("stroke", "none")
                    .set("d", pad_data);
                document = document.add(pad_path);
            }
        }

        // Then render hole (covers the center)
        let hole_circle = Circle::new()
            .set("cx", cx)
            .set("cy", cy)
            .set("r", hole_radius)
            .set("fill", "white")
            .set("stroke", "#999")
            .set("stroke-width", 1);
        document = document.add(hole_circle);
    }

    Some(document.to_string())
}

fn extract_set_geometry(
    set: &FeatureSet,
    layer_ref: Symbol,
    padstack_defs: &HashMap<Symbol, PadStackDef>,
    standard_primitives: &HashMap<Symbol, StandardPrimitive>,
    line_descs: &HashMap<Symbol, LineDesc>,
) -> (Vec<GeoPolygon<f64>>, Vec<GeoPolygon<f64>>) {
    let mut positive_polygons = Vec::new();
    let mut negative_polygons = Vec::new();

    // Extract pads - filter by layer_ref and route by pad_use
    for pad in &set.pads {
        if let (Some(x), Some(y)) = (pad.x, pad.y) {
            let rotation = pad.rotation.unwrap_or(0.0);
            
            if let Some(padstack_ref) = pad.padstack_def_ref {
                if let Some(padstack) = padstack_defs.get(&padstack_ref) {
                    // Filter pad_defs by layer_ref matching current layer
                    for pad_def in &padstack.pad_defs {
                        if pad_def.layer_ref != layer_ref {
                            continue;
                        }
                        
                        if let Some(prim_ref) = pad_def.standard_primitive_ref {
                            if let Some(primitive) = standard_primitives.get(&prim_ref) {
                                if let Some(pad_poly) = primitive_to_polygon(primitive, x, y, rotation) {
                                    // Route by pad_use
                                    match pad_def.pad_use {
                                        PadUse::Antipad => negative_polygons.push(pad_poly),
                                        PadUse::Regular | PadUse::Thermal => positive_polygons.push(pad_poly),
                                    }
                                }
                                break;
                            }
                        }
                    }
                }
            } else {
                positive_polygons.push(create_circle_polygon(x, y, 0.5, 16));
            }
        }
    }

    // Extract traces - expand to filled polygons using line_width
    for trace in &set.traces {
        let line_width = trace.line_desc_ref
            .and_then(|r| line_descs.get(&r))
            .map(|ld| ld.line_width)
            .unwrap_or(0.25); // Default 0.25mm if no LineDesc
        
        if let Some(trace_polys) = stroke_trace_to_polygons(trace, line_width) {
            positive_polygons.extend(trace_polys);
        }
    }

    // Extract copper pours from Features > Polygon
    for polygon in &set.polygons {
        if let Some(geo_poly) = ipc_polygon_to_geo_polygon(polygon, 0.0, 0.0) {
            positive_polygons.push(geo_poly);
        }
    }

    // Extract traces from Features > UserSpecial > Line
    // Use lyon stroke tessellation for manufacturing-grade accuracy
    if !set.lines.is_empty() {
        if let Some(line_polys) = stroke_lines_with_lyon(&set.lines) {
            positive_polygons.extend(line_polys);
        }
    }

    (positive_polygons, negative_polygons)
}

/// Stroke line segments using lyon for manufacturing-grade accuracy
/// Groups consecutive connected lines into continuous paths for proper stroking
fn stroke_lines_with_lyon(lines: &[crate::ecad::Line]) -> Option<Vec<GeoPolygon<f64>>> {
    if lines.is_empty() {
        return None;
    }
    
    // Group consecutive lines into continuous paths
    let paths = group_lines_into_paths(lines);
    
    let mut polygons = Vec::new();
    
    // Process each continuous path with lyon stroke
    for (path_lines, line_width) in paths {
        if path_lines.is_empty() {
            continue;
        }
        
        let mut builder = LyonPath::builder();
        
        // Start path at first line's start point
        builder.begin(LyonPoint::new(
            path_lines[0].start_x as f32,
            path_lines[0].start_y as f32,
        ));
        
        // Add all line endpoints to create continuous path
        for line in &path_lines {
            builder.line_to(LyonPoint::new(line.end_x as f32, line.end_y as f32));
        }
        
        builder.end(false);
        let path = builder.build();
        
        // Tessellate the stroke
        let mut geometry: VertexBuffers<LyonPoint, u16> = VertexBuffers::new();
        let mut tessellator = StrokeTessellator::new();
        
        let stroke_options = StrokeOptions::default()
            .with_line_width(line_width as f32)
            .with_line_cap(LineCap::Round)
            .with_line_join(LineJoin::Round)
            .with_tolerance(0.001); // 1 micron tolerance
        
        tessellator
            .tessellate_path(
                &path,
                &stroke_options,
                &mut BuffersBuilder::new(&mut geometry, |vertex: StrokeVertex| {
                    vertex.position()
                }),
            )
            .ok()?;
        
        // Convert triangle mesh to polygon outline
        if let Some(poly) = extract_outline_from_mesh(&geometry) {
            polygons.push(poly);
        }
    }
    
    Some(polygons)
}

/// Group consecutive connected lines into continuous paths
/// Lines are grouped if they share endpoints and have the same line_width
fn group_lines_into_paths(lines: &[crate::ecad::Line]) -> Vec<(Vec<crate::ecad::Line>, f64)> {
    if lines.is_empty() {
        return Vec::new();
    }
    
    let mut paths = Vec::new();
    let mut current_path = vec![lines[0].clone()];
    let mut current_width = lines[0].line_width;
    
    for line in lines.iter().skip(1) {
        // Check if this line continues from the previous one
        let last = current_path.last().unwrap();
        let epsilon = 1e-6;
        let continues = (last.end_x - line.start_x).abs() < epsilon
            && (last.end_y - line.start_y).abs() < epsilon
            && (current_width - line.line_width).abs() < epsilon;
        
        if continues {
            // Add to current path
            current_path.push(line.clone());
        } else {
            // Start new path
            paths.push((current_path, current_width));
            current_path = vec![line.clone()];
            current_width = line.line_width;
        }
    }
    
    // Don't forget the last path
    if !current_path.is_empty() {
        paths.push((current_path, current_width));
    }
    
    paths
}

/// Extract polygon outline from triangle mesh
fn extract_outline_from_mesh(buffers: &VertexBuffers<LyonPoint, u16>) -> Option<GeoPolygon<f64>> {
    if buffers.vertices.is_empty() {
        return None;
    }
    
    // Build edge map to find boundary edges
    let mut edges: HashMap<(u64, u64), usize> = HashMap::new();
    
    for triangle in buffers.indices.chunks(3) {
        let i0 = triangle[0] as usize;
        let i1 = triangle[1] as usize;
        let i2 = triangle[2] as usize;
        
        // Add each edge (sorted vertices as key)
        for &(a, b) in &[(i0, i1), (i1, i2), (i2, i0)] {
            let key = if a < b { (a as u64, b as u64) } else { (b as u64, a as u64) };
            *edges.entry(key).or_insert(0) += 1;
        }
    }
    
    // Boundary edges appear exactly once
    let boundary_edges: Vec<(usize, usize)> = edges
        .iter()
        .filter(|(_, &count)| count == 1)
        .map(|((a, b), _)| (*a as usize, *b as usize))
        .collect();
    
    if boundary_edges.is_empty() {
        return None;
    }
    
    // Build ordered boundary loop
    let mut coords = Vec::new();
    let mut visited = vec![false; boundary_edges.len()];
    visited[0] = true;
    
    let (current_v, mut next_v) = boundary_edges[0];
    coords.push(Coord {
        x: buffers.vertices[current_v].x as f64,
        y: buffers.vertices[current_v].y as f64,
    });
    
    // Follow the boundary
    for _ in 0..boundary_edges.len() {
        coords.push(Coord {
            x: buffers.vertices[next_v].x as f64,
            y: buffers.vertices[next_v].y as f64,
        });
        
        // Find next edge
        let mut found = false;
        for (i, &(a, b)) in boundary_edges.iter().enumerate() {
            if !visited[i] {
                if a == next_v {
                    next_v = b;
                    visited[i] = true;
                    found = true;
                    break;
                } else if b == next_v {
                    next_v = a;
                    visited[i] = true;
                    found = true;
                    break;
                }
            }
        }
        
        if !found {
            break;
        }
    }
    
    // Close polygon
    if !coords.is_empty() && coords[0] != *coords.last().unwrap() {
        coords.push(coords[0]);
    }
    
    if coords.len() >= 4 {
        Some(GeoPolygon::new(LineString::from(coords), vec![]))
    } else {
        None
    }
}

/// Convert trace polylines to filled polygons by stroking with line_width
fn stroke_trace_to_polygons(
    trace: &crate::Trace,
    line_width: f64,
) -> Option<Vec<GeoPolygon<f64>>> {
    if trace.points.len() < 2 {
        return None;
    }

    let mut polygons = Vec::new();
    let half_width = line_width / 2.0;

    // Create rectangles for each segment
    for i in 0..trace.points.len() - 1 {
        let p1 = &trace.points[i];
        let p2 = &trace.points[i + 1];

        let dx = p2.x - p1.x;
        let dy = p2.y - p1.y;
        let length = (dx * dx + dy * dy).sqrt();

        if length < 1e-9 {
            continue;
        }

        // Unit vector along the line
        let ux = dx / length;
        let uy = dy / length;

        // Perpendicular unit vector
        let px = -uy;
        let py = ux;

        // Rectangle corners
        let corners = vec![
            Coord { x: p1.x + px * half_width, y: p1.y + py * half_width },
            Coord { x: p2.x + px * half_width, y: p2.y + py * half_width },
            Coord { x: p2.x - px * half_width, y: p2.y - py * half_width },
            Coord { x: p1.x - px * half_width, y: p1.y - py * half_width },
            Coord { x: p1.x + px * half_width, y: p1.y + py * half_width }, // Close
        ];

        polygons.push(GeoPolygon::new(LineString::from(corners), vec![]));
    }

    // Add round caps at endpoints
    polygons.push(create_circle_polygon(
        trace.points[0].x,
        trace.points[0].y,
        half_width,
        16,
    ));
    polygons.push(create_circle_polygon(
        trace.points[trace.points.len() - 1].x,
        trace.points[trace.points.len() - 1].y,
        half_width,
        16,
    ));

    Some(polygons)
}

fn geo_polygons_to_svg_data(
    polygons: &[GeoPolygon<f64>],
    min_x: f64,
    min_y: f64,
    scale: f64,
    offset_x: f64,
    offset_y: f64,
) -> Data {
    let mut data = Data::new();
    
    for polygon in polygons {
        // CRITICAL: Normalize winding to prevent overlaps from cancelling
        let normalized = orient_polygon_standard(polygon);
        
        let exterior = normalized.exterior();
        if let Some(first) = exterior.0.first() {
            let sx = (first.x - min_x) * scale + offset_x;
            let sy = (first.y - min_y) * scale + offset_y;
            data = data.move_to((sx, sy));
            
            for coord in exterior.0.iter().skip(1) {
                let sx = (coord.x - min_x) * scale + offset_x;
                let sy = (coord.y - min_y) * scale + offset_y;
                data = data.line_to((sx, sy));
            }
            data = data.close();
        }
        
        // Handle holes
        for interior in normalized.interiors() {
            if let Some(first) = interior.0.first() {
                let sx = (first.x - min_x) * scale + offset_x;
                let sy = (first.y - min_y) * scale + offset_y;
                data = data.move_to((sx, sy));
                
                for coord in interior.0.iter().skip(1) {
                    let sx = (coord.x - min_x) * scale + offset_x;
                    let sy = (coord.y - min_y) * scale + offset_y;
                    data = data.line_to((sx, sy));
                }
                data = data.close();
            }
        }
    }
    
    data
}

/// Calculate signed area of a LineString (positive = CCW, negative = CW)
fn ls_signed_area(ls: &LineString<f64>) -> f64 {
    let pts = &ls.0;
    if pts.len() < 3 {
        return 0.0;
    }
    
    let mut s = 0.0;
    for i in 0..pts.len() - 1 {
        s += pts[i].x * pts[i + 1].y - pts[i + 1].x * pts[i].y;
    }
    s * 0.5
}

/// Orient LineString to desired winding direction
fn orient_ls(ls: &LineString<f64>, want_ccw: bool) -> LineString<f64> {
    let ccw = ls_signed_area(ls) > 0.0;
    if ccw == want_ccw {
        ls.clone()
    } else {
        let mut rev = ls.0.clone();
        rev.reverse();
        LineString::from(rev)
    }
}

/// Normalize polygon: exterior CCW, holes CW
fn orient_polygon_standard(poly: &GeoPolygon<f64>) -> GeoPolygon<f64> {
    let ext = orient_ls(poly.exterior(), true); // exterior CCW
    let holes = poly.interiors().iter().map(|h| orient_ls(h, false)).collect();
    GeoPolygon::new(ext, holes)
}

fn ipc_polygon_to_geo_polygon(
    polygon: &Polygon,
    offset_x: f64,
    offset_y: f64,
) -> Option<GeoPolygon<f64>> {
    let mut coords = Vec::new();
    coords.push(Coord { x: polygon.begin.x + offset_x, y: polygon.begin.y + offset_y });
    
    for step in &polygon.steps {
        match step {
            PolyStep::Segment(seg) => {
                coords.push(Coord { x: seg.x + offset_x, y: seg.y + offset_y });
            }
            PolyStep::Curve(curve) => {
                // TODO: Tessellate arc properly
                coords.push(Coord { x: curve.x + offset_x, y: curve.y + offset_y });
            }
        }
    }
    
    if !coords.is_empty() && coords[0] != *coords.last().unwrap() {
        coords.push(coords[0]);
    }
    
    if coords.len() >= 4 {
        Some(GeoPolygon::new(LineString::from(coords), vec![]))
    } else {
        None
    }
}

fn primitive_to_polygon(
    primitive: &StandardPrimitive,
    x: f64,
    y: f64,
    rotation: f64,
) -> Option<GeoPolygon<f64>> {
    match primitive {
        StandardPrimitive::Circle(circle) => {
            Some(create_circle_polygon(x, y, circle.diameter / 2.0, 32))
        }
        StandardPrimitive::RectCenter(rect) => {
            Some(create_rectangle_polygon(x, y, rect.width, rect.height, rotation))
        }
        StandardPrimitive::Oval(oval) => {
            Some(create_oval_polygon(x, y, oval.width, oval.height, rotation))
        }
        StandardPrimitive::RectRound(rect) => {
            Some(create_rounded_rect_polygon(x, y, rect.width, rect.height, rect.radius, rotation))
        }
        _ => Some(create_circle_polygon(x, y, 0.5, 16)), // Fallback
    }
}

/// Union all polygons together (critical for correct rendering)
fn union_all_polygons(polygons: Vec<GeoPolygon<f64>>) -> MultiPolygon<f64> {
    if polygons.is_empty() {
        return MultiPolygon(vec![]);
    }
    
    // Start with first polygon
    let mut result = MultiPolygon(vec![polygons[0].clone()]);
    
    // Union each subsequent polygon
    for poly in polygons.iter().skip(1) {
        result = result.union(&MultiPolygon(vec![poly.clone()]));
    }
    
    result
}

/// Convert PadShape from board_outline to geo::Polygon
fn padshape_to_polygon(shape: &crate::board_outline::PadShape, x: f64, y: f64) -> Option<GeoPolygon<f64>> {
    use crate::board_outline::PadShape;
    
    match shape {
        PadShape::Circle { diameter } => {
            Some(create_circle_polygon(x, y, diameter / 2.0, 32))
        }
        PadShape::Rect { width, height } => {
            Some(create_rectangle_polygon(x, y, *width, *height, 0.0))
        }
        PadShape::Oval { width, height } => {
            Some(create_oval_polygon(x, y, *width, *height, 0.0))
        }
        PadShape::Polygon { polygon } => {
            ipc_polygon_to_geo_polygon(polygon, x, y)
        }
        PadShape::Composite { shapes } => {
            // For composite, union all shapes
            let mut all_polys = Vec::new();
            for s in shapes {
                if let Some(p) = padshape_to_polygon(s, x, y) {
                    all_polys.push(p);
                }
            }
            union_all_polygons(all_polys).into_iter().next()
        }
    }
}

fn create_circle_polygon(cx: f64, cy: f64, radius: f64, segments: usize) -> GeoPolygon<f64> {
    let mut coords = Vec::with_capacity(segments + 1);
    for i in 0..segments {
        let angle = 2.0 * std::f64::consts::PI * (i as f64) / (segments as f64);
        coords.push(Coord { x: cx + radius * angle.cos(), y: cy + radius * angle.sin() });
    }
    coords.push(coords[0]);
    GeoPolygon::new(LineString::from(coords), vec![])
}

fn create_rectangle_polygon(cx: f64, cy: f64, width: f64, height: f64, rotation: f64) -> GeoPolygon<f64> {
    let half_w = width / 2.0;
    let half_h = height / 2.0;
    let mut corners = vec![(-half_w, -half_h), (half_w, -half_h), (half_w, half_h), (-half_w, half_h)];
    
    if rotation.abs() > 1e-6 {
        let cos_r = rotation.to_radians().cos();
        let sin_r = rotation.to_radians().sin();
        for (x, y) in &mut corners {
            let (rx, ry) = (*x * cos_r - *y * sin_r, *x * sin_r + *y * cos_r);
            *x = rx;
            *y = ry;
        }
    }
    
    let mut coords: Vec<Coord<f64>> = corners.iter().map(|(x, y)| Coord { x: cx + x, y: cy + y }).collect();
    coords.push(coords[0]);
    GeoPolygon::new(LineString::from(coords), vec![])
}

fn create_oval_polygon(cx: f64, cy: f64, width: f64, height: f64, rotation: f64) -> GeoPolygon<f64> {
    let segments = 32;
    let mut coords = Vec::with_capacity(segments + 1);
    
    for i in 0..segments {
        let angle = 2.0 * std::f64::consts::PI * (i as f64) / (segments as f64);
        let (mut x, mut y) = if width > height {
            let r = height / 2.0;
            let half_len = (width - height) / 2.0;
            if angle < std::f64::consts::PI / 2.0 || angle > 3.0 * std::f64::consts::PI / 2.0 {
                (half_len + r * angle.cos(), r * angle.sin())
            } else {
                (-half_len + r * angle.cos(), r * angle.sin())
            }
        } else {
            let r = width / 2.0;
            let half_len = (height - width) / 2.0;
            if angle < std::f64::consts::PI {
                (r * angle.cos(), half_len + r * angle.sin())
            } else {
                (r * angle.cos(), -half_len + r * angle.sin())
            }
        };
        
        if rotation.abs() > 1e-6 {
            let cos_r = rotation.to_radians().cos();
            let sin_r = rotation.to_radians().sin();
            (x, y) = (x * cos_r - y * sin_r, x * sin_r + y * cos_r);
        }
        
        coords.push(Coord { x: cx + x, y: cy + y });
    }
    coords.push(coords[0]);
    GeoPolygon::new(LineString::from(coords), vec![])
}

fn create_rounded_rect_polygon(cx: f64, cy: f64, width: f64, height: f64, _corner_radius: f64, rotation: f64) -> GeoPolygon<f64> {
    // Simplified - just use rectangle for now
    create_rectangle_polygon(cx, cy, width, height, rotation)
}
