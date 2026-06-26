use crate::common::*;
pub use crate::dialects::path::{PathCmd, PathOp};

pub fn paint_polarity(polarity: Polarity) -> PaintPolarity {
    match polarity {
        Polarity::Dark => PaintPolarity::Dark,
        Polarity::Clear => PaintPolarity::Clear,
    }
}

pub fn layer_role(file_function: &[String]) -> LayerRole {
    match file_function.first().map(String::as_str) {
        Some("Copper") => LayerRole::Copper,
        Some("Soldermask") => LayerRole::Soldermask,
        Some("Paste") => LayerRole::Paste,
        Some("Legend") => LayerRole::Legend,
        Some("Profile") => LayerRole::Profile,
        _ => LayerRole::Other,
    }
}

pub fn layer_side(file_function: &[String]) -> Side {
    if file_function.iter().any(|field| field == "Top") {
        Side::Top
    } else if file_function.iter().any(|field| field == "Bot") {
        Side::Bottom
    } else if file_function.iter().any(|field| field == "Inr") {
        Side::Inner
    } else {
        Side::None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeatureKind {
    Flash,
    Draw,
    Arc,
    Region,
    Composite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeatureBucket {
    Pad,
    Trace,
    Fill,
    Cutout,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Polarity {
    Dark,
    Clear,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mirroring {
    None,
    X,
    Y,
    XY,
}

impl MirrorAxes for Mirroring {
    fn mirror_x(self) -> bool {
        matches!(self, Self::X | Self::XY)
    }

    fn mirror_y(self) -> bool {
        matches!(self, Self::Y | Self::XY)
    }
}

pub mod compare;
pub mod raster;
pub mod svg;
pub mod terminal;
