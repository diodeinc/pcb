use std::collections::HashMap;

use anyhow::Result;
use pcb_ir::dialects::artwork::{self, Geometry, Object};
use pcb_ir::dialects::ipc::process::{normalize_for_artwork, retain_features};
use pcb_ir::dialects::ipc::{
    ArtworkScope, Feature, LayoutStepKind, NetMetaLowering, lower_layer_to_artwork_objects_with,
};
use pcb_ir::dialects::{LayerRole, Side};
use pcb_ir::geom::{BBox, ContourBuf, FillRule, LineCap, Paint, Point, Polarity, Resolution};
use pcb_ir::geom::{PathCmd, StrokeStyle};
use pcb_ir::import::ipc2581::{ImportedDesign, LayerId};
use pcb_ir::render::{LayerStyle, RenderOptions};

use crate::accessors::{BoardArrayGridInfo, IpcAccessor};
use crate::layers::layer_role;

type GeometryDocument =
    pcb_ir::dialects::ipc::Document<ipc2581::Symbol, ipc2581::types::LayerFunction>;

const OVERVIEW_STROKE_WIDTH_MM: f64 = 0.1;

fn outline() -> Paint {
    Paint::Stroke(StrokeStyle::round(OVERVIEW_STROKE_WIDTH_MM))
}

const fn style(color: u32, opacity: f64) -> LayerStyle {
    LayerStyle { color, opacity }
}

const BOARD_FILL: LayerStyle = style(0xf1f5f9, 1.0);
const RAIL_GUIDE: LayerStyle = style(0xcbd5e1, 0.62);
const ARRAY_OUTLINE: LayerStyle = style(0x111827, 1.0);
const BOARD_OUTLINE: LayerStyle = style(0x064e3b, 1.0);
/// The edge of the material the fabrication profile removes: routed slots,
/// perforations and V-score reliefs.
const MATERIAL_REMOVAL: LayerStyle = style(0x111827, 0.95);
const VSCORE_GUIDE: LayerStyle = style(0xdc2626, 1.0);

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
    if panel.width() <= 0.0
        || panel.height() <= 0.0
        || grid.board_width.mm() <= 0.0
        || grid.board_height.mm() <= 0.0
        || grid.columns == 0
        || grid.rows == 0
    {
        return Ok(None);
    }

    let mut overview = Overview::default();
    let [board_fills, board_outlines] = overview.boards(doc);
    if board_fills.is_empty() {
        return Ok(None);
    }
    let score_lines = crate::geometry::board_array_vscore_lines(imported)?;
    let profile =
        crate::geometry::board_array_fabrication_profile(imported, doc, &score_lines, resolution)?;

    let guide = Paint::Stroke(StrokeStyle::new(OVERVIEW_STROKE_WIDTH_MM, LineCap::Butt));
    overview.place_layer("Boards", BOARD_FILL, board_fills);
    overview.draw_layer("Rails", RAIL_GUIDE, guide, rail_guides(&grid, panel));
    overview.draw_layer(
        "Array outline",
        ARRAY_OUTLINE,
        outline(),
        profile.array_outlines,
    );
    overview.place_layer("Board outlines", BOARD_OUTLINE, board_outlines);
    overview.draw_layer(
        "Material removal",
        MATERIAL_REMOVAL,
        outline(),
        [profile.material_removal],
    );
    for layer_index in 0..imported.layer_definitions.len() {
        let support =
            imported.materialize_layer(LayerId(layer_index as u32), ArtworkScope::ArraySupport)?;
        overview.draw_support_layer(support, resolution)?;
    }

    artwork::normalize_bounds(&mut overview.artwork);
    let options = RenderOptions::default()
        .with_accuracy(resolution.accuracy)
        .with_id_prefix("overview-")
        .with_styles(overview.styles);
    Ok(Some(pcb_ir::render::artwork_svg(
        &overview.artwork,
        &options,
    )?))
}

/// The overview as artwork: drawing layers first, then every layer's array
/// support geometry, each with the style it draws in.
#[derive(Default)]
struct Overview {
    artwork: artwork::Document<(), Option<ipc2581::Symbol>>,
    styles: Vec<LayerStyle>,
}

impl Overview {
    fn layer(&mut self, name: &str, style: LayerStyle) -> u32 {
        self.role_layer(name, LayerRole::Other, style)
    }

    fn role_layer(&mut self, name: &str, role: LayerRole, style: LayerStyle) -> u32 {
        self.styles.push(style);
        self.artwork
            .push_layer(artwork::Layer::new(name, role, Side::None))
    }

    fn place_layer(
        &mut self,
        name: &str,
        style: LayerStyle,
        geometry: impl IntoIterator<Item = Geometry>,
    ) {
        let layer = self.layer(name, style);
        for geometry in geometry {
            self.artwork
                .push_object(layer, Object::new(Polarity::Dark, geometry));
        }
    }

    /// Every placed board, as its fill and as its outline. A board Step's
    /// profile is one block, so the array draws its board once.
    fn boards(&mut self, doc: &GeometryDocument) -> [Vec<Geometry>; 2] {
        let mut blocks = HashMap::new();
        let placed = doc
            .layout
            .instances
            .iter()
            .filter_map(|instance| {
                let step = doc.layout.steps.get(instance.child_step as usize)?;
                if step.kind != LayoutStepKind::Board {
                    return None;
                }
                let blocks = *blocks
                    .entry(instance.child_step)
                    .or_insert_with(|| self.board_blocks(doc, instance.child_step));
                Some((blocks?, instance.transform))
            })
            .collect::<Vec<_>>();
        [0, 1].map(|kind| {
            placed
                .iter()
                .map(|&(blocks, transform)| Geometry::Instance {
                    block: blocks[kind],
                    transform,
                })
                .collect()
        })
    }

    /// A board Step's profile as a filled block, cutouts open, and as a
    /// block of its outer outline; `None` for a Step without a profile.
    fn board_blocks(&mut self, doc: &GeometryDocument, step: u32) -> Option<[u32; 2]> {
        let profiles = doc.layout.steps[step as usize]
            .profiles
            .indices()
            .filter_map(|profile| doc.profiles.get(profile as usize))
            .collect::<Vec<_>>();
        let contours = |paths: &mut dyn Iterator<Item = u32>| {
            paths
                .flat_map(|path| doc.arena.path_contours(doc.arena.path(path)))
                .collect::<Vec<_>>()
        };
        let outer = contours(&mut profiles.iter().map(|profile| profile.outer_path));
        if outer.is_empty() {
            return None;
        }
        let cutouts = contours(&mut profiles.iter().flat_map(|profile| {
            let cutouts = profile.cutouts.slice(&doc.profile_cutouts);
            cutouts.iter().map(|cutout| cutout.path)
        }));
        let filled = outer.iter().cloned().chain(cutouts).collect::<Vec<_>>();
        let fill = Paint::Fill {
            rule: FillRule::EvenOdd,
        };
        Some(
            [(fill, filled), (outline(), outer)].map(|(paint, contours)| {
                let block = self.artwork.push_block();
                let shape = Object::new(Polarity::Dark, self.shape(paint, contours));
                self.artwork.push_block_object(block, shape);
                block
            }),
        )
    }

    fn shape(&mut self, paint: Paint, contours: Vec<ContourBuf>) -> Geometry {
        let path = self.artwork.push_path(paint, contours);
        match paint {
            Paint::Stroke(_) => Geometry::Stroke { path },
            _ => Geometry::Region { path },
        }
    }

    fn draw_layer(
        &mut self,
        name: &str,
        style: LayerStyle,
        paint: Paint,
        shapes: impl IntoIterator<Item = Vec<ContourBuf>>,
    ) {
        let layer = self.layer(name, style);
        for contours in shapes {
            self.draw(layer, paint, contours);
        }
    }

    fn draw(&mut self, layer: u32, paint: Paint, contours: Vec<ContourBuf>) {
        if contours.is_empty() {
            return;
        }
        let shape = Object::new(Polarity::Dark, self.shape(paint, contours));
        self.artwork.push_object(layer, shape);
    }

    /// Draw the features native to a single-layer support document. V-score
    /// features draw as guides wide enough to see; everything else lowers as
    /// the artwork it is, so paint polarity images as it fabricates.
    fn draw_support_layer(
        &mut self,
        mut doc: GeometryDocument,
        resolution: Resolution,
    ) -> Result<()> {
        let Some(layer) = doc.layers.first() else {
            return Ok(());
        };
        let (name, source_layer) = (layer.name.clone(), layer.source_layer_ref);
        let role = layer_role(layer.layer_function);
        let native =
            |feature: &Feature<ipc2581::Symbol>| feature.source_layer_ref == Some(source_layer);

        let scores = doc
            .features
            .iter()
            .filter(|feature| native(feature) && feature.is_vscore())
            .flat_map(|feature| vscore_guides(&doc, feature))
            .collect::<Vec<_>>();
        if !scores.is_empty() {
            let guides = self.layer(&format!("{name} guides"), VSCORE_GUIDE);
            for (stroke, contours) in scores {
                self.draw(guides, Paint::Stroke(stroke), contours);
            }
        }

        retain_features(&mut doc, |feature| native(feature) && !feature.is_vscore());
        if doc.features.is_empty() {
            return Ok(());
        }
        normalize_for_artwork(&mut doc, resolution)?;
        let objects =
            lower_layer_to_artwork_objects_with(&doc, 0, &mut self.artwork, &mut NetMetaLowering);
        if !objects.is_empty() {
            let layer = self.role_layer(&name, role, LayerStyle::of(role));
            for object in objects {
                self.artwork.push_object(layer, object);
            }
        }
        Ok(())
    }
}

fn vscore_guides(
    doc: &GeometryDocument,
    feature: &Feature<ipc2581::Symbol>,
) -> Vec<(StrokeStyle, Vec<ContourBuf>)> {
    doc.placements_for_feature(feature)
        .iter()
        .flat_map(|&placement| {
            feature.paths.indices().map(move |path| {
                let width = doc.arena.path(path).stroke().map_or(0.0, |s| s.width);
                (
                    StrokeStyle::round(width.max(OVERVIEW_STROKE_WIDTH_MM)),
                    doc.transformed_path_contours(path, placement),
                )
            })
        })
        .collect()
}

/// The inner edges of the panel's rails, where a rail has any width.
fn rail_guides(grid: &BoardArrayGridInfo, panel: BBox) -> Vec<Vec<ContourBuf>> {
    let line = |from: Point, to: Point| {
        vec![ContourBuf::new(vec![
            PathCmd::move_to(from),
            PathCmd::line_to(to),
        ])]
    };
    let vertical = [
        panel.min.x + grid.edge_rail.left.mm(),
        panel.max.x - grid.edge_rail.right.mm(),
    ]
    .into_iter()
    .filter(|&x| x > panel.min.x && x < panel.max.x)
    .map(|x| line(Point::new(x, panel.min.y), Point::new(x, panel.max.y)));
    let horizontal = [
        panel.min.y + grid.edge_rail.bottom.mm(),
        panel.max.y - grid.edge_rail.top.mm(),
    ]
    .into_iter()
    .filter(|&y| y > panel.min.y && y < panel.max.y)
    .map(|y| line(Point::new(panel.min.x, y), Point::new(panel.max.x, y)));
    vertical.chain(horizontal).collect()
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

    /// The first overview layer drawn in `color`, from its group tag to the
    /// end of its paint.
    fn layer<'a>(svg: &'a str, color: &str) -> &'a str {
        let open = format!("<g fill='{color}' stroke='{color}'");
        let start = svg
            .find(&open)
            .unwrap_or_else(|| panic!("no {color} layer in {svg}"));
        let end = svg[start..].find("\n    </g>").unwrap();
        &svg[start..start + end]
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

        assert!(svg.contains("viewBox='-1.05 -25.05 46.1 26.1'"));
        // The board draws once, as a fill block and an outline block.
        assert_eq!(svg.matches("M0 0 L10 0 L10 5 L0 5 Z").count(), 2);
        assert_eq!(layer(&svg, "#f1f5f9").matches("<use").count(), 3 * 2);
        assert_eq!(layer(&svg, "#064e3b").matches("<use").count(), 3 * 2);
        assert_eq!(layer(&svg, "#111827").matches("<path").count(), 1);
        assert_eq!(layer(&svg, "#cbd5e1").matches("<path").count(), 4);
        assert!(!svg.contains("#dc2626"), "no V-score layer, no guides");

        let board_outline_start = svg.find("stroke='#064e3b'").unwrap();
        let rail_start = svg.find("stroke='#cbd5e1'").unwrap();
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

        // The flip group maps world y to screen -y, so the padded outline
        // stroke [8.95, 55.05] x [18.95, 45.05] is this viewBox, and every
        // drawn coordinate below lies inside it.
        assert!(svg.contains("viewBox='8.95 -45.05 46.1 26.1'"));
        assert!(svg.contains("<g transform='scale(1 -1)'>"));
        assert!(layer(&svg, "#111827").contains("d='M10 20 L10 44 L54 44 L54 20 Z'"));
        let board_outlines = layer(&svg, "#064e3b");
        assert!(board_outlines.contains("transform='matrix(1 0 0 1 15 25.5)'"));
        assert!(board_outlines.contains("transform='matrix(1 0 0 1 39 33.5)'"));
        let rails = layer(&svg, "#cbd5e1");
        for guide in [
            "d='M14 20 L14 44'",
            "d='M50 20 L50 44'",
            "d='M10 24 L54 24'",
            "d='M10 40 L54 40'",
        ] {
            assert!(rails.contains(guide), "{guide} not in {rails}");
        }
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

        assert!(layer(&svg, "#111827").contains(" A3 3"));
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

        let guides = layer(&svg, "#dc2626");
        assert_eq!(guides.matches("<path").count(), 2);
        assert!(guides.contains("d='M5 0 L5 24'"));
        assert!(guides.contains("d='M0 5.5 L44 5.5'"));
        assert_eq!(guides.matches("stroke-width='0.1'").count(), 2);

        let vcut_start = svg.find("stroke='#dc2626'").unwrap();
        let board_outline_start = svg.find("stroke='#064e3b'").unwrap();
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

        let removal = &svg[svg.find("stroke='#111827' opacity='0.95'>").unwrap()..];
        assert!(removal[..removal.find("</g>").unwrap()].contains(" Z"));
        let blocks = &svg[..svg.find("</defs>").unwrap()];
        let subpaths = |attribute: &str| {
            let path = blocks.lines().find(|line| line.contains(attribute));
            path.unwrap().matches('M').count()
        };
        assert_eq!(subpaths("stroke-width"), 1, "the outline omits cutouts");
        assert_eq!(subpaths("evenodd"), 2, "the fill opens at cutouts");
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

        assert_eq!(svg.matches("<g fill='#d87822'").count(), 1);
        assert_eq!(layer(&svg, "#d87822").matches("<use").count(), 1);
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

        let copper = layer(&svg, "#d87822");
        assert!(copper.contains("<g mask='url(#overview-m0)'>"));
        assert!(copper.contains("M1 19") && !copper.contains("M4 20"));
        let mask = &svg[svg.find("<mask id='overview-m0'").unwrap()..];
        assert!(
            mask[..mask.find("</mask>").unwrap()].contains("M4 20"),
            "the clear feature masks the copper instead of filling"
        );
    }
}
