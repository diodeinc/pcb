use crate::dialects::ipc::feature::{Feature, FeaturePlacementGroup, FeatureSet, PinRef};
use crate::dialects::ipc::layout::{LayoutGraph, StepProfile, StepProfileCutout};
use crate::dialects::ipc::spec::{Spec, SpecItem, SpecProperty, SpecRef};
use crate::geom::path::ContourBuf;
use crate::geom::{Affine2, BBox, Diagnostic, Paint, PathArena, Resolution};
use ipc2581::Symbol;
use ipc2581::types::LayerFunction;

const IDENTITY_PLACEMENT: [Affine2; 1] = [Affine2::IDENTITY];

/// Source-faithful IPC-2581 geometry document.
///
/// Names are [`Symbol`]s of the source file's interner, which the importer's
/// caller keeps to resolve them.
#[derive(Debug, Clone, Default)]
pub struct Document {
    pub layout: LayoutGraph,
    pub layers: Vec<Layer>,
    pub profiles: Vec<StepProfile>,
    pub profile_cutouts: Vec<StepProfileCutout>,
    pub specs: Vec<Spec>,
    pub spec_items: Vec<SpecItem>,
    pub spec_properties: Vec<SpecProperty>,
    pub spec_refs: Vec<SpecRef>,
    pub feature_sets: Vec<FeatureSet>,
    pub features: Vec<Feature>,
    pub feature_placement_groups: Vec<FeaturePlacementGroup>,
    pub feature_placements: Vec<Affine2>,
    pub pin_refs: Vec<PinRef>,
    pub arena: PathArena,
    pub diagnostics: Vec<Diagnostic>,
}

impl Document {
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

    /// Detach the contours of a path, transformed exactly into another frame.
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

    /// Append a feature to a set, maintaining the set's feature span and
    /// bounds. A set's features must be pushed contiguously.
    pub fn push_feature(&mut self, set_id: u32, mut feature: Feature) -> u32 {
        let id = self.features.len() as u32;
        let set = &mut self.feature_sets[set_id as usize];
        if set.features.is_empty() {
            set.features.start = id;
        }
        set.features.count += 1;
        set.bbox = set.bbox.union(feature.bbox);
        feature.set = Some(set_id);
        self.features.push(feature);
        id
    }

    /// The IPC `Set` a feature came from, if it came from one.
    pub fn feature_set(&self, feature: &Feature) -> Option<&FeatureSet> {
        feature
            .set
            .and_then(|set| self.feature_sets.get(set as usize))
    }

    /// Layer-space placements for one feature definition.
    ///
    /// Ungrouped features already use layer coordinates and therefore have
    /// one identity placement. Grouped features retain one local definition
    /// and expose every placement recorded by the source IPC `Features`
    /// container.
    pub fn placements_for_feature(&self, feature: &Feature) -> &[Affine2] {
        feature
            .placement_group
            .map_or(&IDENTITY_PLACEMENT, |group| {
                self.feature_placement_groups[group as usize]
                    .placements
                    .slice(&self.feature_placements)
            })
    }

    /// Layer-space bounds of a feature's local paths across its placements.
    pub fn placed_paths_bbox(&self, feature: &Feature) -> BBox {
        let local = self.arena.paths_bbox(feature.paths);
        self.placements_for_feature(feature)
            .iter()
            .map(|&placement| local.transformed(placement))
            .fold(BBox::empty(), BBox::union)
    }

    /// Detach every contour occurrence of a feature in layer coordinates.
    pub fn placed_feature_contours(&self, feature: &Feature) -> Vec<ContourBuf> {
        self.placements_for_feature(feature)
            .iter()
            .flat_map(|&placement| {
                feature
                    .paths
                    .indices()
                    .flat_map(move |path| self.transformed_path_contours(path, placement))
            })
            .collect()
    }

    pub fn warn(&mut self, message: impl Into<String>) {
        self.diagnostics.push(Diagnostic::warning(message));
    }

    /// Consume a source layer into its final painted image through the
    /// artwork dialect, prepared at `resolution`.
    ///
    /// Imaging tolerates geometry a native writer would reject, such as
    /// zero-radius arcs; callers exporting artwork validate separately.
    pub fn into_layer_image(
        mut self,
        layer_index: usize,
        role: crate::dialects::LayerRole,
        side: crate::dialects::Side,
        resolution: Resolution,
    ) -> anyhow::Result<crate::geom::ContourSet> {
        super::process::normalize_for_artwork(&mut self, resolution)?;
        let artwork = super::lower_layer_to_artwork(&self, layer_index, role, side);
        let (mut layers, _) =
            crate::dialects::artwork::compose_owner_regions(&artwork, |_| Some(()), resolution)?;
        Ok(layers
            .pop()
            .and_then(|mut owners| owners.pop())
            .map_or_else(
                || crate::geom::ContourSet::empty(resolution),
                |(_, region)| region,
            ))
    }
}

/// One source layer with its extracted features.
#[derive(Debug, Clone)]
pub struct Layer {
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
