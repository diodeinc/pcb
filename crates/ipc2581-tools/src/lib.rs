//! IPC-2581 visualization and export tools
//!
//! This crate provides rendering, visualization, and export functionality
//! for IPC-2581 documents parsed by the `ipc2581` crate.
//!
//! ## Features
//!
//! - **SVG Export**: Render PCB layers to SVG format
//! - **HTML Reports**: Generate interactive HTML documentation
//! - **Board Outline**: Extract and render board outlines
//! - **Copper Layers**: Render copper layer features with proper geometry
//! - **CLI Tool**: `ipc2581` binary for command-line processing

// Re-export the core parser for convenience
pub use ipc2581::*;

// Export visualization modules
pub mod board_outline;
pub mod copper_layer;
pub mod geometry;
pub mod html_generator;
pub mod svg_export;
