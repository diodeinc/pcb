//! Lower parsed Gerber into a pcb-ir artwork document.
//!
//! Standard-aperture flashes and aperture-block instances are preserved so
//! round trips keep both pad identity and reusable hierarchy. Macro flashes
//! and shaped draws are flattened only where pcb-ir has no native equivalent.

use pcb_ir::geom::{AccuracyError, GeometryAccuracy};
use std::collections::HashMap;

use crate::GerberX2;
use crate::types as gerber;
use pcb_ir::dialects::artwork::{self, Aperture, ApertureShape, Document, Geometry, Layer, Object};
use pcb_ir::geom::path::{ContourBuf, PathCmd};
use pcb_ir::geom::region::{self, PaintComposer};
use pcb_ir::geom::{Affine2, Arc, BBox, FillRule, Paint, Point, Polarity, Span, StrokeStyle};

pub type GerberArtworkDocument = Document<Vec<String>, GerberObjectMeta>;

/// Which Gerber operation produced an object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Flash,
    Draw,
    Arc,
    Region,
}

/// Coarse fabrication classification of a Gerber object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectClass {
    Pad,
    Trace,
    Fill,
    Cutout,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GerberObjectMeta {
    pub kind: SourceKind,
    pub class: ObjectClass,
    pub polarity: Polarity,
    pub aperture: Option<i32>,
    pub object_index: u32,
    pub aperture_attributes: Vec<gerber::Attribute>,
    pub object_attributes: Vec<gerber::Attribute>,
    pub mirroring: gerber::Mirroring,
    pub rotation_degrees: f64,
    pub scaling: f64,
}

pub fn extract_document(
    gerber: &GerberX2,
    accuracy: GeometryAccuracy,
) -> std::result::Result<GerberArtworkDocument, AccuracyError> {
    let file_function = file_function(gerber);
    let mut doc = Document::new();
    let layer = doc.push_layer(Layer {
        name: file_function.join(", "),
        role: super::layer_role(&file_function),
        side: super::layer_side(&file_function),
        objects: Span::EMPTY,
        bbox: BBox::empty(),
        meta: file_function,
    });
    let apertures = gerber
        .aperture_definitions()
        .iter()
        .map(|aperture| (aperture.code, aperture))
        .collect::<HashMap<_, _>>();
    let mut blocks = HashMap::<i32, u32>::new();
    for definition in gerber.aperture_definitions() {
        let gerber::ApertureTemplate::Block { objects } = &definition.template else {
            continue;
        };
        let block = doc.push_block();
        extract_objects(
            &mut doc,
            ArtworkTarget::Block(block),
            objects,
            &apertures,
            &blocks,
            accuracy,
        )?;
        blocks.insert(definition.code, block);
    }
    extract_objects(
        &mut doc,
        ArtworkTarget::Layer(layer),
        gerber.objects(),
        &apertures,
        &blocks,
        accuracy,
    )?;

    artwork::normalize_bounds(&mut doc);
    Ok(doc)
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
    apertures: &HashMap<i32, &gerber::ApertureDefinition>,
    blocks: &HashMap<i32, u32>,
    accuracy: GeometryAccuracy,
) -> std::result::Result<(), AccuracyError> {
    for (object_index, object) in objects.iter().enumerate() {
        extract_object(
            doc,
            target,
            object_index,
            object,
            apertures,
            blocks,
            accuracy,
        )?;
    }
    Ok(())
}

fn extract_object(
    doc: &mut GerberArtworkDocument,
    target: ArtworkTarget,
    object_index: usize,
    object: &gerber::GraphicalObject,
    apertures: &HashMap<i32, &gerber::ApertureDefinition>,
    blocks: &HashMap<i32, u32>,
    accuracy: GeometryAccuracy,
) -> std::result::Result<(), AccuracyError> {
    match &object.kind {
        gerber::ObjectKind::Flash { at, aperture } => {
            let Some(definition) = apertures.get(aperture) else {
                doc.warn(format!("flash references undefined aperture D{aperture}"));
                return Ok(());
            };
            let transform = object_transform(object, point(*at));
            let mut meta = meta_from_object(object, object_index, SourceKind::Flash);
            meta.aperture = Some(*aperture);

            if let Some(&block) = blocks.get(aperture) {
                target.push(
                    doc,
                    Object {
                        polarity: meta.polarity,
                        order: Default::default(),
                        geometry: Geometry::Instance { block, transform },
                        bbox: BBox::empty(),
                        meta,
                    },
                );
            } else if let Some(standard) = standard_aperture(&definition.template) {
                let aperture_id = doc.push_aperture(standard);
                target.push(
                    doc,
                    Object {
                        polarity: meta.polarity,
                        order: Default::default(),
                        geometry: Geometry::Flash {
                            aperture: aperture_id,
                            transform,
                        },
                        bbox: BBox::empty(),
                        meta,
                    },
                );
            } else if let Some(geometry) = &definition.geometry {
                push_flattened_paths(
                    doc,
                    target,
                    meta,
                    aperture_paths(geometry, transform, accuracy)?,
                    accuracy,
                )?;
            } else {
                doc.warn(format!(
                    "flash aperture D{aperture} has no lowered geometry"
                ));
            }
        }
        gerber::ObjectKind::Draw {
            start,
            end,
            aperture,
        } => {
            let mut meta = meta_from_object(object, object_index, SourceKind::Draw);
            meta.aperture = Some(*aperture);
            if let Some(width) = circular_aperture_diameter(apertures, *aperture) {
                push_flattened_paths(
                    doc,
                    target,
                    meta,
                    vec![line_path(
                        point(*start),
                        point(*end),
                        width * object.scaling.abs(),
                    )],
                    accuracy,
                )?;
            } else if let Some(geometry) = aperture_geometry(apertures, *aperture) {
                push_flattened_paths(
                    doc,
                    target,
                    meta,
                    swept_aperture(
                        &[point(*start), point(*end)],
                        0.0,
                        object,
                        geometry,
                        accuracy,
                    )?,
                    accuracy,
                )?;
            } else {
                doc.warn(format!("D{aperture} draw aperture has no lowered geometry"));
            }
        }
        gerber::ObjectKind::Arc {
            start,
            end,
            center_offset,
            clockwise,
            aperture,
        } => {
            let mut meta = meta_from_object(object, object_index, SourceKind::Arc);
            meta.aperture = Some(*aperture);
            let start = point(*start);
            let center = Point::new(start.x + center_offset.x, start.y + center_offset.y);
            if let Some(width) = circular_aperture_diameter(apertures, *aperture) {
                push_flattened_paths(
                    doc,
                    target,
                    meta,
                    vec![arc_path(
                        start,
                        point(*end),
                        center,
                        *clockwise,
                        width * object.scaling.abs(),
                    )],
                    accuracy,
                )?;
            } else if let Some(geometry) = aperture_geometry(apertures, *aperture) {
                push_flattened_paths(
                    doc,
                    target,
                    meta,
                    arc_sweep(
                        start,
                        point(*end),
                        center,
                        *clockwise,
                        object,
                        geometry,
                        accuracy,
                    )?,
                    accuracy,
                )?;
            } else {
                doc.warn(format!("D{aperture} arc aperture has no lowered geometry"));
            }
        }
        gerber::ObjectKind::Region { contours } => {
            let meta = meta_from_object(object, object_index, SourceKind::Region);
            push_flattened_paths(doc, target, meta, region_paths(contours), accuracy)?;
        }
    };
    Ok(())
}

/// Convert a standard aperture template into an artwork aperture. Macro and
/// block templates return `None`; blocks are handled as instances and macros
/// use their parsed fallback geometry.
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
        } => {
            if vertices < 3 {
                return None;
            }
            (
                ApertureShape::Polygon {
                    diameter: outer_diameter,
                    vertices: vertices as u32,
                    rotation_degrees: rotation_degrees.unwrap_or(0.0),
                },
                hole_diameter,
            )
        }
        gerber::ApertureTemplate::Macro { .. } | gerber::ApertureTemplate::Block { .. } => {
            return None;
        }
    };
    Some(Aperture {
        shape,
        hole_diameter: hole_diameter.unwrap_or(0.0),
    })
}

fn aperture_geometry<'a>(
    apertures: &'a HashMap<i32, &gerber::ApertureDefinition>,
    code: i32,
) -> Option<&'a gerber::ApertureGeometry> {
    apertures.get(&code)?.geometry.as_ref()
}

fn file_function(gerber: &GerberX2) -> Vec<String> {
    gerber
        .file_attributes()
        .iter()
        .find(|attr| gerber.resolve(attr.name) == ".FileFunction")
        .map(|attr| {
            attr.fields
                .iter()
                .map(|field| gerber.resolve(*field).to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn meta_from_object(
    object: &gerber::GraphicalObject,
    object_index: usize,
    kind: SourceKind,
) -> GerberObjectMeta {
    GerberObjectMeta {
        kind,
        class: classify(object, kind),
        polarity: object.polarity,
        aperture: None,
        object_index: object_index as u32,
        aperture_attributes: object.aperture_attributes.clone(),
        object_attributes: object.object_attributes.clone(),
        mirroring: object.mirroring,
        rotation_degrees: object.rotation_degrees,
        scaling: object.scaling,
    }
}

fn classify(object: &gerber::GraphicalObject, kind: SourceKind) -> ObjectClass {
    if object.polarity == Polarity::Clear {
        return ObjectClass::Cutout;
    }
    match kind {
        SourceKind::Region => ObjectClass::Fill,
        SourceKind::Draw | SourceKind::Arc => ObjectClass::Trace,
        SourceKind::Flash => ObjectClass::Pad,
    }
}

fn circular_aperture_diameter(
    apertures: &HashMap<i32, &gerber::ApertureDefinition>,
    code: i32,
) -> Option<f64> {
    match apertures.get(&code)?.template {
        gerber::ApertureTemplate::Circle {
            diameter,
            hole_diameter: _,
        } => Some(diameter),
        _ => None,
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
    meta: GerberObjectMeta,
    paths: Vec<ExtractedPath>,
    accuracy: GeometryAccuracy,
) -> std::result::Result<(), AccuracyError> {
    if paths.is_empty() {
        return Ok(());
    }

    if paths.len() == 1 && paths[0].polarity == Polarity::Dark {
        let extracted = paths.into_iter().next().unwrap();
        let is_stroked = matches!(extracted.paint, Paint::Stroke(_));
        let path = doc.push_path(extracted.paint, extracted.contours);
        target.push(
            doc,
            Object {
                polarity: meta.polarity,
                order: Default::default(),
                geometry: if is_stroked {
                    Geometry::Stroke { path }
                } else {
                    Geometry::Region { path }
                },
                bbox: doc.path_bbox(path),
                meta,
            },
        );
        return Ok(());
    }

    let mut composer = PaintComposer::default();
    for extracted in paths {
        composer.push(
            extracted.polarity,
            region::ContourSet::from_contours(
                &extracted.contours,
                extracted.paint.fill_rule().unwrap_or(FillRule::NonZero),
                0.0,
                accuracy,
            )?,
        );
    }
    let contours = composer.finish(0.0).to_contours();
    if contours.is_empty() {
        return Ok(());
    }

    let path = doc.push_path(
        Paint::Fill {
            rule: FillRule::NonZero,
        },
        contours,
    );
    target.push(
        doc,
        Object {
            polarity: meta.polarity,
            order: Default::default(),
            geometry: Geometry::Region { path },
            bbox: doc.path_bbox(path),
            meta,
        },
    );

    Ok(())
}

fn aperture_paths(
    geometry: &gerber::ApertureGeometry,
    transform: Affine2,
    accuracy: GeometryAccuracy,
) -> std::result::Result<Vec<ExtractedPath>, AccuracyError> {
    geometry
        .paths
        .iter()
        .map(|path| {
            Ok(ExtractedPath {
                polarity: path.polarity,
                paint: Paint::Fill {
                    rule: FillRule::NonZero,
                },
                contours: path
                    .contours
                    .iter()
                    .map(|contour| transform_contour(&contour.commands, transform, accuracy))
                    .collect::<std::result::Result<Vec<_>, _>>()?,
            })
        })
        .collect()
}

fn transform_contour(
    commands: &[gerber::PathCommand],
    transform: Affine2,
    accuracy: GeometryAccuracy,
) -> std::result::Result<ContourBuf, AccuracyError> {
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
    ContourBuf::new(cmds).transformed(transform, accuracy)
}

fn line_path(start: Point, end: Point, width: f64) -> ExtractedPath {
    ExtractedPath {
        polarity: Polarity::Dark,
        paint: Paint::Stroke(StrokeStyle::round(width)),
        contours: vec![ContourBuf::new(vec![
            PathCmd::move_to(start),
            PathCmd::line_to(end),
        ])],
    }
}

fn arc_path(start: Point, end: Point, center: Point, clockwise: bool, width: f64) -> ExtractedPath {
    ExtractedPath {
        polarity: Polarity::Dark,
        paint: Paint::Stroke(StrokeStyle::round(width)),
        contours: vec![ContourBuf::new(vec![
            PathCmd::move_to(start),
            PathCmd::arc_to(end, center, clockwise),
        ])],
    }
}

fn swept_aperture(
    points: &[Point],
    path_error: f64,
    object: &gerber::GraphicalObject,
    geometry: &gerber::ApertureGeometry,
    accuracy: GeometryAccuracy,
) -> std::result::Result<Vec<ExtractedPath>, AccuracyError> {
    let mut composer = PaintComposer::default();
    for path in aperture_paths(geometry, object_transform(object, Point::ZERO), accuracy)? {
        composer.push(
            path.polarity,
            region::ContourSet::from_contours(&path.contours, FillRule::NonZero, 0.0, accuracy)?,
        );
    }
    let aperture = composer.finish(0.0);
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
    let mut swept = region::ContourSet::new(rings, FillRule::NonZero, 0.0);
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

fn arc_sweep(
    start: Point,
    end: Point,
    center: Point,
    clockwise: bool,
    object: &gerber::GraphicalObject,
    geometry: &gerber::ApertureGeometry,
    accuracy: GeometryAccuracy,
) -> std::result::Result<Vec<ExtractedPath>, AccuracyError> {
    let arc = Arc::new(start, end, center, clockwise);
    let radius = arc.radius();
    let sweep = arc.sweep_radians();
    let path_error = accuracy.max_error_mm() / 4.0;
    let angle = 4.0 * (path_error / (2.0 * radius)).min(1.0).sqrt().asin();
    let steps = (sweep / angle).ceil().max(1.0);
    if !steps.is_finite() || steps > 1_000_000.0 {
        return Err(AccuracyError::SubdivisionLimit);
    }
    let steps = steps as usize;
    let signed_sweep = if clockwise { -sweep } else { sweep };
    let start_angle = start.angle_from(center);
    let points = (0..=steps)
        .map(|index| arc.point_at(start_angle + signed_sweep * index as f64 / steps as f64))
        .collect::<Vec<_>>();
    swept_aperture(&points, path_error, object, geometry, accuracy)
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
