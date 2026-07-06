//! Lower pcb-ir artwork into an idiomatic Gerber X2 layer.
//!
//! This is the write-side mirror of [`crate::geometry::extract_document`]:
//! any artwork document annotated with [`LayerAttributes`]/[`ObjectAttributes`]
//! can be emitted as a Gerber file, regardless of which source dialect
//! produced it.

use std::collections::{BTreeSet, HashMap};

use crate::{
    AttributeValue, Contour, ContourSegment, GerberError, GerberLayer, ObjectKind,
    Point as GerberPoint, Result, WriterAperture, WriterApertureTemplate, WriterObject,
    sanitize_attribute_field,
};
use pcb_ir::dialects::artwork::{Aperture, ApertureShape, Geometry as ArtworkGeometry, PaintStage};
use pcb_ir::geom::path::{self as geom_path, ContourBuf, PathCmd};
use pcb_ir::geom::region::{self, Ring};
use pcb_ir::geom::{FillRule, Point, Polarity, Segment};

/// Gerber file-level attributes carried as artwork layer metadata.
#[derive(Debug, Clone, Default)]
pub struct LayerAttributes {
    pub file_function: Vec<String>,
    pub part: Option<Vec<String>>,
    pub file_polarity: Option<String>,
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
/// Standard-aperture flashes survive as flashes; macro/block flashes and
/// shaped draws are flattened to regions.
pub fn normalize_layer(gerber: &crate::GerberX2) -> Result<String> {
    let annotated = annotate_for_export(gerber, crate::geometry::extract_document(gerber));
    crate::write_layer(&lower_artwork_layer(&annotated)?)
}

/// Convert an extracted layer's interned Gerber metadata into the resolved
/// export annotations.
pub fn annotate_for_export(
    gerber: &crate::GerberX2,
    doc: crate::geometry::GerberArtworkDocument,
) -> ArtworkDocument {
    ArtworkDocument {
        apertures: doc.apertures,
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

pub fn lower_artwork_layer(layer: &ArtworkDocument) -> Result<GerberLayer> {
    let mut apertures = ApertureTable::default();
    let mut plan = GerberPlan::default();
    let layer_attributes = layer
        .layers
        .first()
        .map(|layer| layer.meta.clone())
        .unwrap_or_default();

    for (source_index, object) in layer.objects.iter().enumerate() {
        let objects = lower_artwork_object(layer, object, &mut apertures)?;
        plan.push_group(source_index, object.order.stage, objects);
    }
    let objects = plan.into_ordered_objects()?;

    Ok(GerberLayer {
        file_attributes: lower_layer_attributes(&layer_attributes),
        apertures: apertures.into_apertures(),
        objects,
        ..GerberLayer::default()
    })
}

fn lower_artwork_object(
    layer: &ArtworkDocument,
    object: &pcb_ir::dialects::artwork::Object<ObjectAttributes>,
    apertures: &mut ApertureTable,
) -> Result<Vec<WriterObject>> {
    let attributes = lower_object_attributes(&object.meta);
    let mut objects = Vec::new();
    match object.geometry {
        ArtworkGeometry::Region { path } => {
            objects.extend(lower_region_objects(
                layer,
                path,
                object.polarity,
                &attributes,
            )?);
        }
        ArtworkGeometry::Stroke { path } => {
            let artwork_path = &layer.arena.paths[path as usize];
            let default_function = vec!["Conductor".to_string()];
            let aperture_function = object
                .meta
                .aperture_function
                .as_deref()
                .unwrap_or(default_function.as_slice());
            let stroke_width = artwork_path.stroke().map_or(0.0, |stroke| stroke.width);
            let aperture = apertures.circle(stroke_width, aperture_function)?;
            for contour in layer.arena.path_contours(artwork_path) {
                for segment in contour_segments(&contour.cmds) {
                    objects.push(WriterObject {
                        kind: match segment {
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
                            Segment::Cubic { .. } => {
                                unreachable!("contour_segments flattens cubics")
                            }
                        },
                        polarity: object.polarity,
                        attributes: attributes.clone(),
                    });
                }
            }
        }
        ArtworkGeometry::Flash {
            aperture,
            transform,
        } => {
            if !transform_is_translation(transform) {
                return Err(GerberError::InvalidStructure(
                    "cannot lower transformed artwork flash to Gerber".to_string(),
                ));
            }
            let artwork_aperture = *layer.apertures.get(aperture as usize).ok_or_else(|| {
                GerberError::InvalidStructure(format!(
                    "artwork flash references missing aperture {aperture}"
                ))
            })?;
            let default_function = vec!["Conductor".to_string()];
            let aperture_function = object
                .meta
                .aperture_function
                .as_deref()
                .unwrap_or(default_function.as_slice());
            let aperture = apertures.artwork_aperture(artwork_aperture, aperture_function)?;
            objects.push(WriterObject {
                kind: ObjectKind::Flash {
                    at: lower_point(transform.transform_point(Point::new(0.0, 0.0))),
                    aperture,
                },
                polarity: object.polarity,
                attributes,
            });
        }
    }
    Ok(objects)
}

#[derive(Debug, Default)]
struct GerberPlan {
    groups: Vec<GerberObjectGroup>,
}

#[derive(Debug)]
struct GerberObjectGroup {
    source_index: usize,
    stage: PaintStage,
    objects: Vec<WriterObject>,
}

impl GerberPlan {
    fn push_group(&mut self, source_index: usize, stage: PaintStage, objects: Vec<WriterObject>) {
        if objects.is_empty() {
            return;
        }
        self.groups.push(GerberObjectGroup {
            source_index,
            stage,
            objects,
        });
    }

    fn into_ordered_objects(self) -> Result<Vec<WriterObject>> {
        let order = self.topological_order()?;
        let mut groups = self.groups.into_iter().map(Some).collect::<Vec<_>>();
        let mut objects = Vec::new();
        for group_index in order {
            let Some(group) = groups[group_index].take() else {
                continue;
            };
            objects.extend(group.objects);
        }
        Ok(objects)
    }

    fn topological_order(&self) -> Result<Vec<usize>> {
        let group_count = self.groups.len();
        let base_barrier = group_count;
        let overlay_barrier = group_count + 1;
        let node_count = group_count + 2;
        let mut graph = ScheduleGraph::new(node_count);

        let mut by_stage = [
            Vec::<usize>::new(),
            Vec::<usize>::new(),
            Vec::<usize>::new(),
        ];
        for (index, group) in self.groups.iter().enumerate() {
            by_stage[group.stage as usize].push(index);
        }

        for stage_groups in &by_stage {
            for pair in stage_groups.windows(2) {
                graph.add_edge(pair[0], pair[1]);
            }
        }
        for &group in &by_stage[PaintStage::Base as usize] {
            graph.add_edge(group, base_barrier);
        }
        graph.add_edge(base_barrier, overlay_barrier);
        for &group in &by_stage[PaintStage::Overlay as usize] {
            graph.add_edge(base_barrier, group);
            graph.add_edge(group, overlay_barrier);
        }
        for &group in &by_stage[PaintStage::FinalCutout as usize] {
            graph.add_edge(overlay_barrier, group);
        }

        let priorities = (0..node_count)
            .map(|node| self.schedule_priority(node, base_barrier, overlay_barrier))
            .collect::<Vec<_>>();
        let order = graph.topological_order(&priorities)?;
        Ok(order
            .into_iter()
            .filter(|&node| node < group_count)
            .collect())
    }

    fn schedule_priority(
        &self,
        node: usize,
        base_barrier: usize,
        overlay_barrier: usize,
    ) -> SchedulePriority {
        if node == base_barrier {
            return SchedulePriority {
                stage: PaintStage::Base,
                source_index: usize::MAX,
                barrier: 0,
            };
        }
        if node == overlay_barrier {
            return SchedulePriority {
                stage: PaintStage::Overlay,
                source_index: usize::MAX,
                barrier: 0,
            };
        }
        let group = &self.groups[node];
        SchedulePriority {
            stage: group.stage,
            source_index: group.source_index,
            barrier: 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct SchedulePriority {
    stage: PaintStage,
    source_index: usize,
    barrier: usize,
}

struct ScheduleGraph {
    outgoing: Vec<Vec<usize>>,
    indegree: Vec<usize>,
}

impl ScheduleGraph {
    fn new(node_count: usize) -> Self {
        Self {
            outgoing: vec![Vec::new(); node_count],
            indegree: vec![0; node_count],
        }
    }

    fn add_edge(&mut self, from: usize, to: usize) {
        self.outgoing[from].push(to);
        self.indegree[to] += 1;
    }

    fn topological_order(&self, priorities: &[SchedulePriority]) -> Result<Vec<usize>> {
        let mut indegree = self.indegree.clone();
        let mut ready = BTreeSet::new();
        for (node, &degree) in indegree.iter().enumerate() {
            if degree == 0 {
                ready.insert((priorities[node], node));
            }
        }

        let mut order = Vec::with_capacity(indegree.len());
        while let Some((_, node)) = ready.pop_first() {
            order.push(node);
            for &next in &self.outgoing[node] {
                indegree[next] -= 1;
                if indegree[next] == 0 {
                    ready.insert((priorities[next], next));
                }
            }
        }

        if order.len() != indegree.len() {
            return Err(GerberError::InvalidStructure(
                "Gerber emission schedule contains a cycle".to_string(),
            ));
        }
        Ok(order)
    }
}

#[derive(Default)]
struct ApertureTable {
    next_code: i32,
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

    fn artwork_aperture(&mut self, aperture: Aperture, function: &[String]) -> Result<i32> {
        let hole_diameter = (aperture.hole_diameter > 0.0).then_some(aperture.hole_diameter);
        match aperture.shape {
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
        }
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
        let code = if self.next_code == 0 {
            self.next_code = 10;
            10
        } else {
            self.next_code += 1;
            self.next_code
        };
        self.by_key.insert(key, code);
        self.apertures.push(WriterAperture {
            code,
            template,
            attributes: vec![AttributeValue::new(
                ".AperFunction",
                function.iter().cloned(),
            )],
        });
        Ok(code)
    }

    fn into_apertures(self) -> Vec<WriterAperture> {
        self.apertures
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
    values
}

fn lower_region_objects(
    layer: &ArtworkDocument,
    path_index: u32,
    polarity: Polarity,
    attributes: &[AttributeValue],
) -> Result<Vec<WriterObject>> {
    let artwork_path = &layer.arena.paths[path_index as usize];
    let payloads = layer.arena.path_contours(artwork_path);
    let fill_rule = artwork_path.fill_rule().unwrap_or(FillRule::NonZero);
    let contours = lower_region_image_contours(&payloads, fill_rule)?;
    Ok(contours
        .into_iter()
        .map(|contour| WriterObject {
            kind: ObjectKind::Region {
                contours: vec![contour],
            },
            polarity,
            attributes: attributes.to_vec(),
        })
        .collect())
}

fn lower_region_image_contours(
    payloads: &[ContourBuf],
    fill_rule: FillRule,
) -> Result<Vec<Contour>> {
    let rings = region::rings_from_contours(payloads);
    if payloads.len() == 1 && rings.len() == 1 {
        return Ok(vec![lower_region_contour(&payloads[0])?]);
    }

    region::simplify_shapes(rings, fill_rule)
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
    let merged = pcb_ir::geom::bridge::bridge_shape(shape);
    let payload = region::rings_to_contours(vec![merged]).into_iter().next()?;
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

fn lower_point(point: Point) -> GerberPoint {
    GerberPoint {
        x: point.x,
        y: point.y,
    }
}

fn transform_is_translation(transform: pcb_ir::geom::Affine2) -> bool {
    (transform.m00 - 1.0).abs() <= 1e-9
        && transform.m01.abs() <= 1e-9
        && transform.m10.abs() <= 1e-9
        && (transform.m11 - 1.0).abs() <= 1e-9
}

fn quantize_mm(value: f64) -> i64 {
    (value * 1_000_000.0).round() as i64
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
    use pcb_ir::geom::{BBox, Paint, Span};

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
    fn lowers_compound_region_holes_as_local_cut_ins() {
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

        let gerber = lower_artwork_layer(&artwork).expect("lower artwork");

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

        let gerber = lower_artwork_layer(&artwork).expect("lower artwork");

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
        let geometry = crate::geometry::extract_document(&parsed);
        let summary = pcb_ir::dialects::artwork::compare::summarize(&geometry);
        assert!(
            (summary.area_mm2 - 56.0).abs() < 0.001,
            "deep even-odd topology exported wrong area: {}",
            summary.area_mm2
        );
    }

    #[test]
    fn lowers_single_self_cut_even_odd_region_before_emitting_gerber() {
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

        let gerber = lower_artwork_layer(&artwork).expect("lower artwork");

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

        let gerber = lower_artwork_layer(&artwork).expect("lower artwork");

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
        let geometry = crate::geometry::extract_document(&parsed);
        let summary = pcb_ir::dialects::artwork::compare::summarize(&geometry);
        assert!(
            (summary.area_mm2 - 100.0).abs() < 0.001,
            "donut hole cleared prior base copper; area was {}",
            summary.area_mm2
        );
    }

    #[test]
    fn places_compound_regions_before_overlay_objects() {
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

        let gerber = lower_artwork_layer(&artwork).expect("lower artwork");

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
        let points = [
            Point::new(min_x, min_y),
            Point::new(max_x, min_y),
            Point::new(max_x, max_y),
            Point::new(min_x, max_y),
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
