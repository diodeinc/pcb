/// SVG Export Pipeline for IPC-2581 Copper Layers
///
/// This module implements a staged pipeline for generating high-precision SVG
/// visualizations of copper layers from IPC-2581 data. The pipeline is designed
/// for debuggability, with each stage producing validatable intermediate outputs.
///
/// ## Pipeline Stages
///
/// - **Stage 0**: Input Readiness - Parse and normalize units, build BoardContext
/// - **Stage 1**: Hierarchy & Transformation - Resolve Xforms and flatten features
/// - **Stage 2**: Padstack Expansion - Expand padstack references to actual geometry
/// - **Stage 3**: Primitive Conversion - Convert to renderable paths
/// - **Stage 4**: Boolean Flattening - Apply boolean operations per bucket
/// - **Stage 5**: Composite & Styling - Apply colors and prepare rendering
/// - **Stage 6**: SVG Emission - Generate final SVG document
mod board_context;
mod primitives;
mod resolved_feature;
mod stage0;
mod stage1;
mod stage2;
mod stage3;
mod stage4;
mod stage4_5;
mod timing;

pub mod debug;

pub use board_context::BoardContext;
pub use resolved_feature::{FeatureBucket, ResolvedFeature, ResolvedGeometry};
pub use stage0::build_board_context;
pub use stage1::resolve_features;
pub use stage2::expand_padstacks;
pub use stage3::{convert_to_paths, LayerPaths, PathFeature};
pub use stage4::{flatten_layers, BucketStats, FlattenedLayer};
pub use stage4_5::{subtract_drill_mask, LayerDrillMask};
pub use timing::PipelineTiming;

use crate::Ipc2581Error;

pub type Result<T> = std::result::Result<T, Ipc2581Error>;
