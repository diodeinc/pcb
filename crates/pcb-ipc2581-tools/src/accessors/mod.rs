use ipc2581::Ipc2581;
use ipc2581::types::{Ecad, Step};

mod board;
mod bom;
mod components;
mod drills;
mod layers;
mod metadata;
mod stackup;

// Re-export types
pub use board::{BoardDimensions, StackupInfo};
pub use bom::{AvlLookup, BomStats, CharacteristicsData};
pub use components::ComponentStats;
pub use drills::{DrillHoleType, DrillSize, DrillStats, DrillTypeDistribution};
pub use layers::{LayerStats, NetStats};
pub use metadata::{FileMetadata, SoftwareInfo};
pub use stackup::{
    ColorInfo, ImpedanceControlInfo, MaterialInfo, StackupDetails, StackupLayerInfo,
    StackupLayerType, SurfaceFinishInfo,
};

/// Main accessor for IPC-2581 data extraction
///
/// Provides high-level methods to extract and transform IPC-2581 data
/// into domain models suitable for CLI output and further processing.
pub struct IpcAccessor<'a> {
    ipc: &'a Ipc2581,
}

impl<'a> IpcAccessor<'a> {
    pub fn new(ipc: &'a Ipc2581) -> Self {
        Self { ipc }
    }

    pub fn ipc(&self) -> &'a Ipc2581 {
        self.ipc
    }

    /// Get ECAD section (common helper)
    fn ecad(&self) -> Option<&Ecad> {
        self.ipc.ecad()
    }

    /// Get first step from ECAD (common helper)
    pub fn first_step(&self) -> Option<&Step> {
        self.ecad()?.cad_data.steps.first()
    }
}
