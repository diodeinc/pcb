use crate::{geometry, PolyStep, Polygon};
use kurbo::{Point, Rect, Shape};
use svg::node::element::{path::Data, Path};
use svg::Document;

/// Pad shape for rendering PTHs
#[derive(Debug, Clone)]
pub enum PadShape {
    Circle { diameter: f64 },
    Rect { width: f64, height: f64 },
    Oval { width: f64, height: f64 },
    Polygon { polygon: Polygon },
    Composite { shapes: Vec<PadShape> },
}

pub struct BoardOutlineData<'a> {
    pub outline: &'a Polygon,
    pub cutouts: &'a [Polygon],
    pub slots: &'a [(Polygon, f64, f64)], // (outline, x_offset, y_offset)
    pub npths: &'a [(f64, f64, f64)],     // (x, y, diameter)
    pub pths: &'a [(f64, f64, f64, PadShape)], // (x, y, hole_diameter, pad_shape)
}

// Helper to update bounds for a polygon
fn update_bounds(bounds: &mut Rect, poly: &Polygon, x_offset: f64, y_offset: f64) {
    let mut current_x = poly.begin.x + x_offset;
    let mut current_y = poly.begin.y + y_offset;
    *bounds = bounds.union_pt(Point::new(current_x, current_y));

    for poly_step in &poly.steps {
        match poly_step {
            PolyStep::Segment(s) => {
                current_x = s.x + x_offset;
                current_y = s.y + y_offset;
                *bounds = bounds.union_pt(Point::new(current_x, current_y));
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
                *bounds = bounds.union(arc.bounding_box());
                current_x = c.x + x_offset;
                current_y = c.y + y_offset;
            }
        }
    }
}

pub fn render_board_outline_svg(data: BoardOutlineData) -> String {
    let polygon = data.outline;

    // Calculate bounds
    let mut bounds = Rect::new(
        polygon.begin.x,
        polygon.begin.y,
        polygon.begin.x,
        polygon.begin.y,
    );

    update_bounds(&mut bounds, polygon, 0.0, 0.0);

    for cutout in data.cutouts {
        update_bounds(&mut bounds, cutout, 0.0, 0.0);
    }

    for (slot_outline, x_offset, y_offset) in data.slots {
        update_bounds(&mut bounds, slot_outline, *x_offset, *y_offset);
    }

    let min_x = bounds.x0;
    let min_y = bounds.y0;
    let width = bounds.width();
    let height = bounds.height();

    // Scale to 400px display with 2x oversampling for smooth anti-aliasing
    let scale = 800.0 / width.max(height);

    // Add padding for annotations (dimension lines, scale bar)
    let annotation_padding = 120.0; // pixels in SVG space (2x for oversampling)
    let svg_width = (width * scale).round() + annotation_padding * 2.0;
    let svg_height = (height * scale).round() + annotation_padding * 2.0;
    let display_width = svg_width / 2.0;
    let display_height = svg_height / 2.0;

    // Offset for centering board with padding
    let offset_x = annotation_padding;
    let offset_y = annotation_padding;

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

                        // Convert arc to cubic beziers and add to path
                        let mut beziers = Vec::new();
                        arc.to_cubic_beziers(0.1, |p1, p2, p3| {
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

    // Build board outline path with cutouts using evenodd fill rule
    let mut board_path_data = Data::new();
    board_path_data = add_polygon(board_path_data, polygon, 0.0, 0.0, true);

    // Add cutouts to the same path (will be voids due to evenodd)
    for cutout in data.cutouts {
        board_path_data = add_polygon(board_path_data, cutout, 0.0, 0.0, true);
    }

    // Use clip-path to ensure uniform inner stroke
    let clip_path = svg::node::element::ClipPath::new()
        .set("id", "board-clip")
        .add(Path::new().set("d", board_path_data.clone()));

    let stroked_path = Path::new()
        .set("fill", "none")
        .set("stroke", "#333")
        .set("stroke-width", 4)
        .set("stroke-linejoin", "round")
        .set("stroke-linecap", "round")
        .set("shape-rendering", "geometricPrecision")
        .set("clip-path", "url(#board-clip)")
        .set("d", board_path_data.clone());

    let fill_path = Path::new()
        .set("fill", "white")
        .set("stroke", "none")
        .set("fill-rule", "evenodd")
        .set("d", board_path_data);

    let defs = svg::node::element::Definitions::new().add(clip_path);

    let mut document = Document::new()
        .set("viewBox", (0, 0, svg_width, svg_height))
        .set("width", display_width)
        .set("height", display_height)
        .add(defs)
        .add(fill_path)
        .add(stroked_path);

    // Render slots as gray filled polygons
    for (slot_outline, x_offset, y_offset) in data.slots {
        let slot_path_data = add_polygon(Data::new(), slot_outline, *x_offset, *y_offset, true);
        let slot_path = Path::new()
            .set("fill", "#e9ecef")
            .set("stroke", "#333")
            .set("stroke-width", 1)
            .set("stroke-linejoin", "round")
            .set("stroke-linecap", "round")
            .set("shape-rendering", "geometricPrecision")
            .set("d", slot_path_data);
        document = document.add(slot_path);
    }

    // Render NPTHs (non-plated through holes) as circles with same style as slots
    for (x, y, diameter) in data.npths {
        let cx = (x - min_x) * scale + offset_x;
        let cy = (y - min_y) * scale + offset_y;
        let radius = (diameter / 2.0) * scale;

        let circle = svg::node::element::Circle::new()
            .set("cx", cx)
            .set("cy", cy)
            .set("r", radius)
            .set("fill", "#e9ecef")
            .set("stroke", "#333")
            .set("stroke-width", 1)
            .set("shape-rendering", "geometricPrecision");
        document = document.add(circle);
    }

    // Render PTHs (plated through holes) with accurate pad shapes
    for (x, y, hole_diameter, pad_shape) in data.pths {
        let cx = (x - min_x) * scale + offset_x;
        let cy = (y - min_y) * scale + offset_y;

        // Flatten composite shapes into a list of simple shapes
        let mut shapes_to_render = Vec::new();
        let mut stack = vec![pad_shape];
        while let Some(shape) = stack.pop() {
            match shape {
                PadShape::Composite { shapes } => {
                    // Push all shapes from composite onto stack
                    for s in shapes.iter().rev() {
                        stack.push(s);
                    }
                }
                _ => shapes_to_render.push(shape),
            }
        }

        // Render each shape
        for shape in shapes_to_render {
            match shape {
                PadShape::Circle { diameter } => {
                    let circle = svg::node::element::Circle::new()
                        .set("cx", cx)
                        .set("cy", cy)
                        .set("r", (diameter / 2.0) * scale)
                        .set("fill", "#DAA520")
                        .set("stroke", "none")
                        .set("shape-rendering", "geometricPrecision");
                    document = document.add(circle);
                }
                PadShape::Rect { width, height } => {
                    let w = width * scale;
                    let h = height * scale;
                    let rect = svg::node::element::Rectangle::new()
                        .set("x", cx - w / 2.0)
                        .set("y", cy - h / 2.0)
                        .set("width", w)
                        .set("height", h)
                        .set("fill", "#DAA520")
                        .set("stroke", "none")
                        .set("shape-rendering", "geometricPrecision");
                    document = document.add(rect);
                }
                PadShape::Oval { width, height } => {
                    let ellipse = svg::node::element::Ellipse::new()
                        .set("cx", cx)
                        .set("cy", cy)
                        .set("rx", (width / 2.0) * scale)
                        .set("ry", (height / 2.0) * scale)
                        .set("fill", "#DAA520")
                        .set("stroke", "none")
                        .set("shape-rendering", "geometricPrecision");
                    document = document.add(ellipse);
                }
                PadShape::Polygon { polygon } => {
                    // Render polygon pad shape, offset by (x, y) position
                    let poly_path_data = add_polygon(Data::new(), polygon, *x, *y, true);
                    let poly_path = Path::new()
                        .set("fill", "#DAA520")
                        .set("fill-rule", "evenodd")
                        .set("stroke", "none")
                        .set("shape-rendering", "geometricPrecision")
                        .set("d", poly_path_data);
                    document = document.add(poly_path);
                }
                PadShape::Composite { .. } => {
                    // Should already be flattened
                    unreachable!()
                }
            }
        }

        // Render hole (always circular) with darker gold stroke
        let hole = svg::node::element::Circle::new()
            .set("cx", cx)
            .set("cy", cy)
            .set("r", (hole_diameter / 2.0) * scale)
            .set("fill", "#e9ecef")
            .set("stroke", "#8B7500")
            .set("stroke-width", 1)
            .set("shape-rendering", "geometricPrecision");
        document = document.add(hole);
    }

    // Add dimension annotations (all sizes 2x for oversampling)
    let board_width_svg = width * scale;
    let board_height_svg = height * scale;
    let dim_offset = 30.0; // Distance from board edge to dimension line (2x)

    // Width dimension (top) - round coordinates for pixel perfection
    let dim_y = (offset_y - dim_offset).round();
    let dim_x_start = offset_x.round();
    let dim_x_end = (offset_x + board_width_svg).round();

    // Dimension line
    let width_dim_line = svg::node::element::Line::new()
        .set("x1", dim_x_start)
        .set("y1", dim_y)
        .set("x2", dim_x_end)
        .set("y2", dim_y)
        .set("stroke", "#666")
        .set("stroke-width", 2)
        .set("shape-rendering", "crispEdges");
    document = document.add(width_dim_line);

    // Arrow markers
    let arrow_size = 10.0;
    let left_arrow = svg::node::element::Polygon::new()
        .set(
            "points",
            format!(
                "{},{} {},{} {},{}",
                dim_x_start,
                dim_y,
                dim_x_start + arrow_size,
                dim_y - arrow_size / 2.0,
                dim_x_start + arrow_size,
                dim_y + arrow_size / 2.0
            ),
        )
        .set("fill", "#666");
    document = document.add(left_arrow);

    let right_arrow = svg::node::element::Polygon::new()
        .set(
            "points",
            format!(
                "{},{} {},{} {},{}",
                dim_x_end,
                dim_y,
                dim_x_end - arrow_size,
                dim_y - arrow_size / 2.0,
                dim_x_end - arrow_size,
                dim_y + arrow_size / 2.0
            ),
        )
        .set("fill", "#666");
    document = document.add(right_arrow);

    // Text label (width in mm and inches)
    let width_text = format!("{:.1} mm ({:.2}\")", width, width / 25.4);
    let width_label = svg::node::element::Text::new(width_text)
        .set("x", (dim_x_start + dim_x_end) / 2.0)
        .set("y", dim_y - 12.0)
        .set("text-anchor", "middle")
        .set("font-family", "monospace")
        .set("font-size", 26)
        .set("fill", "#333");
    document = document.add(width_label);

    // Height dimension (right side) - round coordinates for pixel perfection
    let dim_x = (offset_x + board_width_svg + dim_offset).round();
    let dim_y_start = offset_y.round();
    let dim_y_end = (offset_y + board_height_svg).round();

    // Dimension line
    let height_dim_line = svg::node::element::Line::new()
        .set("x1", dim_x)
        .set("y1", dim_y_start)
        .set("x2", dim_x)
        .set("y2", dim_y_end)
        .set("stroke", "#666")
        .set("stroke-width", 2)
        .set("shape-rendering", "crispEdges");
    document = document.add(height_dim_line);

    // Arrow markers
    let top_arrow = svg::node::element::Polygon::new()
        .set(
            "points",
            format!(
                "{},{} {},{} {},{}",
                dim_x,
                dim_y_start,
                dim_x - arrow_size / 2.0,
                dim_y_start + arrow_size,
                dim_x + arrow_size / 2.0,
                dim_y_start + arrow_size
            ),
        )
        .set("fill", "#666");
    document = document.add(top_arrow);

    let bottom_arrow = svg::node::element::Polygon::new()
        .set(
            "points",
            format!(
                "{},{} {},{} {},{}",
                dim_x,
                dim_y_end,
                dim_x - arrow_size / 2.0,
                dim_y_end - arrow_size,
                dim_x + arrow_size / 2.0,
                dim_y_end - arrow_size
            ),
        )
        .set("fill", "#666");
    document = document.add(bottom_arrow);

    // Text label (height in mm and inches) - rotated
    let height_text = format!("{:.1} mm ({:.2}\")", height, height / 25.4);
    let height_label = svg::node::element::Text::new(height_text)
        .set("x", dim_x + 12.0)
        .set("y", (dim_y_start + dim_y_end) / 2.0)
        .set("text-anchor", "middle")
        .set("font-family", "monospace")
        .set("font-size", 26)
        .set("fill", "#333")
        .set(
            "transform",
            format!(
                "rotate(90 {} {})",
                dim_x + 12.0,
                (dim_y_start + dim_y_end) / 2.0
            ),
        );
    document = document.add(height_label);

    // Add scale bar (bottom-left corner)
    let scale_bar_length_mm = if width.max(height) > 200.0 {
        40.0 // 40mm for very large boards
    } else if width.max(height) > 100.0 {
        20.0 // 20mm for medium boards
    } else {
        10.0 // 10mm for small boards
    };
    let scale_bar_length_svg = (scale_bar_length_mm * scale).round();
    let scale_bar_x = (offset_x + 20.0).round();
    let scale_bar_y = (offset_y + board_height_svg + 60.0).round();

    // Scale bar line
    let scale_bar = svg::node::element::Line::new()
        .set("x1", scale_bar_x)
        .set("y1", scale_bar_y)
        .set("x2", scale_bar_x + scale_bar_length_svg)
        .set("y2", scale_bar_y)
        .set("stroke", "#333")
        .set("stroke-width", 2)
        .set("shape-rendering", "crispEdges");
    document = document.add(scale_bar);

    // End ticks
    let tick_height = 8.0;
    let left_tick = svg::node::element::Line::new()
        .set("x1", scale_bar_x)
        .set("y1", scale_bar_y - tick_height)
        .set("x2", scale_bar_x)
        .set("y2", scale_bar_y + tick_height)
        .set("stroke", "#333")
        .set("stroke-width", 2)
        .set("shape-rendering", "crispEdges");
    document = document.add(left_tick);

    let right_tick = svg::node::element::Line::new()
        .set("x1", scale_bar_x + scale_bar_length_svg)
        .set("y1", scale_bar_y - tick_height)
        .set("x2", scale_bar_x + scale_bar_length_svg)
        .set("y2", scale_bar_y + tick_height)
        .set("stroke", "#333")
        .set("stroke-width", 2)
        .set("shape-rendering", "crispEdges");
    document = document.add(right_tick);

    // Scale bar label
    let scale_bar_text = format!("{:.0} mm", scale_bar_length_mm);
    let scale_bar_label = svg::node::element::Text::new(scale_bar_text)
        .set("x", scale_bar_x + scale_bar_length_svg / 2.0)
        .set("y", scale_bar_y + 28.0)
        .set("text-anchor", "middle")
        .set("font-family", "monospace")
        .set("font-size", 22)
        .set("fill", "#333");
    document = document.add(scale_bar_label);

    // Add origin marker at (0,0) - round coordinates for pixel perfection
    let origin_x = ((0.0 - min_x) * scale + offset_x).round();
    let origin_y = ((0.0 - min_y) * scale + offset_y).round();
    let origin_radius = 6.0;
    let crosshair_length = 20.0;

    // Check if origin is within visible bounds
    if origin_x >= offset_x
        && origin_x <= offset_x + board_width_svg
        && origin_y >= offset_y
        && origin_y <= offset_y + board_height_svg
    {
        // Circle at origin
        let origin_circle = svg::node::element::Circle::new()
            .set("cx", origin_x)
            .set("cy", origin_y)
            .set("r", origin_radius)
            .set("fill", "none")
            .set("stroke", "#DC3545")
            .set("stroke-width", 2);
        document = document.add(origin_circle);

        // Horizontal crosshair
        let h_crosshair = svg::node::element::Line::new()
            .set("x1", origin_x - crosshair_length)
            .set("y1", origin_y)
            .set("x2", origin_x + crosshair_length)
            .set("y2", origin_y)
            .set("stroke", "#DC3545")
            .set("stroke-width", 2)
            .set("shape-rendering", "crispEdges");
        document = document.add(h_crosshair);

        // Vertical crosshair
        let v_crosshair = svg::node::element::Line::new()
            .set("x1", origin_x)
            .set("y1", origin_y - crosshair_length)
            .set("x2", origin_x)
            .set("y2", origin_y + crosshair_length)
            .set("stroke", "#DC3545")
            .set("stroke-width", 2)
            .set("shape-rendering", "crispEdges");
        document = document.add(v_crosshair);
    }

    let mut svg_buffer = Vec::new();
    svg::write(&mut svg_buffer, &document).unwrap();
    String::from_utf8(svg_buffer).unwrap()
}
