//! Lower pcb-ir artwork into an idiomatic Gerber X2 layer.
//!
//! This is the write-side mirror of [`crate::geometry::extract_document`]:
//! any artwork document annotated with [`LayerAttributes`]/[`ObjectAttributes`]
//! can be emitted as a Gerber file, regardless of which source dialect
//! produced it.

use pcb_ir::geom::{AccuracyError, GeometryAccuracy};
use std::collections::HashMap;

use crate::{
    AttributeValue, Contour, ContourSegment, GerberError, GerberLayer, ObjectKind,
    Point as GerberPoint, Result, WriterAperture, WriterApertureTemplate, WriterObject,
    sanitize_attribute_field,
};
use pcb_ir::dialects::artwork::legalize::bake_aperture_basis;
use pcb_ir::dialects::artwork::{Aperture, ApertureShape, Geometry as ArtworkGeometry, PaintStage};
use pcb_ir::geom::path::ContourBuf;
use pcb_ir::geom::region::{self, Ring};
use pcb_ir::geom::{Affine2, FillRule, Point, Polarity, Segment, StrokePatternMark};

const GERBER_GEOMETRY_GRID_MM: f64 = 0.001;
const GERBER_OUTLINE_MAX_VERTICES: usize = 5000;

/// Gerber file-level attributes carried as artwork layer metadata.
#[derive(Debug, Clone, Default)]
pub struct LayerAttributes {
    pub file_function: Vec<String>,
    pub part: Option<Vec<String>>,
    pub file_polarity: Option<String>,
    pub same_coordinates: Option<Vec<String>>,
}

/// Gerber X2 object attributes carried as artwork object metadata.
#[derive(Debug, Clone, Default)]
pub struct ObjectAttributes {
    pub aperture_function: Option<Vec<String>>,
    pub net: Option<String>,
    pub component: Option<String>,
    pub pin: Option<String>,
}

/// An artwork document annotated for Gerber export.
pub type ArtworkDocument = pcb_ir::dialects::artwork::Document<LayerAttributes, ObjectAttributes>;

/// Re-emit a parsed Gerber layer through the artwork IR.
///
/// This is the normalize pipeline: extract the parsed layer into artwork,
/// carry its X2 attributes across, and lower it back to idiomatic Gerber.
/// Source flashes survive as flashes; block instances are expanded.
pub fn normalize_layer(gerber: &crate::GerberX2, accuracy: GeometryAccuracy) -> Result<String> {
    let annotated =
        annotate_for_export(gerber, crate::geometry::extract_document(gerber, accuracy)?);
    crate::write_layer(&lower_artwork_layer(&annotated, accuracy)?)
}

/// Convert an extracted layer's interned Gerber metadata into the resolved
/// export annotations.
pub fn annotate_for_export(
    gerber: &crate::GerberX2,
    doc: crate::geometry::GerberArtworkDocument,
) -> ArtworkDocument {
    let annotate =
        |object: pcb_ir::dialects::artwork::Object<crate::geometry::GerberObjectMeta>| {
            pcb_ir::dialects::artwork::Object {
                polarity: object.polarity,
                order: object.order,
                geometry: object.geometry,
                bbox: object.bbox,
                meta: object_attributes(gerber, &object.meta),
            }
        };
    ArtworkDocument {
        apertures: doc.apertures,
        blocks: doc
            .blocks
            .into_iter()
            .map(|block| pcb_ir::dialects::artwork::Block {
                objects: block.objects.into_iter().map(annotate).collect(),
                bbox: block.bbox,
            })
            .collect(),
        layers: doc
            .layers
            .into_iter()
            .map(|layer| pcb_ir::dialects::artwork::Layer {
                name: layer.name,
                role: layer.role,
                side: layer.side,
                objects: layer.objects,
                bbox: layer.bbox,
                meta: LayerAttributes {
                    file_function: layer.meta,
                    part: file_attribute_fields(gerber, ".Part"),
                    file_polarity: file_attribute_fields(gerber, ".FilePolarity")
                        .and_then(|fields| fields.into_iter().next()),
                    same_coordinates: file_attribute_fields(gerber, ".SameCoordinates"),
                },
            })
            .collect(),
        objects: doc.objects.into_iter().map(annotate).collect(),
        arena: doc.arena,
        diagnostics: doc.diagnostics,
    }
}

fn file_attribute_fields(gerber: &crate::GerberX2, name: &str) -> Option<Vec<String>> {
    gerber
        .file_attributes()
        .iter()
        .find(|attribute| gerber.resolve(attribute.name) == name)
        .map(|attribute| resolve_fields(gerber, attribute))
}

fn object_attributes(
    gerber: &crate::GerberX2,
    meta: &crate::geometry::GerberObjectMeta,
) -> ObjectAttributes {
    let component = attribute_fields(gerber, &meta.object_attributes, ".C")
        .or_else(|| attribute_fields(gerber, &meta.object_attributes, ".P"))
        .and_then(|fields| fields.into_iter().next());
    ObjectAttributes {
        aperture_function: attribute_fields(gerber, &meta.aperture_attributes, ".AperFunction"),
        net: attribute_fields(gerber, &meta.object_attributes, ".N")
            .and_then(|fields| fields.into_iter().next()),
        component,
        pin: attribute_fields(gerber, &meta.object_attributes, ".P")
            .and_then(|fields| fields.into_iter().nth(1)),
    }
}

fn attribute_fields(
    gerber: &crate::GerberX2,
    attributes: &[crate::types::Attribute],
    name: &str,
) -> Option<Vec<String>> {
    attributes
        .iter()
        .find(|attribute| gerber.resolve(attribute.name) == name)
        .map(|attribute| resolve_fields(gerber, attribute))
}

fn resolve_fields(gerber: &crate::GerberX2, attribute: &crate::types::Attribute) -> Vec<String> {
    attribute
        .fields
        .iter()
        .map(|field| gerber.resolve(*field).to_string())
        .collect()
}

pub fn lower_artwork_layer(
    layer: &ArtworkDocument,
    accuracy: GeometryAccuracy,
) -> Result<GerberLayer> {
    let layer = pcb_ir::dialects::artwork::expand_instances_preserving_grids(layer);
    let mut apertures = ApertureTable::default();
    let mut plan = GerberPlan::default();
    let layer_attributes = layer
        .layers
        .first()
        .map(|layer| layer.meta.clone())
        .unwrap_or_default();

    // Expansion leaves only primitives and grids of primitive-only blocks:
    // every layer object is its children imaged at each occurrence.
    for object in &layer.objects {
        let (children, polarity, occurrences) = match object.geometry {
            ArtworkGeometry::GridInstance {
                block,
                transform,
                repeat,
            } => (
                layer.blocks[block as usize].objects.as_slice(),
                object.polarity,
                grid_occurrences(transform, repeat),
            ),
            _ => (
                std::slice::from_ref(object),
                Polarity::Dark,
                vec![(Affine2::IDENTITY, None)],
            ),
        };
        for (placement, repeat) in occurrences {
            for child in children {
                let polarity = polarity.compose(child.polarity);
                let mut objects = lower_artwork_object(
                    &layer,
                    child,
                    placement,
                    polarity,
                    &mut apertures,
                    accuracy,
                )?;
                for object in &mut objects {
                    object.repeat = repeat;
                }
                plan.push_group(child.order.stage, polarity, objects);
            }
        }
    }
    let objects = plan.into_ordered_objects();

    Ok(GerberLayer {
        file_attributes: lower_layer_attributes(&layer_attributes),
        apertures: apertures.apertures,
        objects,
        ..GerberLayer::default()
    })
}

/// A grid as Gerber images it: one step-repeated occurrence when its steps
/// run along the axes, otherwise every occurrence on its own.
fn grid_occurrences(
    placement: Affine2,
    grid: pcb_ir::dialects::artwork::GridRepeat,
) -> Vec<(Affine2, Option<crate::StepRepeat>)> {
    match gerber_step_repeat(placement, grid) {
        Some((base, repeat)) => vec![(
            base,
            (repeat.x_repeats > 1 || repeat.y_repeats > 1).then_some(repeat),
        )],
        None => grid
            .offsets()
            .map(|offset| {
                let occurrence = Affine2 {
                    m02: placement.m02 + offset.x,
                    m12: placement.m12 + offset.y,
                    ..placement
                };
                (occurrence, None)
            })
            .collect(),
    }
}

fn gerber_step_repeat(
    mut base: Affine2,
    grid: pcb_ir::dialects::artwork::GridRepeat,
) -> Option<(Affine2, crate::StepRepeat)> {
    // A zero-count axis means no occurrences at all; expansion emits nothing.
    if grid.x_count == 0 || grid.y_count == 0 {
        return None;
    }
    let mut x_repeats = 1;
    let mut y_repeats = 1;
    let mut x_step = 0.0;
    let mut y_step = 0.0;
    let mut shift = Point::ZERO;
    for (count, step) in [(grid.x_count, grid.x_step), (grid.y_count, grid.y_step)] {
        let step = Point::new(gerber_coordinate(step.x), gerber_coordinate(step.y));
        // A zero-step axis collapses to one occurrence: repeated stamps at
        // the same location are image-idempotent in either polarity.
        if count <= 1 || step == Point::ZERO {
            continue;
        }
        if step.y == 0.0 && x_repeats == 1 {
            x_repeats = count as i32;
            x_step = step.x.abs();
            if step.x < 0.0 {
                shift.x += (count - 1) as f64 * step.x;
            }
        } else if step.x == 0.0 && y_repeats == 1 {
            y_repeats = count as i32;
            y_step = step.y.abs();
            if step.y < 0.0 {
                shift.y += (count - 1) as f64 * step.y;
            }
        } else {
            return None;
        }
    }
    base.m02 += shift.x;
    base.m12 += shift.y;
    Some((
        base,
        crate::StepRepeat {
            x_repeats,
            y_repeats,
            x_step,
            y_step,
        },
    ))
}

fn lower_artwork_object(
    layer: &ArtworkDocument,
    object: &pcb_ir::dialects::artwork::Object<ObjectAttributes>,
    transform: Affine2,
    polarity: Polarity,
    apertures: &mut ApertureTable,
    accuracy: GeometryAccuracy,
) -> Result<Vec<WriterObject>> {
    let attributes = lower_object_attributes(&object.meta);
    let aperture_function = object.meta.aperture_function.as_deref().unwrap_or_default();
    let mut objects = Vec::new();
    match object.geometry {
        ArtworkGeometry::Region { path } => {
            objects.extend(lower_region_objects(
                layer,
                path,
                transform,
                polarity,
                &lower_aperture_function(aperture_function),
                &attributes,
                accuracy,
            )?);
        }
        ArtworkGeometry::Stroke { path } => {
            let artwork_path = &layer.arena.paths[path as usize];
            let stroke = artwork_path.stroke().ok_or_else(|| {
                GerberError::InvalidStructure(
                    "artwork stroke geometry references a path without stroke paint".to_string(),
                )
            })?;
            let stroke_width = stroke.width * transform.m00.hypot(transform.m10);
            let aperture =
                apertures.define(Aperture::circle(stroke_width), aperture_function, accuracy)?;
            for contour in layer
                .arena
                .path_contours(artwork_path)
                .into_iter()
                .map(|contour| contour.transformed(transform))
            {
                let segments = contour_segments(&contour, accuracy)?;
                for mark in
                    pcb_ir::geom::stroke_pattern_marks(&segments, stroke.pattern, stroke_width)
                {
                    match mark {
                        StrokePatternMark::Dash(segments) => {
                            objects.extend(segments.into_iter().map(|segment| {
                                WriterObject::new(
                                    lower_stroke_segment(segment, aperture),
                                    polarity,
                                    attributes.clone(),
                                )
                            }));
                        }
                        StrokePatternMark::Dot(at) => objects.push(WriterObject::new(
                            ObjectKind::Flash {
                                at: lower_point(at),
                                aperture,
                            },
                            polarity,
                            attributes.clone(),
                        )),
                    }
                }
            }
        }
        ArtworkGeometry::Flash {
            aperture,
            transform: placement,
        } => {
            let transform = transform.concat(placement);
            let aperture =
                apertures.flash(layer, aperture, transform, aperture_function, accuracy)?;
            objects.push(WriterObject::new(
                ObjectKind::Flash {
                    at: lower_point(Point::new(transform.m02, transform.m12)),
                    aperture,
                },
                polarity,
                attributes,
            ));
        }
        ArtworkGeometry::Instance { .. } | ArtworkGeometry::GridInstance { .. } => {
            unreachable!("instance expansion leaves only primitive geometry")
        }
    }
    Ok(objects)
}

fn lower_stroke_segment(segment: Segment, aperture: i32) -> ObjectKind {
    match segment {
        Segment::Line { start, end } => ObjectKind::Draw {
            start: lower_point(start),
            end: lower_point(end),
            aperture,
        },
        Segment::Arc(arc) => ObjectKind::Arc {
            start: lower_point(arc.start),
            end: lower_point(arc.end),
            center_offset: lower_point(Point::new(
                arc.center.x - arc.start.x,
                arc.center.y - arc.start.y,
            )),
            clockwise: arc.clockwise,
            aperture,
        },
        Segment::Cubic { .. } | Segment::Ellipse(_) => {
            unreachable!("contour_segments flattens curves")
        }
    }
}

#[derive(Debug, Default)]
struct GerberPlan {
    groups: Vec<GerberObjectGroup>,
}

#[derive(Debug)]
struct GerberObjectGroup {
    stage: PaintStage,
    polarity: Polarity,
    objects: Vec<WriterObject>,
}

/// Emission order for commuting groups: stage first, then object attributes
/// and aperture so identical writer state runs together. A group's objects
/// all lower from one artwork object and share attributes.
fn group_order(group: &GerberObjectGroup) -> (PaintStage, &[AttributeValue], i32) {
    let first = group.objects.first();
    (
        group.stage,
        first.map_or(&[], |object| object.attributes.as_slice()),
        first.map_or(i32::MAX, |object| match object.kind {
            ObjectKind::Draw { aperture, .. }
            | ObjectKind::Arc { aperture, .. }
            | ObjectKind::Flash { aperture, .. } => aperture,
            ObjectKind::Region { .. } => i32::MAX,
        }),
    )
}

impl GerberPlan {
    fn push_group(&mut self, stage: PaintStage, polarity: Polarity, objects: Vec<WriterObject>) {
        if objects.is_empty() {
            return;
        }
        self.groups.push(GerberObjectGroup {
            stage,
            polarity,
            objects,
        });
    }

    fn into_ordered_objects(self) -> Vec<WriterObject> {
        // Dark paint commutes with dark paint and clear with clear, but not
        // across a polarity change: stage ordering (fills before pads) may
        // only permute groups within each maximal same-polarity run. Within
        // a stage the same commutativity lets groups cluster by object
        // attributes and aperture, so the writer's attribute and tool state
        // changes as rarely as possible. Final cutouts are terminal by
        // definition and emit after everything.
        let (cutouts, mut painted): (Vec<_>, Vec<_>) = self
            .groups
            .into_iter()
            .partition(|group| group.stage == PaintStage::FinalCutout);
        let mut start = 0;
        while start < painted.len() {
            let polarity = painted[start].polarity;
            let mut end = start + 1;
            while end < painted.len() && painted[end].polarity == polarity {
                end += 1;
            }
            painted[start..end].sort_by(|a, b| group_order(a).cmp(&group_order(b)));
            start = end;
        }
        painted
            .into_iter()
            .chain(cutouts)
            .flat_map(|group| group.objects)
            .collect()
    }
}

#[derive(Default)]
struct ApertureTable {
    by_key: HashMap<ApertureKey, i32>,
    apertures: Vec<WriterAperture>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ApertureKey {
    template: ApertureTemplateKey,
    function: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ApertureTemplateKey {
    /// A source aperture under one linear basis. The layer is immutable
    /// and the accuracy fixed for this table, so a hit skips all preparation.
    Source {
        aperture: u32,
        basis: [i64; 4],
    },
    Circle {
        diameter_nm: i64,
        hole_nm: i64,
    },
    Rectangle {
        width_nm: i64,
        height_nm: i64,
        hole_nm: i64,
    },
    Obround {
        width_nm: i64,
        height_nm: i64,
        hole_nm: i64,
    },
    Polygon {
        diameter_nm: i64,
        vertices: u32,
        rotation_microdeg: i64,
        hole_nm: i64,
    },
    Outline(Vec<Vec<(i64, i64)>>),
}

impl ApertureTable {
    /// The aperture imaging `source` under the linear part of `transform`.
    ///
    /// Gerber's own `%LR`/`%LM`/`%LS` are never used: JLCPCB shifts
    /// off-origin custom apertures under `%LR`, so every basis is baked
    /// into the definition.
    fn flash(
        &mut self,
        layer: &ArtworkDocument,
        source: u32,
        transform: Affine2,
        function: &[String],
        accuracy: GeometryAccuracy,
    ) -> Result<i32> {
        let basis = Affine2 {
            m02: 0.0,
            m12: 0.0,
            ..transform
        };
        let key = ApertureKey {
            template: ApertureTemplateKey::Source {
                aperture: source,
                // Composed placements of one orientation differ in their
                // last bits; a nano-scale basis grid reunites them.
                basis: [basis.m00, basis.m01, basis.m10, basis.m11]
                    .map(|value| (value * 1e9).round() as i64),
            },
            function: function.to_vec(),
        };
        if let Some(code) = self.by_key.get(&key) {
            return Ok(*code);
        }
        let aperture = layer.apertures.get(source as usize).ok_or_else(|| {
            GerberError::InvalidStructure(format!(
                "artwork flash references missing aperture {source}"
            ))
        })?;
        let code = self.define(bake_aperture_basis(aperture, basis), function, accuracy)?;
        self.by_key.insert(key, code);
        Ok(code)
    }

    /// Define an aperture, reusing an identical definition.
    ///
    /// The four standard templates stay standard. Every other shape is one
    /// flattened outline macro: JLCPCB renders primitive 21 rounded
    /// rectangles oversized, and legacy CAM importers evaluate compound
    /// macros per flash.
    fn define(
        &mut self,
        aperture: Aperture,
        function: &[String],
        accuracy: GeometryAccuracy,
    ) -> Result<i32> {
        let bounds = aperture.bbox();
        if !(bounds.width() > 0.0 && bounds.height() > 0.0) {
            return Err(GerberError::InvalidStructure(format!(
                "cannot export empty Gerber aperture {:?}",
                aperture.shape
            )));
        }
        let hole_diameter = (aperture.hole_diameter > 0.0).then_some(aperture.hole_diameter);
        let hole_nm = hole_diameter.map_or(0, quantize_mm);
        let (template_key, template) = match aperture.shape {
            ApertureShape::Circle { diameter } => (
                ApertureTemplateKey::Circle {
                    diameter_nm: quantize_mm(diameter),
                    hole_nm,
                },
                WriterApertureTemplate::Circle {
                    diameter,
                    hole_diameter,
                },
            ),
            ApertureShape::Rectangle { width, height } => (
                ApertureTemplateKey::Rectangle {
                    width_nm: quantize_mm(width),
                    height_nm: quantize_mm(height),
                    hole_nm,
                },
                WriterApertureTemplate::Rectangle {
                    width,
                    height,
                    hole_diameter,
                },
            ),
            ApertureShape::Obround { width, height } => (
                ApertureTemplateKey::Obround {
                    width_nm: quantize_mm(width),
                    height_nm: quantize_mm(height),
                    hole_nm,
                },
                WriterApertureTemplate::Obround {
                    width,
                    height,
                    hole_diameter,
                },
            ),
            ApertureShape::Polygon {
                diameter,
                vertices,
                rotation_degrees,
            } => (
                ApertureTemplateKey::Polygon {
                    diameter_nm: quantize_mm(diameter),
                    vertices,
                    rotation_microdeg: quantize_mm(rotation_degrees),
                    hole_nm,
                },
                WriterApertureTemplate::Polygon {
                    outer_diameter: diameter,
                    vertices: vertices as i32,
                    rotation_degrees: Some(rotation_degrees),
                    hole_diameter,
                },
            ),
            ApertureShape::RoundRect { .. }
            | ApertureShape::RoundedHex { .. }
            | ApertureShape::Contour { .. } => {
                let outlines =
                    prepare_on_grid(&aperture.contours(), aperture.fill_rule(), accuracy)?;
                if outlines.is_empty() {
                    return Err(GerberError::InvalidStructure(
                        "cannot export an empty Gerber aperture outline".to_string(),
                    ));
                }
                (
                    ApertureTemplateKey::Outline(
                        outlines
                            .iter()
                            .map(|ring| {
                                ring.iter()
                                    .map(|[x, y]| (quantize_mm(*x), quantize_mm(*y)))
                                    .collect()
                            })
                            .collect(),
                    ),
                    WriterApertureTemplate::Outline { outlines },
                )
            }
        };
        let key = ApertureKey {
            template: template_key,
            function: function.to_vec(),
        };
        if let Some(code) = self.by_key.get(&key) {
            return Ok(*code);
        }
        let code = 10 + self.apertures.len() as i32;
        self.by_key.insert(key, code);
        self.apertures.push(WriterAperture {
            code,
            template,
            attributes: lower_aperture_function(function),
        });
        Ok(code)
    }
}

fn lower_layer_attributes(attributes: &LayerAttributes) -> Vec<AttributeValue> {
    let mut values = vec![AttributeValue::new(
        ".FileFunction",
        attributes.file_function.iter().cloned(),
    )];
    if let Some(part) = &attributes.part {
        values.push(AttributeValue::new(".Part", part.iter().cloned()));
    }
    if let Some(file_polarity) = &attributes.file_polarity {
        values.push(AttributeValue::new(
            ".FilePolarity",
            [file_polarity.clone()],
        ));
    }
    if let Some(same_coordinates) = &attributes.same_coordinates {
        values.push(AttributeValue::new(
            ".SameCoordinates",
            same_coordinates.iter().cloned(),
        ));
    }
    values
}

fn lower_region_objects(
    layer: &ArtworkDocument,
    path_index: u32,
    transform: Affine2,
    polarity: Polarity,
    aperture_attributes: &[AttributeValue],
    attributes: &[AttributeValue],
    accuracy: GeometryAccuracy,
) -> Result<Vec<WriterObject>> {
    let artwork_path = &layer.arena.paths[path_index as usize];
    let contours = layer
        .arena
        .path_contours(artwork_path)
        .into_iter()
        .map(|contour| contour.transformed(transform))
        .collect::<Vec<_>>();
    let fill_rule = artwork_path.fill_rule().unwrap_or(FillRule::NonZero);
    Ok(prepare_on_grid(&contours, fill_rule, accuracy)?
        .iter()
        .map(|ring| WriterObject {
            aperture_attributes: aperture_attributes.to_vec(),
            ..WriterObject::new(
                ObjectKind::Region {
                    contours: vec![lower_ring(ring)],
                },
                polarity,
                attributes.to_vec(),
            )
        })
        .collect())
}

/// Quantize a filled set onto the Gerber grid as simple additive polygons.
fn prepare_on_grid(
    payloads: &[ContourBuf],
    fill_rule: FillRule,
    accuracy: GeometryAccuracy,
) -> Result<Vec<Ring>> {
    // The grid overlay is the only regularization: it resolves the fill rule
    // on the coordinates the file will actually carry.
    let (rings, uncertainty_mm) = region::flatten_within(payloads, accuracy)?;
    accuracy.check(uncertainty_mm + GERBER_GEOMETRY_GRID_MM / std::f64::consts::SQRT_2)?;
    region::decompose_on_grid(
        rings,
        fill_rule,
        GERBER_GEOMETRY_GRID_MM,
        GERBER_OUTLINE_MAX_VERTICES,
    )
    .map_err(|error| GerberError::InvalidStructure(error.to_string()))
}

fn lower_ring(ring: &Ring) -> Contour {
    let point = |&[x, y]: &[f64; 2]| GerberPoint { x, y };
    Contour {
        segments: ring
            .iter()
            .zip(ring.iter().cycle().skip(1))
            .map(|(start, end)| ContourSegment::Line {
                start: point(start),
                end: point(end),
            })
            .collect(),
    }
}

/// Decode a contour into the line and circular-arc segments Gerber can draw,
/// flattening cubic and elliptical curves within the accuracy budget.
fn contour_segments(
    contour: &ContourBuf,
    accuracy: GeometryAccuracy,
) -> std::result::Result<Vec<Segment>, AccuracyError> {
    Ok(contour.flattened_curves(accuracy)?.segments().collect())
}

fn lower_object_attributes(attributes: &ObjectAttributes) -> Vec<AttributeValue> {
    let mut values = Vec::new();
    if let Some(component) = &attributes.component {
        values.push(AttributeValue::new(
            ".C",
            [sanitize_attribute_field(component)],
        ));
    }
    if let (Some(component), Some(pin)) = (&attributes.component, &attributes.pin) {
        values.push(AttributeValue::new(
            ".P",
            [
                sanitize_attribute_field(component),
                sanitize_attribute_field(pin),
            ],
        ));
    }
    if let Some(net) = &attributes.net {
        values.push(AttributeValue::new(".N", [sanitize_attribute_field(net)]));
    }
    values
}

fn lower_aperture_function(function: &[String]) -> Vec<AttributeValue> {
    (!function.is_empty())
        .then(|| AttributeValue::new(".AperFunction", function.iter().cloned()))
        .into_iter()
        .collect()
}

fn lower_point(point: Point) -> GerberPoint {
    GerberPoint {
        x: point.x,
        y: point.y,
    }
}

fn quantize_mm(value: f64) -> i64 {
    (value * 1_000_000.0).round() as i64
}

fn gerber_coordinate(value: f64) -> f64 {
    quantize_mm(value) as f64 / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use pcb_ir::geom::path::PathCmd;

    /// Independent syntax oracle: the MakerPnP `gerber_parser` crate must
    /// accept everything our writer emits.
    fn assert_external_parser_accepts(content: &str) {
        let reader = std::io::BufReader::new(content.as_bytes());
        if let Err((_, error)) = gerber_parser::parse(reader) {
            panic!("external gerber_parser rejected our output: {error:?}\n---\n{content}");
        }
    }
    use pcb_ir::dialects::artwork::{
        Layer as IrArtworkDocument, Object as ArtworkObject, PaintOrder,
    };
    use pcb_ir::dialects::{LayerRole, Side};
    use pcb_ir::geom::{BBox, Mirror, Paint, Resolution, Span};

    fn assert_strict_simple_rings(rings: &[Ring]) {
        for ring in rings {
            assert!(ring.len() >= 3);
            for (index, point) in ring.iter().enumerate() {
                assert!(!ring[..index].contains(point), "ring repeats a coordinate");
            }
            for index in 0..ring.len() {
                assert_ne!(ring[index], ring[(index + 1) % ring.len()], "zero edge");
            }
            // The small regression fixtures use integer-grid straight edges;
            // a simple ring may only intersect an adjacent edge at its endpoint.
            for a in 0..ring.len() {
                for b in (a + 1)..ring.len() {
                    if b == a + 1 || (a == 0 && b + 1 == ring.len()) {
                        continue;
                    }
                    let [a0, a1] = [ring[a], ring[(a + 1) % ring.len()]];
                    let [b0, b1] = [ring[b], ring[(b + 1) % ring.len()]];
                    let orient = |p: [f64; 2], q: [f64; 2], r: [f64; 2]| {
                        (q[0] - p[0]) * (r[1] - p[1]) - (q[1] - p[1]) * (r[0] - p[0])
                    };
                    let on_segment = |p: [f64; 2], q: [f64; 2], r: [f64; 2]| {
                        orient(p, q, r) == 0.0
                            && r[0] >= p[0].min(q[0])
                            && r[0] <= p[0].max(q[0])
                            && r[1] >= p[1].min(q[1])
                            && r[1] <= p[1].max(q[1])
                    };
                    let [o1, o2, o3, o4] = [
                        orient(a0, a1, b0),
                        orient(a0, a1, b1),
                        orient(b0, b1, a0),
                        orient(b0, b1, a1),
                    ];
                    let crosses = (o1 * o2 < 0.0 && o3 * o4 < 0.0)
                        || on_segment(a0, a1, b0)
                        || on_segment(a0, a1, b1)
                        || on_segment(b0, b1, a0)
                        || on_segment(b0, b1, a1);
                    assert!(!crosses, "non-adjacent edges intersect");
                }
            }
        }
    }

    fn outlines(layer: &GerberLayer) -> Vec<&Ring> {
        layer
            .apertures
            .iter()
            .flat_map(|aperture| match &aperture.template {
                WriterApertureTemplate::Outline { outlines } => outlines.as_slice(),
                _ => &[],
            })
            .collect()
    }

    fn parsed_area(layer: &GerberLayer) -> f64 {
        let text = crate::write_layer(layer).expect("serialize Gerber");
        assert_external_parser_accepts(&text);
        let parsed = crate::GerberX2::parse(&text).expect("parse Gerber");
        let geometry = crate::geometry::extract_document(&parsed, GeometryAccuracy::default())
            .expect("extract Gerber geometry");
        pcb_ir::dialects::artwork::compare::summarize(&geometry, Resolution::default())
            .unwrap()
            .area_mm2
    }

    #[test]
    fn grid_preparation_rejects_subgrid_budgets() {
        let contour = rect_payload(0.0001, 0.0001, 0.0002, 0.0002);
        assert!(
            prepare_on_grid(
                &[contour],
                FillRule::NonZero,
                GeometryAccuracy::new(0.0001).unwrap()
            )
            .is_err()
        );
    }

    #[test]
    fn source_contour_reuse_preserves_placement_function_basis_and_errors() {
        let accuracy = GeometryAccuracy::default();
        let mut artwork = ArtworkDocument::new();
        let outline = rect_payload(1.0, 0.0, 2.0, 0.5);
        let source = artwork.push_aperture(Aperture::solid(ApertureShape::Contour {
            outline: outline.clone(),
            fill_rule: FillRule::EvenOdd,
        }));
        let mut uncertain = outline;
        uncertain.uncertainty_mm = 1.0;
        let invalid = artwork.push_aperture(Aperture::solid(ApertureShape::Contour {
            outline: uncertain,
            fill_rule: FillRule::EvenOdd,
        }));
        let mut table = ApertureTable::default();
        let mut flash = ArtworkObject {
            geometry: ArtworkGeometry::Flash {
                aperture: source,
                transform: Affine2::IDENTITY,
            },
            polarity: Polarity::Dark,
            order: Default::default(),
            bbox: BBox::empty(),
            meta: ObjectAttributes::default(),
        };
        let lower = |object: &ArtworkObject<ObjectAttributes>, basis, table: &mut ApertureTable| {
            lower_artwork_object(&artwork, object, basis, Polarity::Dark, table, accuracy)
        };
        let first = lower(&flash, Affine2::IDENTITY, &mut table).unwrap();
        let ObjectKind::Flash { aperture: code, .. } = first[0].kind else {
            panic!("expected a flash");
        };
        let definitions = table.apertures.len();
        let translated = lower(
            &flash,
            Affine2::translation(Point::new(100_000.000_3, -10_000.000_2)),
            &mut table,
        )
        .unwrap();
        assert!(
            matches!(translated[0].kind, ObjectKind::Flash { aperture, at }
            if aperture == code && at.x == 100_000.000_3 && at.y == -10_000.000_2)
        );
        assert_eq!(table.apertures.len(), definitions);

        // Functions remain separate even for the same source geometry.
        flash.meta.aperture_function = Some(vec!["SMDPad".into()]);
        let attributed = lower(&flash, Affine2::IDENTITY, &mut table).unwrap();
        assert!(
            matches!(attributed[0].kind, ObjectKind::Flash { aperture, .. } if aperture != code)
        );
        flash.meta.aperture_function = None;
        // Every basis is its own definition.
        let scaled = lower(
            &flash,
            Affine2 {
                m00: 2.0,
                ..Affine2::IDENTITY
            },
            &mut table,
        )
        .unwrap();
        assert!(matches!(scaled[0].kind, ObjectKind::Flash { aperture, .. } if aperture != code));

        // A different source ID with the same coordinates must still validate
        // its inherited uncertainty; failures must not register an alias.
        flash.geometry = ArtworkGeometry::Flash {
            aperture: invalid,
            transform: Affine2::IDENTITY,
        };
        assert!(lower(&flash, Affine2::IDENTITY, &mut table).is_err());
        assert!(!table.by_key.contains_key(&ApertureKey {
            template: ApertureTemplateKey::Source {
                aperture: invalid,
                basis: [1_000_000_000, 0, 0, 1_000_000_000],
            },
            function: Vec::new(),
        }));
        // A new export/table must check its own, finer accuracy budget.
        flash.geometry = ArtworkGeometry::Flash {
            aperture: source,
            transform: Affine2::IDENTITY,
        };
        assert!(
            lower_artwork_object(
                &artwork,
                &flash,
                Affine2::IDENTITY,
                Polarity::Dark,
                &mut ApertureTable::default(),
                GeometryAccuracy::new(0.0001).unwrap(),
            )
            .is_err()
        );
    }

    #[test]
    fn sanitizes_net_names_for_gerber_attribute_fields() {
        let attributes = lower_object_attributes(&ObjectAttributes {
            aperture_function: None,
            net: Some("PWR_RST*,A%B".to_string()),
            component: None,
            pin: None,
        });

        assert_eq!(attributes[0].name, ".N");
        assert_eq!(attributes[0].fields, ["PWR_RST__A_B"]);
    }

    #[test]
    fn lowers_pin_attribute_with_component_context() {
        let attributes = lower_object_attributes(&ObjectAttributes {
            aperture_function: None,
            net: None,
            component: Some("U1".to_string()),
            pin: Some("1".to_string()),
        });

        assert_eq!(attributes[0].name, ".C");
        assert_eq!(attributes[0].fields, ["U1"]);
        assert_eq!(attributes[1].name, ".P");
        assert_eq!(attributes[1].fields, ["U1", "1"]);
    }

    #[test]
    fn skips_pin_attribute_without_component_context() {
        let attributes = lower_object_attributes(&ObjectAttributes {
            aperture_function: None,
            net: None,
            component: None,
            pin: Some("1".to_string()),
        });

        assert!(attributes.is_empty());
    }

    #[test]
    fn lowering_bakes_off_origin_aperture_rotation() {
        let accuracy = GeometryAccuracy::default();

        let mut artwork = ArtworkDocument::new();
        let aperture = artwork.push_aperture(Aperture::solid(ApertureShape::Contour {
            outline: ContourBuf::new(vec![
                PathCmd::move_to(Point::new(1.0, 0.0)),
                PathCmd::line_to(Point::new(2.0, 0.0)),
                PathCmd::line_to(Point::new(1.0, 0.5)),
                PathCmd::close(),
            ]),
            fill_rule: FillRule::NonZero,
        }));
        let layer = artwork.push_layer(IrArtworkDocument {
            name: "B.Cu".to_string(),
            role: LayerRole::Copper,
            side: Side::Bottom,
            objects: Span::EMPTY,
            bbox: BBox::empty(),
            meta: LayerAttributes {
                file_function: vec!["Copper".to_string(), "L2".to_string(), "Bot".to_string()],
                part: Some(vec!["Single".to_string()]),
                file_polarity: Some("Positive".to_string()),
                same_coordinates: Some(Vec::new()),
            },
        });
        artwork.push_object(
            layer,
            ArtworkObject {
                polarity: Polarity::Dark,
                order: PaintOrder::default(),
                geometry: ArtworkGeometry::Flash {
                    aperture,
                    transform: Affine2::placement(Point::new(10.0, 20.0), 90.0, Mirror::NONE, 1.0),
                },
                bbox: BBox::empty(),
                meta: ObjectAttributes {
                    aperture_function: Some(vec!["SMDPad".to_string(), "CuDef".to_string()]),
                    ..ObjectAttributes::default()
                },
            },
        );
        pcb_ir::dialects::artwork::normalize_bounds(&mut artwork);

        let gerber = crate::write_layer(&lower_artwork_layer(&artwork, accuracy).unwrap()).unwrap();

        assert!(!gerber.contains("%LR"));
        assert!(!gerber.contains("%LM"));
        assert!(!gerber.contains("%LS"));
        assert_eq!(gerber.matches("D03*").count(), 1);
        crate::GerberX2::parse(&gerber).unwrap();
    }

    #[test]
    fn repeated_translated_regions_remain_expanded() {
        let accuracy = GeometryAccuracy::default();

        let mut artwork = ArtworkDocument::new();
        let layer_id = artwork.push_layer(IrArtworkDocument {
            name: "F.Cu".to_string(),
            role: LayerRole::Copper,
            side: Side::Top,
            objects: Span::EMPTY,
            bbox: BBox::empty(),
            meta: LayerAttributes::default(),
        });
        for offset in [0.0, 20.0] {
            let path = artwork.push_path(
                Paint::Fill {
                    rule: FillRule::NonZero,
                },
                vec![clockwise_rect_payload(offset, 0.0, offset + 10.0, 10.0)],
            );
            artwork.push_object(
                layer_id,
                ArtworkObject {
                    polarity: Polarity::Dark,
                    order: Default::default(),
                    geometry: ArtworkGeometry::Region { path },
                    bbox: artwork.path_bbox(path),
                    meta: ObjectAttributes::default(),
                },
            );
        }

        let gerber = lower_artwork_layer(&artwork, accuracy).expect("lower repeated regions");
        assert!(
            gerber
                .objects
                .iter()
                .all(|object| matches!(&object.kind, ObjectKind::Region { .. }))
        );

        let contents = crate::write_layer(&gerber).expect("write repeated regions");
        assert!(!contents.contains("%ABD"));
        assert!(!contents.contains("%AM"));
        assert_external_parser_accepts(&contents);
        let parsed = crate::GerberX2::parse(&contents).expect("parse repeated regions");
        let geometry = crate::geometry::extract_document(&parsed, accuracy).unwrap();
        let summary =
            pcb_ir::dialects::artwork::compare::summarize(&geometry, Resolution::default())
                .unwrap();
        assert!((summary.area_mm2 - 200.0).abs() < 0.001);
    }

    #[test]
    fn overlapping_clear_regions_remain_independent() {
        let accuracy = GeometryAccuracy::default();

        let mut artwork = ArtworkDocument::new();
        let layer = artwork.push_layer(IrArtworkDocument {
            name: "F.Cu".to_string(),
            role: LayerRole::Copper,
            side: Side::Top,
            objects: Span::EMPTY,
            bbox: BBox::empty(),
            meta: LayerAttributes::default(),
        });
        let base = artwork.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            vec![rect_payload(0.0, 0.0, 10.0, 10.0)],
        );
        artwork.push_object(
            layer,
            ArtworkObject {
                polarity: Polarity::Dark,
                order: Default::default(),
                geometry: ArtworkGeometry::Region { path: base },
                bbox: artwork.path_bbox(base),
                meta: ObjectAttributes::default(),
            },
        );
        for (min_x, min_y, max_x, max_y) in [(1.0, 1.0, 7.0, 7.0), (4.0, 3.0, 9.0, 8.0)] {
            let path = artwork.push_path(
                Paint::Fill {
                    rule: FillRule::NonZero,
                },
                vec![rect_payload(min_x, min_y, max_x, max_y)],
            );
            artwork.push_object(
                layer,
                ArtworkObject {
                    polarity: Polarity::Clear,
                    order: Default::default(),
                    geometry: ArtworkGeometry::Region { path },
                    bbox: artwork.path_bbox(path),
                    meta: ObjectAttributes::default(),
                },
            );
        }

        let expected_mask =
            pcb_ir::dialects::artwork::compose_to_mask(&artwork, Resolution::default()).unwrap();
        let expected_layer = &expected_mask.layers[0];
        let expected_area = pcb_ir::geom::ContourSet::from_painted_paths(
            &expected_mask.arena,
            expected_mask.shapes(expected_layer),
            Resolution::new(pcb_ir::geom::tol::REGION_MM, accuracy),
        )
        .unwrap()
        .area();
        let gerber =
            lower_artwork_layer(&artwork, accuracy).expect("lower overlapping clear regions");
        let clear_regions = gerber
            .objects
            .iter()
            .filter(|object| {
                object.polarity == Polarity::Clear
                    && matches!(object.kind, ObjectKind::Region { .. })
            })
            .count();
        assert_eq!(clear_regions, 2);

        let contents = crate::write_layer(&gerber).expect("write overlapping clear regions");
        assert_external_parser_accepts(&contents);
        let parsed = crate::GerberX2::parse(&contents).expect("parse overlapping clear regions");
        let geometry = crate::geometry::extract_document(&parsed, accuracy).unwrap();
        let summary =
            pcb_ir::dialects::artwork::compare::summarize(&geometry, Resolution::default())
                .unwrap();
        assert!((summary.area_mm2 - expected_area).abs() < 0.001);
    }

    #[test]
    fn nested_clear_regions_expand_without_aperture_blocks() {
        let accuracy = GeometryAccuracy::default();

        let mut artwork = ArtworkDocument::new();
        let layer = artwork.push_layer(IrArtworkDocument {
            name: "F.Cu".to_string(),
            role: LayerRole::Copper,
            side: Side::Top,
            objects: Span::EMPTY,
            bbox: BBox::empty(),
            meta: LayerAttributes::default(),
        });
        let source_block = artwork.push_block();
        let base = artwork.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            vec![rect_payload(0.0, 0.0, 10.0, 4.0)],
        );
        artwork.push_block_object(
            source_block,
            ArtworkObject {
                polarity: Polarity::Dark,
                order: Default::default(),
                geometry: ArtworkGeometry::Region { path: base },
                bbox: artwork.path_bbox(base),
                meta: ObjectAttributes::default(),
            },
        );
        for center_x in [2.0, 6.0] {
            let path = artwork.push_path(
                Paint::Fill {
                    rule: FillRule::NonZero,
                },
                vec![circle_payload(Point::new(center_x, 2.0), 1.0)],
            );
            artwork.push_block_object(
                source_block,
                ArtworkObject {
                    polarity: Polarity::Clear,
                    order: Default::default(),
                    geometry: ArtworkGeometry::Region { path },
                    bbox: artwork.path_bbox(path),
                    meta: ObjectAttributes::default(),
                },
            );
        }
        artwork.push_object(
            layer,
            ArtworkObject {
                polarity: Polarity::Dark,
                order: Default::default(),
                geometry: ArtworkGeometry::Instance {
                    block: source_block,
                    transform: Affine2::placement(Point::new(20.0, 30.0), 90.0, Mirror::Y, 1.0),
                },
                bbox: BBox::empty(),
                meta: ObjectAttributes::default(),
            },
        );

        let gerber = lower_artwork_layer(&artwork, accuracy).expect("lower repeated clear arcs");
        assert!(gerber.objects.iter().all(|object| {
            matches!(
                &object.kind,
                ObjectKind::Region { contours }
                    if contours.iter().all(|contour| contour.segments.iter().all(
                        |segment| matches!(segment, ContourSegment::Line { .. })
                    ))
            )
        }));

        let contents = crate::write_layer(&gerber).expect("write repeated clear arcs");
        assert!(!contents.contains("%ABD"));
        assert_external_parser_accepts(&contents);
        let parsed = crate::GerberX2::parse(&contents).expect("parse repeated clear arcs");
        let geometry = crate::geometry::extract_document(&parsed, accuracy).unwrap();
        fn image<M>(mask: &pcb_ir::dialects::mask::Document<M>) -> pcb_ir::geom::ContourSet {
            let accuracy = GeometryAccuracy::default();

            let layer = &mask.layers[0];
            pcb_ir::geom::ContourSet::from_painted_paths(
                &mask.arena,
                mask.shapes(layer),
                Resolution::new(pcb_ir::geom::tol::REGION_MM, accuracy),
            )
            .unwrap()
        }
        let expected = image(
            &pcb_ir::dialects::artwork::compose_to_mask(&artwork, Resolution::default()).unwrap(),
        );
        let actual = image(
            &pcb_ir::dialects::artwork::compose_to_mask(&geometry, Resolution::default()).unwrap(),
        );
        let symmetric_difference = expected.difference(&actual).unwrap().area()
            + actual.difference(&expected).unwrap().area();
        assert!(symmetric_difference < 0.01, "{symmetric_difference}");
    }

    #[test]
    fn single_flash_instances_expand_without_losing_placement_or_polarity() {
        let accuracy = GeometryAccuracy::default();

        let mut artwork = ArtworkDocument::new();
        let aperture = artwork.push_aperture(Aperture::circle(1.0));
        let block = artwork.push_block();
        artwork.push_block_object(
            block,
            ArtworkObject {
                polarity: Polarity::Clear,
                order: Default::default(),
                geometry: ArtworkGeometry::Flash {
                    aperture,
                    transform: Affine2::translation(Point::new(2.0, 3.0)),
                },
                bbox: BBox::empty(),
                meta: ObjectAttributes {
                    aperture_function: Some(vec!["AntiPad".to_string()]),
                    ..ObjectAttributes::default()
                },
            },
        );
        let layer = artwork.push_layer(IrArtworkDocument {
            name: "F.Cu".to_string(),
            role: LayerRole::Copper,
            side: Side::Top,
            objects: Span::EMPTY,
            bbox: BBox::empty(),
            meta: LayerAttributes::default(),
        });
        artwork.push_object(
            layer,
            ArtworkObject {
                polarity: Polarity::Dark,
                order: Default::default(),
                geometry: ArtworkGeometry::Instance {
                    block,
                    transform: Affine2::translation(Point::new(10.0, 20.0)),
                },
                bbox: BBox::empty(),
                meta: ObjectAttributes::default(),
            },
        );

        let gerber = lower_artwork_layer(&artwork, accuracy).expect("lower aliased placement");
        let [object] = gerber.objects.as_slice() else {
            panic!("expected one direct flash");
        };
        assert_eq!(object.polarity, Polarity::Clear);
        assert!(matches!(
            object.kind,
            ObjectKind::Flash {
                at: GerberPoint { x: 12.0, y: 23.0 },
                ..
            }
        ));
    }

    #[test]
    fn empty_blocks_do_not_reach_gerber() {
        let accuracy = GeometryAccuracy::default();

        let mut artwork = ArtworkDocument::new();
        let empty = artwork.push_block();
        let layer = artwork.push_layer(IrArtworkDocument {
            name: "F.Paste".to_string(),
            role: LayerRole::Paste,
            side: Side::Top,
            objects: Span::EMPTY,
            bbox: BBox::empty(),
            meta: LayerAttributes::default(),
        });
        artwork.push_object(
            layer,
            ArtworkObject {
                polarity: Polarity::Dark,
                order: Default::default(),
                geometry: ArtworkGeometry::Instance {
                    block: empty,
                    transform: Affine2::translation(Point::new(10.0, 20.0)),
                },
                bbox: BBox::empty(),
                meta: ObjectAttributes::default(),
            },
        );

        let gerber = lower_artwork_layer(&artwork, accuracy).expect("lower empty block");
        assert!(gerber.objects.is_empty());
        let contents = crate::write_layer(&gerber).expect("write empty layer");
        assert!(!contents.contains("%ABD"));
        assert_external_parser_accepts(&contents);
    }

    #[test]
    fn lowers_compound_region_holes_as_strict_additive_contours() {
        let accuracy = GeometryAccuracy::default();

        let mut artwork = ArtworkDocument::new();
        let layer_id = artwork.push_layer(IrArtworkDocument {
            name: "F.SilkS".to_string(),
            role: LayerRole::Legend,
            side: Side::None,
            objects: Span::EMPTY,
            bbox: BBox::empty(),
            meta: LayerAttributes::default(),
        });
        let path = artwork.push_path(
            Paint::Fill {
                rule: FillRule::EvenOdd,
            },
            vec![
                rect_payload(0.0, 0.0, 10.0, 10.0),
                rect_payload(2.0, 2.0, 8.0, 8.0),
            ],
        );
        artwork.push_object(
            layer_id,
            ArtworkObject {
                polarity: Polarity::Dark,
                order: Default::default(),
                geometry: ArtworkGeometry::Region { path },
                bbox: artwork.path_bbox(path),
                meta: ObjectAttributes::default(),
            },
        );

        let gerber = lower_artwork_layer(&artwork, accuracy).expect("lower artwork");

        assert!(!gerber.objects.is_empty());
        assert!(gerber.objects.iter().all(|object| {
            object.polarity == Polarity::Dark
                && matches!(&object.kind, ObjectKind::Region { contours } if contours.len() == 1)
        }));
        let contents = crate::write_layer(&gerber).unwrap();
        assert_external_parser_accepts(&contents);
        let parsed = crate::GerberX2::parse(&contents).unwrap();
        let geometry = crate::geometry::extract_document(&parsed, accuracy).unwrap();
        let summary =
            pcb_ir::dialects::artwork::compare::summarize(&geometry, Resolution::default())
                .unwrap();
        assert!(
            (summary.area_mm2 - 64.0).abs() < 0.001,
            "area was {}",
            summary.area_mm2
        );
    }

    #[test]
    fn pinched_even_odd_hole_snaps_to_simple_exact_contours() {
        for (offset, reflected) in [
            (0.0, false),
            (0.0, true),
            (0.00025, false),
            (0.00075, false),
            (0.00025, true),
        ] {
            let map =
                |x: f64, y: f64| Point::new((if reflected { -x } else { x }) + offset, y + offset);
            let payloads = [
                polygon_payload([
                    map(-5.0, -5.0),
                    map(5.0, -5.0),
                    map(5.0, 5.0),
                    map(-5.0, 5.0),
                ]),
                polygon_payload([
                    map(-2.0, -2.0),
                    map(2.0, -2.0),
                    map(0.0004, 0.0),
                    map(2.0, 2.0),
                    map(-2.0, 2.0),
                    map(-0.0004, 0.0),
                ]),
            ];
            let rings = prepare_on_grid(&payloads, FillRule::EvenOdd, GeometryAccuracy::default())
                .expect("decompose pinched hole");
            assert_strict_simple_rings(&rings);
            let snapped = |point: Point| {
                [
                    (point.x / GERBER_GEOMETRY_GRID_MM).round() * GERBER_GEOMETRY_GRID_MM,
                    (point.y / GERBER_GEOMETRY_GRID_MM).round() * GERBER_GEOMETRY_GRID_MM,
                ]
            };
            let snapped_hole = [
                map(-2.0, -2.0),
                map(2.0, -2.0),
                map(0.0004, 0.0),
                map(2.0, 2.0),
                map(-2.0, 2.0),
                map(-0.0004, 0.0),
            ]
            .into_iter()
            .map(snapped)
            .collect();
            let expected_area = 100.0 - region::ring_signed_area(&snapped_hole).abs();
            let area: f64 = rings
                .iter()
                .map(|ring| region::ring_signed_area(ring).abs())
                .sum();
            assert!(
                (area - expected_area).abs() < 1e-12,
                "snap must preserve the analytic material area: {area} versus {expected_area}"
            );
            let contours = rings.iter().map(lower_ring).collect();
            let layer = GerberLayer {
                objects: vec![WriterObject::dark(ObjectKind::Region { contours })],
                ..GerberLayer::default()
            };
            assert!((parsed_area(&layer) - expected_area).abs() < 1e-9);
        }
    }

    #[test]
    fn horizontal_cut_in_rounding_fixtures_emit_no_bridge_coordinates() {
        for source_y in [3.0004, 3.0006] {
            let payloads = [
                polygon_payload([
                    Point::new(0.0, 0.0),
                    Point::new(23.0, 0.0),
                    Point::new(23.0, 17.0),
                    Point::new(7.0, 17.0),
                ]),
                polygon_payload([
                    Point::new(9.0, source_y),
                    Point::new(15.0, 5.0),
                    Point::new(12.0, 9.0),
                ]),
            ];
            let rings =
                prepare_on_grid(&payloads, FillRule::EvenOdd, GeometryAccuracy::default()).unwrap();
            assert_strict_simple_rings(&rings);
            let contours: Vec<_> = rings.iter().map(lower_ring).collect();
            for contour in &contours {
                let edges: Vec<_> = contour
                    .segments
                    .iter()
                    .map(|segment| match segment {
                        ContourSegment::Line { start, end } => (*start, *end),
                        _ => panic!("expected snapped lines"),
                    })
                    .collect();
                assert!(
                    !edges.iter().any(|&(a, b)| edges.contains(&(b, a))),
                    "decomposition must not insert a doubled bridge"
                );
            }
            let layer = GerberLayer {
                objects: vec![WriterObject::dark(ObjectKind::Region { contours })],
                ..GerberLayer::default()
            };
            parsed_area(&layer);
        }
    }

    #[test]
    fn formerly_collapsing_cut_in_fixtures_are_all_serializable() {
        for (top_y, dx) in [(3.001, 0.0), (3.003, 0.0), (3.001, -0.001), (3.003, -0.001)] {
            let payloads = [
                polygon_payload([
                    Point::new(dx, 0.0),
                    Point::new(10.0 + dx, 0.0),
                    Point::new(10.0 + dx, top_y),
                    Point::new(0.001 + dx, top_y),
                ]),
                polygon_payload([
                    Point::new(0.001 + dx, 3.0),
                    Point::new(2.0 + dx, 2.0),
                    Point::new(2.0 + dx, 3.0),
                ]),
            ];
            let rings =
                prepare_on_grid(&payloads, FillRule::EvenOdd, GeometryAccuracy::default()).unwrap();
            assert_strict_simple_rings(&rings);
            let contours: Vec<_> = rings.iter().map(lower_ring).collect();
            let layer = GerberLayer {
                objects: vec![WriterObject::dark(ObjectKind::Region { contours })],
                ..GerberLayer::default()
            };
            parsed_area(&layer);
        }
    }

    #[test]
    fn compound_contour_aperture_is_additive_for_both_polarities() {
        for polarity in [Polarity::Dark, Polarity::Clear] {
            let mut artwork = ArtworkDocument::new();
            let layer = artwork.push_layer(IrArtworkDocument {
                name: "F.Cu".into(),
                role: LayerRole::Copper,
                side: Side::Top,
                objects: Span::EMPTY,
                bbox: BBox::empty(),
                meta: LayerAttributes::default(),
            });
            let base = artwork.push_path(
                Paint::Fill {
                    rule: FillRule::NonZero,
                },
                vec![rect_payload(-10.0, -10.0, 10.0, 10.0)],
            );
            artwork.push_object(
                layer,
                ArtworkObject {
                    polarity: Polarity::Dark,
                    order: Default::default(),
                    geometry: ArtworkGeometry::Region { path: base },
                    bbox: artwork.path_bbox(base),
                    meta: ObjectAttributes::default(),
                },
            );
            let loops = [
                (-5.0, -5.0, 5.0, 5.0),
                (-3.0, -3.0, 3.0, 3.0),
                (-1.0, -1.0, 1.0, 1.0),
            ];
            let mut contour = rect_payload(loops[0].0, loops[0].1, loops[0].2, loops[0].3);
            for &(x0, y0, x1, y1) in &loops[1..] {
                let ring = rect_payload(x0, y0, x1, y1);
                contour.cmds.extend(ring.cmds);
            }
            let aperture = artwork.push_aperture(Aperture::solid(ApertureShape::Contour {
                outline: contour,
                fill_rule: FillRule::EvenOdd,
            }));
            artwork.push_object(
                layer,
                ArtworkObject {
                    polarity,
                    order: PaintOrder {
                        stage: PaintStage::Overlay,
                    },
                    geometry: ArtworkGeometry::Flash {
                        aperture,
                        transform: Affine2::placement(Point::new(2.0, 1.0), 90.0, Mirror::Y, 1.0),
                    },
                    bbox: BBox::empty(),
                    meta: ObjectAttributes::default(),
                },
            );
            let gerber = lower_artwork_layer(&artwork, GeometryAccuracy::default()).unwrap();
            assert!(outlines(&gerber).len() > 1);
            let expected = if polarity == Polarity::Dark {
                400.0
            } else {
                332.0
            };
            assert!((parsed_area(&gerber) - expected).abs() < 0.001);
        }
    }

    #[test]
    fn oversized_non_collinear_outline_splits_code4_primitives() {
        let points: Vec<_> = (0..6001)
            .map(|i| {
                let angle = i as f64 * std::f64::consts::TAU / 6001.0;
                let radius = if i % 2 == 0 { 10.0 } else { 9.8 };
                Point::new(
                    (radius * angle.cos() * 1000.0).round() / 1000.0,
                    (radius * angle.sin() * 1000.0).round() / 1000.0,
                )
            })
            .collect();
        let outline = polygon_payload(points);
        let expected = region::ContourSet::from_contours(
            std::slice::from_ref(&outline),
            FillRule::EvenOdd,
            Resolution::default(),
        )
        .unwrap()
        .area();
        let mut artwork = ArtworkDocument::new();
        let aperture = artwork.push_aperture(Aperture::solid(ApertureShape::Contour {
            outline,
            fill_rule: FillRule::EvenOdd,
        }));
        let layer = artwork.push_layer(IrArtworkDocument {
            name: "F.Cu".into(),
            role: LayerRole::Copper,
            side: Side::Top,
            objects: Span::EMPTY,
            bbox: BBox::empty(),
            meta: LayerAttributes::default(),
        });
        artwork.push_object(
            layer,
            ArtworkObject {
                polarity: Polarity::Dark,
                order: Default::default(),
                geometry: ArtworkGeometry::Flash {
                    aperture,
                    transform: Affine2::IDENTITY,
                },
                bbox: BBox::empty(),
                meta: ObjectAttributes::default(),
            },
        );
        let gerber = lower_artwork_layer(&artwork, GeometryAccuracy::default()).unwrap();
        let outlines = outlines(&gerber);
        assert!(outlines.len() > 1);
        assert!(outlines.iter().all(|outline| outline.len() <= 5000));
        assert!((parsed_area(&gerber) - expected).abs() < 0.01);
    }

    #[test]
    fn deep_nested_even_odd_compound_regions_preserve_topology() {
        let accuracy = GeometryAccuracy::default();

        let mut artwork = ArtworkDocument::new();
        let layer_id = artwork.push_layer(IrArtworkDocument {
            name: "F.Cu".to_string(),
            role: LayerRole::Copper,
            side: Side::Top,
            objects: Span::EMPTY,
            bbox: BBox::empty(),
            meta: LayerAttributes::default(),
        });
        let path = artwork.push_path(
            Paint::Fill {
                rule: FillRule::EvenOdd,
            },
            vec![
                rect_payload(0.0, 0.0, 10.0, 10.0),
                rect_payload(1.0, 1.0, 9.0, 9.0),
                rect_payload(2.0, 2.0, 8.0, 8.0),
                rect_payload(3.0, 3.0, 7.0, 7.0),
            ],
        );
        artwork.push_object(
            layer_id,
            ArtworkObject {
                polarity: Polarity::Dark,
                order: Default::default(),
                geometry: ArtworkGeometry::Region { path },
                bbox: artwork.path_bbox(path),
                meta: ObjectAttributes::default(),
            },
        );

        let gerber = lower_artwork_layer(&artwork, accuracy).expect("lower artwork");

        assert_eq!(gerber.objects.len(), 4);
        assert!(
            gerber
                .objects
                .iter()
                .all(|object| object.polarity == Polarity::Dark
                    && matches!(&object.kind, ObjectKind::Region { contours } if contours.len() == 1))
        );
        let contents = crate::write_layer(&gerber).expect("write Gerber");
        assert_external_parser_accepts(&contents);
        let parsed = crate::GerberX2::parse(&contents).expect("parse Gerber");
        let geometry = crate::geometry::extract_document(&parsed, accuracy).unwrap();
        let summary =
            pcb_ir::dialects::artwork::compare::summarize(&geometry, Resolution::default())
                .unwrap();
        assert!(
            (summary.area_mm2 - 56.0).abs() < 0.001,
            "deep even-odd topology exported wrong area: {}",
            summary.area_mm2
        );
    }

    #[test]
    fn non_pad_copper_contours_lower_to_regions() {
        let accuracy = GeometryAccuracy::default();

        let mut artwork = ArtworkDocument::new();
        let layer_id = artwork.push_layer(IrArtworkDocument {
            name: "F.Cu".to_string(),
            role: LayerRole::Copper,
            side: Side::Top,
            objects: Span::EMPTY,
            bbox: BBox::empty(),
            meta: LayerAttributes::default(),
        });

        // One contour buffer with a material loop and a reverse-wound hole.
        let outer = [
            Point::new(0.0, 0.0),
            Point::new(10.0, 0.0),
            Point::new(10.0, 10.0),
            Point::new(0.0, 10.0),
        ];
        let hole = [
            Point::new(2.0, 2.0),
            Point::new(2.0, 8.0),
            Point::new(8.0, 8.0),
            Point::new(8.0, 2.0),
        ];
        let mut bbox = BBox::empty();
        let mut cmds = Vec::new();
        for ring in [outer, hole] {
            for (index, point) in ring.into_iter().enumerate() {
                bbox.include_point(point);
                cmds.push(if index == 0 {
                    PathCmd::move_to(point)
                } else {
                    PathCmd::line_to(point)
                });
            }
            cmds.push(PathCmd::close());
        }
        let contour = ContourBuf::from_parts(bbox, cmds);

        let path = artwork.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            vec![contour],
        );
        artwork.push_object(
            layer_id,
            ArtworkObject {
                polarity: Polarity::Dark,
                order: Default::default(),
                geometry: ArtworkGeometry::Region { path },
                bbox: artwork.path_bbox(path),
                meta: ObjectAttributes {
                    aperture_function: Some(vec!["Conductor".to_string()]),
                    ..ObjectAttributes::default()
                },
            },
        );

        let gerber = lower_artwork_layer(&artwork, accuracy).expect("lower artwork");
        let contents = crate::write_layer(&gerber).expect("write Gerber");
        assert_external_parser_accepts(&contents);
        assert_eq!(contents.matches("%ABD").count(), 0);
        assert_eq!(contents.matches("%AM").count(), 0);
        assert_eq!(contents.matches("G36*").count(), 2);
        assert!(contents.contains("%TA.AperFunction,Conductor*%"));
        let parsed = crate::GerberX2::parse(&contents).expect("parse Gerber");
        let geometry = crate::geometry::extract_document(&parsed, accuracy).unwrap();
        let summary =
            pcb_ir::dialects::artwork::compare::summarize(&geometry, Resolution::default())
                .unwrap();
        assert!(
            (summary.area_mm2 - 64.0).abs() < 0.01,
            "the hole ring must survive decomposition: {}",
            summary.area_mm2
        );
    }

    #[test]
    fn full_copper_balance_cells_remain_shared_flashes() {
        let accuracy = GeometryAccuracy::default();

        let mut artwork = ArtworkDocument::new();
        let layer_id = artwork.push_layer(IrArtworkDocument {
            name: "F.Cu".to_string(),
            role: LayerRole::Copper,
            side: Side::Top,
            objects: Span::EMPTY,
            bbox: BBox::empty(),
            meta: LayerAttributes::default(),
        });
        let aperture = artwork.push_aperture(Aperture::solid(ApertureShape::RoundedHex {
            radius: 1.0,
            corner_radius: 0.15,
            rotation_degrees: 0.0,
        }));
        artwork.push_object(
            layer_id,
            ArtworkObject {
                polarity: Polarity::Dark,
                order: Default::default(),
                geometry: ArtworkGeometry::Flash {
                    aperture,
                    transform: Affine2::translation(Point::new(4.0, 5.0)),
                },
                bbox: BBox::empty(),
                meta: ObjectAttributes {
                    aperture_function: Some(vec!["CopperBalancing".to_string()]),
                    ..ObjectAttributes::default()
                },
            },
        );

        let gerber = lower_artwork_layer(&artwork, accuracy).expect("lower balance cell");
        // The exact rounded hex flattens once into a concrete one-primitive
        // outline macro; legacy CAM importers never evaluate compound
        // parameterized macros per flash.
        assert_eq!(outlines(&gerber).len(), 1);
        assert!(matches!(gerber.objects[0].kind, ObjectKind::Flash { .. }));
        let contents = crate::write_layer(&gerber).expect("write balance cell");
        assert_external_parser_accepts(&contents);
        let parsed = crate::GerberX2::parse(&contents).expect("parse balance cell");
        let geometry = crate::geometry::extract_document(&parsed, accuracy).unwrap();
        let actual_area =
            pcb_ir::dialects::artwork::compare::summarize(&geometry, Resolution::default())
                .unwrap()
                .area_mm2;
        let expected_area = 3.0 * 3.0_f64.sqrt() / 2.0
            - (2.0 * 3.0_f64.sqrt() - std::f64::consts::PI) * 0.15_f64.powi(2);
        assert!(
            (actual_area - expected_area).abs() < 3e-3,
            "rounded-hex macro area {actual_area}, expected {expected_area}"
        );
    }

    #[test]
    fn zero_count_grids_emit_nothing() {
        let accuracy = GeometryAccuracy::default();

        let mut artwork = ArtworkDocument::new();
        let layer_id = artwork.push_layer(IrArtworkDocument {
            name: "F.Cu".to_string(),
            role: LayerRole::Copper,
            side: Side::Top,
            objects: Span::EMPTY,
            bbox: BBox::empty(),
            meta: LayerAttributes::default(),
        });
        let block = artwork.push_block();
        let aperture = artwork.push_aperture(Aperture::circle(1.0));
        artwork.push_block_object(
            block,
            ArtworkObject::new(
                Polarity::Dark,
                ArtworkGeometry::Flash {
                    aperture,
                    transform: Affine2::IDENTITY,
                },
            ),
        );
        artwork.push_object(
            layer_id,
            ArtworkObject::new(
                Polarity::Dark,
                ArtworkGeometry::GridInstance {
                    block,
                    transform: Affine2::IDENTITY,
                    repeat: pcb_ir::dialects::artwork::GridRepeat {
                        x_count: 0,
                        y_count: 3,
                        x_step: Point::new(5.0, 0.0),
                        y_step: Point::new(0.0, 5.0),
                    },
                },
            ),
        );

        let gerber = lower_artwork_layer(&artwork, accuracy).expect("lower empty grid");
        assert!(gerber.objects.is_empty());
    }

    #[test]
    fn contour_apertures_honor_even_odd_fill() {
        let accuracy = GeometryAccuracy::default();

        // Two same-winding nested loops: NonZero fills solid, EvenOdd carves
        // the inner loop out. The aperture's fill rule must decide.
        let mut artwork = ArtworkDocument::new();
        let layer_id = artwork.push_layer(IrArtworkDocument {
            name: "F.Cu".to_string(),
            role: LayerRole::Copper,
            side: Side::Top,
            objects: Span::EMPTY,
            bbox: BBox::empty(),
            meta: LayerAttributes::default(),
        });

        let outer = rect_payload(0.0, 0.0, 10.0, 10.0);
        let inner = rect_payload(2.0, 2.0, 8.0, 8.0);
        let mut bbox = outer.bbox;
        bbox.include_point(inner.bbox.min);
        bbox.include_point(inner.bbox.max);
        let mut cmds = outer.cmds.clone();
        cmds.extend(inner.cmds.iter().copied());
        let contour = ContourBuf::from_parts(bbox, cmds);

        let aperture = artwork.push_aperture(Aperture::solid(ApertureShape::Contour {
            outline: contour,
            fill_rule: FillRule::EvenOdd,
        }));
        artwork.push_object(
            layer_id,
            ArtworkObject {
                polarity: Polarity::Dark,
                order: Default::default(),
                geometry: ArtworkGeometry::Flash {
                    aperture,
                    transform: pcb_ir::geom::Affine2::translation(Point::new(20.0, 5.0)),
                },
                bbox: BBox {
                    min: Point::new(20.0, 5.0),
                    max: Point::new(30.0, 15.0),
                },
                meta: ObjectAttributes::default(),
            },
        );

        let gerber = lower_artwork_layer(&artwork, accuracy).expect("lower artwork");
        let contents = crate::write_layer(&gerber).expect("write Gerber");
        assert_external_parser_accepts(&contents);
        let parsed = crate::GerberX2::parse(&contents).expect("parse Gerber");
        let geometry = crate::geometry::extract_document(&parsed, accuracy).unwrap();
        let summary =
            pcb_ir::dialects::artwork::compare::summarize(&geometry, Resolution::default())
                .unwrap();
        assert!(
            (summary.area_mm2 - 64.0).abs() < 0.01,
            "even-odd fill must carve the nested loop out: {}",
            summary.area_mm2
        );
    }

    #[test]
    fn contour_apertures_normalize_material_winding() {
        let accuracy = GeometryAccuracy::default();

        // A clockwise-wound solitary loop is still material under NonZero;
        // winding normalization must not turn it into an exposure-off ring.
        let mut artwork = ArtworkDocument::new();
        let layer_id = artwork.push_layer(IrArtworkDocument {
            name: "F.Cu".to_string(),
            role: LayerRole::Copper,
            side: Side::Top,
            objects: Span::EMPTY,
            bbox: BBox::empty(),
            meta: LayerAttributes::default(),
        });

        let clockwise = [
            Point::new(0.0, 0.0),
            Point::new(0.0, 10.0),
            Point::new(10.0, 10.0),
            Point::new(10.0, 0.0),
        ];
        let mut bbox = BBox::empty();
        let mut cmds = Vec::new();
        for (index, point) in clockwise.into_iter().enumerate() {
            bbox.include_point(point);
            cmds.push(if index == 0 {
                PathCmd::move_to(point)
            } else {
                PathCmd::line_to(point)
            });
        }
        cmds.push(PathCmd::close());
        let contour = ContourBuf::from_parts(bbox, cmds);

        let aperture = artwork.push_aperture(Aperture::solid(ApertureShape::Contour {
            outline: contour,
            fill_rule: FillRule::NonZero,
        }));
        artwork.push_object(
            layer_id,
            ArtworkObject {
                polarity: Polarity::Dark,
                order: Default::default(),
                geometry: ArtworkGeometry::Flash {
                    aperture,
                    transform: pcb_ir::geom::Affine2::translation(Point::new(20.0, 5.0)),
                },
                bbox: BBox {
                    min: Point::new(20.0, 5.0),
                    max: Point::new(30.0, 15.0),
                },
                meta: ObjectAttributes::default(),
            },
        );

        let gerber = lower_artwork_layer(&artwork, accuracy).expect("lower artwork");
        let contents = crate::write_layer(&gerber).expect("write Gerber");
        assert_external_parser_accepts(&contents);
        let parsed = crate::GerberX2::parse(&contents).expect("parse Gerber");
        let geometry = crate::geometry::extract_document(&parsed, accuracy).unwrap();
        let summary =
            pcb_ir::dialects::artwork::compare::summarize(&geometry, Resolution::default())
                .unwrap();
        assert!(
            (summary.area_mm2 - 100.0).abs() < 0.01,
            "a clockwise material loop must keep its full area: {}",
            summary.area_mm2
        );
    }

    #[test]
    fn contour_apertures_survive_mirrored_bases() {
        let accuracy = GeometryAccuracy::default();

        // A mirrored basis reverses ring winding when it is baked into the
        // aperture outline; normalization happens after baking, so the
        // material must survive with its full area.
        let mut artwork = ArtworkDocument::new();
        let layer_id = artwork.push_layer(IrArtworkDocument {
            name: "F.Cu".to_string(),
            role: LayerRole::Copper,
            side: Side::Top,
            objects: Span::EMPTY,
            bbox: BBox::empty(),
            meta: LayerAttributes::default(),
        });

        let counter_clockwise = [
            Point::new(0.0, 0.0),
            Point::new(10.0, 0.0),
            Point::new(10.0, 10.0),
            Point::new(0.0, 10.0),
        ];
        let mut bbox = BBox::empty();
        let mut cmds = Vec::new();
        for (index, point) in counter_clockwise.into_iter().enumerate() {
            bbox.include_point(point);
            cmds.push(if index == 0 {
                PathCmd::move_to(point)
            } else {
                PathCmd::line_to(point)
            });
        }
        cmds.push(PathCmd::close());
        let contour = ContourBuf::from_parts(bbox, cmds);

        let aperture = artwork.push_aperture(Aperture::solid(ApertureShape::Contour {
            outline: contour,
            fill_rule: FillRule::NonZero,
        }));
        artwork.push_object(
            layer_id,
            ArtworkObject {
                polarity: Polarity::Dark,
                order: Default::default(),
                geometry: ArtworkGeometry::Flash {
                    aperture,
                    transform: pcb_ir::geom::Affine2 {
                        m00: -1.0,
                        m01: 0.0,
                        m02: 30.0,
                        m10: 0.0,
                        m11: 1.0,
                        m12: 5.0,
                    },
                },
                bbox: BBox {
                    min: Point::new(20.0, 5.0),
                    max: Point::new(30.0, 15.0),
                },
                meta: ObjectAttributes::default(),
            },
        );

        let gerber = lower_artwork_layer(&artwork, accuracy).expect("lower artwork");
        let contents = crate::write_layer(&gerber).expect("write Gerber");
        assert_external_parser_accepts(&contents);
        let parsed = crate::GerberX2::parse(&contents).expect("parse Gerber");
        let geometry = crate::geometry::extract_document(&parsed, accuracy).unwrap();
        let summary =
            pcb_ir::dialects::artwork::compare::summarize(&geometry, Resolution::default())
                .unwrap();
        assert!(
            (summary.area_mm2 - 100.0).abs() < 0.01,
            "a mirrored basis must not invert the loop into a hole: {}",
            summary.area_mm2
        );
    }

    #[test]
    fn lowers_single_self_cut_even_odd_region_before_emitting_gerber() {
        let accuracy = GeometryAccuracy::default();

        let mut artwork = ArtworkDocument::new();
        let layer_id = artwork.push_layer(IrArtworkDocument {
            name: "F.Cu".to_string(),
            role: LayerRole::Copper,
            side: Side::Top,
            objects: Span::EMPTY,
            bbox: BBox::empty(),
            meta: LayerAttributes::default(),
        });
        let path = artwork.push_path(
            Paint::Fill {
                rule: FillRule::EvenOdd,
            },
            vec![self_cut_donut_payload()],
        );
        artwork.push_object(
            layer_id,
            ArtworkObject {
                polarity: Polarity::Dark,
                order: Default::default(),
                geometry: ArtworkGeometry::Region { path },
                bbox: artwork.path_bbox(path),
                meta: ObjectAttributes::default(),
            },
        );

        let gerber = lower_artwork_layer(&artwork, accuracy).expect("lower artwork");

        assert_eq!(gerber.objects[0].polarity, Polarity::Dark);
        assert!(
            !gerber.objects.is_empty()
                && gerber.objects.iter().all(|object| {
                    matches!(&object.kind, ObjectKind::Region { contours } if contours.len() == 1)
                }),
            "fallback regions must be emitted as spec-compliant single-contour objects"
        );
    }

    #[test]
    fn local_compound_region_holes_do_not_clear_prior_base_copper() {
        let accuracy = GeometryAccuracy::default();

        let mut artwork = ArtworkDocument::new();
        let layer_id = artwork.push_layer(IrArtworkDocument {
            name: "F.Cu".to_string(),
            role: LayerRole::Copper,
            side: Side::Top,
            objects: Span::EMPTY,
            bbox: BBox::empty(),
            meta: LayerAttributes::default(),
        });
        let base = artwork.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            vec![rect_payload(0.0, 0.0, 10.0, 10.0)],
        );
        artwork.push_object(
            layer_id,
            ArtworkObject {
                polarity: Polarity::Dark,
                order: PaintOrder {
                    stage: PaintStage::Base,
                },
                geometry: ArtworkGeometry::Region { path: base },
                bbox: artwork.path_bbox(base),
                meta: ObjectAttributes::default(),
            },
        );
        let donut = artwork.push_path(
            Paint::Fill {
                rule: FillRule::EvenOdd,
            },
            vec![
                rect_payload(2.0, 2.0, 8.0, 8.0),
                rect_payload(4.0, 4.0, 6.0, 6.0),
            ],
        );
        artwork.push_object(
            layer_id,
            ArtworkObject {
                polarity: Polarity::Dark,
                order: PaintOrder {
                    stage: PaintStage::Base,
                },
                geometry: ArtworkGeometry::Region { path: donut },
                bbox: artwork.path_bbox(donut),
                meta: ObjectAttributes::default(),
            },
        );

        let gerber = lower_artwork_layer(&artwork, accuracy).expect("lower artwork");

        assert!(
            gerber
                .objects
                .iter()
                .all(|object| object.polarity == Polarity::Dark),
            "local holes must not lower to layer-global clear polarity"
        );
        let contents = crate::write_layer(&gerber).expect("write Gerber");
        assert_external_parser_accepts(&contents);
        let parsed = crate::GerberX2::parse(&contents).expect("parse Gerber");
        let geometry = crate::geometry::extract_document(&parsed, accuracy).unwrap();
        let summary =
            pcb_ir::dialects::artwork::compare::summarize(&geometry, Resolution::default())
                .unwrap();
        assert!(
            (summary.area_mm2 - 100.0).abs() < 0.001,
            "donut hole cleared prior base copper; area was {}",
            summary.area_mm2
        );
    }

    #[test]
    fn places_compound_regions_before_overlay_objects() {
        let accuracy = GeometryAccuracy::default();

        let mut artwork = ArtworkDocument::new();
        let layer_id = artwork.push_layer(IrArtworkDocument {
            name: "F.Cu".to_string(),
            role: LayerRole::Copper,
            side: Side::Top,
            objects: Span::EMPTY,
            bbox: BBox::empty(),
            meta: LayerAttributes::default(),
        });
        let pour = artwork.push_path(
            Paint::Fill {
                rule: FillRule::EvenOdd,
            },
            vec![
                rect_payload(0.0, 0.0, 10.0, 10.0),
                rect_payload(2.0, 2.0, 8.0, 8.0),
            ],
        );
        artwork.push_object(
            layer_id,
            ArtworkObject {
                polarity: Polarity::Dark,
                order: PaintOrder {
                    stage: PaintStage::Base,
                },
                geometry: ArtworkGeometry::Region { path: pour },
                bbox: artwork.path_bbox(pour),
                meta: ObjectAttributes::default(),
            },
        );
        let trace = artwork.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            vec![
                rect_payload(11.0, 0.0, 12.0, 1.0),
                rect_payload(11.0, 2.0, 12.0, 3.0),
            ],
        );
        artwork.push_object(
            layer_id,
            ArtworkObject {
                polarity: Polarity::Dark,
                order: PaintOrder {
                    stage: PaintStage::Overlay,
                },
                geometry: ArtworkGeometry::Region { path: trace },
                bbox: artwork.path_bbox(trace),
                meta: ObjectAttributes {
                    net: Some("TRACE".to_string()),
                    ..ObjectAttributes::default()
                },
            },
        );

        let gerber = lower_artwork_layer(&artwork, accuracy).expect("lower artwork");

        let pour_index = gerber
            .objects
            .iter()
            .position(|object| {
                matches!(
                    &object.kind,
                    ObjectKind::Region { contours } if contours.len() == 1
                ) && object.polarity == Polarity::Dark
            })
            .expect("base pour should emit a dark region");
        let trace_index = gerber
            .objects
            .iter()
            .position(|object| {
                object
                    .attributes
                    .iter()
                    .any(|attr| attr.name == ".N" && attr.fields == ["TRACE"])
            })
            .expect("dark-only multi-contour trace should keep its net attribute");

        assert!(pour_index < trace_index);
        assert!(
            gerber.objects[trace_index..]
                .iter()
                .filter(|object| {
                    object
                        .attributes
                        .iter()
                        .any(|attr| attr.name == ".N" && attr.fields == ["TRACE"])
                })
                .all(|object| object.polarity == Polarity::Dark)
        );
        assert!(
            gerber
                .objects
                .iter()
                .all(|object| object.polarity == Polarity::Dark),
            "positive local holes must not become clear-polarity objects"
        );
    }

    fn rect_payload(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> ContourBuf {
        polygon_payload([
            Point::new(min_x, min_y),
            Point::new(max_x, min_y),
            Point::new(max_x, max_y),
            Point::new(min_x, max_y),
        ])
    }

    fn clockwise_rect_payload(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> ContourBuf {
        polygon_payload([
            Point::new(min_x, min_y),
            Point::new(min_x, max_y),
            Point::new(max_x, max_y),
            Point::new(max_x, min_y),
        ])
    }

    fn circle_payload(center: Point, radius: f64) -> ContourBuf {
        ContourBuf::new(vec![
            PathCmd::move_to(Point::new(center.x + radius, center.y)),
            PathCmd::arc_to(Point::new(center.x - radius, center.y), center, false),
            PathCmd::arc_to(Point::new(center.x + radius, center.y), center, false),
            PathCmd::close(),
        ])
    }

    fn polygon_payload(points: impl IntoIterator<Item = Point>) -> ContourBuf {
        let mut bbox = BBox::empty();
        let mut cmds = Vec::new();
        for (index, point) in points.into_iter().enumerate() {
            bbox.include_point(point);
            cmds.push(if index == 0 {
                PathCmd::move_to(point)
            } else {
                PathCmd::line_to(point)
            });
        }
        cmds.push(PathCmd::close());
        ContourBuf::from_parts(bbox, cmds)
    }

    fn self_cut_donut_payload() -> ContourBuf {
        let points = [
            Point::new(0.0, 0.0),
            Point::new(4.0, 0.0),
            Point::new(4.0, 4.0),
            Point::new(0.0, 4.0),
            Point::new(0.0, 0.0),
            Point::new(1.0, 1.0),
            Point::new(3.0, 1.0),
            Point::new(3.0, 3.0),
            Point::new(1.0, 3.0),
            Point::new(1.0, 1.0),
            Point::new(0.0, 0.0),
        ];
        let mut bbox = BBox::empty();
        let mut cmds = Vec::new();
        for (index, point) in points.into_iter().enumerate() {
            bbox.include_point(point);
            cmds.push(if index == 0 {
                PathCmd::move_to(point)
            } else {
                PathCmd::line_to(point)
            });
        }
        cmds.push(PathCmd::close());
        ContourBuf::from_parts(bbox, cmds)
    }
}
