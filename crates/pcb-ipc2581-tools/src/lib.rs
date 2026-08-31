// Use pipe-safe replacements for standard printing macros in CLI output paths.
#[cfg(feature = "cli")]
#[macro_use(print, println, eprintln)]
extern crate anstream;

use ipc2581::Mode;

pub mod accessors;
pub mod board_array;
pub mod commands;
pub mod copper_balance;
mod generated;
pub mod geometry;
pub mod gerber;
pub mod layers;
pub mod manufacturing;
pub mod placement;
mod steps;
pub mod utils;
pub mod warp;
pub mod xnc;

// Re-export ipc2581 for external use
pub use ipc2581;

#[cfg_attr(feature = "cli", derive(clap::ValueEnum))]
#[derive(Debug, Clone, Copy)]
pub enum OutputFormat {
    Text,
    Json,
}

#[cfg_attr(feature = "cli", derive(clap::ValueEnum))]
#[derive(Debug, Clone, Copy)]
pub enum RenderFormat {
    Auto,
    Svg,
    Png,
}

#[cfg_attr(feature = "cli", derive(clap::ValueEnum))]
#[derive(Debug, Clone, Copy)]
pub enum UnitFormat {
    Mm,
    Mil,
    Inch,
}

/// What a command materializes from an IPC-2581 file.
///
/// One vocabulary, one default, for every command that produces artwork or
/// outlines. `board-array` is the root step of the file with every repeat
/// materialized, and is identical to `board` for a plain board file.
#[cfg_attr(feature = "cli", derive(clap::ValueEnum))]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LayoutTarget {
    /// The canonical source board, as if it were fabricated alone.
    Board,
    /// The file's root step with every nested repeat materialized.
    #[default]
    #[cfg_attr(feature = "cli", value(name = "board-array", alias = "panel"))]
    BoardArray,
}

impl LayoutTarget {
    pub fn artwork_scope(self) -> pcb_ir::dialects::ipc::ArtworkScope {
        match self {
            Self::Board => pcb_ir::dialects::ipc::ArtworkScope::Board,
            Self::BoardArray => pcb_ir::dialects::ipc::ArtworkScope::ArrayFlattened,
        }
    }
}

#[cfg_attr(feature = "cli", derive(clap::ValueEnum))]
#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ViewMode {
    Bom,
    Assembly,
    Fabrication,
    Stackup,
    Test,
    Stencil,
    Dfx,
}

impl ViewMode {
    pub fn as_ipc_mode(self) -> Mode {
        match self {
            Self::Bom => Mode::Bom,
            Self::Assembly => Mode::Assembly,
            Self::Fabrication => Mode::Fabrication,
            Self::Stackup => Mode::Stackup,
            Self::Test => Mode::Test,
            Self::Stencil => Mode::Stencil,
            Self::Dfx => Mode::Dfx,
        }
    }

    pub fn as_str(&self) -> &'static str {
        self.as_ipc_mode().as_str()
    }
}
