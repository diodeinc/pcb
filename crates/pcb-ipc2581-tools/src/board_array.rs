use pcb_ir::geom::Resolution;
use std::fmt::Write;

use anyhow::Result;
use ipc2581::types::LayerFunction;
use pcb_ir::dialects::LayerRole;
use pcb_ir::dialects::ipc::{ArtworkScope, Feature, LayoutStep, LayoutStepKind};
use pcb_ir::geom::{Affine2, BBox, ContourBuf};
use pcb_ir::import::ipc2581::{ImportedDesign, LayerId};
use pcb_ir::render::svg_path_data;

use crate::accessors::{BoardArrayGridInfo, IpcAccessor};
use crate::utils::format::fmt_num;

type GeometryDocument =
    pcb_ir::dialects::ipc::Document<ipc2581::Symbol, ipc2581::types::LayerFunction>;

const OVERVIEW_STROKE_WIDTH_MM: f64 = 0.1;
const OVERVIEW_VIEWBOX_PADDING_MM: f64 = 1.0;

pub fn render_board_array_overview_svg(
    accessor: &IpcAccessor<'_>,
    imported: &ImportedDesign,
    resolution: Resolution,
) -> Result<Option<String>> {
    let Some(grid) = accessor
        .board_layout_info()
        .and_then(|layout| layout.board_array?.grid)
    else {
        return Ok(None);
    };
    let Some(doc) = accessor.layout() else {
        return Ok(None);
    };
    let Some(panel) = pcb_ir::dialects::ipc::panel_bbox(doc) else {
        return Ok(None);
    };
    let layer_overlays = board_array_layer_overlays(imported, resolution)?;
    render_board_array_svg(imported, &grid, panel, doc, &layer_overlays, resolution)
}

/// Draw the array in world millimetres wherever the panel sits; the screen
/// flip is one group transform over the panel's own bounds.
fn render_board_array_svg(
    imported: &ImportedDesign,
    grid: &BoardArrayGridInfo,
    panel: BBox,
    doc: &GeometryDocument,
    layer_overlays: &[BoardArrayLayerOverlay],
    resolution: Resolution,
) -> Result<Option<String>> {
    if panel.width() <= 0.0
        || panel.height() <= 0.0
        || grid.board_width.mm() <= 0.0
        || grid.board_height.mm() <= 0.0
        || grid.columns == 0
        || grid.rows == 0
    {
        return Ok(None);
    }

    let board_fill_paths = board_instance_paths(doc, true);
    let board_outline_paths = board_instance_paths(doc, false);
    if board_outline_paths.is_empty() {
        return Ok(None);
    }
    let profile_paths = board_array_profile_paths(imported, doc, resolution)?;
    let viewbox = layer_overlays
        .iter()
        .flat_map(|overlay| &overlay.paths)
        .fold(panel, |bbox, path| bbox.union(path.bbox))
        .expand(OVERVIEW_VIEWBOX_PADDING_MM);

    let mut svg = String::new();
    writeln!(
        svg,
        "<svg xmlns='http://www.w3.org/2000/svg' viewBox='{} {} {} {}' role='img' data-board-array-overview='true'>",
        fmt_num(viewbox.min.x),
        fmt_num(-viewbox.max.y),
        fmt_num(viewbox.width()),
        fmt_num(viewbox.height())
    )
    .unwrap();
    writeln!(
        svg,
        "  <title>Board array overview: {} columns by {} rows</title>",
        grid.columns, grid.rows
    )
    .unwrap();
    writeln!(svg, "  <g transform='scale(1 -1)'>").unwrap();
    writeln!(
        svg,
        "  <rect x='{}' y='{}' width='{}' height='{}' fill='#ffffff'/>",
        fmt_num(viewbox.min.x),
        fmt_num(viewbox.min.y),
        fmt_num(viewbox.width()),
        fmt_num(viewbox.height())
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

    write_rail_guides(&mut svg, grid, panel, OVERVIEW_STROKE_WIDTH_MM);
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

    writeln!(svg, "  </g>").unwrap();
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
    imported: &ImportedDesign,
    resolution: Resolution,
) -> anyhow::Result<Vec<BoardArrayLayerOverlay>> {
    Ok(imported
        .layer_definitions
        .iter()
        .enumerate()
        .map(|(layer_index, layer)| {
            let doc = imported
                .materialize_layer(LayerId(layer_index as u32), ArtworkScope::ArraySupport)?;
            let paths = layer_paths(doc, resolution)?;
            Ok::<_, anyhow::Error>((!paths.is_empty()).then_some(BoardArrayLayerOverlay {
                function: layer.layer_function,
                paths,
            }))
        })
        .collect::<anyhow::Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect())
}

struct BoardArrayProfileSvgPaths {
    array_outlines: Vec<String>,
    material_removal: Vec<String>,
}

fn board_array_profile_paths(
    imported: &ImportedDesign,
    doc: &GeometryDocument,
    resolution: Resolution,
) -> Result<BoardArrayProfileSvgPaths> {
    let score_lines = crate::geometry::board_array_vscore_lines(imported)?;
    let profile =
        crate::geometry::board_array_fabrication_profile(imported, doc, &score_lines, resolution)?;

    Ok(BoardArrayProfileSvgPaths {
        array_outlines: profile
            .array_outlines
            .iter()
            .filter_map(|payloads| payloads_path_data(payloads))
            .collect(),
        material_removal: payloads_path_data(&profile.material_removal)
            .into_iter()
            .collect(),
    })
}

/// Overlay paths for the features native to a single-layer document.
fn layer_paths(
    mut doc: GeometryDocument,
    resolution: Resolution,
) -> anyhow::Result<Vec<BoardArrayLayerPath>> {
    let Some(layer) = doc.layers.first() else {
        return Ok(Vec::new());
    };
    let (source_layer, function) = (layer.source_layer_ref, layer.layer_function);
    let native =
        |feature: &Feature<ipc2581::Symbol>| feature.source_layer_ref == Some(source_layer);

    // V-score features draw as stroked guides; everything else composes
    // through the shared layer image, which resolves paint polarity.
    let mut paths = doc
        .features
        .iter()
        .filter(|feature| native(feature) && feature.is_vscore())
        .flat_map(|feature| vscore_paths(&doc, feature))
        .collect::<Vec<_>>();

    pcb_ir::dialects::ipc::process::retain_features(&mut doc, |feature| {
        native(feature) && !feature.is_vscore()
    });
    if !doc.features.is_empty() {
        let image = doc.into_layer_image(
            0,
            crate::layers::layer_role(function),
            pcb_ir::dialects::Side::None,
            resolution,
        )?;
        let contours = image.to_contours();
        if !contours.is_empty() {
            paths.push(BoardArrayLayerPath {
                data: svg_path_data(&contours),
                bbox: image.bbox,
                stroke_width: 0.0,
                filled: true,
                stroked: false,
                vscore: false,
            });
        }
    }

    Ok(paths)
}

fn board_instance_paths(doc: &GeometryDocument, include_cutouts: bool) -> Vec<String> {
    doc.layout
        .instances
        .iter()
        .filter_map(|instance| {
            let step = doc.layout.steps.get(instance.child_step as usize)?;
            (step.kind == LayoutStepKind::Board)
                .then(|| step_profile_path_data(doc, step, instance.transform, include_cutouts))?
        })
        .collect()
}

fn step_profile_path_data(
    doc: &GeometryDocument,
    step: &LayoutStep<ipc2581::Symbol>,
    transform: Affine2,
    include_cutouts: bool,
) -> Option<String> {
    let mut contours = Vec::new();
    for profile_index in step.profiles.indices() {
        let profile = doc.profiles.get(profile_index as usize)?;
        contours.extend(doc.transformed_path_contours(profile.outer_path, transform));
        if include_cutouts {
            for cutout in profile.cutouts.slice(&doc.profile_cutouts) {
                contours.extend(doc.transformed_path_contours(cutout.path, transform));
            }
        }
    }
    payloads_path_data(&contours)
}

fn vscore_paths(
    doc: &GeometryDocument,
    feature: &Feature<ipc2581::Symbol>,
) -> Vec<BoardArrayLayerPath> {
    doc.placements_for_feature(feature)
        .iter()
        .flat_map(|&placement| {
            feature.paths.indices().filter_map(move |path_index| {
                let path = doc.arena.path(path_index);
                let data = svg_path_data(&doc.transformed_path_contours(path_index, placement));
                (!data.is_empty()).then_some(BoardArrayLayerPath {
                    data,
                    bbox: path.bbox.transformed(placement),
                    stroke_width: path.stroke().map_or(0.0, |stroke| stroke.width),
                    filled: path.is_filled(),
                    stroked: path.is_stroked(),
                    vscore: true,
                })
            })
        })
        .collect()
}

fn payloads_path_data(payloads: &[ContourBuf]) -> Option<String> {
    let path_data = svg_path_data(payloads);
    (!path_data.is_empty()).then_some(path_data)
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

    match crate::layers::layer_role(function) {
        LayerRole::Drill => BoardArrayLayerStyle {
            class_name: "array-layer-drill",
            fill: "#2563eb",
            stroke: "#1d4ed8",
            fill_opacity: 0.85,
            stroke_opacity: 0.85,
        },
        LayerRole::Copper => BoardArrayLayerStyle {
            class_name: "array-layer-copper",
            fill: "#d87822",
            stroke: "#b45309",
            fill_opacity: 0.90,
            stroke_opacity: 0.85,
        },
        LayerRole::Soldermask => BoardArrayLayerStyle {
            class_name: "array-layer-mask",
            fill: "#159447",
            stroke: "#15803d",
            fill_opacity: 0.55,
            stroke_opacity: 0.70,
        },
        LayerRole::Paste => BoardArrayLayerStyle {
            class_name: "array-layer-paste",
            fill: "#64748b",
            stroke: "#475569",
            fill_opacity: 0.90,
            stroke_opacity: 0.85,
        },
        LayerRole::Legend => BoardArrayLayerStyle {
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

fn write_rail_guides(svg: &mut String, grid: &BoardArrayGridInfo, panel: BBox, stroke_width: f64) {
    for x in [
        panel.min.x + grid.edge_rail.left.mm(),
        panel.max.x - grid.edge_rail.right.mm(),
    ] {
        if x > panel.min.x && x < panel.max.x {
            writeln!(
                svg,
                "  <line class='rail-guide' x1='{}' y1='{}' x2='{}' y2='{}' stroke='#cbd5e1' stroke-width='{}' opacity='0.62'/>",
                fmt_num(x),
                fmt_num(panel.min.y),
                fmt_num(x),
                fmt_num(panel.max.y),
                fmt_num(stroke_width)
            )
            .unwrap();
        }
    }
    for y in [
        panel.min.y + grid.edge_rail.bottom.mm(),
        panel.max.y - grid.edge_rail.top.mm(),
    ] {
        if y > panel.min.y && y < panel.max.y {
            writeln!(
                svg,
                "  <line class='rail-guide' x1='{}' y1='{}' x2='{}' y2='{}' stroke='#cbd5e1' stroke-width='{}' opacity='0.62'/>",
                fmt_num(panel.min.x),
                fmt_num(y),
                fmt_num(panel.max.x),
                fmt_num(y),
                fmt_num(stroke_width)
            )
            .unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::accessors::IpcAccessor;

    use super::*;

    fn overview(ipc: &ipc2581::Ipc2581, resolution: Resolution) -> String {
        let imported = pcb_ir::import::ipc2581::import_design(ipc, resolution).unwrap();
        render_board_array_overview_svg(&IpcAccessor::new(ipc), &imported, resolution)
            .unwrap()
            .unwrap()
    }

    #[test]
    fn renders_simple_board_array_overview_svg() {
        let resolution = Resolution::default();

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
        let svg = overview(&ipc, resolution);

        assert!(svg.contains("data-board-array-overview='true'"));
        assert!(svg.contains("viewBox='-1 -25 46 26'"));
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
    fn draws_a_panel_away_from_the_origin_inside_the_viewbox() {
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
            <PolyBegin x="10" y="20"/>
            <PolyStepSegment x="10" y="44"/>
            <PolyStepSegment x="54" y="44"/>
            <PolyStepSegment x="54" y="20"/>
          </Polygon>
        </Profile>
        <StepRepeat stepRef="board" x="15" y="25.5" nx="3" ny="2" dx="12" dy="8"/>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();

        let svg = overview(&ipc, Resolution::default());

        // The flip group maps world y to screen -y, so the padded panel
        // [9, 55] x [19, 45] is this viewBox, and every drawn coordinate
        // below lies inside it.
        assert!(svg.contains("viewBox='9 -45 46 26'"));
        assert!(svg.contains("<g transform='scale(1 -1)'>"));
        assert!(svg.contains("class='board-array-outline' d='M10 20 L10 44 L54 44 L54 20 Z'"));
        assert!(svg.contains("class='board-outline' d='M15 25.5 L25 25.5 L25 30.5 L15 30.5 Z'"));
        assert!(svg.contains("class='board-outline' d='M39 33.5 L49 33.5 L49 38.5 L39 38.5 Z'"));
        assert!(svg.contains("class='rail-guide' x1='14' y1='20' x2='14' y2='44'"));
        assert!(svg.contains("class='rail-guide' x1='50' y1='20' x2='50' y2='44'"));
        assert!(svg.contains("class='rail-guide' x1='10' y1='24' x2='54' y2='24'"));
        assert!(svg.contains("class='rail-guide' x1='10' y1='40' x2='54' y2='40'"));
    }

    #[test]
    fn renders_board_array_overview_from_array_profile() {
        let resolution = Resolution::default();

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
        let svg = overview(&ipc, resolution);

        assert!(svg.contains("class='board-array-outline'"));
        assert!(svg.contains(" A3 3"));
        assert!(!svg.contains("class='board-array-outline' x="));
    }

    #[test]
    fn renders_board_array_overview_vcuts_from_vcut_layer_only() {
        let resolution = Resolution::default();

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
            </Features>
            <Features>
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
        let svg = overview(&ipc, resolution);

        assert_eq!(svg.matches("vcut-guide").count(), 2);
        assert!(svg.contains("d='M5 0 L5 24'"));
        assert!(svg.contains("d='M0 5.5 L44 5.5'"));
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
        let resolution = Resolution::default();

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
            </Features>
            <Features>
              <Line startX="15" startY="0" endX="15" endY="20">
                <LineDesc lineWidth="0.1" lineEnd="ROUND"/>
              </Line>
            </Features>
            <Features>
              <Line startX="0" startY="5" endX="20" endY="5">
                <LineDesc lineWidth="0.1" lineEnd="ROUND"/>
              </Line>
            </Features>
            <Features>
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
        let svg = overview(&ipc, resolution);

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
        let resolution = Resolution::default();

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
        let svg = overview(&ipc, resolution);

        assert_eq!(svg.matches("array-layer-copper").count(), 1);
        assert!(!svg.contains("M7 9.5 L15 9.5"));
    }

    #[test]
    fn renders_clear_features_as_holes_in_the_layer_overlay() {
        let resolution = Resolution::default();

        let ipc = ipc2581::Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="panel"/>
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
        <LayerFeature layerRef="TOP">
          <Set>
            <Features>
              <UserSpecial>
                <Contour>
                  <Polygon>
                    <PolyBegin x="1" y="19"/>
                    <PolyStepSegment x="9" y="19"/>
                    <PolyStepSegment x="9" y="23"/>
                    <PolyStepSegment x="1" y="23"/>
                    <PolyStepSegment x="1" y="19"/>
                  </Polygon>
                </Contour>
              </UserSpecial>
            </Features>
          </Set>
          <Set polarity="NEGATIVE">
            <Features>
              <UserSpecial>
                <Contour>
                  <Polygon>
                    <PolyBegin x="4" y="20"/>
                    <PolyStepSegment x="6" y="20"/>
                    <PolyStepSegment x="6" y="22"/>
                    <PolyStepSegment x="4" y="22"/>
                    <PolyStepSegment x="4" y="20"/>
                  </Polygon>
                </Contour>
              </UserSpecial>
            </Features>
          </Set>
        </LayerFeature>
        <StepRepeat stepRef="board" x="5" y="5.5" nx="3" ny="2" dx="12" dy="8"/>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();
        let svg = overview(&ipc, resolution);

        assert_eq!(
            svg.matches("array-layer-copper").count(),
            1,
            "the layer composes to one image path"
        );
        let copper = svg
            .lines()
            .find(|line| line.contains("array-layer-copper"))
            .unwrap();
        assert_eq!(
            copper.matches('M').count(),
            2,
            "the clear feature survives as a hole subpath, not a copper fill"
        );
    }
}
