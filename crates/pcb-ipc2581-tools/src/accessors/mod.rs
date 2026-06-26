use ipc2581::Ipc2581;
use ipc2581::types::{Ecad, Step};

use crate::steps;

mod board;
mod bom;
mod components;
mod drills;
mod layers;
mod metadata;
mod stackup;

// Re-export types
pub use board::{
    BoardArrayBoardMargin, BoardArrayDimensions, BoardArrayGridInfo, BoardArrayInfo,
    BoardArrayMargins, BoardDimensions, StackupInfo,
};
pub use bom::{AvlLookup, BomStats, CharacteristicsData};
pub use components::ComponentStats;
pub use drills::{DrillHoleType, DrillSize, DrillStats, DrillTypeDistribution};
pub use layers::{LayerStats, NetStats};
pub use metadata::{FileMetadata, SoftwareInfo};
pub use stackup::{
    ColorInfo, ImpedanceControlInfo, MaterialInfo, StackupDetails, StackupLayerInfo,
    StackupLayerType, SurfaceFinishCategory, SurfaceFinishInfo,
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

    /// Get the primary IPC-2581 job step from Content/StepRef.
    pub fn primary_step(&self) -> Option<&Step> {
        let ecad = self.ecad()?;
        steps::primary_step(self.ipc, &ecad.cad_data.steps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primary_step_prefers_content_step_ref_over_cad_data_order() {
        let ipc = ipc2581::Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="panel"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Step name="board" type="BOARD"/>
      <Step name="panel" type="PALLET"/>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();
        let accessor = IpcAccessor::new(&ipc);

        assert_eq!(ipc.resolve(accessor.first_step().unwrap().name), "board");
        assert_eq!(ipc.resolve(accessor.primary_step().unwrap().name), "panel");
    }
}
