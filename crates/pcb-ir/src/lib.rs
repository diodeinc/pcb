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
//!   [`dialects::nc`] and [`dialects::placement`] carry drill/rout and
//!   component placement data.
//! - [`render`] turns mask documents into SVG, PNG, or terminal output.
//!
//! All geometry is canonically in millimeters.

pub mod dialects;
pub mod geom;
pub mod render;
