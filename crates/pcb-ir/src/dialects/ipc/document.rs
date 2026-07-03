use crate::dialects::ipc::feature::{Feature, FeatureSet, PinRef};
use crate::dialects::ipc::layout::{LayoutGraph, StepProfile, StepProfileCutout};
use crate::dialects::ipc::spec::{Spec, SpecItem, SpecProperty, SpecRef};
use crate::geom::path::ContourBuf;
use crate::geom::{Affine2, BBox, Diagnostic, Paint, PathArena};

/// Source-faithful IPC-2581 geometry document.
///
/// `Symbol` is the caller's interned-string handle; `LayerFunction` is the
/// caller's layer-function type. pcb-ir never resolves either, so the dialect
/// stays decoupled from any particular IPC-2581 parser.
#[derive(Debug, Clone)]
pub struct Document<Symbol, LayerFunction> {
    pub layout: LayoutGraph<Symbol>,
    pub layers: Vec<Layer<Symbol, LayerFunction>>,
    pub profiles: Vec<StepProfile>,
    pub profile_cutouts: Vec<StepProfileCutout>,
    pub specs: Vec<Spec<Symbol>>,
    pub spec_items: Vec<SpecItem<Symbol>>,
    pub spec_properties: Vec<SpecProperty<Symbol>>,
    pub spec_refs: Vec<SpecRef<Symbol>>,
    pub feature_sets: Vec<FeatureSet<Symbol>>,
    pub features: Vec<Feature<Symbol>>,
    pub pin_refs: Vec<PinRef<Symbol>>,
    pub arena: PathArena,
    pub diagnostics: Vec<Diagnostic>,
}

impl<Symbol, LayerFunction> Document<Symbol, LayerFunction> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a styled path over the given contours; returns the path index.
    pub fn push_path(
        &mut self,
        paint: Paint,
        contours: impl IntoIterator<Item = ContourBuf>,
    ) -> u32 {
        self.arena.push_path(paint, contours)
    }

    /// Detach the contours of a path, transformed into another frame.
    pub fn transformed_path_contours(&self, path: u32, transform: Affine2) -> Vec<ContourBuf> {
        let path = self.arena.path(path);
        if transform.is_identity() {
            self.arena.path_contours(path)
        } else {
            self.arena
                .transformed_contour_bufs(path.contours, transform)
        }
    }

    /// Bounding box of a path transformed into another frame.
    pub fn transformed_path_bbox(&self, path: u32, transform: Affine2) -> BBox {
        let path = self.arena.path(path);
        self.arena
            .transformed_contours_bbox(path.contours, transform)
    }

    pub fn warn(&mut self, message: impl Into<String>) {
        self.diagnostics.push(Diagnostic::warning(message));
    }
}

impl<Symbol, LayerFunction> Default for Document<Symbol, LayerFunction> {
    fn default() -> Self {
        Self {
            layout: LayoutGraph::default(),
            layers: Vec::new(),
            profiles: Vec::new(),
            profile_cutouts: Vec::new(),
            specs: Vec::new(),
            spec_items: Vec::new(),
            spec_properties: Vec::new(),
            spec_refs: Vec::new(),
            feature_sets: Vec::new(),
            features: Vec::new(),
            pin_refs: Vec::new(),
            arena: PathArena::default(),
            diagnostics: Vec::new(),
        }
    }
}

/// One source layer with its extracted features.
#[derive(Debug, Clone)]
pub struct Layer<Symbol, LayerFunction> {
    pub name: String,
    pub source_layer_ref: Symbol,
    pub layer_function: LayerFunction,
    /// Spans `doc.spec_refs`.
    pub spec_refs: crate::geom::Span,
    /// Spans `doc.feature_sets`.
    pub sets: crate::geom::Span,
    /// Spans `doc.features`.
    pub features: crate::geom::Span,
    pub bbox: BBox,
}
