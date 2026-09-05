//! Lower pcb-ir artwork into an idiomatic Gerber X2 layer.
//!
//! This is the write-side mirror of [`crate::geometry::extract_document`]:
//! any artwork document annotated with [`LayerAttributes`]/[`ObjectAttributes`]
//! can be emitted as a Gerber file, regardless of which source dialect
//! produced it.

use pcb_ir::geom::GeometryAccuracy;
use std::collections::HashMap;

use crate::{
    AttributeValue, Contour, ContourSegment, GerberError, GerberLayer, ObjectKind,
    Point as GerberPoint, Result, WriterAperture, WriterApertureMacro, WriterApertureTemplate,
    WriterApertureTransform, WriterMacroExpression, WriterMacroPrimitive, WriterObject,
    sanitize_attribute_field,
};
use pcb_ir::dialects::artwork::{Aperture, ApertureShape, Geometry as ArtworkGeometry, PaintStage};
use pcb_ir::geom::path::{self as geom_path, ContourBuf, PathCmd};
use pcb_ir::geom::region::{self, Ring};
use pcb_ir::geom::{Affine2, FillRule, Point, Polarity, Segment, StrokePatternMark};

const GERBER_GEOMETRY_GRID_MM: f64 = 0.001;

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
    /// Lower flashed occurrences as `G36` regions so non-pad copper never
    /// masquerades as pads.
    pub lower_flashes_to_regions: bool,
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
/// Source flashes survive as flashes unless explicit copper feature semantics
/// require region output; block instances are expanded.
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
    ArtworkDocument {
        apertures: doc.apertures,
        blocks: doc
            .blocks
            .into_iter()
            .map(|block| pcb_ir::dialects::artwork::Block {
                objects: block
                    .objects
                    .into_iter()
                    .map(|object| pcb_ir::dialects::artwork::Object {
                        polarity: object.polarity,
                        order: object.order,
                        geometry: object.geometry,
                        bbox: object.bbox,
                        meta: object_attributes(gerber, &object.meta),
                    })
                    .collect(),
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
        objects: doc
            .objects
            .into_iter()
            .map(|object| pcb_ir::dialects::artwork::Object {
                polarity: object.polarity,
                order: object.order,
                geometry: object.geometry,
                bbox: object.bbox,
                meta: object_attributes(gerber, &object.meta),
            })
            .collect(),
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
        lower_flashes_to_regions: false,
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
    let mut layer = pcb_ir::dialects::artwork::expand_instances_preserving_grids(layer, accuracy)?;
    pcb_ir::dialects::artwork::legalize::legalize_for_jlcpcb(&mut layer, accuracy)?;
    let mut apertures = ApertureTable::default();
    let mut plan = GerberPlan::default();
    let layer_attributes = layer
        .layers
        .first()
        .map(|layer| layer.meta.clone())
        .unwrap_or_default();

    // Expansion leaves only primitives and grids of primitive-only blocks.
    for object in &layer.objects {
        match object.geometry {
            ArtworkGeometry::GridInstance {
                block,
                transform,
                repeat,
            } => {
                lower_grid_objects(
                    &layer,
                    block,
                    transform,
                    repeat,
                    object.polarity,
                    &mut apertures,
                    &mut plan,
                    accuracy,
                )?;
            }
            _ => {
                let objects = lower_artwork_object(
                    &layer,
                    object,
                    Affine2::IDENTITY,
                    object.polarity,
                    &mut apertures,
                    accuracy,
                )?;
                plan.push_group(object.order.stage, object.polarity, objects);
            }
        }
    }
    let objects = plan.into_ordered_objects();

    let (aperture_list, aperture_macros) = apertures.into_parts();
    Ok(GerberLayer {
        file_attributes: lower_layer_attributes(&layer_attributes),
        apertures: aperture_list,
        aperture_macros,
        objects,
        ..GerberLayer::default()
    })
}

#[allow(clippy::too_many_arguments)]
fn lower_grid_objects(
    layer: &ArtworkDocument,
    block: u32,
    placement: Affine2,
    grid: pcb_ir::dialects::artwork::GridRepeat,
    polarity: Polarity,
    apertures: &mut ApertureTable,
    plan: &mut GerberPlan,
    accuracy: GeometryAccuracy,
) -> Result<()> {
    if let Some((base, step_repeat)) = gerber_step_repeat(placement, grid) {
        let repeat =
            (step_repeat.x_repeats > 1 || step_repeat.y_repeats > 1).then_some(step_repeat);
        lower_grid_occurrence(
            layer, block, base, polarity, repeat, apertures, plan, accuracy,
        )
    } else {
        for offset in grid.offsets() {
            let occurrence = Affine2 {
                m02: placement.m02 + offset.x,
                m12: placement.m12 + offset.y,
                ..placement
            };
            lower_grid_occurrence(
                layer, block, occurrence, polarity, None, apertures, plan, accuracy,
            )?;
        }
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn lower_grid_occurrence(
    layer: &ArtworkDocument,
    block: u32,
    base: Affine2,
    polarity: Polarity,
    repeat: Option<crate::StepRepeat>,
    apertures: &mut ApertureTable,
    plan: &mut GerberPlan,
    accuracy: GeometryAccuracy,
) -> Result<()> {
    for child in &layer.blocks[block as usize].objects {
        let child_polarity = polarity.compose(child.polarity);
        let mut objects =
            lower_artwork_object(layer, child, base, child_polarity, apertures, accuracy)?;
        for object in &mut objects {
            object.repeat = repeat;
        }
        plan.push_group(child.order.stage, child_polarity, objects);
    }
    Ok(())
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
    let aperture_attributes = lower_aperture_attributes(&object.meta);
    let mut objects = Vec::new();
    match object.geometry {
        ArtworkGeometry::Region { path } => {
            objects.extend(lower_region_objects(
                layer,
                path,
                transform,
                polarity,
                &aperture_attributes,
                &attributes,
                accuracy,
            )?);
        }
        ArtworkGeometry::Stroke { path } => {
            let artwork_path = &layer.arena.paths[path as usize];
            let aperture_function = object.meta.aperture_function.as_deref().unwrap_or_default();
            let region_aperture_attributes = lower_aperture_function(aperture_function);
            let stroke = artwork_path.stroke().ok_or_else(|| {
                GerberError::InvalidStructure(
                    "artwork stroke geometry references a path without stroke paint".to_string(),
                )
            })?;
            let stroke_width = stroke.width * transform.m00.hypot(transform.m10);
            let aperture = apertures.circle(stroke_width, aperture_function)?;
            for contour in layer
                .arena
                .path_contours(artwork_path)
                .into_iter()
                .map(|contour| contour.transformed(transform, accuracy))
            {
                let contour = contour?;
                let segments = contour_segments(&contour.cmds);
                for mark in
                    pcb_ir::geom::stroke_pattern_marks(&segments, stroke.pattern, stroke_width)
                {
                    match mark {
                        StrokePatternMark::Dash(segments) => {
                            objects.extend(segments.into_iter().map(|segment| WriterObject {
                                kind: lower_stroke_segment(segment, aperture),
                                polarity,
                                repeat: None,
                                aperture_transform: WriterApertureTransform::default(),
                                aperture_attributes: Vec::new(),
                                attributes: attributes.clone(),
                            }));
                        }
                        StrokePatternMark::Dot(at) => {
                            if object.meta.lower_flashes_to_regions {
                                objects.extend(lower_aperture_as_regions(
                                    &Aperture::circle(stroke_width),
                                    Affine2::translation(at),
                                    polarity,
                                    &region_aperture_attributes,
                                    &attributes,
                                    accuracy,
                                )?);
                            } else {
                                objects.push(WriterObject {
                                    kind: ObjectKind::Flash {
                                        at: lower_point(at),
                                        aperture,
                                    },
                                    polarity,
                                    repeat: None,
                                    aperture_transform: WriterApertureTransform::default(),
                                    aperture_attributes: Vec::new(),
                                    attributes: attributes.clone(),
                                });
                            }
                        }
                    }
                }
            }
        }
        ArtworkGeometry::Flash {
            aperture,
            transform: placement,
        } => {
            let mut transform = transform.concat(placement);
            let mut artwork_aperture =
                layer
                    .apertures
                    .get(aperture as usize)
                    .cloned()
                    .ok_or_else(|| {
                        GerberError::InvalidStructure(format!(
                            "artwork flash references missing aperture {aperture}"
                        ))
                    })?;
            if !transform.is_translation() {
                let basis = Affine2 {
                    m02: 0.0,
                    m12: 0.0,
                    ..transform
                };
                artwork_aperture = pcb_ir::dialects::artwork::legalize::bake_aperture_basis(
                    &artwork_aperture,
                    basis,
                    accuracy,
                )?;
                transform = Affine2::translation(Point::new(transform.m02, transform.m12));
            }
            let aperture_function = object.meta.aperture_function.as_deref().unwrap_or_default();
            let region_aperture_attributes = lower_aperture_function(aperture_function);
            if object.meta.lower_flashes_to_regions {
                return lower_aperture_as_regions(
                    &artwork_aperture,
                    transform,
                    polarity,
                    &region_aperture_attributes,
                    &attributes,
                    accuracy,
                );
            }
            let aperture =
                apertures.artwork_aperture(artwork_aperture, aperture_function, accuracy)?;
            objects.push(WriterObject {
                kind: ObjectKind::Flash {
                    at: lower_point(Point::new(transform.m02, transform.m12)),
                    aperture,
                },
                polarity,
                repeat: None,
                aperture_transform: WriterApertureTransform::default(),
                aperture_attributes: Vec::new(),
                attributes,
            });
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
        Segment::Cubic { .. } => unreachable!("contour_segments flattens cubics"),
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
    next_code: i32,
    by_key: HashMap<ApertureKey, i32>,
    apertures: Vec<WriterAperture>,
    aperture_macros: Vec<WriterApertureMacro>,
    roundrect_macro_defined: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ApertureKey {
    template: ApertureTemplateKey,
    function: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ApertureTemplateKey {
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
    RoundRect {
        width_nm: i64,
        height_nm: i64,
        radius_nm: i64,
    },
    Contour(Vec<Vec<(i64, i64)>>),
}

impl ApertureTable {
    fn circle(&mut self, diameter: f64, function: &[String]) -> Result<i32> {
        self.circle_with_hole(diameter, None, function)
    }

    fn circle_with_hole(
        &mut self,
        diameter: f64,
        hole_diameter: Option<f64>,
        function: &[String],
    ) -> Result<i32> {
        if diameter <= 0.0 {
            return Err(GerberError::InvalidStructure(format!(
                "cannot export non-positive Gerber stroke aperture diameter {diameter}"
            )));
        }
        self.define(
            ApertureTemplateKey::Circle {
                diameter_nm: quantize_mm(diameter),
                hole_nm: quantize_hole(hole_diameter),
            },
            WriterApertureTemplate::Circle {
                diameter,
                hole_diameter,
            },
            function,
        )
    }

    fn artwork_aperture(
        &mut self,
        aperture: Aperture,
        function: &[String],
        accuracy: GeometryAccuracy,
    ) -> Result<i32> {
        let hole_diameter = (aperture.hole_diameter > 0.0).then_some(aperture.hole_diameter);
        match aperture.shape {
            ApertureShape::Contour { outline, fill_rule } => {
                // Resolve the buffer's raw loops under the aperture's fill
                // rule so winding is canonical: each shape is an outer ring
                // followed by its holes, wound opposite. Larger shapes paint
                // first so an island inside a hole survives the hole's erase.
                let mut shapes = region::simplify_shapes_on_grid(
                    region::ContourSet::from_contours(
                        std::slice::from_ref(&outline),
                        fill_rule,
                        0.0,
                        accuracy,
                    )?
                    .rings,
                    fill_rule,
                    GERBER_GEOMETRY_GRID_MM,
                );
                shapes.sort_by(|a, b| {
                    let area = |shape: &region::Shape| {
                        shape
                            .first()
                            .map_or(0.0, |ring| region::ring_signed_area(ring).abs())
                    };
                    area(b).total_cmp(&area(a))
                });
                let rings = shapes.into_iter().flatten().collect::<Vec<_>>();
                if rings.is_empty() {
                    return Err(GerberError::InvalidStructure(
                        "cannot export an empty contour aperture".to_string(),
                    ));
                }
                let key = rings
                    .iter()
                    .map(|ring| {
                        ring.iter()
                            .map(|[x, y]| (quantize_mm(*x), quantize_mm(*y)))
                            .collect::<Vec<_>>()
                    })
                    .collect::<Vec<_>>();
                if let Some(code) = self.by_key.get(&ApertureKey {
                    template: ApertureTemplateKey::Contour(key.clone()),
                    function: function.to_vec(),
                }) {
                    return Ok(*code);
                }
                let code = self.outline_macro(&rings, function)?;
                self.by_key.insert(
                    ApertureKey {
                        template: ApertureTemplateKey::Contour(key),
                        function: function.to_vec(),
                    },
                    code,
                );
                Ok(code)
            }
            ApertureShape::Circle { diameter } => {
                self.circle_with_hole(diameter, hole_diameter, function)
            }
            ApertureShape::Rectangle { width, height } => {
                if width <= 0.0 || height <= 0.0 {
                    return Err(GerberError::InvalidStructure(format!(
                        "cannot export non-positive Gerber rectangle aperture {width} x {height}"
                    )));
                }
                self.define(
                    ApertureTemplateKey::Rectangle {
                        width_nm: quantize_mm(width),
                        height_nm: quantize_mm(height),
                        hole_nm: quantize_hole(hole_diameter),
                    },
                    WriterApertureTemplate::Rectangle {
                        width,
                        height,
                        hole_diameter,
                    },
                    function,
                )
            }
            ApertureShape::Obround { width, height } => {
                if width <= 0.0 || height <= 0.0 {
                    return Err(GerberError::InvalidStructure(format!(
                        "cannot export non-positive Gerber obround aperture {width} x {height}"
                    )));
                }
                self.define(
                    ApertureTemplateKey::Obround {
                        width_nm: quantize_mm(width),
                        height_nm: quantize_mm(height),
                        hole_nm: quantize_hole(hole_diameter),
                    },
                    WriterApertureTemplate::Obround {
                        width,
                        height,
                        hole_diameter,
                    },
                    function,
                )
            }
            ApertureShape::Polygon {
                diameter,
                vertices,
                rotation_degrees,
            } => {
                if diameter <= 0.0 {
                    return Err(GerberError::InvalidStructure(format!(
                        "cannot export non-positive Gerber polygon aperture diameter {diameter}"
                    )));
                }
                self.define(
                    ApertureTemplateKey::Polygon {
                        diameter_nm: quantize_mm(diameter),
                        vertices,
                        rotation_microdeg: quantize_mm(rotation_degrees),
                        hole_nm: quantize_hole(hole_diameter),
                    },
                    WriterApertureTemplate::Polygon {
                        outer_diameter: diameter,
                        vertices: vertices as i32,
                        rotation_degrees: Some(rotation_degrees),
                        hole_diameter,
                    },
                    function,
                )
            }
            ApertureShape::RoundRect {
                width,
                height,
                radius,
            } => {
                if width <= 0.0 || height <= 0.0 || radius <= 0.0 {
                    return Err(GerberError::InvalidStructure(format!(
                        "cannot export non-positive Gerber rounded-rectangle aperture \
                         {width} x {height} r {radius}"
                    )));
                }
                if hole_diameter.is_some() {
                    return Err(GerberError::InvalidStructure(
                        "cannot export a Gerber rounded-rectangle aperture with a hole".to_string(),
                    ));
                }
                // An oversized radius images clamped, matching how
                // `shapes::rounded_rect` flattens the same aperture; past the
                // limit the macro's rectangle terms would go negative.
                let radius = radius.min(width / 2.0).min(height / 2.0);
                if !self.roundrect_macro_defined {
                    self.aperture_macros.push(roundrect_macro());
                    self.roundrect_macro_defined = true;
                }
                self.define(
                    ApertureTemplateKey::RoundRect {
                        width_nm: quantize_mm(width),
                        height_nm: quantize_mm(height),
                        radius_nm: quantize_mm(radius),
                    },
                    WriterApertureTemplate::Macro {
                        name: ROUNDRECT_MACRO_NAME.to_string(),
                        parameters: vec![width, height, radius],
                    },
                    function,
                )
            }
            ApertureShape::RoundedHex {
                radius,
                corner_radius,
                rotation_degrees,
            } => {
                if hole_diameter.is_some() {
                    return Err(GerberError::InvalidStructure(
                        "cannot export a Gerber rounded-hex aperture with a hole".to_string(),
                    ));
                }
                // Flatten the exact shape once into a concrete one-primitive
                // outline; legacy CAM importers evaluate compound macros per
                // flash.
                let outline =
                    pcb_ir::geom::shapes::rounded_hexagon(radius, corner_radius, rotation_degrees)
                        .ok_or_else(|| {
                            GerberError::InvalidStructure(format!(
                                "cannot export invalid rounded-hex aperture r {radius}, corner r \
                                 {corner_radius}, rotation {rotation_degrees}"
                            ))
                        })?;
                self.artwork_aperture(
                    Aperture::solid(ApertureShape::Contour {
                        outline,
                        fill_rule: FillRule::NonZero,
                    }),
                    function,
                    accuracy,
                )
            }
        }
    }

    /// Define a one-off macro aperture filling the given closed outline,
    /// expressed relative to the flash origin.
    /// One code-4 outline primitive per ring: material rings expose, hole
    /// rings (negative winding) erase what earlier primitives painted.
    fn outline_macro(&mut self, rings: &[Ring], function: &[String]) -> Result<i32> {
        let attributes = (!function.is_empty())
            .then(|| AttributeValue::new(".AperFunction", function.iter().cloned()))
            .into_iter()
            .collect();
        self.outline_macro_with_attributes(rings, attributes)
    }

    fn outline_macro_with_attributes(
        &mut self,
        rings: &[Ring],
        attributes: Vec<AttributeValue>,
    ) -> Result<i32> {
        let name = format!("REPEAT{}", self.aperture_macros.len());
        let mut primitives = Vec::with_capacity(rings.len());
        for outline in rings {
            if outline.len() < 3 {
                return Err(GerberError::InvalidStructure(
                    "cannot export a Gerber outline macro with fewer than three vertices"
                        .to_string(),
                ));
            }
            let exposure = if region::ring_signed_area(outline) < 0.0 {
                0.0
            } else {
                1.0
            };
            let mut parameters = Vec::with_capacity(2 * outline.len() + 5);
            parameters.push(WriterMacroExpression::Number(exposure));
            parameters.push(WriterMacroExpression::Number(outline.len() as f64));
            for [x, y] in outline.iter().chain(std::iter::once(&outline[0])) {
                parameters.push(WriterMacroExpression::Number(*x));
                parameters.push(WriterMacroExpression::Number(*y));
            }
            parameters.push(WriterMacroExpression::Number(0.0));
            primitives.push(WriterMacroPrimitive::Shape {
                code: 4,
                parameters,
            });
        }
        self.aperture_macros.push(WriterApertureMacro {
            name: name.clone(),
            primitives,
        });
        let code = self.allocate_code();
        self.apertures.push(WriterAperture {
            code,
            template: WriterApertureTemplate::Macro {
                name,
                parameters: Vec::new(),
            },
            attributes,
        });
        Ok(code)
    }

    fn define(
        &mut self,
        template_key: ApertureTemplateKey,
        template: WriterApertureTemplate,
        function: &[String],
    ) -> Result<i32> {
        let key = ApertureKey {
            template: template_key,
            function: function.to_vec(),
        };
        if let Some(code) = self.by_key.get(&key) {
            return Ok(*code);
        }
        let code = self.allocate_code();
        self.by_key.insert(key, code);
        self.apertures.push(WriterAperture {
            code,
            template,
            attributes: (!function.is_empty())
                .then(|| AttributeValue::new(".AperFunction", function.iter().cloned()))
                .into_iter()
                .collect(),
        });
        Ok(code)
    }

    fn allocate_code(&mut self) -> i32 {
        if self.next_code == 0 {
            self.next_code = 10;
        } else {
            self.next_code += 1;
        }
        self.next_code
    }

    fn into_parts(self) -> (Vec<WriterAperture>, Vec<WriterApertureMacro>) {
        (self.apertures, self.aperture_macros)
    }
}

const ROUNDRECT_MACRO_NAME: &str = "RoundedRect";

/// The shared parameterized rounded-rectangle macro: two centered rectangles
/// leaving the corner insets uncovered, plus one circle per corner. Parameters
/// are `$1` width, `$2` height, `$3` corner radius; at the obround and circle
/// degeneracies a rectangle collapses to zero area and drops out.
fn roundrect_macro() -> WriterApertureMacro {
    use WriterMacroExpression as Expression;
    let number = |value: f64| Expression::Number(value);
    let variable = |index: usize| Expression::Variable(index);
    let subtract =
        |left: Expression, right: Expression| Expression::Subtract(Box::new(left), Box::new(right));
    let multiply =
        |left: Expression, right: Expression| Expression::Multiply(Box::new(left), Box::new(right));
    let divide =
        |left: Expression, right: Expression| Expression::Divide(Box::new(left), Box::new(right));
    // `$n/2-$3` and its negation `$3-$n/2`: the corner-circle center offset.
    let inset = |axis: usize, positive: bool| {
        if positive {
            subtract(divide(variable(axis), number(2.0)), variable(3))
        } else {
            subtract(variable(3), divide(variable(axis), number(2.0)))
        }
    };

    let mut primitives = vec![
        WriterMacroPrimitive::Shape {
            code: 21,
            parameters: vec![
                number(1.0),
                variable(1),
                subtract(variable(2), multiply(number(2.0), variable(3))),
                number(0.0),
                number(0.0),
                number(0.0),
            ],
        },
        WriterMacroPrimitive::Shape {
            code: 21,
            parameters: vec![
                number(1.0),
                subtract(variable(1), multiply(number(2.0), variable(3))),
                variable(2),
                number(0.0),
                number(0.0),
                number(0.0),
            ],
        },
    ];
    primitives.extend(
        [(true, true), (false, true), (true, false), (false, false)].map(|(right, top)| {
            WriterMacroPrimitive::Shape {
                code: 1,
                parameters: vec![
                    number(1.0),
                    multiply(number(2.0), variable(3)),
                    inset(1, right),
                    inset(2, top),
                ],
            }
        }),
    );
    WriterApertureMacro {
        name: ROUNDRECT_MACRO_NAME.to_string(),
        primitives,
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
    lower_contours_as_regions(
        layer.arena.path_contours(artwork_path),
        artwork_path.fill_rule().unwrap_or(FillRule::NonZero),
        transform,
        polarity,
        aperture_attributes,
        attributes,
        accuracy,
    )
}

fn lower_aperture_as_regions(
    aperture: &Aperture,
    transform: Affine2,
    polarity: Polarity,
    aperture_attributes: &[AttributeValue],
    attributes: &[AttributeValue],
    accuracy: GeometryAccuracy,
) -> Result<Vec<WriterObject>> {
    lower_contours_as_regions(
        aperture.contours(),
        aperture.fill_rule(),
        transform,
        polarity,
        aperture_attributes,
        attributes,
        accuracy,
    )
}

fn lower_contours_as_regions(
    contours: impl IntoIterator<Item = ContourBuf>,
    fill_rule: FillRule,
    transform: Affine2,
    polarity: Polarity,
    aperture_attributes: &[AttributeValue],
    attributes: &[AttributeValue],
    accuracy: GeometryAccuracy,
) -> Result<Vec<WriterObject>> {
    let payloads = contours
        .into_iter()
        .map(|contour| contour.transformed(transform, accuracy))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(lower_region_image_contours(&payloads, fill_rule, accuracy)?
        .into_iter()
        .map(|contour| WriterObject {
            kind: ObjectKind::Region {
                contours: vec![contour],
            },
            polarity,
            repeat: None,
            aperture_transform: WriterApertureTransform::default(),
            aperture_attributes: aperture_attributes.to_vec(),
            attributes: attributes.to_vec(),
        })
        .collect())
}

fn lower_region_image_contours(
    payloads: &[ContourBuf],
    fill_rule: FillRule,
    accuracy: GeometryAccuracy,
) -> Result<Vec<Contour>> {
    let rings = region::ContourSet::from_contours(payloads, fill_rule, 0.0, accuracy)?.rings;
    region::simplify_shapes_on_grid(rings, fill_rule, GERBER_GEOMETRY_GRID_MM)
        .into_iter()
        .filter_map(region_shape_contour)
        .collect::<Result<Vec<_>>>()
}

fn lower_region_contour(contour: &ContourBuf) -> Result<Contour> {
    if contour.cmds.is_empty() {
        return Err(GerberError::InvalidStructure(
            "cannot export empty Gerber region contour".to_string(),
        ));
    }
    Ok(Contour {
        segments: contour_segments(&contour.cmds)
            .into_iter()
            .map(|segment| match segment {
                Segment::Line { start, end } => ContourSegment::Line {
                    start: lower_point(start),
                    end: lower_point(end),
                },
                Segment::Arc(arc) => ContourSegment::Arc {
                    start: lower_point(arc.start),
                    end: lower_point(arc.end),
                    center_offset: lower_point(Point::new(
                        arc.center.x - arc.start.x,
                        arc.center.y - arc.start.y,
                    )),
                    clockwise: arc.clockwise,
                },
                Segment::Cubic { .. } => unreachable!("contour_segments flattens cubics"),
            })
            .collect(),
    })
}

fn region_shape_contour(shape: Vec<Ring>) -> Option<Result<Contour>> {
    let mut merged = pcb_ir::geom::bridge::bridge_shape(shape);
    merged.dedup();
    if merged.first() == merged.last() {
        merged.pop();
    }
    if merged.len() < 3 {
        return None;
    }
    let payload = region::rings_to_contours(vec![merged]).pop()?;
    Some(lower_region_contour(&payload))
}

const CUBIC_FLATTEN_STEPS: usize = 16;

/// Decode a command stream into resolved line/arc segments, flattening cubic
/// curves into line runs.
fn contour_segments(cmds: &[PathCmd]) -> Vec<Segment> {
    let mut segments = Vec::new();
    for segment in geom_path::segments(cmds) {
        match segment {
            Segment::Cubic { start, .. } => {
                let mut points = Vec::with_capacity(CUBIC_FLATTEN_STEPS);
                segment.sample_points(CUBIC_FLATTEN_STEPS, &mut points);
                let mut current = start;
                for end in points {
                    segments.push(Segment::Line {
                        start: current,
                        end,
                    });
                    current = end;
                }
            }
            segment => segments.push(segment),
        }
    }
    segments
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

fn lower_aperture_attributes(attributes: &ObjectAttributes) -> Vec<AttributeValue> {
    attributes
        .aperture_function
        .as_ref()
        .map_or_else(Vec::new, |function| lower_aperture_function(function))
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

fn quantize_hole(hole_diameter: Option<f64>) -> i64 {
    hole_diameter.map_or(0, quantize_mm)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    use pcb_ir::geom::{BBox, Mirror, Paint, Span};

    #[test]
    fn sanitizes_net_names_for_gerber_attribute_fields() {
        let attributes = lower_object_attributes(&ObjectAttributes {
            aperture_function: None,
            lower_flashes_to_regions: false,
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
            lower_flashes_to_regions: false,
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
            lower_flashes_to_regions: false,
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
        let summary = pcb_ir::dialects::artwork::compare::summarize(&geometry, accuracy).unwrap();
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

        let expected_mask = pcb_ir::dialects::artwork::compose_to_mask(&artwork, accuracy).unwrap();
        let expected_layer = &expected_mask.layers[0];
        let expected_area = pcb_ir::geom::ContourSet::from_painted_paths(
            &expected_mask.arena,
            expected_mask.shapes(expected_layer),
            pcb_ir::geom::tol::REGION_MM,
            accuracy,
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
        let summary = pcb_ir::dialects::artwork::compare::summarize(&geometry, accuracy).unwrap();
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
                pcb_ir::geom::tol::REGION_MM,
                accuracy,
            )
            .unwrap()
        }
        let expected =
            image(&pcb_ir::dialects::artwork::compose_to_mask(&artwork, accuracy).unwrap());
        let actual =
            image(&pcb_ir::dialects::artwork::compose_to_mask(&geometry, accuracy).unwrap());
        let symmetric_difference =
            expected.difference(&actual).area() + actual.difference(&expected).area();
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
        assert!(
            !gerber
                .apertures
                .iter()
                .any(|aperture| matches!(aperture.template, WriterApertureTemplate::Block { .. }))
        );
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
        assert!(
            !gerber
                .apertures
                .iter()
                .any(|aperture| matches!(aperture.template, WriterApertureTemplate::Block { .. }))
        );
        let contents = crate::write_layer(&gerber).expect("write empty layer");
        assert!(!contents.contains("%ABD"));
        assert_external_parser_accepts(&contents);
    }

    #[test]
    fn lowers_compound_region_holes_as_local_cut_ins() {
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

        assert_eq!(gerber.objects.len(), 1);
        assert_eq!(gerber.objects[0].polarity, Polarity::Dark);
        let ObjectKind::Region { contours } = &gerber.objects[0].kind else {
            panic!("expected local cut-in region");
        };
        assert_eq!(contours.len(), 1);
        assert_eq!(
            contours[0].segments.len(),
            10,
            "outer rectangle plus inner rectangle should be connected by two cut-in segments"
        );
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

        assert_eq!(gerber.objects.len(), 2);
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
        let summary = pcb_ir::dialects::artwork::compare::summarize(&geometry, accuracy).unwrap();
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
                meta: ObjectAttributes {
                    aperture_function: Some(vec!["Conductor".to_string()]),
                    lower_flashes_to_regions: true,
                    ..ObjectAttributes::default()
                },
            },
        );

        let gerber = lower_artwork_layer(&artwork, accuracy).expect("lower artwork");
        let contents = crate::write_layer(&gerber).expect("write Gerber");
        assert_external_parser_accepts(&contents);
        assert_eq!(contents.matches("%ABD").count(), 0);
        assert_eq!(contents.matches("%AM").count(), 0);
        assert_eq!(contents.matches("G36*").count(), 1);
        assert!(contents.contains("%TA.AperFunction,Conductor*%"));
        let parsed = crate::GerberX2::parse(&contents).expect("parse Gerber");
        let geometry = crate::geometry::extract_document(&parsed, accuracy).unwrap();
        let summary = pcb_ir::dialects::artwork::compare::summarize(&geometry, accuracy).unwrap();
        assert!(
            (summary.area_mm2 - 64.0).abs() < 0.01,
            "the hole ring must survive inside the aperture macro: {}",
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
                    lower_flashes_to_regions: false,
                    ..ObjectAttributes::default()
                },
            },
        );

        let gerber = lower_artwork_layer(&artwork, accuracy).expect("lower balance cell");
        // The exact rounded hex flattens once into a concrete one-primitive
        // outline macro; legacy CAM importers never evaluate compound
        // parameterized macros per flash.
        assert_eq!(gerber.aperture_macros.len(), 1);
        assert_eq!(gerber.aperture_macros[0].primitives.len(), 1);
        assert!(matches!(
            &gerber.aperture_macros[0].primitives[0],
            WriterMacroPrimitive::Shape { code: 4, .. }
        ));
        assert!(matches!(gerber.objects[0].kind, ObjectKind::Flash { .. }));
        let contents = crate::write_layer(&gerber).expect("write balance cell");
        assert_external_parser_accepts(&contents);
        let parsed = crate::GerberX2::parse(&contents).expect("parse balance cell");
        let geometry = crate::geometry::extract_document(&parsed, accuracy).unwrap();
        let actual_area = pcb_ir::dialects::artwork::compare::summarize(&geometry, accuracy)
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
        let summary = pcb_ir::dialects::artwork::compare::summarize(&geometry, accuracy).unwrap();
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
        let summary = pcb_ir::dialects::artwork::compare::summarize(&geometry, accuracy).unwrap();
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
        let summary = pcb_ir::dialects::artwork::compare::summarize(&geometry, accuracy).unwrap();
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
        let summary = pcb_ir::dialects::artwork::compare::summarize(&geometry, accuracy).unwrap();
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
