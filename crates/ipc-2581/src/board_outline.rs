use crate::{PolyStep, Polygon};
use kurbo::Point;
use svg::node::element::{path::Data, Path};
use svg::Document;

pub fn render_board_outline_svg(polygon: &Polygon) -> String {
    // Calculate bounds
    let mut min_x = polygon.begin.x;
    let mut max_x = polygon.begin.x;
    let mut min_y = polygon.begin.y;
    let mut max_y = polygon.begin.y;

    for poly_step in &polygon.steps {
        let (x, y) = match poly_step {
            PolyStep::Segment(s) => (s.x, s.y),
            PolyStep::Curve(c) => (c.x, c.y),
        };
        min_x = min_x.min(x);
        max_x = max_x.max(x);
        min_y = min_y.min(y);
        max_y = max_y.max(y);
    }

    let width = max_x - min_x;
    let height = max_y - min_y;

    // Scale to 500px max dimension with 2x oversampling for retina displays
    let scale = 1000.0 / width.max(height);
    let svg_width = (width * scale).round();
    let svg_height = (height * scale).round();
    let display_width = svg_width / 2.0;
    let display_height = svg_height / 2.0;

    // Build simple SVG path - no tessellation, no fancy stuff
    let mut data = Data::new();
    data = data.move_to((
        (polygon.begin.x - min_x) * scale,
        (polygon.begin.y - min_y) * scale,
    ));

    let mut current_x = polygon.begin.x;
    let mut current_y = polygon.begin.y;

    for poly_step in &polygon.steps {
        match poly_step {
            PolyStep::Segment(s) => {
                data = data.line_to(((s.x - min_x) * scale, (s.y - min_y) * scale));
                current_x = s.x;
                current_y = s.y;
            }
            PolyStep::Curve(c) => {
                // Convert to scaled coordinates
                let center = Point::new((c.center_x - min_x) * scale, (c.center_y - min_y) * scale);
                let start = Point::new((current_x - min_x) * scale, (current_y - min_y) * scale);
                let end = Point::new((c.x - min_x) * scale, (c.y - min_y) * scale);

                // Calculate arc parameters
                let radius = (start - center).hypot();
                let angular_span = ((end - center).atan2() - (start - center).atan2()).abs();
                let large_arc = angular_span > std::f64::consts::PI;
                let sweep_flag = !c.clockwise;

                data = data.elliptical_arc_to((
                    radius,
                    radius,
                    0.0,
                    large_arc as u8,
                    sweep_flag as u8,
                    end.x,
                    end.y,
                ));

                current_x = c.x;
                current_y = c.y;
            }
        }
    }

    data = data.close();

    // Use clip-path to ensure uniform inner stroke (prevents corner bulge)
    let clip_path = svg::node::element::ClipPath::new()
        .set("id", "board-clip")
        .add(Path::new().set("d", data.clone()));

    let stroked_path = Path::new()
        .set("fill", "none")
        .set("stroke", "#333")
        .set("stroke-width", 4)
        .set("stroke-linejoin", "round")
        .set("stroke-linecap", "round")
        .set("shape-rendering", "geometricPrecision")
        .set("clip-path", "url(#board-clip)")
        .set("d", data.clone());

    let fill_path = Path::new()
        .set("fill", "white")
        .set("stroke", "none")
        .set("d", data);

    let defs = svg::node::element::Definitions::new().add(clip_path);

    let document = Document::new()
        .set("viewBox", (0, 0, svg_width, svg_height))
        .set("width", display_width)
        .set("height", display_height)
        .add(defs)
        .add(fill_path)
        .add(stroked_path);

    let mut svg_buffer = Vec::new();
    svg::write(&mut svg_buffer, &document).unwrap();
    String::from_utf8(svg_buffer).unwrap()
}
