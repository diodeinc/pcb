//! A board as it looks from one side: the physical layers a viewer sees,
//! stacked in the order they are built up.
//!
//! The drawing is ordinary artwork. Every layer is sequential paint over the
//! same tables, so what a layer lacks is said with clears: the laminate is
//! the profile less everything cut through it, the mask is the laminate less
//! its openings, and the legend is cut where the mask opens. Nothing under
//! the outer copper can be seen from outside and none of it is drawn.

use anyhow::{Context, Result, bail};
use ipc2581::Symbol;
use pcb_ir::dialects::artwork::{self, Geometry, Object, PaintStage};
use pcb_ir::dialects::ipc::{ArtworkScope, ProfileSet, profile_occurrences_for};
use pcb_ir::dialects::{LayerRole, Side};
use pcb_ir::geom::{
    ContourBuf, FillRule, LineCap, Paint, PathCmd, Polarity, Resolution, StrokeStyle,
};
use pcb_ir::import::ipc2581::{ImportedDesign, LayerId};
use pcb_ir::render::LayerStyle;

use crate::geometry::render::layer_objects;
use crate::geometry::step_artwork::{finish_step_graph_artwork, root_step};
use crate::layers::{ir_side, layer_role};

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
    /// The default look in the colours the design's stackup specifies: its
    /// mask, its legend ink and its surface finish.
    pub fn of_stackup(stackup: Option<&crate::accessors::StackupDetails>) -> Self {
        let rgb = |(red, green, blue): (u8, u8, u8)| u32::from_be_bytes([0, red, green, blue]);
        let color = |info: &crate::accessors::ColorInfo| info.rgb_color().map(rgb);
        let mask_ink = |info: &crate::accessors::ColorInfo| {
            let name = info.name.as_deref()?.to_lowercase();
            let ink = MASK_INKS.iter().find(|(ink, _)| *ink == name)?;
            Some(ink.1)
        };
        let mut style = Self::default();
        let Some(stackup) = stackup else {
            return style;
        };
        let mask = stackup.soldermask_color.as_ref();
        if let Some(mask) = mask.and_then(|mask| mask_ink(mask).or_else(|| color(mask))) {
            style.mask.color = mask;
        }
        if let Some(legend) = stackup.silkscreen_color.as_ref().and_then(color) {
            style.legend.color = legend;
        }
        if let Some(finish) = &stackup.surface_finish {
            style.finish.color = rgb(finish.rgb_color());
        }
        style
    }
}

/// A composite drawing and the style of each of its layers, by index.
pub struct Composite {
    pub artwork: CompositeDocument,
    pub styles: Vec<LayerStyle>,
    /// Whether the side is seen from behind the document's frame, as the
    /// bottom is: the board turned over about its vertical axis.
    pub mirrored: bool,
}

/// Width a V-score groove opens to at the surface.
const SCORE_GROOVE_WIDTH_MM: f64 = 0.4;

/// Draw the `side` of the board or array `view` selects.
pub fn composite_artwork(
    imported: &ImportedDesign,
    side: Side,
    view: ArtworkScope,
    style: &CompositeStyle,
    resolution: Resolution,
) -> Result<Composite> {
    let board = match view {
        ArtworkScope::Board => true,
        ArtworkScope::ArrayFlattened => false,
        ArtworkScope::ArrayLocal | ArtworkScope::ArraySupport => {
            bail!("a composite view draws a board or its whole array")
        }
    };
    if !matches!(side, Side::Top | Side::Bottom) {
        bail!("a composite view looks at the top or the bottom of the board");
    }
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
            layer_role(layer.layer_function) == role && ir_side(layer.side) == side
        }
    };
    let outer_copper = imported
        .layer_definitions
        .iter()
        .find(|layer| on_side(LayerRole::Copper)(layer))
        .with_context(|| format!("IPC-2581 design has no {side:?} copper layer"))?
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
    })?
    .into_iter()
    .flatten()
    .flatten();
    let cutouts = slots.into_iter().chain(holes).collect::<Vec<_>>();
    let masks = lower(&on_side(LayerRole::Soldermask))?;
    let legend = lower(&on_side(LayerRole::Legend))?
        .into_iter()
        .flat_map(|[painted, _]| painted)
        .collect::<Vec<_>>();
    // A mask layer images its openings, so they clear whatever they paint.
    let openings = masks
        .iter()
        .flat_map(|[painted, _]| painted)
        .map(|opening| CompositeObject {
            polarity: Polarity::Clear.compose(opening.polarity),
            ..opening.clone()
        })
        .collect::<Vec<_>>();

    // The material left standing is the profile less what is cut through it:
    // what an array's fabrication routs away, and every hole and slot.
    let [body, removal] = body_objects(&mut artwork, imported, view, resolution)?;
    // A cut clears on every layer, painted or not. Left a final cutout it
    // would image as itself wherever a layer paints nothing under it.
    let cuts = removal
        .into_iter()
        .chain(cutouts)
        .map(|mut cut| {
            cut.polarity = Polarity::Clear;
            cut.order.stage = PaintStage::Overlay;
            cut
        })
        .collect::<Vec<_>>();
    let scores = if board {
        Vec::new()
    } else {
        score_objects(&mut artwork, imported)?
    };

    let mut composite = Composite {
        artwork,
        styles: Vec::new(),
        mirrored: side == Side::Bottom,
    };
    composite.layer("Substrate", style.substrate, [&body, &cuts]);
    composite.layer("Finish", style.finish, [&copper, &cuts]);
    // Without a mask layer the board has no mask: all its copper is open.
    if !masks.is_empty() {
        composite.layer("Copper", style.copper, [&copper, &openings, &cuts]);
        composite.layer("Mask", style.mask, [&body, &openings, &cuts]);
    }
    // Mask openings cut the legend, as a fabricator clips it off the pads.
    if !legend.is_empty() {
        composite.layer("Legend", style.legend, [&legend, &openings, &cuts]);
    }
    if !scores.is_empty() {
        composite.layer("Score", style.score, [&scores]);
    }

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

/// The root profile filled, and the clear of what an array's fabrication
/// removes from it: board cutouts and V-score reliefs.
fn body_objects(
    artwork: &mut CompositeDocument,
    imported: &ImportedDesign,
    view: ArtworkScope,
    resolution: Resolution,
) -> Result<[Vec<CompositeObject>; 2]> {
    let geometry = &imported.geometry;
    let fill = Paint::Fill {
        rule: FillRule::EvenOdd,
    };
    let profile_set = match view {
        ArtworkScope::Board => ProfileSet::BoardOutlines,
        _ => ProfileSet::RootOnly,
    };
    let mut region = |polarity: Polarity, contours: Vec<ContourBuf>| {
        let path = artwork.push_path(fill, contours);
        let mut object = CompositeObject::new(polarity, Geometry::Region { path });
        object.order.stage = PaintStage::Base;
        object
    };
    let body = profile_occurrences_for(geometry, profile_set)
        .into_iter()
        .map(|occurrence| {
            let cutouts = occurrence.profile.cutouts.slice(&geometry.profile_cutouts);
            let contours = std::iter::once(occurrence.profile.outer_path)
                .chain(cutouts.iter().map(|cutout| cutout.path))
                .flat_map(|path| geometry.transformed_path_contours(path, occurrence.transform))
                .collect();
            region(Polarity::Dark, contours)
        })
        .collect::<Vec<_>>();
    if body.is_empty() {
        bail!("IPC-2581 design has no profile to draw a board from");
    }
    if view == ArtworkScope::Board {
        return Ok([body, Vec::new()]);
    }
    let score_lines = crate::geometry::board_array_vscore_lines(imported)?;
    let removal = crate::geometry::board_array_fabrication_profile(
        imported,
        geometry,
        &score_lines,
        resolution,
    )?
    .material_removal;
    let removal = (!removal.is_empty()).then(|| region(Polarity::Clear, removal));
    Ok([body, removal.into_iter().collect()])
}

/// The array's V-score lines as the grooves they leave in the surface.
fn score_objects(
    artwork: &mut CompositeDocument,
    imported: &ImportedDesign,
) -> Result<Vec<CompositeObject>> {
    let groove = Paint::Stroke(StrokeStyle::new(SCORE_GROOVE_WIDTH_MM, LineCap::Butt));
    Ok(crate::geometry::board_array_vscore_lines(imported)?
        .into_iter()
        .map(|line| {
            let contour = ContourBuf::new(vec![
                PathCmd::move_to(line.start),
                PathCmd::line_to(line.end),
            ]);
            let path = artwork.push_path(groove, [contour]);
            CompositeObject::new(Polarity::Dark, Geometry::Stroke { path })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 10 x 6 mm board: a 4 x 2 mm pad whose left half the mask opens, a
    /// legend mark half over that opening, and a drilled hole.
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
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="F.Silk" layerFunction="SILKSCREEN" side="TOP" polarity="POSITIVE"/>
      <Layer name="F.Mask" layerFunction="SOLDERMASK" side="TOP" polarity="POSITIVE"/>
      <Layer name="F.Cu" layerFunction="CONDUCTOR" side="TOP" polarity="POSITIVE"/>
      <Layer name="B.Cu" layerFunction="CONDUCTOR" side="BOTTOM" polarity="POSITIVE"/>
      <Layer name="User" layerFunction="DOCUMENT" side="TOP" polarity="POSITIVE"/>
      <Layer name="Drill" layerFunction="DRILL" side="ALL" polarity="POSITIVE">
        <Span fromLayer="F.Cu" toLayer="B.Cu"/>
      </Layer>
      <Step name="board" type="BOARD">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="10" y="0"/>
            <PolyStepSegment x="10" y="6"/>
            <PolyStepSegment x="0" y="6"/>
          </Polygon>
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
          </Set>
        </LayerFeature>
        <LayerFeature layerRef="User">
          <Set>
            <Pad padstackDefRef="note">
              <Location x="7" y="1"/>
            </Pad>
          </Set>
        </LayerFeature>
        <LayerFeature layerRef="Drill">
          <Set>
            <Hole name="H1" diameter="1" platingStatus="NONPLATED" x="8" y="3"/>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#;

    /// Each layer's name and the area it paints, in paint order.
    fn layer_areas(side: Side, style: &CompositeStyle) -> Vec<(String, f64)> {
        let ipc = ipc2581::Ipc2581::parse(BOARD).unwrap();
        let resolution = Resolution::default();
        let imported = pcb_ir::import::ipc2581::import_design(&ipc, resolution).unwrap();
        let composite =
            composite_artwork(&imported, side, ArtworkScope::Board, style, resolution).unwrap();
        assert_eq!(composite.styles.len(), composite.artwork.layers.len());
        assert_eq!(composite.mirrored, side == Side::Bottom);
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
    fn the_top_stacks_finish_in_openings_under_a_mask_the_hole_cuts_through() {
        let hole = std::f64::consts::PI * 0.25;
        assert_areas(
            &layer_areas(Side::Top, &CompositeStyle::default()),
            &[
                ("Substrate", 60.0 - hole),
                ("Finish", 8.0),
                // The opening uncovers the pad's left half and cuts the half
                // of the legend mark printed over it.
                ("Copper", 4.0),
                ("Mask", 60.0 - 4.0 - hole),
                ("Legend", 1.0),
            ],
        );
    }

    #[test]
    fn a_stackup_names_the_mask_ink_and_the_legend() {
        use crate::accessors::{ColorInfo, StackupDetails};
        let named = |name: &str| {
            Some(ColorInfo {
                name: Some(name.to_string()),
                rgb: None,
            })
        };
        let stackup = |mask: &str, legend: &str| StackupDetails {
            name: String::new(),
            overall_thickness_mm: None,
            layer_count: 0,
            layers: Vec::new(),
            soldermask_color: named(mask),
            silkscreen_color: named(legend),
            surface_finish: None,
            outer_copper_oz: None,
            inner_copper_oz: None,
        };
        let default = CompositeStyle::default();

        let style = CompositeStyle::of_stackup(Some(&stackup("Blue", "Black")));
        assert_eq!(style.mask, super::style(0x0a2260, default.mask.opacity));
        assert_eq!(style.legend.color, 0x000000);
        assert_eq!(style.finish, default.finish);
        // A name that is no mask ink falls to the colour it resolves to, and
        // one that resolves to nothing leaves the default.
        let orange = CompositeStyle::of_stackup(Some(&stackup("Orange", "White")));
        assert_eq!(orange.mask.color, 0xff8c00);
        let unknown = CompositeStyle::of_stackup(Some(&stackup("Chartreuse", "White")));
        assert_eq!(unknown, default);
        assert_eq!(CompositeStyle::of_stackup(None), default);
    }

    #[test]
    fn a_side_without_a_mask_layer_is_bare_and_still_drilled() {
        let hole = std::f64::consts::PI * 0.25;
        assert_areas(
            &layer_areas(Side::Bottom, &CompositeStyle::default()),
            &[("Substrate", 60.0 - hole), ("Finish", 0.0)],
        );
    }
}
