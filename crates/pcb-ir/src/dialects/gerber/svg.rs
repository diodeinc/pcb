use std::fmt::Write;

use crate::common::*;
use crate::dialects::gerber::*;

const VIEWBOX_PADDING_MM: f64 = 1.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderMode {
    Final,
    Debug,
}

#[derive(Debug, Clone, Copy)]
pub struct SvgOptions {
    pub mode: RenderMode,
    pub width_px: Option<u32>,
    pub height_px: Option<u32>,
}

impl Default for SvgOptions {
    fn default() -> Self {
        Self {
            mode: RenderMode::Final,
            width_px: None,
            height_px: None,
        }
    }
}

pub fn render_svg<A: Clone>(doc: &GeometryDocument<A>) -> String {
    render_svg_with_options(doc, SvgOptions::default())
}

pub fn render_svg_sized<A: Clone>(
    doc: &GeometryDocument<A>,
    width_px: u32,
    height_px: u32,
) -> String {
    render_svg_with_options(
        doc,
        SvgOptions {
            width_px: Some(width_px),
            height_px: Some(height_px),
            ..SvgOptions::default()
        },
    )
}

pub fn render_svg_with_options<A: Clone>(doc: &GeometryDocument<A>, options: SvgOptions) -> String {
    if options.mode == RenderMode::Final {
        let geom = lower_to_geom(doc);
        let mask = crate::dialects::geom::lower_filled_to_mask(&geom);
        return match (options.width_px, options.height_px) {
            (Some(width), Some(height)) => {
                crate::dialects::mask::render_svg_sized(&mask, 0, width, height)
            }
            _ => crate::dialects::mask::render_svg(&mask, 0),
        };
    }

    let bbox = render_bbox(doc);
    let viewbox_y = -bbox.max.y;
    let mut svg = String::new();
    let size = match (options.width_px, options.height_px) {
        (Some(w), Some(h)) => format!(" width='{w}' height='{h}'"),
        _ => String::new(),
    };
    writeln!(
        svg,
        "<svg xmlns='http://www.w3.org/2000/svg'{size} viewBox='{} {} {} {}'>",
        fmt_num(bbox.min.x),
        fmt_num(viewbox_y),
        fmt_num(bbox.width()),
        fmt_num(bbox.height())
    )
    .unwrap();
    writeln!(svg, "  <title>{}</title>", escape_xml(&layer_title(doc))).unwrap();
    writeln!(svg, "  <g transform='scale(1 -1)'>").unwrap();

    for bucket in bucket_order(options.mode) {
        for feature in doc
            .features
            .iter()
            .filter(|feature| feature.bucket == *bucket && feature.path_count > 0)
        {
            for path in &doc.paths
                [feature.path_start as usize..(feature.path_start + feature.path_count) as usize]
            {
                write_path(&mut svg, doc, feature, path, options.mode);
            }
        }
    }

    writeln!(svg, "  </g>").unwrap();
    writeln!(svg, "</svg>").unwrap();
    svg
}

pub fn render_bbox<A>(doc: &GeometryDocument<A>) -> BBox {
    if doc.bbox.is_empty() {
        BBox {
            min: Point::new(0.0, 0.0),
            max: Point::new(100.0, 100.0),
        }
    } else {
        doc.bbox.expand(VIEWBOX_PADDING_MM)
    }
}

fn bucket_order(mode: RenderMode) -> &'static [FeatureBucket] {
    match mode {
        RenderMode::Final => &[FeatureBucket::Fill, FeatureBucket::Unknown],
        RenderMode::Debug => &[
            FeatureBucket::Fill,
            FeatureBucket::Trace,
            FeatureBucket::Pad,
            FeatureBucket::Cutout,
            FeatureBucket::Unknown,
        ],
    }
}

fn write_path<A>(
    svg: &mut String,
    doc: &GeometryDocument<A>,
    feature: &GeometryFeature<A>,
    path: &GeometryPath,
    mode: RenderMode,
) {
    let d = path_data(doc, path);
    if d.is_empty() {
        return;
    }
    let style = style_for(doc, feature, mode);
    let fill_rule = match path.fill_rule {
        FillRule::NonZero => "nonzero",
        FillRule::EvenOdd => "evenodd",
    };
    if mode == RenderMode::Final && is_profile_layer(doc) {
        let stroke_width = if path.stroke_width > 0.0 {
            path.stroke_width
        } else {
            0.1
        };
        writeln!(
            svg,
            "    <path d='{d}' fill='none' stroke='#000000' stroke-width='{}' stroke-linecap='{}' stroke-linejoin='round' data-board-outline='true'/>",
            fmt_num(stroke_width),
            line_cap(path.line_cap)
        )
        .unwrap();
        return;
    }
    if path.flags.stroked {
        writeln!(
            svg,
            "    <path d='{d}' fill='none' stroke='{}' stroke-opacity='{}' stroke-width='{}' stroke-linecap='{}' stroke-linejoin='round' data-kind='{:?}' data-bucket='{:?}'/>",
            style.color, fmt_num(style.opacity), fmt_num(path.stroke_width), line_cap(path.line_cap), feature.kind, feature.bucket
        ).unwrap();
    } else if path.flags.filled {
        writeln!(
            svg,
            "    <path d='{d}' fill='{}' fill-opacity='{}' fill-rule='{fill_rule}' data-kind='{:?}' data-bucket='{:?}'/>",
            style.color, fmt_num(style.opacity), feature.kind, feature.bucket
        ).unwrap();
    }
}

fn is_profile_layer<A>(doc: &GeometryDocument<A>) -> bool {
    matches!(
        doc.file_function.first().map(String::as_str),
        Some("Profile")
    )
}

fn path_data<A>(doc: &GeometryDocument<A>, path: &GeometryPath) -> String {
    let mut data = String::new();
    for contour in &doc.contours
        [path.contour_start as usize..(path.contour_start + path.contour_count) as usize]
    {
        let mut current = Point::default();
        for cmd in &doc.path_cmds
            [contour.cmd_start as usize..(contour.cmd_start + contour.cmd_count) as usize]
        {
            match cmd.op {
                PathOp::MoveTo => {
                    current = cmd.p0;
                    write!(data, "M{} {}", fmt_num(cmd.p0.x), fmt_num(cmd.p0.y)).unwrap();
                }
                PathOp::LineTo => {
                    current = cmd.p0;
                    write!(data, " L{} {}", fmt_num(cmd.p0.x), fmt_num(cmd.p0.y)).unwrap();
                }
                PathOp::ArcTo => {
                    let radius = current.distance_to(cmd.p1);
                    let sweep_flag = if cmd.clockwise { 0 } else { 1 };
                    let large_arc = if arc_sweep_radians(current, cmd.p0, cmd.p1, cmd.clockwise)
                        > std::f64::consts::PI
                    {
                        1
                    } else {
                        0
                    };
                    if current.distance_to(cmd.p0) <= 1e-9 && radius > 0.0 {
                        let mid =
                            Point::new(2.0 * cmd.p1.x - current.x, 2.0 * cmd.p1.y - current.y);
                        write!(
                            data,
                            " A{} {} 0 1 {sweep_flag} {} {} A{} {} 0 1 {sweep_flag} {} {}",
                            fmt_num(radius),
                            fmt_num(radius),
                            fmt_num(mid.x),
                            fmt_num(mid.y),
                            fmt_num(radius),
                            fmt_num(radius),
                            fmt_num(cmd.p0.x),
                            fmt_num(cmd.p0.y)
                        )
                        .unwrap();
                    } else {
                        write!(
                            data,
                            " A{} {} 0 {large_arc} {sweep_flag} {} {}",
                            fmt_num(radius),
                            fmt_num(radius),
                            fmt_num(cmd.p0.x),
                            fmt_num(cmd.p0.y)
                        )
                        .unwrap();
                    }
                    current = cmd.p0;
                }
                PathOp::Close => data.push_str(" Z"),
            }
        }
    }
    data
}

struct Style {
    color: &'static str,
    opacity: f64,
}

fn style_for<A>(
    doc: &GeometryDocument<A>,
    feature: &GeometryFeature<A>,
    mode: RenderMode,
) -> Style {
    if mode == RenderMode::Debug {
        return match feature.bucket {
            FeatureBucket::Pad => Style {
                color: "#f4b942",
                opacity: 0.78,
            },
            FeatureBucket::Trace => Style {
                color: "#d87822",
                opacity: 0.78,
            },
            FeatureBucket::Fill => Style {
                color: "#b7661f",
                opacity: 0.78,
            },
            FeatureBucket::Cutout => Style {
                color: "#808080",
                opacity: 1.0,
            },
            FeatureBucket::Unknown => Style {
                color: "#5c7cfa",
                opacity: 0.78,
            },
        };
    }
    match doc.file_function.first().map(String::as_str) {
        Some("Paste") => Style {
            color: "#aeb4bb",
            opacity: 0.9,
        },
        Some("Soldermask") => Style {
            color: "#159447",
            opacity: 0.55,
        },
        Some("Legend") => Style {
            color: "#f8f9fa",
            opacity: 0.95,
        },
        Some("Profile") => Style {
            color: "#606060",
            opacity: 1.0,
        },
        Some("Copper") => Style {
            color: "#d87822",
            opacity: 0.9,
        },
        _ => Style {
            color: "#5c7cfa",
            opacity: 0.85,
        },
    }
}

fn layer_title<A>(doc: &GeometryDocument<A>) -> String {
    if doc.file_function.is_empty() {
        "Gerber X2 layer".to_string()
    } else {
        doc.file_function.join(", ")
    }
}

fn line_cap(line_cap: LineCap) -> &'static str {
    match line_cap {
        LineCap::Round => "round",
        LineCap::Square => "square",
        LineCap::Butt => "butt",
    }
}

fn fmt_num(value: f64) -> String {
    let value = if value.abs() < 1e-9 { 0.0 } else { value };
    let mut s = format!("{value:.6}");
    while s.contains('.') && s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
    s
}

fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
