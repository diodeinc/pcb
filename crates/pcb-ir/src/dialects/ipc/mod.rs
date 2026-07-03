//! The IPC-2581 source dialect: layout graph, layers, feature sets, specs,
//! and source-faithful feature geometry.
//!
//! Lowering flows out of this dialect: [`process`] normalizes documents,
//! [`lower`] produces per-layer [`artwork`](crate::dialects::artwork) and
//! fabrication profiles, [`relief`] computes V-score route reliefs, and
//! [`analysis`] derives board/panel views from the layout graph.

pub mod analysis;
pub mod document;
pub mod feature;
pub mod layout;
pub mod lower;
pub mod process;
pub mod relief;
pub mod spec;
pub mod validate;

pub use analysis::{
    ProfileOccurrence, ProfileOccurrenceRole, ProfileSet, SimpleBoardArrayLayout, View, board_bbox,
    board_instance_count, board_step_count, layout_child_repeats, layout_instances_by_kind,
    layout_repeat_instances, layout_steps_by_kind, panel_bbox, panel_step_count,
    profile_occurrences_for, root_panel_step, root_step, simple_board_array_layout,
};
pub use document::{Document, Layer};
pub use feature::{
    Feature, FeatureBucket, FeatureDomain, FeatureFlags, FeatureIntent, FeatureKind,
    FeatureMaterial, FeatureOperation, FeatureRole, FeatureSet, FeatureSpan, FiducialKind, PinRef,
    PlatingKind, SourceRef,
};
pub use layout::{
    LayoutGraph, LayoutInstance, LayoutMargins, LayoutRepeat, LayoutStep, LayoutStepKind,
    StepProfile, StepProfileCutout,
};
pub use lower::{
    BoardArrayFabricationProfile, BoardArrayReliefFeatures, FabricationProfileOptions,
    board_array_fabrication_profile, lower_layer_to_artwork, lower_to_nc,
};
pub use spec::{Spec, SpecItem, SpecItemKind, SpecProperty, SpecRef};
pub use validate::{validate_artwork_ready, validate_homogeneous_features};
