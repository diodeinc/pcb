//! Fully composed layer image geometry.
//!
//! The mask dialect is the common render/compare target after ordered paint
//! operations have been resolved. It stores only final positive filled shapes
//! per layer; dark/clear ordering belongs in artwork lowering. Rendering
//! lives in [`crate::render`].

use crate::dialects::{LayerRole, Side};
use crate::geom::path::ContourBuf;
use crate::geom::{BBox, Diagnostic, FillRule, Paint, Path, PathArena, Span};

#[derive(Debug, Clone, Default)]
pub struct Document<LayerMeta = ()> {
    pub layers: Vec<Layer<LayerMeta>>,
    /// Shape storage: `layer.shapes` spans `arena.paths`, and every path is
    /// fill-painted.
    pub arena: PathArena,
    pub diagnostics: Vec<Diagnostic>,
}

impl<LayerMeta> Document<LayerMeta> {
    pub fn new() -> Self {
        Self {
            layers: Vec::new(),
            arena: PathArena::default(),
            diagnostics: Vec::new(),
        }
    }

    pub fn push_layer(&mut self, mut layer: Layer<LayerMeta>) -> u32 {
        layer.shapes = Span::new(self.arena.paths.len() as u32, 0);
        let id = self.layers.len() as u32;
        self.layers.push(layer);
        id
    }

    /// Append a filled shape to a layer. Shapes for one layer must be pushed
    /// contiguously.
    pub fn push_shape(
        &mut self,
        layer_id: u32,
        fill_rule: FillRule,
        contours: impl IntoIterator<Item = ContourBuf>,
    ) -> u32 {
        let path = self
            .arena
            .push_path(Paint::Fill { rule: fill_rule }, contours);
        let bbox = self.arena.path(path).bbox;
        let layer = &mut self.layers[layer_id as usize];
        if layer.shapes.is_empty() {
            layer.shapes.start = path;
        }
        layer.shapes.count += 1;
        layer.bbox = layer.bbox.union(bbox);
        path
    }

    pub fn shapes(&self, layer: &Layer<LayerMeta>) -> &[Path] {
        layer.shapes.slice(&self.arena.paths)
    }

    pub fn validate(&self) -> Result<(), crate::geom::Diagnostics> {
        let mut diagnostics = crate::geom::Diagnostics::default();
        for (index, layer) in self.layers.iter().enumerate() {
            if let Err(message) =
                layer
                    .shapes
                    .validate("mask layer shapes", index, self.arena.paths.len())
            {
                diagnostics.error(message);
            }
            if let Err(message) = crate::geom::validate_bbox("mask layer", index, layer.bbox) {
                diagnostics.error(message);
            }
        }
        for (index, path) in self.arena.paths.iter().enumerate() {
            if !path.is_filled() {
                diagnostics.error(format!("mask shape {index} is not fill-painted"));
            }
        }
        self.arena.validate_into("mask", &mut diagnostics);
        diagnostics.into_result()
    }
}

#[derive(Debug, Clone)]
pub struct Layer<Meta = ()> {
    pub name: String,
    pub role: LayerRole,
    pub side: Side,
    pub shapes: Span,
    pub bbox: BBox,
    pub meta: Meta,
}

impl<Meta: Default> Layer<Meta> {
    pub fn new(name: impl Into<String>, role: LayerRole, side: Side) -> Self {
        Self {
            name: name.into(),
            role,
            side,
            shapes: Span::EMPTY,
            bbox: BBox::empty(),
            meta: Meta::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Point;
    use crate::geom::path::PathCmd;

    #[test]
    fn stores_final_shapes_by_layer() {
        let mut doc = Document::<()>::new();
        let layer = doc.push_layer(Layer::new("F.Cu", LayerRole::Copper, Side::Top));
        doc.push_shape(
            layer,
            FillRule::NonZero,
            vec![ContourBuf::new(vec![
                PathCmd::move_to(Point::new(0.0, 0.0)),
                PathCmd::close(),
            ])],
        );

        assert_eq!(doc.layers[0].shapes.len(), 1);
        assert_eq!(doc.arena.paths[0].contours.len(), 1);
        assert_eq!(doc.arena.cmds.len(), 2);
        doc.validate().unwrap();
    }
}
