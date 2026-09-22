//! Source-independent ordered fabrication artwork.
//!
//! This dialect intentionally keeps an object stream instead of immediately
//! flattening everything into polygons. It is the common interchange target
//! for source dialects such as IPC-2581 and Gerber when we still care about
//! idiomatic fabrication objects: flashes, strokes, regions, and ordered
//! dark/clear paint operations.

pub mod compare;
pub mod legalize;

use crate::geom::{AccuracyError, Resolution};
use std::collections::HashMap;
use std::hash::Hash;

use crate::dialects::mask;
use crate::dialects::{LayerRole, Side};
use crate::geom::path::ContourBuf;
use crate::geom::region::{self};
use crate::geom::{
    Affine2, BBox, Diagnostic, FillRule, Paint, PathArena, Point, Polarity, Span, shapes,
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
        for (index, aperture) in self.apertures.iter().enumerate() {
            if !aperture.hole_fits() {
                diagnostics.error(format!(
                    "artwork aperture {index} has a hole reaching outside its shape"
                ));
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

/// Where an object stands in its layer's paint.
///
/// Paint is sequential: a clear removes whatever was painted before it, in
/// source order, whatever the stages involved. `Base` and `Overlay` only
/// say what kind of material an object is, for targets that group their
/// output by it; `FinalCutout` alone changes the image, by painting after
/// everything else.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum PaintStage {
    /// Base images such as pours.
    Base,
    /// Pads, vias, traces, fiducials.
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
        if self.x_count == 0 || self.y_count == 0 {
            return BBox::empty();
        }
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

/// A standard aperture: a primitive shape with an optional round hole.
#[derive(Debug, Clone, PartialEq)]
pub struct Aperture {
    pub shape: ApertureShape,
    /// Diameter of the round hole through the aperture; `0.0` means solid.
    /// The hole lies inside the shape ([`Aperture::hole_fits`]): the image is
    /// the shape less the hole, which even-odd fill gives only then.
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
            ApertureShape::Obround { width, height } => shapes::obround(*width, *height),
            ApertureShape::Polygon {
                diameter,
                vertices,
                rotation_degrees,
            } => shapes::regular_polygon(*diameter, *vertices, *rotation_degrees),
            ApertureShape::RoundRect {
                width,
                height,
                radius,
            } => shapes::rounded_rect(*width, *height, *radius, shapes::ALL_CORNERS),
            ApertureShape::Contour { outline, .. } => return vec![outline.clone()],
        };
        let mut contours: Vec<ContourBuf> = outer.into_iter().collect();
        if !contours.is_empty() && self.hole_diameter > 0.0 {
            contours.extend(shapes::circle(self.hole_diameter));
        }
        contours
    }

    /// Whether the hole lies inside the shape, so that filling the two
    /// even-odd images the shape less the hole. A hole reaching outside
    /// would paint its overhang, so such an image is not an aperture with a
    /// hole; its source composes it into a contour instead.
    pub fn hole_fits(&self) -> bool {
        let inscribed = match &self.shape {
            ApertureShape::Circle { diameter } => *diameter,
            ApertureShape::Rectangle { width, height }
            | ApertureShape::Obround { width, height }
            | ApertureShape::RoundRect { width, height, .. } => width.min(*height),
            ApertureShape::Polygon {
                diameter, vertices, ..
            } => diameter * (std::f64::consts::PI / f64::from(*vertices)).cos(),
            ApertureShape::Contour { .. } => return self.hole_diameter <= 0.0,
        };
        self.hole_diameter <= inscribed
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
            ApertureShape::Circle { diameter } | ApertureShape::Polygon { diameter, .. } => {
                BBox::from_point(Point::ZERO).expand(diameter / 2.0)
            }
            ApertureShape::Rectangle { width, height }
            | ApertureShape::Obround { width, height }
            | ApertureShape::RoundRect { width, height, .. } => BBox::new(
                Point::new(-width / 2.0, -height / 2.0),
                Point::new(width / 2.0, height / 2.0),
            ),
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

/// A layer's objects in paint order, each with the polarity it images with.
///
/// Objects paint in source order, then the final cutouts. A final cutout
/// removes material wherever the layer has any, and on every copper layer;
/// on a layer of nothing but cutouts, such as a drill or rout document, the
/// cutouts are the image and keep their own polarity.
pub fn paint_ordered<'a, LayerMeta, ObjectMeta>(
    layer: &Layer<LayerMeta>,
    objects: &'a [Object<ObjectMeta>],
) -> Vec<(Polarity, &'a Object<ObjectMeta>)> {
    let (cutouts, painted): (Vec<_>, Vec<_>) = objects
        .iter()
        .partition(|object| object.order.stage == PaintStage::FinalCutout);
    let removes = !painted.is_empty() || layer.role == LayerRole::Copper;
    painted
        .into_iter()
        .map(|object| (object.polarity, object))
        .chain(cutouts.into_iter().map(|object| {
            let polarity = if removes {
                Polarity::Clear
            } else {
                object.polarity
            };
            (polarity, object)
        }))
        .collect()
}

/// One layer's owner images in first-paint order; every image is a
/// regularized ring set.
pub type OwnerImages<Owner> = Vec<(Owner, region::ContourSet)>;

pub type OwnerRegionLayers<Owner> = Vec<OwnerImages<Owner>>;

/// A flash or path as one placement images it.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Primitive {
    Flash(u32),
    Path(u32),
}

/// One primitive of a layer's paint, with every enclosing block instance
/// resolved into its polarity and placement.
pub(crate) struct Placed<'a, ObjectMeta> {
    pub(crate) polarity: Polarity,
    pub(crate) primitive: Primitive,
    pub(crate) transform: Affine2,
    /// Whether another placement can image the same primitive: apertures and
    /// block content are shared, a layer's own paths are not.
    shared: bool,
    meta: &'a ObjectMeta,
}

/// A layer's paint as the sequence of primitives that images it: what the
/// region fold composes and what a raster paints, in the same order.
pub(crate) fn placed_layer<'a, LayerMeta, ObjectMeta>(
    doc: &'a Document<LayerMeta, ObjectMeta>,
    layer: &Layer<LayerMeta>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<Placed<'a, ObjectMeta>> {
    let mut placed = Vec::new();
    for (polarity, object) in paint_ordered(layer, layer.objects.slice(&doc.objects)) {
        // A final cutout images clear whatever its own polarity says.
        let context = polarity.compose(object.polarity);
        place_primitives(
            doc,
            object,
            (context, Affine2::IDENTITY, doc.blocks.len()),
            &mut placed,
            diagnostics,
        );
    }
    placed
}

/// Walk one object down to its primitives without materializing anything.
fn place_primitives<'a, LayerMeta, ObjectMeta>(
    doc: &'a Document<LayerMeta, ObjectMeta>,
    object: &'a Object<ObjectMeta>,
    (polarity, transform, block_limit): (Polarity, Affine2, usize),
    out: &mut Vec<Placed<'a, ObjectMeta>>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let polarity = polarity.compose(object.polarity);
    let shared = block_limit < doc.blocks.len();
    let (block, placements): (u32, Vec<Affine2>) = match object.geometry {
        Geometry::Flash {
            aperture,
            transform: placement,
        } => {
            if aperture as usize >= doc.apertures.len() {
                diagnostics.push(Diagnostic::warning(
                    "Skipping artwork flash with invalid aperture reference",
                ));
                return;
            }
            out.push(Placed {
                polarity,
                primitive: Primitive::Flash(aperture),
                transform: transform.concat(placement),
                shared: true,
                meta: &object.meta,
            });
            return;
        }
        Geometry::Stroke { path } | Geometry::Region { path } => {
            if path as usize >= doc.arena.paths.len() {
                diagnostics.push(Diagnostic::warning(
                    "Skipping artwork path with invalid path reference",
                ));
                return;
            }
            out.push(Placed {
                polarity,
                primitive: Primitive::Path(path),
                transform,
                shared,
                meta: &object.meta,
            });
            return;
        }
        Geometry::Instance {
            block,
            transform: placement,
        } => (block, vec![placement]),
        Geometry::GridInstance {
            block,
            transform: placement,
            repeat,
        } => (
            block,
            repeat
                .offsets()
                .map(|offset| Affine2::translation(offset).concat(placement))
                .collect(),
        ),
    };
    // Blocks reference only earlier blocks, so a walk always terminates.
    if block as usize >= block_limit {
        diagnostics.push(Diagnostic::warning(format!(
            "Skipping artwork instance of missing or non-earlier block {block}"
        )));
        return;
    }
    for placement in placements {
        for child in &doc.blocks[block as usize].objects {
            place_primitives(
                doc,
                child,
                (polarity, transform.concat(placement), block as usize),
                out,
                diagnostics,
            );
        }
    }
}

/// Images of placed primitives. A shared primitive is prepared once per
/// orientation at the origin and translated to each placement, which is
/// exact for polygons: a panel of one board images that board once.
struct PrimitiveImages {
    resolution: Resolution,
    prepared: HashMap<(Primitive, [u64; 4]), region::ContourSet>,
}

impl PrimitiveImages {
    fn image<LayerMeta, ObjectMeta>(
        &mut self,
        doc: &Document<LayerMeta, ObjectMeta>,
        placed: &Placed<'_, ObjectMeta>,
    ) -> Result<region::ContourSet, AccuracyError> {
        let Affine2 {
            m00,
            m01,
            m02,
            m10,
            m11,
            m12,
        } = placed.transform;
        if !placed.shared {
            return prepare(doc, placed.primitive, placed.transform, self.resolution);
        }
        let orientation = Affine2 {
            m02: 0.0,
            m12: 0.0,
            ..placed.transform
        };
        let key = (placed.primitive, [m00, m01, m10, m11].map(f64::to_bits));
        let prepared = match self.prepared.entry(key) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => entry.insert(prepare(
                doc,
                placed.primitive,
                orientation,
                self.resolution,
            )?),
        };
        prepared.translated(Point::new(m02, m12))
    }
}

fn prepare<LayerMeta, ObjectMeta>(
    doc: &Document<LayerMeta, ObjectMeta>,
    primitive: Primitive,
    transform: Affine2,
    resolution: Resolution,
) -> Result<region::ContourSet, AccuracyError> {
    match primitive {
        Primitive::Flash(aperture) => {
            let aperture = &doc.apertures[aperture as usize];
            let contours = aperture
                .contours()
                .into_iter()
                .map(|contour| contour.transformed(transform))
                .collect::<Vec<_>>();
            region::ContourSet::from_contours(&contours, aperture.fill_rule(), resolution)
        }
        // Strokes outline in the path's own frame, so a placement that is
        // not a similarity images the stroke it actually transforms.
        Primitive::Path(path) => region::ContourSet::from_placed_painted_paths(
            &doc.arena,
            [(doc.arena.path(path), transform)],
            resolution,
        ),
    }
}

/// Compose owner regions from retained source curves at one resolution.
///
/// Dark objects add material to the owner the caller names for them; owners
/// the caller declines are skipped, since they cannot change another owner's
/// image. Clear objects and final cutouts subtract from every owner painted
/// before them. This is where artwork is prepared: the returned regions
/// carry the cost of strokes, flattening and paint folds, and every region
/// derived from them inherits the budget.
pub fn compose_owner_regions<LayerMeta, ObjectMeta, Owner: Clone + Eq + Hash>(
    doc: &Document<LayerMeta, ObjectMeta>,
    owner: impl Fn(&ObjectMeta) -> Option<Owner>,
    resolution: Resolution,
) -> Result<(OwnerRegionLayers<Owner>, Vec<Diagnostic>), AccuracyError> {
    let mut diagnostics = doc.diagnostics.clone();
    let mut images = PrimitiveImages {
        resolution: resolution.strict(),
        prepared: HashMap::new(),
    };

    let mut layers = Vec::with_capacity(doc.layers.len());
    for layer in &doc.layers {
        let placed = placed_layer(doc, layer, &mut diagnostics);

        // Preserve first-paint order for deterministic attributed output and
        // retain constant-time lookup for later objects of the same owner.
        let mut owner_indices: HashMap<Owner, usize> = HashMap::new();
        let mut states: Vec<(Owner, region::PaintComposer, BBox)> = Vec::new();
        for placed in &placed {
            let selected = match placed.polarity {
                Polarity::Dark => match owner(placed.meta) {
                    Some(owner) => Some(owner),
                    None => continue,
                },
                Polarity::Clear => None,
            };
            let image = images.image(doc, placed)?;
            if image.is_empty() {
                continue;
            }
            match selected {
                Some(owner) => {
                    let index = *owner_indices.entry(owner.clone()).or_insert_with(|| {
                        states.push((owner, region::PaintComposer::new(resolution), BBox::empty()));
                        states.len() - 1
                    });
                    let (_, composer, bbox) = &mut states[index];
                    *bbox = bbox.union(image.bbox);
                    composer.push(Polarity::Dark, image);
                }
                None => {
                    for (_, composer, _) in states
                        .iter_mut()
                        .filter(|(_, _, bbox)| bbox.intersects(image.bbox))
                    {
                        composer.push(Polarity::Clear, image.clone());
                    }
                }
            }
        }

        let mut owners = Vec::with_capacity(states.len());
        for (owner, composer, _) in states {
            let image = composer.finish()?;
            if !image.is_empty() {
                owners.push((owner, image));
            }
        }
        layers.push(owners);
    }
    Ok((layers, diagnostics))
}

/// The composed image of the document's first layer, whatever owns it.
pub fn compose_layer_image<LayerMeta, ObjectMeta>(
    doc: &Document<LayerMeta, ObjectMeta>,
    resolution: Resolution,
) -> Result<region::ContourSet, AccuracyError> {
    let (layers, _) = compose_owner_regions(doc, |_| Some(()), resolution)?;
    let image = layers
        .into_iter()
        .next()
        .and_then(|mut owners| owners.pop());
    Ok(image.map_or_else(|| region::ContourSet::empty(resolution), |(_, image)| image))
}

/// Compose each layer's ordered dark/clear paint into its final positive
/// image.
pub fn compose_to_mask<LayerMeta: Clone, ObjectMeta>(
    doc: &Document<LayerMeta, ObjectMeta>,
    resolution: Resolution,
) -> Result<mask::Document<LayerMeta>, AccuracyError> {
    let (images, diagnostics) = compose_owner_regions(doc, |_| Some(()), resolution.strict())?;
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

    for (layer_index, owners) in images.into_iter().enumerate() {
        let contours = owners
            .into_iter()
            .flat_map(|(_, image)| image.to_contours())
            .collect::<Vec<_>>();
        if !contours.is_empty() {
            mask.push_shape(layer_index as u32, FillRule::NonZero, contours);
        }
    }

    mask.diagnostics.extend(diagnostics);
    Ok(mask)
}

fn geometry_bbox<LayerMeta, ObjectMeta>(
    doc: &Document<LayerMeta, ObjectMeta>,
    geometry: Geometry,
) -> BBox {
    let block_bbox = |block: u32, transform| {
        let block = doc.blocks.get(block as usize)?;
        Some(block.bbox.transformed(transform))
    };
    let bbox = match geometry {
        Geometry::Region { path } | Geometry::Stroke { path } => {
            doc.arena.paths.get(path as usize).map(|path| path.bbox)
        }
        Geometry::Flash {
            aperture,
            transform,
        } => doc
            .apertures
            .get(aperture as usize)
            .map(|aperture| aperture.bbox().transformed(transform)),
        Geometry::Instance { block, transform } => block_bbox(block, transform),
        Geometry::GridInstance {
            block,
            transform,
            repeat,
        } => block_bbox(block, transform).map(|bbox| repeat.bbox(bbox)),
    };
    bbox.unwrap_or_else(BBox::empty)
}

/// Materialize all reusable block instances into ordinary layer objects.
///
/// This is the explicit fallback for consumers that cannot preserve
/// hierarchy. Paths are transformed exactly once at each final placement;
/// aperture flashes retain composed affine transforms.
pub fn expand_instances<LayerMeta: Clone, ObjectMeta: Clone>(
    doc: &Document<LayerMeta, ObjectMeta>,
) -> Document<LayerMeta, ObjectMeta> {
    expand_instances_with_grid_policy(doc, false)
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
    expand_instances_with_grid_policy(doc, true)
}

fn expand_instances_with_grid_policy<LayerMeta: Clone, ObjectMeta: Clone>(
    doc: &Document<LayerMeta, ObjectMeta>,
    preserve_grids: bool,
) -> Document<LayerMeta, ObjectMeta> {
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
        flatten_leaf_blocks(doc, &mut out, &block_contains_grid);
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
                    block_contains_grid: &block_contains_grid,
                },
                doc.blocks.len(),
            );
        }
    }
    normalize_bounds(&mut out);
    out
}

/// Rebuild every grid-free block as primitive-only geometry so retained
/// grids never require a hierarchy walk in consumers. Blocks containing
/// grids are copied verbatim; expansion never retains a grid of them.
fn flatten_leaf_blocks<LayerMeta, ObjectMeta: Clone>(
    doc: &Document<LayerMeta, ObjectMeta>,
    out: &mut Document<LayerMeta, ObjectMeta>,
    block_contains_grid: &[bool],
) {
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
                        let geometry =
                            transform_primitive_geometry(out, child_object.geometry, transform);
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
}

/// Apply `transform` to primitive geometry within one document, copying
/// transformed paths into its arena.
fn transform_primitive_geometry<LayerMeta, ObjectMeta>(
    doc: &mut Document<LayerMeta, ObjectMeta>,
    geometry: Geometry,
    transform: Affine2,
) -> Geometry {
    match geometry {
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
                let contours = doc
                    .arena
                    .transformed_contour_bufs(source_path.contours, transform);
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
    }
}

#[derive(Clone, Copy)]
struct InstanceExpansion<'a> {
    transform: Affine2,
    polarity: Polarity,
    preserve_grids: bool,
    block_contains_grid: &'a [bool],
}

fn expand_object_into_layer<LayerMeta, ObjectMeta: Clone>(
    source: &Document<LayerMeta, ObjectMeta>,
    target: &mut Document<LayerMeta, ObjectMeta>,
    layer: u32,
    object: &Object<ObjectMeta>,
    expansion: InstanceExpansion<'_>,
    block_limit: usize,
) {
    let transform = expansion.transform;
    let polarity = expansion.polarity.compose(object.polarity);
    let (block, placement, repeat) = match object.geometry {
        Geometry::Instance { block, transform } => (block, transform, None),
        Geometry::GridInstance {
            block,
            transform,
            repeat,
        } => (block, transform, Some(repeat)),
        geometry => {
            // The target arena starts as a clone of the source arena, so
            // source path indices resolve identically in the target.
            let geometry = transform_primitive_geometry(target, geometry, transform);
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
            return;
        }
    };
    let kind = if repeat.is_some() {
        "grid instance"
    } else {
        "instance"
    };
    let Some(block_definition) = source.blocks.get(block as usize) else {
        target.warn(format!("Skipping artwork {kind} of missing block {block}"));
        return;
    };
    if block as usize >= block_limit {
        target.warn(format!(
            "Skipping artwork {kind} of non-earlier block {block}"
        ));
        return;
    }
    let placements = match repeat {
        None => vec![placement],
        Some(repeat)
            if expansion.preserve_grids && !expansion.block_contains_grid[block as usize] =>
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
                            x_step: transform.transform_vector(repeat.x_step),
                            y_step: transform.transform_vector(repeat.y_step),
                            ..repeat
                        },
                    },
                    bbox: BBox::empty(),
                    meta: object.meta.clone(),
                },
            );
            return;
        }
        Some(repeat) => repeat
            .offsets()
            .map(|offset| Affine2 {
                m02: placement.m02 + offset.x,
                m12: placement.m12 + offset.y,
                ..placement
            })
            .collect(),
    };
    for placement in placements {
        for child in &block_definition.objects {
            expand_object_into_layer(
                source,
                target,
                layer,
                child,
                InstanceExpansion {
                    transform: transform.concat(placement),
                    polarity,
                    ..expansion
                },
                block as usize,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::path::PathCmd;
    use crate::geom::{LineCap, StrokeStyle};

    fn rect<Meta>(doc: &mut Document<(), Meta>, x0: f64, y0: f64, x1: f64, y1: f64) -> u32 {
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
    }

    #[test]
    fn an_aperture_hole_must_lie_inside_its_shape() {
        let holed = |shape, hole_diameter| Aperture {
            shape,
            hole_diameter,
        };
        let rectangle = |hole| {
            let (width, height) = (1.0, 0.4);
            holed(ApertureShape::Rectangle { width, height }, hole)
        };
        let hexagon = |hole| {
            let shape = ApertureShape::Polygon {
                diameter: 2.0,
                vertices: 6,
                rotation_degrees: 0.0,
            };
            holed(shape, hole)
        };
        assert!(rectangle(0.0).hole_fits() && rectangle(0.4).hole_fits());
        assert!(!rectangle(0.5).hole_fits());
        // A hexagon's flats sit cos 30° of the way to its vertices.
        assert!(hexagon(1.73).hole_fits() && !hexagon(1.74).hole_fits());

        let mut doc = Document::<(), ()>::new();
        doc.push_aperture(rectangle(0.4));
        assert!(doc.validate().is_ok());
        doc.push_aperture(rectangle(0.5));
        assert!(doc.validate().is_err());
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
    fn composition_stages_overlays_over_base_clears_and_final_cutouts_last() {
        let mut doc = Document::<(), ()>::new();
        let layer = doc.push_layer(Layer::new("F.Cu", LayerRole::Copper, Side::Top));
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

        let mask = compose_to_mask(&doc, Resolution::default()).unwrap();
        let shape = mask.layers[0].shapes.slice(&mask.arena.paths)[0];
        let image = region::ContourSet::from_contours(
            &mask.arena.path_contours(&shape),
            FillRule::NonZero,
            Resolution::default(),
        )
        .unwrap();

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

        let (mut layers, diagnostics) =
            compose_owner_regions(&doc, |meta| Some(*meta), Resolution::default()).unwrap();
        assert!(diagnostics.is_empty());
        let composed = layers.remove(0);
        assert_eq!(
            composed.iter().map(|(owner, _)| *owner).collect::<Vec<_>>(),
            ["A", "B", "C"]
        );
        let owners = composed.into_iter().collect::<HashMap<_, _>>();

        assert!(owners["A"].contains_point(Point::new(2.0, 2.0)));
        assert!(!owners["A"].contains_point(Point::new(5.0, 5.0)));
        assert!(owners["B"].contains_point(Point::new(5.0, 5.0)));
        assert!(owners["A"].contains_point(Point::new(8.5, 8.5)));
        assert!(owners["C"].contains_point(Point::new(8.5, 8.5)));

        let (mut selected, diagnostics) = compose_owner_regions(
            &doc,
            |meta| (*meta != "C").then_some(*meta),
            Resolution::default(),
        )
        .unwrap();
        assert!(diagnostics.is_empty());
        let selected = selected.remove(0);
        assert_eq!(
            selected.iter().map(|(owner, _)| *owner).collect::<Vec<_>>(),
            ["A", "B"]
        );
        let selected = selected.into_iter().collect::<HashMap<_, _>>();
        assert!(!selected["A"].contains_point(Point::new(5.0, 5.0)));
        assert!(selected["B"].contains_point(Point::new(5.0, 5.0)));
    }

    #[test]
    fn a_block_placed_many_times_images_like_its_expansion() {
        let mut doc = Document::<(), ()>::new();
        let aperture = doc.push_aperture(Aperture::circle(1.0));
        let block = doc.push_block();
        let path = doc.push_path(
            Paint::Stroke(StrokeStyle::new(0.2, LineCap::Round)),
            vec![ContourBuf::new(vec![
                PathCmd::move_to(Point::new(0.0, 0.0)),
                PathCmd::line_to(Point::new(3.0, 1.0)),
            ])],
        );
        doc.push_block_object(
            block,
            Object::new(Polarity::Dark, Geometry::Stroke { path }),
        );
        doc.push_block_object(
            block,
            Object::new(
                Polarity::Dark,
                Geometry::Flash {
                    aperture,
                    transform: Affine2::translation(Point::new(3.0, 1.0)),
                },
            ),
        );
        let layer = doc.push_layer(Layer::new("F.Cu", LayerRole::Copper, Side::Top));
        for (x, degrees) in [(0.0, 0.0), (10.0, 90.0), (20.0, 0.0)] {
            doc.push_object(
                layer,
                Object::new(
                    Polarity::Dark,
                    Geometry::Instance {
                        block,
                        transform: Affine2::placement(
                            Point::new(x, 5.0),
                            degrees,
                            crate::geom::Mirror::NONE,
                            1.0,
                        ),
                    },
                ),
            );
        }
        normalize_bounds(&mut doc);

        let image =
            |doc: &Document<(), ()>| compose_layer_image(doc, Resolution::default()).unwrap();
        let shared = image(&doc);
        let expanded = image(&expand_instances(&doc));
        assert!((shared.area() - expanded.area()).abs() < 1e-9);
        assert!(shared.difference(&expanded).unwrap().area() < 1e-9);
        assert!(shared.contains_point(Point::new(9.0, 8.0)));
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
}
