use std::fmt::Write;

use anyhow::{Context, Result};
use ipc2581::Ipc2581;
use ipc2581::types::LayerFunction;
use pcb_ir::dialects::ipc::{LayoutStep, LayoutStepKind, View};
use pcb_ir::geom::path::{PathCmd, PathOp};
use pcb_ir::geom::{Affine2, Arc, BBox, ContourBuf, Point};

use crate::accessors::{BoardArrayGridInfo, BoardArrayInfo, IpcAccessor};
use crate::utils::format::fmt_num;

type GeometryDocument =
    pcb_ir::dialects::ipc::Document<ipc2581::Symbol, ipc2581::types::LayerFunction>;

const OVERVIEW_STROKE_WIDTH_MM: f64 = 0.1;
const OVERVIEW_VIEWBOX_PADDING_MM: f64 = 1.0;
const POINT_EPSILON_MM: f64 = 1e-9;

pub fn render_board_array_overview_svg(accessor: &IpcAccessor<'_>) -> Result<Option<String>> {
    let Some(layout) = accessor.board_layout_info() else {
        return Ok(None);
    };
    let Some(board_array) = layout.board_array.as_ref() else {
        return Ok(None);
    };
    let doc = crate::geometry::extract_layout(accessor.ipc())
        .context("failed to extract board-array geometry for overview")?;
    let Some(dimensions) = board_array.dimensions.as_ref() else {
        return Ok(None);
    };
    let array_height = dimensions.height_mm();
    let layer_overlays = board_array_layer_overlays(accessor, array_height);
    render_board_array_svg(accessor.ipc(), board_array, &doc, &layer_overlays)
}

fn render_board_array_svg(
    ipc: &Ipc2581,
    board_array: &BoardArrayInfo,
    doc: &GeometryDocument,
    layer_overlays: &[BoardArrayLayerOverlay],
) -> Result<Option<String>> {
    let Some(dimensions) = board_array.dimensions.as_ref() else {
        return Ok(None);
    };
    let Some(grid) = board_array.grid.as_ref() else {
        return Ok(None);
    };
    let array_width = dimensions.width_mm();
    let array_height = dimensions.height_mm();

    if array_width <= 0.0
        || array_height <= 0.0
        || grid.board_width.mm() <= 0.0
        || grid.board_height.mm() <= 0.0
        || grid.columns == 0
        || grid.rows == 0
    {
        return Ok(None);
    }

    let board_fill_paths = board_instance_paths(doc, array_height, true);
    let board_outline_paths = board_instance_paths(doc, array_height, false);
    if board_outline_paths.is_empty() {
        return Ok(None);
    }
    let profile_paths = board_array_profile_paths(ipc, doc, array_height)?;
    let viewbox = overview_viewbox(array_width, array_height, layer_overlays);
    let viewbox_width = viewbox.width();
    let viewbox_height = viewbox.height();

    let mut svg = String::new();
    writeln!(
        svg,
        "<svg xmlns='http://www.w3.org/2000/svg' viewBox='{} {} {} {}' role='img' data-board-array-overview='true'>",
        fmt_num(viewbox.min.x),
        fmt_num(viewbox.min.y),
        fmt_num(viewbox_width),
        fmt_num(viewbox_height)
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
        "  <rect x='{}' y='{}' width='{}' height='{}' fill='#ffffff'/>",
        fmt_num(viewbox.min.x),
        fmt_num(viewbox.min.y),
        fmt_num(viewbox_width),
        fmt_num(viewbox_height)
    )
    .unwrap();

    write_board_paths(
        &mut svg,
        &board_fill_paths,
        "board-fill",
        "#f1f5f9",
        "none",
        0.0,
    );

    write_rail_guides(
        &mut svg,
        grid,
        array_width,
        array_height,
        OVERVIEW_STROKE_WIDTH_MM,
    );
    for outline_path in &profile_paths.array_outlines {
        writeln!(
            svg,
            "  <path class='board-array-outline' d='{outline_path}' fill='none' stroke='#111827' stroke-width='{}'/>",
            fmt_num(OVERVIEW_STROKE_WIDTH_MM)
        )
        .unwrap();
    }

    write_board_paths(
        &mut svg,
        &board_outline_paths,
        "board-outline",
        "none",
        "#064e3b",
        OVERVIEW_STROKE_WIDTH_MM,
    );
    write_profile_cutout_paths(&mut svg, &profile_paths.material_removal);
    write_layer_overlays(&mut svg, layer_overlays);

    writeln!(svg, "</svg>").unwrap();
    Ok(Some(svg))
}

struct BoardArrayLayerOverlay {
    function: LayerFunction,
    paths: Vec<BoardArrayLayerPath>,
}

struct BoardArrayLayerPath {
    data: String,
    bbox: BBox,
    stroke_width: f64,
    filled: bool,
    stroked: bool,
    vscore: bool,
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
                View::ArraySupport,
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

struct BoardArrayProfileSvgPaths {
    array_outlines: Vec<String>,
    material_removal: Vec<String>,
}

fn board_array_profile_paths(
    ipc: &Ipc2581,
    doc: &GeometryDocument,
    array_height: f64,
) -> Result<BoardArrayProfileSvgPaths> {
    let score_lines = crate::geometry::board_array_vscore_lines(ipc)?;
    let profile = crate::geometry::board_array_fabrication_profile(ipc, doc, &score_lines)?;
    let transform = y_flip_transform(array_height);

    Ok(BoardArrayProfileSvgPaths {
        array_outlines: payload_groups_path_data(&profile.array_outlines, transform),
        material_removal: payloads_path_data(&profile.material_removal, transform)
            .into_iter()
            .collect(),
    })
}

fn payload_groups_path_data(payload_groups: &[Vec<ContourBuf>], transform: Affine2) -> Vec<String> {
    payload_groups
        .iter()
        .filter_map(|payloads| payloads_path_data(payloads, transform))
        .collect()
}

fn layer_paths(doc: &GeometryDocument, panel_height: f64) -> Vec<BoardArrayLayerPath> {
    let Some(layer) = doc.layers.first() else {
        return Vec::new();
    };
    let transform = y_flip_transform(panel_height);

    layer
        .features
        .slice(&doc.features)
        .iter()
        .filter(|feature| feature.source_layer_ref == Some(layer.source_layer_ref))
        .flat_map(|feature| feature_paths(doc, feature, transform))
        .collect()
}

fn board_instance_paths(
    doc: &GeometryDocument,
    panel_height: f64,
    include_cutouts: bool,
) -> Vec<String> {
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
        if let Some(path) = step_profile_path_data(doc, step, transform, include_cutouts) {
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
    include_cutouts: bool,
) -> Option<String> {
    let mut path_data = String::new();
    for profile_index in step.profiles.indices() {
        let profile = doc.profiles.get(profile_index as usize)?;
        append_transformed_path_data(&mut path_data, doc, profile.outer_path, transform)?;
        if !include_cutouts {
            continue;
        }
        for cutout in profile.cutouts.slice(&doc.profile_cutouts) {
            append_transformed_path_data(&mut path_data, doc, cutout.path, transform)?;
        }
    }

    (!path_data.is_empty()).then_some(path_data)
}

fn overview_viewbox(
    array_width: f64,
    array_height: f64,
    layer_overlays: &[BoardArrayLayerOverlay],
) -> BBox {
    let mut bbox = BBox {
        min: Point::new(0.0, 0.0),
        max: Point::new(array_width, array_height),
    };
    for path in layer_overlays
        .iter()
        .flat_map(|overlay| overlay.paths.iter())
        .filter(|path| !path.bbox.is_empty())
    {
        bbox = bbox.union(path.bbox);
    }
    bbox.expand(OVERVIEW_VIEWBOX_PADDING_MM)
}

fn feature_paths(
    doc: &GeometryDocument,
    feature: &pcb_ir::dialects::ipc::Feature<ipc2581::Symbol>,
    transform: Affine2,
) -> Vec<BoardArrayLayerPath> {
    feature
        .paths
        .indices()
        .filter_map(|path_index| {
            let path = doc.arena.paths.get(path_index as usize)?;
            let mut data = String::new();
            append_transformed_path_data(&mut data, doc, path_index, transform)?;
            (!data.is_empty()).then_some(BoardArrayLayerPath {
                data,
                bbox: transform_bbox(path.bbox, transform),
                stroke_width: path.stroke().map(|stroke| stroke.width).unwrap_or(0.0),
                filled: path.is_filled(),
                stroked: path.is_stroked(),
                vscore: feature.is_vscore(),
            })
        })
        .collect()
}

fn transform_bbox(bbox: BBox, transform: Affine2) -> BBox {
    if bbox.is_empty() {
        return BBox::empty();
    }

    [
        bbox.min,
        Point::new(bbox.max.x, bbox.min.y),
        bbox.max,
        Point::new(bbox.min.x, bbox.max.y),
    ]
    .into_iter()
    .fold(BBox::empty(), |mut transformed, point| {
        transformed.include_point(transform.transform_point(point));
        transformed
    })
}

fn append_transformed_path_data(
    path_data: &mut String,
    doc: &GeometryDocument,
    path_index: u32,
    transform: Affine2,
) -> Option<()> {
    let path = doc.arena.paths.get(path_index as usize)?;
    for contour in doc.arena.contours(path.contours) {
        let transformed =
            pcb_ir::geom::path::transform_cmds(doc.arena.cmds(*contour).iter().copied(), transform);
        append_path_cmds(path_data, &transformed.cmds);
    }
    Some(())
}

fn payloads_path_data(payloads: &[ContourBuf], transform: Affine2) -> Option<String> {
    let mut path_data = String::new();
    for payload in payloads {
        let transformed =
            pcb_ir::geom::path::transform_cmds(payload.cmds.iter().copied(), transform);
        append_path_cmds(&mut path_data, &transformed.cmds);
    }
    (!path_data.is_empty()).then_some(path_data)
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
        u8::from(Arc::new(start, end, center, clockwise).sweep_radians() > std::f64::consts::PI);
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
        for path in &overlay.paths {
            let style = board_array_layer_style(overlay.function, path.vscore);
            let force_stroke = path.vscore;
            if force_stroke || (path.stroked && !path.filled) {
                writeln!(
                    svg,
                    "  <path class='array-layer {}' d='{}' fill='none' stroke='{}' stroke-width='{}' stroke-linecap='round' stroke-linejoin='round' opacity='{}'/>",
                    style.class_name,
                    path.data,
                    style.stroke,
                    fmt_num(path.stroke_width.max(OVERVIEW_STROKE_WIDTH_MM)),
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

fn write_profile_cutout_paths(svg: &mut String, paths: &[String]) {
    for path in paths {
        writeln!(
            svg,
            "  <path class='board-array-profile-cutout' d='{path}' fill='#ffffff' stroke='#111827' stroke-width='{}' stroke-linejoin='round' fill-rule='nonzero' opacity='0.95'/>",
            fmt_num(OVERVIEW_STROKE_WIDTH_MM)
        )
        .unwrap();
    }
}

fn board_array_layer_style(function: LayerFunction, vscore: bool) -> BoardArrayLayerStyle {
    if vscore {
        return BoardArrayLayerStyle {
            class_name: "vcut-guide array-layer-vscore",
            fill: "none",
            stroke: "#dc2626",
            fill_opacity: 0.0,
            stroke_opacity: 1.0,
        };
    }

    match function {
        LayerFunction::Drill => BoardArrayLayerStyle {
            class_name: "array-layer-drill",
            fill: "#2563eb",
            stroke: "#1d4ed8",
            fill_opacity: 0.85,
            stroke_opacity: 0.85,
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
            fill_opacity: 0.90,
            stroke_opacity: 0.85,
        },
        LayerFunction::Soldermask => BoardArrayLayerStyle {
            class_name: "array-layer-mask",
            fill: "#159447",
            stroke: "#15803d",
            fill_opacity: 0.55,
            stroke_opacity: 0.70,
        },
        LayerFunction::Solderpaste | LayerFunction::Pastemask => BoardArrayLayerStyle {
            class_name: "array-layer-paste",
            fill: "#64748b",
            stroke: "#475569",
            fill_opacity: 0.90,
            stroke_opacity: 0.85,
        },
        LayerFunction::Silkscreen | LayerFunction::Legend => BoardArrayLayerStyle {
            class_name: "array-layer-legend",
            fill: "#111827",
            stroke: "#111827",
            fill_opacity: 0.95,
            stroke_opacity: 0.90,
        },
        _ => BoardArrayLayerStyle {
            class_name: "array-layer-fab",
            fill: "#334155",
            stroke: "#334155",
            fill_opacity: 0.85,
            stroke_opacity: 0.85,
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
    for x in [
        grid.edge_rail.left.mm(),
        array_width - grid.edge_rail.right.mm(),
    ] {
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
    for y in [
        grid.edge_rail.bottom.mm(),
        array_height - grid.edge_rail.top.mm(),
    ] {
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
    <CadHeader units="MILLIMETER">
      <Spec name="VCut_1">
        <V_Cut type="OFFSET">
          <Property value="0" unit="MM"/>
        </V_Cut>
      </Spec>
    </CadHeader>
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

        let svg = render_board_array_overview_svg(&accessor).unwrap().unwrap();

        assert!(svg.contains("data-board-array-overview='true'"));
        assert!(svg.contains("viewBox='-1 -1 46 26'"));
        assert_eq!(svg.matches("class='board-outline'").count(), 3 * 2);
        assert!(svg.contains("fill='#f1f5f9'"));
        assert!(svg.contains("stroke='#064e3b'"));
        assert!(svg.contains("class='board-array-outline'"));
        assert!(!svg.contains("class='board-array-outline' x="));
        assert!(!svg.contains("class='vcut-guide'"));
        assert!(!svg.contains("class='score-guide'"));
        assert!(svg.contains("class='rail-guide'"));

        let board_outline_start = svg.find("class='board-outline'").unwrap();
        let rail_start = svg.find("class='rail-guide'").unwrap();
        assert!(rail_start < board_outline_start);
    }

    #[test]
    fn renders_board_array_overview_from_array_profile() {
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
            <PolyBegin x="0" y="3"/>
            <PolyStepSegment x="0" y="21"/>
            <PolyStepCurve x="3" y="24" centerX="3" centerY="21" clockwise="true"/>
            <PolyStepSegment x="41" y="24"/>
            <PolyStepCurve x="44" y="21" centerX="41" centerY="21" clockwise="true"/>
            <PolyStepSegment x="44" y="3"/>
            <PolyStepCurve x="41" y="0" centerX="41" centerY="3" clockwise="true"/>
            <PolyStepSegment x="3" y="0"/>
            <PolyStepCurve x="0" y="3" centerX="3" centerY="3" clockwise="true"/>
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

        let svg = render_board_array_overview_svg(&accessor).unwrap().unwrap();

        assert!(svg.contains("class='board-array-outline'"));
        assert!(svg.contains(" A3 3"));
        assert!(!svg.contains("class='board-array-outline' x="));
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
            <SpecRef id="VCut_1"/>
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

        let svg = render_board_array_overview_svg(&accessor).unwrap().unwrap();

        assert_eq!(svg.matches("vcut-guide").count(), 2);
        assert!(svg.contains("d='M5 24 L5 0'"));
        assert!(svg.contains("d='M0 18.5 L44 18.5'"));
        assert!(svg.contains("stroke='#dc2626'"));
        assert!(svg.contains("stroke-width='0.1'"));
        assert!(!svg.contains("stroke-dasharray"));
        assert!(!svg.contains("class='score-guide'"));

        let vcut_start = svg.find("vcut-guide").unwrap();
        let board_outline_start = svg.find("class='board-outline'").unwrap();
        assert!(board_outline_start < vcut_start);
    }

    #[test]
    fn renders_board_array_overview_vcut_relief_contours() {
        let ipc = ipc2581::Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="panel"/>
    <LayerRef name="VCUT"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER">
      <Spec name="VCut_1">
        <V_Cut type="OFFSET">
          <Property value="0" unit="MM"/>
        </V_Cut>
      </Spec>
    </CadHeader>
    <CadData>
      <Layer name="VCUT" layerFunction="V_CUT" side="NONE" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="10" y="0"/>
            <PolyStepSegment x="10" y="10"/>
            <PolyStepSegment x="6" y="10"/>
            <PolyStepSegment x="5" y="8"/>
            <PolyStepSegment x="4" y="10"/>
            <PolyStepSegment x="0" y="10"/>
          </Polygon>
          <Cutout>
            <PolyBegin x="0" y="2"/>
            <PolyStepSegment x="2" y="2"/>
            <PolyStepSegment x="2" y="4"/>
            <PolyStepSegment x="0" y="4"/>
          </Cutout>
        </Profile>
      </Step>
      <Step name="panel" type="PALLET">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="0" y="20"/>
            <PolyStepSegment x="20" y="20"/>
            <PolyStepSegment x="20" y="0"/>
          </Polygon>
        </Profile>
        <StepRepeat stepRef="board" x="5" y="5" nx="1" ny="1" dx="0" dy="0"/>
        <LayerFeature layerRef="VCUT">
          <Set>
            <SpecRef id="VCut_1"/>
            <Features>
              <Line startX="5" startY="0" endX="5" endY="20">
                <LineDesc lineWidth="0.1" lineEnd="ROUND"/>
              </Line>
              <Line startX="15" startY="0" endX="15" endY="20">
                <LineDesc lineWidth="0.1" lineEnd="ROUND"/>
              </Line>
              <Line startX="0" startY="5" endX="20" endY="5">
                <LineDesc lineWidth="0.1" lineEnd="ROUND"/>
              </Line>
              <Line startX="0" startY="15" endX="20" endY="15">
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

        let svg = render_board_array_overview_svg(&accessor).unwrap().unwrap();

        assert!(svg.contains("class='board-array-profile-cutout'"));
        assert!(svg.contains("fill='#ffffff'"));
        assert!(svg.contains("stroke='#111827'"));
        assert!(svg.contains(" Z"));
        let board_outline = svg
            .lines()
            .find(|line| line.contains("class='board-outline'"))
            .unwrap();
        assert_eq!(board_outline.matches(" M").count(), 0);
    }

    #[test]
    fn renders_nested_board_cell_support_geometry_without_board_features() {
        let ipc = ipc2581::Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="array"/>
    <LayerRef name="TOP"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="10" y="0"/>
            <PolyStepSegment x="10" y="5"/>
            <PolyStepSegment x="0" y="5"/>
          </Polygon>
        </Profile>
        <LayerFeature layerRef="TOP">
          <Set>
            <Features>
              <Line startX="1" startY="2.5" endX="9" endY="2.5">
                <LineDesc lineWidth="0.2" lineEnd="ROUND"/>
              </Line>
            </Features>
          </Set>
        </LayerFeature>
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
        <LayerFeature layerRef="TOP">
          <Set>
            <LocalFiducial>
              <Location x="1" y="1"/>
              <Circle diameter="1"/>
            </LocalFiducial>
          </Set>
        </LayerFeature>
        <StepRepeat stepRef="board" x="2" y="2" nx="1" ny="1" dx="0" dy="0"/>
      </Step>
      <Step name="array" type="PALLET">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="20" y="0"/>
            <PolyStepSegment x="20" y="15"/>
            <PolyStepSegment x="0" y="15"/>
          </Polygon>
        </Profile>
        <StepRepeat stepRef="board_cell" x="4" y="5" nx="1" ny="1" dx="12" dy="8"/>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();
        let accessor = IpcAccessor::new(&ipc);

        let svg = render_board_array_overview_svg(&accessor).unwrap().unwrap();

        assert_eq!(svg.matches("array-layer-copper").count(), 1);
        assert!(!svg.contains("M7 5.5 L15 5.5"));
    }
}
