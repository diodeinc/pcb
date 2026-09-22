//! A board as it looks from one side: the physical layers a viewer sees,
//! stacked in the order they are built up.
//!
//! The drawing is ordinary artwork. Every layer is sequential paint over the
//! same tables, and every layer ends in the same cuts: all that is not board
//! material, from the space around the profile to the last drilled hole. So
//! the laminate is a sheet less the cuts, the mask is that less its
//! openings, and no artwork shows where there is no board to carry it.
//! Nothing under the outer copper can be seen from outside and none of it is
//! drawn.

use anyhow::{Context, Result, bail};
use ipc2581::Symbol;
use pcb_ir::dialects::artwork::{self, Geometry, Object, PaintStage};
use pcb_ir::dialects::ipc::{ProfileSet, profile_occurrences_for};
use pcb_ir::dialects::{LayerRole, Side};
use pcb_ir::geom::{
    BBox, ContourBuf, FillRule, LineCap, Paint, PathCmd, Point, Polarity, Resolution, StrokeStyle,
};
use pcb_ir::import::ipc2581::{ImportedDesign, LayerId};
use pcb_ir::render::LayerStyle;

use crate::accessors::{ColorInfo, IpcAccessor};
use crate::geometry::render::layer_objects;
use crate::geometry::step_artwork::{finish_step_graph_artwork, root_step};
use crate::layers::{ir_side, layer_role};
use crate::{BoardSide, LayoutTarget};

pub type CompositeDocument = artwork::Document<(), Option<Symbol>>;
type CompositeObject = Object<Option<Symbol>>;

/// How each physical layer of the stack draws.
#[derive(Debug, Clone, PartialEq)]
pub struct CompositeStyle {
    /// Bare laminate.
    pub substrate: LayerStyle,
    /// Copper under the mask.
    pub copper: LayerStyle,
    /// Copper the mask leaves open, in its surface finish.
    pub finish: LayerStyle,
    /// The mask itself; its opacity is how much of the copper shows through.
    pub mask: LayerStyle,
    pub legend: LayerStyle,
    /// V-score grooves across an array.
    pub score: LayerStyle,
}

const fn style(color: u32, opacity: f64) -> LayerStyle {
    LayerStyle { color, opacity }
}

impl Default for CompositeStyle {
    /// Neutral greys under a dark translucent mask, the palette tracespace's
    /// `pcb-stackup` made familiar: grey copper lightens the mask evenly, so
    /// traces read by brightness under a mask of any colour.
    fn default() -> Self {
        Self {
            substrate: style(0x666666, 1.0),
            copper: style(0xcccccc, 1.0),
            finish: style(0xcc9933, 1.0),
            mask: style(0x004200, 0.75),
            legend: style(0xffffff, 1.0),
            score: style(0x000000, 0.35),
        }
    }
}

/// Mask inks by the name a stackup gives them: the dark, slightly dull colour
/// of the cured mask, which the copper under it then lightens. A name outside
/// the list draws in whatever colour the stackup resolves it to.
const MASK_INKS: [(&str, u32); 7] = [
    ("green", 0x004200),
    ("black", 0x0a0a0a),
    ("blue", 0x0a2260),
    ("red", 0x7a0c0c),
    ("purple", 0x3a1466),
    ("yellow", 0xc79a00),
    ("white", 0xf0f0f0),
];

impl CompositeStyle {
    /// The default look in the colours the design's stackup gives this side:
    /// its mask, its legend ink and the board's surface finish. What the
    /// stackup leaves unsaid keeps the default.
    pub fn of_design(accessor: &IpcAccessor<'_>, side: BoardSide) -> Self {
        let rgb = |(red, green, blue): (u8, u8, u8)| u32::from_be_bytes([0, red, green, blue]);
        let color = |ink: &ColorInfo| ink.rgb_color().map(rgb);
        let mask_ink = |ink: &ColorInfo| {
            let name = ink.name.as_deref()?.to_lowercase();
            Some(MASK_INKS.iter().find(|(known, _)| *known == name)?.1)
        };
        let inks = accessor.stackup_inks();
        let ink = |role: LayerRole| {
            let on_side = |layer: &ipc2581::types::Layer| {
                layer_role(layer.layer_function) == role && ir_side(layer.side) == side.ir_side()
            };
            inks.iter()
                .find(|(layer, _)| on_side(layer))
                .map(|ink| &ink.1)
        };
        let finish = accessor
            .stackup_details()
            .and_then(|stackup| Some(rgb(stackup.surface_finish?.rgb_color())));
        let default = Self::default();
        let mask = ink(LayerRole::Soldermask).and_then(|ink| mask_ink(ink).or_else(|| color(ink)));
        let legend = ink(LayerRole::Legend).and_then(color);
        Self {
            mask: style(mask.unwrap_or(default.mask.color), default.mask.opacity),
            legend: style(
                legend.unwrap_or(default.legend.color),
                default.legend.opacity,
            ),
            finish: style(
                finish.unwrap_or(default.finish.color),
                default.finish.opacity,
            ),
            ..default
        }
    }
}

/// A composite drawing, the style of each of its layers by index, and how to
/// look at it.
pub struct Composite {
    pub artwork: CompositeDocument,
    pub styles: Vec<LayerStyle>,
    /// The board and a margin around it. Artwork may reach further; the
    /// picture is of the board.
    pub viewport: BBox,
    /// Whether the side is seen from behind the document's frame, as the
    /// bottom is: the board turned over about its vertical axis.
    pub mirrored: bool,
}

/// Margin the view keeps around the board.
const VIEW_MARGIN_MM: f64 = 1.0;
/// Width a V-score groove opens to at the surface.
const SCORE_GROOVE_WIDTH_MM: f64 = 0.4;

/// Draw the `side` of the board or array `target` selects.
pub fn composite_artwork(
    imported: &ImportedDesign,
    side: BoardSide,
    target: LayoutTarget,
    style: &CompositeStyle,
    resolution: Resolution,
) -> Result<Composite> {
    let board = target == LayoutTarget::Board;
    let root = root_step(imported, board)?;
    let mut artwork = CompositeDocument::new();

    // Source layers lowered into the shared tables, each as its objects by
    // stage.
    let mut lower = |selects: &dyn Fn(&ipc2581::types::Layer) -> bool| -> Result<Vec<_>> {
        imported
            .layer_definitions
            .iter()
            .enumerate()
            .filter(|(_, layer)| selects(layer))
            .map(|(index, _)| {
                let layer = LayerId(index as u32);
                Ok(layer_objects(imported, layer, root, &mut artwork, resolution)?.0)
            })
            .collect()
    };
    let on_side = |role: LayerRole| {
        move |layer: &ipc2581::types::Layer| {
            layer_role(layer.layer_function) == role && ir_side(layer.side) == side.ir_side()
        }
    };
    let outer_copper = imported
        .layer_definitions
        .iter()
        .find(|layer| on_side(LayerRole::Copper)(layer))
        .with_context(|| format!("IPC-2581 design has no {side} copper layer"))?
        .name;
    let [copper, slots] = lower(&|layer| layer.name == outer_copper)?
        .into_iter()
        .next()
        .context("the outer copper layer was just found")?;
    // Slots come with the copper their span reaches; holes image only on
    // their own layer. One whose span ends at this side's copper, or names
    // no end, opens onto this side.
    let holes = lower(&|layer| {
        layer_role(layer.layer_function) == LayerRole::Drill
            && layer.span.is_none_or(|span| {
                [span.from_layer, span.to_layer]
                    .into_iter()
                    .any(|end| end.is_none_or(|end| end == outer_copper))
            })
    })?;
    let painted = |layers: Vec<[Vec<CompositeObject>; 2]>| {
        layers.into_iter().flat_map(|[painted, _]| painted)
    };
    // A mask layer images its openings, so they clear whatever they paint. A
    // side without one has no openings: its mask covers it whole.
    let openings = painted(lower(&on_side(LayerRole::Soldermask))?)
        .map(|opening| CompositeObject {
            polarity: Polarity::Clear.compose(opening.polarity),
            ..opening
        })
        .collect::<Vec<_>>();
    let legend = painted(lower(&on_side(LayerRole::Legend))?).collect::<Vec<_>>();

    let material = material_objects(&mut artwork, imported, board)?;
    // An array's fabrication cuts it further and scores it; a board drawn
    // alone has neither, whatever array its file places it in.
    let (removal, scores) = if board {
        (Vec::new(), Vec::new())
    } else {
        array_objects(&mut artwork, imported, resolution)?
    };
    // A cut clears on every layer, painted or not. Left a final cutout it
    // would image as itself wherever a layer paints nothing under it.
    let cuts = std::iter::once(material.outside)
        .chain(removal)
        .chain(slots)
        .chain(holes.into_iter().flatten().flatten())
        .map(|mut cut| {
            cut.polarity = Polarity::Clear;
            cut.order.stage = PaintStage::Overlay;
            cut
        })
        .collect::<Vec<_>>();
    let sheet = vec![material.sheet];

    let mut composite = Composite {
        artwork,
        styles: Vec::new(),
        viewport: material.bounds.expand(VIEW_MARGIN_MM),
        mirrored: side == BoardSide::Bottom,
    };
    composite.layer("Substrate", style.substrate, [&sheet, &cuts]);
    // All the copper in its finish, then what the mask covers over it: the
    // finish is left showing exactly where the mask opens.
    composite.layer("Finish", style.finish, [&copper, &cuts]);
    composite.layer("Copper", style.copper, [&copper, &openings, &cuts]);
    composite.layer("Mask", style.mask, [&sheet, &openings, &cuts]);
    // Mask openings cut the legend, as a fabricator clips it off the pads.
    composite.layer("Legend", style.legend, [&legend, &openings, &cuts]);
    composite.layer("Score", style.score, [&scores, &cuts]);

    finish_step_graph_artwork(&mut composite.artwork)?;
    Ok(composite)
}

impl Composite {
    /// A layer painting `objects` in order. Every layer is drawn material of
    /// no fabrication role: what it looks like is its style's to say.
    fn layer<const N: usize>(
        &mut self,
        name: &str,
        style: LayerStyle,
        objects: [&Vec<CompositeObject>; N],
    ) {
        self.styles.push(style);
        let layer =
            self.artwork
                .push_layer(artwork::Layer::new(name, LayerRole::Other, Side::None));
        for object in objects.into_iter().flatten() {
            self.artwork.push_object(layer, object.clone());
        }
    }
}

fn region(
    artwork: &mut CompositeDocument,
    polarity: Polarity,
    contours: impl IntoIterator<Item = ContourBuf>,
) -> CompositeObject {
    let fill = Paint::Fill {
        rule: FillRule::EvenOdd,
    };
    let path = artwork.push_path(fill, contours);
    CompositeObject::new(polarity, Geometry::Region { path })
}

/// Where the board is, said once for every layer.
struct Material {
    /// Bounds of the profile.
    bounds: BBox,
    /// A sheet reaching past everything the view shows.
    sheet: CompositeObject,
    /// All of the sheet that is not board: around the profile and inside
    /// its cutouts.
    outside: CompositeObject,
}

fn material_objects(
    artwork: &mut CompositeDocument,
    imported: &ImportedDesign,
    board: bool,
) -> Result<Material> {
    let geometry = &imported.geometry;
    let profile_set = if board {
        ProfileSet::BoardOutlines
    } else {
        ProfileSet::RootOnly
    };
    let occurrences = profile_occurrences_for(geometry, profile_set);
    let bounds = occurrences
        .iter()
        .map(|occurrence| {
            geometry.transformed_path_bbox(occurrence.profile.outer_path, occurrence.transform)
        })
        .fold(BBox::empty(), BBox::union);
    if bounds.is_empty() {
        bail!("IPC-2581 design has no profile to draw a board from");
    }
    let profile = occurrences.iter().flat_map(|occurrence| {
        let cutouts = occurrence.profile.cutouts.slice(&geometry.profile_cutouts);
        std::iter::once(occurrence.profile.outer_path)
            .chain(cutouts.iter().map(|cutout| cutout.path))
            .flat_map(|path| geometry.transformed_path_contours(path, occurrence.transform))
    });
    // Twice the view's margin, so the sheet's own edge is never in view.
    let BBox { min, max } = bounds.expand(2.0 * VIEW_MARGIN_MM);
    let sheet = ContourBuf::new(vec![
        PathCmd::move_to(min),
        PathCmd::line_to(Point::new(max.x, min.y)),
        PathCmd::line_to(max),
        PathCmd::line_to(Point::new(min.x, max.y)),
        PathCmd::close(),
    ]);
    // Even-odd, the profile inside the sheet is a hole in it and a cutout
    // inside the profile is filled again.
    let outside = std::iter::once(sheet.clone())
        .chain(profile)
        .collect::<Vec<_>>();
    Ok(Material {
        bounds,
        sheet: region(artwork, Polarity::Dark, [sheet]),
        outside: region(artwork, Polarity::Clear, outside),
    })
}

/// What an array's fabrication adds to it: the material it routs away, board
/// cutouts and V-score reliefs among it, and its V-score lines as the grooves
/// they leave in the surface.
fn array_objects(
    artwork: &mut CompositeDocument,
    imported: &ImportedDesign,
    resolution: Resolution,
) -> Result<(Vec<CompositeObject>, Vec<CompositeObject>)> {
    let lines = crate::geometry::board_array_vscore_lines(imported)?;
    let removal = crate::geometry::board_array_fabrication_profile(
        imported,
        &imported.geometry,
        &lines,
        resolution,
    )?
    .material_removal;
    let groove = Paint::Stroke(StrokeStyle::new(SCORE_GROOVE_WIDTH_MM, LineCap::Butt));
    let scores = lines
        .into_iter()
        .map(|line| {
            let contour = ContourBuf::new(vec![
                PathCmd::move_to(line.start),
                PathCmd::line_to(line.end),
            ]);
            let path = artwork.push_path(groove, [contour]);
            CompositeObject::new(Polarity::Dark, Geometry::Stroke { path })
        })
        .collect();
    Ok((vec![region(artwork, Polarity::Clear, removal)], scores))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 10 x 6 mm board with a 1 mm square cut out of it. One 4 x 2 mm pad
    /// has its left half opened by the mask and a legend mark half over that
    /// opening; another covers the cutout and hangs 1 mm over the board's
    /// edge; and a hole is drilled clear of both. The top's inks are blue
    /// and black, the bottom's red and white.
    const BOARD: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <DictionaryStandard units="MILLIMETER">
      <EntryStandard id="pad">
        <RectCenter width="4" height="2"/>
      </EntryStandard>
      <EntryStandard id="opening">
        <RectCenter width="2" height="2"/>
      </EntryStandard>
      <EntryStandard id="mark">
        <RectCenter width="2" height="1"/>
      </EntryStandard>
    </DictionaryStandard>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER">
      <Spec name="top-legend">
        <General type="MATERIAL"><Property text="Color : Black"/></General>
      </Spec>
      <Spec name="top-mask">
        <General type="MATERIAL"><Property text="Color : Blue"/></General>
      </Spec>
      <Spec name="bottom-mask">
        <General type="MATERIAL"><Property text="Color : Red"/></General>
      </Spec>
      <Spec name="bottom-legend">
        <General type="MATERIAL"><Property text="Color : Chartreuse"/></General>
      </Spec>
    </CadHeader>
    <CadData>
      <Layer name="F.Silk" layerFunction="SILKSCREEN" side="TOP" polarity="POSITIVE"/>
      <Layer name="F.Mask" layerFunction="SOLDERMASK" side="TOP" polarity="POSITIVE"/>
      <Layer name="F.Cu" layerFunction="CONDUCTOR" side="TOP" polarity="POSITIVE"/>
      <Layer name="B.Cu" layerFunction="CONDUCTOR" side="BOTTOM" polarity="POSITIVE"/>
      <Layer name="B.Mask" layerFunction="SOLDERMASK" side="BOTTOM" polarity="POSITIVE"/>
      <Layer name="B.Silk" layerFunction="SILKSCREEN" side="BOTTOM" polarity="POSITIVE"/>
      <Layer name="User" layerFunction="DOCUMENT" side="TOP" polarity="POSITIVE"/>
      <Layer name="Drill" layerFunction="DRILL" side="ALL" polarity="POSITIVE">
        <Span fromLayer="F.Cu" toLayer="B.Cu"/>
      </Layer>
      <Stackup name="Stackup" overallThickness="1.6">
        <StackupGroup name="Group">
          <StackupLayer layerOrGroupRef="F.Silk" thickness="0" sequence="0">
            <SpecRef id="top-legend"/>
          </StackupLayer>
          <StackupLayer layerOrGroupRef="F.Mask" thickness="0.01" sequence="1">
            <SpecRef id="top-mask"/>
          </StackupLayer>
          <StackupLayer layerOrGroupRef="F.Cu" thickness="0.035" sequence="2"/>
          <StackupLayer layerOrGroupRef="B.Cu" thickness="0.035" sequence="3"/>
          <StackupLayer layerOrGroupRef="B.Mask" thickness="0.01" sequence="4">
            <SpecRef id="bottom-mask"/>
          </StackupLayer>
          <StackupLayer layerOrGroupRef="B.Silk" thickness="0" sequence="5">
            <SpecRef id="bottom-legend"/>
          </StackupLayer>
        </StackupGroup>
      </Stackup>
      <Step name="board" type="BOARD">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="10" y="0"/>
            <PolyStepSegment x="10" y="6"/>
            <PolyStepSegment x="0" y="6"/>
          </Polygon>
          <Cutout>
            <PolyBegin x="7" y="0.5"/>
            <PolyStepSegment x="8" y="0.5"/>
            <PolyStepSegment x="8" y="1.5"/>
            <PolyStepSegment x="7" y="1.5"/>
          </Cutout>
        </Profile>
        <PadStackDef name="pad">
          <PadstackPadDef layerRef="F.Cu" padUse="REGULAR">
            <StandardPrimitiveRef id="pad"/>
          </PadstackPadDef>
        </PadStackDef>
        <PadStackDef name="opening">
          <PadstackPadDef layerRef="F.Mask" padUse="REGULAR">
            <StandardPrimitiveRef id="opening"/>
          </PadstackPadDef>
        </PadStackDef>
        <PadStackDef name="mark">
          <PadstackPadDef layerRef="F.Silk" padUse="REGULAR">
            <StandardPrimitiveRef id="mark"/>
          </PadstackPadDef>
        </PadStackDef>
        <PadStackDef name="note">
          <PadstackPadDef layerRef="User" padUse="REGULAR">
            <StandardPrimitiveRef id="pad"/>
          </PadstackPadDef>
        </PadStackDef>
        <LayerFeature layerRef="F.Silk">
          <Set>
            <Pad padstackDefRef="mark">
              <Location x="3" y="3.5"/>
            </Pad>
          </Set>
        </LayerFeature>
        <LayerFeature layerRef="F.Mask">
          <Set>
            <Pad padstackDefRef="opening">
              <Location x="2" y="3"/>
            </Pad>
          </Set>
        </LayerFeature>
        <LayerFeature layerRef="F.Cu">
          <Set>
            <Pad padstackDefRef="pad">
              <Location x="3" y="3"/>
            </Pad>
            <Pad padstackDefRef="pad">
              <Location x="9" y="1"/>
            </Pad>
          </Set>
        </LayerFeature>
        <LayerFeature layerRef="User">
          <Set>
            <Pad padstackDefRef="note">
              <Location x="3" y="5"/>
            </Pad>
          </Set>
        </LayerFeature>
        <LayerFeature layerRef="Drill">
          <Set>
            <Hole name="H1" diameter="1" platingStatus="NONPLATED" x="8" y="4"/>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#;

    /// Board material: the profile less its cutout and the hole.
    const MATERIAL_MM2: f64 = 60.0 - 1.0 - std::f64::consts::PI * 0.25;

    /// Each layer's name and the area it paints, in paint order.
    fn layer_areas(side: BoardSide) -> Vec<(String, f64)> {
        let ipc = ipc2581::Ipc2581::parse(BOARD).unwrap();
        let resolution = Resolution::default();
        let imported = pcb_ir::import::ipc2581::import_design(&ipc, resolution).unwrap();
        let composite = composite_artwork(
            &imported,
            side,
            LayoutTarget::Board,
            &CompositeStyle::default(),
            resolution,
        )
        .unwrap();
        assert_eq!(composite.styles.len(), composite.artwork.layers.len());
        assert_eq!(composite.mirrored, side == BoardSide::Bottom);
        assert_eq!(
            composite.viewport,
            BBox::new(Point::new(-1.0, -1.0), Point::new(11.0, 7.0)),
            "the view frames the board, not the copper hanging over its edge"
        );
        let (images, _) =
            artwork::compose_owner_regions(&composite.artwork, |_| Some(()), resolution).unwrap();
        composite
            .artwork
            .layers
            .iter()
            .zip(images)
            .map(|(layer, owners)| {
                let area = owners.iter().map(|(_, image)| image.area()).sum();
                (layer.name.clone(), area)
            })
            .collect()
    }

    fn assert_areas(actual: &[(String, f64)], expected: &[(&str, f64)]) {
        assert_eq!(
            actual
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            expected.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
        );
        for ((name, actual), (_, expected)) in actual.iter().zip(expected) {
            assert!(
                (actual - expected).abs() < 1e-2,
                "{name} paints {actual} mm², not {expected}"
            );
        }
    }

    #[test]
    fn the_top_shows_finish_in_openings_and_nothing_where_there_is_no_board() {
        // The second pad keeps 5 of its 8 mm²: 2 hang over the edge and 1 is
        // over the cutout.
        assert_areas(
            &layer_areas(BoardSide::Top),
            &[
                ("Substrate", MATERIAL_MM2),
                ("Finish", 8.0 + 5.0),
                // The opening uncovers the first pad's left half and cuts the
                // half of the legend mark printed over it.
                ("Copper", 4.0 + 5.0),
                ("Mask", MATERIAL_MM2 - 4.0),
                ("Legend", 1.0),
                ("Score", 0.0),
            ],
        );
    }

    #[test]
    fn a_side_with_nothing_on_it_is_masked_laminate_and_still_drilled() {
        assert_areas(
            &layer_areas(BoardSide::Bottom),
            &[
                ("Substrate", MATERIAL_MM2),
                ("Finish", 0.0),
                ("Copper", 0.0),
                ("Mask", MATERIAL_MM2),
                ("Legend", 0.0),
                ("Score", 0.0),
            ],
        );
    }

    #[test]
    fn each_side_draws_in_the_inks_its_own_stackup_layers_name() {
        let ipc = ipc2581::Ipc2581::parse(BOARD).unwrap();
        let accessor = IpcAccessor::new(&ipc);
        let default = CompositeStyle::default();

        let top = CompositeStyle::of_design(&accessor, BoardSide::Top);
        assert_eq!(top.mask, style(0x0a2260, default.mask.opacity));
        assert_eq!(top.legend.color, 0x000000);
        // A name no table knows leaves the default, as does a finish the
        // stackup does not give.
        let bottom = CompositeStyle::of_design(&accessor, BoardSide::Bottom);
        assert_eq!(bottom.mask.color, 0x7a0c0c);
        assert_eq!(bottom.legend, default.legend);
        assert_eq!(
            (&top.finish, &bottom.finish),
            (&default.finish, &default.finish)
        );
        assert_eq!(top.substrate, default.substrate);
    }
}
