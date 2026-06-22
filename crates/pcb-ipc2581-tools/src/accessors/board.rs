use serde::{Deserialize, Serialize};

use super::IpcAccessor;
use crate::geometry;
use crate::utils::Length;

/// Board physical dimensions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardDimensions {
    pub width: Length,
    pub height: Length,
}

pub type PanelDimensions = BoardDimensions;

impl BoardDimensions {
    pub fn new(width_mm: f64, height_mm: f64) -> Self {
        Self {
            width: Length::from_mm(width_mm),
            height: Length::from_mm(height_mm),
        }
    }

    // Legacy accessors for backward compatibility
    pub fn width_mm(&self) -> f64 {
        self.width.mm()
    }

    pub fn height_mm(&self) -> f64 {
        self.height.mm()
    }

    pub fn width_inch(&self) -> f64 {
        self.width.inch()
    }

    pub fn height_inch(&self) -> f64 {
        self.height.inch()
    }
}

/// Board and panel geometry summary extracted from canonical IPC layout IR.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardLayoutInfo {
    pub board_dimensions: Option<BoardDimensions>,
    pub panel: Option<PanelInfo>,
}

/// IPC-2581 panel placement summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PanelInfo {
    pub step_name: String,
    pub board_count: usize,
    pub board_instances: usize,
    pub dimensions: Option<PanelDimensions>,
}

/// Board stackup information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StackupInfo {
    pub thickness: Option<Length>,
    pub layer_count: usize,
}

impl StackupInfo {
    // Legacy accessor for backward compatibility
    pub fn overall_thickness_mm(&self) -> Option<f64> {
        self.thickness.map(|t| t.mm())
    }
}

impl<'a> IpcAccessor<'a> {
    /// Extract board and panel geometry from canonical IPC layout IR.
    pub fn board_layout_info(&self) -> Option<BoardLayoutInfo> {
        let doc = geometry::extract_profiles(self.ipc()).ok()?;
        let board_dimensions =
            pcb_ir::dialects::ipc::board_bbox(&doc).and_then(dimensions_from_bbox);
        let panel = pcb_ir::dialects::ipc::root_panel_step(&doc).map(|(_, panel_step)| PanelInfo {
            step_name: self.ipc().resolve(panel_step.source_step_ref).to_string(),
            board_count: pcb_ir::dialects::ipc::board_step_count(&doc),
            board_instances: pcb_ir::dialects::ipc::board_instance_count(&doc),
            dimensions: pcb_ir::dialects::ipc::panel_bbox(&doc).and_then(dimensions_from_bbox),
        });

        if board_dimensions.is_none() && panel.is_none() {
            return None;
        }

        Some(BoardLayoutInfo {
            board_dimensions,
            panel,
        })
    }

    /// Extract board physical dimensions from canonical IPC profile geometry.
    pub fn board_dimensions(&self) -> Option<BoardDimensions> {
        self.board_layout_info()?.board_dimensions
    }

    /// Extract panel physical dimensions from canonical IPC panel geometry.
    pub fn panel_dimensions(&self) -> Option<PanelDimensions> {
        self.board_layout_info()?.panel?.dimensions
    }

    /// Extract panel placement information from canonical IPC layout geometry.
    pub fn panel_info(&self) -> Option<PanelInfo> {
        self.board_layout_info()?.panel
    }

    /// Extract stackup information (thickness and layer count)
    pub fn stackup_info(&self) -> Option<StackupInfo> {
        let ecad = self.ecad()?;
        let stackup = ecad.cad_data.stackups.first()?;

        Some(StackupInfo {
            thickness: stackup.overall_thickness.map(Length::from),
            layer_count: stackup.layers.len(),
        })
    }
}

fn dimensions_from_bbox(bbox: pcb_ir::common::BBox) -> Option<BoardDimensions> {
    if bbox.width() > 0.0 && bbox.height() > 0.0 {
        Some(BoardDimensions::new(bbox.width(), bbox.height()))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn board_dimensions_use_arc_aware_profile_ir() {
        let ipc = ipc2581::Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Step name="board" type="BOARD">
        <Profile>
          <Polygon>
            <PolyBegin x="1" y="0"/>
            <PolyStepCurve x="-1" y="0" centerX="0" centerY="0" clockwise="false"/>
            <PolyStepCurve x="1" y="0" centerX="0" centerY="0" clockwise="false"/>
          </Polygon>
        </Profile>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();
        let accessor = IpcAccessor::new(&ipc);

        let dimensions = accessor.board_dimensions().unwrap();

        assert_close(dimensions.width_mm(), 2.0);
        assert_close(dimensions.height_mm(), 2.0);
    }

    #[test]
    fn board_dimensions_use_repeated_board_definition_not_panel_extents() {
        let ipc = ipc2581::Ipc2581::parse(panel_fixture()).unwrap();
        let accessor = IpcAccessor::new(&ipc);

        let dimensions = accessor.board_dimensions().unwrap();

        assert_close(dimensions.width_mm(), 10.0);
        assert_close(dimensions.height_mm(), 5.0);
    }

    #[test]
    fn panel_dimensions_use_primary_step_repeated_profile_extents() {
        let ipc = ipc2581::Ipc2581::parse(panel_fixture()).unwrap();
        let accessor = IpcAccessor::new(&ipc);

        let layout = accessor.board_layout_info().unwrap();
        let panel = layout.panel.as_ref().unwrap();
        let dimensions = panel.dimensions.as_ref().unwrap();

        assert_close(dimensions.width_mm(), 30.0);
        assert_close(dimensions.height_mm(), 5.0);
        assert_eq!(panel.step_name, "panel");
        assert_eq!(panel.board_count, 1);
        assert_eq!(panel.board_instances, 2);
    }

    fn panel_fixture() -> &'static str {
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="panel"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Step name="board" type="BOARD">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="10" y="0"/>
            <PolyStepSegment x="10" y="5"/>
            <PolyStepSegment x="0" y="5"/>
          </Polygon>
        </Profile>
      </Step>
      <Step name="panel" type="PALLET">
        <StepRepeat stepRef="board" x="10" y="20" nx="2" ny="1" dx="20" dy="0"/>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-9,
            "expected {expected}, got {actual}"
        );
    }
}
