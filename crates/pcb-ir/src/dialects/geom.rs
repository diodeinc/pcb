use crate::common::*;
use crate::dialects::mask;
use crate::dialects::path::{PathCmd, PathPayload};

/// Canonical source-independent vector geometry.
///
/// This dialect is meant to host geometry passes shared by all frontends:
/// bounds normalization, stroke outlining, boolean operations, simplification,
/// cutout subtraction, and comparison. It deliberately uses fat arena structs
/// so passes can mutate index ranges without allocating per-object graphs.
#[derive(Debug, Clone)]
pub struct GeomDocument<LayerMeta = (), ObjectMeta = ()> {
    pub unit: Unit,
    pub layers: Vec<GeomLayer<LayerMeta>>,
    pub objects: Vec<GeomObject<ObjectMeta>>,
    pub paths: Vec<GeomPath>,
    pub contours: Vec<GeomContour>,
    pub path_cmds: Vec<PathCmd>,
    pub diagnostics: Vec<GeometryDiagnostic>,
}

impl<LayerMeta, ObjectMeta> GeomDocument<LayerMeta, ObjectMeta> {
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

    pub fn push_layer(&mut self, mut layer: GeomLayer<LayerMeta>) -> u32 {
        layer.object_start = self.objects.len() as u32;
        layer.object_count = 0;
        let id = self.layers.len() as u32;
        self.layers.push(layer);
        id
    }

    pub fn push_object(&mut self, layer_id: u32, object: GeomObject<ObjectMeta>) -> u32 {
        let id = self.objects.len() as u32;
        self.objects.push(object);
        let layer = &mut self.layers[layer_id as usize];
        if layer.object_count == 0 {
            layer.object_start = id;
        }
        layer.object_count += 1;
        id
    }

    pub fn push_path(&mut self, mut path: GeomPath, contours: Vec<PathPayload>) -> u32 {
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
        self.contours.push(GeomContour {
            cmd_start,
            cmd_count: self.path_cmds.len() as u32 - cmd_start,
            bbox: contour.bbox,
        });
    }
}

pub fn lower_filled_to_mask<LayerMeta: Clone, ObjectMeta>(
    geom: &GeomDocument<LayerMeta, ObjectMeta>,
) -> mask::MaskDocument<LayerMeta> {
    let mut mask = mask::MaskDocument::new(geom.unit);

    for layer in &geom.layers {
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

    for (layer_index, layer) in geom.layers.iter().enumerate() {
        let objects = &geom.objects
            [layer.object_start as usize..(layer.object_start + layer.object_count) as usize];
        for object in objects {
            if object.paint != PaintPolarity::Dark {
                mask.diagnostics.push(GeometryDiagnostic {
                    severity: DiagnosticSeverity::Warning,
                    message: "Skipping clear geometry; geom composition pass has not run"
                        .to_string(),
                });
                continue;
            }

            let path = &geom.paths[object.path as usize];
            let GeomPathKind::Fill { fill_rule } = path.kind else {
                mask.diagnostics.push(GeometryDiagnostic {
                    severity: DiagnosticSeverity::Warning,
                    message: "Skipping stroked geometry; stroke outlining pass has not run"
                        .to_string(),
                });
                continue;
            };

            mask.push_shape(
                layer_index as u32,
                mask::MaskShape::new(fill_rule),
                path_payloads(geom, path),
            );
        }
    }

    mask.diagnostics.extend(geom.diagnostics.clone());
    mask
}

fn path_payloads<LayerMeta, ObjectMeta>(
    doc: &GeomDocument<LayerMeta, ObjectMeta>,
    path: &GeomPath,
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
pub struct GeomLayer<Meta = ()> {
    pub name: String,
    pub role: LayerRole,
    pub side: Side,
    pub object_start: u32,
    pub object_count: u32,
    pub bbox: BBox,
    pub meta: Meta,
}

impl<Meta: Default> GeomLayer<Meta> {
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
pub struct GeomObject<Meta = ()> {
    pub paint: PaintPolarity,
    pub path: u32,
    pub bbox: BBox,
    pub meta: Meta,
}

impl<Meta: Default> GeomObject<Meta> {
    pub fn new(paint: PaintPolarity, path: u32) -> Self {
        Self {
            paint,
            path,
            bbox: BBox::empty(),
            meta: Meta::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct GeomPath {
    pub contour_start: u32,
    pub contour_count: u32,
    pub bbox: BBox,
    pub kind: GeomPathKind,
}

impl GeomPath {
    pub fn filled(fill_rule: FillRule) -> Self {
        Self {
            contour_start: 0,
            contour_count: 0,
            bbox: BBox::empty(),
            kind: GeomPathKind::Fill { fill_rule },
        }
    }

    pub fn stroked(width: f64, line_cap: LineCap, line_join: LineJoin) -> Self {
        Self {
            contour_start: 0,
            contour_count: 0,
            bbox: BBox::empty(),
            kind: GeomPathKind::Stroke {
                width,
                line_cap,
                line_join,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GeomPathKind {
    Fill {
        fill_rule: FillRule,
    },
    Stroke {
        width: f64,
        line_cap: LineCap,
        line_join: LineJoin,
    },
}

#[derive(Debug, Clone)]
pub struct GeomContour {
    pub cmd_start: u32,
    pub cmd_count: u32,
    pub bbox: BBox,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_geom_as_fat_struct_ranges() {
        let mut doc = GeomDocument::<(), ()>::new(Unit::Millimeter);
        let layer = doc.push_layer(GeomLayer::new("mask", LayerRole::Soldermask, Side::Top));
        let path = doc.push_path(
            GeomPath::stroked(0.2, LineCap::Round, LineJoin::Round),
            vec![PathPayload {
                bbox: BBox::from_point(Point::new(1.0, 1.0)),
                cmds: vec![
                    PathCmd::move_to(Point::new(1.0, 1.0)),
                    PathCmd::line_to(Point::new(2.0, 1.0)),
                ],
            }],
        );
        doc.push_object(layer, GeomObject::new(PaintPolarity::Dark, path));

        assert_eq!(doc.layers[layer as usize].object_count, 1);
        assert_eq!(doc.objects[0].path, path);
        assert_eq!(doc.contours.len(), 1);
    }

    #[test]
    fn lowers_filled_geom_to_mask() {
        let mut doc = GeomDocument::<(), ()>::new(Unit::Millimeter);
        let layer = doc.push_layer(GeomLayer::new("F.Cu", LayerRole::Copper, Side::Top));
        let path = doc.push_path(
            GeomPath::filled(FillRule::NonZero),
            vec![PathPayload {
                bbox: BBox::from_point(Point::new(0.0, 0.0)),
                cmds: vec![PathCmd::move_to(Point::new(0.0, 0.0)), PathCmd::close()],
            }],
        );
        doc.push_object(layer, GeomObject::new(PaintPolarity::Dark, path));

        let mask = lower_filled_to_mask(&doc);

        assert_eq!(mask.layers.len(), 1);
        assert_eq!(mask.layers[0].shape_count, 1);
        assert_eq!(mask.shapes[0].contour_count, 1);
        assert!(mask.diagnostics.is_empty());
    }
}
