//! Raster backend: paints documents straight into a pixmap.
//!
//! Sequential polarity is sequential painting. A layer paints into its own
//! pixmap, dark objects over it and clear objects out of it, and the finished
//! layer composites onto the image at its role's opacity, so overlapping
//! objects never darken each other and a clear never reaches another layer.

use std::f64::consts::FRAC_PI_2;

use tiny_skia::{BlendMode, PathBuilder, Pixmap, Stroke, Transform};

use crate::dialects::artwork::{self, Placed, Primitive};
use crate::dialects::{LayerRole, mask};
use crate::geom::path::PathCmd;
use crate::geom::{
    AccuracyError, Affine2, BBox, EllipticalArc, FillRule, GeometryAccuracy, LineCap, Paint, Point,
    Polarity, StrokeStyle,
};
use crate::render::{Drawn, LayerStyle, RenderOptions, SizeConstraint};

/// Width of the outline a composed profile layer draws as.
const PROFILE_STROKE_MM: f64 = 0.1;

/// Rasterize mask layers to a PNG. `Auto` renders at the default maximum
/// dimension; `MaxDimension`/`Fixed` control the output size.
pub fn png<LayerMeta>(
    doc: &mask::Document<LayerMeta>,
    options: &RenderOptions,
) -> Result<Vec<u8>, String> {
    let layers = crate::render::layer_indices(doc.layers.len(), options.layers.as_deref());
    let bbox = options.viewport_over(layers.iter().map(|&index| doc.layers[index].bbox));
    let mut canvas = Canvas::new(bbox, options.size)?;
    for &index in &layers {
        let layer = &doc.layers[index];
        let style = options.style(index, layer.role);
        let ink = |rule| match layer.role {
            LayerRole::Profile => Ink::Stroke(Stroke {
                width: PROFILE_STROKE_MM as f32,
                line_join: tiny_skia::LineJoin::Round,
                ..Stroke::default()
            }),
            _ => Ink::Fill(fill_rule(rule)),
        };
        for shape in doc.shapes(layer) {
            let contours = doc.arena.contours(shape.contours).iter();
            let path = outline(
                contours.map(|contour| doc.arena.cmds(*contour)),
                canvas.px_per_mm(),
            );
            if let Some(path) = path {
                let ink = ink(shape.fill_rule().expect("mask shapes are filled"));
                canvas.paint(
                    &Shape { path, ink },
                    Polarity::Dark,
                    style,
                    Affine2::IDENTITY,
                );
            }
        }
        canvas.composite(style);
    }
    canvas.encode()
}

/// Rasterize artwork layers to a PNG.
///
/// The image is the one the SVG backend describes, painted directly: every
/// placed primitive in paint order, shared geometry built once and drawn
/// under each placement's transform.
pub fn artwork_png<LayerMeta, ObjectMeta>(
    doc: &artwork::Document<LayerMeta, ObjectMeta>,
    options: &RenderOptions,
) -> Result<Vec<u8>, String> {
    let layers = crate::render::layer_indices(doc.layers.len(), options.layers.as_deref());
    let bbox = options.viewport_over(layers.iter().map(|&index| doc.layers[index].bbox));
    let mut canvas = Canvas::new(bbox, options.size)?;
    let placed = layers
        .iter()
        .map(|&index| artwork::placed_layer(doc, &doc.layers[index], &mut Vec::new()))
        .collect::<Vec<_>>();
    let extent = layers
        .iter()
        .map(|&index| doc.layers[index].bbox)
        .fold(BBox::empty(), BBox::union);
    let shapes = Shapes::build(doc, &placed, extent, canvas.px_per_mm(), options.accuracy)
        .map_err(|error| error.to_string())?;
    for (&index, placed) in layers.iter().zip(&placed) {
        let style = options.style(index, doc.layers[index].role);
        for placed in placed {
            if let Some(shape) = shapes.of(placed.primitive) {
                canvas.paint(shape, placed.polarity, style, placed.transform);
            }
        }
        canvas.composite(style);
    }
    canvas.encode()
}

/// The output image, the layer being painted, and the map onto both.
struct Canvas {
    image: Pixmap,
    layer: Pixmap,
    /// Y-up millimetres to y-down pixels: the viewport fitted inside the
    /// raster and centred, as an SVG `viewBox` is.
    view: Affine2,
}

impl Canvas {
    fn new(bbox: BBox, size: SizeConstraint) -> Result<Self, String> {
        let (width, height) = size.pixels(bbox).unwrap_or_else(|| {
            crate::render::pixel_size(bbox, crate::render::DEFAULT_MAX_DIMENSION_PX)
        });
        let pixmap = || {
            Pixmap::new(width, height)
                .ok_or_else(|| format!("failed to allocate {width}x{height} PNG raster"))
        };
        let (width, height) = (f64::from(width), f64::from(height));
        let scale = (width / bbox.width()).min(height / bbox.height());
        Ok(Self {
            image: pixmap()?,
            layer: pixmap()?,
            view: Affine2 {
                m00: scale,
                m01: 0.0,
                m02: (width - scale * bbox.width()) / 2.0 - scale * bbox.min.x,
                m10: 0.0,
                m11: -scale,
                m12: (height - scale * bbox.height()) / 2.0 + scale * bbox.max.y,
            },
        })
    }

    fn px_per_mm(&self) -> f64 {
        self.view.m00
    }

    /// Paint one placement of a shape into the open layer: dark lays the
    /// layer's colour over it, clear erases what the layer holds there.
    fn paint(&mut self, shape: &Shape, polarity: Polarity, style: LayerStyle, placement: Affine2) {
        let mut paint = tiny_skia::Paint::default();
        match polarity {
            Polarity::Dark => {
                let [_, red, green, blue] = style.color.to_be_bytes();
                paint.set_color_rgba8(red, green, blue, u8::MAX);
            }
            Polarity::Clear => paint.blend_mode = BlendMode::DestinationOut,
        }
        let Affine2 {
            m00,
            m01,
            m02,
            m10,
            m11,
            m12,
        } = self.view.concat(placement);
        let transform = Transform::from_row(
            m00 as f32, m10 as f32, m01 as f32, m11 as f32, m02 as f32, m12 as f32,
        );
        // A stroke outlines in the shape's own frame and the outline takes
        // the transform, so a placement that is not a similarity images the
        // stroke it actually transforms.
        match &shape.ink {
            Ink::Fill(rule) => self
                .layer
                .fill_path(&shape.path, &paint, *rule, transform, None),
            Ink::Stroke(stroke) => {
                self.layer
                    .stroke_path(&shape.path, &paint, stroke, transform, None)
            }
        }
    }

    /// Lay the painted layer over the image at its opacity and leave it
    /// empty for the next.
    ///
    /// Both pixmaps hold premultiplied colour, so every channel, alpha
    /// included, composites alike: `layer·opacity + image·(1 − layer
    /// alpha·opacity)`. Most of a layer is empty and is skipped.
    fn composite(&mut self, style: LayerStyle) {
        let faded: [u8; 256] =
            std::array::from_fn(|value| (value as f64 * style.opacity).round() as u8);
        let (image, _) = self.image.data_mut().as_chunks_mut::<4>();
        let (layer, _) = self.layer.data_mut().as_chunks_mut::<4>();
        for (image, layer) in image.iter_mut().zip(layer) {
            if layer[3] == 0 {
                continue;
            }
            let kept = 255 - u32::from(faded[usize::from(layer[3])]);
            for (image, layer) in image.iter_mut().zip(layer) {
                *image =
                    faded[usize::from(*layer)] + ((u32::from(*image) * kept + 127) / 255) as u8;
                *layer = 0;
            }
        }
    }

    fn encode(self) -> Result<Vec<u8>, String> {
        self.image
            .encode_png()
            .map_err(|err| format!("failed to encode PNG: {err}"))
    }
}

enum Ink {
    Fill(tiny_skia::FillRule),
    Stroke(Stroke),
}

/// Geometry in its own frame, ready to paint under any placement.
struct Shape {
    path: tiny_skia::Path,
    ink: Ink,
}

/// The apertures and paths the painted layers image, by index; `None` where
/// nothing places one or it has no extent.
struct Shapes {
    apertures: Vec<Option<Shape>>,
    paths: Vec<Option<Shape>>,
}

impl Shapes {
    /// Build each placed primitive once, for the largest scale any placement
    /// draws it at: that scale sets both how finely its arcs divide and what
    /// the accuracy budget leaves it in its own frame.
    fn build<LayerMeta, ObjectMeta>(
        doc: &artwork::Document<LayerMeta, ObjectMeta>,
        placed: &[Vec<Placed<'_, ObjectMeta>>],
        extent: BBox,
        px_per_mm: f64,
        accuracy: GeometryAccuracy,
    ) -> Result<Self, AccuracyError> {
        let mut apertures = vec![None::<f64>; doc.apertures.len()];
        let mut paths = vec![None::<f64>; doc.arena.paths.len()];
        for placed in placed.iter().flatten() {
            let slot = match placed.primitive {
                Primitive::Flash(aperture) => &mut apertures[aperture as usize],
                Primitive::Path(path) => &mut paths[path as usize],
            };
            let scale = placed.transform.max_scale();
            *slot = Some(slot.map_or(scale, |largest| largest.max(scale)));
        }
        let local = |scale: f64| {
            let accuracy = crate::render::local_accuracy(accuracy, extent, scale)?;
            Ok::<_, AccuracyError>((accuracy, scale * px_per_mm))
        };
        Ok(Self {
            apertures: doc
                .apertures
                .iter()
                .zip(apertures)
                .map(|(aperture, scale)| {
                    let Some(scale) = scale else { return Ok(None) };
                    let (accuracy, px_per_unit) = local(scale)?;
                    aperture_shape(aperture, accuracy, px_per_unit)
                })
                .collect::<Result<_, AccuracyError>>()?,
            paths: (0u32..)
                .zip(paths)
                .map(|(path, scale)| {
                    let Some(scale) = scale else { return Ok(None) };
                    let (accuracy, px_per_unit) = local(scale)?;
                    path_shape(doc, path, accuracy, px_per_unit)
                })
                .collect::<Result<_, AccuracyError>>()?,
        })
    }

    fn of(&self, primitive: Primitive) -> Option<&Shape> {
        match primitive {
            Primitive::Flash(aperture) => self.apertures[aperture as usize].as_ref(),
            Primitive::Path(path) => self.paths[path as usize].as_ref(),
        }
    }
}

fn aperture_shape(
    aperture: &artwork::Aperture,
    accuracy: GeometryAccuracy,
    px_per_unit: f64,
) -> Result<Option<Shape>, AccuracyError> {
    let contours = aperture.contours();
    for contour in &contours {
        accuracy.check(contour.uncertainty_mm)?;
    }
    let path = outline(
        contours.iter().map(|contour| &contour.cmds[..]),
        px_per_unit,
    );
    Ok(path.map(|path| Shape {
        path,
        ink: Ink::Fill(fill_rule(aperture.fill_rule())),
    }))
}

fn path_shape<LayerMeta, ObjectMeta>(
    doc: &artwork::Document<LayerMeta, ObjectMeta>,
    path: u32,
    accuracy: GeometryAccuracy,
    px_per_unit: f64,
) -> Result<Option<Shape>, AccuracyError> {
    let arena = &doc.arena;
    let path = arena.path(path);
    let shape = |path: Option<tiny_skia::Path>, ink| path.map(|path| Shape { path, ink });
    let ink = match path.paint {
        Paint::None => return Ok(None),
        // A raster stroke has no notion of IPC line patterns, so a patterned
        // stroke images through the same expansion the region fold uses.
        Paint::Stroke(stroke) if !stroke.is_solid() => {
            let dashes =
                crate::geom::path::stroke_to_fill(&arena.path_contours(path), stroke, accuracy)?
                    .unwrap_or_default();
            let cmds = dashes.iter().map(|contour| &contour.cmds[..]);
            return Ok(shape(
                outline(cmds, px_per_unit),
                Ink::Fill(tiny_skia::FillRule::Winding),
            ));
        }
        Paint::Stroke(stroke) => Ink::Stroke(stroke_style(stroke)),
        Paint::Fill { rule } => Ink::Fill(fill_rule(rule)),
    };
    let contours = arena.contours(path.contours);
    for contour in contours {
        accuracy.check(contour.uncertainty_mm)?;
    }
    let cmds = contours.iter().map(|contour| arena.cmds(*contour));
    Ok(shape(outline(cmds, px_per_unit), ink))
}

fn fill_rule(rule: FillRule) -> tiny_skia::FillRule {
    match rule {
        FillRule::NonZero => tiny_skia::FillRule::Winding,
        FillRule::EvenOdd => tiny_skia::FillRule::EvenOdd,
    }
}

fn stroke_style(stroke: StrokeStyle) -> Stroke {
    Stroke {
        width: stroke.width as f32,
        line_cap: match stroke.cap {
            LineCap::Round => tiny_skia::LineCap::Round,
            LineCap::Square => tiny_skia::LineCap::Square,
            LineCap::Butt => tiny_skia::LineCap::Butt,
        },
        line_join: tiny_skia::LineJoin::Round,
        ..Stroke::default()
    }
}

/// Contours as one raster path in their own frame; `px_per_unit` is the
/// largest scale the path is drawn at.
fn outline<'a>(
    contours: impl IntoIterator<Item = &'a [PathCmd]>,
    px_per_unit: f64,
) -> Option<tiny_skia::Path> {
    let mut path = PathBuilder::new();
    for cmds in contours {
        for drawn in crate::render::drawn(cmds.iter().copied()) {
            match drawn {
                Drawn::Move(to) => path.move_to(to.x as f32, to.y as f32),
                Drawn::Line(to) => path.line_to(to.x as f32, to.y as f32),
                Drawn::Arc(arc) => arc_to(&mut path, arc, px_per_unit),
                Drawn::Close => path.close(),
            }
        }
    }
    path.finish()
}

fn cubic_to(path: &mut PathBuilder, c1: Point, c2: Point, to: Point) {
    path.cubic_to(
        c1.x as f32,
        c1.y as f32,
        c2.x as f32,
        c2.y as f32,
        to.x as f32,
        to.y as f32,
    );
}

/// Each source of arc error gets half of a tenth of a pixel.
const ARC_ERROR_PX: f64 = 0.05;

/// The longest sweep one cubic may draw of an arc whose longer semi-axis is
/// `radius_px`, keeping the drawn arc within a tenth of a pixel of the true
/// one.
///
/// The cubic whose handles run `4/3·tan(δ/4)` along the end tangents leaves a
/// unit arc of sweep `δ ≤ π/2` by at most `δ⁶/55000` (2.7e-4 at a quarter
/// turn), and the rasterizer then draws any cubic as at most 64 chords, each
/// leaving the arc by `(δ/64)²/8`. An affine image scales both by at most
/// the longer semi-axis.
fn arc_step(radius_px: f64) -> f64 {
    let budget = ARC_ERROR_PX / radius_px;
    (55_000.0 * budget)
        .powf(1.0 / 6.0)
        .min(64.0 * (8.0 * budget).sqrt())
        .min(FRAC_PI_2)
}

fn arc_to(path: &mut PathBuilder, arc: EllipticalArc, px_per_unit: f64) {
    let sweep = arc.signed_sweep_radians();
    let pieces = (sweep.abs() / arc_step(arc.max_scale() * px_per_unit))
        .ceil()
        .max(1.0);
    let step = sweep / pieces;
    let handle = 4.0 / 3.0 * (step / 4.0).tan();
    let tangent = |angle: f64| arc.y_axis * angle.cos() - arc.x_axis * angle.sin();
    let start = arc.start_angle();
    let mut from = arc.start;
    for piece in 1..=pieces as u32 {
        let (from_angle, to_angle) = (
            start + step * f64::from(piece - 1),
            start + step * f64::from(piece),
        );
        let to = if f64::from(piece) == pieces {
            arc.end
        } else {
            arc.point_at(to_angle)
        };
        cubic_to(
            path,
            from + tangent(from_angle) * handle,
            to - tangent(to_angle) * handle,
            to,
        );
        from = to;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialects::Side;
    use crate::dialects::artwork::Geometry;
    use crate::geom::path::ContourBuf;
    use crate::render::svg::tests::{assert_native_and_composed_samples, copper_artwork, square};

    fn fill(doc: &mut artwork::Document<(), ()>, contour: ContourBuf) -> Geometry {
        let rule = FillRule::NonZero;
        Geometry::Region {
            path: doc.push_path(Paint::Fill { rule }, [contour]),
        }
    }

    #[test]
    fn arcs_divide_until_their_cubics_stay_within_the_pixel_budget() {
        for (radius_px, pieces) in [(10.0, 4), (1_000.0, 6), (100_000.0, 50)] {
            let px_per_unit = radius_px / 100.0;
            let start = Point::new(100.0, 0.0);
            let circle = [
                PathCmd::move_to(start),
                PathCmd::arc_to(start, Point::ZERO, false),
            ];
            let path = outline([&circle[..]], px_per_unit).unwrap();
            let mut from = tiny_skia::Point::zero();
            let mut cubics = 0;
            for segment in path.segments() {
                let tiny_skia::PathSegment::CubicTo(c1, c2, to) = segment else {
                    if let tiny_skia::PathSegment::MoveTo(to) = segment {
                        from = to;
                    }
                    continue;
                };
                cubics += 1;
                for step in 0..=32 {
                    let t = f64::from(step) / 32.0;
                    let u = 1.0 - t;
                    let at = |p: fn(&tiny_skia::Point) -> f32| {
                        u * u * u * f64::from(p(&from))
                            + 3.0 * u * u * t * f64::from(p(&c1))
                            + 3.0 * u * t * t * f64::from(p(&c2))
                            + t * t * t * f64::from(p(&to))
                    };
                    let error_px = (at(|p| p.x).hypot(at(|p| p.y)) - 100.0).abs() * px_per_unit;
                    assert!(error_px <= ARC_ERROR_PX, "{radius_px}: {error_px}");
                }
                from = to;
            }
            assert_eq!(cubics, pieces, "radius {radius_px} px");
            let chord = std::f64::consts::TAU / f64::from(pieces) / 64.0;
            assert!(radius_px * chord * chord / 8.0 <= ARC_ERROR_PX);
        }
    }

    #[test]
    fn a_stroke_placed_by_a_non_similarity_images_the_transformed_outline() {
        // A 1 mm trace along x under diag(2, 0.5) is 0.5 mm tall and, with
        // its round ends stretched, reaches 1 mm past each end.
        let mut doc = copper_artwork();
        let block = doc.push_block();
        let trace = doc.push_path(
            Paint::Stroke(StrokeStyle::round(1.0)),
            [ContourBuf::new(vec![
                PathCmd::move_to(Point::new(1.0, 4.0)),
                PathCmd::line_to(Point::new(4.0, 4.0)),
            ])],
        );
        doc.push_block_object(
            block,
            artwork::Object::new(Polarity::Dark, Geometry::Stroke { path: trace }),
        );
        let transform = Affine2 {
            m00: 2.0,
            m11: 0.5,
            ..Affine2::IDENTITY
        };
        doc.push_object(
            0,
            artwork::Object::new(Polarity::Dark, Geometry::Instance { block, transform }),
        );
        artwork::normalize_bounds(&mut doc);

        assert_native_and_composed_samples(
            &doc,
            BBox::new(Point::ZERO, Point::new(10.0, 10.0)),
            &[
                (Point::new(5.0, 2.0), true),
                (Point::new(5.0, 2.2), true),
                (Point::new(5.0, 1.8), true),
                (Point::new(5.0, 2.3), false),
                (Point::new(5.0, 1.7), false),
                (Point::new(1.1, 2.0), true),
                (Point::new(8.9, 2.0), true),
                (Point::new(0.9, 2.0), false),
                (Point::new(9.1, 2.0), false),
            ],
        );
    }

    #[test]
    fn a_block_that_clears_erases_earlier_paint_at_every_placement() {
        let mut doc = copper_artwork();
        let pour = fill(&mut doc, square(20.0));
        doc.push_object(0, artwork::Object::new(Polarity::Dark, pour));
        // An antipad with its pad: the clear reaches the pour under it, the
        // pad painted after the clear survives.
        let block = doc.push_block();
        for (diameter, polarity) in [(4.0, Polarity::Clear), (2.0, Polarity::Dark)] {
            let aperture = doc.push_aperture(artwork::Aperture::circle(diameter));
            doc.push_block_object(
                block,
                artwork::Object::new(
                    polarity,
                    Geometry::Flash {
                        aperture,
                        transform: Affine2::IDENTITY,
                    },
                ),
            );
        }
        doc.push_object(
            0,
            artwork::Object::new(
                Polarity::Dark,
                Geometry::GridInstance {
                    block,
                    transform: Affine2::translation(Point::new(5.0, 5.0)),
                    repeat: artwork::GridRepeat {
                        x_count: 2,
                        y_count: 1,
                        x_step: Point::new(10.0, 0.0),
                        y_step: Point::ZERO,
                    },
                },
            ),
        );
        // A clear placement inverts the block: it erases the pad's disk and
        // repaints the antipad's.
        doc.push_object(
            0,
            artwork::Object::new(
                Polarity::Clear,
                Geometry::Instance {
                    block,
                    transform: Affine2::translation(Point::new(10.0, 15.0)),
                },
            ),
        );
        artwork::normalize_bounds(&mut doc);

        assert_native_and_composed_samples(
            &doc,
            BBox::new(Point::ZERO, Point::new(20.0, 20.0)),
            &[
                (Point::new(1.0, 1.0), true),
                (Point::new(5.0, 5.0), true),
                (Point::new(6.5, 5.0), false),
                (Point::new(15.0, 5.0), true),
                (Point::new(16.5, 5.0), false),
                (Point::new(10.0, 15.0), false),
                (Point::new(11.5, 15.0), true),
            ],
        );
    }

    #[test]
    fn a_clear_erases_only_its_own_layer() {
        let mut doc = copper_artwork();
        let pour = fill(&mut doc, square(10.0));
        doc.push_object(0, artwork::Object::new(Polarity::Dark, pour));
        let mask = doc.push_layer(artwork::Layer::new(
            "F.Mask",
            LayerRole::Soldermask,
            Side::Top,
        ));
        let cover = fill(&mut doc, square(10.0));
        doc.push_object(mask, artwork::Object::new(Polarity::Dark, cover));
        let opening = fill(&mut doc, square(4.0));
        doc.push_object(mask, artwork::Object::new(Polarity::Clear, opening));
        artwork::normalize_bounds(&mut doc);

        let options = RenderOptions::default()
            .with_viewport(BBox::new(Point::ZERO, Point::new(10.0, 10.0)))
            .with_size(SizeConstraint::Fixed {
                width_px: 100,
                height_px: 100,
            });
        let image = Pixmap::decode_png(&artwork_png(&doc, &options).unwrap()).unwrap();
        let pixel = |x: f64, y: f64| {
            let pixel = image.pixel((10.0 * x) as u32, (10.0 * (10.0 - y)) as u32);
            pixel.unwrap().demultiply()
        };
        // Through the opening the copper shows at its own opacity; elsewhere
        // the mask tints it.
        let (copper, masked) = (pixel(2.0, 2.0), pixel(7.0, 7.0));
        assert_eq!(copper.alpha(), 230);
        assert!(copper.red().abs_diff(0xd8) <= 1 && copper.green().abs_diff(0x78) <= 1);
        assert!(masked.alpha() > copper.alpha() && masked.red() < copper.red());
    }

    #[test]
    fn a_layer_style_overrides_its_role_in_both_backends() {
        let mut doc = copper_artwork();
        let pour = fill(&mut doc, square(10.0));
        doc.push_object(0, artwork::Object::new(Polarity::Dark, pour));
        artwork::normalize_bounds(&mut doc);
        let options = RenderOptions::default()
            .with_size(SizeConstraint::MaxDimension(10))
            .with_styles([LayerStyle {
                color: 0x204060,
                opacity: 1.0,
            }]);

        let svg = crate::render::artwork_svg(&doc, &options).unwrap();
        assert!(svg.contains("<g fill='#204060' stroke='#204060' opacity='1'>"));
        let image = Pixmap::decode_png(&artwork_png(&doc, &options).unwrap()).unwrap();
        let pixel = image.pixel(5, 5).unwrap();
        assert_eq!(
            [pixel.red(), pixel.green(), pixel.blue(), pixel.alpha()],
            [0x20, 0x40, 0x60, 0xff]
        );
    }

    #[test]
    fn a_zero_length_round_stroke_paints_a_dot() {
        let mut doc = copper_artwork();
        let dot = doc.push_path(
            Paint::Stroke(StrokeStyle::round(2.0)),
            [ContourBuf::new(vec![
                PathCmd::move_to(Point::new(5.0, 5.0)),
                PathCmd::line_to(Point::new(5.0, 5.0)),
            ])],
        );
        doc.push_object(
            0,
            artwork::Object::new(Polarity::Dark, Geometry::Stroke { path: dot }),
        );
        artwork::normalize_bounds(&mut doc);

        assert_native_and_composed_samples(
            &doc,
            BBox::new(Point::ZERO, Point::new(10.0, 10.0)),
            &[
                (Point::new(5.0, 5.0), true),
                (Point::new(5.8, 5.0), true),
                (Point::new(5.0, 6.2), false),
            ],
        );
    }
}
