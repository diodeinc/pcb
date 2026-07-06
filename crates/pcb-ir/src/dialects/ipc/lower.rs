//! Lowerings out of the IPC dialect: per-layer artwork, NC drill/rout
//! documents, and fabrication profiles.

use crate::dialects::ipc::analysis::{
    ProfileOccurrenceRole, ProfileSet, profile_occurrences_for, root_panel_step,
};
use crate::dialects::ipc::feature::{
    Feature, FeatureBucket, FeatureKind, FeatureOperation, FeatureSpan, PlatingKind,
};
use crate::dialects::ipc::layout::{LayoutStepKind, StepProfile};
use crate::dialects::ipc::{Document, relief};
use crate::dialects::{LayerRole, Side};
use crate::dialects::{artwork, nc};
use crate::geom::path::ContourBuf;
use crate::geom::{Affine2, BBox, ContourSet, PaintKind, Point, Polarity, Span};

/// Lower one layer's features into a single-layer artwork document.
///
/// Run [`process::normalize_for_artwork`](crate::dialects::ipc::process::normalize_for_artwork)
/// first so set voids, negative polarity, and cutouts are resolved.
pub fn lower_layer_to_artwork<Symbol: Clone, LayerFunction: Clone>(
    doc: &Document<Symbol, LayerFunction>,
    layer_index: usize,
    role: LayerRole,
    side: Side,
) -> artwork::Document<LayerFunction, Option<Symbol>> {
    let mut out = artwork::Document::new();
    let layer = &doc.layers[layer_index];
    let artwork_layer = out.push_layer(artwork::Layer {
        name: layer.name.clone(),
        role,
        side,
        objects: Span::EMPTY,
        bbox: BBox::empty(),
        meta: layer.layer_function.clone(),
    });

    for feature in layer.features.slice(&doc.features) {
        for path in feature.paths.slice(&doc.arena.paths) {
            let make_geometry: fn(u32) -> artwork::Geometry = match path.paint.kind() {
                PaintKind::Fill => |path| artwork::Geometry::Region { path },
                PaintKind::Stroke => |path| artwork::Geometry::Stroke { path },
                PaintKind::None => continue,
            };
            let path_id = out.push_path(path.paint, doc.arena.path_contours(path));
            out.push_object(
                artwork_layer,
                artwork::Object {
                    polarity: feature.polarity,
                    order: paint_order(feature),
                    geometry: make_geometry(path_id),
                    bbox: path.bbox,
                    meta: feature.net.clone(),
                },
            );
        }
    }

    out.diagnostics.extend(doc.diagnostics.clone());
    artwork::normalize_bounds(&mut out);
    out
}

pub fn paint_order<Symbol>(feature: &Feature<Symbol>) -> artwork::PaintOrder {
    let stage = if feature.bucket == FeatureBucket::Cutout {
        artwork::PaintStage::FinalCutout
    } else if feature.polarity == Polarity::Clear
        || feature.flags.clears_previous_in_set
        || feature.bucket == FeatureBucket::Fill
    {
        artwork::PaintStage::Base
    } else {
        artwork::PaintStage::Overlay
    };
    artwork::PaintOrder { stage }
}

/// Lower drill and rout features into an NC document.
///
/// Holes become drills; simple oval slots become slots. Route-operation slots
/// that are not simple ovals are skipped (they are routed, not drilled), as
/// are route slots defined outside board steps; any other non-oval slot is an
/// error because it cannot be represented in NC output.
pub fn lower_to_nc<Symbol: Copy, LayerFunction>(
    doc: &Document<Symbol, LayerFunction>,
    nc: &mut nc::Document<Symbol>,
) -> Result<(), String> {
    for layer in &doc.layers {
        for feature in layer.features.slice(&doc.features) {
            match feature.kind {
                FeatureKind::Hole if feature.outer_diameter > 0.0 => {
                    nc.objects.push(nc_object_from_feature(
                        doc,
                        feature,
                        nc::Geometry::Drill {
                            at: feature.center,
                            diameter: feature.outer_diameter,
                        },
                    )?);
                }
                FeatureKind::Slot => {
                    if feature.intent.operation == FeatureOperation::Route
                        && feature.source_step_kind != LayoutStepKind::Board
                    {
                        continue;
                    }
                    let Some((diameter, start, end)) = nc_linear_slot(feature) else {
                        if feature.intent.operation == FeatureOperation::Route {
                            continue;
                        }
                        return Err(format!(
                            "cannot export slot on layer '{}' to NC because it is not a simple oval slot",
                            layer.name
                        ));
                    };
                    let geometry = nc::Geometry::Slot {
                        diameter,
                        start,
                        end,
                    };
                    nc.objects
                        .push(nc_object_from_feature(doc, feature, geometry)?);
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn nc_object_from_feature<Symbol: Copy, LayerFunction>(
    doc: &Document<Symbol, LayerFunction>,
    feature: &Feature<Symbol>,
    geometry: nc::Geometry,
) -> Result<nc::Object<Symbol>, String> {
    let plating = match feature.intent.plating {
        PlatingKind::Via | PlatingKind::ViaCapped | PlatingKind::Plated => nc::Plating::Plated,
        PlatingKind::NonPlated | PlatingKind::None => nc::Plating::NonPlated,
        PlatingKind::Unknown => {
            return Err("cannot export drill/rout feature to NC with unknown plating".to_string());
        }
    };
    let function = if matches!(
        feature.intent.plating,
        PlatingKind::Via | PlatingKind::ViaCapped
    ) {
        nc::Function::Via
    } else {
        nc::Function::Component
    };
    let span = match feature.intent.span {
        FeatureSpan::ThroughBoard | FeatureSpan::Unknown => nc::DrillSpan::ThroughBoard,
        FeatureSpan::Layer(layer) => nc::DrillSpan::FromTo {
            from: Some(layer),
            to: Some(layer),
        },
        FeatureSpan::FromTo { from, to } => nc::DrillSpan::FromTo { from, to },
    };

    let pin_ref = feature.pin_refs.slice(&doc.pin_refs).first();
    Ok(nc::Object {
        geometry,
        plating,
        span,
        function,
        net: feature.net,
        component: pin_ref.and_then(|pin_ref| pin_ref.component_ref),
        pin: pin_ref.map(|pin_ref| pin_ref.pin),
    })
}

/// Interpret a slot feature as a round-tool linear slot: `(diameter, start, end)`.
fn nc_linear_slot<Symbol>(feature: &Feature<Symbol>) -> Option<(f64, Point, Point)> {
    if feature.width <= 0.0 || feature.height <= 0.0 || feature.scale <= 0.0 {
        return None;
    }
    let diameter = feature.width.min(feature.height) * feature.scale;
    if diameter <= tol_epsilon() {
        return None;
    }
    let long = feature.width.max(feature.height);
    let short = feature.width.min(feature.height);
    let centerline = (long - short).max(0.0) / 2.0;
    if centerline <= tol_epsilon() {
        return None;
    }
    let (start, end) = if feature.width >= feature.height {
        (Point::new(-centerline, 0.0), Point::new(centerline, 0.0))
    } else {
        (Point::new(0.0, -centerline), Point::new(0.0, centerline))
    };
    Some((
        diameter,
        feature.transform.transform_point(start),
        feature.transform.transform_point(end),
    ))
}

fn tol_epsilon() -> f64 {
    crate::geom::tol::EPSILON_MM
}

/// Options for [`board_array_fabrication_profile`].
#[derive(Debug, Clone, Default)]
pub struct FabricationProfileOptions {
    pub relief_features: BoardArrayReliefFeatures,
    /// Collect per-boundary construction geometry in the returned debug data.
    pub debug: bool,
}

#[derive(Debug, Clone, Default)]
pub struct BoardArrayFabricationProfile {
    /// Exterior profile contours for the generated board array.
    pub array_outlines: Vec<Vec<ContourBuf>>,
    /// Closed material-removal contours inside the array profile.
    ///
    /// This is the regularized union of source profile cutouts, repeated board
    /// cutouts, and V-score relief regions. Keeping it as one unioned planar
    /// region means overlapping cutouts/reliefs collapse before downstream
    /// Gerber/SVG/profile export sees them.
    pub material_removal: Vec<ContourBuf>,
}

#[derive(Debug, Clone, Default)]
pub struct BoardArrayReliefFeatures {
    /// Through-board features that interrupt V-score separation.
    ///
    /// For non-plated holes/slots this is the mechanical aperture. For plated
    /// holes/slots this is the actual pad/copper envelope, so score reliefs are
    /// derived from source geometry instead of clearance guesses.
    pub score_blockers: Vec<ContourBuf>,
}

/// Compose the physical outline and material removal of a board array,
/// including tool-aware V-score relief pockets.
pub fn board_array_fabrication_profile<Symbol, LayerFunction>(
    doc: &Document<Symbol, LayerFunction>,
    score_lines: &[relief::VScoreLine],
    options: FabricationProfileOptions,
) -> Result<(BoardArrayFabricationProfile, relief::VScoreReliefDebug), relief::VScoreReliefError> {
    if root_panel_step(doc).is_none() {
        return Ok((
            BoardArrayFabricationProfile::default(),
            relief::VScoreReliefDebug::default(),
        ));
    }

    let input = collect_board_array_fabrication_profile_input(doc);
    compose_board_array_fabrication_profile(input, score_lines, options)
}

#[derive(Debug, Clone, Default)]
struct BoardArrayFabricationProfileInput {
    array_outlines: Vec<Vec<ContourBuf>>,
    source_material_removal: Vec<Vec<ContourBuf>>,
    board_boundaries: Vec<ContourBuf>,
    board_cutouts: Vec<ContourBuf>,
}

fn collect_board_array_fabrication_profile_input<Symbol, LayerFunction>(
    doc: &Document<Symbol, LayerFunction>,
) -> BoardArrayFabricationProfileInput {
    let mut input = BoardArrayFabricationProfileInput::default();

    for occurrence in profile_occurrences_for(doc, ProfileSet::RootOnly) {
        input.array_outlines.push(
            doc.transformed_path_contours(occurrence.profile.outer_path, occurrence.transform),
        );
        input
            .source_material_removal
            .extend(transformed_profile_cutout_contours(
                doc,
                occurrence.profile,
                occurrence.transform,
            ));
    }

    for occurrence in profile_occurrences_for(doc, ProfileSet::FabricationOutlines)
        .into_iter()
        .filter(|occurrence| occurrence.role == ProfileOccurrenceRole::BoardInstance)
    {
        let cutouts =
            transformed_profile_cutout_contours(doc, occurrence.profile, occurrence.transform);
        input
            .board_cutouts
            .extend(cutouts.iter().flatten().cloned());
        input.source_material_removal.extend(cutouts);
        input.board_boundaries.extend(
            doc.transformed_path_contours(occurrence.profile.outer_path, occurrence.transform),
        );
    }

    input
}

fn compose_board_array_fabrication_profile(
    input: BoardArrayFabricationProfileInput,
    score_lines: &[relief::VScoreLine],
    options: FabricationProfileOptions,
) -> Result<(BoardArrayFabricationProfile, relief::VScoreReliefDebug), relief::VScoreReliefError> {
    // M = source cutouts ∪ board cutouts ∪ V-score relief material.
    // Store M as a `ContourSet` until the end so every contribution is merged
    // with the same regularized Boolean union.
    let mut material_removal = ContourSet::empty(relief::DEFAULT_RELIEF_TOLERANCE_MM);

    for contours in &input.source_material_removal {
        material_removal.union_assign(&ContourSet::from_filled_contours(
            contours,
            relief::DEFAULT_RELIEF_TOLERANCE_MM,
        ));
    }

    let mut relief_debug = relief::VScoreReliefDebug::default();
    if !score_lines.is_empty() && !input.board_boundaries.is_empty() {
        let relief_input = relief::VScoreReliefInput {
            board_boundaries: input.board_boundaries,
            board_cutouts: input.board_cutouts,
            score_blockers: options.relief_features.score_blockers,
            score_lines: score_lines.to_vec(),
            tool_diameter_mm: relief::DEFAULT_ROUTE_TOOL_DIAMETER_MM,
            tolerance_mm: relief::DEFAULT_RELIEF_TOLERANCE_MM,
        };
        let reliefs = if options.debug {
            let output = relief::vscore_route_reliefs_with_debug(&relief_input)?;
            relief_debug = output.debug;
            output.relief_contours
        } else {
            relief::vscore_route_reliefs(&relief_input)?
        };
        material_removal.union_assign(&ContourSet::from_filled_contours(
            &reliefs,
            relief::DEFAULT_RELIEF_TOLERANCE_MM,
        ));
    }

    Ok((
        BoardArrayFabricationProfile {
            array_outlines: input.array_outlines,
            material_removal: material_removal.to_contours_with_arcs(),
        },
        relief_debug,
    ))
}

fn transformed_profile_cutout_contours<Symbol, LayerFunction>(
    doc: &Document<Symbol, LayerFunction>,
    step_profile: &StepProfile,
    transform: Affine2,
) -> Vec<Vec<ContourBuf>> {
    step_profile
        .cutouts
        .slice(&doc.profile_cutouts)
        .iter()
        .map(|cutout| doc.transformed_path_contours(cutout.path, transform))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::path::PathCmd;
    use crate::geom::{BBox, Point};

    #[test]
    fn material_removal_union_is_winding_insensitive() {
        let mut region = ContourSet::empty(0.001);

        region.union_assign(&ContourSet::from_filled_contours(
            &[reversed_rectangle_contour(0.0, 0.0, 2.0, 2.0)],
            0.001,
        ));
        region.union_assign(&ContourSet::from_filled_contours(
            &[rectangle_contour(1.0, 0.0, 4.0, 2.0)],
            0.001,
        ));

        let bbox = region
            .to_contours()
            .iter()
            .fold(BBox::empty(), |bbox, contour| bbox.union(contour.bbox));
        assert_eq!(bbox.min, Point::new(0.0, 0.0));
        assert_eq!(bbox.max, Point::new(4.0, 2.0));
    }

    fn rectangle_contour(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> ContourBuf {
        ContourBuf::new(vec![
            PathCmd::move_to(Point::new(min_x, min_y)),
            PathCmd::line_to(Point::new(max_x, min_y)),
            PathCmd::line_to(Point::new(max_x, max_y)),
            PathCmd::line_to(Point::new(min_x, max_y)),
            PathCmd::close(),
        ])
    }

    fn reversed_rectangle_contour(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> ContourBuf {
        ContourBuf::new(vec![
            PathCmd::move_to(Point::new(min_x, max_y)),
            PathCmd::line_to(Point::new(max_x, max_y)),
            PathCmd::line_to(Point::new(max_x, min_y)),
            PathCmd::line_to(Point::new(min_x, min_y)),
            PathCmd::close(),
        ])
    }
}
