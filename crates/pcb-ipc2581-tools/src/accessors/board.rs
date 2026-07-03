use serde::{Deserialize, Serialize};

use super::IpcAccessor;
use crate::geometry;
use crate::utils::Length;
use pcb_ir::dialects::ipc::{LayoutMargins, SimpleBoardArrayLayout};

/// Board physical dimensions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardDimensions {
    pub width: Length,
    pub height: Length,
}

pub type BoardArrayDimensions = BoardDimensions;

impl BoardDimensions {
    pub fn new(width_mm: f64, height_mm: f64) -> Self {
        Self {
            width: Length::from_mm(width_mm),
            height: Length::from_mm(height_mm),
        }
    }

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

/// Board and board-array geometry summary extracted from canonical IPC layout IR.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardLayoutInfo {
    pub board_name: Option<String>,
    pub board_dimensions: Option<BoardDimensions>,
    pub board_array: Option<BoardArrayInfo>,
}

/// IPC-2581 board-array placement summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardArrayInfo {
    pub step_name: String,
    pub board_count: usize,
    pub board_instances: usize,
    pub dimensions: Option<BoardArrayDimensions>,
    pub grid: Option<BoardArrayGridInfo>,
}

/// Best-effort summary of a simple rectangular board array.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardArrayGridInfo {
    pub columns: u32,
    pub rows: u32,
    pub board_width: Length,
    pub board_height: Length,
    pub pitch_x: Option<Length>,
    pub pitch_y: Option<Length>,
    pub board_margin: Option<BoardArrayBoardMargin>,
    pub edge_rail_width: Option<Length>,
    pub edge_rail: BoardArrayMargins,
    pub margins: BoardArrayMargins,
}

/// Distances from the tiled board array to the array profile extents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardArrayMargins {
    pub left: Length,
    pub right: Length,
    pub bottom: Length,
    pub top: Length,
}

/// Margin around each board bbox before the margin-expanded board tile is repeated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardArrayBoardMargin {
    pub top: Length,
    pub right: Length,
    pub bottom: Length,
    pub left: Length,
}

impl BoardArrayMargins {
    pub fn format_shorthand<F>(&self, format_length: F) -> String
    where
        F: FnMut(f64) -> String,
    {
        format_box_shorthand(
            self.top.mm(),
            self.right.mm(),
            self.bottom.mm(),
            self.left.mm(),
            format_length,
        )
    }
}

impl BoardArrayBoardMargin {
    pub fn format_shorthand<F>(&self, format_length: F) -> String
    where
        F: FnMut(f64) -> String,
    {
        format_box_shorthand(
            self.top.mm(),
            self.right.mm(),
            self.bottom.mm(),
            self.left.mm(),
            format_length,
        )
    }
}

/// Board stackup information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StackupInfo {
    pub thickness: Option<Length>,
    pub layer_count: usize,
}

impl StackupInfo {
    pub fn overall_thickness_mm(&self) -> Option<f64> {
        self.thickness.map(|t| t.mm())
    }
}

impl<'a> IpcAccessor<'a> {
    /// Extract board and board-array geometry from canonical IPC layout IR.
    pub fn board_layout_info(&self) -> Option<BoardLayoutInfo> {
        let doc = geometry::extract_layout(self.ipc()).ok()?;
        let board_name = pcb_ir::dialects::ipc::layout_steps_by_kind(
            &doc,
            pcb_ir::dialects::ipc::LayoutStepKind::Board,
        )
        .next()
        .map(|(_, step)| self.ipc().resolve(step.source_step_ref).to_string());
        let board_dimensions =
            pcb_ir::dialects::ipc::board_bbox(&doc).and_then(dimensions_from_bbox);
        let simple_array = pcb_ir::dialects::ipc::simple_board_array_layout(&doc);
        let board_array =
            pcb_ir::dialects::ipc::root_panel_step(&doc).map(|(_, panel_step)| BoardArrayInfo {
                step_name: self.ipc().resolve(panel_step.source_step_ref).to_string(),
                board_count: pcb_ir::dialects::ipc::board_step_count(&doc),
                board_instances: pcb_ir::dialects::ipc::board_instance_count(&doc),
                dimensions: pcb_ir::dialects::ipc::panel_bbox(&doc).and_then(dimensions_from_bbox),
                grid: simple_array.map(board_array_grid_from_ir),
            });

        if board_dimensions.is_none() && board_array.is_none() {
            return None;
        }

        Some(BoardLayoutInfo {
            board_name,
            board_dimensions,
            board_array,
        })
    }

    /// Extract board physical dimensions from canonical IPC profile geometry.
    pub fn board_dimensions(&self) -> Option<BoardDimensions> {
        self.board_layout_info()?.board_dimensions
    }

    /// Extract board-array physical dimensions from canonical IPC geometry.
    pub fn board_array_dimensions(&self) -> Option<BoardArrayDimensions> {
        self.board_layout_info()?.board_array?.dimensions
    }

    /// Extract board-array placement information from canonical IPC layout geometry.
    pub fn board_array_info(&self) -> Option<BoardArrayInfo> {
        self.board_layout_info()?.board_array
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

fn dimensions_from_bbox(bbox: pcb_ir::geom::BBox) -> Option<BoardDimensions> {
    if bbox.width() > 0.0 && bbox.height() > 0.0 {
        Some(BoardDimensions::new(bbox.width(), bbox.height()))
    } else {
        None
    }
}

const GRID_EPSILON: f64 = 1e-6;

fn nearly_equal(a: f64, b: f64) -> bool {
    (a - b).abs() <= GRID_EPSILON
}

fn format_box_shorthand<F>(
    top: f64,
    right: f64,
    bottom: f64,
    left: f64,
    mut format_length: F,
) -> String
where
    F: FnMut(f64) -> String,
{
    if nearly_equal(top, right) && nearly_equal(top, bottom) && nearly_equal(top, left) {
        return format_length(top);
    }
    if nearly_equal(top, bottom) && nearly_equal(right, left) {
        return format!(
            "{} vertical / {} horizontal",
            format_length(top),
            format_length(right)
        );
    }
    format!(
        "T {} / R {} / B {} / L {}",
        format_length(top),
        format_length(right),
        format_length(bottom),
        format_length(left)
    )
}

fn board_array_grid_from_ir(layout: SimpleBoardArrayLayout) -> BoardArrayGridInfo {
    BoardArrayGridInfo {
        columns: layout.columns,
        rows: layout.rows,
        board_width: Length::from_mm(layout.board_width),
        board_height: Length::from_mm(layout.board_height),
        pitch_x: layout.pitch_x.map(Length::from_mm),
        pitch_y: layout.pitch_y.map(Length::from_mm),
        board_margin: layout.board_margin.map(board_margin_from_ir),
        edge_rail_width: layout.edge_rail_width.map(Length::from_mm),
        edge_rail: margins_from_ir(layout.edge_rail),
        margins: margins_from_ir(layout.margins),
    }
}

fn margins_from_ir(margins: LayoutMargins) -> BoardArrayMargins {
    BoardArrayMargins {
        left: Length::from_mm(margins.left),
        right: Length::from_mm(margins.right),
        bottom: Length::from_mm(margins.bottom),
        top: Length::from_mm(margins.top),
    }
}

fn board_margin_from_ir(margin: LayoutMargins) -> BoardArrayBoardMargin {
    BoardArrayBoardMargin {
        top: Length::from_mm(margin.top),
        right: Length::from_mm(margin.right),
        bottom: Length::from_mm(margin.bottom),
        left: Length::from_mm(margin.left),
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
    fn board_array_dimensions_use_primary_step_repeated_profile_extents() {
        let ipc = ipc2581::Ipc2581::parse(panel_fixture()).unwrap();
        let accessor = IpcAccessor::new(&ipc);

        let layout = accessor.board_layout_info().unwrap();
        assert_eq!(layout.board_name.as_deref(), Some("board"));
        let board_array = layout.board_array.as_ref().unwrap();
        let dimensions = board_array.dimensions.as_ref().unwrap();

        assert_close(dimensions.width_mm(), 30.0);
        assert_close(dimensions.height_mm(), 5.0);
        assert_eq!(board_array.step_name, "panel");
        assert_eq!(board_array.board_count, 1);
        assert_eq!(board_array.board_instances, 2);
        let grid = board_array.grid.as_ref().unwrap();
        assert_eq!(grid.columns, 2);
        assert_eq!(grid.rows, 1);
        assert_close(grid.pitch_x.unwrap().mm() - grid.board_width.mm(), 10.0);
        assert!(grid.edge_rail_width.is_none());
    }

    #[test]
    fn board_array_grid_recovers_board_margin_rail_and_gaps() {
        let ipc = ipc2581::Ipc2581::parse(generated_panel_fixture()).unwrap();
        let accessor = IpcAccessor::new(&ipc);

        let layout = accessor.board_layout_info().unwrap();
        let grid = layout.board_array.as_ref().unwrap().grid.as_ref().unwrap();

        assert_eq!(grid.columns, 3);
        assert_eq!(grid.rows, 2);
        assert_close(grid.board_width.mm(), 10.0);
        assert_close(grid.board_height.mm(), 5.0);
        assert_close(grid.pitch_x.unwrap().mm(), 12.0);
        assert_close(grid.pitch_y.unwrap().mm(), 8.0);
        assert_close(grid.edge_rail_width.unwrap().mm(), 4.0);
        let board_margin = grid.board_margin.as_ref().unwrap();
        assert_close(board_margin.left.mm(), 1.0);
        assert_close(board_margin.right.mm(), 1.0);
        assert_close(board_margin.bottom.mm(), 1.5);
        assert_close(board_margin.top.mm(), 1.5);
        assert_close(grid.margins.left.mm(), 4.0);
        assert_close(grid.margins.right.mm(), 4.0);
        assert_close(grid.margins.bottom.mm(), 4.0);
        assert_close(grid.margins.top.mm(), 4.0);
    }

    #[test]
    fn board_cell_array_accepts_zero_pitch_on_unused_axis() {
        let ipc = ipc2581::Ipc2581::parse(generated_single_column_panel_fixture()).unwrap();
        let accessor = IpcAccessor::new(&ipc);

        let layout = accessor.board_layout_info().unwrap();
        let grid = layout.board_array.as_ref().unwrap().grid.as_ref().unwrap();

        assert_eq!(grid.columns, 1);
        assert_eq!(grid.rows, 2);
        assert!(grid.pitch_x.is_none());
        assert_close(grid.pitch_y.unwrap().mm(), 8.0);
        let board_margin = grid.board_margin.as_ref().unwrap();
        assert_close(board_margin.left.mm(), 1.0);
        assert_close(board_margin.right.mm(), 1.0);
        assert_close(board_margin.bottom.mm(), 1.5);
        assert_close(board_margin.top.mm(), 1.5);
    }

    fn panel_fixture() -> &'static str {
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="panel"/>
    <StepRef name="board_cell"/>
    <StepRef name="board"/>
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

    fn generated_panel_fixture() -> &'static str {
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
      <Step name="board_cell" type="PALLET">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="12" y="0"/>
            <PolyStepSegment x="12" y="8"/>
            <PolyStepSegment x="0" y="8"/>
          </Polygon>
        </Profile>
        <StepRepeat stepRef="board" x="1" y="1.5" nx="1" ny="1" dx="0" dy="0"/>
      </Step>
      <Step name="panel" type="PALLET">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="0" y="24"/>
            <PolyStepSegment x="44" y="24"/>
            <PolyStepSegment x="44" y="0"/>
          </Polygon>
        </Profile>
        <StepRepeat stepRef="board_cell" x="4" y="4" nx="3" ny="2" dx="12" dy="8"/>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#
    }

    fn generated_single_column_panel_fixture() -> &'static str {
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
      <Step name="board_cell" type="PALLET">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="12" y="0"/>
            <PolyStepSegment x="12" y="8"/>
            <PolyStepSegment x="0" y="8"/>
          </Polygon>
        </Profile>
        <StepRepeat stepRef="board" x="1" y="1.5" nx="1" ny="1" dx="0" dy="0"/>
      </Step>
      <Step name="panel" type="PALLET">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="0" y="24"/>
            <PolyStepSegment x="20" y="24"/>
            <PolyStepSegment x="20" y="0"/>
          </Polygon>
        </Profile>
        <StepRepeat stepRef="board_cell" x="4" y="4" nx="1" ny="2" dx="0" dy="8"/>
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
