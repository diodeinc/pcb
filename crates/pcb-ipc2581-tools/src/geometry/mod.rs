pub mod dxf;
mod extract;
pub mod render;

use anyhow::{Context, Result, bail};
use ipc2581::{Ipc2581, Symbol, types::LayerFunction};
use pcb_ir::dialects::ipc::{
    BoardArrayFabricationProfile, BoardArrayReliefFeatures, Feature, FeatureBucket, FeatureDomain,
    FeatureKind, PlatingKind, View,
    relief::{
        DEFAULT_RELIEF_TOLERANCE_MM, DEFAULT_SCORE_ALIGNMENT_TOLERANCE_MM, VScoreLine,
        vscore_lines_for,
    },
};
use pcb_ir::geom::{BBox, ContourBuf, ContourSet, Point, Polarity};

pub use extract::{extract_layer, extract_layer_for_view, extract_layout};

type GeometryDocument =
    pcb_ir::dialects::ipc::Document<ipc2581::Symbol, ipc2581::types::LayerFunction>;

pub fn board_array_vscore_lines(ipc: &Ipc2581) -> Result<Vec<VScoreLine>> {
    let ecad = ipc.ecad().context("IPC-2581 file has no ECAD section")?;
    let mut lines = Vec::new();
    for source_layer in ecad
        .cad_data
        .layers
        .iter()
        .filter(|layer| layer.layer_function == LayerFunction::VCut)
    {
        let layer_name = ipc.resolve(source_layer.name);
        let doc = extract_layer_for_view(ipc, layer_name, View::ArrayFlattened)
            .with_context(|| format!("failed to extract IPC-2581 V-cut layer '{layer_name}'"))?;
        lines.extend(vscore_lines_for(&doc));
    }
    Ok(lines)
}

pub fn board_array_fabrication_profile(
    ipc: &Ipc2581,
    layout: &GeometryDocument,
    score_lines: &[VScoreLine],
) -> Result<BoardArrayFabricationProfile> {
    let (profile, _) = board_array_fabrication_profile_with_debug(ipc, layout, score_lines)?;
    Ok(profile)
}

pub fn board_array_fabrication_profile_with_debug(
    ipc: &Ipc2581,
    layout: &GeometryDocument,
    score_lines: &[VScoreLine],
) -> Result<(
    BoardArrayFabricationProfile,
    pcb_ir::dialects::ipc::relief::VScoreReliefDebug,
)> {
    let relief_features = board_array_relief_features(ipc, score_lines)?;
    Ok(pcb_ir::dialects::ipc::board_array_fabrication_profile(
        layout,
        score_lines,
        pcb_ir::dialects::ipc::FabricationProfileOptions {
            relief_features,
            debug: true,
        },
    )?)
}

fn board_array_relief_features(
    ipc: &Ipc2581,
    score_lines: &[VScoreLine],
) -> Result<BoardArrayReliefFeatures> {
    if score_lines.is_empty() {
        return Ok(BoardArrayReliefFeatures::default());
    }

    let (cutouts, envelopes) = collect_relief_feature_candidates(ipc)?;
    let mut score_blockers = Vec::new();
    for cutout in cutouts
        .into_iter()
        .filter(|cutout| payloads_touch_score_lines(&cutout.payloads, score_lines))
    {
        if plated_like(cutout.plating) {
            let matches = envelopes
                .iter()
                .filter(|envelope| envelope_matches_cutout(envelope, &cutout))
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
            for envelope in matches {
                score_blockers.extend(envelope.payloads.clone());
            }
        } else {
            score_blockers.extend(cutout.payloads);
        }
    }

    let blockers = ContourSet::from_filled_contours(&score_blockers, DEFAULT_RELIEF_TOLERANCE_MM);
    Ok(BoardArrayReliefFeatures {
        score_blockers: blockers.to_contours(),
    })
}

fn collect_relief_feature_candidates(
    ipc: &Ipc2581,
) -> Result<(Vec<ReliefFeatureCandidate>, Vec<ReliefFeatureCandidate>)> {
    let ecad = ipc.ecad().context("IPC-2581 file has no ECAD section")?;
    let mut cutouts = Vec::new();
    let mut envelopes = Vec::new();

    for layer in ecad
        .cad_data
        .layers
        .iter()
        .filter(|layer| relief_feature_layer(layer.layer_function))
    {
        let layer_name = ipc.resolve(layer.name);
        let doc = extract_layer_for_view(ipc, layer_name, View::ArrayFlattened)
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
    payloads: Vec<ContourBuf>,
    bbox: BBox,
    plating: PlatingKind,
    padstack_ref: Option<Symbol>,
    net: Option<Symbol>,
}

impl ReliefFeatureCandidate {
    fn new(doc: &GeometryDocument, feature: &Feature<Symbol>) -> Self {
        Self {
            payloads: feature_contours(doc, feature),
            bbox: feature.bbox,
            plating: feature.intent.plating,
            padstack_ref: feature.padstack_ref,
            net: feature.net,
        }
    }
}

fn feature_contours(doc: &GeometryDocument, feature: &Feature<Symbol>) -> Vec<ContourBuf> {
    feature
        .paths
        .slice(&doc.arena.paths)
        .iter()
        .flat_map(|path| doc.arena.path_contours(path))
        .collect()
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

fn payloads_touch_score_lines(payloads: &[ContourBuf], score_lines: &[VScoreLine]) -> bool {
    if payloads.is_empty() {
        return false;
    }
    let bbox = payloads
        .iter()
        .fold(BBox::empty(), |bbox, payload| bbox.union(payload.bbox));
    let region = ContourSet::from_filled_contours(payloads, DEFAULT_RELIEF_TOLERANCE_MM);
    score_lines.iter().any(|line| {
        let strip = score_line_strip(*line);
        bbox.intersects(strip.bbox) && !region.intersection(&strip).is_empty()
    })
}

fn score_line_strip(line: VScoreLine) -> ContourSet {
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
    ContourSet::rectangle(bbox, DEFAULT_RELIEF_TOLERANCE_MM)
}

fn envelope_matches_cutout(
    envelope: &ReliefFeatureCandidate,
    cutout: &ReliefFeatureCandidate,
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
    payloads_intersect(&envelope.payloads, &cutout.payloads)
}

fn payloads_intersect(left: &[ContourBuf], right: &[ContourBuf]) -> bool {
    let left_region = ContourSet::from_filled_contours(left, DEFAULT_RELIEF_TOLERANCE_MM);
    let right_region = ContourSet::from_filled_contours(right, DEFAULT_RELIEF_TOLERANCE_MM);
    !left_region.intersection(&right_region).is_empty()
}
