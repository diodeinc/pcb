use clap::ValueEnum;

pub mod accessors;
pub mod commands;
pub mod utils;

// Re-export ipc2581 for external use
pub use ipc2581;

#[derive(ValueEnum, Debug, Clone, Copy)]
pub enum OutputFormat {
    Text,
    Json,
}

#[derive(ValueEnum, Debug, Clone, Copy)]
pub enum UnitFormat {
    Mm,
    Mil,
    Inch,
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
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Bom => "BOM",
            Self::Assembly => "ASSEMBLY",
            Self::Fabrication => "FABRICATION",
            Self::Stackup => "STACKUP",
            Self::Test => "TEST",
            Self::Stencil => "STENCIL",
            Self::Dfx => "DFX",
        }
    }
}
