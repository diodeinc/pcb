use crate::{PolyStep, Polygon};
use kurbo::{Arc as KurboArc, Point, Rect, Shape, Vec2};
use svg::node::element::{path::Data, Path};
use svg::Document;

/// Pad shape for rendering PTHs
#[derive(Debug, Clone)]
pub enum PadShape {
    Circle { diameter: f64 },
    Rect { width: f64, height: f64 },
    Oval { width: f64, height: f64 },
}

pub struct BoardOutlineData<'a> {
    pub outline: &'a Polygon,
    pub cutouts: &'a [Polygon],
    pub slots: &'a [(Polygon, f64, f64)], // (outline, x_offset, y_offset)
    pub npths: &'a [(f64, f64, f64)],     // (x, y, diameter)
    pub pths: &'a [(f64, f64, f64, PadShape)], // (x, y, hole_diameter, pad_shape)
}

// Helper to create kurbo Arc from IPC-2581 curve data
fn create_arc(
    start_x: f64,
    start_y: f64,
    end_x: f64,
    end_y: f64,
    center_x: f64,
    center_y: f64,
    clockwise: bool,
) -> KurboArc {
    let start_angle = (start_y - center_y).atan2(start_x - center_x);
    let end_angle = (end_y - center_y).atan2(end_x - center_x);
    let radius = ((start_x - center_x).powi(2) + (start_y - center_y).powi(2)).sqrt();

    let mut sweep_angle = end_angle - start_angle;
    if clockwise {
        if sweep_angle > 0.0 {
            sweep_angle -= 2.0 * std::f64::consts::PI;
        }
    } else if sweep_angle < 0.0 {
        sweep_angle += 2.0 * std::f64::consts::PI;
    }

    KurboArc::new(
        Point::new(center_x, center_y),
        Vec2::new(radius, radius),
        start_angle,
        sweep_angle,
        0.0,
    )
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
                let arc = create_arc(
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

    // Scale to 500px max dimension with 2x oversampling for retina displays
    let scale = 1000.0 / width.max(height);
    let svg_width = (width * scale).round();
    let svg_height = (height * scale).round();
    let display_width = svg_width / 2.0;
    let display_height = svg_height / 2.0;

    // Helper to convert polygon to SVG path data
    let add_polygon =
        |mut data: Data, poly: &Polygon, x_offset: f64, y_offset: f64, close: bool| -> Data {
            let mut current_x = poly.begin.x + x_offset;
            let mut current_y = poly.begin.y + y_offset;
            data = data.move_to(((current_x - min_x) * scale, (current_y - min_y) * scale));

            for poly_step in &poly.steps {
                match poly_step {
                    PolyStep::Segment(s) => {
                        current_x = s.x + x_offset;
                        current_y = s.y + y_offset;
                        data = data
                            .line_to(((current_x - min_x) * scale, (current_y - min_y) * scale));
                    }
                    PolyStep::Curve(c) => {
                        let arc = create_arc(
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
                                (p1.x - min_x) * scale,
                                (p1.y - min_y) * scale,
                                (p2.x - min_x) * scale,
                                (p2.y - min_y) * scale,
                                (p3.x - min_x) * scale,
                                (p3.y - min_y) * scale,
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
        let cx = (x - min_x) * scale;
        let cy = (y - min_y) * scale;
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
        let cx = (x - min_x) * scale;
        let cy = (y - min_y) * scale;

        // Render outer pad shape (copper) with gold color
        match pad_shape {
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

    let mut svg_buffer = Vec::new();
    svg::write(&mut svg_buffer, &document).unwrap();
    String::from_utf8(svg_buffer).unwrap()
}
