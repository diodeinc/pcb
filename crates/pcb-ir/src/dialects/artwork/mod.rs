//! Source-independent ordered fabrication artwork.
//!
//! This dialect intentionally keeps an object stream instead of immediately
//! flattening everything into polygons. It is the common interchange target
//! for source dialects such as IPC-2581 and Gerber when we still care about
//! idiomatic fabrication objects: flashes, strokes, regions, and ordered
//! dark/clear paint operations.

pub mod compare;
pub mod legalize;

use std::collections::HashMap;
use std::hash::Hash;

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
        Geometry::Instance { block, .. } | Geometry::GridInstance { block, .. }
            if block as usize >= doc.blocks.len() =>
        {
            Some(format!("references missing block {block}"))
        }
        Geometry::Instance { block, .. } | Geometry::GridInstance { block, .. }
            if block as usize >= block_limit =>
        {
            Some(format!(
                "references block {block}; reusable blocks must reference earlier blocks"
            ))
        }
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

/// A regular two-axis repetition of one reusable artwork block.
///
/// Step vectors are expressed in the instance's parent coordinate system.
/// Keeping them as vectors allows outer panel placements to rotate the grid
/// without expanding its members.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridRepeat {
    pub x_count: u32,
    pub y_count: u32,
    pub x_step: Point,
    pub y_step: Point,
}

impl GridRepeat {
    /// Translation of occurrence `(ix, iy)` relative to the grid's base
    /// placement.
    pub fn offset(self, ix: u32, iy: u32) -> Point {
        Point::new(
            ix as f64 * self.x_step.x + iy as f64 * self.y_step.x,
            ix as f64 * self.x_step.y + iy as f64 * self.y_step.y,
        )
    }

    /// Translations of every occurrence, x-fastest.
    pub fn offsets(self) -> impl Iterator<Item = Point> {
        (0..self.y_count).flat_map(move |iy| (0..self.x_count).map(move |ix| self.offset(ix, iy)))
    }

    /// Bounds of one occurrence's `base` bounds repeated across the grid.
    pub fn bbox(self, base: BBox) -> BBox {
        let x = self.x_count.saturating_sub(1);
        let y = self.y_count.saturating_sub(1);
        [(0, 0), (x, 0), (0, y), (x, y)]
            .into_iter()
            .map(|(ix, iy)| self.offset(ix, iy))
            .fold(BBox::empty(), |bbox, offset| {
                bbox.union(BBox::new(base.min + offset, base.max + offset))
            })
    }
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
    /// A reusable ordered sub-image placed on a regular grid.
    GridInstance {
        block: u32,
        transform: Affine2,
        repeat: GridRepeat,
    },
}

impl Geometry {
    pub fn path(self) -> Option<u32> {
        match self {
            Self::Flash { .. } | Self::Instance { .. } | Self::GridInstance { .. } => None,
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
    /// Regular hexagon with circularly rounded corners. `radius` is the
    /// center-to-vertex radius before rounding.
    RoundedHex {
        radius: f64,
        corner_radius: f64,
        rotation_degrees: f64,
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
            ApertureShape::RoundedHex {
                radius,
                corner_radius,
                rotation_degrees,
            } => shapes::rounded_hexagon(*radius, *corner_radius, *rotation_degrees),
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
            ApertureShape::RoundedHex {
                radius,
                corner_radius,
                rotation_degrees,
            } => shapes::rounded_hexagon(*radius, *corner_radius, *rotation_degrees)
                .map_or_else(BBox::empty, |contour| contour.bbox),
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
    expand_native_geometry_with_accuracy(doc, None).expect("default artwork preparation")
}

fn expand_native_geometry_with_accuracy<LayerMeta: Clone, ObjectMeta: Clone>(
    doc: Document<LayerMeta, ObjectMeta>,
    accuracy: Option<crate::geom::GeometryAccuracy>,
) -> Result<Document<LayerMeta, ObjectMeta>, crate::geom::AccuracyError> {
    let mut doc = if doc.blocks.is_empty() {
        doc
    } else {
        expand_instances_with_grid_policy(&doc, false, accuracy)?
    };
    expand_strokes_to_regions(&mut doc, accuracy)?;
    expand_flashes_to_regions(&mut doc, accuracy)?;
    normalize_bounds(&mut doc);
    Ok(doc)
}

/// Compose ordered dark/clear objects into final positive per-layer images.
///
/// Objects paint stage by stage — [`PaintStage::Base`], then
/// [`PaintStage::Overlay`], then [`PaintStage::FinalCutout`] — preserving
/// paint order within each stage, so overlay objects survive base-stage
/// clears and final cutouts remove painted material. A non-copper layer
/// containing only final-cutout objects, such as a drill or rout document,
/// images the removals themselves; cutout-only copper layers remain empty.
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

/// One fully composed layer image together with the surviving material
/// attributed to each caller-defined owner. Owner images may overlap when
/// different source objects claim the same physical copper; their union is
/// always exactly `image`.
#[derive(Debug)]
pub struct AttributedImage<Owner> {
    pub image: Vec<Ring>,
    pub owners: Vec<(Owner, Vec<Ring>)>,
}

/// Ordered artwork composition with caller-defined ownership.
///
/// Dark objects add material to their owner. Clear objects and final cutouts
/// subtract from every owner painted before them. This is the same canonical
/// paint fold used by [`compose_to_mask`], with labels retained instead of
/// discarded after unioning.
pub fn compose_attributed<LayerMeta: Clone, ObjectMeta: Clone, Owner: Clone + Eq + Hash>(
    doc: &Document<LayerMeta, ObjectMeta>,
    owner: impl Fn(&ObjectMeta) -> Owner,
) -> (Vec<AttributedImage<Owner>>, Vec<Diagnostic>) {
    compose_selected_attributed(doc, |meta| Some(owner(meta)))
}

/// Ordered artwork composition for only the owners selected by the caller,
/// with each layer's image as the union of its owners.
///
/// Unselected dark objects cannot change a selected owner's image and are
/// skipped. Clear objects and final cutouts still subtract from selected
/// owners in source paint order.
pub(crate) fn compose_selected_attributed<
    LayerMeta: Clone,
    ObjectMeta: Clone,
    Owner: Clone + Eq + Hash,
>(
    doc: &Document<LayerMeta, ObjectMeta>,
    owner: impl Fn(&ObjectMeta) -> Option<Owner>,
) -> (Vec<AttributedImage<Owner>>, Vec<Diagnostic>) {
    let (layers, diagnostics) = compose_selected_owners(doc, owner);
    let layers = layers
        .into_iter()
        .map(|owners| AttributedImage {
            image: region::union_rings(
                owners
                    .iter()
                    .flat_map(|(_, image)| image.iter().cloned())
                    .collect(),
                FillRule::NonZero,
            ),
            owners,
        })
        .collect();
    (layers, diagnostics)
}

/// One layer's owner images in first-paint order; every image is a
/// regularized ring set.
pub type OwnerImages<Owner> = Vec<(Owner, Vec<Ring>)>;

pub type OwnerRegionLayers<Owner> = Vec<Vec<(Owner, region::ContourSet)>>;

/// Each layer's owner images from the ordered paint fold, for only the
/// owners selected by the caller.
pub fn compose_selected_owners<LayerMeta: Clone, ObjectMeta: Clone, Owner: Clone + Eq + Hash>(
    doc: &Document<LayerMeta, ObjectMeta>,
    owner: impl Fn(&ObjectMeta) -> Option<Owner>,
) -> (Vec<OwnerImages<Owner>>, Vec<Diagnostic>) {
    let (layers, diagnostics) =
        compose_owner_regions(doc, owner, 0.0, None).expect("default artwork preparation");
    (
        layers
            .into_iter()
            .map(|owners| {
                owners
                    .into_iter()
                    .map(|(owner, region)| (owner, region.rings))
                    .collect()
            })
            .collect(),
        diagnostics,
    )
}

/// Compose owner regions using retained source curves and an explicit budget.
/// Returned regions carry the errors of strokes, transforms, and paint folds.
/// `tolerance` controls ring significance independently of approximation.
pub fn compose_owner_regions<LayerMeta: Clone, ObjectMeta: Clone, Owner: Clone + Eq + Hash>(
    doc: &Document<LayerMeta, ObjectMeta>,
    owner: impl Fn(&ObjectMeta) -> Option<Owner>,
    tolerance: f64,
    accuracy: Option<crate::geom::GeometryAccuracy>,
) -> Result<(OwnerRegionLayers<Owner>, Vec<Diagnostic>), crate::geom::AccuracyError> {
    if !tolerance.is_finite() || tolerance < 0.0 {
        return Err(crate::geom::AccuracyError::InvalidGeometry(
            "invalid significance tolerance",
        ));
    }
    let doc = expand_native_geometry_with_accuracy(doc.clone(), accuracy)?;
    struct OwnerState {
        composer: region::PaintComposer,
        bbox: BBox,
    }

    let mut layers = Vec::with_capacity(doc.layers.len());
    for layer in &doc.layers {
        let objects = paint_ordered(layer.objects.slice(&doc.objects));
        let has_material = objects
            .iter()
            .any(|object| object.order.stage != PaintStage::FinalCutout);
        // Preserve first-paint order for deterministic attributed output and
        // retain constant-time lookup for later objects of the same owner.
        let mut owner_indices: HashMap<Owner, usize> = HashMap::new();
        let mut states: Vec<(Owner, OwnerState)> = Vec::new();
        for object in objects {
            let polarity = if object.order.stage == PaintStage::FinalCutout
                && (has_material || layer.role == LayerRole::Copper)
            {
                Polarity::Clear
            } else {
                object.polarity
            };
            let selected_owner = match polarity {
                Polarity::Dark => {
                    let Some(owner) = owner(&object.meta) else {
                        continue;
                    };
                    Some(owner)
                }
                Polarity::Clear => None,
            };
            let image = object_image_region(&doc, object, accuracy)?;
            if image.is_empty() {
                continue;
            }
            match polarity {
                Polarity::Dark => {
                    let owner = selected_owner.expect("dark object has a selected owner");
                    let index = match owner_indices.get(&owner) {
                        Some(&index) => index,
                        None => {
                            let index = states.len();
                            owner_indices.insert(owner.clone(), index);
                            states.push((
                                owner,
                                OwnerState {
                                    composer: region::PaintComposer::default(),
                                    bbox: BBox::empty(),
                                },
                            ));
                            index
                        }
                    };
                    let state = &mut states[index].1;
                    state.bbox = state.bbox.union(object.bbox);
                    state.composer.push_region(Polarity::Dark, image);
                }
                Polarity::Clear => {
                    for state in states
                        .iter_mut()
                        .map(|(_, state)| state)
                        .filter(|state| state.bbox.intersects(object.bbox))
                    {
                        state.composer.push_region(Polarity::Clear, image.clone());
                    }
                }
            }
        }

        let mut images = Vec::with_capacity(states.len());
        for (owner, state) in states {
            let image = state.composer.finish_set(tolerance);
            if let Some(accuracy) = accuracy {
                accuracy.check(image.uncertainty_mm)?;
            }
            if !image.is_empty() {
                images.push((owner, image));
            }
        }
        layers.push(images);
    }
    Ok((layers, doc.diagnostics))
}

pub fn compose_to_mask<LayerMeta: Clone, ObjectMeta: Clone>(
    doc: &Document<LayerMeta, ObjectMeta>,
) -> mask::Document<LayerMeta> {
    let (images, diagnostics) = compose_attributed(doc, |_| ());
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

    for (layer_index, image) in images.into_iter().enumerate() {
        let contours = region::rings_to_contours(image.image);
        if !contours.is_empty() {
            mask.push_shape(layer_index as u32, FillRule::NonZero, contours);
        }
    }

    mask.diagnostics.extend(diagnostics);
    mask
}

fn expand_strokes_to_regions<LayerMeta, ObjectMeta>(
    doc: &mut Document<LayerMeta, ObjectMeta>,
    accuracy: Option<crate::geom::GeometryAccuracy>,
) -> Result<(), crate::geom::AccuracyError> {
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
        let source = doc.arena.path_contours(&path);
        let contours = match accuracy {
            Some(accuracy) => {
                crate::geom::path::stroke_to_fill_with_accuracy(&source, stroke.into(), accuracy)?
            }
            None => crate::geom::path::stroke_to_fill(&source, stroke.into()),
        };
        let Some(contours) = contours else {
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
    Ok(())
}

fn transform_contours(
    contours: Vec<ContourBuf>,
    transform: Affine2,
    accuracy: Option<crate::geom::GeometryAccuracy>,
) -> Result<Vec<ContourBuf>, crate::geom::AccuracyError> {
    contours
        .into_iter()
        .map(|contour| match accuracy {
            Some(accuracy) => contour.transformed_with_accuracy(transform, accuracy),
            None => Ok(contour.transformed(transform)),
        })
        .collect()
}

fn expand_flashes_to_regions<LayerMeta, ObjectMeta>(
    doc: &mut Document<LayerMeta, ObjectMeta>,
    accuracy: Option<crate::geom::GeometryAccuracy>,
) -> Result<(), crate::geom::AccuracyError> {
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
        let contours = transform_contours(aperture.contours(), transform, accuracy)?;
        let path_id = doc.push_path(
            Paint::Fill {
                rule: aperture.fill_rule(),
            },
            contours,
        );
        doc.objects[object_index].geometry = Geometry::Region { path: path_id };
        doc.objects[object_index].bbox = doc.path_bbox(path_id);
    }
    Ok(())
}

fn object_image_region<LayerMeta, ObjectMeta>(
    doc: &Document<LayerMeta, ObjectMeta>,
    object: &Object<ObjectMeta>,
    accuracy: Option<crate::geom::GeometryAccuracy>,
) -> Result<region::ContourSet, crate::geom::AccuracyError> {
    let Geometry::Region { path } = object.geometry else {
        return Ok(region::ContourSet::empty(0.0));
    };
    let Some(path) = doc.arena.paths.get(path as usize) else {
        return Ok(region::ContourSet::empty(0.0));
    };
    let contours = doc.arena.path_contours(path);
    let rule = path.fill_rule().unwrap_or(FillRule::NonZero);
    region::ContourSet::prepare_contours(&contours, rule, 0.0, accuracy)
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
                    .map(|contour| contour.transformed(transform))
                    .fold(BBox::empty(), |bbox, contour| bbox.union(contour.bbox))
            })
            .unwrap_or_else(BBox::empty),
        Geometry::Instance { block, transform } => doc
            .blocks
            .get(block as usize)
            .map(|block| block.bbox.transformed(transform))
            .unwrap_or_else(BBox::empty),
        Geometry::GridInstance {
            block,
            transform,
            repeat,
        } => doc
            .blocks
            .get(block as usize)
            .map(|block| repeat.bbox(block.bbox.transformed(transform)))
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
    expand_instances_with_grid_policy(doc, false, None).expect("default instance expansion")
}

/// Materialize ordinary hierarchy while preserving explicit regular grids.
///
/// The result contains no ordinary instances anywhere: nested grids
/// materialize until each retained grid references a primitive-only block,
/// with outer transforms composed into its base placement, step vectors,
/// and block contents.
pub fn expand_instances_preserving_grids<LayerMeta: Clone, ObjectMeta: Clone>(
    doc: &Document<LayerMeta, ObjectMeta>,
) -> Document<LayerMeta, ObjectMeta> {
    expand_instances_with_grid_policy(doc, true, None).expect("default instance expansion")
}

fn expand_instances_with_grid_policy<LayerMeta: Clone, ObjectMeta: Clone>(
    doc: &Document<LayerMeta, ObjectMeta>,
    preserve_grids: bool,
    accuracy: Option<crate::geom::GeometryAccuracy>,
) -> Result<Document<LayerMeta, ObjectMeta>, crate::geom::AccuracyError> {
    let mut out = Document::new();
    out.apertures = doc.apertures.clone();
    out.arena = doc.arena.clone();
    out.diagnostics = doc.diagnostics.clone();
    let mut block_contains_grid = Vec::with_capacity(doc.blocks.len());
    for block in &doc.blocks {
        block_contains_grid.push(block.objects.iter().any(|object| {
            match object.geometry {
                Geometry::GridInstance { .. } => true,
                Geometry::Instance { block, .. } => block_contains_grid
                    .get(block as usize)
                    .copied()
                    .unwrap_or(false),
                _ => false,
            }
        }));
    }
    if preserve_grids {
        flatten_leaf_blocks(doc, &mut out, &block_contains_grid, accuracy)?;
    }

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
                InstanceExpansion {
                    transform: Affine2::IDENTITY,
                    polarity: Polarity::Dark,
                    preserve_grids,
                    accuracy,
                    block_contains_grid: &block_contains_grid,
                },
                doc.blocks.len(),
            )?;
        }
    }
    normalize_bounds(&mut out);
    Ok(out)
}

/// Rebuild every grid-free block as primitive-only geometry so retained
/// grids never require a hierarchy walk in consumers. Blocks containing
/// grids are copied verbatim; expansion never retains a grid of them.
fn flatten_leaf_blocks<LayerMeta, ObjectMeta: Clone>(
    doc: &Document<LayerMeta, ObjectMeta>,
    out: &mut Document<LayerMeta, ObjectMeta>,
    block_contains_grid: &[bool],
    accuracy: Option<crate::geom::GeometryAccuracy>,
) -> Result<(), crate::geom::AccuracyError> {
    for (index, block) in doc.blocks.iter().enumerate() {
        let id = out.push_block();
        for object in &block.objects {
            match object.geometry {
                Geometry::Instance {
                    block: child,
                    transform,
                } if !block_contains_grid[index] => {
                    if child as usize >= index {
                        out.warn(format!(
                            "Skipping artwork instance of non-earlier block {child}"
                        ));
                        continue;
                    }
                    let children = out.blocks[child as usize].objects.clone();
                    for child_object in children {
                        let geometry = transform_primitive_geometry(
                            out,
                            child_object.geometry,
                            transform,
                            accuracy,
                        )?;
                        out.push_block_object(
                            id,
                            Object {
                                polarity: object.polarity.compose(child_object.polarity),
                                order: child_object.order,
                                geometry,
                                bbox: BBox::empty(),
                                meta: child_object.meta,
                            },
                        );
                    }
                }
                _ => {
                    out.push_block_object(id, object.clone());
                }
            }
        }
    }
    Ok(())
}

/// Apply `transform` to primitive geometry within one document, copying
/// transformed paths into its arena.
fn transform_primitive_geometry<LayerMeta, ObjectMeta>(
    doc: &mut Document<LayerMeta, ObjectMeta>,
    geometry: Geometry,
    transform: Affine2,
    accuracy: Option<crate::geom::GeometryAccuracy>,
) -> Result<Geometry, crate::geom::AccuracyError> {
    Ok(match geometry {
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
                let source_path = doc.arena.path(path);
                let paint = source_path.paint.scaled(transform.m00.hypot(transform.m10));
                let contours =
                    transform_contours(doc.arena.path_contours(source_path), transform, accuracy)?;
                doc.push_path(paint, contours)
            };
            match geometry {
                Geometry::Stroke { .. } => Geometry::Stroke { path },
                Geometry::Region { .. } => Geometry::Region { path },
                _ => unreachable!(),
            }
        }
        Geometry::Instance { .. } | Geometry::GridInstance { .. } => {
            unreachable!("flattened blocks contain only primitive geometry")
        }
    })
}

#[derive(Clone, Copy)]
struct InstanceExpansion<'a> {
    transform: Affine2,
    polarity: Polarity,
    preserve_grids: bool,
    block_contains_grid: &'a [bool],
    accuracy: Option<crate::geom::GeometryAccuracy>,
}

fn expand_object_into_layer<LayerMeta, ObjectMeta: Clone>(
    source: &Document<LayerMeta, ObjectMeta>,
    target: &mut Document<LayerMeta, ObjectMeta>,
    layer: u32,
    object: &Object<ObjectMeta>,
    expansion: InstanceExpansion<'_>,
    block_limit: usize,
) -> Result<(), crate::geom::AccuracyError> {
    let transform = expansion.transform;
    let polarity = expansion.polarity.compose(object.polarity);
    if let Geometry::Instance {
        block,
        transform: placement,
    } = object.geometry
    {
        let Some(block_definition) = source.blocks.get(block as usize) else {
            target.warn(format!(
                "Skipping artwork instance of missing block {block}"
            ));
            return Ok(());
        };
        if block as usize >= block_limit {
            target.warn(format!(
                "Skipping artwork instance of non-earlier block {block}"
            ));
            return Ok(());
        }
        let transform = transform.concat(placement);
        for child in &block_definition.objects {
            expand_object_into_layer(
                source,
                target,
                layer,
                child,
                InstanceExpansion {
                    transform,
                    polarity,
                    ..expansion
                },
                block as usize,
            )?;
        }
        return Ok(());
    }

    if let Geometry::GridInstance {
        block,
        transform: placement,
        repeat,
    } = object.geometry
    {
        let Some(block_definition) = source.blocks.get(block as usize) else {
            target.warn(format!(
                "Skipping artwork grid instance of missing block {block}"
            ));
            return Ok(());
        };
        if block as usize >= block_limit {
            target.warn(format!(
                "Skipping artwork grid instance of non-earlier block {block}"
            ));
            return Ok(());
        }
        if expansion.preserve_grids
            && !expansion
                .block_contains_grid
                .get(block as usize)
                .copied()
                .unwrap_or(false)
        {
            target.push_object(
                layer,
                Object {
                    polarity,
                    order: object.order,
                    geometry: Geometry::GridInstance {
                        block,
                        transform: transform.concat(placement),
                        repeat: GridRepeat {
                            x_count: repeat.x_count,
                            y_count: repeat.y_count,
                            x_step: transform.transform_vector(repeat.x_step),
                            y_step: transform.transform_vector(repeat.y_step),
                        },
                    },
                    bbox: BBox::empty(),
                    meta: object.meta.clone(),
                },
            );
            return Ok(());
        }
        for offset in repeat.offsets() {
            let placement = Affine2 {
                m02: placement.m02 + offset.x,
                m12: placement.m12 + offset.y,
                ..placement
            };
            let occurrence = transform.concat(placement);
            for child in &block_definition.objects {
                expand_object_into_layer(
                    source,
                    target,
                    layer,
                    child,
                    InstanceExpansion {
                        transform: occurrence,
                        polarity,
                        ..expansion
                    },
                    block as usize,
                )?;
            }
        }
        return Ok(());
    }

    // The target arena starts as a clone of the source arena, so source path
    // indices resolve identically in the target.
    let geometry =
        transform_primitive_geometry(target, object.geometry, transform, expansion.accuracy)?;
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
    Ok(())
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
    fn attributed_composition_preserves_owner_claims_through_clear_and_overlay() {
        let mut doc = Document::<(), &'static str>::new();
        let layer = doc.push_layer(Layer::new("F.Cu", LayerRole::Copper, Side::Top));
        let rect = |doc: &mut Document<(), &'static str>, x0, y0, x1, y1| {
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
        let object = |polarity, path, stage, meta| {
            let mut object = Object::new(polarity, Geometry::Region { path });
            object.order = PaintOrder { stage };
            object.meta = meta;
            object
        };

        let base = rect(&mut doc, 0.0, 0.0, 10.0, 10.0);
        doc.push_object(layer, object(Polarity::Dark, base, PaintStage::Base, "A"));
        let clear = rect(&mut doc, 3.0, 3.0, 7.0, 7.0);
        doc.push_object(
            layer,
            object(Polarity::Clear, clear, PaintStage::Base, "ignored"),
        );
        let overlay = rect(&mut doc, 4.0, 4.0, 6.0, 6.0);
        doc.push_object(
            layer,
            object(Polarity::Dark, overlay, PaintStage::Overlay, "B"),
        );
        let overlap = rect(&mut doc, 8.0, 8.0, 9.0, 9.0);
        doc.push_object(
            layer,
            object(Polarity::Dark, overlap, PaintStage::Overlay, "C"),
        );

        let (mut layers, diagnostics) = compose_attributed(&doc, |meta| *meta);
        assert!(diagnostics.is_empty());
        let composed = layers.remove(0);
        let physical = region::ContourSet::new(
            composed.image,
            FillRule::NonZero,
            crate::geom::tol::REGION_MM,
        );
        assert_eq!(
            composed
                .owners
                .iter()
                .map(|(owner, _)| *owner)
                .collect::<Vec<_>>(),
            ["A", "B", "C"]
        );
        let owners = composed
            .owners
            .into_iter()
            .map(|(owner, rings)| {
                (
                    owner,
                    region::ContourSet::new(rings, FillRule::NonZero, crate::geom::tol::REGION_MM),
                )
            })
            .collect::<HashMap<_, _>>();

        assert!(owners["A"].contains_point(Point::new(2.0, 2.0)));
        assert!(!owners["A"].contains_point(Point::new(5.0, 5.0)));
        assert!(owners["B"].contains_point(Point::new(5.0, 5.0)));
        assert!(owners["A"].contains_point(Point::new(8.5, 8.5)));
        assert!(owners["C"].contains_point(Point::new(8.5, 8.5)));
        assert!(physical.contains_point(Point::new(5.0, 5.0)));
        assert!(physical.contains_point(Point::new(8.5, 8.5)));

        let (mut selected, diagnostics) =
            compose_selected_attributed(&doc, |meta| (*meta != "C").then_some(*meta));
        assert!(diagnostics.is_empty());
        let selected = selected.remove(0);
        assert_eq!(
            selected
                .owners
                .iter()
                .map(|(owner, _)| *owner)
                .collect::<Vec<_>>(),
            ["A", "B"]
        );
        let selected = selected
            .owners
            .into_iter()
            .map(|(owner, rings)| {
                (
                    owner,
                    region::ContourSet::new(rings, FillRule::NonZero, crate::geom::tol::REGION_MM),
                )
            })
            .collect::<HashMap<_, _>>();
        assert!(!selected["A"].contains_point(Point::new(5.0, 5.0)));
        assert!(selected["B"].contains_point(Point::new(5.0, 5.0)));
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
