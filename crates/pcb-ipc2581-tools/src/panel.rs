use std::fmt::Write;

use pcb_ir::common::{Affine2, Point, arc_sweep_radians};
use pcb_ir::dialects::ipc::{LayoutStep, LayoutStepKind, PathCmd, PathOp};

use crate::accessors::{IpcAccessor, PanelGridInfo, PanelInfo};

type GeometryDocument =
    pcb_ir::dialects::ipc::GeometryDocument<ipc2581::Symbol, ipc2581::types::LayerFunction>;

const MIN_STROKE: f64 = 0.08;
const POINT_EPSILON_MM: f64 = 1e-9;

pub fn render_overview_svg(accessor: &IpcAccessor<'_>) -> Option<String> {
    let layout = accessor.board_layout_info()?;
    let panel = layout.panel.as_ref()?;
    let doc = crate::geometry::extract_layout(accessor.ipc()).ok()?;
    render_panel_svg(panel, &doc)
}

fn render_panel_svg(panel: &PanelInfo, doc: &GeometryDocument) -> Option<String> {
    let dimensions = panel.dimensions.as_ref()?;
    let grid = panel.grid.as_ref()?;
    let panel_width = dimensions.width_mm();
    let panel_height = dimensions.height_mm();

    if panel_width <= 0.0
        || panel_height <= 0.0
        || grid.board_width.mm() <= 0.0
        || grid.board_height.mm() <= 0.0
        || grid.columns == 0
        || grid.rows == 0
    {
        return None;
    }

    let scale = (panel_width.max(panel_height) / 450.0).max(MIN_STROKE);
    let panel_stroke = scale * 1.9;
    let board_stroke = scale * 0.75;
    let score_stroke = scale * 0.85;
    let rail_stroke = scale * 0.5;
    let score_dash = format!(
        "{} {}",
        fmt_num(score_stroke * 7.6),
        fmt_num(score_stroke * 5.0)
    );
    let board_paths = board_instance_paths(doc, panel_height);
    if board_paths.is_empty() {
        return None;
    }

    let mut svg = String::new();
    writeln!(
        svg,
        "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 {} {}' role='img' data-panel-overview='true'>",
        fmt_num(panel_width),
        fmt_num(panel_height)
    )
    .unwrap();
    writeln!(
        svg,
        "  <title>{}</title>",
        escape_xml(&format!(
            "Panel overview: {} columns by {} rows",
            grid.columns, grid.rows
        ))
    )
    .unwrap();
    writeln!(
        svg,
        "  <rect x='0' y='0' width='{}' height='{}' fill='#ffffff'/>",
        fmt_num(panel_width),
        fmt_num(panel_height)
    )
    .unwrap();

    write_board_paths(&mut svg, &board_paths, "board-fill", "#f1f5f9", "none", 0.0);

    write_rail_guides(&mut svg, grid, panel_width, panel_height, rail_stroke);
    write_score_guides(
        &mut svg,
        grid,
        panel_width,
        panel_height,
        score_stroke,
        &score_dash,
    );
    writeln!(
        svg,
        "  <rect class='panel-outline' x='0' y='0' width='{}' height='{}' fill='none' stroke='#111827' stroke-width='{}'/>",
        fmt_num(panel_width),
        fmt_num(panel_height),
        fmt_num(panel_stroke)
    )
    .unwrap();

    write_board_paths(
        &mut svg,
        &board_paths,
        "board-outline",
        "none",
        "#064e3b",
        board_stroke,
    );

    writeln!(svg, "</svg>").unwrap();
    Some(svg)
}

fn board_instance_paths(doc: &GeometryDocument, panel_height: f64) -> Vec<String> {
    let flip_y = Affine2 {
        m00: 1.0,
        m01: 0.0,
        m02: 0.0,
        m10: 0.0,
        m11: -1.0,
        m12: panel_height,
    };
    let mut paths = Vec::new();

    for instance in &doc.layout.instances {
        let Some(step) = doc.layout.steps.get(instance.child_step as usize) else {
            continue;
        };
        if step.kind != LayoutStepKind::Board {
            continue;
        }

        let transform = flip_y.concat(instance.transform);
        if let Some(path) = step_profile_path_data(doc, step, transform) {
            paths.push(path);
        }
    }

    paths
}

fn step_profile_path_data(
    doc: &GeometryDocument,
    step: &LayoutStep<ipc2581::Symbol>,
    transform: Affine2,
) -> Option<String> {
    let mut path_data = String::new();
    for profile_index in step.profile_start..step.profile_start + step.profile_count {
        let profile = doc.profiles.get(profile_index as usize)?;
        append_transformed_path_data(&mut path_data, doc, profile.outer_path, transform)?;
        for cutout in &doc.profile_cutouts
            [profile.cutout_start as usize..(profile.cutout_start + profile.cutout_count) as usize]
        {
            append_transformed_path_data(&mut path_data, doc, cutout.path, transform)?;
        }
    }

    (!path_data.is_empty()).then_some(path_data)
}

fn append_transformed_path_data(
    path_data: &mut String,
    doc: &GeometryDocument,
    path_index: u32,
    transform: Affine2,
) -> Option<()> {
    let path = doc.paths.get(path_index as usize)?;
    for contour in &doc.contours
        [path.contour_start as usize..(path.contour_start + path.contour_count) as usize]
    {
        let cmds = &doc.path_cmds
            [contour.cmd_start as usize..(contour.cmd_start + contour.cmd_count) as usize];
        let (_, cmds) = pcb_ir::dialects::path::transform_cmds(cmds.iter().copied(), transform);
        append_path_cmds(path_data, &cmds);
    }
    Some(())
}

fn append_path_cmds(data: &mut String, cmds: &[PathCmd]) {
    let mut current = Point::default();
    for cmd in cmds {
        match cmd.op {
            PathOp::MoveTo => {
                current = cmd.p0;
                if !data.is_empty() {
                    data.push(' ');
                }
                write!(data, "M{} {}", fmt_num(cmd.p0.x), fmt_num(cmd.p0.y)).unwrap();
            }
            PathOp::LineTo => {
                current = cmd.p0;
                write!(data, " L{} {}", fmt_num(cmd.p0.x), fmt_num(cmd.p0.y)).unwrap();
            }
            PathOp::ArcTo => {
                write_arc_to_path_data(data, current, cmd.p0, cmd.p1, cmd.clockwise);
                current = cmd.p0;
            }
            PathOp::CubicTo => {
                current = cmd.p2;
                write!(
                    data,
                    " C{} {},{} {},{} {}",
                    fmt_num(cmd.p0.x),
                    fmt_num(cmd.p0.y),
                    fmt_num(cmd.p1.x),
                    fmt_num(cmd.p1.y),
                    fmt_num(cmd.p2.x),
                    fmt_num(cmd.p2.y)
                )
                .unwrap();
            }
            PathOp::Close => data.push_str(" Z"),
        }
    }
}

fn write_arc_to_path_data(
    data: &mut String,
    start: Point,
    end: Point,
    center: Point,
    clockwise: bool,
) {
    let radius = start.distance_to(center);
    if radius <= POINT_EPSILON_MM {
        write!(data, " L{} {}", fmt_num(end.x), fmt_num(end.y)).unwrap();
        return;
    }

    let sweep_flag = if clockwise { 0 } else { 1 };
    if start.distance_to(end) <= POINT_EPSILON_MM {
        let midpoint = Point::new(2.0 * center.x - start.x, 2.0 * center.y - start.y);
        write_svg_arc(data, radius, 0, sweep_flag, midpoint);
        write_svg_arc(data, radius, 0, sweep_flag, end);
        return;
    }

    let large_arc =
        u8::from(arc_sweep_radians(start, end, center, clockwise) > std::f64::consts::PI);
    write_svg_arc(data, radius, large_arc, sweep_flag, end);
}

fn write_svg_arc(data: &mut String, radius: f64, large_arc: u8, sweep_flag: u8, end: Point) {
    write!(
        data,
        " A{} {} 0 {large_arc} {sweep_flag} {} {}",
        fmt_num(radius),
        fmt_num(radius),
        fmt_num(end.x),
        fmt_num(end.y)
    )
    .unwrap();
}

fn write_board_paths(
    svg: &mut String,
    paths: &[String],
    class_name: &str,
    fill: &str,
    stroke: &str,
    stroke_width: f64,
) {
    for path in paths {
        writeln!(
            svg,
            "  <path class='{class_name}' d='{path}' fill='{fill}' stroke='{stroke}' stroke-width='{}' fill-rule='evenodd'/>",
            fmt_num(stroke_width)
        )
        .unwrap();
    }
}

fn write_rail_guides(
    svg: &mut String,
    grid: &PanelGridInfo,
    panel_width: f64,
    panel_height: f64,
    stroke_width: f64,
) {
    let Some(rail) = grid.edge_rail_width.map(|rail| rail.mm()) else {
        return;
    };
    for x in [rail, panel_width - rail] {
        if x > 0.0 && x < panel_width {
            writeln!(
                svg,
                "  <line class='rail-guide' x1='{}' y1='0' x2='{}' y2='{}' stroke='#cbd5e1' stroke-width='{}' opacity='0.62'/>",
                fmt_num(x),
                fmt_num(x),
                fmt_num(panel_height),
                fmt_num(stroke_width)
            )
            .unwrap();
        }
    }
    for y in [rail, panel_height - rail] {
        if y > 0.0 && y < panel_height {
            writeln!(
                svg,
                "  <line class='rail-guide' x1='0' y1='{}' x2='{}' y2='{}' stroke='#cbd5e1' stroke-width='{}' opacity='0.62'/>",
                fmt_num(y),
                fmt_num(panel_width),
                fmt_num(y),
                fmt_num(stroke_width)
            )
            .unwrap();
        }
    }
}

fn write_score_guides(
    svg: &mut String,
    grid: &PanelGridInfo,
    panel_width: f64,
    panel_height: f64,
    stroke_width: f64,
    dash: &str,
) {
    for x in score_x_positions(grid) {
        writeln!(
            svg,
            "  <line class='score-guide' x1='{}' y1='0' x2='{}' y2='{}' stroke='#b91c1c' stroke-width='{}' stroke-dasharray='{dash}' opacity='0.78'/>",
            fmt_num(x),
            fmt_num(x),
            fmt_num(panel_height),
            fmt_num(stroke_width)
        )
        .unwrap();
    }
    for y in score_y_positions(grid, panel_height) {
        writeln!(
            svg,
            "  <line class='score-guide' x1='0' y1='{}' x2='{}' y2='{}' stroke='#b91c1c' stroke-width='{}' stroke-dasharray='{dash}' opacity='0.78'/>",
            fmt_num(y),
            fmt_num(panel_width),
            fmt_num(y),
            fmt_num(stroke_width)
        )
        .unwrap();
    }
}

fn score_x_positions(grid: &PanelGridInfo) -> Vec<f64> {
    let pitch_x = grid
        .pitch_x
        .map(|pitch| pitch.mm())
        .unwrap_or_else(|| grid.board_width.mm());
    let mut positions = Vec::new();
    for column in 0..grid.columns {
        let x = grid.margins.left.mm() + column as f64 * pitch_x;
        positions.push(x);
        positions.push(x + grid.board_width.mm());
    }
    sorted_positions(positions)
}

fn score_y_positions(grid: &PanelGridInfo, panel_height: f64) -> Vec<f64> {
    let pitch_y = grid
        .pitch_y
        .map(|pitch| pitch.mm())
        .unwrap_or_else(|| grid.board_height.mm());
    let mut positions = Vec::new();
    for row in 0..grid.rows {
        let board_bottom = grid.margins.bottom.mm() + row as f64 * pitch_y;
        let y = panel_height - board_bottom - grid.board_height.mm();
        positions.push(y);
        positions.push(y + grid.board_height.mm());
    }
    sorted_positions(positions)
}

fn sorted_positions(mut positions: Vec<f64>) -> Vec<f64> {
    positions.retain(|position| position.is_finite());
    positions.sort_by(f64::total_cmp);
    positions.dedup_by(|a, b| (*a - *b).abs() <= 1e-6);
    positions
}

fn escape_xml(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn fmt_num(value: f64) -> String {
    let mut text = format!("{value:.6}");
    while text.contains('.') && text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    if text == "-0" { "0".to_string() } else { text }
}

#[cfg(test)]
mod tests {
    use crate::accessors::IpcAccessor;

    use super::*;

    #[test]
    fn renders_simple_panel_overview_svg() {
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
            <PolyStepSegment x="0" y="27"/>
            <PolyStepSegment x="46" y="27"/>
            <PolyStepSegment x="46" y="0"/>
          </Polygon>
        </Profile>
        <StepRepeat stepRef="board" x="6" y="7" nx="3" ny="2" dx="12" dy="8"/>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();
        let accessor = IpcAccessor::new(&ipc);

        let svg = render_overview_svg(&accessor).unwrap();

        assert!(svg.contains("data-panel-overview='true'"));
        assert!(svg.contains("viewBox='0 0 46 27'"));
        assert_eq!(svg.matches("class='board-outline'").count(), 3 * 2);
        assert!(svg.contains("fill='#f1f5f9'"));
        assert!(svg.contains("stroke='#064e3b'"));
        assert!(svg.contains("class='score-guide'"));
        assert!(svg.contains("class='rail-guide'"));

        let score_start = svg.find("class='score-guide'").unwrap();
        let board_outline_start = svg.find("class='board-outline'").unwrap();
        assert!(score_start < board_outline_start);
    }
}
