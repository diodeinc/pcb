use std::fmt::Write;

use super::ir::*;

pub fn render_layer_svg(doc: &GeometryDocument, layer_index: usize) -> String {
    render_layer_svg_with_size(doc, layer_index, None)
}

pub fn render_layer_svg_sized(
    doc: &GeometryDocument,
    layer_index: usize,
    width_px: u32,
    height_px: u32,
) -> String {
    render_layer_svg_with_size(doc, layer_index, Some((width_px, height_px)))
}

fn render_layer_svg_with_size(
    doc: &GeometryDocument,
    layer_index: usize,
    pixel_size: Option<(u32, u32)>,
) -> String {
    let layer = &doc.layers[layer_index];
    let geometry_bbox = if layer.bbox.is_empty() {
        BBox {
            min: Point::new(0.0, 0.0),
            max: Point::new(100.0, 100.0),
        }
    } else {
        layer.bbox
    };
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

    for feature in &doc.features
        [layer.feature_start as usize..(layer.feature_start + layer.feature_count) as usize]
    {
        write_feature_paths(&mut svg, doc, feature, false);
    }

    for feature in &doc.features
        [layer.feature_start as usize..(layer.feature_start + layer.feature_count) as usize]
    {
        write_feature_paths(&mut svg, doc, feature, true);
    }

    writeln!(svg, "  </g>").unwrap();
    writeln!(svg, "</svg>").unwrap();
    svg
}

fn write_feature_paths(
    svg: &mut String,
    doc: &GeometryDocument,
    feature: &GeometryFeature,
    cutouts: bool,
) {
    if (feature.bucket == FeatureBucket::Cutout) != cutouts {
        return;
    }

    for path in
        &doc.paths[feature.path_start as usize..(feature.path_start + feature.path_count) as usize]
    {
        write_path(svg, doc, feature, path);
    }
}

fn write_path(
    svg: &mut String,
    doc: &GeometryDocument,
    feature: &GeometryFeature,
    path: &GeometryPath,
) {
    let d = path_data(doc, path);
    let color = color_for_bucket(feature.bucket, feature.polarity);
    let opacity = opacity_for_bucket(feature.bucket, feature.polarity);
    let fill_rule = match path.fill_rule {
        FillRule::NonZero => "nonzero",
        FillRule::EvenOdd => "evenodd",
    };

    if path.flags.stroked {
        writeln!(
            svg,
            "    <path d='{}' fill='none' stroke='{}' stroke-opacity='{}' stroke-width='{}' stroke-linecap='{}' stroke-linejoin='round'/>",
            d,
            color,
            fmt_num(opacity),
            fmt_num(path.stroke_width),
            line_cap(path.line_cap)
        )
        .unwrap();
    } else {
        writeln!(
            svg,
            "    <path d='{}' fill='{}' fill-opacity='{}' fill-rule='{}'/>",
            d,
            color,
            fmt_num(opacity),
            fill_rule
        )
        .unwrap();
    }
}

fn path_data(doc: &GeometryDocument, path: &GeometryPath) -> String {
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
                    let large_arc = u8::from(
                        arc_sweep_radians(current, cmd.p0, cmd.p1, cmd.clockwise)
                            > std::f64::consts::PI,
                    );
                    let sweep = if cmd.clockwise { 0 } else { 1 };
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
        FeatureBucket::Fill => 0.82,
        FeatureBucket::Trace => 0.95,
        FeatureBucket::Smd | FeatureBucket::Pth => 0.95,
        FeatureBucket::Via => 0.9,
        FeatureBucket::Cutout | FeatureBucket::Antipad => 0.85,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_arc_path_command_without_flattening() {
        let mut interner = ipc2581::Interner::new();
        let mut doc = GeometryDocument::new("test".to_string());
        let mut bbox = BBox::empty();
        bbox.include_point(Point::new(-1.0, 0.0));
        bbox.include_point(Point::new(1.0, 0.0));
        bbox.include_point(Point::new(0.0, -1.0));
        doc.push_path(
            GeometryPath::filled(FillRule::NonZero, bbox),
            [
                PathCmd::move_to(Point::new(1.0, 0.0)),
                PathCmd::arc_to(Point::new(-1.0, 0.0), Point::new(0.0, 0.0), true),
                PathCmd::close(),
            ],
        );
        doc.features.push(GeometryFeature {
            path_count: 1,
            bbox,
            ..GeometryFeature::new(
                FeatureKind::Polygon,
                FeatureBucket::Fill,
                GeometryPolarity::Positive,
            )
        });
        doc.layers.push(GeometryLayer {
            name: "F.Cu".to_string(),
            source_layer_ref: interner.intern("F.Cu"),
            feature_start: 0,
            feature_count: 1,
            bbox,
        });

        let svg = render_layer_svg(&doc, 0);

        assert!(svg.contains(" A1 1 0 0 0 -1 0"));
    }

    #[test]
    fn renders_compound_path_contours_as_one_svg_path() {
        let mut interner = ipc2581::Interner::new();
        let mut doc = GeometryDocument::new("test".to_string());
        let outer = BBox {
            min: Point::new(0.0, 0.0),
            max: Point::new(10.0, 10.0),
        };
        let inner = BBox {
            min: Point::new(2.0, 2.0),
            max: Point::new(4.0, 4.0),
        };
        doc.push_compound_path(
            GeometryPath::filled(FillRule::EvenOdd, BBox::empty()),
            [
                (
                    outer,
                    vec![
                        PathCmd::move_to(Point::new(0.0, 0.0)),
                        PathCmd::line_to(Point::new(10.0, 0.0)),
                        PathCmd::line_to(Point::new(10.0, 10.0)),
                        PathCmd::line_to(Point::new(0.0, 10.0)),
                        PathCmd::close(),
                    ],
                ),
                (
                    inner,
                    vec![
                        PathCmd::move_to(Point::new(2.0, 2.0)),
                        PathCmd::line_to(Point::new(4.0, 2.0)),
                        PathCmd::line_to(Point::new(4.0, 4.0)),
                        PathCmd::line_to(Point::new(2.0, 4.0)),
                        PathCmd::close(),
                    ],
                ),
            ],
        );
        doc.features.push(GeometryFeature {
            path_count: 1,
            bbox: outer,
            ..GeometryFeature::new(
                FeatureKind::Polygon,
                FeatureBucket::Fill,
                GeometryPolarity::Positive,
            )
        });
        doc.layers.push(GeometryLayer {
            name: "F.Cu".to_string(),
            source_layer_ref: interner.intern("F.Cu"),
            feature_start: 0,
            feature_count: 1,
            bbox: outer,
        });

        let svg = render_layer_svg(&doc, 0);

        assert_eq!(svg.matches("<path d='").count(), 1);
        assert!(svg.contains(" Z M2 2"));
        assert!(svg.contains("fill-rule='evenodd'"));
    }

    #[test]
    fn renders_cutout_features_after_positive_geometry() {
        let mut interner = ipc2581::Interner::new();
        let mut doc = GeometryDocument::new("test".to_string());
        let bbox = BBox {
            min: Point::new(0.0, 0.0),
            max: Point::new(1.0, 1.0),
        };
        doc.push_path(
            GeometryPath::filled(FillRule::NonZero, bbox),
            [
                PathCmd::move_to(Point::new(0.0, 0.0)),
                PathCmd::line_to(Point::new(1.0, 0.0)),
                PathCmd::line_to(Point::new(1.0, 1.0)),
                PathCmd::close(),
            ],
        );
        doc.features.push(GeometryFeature {
            path_count: 1,
            bbox,
            ..GeometryFeature::new(
                FeatureKind::Polygon,
                FeatureBucket::Fill,
                GeometryPolarity::Positive,
            )
        });
        doc.push_path(
            GeometryPath::filled(FillRule::NonZero, bbox),
            [
                PathCmd::move_to(Point::new(0.0, 0.0)),
                PathCmd::line_to(Point::new(1.0, 0.0)),
                PathCmd::line_to(Point::new(1.0, 1.0)),
                PathCmd::close(),
            ],
        );
        doc.features.push(GeometryFeature {
            path_start: 1,
            path_count: 1,
            bbox,
            ..GeometryFeature::new(
                FeatureKind::Hole,
                FeatureBucket::Cutout,
                GeometryPolarity::Positive,
            )
        });
        doc.layers.push(GeometryLayer {
            name: "F.Cu".to_string(),
            source_layer_ref: interner.intern("F.Cu"),
            feature_start: 0,
            feature_count: 2,
            bbox,
        });

        let svg = render_layer_svg(&doc, 0);

        assert_eq!(svg.matches("<path d='").count(), 2);
        assert!(svg.find("fill='#2f9e44'").unwrap() < svg.find("fill='#64748b'").unwrap());
    }
}
