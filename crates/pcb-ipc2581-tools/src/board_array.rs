use std::fmt::Write;

use ipc2581::types::LayerFunction;
use pcb_ir::common::{Affine2, Point, arc_sweep_radians};
use pcb_ir::dialects::ipc::{GeometryView, LayoutStep, LayoutStepKind, PathCmd, PathOp};

use crate::accessors::{BoardArrayGridInfo, BoardArrayInfo, IpcAccessor};

type GeometryDocument =
    pcb_ir::dialects::ipc::GeometryDocument<ipc2581::Symbol, ipc2581::types::LayerFunction>;

const OVERVIEW_STROKE_WIDTH_MM: f64 = 0.1;
const POINT_EPSILON_MM: f64 = 1e-9;

pub fn render_board_array_overview_svg(accessor: &IpcAccessor<'_>) -> Option<String> {
    let layout = accessor.board_layout_info()?;
    let board_array = layout.board_array.as_ref()?;
    let doc = crate::geometry::extract_layout(accessor.ipc()).ok()?;
    let array_height = board_array.dimensions.as_ref()?.height_mm();
    let layer_overlays = board_array_layer_overlays(accessor, array_height);
    render_board_array_svg(board_array, &doc, &layer_overlays)
}

fn render_board_array_svg(
    board_array: &BoardArrayInfo,
    doc: &GeometryDocument,
    layer_overlays: &[BoardArrayLayerOverlay],
) -> Option<String> {
    let dimensions = board_array.dimensions.as_ref()?;
    let grid = board_array.grid.as_ref()?;
    let array_width = dimensions.width_mm();
    let array_height = dimensions.height_mm();

    if array_width <= 0.0
        || array_height <= 0.0
        || grid.board_width.mm() <= 0.0
        || grid.board_height.mm() <= 0.0
        || grid.columns == 0
        || grid.rows == 0
    {
        return None;
    }

    let board_paths = board_instance_paths(doc, array_height);
    if board_paths.is_empty() {
        return None;
    }

    let mut svg = String::new();
    writeln!(
        svg,
        "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 {} {}' role='img' data-board-array-overview='true'>",
        fmt_num(array_width),
        fmt_num(array_height)
    )
    .unwrap();
    writeln!(
        svg,
        "  <title>{}</title>",
        escape_xml(&format!(
            "Board array overview: {} columns by {} rows",
            grid.columns, grid.rows
        ))
    )
    .unwrap();
    writeln!(
        svg,
        "  <rect x='0' y='0' width='{}' height='{}' fill='#ffffff'/>",
        fmt_num(array_width),
        fmt_num(array_height)
    )
    .unwrap();

    write_board_paths(&mut svg, &board_paths, "board-fill", "#f1f5f9", "none", 0.0);

    write_layer_overlays(&mut svg, layer_overlays);
    write_rail_guides(
        &mut svg,
        grid,
        array_width,
        array_height,
        OVERVIEW_STROKE_WIDTH_MM,
    );
    writeln!(
        svg,
        "  <rect class='board-array-outline' x='0' y='0' width='{}' height='{}' fill='none' stroke='#111827' stroke-width='{}'/>",
        fmt_num(array_width),
        fmt_num(array_height),
        fmt_num(OVERVIEW_STROKE_WIDTH_MM)
    )
    .unwrap();

    write_board_paths(
        &mut svg,
        &board_paths,
        "board-outline",
        "none",
        "#064e3b",
        OVERVIEW_STROKE_WIDTH_MM,
    );

    writeln!(svg, "</svg>").unwrap();
    Some(svg)
}

struct BoardArrayLayerOverlay {
    function: LayerFunction,
    paths: Vec<BoardArrayLayerPath>,
}

struct BoardArrayLayerPath {
    data: String,
    filled: bool,
    stroked: bool,
}

struct BoardArrayLayerStyle {
    class_name: &'static str,
    fill: &'static str,
    stroke: &'static str,
    fill_opacity: f64,
    stroke_opacity: f64,
}

fn board_array_layer_overlays(
    accessor: &IpcAccessor<'_>,
    array_height: f64,
) -> Vec<BoardArrayLayerOverlay> {
    let Some(ecad) = accessor.ipc().ecad() else {
        return Vec::new();
    };

    ecad.cad_data
        .layers
        .iter()
        .filter_map(|layer| {
            let layer_name = accessor.ipc().resolve(layer.name);
            let Ok(mut doc) = crate::geometry::extract_layer_for_view(
                accessor.ipc(),
                layer_name,
                GeometryView::ArrayLocal,
            ) else {
                return None;
            };
            if !crate::geometry::render::layer_has_native_content(&doc) {
                return None;
            }
            pcb_ir::dialects::ipc::process::compose_for_rendering(&mut doc);
            let paths = layer_paths(&doc, array_height);
            (!paths.is_empty()).then_some(BoardArrayLayerOverlay {
                function: layer.layer_function,
                paths,
            })
        })
        .collect()
}

fn layer_paths(doc: &GeometryDocument, panel_height: f64) -> Vec<BoardArrayLayerPath> {
    let Some(layer) = doc.layers.first() else {
        return Vec::new();
    };
    let transform = y_flip_transform(panel_height);

    doc.features[layer.feature_start as usize..(layer.feature_start + layer.feature_count) as usize]
        .iter()
        .filter(|feature| feature.source_layer_ref == Some(layer.source_layer_ref))
        .flat_map(|feature| feature_paths(doc, feature, transform))
        .collect()
}

fn board_instance_paths(doc: &GeometryDocument, panel_height: f64) -> Vec<String> {
    let flip_y = y_flip_transform(panel_height);
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

fn y_flip_transform(panel_height: f64) -> Affine2 {
    Affine2 {
        m00: 1.0,
        m01: 0.0,
        m02: 0.0,
        m10: 0.0,
        m11: -1.0,
        m12: panel_height,
    }
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

fn feature_paths(
    doc: &GeometryDocument,
    feature: &pcb_ir::dialects::ipc::GeometryFeature<ipc2581::Symbol>,
    transform: Affine2,
) -> Vec<BoardArrayLayerPath> {
    (feature.path_start..feature.path_start + feature.path_count)
        .filter_map(|path_index| {
            let path = doc.paths.get(path_index as usize)?;
            let mut data = String::new();
            append_transformed_path_data(&mut data, doc, path_index, transform)?;
            (!data.is_empty()).then_some(BoardArrayLayerPath {
                data,
                filled: path.flags.filled,
                stroked: path.flags.stroked,
            })
        })
        .collect()
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

fn write_layer_overlays(svg: &mut String, layer_overlays: &[BoardArrayLayerOverlay]) {
    for overlay in layer_overlays {
        let style = board_array_layer_style(overlay.function);
        for path in &overlay.paths {
            let force_stroke =
                matches!(overlay.function, LayerFunction::VCut | LayerFunction::Score);
            if force_stroke || (path.stroked && !path.filled) {
                writeln!(
                    svg,
                    "  <path class='array-layer {}' d='{}' fill='none' stroke='{}' stroke-width='{}' stroke-linejoin='round' opacity='{}'/>",
                    style.class_name,
                    path.data,
                    style.stroke,
                    fmt_num(OVERVIEW_STROKE_WIDTH_MM),
                    fmt_num(style.stroke_opacity)
                )
                .unwrap();
            } else if path.filled {
                writeln!(
                    svg,
                    "  <path class='array-layer {}' d='{}' fill='{}' fill-opacity='{}' stroke='none' fill-rule='evenodd'/>",
                    style.class_name,
                    path.data,
                    style.fill,
                    fmt_num(style.fill_opacity)
                )
                .unwrap();
            }
        }
    }
}

fn board_array_layer_style(function: LayerFunction) -> BoardArrayLayerStyle {
    match function {
        LayerFunction::VCut | LayerFunction::Score => BoardArrayLayerStyle {
            class_name: "vcut-guide array-layer-vscore",
            fill: "none",
            stroke: "#dc2626",
            fill_opacity: 0.0,
            stroke_opacity: 0.78,
        },
        LayerFunction::Drill => BoardArrayLayerStyle {
            class_name: "array-layer-drill",
            fill: "#2563eb",
            stroke: "#1d4ed8",
            fill_opacity: 0.42,
            stroke_opacity: 0.64,
        },
        LayerFunction::Conductor
        | LayerFunction::CondFilm
        | LayerFunction::CondFoil
        | LayerFunction::Plane
        | LayerFunction::Signal
        | LayerFunction::Mixed => BoardArrayLayerStyle {
            class_name: "array-layer-copper",
            fill: "#d87822",
            stroke: "#b45309",
            fill_opacity: 0.30,
            stroke_opacity: 0.52,
        },
        LayerFunction::Soldermask => BoardArrayLayerStyle {
            class_name: "array-layer-mask",
            fill: "#159447",
            stroke: "#15803d",
            fill_opacity: 0.18,
            stroke_opacity: 0.42,
        },
        LayerFunction::Solderpaste | LayerFunction::Pastemask => BoardArrayLayerStyle {
            class_name: "array-layer-paste",
            fill: "#64748b",
            stroke: "#475569",
            fill_opacity: 0.30,
            stroke_opacity: 0.50,
        },
        LayerFunction::Silkscreen | LayerFunction::Legend => BoardArrayLayerStyle {
            class_name: "array-layer-legend",
            fill: "#111827",
            stroke: "#111827",
            fill_opacity: 0.34,
            stroke_opacity: 0.54,
        },
        _ => BoardArrayLayerStyle {
            class_name: "array-layer-fab",
            fill: "#334155",
            stroke: "#334155",
            fill_opacity: 0.22,
            stroke_opacity: 0.56,
        },
    }
}

fn write_rail_guides(
    svg: &mut String,
    grid: &BoardArrayGridInfo,
    array_width: f64,
    array_height: f64,
    stroke_width: f64,
) {
    let Some(rail) = grid.edge_rail_width.map(|rail| rail.mm()) else {
        return;
    };
    for x in [rail, array_width - rail] {
        if x > 0.0 && x < array_width {
            writeln!(
                svg,
                "  <line class='rail-guide' x1='{}' y1='0' x2='{}' y2='{}' stroke='#cbd5e1' stroke-width='{}' opacity='0.62'/>",
                fmt_num(x),
                fmt_num(x),
                fmt_num(array_height),
                fmt_num(stroke_width)
            )
            .unwrap();
        }
    }
    for y in [rail, array_height - rail] {
        if y > 0.0 && y < array_height {
            writeln!(
                svg,
                "  <line class='rail-guide' x1='0' y1='{}' x2='{}' y2='{}' stroke='#cbd5e1' stroke-width='{}' opacity='0.62'/>",
                fmt_num(y),
                fmt_num(array_width),
                fmt_num(y),
                fmt_num(stroke_width)
            )
            .unwrap();
        }
    }
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
    fn renders_simple_board_array_overview_svg() {
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
            <PolyStepSegment x="0" y="24"/>
            <PolyStepSegment x="44" y="24"/>
            <PolyStepSegment x="44" y="0"/>
          </Polygon>
        </Profile>
        <StepRepeat stepRef="board" x="5" y="5.5" nx="3" ny="2" dx="12" dy="8"/>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();
        let accessor = IpcAccessor::new(&ipc);

        let svg = render_board_array_overview_svg(&accessor).unwrap();

        assert!(svg.contains("data-board-array-overview='true'"));
        assert!(svg.contains("viewBox='0 0 44 24'"));
        assert_eq!(svg.matches("class='board-outline'").count(), 3 * 2);
        assert!(svg.contains("fill='#f1f5f9'"));
        assert!(svg.contains("stroke='#064e3b'"));
        assert!(!svg.contains("class='vcut-guide'"));
        assert!(!svg.contains("class='score-guide'"));
        assert!(svg.contains("class='rail-guide'"));

        let board_outline_start = svg.find("class='board-outline'").unwrap();
        let rail_start = svg.find("class='rail-guide'").unwrap();
        assert!(rail_start < board_outline_start);
    }

    #[test]
    fn renders_board_array_overview_vcuts_from_vcut_layer_only() {
        let ipc = ipc2581::Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="panel"/>
    <LayerRef name="VCUT"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="VCUT" layerFunction="V_CUT" side="NONE" polarity="POSITIVE"/>
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
        <LayerFeature layerRef="VCUT">
          <Set>
            <Features>
              <Line startX="5" startY="0" endX="5" endY="24">
                <LineDesc lineWidth="0.1" lineEnd="ROUND"/>
              </Line>
              <Line startX="0" startY="5.5" endX="44" endY="5.5">
                <LineDesc lineWidth="0.1" lineEnd="ROUND"/>
              </Line>
            </Features>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();
        let accessor = IpcAccessor::new(&ipc);

        let svg = render_board_array_overview_svg(&accessor).unwrap();

        assert_eq!(svg.matches("vcut-guide").count(), 2);
        assert!(svg.contains("d='M5 24 L5 0'"));
        assert!(svg.contains("d='M0 18.5 L44 18.5'"));
        assert!(svg.contains("stroke='#dc2626'"));
        assert!(svg.contains("stroke-width='0.1'"));
        assert!(!svg.contains("stroke-dasharray"));
        assert!(!svg.contains("class='score-guide'"));

        let vcut_start = svg.find("vcut-guide").unwrap();
        let board_outline_start = svg.find("class='board-outline'").unwrap();
        assert!(vcut_start < board_outline_start);
    }
}
