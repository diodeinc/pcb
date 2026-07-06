//! Source-independent ordered fabrication artwork.
//!
//! This dialect intentionally keeps an object stream instead of immediately
//! flattening everything into polygons. It is the common interchange target
//! for source dialects such as IPC-2581 and Gerber when we still care about
//! idiomatic fabrication objects: flashes, strokes, regions, and ordered
//! dark/clear paint operations.

pub mod compare;

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
    pub layers: Vec<Layer<LayerMeta>>,
    pub objects: Vec<Object<ObjectMeta>>,
    pub arena: PathArena,
    pub diagnostics: Vec<Diagnostic>,
}

impl<LayerMeta, ObjectMeta> Document<LayerMeta, ObjectMeta> {
    pub fn new() -> Self {
        Self {
            apertures: Vec::new(),
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
            match object.geometry {
                Geometry::Flash { aperture, .. } => {
                    if aperture as usize >= self.apertures.len() {
                        diagnostics.error(format!(
                            "artwork object {index} references missing aperture {aperture}"
                        ));
                    }
                }
                Geometry::Stroke { path } | Geometry::Region { path } => {
                    if path as usize >= self.arena.paths.len() {
                        diagnostics.error(format!(
                            "artwork object {index} references missing path {path}"
                        ));
                    }
                }
            }
            if let Err(message) = crate::geom::validate_bbox("artwork object", index, object.bbox) {
                diagnostics.error(message);
            }
        }
        self.arena.validate_into("artwork", &mut diagnostics);
        diagnostics.into_result()
    }
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
}

impl Geometry {
    pub fn path(self) -> Option<u32> {
        match self {
            Self::Flash { .. } => None,
            Self::Stroke { path } | Self::Region { path } => Some(path),
        }
    }
}

/// A standard aperture: a primitive shape with an optional round hole.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aperture {
    pub shape: ApertureShape,
    /// Diameter of the round hole through the aperture; `0.0` means solid.
    pub hole_diameter: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
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
        let outer = match self.shape {
            ApertureShape::Circle { diameter } => shapes::circle(diameter),
            ApertureShape::Rectangle { width, height } => shapes::rect(width, height),
            ApertureShape::Obround { width, height } => shapes::obround(width, height, true),
            ApertureShape::Polygon {
                diameter,
                vertices,
                rotation_degrees,
            } => shapes::regular_polygon(diameter, vertices, rotation_degrees),
        };
        let mut contours: Vec<ContourBuf> = outer.into_iter().collect();
        if !contours.is_empty() && self.hole_diameter > 0.0 {
            contours.extend(shapes::circle(self.hole_diameter));
        }
        contours
    }

    pub fn fill_rule(&self) -> FillRule {
        if self.hole_diameter > 0.0 {
            FillRule::EvenOdd
        } else {
            FillRule::NonZero
        }
    }

    pub fn bbox(&self) -> BBox {
        match self.shape {
            ApertureShape::Circle { diameter } => {
                BBox::from_point(Point::ZERO).expand(diameter / 2.0)
            }
            ApertureShape::Rectangle { width, height }
            | ApertureShape::Obround { width, height } => BBox::new(
                Point::new(-width / 2.0, -height / 2.0),
                Point::new(width / 2.0, height / 2.0),
            ),
            ApertureShape::Polygon { diameter, .. } => {
                BBox::from_point(Point::ZERO).expand(diameter / 2.0)
            }
        }
    }
}

/// Recompute object and layer bounds bottom-up (after arena mutation).
pub fn normalize_bounds<LayerMeta, ObjectMeta>(doc: &mut Document<LayerMeta, ObjectMeta>) {
    doc.arena.recompute_bounds();
    for object_index in 0..doc.objects.len() {
        doc.objects[object_index].bbox = object_bbox(doc, object_index);
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
    mut doc: Document<LayerMeta, ObjectMeta>,
) -> Document<LayerMeta, ObjectMeta> {
    expand_strokes_to_regions(&mut doc);
    expand_flashes_to_regions(&mut doc);
    normalize_bounds(&mut doc);
    doc
}

/// Compose ordered dark/clear objects into final positive per-layer images.
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
        let mut composer = region::PaintComposer::default();
        for object in layer.objects.slice(&doc.objects) {
            let image = object_image_rings(&doc, object);
            if image.is_empty() {
                continue;
            }
            composer.push(object.polarity, image);
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
        let Some(aperture) = doc.apertures.get(aperture as usize).copied() else {
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
        Geometry::Flash { .. } | Geometry::Stroke { .. } => Vec::new(),
    }
}

fn object_bbox<LayerMeta, ObjectMeta>(
    doc: &Document<LayerMeta, ObjectMeta>,
    object_index: usize,
) -> BBox {
    match doc.objects[object_index].geometry {
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
    }
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
