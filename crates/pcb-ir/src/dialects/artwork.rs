use crate::common::*;
use crate::dialects::mask;
use crate::dialects::path::{self, PathCmd, PathPayload};

/// Source-independent ordered fabrication artwork.
///
/// This dialect intentionally keeps an object stream instead of immediately
/// flattening everything into polygons. It is the common target for source
/// dialects such as IPC-2581 and Gerber when we still care about idiomatic
/// fabrication objects: strokes, regions, flashes, object attributes, and
/// ordered dark/clear paint operations.
#[derive(Debug, Clone)]
pub struct ArtworkDocument<LayerMeta = (), ObjectMeta = ()> {
    pub unit: Unit,
    pub apertures: Vec<ArtworkAperture>,
    pub layers: Vec<ArtworkLayer<LayerMeta>>,
    pub objects: Vec<ArtworkObject<ObjectMeta>>,
    pub paths: Vec<ArtworkPath>,
    pub contours: Vec<ArtworkContour>,
    pub path_cmds: Vec<PathCmd>,
    pub diagnostics: Vec<GeometryDiagnostic>,
}

impl<LayerMeta, ObjectMeta> ArtworkDocument<LayerMeta, ObjectMeta> {
    pub fn new(unit: Unit) -> Self {
        Self {
            unit,
            apertures: Vec::new(),
            layers: Vec::new(),
            objects: Vec::new(),
            paths: Vec::new(),
            contours: Vec::new(),
            path_cmds: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    pub fn push_layer(&mut self, mut layer: ArtworkLayer<LayerMeta>) -> u32 {
        layer.object_start = self.objects.len() as u32;
        layer.object_count = 0;
        let id = self.layers.len() as u32;
        self.layers.push(layer);
        id
    }

    pub fn push_aperture(&mut self, aperture: ArtworkAperture) -> u32 {
        let id = self.apertures.len() as u32;
        self.apertures.push(aperture);
        id
    }

    pub fn push_object(&mut self, layer_id: u32, object: ArtworkObject<ObjectMeta>) -> u32 {
        let id = self.objects.len() as u32;
        self.objects.push(object);
        let layer = &mut self.layers[layer_id as usize];
        if layer.object_count == 0 {
            layer.object_start = id;
        }
        layer.object_count += 1;
        id
    }

    pub fn push_path(&mut self, mut path: ArtworkPath, contours: Vec<PathPayload>) -> u32 {
        let contour_start = self.contours.len() as u32;
        let mut bbox = BBox::empty();
        for contour in contours {
            bbox = bbox.union(contour.bbox);
            self.push_contour(contour);
        }
        path.contour_start = contour_start;
        path.contour_count = self.contours.len() as u32 - contour_start;
        path.bbox = bbox;

        let id = self.paths.len() as u32;
        self.paths.push(path);
        id
    }

    fn push_contour(&mut self, contour: PathPayload) {
        let cmd_start = self.path_cmds.len() as u32;
        self.path_cmds.extend(contour.cmds);
        self.contours.push(ArtworkContour {
            cmd_start,
            cmd_count: self.path_cmds.len() as u32 - cmd_start,
            bbox: contour.bbox,
        });
    }

    pub fn validate(&self) -> Result<(), String> {
        for (index, layer) in self.layers.iter().enumerate() {
            validate_range(
                "artwork layer objects",
                index,
                layer.object_start,
                layer.object_count,
                self.objects.len(),
            )?;
            validate_bbox("artwork layer", index, layer.bbox)?;
        }
        for (index, object) in self.objects.iter().enumerate() {
            match object.geometry {
                ArtworkGeometry::Flash { aperture, .. } => {
                    if aperture as usize >= self.apertures.len() {
                        return Err(format!(
                            "artwork object {index} references missing aperture {aperture}"
                        ));
                    }
                }
                _ => {
                    if let Some(path) = object.geometry.path()
                        && path as usize >= self.paths.len()
                    {
                        return Err(format!(
                            "artwork object {index} references missing path {path}"
                        ));
                    }
                }
            }
            validate_bbox("artwork object", index, object.bbox)?;
        }
        for (index, path) in self.paths.iter().enumerate() {
            validate_range(
                "artwork path contours",
                index,
                path.contour_start,
                path.contour_count,
                self.contours.len(),
            )?;
            validate_bbox("artwork path", index, path.bbox)?;
        }
        for (index, contour) in self.contours.iter().enumerate() {
            validate_range(
                "artwork contour commands",
                index,
                contour.cmd_start,
                contour.cmd_count,
                self.path_cmds.len(),
            )?;
            validate_bbox("artwork contour", index, contour.bbox)?;
        }
        path::validate_cmd_points("artwork", &self.path_cmds)
    }
}

pub fn path_payloads<LayerMeta, ObjectMeta>(
    doc: &ArtworkDocument<LayerMeta, ObjectMeta>,
    path: &ArtworkPath,
) -> Vec<PathPayload> {
    doc.contours[path.contour_start as usize..(path.contour_start + path.contour_count) as usize]
        .iter()
        .map(|contour| PathPayload {
            bbox: contour.bbox,
            cmds: doc.path_cmds
                [contour.cmd_start as usize..(contour.cmd_start + contour.cmd_count) as usize]
                .to_vec(),
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct ArtworkLayer<Meta = ()> {
    pub name: String,
    pub role: LayerRole,
    pub side: Side,
    pub object_start: u32,
    pub object_count: u32,
    pub bbox: BBox,
    pub meta: Meta,
}

impl<Meta: Default> ArtworkLayer<Meta> {
    pub fn new(name: impl Into<String>, role: LayerRole, side: Side) -> Self {
        Self {
            name: name.into(),
            role,
            side,
            object_start: 0,
            object_count: 0,
            bbox: BBox::empty(),
            meta: Meta::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ArtworkObject<Meta = ()> {
    pub paint: PaintPolarity,
    pub order: PaintOrder,
    pub geometry: ArtworkGeometry,
    pub net: Option<String>,
    pub bbox: BBox,
    pub meta: Meta,
}

impl<Meta: Default> ArtworkObject<Meta> {
    pub fn new(paint: PaintPolarity, geometry: ArtworkGeometry) -> Self {
        Self {
            paint,
            order: PaintOrder::default(),
            geometry,
            net: None,
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
pub enum ArtworkAperture {
    Circle { diameter: f64 },
    Rectangle { width: f64, height: f64 },
    Obround { width: f64, height: f64 },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ArtworkGeometry {
    Flash { aperture: u32, transform: Affine2 },
    CircleFlash { at: Point, diameter: f64 },
    Stroke { path: u32 },
    Region { path: u32 },
}

impl ArtworkGeometry {
    fn path(self) -> Option<u32> {
        match self {
            Self::Flash { .. } | Self::CircleFlash { .. } => None,
            Self::Stroke { path } | Self::Region { path } => Some(path),
        }
    }
}

pub fn normalize_bounds<LayerMeta, ObjectMeta>(
    artwork: &mut ArtworkDocument<LayerMeta, ObjectMeta>,
) {
    for contour_index in 0..artwork.contours.len() {
        let contour = &artwork.contours[contour_index];
        artwork.contours[contour_index].bbox = path::contour_bbox(
            &artwork.path_cmds
                [contour.cmd_start as usize..(contour.cmd_start + contour.cmd_count) as usize],
        );
    }
    for path_index in 0..artwork.paths.len() {
        artwork.paths[path_index].bbox = path_bbox(artwork, path_index);
    }
    for object_index in 0..artwork.objects.len() {
        artwork.objects[object_index].bbox = object_bbox(artwork, object_index);
    }
    for layer_index in 0..artwork.layers.len() {
        let layer = &artwork.layers[layer_index];
        artwork.layers[layer_index].bbox = artwork.objects
            [layer.object_start as usize..(layer.object_start + layer.object_count) as usize]
            .iter()
            .fold(BBox::empty(), |bbox, object| bbox.union(object.bbox));
    }
}

pub fn expand_native_geometry_to_regions<LayerMeta, ObjectMeta>(
    mut artwork: ArtworkDocument<LayerMeta, ObjectMeta>,
) -> ArtworkDocument<LayerMeta, ObjectMeta> {
    expand_strokes_to_regions(&mut artwork);
    expand_flashes_to_regions(&mut artwork);
    normalize_bounds(&mut artwork);
    artwork
}

pub fn compose_to_mask<LayerMeta: Clone, ObjectMeta: Clone>(
    artwork: &ArtworkDocument<LayerMeta, ObjectMeta>,
) -> mask::MaskDocument<LayerMeta> {
    let artwork = expand_native_geometry_to_regions(artwork.clone());
    let mut mask = mask::MaskDocument::new(artwork.unit);

    for layer in &artwork.layers {
        mask.push_layer(mask::MaskLayer {
            name: layer.name.clone(),
            role: layer.role,
            side: layer.side,
            shape_start: 0,
            shape_count: 0,
            bbox: BBox::empty(),
            meta: layer.meta.clone(),
        });
    }

    for (layer_index, layer) in artwork.layers.iter().enumerate() {
        let mut composer = path::PaintComposer::default();
        for object in &artwork.objects
            [layer.object_start as usize..(layer.object_start + layer.object_count) as usize]
        {
            let image = object_image_contours(&artwork, object);
            if image.is_empty() {
                continue;
            }
            let op = match object.paint {
                PaintPolarity::Dark => path::PaintOp::Dark,
                PaintPolarity::Clear => path::PaintOp::Clear,
            };
            composer.push(op, image);
        }

        let contours = path::polygon_contours_to_payloads(composer.finish());
        if !contours.is_empty() {
            mask.push_shape(
                layer_index as u32,
                mask::MaskShape::new(FillRule::NonZero),
                contours,
            );
        }
    }

    mask.diagnostics.extend(artwork.diagnostics);
    mask
}

fn expand_strokes_to_regions<LayerMeta, ObjectMeta>(
    artwork: &mut ArtworkDocument<LayerMeta, ObjectMeta>,
) {
    for object_index in 0..artwork.objects.len() {
        let ArtworkGeometry::Stroke { path: path_index } = artwork.objects[object_index].geometry
        else {
            continue;
        };
        let Some(path) = artwork.paths.get(path_index as usize).cloned() else {
            artwork.diagnostics.push(GeometryDiagnostic {
                severity: DiagnosticSeverity::Warning,
                message: "Skipping artwork stroke with invalid path reference".to_string(),
            });
            continue;
        };
        let Some(contours) = path::stroke_to_fill(
            &path_payloads(artwork, &path),
            path::StrokeToFillStyle::new(path.stroke_width, path.line_cap, path.line_join),
        ) else {
            continue;
        };
        let path_id = artwork.push_path(ArtworkPath::filled(FillRule::NonZero), contours);
        artwork.objects[object_index].geometry = ArtworkGeometry::Region { path: path_id };
        artwork.objects[object_index].bbox = artwork.paths[path_id as usize].bbox;
    }
}

fn expand_flashes_to_regions<LayerMeta, ObjectMeta>(
    artwork: &mut ArtworkDocument<LayerMeta, ObjectMeta>,
) {
    for object_index in 0..artwork.objects.len() {
        let payloads = match artwork.objects[object_index].geometry {
            ArtworkGeometry::Flash {
                aperture,
                transform,
            } => {
                let Some(aperture) = artwork.apertures.get(aperture as usize).copied() else {
                    artwork.diagnostics.push(GeometryDiagnostic {
                        severity: DiagnosticSeverity::Warning,
                        message: "Skipping artwork flash with invalid aperture reference"
                            .to_string(),
                    });
                    continue;
                };
                aperture_payloads(aperture, transform)
            }
            ArtworkGeometry::CircleFlash { at, diameter } => {
                circle_payloads(diameter / 2.0, Affine2::placement(at, 0.0, false, 1.0))
            }
            ArtworkGeometry::Stroke { .. } | ArtworkGeometry::Region { .. } => continue,
        };
        let path_id = artwork.push_path(ArtworkPath::filled(FillRule::NonZero), payloads);
        artwork.objects[object_index].geometry = ArtworkGeometry::Region { path: path_id };
        artwork.objects[object_index].bbox = artwork.paths[path_id as usize].bbox;
    }
}

fn object_image_contours<LayerMeta, ObjectMeta>(
    artwork: &ArtworkDocument<LayerMeta, ObjectMeta>,
    object: &ArtworkObject<ObjectMeta>,
) -> Vec<path::PolygonContour> {
    match object.geometry {
        ArtworkGeometry::Region { path } => artwork
            .paths
            .get(path as usize)
            .map(|path| {
                path::simplify_polygon_contours(
                    path::payloads_to_polygon_contours(&path_payloads(artwork, path)),
                    path.fill_rule,
                )
            })
            .unwrap_or_default(),
        ArtworkGeometry::Flash { .. } | ArtworkGeometry::CircleFlash { .. } => Vec::new(),
        ArtworkGeometry::Stroke { .. } => Vec::new(),
    }
}

fn path_bbox<LayerMeta, ObjectMeta>(
    artwork: &ArtworkDocument<LayerMeta, ObjectMeta>,
    path_index: usize,
) -> BBox {
    let path = &artwork.paths[path_index];
    let bbox = artwork.contours
        [path.contour_start as usize..(path.contour_start + path.contour_count) as usize]
        .iter()
        .fold(BBox::empty(), |bbox, contour| bbox.union(contour.bbox));
    if path.flags.stroked {
        bbox.expand(path.stroke_width / 2.0)
    } else {
        bbox
    }
}

fn object_bbox<LayerMeta, ObjectMeta>(
    artwork: &ArtworkDocument<LayerMeta, ObjectMeta>,
    object_index: usize,
) -> BBox {
    match artwork.objects[object_index].geometry {
        ArtworkGeometry::Region { path } | ArtworkGeometry::Stroke { path } => artwork
            .paths
            .get(path as usize)
            .map(|path| path.bbox)
            .unwrap_or_else(BBox::empty),
        ArtworkGeometry::CircleFlash { at, diameter } => {
            BBox::from_point(at).expand(diameter / 2.0)
        }
        ArtworkGeometry::Flash {
            aperture,
            transform,
        } => artwork
            .apertures
            .get(aperture as usize)
            .copied()
            .map(|aperture| {
                aperture_payloads(aperture, transform)
                    .iter()
                    .fold(BBox::empty(), |bbox, payload| bbox.union(payload.bbox))
            })
            .unwrap_or_else(BBox::empty),
    }
}

fn aperture_payloads(aperture: ArtworkAperture, transform: Affine2) -> Vec<PathPayload> {
    match aperture {
        ArtworkAperture::Circle { diameter } => circle_payloads(diameter / 2.0, transform),
        ArtworkAperture::Rectangle { width, height } => rect_payloads(width, height, transform),
        ArtworkAperture::Obround { width, height } => obround_payloads(width, height, transform),
    }
}

fn circle_payloads(radius: f64, transform: Affine2) -> Vec<PathPayload> {
    if radius <= 0.0 {
        return Vec::new();
    }
    let raw = [
        PathCmd::move_to(Point::new(radius, 0.0)),
        PathCmd::arc_to(Point::new(-radius, 0.0), Point::new(0.0, 0.0), false),
        PathCmd::arc_to(Point::new(radius, 0.0), Point::new(0.0, 0.0), false),
        PathCmd::close(),
    ];
    vec![transformed_payload(&raw, transform)]
}

fn rect_payloads(width: f64, height: f64, transform: Affine2) -> Vec<PathPayload> {
    if width <= 0.0 || height <= 0.0 {
        return Vec::new();
    }
    let hw = width / 2.0;
    let hh = height / 2.0;
    let raw = [
        PathCmd::move_to(Point::new(-hw, -hh)),
        PathCmd::line_to(Point::new(hw, -hh)),
        PathCmd::line_to(Point::new(hw, hh)),
        PathCmd::line_to(Point::new(-hw, hh)),
        PathCmd::close(),
    ];
    vec![transformed_payload(&raw, transform)]
}

fn obround_payloads(width: f64, height: f64, transform: Affine2) -> Vec<PathPayload> {
    if width <= 0.0 || height <= 0.0 {
        return Vec::new();
    }
    let rx = width / 2.0;
    let ry = height / 2.0;
    let raw = if width >= height {
        let r = ry;
        let cx = rx - r;
        vec![
            PathCmd::move_to(Point::new(-cx, -r)),
            PathCmd::line_to(Point::new(cx, -r)),
            PathCmd::arc_to(Point::new(cx, r), Point::new(cx, 0.0), false),
            PathCmd::line_to(Point::new(-cx, r)),
            PathCmd::arc_to(Point::new(-cx, -r), Point::new(-cx, 0.0), false),
            PathCmd::close(),
        ]
    } else {
        let r = rx;
        let cy = ry - r;
        vec![
            PathCmd::move_to(Point::new(r, -cy)),
            PathCmd::line_to(Point::new(r, cy)),
            PathCmd::arc_to(Point::new(-r, cy), Point::new(0.0, cy), false),
            PathCmd::line_to(Point::new(-r, -cy)),
            PathCmd::arc_to(Point::new(r, -cy), Point::new(0.0, -cy), false),
            PathCmd::close(),
        ]
    };
    vec![transformed_payload(&raw, transform)]
}

fn transformed_payload(cmds: &[PathCmd], transform: Affine2) -> PathPayload {
    path::transform_cmds(cmds.iter().copied(), transform).into()
}

#[derive(Debug, Clone)]
pub struct ArtworkPath {
    pub contour_start: u32,
    pub contour_count: u32,
    pub bbox: BBox,
    pub fill_rule: FillRule,
    pub stroke_width: f64,
    pub line_cap: LineCap,
    pub line_join: LineJoin,
    pub flags: PathFlags,
}

impl ArtworkPath {
    pub fn filled(fill_rule: FillRule) -> Self {
        Self {
            contour_start: 0,
            contour_count: 0,
            bbox: BBox::empty(),
            fill_rule,
            stroke_width: 0.0,
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            flags: PathFlags {
                filled: true,
                stroked: false,
            },
        }
    }

    pub fn stroked(width: f64, line_cap: LineCap, line_join: LineJoin) -> Self {
        Self {
            contour_start: 0,
            contour_count: 0,
            bbox: BBox::empty(),
            fill_rule: FillRule::NonZero,
            stroke_width: width,
            line_cap,
            line_join,
            flags: PathFlags {
                filled: false,
                stroked: true,
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct ArtworkContour {
    pub cmd_start: u32,
    pub cmd_count: u32,
    pub bbox: BBox,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_layers_objects_and_paths_in_fat_struct_arenas() {
        let mut doc = ArtworkDocument::<(), ()>::new(Unit::Millimeter);
        let layer = doc.push_layer(ArtworkLayer::new("F.Cu", LayerRole::Copper, Side::Top));
        let path = doc.push_path(
            ArtworkPath::filled(FillRule::NonZero),
            vec![PathPayload {
                bbox: BBox::from_point(Point::new(0.0, 0.0)),
                cmds: vec![PathCmd::move_to(Point::new(0.0, 0.0)), PathCmd::close()],
            }],
        );

        doc.push_object(
            layer,
            ArtworkObject::new(PaintPolarity::Dark, ArtworkGeometry::Region { path }),
        );

        assert_eq!(doc.layers[0].object_start, 0);
        assert_eq!(doc.layers[0].object_count, 1);
        assert_eq!(doc.objects.len(), 1);
        assert_eq!(doc.paths[path as usize].contour_count, 1);
        doc.validate().unwrap();
    }

    #[test]
    fn composes_ordered_artwork_to_mask() {
        let mut doc = ArtworkDocument::<(), ()>::new(Unit::Millimeter);
        let layer = doc.push_layer(ArtworkLayer::new("F.Cu", LayerRole::Copper, Side::Top));
        let path = doc.push_path(
            ArtworkPath::stroked(0.15, LineCap::Round, LineJoin::Round),
            vec![PathPayload {
                bbox: BBox::from_point(Point::new(0.0, 0.0)),
                cmds: vec![
                    PathCmd::move_to(Point::new(0.0, 0.0)),
                    PathCmd::line_to(Point::new(1.0, 0.0)),
                ],
            }],
        );

        doc.push_object(
            layer,
            ArtworkObject::new(PaintPolarity::Dark, ArtworkGeometry::Stroke { path }),
        );

        let mask = compose_to_mask(&doc);

        assert_eq!(mask.layers.len(), 1);
        assert_eq!(mask.layers[0].shape_count, 1);
        assert_eq!(mask.shapes[0].fill_rule, FillRule::NonZero);
        assert!(!mask.layers[0].bbox.is_empty());
        mask.validate().unwrap();
    }
}
