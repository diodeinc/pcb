use anyhow::Context;
use ipc2581::Symbol;
use pcb_ir::geom::{GeometryAccuracy, Resolution};
use pcb_ir::render::RenderOptions;

use crate::layers::layer_role;
use ipc2581::types::LayerFunction;
use pcb_ir::dialects::artwork::{Geometry, Object, PaintOrder, PaintStage};
use pcb_ir::dialects::ipc::{
    ArtworkScope, Feature, FeatureBucket, ProfileSet, profile_occurrences_for,
};
use pcb_ir::dialects::{LayerRole, Side};
use pcb_ir::geom::{BBox, Paint, Polarity, Span, StrokeStyle};
use pcb_ir::import::ipc2581::ImportedDesign;

type GeometryDocument = pcb_ir::dialects::ipc::Document<Symbol, LayerFunction>;
type ArtworkDocument = pcb_ir::dialects::artwork::Document<LayerFunction, Option<Symbol>>;

const DISPLAY_PROFILE_STROKE_WIDTH_MM: f64 = 0.1;

/// Materialize and normalize a layer using the same artwork rules as Gerber export.
pub fn prepare_layer(
    imported: &ImportedDesign,
    layer_name: &str,
    view: ArtworkScope,
    resolution: Resolution,
) -> anyhow::Result<GeometryDocument> {
    let layer = imported
        .layer_id(layer_name)
        .with_context(|| format!("IPC-2581 layer '{layer_name}' was not found"))?;
    let mut geometry = imported.materialize_layer(layer, view)?;
    pcb_ir::dialects::ipc::process::normalize_for_artwork(&mut geometry, resolution)?;
    pcb_ir::dialects::ipc::validate_artwork_ready(&geometry)
        .map_err(anyhow::Error::msg)
        .with_context(|| format!("IPC-2581 layer '{layer_name}' is not artwork-ready"))?;
    Ok(geometry)
}

/// Render a prepared layer as SVG, drawing anything the target cannot carry
/// natively within the options' accuracy.
pub fn render_layer_svg(
    geometry: &GeometryDocument,
    include_profiles: bool,
    profile_set: ProfileSet,
    options: &RenderOptions,
) -> anyhow::Result<String> {
    let artwork = layer_artwork(geometry, include_profiles, profile_set)?;
    Ok(pcb_ir::render::artwork_svg(&artwork, options)?)
}

pub fn render_layer_png(
    geometry: &GeometryDocument,
    include_profiles: bool,
    profile_set: ProfileSet,
    accuracy: GeometryAccuracy,
) -> Result<Vec<u8>, String> {
    let artwork = layer_artwork(geometry, include_profiles, profile_set)
        .map_err(|error| error.to_string())?;
    pcb_ir::render::artwork_png(&artwork, &RenderOptions::default().with_accuracy(accuracy))
}

#[cfg(feature = "cli")]
pub fn render_layer_terminal(
    geometry: &GeometryDocument,
    include_profiles: bool,
    profile_set: ProfileSet,
    accuracy: GeometryAccuracy,
) -> Result<(), String> {
    let artwork = layer_artwork(geometry, include_profiles, profile_set)
        .map_err(|error| error.to_string())?;
    pcb_ir::render::artwork_to_terminal(&artwork, &RenderOptions::default().with_accuracy(accuracy))
}

/// Whether a normalized single-layer document paints anything of its own.
///
/// Borrowed features, such as the rout slots every layer in their span
/// carries, do not count. Normalization leaves a feature that set voids or
/// cutouts erased without paths, so a surviving dark feature with painted
/// paths is content. Cutouts image as themselves only where composition
/// lets them: on a non-copper layer holding nothing else, such as a drill
/// layer.
pub fn layer_has_native_content(geometry: &GeometryDocument) -> bool {
    let Some(layer) = geometry.layers.first() else {
        return false;
    };
    let features = layer.features.slice(&geometry.features);
    let paints = |feature: &&Feature<Symbol>| {
        feature
            .paths
            .slice(&geometry.arena.paths)
            .iter()
            .any(|path| path.paint.is_painted() && !path.bbox.is_empty())
    };
    let is_cutout = |feature: &Feature<Symbol>| feature.bucket == FeatureBucket::Cutout;
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

/// Lower a single-layer geometry document to artwork, with the display
/// profile outlines a viewer expects overlaid.
pub fn layer_artwork(
    geometry: &GeometryDocument,
    include_profiles: bool,
    profile_set: ProfileSet,
) -> anyhow::Result<ArtworkDocument> {
    let layer = &geometry.layers[0];
    let mut artwork = pcb_ir::dialects::ipc::lower_layer_to_artwork(
        geometry,
        0,
        layer_role(layer.layer_function),
        Side::None,
    );
    if include_profiles {
        append_display_profiles(&mut artwork, geometry, profile_set, layer.layer_function)?;
    }
    Ok(artwork)
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
            layer_has_native_content(
                &prepare_layer(&imported, layer, ArtworkScope::Board, resolution).unwrap(),
            )
        };

        assert!(native_content("F.Cu"), "a surviving pad is content");
        assert!(!native_content("In1.Cu"), "the slot erases the only pad");
        assert!(!native_content("B.Cu"), "a borrowed slot is not content");
        assert!(native_content("Drill"), "holes image on their own layer");
        assert!(native_content("Rout"), "slots image on their own layer");
    }
}
