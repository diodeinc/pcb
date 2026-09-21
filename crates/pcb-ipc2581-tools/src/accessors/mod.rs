use ipc2581::Ipc2581;
use ipc2581::types::{Ecad, Step};
use pcb_ir::dialects::ipc::{LayoutStepKind, layout_steps_by_kind};

use crate::geometry::{self, GeometryDocument};

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
pub use bom::{Alternative, AvlLookup, CharacteristicsData};
pub use components::ComponentStats;
pub use drills::{DrillHoleType, DrillSize, DrillStats, DrillTypeDistribution, drill_stats};
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
    layout: Option<GeometryDocument>,
    board_step: Option<&'a Step>,
}

impl<'a> IpcAccessor<'a> {
    pub fn new(ipc: &'a Ipc2581) -> Self {
        let layout = geometry::extract_layout(ipc).ok();
        // The first board the layout reaches from the primary step: the
        // primary step itself, or the board an array repeats.
        let board_step = layout.as_ref().and_then(|layout| {
            let (_, board) = layout_steps_by_kind(layout, LayoutStepKind::Board).next()?;
            ipc.ecad()?
                .cad_data
                .steps
                .iter()
                .find(|step| step.name == board.source_step_ref)
        });
        Self {
            ipc,
            layout,
            board_step,
        }
    }

    pub fn ipc(&self) -> &'a Ipc2581 {
        self.ipc
    }

    /// Get ECAD section (common helper)
    fn ecad(&self) -> Option<&'a Ecad> {
        self.ipc.ecad()
    }

    /// The layout graph of the primary step, when the file has one.
    pub fn layout(&self) -> Option<&GeometryDocument> {
        self.layout.as_ref()
    }

    /// The step that holds the design's components, nets and features.
    /// CadData order says nothing: an array file may list its pallet first.
    pub fn board_step(&self) -> Option<&'a Step> {
        self.board_step
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn board_stats_come_from_the_board_step_wherever_it_is_listed() {
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
      <Step name="panel" type="PALLET">
        <StepRepeat stepRef="board" x="5" y="5" nx="2" ny="1" dx="12" dy="0"/>
      </Step>
      <Step name="board" type="BOARD">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="10" y="0"/>
            <PolyStepSegment x="10" y="5"/>
            <PolyStepSegment x="0" y="5"/>
          </Polygon>
        </Profile>
        <Component refDes="R1" packageRef="R0402" layerRef="TOP" mountType="SMT" part="part-A">
          <Location x="1" y="1"/>
        </Component>
        <LogicalNet name="GND"/>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();
        let accessor = IpcAccessor::new(&ipc);

        assert_eq!(ipc.resolve(accessor.board_step().unwrap().name), "board");
        assert_eq!(accessor.component_stats().unwrap().total, 1);
        assert_eq!(accessor.net_stats().unwrap().count, 1);
    }
}
