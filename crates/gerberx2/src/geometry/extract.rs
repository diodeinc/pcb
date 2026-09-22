//! Lower parsed Gerber into a pcb-ir artwork document.
//!
//! Flashes stay flashes, of a standard aperture or of a macro composed once
//! into a contour aperture; block apertures stay block instances and
//! step-repeats stay grids of one block. Round trips so keep both pad
//! identity and reusable hierarchy, and a panel costs one board. Only draws
//! through a shaped aperture are flattened: pcb-ir has no native equivalent.

use pcb_ir::geom::{AccuracyError, GeometryAccuracy, Resolution};
use std::collections::{HashMap, HashSet};

use crate::GerberX2;
use crate::types as gerber;
use pcb_ir::dialects::artwork::{
    self, Aperture, ApertureShape, Document, Geometry, GridRepeat, Layer, Object,
};
use pcb_ir::geom::path::{ContourBuf, PathCmd};
use pcb_ir::geom::region::{self, PaintComposer};
use pcb_ir::geom::{Affine2, Arc, BBox, FillRule, Paint, Point, Polarity, Span, StrokeStyle};

pub type GerberArtworkDocument = Document<Vec<String>, GerberObjectMeta>;

/// The X2 attribute sets an extracted object was imaged under, in
/// [`GerberX2::attributes`].
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct GerberObjectMeta {
    pub aperture_attributes: Span,
    pub object_attributes: Span,
}

pub fn extract_document(
    gerber: &GerberX2,
    accuracy: GeometryAccuracy,
) -> std::result::Result<GerberArtworkDocument, AccuracyError> {
    let file_function =
        crate::from_artwork::file_attribute_fields(gerber, ".FileFunction").unwrap_or_default();
    let mut doc = Document::new();
    let layer = doc.push_layer(Layer {
        name: file_function.join(", "),
        role: super::layer_role(&file_function),
        side: super::layer_side(&file_function),
        objects: Span::EMPTY,
        bbox: BBox::empty(),
        meta: file_function,
    });
    let mut tables = Tables {
        definitions: gerber
            .aperture_definitions()
            .iter()
            .map(|aperture| (aperture.code, aperture))
            .collect(),
        flashes: flash_apertures(gerber, &mut doc, accuracy)?,
        blocks: HashMap::new(),
        accuracy,
    };
    for definition in gerber.aperture_definitions() {
        let gerber::ApertureTemplate::Block { objects } = &definition.template else {
            continue;
        };
        let block = doc.push_block();
        extract_objects(&mut doc, ArtworkTarget::Block(block), objects, &tables)?;
        tables.blocks.insert(definition.code, block);
    }
    // The stream lands on the layer in order, each step-repeated run as one
    // block on a grid.
    let target = ArtworkTarget::Layer(layer);
    let mut next = 0;
    for step in gerber.step_repeats() {
        let run = step.objects.range();
        extract_objects(
            &mut doc,
            target,
            &gerber.objects()[next..run.start],
            &tables,
        )?;
        let block = doc.push_block();
        extract_objects(
            &mut doc,
            ArtworkTarget::Block(block),
            &gerber.objects()[run.clone()],
            &tables,
        )?;
        target.push(
            &mut doc,
            Object {
                polarity: Polarity::Dark,
                order: Default::default(),
                geometry: Geometry::GridInstance {
                    block,
                    transform: Affine2::IDENTITY,
                    repeat: grid_repeat(step.repeat),
                },
                bbox: BBox::empty(),
                meta: GerberObjectMeta::default(),
            },
        );
        next = run.end;
    }
    extract_objects(&mut doc, target, &gerber.objects()[next..], &tables)?;

    artwork::normalize_bounds(&mut doc);
    Ok(doc)
}

/// Gerber images a step-repeat column by column, so its Y axis is the
/// grid's fast one; the order only shows where occurrences overlap.
fn grid_repeat(repeat: gerber::StepRepeat) -> GridRepeat {
    GridRepeat {
        x_count: repeat.y_repeats as u32,
        x_step: Point::new(0.0, repeat.y_step),
        y_count: repeat.x_repeats as u32,
        y_step: Point::new(repeat.x_step, 0.0),
    }
}

/// Per-file lookups shared by every extracted object.
struct Tables<'a> {
    definitions: HashMap<i32, &'a gerber::ApertureDefinition>,
    /// The artwork aperture of every flashed standard or macro aperture.
    flashes: HashMap<i32, u32>,
    /// The artwork block of every block aperture defined so far.
    blocks: HashMap<i32, u32>,
    accuracy: GeometryAccuracy,
}

/// Every object of the file, inside block apertures or not.
fn all_objects(gerber: &GerberX2) -> impl Iterator<Item = &gerber::GraphicalObject> {
    gerber
        .aperture_definitions()
        .iter()
        .filter_map(|definition| match &definition.template {
            gerber::ApertureTemplate::Block { objects } => Some(objects),
            _ => None,
        })
        .flatten()
        .chain(gerber.objects())
}

/// Define the artwork aperture of every standard or macro aperture the file
/// flashes, so each macro is composed once however often it is flashed.
fn flash_apertures(
    gerber: &GerberX2,
    doc: &mut GerberArtworkDocument,
    accuracy: GeometryAccuracy,
) -> std::result::Result<HashMap<i32, u32>, AccuracyError> {
    let flashed = all_objects(gerber)
        .filter_map(|object| match object.kind {
            gerber::ObjectKind::Flash { aperture, .. } => Some(aperture),
            _ => None,
        })
        .collect::<HashSet<_>>();
    // A macro is composed in aperture space, so its budget shrinks by the
    // largest scale a flash can image it under: `%LS`, compounded once
    // through a block aperture.
    let max_scale = all_objects(gerber)
        .map(|object| object.scaling.abs())
        .fold(1.0, f64::max);
    let local = GeometryAccuracy::new(accuracy.max_error_mm() / (max_scale * max_scale))?;
    let mut apertures = HashMap::new();
    for definition in gerber
        .aperture_definitions()
        .iter()
        .filter(|definition| flashed.contains(&definition.code))
    {
        let aperture = match (
            standard_aperture(&definition.template),
            &definition.geometry,
        ) {
            (Some(standard), _) => Some(standard),
            (None, Some(geometry)) => macro_aperture(geometry, local)?,
            (None, None) => None,
        };
        if let Some(aperture) = aperture {
            apertures.insert(definition.code, doc.push_aperture(aperture));
        }
    }
    Ok(apertures)
}

/// Compose a macro's primitives into one contour aperture. An exposure-off
/// primitive erases only what the macro imaged before it, never the layer
/// under a flash, so the composition is the aperture's whole image.
fn macro_aperture(
    geometry: &gerber::ApertureGeometry,
    accuracy: GeometryAccuracy,
) -> std::result::Result<Option<Aperture>, AccuracyError> {
    let paths = aperture_paths(geometry, Affine2::IDENTITY);
    let contours = match paths.as_slice() {
        [path] if path.polarity == Polarity::Dark => path.contours.clone(),
        _ => compose_paths(paths, accuracy)?.to_contours(),
    };
    let uncertainty_mm = contours
        .iter()
        .map(|contour| contour.uncertainty_mm)
        .fold(0.0, f64::max);
    let cmds = contours
        .into_iter()
        .flat_map(|contour| contour.cmds)
        .collect::<Vec<_>>();
    Ok((!cmds.is_empty()).then(|| {
        Aperture::solid(ApertureShape::Contour {
            outline: ContourBuf::new(cmds).with_uncertainty(uncertainty_mm),
            fill_rule: FillRule::NonZero,
        })
    }))
}

#[derive(Debug, Clone, Copy)]
enum ArtworkTarget {
    Layer(u32),
    Block(u32),
}

impl ArtworkTarget {
    fn push(self, doc: &mut GerberArtworkDocument, object: Object<GerberObjectMeta>) {
        match self {
            Self::Layer(layer) => doc.push_object(layer, object),
            Self::Block(block) => doc.push_block_object(block, object),
        };
    }
}

fn extract_objects(
    doc: &mut GerberArtworkDocument,
    target: ArtworkTarget,
    objects: &[gerber::GraphicalObject],
    tables: &Tables<'_>,
) -> std::result::Result<(), AccuracyError> {
    objects
        .iter()
        .try_for_each(|object| extract_object(doc, target, object, tables))
}

fn extract_object(
    doc: &mut GerberArtworkDocument,
    target: ArtworkTarget,
    object: &gerber::GraphicalObject,
    tables: &Tables<'_>,
) -> std::result::Result<(), AccuracyError> {
    let accuracy = tables.accuracy;
    // A draw is its aperture, its ends and, along an arc, its circle.
    let (aperture, start, end, arc) = match &object.kind {
        gerber::ObjectKind::Flash { at, aperture } => {
            if !tables.definitions.contains_key(aperture) {
                doc.warn(format!("flash references undefined aperture D{aperture}"));
                return Ok(());
            }
            let transform = object_transform(object, point(*at));
            let geometry = if let Some(&block) = tables.blocks.get(aperture) {
                Geometry::Instance { block, transform }
            } else if let Some(&aperture) = tables.flashes.get(aperture) {
                Geometry::Flash {
                    aperture,
                    transform,
                }
            } else {
                // The aperture images nothing.
                return Ok(());
            };
            target.push(
                doc,
                Object {
                    polarity: object.polarity,
                    order: Default::default(),
                    geometry,
                    bbox: BBox::empty(),
                    meta: meta_from_object(object),
                },
            );
            return Ok(());
        }
        gerber::ObjectKind::Region { contours } => {
            return push_flattened_paths(doc, target, object, region_paths(contours), accuracy);
        }
        gerber::ObjectKind::Draw {
            start,
            end,
            aperture,
        } => (*aperture, point(*start), point(*end), None),
        gerber::ObjectKind::Arc {
            start,
            end,
            center_offset,
            clockwise,
            aperture,
        } => {
            let (start, end) = (point(*start), point(*end));
            let center = Point::new(start.x + center_offset.x, start.y + center_offset.y);
            let arc = Arc::new(start, end, center, *clockwise);
            (*aperture, start, end, Some(arc))
        }
    };
    let definition = tables.definitions.get(&aperture);
    let paths = if let Some(gerber::ApertureTemplate::Circle { diameter, .. }) =
        definition.map(|definition| &definition.template)
    {
        let to = arc.map_or(PathCmd::line_to(end), |arc| {
            PathCmd::arc_to(end, arc.center, arc.clockwise)
        });
        vec![ExtractedPath {
            polarity: Polarity::Dark,
            paint: Paint::Stroke(StrokeStyle::round(diameter * object.scaling.abs())),
            contours: vec![ContourBuf::new(vec![PathCmd::move_to(start), to])],
        }]
    } else if let Some(geometry) = definition.and_then(|definition| definition.geometry.as_ref()) {
        let (points, path_error) = match arc {
            Some(arc) => arc_points(arc, accuracy)?,
            None => (vec![start, end], 0.0),
        };
        swept_aperture(&points, path_error, object, geometry, accuracy)?
    } else {
        let kind = if arc.is_some() { "arc" } else { "draw" };
        doc.warn(format!(
            "D{aperture} {kind} aperture has no lowered geometry"
        ));
        return Ok(());
    };
    push_flattened_paths(doc, target, object, paths, accuracy)
}

/// Convert a standard aperture template into an artwork aperture. Macro and
/// block templates return `None`; blocks are handled as instances and macros
/// use their parsed fallback geometry. So does a standard template whose hole
/// reaches outside its shape: its image is the shape less the hole, which
/// only composing the two gives.
fn standard_aperture(template: &gerber::ApertureTemplate) -> Option<Aperture> {
    let (shape, hole_diameter) = match *template {
        gerber::ApertureTemplate::Circle {
            diameter,
            hole_diameter,
        } => (ApertureShape::Circle { diameter }, hole_diameter),
        gerber::ApertureTemplate::Rectangle {
            width,
            height,
            hole_diameter,
        } => (ApertureShape::Rectangle { width, height }, hole_diameter),
        gerber::ApertureTemplate::Obround {
            width,
            height,
            hole_diameter,
        } => (ApertureShape::Obround { width, height }, hole_diameter),
        gerber::ApertureTemplate::Polygon {
            outer_diameter,
            vertices,
            rotation_degrees,
            hole_diameter,
        } => (
            ApertureShape::Polygon {
                diameter: outer_diameter,
                vertices: vertices as u32,
                rotation_degrees: rotation_degrees.unwrap_or(0.0),
            },
            hole_diameter,
        ),
        gerber::ApertureTemplate::Macro { .. } | gerber::ApertureTemplate::Block { .. } => {
            return None;
        }
    };
    let aperture = Aperture {
        shape,
        hole_diameter: hole_diameter.unwrap_or(0.0),
    };
    aperture.hole_fits().then_some(aperture)
}

fn meta_from_object(object: &gerber::GraphicalObject) -> GerberObjectMeta {
    GerberObjectMeta {
        aperture_attributes: object.aperture_attributes,
        object_attributes: object.object_attributes,
    }
}

/// One flattened piece of an object: per-piece polarity (macro geometry can
/// carry clear parts) plus its paint and contours.
#[derive(Debug, Clone)]
struct ExtractedPath {
    polarity: Polarity,
    paint: Paint,
    contours: Vec<ContourBuf>,
}

fn push_flattened_paths(
    doc: &mut GerberArtworkDocument,
    target: ArtworkTarget,
    object: &gerber::GraphicalObject,
    mut paths: Vec<ExtractedPath>,
    accuracy: GeometryAccuracy,
) -> std::result::Result<(), AccuracyError> {
    // A lone dark piece keeps its own paint; anything else is composed.
    let (paint, contours) = match paths.as_slice() {
        [] => return Ok(()),
        [path] if path.polarity == Polarity::Dark => {
            let path = paths.pop().unwrap();
            (path.paint, path.contours)
        }
        _ => {
            let contours = compose_paths(paths, accuracy)?.to_contours();
            if contours.is_empty() {
                return Ok(());
            }
            let rule = FillRule::NonZero;
            (Paint::Fill { rule }, contours)
        }
    };
    let is_stroked = matches!(paint, Paint::Stroke(_));
    let path = doc.push_path(paint, contours);
    target.push(
        doc,
        Object {
            polarity: object.polarity,
            order: Default::default(),
            geometry: if is_stroked {
                Geometry::Stroke { path }
            } else {
                Geometry::Region { path }
            },
            bbox: doc.path_bbox(path),
            meta: meta_from_object(object),
        },
    );
    Ok(())
}

/// Paint the pieces in order into one non-zero filled image.
fn compose_paths(
    paths: Vec<ExtractedPath>,
    accuracy: GeometryAccuracy,
) -> std::result::Result<region::ContourSet, AccuracyError> {
    let resolution = Resolution::new(0.0, accuracy);
    let mut composer = PaintComposer::new(resolution);
    for extracted in paths {
        composer.push(
            extracted.polarity,
            region::ContourSet::from_contours(
                &extracted.contours,
                extracted.paint.fill_rule().unwrap_or(FillRule::NonZero),
                resolution,
            )?,
        );
    }
    composer.finish()
}

fn aperture_paths(geometry: &gerber::ApertureGeometry, transform: Affine2) -> Vec<ExtractedPath> {
    geometry
        .paths
        .iter()
        .map(|path| ExtractedPath {
            polarity: path.polarity,
            paint: Paint::Fill {
                rule: FillRule::NonZero,
            },
            contours: path
                .contours
                .iter()
                .map(|contour| transform_contour(&contour.commands, transform))
                .collect(),
        })
        .collect()
}

fn transform_contour(commands: &[gerber::PathCommand], transform: Affine2) -> ContourBuf {
    let cmds = commands
        .iter()
        .map(|command| match *command {
            gerber::PathCommand::MoveTo(p) => PathCmd::move_to(point(p)),
            gerber::PathCommand::LineTo(p) => PathCmd::line_to(point(p)),
            gerber::PathCommand::ArcTo {
                end,
                center,
                clockwise,
            } => PathCmd::arc_to(point(end), point(center), clockwise),
            gerber::PathCommand::Close => PathCmd::close(),
        })
        .collect::<Vec<_>>();
    ContourBuf::new(cmds).transformed(transform)
}

fn swept_aperture(
    points: &[Point],
    path_error: f64,
    object: &gerber::GraphicalObject,
    geometry: &gerber::ApertureGeometry,
    accuracy: GeometryAccuracy,
) -> std::result::Result<Vec<ExtractedPath>, AccuracyError> {
    let resolution = Resolution::new(0.0, accuracy);
    let aperture = compose_paths(
        aperture_paths(geometry, object_transform(object, Point::ZERO)),
        accuracy,
    )?;
    let edge_count: usize = aperture.rings.iter().map(Vec::len).sum();
    if points.len().saturating_mul(edge_count) > 1_000_000 {
        return Err(AccuracyError::SubdivisionLimit);
    }
    let mut rings = Vec::new();
    for at in points {
        rings.extend(
            aperture
                .rings
                .iter()
                .map(|ring| ring.iter().map(|[x, y]| [x + at.x, y + at.y]).collect()),
        );
    }
    // Sweep each boundary edge continuously, including the boundaries of holes.
    for pair in points.windows(2) {
        for ring in &aperture.rings {
            for (a, b) in ring
                .iter()
                .zip(ring.iter().cycle().skip(1))
                .take(ring.len())
            {
                let mut quad = vec![
                    [a[0] + pair[0].x, a[1] + pair[0].y],
                    [b[0] + pair[0].x, b[1] + pair[0].y],
                    [b[0] + pair[1].x, b[1] + pair[1].y],
                    [a[0] + pair[1].x, a[1] + pair[1].y],
                ];
                if region::ring_signed_area(&quad) < 0.0 {
                    quad.reverse();
                }
                rings.push(quad);
            }
        }
    }
    let mut swept = region::ContourSet::from_rings(rings, FillRule::NonZero, resolution)?;
    swept.uncertainty_mm += aperture.uncertainty_mm + path_error;
    accuracy.check(swept.uncertainty_mm)?;
    Ok(vec![ExtractedPath {
        polarity: Polarity::Dark,
        paint: Paint::Fill {
            rule: FillRule::NonZero,
        },
        contours: swept.to_contours(),
    }])
}

/// Points along `arc` whose chords stay within a quarter of the budget,
/// and that chord error.
fn arc_points(
    arc: Arc,
    accuracy: GeometryAccuracy,
) -> std::result::Result<(Vec<Point>, f64), AccuracyError> {
    let radius = arc.radius();
    let sweep = arc.sweep_radians();
    let path_error = accuracy.max_error_mm() / 4.0;
    let angle = 4.0 * (path_error / (2.0 * radius)).min(1.0).sqrt().asin();
    let steps = (sweep / angle).ceil().max(1.0);
    if !steps.is_finite() || steps > 1_000_000.0 {
        return Err(AccuracyError::SubdivisionLimit);
    }
    let steps = steps as usize;
    let signed_sweep = if arc.clockwise { -sweep } else { sweep };
    let start_angle = arc.start.angle_from(arc.center);
    let points = (0..=steps)
        .map(|index| arc.point_at(start_angle + signed_sweep * index as f64 / steps as f64))
        .collect();
    Ok((points, path_error))
}

fn object_transform(object: &gerber::GraphicalObject, at: Point) -> Affine2 {
    Affine2::placement(
        at,
        object.rotation_degrees,
        object.mirroring.into(),
        object.scaling,
    )
}

fn region_paths(contours: &[gerber::Contour]) -> Vec<ExtractedPath> {
    contours
        .iter()
        .map(|contour| ExtractedPath {
            polarity: Polarity::Dark,
            paint: Paint::Fill {
                rule: FillRule::EvenOdd,
            },
            contours: vec![region_contour(contour)],
        })
        .collect()
}

fn region_contour(contour: &gerber::Contour) -> ContourBuf {
    let mut cmds = Vec::new();
    if let Some(first) = contour.segments.first() {
        let start = match *first {
            gerber::ContourSegment::Line { start, .. }
            | gerber::ContourSegment::Arc { start, .. } => point(start),
        };
        cmds.push(PathCmd::move_to(start));
    }
    for segment in &contour.segments {
        cmds.push(match *segment {
            gerber::ContourSegment::Line { end, .. } => PathCmd::line_to(point(end)),
            gerber::ContourSegment::Arc {
                start,
                end,
                center_offset,
                clockwise,
            } => {
                let start = point(start);
                PathCmd::arc_to(
                    point(end),
                    Point::new(start.x + center_offset.x, start.y + center_offset.y),
                    clockwise,
                )
            }
        });
    }
    cmds.push(PathCmd::close());
    ContourBuf::new(cmds)
}

fn point(p: gerber::Point) -> Point {
    Point::new(p.x, p.y)
}
