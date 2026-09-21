use crate::geom::{AccuracyError, EllipticalArc, GeometryAccuracy};
use std::fmt::Write;

use crate::dialects::LayerRole;
use crate::dialects::artwork::{self, Geometry};
use crate::dialects::mask;
use crate::geom::path::PathCmd;

use crate::geom::{
    Affine2, BBox, FillRule, LineCap, LineJoin, Path, PathArena, Point, Polarity, StrokeStyle,
};
use crate::render::{Drawn, LayerStyle, RenderOptions, SizeConstraint};

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
        let style = options.style(layer_index, layer.role);
        for shape in doc.shapes(layer) {
            write_shape(&mut svg, &doc.arena, layer.role, style, shape);
        }
    }

    close_svg(svg)
}

/// Render artwork layers to an SVG document.
///
/// Apertures and blocks become `<defs>` that flashes and instances reference
/// with `<use>`, so repeated geometry stays repeated instead of being copied
/// per placement: a panel draws its board once. Polarity runs paint
/// sequentially: a clear run masks everything painted before it, which is
/// exactly how the same artwork images in Gerber.
pub fn artwork_svg<LayerMeta: Clone, ObjectMeta: Clone>(
    doc: &artwork::Document<LayerMeta, ObjectMeta>,
    options: &RenderOptions,
) -> Result<String, AccuracyError> {
    // A `<use>` can only add paint, so a block that clears has no symbol.
    let clears = |block: &artwork::Block<ObjectMeta>| {
        block
            .objects
            .iter()
            .any(|object| object.polarity == Polarity::Clear)
    };
    if doc.blocks.iter().any(clears) {
        return artwork_svg(&artwork::expand_instances(doc), options);
    }
    let ids = options.id_prefix.as_str();
    let layers = crate::render::layer_indices(doc.layers.len(), options.layers.as_deref());
    let bbox = options.viewport_or(crate::render::artwork_bbox(doc, Some(&layers)));
    let scales = PlacementScales::of(doc, &layers);
    // Shared geometry is written once in its own frame, so its budget is
    // what the largest placement leaves of the document's.
    let extent = layers
        .iter()
        .map(|&index| doc.layers[index].bbox)
        .fold(BBox::empty(), BBox::union);
    let local = |scale: f64| crate::render::local_accuracy(options.accuracy, extent, scale);

    let mut defs = String::new();
    for (index, aperture) in doc.apertures.iter().enumerate() {
        let Some(scale) = scales.apertures[index] else {
            continue;
        };
        // Colour is inherited from the referencing group so one aperture can
        // serve both a dark run and a clear run's mask; `stroke` is not, or
        // every filled shape would gain the default one-unit outline.
        write!(defs, "    <path id='{ids}a{index}' d='").unwrap();
        let accuracy = local(scale)?;
        for contour in aperture.contours() {
            check_native(contour.uncertainty_mm, accuracy)?;
            write_contour(&mut defs, contour.cmds.iter().copied());
        }
        writeln!(
            defs,
            "' fill-rule='{}' stroke='none'/>",
            fill_rule_name(aperture.fill_rule())
        )
        .unwrap();
    }
    for (index, block) in doc.blocks.iter().enumerate() {
        let Some(scale) = scales.blocks[index] else {
            continue;
        };
        writeln!(defs, "    <g id='{ids}b{index}'>").unwrap();
        let accuracy = local(scale)?;
        for object in &block.objects {
            write_artwork_object(&mut defs, doc, LayerRole::Other, object, accuracy, ids)?;
        }
        writeln!(defs, "    </g>").unwrap();
    }

    let mut body = String::new();
    let mut masks = 0;
    for &layer_index in &layers {
        write_artwork_layer(&mut body, &mut defs, &mut masks, doc, layer_index, options)?;
    }

    let title = layers
        .first()
        .and_then(|&index| doc.layers.get(index))
        .map(|layer| layer.name.as_str());
    let mut svg = open_svg(&bbox, pixel_size(options, bbox), title);
    writeln!(svg, "  <defs>\n{defs}  </defs>").unwrap();
    svg.push_str(&body);
    Ok(close_svg(svg))
}

/// The largest scale at which each aperture and block is placed, through
/// every chain of instances that reaches it; `None` where nothing does.
struct PlacementScales {
    apertures: Vec<Option<f64>>,
    blocks: Vec<Option<f64>>,
}

impl PlacementScales {
    fn of<LayerMeta, ObjectMeta>(
        doc: &artwork::Document<LayerMeta, ObjectMeta>,
        layers: &[usize],
    ) -> Self {
        let mut scales = Self {
            apertures: vec![None; doc.apertures.len()],
            blocks: vec![None; doc.blocks.len()],
        };
        for &layer in layers {
            scales.place(doc.layers[layer].objects.slice(&doc.objects), 1.0);
        }
        // Blocks reference only earlier blocks, so one backward sweep has
        // every block's scale settled before its children read it.
        for index in (0..doc.blocks.len()).rev() {
            if let Some(scale) = scales.blocks[index] {
                scales.place(&doc.blocks[index].objects, scale);
            }
        }
        scales
    }

    fn place<ObjectMeta>(&mut self, objects: &[artwork::Object<ObjectMeta>], scale: f64) {
        for object in objects {
            let (slot, placement) = match object.geometry {
                Geometry::Flash {
                    aperture,
                    transform,
                } => (self.apertures.get_mut(aperture as usize), transform),
                Geometry::Instance { block, transform }
                | Geometry::GridInstance {
                    block, transform, ..
                } => (self.blocks.get_mut(block as usize), transform),
                Geometry::Stroke { .. } | Geometry::Region { .. } => continue,
            };
            if let Some(slot) = slot {
                let placed = scale * placement.max_scale();
                *slot = Some(slot.map_or(placed, |largest| largest.max(placed)));
            }
        }
    }
}

fn write_artwork_layer<LayerMeta, ObjectMeta>(
    body: &mut String,
    defs: &mut String,
    masks: &mut usize,
    doc: &artwork::Document<LayerMeta, ObjectMeta>,
    layer_index: usize,
    options: &RenderOptions,
) -> Result<(), AccuracyError> {
    let layer = &doc.layers[layer_index];
    let ids = options.id_prefix.as_str();
    // Sequential polarity: dark runs paint in order, and every clear run
    // becomes a mask over everything painted before it.
    let mut runs: Vec<(Polarity, String)> = Vec::new();
    for (polarity, object) in artwork::paint_ordered(layer, layer.objects.slice(&doc.objects)) {
        if runs.last().is_none_or(|(run, _)| *run != polarity) {
            runs.push((polarity, String::new()));
        }
        let (_, run) = runs.last_mut().expect("a run was just opened");
        write_artwork_object(run, doc, layer.role, object, options.accuracy, ids)?;
    }
    // A clear run removes from what is already painted, so with nothing
    // under it there is nothing to remove.
    let first_dark = runs
        .iter()
        .position(|(polarity, _)| *polarity == Polarity::Dark)
        .unwrap_or(runs.len());
    let runs = &runs[first_dark..];

    let bounds = layer.bbox.expand(1.0);
    let clear_runs = runs
        .iter()
        .filter(|(polarity, _)| *polarity == Polarity::Clear)
        .count();
    // One group opacity rather than per-object alpha, so overlapping objects
    // composite once instead of darkening where they touch.
    let LayerStyle { color, opacity } = options.style(layer_index, layer.role);
    writeln!(
        body,
        "    <g fill='#{color:06x}' stroke='#{color:06x}' opacity='{}'>",
        num(opacity)
    )
    .unwrap();
    // Each mask wraps all paint before its clear run, so the groups open
    // outermost-last-mask first and close one per clear run.
    for mask in (*masks..*masks + clear_runs).rev() {
        writeln!(body, "      <g mask='url(#{ids}m{mask})'>").unwrap();
    }
    for (polarity, run) in runs {
        match polarity {
            Polarity::Dark => body.push_str(run),
            Polarity::Clear => {
                let (x, y) = (num(bounds.min.x), num(bounds.min.y));
                let (width, height) = (num(bounds.width()), num(bounds.height()));
                writeln!(
                    defs,
                    "    <mask id='{ids}m{masks}' maskUnits='userSpaceOnUse' x='{x}' y='{y}' width='{width}' height='{height}'>\n      <rect x='{x}' y='{y}' width='{width}' height='{height}' fill='#ffffff'/>\n      <g fill='#000000' stroke='#000000'>\n{run}      </g>\n    </mask>",
                )
                .unwrap();
                *masks += 1;
                writeln!(body, "      </g>").unwrap();
            }
        }
    }
    writeln!(body, "    </g>").unwrap();
    Ok(())
}

fn write_artwork_object<LayerMeta, ObjectMeta>(
    out: &mut String,
    doc: &artwork::Document<LayerMeta, ObjectMeta>,
    role: LayerRole,
    object: &artwork::Object<ObjectMeta>,
    accuracy: GeometryAccuracy,
    ids: &str,
) -> Result<(), AccuracyError> {
    match object.geometry {
        Geometry::Flash {
            aperture,
            transform,
        } => {
            writeln!(
                out,
                "      <use href='#{ids}a{aperture}'{}/>",
                SvgTransform(transform)
            )
            .unwrap();
        }
        Geometry::Instance { block, transform } => {
            writeln!(
                out,
                "      <use href='#{ids}b{block}'{}/>",
                SvgTransform(transform)
            )
            .unwrap();
        }
        Geometry::GridInstance {
            block,
            transform,
            repeat,
        } => {
            for offset in repeat.offsets() {
                writeln!(
                    out,
                    "      <use href='#{ids}b{block}'{}/>",
                    SvgTransform(Affine2::translation(offset).concat(transform))
                )
                .unwrap();
            }
        }
        Geometry::Region { path } => {
            let path = doc.arena.path(path);
            out.push_str("      <path d='");
            write_native_path(out, &doc.arena, path, accuracy)?;
            writeln!(
                out,
                "' fill-rule='{}' stroke='none'/>",
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
                accuracy,
            )?
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
            out.push_str("      <path d='");
            write_native_path(out, &doc.arena, doc.arena.path(path), accuracy)?;
            writeln!(
                out,
                "' fill='none' stroke-width='{}' stroke-linecap='{}' stroke-linejoin='{}'{outline}/>",
                num(stroke.width),
                line_cap_name(stroke.cap),
                line_join_name(stroke.join),
            )
            .unwrap();
        }
    };
    Ok(())
}

/// Coordinates are written to six decimals, so every emitted point may sit
/// this far from its source along each axis.
const SVG_COORDINATE_GRID_MM: f64 = 1e-6;

/// Whether the SVG may draw a contour natively: the approximation it already
/// carries plus coordinate rounding counts against the budget.
fn check_native(uncertainty_mm: f64, accuracy: GeometryAccuracy) -> Result<(), AccuracyError> {
    accuracy.check(uncertainty_mm + SVG_COORDINATE_GRID_MM / std::f64::consts::SQRT_2)
}

fn write_native_path(
    out: &mut String,
    arena: &PathArena,
    path: &Path,
    accuracy: GeometryAccuracy,
) -> Result<(), AccuracyError> {
    for contour in arena.contours(path.contours) {
        check_native(contour.uncertainty_mm, accuracy)?;
        write_contour(out, arena.cmds(*contour).iter().copied());
    }
    Ok(())
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

/// A placement as a `transform` attribute. The linear part multiplies every
/// coordinate it places, so it keeps nine decimals where points keep six.
struct SvgTransform(Affine2);

impl std::fmt::Display for SvgTransform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Affine2 {
            m00,
            m01,
            m02,
            m10,
            m11,
            m12,
        } = self.0;
        let linear = |value| Num { value, decimals: 9 };
        write!(
            f,
            " transform='matrix({} {} {} {} {} {})'",
            linear(m00),
            linear(m10),
            linear(m01),
            linear(m11),
            num(m02),
            num(m12),
        )
    }
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
        num(bbox.min.x),
        num(-bbox.max.y),
        num(bbox.width()),
        num(bbox.height())
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

fn write_shape(
    svg: &mut String,
    arena: &PathArena,
    role: LayerRole,
    LayerStyle { color, opacity }: LayerStyle,
    shape: &Path,
) {
    let d = path_data(arena, shape);
    if d.is_empty() {
        return;
    }
    if role == LayerRole::Profile {
        writeln!(
            svg,
            "    <path d='{d}' fill='none' stroke='#{color:06x}' stroke-width='0.1' stroke-linejoin='round' data-board-outline='true'/>",
        )
        .unwrap();
    } else {
        writeln!(
            svg,
            "    <path d='{d}' fill='#{color:06x}' fill-opacity='{}' fill-rule='{}'/>",
            num(opacity),
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
    for drawn in crate::render::drawn(cmds) {
        match drawn {
            Drawn::Move(to) => {
                if !data.ends_with('\'') && !data.is_empty() {
                    data.push(' ');
                }
                write!(data, "M{} {}", num(to.x), num(to.y)).unwrap();
            }
            Drawn::Line(to) => write!(data, " L{} {}", num(to.x), num(to.y)).unwrap(),
            Drawn::Arc(arc) => write_elliptical_arc(data, arc),
            Drawn::Cubic(c1, c2, to) => write!(
                data,
                " C{} {},{} {},{} {}",
                num(c1.x),
                num(c1.y),
                num(c2.x),
                num(c2.y),
                num(to.x),
                num(to.y)
            )
            .unwrap(),
            Drawn::Close => data.push_str(" Z"),
        }
    }
}

/// An elliptical arc as SVG `A` commands: the principal axes and their
/// rotation describe the ellipse, the sweep flag picks the direction.
///
/// An arc of more than half a turn is drawn in two halves. SVG cannot say a
/// full turn in one command, drops an arc whose ends print alike, and picks
/// either side of an exact half turn; two short arcs through the midpoint
/// have none of those cases.
fn write_elliptical_arc(data: &mut String, arc: EllipticalArc) {
    let (major, minor, rotation) = arc.principal_axes();
    let sweep_flag = if arc.clockwise { 0 } else { 1 };
    let rotation_degrees = rotation.to_degrees();
    let mut write_piece = |end: Point| {
        write!(
            data,
            " A{} {} {} 0 {sweep_flag} {} {}",
            num(major),
            num(minor),
            num(rotation_degrees),
            num(end.x),
            num(end.y)
        )
        .unwrap();
    };
    let sweep = arc.signed_sweep_radians();
    if sweep.abs() > std::f64::consts::PI {
        write_piece(arc.point_at(arc.start_angle() + sweep / 2.0));
    }
    write_piece(arc.end);
}

fn escape_xml(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// A number in fixed notation with trailing zeros trimmed, written without
/// allocating: a layer's path data is millions of these.
struct Num {
    value: f64,
    decimals: u32,
}

fn num(value: f64) -> Num {
    Num { value, decimals: 6 }
}

impl std::fmt::Display for Num {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let unit = 10_u64.pow(self.decimals);
        let scaled = (self.value.abs() * unit as f64).round();
        // Past the exact integers, or not a number at all.
        if scaled.is_nan() || scaled >= 9.0e15 {
            return write!(f, "{}", self.value);
        }
        let scaled = scaled as u64;
        let (whole, mut fraction) = (scaled / unit, scaled % unit);
        if self.value < 0.0 && scaled != 0 {
            f.write_str("-")?;
        }
        write!(f, "{whole}")?;
        if fraction != 0 {
            let mut digits = self.decimals as usize;
            while fraction % 10 == 0 {
                fraction /= 10;
                digits -= 1;
            }
            write!(f, ".{fraction:0digits$}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::dialects::artwork::PaintStage;
    use crate::dialects::{Side, mask::Layer};
    use crate::geom::path::ContourBuf;
    use crate::geom::{BBox, Paint, Resolution};

    pub(crate) fn square(size: f64) -> ContourBuf {
        ContourBuf::new(vec![
            PathCmd::move_to(Point::new(0.0, 0.0)),
            PathCmd::line_to(Point::new(size, 0.0)),
            PathCmd::line_to(Point::new(size, size)),
            PathCmd::line_to(Point::new(0.0, size)),
            PathCmd::close(),
        ])
    }

    pub(crate) fn copper_artwork() -> artwork::Document<(), ()> {
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
            crate::geom::shapes::circle(2.0)
                .unwrap()
                .transformed(Affine2::translation(Point::new(2.5, 0.0))),
        ];
        let measured =
            crate::geom::ContourSet::from_filled_contours(&contours, Resolution::default())
                .unwrap();
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
        let raster = tiny_skia::Pixmap::decode_png(&png).unwrap();
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

    pub(crate) fn assert_native_and_composed_samples(
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
        let composed = crate::render::png(
            &artwork::compose_to_mask(doc, Resolution::default()).unwrap(),
            &options,
        )
        .unwrap();
        for (name, png) in [("native", native), ("composed", composed)] {
            let raster = tiny_skia::Pixmap::decode_png(&png).unwrap();
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
        let rendered = artwork_svg(&doc, &RenderOptions::default()).unwrap();
        assert_eq!(rendered.matches("<mask ").count(), 2);
        assert_eq!(rendered.matches(" A").count(), 12);
    }

    #[test]
    fn a_cutout_only_copper_layer_is_empty_but_drill_artwork_remains_visible() {
        let mut doc = copper_artwork();
        let aperture = doc.push_aperture(artwork::Aperture {
            shape: artwork::ApertureShape::Obround {
                width: 2.0,
                height: 1.0,
            },
            hole_diameter: 0.0,
        });
        let mut cutout = artwork::Object::new(
            Polarity::Dark,
            Geometry::Flash {
                aperture,
                transform: Affine2::translation(Point::new(1.5, 1.0)),
            },
        );
        cutout.order.stage = PaintStage::FinalCutout;
        doc.push_object(0, cutout);
        artwork::normalize_bounds(&mut doc);
        let bounds = BBox::new(Point::ZERO, Point::new(3.0, 2.0));
        for (role, filled) in [(LayerRole::Copper, false), (LayerRole::Drill, true)] {
            doc.layers[0].role = role;
            assert_native_and_composed_samples(
                &doc,
                bounds,
                &[
                    (Point::new(1.5, 1.0), filled),
                    (Point::new(0.75, 1.0), filled),
                    (Point::new(2.25, 1.0), filled),
                    (Point::new(0.25, 1.0), false),
                    (Point::new(1.5, 1.75), false),
                ],
            );
        }
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
        let rendered = artwork_svg(&doc, &RenderOptions::default()).unwrap();
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
        assert!(
            artwork_svg(&artwork, &options)
                .unwrap()
                .contains("viewBox='-4 -9 10 2'")
        );
        let png = crate::render::artwork_png(&artwork, &options).unwrap();
        assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 100);
        assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()), 20);
    }

    #[test]
    fn numbers_print_fixed_and_trimmed_without_negative_zero() {
        for (value, text) in [
            (0.0, "0"),
            (-0.0, "0"),
            (-0.000_000_4, "0"),
            (1.5, "1.5"),
            (-2.000_001, "-2.000001"),
            (123.456_789_4, "123.456789"),
            (0.000_001, "0.000001"),
            (1e7, "10000000"),
        ] {
            assert_eq!(num(value).to_string(), text);
        }
        assert_eq!(
            Num {
                value: std::f64::consts::FRAC_1_SQRT_2,
                decimals: 9
            }
            .to_string(),
            "0.707106781"
        );
    }

    #[test]
    fn arcs_past_half_a_turn_draw_as_two_short_arcs() {
        let three_quarters = ContourBuf::new(vec![
            PathCmd::move_to(Point::new(1.0, 0.0)),
            PathCmd::arc_to(Point::new(0.0, -1.0), Point::ZERO, false),
        ]);
        assert_eq!(
            svg_path_data(&[three_quarters]),
            "M1 0 A1 1 0 0 1 -0.707107 0.707107 A1 1 0 0 1 0 -1"
        );
        let full = ContourBuf::new(vec![
            PathCmd::move_to(Point::new(1.0, 0.0)),
            PathCmd::arc_to(Point::new(1.0, 0.0), Point::ZERO, true),
        ]);
        assert_eq!(
            svg_path_data(&[full]),
            "M1 0 A1 1 0 0 0 -1 0 A1 1 0 0 0 1 0"
        );
    }

    #[test]
    fn a_block_is_drawn_once_and_used_at_every_placement() {
        let mut doc = artwork::Document::<(), ()>::new();
        let block = doc.push_block();
        let path = doc.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            [square(1.0)],
        );
        doc.push_block_object(
            block,
            artwork::Object::new(Polarity::Dark, Geometry::Region { path }),
        );
        let layer = doc.push_layer(artwork::Layer::new("F.Cu", LayerRole::Copper, Side::Top));
        doc.push_object(
            layer,
            artwork::Object::new(
                Polarity::Dark,
                Geometry::GridInstance {
                    block,
                    transform: Affine2::IDENTITY,
                    repeat: artwork::GridRepeat {
                        x_count: 3,
                        y_count: 2,
                        x_step: Point::new(5.0, 0.0),
                        y_step: Point::new(0.0, 4.0),
                    },
                },
            ),
        );
        artwork::normalize_bounds(&mut doc);

        let svg = artwork_svg(&doc, &RenderOptions::default().with_id_prefix("p-")).unwrap();
        assert_eq!(svg.matches("<g id='p-b0'>").count(), 1);
        assert_eq!(svg.matches("M0 0 L1 0").count(), 1);
        assert_eq!(svg.matches("<use href='#p-b0'").count(), 6);
        assert!(svg.contains("<use href='#p-b0' transform='matrix(1 0 0 1 10 4)'/>"));
    }

    #[test]
    fn svg_charges_coordinate_rounding_against_the_budget() {
        let exact = square(1.0).uncertainty_mm;
        assert!(check_native(exact, GeometryAccuracy::new(1e-6).unwrap()).is_ok());
        assert!(matches!(
            check_native(exact, GeometryAccuracy::new(1e-7).unwrap()),
            Err(AccuracyError::BudgetExceeded { .. })
        ));
    }

    #[test]
    fn direct_and_instanced_renders_enforce_the_same_contour_budget() {
        let accuracy = GeometryAccuracy::new(0.001).unwrap();
        for contour in [
            square(1.0).with_uncertainty(0.02),
            crate::geom::shapes::ellipse(4.0, 2.0).unwrap(),
        ] {
            let refinable = contour.uncertainty_mm == 0.0;
            let mut source = copper_artwork();
            let aperture =
                source.push_aperture(artwork::Aperture::solid(artwork::ApertureShape::Contour {
                    outline: contour.clone(),
                    fill_rule: FillRule::NonZero,
                }));
            let region = source.push_path(
                Paint::Fill {
                    rule: FillRule::NonZero,
                },
                vec![contour.clone()],
            );
            let stroke = source.push_path(Paint::Stroke(StrokeStyle::round(0.1)), vec![contour]);
            for geometry in [
                Geometry::Flash {
                    aperture,
                    transform: Affine2::IDENTITY,
                },
                Geometry::Region { path: region },
                Geometry::Stroke { path: stroke },
            ] {
                for instanced in [false, true] {
                    let mut doc = source.clone();
                    let object = artwork::Object::new(Polarity::Dark, geometry);
                    if instanced {
                        let block = doc.push_block();
                        doc.push_block_object(block, object);
                        doc.push_object(
                            0,
                            artwork::Object::new(
                                Polarity::Dark,
                                Geometry::Instance {
                                    block,
                                    transform: Affine2::translation(Point::new(2.0, 3.0)),
                                },
                            ),
                        );
                    } else {
                        doc.push_object(0, object);
                    }
                    artwork::normalize_bounds(&mut doc);
                    let options = RenderOptions::default()
                        .with_accuracy(accuracy)
                        .with_size(SizeConstraint::MaxDimension(64));
                    for (backend, drawn) in [
                        ("svg", artwork_svg(&doc, &options).is_ok()),
                        ("png", crate::render::artwork_png(&doc, &options).is_ok()),
                    ] {
                        assert_eq!(
                            drawn, refinable,
                            "{backend}: geometry={geometry:?}, instanced={instanced}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn svg_flash_budget_accounts_for_placement_scale() {
        let accuracy = GeometryAccuracy::new(0.01).unwrap();
        for (scale, succeeds) in [(0.1, true), (2.0, false)] {
            let mut doc = copper_artwork();
            let aperture =
                doc.push_aperture(artwork::Aperture::solid(artwork::ApertureShape::Contour {
                    outline: square(1.0).with_uncertainty(0.02),
                    fill_rule: FillRule::NonZero,
                }));
            doc.push_object(
                0,
                artwork::Object::new(
                    Polarity::Dark,
                    Geometry::Flash {
                        aperture,
                        transform: Affine2 {
                            m00: scale,
                            m11: scale,
                            ..Affine2::IDENTITY
                        },
                    },
                ),
            );
            artwork::normalize_bounds(&mut doc);
            assert_eq!(
                artwork_svg(
                    &doc,
                    &RenderOptions {
                        accuracy,
                        ..RenderOptions::default()
                    }
                )
                .is_ok(),
                succeeds
            );
        }
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

        let svg = artwork_svg(&doc, &RenderOptions::default()).unwrap();

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

        let svg = artwork_svg(&doc, &RenderOptions::default()).unwrap();

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

        let svg = artwork_svg(&doc, &RenderOptions::default()).unwrap();

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

        let svg = artwork_svg(&doc, &RenderOptions::default()).unwrap();

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
