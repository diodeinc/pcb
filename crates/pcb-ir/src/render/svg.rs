use std::fmt::Write;

use crate::dialects::LayerRole;
use crate::dialects::mask;
use crate::geom::path::{PathCmd, PathOp};
use crate::geom::{Arc, FillRule, Path, Point};
use crate::render::{RenderOptions, SizeConstraint};

const POINT_EPSILON_MM: f64 = 1e-9;

/// Render mask layers to an SVG document (millimeter units, y-up source
/// coordinates flipped for screen display).
pub fn svg<LayerMeta>(doc: &mask::Document<LayerMeta>, options: &RenderOptions) -> String {
    let layers = crate::render::layer_indices(doc, options.layers.as_deref());
    let pixel_size = match options.size {
        SizeConstraint::Auto => None,
        SizeConstraint::Fixed {
            width_px,
            height_px,
        } => Some((width_px, height_px)),
        SizeConstraint::MaxDimension(max) => Some(crate::render::pixel_size(
            doc,
            options.layers.as_deref(),
            max,
        )),
    };
    render_layers(doc, &layers, pixel_size)
}

fn render_layers<LayerMeta>(
    doc: &mask::Document<LayerMeta>,
    layer_indices: &[usize],
    pixel_size: Option<(u32, u32)>,
) -> String {
    let bbox = crate::render::bbox(doc, Some(layer_indices));
    let viewbox_y = -bbox.max.y;
    let mut svg = String::new();
    let size = pixel_size
        .map(|(width, height)| format!(" width='{width}' height='{height}'"))
        .unwrap_or_default();
    writeln!(
        svg,
        "<svg xmlns='http://www.w3.org/2000/svg'{size} viewBox='{} {} {} {}'>",
        fmt_num(bbox.min.x),
        fmt_num(viewbox_y),
        fmt_num(bbox.width()),
        fmt_num(bbox.height())
    )
    .unwrap();
    let title = layer_indices
        .first()
        .and_then(|&index| doc.layers.get(index))
        .map(|layer| layer.name.as_str())
        .unwrap_or("mask");
    writeln!(svg, "  <title>{}</title>", escape_xml(title)).unwrap();
    writeln!(svg, "  <g transform='scale(1 -1)'>").unwrap();

    for &layer_index in layer_indices {
        let layer = &doc.layers[layer_index];
        for shape in doc.shapes(layer) {
            write_shape(&mut svg, doc, layer.role, shape);
        }
    }

    writeln!(svg, "  </g>").unwrap();
    writeln!(svg, "</svg>").unwrap();
    svg
}

fn write_shape<LayerMeta>(
    svg: &mut String,
    doc: &mask::Document<LayerMeta>,
    role: LayerRole,
    shape: &Path,
) {
    let d = path_data(doc, shape);
    if d.is_empty() {
        return;
    }
    let fill_rule = match shape.fill_rule().unwrap_or(FillRule::NonZero) {
        FillRule::NonZero => "nonzero",
        FillRule::EvenOdd => "evenodd",
    };
    let (color, opacity) = layer_style(role);
    if role == LayerRole::Profile {
        writeln!(
            svg,
            "    <path d='{d}' fill='none' stroke='#000000' stroke-width='0.1' stroke-linejoin='round' data-board-outline='true'/>",
        )
        .unwrap();
    } else {
        writeln!(
            svg,
            "    <path d='{d}' fill='{color}' fill-opacity='{}' fill-rule='{fill_rule}'/>",
            fmt_num(opacity)
        )
        .unwrap();
    }
}

fn path_data<LayerMeta>(doc: &mask::Document<LayerMeta>, shape: &Path) -> String {
    let mut data = String::new();
    for contour in doc.arena.contours(shape.contours) {
        let mut current = Point::default();
        for cmd in doc.arena.cmds(*contour) {
            match cmd.op {
                PathOp::MoveTo => {
                    current = cmd.p0;
                    if !data.is_empty() {
                        data.push(' ');
                    }
                    write!(data, "M{} {}", fmt_num(cmd.p0.x), fmt_num(cmd.p0.y)).unwrap();
                }
                PathOp::LineTo => {
                    current = cmd.p0;
                    write!(data, " L{} {}", fmt_num(cmd.p0.x), fmt_num(cmd.p0.y)).unwrap();
                }
                PathOp::ArcTo => {
                    write_arc(&mut data, current, *cmd);
                    current = cmd.p0;
                }
                PathOp::CubicTo => {
                    current = cmd.p2;
                    write!(
                        data,
                        " C{} {},{} {},{} {}",
                        fmt_num(cmd.p0.x),
                        fmt_num(cmd.p0.y),
                        fmt_num(cmd.p1.x),
                        fmt_num(cmd.p1.y),
                        fmt_num(cmd.p2.x),
                        fmt_num(cmd.p2.y)
                    )
                    .unwrap();
                }
                PathOp::Close => data.push_str(" Z"),
            }
        }
    }
    data
}

fn write_arc(data: &mut String, start: Point, cmd: PathCmd) {
    let arc = Arc::new(start, cmd.p0, cmd.p1, cmd.clockwise);
    let radius = arc.radius();
    if radius <= POINT_EPSILON_MM {
        write!(data, " L{} {}", fmt_num(arc.end.x), fmt_num(arc.end.y)).unwrap();
        return;
    }

    let sweep_flag = if arc.clockwise { 0 } else { 1 };
    if arc.start.distance_to(arc.end) <= POINT_EPSILON_MM {
        // A full circle cannot be one SVG arc; split at the antipode.
        let midpoint = arc.center * 2.0 - arc.start;
        write_svg_arc(data, radius, 0, sweep_flag, midpoint);
        write_svg_arc(data, radius, 0, sweep_flag, arc.end);
        return;
    }

    let large_arc = u8::from(arc.sweep_radians() > std::f64::consts::PI);
    write_svg_arc(data, radius, large_arc, sweep_flag, arc.end);
}

fn write_svg_arc(data: &mut String, radius: f64, large_arc: u8, sweep_flag: u8, end: Point) {
    write!(
        data,
        " A{} {} 0 {large_arc} {sweep_flag} {} {}",
        fmt_num(radius),
        fmt_num(radius),
        fmt_num(end.x),
        fmt_num(end.y)
    )
    .unwrap();
}

fn layer_style(role: LayerRole) -> (&'static str, f64) {
    match role {
        LayerRole::Copper => ("#d87822", 0.9),
        LayerRole::Soldermask => ("#159447", 0.55),
        LayerRole::Paste => ("#aeb4bb", 0.9),
        LayerRole::Legend => ("#000000", 0.95),
        LayerRole::Profile => ("#000000", 1.0),
        LayerRole::Drill | LayerRole::Mechanical | LayerRole::Other => ("#5c7cfa", 0.85),
    }
}

fn escape_xml(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub(crate) fn fmt_num(value: f64) -> String {
    let mut text = format!("{value:.6}");
    while text.contains('.') && text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    if text == "-0" { "0".to_string() } else { text }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialects::{Side, mask::Layer};
    use crate::geom::BBox;
    use crate::geom::path::ContourBuf;

    #[test]
    fn renders_full_circle_arc_as_two_svg_arcs() {
        let mut doc = mask::Document::<()>::new();
        let layer = doc.push_layer(Layer::new("F.Cu", LayerRole::Copper, Side::Top));
        doc.push_shape(
            layer,
            FillRule::NonZero,
            vec![ContourBuf::from_parts(
                BBox::new(Point::new(-1.0, -1.0), Point::new(1.0, 1.0)),
                vec![
                    PathCmd::move_to(Point::new(1.0, 0.0)),
                    PathCmd::arc_to(Point::new(1.0, 0.0), Point::new(0.0, 0.0), false),
                    PathCmd::close(),
                ],
            )],
        );

        let svg = svg(&doc, &RenderOptions::layer(0));

        assert_eq!(svg.matches(" A1 1 0 0 1 ").count(), 2);
        assert!(svg.contains("-1 0"));
    }

    #[test]
    fn renders_profile_layer_as_black_outline_overlay() {
        let mut doc = mask::Document::<()>::new();
        let copper = doc.push_layer(Layer::new("F.Cu", LayerRole::Copper, Side::Top));
        let profile = doc.push_layer(Layer::new("Profile", LayerRole::Profile, Side::None));
        let contour = ContourBuf::new(vec![
            PathCmd::move_to(Point::new(0.0, 0.0)),
            PathCmd::line_to(Point::new(1.0, 0.0)),
            PathCmd::line_to(Point::new(1.0, 1.0)),
            PathCmd::close(),
        ]);
        doc.push_shape(copper, FillRule::NonZero, vec![contour.clone()]);
        doc.push_shape(profile, FillRule::NonZero, vec![contour]);

        let svg = svg(
            &doc,
            &RenderOptions::layers(vec![copper as usize, profile as usize]),
        );

        assert!(svg.contains("fill='#d87822'"));
        assert!(svg.contains("stroke='#000000'"));
        assert!(svg.contains("data-board-outline='true'"));
    }

    #[test]
    fn renders_legend_layer_as_black_for_legibility() {
        let mut doc = mask::Document::<()>::new();
        let legend = doc.push_layer(Layer::new("F.Silkscreen", LayerRole::Legend, Side::Top));
        doc.push_shape(
            legend,
            FillRule::NonZero,
            vec![ContourBuf::new(vec![
                PathCmd::move_to(Point::new(0.0, 0.0)),
                PathCmd::line_to(Point::new(1.0, 0.0)),
                PathCmd::line_to(Point::new(1.0, 1.0)),
                PathCmd::close(),
            ])],
        );

        let svg = svg(&doc, &RenderOptions::layer(0));

        assert!(svg.contains("fill='#000000'"));
    }
}
