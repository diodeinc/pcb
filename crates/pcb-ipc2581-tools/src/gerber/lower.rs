use std::collections::{BTreeSet, HashMap};

use anyhow::{Context, Result, bail};
use gerberx2::{
    AttributeValue, Contour, ContourSegment, GerberLayer, ObjectKind, Point as GerberPoint,
    WriterAperture, WriterApertureTemplate, WriterObject, sanitize_attribute_field,
};
use pcb_ir::common::{BBox, FillRule, PaintPolarity, Point};
use pcb_ir::dialects::artwork::{ArtworkAperture, ArtworkGeometry, ArtworkPath, PaintStage};
use pcb_ir::dialects::gerber::Polarity;
use pcb_ir::dialects::path::{self as common_path, PathCmd, PathOp, PathPayload, PolygonContour};

use super::artwork::{ArtworkLayer, LayerAttributes, ObjectAttributes};

pub fn lower_artwork_layer(layer: &ArtworkLayer) -> Result<GerberLayer> {
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
    layer: &ArtworkLayer,
    object: &pcb_ir::dialects::artwork::ArtworkObject<ObjectAttributes>,
    apertures: &mut ApertureTable,
) -> Result<Vec<WriterObject>> {
    let attributes = lower_object_attributes(&object.meta);
    let mut objects = Vec::new();
    match object.geometry {
        ArtworkGeometry::Region { path } => {
            objects.extend(lower_region_objects(
                layer,
                path,
                object.paint,
                &attributes,
            )?);
        }
        ArtworkGeometry::Stroke { path } => {
            let artwork_path = &layer.paths[path as usize];
            let default_function = vec!["Conductor".to_string()];
            let aperture_function = object
                .meta
                .aperture_function
                .as_deref()
                .unwrap_or(default_function.as_slice());
            let aperture = apertures.circle(artwork_path.stroke_width, aperture_function)?;
            for contour in path_contours(layer, artwork_path) {
                for segment in contour_segments(&contour.cmds)? {
                    objects.push(WriterObject {
                        kind: match segment {
                            Segment::Line { start, end } => ObjectKind::Draw {
                                start: lower_point(start),
                                end: lower_point(end),
                                aperture,
                            },
                            Segment::Arc {
                                start,
                                end,
                                center,
                                clockwise,
                            } => ObjectKind::Arc {
                                start: lower_point(start),
                                end: lower_point(end),
                                center_offset: lower_point(Point::new(
                                    center.x - start.x,
                                    center.y - start.y,
                                )),
                                clockwise,
                                aperture,
                            },
                        },
                        polarity: lower_polarity(object.paint),
                        attributes: attributes.clone(),
                    });
                }
            }
        }
        ArtworkGeometry::CircleFlash { at, diameter } => {
            let default_function = vec!["Conductor".to_string()];
            let aperture_function = object
                .meta
                .aperture_function
                .as_deref()
                .unwrap_or(default_function.as_slice());
            let aperture = apertures.circle(diameter, aperture_function)?;
            objects.push(WriterObject {
                kind: ObjectKind::Flash {
                    at: lower_point(at),
                    aperture,
                },
                polarity: lower_polarity(object.paint),
                attributes,
            });
        }
        ArtworkGeometry::Flash {
            aperture,
            transform,
        } => {
            if !transform_is_translation(transform) {
                bail!("cannot lower transformed artwork flash to Gerber");
            }
            let artwork_aperture = *layer
                .apertures
                .get(aperture as usize)
                .with_context(|| format!("artwork flash references missing aperture {aperture}"))?;
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
                polarity: lower_polarity(object.paint),
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
            bail!("Gerber emission schedule contains a cycle");
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
    Circle { diameter_nm: i64 },
    Rectangle { width_nm: i64, height_nm: i64 },
    Obround { width_nm: i64, height_nm: i64 },
}

impl ApertureTable {
    fn circle(&mut self, diameter: f64, function: &[String]) -> Result<i32> {
        if diameter <= 0.0 {
            bail!("cannot export non-positive Gerber stroke aperture diameter {diameter}");
        }
        self.define(
            ApertureTemplateKey::Circle {
                diameter_nm: quantize_mm(diameter),
            },
            WriterApertureTemplate::Circle {
                diameter,
                hole_diameter: None,
            },
            function,
        )
    }

    fn artwork_aperture(&mut self, aperture: ArtworkAperture, function: &[String]) -> Result<i32> {
        match aperture {
            ArtworkAperture::Circle { diameter } => self.circle(diameter, function),
            ArtworkAperture::Rectangle { width, height } => {
                if width <= 0.0 || height <= 0.0 {
                    bail!(
                        "cannot export non-positive Gerber rectangle aperture {width} x {height}"
                    );
                }
                self.define(
                    ApertureTemplateKey::Rectangle {
                        width_nm: quantize_mm(width),
                        height_nm: quantize_mm(height),
                    },
                    WriterApertureTemplate::Rectangle {
                        width,
                        height,
                        hole_diameter: None,
                    },
                    function,
                )
            }
            ArtworkAperture::Obround { width, height } => {
                if width <= 0.0 || height <= 0.0 {
                    bail!("cannot export non-positive Gerber obround aperture {width} x {height}");
                }
                self.define(
                    ApertureTemplateKey::Obround {
                        width_nm: quantize_mm(width),
                        height_nm: quantize_mm(height),
                    },
                    WriterApertureTemplate::Obround {
                        width,
                        height,
                        hole_diameter: None,
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
    layer: &ArtworkLayer,
    path_index: u32,
    paint: PaintPolarity,
    attributes: &[AttributeValue],
) -> Result<Vec<WriterObject>> {
    let artwork_path = &layer.paths[path_index as usize];
    let payloads = path_contours(layer, artwork_path);
    let contours = lower_region_image_contours(&payloads, artwork_path.fill_rule)?;
    if contours.is_empty() {
        return Ok(Vec::new());
    }
    Ok(vec![WriterObject {
        kind: ObjectKind::Region { contours },
        polarity: lower_polarity(paint),
        attributes: attributes.to_vec(),
    }])
}

fn lower_region_image_contours(
    payloads: &[PathPayload],
    fill_rule: FillRule,
) -> Result<Vec<Contour>> {
    if fill_rule == FillRule::NonZero {
        return payloads.iter().map(lower_region_contour).collect();
    }

    if let Some(contours) = lower_simple_compound_region_contours(payloads)? {
        return Ok(contours);
    }

    let contours = common_path::payloads_to_polygon_contours(payloads);
    let contours = common_path::simplify_polygon_contours(contours, fill_rule);
    let mut parts = contours
        .into_iter()
        .filter_map(region_part)
        .collect::<Result<Vec<_>>>()?;
    assign_region_depths(&mut parts);
    parts.sort_by_key(|part| part.depth);

    Ok(parts
        .into_iter()
        .map(oriented_region_part_contour)
        .collect())
}

fn lower_simple_compound_region_contours(payloads: &[PathPayload]) -> Result<Option<Vec<Contour>>> {
    let polygons = common_path::payloads_to_polygon_contours(payloads);
    if polygons.len() != payloads.len() {
        return Ok(None);
    }

    let mut parts = payloads
        .iter()
        .zip(polygons)
        .filter_map(|(payload, polygon)| original_region_part(payload, polygon))
        .collect::<Result<Vec<_>>>()?;
    if parts.len() != payloads.len() || !region_parts_are_nested_or_disjoint(&parts) {
        return Ok(None);
    }

    assign_region_depths(&mut parts);
    parts.sort_by_key(|part| part.depth);

    Ok(Some(
        parts
            .into_iter()
            .map(oriented_region_part_contour)
            .collect(),
    ))
}

fn lower_region_contour(contour: &PathPayload) -> Result<Contour> {
    if contour.cmds.is_empty() {
        bail!("cannot export empty Gerber region contour");
    }
    Ok(Contour {
        segments: contour_segments(&contour.cmds)?
            .into_iter()
            .map(|segment| match segment {
                Segment::Line { start, end } => ContourSegment::Line {
                    start: lower_point(start),
                    end: lower_point(end),
                },
                Segment::Arc {
                    start,
                    end,
                    center,
                    clockwise,
                } => ContourSegment::Arc {
                    start: lower_point(start),
                    end: lower_point(end),
                    center_offset: lower_point(Point::new(center.x - start.x, center.y - start.y)),
                    clockwise,
                },
            })
            .collect(),
    })
}

fn path_contours(
    layer: &ArtworkLayer,
    path: &ArtworkPath,
) -> Vec<pcb_ir::dialects::path::PathPayload> {
    layer.contours[path.contour_start as usize..(path.contour_start + path.contour_count) as usize]
        .iter()
        .map(|contour| pcb_ir::dialects::path::PathPayload {
            bbox: contour.bbox,
            cmds: layer.path_cmds
                [contour.cmd_start as usize..(contour.cmd_start + contour.cmd_count) as usize]
                .to_vec(),
        })
        .collect()
}

#[derive(Debug)]
struct RegionPart {
    polygon: PolygonContour,
    contour: Contour,
    bbox: BBox,
    area: f64,
    depth: usize,
}

fn region_part(polygon: PolygonContour) -> Option<Result<RegionPart>> {
    let payload = common_path::polygon_contours_to_payloads(vec![polygon.clone()])
        .into_iter()
        .next()?;
    original_region_part(&payload, polygon)
}

fn original_region_part(
    payload: &PathPayload,
    polygon: PolygonContour,
) -> Option<Result<RegionPart>> {
    if polygon.is_empty() {
        return None;
    }
    let bbox = payload.bbox;
    let area = polygon_area_abs(&polygon);
    Some(lower_region_contour(payload).map(|contour| RegionPart {
        polygon,
        contour,
        bbox,
        area,
        depth: 0,
    }))
}

fn region_parts_are_nested_or_disjoint(parts: &[RegionPart]) -> bool {
    for left_index in 0..parts.len() {
        for right_index in left_index + 1..parts.len() {
            let left = &parts[left_index];
            let right = &parts[right_index];
            if !left.bbox.intersects(right.bbox) {
                continue;
            }

            let left_contains_right = region_part_contains(left, right);
            let right_contains_left = region_part_contains(right, left);
            if left_contains_right == right_contains_left {
                return false;
            }
        }
    }
    true
}

fn region_part_contains(outer: &RegionPart, inner: &RegionPart) -> bool {
    bbox_contains_bbox(outer.bbox, inner.bbox) && point_in_polygon(inner.polygon[0], &outer.polygon)
}

fn assign_region_depths(parts: &mut [RegionPart]) {
    let parents = (0..parts.len())
        .map(|index| region_parent(index, parts))
        .collect::<Vec<_>>();
    let mut depths = vec![None; parts.len()];
    for (index, part) in parts.iter_mut().enumerate() {
        part.depth = region_depth(index, &parents, &mut depths);
    }
}

fn region_parent(index: usize, parts: &[RegionPart]) -> Option<usize> {
    let point = parts[index].polygon[0];
    let mut parent: Option<usize> = None;
    for candidate in 0..parts.len() {
        if candidate == index
            || parts[candidate].area <= parts[index].area
            || !bbox_contains_bbox(parts[candidate].bbox, parts[index].bbox)
            || !point_in_polygon(point, &parts[candidate].polygon)
        {
            continue;
        }
        if let Some(current_parent) = parent
            && parts[candidate].area >= parts[current_parent].area
        {
            continue;
        }
        parent = Some(candidate);
    }
    parent
}

fn region_depth(index: usize, parents: &[Option<usize>], depths: &mut [Option<usize>]) -> usize {
    if let Some(depth) = depths[index] {
        return depth;
    }
    let depth = parents[index]
        .map(|parent| region_depth(parent, parents, depths) + 1)
        .unwrap_or(0);
    depths[index] = Some(depth);
    depth
}

fn oriented_region_part_contour(part: RegionPart) -> Contour {
    let signed_area = polygon_area_signed(&part.polygon);
    let wants_positive = part.depth.is_multiple_of(2);
    if (signed_area >= 0.0) == wants_positive {
        part.contour
    } else {
        reverse_contour(part.contour)
    }
}

fn reverse_contour(contour: Contour) -> Contour {
    Contour {
        segments: contour
            .segments
            .into_iter()
            .rev()
            .map(reverse_segment)
            .collect(),
    }
}

fn reverse_segment(segment: ContourSegment) -> ContourSegment {
    match segment {
        ContourSegment::Line { start, end } => ContourSegment::Line {
            start: end,
            end: start,
        },
        ContourSegment::Arc {
            start,
            end,
            center_offset,
            clockwise,
        } => {
            let center = Point::new(start.x + center_offset.x, start.y + center_offset.y);
            ContourSegment::Arc {
                start: end,
                end: start,
                center_offset: GerberPoint {
                    x: center.x - end.x,
                    y: center.y - end.y,
                },
                clockwise: !clockwise,
            }
        }
    }
}

fn bbox_contains_bbox(outer: BBox, inner: BBox) -> bool {
    !outer.is_empty()
        && !inner.is_empty()
        && outer.min.x <= inner.min.x
        && outer.min.y <= inner.min.y
        && outer.max.x >= inner.max.x
        && outer.max.y >= inner.max.y
}

fn point_in_polygon(point: [f64; 2], polygon: &PolygonContour) -> bool {
    let [x, y] = point;
    let mut inside = false;
    for index in 0..polygon.len() {
        let [x0, y0] = polygon[index];
        let [x1, y1] = polygon[(index + 1) % polygon.len()];
        if ((y0 > y) != (y1 > y)) && x < (x1 - x0) * (y - y0) / (y1 - y0) + x0 {
            inside = !inside;
        }
    }
    inside
}

fn polygon_area_abs(polygon: &PolygonContour) -> f64 {
    polygon_area_signed(polygon).abs()
}

fn polygon_area_signed(polygon: &PolygonContour) -> f64 {
    let mut area = 0.0;
    for index in 0..polygon.len() {
        let [x0, y0] = polygon[index];
        let [x1, y1] = polygon[(index + 1) % polygon.len()];
        area += x0 * y1 - x1 * y0;
    }
    area / 2.0
}

#[derive(Debug, Clone, Copy)]
enum Segment {
    Line {
        start: Point,
        end: Point,
    },
    Arc {
        start: Point,
        end: Point,
        center: Point,
        clockwise: bool,
    },
}

fn contour_segments(cmds: &[PathCmd]) -> Result<Vec<Segment>> {
    let mut first = None;
    let mut current = None;
    let mut segments = Vec::new();
    for cmd in cmds {
        match cmd.op {
            PathOp::MoveTo => {
                first = Some(cmd.p0);
                current = Some(cmd.p0);
            }
            PathOp::LineTo => {
                let start = current.context("path line command appears before move command")?;
                segments.push(Segment::Line { start, end: cmd.p0 });
                current = Some(cmd.p0);
            }
            PathOp::ArcTo => {
                let start = current.context("path arc command appears before move command")?;
                segments.push(Segment::Arc {
                    start,
                    end: cmd.p0,
                    center: cmd.p1,
                    clockwise: cmd.clockwise,
                });
                current = Some(cmd.p0);
            }
            PathOp::CubicTo => {
                let start = current.context("path cubic command appears before move command")?;
                let steps = 16;
                for step in 1..=steps {
                    let end =
                        cubic_point(start, cmd.p0, cmd.p1, cmd.p2, step as f64 / steps as f64);
                    let segment_start = current.unwrap_or(start);
                    segments.push(Segment::Line {
                        start: segment_start,
                        end,
                    });
                    current = Some(end);
                }
            }
            PathOp::Close => {
                if let (Some(start), Some(end)) = (first, current)
                    && !points_close(start, end)
                {
                    segments.push(Segment::Line {
                        start: end,
                        end: start,
                    });
                }
                current = first;
            }
        }
    }
    Ok(segments)
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

fn lower_polarity(paint: PaintPolarity) -> Polarity {
    match paint {
        PaintPolarity::Dark => Polarity::Dark,
        PaintPolarity::Clear => Polarity::Clear,
    }
}

fn lower_point(point: Point) -> GerberPoint {
    GerberPoint {
        x: point.x,
        y: point.y,
    }
}

fn transform_is_translation(transform: pcb_ir::common::Affine2) -> bool {
    (transform.m00 - 1.0).abs() <= 1e-9
        && transform.m01.abs() <= 1e-9
        && transform.m10.abs() <= 1e-9
        && (transform.m11 - 1.0).abs() <= 1e-9
}

fn points_close(a: Point, b: Point) -> bool {
    a.distance_to(b) <= 1e-9
}

fn cubic_point(start: Point, c1: Point, c2: Point, end: Point, t: f64) -> Point {
    let mt = 1.0 - t;
    Point::new(
        mt.powi(3) * start.x
            + 3.0 * mt.powi(2) * t * c1.x
            + 3.0 * mt * t.powi(2) * c2.x
            + t.powi(3) * end.x,
        mt.powi(3) * start.y
            + 3.0 * mt.powi(2) * t * c1.y
            + 3.0 * mt * t.powi(2) * c2.y
            + t.powi(3) * end.y,
    )
}

fn quantize_mm(value: f64) -> i64 {
    (value * 1_000_000.0).round() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use pcb_ir::common::{FillRule, LayerRole, Side, Unit};
    use pcb_ir::dialects::artwork::{ArtworkLayer as IrArtworkLayer, ArtworkObject, PaintOrder};

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
    fn lowers_compound_region_holes_as_object_local_contours() {
        let mut artwork = ArtworkLayer::new(Unit::Millimeter);
        let layer_id = artwork.push_layer(IrArtworkLayer {
            name: "F.SilkS".to_string(),
            role: LayerRole::Legend,
            side: Side::None,
            object_start: 0,
            object_count: 0,
            bbox: BBox::empty(),
            meta: LayerAttributes::default(),
        });
        let path = artwork.push_path(
            ArtworkPath::filled(FillRule::EvenOdd),
            vec![
                rect_payload(0.0, 0.0, 10.0, 10.0),
                rect_payload(2.0, 2.0, 8.0, 8.0),
            ],
        );
        artwork.push_object(
            layer_id,
            ArtworkObject {
                paint: PaintPolarity::Dark,
                order: Default::default(),
                geometry: ArtworkGeometry::Region { path },
                net: None,
                bbox: artwork.paths[path as usize].bbox,
                meta: ObjectAttributes::default(),
            },
        );

        let gerber = lower_artwork_layer(&artwork).expect("lower artwork");

        assert_eq!(gerber.objects.len(), 1);
        assert_eq!(gerber.objects[0].polarity, Polarity::Dark);
        let ObjectKind::Region { contours } = &gerber.objects[0].kind else {
            panic!("expected multi-contour region");
        };
        assert_eq!(contours.len(), 2);
    }

    #[test]
    fn preserves_arcs_for_simple_compound_region_holes() {
        let mut artwork = ArtworkLayer::new(Unit::Millimeter);
        let layer_id = artwork.push_layer(IrArtworkLayer {
            name: "F.SilkS".to_string(),
            role: LayerRole::Legend,
            side: Side::None,
            object_start: 0,
            object_count: 0,
            bbox: BBox::empty(),
            meta: LayerAttributes::default(),
        });
        let path = artwork.push_path(
            ArtworkPath::filled(FillRule::EvenOdd),
            vec![circle_payload(0.0, 0.0, 5.0), circle_payload(0.0, 0.0, 2.0)],
        );
        artwork.push_object(
            layer_id,
            ArtworkObject {
                paint: PaintPolarity::Dark,
                order: Default::default(),
                geometry: ArtworkGeometry::Region { path },
                net: None,
                bbox: artwork.paths[path as usize].bbox,
                meta: ObjectAttributes::default(),
            },
        );

        let gerber = lower_artwork_layer(&artwork).expect("lower artwork");

        assert_eq!(gerber.objects.len(), 1);
        assert_eq!(gerber.objects[0].polarity, Polarity::Dark);
        let ObjectKind::Region { contours } = &gerber.objects[0].kind else {
            panic!("expected multi-contour region");
        };
        assert_eq!(contours.len(), 2);
        assert!(
            contours[1]
                .segments
                .iter()
                .any(|segment| matches!(segment, ContourSegment::Arc { .. }))
        );
    }

    #[test]
    fn lowers_single_self_cut_even_odd_region_before_emitting_gerber() {
        let mut artwork = ArtworkLayer::new(Unit::Millimeter);
        let layer_id = artwork.push_layer(IrArtworkLayer {
            name: "F.Cu".to_string(),
            role: LayerRole::Copper,
            side: Side::Top,
            object_start: 0,
            object_count: 0,
            bbox: BBox::empty(),
            meta: LayerAttributes::default(),
        });
        let path = artwork.push_path(
            ArtworkPath::filled(FillRule::EvenOdd),
            vec![self_cut_donut_payload()],
        );
        artwork.push_object(
            layer_id,
            ArtworkObject {
                paint: PaintPolarity::Dark,
                order: Default::default(),
                geometry: ArtworkGeometry::Region { path },
                net: None,
                bbox: artwork.paths[path as usize].bbox,
                meta: ObjectAttributes::default(),
            },
        );

        let gerber = lower_artwork_layer(&artwork).expect("lower artwork");

        assert_eq!(gerber.objects.len(), 1);
        assert_eq!(gerber.objects[0].polarity, Polarity::Dark);
        let ObjectKind::Region { contours } = &gerber.objects[0].kind else {
            panic!("expected simplified multi-contour region");
        };
        assert!(!contours.is_empty());
    }

    #[test]
    fn keeps_object_local_holes_inside_base_region_before_overlay() {
        let mut artwork = ArtworkLayer::new(Unit::Millimeter);
        let layer_id = artwork.push_layer(IrArtworkLayer {
            name: "F.Cu".to_string(),
            role: LayerRole::Copper,
            side: Side::Top,
            object_start: 0,
            object_count: 0,
            bbox: BBox::empty(),
            meta: LayerAttributes::default(),
        });
        let pour = artwork.push_path(
            ArtworkPath::filled(FillRule::EvenOdd),
            vec![
                rect_payload(0.0, 0.0, 10.0, 10.0),
                rect_payload(2.0, 2.0, 8.0, 8.0),
            ],
        );
        artwork.push_object(
            layer_id,
            ArtworkObject {
                paint: PaintPolarity::Dark,
                order: PaintOrder {
                    stage: PaintStage::Base,
                },
                geometry: ArtworkGeometry::Region { path: pour },
                net: None,
                bbox: artwork.paths[pour as usize].bbox,
                meta: ObjectAttributes::default(),
            },
        );
        let trace = artwork.push_path(
            ArtworkPath::filled(FillRule::NonZero),
            vec![
                rect_payload(11.0, 0.0, 12.0, 1.0),
                rect_payload(11.0, 2.0, 12.0, 3.0),
            ],
        );
        artwork.push_object(
            layer_id,
            ArtworkObject {
                paint: PaintPolarity::Dark,
                order: PaintOrder {
                    stage: PaintStage::Overlay,
                },
                geometry: ArtworkGeometry::Region { path: trace },
                net: None,
                bbox: artwork.paths[trace as usize].bbox,
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
                    ObjectKind::Region { contours } if contours.len() == 2
                )
            })
            .expect("base pour should stay one multi-contour region");
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
            gerber
                .objects
                .iter()
                .all(|object| object.polarity == Polarity::Dark)
        );
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
    }

    fn rect_payload(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> PathPayload {
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
        PathPayload { bbox, cmds }
    }

    fn self_cut_donut_payload() -> PathPayload {
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
        PathPayload { bbox, cmds }
    }

    fn circle_payload(cx: f64, cy: f64, radius: f64) -> PathPayload {
        let center = Point::new(cx, cy);
        let points = [
            Point::new(cx + radius, cy),
            Point::new(cx, cy + radius),
            Point::new(cx - radius, cy),
            Point::new(cx, cy - radius),
            Point::new(cx + radius, cy),
        ];
        let mut bbox = BBox::empty();
        bbox.include_circular_arc(points[0], points[1], center, false);
        bbox.include_circular_arc(points[1], points[2], center, false);
        bbox.include_circular_arc(points[2], points[3], center, false);
        bbox.include_circular_arc(points[3], points[4], center, false);
        let cmds = vec![
            PathCmd::move_to(points[0]),
            PathCmd::arc_to(points[1], center, false),
            PathCmd::arc_to(points[2], center, false),
            PathCmd::arc_to(points[3], center, false),
            PathCmd::arc_to(points[4], center, false),
            PathCmd::close(),
        ];
        PathPayload { bbox, cmds }
    }
}
