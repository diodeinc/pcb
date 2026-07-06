//! Lower parsed Gerber into a pcb-ir artwork document.
//!
//! Standard-aperture flashes are preserved as native `Flash` objects with an
//! aperture table so round trips keep pad identity; macro/block flashes and
//! shaped draws are flattened to filled regions.

use std::collections::HashMap;

use crate::GerberX2;
use crate::types as gerber;
use pcb_ir::dialects::artwork::{self, Aperture, ApertureShape, Document, Geometry, Layer, Object};
use pcb_ir::geom::path::{ContourBuf, PathCmd, transform_cmds};
use pcb_ir::geom::region::{self, PaintComposer};
use pcb_ir::geom::{Affine2, Arc, BBox, FillRule, Paint, Point, Polarity, Span, StrokeStyle};

const SWEEP_SAMPLE_MM: f64 = 0.025;

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

pub fn extract_document(gerber: &GerberX2) -> GerberArtworkDocument {
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

    for (object_index, object) in gerber.objects().iter().enumerate() {
        match &object.kind {
            gerber::ObjectKind::Flash { at, aperture } => {
                let Some(definition) = apertures.get(aperture) else {
                    doc.warn(format!("flash references undefined aperture D{aperture}"));
                    continue;
                };
                let transform = object_transform(object, point(*at));
                let mut meta = meta_from_object(object, object_index, SourceKind::Flash);
                meta.aperture = Some(*aperture);

                if let Some(standard) = standard_aperture(&definition.template) {
                    let aperture_id = doc.push_aperture(standard);
                    doc.push_object(
                        layer,
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
                        &mut doc,
                        layer,
                        meta,
                        aperture_paths(geometry, transform),
                    );
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
                if let Some(width) = circular_aperture_diameter(&apertures, *aperture) {
                    push_flattened_paths(
                        &mut doc,
                        layer,
                        meta,
                        vec![line_path(
                            point(*start),
                            point(*end),
                            width * object.scaling.abs(),
                        )],
                    );
                } else if let Some(geometry) = aperture_geometry(&apertures, *aperture) {
                    push_flattened_paths(
                        &mut doc,
                        layer,
                        meta,
                        sampled_line_sweep(point(*start), point(*end), object, geometry),
                    );
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
                if let Some(width) = circular_aperture_diameter(&apertures, *aperture) {
                    push_flattened_paths(
                        &mut doc,
                        layer,
                        meta,
                        vec![arc_path(
                            start,
                            point(*end),
                            center,
                            *clockwise,
                            width * object.scaling.abs(),
                        )],
                    );
                } else if let Some(geometry) = aperture_geometry(&apertures, *aperture) {
                    push_flattened_paths(
                        &mut doc,
                        layer,
                        meta,
                        sampled_arc_sweep(start, point(*end), center, *clockwise, object, geometry),
                    );
                } else {
                    doc.warn(format!("D{aperture} arc aperture has no lowered geometry"));
                }
            }
            gerber::ObjectKind::Region { contours } => {
                let meta = meta_from_object(object, object_index, SourceKind::Region);
                push_flattened_paths(&mut doc, layer, meta, region_paths(contours));
            }
        }
    }

    artwork::normalize_bounds(&mut doc);
    doc
}

/// Convert a standard aperture template into an artwork aperture. Macro and
/// block templates return `None` and are flattened instead.
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
    layer: u32,
    meta: GerberObjectMeta,
    paths: Vec<ExtractedPath>,
) {
    if paths.is_empty() {
        return;
    }

    if paths.len() == 1 && paths[0].polarity == Polarity::Dark {
        let extracted = paths.into_iter().next().unwrap();
        let is_stroked = matches!(extracted.paint, Paint::Stroke(_));
        let path = doc.push_path(extracted.paint, extracted.contours);
        doc.push_object(
            layer,
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
        return;
    }

    let mut composer = PaintComposer::default();
    for extracted in paths {
        let rings = region::simplify_rings(
            region::rings_from_contours(&extracted.contours),
            extracted.paint.fill_rule().unwrap_or(FillRule::NonZero),
        );
        if rings.is_empty() {
            continue;
        }
        composer.push(extracted.polarity, rings);
    }
    let contours = region::rings_to_contours(composer.finish());
    if contours.is_empty() {
        return;
    }

    let path = doc.push_path(
        Paint::Fill {
            rule: FillRule::NonZero,
        },
        contours,
    );
    doc.push_object(
        layer,
        Object {
            polarity: meta.polarity,
            order: Default::default(),
            geometry: Geometry::Region { path },
            bbox: doc.path_bbox(path),
            meta,
        },
    );
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
    transform_cmds(cmds, transform)
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

fn sampled_line_sweep(
    start: Point,
    end: Point,
    object: &gerber::GraphicalObject,
    geometry: &gerber::ApertureGeometry,
) -> Vec<ExtractedPath> {
    let length = start.distance_to(end);
    let steps = sample_steps(length);
    (0..=steps)
        .flat_map(|index| {
            let t = index as f64 / steps.max(1) as f64;
            let at = start + (end - start) * t;
            aperture_paths(geometry, object_transform(object, at))
        })
        .collect()
}

fn sampled_arc_sweep(
    start: Point,
    end: Point,
    center: Point,
    clockwise: bool,
    object: &gerber::GraphicalObject,
    geometry: &gerber::ApertureGeometry,
) -> Vec<ExtractedPath> {
    let arc = Arc::new(start, end, center, clockwise);
    let radius = arc.radius();
    let sweep = arc.sweep_radians();
    let steps = sample_steps(radius * sweep);
    let signed_sweep = if clockwise { -sweep } else { sweep };
    let start_angle = start.angle_from(center);
    (0..=steps)
        .flat_map(|index| {
            let t = index as f64 / steps.max(1) as f64;
            let at = arc.point_at(start_angle + signed_sweep * t);
            aperture_paths(geometry, object_transform(object, at))
        })
        .collect()
}

fn object_transform(object: &gerber::GraphicalObject, at: Point) -> Affine2 {
    Affine2::placement(
        at,
        object.rotation_degrees,
        object.mirroring.into(),
        object.scaling,
    )
}

fn sample_steps(length: f64) -> usize {
    (length / SWEEP_SAMPLE_MM).ceil().max(1.0) as usize
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
