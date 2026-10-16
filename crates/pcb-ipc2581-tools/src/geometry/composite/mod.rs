//! A design's board as it looks from either side, drawn by
//! [`pcb_ir::render::composite`] from the layers the design images.

use anyhow::{Context, Result, anyhow};
use ipc2581::Symbol;
use pcb_ir::dialects::artwork::{Geometry, Object};
use pcb_ir::dialects::ipc::{ProfileSet, profile_occurrences_for};
use pcb_ir::dialects::{LayerRole, Side};
use pcb_ir::geom::{ContourBuf, LineCap, Paint, PathCmd, Polarity, Resolution, StrokeStyle};
use pcb_ir::import::ipc2581::{ImportedDesign, LayerId};
use pcb_ir::render::composite::{Composite, CompositeDocument, CompositeStyle, Fabrication, Face};

use crate::accessors::{ColorInfo, IpcAccessor};
use crate::geometry::render::layer_objects;
use crate::geometry::step_artwork::root_step;
use crate::layers::{ir_side, layer_role};
use crate::{BoardSide, LayoutTarget};

pub mod gerber;

type CompositeObject = Object<Option<Symbol>>;

/// Width a V-score groove opens to at the surface.
const SCORE_GROOVE_WIDTH_MM: f64 = 0.4;

/// Draw `sides` of the board or array `target` selects, left to right, each
/// in the inks its stackup gives it. The bottom is drawn as it looks turned
/// over about the board's vertical axis.
pub fn composite_artwork(
    imported: &ImportedDesign,
    accessor: &IpcAccessor<'_>,
    sides: &[BoardSide],
    target: LayoutTarget,
    resolution: Resolution,
) -> Result<Composite<Option<Symbol>>> {
    let board = target == LayoutTarget::Board;
    let mut composite = Composite::new();
    let material = composite
        .material(profile_contours(imported, board))
        .context("IPC-2581 design has no profile to draw a board from")?;
    // An array's fabrication cuts it further and scores it; a board drawn
    // alone has neither, whatever array its file places it in.
    let fabrication = if board {
        Fabrication::default()
    } else {
        array_fabrication(&mut composite, imported, resolution)?
    };
    let faces = sides
        .iter()
        .map(|&side| {
            face(
                &mut composite.artwork,
                imported,
                accessor,
                side,
                board,
                resolution,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    composite
        .draw(&material, &fabrication, faces)
        .map_err(|error| anyhow!("invalid composite artwork: {error}"))?;
    Ok(composite)
}

/// The colours the design's stackup gives `side`: its mask, its legend ink
/// and the board's surface finish. What the stackup leaves unsaid keeps the
/// default.
pub fn design_style(accessor: &IpcAccessor<'_>, side: BoardSide) -> CompositeStyle {
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
    let mask = ink(LayerRole::Soldermask).and_then(mask_color);
    let legend = ink(LayerRole::Legend).and_then(|ink| Some(rgb(ink.rgb_color()?)));
    CompositeStyle::with_colors(mask, legend, finish)
}

/// A mask ink's colour: the cured ink its name stands for, else the colour
/// the stackup resolves it to.
pub(crate) fn mask_color(ink: &ColorInfo) -> Option<u32> {
    ink.name
        .as_deref()
        .and_then(CompositeStyle::mask_ink)
        .or_else(|| Some(rgb(ink.rgb_color()?)))
}

pub(crate) fn rgb((red, green, blue): (u8, u8, u8)) -> u32 {
    u32::from_be_bytes([0, red, green, blue])
}

fn face(
    artwork: &mut CompositeDocument<Option<Symbol>>,
    imported: &ImportedDesign,
    accessor: &IpcAccessor<'_>,
    side: BoardSide,
    board: bool,
    resolution: Resolution,
) -> Result<Face<Option<Symbol>>> {
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
    Ok(Face {
        side: side.ir_side(),
        style: design_style(accessor, side),
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

/// The profile of the board, or of the array and every board in it: each
/// outline and its cutouts.
fn profile_contours(imported: &ImportedDesign, board: bool) -> Vec<ContourBuf> {
    let geometry = &imported.geometry;
    let profile_set = if board {
        ProfileSet::BoardOutlines
    } else {
        ProfileSet::RootOnly
    };
    let occurrences = profile_occurrences_for(geometry, profile_set);
    occurrences
        .iter()
        .flat_map(|occurrence| {
            let cutouts = occurrence.profile.cutouts.slice(&geometry.profile_cutouts);
            std::iter::once(occurrence.profile.outer_path)
                .chain(cutouts.iter().map(|cutout| cutout.path))
                .flat_map(|path| geometry.transformed_path_contours(path, occurrence.transform))
        })
        .collect()
}

/// What an array's fabrication adds to it: the material it routs away, board
/// cutouts and V-score reliefs among it, and its V-score lines as the grooves
/// they leave in the surface.
fn array_fabrication(
    composite: &mut Composite<Option<Symbol>>,
    imported: &ImportedDesign,
    resolution: Resolution,
) -> Result<Fabrication<Option<Symbol>>> {
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
            let path = composite.artwork.push_path(groove, [contour]);
            CompositeObject::new(Polarity::Dark, Geometry::Stroke { path })
        })
        .collect();
    Ok(Fabrication {
        removal: vec![composite.region(Polarity::Clear, removal)],
        scores,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pcb_ir::dialects::artwork;
    use pcb_ir::geom::{BBox, ContourSet, Point};

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

        let top = design_style(&accessor, BoardSide::Top);
        assert_eq!(top.mask.color, 0x0a2260);
        assert_eq!(top.mask.opacity, default.mask.opacity);
        assert_eq!(top.legend.color, 0x000000);
        // A name no table knows leaves the default, as does a finish the
        // stackup does not give.
        let bottom = design_style(&accessor, BoardSide::Bottom);
        assert_eq!(bottom.mask.color, 0x7a0c0c);
        assert_eq!(bottom.legend, default.legend);
        assert_eq!(
            (&top.finish, &bottom.finish),
            (&default.finish, &default.finish)
        );
        assert_eq!(top.substrate, default.substrate);
    }
}
