use crate::common::*;
use crate::dialects::geom;
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
            if let Some(path) = object.geometry.path()
                && path as usize >= self.paths.len()
            {
                return Err(format!(
                    "artwork object {index} references missing path {path}"
                ));
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

pub fn lower_to_geom<LayerMeta: Clone, ObjectMeta: Clone>(
    artwork: &ArtworkDocument<LayerMeta, ObjectMeta>,
) -> geom::GeomDocument<LayerMeta, ObjectMeta> {
    let mut geom = geom::GeomDocument::new(artwork.unit);

    for layer in &artwork.layers {
        geom.push_layer(geom::GeomLayer {
            name: layer.name.clone(),
            role: layer.role,
            side: layer.side,
            object_start: 0,
            object_count: 0,
            bbox: layer.bbox,
            meta: layer.meta.clone(),
        });
    }

    let path_map = artwork
        .paths
        .iter()
        .map(|path| {
            let geom_path = match path.flags {
                PathFlags { filled: true, .. } => geom::GeomPath::filled(path.fill_rule),
                PathFlags { stroked: true, .. } => {
                    geom::GeomPath::stroked(path.stroke_width, path.line_cap, path.line_join)
                }
                _ => geom::GeomPath::filled(path.fill_rule),
            };
            geom.push_path(geom_path, path_payloads(artwork, path))
        })
        .collect::<Vec<_>>();

    for (layer_index, layer) in artwork.layers.iter().enumerate() {
        let layer_objects = &artwork.objects
            [layer.object_start as usize..(layer.object_start + layer.object_count) as usize];
        for object in layer_objects {
            let Some(path) = object.geometry.path() else {
                geom.diagnostics.push(GeometryDiagnostic {
                    severity: DiagnosticSeverity::Warning,
                    message: "Skipping artwork flash without expanded aperture geometry"
                        .to_string(),
                });
                continue;
            };
            let Some(&path) = path_map.get(path as usize) else {
                geom.diagnostics.push(GeometryDiagnostic {
                    severity: DiagnosticSeverity::Warning,
                    message: "Skipping artwork object with invalid path reference".to_string(),
                });
                continue;
            };
            geom.push_object(
                layer_index as u32,
                geom::GeomObject {
                    paint: object.paint,
                    path,
                    bbox: object.bbox,
                    meta: object.meta.clone(),
                },
            );
        }
    }

    geom.diagnostics.extend(artwork.diagnostics.clone());
    geom
}

fn path_payloads<LayerMeta, ObjectMeta>(
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
    pub geometry: ArtworkGeometry,
    pub net: Option<String>,
    pub bbox: BBox,
    pub meta: Meta,
}

impl<Meta: Default> ArtworkObject<Meta> {
    pub fn new(paint: PaintPolarity, geometry: ArtworkGeometry) -> Self {
        Self {
            paint,
            geometry,
            net: None,
            bbox: BBox::empty(),
            meta: Meta::default(),
        }
    }
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
    fn lowers_ordered_artwork_paths_to_geom() {
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

        let geom = lower_to_geom(&doc);

        assert_eq!(geom.layers.len(), 1);
        assert_eq!(geom.objects.len(), 1);
        assert_eq!(geom.paths.len(), 1);
        assert!(matches!(
            geom.paths[0].kind,
            geom::GeomPathKind::Stroke { width, .. } if width == 0.15
        ));
        assert_eq!(geom.path_cmds.len(), 2);
        geom.validate().unwrap();
    }
}
