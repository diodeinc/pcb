//! `pcb-route-native`: geometry extraction and grid rasterization for a
//! from-scratch Rust autorouter.
//!
//! This crate currently covers exactly one thing: turning a parsed
//! `.kicad_pcb` file into a single-layer routing grid (obstacle / keepout /
//! free cells). It intentionally does **not** do any pathfinding yet — that's
//! a separate, later addition once this grid model has landed and been
//! reviewed on its own.

pub mod geometry;
pub mod grid;

pub use geometry::{
    BoardGeometry, BoardOutline, Footprint, GeometryError, Pad, PadShape, Point, parse_board,
};
pub use grid::{CellState, GridConfig, RoutingGrid, build_grid};
