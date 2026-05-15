use std::fmt::Write;

use crate::common::*;
use crate::dialects::ipc::*;

const VIEWBOX_PADDING_MM: f64 = 1.0;

pub fn render_layer_bbox<S, L>(doc: &GeometryDocument<S, L>, layer_index: usize) -> BBox {
    let layer = &doc.layers[layer_index];
    let bbox = doc
        .board_outlines
        .iter()
        .fold(layer.bbox, |bbox, outline| bbox.union(outline.bbox));

    if bbox.is_empty() {
        BBox {
            min: Point::new(0.0, 0.0),
            max: Point::new(100.0, 100.0),
        }
    } else if doc.board_outlines.is_empty() {
        bbox
    } else {
        bbox.expand(VIEWBOX_PADDING_MM)
    }
}

pub fn render_layer_svg<S, L>(doc: &GeometryDocument<S, L>, layer_index: usize) -> String {
    render_layer_svg_with_size(doc, layer_index, None, fallback_flat_style::<L>)
}

pub fn render_layer_svg_sized<S, L>(
    doc: &GeometryDocument<S, L>,
    layer_index: usize,
    width_px: u32,
    height_px: u32,
) -> String {
    render_layer_svg_with_size(
        doc,
        layer_index,
        Some((width_px, height_px)),
        fallback_flat_style::<L>,
    )
}

pub fn render_layer_svg_with_style<S, L, F>(
    doc: &GeometryDocument<S, L>,
    layer_index: usize,
    flat_style: F,
) -> String
where
    F: Copy + Fn(&L) -> (&'static str, f64),
{
    render_layer_svg_with_size(doc, layer_index, None, flat_style)
}

pub fn render_layer_svg_sized_with_style<S, L, F>(
    doc: &GeometryDocument<S, L>,
    layer_index: usize,
    width_px: u32,
    height_px: u32,
    flat_style: F,
) -> String
where
    F: Copy + Fn(&L) -> (&'static str, f64),
{
    render_layer_svg_with_size(doc, layer_index, Some((width_px, height_px)), flat_style)
}

fn render_layer_svg_with_size<S, L, F>(
    doc: &GeometryDocument<S, L>,
    layer_index: usize,
    pixel_size: Option<(u32, u32)>,
    flat_style: F,
) -> String
where
    F: Copy + Fn(&L) -> (&'static str, f64),
{
    let layer = &doc.layers[layer_index];
    let geometry_bbox = render_layer_bbox(doc, layer_index);
    let viewbox_y = -geometry_bbox.max.y;

    let mut svg = String::new();
    if let Some((width_px, height_px)) = pixel_size {
        writeln!(
            svg,
            "<svg xmlns='http://www.w3.org/2000/svg' width='{width_px}' height='{height_px}' viewBox='{} {} {} {}'>",
            fmt_num(geometry_bbox.min.x),
            fmt_num(viewbox_y),
            fmt_num(geometry_bbox.width()),
            fmt_num(geometry_bbox.height())
        )
        .unwrap();
    } else {
        writeln!(
            svg,
            "<svg xmlns='http://www.w3.org/2000/svg' viewBox='{} {} {} {}'>",
            fmt_num(geometry_bbox.min.x),
            fmt_num(viewbox_y),
            fmt_num(geometry_bbox.width()),
            fmt_num(geometry_bbox.height())
        )
        .unwrap();
    }
    writeln!(svg, "  <title>{}</title>", escape_xml(&doc.board_name)).unwrap();
    writeln!(
        svg,
        "  <g data-layer='{}' transform='scale(1 -1)'>",
        escape_xml(&layer.name)
    )
    .unwrap();

    write_feature_paths_by_bucket(&mut svg, doc, layer, &[FeatureBucket::Fill], flat_style);
    write_feature_paths_by_bucket(&mut svg, doc, layer, &[FeatureBucket::Trace], flat_style);
    write_feature_paths_by_bucket(&mut svg, doc, layer, &[FeatureBucket::Smd], flat_style);
    write_feature_paths_by_bucket(&mut svg, doc, layer, &[FeatureBucket::Thermal], flat_style);
    write_feature_paths_by_bucket(
        &mut svg,
        doc,
        layer,
        &[
            FeatureBucket::Pth,
            FeatureBucket::Via,
            FeatureBucket::Cutout,
            FeatureBucket::Antipad,
        ],
        flat_style,
    );

    write_board_outlines(&mut svg, doc);

    writeln!(svg, "  </g>").unwrap();
    writeln!(svg, "</svg>").unwrap();
    svg
}

fn write_board_outlines<S, L>(svg: &mut String, doc: &GeometryDocument<S, L>) {
    for outline in &doc.board_outlines {
        for path in &doc.paths
            [outline.path_start as usize..(outline.path_start + outline.path_count) as usize]
        {
            writeln!(
                svg,
                "    <path d='{}' fill='none' stroke='#000000' stroke-width='{}' stroke-linecap='{}' stroke-linejoin='round' data-board-outline='true'/>",
                path_data(doc, path),
                fmt_num(path.stroke_width),
                line_cap(path.line_cap)
            )
            .unwrap();
        }
    }
}

fn write_feature_paths_by_bucket<S, L, F>(
    svg: &mut String,
    doc: &GeometryDocument<S, L>,
    layer: &GeometryLayer<S, L>,
    buckets: &[FeatureBucket],
    flat_style: F,
) where
    F: Copy + Fn(&L) -> (&'static str, f64),
{
    for feature in &doc.features
        [layer.feature_start as usize..(layer.feature_start + layer.feature_count) as usize]
    {
        if !buckets.contains(&feature.bucket) {
            continue;
        }

        for path in &doc.paths
            [feature.path_start as usize..(feature.path_start + feature.path_count) as usize]
        {
            write_path(svg, doc, layer, feature, path, flat_style);
        }
    }
}

fn write_path<S, L, F>(
    svg: &mut String,
    doc: &GeometryDocument<S, L>,
    layer: &GeometryLayer<S, L>,
    feature: &GeometryFeature<S>,
    path: &GeometryPath,
    flat_style: F,
) where
    F: Copy + Fn(&L) -> (&'static str, f64),
{
    let d = path_data(doc, path);
    let style = style_for_feature(layer, feature, flat_style);
    let fill_rule = match path.fill_rule {
        FillRule::NonZero => "nonzero",
        FillRule::EvenOdd => "evenodd",
    };

    if path.flags.stroked {
        writeln!(
            svg,
            "    <path d='{}' fill='none' stroke='{}' stroke-opacity='{}' stroke-width='{}' stroke-linecap='{}' stroke-linejoin='round'/>",
            d,
            style.color,
            fmt_num(style.opacity),
            fmt_num(path.stroke_width),
            line_cap(path.line_cap)
        )
        .unwrap();
    } else if path.flags.filled {
        writeln!(
            svg,
            "    <path d='{}' fill='{}' fill-opacity='{}' fill-rule='{}'/>",
            d,
            style.color,
            fmt_num(style.opacity),
            fill_rule
        )
        .unwrap();
    }
}

fn path_data<S, L>(doc: &GeometryDocument<S, L>, path: &GeometryPath) -> String {
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
                    if !data.is_empty() {
                        data.push(' ');
                    }
                    write!(data, "M{} {}", fmt_num(cmd.p0.x), fmt_num(cmd.p0.y)).unwrap()
                }
                PathOp::LineTo => {
                    current = cmd.p0;
                    write!(data, " L{} {}", fmt_num(cmd.p0.x), fmt_num(cmd.p0.y)).unwrap()
                }
                PathOp::ArcTo => {
                    let radius = current.distance_to(cmd.p1);
                    let sweep = if cmd.clockwise { 0 } else { 1 };
                    if current.distance_to(cmd.p0) <= 1e-9 && radius > 0.0 {
                        let mid =
                            Point::new(2.0 * cmd.p1.x - current.x, 2.0 * cmd.p1.y - current.y);
                        write!(
                            data,
                            " A{} {} 0 0 {} {} {} A{} {} 0 0 {} {} {}",
                            fmt_num(radius),
                            fmt_num(radius),
                            sweep,
                            fmt_num(mid.x),
                            fmt_num(mid.y),
                            fmt_num(radius),
                            fmt_num(radius),
                            sweep,
                            fmt_num(cmd.p0.x),
                            fmt_num(cmd.p0.y)
                        )
                        .unwrap();
                        current = cmd.p0;
                        continue;
                    }

                    let large_arc = u8::from(
                        arc_sweep_radians(current, cmd.p0, cmd.p1, cmd.clockwise)
                            > std::f64::consts::PI,
                    );
                    current = cmd.p0;
                    write!(
                        data,
                        " A{} {} 0 {} {} {} {}",
                        fmt_num(radius),
                        fmt_num(radius),
                        large_arc,
                        sweep,
                        fmt_num(cmd.p0.x),
                        fmt_num(cmd.p0.y)
                    )
                    .unwrap()
                }
                PathOp::CubicTo => write!(
                    data,
                    " C{} {},{} {},{} {}",
                    fmt_num(cmd.p0.x),
                    fmt_num(cmd.p0.y),
                    fmt_num(cmd.p1.x),
                    fmt_num(cmd.p1.y),
                    fmt_num(cmd.p2.x),
                    fmt_num(cmd.p2.y)
                )
                .map(|_| current = cmd.p2)
                .unwrap(),
                PathOp::Close => data.push_str(" Z"),
            }
        }
    }
    data
}

#[derive(Debug, Clone, Copy)]
struct Style {
    color: &'static str,
    opacity: f64,
}

fn style_for_feature<S, L, F>(
    layer: &GeometryLayer<S, L>,
    feature: &GeometryFeature<S>,
    flat_style: F,
) -> Style
where
    F: Copy + Fn(&L) -> (&'static str, f64),
{
    if feature.kind == FeatureKind::FlattenedBucket
        && feature.polarity == GeometryPolarity::Positive
    {
        return flat_layer_style(layer, flat_style);
    }
    Style {
        color: color_for_bucket(feature.bucket, feature.polarity),
        opacity: opacity_for_bucket(feature.bucket, feature.polarity),
    }
}

fn flat_layer_style<S, L, F>(layer: &GeometryLayer<S, L>, flat_style: F) -> Style
where
    F: Copy + Fn(&L) -> (&'static str, f64),
{
    let (color, opacity) = flat_style(&layer.layer_function);
    Style { color, opacity }
}

fn fallback_flat_style<L>(_: &L) -> (&'static str, f64) {
    ("#5c7cfa", 0.85)
}

fn color_for_bucket(bucket: FeatureBucket, polarity: GeometryPolarity) -> &'static str {
    if polarity == GeometryPolarity::Negative {
        return "#111827";
    }

    match bucket {
        FeatureBucket::Smd => "#d97706",
        FeatureBucket::Pth => "#7c3aed",
        FeatureBucket::Via => "#2563eb",
        FeatureBucket::Trace => "#c2410c",
        FeatureBucket::Fill => "#2f9e44",
        FeatureBucket::Cutout => "#64748b",
        FeatureBucket::Thermal => "#ca8a04",
        FeatureBucket::Antipad => "#64748b",
    }
}

fn opacity_for_bucket(bucket: FeatureBucket, polarity: GeometryPolarity) -> f64 {
    if polarity == GeometryPolarity::Negative {
        return 0.8;
    }

    match bucket {
        FeatureBucket::Fill => 0.78,
        FeatureBucket::Trace => 0.95,
        FeatureBucket::Smd | FeatureBucket::Pth => 0.95,
        FeatureBucket::Via => 0.9,
        FeatureBucket::Cutout | FeatureBucket::Antipad => 1.0,
        FeatureBucket::Thermal => 0.92,
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
    let value = if value.abs() < 0.000_000_5 {
        0.0
    } else {
        value
    };
    format!("{value:.6}")
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
