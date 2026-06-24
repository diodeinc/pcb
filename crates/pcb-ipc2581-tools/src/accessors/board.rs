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

impl BoardArrayBoardMargin {
    pub fn format_shorthand<F>(&self, mut format_length: F) -> String
    where
        F: FnMut(f64) -> String,
    {
        let top = self.top.mm();
        let right = self.right.mm();
        let bottom = self.bottom.mm();
        let left = self.left.mm();

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
        let board_array =
            pcb_ir::dialects::ipc::root_panel_step(&doc).map(|(panel_index, panel_step)| {
                BoardArrayInfo {
                    step_name: self.ipc().resolve(panel_step.source_step_ref).to_string(),
                    board_count: pcb_ir::dialects::ipc::board_step_count(&doc),
                    board_instances: pcb_ir::dialects::ipc::board_instance_count(&doc),
                    dimensions: pcb_ir::dialects::ipc::panel_bbox(&doc)
                        .and_then(dimensions_from_bbox),
                    grid: infer_simple_board_array_grid(&doc, panel_index),
                }
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

fn dimensions_from_bbox(bbox: pcb_ir::common::BBox) -> Option<BoardDimensions> {
    if bbox.width() > 0.0 && bbox.height() > 0.0 {
        Some(BoardDimensions::new(bbox.width(), bbox.height()))
    } else {
        None
    }
}

type IpcGeometryDocument =
    pcb_ir::dialects::ipc::GeometryDocument<ipc2581::Symbol, ipc2581::types::LayerFunction>;

const GRID_EPSILON: f64 = 1e-6;

fn infer_simple_board_array_grid(
    doc: &IpcGeometryDocument,
    panel_step_index: u32,
) -> Option<BoardArrayGridInfo> {
    use pcb_ir::dialects::ipc::{LayoutStepKind, layout_child_repeats, layout_repeat_instances};

    let panel_step = doc.layout.steps.get(panel_step_index as usize)?;
    let panel_bbox = panel_step.bbox;
    if panel_bbox.is_empty() || panel_bbox.width() <= 0.0 || panel_bbox.height() <= 0.0 {
        return None;
    }

    let mut root_repeats = layout_child_repeats(doc, panel_step_index, None);
    let (_, repeat) = root_repeats.next()?;
    if root_repeats.next().is_some()
        || repeat.nx == 0
        || repeat.ny == 0
        || !nearly_zero(repeat.angle)
        || repeat.mirror
    {
        return None;
    }

    let board_step = doc.layout.steps.get(repeat.child_step as usize)?;
    if board_step.kind != LayoutStepKind::Board
        || board_step.bbox.is_empty()
        || board_step.bbox.width() <= 0.0
        || board_step.bbox.height() <= 0.0
    {
        return None;
    }

    let instance_count = layout_repeat_instances(doc, repeat).count() as u32;
    if instance_count != repeat.nx.saturating_mul(repeat.ny) || repeat.bbox.is_empty() {
        return None;
    }

    let board_width = board_step.bbox.width();
    let board_height = board_step.bbox.height();
    let margins = margins_between(repeat.bbox, panel_bbox)?;
    let pitch_x = (repeat.nx > 1)
        .then_some(repeat.dx)
        .filter(|pitch| pitch.is_finite() && *pitch + GRID_EPSILON >= board_width && *pitch > 0.0);
    let pitch_y = (repeat.ny > 1)
        .then_some(repeat.dy)
        .filter(|pitch| pitch.is_finite() && *pitch + GRID_EPSILON >= board_height && *pitch > 0.0);
    if (repeat.nx > 1 && pitch_x.is_none()) || (repeat.ny > 1 && pitch_y.is_none()) {
        return None;
    }
    let horizontal_gap = pitch_x.map(|pitch| clamp_zero(pitch - board_width));
    let vertical_gap = pitch_y.map(|pitch| clamp_zero(pitch - board_height));
    let edge_rail_width = infer_edge_rail_width(&margins, horizontal_gap, vertical_gap);
    let board_margin = edge_rail_width.and_then(|edge| board_margin_from_margins(&margins, edge));

    Some(BoardArrayGridInfo {
        columns: repeat.nx,
        rows: repeat.ny,
        board_width: Length::from_mm(board_width),
        board_height: Length::from_mm(board_height),
        pitch_x: pitch_x.map(Length::from_mm),
        pitch_y: pitch_y.map(Length::from_mm),
        board_margin,
        edge_rail_width: edge_rail_width.map(Length::from_mm),
        margins,
    })
}

fn margins_between(
    tiles: pcb_ir::common::BBox,
    board_array: pcb_ir::common::BBox,
) -> Option<BoardArrayMargins> {
    let left = clamp_zero(tiles.min.x - board_array.min.x);
    let right = clamp_zero(board_array.max.x - tiles.max.x);
    let bottom = clamp_zero(tiles.min.y - board_array.min.y);
    let top = clamp_zero(board_array.max.y - tiles.max.y);

    if [left, right, bottom, top]
        .iter()
        .all(|value| value.is_finite() && *value >= 0.0)
    {
        Some(BoardArrayMargins {
            left: Length::from_mm(left),
            right: Length::from_mm(right),
            bottom: Length::from_mm(bottom),
            top: Length::from_mm(top),
        })
    } else {
        None
    }
}

fn infer_edge_rail_width(
    margins: &BoardArrayMargins,
    horizontal_gap: Option<f64>,
    vertical_gap: Option<f64>,
) -> Option<f64> {
    let mut candidates = Vec::new();
    if let Some(gap) = horizontal_gap {
        candidates.push((margins.left.mm() + margins.right.mm() - gap) / 2.0);
    }
    if let Some(gap) = vertical_gap {
        candidates.push((margins.bottom.mm() + margins.top.mm() - gap) / 2.0);
    }

    average_if_consistent(candidates)
}

fn board_margin_from_margins(
    margins: &BoardArrayMargins,
    edge_rail_width: f64,
) -> Option<BoardArrayBoardMargin> {
    let left = margins.left.mm() - edge_rail_width;
    let right = margins.right.mm() - edge_rail_width;
    let bottom = margins.bottom.mm() - edge_rail_width;
    let top = margins.top.mm() - edge_rail_width;
    if [left, right, bottom, top]
        .iter()
        .any(|value| !value.is_finite() || *value < -GRID_EPSILON)
    {
        return None;
    }

    Some(BoardArrayBoardMargin {
        top: Length::from_mm(clamp_zero(top)),
        right: Length::from_mm(clamp_zero(right)),
        bottom: Length::from_mm(clamp_zero(bottom)),
        left: Length::from_mm(clamp_zero(left)),
    })
}

fn average_if_consistent(candidates: Vec<f64>) -> Option<f64> {
    if candidates.is_empty()
        || candidates
            .iter()
            .any(|candidate| !candidate.is_finite() || *candidate < -GRID_EPSILON)
    {
        return None;
    }

    let average = candidates.iter().sum::<f64>() / candidates.len() as f64;
    candidates
        .iter()
        .all(|candidate| nearly_equal(*candidate, average))
        .then_some(clamp_zero(average))
}

fn nearly_zero(value: f64) -> bool {
    value.abs() <= GRID_EPSILON
}

fn nearly_equal(a: f64, b: f64) -> bool {
    (a - b).abs() <= GRID_EPSILON
}

fn clamp_zero(value: f64) -> f64 {
    if nearly_zero(value) { 0.0 } else { value }
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
        assert_close(grid.margins.left.mm(), 5.0);
        assert_close(grid.margins.right.mm(), 5.0);
        assert_close(grid.margins.bottom.mm(), 5.5);
        assert_close(grid.margins.top.mm(), 5.5);
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
      <Step name="panel" type="PALLET">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="0" y="24"/>
            <PolyStepSegment x="44" y="24"/>
            <PolyStepSegment x="44" y="0"/>
          </Polygon>
        </Profile>
        <StepRepeat stepRef="board" x="5" y="5.5" nx="3" ny="2" dx="12" dy="8"/>
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
