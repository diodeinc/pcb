use clap::ValueEnum;
use ipc2581::Mode;

pub mod accessors;
pub mod commands;
pub mod geometry;
pub mod gerber;
pub mod panel;
mod steps;
pub mod utils;

// Re-export ipc2581 for external use
pub use ipc2581;

#[derive(ValueEnum, Debug, Clone, Copy)]
pub enum OutputFormat {
    Text,
    Json,
}

#[derive(ValueEnum, Debug, Clone, Copy)]
pub enum RenderFormat {
    Auto,
    Svg,
    Png,
}

#[derive(ValueEnum, Debug, Clone, Copy)]
pub enum UnitFormat {
    Mm,
    Mil,
    Inch,
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutTarget {
    Board,
    Panel,
    Layout,
}

impl LayoutTarget {
    pub fn profile_set(self) -> pcb_ir::dialects::ipc::ProfileSet {
        match self {
            Self::Board => pcb_ir::dialects::ipc::ProfileSet::BoardOutlines,
            Self::Panel => pcb_ir::dialects::ipc::ProfileSet::FabricationOutlines,
            Self::Layout => pcb_ir::dialects::ipc::ProfileSet::LayoutBoundaries,
        }
    }
}

#[derive(ValueEnum, Debug, Clone, Copy)]
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
