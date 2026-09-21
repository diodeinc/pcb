use anyhow::Context;
use ipc2581::Symbol;
use pcb_ir::geom::Resolution;

use crate::geometry::step_artwork::{root_step, step_graph_artwork};
use crate::layers::layer_role;
use ipc2581::types::LayerFunction;
use pcb_ir::dialects::artwork::{Geometry, Object, PaintOrder, PaintStage};
use pcb_ir::dialects::ipc::{
    ArtworkScope, Feature, FeatureBucket, NetMetaLowering, ProfileSet,
    lower_layer_to_artwork_objects_with, profile_occurrences_for,
};
use pcb_ir::dialects::{LayerRole, Side};
use pcb_ir::geom::{BBox, Paint, Polarity, Span, StrokeStyle};
use pcb_ir::import::ipc2581::{GeometryDocument, ImportedDesign};

type ArtworkDocument = pcb_ir::dialects::artwork::Document<LayerFunction, Option<Symbol>>;

const DISPLAY_PROFILE_STROKE_WIDTH_MM: f64 = 0.1;

/// One layer as a viewer draws it.
pub struct LayerView {
    pub artwork: ArtworkDocument,
    /// Whether any Step paints content of its own on the layer.
    pub has_native_content: bool,
}

/// Lower one layer to viewer artwork under the same normalization Gerber
/// export uses, with the display profile outlines a viewer expects overlaid.
///
/// Every Step lowers once, so an array draws its board as one shared block.
pub fn layer_artwork(
    imported: &ImportedDesign,
    layer_name: &str,
    view: ArtworkScope,
    include_profiles: bool,
    resolution: Resolution,
) -> anyhow::Result<LayerView> {
    let layer = imported
        .layer_id(layer_name)
        .with_context(|| format!("IPC-2581 layer '{layer_name}' was not found"))?;
    let board = match view {
        ArtworkScope::Board => true,
        ArtworkScope::ArrayFlattened => false,
        ArtworkScope::ArrayLocal | ArtworkScope::ArraySupport => {
            anyhow::bail!("a layer view draws a board or its whole array")
        }
    };
    let layer_function = imported
        .layer_definition(layer)
        .context("layer id is outside the imported design")?
        .layer_function;
    let mut has_native_content = false;
    let mut artwork = step_graph_artwork(
        imported,
        layer,
        root_step(imported, board)?,
        pcb_ir::dialects::artwork::Layer {
            name: layer_name.to_string(),
            role: layer_role(layer_function),
            side: Side::None,
            objects: Span::EMPTY,
            bbox: BBox::empty(),
            meta: layer_function,
        },
        |_, mut local, artwork| {
            pcb_ir::dialects::ipc::process::normalize_for_artwork(&mut local, resolution)?;
            pcb_ir::dialects::ipc::validate_artwork_ready(&local)
                .map_err(anyhow::Error::msg)
                .with_context(|| format!("IPC-2581 layer '{layer_name}' is not artwork-ready"))?;
            has_native_content |= layer_has_native_content(&local);
            Ok(lower_layer_to_artwork_objects_with(
                &local,
                0,
                artwork,
                &mut NetMetaLowering,
            ))
        },
    )?;
    if include_profiles {
        append_display_profiles(
            &mut artwork,
            &imported.geometry,
            view.profile_set(),
            layer_function,
        )?;
    }
    Ok(LayerView {
        artwork,
        has_native_content,
    })
}

/// Whether a normalized single-layer document paints anything of its own.
///
/// Borrowed features, such as the rout slots every layer in their span
/// carries, do not count. Normalization leaves a feature that a set void
/// erased without paths, so a surviving dark feature with painted paths is
/// content; what clears and cutouts later remove from it is for composition
/// to say, not for this check. Cutouts image as themselves only where
/// composition lets them: on a non-copper layer holding nothing else, such
/// as a drill layer.
fn layer_has_native_content(geometry: &GeometryDocument) -> bool {
    let Some(layer) = geometry.layers.first() else {
        return false;
    };
    let features = layer.features.slice(&geometry.features);
    let paints = |feature: &&Feature| {
        feature
            .paths
            .slice(&geometry.arena.paths)
            .iter()
            .any(|path| path.paint.is_painted() && !path.bbox.is_empty())
    };
    let is_cutout = |feature: &Feature| feature.bucket == FeatureBucket::Cutout;
    let cutouts_image = !crate::layers::is_copper(layer.layer_function)
        && features.iter().filter(paints).all(is_cutout);
    features
        .iter()
        .filter(|feature| feature.source_layer_ref == Some(layer.source_layer_ref))
        .filter(paints)
        .any(|feature| {
            if is_cutout(feature) {
                cutouts_image
            } else {
                feature.polarity == Polarity::Dark
            }
        })
}

fn append_display_profiles(
    artwork: &mut ArtworkDocument,
    geometry: &GeometryDocument,
    profile_set: ProfileSet,
    layer_function: LayerFunction,
) -> anyhow::Result<()> {
    let profile_layer = artwork.push_layer(pcb_ir::dialects::artwork::Layer {
        name: "Profile".to_string(),
        role: LayerRole::Profile,
        side: Side::None,
        objects: Span::EMPTY,
        bbox: BBox::empty(),
        meta: layer_function,
    });

    for occurrence in profile_occurrences_for(geometry, profile_set) {
        append_display_profile_path(
            artwork,
            profile_layer,
            geometry,
            occurrence.profile.outer_path,
            occurrence.transform,
        )?;
        for cutout in occurrence.profile.cutouts.slice(&geometry.profile_cutouts) {
            append_display_profile_path(
                artwork,
                profile_layer,
                geometry,
                cutout.path,
                occurrence.transform,
            )?;
        }
    }

    pcb_ir::dialects::artwork::normalize_bounds(artwork);

    Ok(())
}

fn append_display_profile_path(
    artwork: &mut ArtworkDocument,
    layer: u32,
    geometry: &GeometryDocument,
    path: u32,
    transform: pcb_ir::geom::Affine2,
) -> anyhow::Result<()> {
    let path = artwork.push_path(
        Paint::Stroke(StrokeStyle::round(DISPLAY_PROFILE_STROKE_WIDTH_MM)),
        geometry.transformed_path_contours(path, transform),
    );
    artwork.push_object(
        layer,
        Object {
            polarity: Polarity::Dark,
            order: PaintOrder {
                stage: PaintStage::Overlay,
            },
            geometry: Geometry::Stroke { path },
            bbox: artwork.path_bbox(path),
            meta: None,
        },
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_content_ignores_borrowed_and_erased_features() {
        let ipc = ipc2581::Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <DictionaryStandard units="MILLIMETER">
      <EntryStandard id="pad">
        <Circle diameter="0.4"/>
      </EntryStandard>
    </DictionaryStandard>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="F.Cu" layerFunction="CONDUCTOR" side="TOP" polarity="POSITIVE"/>
      <Layer name="In1.Cu" layerFunction="CONDUCTOR" side="INTERNAL" polarity="POSITIVE"/>
      <Layer name="B.Cu" layerFunction="CONDUCTOR" side="BOTTOM" polarity="POSITIVE"/>
      <Layer name="Drill" layerFunction="DRILL" side="ALL" polarity="POSITIVE"/>
      <Layer name="Rout" layerFunction="ROUT" side="ALL" polarity="POSITIVE">
        <Span fromLayer="F.Cu" toLayer="B.Cu"/>
      </Layer>
      <Step name="board" type="BOARD">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="10" y="0"/>
            <PolyStepSegment x="10" y="5"/>
            <PolyStepSegment x="0" y="5"/>
          </Polygon>
        </Profile>
        <PadStackDef name="top">
          <PadstackPadDef layerRef="F.Cu" padUse="REGULAR">
            <StandardPrimitiveRef id="pad"/>
          </PadstackPadDef>
        </PadStackDef>
        <PadStackDef name="inner">
          <PadstackPadDef layerRef="In1.Cu" padUse="REGULAR">
            <StandardPrimitiveRef id="pad"/>
          </PadstackPadDef>
        </PadStackDef>
        <LayerFeature layerRef="F.Cu">
          <Set>
            <Pad padstackDefRef="top">
              <Location x="2" y="3"/>
            </Pad>
          </Set>
        </LayerFeature>
        <LayerFeature layerRef="In1.Cu">
          <Set>
            <Pad padstackDefRef="inner">
              <Location x="7" y="2"/>
            </Pad>
          </Set>
        </LayerFeature>
        <LayerFeature layerRef="Drill">
          <Set>
            <Hole name="H1" diameter="0.8" platingStatus="NONPLATED" x="4" y="4"/>
          </Set>
        </LayerFeature>
        <LayerFeature layerRef="Rout">
          <Set>
            <SlotCavity name="S1" platingStatus="NONPLATED" plusTol="0" minusTol="0">
              <Location x="7" y="2"/>
              <Oval width="3" height="1"/>
            </SlotCavity>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();
        let resolution = Resolution::default();
        let imported = pcb_ir::import::ipc2581::import_design(&ipc, resolution).unwrap();
        let native_content = |layer| {
            layer_artwork(&imported, layer, ArtworkScope::Board, false, resolution)
                .unwrap()
                .has_native_content
        };

        assert!(native_content("F.Cu"), "a surviving pad is content");
        assert!(
            native_content("In1.Cu"),
            "a pad under a slot is still drawn"
        );
        assert!(!native_content("B.Cu"), "a borrowed slot is not content");
        assert!(native_content("Drill"), "holes image on their own layer");
        assert!(native_content("Rout"), "slots image on their own layer");
    }
    #[test]
    fn an_array_view_draws_its_board_once_and_cuts_every_placement() {
        let ipc = ipc2581::Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="array"/>
    <DictionaryStandard units="MILLIMETER">
      <EntryStandard id="pad">
        <RectCenter width="4" height="2"/>
      </EntryStandard>
    </DictionaryStandard>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="F.Cu" layerFunction="CONDUCTOR" side="TOP" polarity="POSITIVE"/>
      <Layer name="B.Cu" layerFunction="CONDUCTOR" side="BOTTOM" polarity="POSITIVE"/>
      <Layer name="Rout" layerFunction="ROUT" side="ALL" polarity="POSITIVE">
        <Span fromLayer="F.Cu" toLayer="B.Cu"/>
      </Layer>
      <Step name="array" type="PALLET">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="40" y="0"/>
            <PolyStepSegment x="40" y="10"/>
            <PolyStepSegment x="0" y="10"/>
          </Polygon>
        </Profile>
        <StepRepeat stepRef="board" x="0" y="0" nx="3" ny="1" dx="12" dy="0" angle="0" mirror="false"/>
      </Step>
      <Step name="board" type="BOARD">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="10" y="0"/>
            <PolyStepSegment x="10" y="5"/>
            <PolyStepSegment x="0" y="5"/>
          </Polygon>
        </Profile>
        <PadStackDef name="top">
          <PadstackPadDef layerRef="F.Cu" padUse="REGULAR">
            <StandardPrimitiveRef id="pad"/>
          </PadstackPadDef>
        </PadStackDef>
        <LayerFeature layerRef="F.Cu">
          <Set>
            <Pad padstackDefRef="top">
              <Location x="5" y="2"/>
            </Pad>
          </Set>
        </LayerFeature>
        <LayerFeature layerRef="Rout">
          <Set>
            <SlotCavity name="S1" platingStatus="NONPLATED" plusTol="0" minusTol="0">
              <Location x="5" y="2"/>
              <RectCenter width="1" height="1"/>
            </SlotCavity>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();
        let resolution = Resolution::default();
        let imported = pcb_ir::import::ipc2581::import_design(&ipc, resolution).unwrap();
        let artwork = layer_artwork(
            &imported,
            "F.Cu",
            ArtworkScope::ArrayFlattened,
            false,
            resolution,
        )
        .unwrap()
        .artwork;

        let stages = artwork.layers[0]
            .objects
            .slice(&artwork.objects)
            .iter()
            .map(|object| {
                assert!(matches!(object.geometry, Geometry::GridInstance { .. }));
                object.order.stage
            })
            .collect::<Vec<_>>();
        assert_eq!(artwork.blocks.len(), 2, "the board's paint and its cutouts");
        assert_eq!(stages.len(), 2);
        assert_eq!(stages[1], PaintStage::FinalCutout);
        assert_ne!(stages[0], PaintStage::FinalCutout);

        let (images, _) =
            pcb_ir::dialects::artwork::compose_owner_regions(&artwork, |_| Some(()), resolution)
                .unwrap();
        let area = images[0].iter().map(|(_, image)| image.area()).sum::<f64>();
        assert!(
            (area - 3.0 * (8.0 - 1.0)).abs() < 1e-6,
            "every placement keeps its pad less its slot, not {area} mm²"
        );
    }
}
