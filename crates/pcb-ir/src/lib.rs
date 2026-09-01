//! Intermediate representations, geometry passes, and renderers for PCB
//! fabrication data.
//!
//! The crate is organized MLIR-style:
//!
//! - [`geom`] is the shared geometry substrate: points, transforms, path
//!   commands, regularized regions, primitive shapes, and the flat
//!   [`geom::PathArena`] every dialect document embeds.
//! - [`dialects`] hold the representations at each level of lowering:
//!   [`dialects::ipc`] (source-faithful IPC-2581 geometry) lowers to
//!   [`dialects::artwork`] (ordered fabrication object streams), which
//!   composes to [`dialects::mask`] (final positive layer images).
//!   [`dialects::assembly`] joins source-independent component, BOM, package,
//!   AVL, and layout facts and lowers to [`dialects::placement`].
//!   [`dialects::nc`] carries drill and rout data.
//! - [`render`] turns mask documents into SVG, PNG, or terminal output.
//!
//! All geometry is canonically in millimeters.
//!
//! All dialects, imports, geometry passes, and SVG/PNG renderers support
//! `wasm32-unknown-unknown`. Copper balancing runs sequentially on WebAssembly;
//! native builds retain parallel per-layer solves. Terminal renderers are
//! available only on native targets. Rasterization requires no system fonts.

pub mod dialects;
pub mod geom;
pub mod import;
pub mod render;
