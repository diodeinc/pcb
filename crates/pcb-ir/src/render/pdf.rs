//! PDF backend: artwork as a vector plot.
//!
//! A plot is opaque ink laid on paper in paint order. Dark paint draws in
//! its layer's ink and clear paint in the paper's colour, since PDF erases
//! only through transparency groups, which a printer flattens to a raster.
//! That images a layer exactly wherever nothing was drawn under it: a clear
//! also covers what earlier layers of the same plot painted there.
//!
//! Apertures and blocks become form XObjects that flashes and instances
//! draw by name, so repeated geometry stays repeated: an array plots its
//! board once. The plot is itself a form in millimetres, y up, for its
//! caller to place on a page at whatever scale the page draws it.

use std::collections::{BTreeMap, HashMap};
use std::f64::consts::{FRAC_PI_2, SQRT_2};
use std::fmt::Write as _;
use std::io::Write as _;

use pdf_writer::{Filter, Finish, Name, Pdf, Rect, Ref};

use crate::dialects::artwork::{self, Geometry};
use crate::geom::path::PathCmd;
use crate::geom::{
    AccuracyError, Affine2, BBox, EllipticalArc, FillRule, GeometryAccuracy, LineCap, Point,
    Polarity, Segment, StrokeStyle,
};
use crate::render::{Drawn, LayerStyle, Num, PlacementScales, RenderOptions};

/// The colour clear paint draws in.
const PAPER: u32 = 0xffffff;
/// Coordinates are written to four decimals, so every emitted point may sit
/// this far from its source along each axis.
const COORDINATE_GRID_MM: f64 = 1e-4;
/// How far a cubic may leave the arc it draws, at the largest scale the arc
/// is placed at.
const ARC_ERROR_MM: f64 = 1e-4;
/// Room the form of shared geometry keeps around the bounds of what it
/// draws. The plot itself is clipped to exactly what it shows.
const FORM_MARGIN_MM: f64 = 1.0;

/// Plot artwork layers into `pdf` as one form XObject, taking object ids
/// from `alloc`.
///
/// The form shows `options.viewport`, or the layers' bounds padded, in the
/// artwork's millimetres, y up, and is clipped to it. A layer's opacity is
/// how strongly its ink shows against the paper.
pub fn artwork_pdf_form<LayerMeta, ObjectMeta>(
    pdf: &mut Pdf,
    alloc: &mut Ref,
    doc: &artwork::Document<LayerMeta, ObjectMeta>,
    options: &RenderOptions,
) -> Result<Ref, AccuracyError> {
    let layers = crate::render::layer_indices(doc.layers.len(), options.layers.as_deref());
    let bbox = options.viewport_over(layers.iter().map(|&index| doc.layers[index].bbox));
    let extent = layers
        .iter()
        .map(|&index| doc.layers[index].bbox)
        .fold(BBox::empty(), BBox::union);
    let mut plot = Plot {
        doc,
        pdf,
        alloc,
        accuracy: options.accuracy,
        extent,
        scales: PlacementScales::of(doc, &layers),
        apertures: vec![None; doc.apertures.len()],
        blocks: HashMap::new(),
    };
    plot.write_apertures()?;

    let mut stream = Stream::default();
    let frame = Frame {
        accuracy: options.accuracy,
        scale: 1.0,
    };
    for &index in &layers {
        let layer = &doc.layers[index];
        let ink = ink(options.style(index, layer.role));
        let painted = artwork::paint_ordered(layer, layer.objects.slice(&doc.objects));
        // A clear removes what its layer painted before it, so with nothing
        // painted yet there is nothing to remove.
        let first_dark = painted
            .iter()
            .position(|(polarity, _)| *polarity == Polarity::Dark)
            .unwrap_or(painted.len());
        for &(polarity, object) in &painted[first_dark..] {
            // A final cutout images clear whatever its own polarity says.
            let context = polarity.compose(object.polarity);
            plot.object(&mut stream, object, context, ink, frame)?;
        }
    }
    Ok(plot.form(stream, bbox, 0.0))
}

/// Deflate `data` as a PDF `FlateDecode` stream.
pub fn deflate(data: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder
        .write_all(data)
        .and_then(|()| encoder.finish())
        .expect("writing to a vector cannot fail")
}

/// A layer's ink: its colour as strong against the paper as its opacity.
fn ink(LayerStyle { color, opacity }: LayerStyle) -> u32 {
    let opacity = opacity.clamp(0.0, 1.0);
    let [_, red, green, blue] = color.to_be_bytes();
    let [_, paper_red, paper_green, paper_blue] = PAPER.to_be_bytes();
    let mix = |ink: u8, paper: u8| {
        (f64::from(ink) * opacity + f64::from(paper) * (1.0 - opacity)).round() as u8
    };
    u32::from_be_bytes([
        0,
        mix(red, paper_red),
        mix(green, paper_green),
        mix(blue, paper_blue),
    ])
}

/// The frame geometry is written in: the budget it has there, and the
/// largest scale anything places it at.
#[derive(Clone, Copy)]
struct Frame {
    accuracy: GeometryAccuracy,
    scale: f64,
}

impl Frame {
    /// Whether a contour may be drawn natively: the approximation it already
    /// carries, coordinate rounding and its arcs' cubics count against the
    /// budget.
    fn check(self, uncertainty_mm: f64) -> Result<(), AccuracyError> {
        self.accuracy
            .check(uncertainty_mm + COORDINATE_GRID_MM / SQRT_2 + ARC_ERROR_MM / self.scale)
    }
}

struct Plot<'a, LayerMeta, ObjectMeta> {
    doc: &'a artwork::Document<LayerMeta, ObjectMeta>,
    pdf: &'a mut Pdf,
    alloc: &'a mut Ref,
    accuracy: GeometryAccuracy,
    extent: BBox,
    scales: PlacementScales,
    /// The form of every aperture something places.
    apertures: Vec<Option<Ref>>,
    /// Block forms by block, ink and the polarity they are placed under. A
    /// form carries its colours, so a block drawn in two inks is two forms.
    blocks: HashMap<(u32, u32, Polarity), Ref>,
}

impl<LayerMeta, ObjectMeta> Plot<'_, LayerMeta, ObjectMeta> {
    /// Shared geometry is written once in its own frame, so its budget is
    /// what the largest placement leaves of the document's.
    fn local(&self, scale: f64) -> Result<Frame, AccuracyError> {
        Ok(Frame {
            accuracy: crate::render::local_accuracy(self.accuracy, self.extent, scale)?,
            scale,
        })
    }

    /// An aperture is a shape without a colour: it paints in whatever ink
    /// the flash that draws it has set.
    fn write_apertures(&mut self) -> Result<(), AccuracyError> {
        for (index, aperture) in self.doc.apertures.iter().enumerate() {
            let Some(scale) = self.scales.apertures[index] else {
                continue;
            };
            let frame = self.local(scale)?;
            let mut stream = Stream::default();
            for contour in aperture.contours() {
                frame.check(contour.uncertainty_mm)?;
                stream.contour(contour.cmds.iter().copied(), frame.scale);
            }
            stream.fill(aperture.fill_rule());
            self.apertures[index] = Some(self.form(stream, aperture.bbox(), FORM_MARGIN_MM));
        }
        Ok(())
    }

    /// The form of `block` as it draws in `ink` under `polarity`.
    fn block(&mut self, block: u32, ink: u32, polarity: Polarity) -> Result<Ref, AccuracyError> {
        if let Some(&id) = self.blocks.get(&(block, ink, polarity)) {
            return Ok(id);
        }
        let doc = self.doc;
        let scale = self.scales.blocks[block as usize].unwrap_or(1.0);
        let frame = self.local(scale)?;
        let mut stream = Stream::default();
        for object in &doc.blocks[block as usize].objects {
            self.object(&mut stream, object, polarity, ink, frame)?;
        }
        let id = self.form(stream, doc.blocks[block as usize].bbox, FORM_MARGIN_MM);
        self.blocks.insert((block, ink, polarity), id);
        Ok(id)
    }

    fn object(
        &mut self,
        stream: &mut Stream,
        object: &artwork::Object<ObjectMeta>,
        context: Polarity,
        ink: u32,
        frame: Frame,
    ) -> Result<(), AccuracyError> {
        let doc = self.doc;
        let polarity = context.compose(object.polarity);
        let color = match polarity {
            Polarity::Dark => ink,
            Polarity::Clear => PAPER,
        };
        match object.geometry {
            Geometry::Flash {
                aperture,
                transform,
            } => {
                if let Some(Some(id)) = self.apertures.get(aperture as usize) {
                    stream.ink(color);
                    stream.place('A', *id, transform);
                }
            }
            Geometry::Instance { block, transform } => {
                if (block as usize) < doc.blocks.len() {
                    let id = self.block(block, ink, polarity)?;
                    stream.place('B', id, transform);
                }
            }
            Geometry::GridInstance {
                block,
                transform,
                repeat,
            } => {
                if (block as usize) < doc.blocks.len() {
                    let id = self.block(block, ink, polarity)?;
                    for offset in repeat.offsets() {
                        stream.place('B', id, Affine2::translation(offset).concat(transform));
                    }
                }
            }
            Geometry::Region { path } => {
                let path = doc.arena.path(path);
                stream.ink(color);
                for contour in doc.arena.contours(path.contours) {
                    frame.check(contour.uncertainty_mm)?;
                    stream.contour(doc.arena.cmds(*contour).iter().copied(), frame.scale);
                }
                stream.fill(
                    path.fill_rule()
                        .expect("region geometry carries a fill paint"),
                );
            }
            Geometry::Stroke { path } => {
                let path = doc.arena.path(path);
                let stroke = path
                    .stroke()
                    .expect("stroke geometry carries a stroke paint");
                // A pen with no width paints nothing; PDF would draw a
                // zero-width stroke as the thinnest line a device has.
                if stroke.width <= 0.0 {
                    return Ok(());
                }
                stream.ink(color);
                // A pen wider than the arc it follows folds its inner edge
                // over, and viewers disagree on what that paints: some draw
                // a dot as a ring. Such a stroke images through its outline.
                let folds = doc.arena.contours(path.contours).iter().any(|contour| {
                    crate::geom::path::segments(doc.arena.cmds(*contour)).any(|segment| {
                        matches!(segment, Segment::Arc(arc) if arc.radius() < stroke.width / 2.0)
                    })
                });
                if stroke.is_solid() && !folds {
                    for contour in doc.arena.contours(path.contours) {
                        frame.check(contour.uncertainty_mm)?;
                        stream.contour(doc.arena.cmds(*contour).iter().copied(), frame.scale);
                    }
                    stream.stroke(stroke);
                } else {
                    // PDF dashes know nothing of IPC line patterns, so a
                    // patterned stroke images through the same expansion
                    // the mask compositor uses, as does a pen that folds.
                    let dashes = crate::geom::path::stroke_to_fill(
                        &doc.arena.path_contours(path),
                        stroke,
                        frame.accuracy,
                    )?
                    .unwrap_or_default();
                    for dash in &dashes {
                        stream.contour(dash.cmds.iter().copied(), frame.scale);
                    }
                    stream.fill(FillRule::NonZero);
                }
            }
        }
        Ok(())
    }

    /// Write `stream` as a form clipped to `bbox` and a margin around it.
    fn form(&mut self, stream: Stream, bbox: BBox, margin: f64) -> Ref {
        let id = self.alloc.bump();
        let data = deflate(stream.ops.as_bytes());
        let mut form = self.pdf.form_xobject(id, &data);
        form.filter(Filter::FlateDecode);
        form.bbox(rect(bbox.expand(margin)));
        let mut resources = form.resources();
        if !stream.forms.is_empty() {
            let mut forms = resources.x_objects();
            for (name, id) in &stream.forms {
                forms.pair(Name(name.as_bytes()), *id);
            }
        }
        resources.finish();
        form.finish();
        id
    }
}

fn rect(bbox: BBox) -> Rect {
    if bbox.is_empty() {
        return Rect::new(0.0, 0.0, 0.0, 0.0);
    }
    Rect::new(
        bbox.min.x as f32,
        bbox.min.y as f32,
        bbox.max.x as f32,
        bbox.max.y as f32,
    )
}

/// One content stream as it is written: its operators, the forms it draws
/// by name, and the graphics state it has set so far.
#[derive(Default)]
struct Stream {
    ops: String,
    forms: BTreeMap<String, Ref>,
    color: Option<u32>,
    stroke: Option<(u64, LineCap)>,
}

fn coordinate(value: f64) -> Num {
    Num { value, decimals: 4 }
}

impl Stream {
    /// Paint what follows, fills and strokes alike, in `color`.
    fn ink(&mut self, color: u32) {
        if self.color == Some(color) {
            return;
        }
        self.color = Some(color);
        let [_, red, green, blue] = color.to_be_bytes().map(|channel| Num {
            value: f64::from(channel) / 255.0,
            decimals: 3,
        });
        writeln!(self.ops, "{red} {green} {blue} rg {red} {green} {blue} RG").unwrap();
    }

    /// Draw form `id` under `transform`.
    fn place(&mut self, kind: char, id: Ref, transform: Affine2) {
        let name = format!("{kind}{}", id.get());
        let [a, b, c, d] = crate::render::linear(transform);
        let (x, y) = (coordinate(transform.m02), coordinate(transform.m12));
        writeln!(self.ops, "q {a} {b} {c} {d} {x} {y} cm /{name} Do Q").unwrap();
        self.forms.insert(name, id);
    }

    fn point(&mut self, point: Point) {
        write!(self.ops, "{} {} ", coordinate(point.x), coordinate(point.y)).unwrap();
    }

    fn contour(&mut self, cmds: impl IntoIterator<Item = PathCmd>, scale: f64) {
        for drawn in crate::render::drawn(cmds) {
            match drawn {
                Drawn::Move(to) => {
                    self.point(to);
                    self.ops.push_str("m\n");
                }
                Drawn::Line(to) => {
                    self.point(to);
                    self.ops.push_str("l\n");
                }
                Drawn::Arc(arc) => self.arc(arc, scale),
                Drawn::Close => self.ops.push_str("h\n"),
            }
        }
    }

    /// An elliptical arc as cubics, each short enough to stay within
    /// [`ARC_ERROR_MM`] of it at `scale`.
    ///
    /// The cubic whose handles run `4/3·tan(δ/4)` along the end tangents
    /// leaves a unit arc of sweep `δ ≤ π/2` by at most `δ⁶/55000`, and an
    /// affine image scales that by at most the longer semi-axis.
    fn arc(&mut self, arc: EllipticalArc, scale: f64) {
        let sweep = arc.signed_sweep_radians();
        let radius = arc.max_scale() * scale;
        let longest = (55_000.0 * ARC_ERROR_MM / radius)
            .powf(1.0 / 6.0)
            .min(FRAC_PI_2);
        let pieces = (sweep.abs() / longest).ceil().max(1.0);
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
            self.point(from + tangent(from_angle) * handle);
            self.point(to - tangent(to_angle) * handle);
            self.point(to);
            self.ops.push_str("c\n");
            from = to;
        }
    }

    fn fill(&mut self, rule: FillRule) {
        self.ops.push_str(match rule {
            FillRule::NonZero => "f\n",
            FillRule::EvenOdd => "f*\n",
        });
    }

    fn stroke(&mut self, stroke: StrokeStyle) {
        let state = (stroke.width.to_bits(), stroke.cap);
        if self.stroke != Some(state) {
            self.stroke = Some(state);
            let cap = match stroke.cap {
                LineCap::Butt => 0,
                LineCap::Round => 1,
                LineCap::Square => 2,
            };
            // A form draws in the state of the page that places it, so the
            // whole pen is set: a page's dashes must not reach a plot.
            writeln!(
                self.ops,
                "{} w {cap} J 1 j [] 0 d",
                coordinate(stroke.width)
            )
            .unwrap();
        }
        self.ops.push_str("S\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialects::{LayerRole, Side};
    use crate::geom::Paint;
    use crate::render::svg::tests::{copper_artwork, square};
    use std::io::Read as _;

    /// Every stream of a PDF, inflated, in the order they were written.
    fn streams(pdf: &[u8]) -> Vec<String> {
        let mut streams = Vec::new();
        let mut rest = pdf;
        while let Some(start) = find(rest, b"stream\n") {
            let body = &rest[start + 7..];
            let end = find(body, b"\nendstream").unwrap();
            let mut text = String::new();
            flate2::read::ZlibDecoder::new(&body[..end])
                .read_to_string(&mut text)
                .unwrap();
            streams.push(text);
            rest = &body[end + b"\nendstream".len()..];
        }
        streams
    }

    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    /// Plot `doc`; returns the PDF's objects and its streams.
    fn plot(doc: &artwork::Document<(), ()>, options: &RenderOptions) -> (String, Vec<String>) {
        let mut pdf = Pdf::new();
        artwork_pdf_form(&mut pdf, &mut Ref::new(1), doc, options).unwrap();
        let pdf = pdf.finish();
        (String::from_utf8_lossy(&pdf).into_owned(), streams(&pdf))
    }

    #[test]
    fn a_block_is_written_once_and_drawn_at_every_placement() {
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

        let (pdf, streams) = plot(&doc, &RenderOptions::default());

        assert_eq!(streams.len(), 2, "the block and the plot");
        assert_eq!(streams[0].matches("0 0 m\n1 0 l").count(), 1);
        assert_eq!(streams[1].matches(" Do Q").count(), 6);
        assert!(streams[1].contains("q 1 0 0 1 10 4 cm /B1 Do Q"));
        // The block keeps a margin; the plot is clipped to what it shows.
        assert!(pdf.contains("/BBox [-1 -1 2 2]"), "{pdf}");
        assert!(pdf.contains("/BBox [-1 -1 12 6]"), "{pdf}");
    }

    #[test]
    fn clear_paint_draws_in_the_paper_colour_after_the_first_dark() {
        let mut doc = copper_artwork();
        let fill = Paint::Fill {
            rule: FillRule::NonZero,
        };
        for (size, polarity) in [
            (1.0, Polarity::Clear),
            (10.0, Polarity::Dark),
            (2.0, Polarity::Clear),
        ] {
            let path = doc.push_path(fill, vec![square(size)]);
            doc.push_object(0, artwork::Object::new(polarity, Geometry::Region { path }));
        }
        artwork::normalize_bounds(&mut doc);
        let style = LayerStyle {
            color: 0x000000,
            opacity: 0.6,
        };

        let (_, streams) = plot(&doc, &RenderOptions::default().with_styles([style]));

        // The leading clear has nothing to remove; ink at 60 % is a grey.
        assert_eq!(
            streams[0],
            "0.4 0.4 0.4 rg 0.4 0.4 0.4 RG\n0 0 m\n10 0 l\n10 10 l\n0 10 l\nh\nf\n\
             1 1 1 rg 1 1 1 RG\n0 0 m\n2 0 l\n2 2 l\n0 2 l\nh\nf\n"
        );
    }

    #[test]
    fn a_block_placed_clear_is_its_own_form() {
        let mut doc = copper_artwork();
        let block = doc.push_block();
        let aperture = doc.push_aperture(artwork::Aperture::circle(1.0));
        doc.push_block_object(
            block,
            artwork::Object::new(
                Polarity::Dark,
                Geometry::Flash {
                    aperture,
                    transform: Affine2::IDENTITY,
                },
            ),
        );
        let pour = doc.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            vec![square(10.0)],
        );
        doc.push_object(
            0,
            artwork::Object::new(Polarity::Dark, Geometry::Region { path: pour }),
        );
        for polarity in [Polarity::Dark, Polarity::Clear] {
            let transform = Affine2::translation(Point::new(5.0, 5.0));
            doc.push_object(
                0,
                artwork::Object::new(polarity, Geometry::Instance { block, transform }),
            );
        }
        artwork::normalize_bounds(&mut doc);
        let style = LayerStyle {
            color: 0xff0000,
            opacity: 1.0,
        };

        let (_, streams) = plot(&doc, &RenderOptions::default().with_styles([style]));

        // The aperture, the block in ink, the block in paper, the plot.
        assert_eq!(streams.len(), 4);
        assert!(!streams[0].contains("rg"), "an aperture has no colour");
        assert!(streams[1].starts_with("1 0 0 rg 1 0 0 RG\nq 1 0 0 1 0 0 cm /A1 Do Q"));
        assert!(streams[2].starts_with("1 1 1 rg 1 1 1 RG\nq 1 0 0 1 0 0 cm /A1 Do Q"));
    }

    #[test]
    fn arcs_are_drawn_as_cubics_within_the_arc_budget() {
        let mut stream = Stream::default();
        let circle = crate::geom::shapes::circle(200.0).unwrap();
        stream.contour(circle.cmds.iter().copied(), 1.0);
        let cubics = stream
            .ops
            .lines()
            .filter(|line| line.ends_with(" c"))
            .map(|line| {
                let numbers = line
                    .split(' ')
                    .filter_map(|number| number.parse::<f64>().ok())
                    .collect::<Vec<_>>();
                [0, 2, 4].map(|at| Point::new(numbers[at], numbers[at + 1]))
            })
            .collect::<Vec<_>>();
        assert!(
            cubics.len() > 4,
            "a large arc takes more than quarter turns"
        );

        // The midpoint of every cubic lies on the circle it draws.
        let mut from = Point::new(100.0, 0.0);
        for [c1, c2, to] in cubics {
            let middle = (from + (c1 + c2) * 3.0 + to) * 0.125;
            let radius = (middle.x * middle.x + middle.y * middle.y).sqrt();
            assert!(
                (radius - 100.0).abs() < ARC_ERROR_MM + COORDINATE_GRID_MM,
                "{radius}"
            );
            from = to;
        }
    }

    #[test]
    fn patterned_strokes_plot_as_filled_dashes() {
        let mut doc = copper_artwork();
        let path = doc.push_path(
            Paint::Stroke(StrokeStyle {
                pattern: crate::geom::LinePattern::Dashed,
                ..StrokeStyle::round(0.2)
            }),
            vec![crate::geom::path::ContourBuf::new(vec![
                PathCmd::move_to(Point::new(0.0, 0.0)),
                PathCmd::line_to(Point::new(20.0, 0.0)),
            ])],
        );
        doc.push_object(
            0,
            artwork::Object::new(Polarity::Dark, Geometry::Stroke { path }),
        );
        artwork::normalize_bounds(&mut doc);

        let (_, streams) = plot(&doc, &RenderOptions::default());

        assert!(!streams[0].contains(" w "), "{}", streams[0]);
        assert!(streams[0].matches(" m\n").count() > 1, "{}", streams[0]);
        assert!(streams[0].ends_with("f\n"));
    }
}
