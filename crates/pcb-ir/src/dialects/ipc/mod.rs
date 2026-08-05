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
    ProfileOccurrence, ProfileOccurrenceRole, ProfileSet, SimpleBoardArrayLayout, View, board_bbox,
    board_instance_count, board_step_count, layout_child_repeats, layout_instances_by_kind,
    layout_repeat_instances, layout_steps_by_kind, panel_bbox, panel_step_count,
    profile_occurrences_for, root_panel_step, root_step, simple_board_array_layout,
};
pub use balancing_region::{
    BalancingRegionError, BalancingRegionOptions, BoardArrayBalancingCollection,
    BoardArrayBalancingInput, BoardArrayBalancingIntermediates, BoardArrayBalancingResult,
    BoardArrayCopperLayer, BoardArrayCopperReach, BoardArrayScopedObstacle,
    BoardArraySupportDocument, BoardArraySupportLayerGeometry, BoardArraySupportLayerPolicy,
    ClearanceCertificate, DEFAULT_BALANCING_CLEARANCE_MM, DEFAULT_BALANCING_GAP_RADIUS_MM,
    DEFAULT_BALANCING_NUMERICAL_GUARD_MM, DEFAULT_BALANCING_REGULARIZATION_RADIUS_MM,
    board_array_balancing_region, collect_board_array_balancing_input,
    collect_fab_panel_balancing_input, inspect_board_array_balancing_input,
};
pub use document::{Document, Layer};
pub use feature::{
    Feature, FeatureBucket, FeatureDomain, FeatureFlags, FeatureIntent, FeatureKind,
    FeatureMaterial, FeatureOperation, FeatureRole, FeatureSet, FeatureSpan, FiducialKind, PinRef,
    PlatingKind, SourceRef,
};
pub use layout::{
    LayoutGraph, LayoutInstance, LayoutMargins, LayoutPurpose, LayoutRepeat, LayoutStep,
    LayoutStepKind, StepProfile, StepProfileCutout,
};
pub use lower::{
    BoardArrayFabricationProfile, BoardArrayReliefFeatures, FabricationProfileOptions,
    board_array_fabrication_profile, lower_layer_to_artwork, lower_to_nc,
};
pub use spec::{Spec, SpecItem, SpecItemKind, SpecProperty, SpecRef};
pub use surface_layers::{
    PhysicalLayer, SurfaceLayerError, TwoSidedSurfaceLayers, resolve_two_sided_surface_layers,
};
pub use validate::{validate_artwork_ready, validate_homogeneous_features};
