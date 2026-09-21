//! The IPC-2581 source dialect: layout graph, layers, feature sets, specs,
//! and source-faithful feature geometry.
//!
//! Lowering flows out of this dialect: [`process`] normalizes documents,
//! [`lower`] produces per-layer [`artwork`](crate::dialects::artwork) and
//! fabrication profiles, [`relief`] computes V-score route reliefs, and
//! [`analysis`] derives board/panel views from the layout graph.

pub mod analysis;
pub mod balancing_region;
pub mod document;
pub mod feature;
pub mod layout;
pub mod lower;
pub mod process;
pub mod relief;
pub mod spec;
pub mod surface_layers;
pub mod validate;

pub use analysis::{
    ArtworkScope, ProfileOccurrence, ProfileOccurrenceRole, ProfileSet, SimpleBoardArrayLayout,
    board_bbox, board_instance_count, board_step_count, layout_child_repeats,
    layout_instances_by_kind, layout_repeat_instances, layout_steps_by_kind, panel_bbox,
    panel_step_count, profile_occurrences_for, root_panel_step, root_step,
    simple_board_array_layout,
};
pub use balancing_region::{
    BalancingRegionError, BalancingRegionOptions, BoardArrayBalancingCollection,
    BoardArrayBalancingInput, BoardArrayBalancingIntermediates, BoardArrayBalancingResult,
    BoardArrayCopperLayer, BoardArrayCopperReach, BoardArrayScopedObstacle,
    BoardArraySupportDocument, BoardArraySupportLayerGeometry, BoardArraySupportLayerPolicy,
    ClearanceCertificate, DEFAULT_BALANCING_CLEARANCE_MM, DEFAULT_BALANCING_GAP_RADIUS_MM,
    DEFAULT_BALANCING_REGULARIZATION_RADIUS_MM, board_array_balancing_region,
    collect_board_array_balancing_input, collect_fab_panel_balancing_input,
    inspect_board_array_balancing_input,
};
pub use document::{Document, Layer};
pub use feature::{
    CopperBalanceVoid, Feature, FeatureBucket, FeatureDomain, FeatureIntent, FeatureKind,
    FeatureMaterial, FeatureOperation, FeaturePlacementGroup, FeatureRole, FeatureSet, FeatureSpan,
    FiducialKind, GeometryUsage, PinRef, PlatingKind, PrimitiveRef, SimpleShape, SourceRef,
};
pub use layout::{
    LayoutGraph, LayoutInstance, LayoutMargins, LayoutPurpose, LayoutRepeat, LayoutStep,
    LayoutStepKind, StepProfile, StepProfileCutout,
};
pub use lower::{
    ArtworkTarget, BoardArrayFabricationProfile, BoardArrayReliefFeatures,
    FabricationProfileOptions, board_array_fabrication_profile, contour_flash_aperture,
    lower_layer_to_artwork, lower_layer_to_artwork_objects_with, lower_layer_to_artwork_with,
    lower_to_nc,
};
pub use spec::{Spec, SpecItem, SpecItemKind, SpecProperty, SpecRef};
pub use surface_layers::{
    PhysicalLayer, SurfaceLayerError, TwoSidedSurfaceLayers, resolve_two_sided_surface_layers,
};
pub use validate::validate_artwork_ready;

/// A symbol that equals another exactly when their indices are equal. The
/// dialect compares names and never resolves them, so tests need no text.
#[cfg(test)]
pub(crate) fn test_symbol(index: u32) -> ipc2581::Symbol {
    let mut interner = ipc2581::Interner::new();
    (0..=index)
        .map(|index| interner.intern(&index.to_string()))
        .last()
        .expect("the range is never empty")
}
