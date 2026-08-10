//! Source-independent ordered fabrication artwork.
//!
//! This dialect intentionally keeps an object stream instead of immediately
//! flattening everything into polygons. It is the common interchange target
//! for source dialects such as IPC-2581 and Gerber when we still care about
//! idiomatic fabrication objects: flashes, strokes, regions, and ordered
//! dark/clear paint operations.

pub mod compare;
pub mod legalize;

use crate::dialects::mask;
use crate::dialects::{LayerRole, Side};
use crate::geom::path::ContourBuf;
use crate::geom::region::{self, Ring};
use crate::geom::{
    Affine2, BBox, Diagnostic, FillRule, Paint, PathArena, Point, Polarity, Span, StrokeStyle,
    shapes,
};

#[derive(Debug, Clone, Default)]
pub struct Document<LayerMeta = (), ObjectMeta = ()> {
    pub apertures: Vec<Aperture>,
    /// Reusable ordered sub-images. Blocks are target-independent; consumers
    /// either preserve or expand them explicitly.
    pub blocks: Vec<Block<ObjectMeta>>,
    pub layers: Vec<Layer<LayerMeta>>,
    pub objects: Vec<Object<ObjectMeta>>,
    pub arena: PathArena,
    pub diagnostics: Vec<Diagnostic>,
}

impl<LayerMeta, ObjectMeta> Document<LayerMeta, ObjectMeta> {
    pub fn new() -> Self {
        Self {
            apertures: Vec::new(),
            blocks: Vec::new(),
            layers: Vec::new(),
            objects: Vec::new(),
            arena: PathArena::default(),
            diagnostics: Vec::new(),
        }
    }

    pub fn push_layer(&mut self, mut layer: Layer<LayerMeta>) -> u32 {
        layer.objects = Span::new(self.objects.len() as u32, 0);
        let id = self.layers.len() as u32;
        self.layers.push(layer);
        id
    }

    /// Create an empty reusable artwork block.
    pub fn push_block(&mut self) -> u32 {
        let id = self.blocks.len() as u32;
        self.blocks.push(Block {
            objects: Vec::new(),
            bbox: BBox::empty(),
        });
        id
    }

    /// Append an object to a block.
    pub fn push_block_object(&mut self, block_id: u32, object: Object<ObjectMeta>) -> u32 {
        let block = &mut self.blocks[block_id as usize];
        let id = block.objects.len() as u32;
        let bbox = object.bbox;
        block.objects.push(object);
        block.bbox = block.bbox.union(bbox);
        id
    }

    /// Register an aperture, reusing an existing identical definition.
    pub fn push_aperture(&mut self, aperture: Aperture) -> u32 {
        if let Some(existing) = self
            .apertures
            .iter()
            .position(|candidate| *candidate == aperture)
        {
            return existing as u32;
        }
        let id = self.apertures.len() as u32;
        self.apertures.push(aperture);
        id
    }

    /// Append an object to a layer, maintaining the layer's object span and
    /// bounding box. Objects for one layer must be pushed contiguously.
    pub fn push_object(&mut self, layer_id: u32, object: Object<ObjectMeta>) -> u32 {
        let id = self.objects.len() as u32;
        let bbox = object.bbox;
        self.objects.push(object);
        let layer = &mut self.layers[layer_id as usize];
        if layer.objects.is_empty() {
            layer.objects.start = id;
        }
        layer.objects.count += 1;
        layer.bbox = layer.bbox.union(bbox);
        id
    }

    /// Append a styled path; returns its index into `arena.paths`.
    pub fn push_path(
        &mut self,
        paint: Paint,
        contours: impl IntoIterator<Item = ContourBuf>,
    ) -> u32 {
        self.arena.push_path(paint, contours)
    }

    pub fn path_bbox(&self, path: u32) -> BBox {
        self.arena.path(path).bbox
    }

    pub fn warn(&mut self, message: impl Into<String>) {
        self.diagnostics.push(Diagnostic::warning(message));
    }

    pub fn validate(&self) -> Result<(), crate::geom::Diagnostics> {
        let mut diagnostics = crate::geom::Diagnostics::default();
        for (index, block) in self.blocks.iter().enumerate() {
            if let Err(message) = crate::geom::validate_bbox("artwork block", index, block.bbox) {
                diagnostics.error(message);
            }
            let object_name = format!("artwork block {index} object");
            for (object_index, object) in block.objects.iter().enumerate() {
                if let Some(error) = geometry_ref_error(self, object.geometry, index) {
                    diagnostics.error(format!("{object_name} {object_index} {error}"));
                }
                if let Err(message) =
                    crate::geom::validate_bbox(&object_name, object_index, object.bbox)
                {
                    diagnostics.error(message);
                }
            }
        }
        for (index, layer) in self.layers.iter().enumerate() {
            if let Err(message) =
                layer
                    .objects
                    .validate("artwork layer objects", index, self.objects.len())
            {
                diagnostics.error(message);
            }
            if let Err(message) = crate::geom::validate_bbox("artwork layer", index, layer.bbox) {
                diagnostics.error(message);
            }
        }
        for (index, object) in self.objects.iter().enumerate() {
            if let Some(error) = geometry_ref_error(self, object.geometry, self.blocks.len()) {
                diagnostics.error(format!("artwork object {index} {error}"));
            }
            if let Err(message) = crate::geom::validate_bbox("artwork object", index, object.bbox) {
                diagnostics.error(message);
            }
        }
        self.arena.validate_into("artwork", &mut diagnostics);
        diagnostics.into_result()
    }
}

fn geometry_ref_error<LayerMeta, ObjectMeta>(
    doc: &Document<LayerMeta, ObjectMeta>,
    geometry: Geometry,
    block_limit: usize,
) -> Option<String> {
    match geometry {
        Geometry::Flash { aperture, .. } if aperture as usize >= doc.apertures.len() => {
            Some(format!("references missing aperture {aperture}"))
        }
        Geometry::Stroke { path } | Geometry::Region { path }
            if path as usize >= doc.arena.paths.len() =>
        {
            Some(format!("references missing path {path}"))
        }
        Geometry::Instance { block, .. } if block as usize >= doc.blocks.len() => {
            Some(format!("references missing block {block}"))
        }
        Geometry::Instance { block, .. } if block as usize >= block_limit => Some(format!(
            "references block {block}; reusable blocks must reference earlier blocks"
        )),
        _ => None,
    }
}

/// A reusable ordered sub-image in local coordinates.
#[derive(Debug, Clone)]
pub struct Block<Meta = ()> {
    /// Blocks are topologically ordered: instances may only reference an
    /// earlier block. This makes cycles unrepresentable in valid artwork.
    pub objects: Vec<Object<Meta>>,
    pub bbox: BBox,
}

#[derive(Debug, Clone)]
pub struct Layer<Meta = ()> {
    pub name: String,
    pub role: LayerRole,
    pub side: Side,
    pub objects: Span,
    pub bbox: BBox,
    pub meta: Meta,
}

impl<Meta: Default> Layer<Meta> {
    pub fn new(name: impl Into<String>, role: LayerRole, side: Side) -> Self {
        Self {
            name: name.into(),
            role,
            side,
            objects: Span::EMPTY,
            bbox: BBox::empty(),
            meta: Meta::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Object<Meta = ()> {
    pub polarity: Polarity,
    pub order: PaintOrder,
    pub geometry: Geometry,
    pub bbox: BBox,
    pub meta: Meta,
}

impl<Meta: Default> Object<Meta> {
    pub fn new(polarity: Polarity, geometry: Geometry) -> Self {
        Self {
            polarity,
            order: PaintOrder::default(),
            geometry,
            bbox: BBox::empty(),
            meta: Meta::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum PaintStage {
    /// Base images such as pours that local clear objects may subtract.
    Base,
    /// Dark objects that must survive base-stage clears: pads, vias, traces, fiducials.
    #[default]
    Overlay,
    /// Deliberate final removals applied after all material has been painted.
    FinalCutout,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PaintOrder {
    pub stage: PaintStage,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Geometry {
    /// A standard aperture stamped under a placement transform.
    Flash { aperture: u32, transform: Affine2 },
    /// A stroked centerline path (`arena.paths` index, stroke paint).
    Stroke { path: u32 },
    /// A filled region path (`arena.paths` index, fill paint).
    Region { path: u32 },
    /// A reusable ordered sub-image placed under an affine transform.
    Instance { block: u32, transform: Affine2 },
}

impl Geometry {
    pub fn path(self) -> Option<u32> {
        match self {
            Self::Flash { .. } | Self::Instance { .. } => None,
            Self::Stroke { path } | Self::Region { path } => Some(path),
        }
    }
}

/// A standard aperture: a primitive shape with an optional round hole.
#[derive(Debug, Clone, PartialEq)]
pub struct Aperture {
    pub shape: ApertureShape,
    /// Diameter of the round hole through the aperture; `0.0` means solid.
    pub hole_diameter: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ApertureShape {
    Circle {
        diameter: f64,
    },
    Rectangle {
        width: f64,
        height: f64,
    },
    Obround {
        width: f64,
        height: f64,
    },
    /// Regular polygon inscribed in `diameter`, first vertex at
    /// `rotation_degrees` from the positive X axis.
    Polygon {
        diameter: f64,
        vertices: u32,
        rotation_degrees: f64,
    },
    /// Rectangle with all four corners rounded to `radius`. A radius above
    /// `min(width, height) / 2` images clamped to that limit.
    RoundRect {
        width: f64,
        height: f64,
        radius: f64,
    },
    /// An arbitrary origin-local filled contour, shared by every flash of
    /// this aperture, painted under its source path's fill rule. This is
    /// how repeated dictionary instances stay instances all the way to the
    /// output.
    Contour {
        outline: ContourBuf,
        fill_rule: FillRule,
    },
}

impl Aperture {
    pub fn solid(shape: ApertureShape) -> Self {
        Self {
            shape,
            hole_diameter: 0.0,
        }
    }

    pub fn circle(diameter: f64) -> Self {
        Self::solid(ApertureShape::Circle { diameter })
    }

    /// Flatten to local-space contours. With a hole, the result is the outer
    /// shape plus the hole contour and must be filled with `EvenOdd`.
    pub fn contours(&self) -> Vec<ContourBuf> {
        let outer = match &self.shape {
            ApertureShape::Circle { diameter } => shapes::circle(*diameter),
            ApertureShape::Rectangle { width, height } => shapes::rect(*width, *height),
            ApertureShape::Obround { width, height } => shapes::obround(*width, *height, true),
            ApertureShape::Polygon {
                diameter,
                vertices,
                rotation_degrees,
            } => shapes::regular_polygon(*diameter, *vertices, *rotation_degrees),
            ApertureShape::RoundRect {
                width,
                height,
                radius,
            } => shapes::rounded_rect(*width, *height, *radius, shapes::ALL_CORNERS, true),
            ApertureShape::Contour { outline, .. } => return vec![outline.clone()],
        };
        let mut contours: Vec<ContourBuf> = outer.into_iter().collect();
        if !contours.is_empty() && self.hole_diameter > 0.0 {
            contours.extend(shapes::circle(self.hole_diameter));
        }
        contours
    }

    pub fn fill_rule(&self) -> FillRule {
        match &self.shape {
            ApertureShape::Contour { fill_rule, .. } => *fill_rule,
            _ if self.hole_diameter > 0.0 => FillRule::EvenOdd,
            _ => FillRule::NonZero,
        }
    }

    pub fn bbox(&self) -> BBox {
        match &self.shape {
            ApertureShape::Circle { diameter } => {
                BBox::from_point(Point::ZERO).expand(diameter / 2.0)
            }
            ApertureShape::Rectangle { width, height }
            | ApertureShape::Obround { width, height }
            | ApertureShape::RoundRect { width, height, .. } => BBox::new(
                Point::new(-width / 2.0, -height / 2.0),
                Point::new(width / 2.0, height / 2.0),
            ),
            ApertureShape::Polygon { diameter, .. } => {
                BBox::from_point(Point::ZERO).expand(diameter / 2.0)
            }
            ApertureShape::Contour { outline, .. } => outline.bbox,
        }
    }
}

/// Recompute object and layer bounds bottom-up (after arena mutation).
pub fn normalize_bounds<LayerMeta, ObjectMeta>(doc: &mut Document<LayerMeta, ObjectMeta>) {
    doc.arena.recompute_bounds();
    for block_index in 0..doc.blocks.len() {
        let bboxes = doc.blocks[block_index]
            .objects
            .iter()
            .map(|object| geometry_bbox(doc, object.geometry))
            .collect::<Vec<_>>();
        for (object, bbox) in doc.blocks[block_index].objects.iter_mut().zip(bboxes) {
            object.bbox = bbox;
        }
        doc.blocks[block_index].bbox = doc.blocks[block_index]
            .objects
            .iter()
            .fold(BBox::empty(), |bbox, object| bbox.union(object.bbox));
    }
    for object_index in 0..doc.objects.len() {
        doc.objects[object_index].bbox = geometry_bbox(doc, doc.objects[object_index].geometry);
    }
    for layer in &mut doc.layers {
        layer.bbox = layer
            .objects
            .slice(&doc.objects)
            .iter()
            .fold(BBox::empty(), |bbox, object| bbox.union(object.bbox));
    }
}

/// Rewrite flashes and strokes into filled region objects.
pub fn expand_native_geometry_to_regions<LayerMeta, ObjectMeta>(
    doc: Document<LayerMeta, ObjectMeta>,
) -> Document<LayerMeta, ObjectMeta>
where
    LayerMeta: Clone,
    ObjectMeta: Clone,
{
    let mut doc = if doc.blocks.is_empty() {
        doc
    } else {
        expand_instances(&doc)
    };
    expand_strokes_to_regions(&mut doc);
    expand_flashes_to_regions(&mut doc);
    normalize_bounds(&mut doc);
    doc
}

/// Compose ordered dark/clear objects into final positive per-layer images.
///
/// Objects paint stage by stage — [`PaintStage::Base`], then
/// [`PaintStage::Overlay`], then [`PaintStage::FinalCutout`] — preserving
/// paint order within each stage, so overlay objects survive base-stage
/// clears and final cutouts remove painted material. A layer containing only
/// final-cutout objects, such as a drill or rout document, images the
/// removals themselves.
/// Order a layer's objects for painting.
///
/// Dark paint commutes with dark paint and clear with clear, but not across
/// a polarity change, so stage ordering may only permute objects within each
/// maximal same-polarity run. Final cutouts are terminal by definition and
/// paint after everything.
pub fn paint_ordered<ObjectMeta>(objects: &[Object<ObjectMeta>]) -> Vec<&Object<ObjectMeta>> {
    let (cutouts, mut painted): (Vec<_>, Vec<_>) = objects
        .iter()
        .partition(|object| object.order.stage == PaintStage::FinalCutout);
    let mut start = 0;
    while start < painted.len() {
        let polarity = painted[start].polarity;
        let mut end = start + 1;
        while end < painted.len() && painted[end].polarity == polarity {
            end += 1;
        }
        painted[start..end].sort_by_key(|object| object.order.stage);
        start = end;
    }
    painted.extend(cutouts);
    painted
}

pub fn compose_to_mask<LayerMeta: Clone, ObjectMeta: Clone>(
    doc: &Document<LayerMeta, ObjectMeta>,
) -> mask::Document<LayerMeta> {
    let doc = expand_native_geometry_to_regions(doc.clone());
    let mut mask = mask::Document::new();

    for layer in &doc.layers {
        mask.push_layer(mask::Layer {
            name: layer.name.clone(),
            role: layer.role,
            side: layer.side,
            shapes: Span::EMPTY,
            bbox: BBox::empty(),
            meta: layer.meta.clone(),
        });
    }

    for (layer_index, layer) in doc.layers.iter().enumerate() {
        let objects = paint_ordered(layer.objects.slice(&doc.objects));
        let has_material = objects
            .iter()
            .any(|object| object.order.stage != PaintStage::FinalCutout);
        let mut composer = region::PaintComposer::default();
        for object in objects {
            let image = object_image_rings(&doc, object);
            if image.is_empty() {
                continue;
            }
            let polarity = if object.order.stage == PaintStage::FinalCutout && has_material {
                Polarity::Clear
            } else {
                object.polarity
            };
            composer.push(polarity, image);
        }

        let contours = region::rings_to_contours(composer.finish());
        if !contours.is_empty() {
            mask.push_shape(layer_index as u32, FillRule::NonZero, contours);
        }
    }

    mask.diagnostics.extend(doc.diagnostics);
    mask
}

fn expand_strokes_to_regions<LayerMeta, ObjectMeta>(doc: &mut Document<LayerMeta, ObjectMeta>) {
    for object_index in 0..doc.objects.len() {
        let Geometry::Stroke { path: path_index } = doc.objects[object_index].geometry else {
            continue;
        };
        let Some(path) = doc.arena.paths.get(path_index as usize).copied() else {
            doc.warn("Skipping artwork stroke with invalid path reference");
            continue;
        };
        let Some(stroke) = path.stroke() else {
            doc.warn("Skipping artwork stroke with fill paint");
            continue;
        };
        let Some(contours) =
            crate::geom::path::stroke_to_fill(&doc.arena.path_contours(&path), stroke.into())
        else {
            continue;
        };
        let path_id = doc.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            contours,
        );
        doc.objects[object_index].geometry = Geometry::Region { path: path_id };
        doc.objects[object_index].bbox = doc.path_bbox(path_id);
    }
}

fn expand_flashes_to_regions<LayerMeta, ObjectMeta>(doc: &mut Document<LayerMeta, ObjectMeta>) {
    for object_index in 0..doc.objects.len() {
        let Geometry::Flash {
            aperture,
            transform,
        } = doc.objects[object_index].geometry
        else {
            continue;
        };
        let Some(aperture) = doc.apertures.get(aperture as usize).cloned() else {
            doc.warn("Skipping artwork flash with invalid aperture reference");
            continue;
        };
        let contours = aperture
            .contours()
            .into_iter()
            .map(|contour| crate::geom::path::transform_cmds(contour.cmds, transform))
            .collect::<Vec<_>>();
        let path_id = doc.push_path(
            Paint::Fill {
                rule: aperture.fill_rule(),
            },
            contours,
        );
        doc.objects[object_index].geometry = Geometry::Region { path: path_id };
        doc.objects[object_index].bbox = doc.path_bbox(path_id);
    }
}

fn object_image_rings<LayerMeta, ObjectMeta>(
    doc: &Document<LayerMeta, ObjectMeta>,
    object: &Object<ObjectMeta>,
) -> Vec<Ring> {
    match object.geometry {
        Geometry::Region { path } => doc
            .arena
            .paths
            .get(path as usize)
            .map(|path| {
                region::simplify_rings(
                    region::rings_from_contours(&doc.arena.path_contours(path)),
                    path.fill_rule().unwrap_or(FillRule::NonZero),
                )
            })
            .unwrap_or_default(),
        Geometry::Flash { .. } | Geometry::Stroke { .. } | Geometry::Instance { .. } => Vec::new(),
    }
}

fn geometry_bbox<LayerMeta, ObjectMeta>(
    doc: &Document<LayerMeta, ObjectMeta>,
    geometry: Geometry,
) -> BBox {
    match geometry {
        Geometry::Region { path } | Geometry::Stroke { path } => doc
            .arena
            .paths
            .get(path as usize)
            .map(|path| path.bbox)
            .unwrap_or_else(BBox::empty),
        Geometry::Flash {
            aperture,
            transform,
        } => doc
            .apertures
            .get(aperture as usize)
            .map(|aperture| {
                aperture
                    .contours()
                    .into_iter()
                    .map(|contour| crate::geom::path::transform_cmds(contour.cmds, transform))
                    .fold(BBox::empty(), |bbox, contour| bbox.union(contour.bbox))
            })
            .unwrap_or_else(BBox::empty),
        Geometry::Instance { block, transform } => doc
            .blocks
            .get(block as usize)
            .map(|block| block.bbox.transformed(transform))
            .unwrap_or_else(BBox::empty),
    }
}

/// Materialize all reusable block instances into ordinary layer objects.
///
/// This is the explicit fallback for consumers that cannot preserve
/// hierarchy. Paths are transformed exactly once at each final placement;
/// aperture flashes retain composed affine transforms.
pub fn expand_instances<LayerMeta: Clone, ObjectMeta: Clone>(
    doc: &Document<LayerMeta, ObjectMeta>,
) -> Document<LayerMeta, ObjectMeta> {
    let mut out = Document::new();
    out.apertures = doc.apertures.clone();
    out.arena = doc.arena.clone();
    out.diagnostics = doc.diagnostics.clone();

    for layer in &doc.layers {
        out.push_layer(Layer {
            name: layer.name.clone(),
            role: layer.role,
            side: layer.side,
            objects: Span::EMPTY,
            bbox: BBox::empty(),
            meta: layer.meta.clone(),
        });
    }

    for (layer_index, layer) in doc.layers.iter().enumerate() {
        for object in layer.objects.slice(&doc.objects) {
            expand_object_into_layer(
                doc,
                &mut out,
                layer_index as u32,
                object,
                Affine2::IDENTITY,
                Polarity::Dark,
                doc.blocks.len(),
            );
        }
    }
    normalize_bounds(&mut out);
    out
}

fn expand_object_into_layer<LayerMeta, ObjectMeta: Clone>(
    source: &Document<LayerMeta, ObjectMeta>,
    target: &mut Document<LayerMeta, ObjectMeta>,
    layer: u32,
    object: &Object<ObjectMeta>,
    transform: Affine2,
    polarity: Polarity,
    block_limit: usize,
) {
    let polarity = polarity.compose(object.polarity);
    if let Geometry::Instance {
        block,
        transform: placement,
    } = object.geometry
    {
        let Some(block_definition) = source.blocks.get(block as usize) else {
            target.warn(format!(
                "Skipping artwork instance of missing block {block}"
            ));
            return;
        };
        if block as usize >= block_limit {
            target.warn(format!(
                "Skipping artwork instance of non-earlier block {block}"
            ));
            return;
        }
        let transform = transform.concat(placement);
        for child in &block_definition.objects {
            expand_object_into_layer(
                source,
                target,
                layer,
                child,
                transform,
                polarity,
                block as usize,
            );
        }
        return;
    }

    let geometry = match object.geometry {
        Geometry::Flash {
            aperture,
            transform: flash,
        } => Geometry::Flash {
            aperture,
            transform: transform.concat(flash),
        },
        Geometry::Stroke { path } | Geometry::Region { path } => {
            let path = if transform.is_identity() {
                path
            } else {
                let source_path = source.arena.path(path);
                let scale = transform.m00.hypot(transform.m10);
                target.push_path(
                    source_path.paint.scaled(scale),
                    source
                        .arena
                        .transformed_contour_bufs(source_path.contours, transform),
                )
            };
            match object.geometry {
                Geometry::Stroke { .. } => Geometry::Stroke { path },
                Geometry::Region { .. } => Geometry::Region { path },
                _ => unreachable!(),
            }
        }
        Geometry::Instance { .. } => unreachable!(),
    };
    target.push_object(
        layer,
        Object {
            polarity,
            order: object.order,
            geometry,
            bbox: BBox::empty(),
            meta: object.meta.clone(),
        },
    );
}

/// Convenience constructors for stroked paths shared by lowerings.
pub fn stroke_paint(width: f64, cap: crate::geom::LineCap) -> Paint {
    Paint::Stroke(StrokeStyle::new(width, cap))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::path::PathCmd;
    use crate::geom::{LineCap, LinePattern};

    #[test]
    fn stores_layers_objects_and_paths_in_fat_struct_arenas() {
        let mut doc = Document::<(), ()>::new();
        let layer = doc.push_layer(Layer::new("F.Cu", LayerRole::Copper, Side::Top));
        let path = doc.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            vec![ContourBuf::new(vec![
                PathCmd::move_to(Point::new(0.0, 0.0)),
                PathCmd::close(),
            ])],
        );

        doc.push_object(
            layer,
            Object::new(Polarity::Dark, Geometry::Region { path }),
        );

        assert_eq!(doc.layers[0].objects, Span::new(0, 1));
        assert_eq!(doc.objects.len(), 1);
        assert_eq!(doc.arena.path(path).contours.len(), 1);
        doc.validate().unwrap();
    }

    #[test]
    fn expanding_scaled_instances_scales_stroke_widths() {
        let mut doc = Document::<(), ()>::new();
        let block = doc.push_block();
        let path = doc.push_path(
            Paint::Stroke(crate::geom::StrokeStyle::round(0.2)),
            vec![ContourBuf::new(vec![
                PathCmd::move_to(Point::new(0.0, 0.0)),
                PathCmd::line_to(Point::new(1.0, 0.0)),
            ])],
        );
        doc.push_block_object(
            block,
            Object::new(Polarity::Dark, Geometry::Stroke { path }),
        );
        let layer = doc.push_layer(Layer::new("F.Cu", LayerRole::Copper, Side::Top));
        doc.push_object(
            layer,
            Object::new(
                Polarity::Dark,
                Geometry::Instance {
                    block,
                    transform: Affine2::placement(
                        Point::new(5.0, 5.0),
                        90.0,
                        crate::geom::Mirror::NONE,
                        2.0,
                    ),
                },
            ),
        );
        normalize_bounds(&mut doc);

        let expanded = expand_instances(&doc);
        let stroke = expanded
            .objects
            .iter()
            .find_map(|object| match object.geometry {
                Geometry::Stroke { path } => expanded.arena.path(path).paint.stroke(),
                _ => None,
            })
            .expect("expanded stroke object");
        assert!((stroke.width - 0.4).abs() <= 1e-9);
    }

    #[test]
    fn preserves_nested_reusable_blocks_until_explicit_expansion() {
        let mut doc = Document::<(), ()>::new();
        let aperture = doc.push_aperture(Aperture::circle(1.0));
        let board = doc.push_block();
        doc.push_block_object(
            board,
            Object::new(
                Polarity::Clear,
                Geometry::Flash {
                    aperture,
                    transform: Affine2::IDENTITY,
                },
            ),
        );

        let array = doc.push_block();
        for x in [0.0, 2.0] {
            doc.push_block_object(
                array,
                Object::new(
                    Polarity::Clear,
                    Geometry::Instance {
                        block: board,
                        transform: Affine2::translation(Point::new(x, 0.0)),
                    },
                ),
            );
        }

        let layer = doc.push_layer(Layer::new("F.Cu", LayerRole::Copper, Side::Top));
        doc.push_object(
            layer,
            Object::new(
                Polarity::Dark,
                Geometry::Instance {
                    block: array,
                    transform: Affine2::placement(
                        Point::new(10.0, 20.0),
                        90.0,
                        crate::geom::Mirror::NONE,
                        1.0,
                    ),
                },
            ),
        );
        normalize_bounds(&mut doc);
        doc.validate().unwrap();

        assert_eq!(doc.layers[0].bbox.min, Point::new(9.5, 19.5));
        assert_eq!(doc.layers[0].bbox.max, Point::new(10.5, 22.5));

        let expanded = expand_instances(&doc);
        assert_eq!(expanded.objects.len(), 2);
        assert!(
            expanded
                .objects
                .iter()
                .all(|object| object.polarity == Polarity::Dark),
            "clear block flashes toggle polarity through every nesting level"
        );
        assert_eq!(expanded.layers[0].bbox, doc.layers[0].bbox);
    }

    #[test]
    fn composes_ordered_artwork_to_mask() {
        let mut doc = Document::<(), ()>::new();
        let layer = doc.push_layer(Layer::new("F.Cu", LayerRole::Copper, Side::Top));
        let path = doc.push_path(
            Paint::Stroke(StrokeStyle::new(0.15, LineCap::Round)),
            vec![ContourBuf::new(vec![
                PathCmd::move_to(Point::new(0.0, 0.0)),
                PathCmd::line_to(Point::new(1.0, 0.0)),
            ])],
        );

        doc.push_object(
            layer,
            Object::new(Polarity::Dark, Geometry::Stroke { path }),
        );

        let mask = compose_to_mask(&doc);

        assert_eq!(mask.layers.len(), 1);
        assert_eq!(mask.layers[0].shapes.len(), 1);
        assert!(!mask.layers[0].bbox.is_empty());
        mask.validate().unwrap();
    }

    #[test]
    fn composition_stages_overlays_over_base_clears_and_final_cutouts_last() {
        let mut doc = Document::<(), ()>::new();
        let layer = doc.push_layer(Layer::new("F.Cu", LayerRole::Copper, Side::Top));
        let rect = |doc: &mut Document<(), ()>, x0: f64, y0: f64, x1: f64, y1: f64| {
            doc.push_path(
                Paint::Fill {
                    rule: FillRule::NonZero,
                },
                vec![ContourBuf::new(vec![
                    PathCmd::move_to(Point::new(x0, y0)),
                    PathCmd::line_to(Point::new(x1, y0)),
                    PathCmd::line_to(Point::new(x1, y1)),
                    PathCmd::line_to(Point::new(x0, y1)),
                    PathCmd::close(),
                ])],
            )
        };
        let stage_object = |polarity, path, stage| {
            let mut object = Object::new(polarity, Geometry::Region { path });
            object.order = PaintOrder { stage };
            object
        };

        // The thermal-relief pattern in its paint order: pour, clearance,
        // then the overlay trace over the cleared area, then a dark-drawn
        // final cutout. Stage ordering may only permute within polarity
        // runs, so the trace survives because it follows the clear.
        let base = rect(&mut doc, 0.0, 0.0, 10.0, 10.0);
        doc.push_object(layer, stage_object(Polarity::Dark, base, PaintStage::Base));
        let base_clear = rect(&mut doc, 3.0, 3.0, 7.0, 7.0);
        doc.push_object(
            layer,
            stage_object(Polarity::Clear, base_clear, PaintStage::Base),
        );
        let overlay = rect(&mut doc, 4.0, 4.0, 6.0, 6.0);
        doc.push_object(
            layer,
            stage_object(Polarity::Dark, overlay, PaintStage::Overlay),
        );
        let cutout = rect(&mut doc, 0.0, 0.0, 1.0, 1.0);
        doc.push_object(
            layer,
            stage_object(Polarity::Dark, cutout, PaintStage::FinalCutout),
        );

        let mask = compose_to_mask(&doc);
        let shape = mask.layers[0].shapes.slice(&mask.arena.paths)[0];
        let image = region::ContourSet::from_contours(
            &mask.arena.path_contours(&shape),
            FillRule::NonZero,
            crate::geom::tol::REGION_MM,
        );

        // The overlay trace follows the clear in paint order and survives.
        assert!(image.contains_point(Point::new(5.0, 5.0)));
        // The base clear still removes material around the overlay.
        assert!(!image.contains_point(Point::new(3.5, 5.0)));
        // The dark-drawn final cutout removes material.
        assert!(!image.contains_point(Point::new(0.5, 0.5)));
        assert!(image.contains_point(Point::new(9.0, 9.0)));
    }

    #[test]
    fn flash_expansion_honors_aperture_holes() {
        let mut doc = Document::<(), ()>::new();
        let layer = doc.push_layer(Layer::new("F.Cu", LayerRole::Copper, Side::Top));
        let aperture = doc.push_aperture(Aperture {
            shape: ApertureShape::Circle { diameter: 2.0 },
            hole_diameter: 1.0,
        });
        doc.push_object(
            layer,
            Object::new(
                Polarity::Dark,
                Geometry::Flash {
                    aperture,
                    transform: Affine2::IDENTITY,
                },
            ),
        );

        let mask = compose_to_mask(&doc);
        let expected = std::f64::consts::PI * (1.0 - 0.25);
        let shape = mask.layers[0].shapes.slice(&mask.arena.paths)[0];
        let area = region::ContourSet::from_contours(
            &mask.arena.path_contours(&shape),
            FillRule::NonZero,
            crate::geom::tol::REGION_MM,
        )
        .area();

        assert!(
            (area - expected).abs() < 0.02,
            "expected annulus area ~{expected}, got {area}"
        );
    }

    #[test]
    fn aperture_definitions_are_deduplicated() {
        let mut doc = Document::<(), ()>::new();

        let a = doc.push_aperture(Aperture::circle(1.5));
        let b = doc.push_aperture(Aperture::circle(1.5));
        let c = doc.push_aperture(Aperture::circle(2.0));

        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(doc.apertures.len(), 2);
    }

    #[test]
    fn stroked_paths_preserve_line_pattern() {
        let stroke = StrokeStyle {
            width: 0.1,
            cap: LineCap::Round,
            join: crate::geom::LineJoin::Round,
            pattern: LinePattern::Phantom,
        };
        let path = Path::stroked(stroke);

        assert_eq!(path.stroke().unwrap().pattern, LinePattern::Phantom);
    }

    use crate::geom::Path;
}
