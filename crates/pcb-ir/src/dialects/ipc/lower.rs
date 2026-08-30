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
use crate::dialects::ipc::layout::{LayoutPurpose, StepProfile};
use crate::dialects::ipc::{Document, relief};
use crate::dialects::{LayerRole, Side};
use crate::dialects::{artwork, nc};
use crate::geom::path::{ContourBuf, transform_cmds};
use crate::geom::{
    Affine2, BBox, ContourSet, FillRule, Paint, Point, Polarity, Span, StrokeStyle, tol,
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
    ObjectMeta: Default,
{
    let mut out = artwork::Document::new();
    let artwork_layer = out.push_layer(header);
    for object in lower_layer_to_artwork_objects_with(doc, layer_index, &mut out, lowering) {
        out.push_object(artwork_layer, object);
    }
    artwork::normalize_bounds(&mut out);
    out
}

/// Lower one IPC layer's features into artwork objects for the caller to
/// place on a layer or in a block.
///
/// This is the hierarchy-preserving counterpart to
/// [`lower_layer_to_artwork_with`]. Apertures, paths, and diagnostics are
/// interned directly in `out`; placement-group blocks are created before the
/// returned objects so block references remain topologically ordered.
pub fn lower_layer_to_artwork_objects_with<Symbol, LayerFunction, LayerMeta, ObjectMeta>(
    doc: &Document<Symbol, LayerFunction>,
    layer_index: usize,
    out: &mut artwork::Document<LayerMeta, ObjectMeta>,
    lowering: &mut impl ArtworkLowering<Symbol, ObjectMeta>,
) -> Vec<artwork::Object<ObjectMeta>>
where
    Symbol: Copy + Eq + Hash,
    ObjectMeta: Default,
{
    let layer = &doc.layers[layer_index];
    let layer_features = layer.features.slice(&doc.features);
    let mut instance_apertures = HashMap::<Symbol, u32>::new();
    let mut objects = Vec::new();

    for (offset, feature) in layer_features.iter().enumerate() {
        let Some(group_id) = feature.placement_group else {
            lower_feature_artwork(
                doc,
                feature,
                out,
                lowering,
                &mut instance_apertures,
                &mut objects,
            );
            continue;
        };
        let group = doc.feature_placement_groups[group_id as usize];
        if layer.features.start + offset as u32 != group.features.start {
            continue;
        }

        let mut block_objects = Vec::new();
        for member in group.features.slice(&doc.features) {
            lower_feature_artwork(
                doc,
                member,
                out,
                lowering,
                &mut instance_apertures,
                &mut block_objects,
            );
        }
        // Partition lowered members by polarity and paint stage: each class
        // becomes one reusable block whose instances carry the class polarity
        // and stage, so polarity runs and stage sorting order compact groups
        // exactly like the flat features they stand for. Block content is
        // normalized to dark; the instance polarity composes it back.
        let mut classes = Vec::<(Polarity, artwork::PaintOrder, u32)>::new();
        for mut object in block_objects {
            let class = (object.polarity, object.order.stage);
            let block = match classes
                .iter()
                .find(|(polarity, order, _)| (*polarity, order.stage) == class)
            {
                Some(&(_, _, block)) => block,
                None => {
                    let block = out.push_block();
                    classes.push((object.polarity, object.order, block));
                    block
                }
            };
            object.polarity = Polarity::Dark;
            out.push_block_object(block, object);
        }
        for (polarity, order, block) in classes {
            objects.extend(group.placements.slice(&doc.feature_placements).iter().map(
                |&transform| artwork::Object {
                    polarity,
                    order,
                    geometry: artwork::Geometry::Instance { block, transform },
                    bbox: out.blocks[block as usize].bbox.transformed(transform),
                    meta: ObjectMeta::default(),
                },
            ));
        }
    }
    out.diagnostics.extend(doc.diagnostics.clone());
    objects
}

fn lower_feature_artwork<Symbol, LayerFunction, LayerMeta, ObjectMeta>(
    doc: &Document<Symbol, LayerFunction>,
    feature: &Feature<Symbol>,
    out: &mut artwork::Document<LayerMeta, ObjectMeta>,
    lowering: &mut impl ArtworkLowering<Symbol, ObjectMeta>,
    instance_apertures: &mut HashMap<Symbol, u32>,
    objects: &mut Vec<artwork::Object<ObjectMeta>>,
) where
    Symbol: Copy + Eq + Hash,
{
    if let Some((aperture, transform, bbox)) =
        flash_for(out, doc, feature, lowering, instance_apertures)
    {
        objects.push(artwork::Object {
            polarity: feature.polarity,
            order: lowering.paint_order(feature),
            geometry: artwork::Geometry::Flash {
                aperture,
                transform,
            },
            bbox,
            meta: lowering.object_meta(feature, ArtworkObjectKind::Flash),
        });
        return;
    }

    objects.extend(
        feature
            .paths
            .slice(&doc.arena.paths)
            .iter()
            .filter_map(|path| {
                let (paint, kind, make_geometry): (_, _, fn(u32) -> artwork::Geometry) =
                    match path.paint {
                        Paint::Fill { rule } => {
                            (Paint::Fill { rule }, ArtworkObjectKind::Region, |path| {
                                artwork::Geometry::Region { path }
                            })
                        }
                        Paint::Stroke(stroke) => (
                            Paint::Stroke(lowering.stroke_style(stroke)),
                            ArtworkObjectKind::Stroke,
                            |path| artwork::Geometry::Stroke { path },
                        ),
                        Paint::None => return None,
                    };
                let path_id = out.push_path(paint, doc.arena.path_contours(path));
                Some(artwork::Object {
                    polarity: feature.polarity,
                    order: lowering.paint_order(feature),
                    geometry: make_geometry(path_id),
                    bbox: out.path_bbox(path_id),
                    meta: lowering.object_meta(feature, kind),
                })
            }),
    );
}

/// The shared aperture a feature flashes through, if any: one the source
/// declares, one derived from a repeated dictionary instance, or a plain
/// circle for a drilled or fiducial feature.
fn flash_for<Symbol, LayerFunction, LayerMeta, ObjectMeta>(
    out: &mut artwork::Document<LayerMeta, ObjectMeta>,
    doc: &Document<Symbol, LayerFunction>,
    feature: &Feature<Symbol>,
    lowering: &mut impl ArtworkLowering<Symbol, ObjectMeta>,
    apertures: &mut HashMap<Symbol, u32>,
) -> Option<(u32, Affine2, BBox)>
where
    Symbol: Copy + Eq + Hash,
{
    if let Some((aperture, transform, bbox)) = lowering.source_aperture(feature) {
        return Some((out.push_aperture(aperture), transform, bbox));
    }
    if let Some((aperture, transform)) = instance_aperture(out, doc, feature, apertures) {
        return Some((aperture, transform, feature.bbox));
    }
    let (at, diameter) = circle_flash(doc, feature)?;
    Some((
        out.push_aperture(artwork::Aperture::circle(diameter)),
        Affine2::translation(at),
        BBox::from_point(at).expand(diameter / 2.0),
    ))
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
    if let Some(&aperture) = apertures.get(&primitive) {
        return Some((aperture, feature.transform));
    }
    // Derive the origin-local template from this first instance; every
    // sibling shares the aperture and differs only by its rigid transform.
    let shape = contour_flash_aperture(doc, feature)?;
    let aperture = out.push_aperture(artwork::Aperture::solid(shape));
    apertures.insert(primitive, aperture);
    Some((aperture, feature.transform))
}

/// The feature's whole image as an origin-local contour aperture: its single
/// filled path pulled back through the inverse of its placement transform.
/// Flashing the aperture through `feature.transform` reproduces the source
/// image exactly, so repeated placements of one shape share one definition.
pub fn contour_flash_aperture<Symbol, LayerFunction>(
    doc: &Document<Symbol, LayerFunction>,
    feature: &Feature<Symbol>,
) -> Option<artwork::ApertureShape> {
    let [path] = feature.paths.slice(&doc.arena.paths) else {
        return None;
    };
    if !path.is_filled() {
        return None;
    }
    let inverse = feature.transform.inverse()?;
    let local = doc
        .arena
        .path_contours(path)
        .iter()
        .map(|contour| transform_cmds(contour.cmds.iter().copied(), inverse))
        .collect::<Vec<_>>();
    let [outline] = local.try_into().ok()?;
    Some(artwork::ApertureShape::Contour {
        outline,
        fill_rule: path.fill_rule().unwrap_or(FillRule::NonZero),
    })
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
/// Holes become drills and simple oval slots become slots. Any other slot is
/// an explicit error: callers must never mistake silently omitted material
/// removal for a complete manufacturing program.
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
                    let Some((diameter, start, end)) = nc_linear_slot(feature) else {
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
            material_removal: material_removal.to_contours(),
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
    use crate::dialects::ipc::feature::FeaturePlacementGroup;
    use crate::geom::path::PathCmd;
    use crate::geom::{BBox, Point};

    #[test]
    fn preserves_shared_feature_group_when_lowered_or_expanded() {
        let mut doc = Document::<u32, ()>::new();
        let first_path = doc.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            [rectangle_contour(0.0, 0.0, 2.0, 1.0)],
        );
        let second_path = doc.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            [rectangle_contour(3.0, 0.0, 4.0, 1.0)],
        );
        for path in [first_path, second_path] {
            let mut feature = Feature::new(FeatureKind::Polygon, Polarity::Dark);
            feature.paths = Span::single(path);
            feature.placement_group = Some(0);
            doc.features.push(feature);
        }
        doc.feature_placements.extend([
            Affine2::translation(Point::new(10.0, 20.0)),
            Affine2::translation(Point::new(30.0, 40.0)),
        ]);
        doc.feature_placement_groups.push(FeaturePlacementGroup {
            placements: Span::new(0, 2),
            features: Span::new(0, 2),
        });
        doc.layers.push(crate::dialects::ipc::Layer {
            name: "TOP".to_string(),
            source_layer_ref: 0,
            layer_function: (),
            spec_refs: Span::EMPTY,
            sets: Span::EMPTY,
            features: Span::new(0, 2),
            bbox: BBox::empty(),
        });

        let artwork = lower_layer_to_artwork(&doc, 0, LayerRole::Copper, Side::Top);

        assert_eq!(artwork.blocks.len(), 1);
        assert_eq!(artwork.blocks[0].objects.len(), 2);
        assert_eq!(artwork.objects.len(), 2);
        assert_eq!(artwork.arena.paths.len(), 2);
        assert!(artwork.objects.iter().all(|object| matches!(
            object.geometry,
            artwork::Geometry::Instance { block: 0, .. }
        )));

        crate::dialects::ipc::process::expand_feature_placement_groups(&mut doc);
        assert_eq!(
            doc.features
                .iter()
                .map(|feature| feature.bbox.min.x)
                .collect::<Vec<_>>(),
            [10.0, 13.0, 30.0, 33.0]
        );
    }

    #[test]
    fn placement_group_instances_carry_member_polarity_and_stage() {
        let mut doc = Document::<u32, ()>::new();
        let path = doc.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            [rectangle_contour(0.0, 0.0, 1.0, 1.0)],
        );
        let mut feature = Feature::new(FeatureKind::Polygon, Polarity::Clear);
        feature.paths = Span::single(path);
        feature.placement_group = Some(0);
        doc.features.push(feature);
        doc.feature_placements.extend([
            Affine2::translation(Point::new(10.0, 0.0)),
            Affine2::translation(Point::new(20.0, 0.0)),
        ]);
        doc.feature_placement_groups.push(FeaturePlacementGroup {
            placements: Span::new(0, 2),
            features: Span::single(0),
        });
        doc.layers.push(crate::dialects::ipc::Layer {
            name: "TOP".to_string(),
            source_layer_ref: 0,
            layer_function: (),
            spec_refs: Span::EMPTY,
            sets: Span::EMPTY,
            features: Span::single(0),
            bbox: BBox::empty(),
        });

        let artwork = lower_layer_to_artwork(&doc, 0, LayerRole::Copper, Side::Top);

        // The instances stand in for clear features: they must form the same
        // polarity run the flat lowering produced, with block content stored
        // dark so composition restores the member polarity.
        assert_eq!(artwork.objects.len(), 2);
        assert!(
            artwork
                .objects
                .iter()
                .all(|object| object.polarity == Polarity::Clear)
        );
        assert!(
            artwork.blocks[0]
                .objects
                .iter()
                .all(|object| object.polarity == Polarity::Dark)
        );
        let expanded = artwork::expand_instances(&artwork);
        assert_eq!(expanded.objects.len(), 2);
        assert!(
            expanded
                .objects
                .iter()
                .all(|object| object.polarity == Polarity::Clear)
        );
    }

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
