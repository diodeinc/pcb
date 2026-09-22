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
use pcb_ir::dialects::artwork::{self, Geometry, Object, PaintOrder, transformed_geometry};
use pcb_ir::dialects::ipc::{ProfileSet, profile_occurrences_for};
use pcb_ir::dialects::{LayerRole, Side};
use pcb_ir::geom::{
    Affine2, BBox, ContourBuf, FillRule, LineCap, Paint, PathCmd, Point, Polarity, Resolution,
    StrokeStyle,
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

/// A composite drawing, the style of each of its layers by index, and where
/// to look at it.
pub struct Composite {
    pub artwork: CompositeDocument,
    pub styles: Vec<LayerStyle>,
    /// The boards drawn and a margin around them. Artwork may reach further;
    /// the picture is of the board.
    pub viewport: BBox,
}

/// Margin the view keeps around the board.
const VIEW_MARGIN_MM: f64 = 1.0;
/// Space between two sides drawn next to each other.
const SIDE_GAP_MM: f64 = 5.0;
/// Width a V-score groove opens to at the surface.
const SCORE_GROOVE_WIDTH_MM: f64 = 0.4;
/// How far past the board the sheet every layer is cut from reaches: further
/// than any artwork, so none of it is left to show beside the board or over
/// the side drawn next to it.
const SHEET_MARGIN_MM: f64 = 1000.0;

/// Draw `sides` of the board or array `target` selects, left to right, each
/// in the inks its stackup gives it. The bottom is drawn as it looks turned
/// over about the board's vertical axis.
pub fn composite_artwork(
    imported: &ImportedDesign,
    accessor: &IpcAccessor<'_>,
    sides: &[BoardSide],
    target: LayoutTarget,
    resolution: Resolution,
) -> Result<Composite> {
    let board = target == LayoutTarget::Board;
    let mut composite = Composite {
        artwork: CompositeDocument::new(),
        styles: Vec::new(),
        viewport: BBox::empty(),
    };
    let material = material_objects(&mut composite.artwork, imported, board)?;
    // An array's fabrication cuts it further and scores it; a board drawn
    // alone has neither, whatever array its file places it in.
    let (removal, scores) = if board {
        (Vec::new(), Vec::new())
    } else {
        array_objects(&mut composite.artwork, imported, resolution)?
    };
    let bounds = material.bounds;
    let turned_over = Affine2 {
        m00: -1.0,
        m02: bounds.min.x + bounds.max.x,
        ..Affine2::IDENTITY
    };
    for (index, &side) in sides.iter().enumerate() {
        let across = Point::new(index as f64 * (bounds.width() + SIDE_GAP_MM), 0.0);
        let view = match side {
            BoardSide::Top => Affine2::IDENTITY,
            BoardSide::Bottom => turned_over,
        };
        let surface = surface_objects(&mut composite.artwork, imported, side, board, resolution)?;
        composite.stack(
            Affine2::translation(across).concat(view),
            &CompositeStyle::of_design(accessor, side),
            Stack {
                sheet: vec![material.sheet.clone()],
                cuts: std::iter::once(material.outside.clone())
                    .chain(removal.iter().cloned())
                    .chain(surface.cutouts)
                    .collect(),
                scores: scores.clone(),
                copper: surface.copper,
                masked: surface.masked,
                openings: surface.openings,
                legend: surface.legend,
            },
        );
        let placed = BBox::new(bounds.min + across, bounds.max + across);
        composite.viewport = composite.viewport.union(placed);
    }
    composite.viewport = composite.viewport.expand(VIEW_MARGIN_MM);

    finish_step_graph_artwork(&mut composite.artwork)?;
    Ok(composite)
}

/// What one side of the board carries, as its source layers image it.
struct Surface {
    copper: Vec<CompositeObject>,
    /// Whether the side has a mask layer at all. One that opens nothing
    /// still masks the whole side; a side without one is bare.
    masked: bool,
    /// The mask layers' images: where the mask is open.
    openings: Vec<CompositeObject>,
    legend: Vec<CompositeObject>,
    /// Every slot and hole that opens onto this side.
    cutouts: Vec<CompositeObject>,
}

fn surface_objects(
    artwork: &mut CompositeDocument,
    imported: &ImportedDesign,
    side: BoardSide,
    board: bool,
    resolution: Resolution,
) -> Result<Surface> {
    let root = root_step(imported, board)?;
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
                Ok(layer_objects(imported, layer, root, artwork, resolution)?.0)
            })
            .collect()
    };
    let on_side = |role: LayerRole| {
        move |layer: &ipc2581::types::Layer| {
            layer_role(layer.layer_function) == role && ir_side(layer.side) == side.ir_side()
        }
    };
    let outer_copper = |side: Side| {
        let layers = imported.layer_definitions.iter();
        let is_outer = |layer: &&ipc2581::types::Layer| {
            layer_role(layer.layer_function) == LayerRole::Copper && ir_side(layer.side) == side
        };
        layers.filter(is_outer).map(|layer| layer.name).next()
    };
    let surface = outer_copper(side.ir_side())
        .with_context(|| format!("IPC-2581 design has no {side} copper layer"))?;
    let [copper, slots] = lower(&|layer| layer.name == surface)?
        .into_iter()
        .next()
        .context("the outer copper layer was just found")?;
    // Slots come with the copper their span reaches; holes image only on
    // their own layer. One opens onto this side if its span ends at this
    // side's copper; an end it does not name is the top's, then the
    // bottom's, and one with no span goes through.
    let holes = lower(&|layer| {
        layer_role(layer.layer_function) == LayerRole::Drill
            && layer.span.is_none_or(|span| {
                let from = span.from_layer.or(outer_copper(Side::Top));
                let to = span.to_layer.or(outer_copper(Side::Bottom));
                [from, to].contains(&Some(surface))
            })
    })?;
    let painted = |layers: Vec<[Vec<CompositeObject>; 2]>| {
        layers.into_iter().flat_map(|[painted, _]| painted)
    };
    let masks = lower(&on_side(LayerRole::Soldermask))?;
    Ok(Surface {
        copper,
        masked: !masks.is_empty(),
        openings: painted(masks).collect(),
        legend: painted(lower(&on_side(LayerRole::Legend))?).collect(),
        cutouts: slots
            .into_iter()
            .chain(holes.into_iter().flatten().flatten())
            .collect(),
    })
}

/// One side's objects in the document's frame, before they are placed.
struct Stack {
    sheet: Vec<CompositeObject>,
    copper: Vec<CompositeObject>,
    masked: bool,
    openings: Vec<CompositeObject>,
    legend: Vec<CompositeObject>,
    scores: Vec<CompositeObject>,
    cuts: Vec<CompositeObject>,
}

impl Composite {
    /// Stack one side's six layers under `placement`. Every layer is its
    /// paint, then what the mask opens where that clips it, then the cuts.
    fn stack(&mut self, placement: Affine2, style: &CompositeStyle, stack: Stack) {
        let mut place = |objects: Vec<CompositeObject>, polarity: Option<Polarity>| {
            objects
                .into_iter()
                .map(|object| CompositeObject {
                    polarity: polarity.unwrap_or(object.polarity),
                    order: PaintOrder::default(),
                    geometry: transformed_geometry(&mut self.artwork, object.geometry, placement),
                    ..object
                })
                .collect::<Vec<_>>()
        };
        let sheet = place(stack.sheet, None);
        let copper = place(stack.copper, None);
        let legend = place(stack.legend, None);
        let scores = place(stack.scores, None);
        // A mask layer images its openings, so they clear whatever they
        // paint.
        let openings = place(stack.openings, None)
            .into_iter()
            .map(|opening| CompositeObject {
                polarity: Polarity::Clear.compose(opening.polarity),
                ..opening
            })
            .collect::<Vec<_>>();
        // A cut clears on every layer, painted or not. Left a final cutout
        // it would image as itself wherever a layer paints nothing under it.
        let cuts = place(stack.cuts, Some(Polarity::Clear));
        // A mask layer lays the mask over the whole sheet, however little it
        // opens, and covers the copper under it. A side without one is bare.
        let (mask, covered) = if stack.masked {
            (sheet.clone(), copper.clone())
        } else {
            (Vec::new(), Vec::new())
        };

        self.layer("Substrate", style.substrate, [&sheet, &cuts]);
        // All the copper in its finish, then what the mask covers over it:
        // the finish is left showing exactly where the mask opens.
        self.layer("Finish", style.finish, [&copper, &cuts]);
        self.layer("Copper", style.copper, [&covered, &openings, &cuts]);
        self.layer("Mask", style.mask, [&mask, &openings, &cuts]);
        // Mask openings cut the legend, as a fabricator clips it off the pads.
        self.layer("Legend", style.legend, [&legend, &openings, &cuts]);
        self.layer("Score", style.score, [&scores, &cuts]);
    }

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
    /// A sheet reaching past all the artwork.
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
    let BBox { min, max } = bounds.expand(SHEET_MARGIN_MM);
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
    use pcb_ir::geom::ContourSet;

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

    /// Each layer's name and image, in paint order, and the view.
    fn composed(board: &str, sides: &[BoardSide]) -> (Vec<(String, ContourSet)>, BBox) {
        let ipc = ipc2581::Ipc2581::parse(board).unwrap();
        let resolution = Resolution::default();
        let imported = pcb_ir::import::ipc2581::import_design(&ipc, resolution).unwrap();
        let accessor = IpcAccessor::new(&ipc);
        let composite =
            composite_artwork(&imported, &accessor, sides, LayoutTarget::Board, resolution)
                .unwrap();
        assert_eq!(composite.styles.len(), composite.artwork.layers.len());
        let (images, _) =
            artwork::compose_owner_regions(&composite.artwork, |_| Some(()), resolution).unwrap();
        let layers = composite.artwork.layers.iter().zip(images);
        let layers = layers.map(|(layer, owners)| {
            let image =
                ContourSet::union_all(resolution, owners.into_iter().map(|(_, image)| image))
                    .unwrap();
            (layer.name.clone(), image)
        });
        (layers.collect(), composite.viewport)
    }

    /// Each layer's name and the area it paints, in paint order.
    fn layer_areas(board: &str, side: BoardSide) -> Vec<(String, f64)> {
        let (layers, viewport) = composed(board, &[side]);
        assert_eq!(
            viewport,
            BBox::new(Point::new(-1.0, -1.0), Point::new(11.0, 7.0)),
            "the view frames the board, not the copper hanging over its edge"
        );
        layers
            .into_iter()
            .map(|(name, image)| (name, image.area()))
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
            &layer_areas(BOARD, BoardSide::Top),
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
    fn a_mask_layer_that_opens_nothing_masks_the_whole_side() {
        assert_areas(
            &layer_areas(BOARD, BoardSide::Bottom),
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
    fn a_side_without_a_mask_layer_is_bare() {
        // The openings now sit on a layer that is no mask, so they are one
        // more drawing to leave out.
        let top_mask = r#"layerFunction="SOLDERMASK" side="TOP""#;
        let bare = BOARD.replace(top_mask, r#"layerFunction="DOCUMENT" side="TOP""#);
        assert_areas(
            &layer_areas(&bare, BoardSide::Top),
            &[
                ("Substrate", MATERIAL_MM2),
                ("Finish", 8.0 + 5.0),
                ("Copper", 0.0),
                ("Mask", 0.0),
                // With no opening to clip it, the legend mark prints whole.
                ("Legend", 2.0),
                ("Score", 0.0),
            ],
        );
    }

    #[test]
    fn a_hole_opens_only_onto_the_sides_its_span_reaches() {
        // Naming one end leaves the other the bottom's: a hole from the
        // bottom copper to the bottom never reaches the top.
        let through = r#"<Span fromLayer="F.Cu" toLayer="B.Cu"/>"#;
        let blind = BOARD.replace(through, r#"<Span fromLayer="B.Cu"/>"#);
        let substrate = |side| layer_areas(&blind, side)[0].1;
        let hole = std::f64::consts::PI * 0.25;
        assert!((substrate(BoardSide::Top) - (MATERIAL_MM2 + hole)).abs() < 1e-2);
        assert!((substrate(BoardSide::Bottom) - MATERIAL_MM2).abs() < 1e-2);
    }

    #[test]
    fn two_sides_draw_left_to_right_with_the_bottom_turned_over() {
        let (layers, viewport) = composed(BOARD, &[BoardSide::Top, BoardSide::Bottom]);
        assert_eq!(
            viewport,
            BBox::new(Point::new(-1.0, -1.0), Point::new(26.0, 7.0))
        );
        let names = layers.iter().map(|layer| layer.0.as_str());
        assert_eq!(names.clone().count(), 12);
        assert!(names.clone().take(6).eq(names.skip(6)));

        // The cutout spans x 7..8 of the top. Turned over it spans 2..3, and
        // the bottom starts a board and a gap to the right, at 15.
        let material = |layer: usize, x: f64| {
            let probe = BBox::new(Point::new(x + 0.1, 0.6), Point::new(x + 0.9, 1.4));
            let probe = ContourSet::rectangle(probe, Resolution::default());
            layers[layer].1.intersection(&probe).unwrap().area() > 0.5
        };
        let (top, bottom) = (0, 6);
        assert!(!material(top, 7.0) && material(top, 2.0));
        assert!(!material(bottom, 15.0 + 2.0) && material(bottom, 15.0 + 7.0));
        assert!(!material(top, 15.0 + 7.0) && !material(bottom, 2.0));
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
