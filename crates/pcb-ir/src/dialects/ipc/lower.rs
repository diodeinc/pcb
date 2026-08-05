//! Lowerings out of the IPC dialect: per-layer artwork, NC drill/rout
//! documents, and fabrication profiles.

use std::collections::HashMap;
use std::hash::Hash;

use crate::dialects::ipc::analysis::{
    ProfileOccurrenceRole, ProfileSet, profile_occurrences_for, root_panel_step,
};
use crate::dialects::ipc::feature::{
    Feature, FeatureBucket, FeatureKind, FeatureOperation, FeatureRole, FeatureSpan, PlatingKind,
    PrimitiveRef,
};
use crate::dialects::ipc::layout::{LayoutPurpose, LayoutStepKind, StepProfile};
use crate::dialects::ipc::{Document, relief};
use crate::dialects::{LayerRole, Side};
use crate::dialects::{artwork, nc};
use crate::geom::path::{ContourBuf, transform_cmds};
use crate::geom::{
    Affine2, BBox, ContourSet, Diagnostic, FillRule, Paint, PaintKind, Point, Polarity, Span,
    StrokeStyle, tol,
};

/// How one artwork object was expressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtworkObjectKind {
    /// A shared aperture stamped under a placement transform.
    Flash,
    /// A filled region path.
    Region,
    /// A stroked centerline path.
    Stroke,
}

/// Source-specific hooks for artwork lowering.
///
/// Everything a lowering can decide from the IR alone — dictionary-instance
/// apertures, circular flashes, per-path regions and strokes, paint staging —
/// lives in [`lower_layer_to_artwork_with`]. A source dialect implements this
/// trait only for what it alone knows: apertures declared by its own shape
/// catalogue, the stroke styles its target can express, and the per-object
/// metadata that target carries.
pub trait ArtworkLowering<Symbol, ObjectMeta> {
    /// An aperture the source document declares for this feature, with its
    /// placement and bounds. Returning `None` falls through to the generic
    /// instance, circle, and per-path tiers.
    fn source_aperture(
        &mut self,
        _feature: &Feature<Symbol>,
    ) -> Option<(artwork::Aperture, Affine2, BBox)> {
        None
    }

    /// Rewrite a stroke into what the target can express. Gerber traces, for
    /// example, are round-joined by construction.
    fn stroke_style(&mut self, stroke: StrokeStyle) -> StrokeStyle {
        stroke
    }

    /// Which paint stage a feature belongs to. Override where the target
    /// stages material removal differently from [`paint_order`].
    fn paint_order(&mut self, feature: &Feature<Symbol>) -> artwork::PaintOrder {
        paint_order(feature)
    }

    fn object_meta(&mut self, feature: &Feature<Symbol>, kind: ArtworkObjectKind) -> ObjectMeta;
}

/// The default lowering: no source catalogue, native strokes, net metadata.
struct NetMetaLowering;

impl<Symbol: Clone> ArtworkLowering<Symbol, Option<Symbol>> for NetMetaLowering {
    fn object_meta(
        &mut self,
        feature: &Feature<Symbol>,
        _kind: ArtworkObjectKind,
    ) -> Option<Symbol> {
        feature.net.clone()
    }
}

/// Lower one layer's features into a single-layer artwork document.
///
/// Run [`process::normalize_for_artwork`](crate::dialects::ipc::process::normalize_for_artwork)
/// first so set voids, negative polarity, and cutouts are resolved.
pub fn lower_layer_to_artwork<Symbol, LayerFunction>(
    doc: &Document<Symbol, LayerFunction>,
    layer_index: usize,
    role: LayerRole,
    side: Side,
) -> artwork::Document<LayerFunction, Option<Symbol>>
where
    Symbol: Copy + Eq + Hash,
    LayerFunction: Clone,
{
    let layer = &doc.layers[layer_index];
    lower_layer_to_artwork_with(
        doc,
        layer_index,
        artwork::Layer {
            name: layer.name.clone(),
            role,
            side,
            objects: Span::EMPTY,
            bbox: BBox::empty(),
            meta: layer.layer_function.clone(),
        },
        &mut NetMetaLowering,
    )
}

/// Lower one layer's features into artwork, resolving repeated geometry to
/// shared apertures.
///
/// Repeated dictionary instances stay instances: every sibling placement of
/// one dictionary entry flashes through a single aperture instead of carrying
/// its own copy of the shape. Targets that can express instancing — Gerber
/// apertures, SVG `<use>` — inherit that directly, and targets that cannot
/// expand it in [`artwork::compose_to_mask`].
pub fn lower_layer_to_artwork_with<Symbol, LayerFunction, LayerMeta, ObjectMeta>(
    doc: &Document<Symbol, LayerFunction>,
    layer_index: usize,
    header: artwork::Layer<LayerMeta>,
    lowering: &mut impl ArtworkLowering<Symbol, ObjectMeta>,
) -> artwork::Document<LayerMeta, ObjectMeta>
where
    Symbol: Copy + Eq + Hash,
{
    let mut out = artwork::Document::new();
    let layer = &doc.layers[layer_index];
    let layer_name = layer.name.clone();
    let artwork_layer = out.push_layer(header);
    let mut instance_apertures = HashMap::<Symbol, u32>::new();

    for feature in layer.features.slice(&doc.features) {
        if let Some((aperture, transform, bbox)) = lowering.source_aperture(feature) {
            let aperture = out.push_aperture(aperture);
            push_flash(
                &mut out,
                artwork_layer,
                lowering,
                feature,
                aperture,
                transform,
                bbox,
            );
            continue;
        }

        if let Some((aperture, transform)) =
            instance_aperture(&mut out, doc, feature, &mut instance_apertures)
        {
            push_flash(
                &mut out,
                artwork_layer,
                lowering,
                feature,
                aperture,
                transform,
                feature.bbox,
            );
            continue;
        }

        if let Some((at, diameter)) = circle_flash(doc, feature) {
            let aperture = out.push_aperture(artwork::Aperture::circle(diameter));
            push_flash(
                &mut out,
                artwork_layer,
                lowering,
                feature,
                aperture,
                Affine2::translation(at),
                BBox::from_point(at).expand(diameter / 2.0),
            );
            continue;
        }

        for path in feature.paths.slice(&doc.arena.paths) {
            let (paint, kind, make_geometry): (_, _, fn(u32) -> artwork::Geometry) =
                match path.paint {
                    Paint::Stroke(stroke) => (
                        Paint::Stroke(lowering.stroke_style(stroke)),
                        ArtworkObjectKind::Stroke,
                        |path| artwork::Geometry::Stroke { path },
                    ),
                    paint if paint.kind() == PaintKind::Fill => {
                        (paint, ArtworkObjectKind::Region, |path| {
                            artwork::Geometry::Region { path }
                        })
                    }
                    _ => {
                        out.diagnostics.push(Diagnostic::warning(format!(
                            "dropped unpainted path on layer '{layer_name}'"
                        )));
                        continue;
                    }
                };
            let path_id = out.push_path(paint, doc.arena.path_contours(path));
            let bbox = out.path_bbox(path_id);
            let meta = lowering.object_meta(feature, kind);
            out.push_object(
                artwork_layer,
                artwork::Object {
                    polarity: feature.polarity,
                    order: lowering.paint_order(feature),
                    geometry: make_geometry(path_id),
                    bbox,
                    meta,
                },
            );
        }
    }

    out.diagnostics.extend(doc.diagnostics.clone());
    artwork::normalize_bounds(&mut out);
    out
}

fn push_flash<Symbol, LayerFunction, ObjectMeta>(
    out: &mut artwork::Document<LayerFunction, ObjectMeta>,
    artwork_layer: u32,
    lowering: &mut impl ArtworkLowering<Symbol, ObjectMeta>,
    feature: &Feature<Symbol>,
    aperture: u32,
    transform: Affine2,
    bbox: BBox,
) {
    let meta = lowering.object_meta(feature, ArtworkObjectKind::Flash);
    let order = lowering.paint_order(feature);
    out.push_object(
        artwork_layer,
        artwork::Object {
            polarity: feature.polarity,
            order,
            geometry: artwork::Geometry::Flash {
                aperture,
                transform,
            },
            bbox,
            meta,
        },
    );
}

/// A user-dictionary instance feature: a placed reference whose local shape
/// is shared by every sibling instance. The shape flashes through one contour
/// aperture per dictionary entry, keeping repeated geometry repeated all the
/// way to the output. Standard-dictionary references stay out: those are
/// exact catalogue primitives that a source lowering flashes through standard
/// apertures instead.
fn instance_aperture<Symbol, LayerFunction, LayerMeta, ObjectMeta>(
    out: &mut artwork::Document<LayerMeta, ObjectMeta>,
    doc: &Document<Symbol, LayerFunction>,
    feature: &Feature<Symbol>,
    apertures: &mut HashMap<Symbol, u32>,
) -> Option<(u32, Affine2)>
where
    Symbol: Copy + Eq + Hash,
{
    let Some(PrimitiveRef::User(primitive)) = feature.primitive_ref else {
        return None;
    };
    if feature.kind != FeatureKind::Primitive || !is_rigid(feature.transform) {
        return None;
    }
    let [path] = feature.paths.slice(&doc.arena.paths) else {
        return None;
    };
    if !path.is_filled() {
        return None;
    }
    if let Some(&aperture) = apertures.get(&primitive) {
        return Some((aperture, feature.transform));
    }
    // Derive the origin-local template from this first instance; every
    // sibling shares the aperture and differs only by its rigid transform.
    let inverse = feature.transform.inverse()?;
    let local = doc
        .arena
        .path_contours(path)
        .iter()
        .map(|contour| transform_cmds(contour.cmds.iter().copied(), inverse))
        .collect::<Vec<_>>();
    let [local] = local.as_slice() else {
        return None;
    };
    let aperture = out.push_aperture(artwork::Aperture::solid(artwork::ApertureShape::Contour {
        outline: local.clone(),
        fill_rule: path.fill_rule().unwrap_or(FillRule::NonZero),
    }));
    apertures.insert(primitive, aperture);
    Some((aperture, feature.transform))
}

/// A drilled or fiducial feature whose whole image is one filled circle.
fn circle_flash<Symbol, LayerFunction>(
    doc: &Document<Symbol, LayerFunction>,
    feature: &Feature<Symbol>,
) -> Option<(Point, f64)> {
    if feature.outer_diameter <= 0.0 || feature.paths.len() != 1 {
        return None;
    }
    if !feature.paths.slice(&doc.arena.paths)[0].is_filled() {
        return None;
    }
    (feature.is_fiducial()
        || feature.intent.role == FeatureRole::Hole
        || feature.intent.operation == FeatureOperation::Drill)
        .then_some((feature.center, feature.outer_diameter))
}

/// Rotation plus translation, without mirroring or scaling.
fn is_rigid(transform: Affine2) -> bool {
    let determinant = transform.m00 * transform.m11 - transform.m01 * transform.m10;
    (determinant - 1.0).abs() <= tol::EPSILON_MM
        && (transform.m00 * transform.m00 + transform.m10 * transform.m10 - 1.0).abs()
            <= tol::EPSILON_MM
}

/// Which paint stage a feature belongs to.
///
/// Targets that image a removal as a clear (mask composition, SVG) and
/// targets that only order it (Gerber) disagree on how wide `FinalCutout`
/// should reach, so a source lowering may override this through
/// [`ArtworkLowering::paint_order`].
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

/// Semantic profile geometry derived from the IPC layout hierarchy.
///
/// The three fields remain separate in IR: the root boundary, direct child
/// panel boundaries, and material-removal regions have different fabrication
/// meanings even though a plain board-array export may place them in one file.
#[derive(Debug, Clone, Default)]
pub struct BoardArrayFabricationProfile {
    pub purpose: LayoutPurpose,
    /// Nominal exterior profile contours of the root panel.
    pub array_outlines: Vec<Vec<ContourBuf>>,
    /// Nominal profile contours of direct child assembly panels.
    pub assembly_panel_outlines: Vec<Vec<ContourBuf>>,
    /// Closed material-removal contours inside the array profile.
    ///
    /// This is the regularized union of root, assembly-panel, and board profile
    /// cutouts plus V-score relief regions. Keeping it as one planar region
    /// makes overlaps collapse before downstream artwork or Gerber lowering.
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
    let Some((_, root_panel)) = root_panel_step(doc) else {
        return Ok((
            BoardArrayFabricationProfile::default(),
            relief::VScoreReliefDebug::default(),
        ));
    };

    let input = collect_board_array_fabrication_profile_input(doc, root_panel.purpose);
    compose_board_array_fabrication_profile(input, score_lines, options)
}

#[derive(Debug, Clone, Default)]
struct BoardArrayFabricationProfileInput {
    purpose: LayoutPurpose,
    array_outlines: Vec<Vec<ContourBuf>>,
    source_material_removal: Vec<Vec<ContourBuf>>,
    board_boundaries: Vec<ContourBuf>,
    board_cutouts: Vec<ContourBuf>,
    assembly_panel_outlines: Vec<Vec<ContourBuf>>,
}

fn collect_board_array_fabrication_profile_input<Symbol, LayerFunction>(
    doc: &Document<Symbol, LayerFunction>,
    purpose: LayoutPurpose,
) -> BoardArrayFabricationProfileInput {
    let mut input = BoardArrayFabricationProfileInput {
        purpose,
        ..BoardArrayFabricationProfileInput::default()
    };

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

    if purpose == LayoutPurpose::FabricationPanel {
        for occurrence in profile_occurrences_for(doc, ProfileSet::LayoutBoundaries)
            .into_iter()
            .filter(|occurrence| {
                occurrence.role == ProfileOccurrenceRole::PanelInstance && occurrence.depth == 1
            })
        {
            input.assembly_panel_outlines.push(
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
            purpose: input.purpose,
            array_outlines: input.array_outlines,
            assembly_panel_outlines: input.assembly_panel_outlines,
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

    #[test]
    fn assembly_panel_outlines_remain_nominal_and_are_not_material_removal() {
        let finished = rectangle_contour(10.0, 20.0, 110.0, 100.0);
        let input = BoardArrayFabricationProfileInput {
            purpose: LayoutPurpose::FabricationPanel,
            assembly_panel_outlines: vec![vec![finished]],
            ..BoardArrayFabricationProfileInput::default()
        };
        let (profile, _) =
            compose_board_array_fabrication_profile(input, &[], Default::default()).unwrap();

        assert_eq!(profile.purpose, LayoutPurpose::FabricationPanel);
        assert!(profile.material_removal.is_empty());
        assert_eq!(profile.assembly_panel_outlines.len(), 1);
        let bbox = profile.assembly_panel_outlines[0]
            .iter()
            .fold(BBox::empty(), |bbox, contour| bbox.union(contour.bbox));
        assert_eq!(bbox.min, Point::new(10.0, 20.0));
        assert_eq!(bbox.max, Point::new(110.0, 100.0));
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
