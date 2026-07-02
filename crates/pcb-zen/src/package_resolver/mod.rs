//! Package manifest dependency resolver shared by CLI, LSP, and WASM-facing eval paths.

mod manifest;
mod materialize;
mod mvs;
mod resolve;
mod scan;
mod versions;

pub use materialize::plan_vendor_selected;
pub use mvs::{DepGraph, DepGraphNode, PackageResolution, PackageResolver};
pub use pcb_zen_core::resolution::{
    FrozenDepId as ResolvedDepId, compatibility_lane, parse_lane_qualified_key,
};
pub use resolve::{
    build_frozen_resolution_maps, resolve_workspace_dependencies, target_package_urls_for_path,
};
pub use versions::SpecVersionResolver;
