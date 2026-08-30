use std::fmt::Write;

use crate::dialects::LayerRole;
use crate::dialects::artwork::{self, Geometry, PaintStage};
use crate::dialects::mask;
use crate::geom::path::{PathCmd, PathOp};

use crate::geom::{
    Affine2, Arc, BBox, FillRule, LineCap, LineJoin, Path, PathArena, Point, Polarity, StrokeStyle,
};
use crate::render::{RenderOptions, SizeConstraint};

const POINT_EPSILON_MM: f64 = 1e-9;

/// Render mask layers to an SVG document (millimeter units, y-up source
/// coordinates flipped for screen display).
pub fn svg<LayerMeta>(doc: &mask::Document<LayerMeta>, options: &RenderOptions) -> String {
    let layers = crate::render::layer_indices(doc.layers.len(), options.layers.as_deref());
    let bbox = options.viewport_or(crate::render::bbox(doc, Some(&layers)));
    let title = layers
        .first()
        .and_then(|&index| doc.layers.get(index))
        .map(|layer| layer.name.as_str());
    let mut svg = open_svg(&bbox, pixel_size(options, bbox), title);

    for &layer_index in &layers {
        let layer = &doc.layers[layer_index];
        for shape in doc.shapes(layer) {
            write_shape(&mut svg, &doc.arena, layer.role, shape);
        }
    }

    close_svg(svg)
}

/// Render artwork layers to an SVG document.
///
/// Apertures become `<defs>` shapes that flashes reference with `<use>`, so
/// repeated geometry stays repeated instead of being copied per placement.
/// Polarity runs paint sequentially: a clear run masks everything painted
/// before it, which is exactly how the same artwork images in Gerber.
pub fn artwork_svg<LayerMeta: Clone, ObjectMeta: Clone>(
    doc: &artwork::Document<LayerMeta, ObjectMeta>,
    options: &RenderOptions,
) -> String {
    if !doc.blocks.is_empty() {
        return artwork_svg(&artwork::expand_instances(doc), options);
    }
    let layers = crate::render::layer_indices(doc.layers.len(), options.layers.as_deref());
    let bbox = options.viewport_or(crate::render::artwork_bbox(doc, Some(&layers)));
    let mut defs = String::new();
    let mut body = String::new();

    for (aperture_index, aperture) in doc.apertures.iter().enumerate() {
        // Colour is inherited from the referencing group so one aperture can
        // serve both a dark run and a clear run's mask; `stroke` is not, or
        // every filled shape would gain the default one-unit outline.
        writeln!(
            defs,
            "    <path id='a{aperture_index}' d='{}' fill-rule='{}' stroke='none'/>",
            svg_path_data(&aperture.contours()),
            fill_rule_name(aperture.fill_rule())
        )
        .unwrap();
    }

    for &layer_index in &layers {
        let layer = &doc.layers[layer_index];
        write_artwork_layer(&mut body, &mut defs, doc, layer);
    }

    let title = layers
        .first()
        .and_then(|&index| doc.layers.get(index))
        .map(|layer| layer.name.as_str());
    let mut svg = open_svg(&bbox, pixel_size(options, bbox), title);
    writeln!(svg, "  <defs>\n{defs}  </defs>").unwrap();
    svg.push_str(&body);
    close_svg(svg)
}

fn write_artwork_layer<LayerMeta, ObjectMeta>(
    body: &mut String,
    defs: &mut String,
    doc: &artwork::Document<LayerMeta, ObjectMeta>,
    layer: &artwork::Layer<LayerMeta>,
) {
    let objects = artwork::paint_ordered(layer.objects.slice(&doc.objects));
    let has_material = objects
        .iter()
        .any(|object| object.order.stage != PaintStage::FinalCutout);

    // Sequential polarity: paint dark runs in order, and fold every clear run
    // into a mask over everything painted before it.
    let mut painted = String::new();
    let mut run = String::new();
    let mut run_polarity = Polarity::Dark;
    for object in objects {
        let polarity = if object.order.stage == PaintStage::FinalCutout && has_material {
            Polarity::Clear
        } else {
            object.polarity
        };
        if polarity != run_polarity {
            flush_run(&mut painted, defs, &mut run, run_polarity, layer);
            run_polarity = polarity;
        }
        write_artwork_object(&mut run, doc, layer.role, object);
    }
    flush_run(&mut painted, defs, &mut run, run_polarity, layer);

    // One group opacity rather than per-object alpha, so overlapping objects
    // composite once instead of darkening where they touch.
    let (color, opacity) = layer_style(layer.role);
    writeln!(
        body,
        "    <g fill='{color}' stroke='{color}' opacity='{}'>\n{painted}    </g>",
        fmt_num(opacity)
    )
    .unwrap();
}

fn flush_run<LayerMeta>(
    painted: &mut String,
    defs: &mut String,
    run: &mut String,
    polarity: Polarity,
    layer: &artwork::Layer<LayerMeta>,
) {
    let run = std::mem::take(run);
    match polarity {
        Polarity::Dark => painted.push_str(&run),
        // A clear run removes from what is already painted, so with nothing
        // under it there is nothing to remove.
        Polarity::Clear if painted.is_empty() => {}
        Polarity::Clear => {
            let mask_id = format!("m{}", defs.matches("<mask ").count());
            let bounds = layer.bbox.expand(1.0);
            let (x, y) = (fmt_num(bounds.min.x), fmt_num(bounds.min.y));
            let (width, height) = (fmt_num(bounds.width()), fmt_num(bounds.height()));
            writeln!(
                defs,
                "    <mask id='{mask_id}' maskUnits='userSpaceOnUse' x='{x}' y='{y}' width='{width}' height='{height}'>\n      <rect x='{x}' y='{y}' width='{width}' height='{height}' fill='#ffffff'/>\n      <g fill='#000000' stroke='#000000'>\n{run}      </g>\n    </mask>",
            )
            .unwrap();
            *painted = format!("      <g mask='url(#{mask_id})'>\n{painted}      </g>\n");
        }
    }
}

fn write_artwork_object<LayerMeta, ObjectMeta>(
    out: &mut String,
    doc: &artwork::Document<LayerMeta, ObjectMeta>,
    role: LayerRole,
    object: &artwork::Object<ObjectMeta>,
) {
    match object.geometry {
        Geometry::Flash {
            aperture,
            transform,
        } => {
            writeln!(
                out,
                "      <use href='#a{aperture}'{}/>",
                svg_transform(transform)
            )
            .unwrap();
        }
        Geometry::Region { path } => {
            let path = doc.arena.path(path);
            writeln!(
                out,
                "      <path d='{}' fill-rule='{}' stroke='none'/>",
                path_data(&doc.arena, path),
                fill_rule_name(
                    path.fill_rule()
                        .expect("region geometry carries a fill paint")
                )
            )
            .unwrap();
        }
        // SVG has no notion of IPC line patterns, so a patterned stroke
        // images through the same expansion the mask compositor uses.
        Geometry::Stroke { path } if !stroke_of(doc, path).is_solid() => {
            let contours = crate::geom::path::stroke_to_fill(
                &doc.arena.path_contours(doc.arena.path(path)),
                stroke_of(doc, path).into(),
            )
            .unwrap_or_default();
            writeln!(
                out,
                "      <path d='{}' fill-rule='nonzero' stroke='none'/>",
                svg_path_data(&contours)
            )
            .unwrap();
        }
        Geometry::Stroke { path } => {
            let stroke = stroke_of(doc, path);
            let outline = if role == LayerRole::Profile {
                " data-board-outline='true'"
            } else {
                ""
            };
            writeln!(
                out,
                "      <path d='{}' fill='none' stroke-width='{}' stroke-linecap='{}' stroke-linejoin='{}'{outline}/>",
                path_data(&doc.arena, doc.arena.path(path)),
                fmt_num(stroke.width),
                line_cap_name(stroke.cap),
                line_join_name(stroke.join),
            )
            .unwrap();
        }
        Geometry::Instance { .. } | Geometry::GridInstance { .. } => {
            unreachable!("artwork instances are expanded before SVG rendering")
        }
    }
}

fn stroke_of<LayerMeta, ObjectMeta>(
    doc: &artwork::Document<LayerMeta, ObjectMeta>,
    path: u32,
) -> StrokeStyle {
    doc.arena
        .path(path)
        .stroke()
        .expect("stroke geometry carries a stroke paint")
}

fn line_cap_name(cap: LineCap) -> &'static str {
    match cap {
        LineCap::Round => "round",
        LineCap::Square => "square",
        LineCap::Butt => "butt",
    }
}

fn line_join_name(join: LineJoin) -> &'static str {
    match join {
        LineJoin::Round => "round",
        LineJoin::Bevel => "bevel",
        LineJoin::Miter => "miter",
    }
}

fn svg_transform(transform: Affine2) -> String {
    format!(
        " transform='matrix({} {} {} {} {} {})'",
        fmt_num(transform.m00),
        fmt_num(transform.m10),
        fmt_num(transform.m01),
        fmt_num(transform.m11),
        fmt_num(transform.m02),
        fmt_num(transform.m12),
    )
}

fn pixel_size(options: &RenderOptions, bbox: BBox) -> Option<(u32, u32)> {
    match options.size {
        SizeConstraint::Auto => None,
        SizeConstraint::Fixed {
            width_px,
            height_px,
        } => Some((width_px, height_px)),
        SizeConstraint::MaxDimension(max) => Some(crate::render::pixel_size(bbox, max)),
    }
}

fn open_svg(bbox: &BBox, pixel_size: Option<(u32, u32)>, title: Option<&str>) -> String {
    let title = title.unwrap_or("layer");
    let mut svg = String::new();
    let size = pixel_size
        .map(|(width, height)| format!(" width='{width}' height='{height}'"))
        .unwrap_or_default();
    writeln!(
        svg,
        "<svg xmlns='http://www.w3.org/2000/svg' xmlns:xlink='http://www.w3.org/1999/xlink'{size} viewBox='{} {} {} {}'>",
        fmt_num(bbox.min.x),
        fmt_num(-bbox.max.y),
        fmt_num(bbox.width()),
        fmt_num(bbox.height())
    )
    .unwrap();
    writeln!(svg, "  <title>{}</title>", escape_xml(title)).unwrap();
    writeln!(svg, "  <g transform='scale(1 -1)'>").unwrap();
    svg
}

fn close_svg(mut svg: String) -> String {
    writeln!(svg, "  </g>").unwrap();
    writeln!(svg, "</svg>").unwrap();
    svg
}

fn fill_rule_name(rule: FillRule) -> &'static str {
    match rule {
        FillRule::NonZero => "nonzero",
        FillRule::EvenOdd => "evenodd",
    }
}

fn write_shape(svg: &mut String, arena: &PathArena, role: LayerRole, shape: &Path) {
    let d = path_data(arena, shape);
    if d.is_empty() {
        return;
    }
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
            "    <path d='{d}' fill='{color}' fill-opacity='{}' fill-rule='{}'/>",
            fmt_num(opacity),
            fill_rule_name(shape.fill_rule().expect("mask shapes are filled"))
        )
        .unwrap();
    }
}

/// Native SVG path data in world millimeters, without the document's screen
/// flip. Arcs and cubic curves remain native, including full-circle arcs.
pub fn svg_path_data(contours: &[crate::geom::path::ContourBuf]) -> String {
    let mut data = String::new();
    for contour in contours {
        write_contour(&mut data, contour.cmds.iter().copied());
    }
    data
}

fn path_data(arena: &PathArena, shape: &Path) -> String {
    let mut data = String::new();
    for contour in arena.contours(shape.contours) {
        write_contour(&mut data, arena.cmds(*contour).iter().copied());
    }
    data
}

fn write_contour(data: &mut String, cmds: impl IntoIterator<Item = PathCmd>) {
    let mut current = Point::default();
    for cmd in cmds {
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
                write_arc(data, current, cmd);
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
    use crate::geom::path::ContourBuf;
    use crate::geom::{BBox, Paint};

    fn square(size: f64) -> ContourBuf {
        ContourBuf::new(vec![
            PathCmd::move_to(Point::new(0.0, 0.0)),
            PathCmd::line_to(Point::new(size, 0.0)),
            PathCmd::line_to(Point::new(size, size)),
            PathCmd::line_to(Point::new(0.0, size)),
            PathCmd::close(),
        ])
    }

    fn copper_artwork() -> artwork::Document<(), ()> {
        let mut doc = artwork::Document::new();
        doc.push_layer(artwork::Layer {
            name: "F.Cu".to_string(),
            role: LayerRole::Copper,
            side: Side::Top,
            objects: crate::geom::Span::EMPTY,
            bbox: BBox::empty(),
            meta: (),
        });
        doc
    }

    #[test]
    fn native_path_data_retains_curves_in_world_coordinates() {
        let contour = ContourBuf::new(vec![
            PathCmd::move_to(Point::new(12.0, -20.0)),
            PathCmd::arc_to(Point::new(8.0, -20.0), Point::new(10.0, -20.0), false),
            PathCmd::cubic_to(
                Point::new(7.0, -19.0),
                Point::new(7.0, -21.0),
                Point::new(8.0, -22.0),
            ),
            PathCmd::close(),
        ]);
        assert_eq!(
            svg_path_data(&[contour]),
            "M12 -20 A2 2 0 0 1 8 -20 C7 -19,7 -21,8 -22 Z"
        );
    }

    #[test]
    fn native_independently_filled_paths_preserve_voids_and_overlapping_material() {
        // Both loops intentionally have the same winding. Each source contour
        // is filled even-odd, and separate contours then union as material.
        let mut annulus = crate::geom::shapes::circle(6.0).unwrap().cmds;
        annulus.extend(crate::geom::shapes::circle(2.0).unwrap().cmds);
        let contours = vec![
            ContourBuf::new(annulus),
            crate::geom::path::transform_cmds(
                crate::geom::shapes::circle(2.0).unwrap().cmds,
                Affine2::translation(Point::new(2.5, 0.0)),
            ),
        ];
        let measured =
            crate::geom::ContourSet::from_filled_contours(&contours, crate::geom::tol::REGION_MM);
        let mut doc = mask::Document::<()>::new();
        let layer = doc.push_layer(mask::Layer::new("Routes", LayerRole::Drill, Side::None));
        for contour in contours {
            doc.push_shape(layer, FillRule::EvenOdd, vec![contour]);
        }
        let viewport = BBox::new(Point::new(-4.0, -4.0), Point::new(4.0, 4.0));
        let options =
            RenderOptions::default()
                .with_viewport(viewport)
                .with_size(SizeConstraint::Fixed {
                    width_px: 800,
                    height_px: 800,
                });
        let png = crate::render::png(&doc, &options).unwrap();
        let raster = resvg::tiny_skia::Pixmap::decode_png(&png).unwrap();
        for (point, filled) in [
            (Point::ZERO, false),
            (Point::new(0.5, 0.0), false),
            (Point::new(2.5, 0.0), true),
            (Point::new(2.8, 0.0), true),
            (Point::new(3.2, 0.0), true),
            (Point::new(3.7, 0.0), false),
        ] {
            assert_eq!(measured.contains_point(point), filled);
            let x = (100.0 * (point.x + 4.0)) as u32;
            let y = (100.0 * (4.0 - point.y)) as u32;
            assert_eq!(
                raster.pixel(x, y).unwrap().alpha() > 0,
                filled,
                "native rendering disagrees with checked material at {point:?}"
            );
        }
    }

    fn assert_native_and_composed_samples(
        doc: &artwork::Document<(), ()>,
        viewport: BBox,
        samples: &[(Point, bool)],
    ) {
        let options =
            RenderOptions::default()
                .with_viewport(viewport)
                .with_size(SizeConstraint::Fixed {
                    width_px: 800,
                    height_px: 800,
                });
        let native = crate::render::artwork_png(doc, &options).unwrap();
        let composed = crate::render::png(&artwork::compose_to_mask(doc), &options).unwrap();
        for (name, png) in [("native", native), ("composed", composed)] {
            let raster = resvg::tiny_skia::Pixmap::decode_png(&png).unwrap();
            for &(at, filled) in samples {
                let x = (800.0 * (at.x - viewport.min.x) / viewport.width()) as u32;
                let y = (800.0 * (viewport.max.y - at.y) / viewport.height()) as u32;
                let alpha = raster.pixel(x, y).unwrap().alpha();
                assert_eq!(alpha > 0, filled, "{name} image at {at:?}: alpha {alpha}");
            }
        }
    }

    #[test]
    fn native_artwork_preserves_clear_runs_repaint_and_final_drill_cutouts() {
        let mut doc = copper_artwork();
        let background = doc.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            vec![square(20.0)],
        );
        doc.push_object(
            0,
            artwork::Object::new(Polarity::Dark, Geometry::Region { path: background }),
        );
        for (diameter, polarity, stage) in [
            (8.0, Polarity::Clear, PaintStage::Base),
            (4.0, Polarity::Dark, PaintStage::Overlay),
            (1.0, Polarity::Dark, PaintStage::FinalCutout),
        ] {
            let aperture = doc.push_aperture(artwork::Aperture::circle(diameter));
            let mut object = artwork::Object::new(
                polarity,
                Geometry::Flash {
                    aperture,
                    transform: Affine2::translation(Point::new(10.0, 10.0)),
                },
            );
            object.order.stage = stage;
            doc.push_object(0, object);
        }
        artwork::normalize_bounds(&mut doc);

        // The nested mask is a real paint operation, not a black outline or
        // a layer-colored disk: the center and the cleared annulus are empty,
        // while copper painted after the first clear operation survives.
        assert_native_and_composed_samples(
            &doc,
            BBox::new(Point::ZERO, Point::new(20.0, 20.0)),
            &[
                (Point::new(10.0, 10.0), false),
                (Point::new(11.0, 10.0), true),
                (Point::new(13.0, 10.0), false),
                (Point::new(15.0, 10.0), true),
            ],
        );
        let rendered = artwork_svg(&doc, &RenderOptions::default());
        assert_eq!(rendered.matches("<mask ").count(), 2);
        assert_eq!(rendered.matches(" A").count(), 12);
    }

    #[test]
    fn native_artwork_keeps_aperture_holes_under_rotated_mirrored_placement() {
        let mut doc = copper_artwork();
        let aperture = doc.push_aperture(artwork::Aperture {
            shape: artwork::ApertureShape::Obround {
                width: 6.0,
                height: 2.0,
            },
            hole_diameter: 0.8,
        });
        let transform = Affine2::placement(
            Point::new(8.0, 7.0),
            37.0,
            crate::geom::Mirror { x: true, y: false },
            1.0,
        );
        doc.push_object(
            0,
            artwork::Object::new(
                Polarity::Dark,
                Geometry::Flash {
                    aperture,
                    transform,
                },
            ),
        );
        artwork::normalize_bounds(&mut doc);

        let samples = [
            (Point::ZERO, false),
            (Point::new(1.0, 0.0), true),
            (Point::new(2.8, 0.0), true),
            (Point::new(3.2, 0.0), false),
            (Point::new(0.0, 1.2), false),
        ]
        .map(|(at, filled)| (transform.transform_point(at), filled));
        assert_native_and_composed_samples(
            &doc,
            BBox::new(Point::ZERO, Point::new(16.0, 16.0)),
            &samples,
        );
        let rendered = artwork_svg(&doc, &RenderOptions::default());
        assert!(rendered.contains("fill-rule='evenodd'"));
        assert!(rendered.contains("<use href='#a0' transform='matrix("));
        assert!(rendered.contains(" A"), "native rounded edges stay arcs");
    }

    #[test]
    fn explicit_viewport_preserves_world_coordinates_and_raster_aspect() {
        let viewport = BBox::new(Point::new(-4.0, 7.0), Point::new(6.0, 9.0));
        let options = RenderOptions::default()
            .with_viewport(viewport)
            .with_size(SizeConstraint::MaxDimension(100));
        let mut mask = mask::Document::<()>::new();
        let layer = mask.push_layer(Layer::new("Copper", LayerRole::Copper, Side::Top));
        mask.push_shape(layer, FillRule::NonZero, vec![square(20.0)]);
        let rendered = svg(&mask, &options);
        assert!(rendered.contains("viewBox='-4 -9 10 2'"));
        assert!(rendered.contains("width='100' height='20'"));
        assert!(rendered.contains("scale(1 -1)"));
        assert!(rendered.contains("M0 0 L20 0"));
        let png = crate::render::png(&mask, &options).unwrap();
        assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 100);
        assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()), 20);

        let artwork = copper_artwork();
        assert!(artwork_svg(&artwork, &options).contains("viewBox='-4 -9 10 2'"));
        let png = crate::render::artwork_png(&artwork, &options).unwrap();
        assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 100);
        assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()), 20);
    }

    #[test]
    fn artwork_svg_shares_one_defs_shape_across_repeated_flashes() {
        let mut doc = copper_artwork();
        let aperture = doc.push_aperture(artwork::Aperture::circle(1.0));
        for index in 0..4 {
            doc.push_object(
                0,
                artwork::Object::new(
                    Polarity::Dark,
                    Geometry::Flash {
                        aperture,
                        transform: Affine2::translation(Point::new(f64::from(index) * 2.0, 0.0)),
                    },
                ),
            );
        }
        artwork::normalize_bounds(&mut doc);

        let svg = artwork_svg(&doc, &RenderOptions::default());

        assert_eq!(svg.matches("<path id='a0'").count(), 1);
        assert_eq!(svg.matches("<use href='#a0'").count(), 4);
    }

    #[test]
    fn artwork_svg_keeps_filled_regions_unstroked() {
        // A layer group carries the colour for both fills and strokes, so a
        // region that does not opt out would gain the default one-unit
        // outline and swallow neighbouring clearances.
        let mut doc = copper_artwork();
        let path = doc.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            vec![square(10.0)],
        );
        doc.push_object(
            0,
            artwork::Object::new(Polarity::Dark, Geometry::Region { path }),
        );
        artwork::normalize_bounds(&mut doc);

        let svg = artwork_svg(&doc, &RenderOptions::default());

        assert!(svg.contains("stroke='none'"), "{svg}");
    }

    #[test]
    fn artwork_svg_images_patterned_strokes_as_dashes() {
        let mut doc = copper_artwork();
        let path = doc.push_path(
            Paint::Stroke(StrokeStyle {
                pattern: crate::geom::LinePattern::Dashed,
                ..StrokeStyle::round(0.2)
            }),
            vec![ContourBuf::new(vec![
                PathCmd::move_to(Point::new(0.0, 0.0)),
                PathCmd::line_to(Point::new(20.0, 0.0)),
            ])],
        );
        doc.push_object(
            0,
            artwork::Object::new(Polarity::Dark, Geometry::Stroke { path }),
        );
        artwork::normalize_bounds(&mut doc);

        let svg = artwork_svg(&doc, &RenderOptions::default());

        // Expanded into separate filled dashes rather than one native stroke.
        assert!(!svg.contains("stroke-width"), "{svg}");
        assert!(svg.matches('M').count() > 1, "{svg}");
    }

    #[test]
    fn artwork_svg_masks_a_clear_run_over_earlier_paint() {
        let mut doc = copper_artwork();
        let pour = doc.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            vec![square(10.0)],
        );
        let void = doc.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            vec![square(2.0)],
        );
        doc.push_object(
            0,
            artwork::Object::new(Polarity::Dark, Geometry::Region { path: pour }),
        );
        doc.push_object(
            0,
            artwork::Object::new(Polarity::Clear, Geometry::Region { path: void }),
        );
        artwork::normalize_bounds(&mut doc);

        let svg = artwork_svg(&doc, &RenderOptions::default());

        assert_eq!(svg.matches("<mask id='m0'").count(), 1);
        assert_eq!(svg.matches("<g mask='url(#m0)'>").count(), 1);
        // The mask's covering rect shares the user space of the geometry it
        // masks, so it spans the layer bounds in source coordinates. Flipping
        // it would leave the masked content outside the rect and drop the
        // whole layer.
        assert!(
            svg.contains("<rect x='-1' y='-1' width='12' height='12'"),
            "{svg}"
        );
    }

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
        assert!(svg.contains("stroke-width='0.1'"));
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
