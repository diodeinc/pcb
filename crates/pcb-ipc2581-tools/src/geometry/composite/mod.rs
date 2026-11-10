//! A design's board as it looks from either side, drawn by
//! [`pcb_ir::render::composite`] from the layers the design images.

use anyhow::{Context, Result, anyhow};
use ipc2581::Symbol;
use pcb_ir::dialects::artwork::{Geometry, Object};
use pcb_ir::dialects::ipc::{ArtworkScope, ProfileSet, profile_occurrences_for};
use pcb_ir::dialects::{LayerRole, Side};
use pcb_ir::geom::{
    ContourBuf, ContourSet, LineCap, Paint, PathCmd, Polarity, Resolution, StrokeStyle,
};
use pcb_ir::import::ipc2581::{ImportedDesign, LayerId};
use pcb_ir::render::composite::{
    Composite, CompositeStyle, Fabrication, Face, SurfaceMap, Thickness,
};

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
    let scope = match board {
        true => ArtworkScope::Board,
        false => ArtworkScope::ArrayFlattened,
    };
    let drills = imported.surface_drills(scope, outer_copper(imported)?, resolution)?;
    let faces = sides
        .iter()
        .map(|&side| {
            let drilled = &drills[(side == BoardSide::Bottom) as usize];
            face(
                &mut composite,
                imported,
                accessor,
                side,
                board,
                drilled,
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
    composite: &mut Composite<Option<Symbol>>,
    imported: &ImportedDesign,
    accessor: &IpcAccessor<'_>,
    side: BoardSide,
    board: bool,
    drilled: &ContourSet,
    resolution: Resolution,
) -> Result<Face<Option<Symbol>>> {
    let root = root_step(imported, board)?;
    let artwork = &mut composite.artwork;
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
    let surface = first_layer(imported, LayerRole::Copper, side.ir_side())
        .with_context(|| format!("IPC-2581 design has no {side} copper layer"))?;
    // The copper's slots are among the drills.
    let [copper, _] = lower(&|layer| layer.name == surface)?
        .into_iter()
        .next()
        .context("the outer copper layer was just found")?;
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
        cutouts: vec![composite.region(Polarity::Clear, drilled.to_contours())],
    })
}

/// The top and bottom outer copper layers; a board with one has it on both.
fn outer_copper(imported: &ImportedDesign) -> Result<[Symbol; 2]> {
    let [top, bottom] =
        [Side::Top, Side::Bottom].map(|side| first_layer(imported, LayerRole::Copper, side));
    top.or(bottom)
        .zip(bottom.or(top))
        .map(|(top, bottom)| [top, bottom])
        .context("IPC-2581 design has no outer copper layer")
}

/// The first layer of `role` on `side`: for copper, the side's outer copper.
fn first_layer(imported: &ImportedDesign, role: LayerRole, side: Side) -> Option<Symbol> {
    let mut layers = imported.layer_definitions.iter();
    layers
        .find(|layer| layer_role(layer.layer_function) == role && ir_side(layer.side) == side)
        .map(|layer| layer.name)
}

/// One side of the board alone: the face [`composite_artwork`] draws, holes
/// open as [`ImportedDesign::surface_openings`] leaves them, and the
/// thicknesses the stackup gives.
pub fn surface_map(
    imported: &ImportedDesign,
    accessor: &IpcAccessor<'_>,
    side: BoardSide,
    resolution: Resolution,
) -> Result<SurfaceMap> {
    let copper = outer_copper(imported)?;
    let index = (side == BoardSide::Bottom) as usize;
    let open = &imported.surface_openings(ArtworkScope::Board, copper, resolution)?[index];
    let drilled = &imported.surface_drills(ArtworkScope::Board, copper, resolution)?[index];
    let mut composite = Composite::new();
    let material = composite
        .material(profile_contours(imported, true))
        .context("IPC-2581 design has no profile to map a board from")?;
    let face = face(
        &mut composite,
        imported,
        accessor,
        side,
        true,
        drilled,
        resolution,
    )?;
    // A stated thickness of zero says nothing.
    let stackup = accessor.stackup_details();
    let thickness = |role| {
        let name = imported.resolve(first_layer(imported, role, side.ir_side())?);
        let layers = &stackup.as_ref()?.layers;
        let layer = layers.iter().find(|layer| layer.name == name)?;
        layer.thickness_mm.filter(|mm| *mm > 0.0)
    };
    let thickness = Thickness {
        copper: thickness(LayerRole::Copper),
        mask: thickness(LayerRole::Soldermask),
        legend: thickness(LayerRole::Legend),
    };
    composite.surface_map(&material, face, open, thickness, resolution)
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
    use pcb_ir::render::composite::Surface;

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

    const HOLE_MM2: f64 = std::f64::consts::PI * 0.25;
    /// Board material: the profile less its cutout and the hole.
    const MATERIAL_MM2: f64 = 60.0 - 1.0 - HOLE_MM2;

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
        assert!((substrate(BoardSide::Top) - (MATERIAL_MM2 + HOLE_MM2)).abs() < 1e-2);
        assert!((substrate(BoardSide::Bottom) - MATERIAL_MM2).abs() < 1e-2);
        // The surface openings read the span the same way.
        let [top, bottom] = surface_maps_of(&blind);
        assert_eq!(area(&top, Surface::Hole), 0.0);
        assert!((area(&bottom, Surface::Hole) - HOLE_MM2).abs() < 1e-2);

        // A rout layer with no span cuts through, drawn and mapped alike.
        let layer = r#"<Layer name="Rout" layerFunction="ROUT" side="ALL" polarity="POSITIVE"/>"#;
        let feature = r#"<LayerFeature layerRef="Rout"><Set>
            <SlotCavity name="S1" platingStatus="NONPLATED" plusTol="0" minusTol="0">
              <Location x="5.5" y="5"/><Oval width="2" height="0.5"/>
            </SlotCavity>
          </Set></LayerFeature>"#;
        let routed = BOARD
            .replace("<Stackup ", &format!("{layer}<Stackup "))
            .replace("</Step>", &format!("{feature}</Step>"));
        let slot = 1.5 * 0.5 + std::f64::consts::PI * 0.0625;
        for (side, map) in [BoardSide::Top, BoardSide::Bottom]
            .into_iter()
            .zip(surface_maps_of(&routed))
        {
            assert!((layer_areas(&routed, side)[0].1 - (MATERIAL_MM2 - slot)).abs() < 1e-2);
            assert!((area(&map, Surface::Hole) - (HOLE_MM2 + slot)).abs() < 1e-2);
        }
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

    /// [`BOARD`] with a 20 µm legend, a second legend mark on bare mask over
    /// (4..6, 0.5..1.5), and three 0.4 mm vias: one in a 2 x 2 mm top mask
    /// opening of its own, one under the top mask and one capped.
    fn with_vias() -> String {
        let silk = r#"layerOrGroupRef="F.Silk" thickness="0""#;
        let mark = r#"<Pad padstackDefRef="mark">"#;
        let bare_mark = r#"<Pad padstackDefRef="mark"><Location x="5" y="1"/></Pad>"#;
        let hole = r#"<Hole name="H1" diameter="1" platingStatus="NONPLATED" x="8" y="4"/>"#;
        let vias = r#"
            <Hole name="V1" diameter="0.4" platingStatus="VIA" x="6.2" y="4.6"/>
            <Hole name="V2" diameter="0.4" platingStatus="VIA" x="2" y="5"/>
            <Hole name="V3" diameter="0.4" platingStatus="VIA_CAPPED" x="4.5" y="5.2"/>"#;
        let opening = r#"<Pad padstackDefRef="opening">"#;
        let via_opening = r#"<Pad padstackDefRef="opening"><Location x="6.2" y="4.6"/></Pad>"#;
        BOARD
            .replace(silk, r#"layerOrGroupRef="F.Silk" thickness="0.02""#)
            .replace(mark, &format!("{bare_mark}{mark}"))
            .replace(hole, &format!("{hole}{vias}"))
            .replace(opening, &format!("{via_opening}{opening}"))
    }

    const VIA_MM2: f64 = std::f64::consts::PI * 0.04;
    const PROFILE_MM2: f64 = 60.0 - 1.0;

    fn surface_maps_of(board: &str) -> [SurfaceMap; 2] {
        let ipc = ipc2581::Ipc2581::parse(board).unwrap();
        let resolution = Resolution::default();
        let imported = pcb_ir::import::ipc2581::import_design(&ipc, resolution).unwrap();
        let accessor = IpcAccessor::new(&ipc);
        [BoardSide::Top, BoardSide::Bottom]
            .map(|side| surface_map(&imported, &accessor, side, resolution).unwrap())
    }

    fn area(map: &SurfaceMap, surface: Surface) -> f64 {
        let regions = map.regions.iter().filter(|(each, _)| *each == surface);
        regions.map(|(_, region)| region.area()).sum()
    }

    /// Each surface's area, 0 where not listed, and that together they cover
    /// the board once.
    fn assert_surfaces(map: &SurfaceMap, expected: &[(Surface, f64)]) {
        for &(surface, expected) in expected {
            // Holes image as polygons inside their circles.
            let actual = area(map, surface);
            assert!(
                (actual - expected).abs() < 2e-2,
                "{surface:?} covers {actual} mm², not {expected}"
            );
        }
        let regions = map.regions.iter().map(|(_, region)| region.clone());
        let union = ContourSet::union_all(Resolution::default(), regions).unwrap();
        let sum: f64 = map.regions.iter().map(|(_, region)| region.area()).sum();
        assert!((union.area() - PROFILE_MM2).abs() < 1e-2 && (sum - union.area()).abs() < 1e-2);
    }

    #[test]
    fn a_surface_map_classifies_the_composite_and_opens_only_untented_uncapped_vias() {
        use Surface::*;
        let [top, bottom] = surface_maps_of(&with_vias());
        assert_surfaces(
            &top,
            &[
                // The first pad's left half in its opening; the rest of it
                // and the second pad's 5 mm² on the board under the mask,
                // less the half legend mark the opening leaves.
                (Finish, 4.0),
                (MaskOverCopper, 4.0 + 5.0 - 1.0),
                (LegendOverCopper, 1.0),
                (LegendOverLaminate, 2.0),
                (BareLaminate, 4.0 - VIA_MM2),
                (Hole, HOLE_MM2 + VIA_MM2),
                (CoveredHole, 2.0 * VIA_MM2),
                (
                    MaskOverLaminate,
                    PROFILE_MM2 - 19.0 - HOLE_MM2 - 2.0 * VIA_MM2,
                ),
            ],
        );
        // A mask layer with no artwork masks the bottom but proves no via
        // tented.
        assert_surfaces(
            &bottom,
            &[
                (Hole, HOLE_MM2 + 2.0 * VIA_MM2),
                (CoveredHole, VIA_MM2),
                (MaskOverLaminate, PROFILE_MM2 - HOLE_MM2 - 3.0 * VIA_MM2),
            ],
        );
        // 35 µm copper, 10 µm mask and 20 µm legend; the bottom's legend of
        // no thickness says nothing.
        let height = |surface: Surface| surface.height_mm(top.thickness);
        assert_eq!((height(Finish), height(Hole)), (Some(0.035), None));
        assert!((height(LegendOverCopper).unwrap() - 0.065).abs() < 1e-12);
        assert_eq!(bottom.thickness.legend, None);
    }

    #[test]
    fn a_side_without_a_mask_layer_or_with_lost_artwork_is_unknown_but_for_its_holes() {
        use Surface::*;
        let top_mask = r#"layerFunction="SOLDERMASK" side="TOP""#;
        let bare = with_vias().replace(top_mask, r#"layerFunction="DOCUMENT" side="TOP""#);
        // No mask tents the via.
        assert_surfaces(
            &surface_maps_of(&bare)[0],
            &[
                (Hole, HOLE_MM2 + 2.0 * VIA_MM2),
                (CoveredHole, VIA_MM2),
                (Unknown, PROFILE_MM2 - HOLE_MM2 - 3.0 * VIA_MM2),
            ],
        );
        // The legend mark has no shape on its layer and is skipped.
        let mark = r#"<PadstackPadDef layerRef="F.Silk" padUse="REGULAR">"#;
        let lost = BOARD.replace(mark, r#"<PadstackPadDef layerRef="User" padUse="REGULAR">"#);
        assert_surfaces(
            &surface_maps_of(&lost)[0],
            &[(Hole, HOLE_MM2), (Unknown, PROFILE_MM2 - HOLE_MM2)],
        );
    }
}
