pub mod dxf;
pub mod render;

use anyhow::{Context, Result, bail};
use ipc2581::{Symbol, types::LayerFunction};
use pcb_ir::dialects::ipc::{
    ArtworkScope, BoardArrayFabricationProfile, BoardArrayReliefFeatures, Feature, FeatureBucket,
    FeatureDomain, FeatureKind, PlatingKind,
    relief::{
        DEFAULT_RELIEF_TOLERANCE_MM, DEFAULT_SCORE_ALIGNMENT_TOLERANCE_MM, VScoreLine,
        vscore_lines_for,
    },
};
use pcb_ir::geom::Resolution;
use pcb_ir::geom::{BBox, ContourBuf, ContourSet, Point, Polarity};
use pcb_ir::import::ipc2581::{ImportedDesign, LayerId};

pub use pcb_ir::import::ipc2581::{extract_layer, extract_layer_for_view, extract_layout};
pub(crate) use pcb_ir::import::ipc2581::{is_panel_step, step_repeat_transform};

pub(crate) type GeometryDocument =
    pcb_ir::dialects::ipc::Document<ipc2581::Symbol, ipc2581::types::LayerFunction>;

/// V-score centerlines per scoring layer (`VCut` and `Score` functions) for
/// the given artwork scope.
pub fn vscore_lines(
    imported: &ImportedDesign,
    scope: ArtworkScope,
) -> Result<Vec<(ipc2581::Symbol, LayerFunction, VScoreLine)>> {
    let mut lines = Vec::new();
    for (layer_index, source_layer) in
        imported
            .layer_definitions
            .iter()
            .enumerate()
            .filter(|(_, layer)| {
                matches!(
                    layer.layer_function,
                    LayerFunction::VCut | LayerFunction::Score
                )
            })
    {
        let layer_name = imported.resolve(source_layer.name);
        let doc = imported
            .materialize_layer(LayerId(layer_index as u32), scope)
            .with_context(|| format!("failed to extract IPC-2581 V-score layer '{layer_name}'"))?;
        lines.extend(
            vscore_lines_for(&doc)
                .into_iter()
                .map(|line| (source_layer.name, source_layer.layer_function, line)),
        );
    }
    Ok(lines)
}

pub fn board_array_vscore_lines(imported: &ImportedDesign) -> Result<Vec<VScoreLine>> {
    Ok(vscore_lines(imported, ArtworkScope::ArrayFlattened)?
        .into_iter()
        .map(|(_, _, line)| line)
        .collect())
}

pub fn board_array_fabrication_profile(
    imported: &ImportedDesign,
    layout: &GeometryDocument,
    score_lines: &[VScoreLine],
    resolution: Resolution,
) -> Result<BoardArrayFabricationProfile> {
    let (profile, _) =
        board_array_fabrication_profile_with_debug(imported, layout, score_lines, resolution)?;
    Ok(profile)
}

pub fn board_array_fabrication_profile_with_debug(
    imported: &ImportedDesign,
    layout: &GeometryDocument,
    score_lines: &[VScoreLine],
    resolution: Resolution,
) -> Result<(
    BoardArrayFabricationProfile,
    pcb_ir::dialects::ipc::relief::VScoreReliefDebug,
)> {
    let relief_features = board_array_relief_features(imported, score_lines, resolution)?;
    Ok(pcb_ir::dialects::ipc::board_array_fabrication_profile(
        layout,
        score_lines,
        pcb_ir::dialects::ipc::FabricationProfileOptions {
            relief_features,
            debug: true,
        },
        resolution,
    )?)
}

fn board_array_relief_features(
    imported: &ImportedDesign,
    score_lines: &[VScoreLine],
    resolution: Resolution,
) -> Result<BoardArrayReliefFeatures> {
    if score_lines.is_empty() {
        return Ok(BoardArrayReliefFeatures::default());
    }

    let resolution = resolution.with_tolerance(DEFAULT_RELIEF_TOLERANCE_MM);
    let (cutouts, envelopes) = collect_relief_feature_candidates(imported)?;
    let strips = score_lines
        .iter()
        .map(|line| score_line_strip(*line, resolution))
        .collect::<Vec<_>>();
    // Regions are prepared only for candidates near a score line: on a dense
    // panel almost every hole is nowhere near one.
    let near_score_line = |candidate: &ReliefFeatureCandidate| {
        strips
            .iter()
            .any(|strip| candidate.bbox.intersects(strip.bbox))
    };
    let mut crossing = Vec::new();
    for cutout in cutouts.into_iter().filter(near_score_line) {
        let region = cutout.region(resolution)?;
        if strips
            .iter()
            .any(|strip| !region.intersection(strip).is_empty())
        {
            crossing.push((cutout, region));
        }
    }
    let envelopes = envelopes
        .into_iter()
        .filter(|envelope| {
            crossing
                .iter()
                .any(|(cutout, _)| envelope.bbox.intersects(cutout.bbox))
        })
        .map(|envelope| Ok((envelope.region(resolution)?, envelope)))
        .collect::<Result<Vec<_>>>()?;
    // Every blocker joins one batched union: unioning them one at a time is
    // quadratic in the number of cutouts on dense panels.
    let mut blockers = Vec::new();
    for (cutout, region) in crossing {
        if plated_like(cutout.plating) {
            let matches = envelopes
                .iter()
                .filter(|(envelope_region, envelope)| {
                    envelope_matches_cutout(envelope, envelope_region, &cutout, &region)
                })
                .map(|(envelope_region, _)| envelope_region.clone())
                .collect::<Vec<_>>();
            if matches.is_empty() {
                bail!(
                    "plated edge cutout at [{:.3}, {:.3}]..[{:.3}, {:.3}] has no matching pad envelope for V-score relief generation",
                    cutout.bbox.min.x,
                    cutout.bbox.min.y,
                    cutout.bbox.max.x,
                    cutout.bbox.max.y
                );
            }
            blockers.extend(matches);
        } else {
            blockers.push(region);
        }
    }
    let score_blockers = ContourSet::union_all(resolution, blockers)?;

    Ok(BoardArrayReliefFeatures {
        score_blockers: score_blockers.to_contours(),
    })
}

fn collect_relief_feature_candidates(
    imported: &ImportedDesign,
) -> Result<(Vec<ReliefFeatureCandidate>, Vec<ReliefFeatureCandidate>)> {
    let mut cutouts = Vec::new();
    let mut envelopes = Vec::new();

    for (layer_index, layer) in imported
        .layer_definitions
        .iter()
        .enumerate()
        .filter(|(_, layer)| relief_feature_layer(layer.layer_function))
    {
        let layer_name = imported.resolve(layer.name);
        let doc = imported
            .materialize_layer(LayerId(layer_index as u32), ArtworkScope::ArrayFlattened)
            .with_context(|| format!("failed to extract IPC-2581 layer '{layer_name}'"))?;
        for feature in &doc.features {
            if is_through_cutout(feature) {
                cutouts.push(ReliefFeatureCandidate::new(&doc, feature));
            } else if is_pad_envelope(feature) {
                envelopes.push(ReliefFeatureCandidate::new(&doc, feature));
            }
        }
    }

    Ok((cutouts, envelopes))
}

#[derive(Debug, Clone)]
struct ReliefFeatureCandidate {
    contours: Vec<ContourBuf>,
    bbox: BBox,
    plating: PlatingKind,
    padstack_ref: Option<Symbol>,
    net: Option<Symbol>,
}

impl ReliefFeatureCandidate {
    fn new(doc: &GeometryDocument, feature: &Feature<Symbol>) -> Self {
        Self {
            contours: doc.placed_feature_contours(feature),
            bbox: feature.bbox,
            plating: feature.intent.plating,
            padstack_ref: feature.padstack_ref,
            net: feature.net,
        }
    }

    fn region(&self, resolution: Resolution) -> Result<ContourSet> {
        Ok(ContourSet::from_filled_contours(
            &self.contours,
            resolution,
        )?)
    }
}

fn relief_feature_layer(layer_function: LayerFunction) -> bool {
    matches!(layer_function, LayerFunction::Drill | LayerFunction::Rout)
        || crate::layers::is_copper(layer_function)
}

fn is_through_cutout(feature: &Feature<Symbol>) -> bool {
    matches!(feature.kind, FeatureKind::Hole | FeatureKind::Slot)
        && feature.bucket == FeatureBucket::Cutout
        && matches!(
            feature.intent.plating,
            PlatingKind::Plated
                | PlatingKind::NonPlated
                | PlatingKind::Via
                | PlatingKind::ViaCapped
        )
}

fn is_pad_envelope(feature: &Feature<Symbol>) -> bool {
    feature.kind == FeatureKind::Padstack
        && feature.polarity == Polarity::Dark
        && feature.intent.domain == FeatureDomain::Copper
}

fn plated_like(plating: PlatingKind) -> bool {
    matches!(
        plating,
        PlatingKind::Plated | PlatingKind::Via | PlatingKind::ViaCapped
    )
}

fn score_line_strip(line: VScoreLine, resolution: Resolution) -> ContourSet {
    let width = DEFAULT_SCORE_ALIGNMENT_TOLERANCE_MM.max(line.width / 2.0);
    let bbox = BBox {
        min: Point::new(
            line.start.x.min(line.end.x) - width,
            line.start.y.min(line.end.y) - width,
        ),
        max: Point::new(
            line.start.x.max(line.end.x) + width,
            line.start.y.max(line.end.y) + width,
        ),
    };
    ContourSet::rectangle(bbox, resolution)
}

fn envelope_matches_cutout(
    envelope: &ReliefFeatureCandidate,
    envelope_region: &ContourSet,
    cutout: &ReliefFeatureCandidate,
    cutout_region: &ContourSet,
) -> bool {
    if !envelope.bbox.intersects(cutout.bbox) {
        return false;
    }
    if let (Some(envelope_net), Some(cutout_net)) = (envelope.net, cutout.net)
        && envelope_net != cutout_net
    {
        return false;
    }
    if let (Some(envelope_padstack), Some(cutout_padstack)) =
        (envelope.padstack_ref, cutout.padstack_ref)
        && envelope_padstack == cutout_padstack
    {
        return true;
    }
    !envelope_region.intersection(cutout_region).is_empty()
}
