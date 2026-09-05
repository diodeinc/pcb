use pcb_ir::geom::{AccuracyError, GeometryAccuracy};
use std::collections::{HashMap, HashSet};
#[cfg(feature = "cli")]
use std::fmt::Write as _;
#[cfg(feature = "cli")]
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use gerberx2::{GerberLayer, write_layer};
use ipc2581::Ipc2581;
use ipc2581::types::{
    FillProperty, LayerFunction, Side as IpcSide, StandardPrimitive,
    ecad::{Layer, Step},
};

use crate::geometry;
use gerberx2::from_artwork::lower_artwork_layer;
use gerberx2::from_artwork::{ArtworkDocument as GerberArtwork, LayerAttributes, ObjectAttributes};
use pcb_ir::dialects::artwork::{
    Aperture, ApertureShape, Geometry as ArtworkGeometry, GridRepeat, Object as ArtworkObject,
    PaintOrder, PaintStage,
};
#[cfg(feature = "cli")]
use pcb_ir::dialects::ipc::relief;
use pcb_ir::dialects::ipc::{
    ArtworkLowering, ArtworkObjectKind, ArtworkScope, CopperBalanceKind, Feature, FeatureBucket,
    FeatureDomain, FeatureOperation, FeatureRole, FiducialKind, LayoutPurpose, PlatingKind,
    PrimitiveRef, ProfileSet, lower_layer_to_artwork_objects_with, lower_layer_to_artwork_with,
    profile_occurrences_for,
};
use pcb_ir::dialects::{LayerRole, Side as IrSide};
use pcb_ir::geom::path::ContourBuf;
#[cfg(feature = "cli")]
use pcb_ir::geom::path::{PathCmd, PathOp};
use pcb_ir::geom::{
    Affine2, BBox, LineCap, LineJoin, LinePattern, Paint, Point, Polarity, Span, StrokeStyle,
};
use pcb_ir::import::ipc2581::{ImportedDesign, LayerId, import_design};

#[cfg(feature = "cli")]
use pcb_ir::geom::Arc;

type IpcGeometryDocument = pcb_ir::dialects::ipc::Document<ipc2581::Symbol, LayerFunction>;

#[derive(Debug, Clone)]
pub struct GerberX2File {
    pub filename: String,
    pub layer: GerberLayer,
    pub contents: String,
}

#[derive(Debug, Clone, Default)]
pub struct GerberExportOptions {
    pub relief_debug_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy)]
struct ProfileGerberStyle {
    stroke_width_mm: f64,
    line_cap: LineCap,
    line_join: LineJoin,
}

impl Default for ProfileGerberStyle {
    fn default() -> Self {
        Self {
            stroke_width_mm: 0.05,
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
        }
    }
}

pub fn build_gerber_x2_files(
    ipc: &Ipc2581,
    view: ArtworkScope,
    accuracy: GeometryAccuracy,
) -> Result<Vec<GerberX2File>> {
    build_gerber_x2_files_with_options(ipc, view, &GerberExportOptions::default(), accuracy)
}

pub fn build_gerber_x2_files_with_options(
    ipc: &Ipc2581,
    view: ArtworkScope,
    options: &GerberExportOptions,
    accuracy: GeometryAccuracy,
) -> Result<Vec<GerberX2File>> {
    let imported = import_design(ipc, accuracy)?;
    build_gerber_x2_files_from_design_with_options(&imported, view, options, accuracy)
}

pub(crate) fn build_gerber_x2_files_from_design_with_options(
    imported: &ImportedDesign,
    view: ArtworkScope,
    options: &GerberExportOptions,
    accuracy: GeometryAccuracy,
) -> Result<Vec<GerberX2File>> {
    // With no repeated instances, a board-array request denotes this board
    // itself. Use the board path so its Step/Profile remains authoritative
    // even when the source has no BOARD_OUTLINE layer artwork.
    let view = if view == ArtworkScope::ArrayFlattened
        && imported.geometry.layout.repeats.is_empty()
        && pcb_ir::dialects::ipc::root_step(&imported.geometry)
            .is_some_and(|(_, step)| step.kind == pcb_ir::dialects::ipc::LayoutStepKind::Board)
    {
        ArtworkScope::Board
    } else {
        view
    };
    let mut files = Vec::new();
    let plans = export_layer_plans(imported, &imported.layer_definitions);
    let has_profile_plan = plans
        .iter()
        .any(|plan| plan.role == GerberLayerRole::Profile);
    let part = gerber_part_for_ipc_view(imported, view)?;

    for plan in &plans {
        let source_layer = plan.layer;
        let layer_name = imported.resolve(source_layer.name);
        let spec = GerberArtworkSpec {
            role: plan.role,
            side: ir_side(source_layer.side),
            meta: layer_attributes(plan.file_function.clone(), part, plan.role),
            view,
        };
        let artwork = if view == ArtworkScope::ArrayFlattened {
            hierarchical_artwork_from_ipc_layer(
                imported,
                plan.layer_id,
                layer_name,
                spec,
                accuracy,
            )?
        } else {
            let mut doc = imported
                .materialize_layer(plan.layer_id, view, accuracy)
                .with_context(|| format!("failed to extract IPC-2581 layer '{layer_name}'"))?;
            pcb_ir::dialects::ipc::process::normalize_for_artwork(&mut doc, accuracy)?;
            if let Err(error) = pcb_ir::dialects::ipc::validate_artwork_ready(&doc) {
                bail!("IPC-2581 layer '{layer_name}' is not artwork-ready: {error}");
            }
            artwork_from_ipc_layer(imported, &doc, 0, spec, accuracy)?
        };
        if matches!(plan.role, GerberLayerRole::Vcut | GerberLayerRole::Score)
            && artwork.layers[0].objects.is_empty()
        {
            continue;
        }
        let layer = lower_artwork_layer(&artwork, accuracy)?;
        if plan.role == GerberLayerRole::Profile && layer.objects.is_empty() {
            continue;
        }
        let contents = write_layer(&layer)?;
        files.push(GerberX2File {
            filename: plan.filename.clone(),
            layer,
            contents,
        });
    }
    if view == ArtworkScope::ArrayFlattened {
        files.extend(board_array_profile_gerber_files(
            imported,
            options.relief_debug_dir.as_deref(),
            accuracy,
        )?);
    } else if !has_profile_plan
        && let Some(file) = synthetic_profile_gerber_file(imported, view, accuracy)?
    {
        files.push(file);
    }

    Ok(files)
}

struct ExportLayerPlan<'a> {
    layer_id: LayerId,
    layer: &'a Layer,
    role: GerberLayerRole,
    filename: String,
    file_function: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GerberLayerRole {
    Copper,
    Paste,
    Soldermask,
    Legend,
    AssemblyDrawing,
    FabricationDrawing,
    Profile,
    Vcut,
    Score,
}

fn export_layer_plans<'a>(
    imported: &ImportedDesign,
    layers: &'a [Layer],
) -> Vec<ExportLayerPlan<'a>> {
    let copper_count = copper_layer_count(layers);
    let mut copper_index = 0;
    let mut plans = Vec::new();
    let mut used_filenames = HashSet::new();

    for (layer_index, layer) in layers.iter().enumerate() {
        let Some(role) = gerber_layer_role(layer.layer_function) else {
            continue;
        };
        if role == GerberLayerRole::Copper {
            copper_index += 1;
        }
        let source_layer_name = imported.resolve(layer.name);
        let (filename, file_function) = layer_output(
            role,
            layer.side,
            copper_index,
            copper_count,
            source_layer_name,
        );
        let filename = allocate_filename(&mut used_filenames, &filename, source_layer_name);
        plans.push(ExportLayerPlan {
            layer_id: LayerId(layer_index as u32),
            layer,
            role,
            filename,
            file_function,
        });
    }

    plans
}

fn copper_layer_count(layers: &[Layer]) -> usize {
    layers
        .iter()
        .filter(|layer| gerber_layer_role(layer.layer_function) == Some(GerberLayerRole::Copper))
        .count()
}

fn allocate_filename(
    used: &mut HashSet<String>,
    preferred: &str,
    source_layer_name: &str,
) -> String {
    if used.insert(preferred.to_string()) {
        return preferred.to_string();
    }

    let (stem, extension) = split_filename(preferred);
    let extension = extension
        .map(|extension| format!(".{extension}"))
        .unwrap_or_default();
    let source_stem = sanitize_filename_stem(source_layer_name);
    let source_stem = if source_stem.is_empty() {
        stem.to_string()
    } else {
        source_stem
    };

    for index in 1.. {
        let candidate = if index == 1 {
            format!("{source_stem}{extension}")
        } else {
            format!("{source_stem}_{index}{extension}")
        };
        if used.insert(candidate.clone()) {
            return candidate;
        }
    }
    unreachable!("unbounded filename allocation should find an unused name")
}

fn split_filename(filename: &str) -> (&str, Option<&str>) {
    filename
        .rsplit_once('.')
        .map_or((filename, None), |(stem, extension)| {
            (stem, Some(extension))
        })
}

fn sanitize_filename_stem(name: &str) -> String {
    let mut stem = String::new();
    let mut last_was_separator = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            stem.push(ch);
            last_was_separator = false;
        } else if !last_was_separator {
            stem.push('_');
            last_was_separator = true;
        }
    }
    stem.trim_matches('_').to_string()
}

fn gerber_layer_role(function: LayerFunction) -> Option<GerberLayerRole> {
    if crate::layers::is_copper(function) {
        return Some(GerberLayerRole::Copper);
    }
    match function {
        LayerFunction::Solderpaste | LayerFunction::Pastemask => Some(GerberLayerRole::Paste),
        LayerFunction::Soldermask => Some(GerberLayerRole::Soldermask),
        LayerFunction::Silkscreen | LayerFunction::Legend => Some(GerberLayerRole::Legend),
        LayerFunction::Assembly => Some(GerberLayerRole::AssemblyDrawing),
        LayerFunction::BoardFab => Some(GerberLayerRole::FabricationDrawing),
        LayerFunction::Drill | LayerFunction::Rout => None,
        LayerFunction::BoardOutline => Some(GerberLayerRole::Profile),
        LayerFunction::VCut => Some(GerberLayerRole::Vcut),
        LayerFunction::Score => Some(GerberLayerRole::Score),
        _ => None,
    }
}

impl GerberLayerRole {
    fn ir_role(self) -> LayerRole {
        match self {
            GerberLayerRole::Copper => LayerRole::Copper,
            GerberLayerRole::Paste => LayerRole::Paste,
            GerberLayerRole::Soldermask => LayerRole::Soldermask,
            GerberLayerRole::Legend => LayerRole::Legend,
            GerberLayerRole::AssemblyDrawing | GerberLayerRole::FabricationDrawing => {
                LayerRole::Mechanical
            }
            GerberLayerRole::Profile | GerberLayerRole::Vcut | GerberLayerRole::Score => {
                LayerRole::Profile
            }
        }
    }
}

fn layer_output(
    role: GerberLayerRole,
    side: Option<IpcSide>,
    copper_index: usize,
    copper_count: usize,
    source_layer_name: &str,
) -> (String, Vec<String>) {
    match role {
        GerberLayerRole::Copper => copper_layer_output(side, copper_index, copper_count),
        GerberLayerRole::Paste => match side {
            Some(IpcSide::Bottom) => (
                "B_Paste.gbp".to_string(),
                vec!["Paste".into(), "Bot".into()],
            ),
            _ => (
                "F_Paste.gtp".to_string(),
                vec!["Paste".into(), "Top".into()],
            ),
        },
        GerberLayerRole::Soldermask => match side {
            Some(IpcSide::Bottom) => (
                "B_Mask.gbs".to_string(),
                vec!["Soldermask".into(), "Bot".into()],
            ),
            _ => (
                "F_Mask.gts".to_string(),
                vec!["Soldermask".into(), "Top".into()],
            ),
        },
        GerberLayerRole::Legend => match side {
            Some(IpcSide::Bottom) => (
                "B_SilkS.gbo".to_string(),
                vec!["Legend".into(), "Bot".into()],
            ),
            _ => (
                "F_SilkS.gto".to_string(),
                vec!["Legend".into(), "Top".into()],
            ),
        },
        GerberLayerRole::AssemblyDrawing => assembly_drawing_layer_output(source_layer_name, side),
        GerberLayerRole::FabricationDrawing => (
            drawing_filename(source_layer_name, "Fabrication_Drawing"),
            vec!["FabricationDrawing".into()],
        ),
        GerberLayerRole::Profile => (
            "Edge_Cuts.gm1".to_string(),
            vec!["Profile".into(), "NP".into()],
        ),
        GerberLayerRole::Vcut => fabrication_line_layer_output("V_Cut.gbr", &["Vcut"], side),
        // Gerber calls the scored-line data function `Vcut`; the specification
        // explicitly treats scoring as the same fabrication operation.
        GerberLayerRole::Score => fabrication_line_layer_output("Score.gbr", &["Vcut"], side),
    }
}

fn assembly_drawing_layer_output(
    source_layer_name: &str,
    side: Option<IpcSide>,
) -> (String, Vec<String>) {
    let fallback_stem = match side {
        Some(IpcSide::Top) => "F_Fab",
        Some(IpcSide::Bottom) => "B_Fab",
        _ => "Assembly",
    };
    let filename = drawing_filename(source_layer_name, fallback_stem);
    let file_function = match side {
        Some(IpcSide::Top) => vec!["AssemblyDrawing".into(), "Top".into()],
        Some(IpcSide::Bottom) => vec!["AssemblyDrawing".into(), "Bot".into()],
        _ => vec!["OtherDrawing".into(), "Assembly".into()],
    };
    (filename, file_function)
}

fn drawing_filename(source_layer_name: &str, fallback_stem: &str) -> String {
    let source_stem = sanitize_filename_stem(source_layer_name);
    format!(
        "{}.gbr",
        if source_stem.is_empty() {
            fallback_stem
        } else {
            &source_stem
        }
    )
}

fn fabrication_line_layer_output(
    filename: &str,
    function: &[&str],
    side: Option<IpcSide>,
) -> (String, Vec<String>) {
    let mut file_function = function
        .iter()
        .map(|field| (*field).to_string())
        .collect::<Vec<_>>();
    match side {
        Some(IpcSide::Top) => file_function.push("Top".to_string()),
        Some(IpcSide::Bottom) => file_function.push("Bot".to_string()),
        Some(IpcSide::Both)
        | Some(IpcSide::All)
        | Some(IpcSide::None)
        | Some(IpcSide::Internal)
        | None => {}
    }
    (filename.to_string(), file_function)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GerberPart {
    Single,
    Array,
    FabricationPanel,
}

impl GerberPart {
    fn as_str(self) -> &'static str {
        match self {
            Self::Single => "Single",
            Self::Array => "Array",
            Self::FabricationPanel => "FabricationPanel",
        }
    }
}

fn primary_step(imported: &ImportedDesign) -> Result<&Step> {
    imported
        .content
        .step_refs
        .first()
        .and_then(|step_ref| imported.steps.iter().find(|step| step.name == *step_ref))
        .or_else(|| imported.steps.first())
        .context("IPC-2581 ECAD section has no Step")
}

fn gerber_part_for_ipc_view(imported: &ImportedDesign, view: ArtworkScope) -> Result<GerberPart> {
    let step = primary_step(imported)?;
    Ok(
        if view == ArtworkScope::Board || !geometry::is_panel_step(step) {
            GerberPart::Single
        } else if imported.resolve(step.name) == crate::steps::FAB_PANEL_STEP_NAME {
            GerberPart::FabricationPanel
        } else {
            GerberPart::Array
        },
    )
}

fn layer_attributes(
    file_function: Vec<String>,
    part: GerberPart,
    role: GerberLayerRole,
) -> LayerAttributes {
    LayerAttributes {
        file_function,
        part: Some(vec![part.as_str().to_string()]),
        // Solder mask artwork represents openings (absence of mask); all
        // other exported physical images represent the material itself.
        file_polarity: Some(
            if role == GerberLayerRole::Soldermask {
                "Negative"
            } else {
                "Positive"
            }
            .to_string(),
        ),
        // Every manufacturing layer is emitted in the source IPC coordinate
        // system, without per-file shifts or bottom-side mirroring.
        same_coordinates: Some(Vec::new()),
    }
}

fn copper_layer_output(
    side: Option<IpcSide>,
    copper_index: usize,
    copper_count: usize,
) -> (String, Vec<String>) {
    let side_field = match side {
        Some(IpcSide::Top) => "Top",
        Some(IpcSide::Bottom) => "Bot",
        _ => "Inr",
    };
    let filename = match side {
        Some(IpcSide::Top) => "F_Cu.gtl".to_string(),
        Some(IpcSide::Bottom) => "B_Cu.gbl".to_string(),
        // KiCad numbers inner layers from 1, excluding the top layer.
        _ => format!("In{}_Cu.gbr", copper_index - 1),
    };
    let index = match side {
        Some(IpcSide::Top) => 1,
        Some(IpcSide::Bottom) => copper_count,
        _ => copper_index,
    };
    (
        filename,
        vec![
            "Copper".to_string(),
            format!("L{index}"),
            side_field.to_string(),
        ],
    )
}

fn artwork_from_ipc_layer(
    imported: &ImportedDesign,
    doc: &IpcGeometryDocument,
    layer_index: usize,
    spec: GerberArtworkSpec,
    accuracy: GeometryAccuracy,
) -> Result<GerberArtwork> {
    let layer = &doc.layers[layer_index];
    let header = pcb_ir::dialects::artwork::Layer {
        name: layer.name.clone(),
        role: spec.role.ir_role(),
        side: spec.side,
        objects: Span::EMPTY,
        bbox: layer.bbox,
        meta: spec.meta,
    };
    let mut lowering = GerberLowering {
        imported,
        doc,
        role: spec.role,
        side: spec.side,
    };
    let mut artwork =
        lower_layer_to_artwork_with(doc, layer_index, header, &mut lowering, accuracy)?;

    if spec.role == GerberLayerRole::Profile
        && spec.view != ArtworkScope::ArrayFlattened
        && artwork.layers[0].objects.is_empty()
    {
        append_profile_occurrences(
            &mut artwork,
            0,
            doc,
            spec.view.profile_set(),
            ProfileGerberStyle::default(),
            accuracy,
        )?;
    }
    Ok(artwork)
}

/// Preserve the reusable IPC Step graph as reusable artwork blocks.
///
/// Each source Step is normalized and lowered exactly once in local
/// coordinates. The root Step's geometry lands directly on the layer while
/// repeated child Steps become transformed block instances, so a board
/// repeated into an assembly panel and that panel repeated into a fabrication
/// panel remains a semantic hierarchy — and a Step without repeats lowers to
/// plain flat artwork.
fn hierarchical_artwork_from_ipc_layer(
    imported: &ImportedDesign,
    layer: LayerId,
    layer_name: &str,
    spec: GerberArtworkSpec,
    accuracy: GeometryAccuracy,
) -> Result<GerberArtwork> {
    let root = primary_step(imported)?;
    let mut artwork = GerberArtwork::new();
    let artwork_layer = artwork.push_layer(pcb_ir::dialects::artwork::Layer {
        name: layer_name.to_string(),
        role: spec.role.ir_role(),
        side: spec.side,
        objects: Span::EMPTY,
        bbox: BBox::empty(),
        meta: spec.meta,
    });
    let context = HierarchicalArtworkContext {
        imported,
        layer,
        layer_name,
        role: spec.role,
        side: spec.side,
    };
    let mut blocks = HashMap::from([(root.name, None)]);
    let objects = build_step_artwork_objects(&context, root, &mut artwork, &mut blocks, accuracy)?;
    for object in objects {
        artwork.push_object(artwork_layer, object);
    }
    pcb_ir::dialects::artwork::normalize_bounds(&mut artwork);
    artwork.validate().map_err(|error| {
        anyhow::anyhow!("invalid hierarchical artwork for '{layer_name}': {error}")
    })?;
    Ok(artwork)
}

struct HierarchicalArtworkContext<'a> {
    imported: &'a ImportedDesign,
    layer: LayerId,
    layer_name: &'a str,
    role: GerberLayerRole,
    side: IrSide,
}

fn build_step_artwork_block(
    context: &HierarchicalArtworkContext<'_>,
    step: &Step,
    artwork: &mut GerberArtwork,
    blocks: &mut HashMap<ipc2581::Symbol, Option<u32>>,
    accuracy: GeometryAccuracy,
) -> Result<u32> {
    match blocks.get(&step.name).copied() {
        Some(Some(block)) => return Ok(block),
        Some(None) => bail!(
            "StepRepeat cycle references Step '{}'",
            context.imported.resolve(step.name)
        ),
        None => {}
    }
    blocks.insert(step.name, None);
    let objects = build_step_artwork_objects(context, step, artwork, blocks, accuracy)?;
    let block = artwork.push_block();
    for object in objects {
        artwork.push_block_object(block, object);
    }
    blocks.insert(step.name, Some(block));
    Ok(block)
}

fn build_step_artwork_objects(
    context: &HierarchicalArtworkContext<'_>,
    step: &Step,
    artwork: &mut GerberArtwork,
    blocks: &mut HashMap<ipc2581::Symbol, Option<u32>>,
    accuracy: GeometryAccuracy,
) -> Result<Vec<ArtworkObject<ObjectAttributes>>> {
    let children = step
        .step_repeats
        .iter()
        .map(|repeat| {
            let child_step = context
                .imported
                .steps
                .iter()
                .find(|candidate| candidate.name == repeat.step_ref)
                .with_context(|| {
                    format!(
                        "StepRepeat references unknown Step '{}'",
                        context.imported.resolve(repeat.step_ref)
                    )
                })?;
            let child = build_step_artwork_block(context, child_step, artwork, blocks, accuracy)?;
            Ok((child, repeat))
        })
        .collect::<Result<Vec<_>>>()?;

    let step_id = context
        .imported
        .step_id(step.name)
        .context("source Step is missing from the canonical layout graph")?;
    let mut local = context
        .imported
        .materialize_step_layer(step_id, context.layer, accuracy)
        .with_context(|| {
            format!(
                "failed to materialize IPC-2581 Step '{}' layer '{}'",
                context.imported.resolve(step.name),
                context.layer_name
            )
        })?;
    pcb_ir::dialects::ipc::process::normalize_for_artwork(&mut local, accuracy)?;
    if let Err(error) = pcb_ir::dialects::ipc::validate_artwork_ready(&local) {
        bail!(
            "IPC-2581 Step '{}' layer '{}' is not artwork-ready: {error}",
            context.imported.resolve(step.name),
            context.layer_name
        );
    }

    let mut lowering = GerberLowering {
        imported: context.imported,
        doc: &local,
        role: context.role,
        side: context.side,
    };
    let mut objects =
        lower_layer_to_artwork_objects_with(&local, 0, artwork, &mut lowering, accuracy)?;
    for (child, repeat) in children {
        if artwork.blocks[child as usize].objects.is_empty() || repeat.nx == 0 || repeat.ny == 0 {
            continue;
        }
        if repeat.nx > 1 || repeat.ny > 1 {
            objects.push(ArtworkObject::new(
                Polarity::Dark,
                ArtworkGeometry::GridInstance {
                    block: child,
                    transform: geometry::step_repeat_transform(repeat, 0, 0),
                    repeat: GridRepeat {
                        x_count: repeat.nx,
                        y_count: repeat.ny,
                        x_step: Point::new(repeat.dx, 0.0),
                        y_step: Point::new(0.0, repeat.dy),
                    },
                },
            ));
        } else {
            objects.push(ArtworkObject::new(
                Polarity::Dark,
                ArtworkGeometry::Instance {
                    block: child,
                    transform: geometry::step_repeat_transform(repeat, 0, 0),
                },
            ));
        }
    }
    Ok(objects)
}

/// Gerber's source-specific half of IPC artwork lowering: standard-dictionary
/// primitives flash through standard apertures, traces are round-joined, and
/// every object carries X2 attributes.
struct GerberLowering<'a> {
    imported: &'a ImportedDesign,
    doc: &'a IpcGeometryDocument,
    role: GerberLayerRole,
    side: IrSide,
}

impl ArtworkLowering<ipc2581::Symbol, ObjectAttributes> for GerberLowering<'_> {
    fn source_aperture(
        &mut self,
        feature: &Feature<ipc2581::Symbol>,
        accuracy: GeometryAccuracy,
    ) -> std::result::Result<Option<(Aperture, Affine2, BBox)>, AccuracyError> {
        if let Some(void) = feature.flags.copper_balance_void {
            let aperture = Aperture::solid(ApertureShape::RoundedHex {
                radius: void.radius_mm,
                corner_radius: void.corner_radius_mm,
                rotation_degrees: 0.0,
            });
            let bbox = aperture.bbox().transformed(feature.transform);
            return Ok(Some((aperture, feature.transform, bbox)));
        }
        standard_flash_aperture(self.imported, self.doc, feature, accuracy)
    }

    fn stroke_style(&mut self, stroke: StrokeStyle) -> StrokeStyle {
        StrokeStyle {
            join: LineJoin::Round,
            ..stroke
        }
    }

    /// Gerber orders removals rather than imaging them as clears, so every
    /// drilled or routed feature stages last regardless of its bucket.
    fn paint_order(&mut self, feature: &Feature<ipc2581::Symbol>) -> PaintOrder {
        let stage = if feature.intent.role == FeatureRole::Cutout || feature.is_drill_like() {
            PaintStage::FinalCutout
        } else if feature.polarity == Polarity::Clear
            || feature.flags.clears_previous_in_set
            || feature.bucket == FeatureBucket::Fill
        {
            PaintStage::Base
        } else {
            PaintStage::Overlay
        };
        PaintOrder { stage }
    }

    fn object_meta(
        &mut self,
        feature: &Feature<ipc2581::Symbol>,
        _kind: ArtworkObjectKind,
    ) -> ObjectAttributes {
        object_attributes(
            self.imported,
            self.doc,
            feature,
            self.role,
            self.side,
            aperture_function(feature, self.role, self.side),
        )
    }
}

struct GerberArtworkSpec {
    role: GerberLayerRole,
    side: IrSide,
    meta: LayerAttributes,
    view: ArtworkScope,
}

fn synthetic_profile_gerber_file(
    imported: &ImportedDesign,
    view: ArtworkScope,
    accuracy: GeometryAccuracy,
) -> Result<Option<GerberX2File>> {
    let doc = &imported.geometry;
    let mut artwork = GerberArtwork::new();
    let artwork_layer = artwork.push_layer(pcb_ir::dialects::artwork::Layer {
        name: "Edge.Cuts".to_string(),
        role: LayerRole::Profile,
        side: IrSide::None,
        objects: Span::EMPTY,
        bbox: BBox::empty(),
        meta: layer_attributes(
            vec!["Profile".to_string(), "NP".to_string()],
            gerber_part_for_ipc_view(imported, view)?,
            GerberLayerRole::Profile,
        ),
    });
    append_profile_occurrences(
        &mut artwork,
        artwork_layer,
        doc,
        view.profile_set(),
        ProfileGerberStyle::default(),
        accuracy,
    )?;
    if artwork.layers[artwork_layer as usize].objects.is_empty() {
        return Ok(None);
    }

    let layer = lower_artwork_layer(&artwork, accuracy)?;
    let contents = write_layer(&layer)?;
    Ok(Some(GerberX2File {
        filename: "Edge_Cuts.gm1".to_string(),
        layer,
        contents,
    }))
}

fn board_array_profile_gerber_files(
    imported: &ImportedDesign,
    relief_debug_dir: Option<&Path>,
    accuracy: GeometryAccuracy,
) -> Result<Vec<GerberX2File>> {
    let doc = &imported.geometry;
    let score_lines = geometry::board_array_vscore_lines_from_design(imported, accuracy)?;
    #[cfg(not(feature = "cli"))]
    if relief_debug_dir.is_some() {
        bail!("filesystem debug output requires the cli feature");
    }
    #[cfg(feature = "cli")]
    let profile = if let Some(debug_dir) = relief_debug_dir {
        let (profile, relief_debug) =
            geometry::board_array_fabrication_profile_from_design_with_debug(
                imported,
                doc,
                &score_lines,
                accuracy,
            )?;
        write_vscore_relief_debug(debug_dir, &relief_debug)?;
        profile
    } else {
        geometry::board_array_fabrication_profile_from_design(
            imported,
            doc,
            &score_lines,
            accuracy,
        )?
    };
    #[cfg(not(feature = "cli"))]
    let profile = geometry::board_array_fabrication_profile_from_design(
        imported,
        doc,
        &score_lines,
        accuracy,
    )?;
    if profile.purpose == LayoutPurpose::Product {
        let mut contour_groups = profile.array_outlines;
        contour_groups.push(profile.material_removal);
        return Ok(profile_gerber_file(
            "Board Array Profile",
            "Board_Array_Profile.gm1",
            contour_groups,
            GerberPart::Array,
            accuracy,
        )?
        .into_iter()
        .collect());
    }

    Ok([
        profile_gerber_file(
            "Fab Panel Outline",
            "Fab_Panel_Outline.gm1",
            profile.array_outlines,
            GerberPart::FabricationPanel,
            accuracy,
        )?,
        profile_gerber_file(
            "Assembly Panel Outlines",
            "Assembly_Panel_Outlines.gm1",
            profile.assembly_panel_outlines,
            GerberPart::FabricationPanel,
            accuracy,
        )?,
        profile_gerber_file(
            "Board Cutouts",
            "Board_Cutouts.gm1",
            vec![profile.material_removal],
            GerberPart::FabricationPanel,
            accuracy,
        )?,
    ]
    .into_iter()
    .flatten()
    .collect())
}

fn profile_gerber_file(
    layer_name: &str,
    filename: &str,
    contour_groups: Vec<Vec<ContourBuf>>,
    part: GerberPart,
    accuracy: GeometryAccuracy,
) -> Result<Option<GerberX2File>> {
    let mut artwork = GerberArtwork::new();
    let artwork_layer = artwork.push_layer(pcb_ir::dialects::artwork::Layer {
        name: layer_name.to_string(),
        role: LayerRole::Profile,
        side: IrSide::None,
        objects: Span::EMPTY,
        bbox: BBox::empty(),
        meta: layer_attributes(
            vec!["Profile".to_string(), "NP".to_string()],
            part,
            GerberLayerRole::Profile,
        ),
    });
    let style = ProfileGerberStyle::default();
    for contours in contour_groups
        .into_iter()
        .filter(|contours| !contours.is_empty())
    {
        append_profile_payloads(&mut artwork, artwork_layer, contours, style);
    }
    if artwork.layers[artwork_layer as usize].objects.is_empty() {
        return Ok(None);
    }

    let layer = lower_artwork_layer(&artwork, accuracy)?;
    let contents = write_layer(&layer)?;
    Ok(Some(GerberX2File {
        filename: filename.to_string(),
        layer,
        contents,
    }))
}

#[cfg(feature = "cli")]
fn write_vscore_relief_debug(output_dir: &Path, debug: &relief::VScoreReliefDebug) -> Result<()> {
    let Some(svg) = render_vscore_relief_debug_svg(debug) else {
        return Ok(());
    };
    fs::create_dir_all(output_dir).with_context(|| {
        format!(
            "failed to create V-score relief debug directory {}",
            output_dir.display()
        )
    })?;
    let output = output_dir.join("vscore-reliefs.svg");
    fs::write(&output, svg).with_context(|| {
        format!(
            "failed to write V-score relief debug SVG {}",
            output.display()
        )
    })
}

#[cfg(feature = "cli")]
fn render_vscore_relief_debug_svg(debug: &relief::VScoreReliefDebug) -> Option<String> {
    if debug.entries.is_empty() {
        return None;
    }

    let bbox = debug
        .entries
        .iter()
        .fold(BBox::empty(), |bbox, entry| {
            bbox.union(payloads_bbox(&entry.board_boundary))
                .union(entry.score_cell.bbox)
                .union(payloads_bbox(&entry.dead_space_pockets))
                .union(payloads_bbox(&entry.legal_tool_centers))
                .union(payloads_bbox(&entry.relief_contours))
        })
        .union(payloads_bbox(&debug.merged_relief_contours));
    if bbox.is_empty() {
        return None;
    }

    let padding = 2.0;
    let mut svg = String::new();
    writeln!(
        svg,
        "<svg xmlns='http://www.w3.org/2000/svg' viewBox='{} {} {} {}' data-vscore-relief-debug='true'>",
        debug_num(bbox.min.x - padding),
        debug_num(-(bbox.max.y + padding)),
        debug_num(bbox.width() + 2.0 * padding),
        debug_num(bbox.height() + 2.0 * padding)
    )
    .unwrap();
    writeln!(
        svg,
        "  <rect x='{}' y='{}' width='{}' height='{}' fill='#ffffff'/>",
        debug_num(bbox.min.x - padding),
        debug_num(-(bbox.max.y + padding)),
        debug_num(bbox.width() + 2.0 * padding),
        debug_num(bbox.height() + 2.0 * padding)
    )
    .unwrap();
    writeln!(svg, "  <g transform='scale(1 -1)'>").unwrap();

    for (index, entry) in debug.entries.iter().enumerate() {
        write_debug_path(
            &mut svg,
            index,
            std::slice::from_ref(&entry.score_cell),
            DebugSvgPathStyle {
                class_name: "score-cell",
                fill: "none",
                stroke: "#64748b",
                stroke_width: "0.08",
                extra_attrs: "stroke-dasharray='0.6 0.6'",
            },
        );
        write_debug_path(
            &mut svg,
            index,
            &entry.board_boundary,
            DebugSvgPathStyle {
                class_name: "board-boundary",
                fill: "none",
                stroke: "#064e3b",
                stroke_width: "0.08",
                extra_attrs: "",
            },
        );
        write_debug_path(
            &mut svg,
            index,
            &entry.dead_space_pockets,
            DebugSvgPathStyle {
                class_name: "dead-space-pocket",
                fill: "#f59e0b",
                stroke: "#f59e0b",
                stroke_width: "0.05",
                extra_attrs: "fill-opacity='0.18'",
            },
        );
        write_debug_path(
            &mut svg,
            index,
            &entry.legal_tool_centers,
            DebugSvgPathStyle {
                class_name: "legal-tool-center",
                fill: "#2563eb",
                stroke: "#1d4ed8",
                stroke_width: "0.05",
                extra_attrs: "fill-opacity='0.16'",
            },
        );
        write_debug_path(
            &mut svg,
            index,
            &entry.relief_contours,
            DebugSvgPathStyle {
                class_name: "relief-contour",
                fill: "none",
                stroke: "#dc2626",
                stroke_width: "0.1",
                extra_attrs: "",
            },
        );
    }
    write_debug_path(
        &mut svg,
        debug.entries.len(),
        &debug.merged_relief_contours,
        DebugSvgPathStyle {
            class_name: "merged-relief-contour",
            fill: "none",
            stroke: "#7c3aed",
            stroke_width: "0.14",
            extra_attrs: "",
        },
    );

    writeln!(svg, "  </g>").unwrap();
    writeln!(svg, "</svg>").unwrap();
    Some(svg)
}

#[derive(Debug, Clone, Copy)]
#[cfg(feature = "cli")]
struct DebugSvgPathStyle {
    class_name: &'static str,
    fill: &'static str,
    stroke: &'static str,
    stroke_width: &'static str,
    extra_attrs: &'static str,
}

#[cfg(feature = "cli")]
fn write_debug_path(
    svg: &mut String,
    entry_index: usize,
    payloads: &[ContourBuf],
    style: DebugSvgPathStyle,
) {
    let Some(path_data) = debug_path_data(payloads) else {
        return;
    };
    writeln!(
        svg,
        "    <path class='{}' data-entry='{entry_index}' d='{path_data}' fill='{}' stroke='{}' stroke-width='{}' {} fill-rule='evenodd'/>",
        style.class_name, style.fill, style.stroke, style.stroke_width, style.extra_attrs
    )
    .unwrap();
}

#[cfg(feature = "cli")]
fn debug_path_data(payloads: &[ContourBuf]) -> Option<String> {
    let mut data = String::new();
    for payload in payloads {
        append_debug_path_cmds(&mut data, &payload.cmds);
    }
    (!data.is_empty()).then_some(data)
}

#[cfg(feature = "cli")]
fn append_debug_path_cmds(data: &mut String, cmds: &[PathCmd]) {
    let mut current = Point::default();
    for cmd in cmds {
        match cmd.op {
            PathOp::MoveTo => {
                current = cmd.p0;
                if !data.is_empty() {
                    data.push(' ');
                }
                write!(data, "M{} {}", debug_num(cmd.p0.x), debug_num(cmd.p0.y)).unwrap();
            }
            PathOp::LineTo => {
                current = cmd.p0;
                write!(data, " L{} {}", debug_num(cmd.p0.x), debug_num(cmd.p0.y)).unwrap();
            }
            PathOp::ArcTo => {
                write_debug_arc(data, current, cmd.p0, cmd.p1, cmd.clockwise);
                current = cmd.p0;
            }
            PathOp::CubicTo => {
                current = cmd.p2;
                write!(
                    data,
                    " C{} {},{} {},{} {}",
                    debug_num(cmd.p0.x),
                    debug_num(cmd.p0.y),
                    debug_num(cmd.p1.x),
                    debug_num(cmd.p1.y),
                    debug_num(cmd.p2.x),
                    debug_num(cmd.p2.y)
                )
                .unwrap();
            }
            PathOp::Close => data.push_str(" Z"),
        }
    }
}

#[cfg(feature = "cli")]
fn write_debug_arc(data: &mut String, start: Point, end: Point, center: Point, clockwise: bool) {
    let radius = start.distance_to(center);
    if radius <= 1e-9 {
        write!(data, " L{} {}", debug_num(end.x), debug_num(end.y)).unwrap();
        return;
    }

    let sweep_flag = if clockwise { 0 } else { 1 };
    if start.distance_to(end) <= 1e-9 {
        let midpoint = Point::new(2.0 * center.x - start.x, 2.0 * center.y - start.y);
        write_debug_svg_arc(data, radius, 0, sweep_flag, midpoint);
        write_debug_svg_arc(data, radius, 0, sweep_flag, end);
        return;
    }

    let large_arc =
        u8::from(Arc::new(start, end, center, clockwise).sweep_radians() > std::f64::consts::PI);
    write_debug_svg_arc(data, radius, large_arc, sweep_flag, end);
}

#[cfg(feature = "cli")]
fn write_debug_svg_arc(data: &mut String, radius: f64, large_arc: u8, sweep_flag: u8, end: Point) {
    write!(
        data,
        " A{} {} 0 {large_arc} {sweep_flag} {} {}",
        debug_num(radius),
        debug_num(radius),
        debug_num(end.x),
        debug_num(end.y)
    )
    .unwrap();
}

#[cfg(feature = "cli")]
fn payloads_bbox(payloads: &[ContourBuf]) -> BBox {
    payloads
        .iter()
        .fold(BBox::empty(), |bbox, payload| bbox.union(payload.bbox))
}

#[cfg(feature = "cli")]
fn debug_num(value: f64) -> String {
    let mut text = format!("{value:.6}");
    while text.contains('.') && text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    if text == "-0" { "0".to_string() } else { text }
}

fn append_profile_occurrences(
    artwork: &mut GerberArtwork,
    layer: u32,
    doc: &IpcGeometryDocument,
    profile_set: ProfileSet,
    style: ProfileGerberStyle,
    accuracy: GeometryAccuracy,
) -> anyhow::Result<()> {
    let _: () = for occurrence in profile_occurrences_for(doc, profile_set) {
        append_profile_path(
            artwork,
            layer,
            doc,
            occurrence.profile.outer_path,
            occurrence.transform,
            style,
            accuracy,
        )?;
        append_profile_cutouts(
            artwork,
            layer,
            doc,
            occurrence.profile,
            occurrence.transform,
            style,
            accuracy,
        )?;
    };
    Ok(())
}

fn append_profile_cutouts(
    artwork: &mut GerberArtwork,
    layer: u32,
    doc: &IpcGeometryDocument,
    profile: &pcb_ir::dialects::ipc::StepProfile,
    transform: Affine2,
    style: ProfileGerberStyle,
    accuracy: GeometryAccuracy,
) -> anyhow::Result<()> {
    let _: () = for cutout in profile.cutouts.slice(&doc.profile_cutouts) {
        append_profile_path(artwork, layer, doc, cutout.path, transform, style, accuracy)?;
    };
    Ok(())
}

fn append_profile_path(
    artwork: &mut GerberArtwork,
    layer: u32,
    doc: &IpcGeometryDocument,
    path: u32,
    transform: Affine2,
    style: ProfileGerberStyle,
    accuracy: GeometryAccuracy,
) -> anyhow::Result<()> {
    append_profile_payloads(
        artwork,
        layer,
        doc.transformed_path_contours(path, transform, accuracy)?,
        style,
    );

    Ok(())
}

fn append_profile_payloads(
    artwork: &mut GerberArtwork,
    layer: u32,
    payloads: Vec<ContourBuf>,
    style: ProfileGerberStyle,
) {
    let path = artwork.push_path(
        Paint::Stroke(StrokeStyle {
            width: style.stroke_width_mm,
            cap: style.line_cap,
            join: style.line_join,
            pattern: LinePattern::Solid,
        }),
        payloads,
    );
    let bbox = artwork.path_bbox(path);
    artwork.push_object(
        layer,
        ArtworkObject {
            polarity: Polarity::Dark,
            order: PaintOrder {
                stage: PaintStage::Overlay,
            },
            geometry: ArtworkGeometry::Stroke { path },
            bbox,
            meta: ObjectAttributes {
                aperture_function: Some(vec!["Profile".to_string()]),
                ..ObjectAttributes::default()
            },
        },
    );
}

fn ir_side(side: Option<IpcSide>) -> IrSide {
    match side {
        Some(IpcSide::Top) => IrSide::Top,
        Some(IpcSide::Bottom) => IrSide::Bottom,
        _ => IrSide::None,
    }
}

fn standard_flash_aperture(
    imported: &ImportedDesign,
    doc: &IpcGeometryDocument,
    feature: &Feature<ipc2581::Symbol>,
    accuracy: GeometryAccuracy,
) -> std::result::Result<Option<(Aperture, Affine2, BBox)>, AccuracyError> {
    if !standard_flash_feature_is_eligible(feature) {
        return Ok(None);
    }

    let Some(primitive) = standard_primitive_for_feature(imported, feature) else {
        return Ok(None);
    };
    if !standard_primitive_is_solid_fill(primitive) {
        return Ok(None);
    }

    if let Some(aperture) = exact_flash_aperture(primitive, feature.transform) {
        let at = feature.center;
        let bbox = flash_bbox(at, &aperture);
        return Ok(Some((aperture, Affine2::translation(at), bbox)));
    }

    // Every other solid catalogue shape flashes through a contour aperture
    // shared per shape, keeping repeated pads one definition each instead of
    // re-painting a region at every placement.
    let Some(shape) = pcb_ir::dialects::ipc::contour_flash_aperture(doc, feature, accuracy)? else {
        return Ok(None);
    };
    Ok(Some((
        Aperture::solid(shape),
        feature.transform,
        feature.bbox,
    )))
}

/// The catalogue primitives Gerber expresses as exact standard apertures.
fn exact_flash_aperture(primitive: &StandardPrimitive, transform: Affine2) -> Option<Aperture> {
    let aperture = match primitive {
        StandardPrimitive::Circle(circle) => {
            let scale = uniform_scale(transform)?;
            Aperture::solid(ApertureShape::Circle {
                diameter: circle.shape.diameter * scale,
            })
        }
        StandardPrimitive::RectCenter(rect) => {
            let (width, height) =
                axis_aligned_size(transform, rect.shape.size.width, rect.shape.size.height)?;
            Aperture::solid(ApertureShape::Rectangle { width, height })
        }
        StandardPrimitive::Oval(oval) => {
            let (width, height) =
                axis_aligned_size(transform, oval.shape.size.width, oval.shape.size.height)?;
            Aperture::solid(ApertureShape::Obround { width, height })
        }
        StandardPrimitive::RectRound(rect) => {
            let corners = [
                rect.shape.upper_right,
                rect.shape.upper_left,
                rect.shape.lower_right,
                rect.shape.lower_left,
            ];
            if corners.iter().any(|rounded| !rounded) {
                return None;
            }
            let (width, height) =
                axis_aligned_size(transform, rect.shape.size.width, rect.shape.size.height)?;
            let radius = rect.shape.radius * uniform_scale(transform)?;
            if radius <= 0.0 {
                Aperture::solid(ApertureShape::Rectangle { width, height })
            } else {
                Aperture::solid(ApertureShape::RoundRect {
                    width,
                    height,
                    radius,
                })
            }
        }
        StandardPrimitive::Hexagon(hexagon) => {
            regular_polygon_aperture(6, hexagon.shape.point_to_point, transform)?
        }
        StandardPrimitive::Octagon(octagon) => {
            regular_polygon_aperture(8, octagon.shape.point_to_point, transform)?
        }
        _ => return None,
    };
    Some(aperture)
}

/// IPC hexagons and octagons place their first vertex pointing down (-90°);
/// a rigid rotation folds into the Gerber polygon aperture's own rotation,
/// while mirrored placements keep the contour fallback.
fn regular_polygon_aperture(
    vertices: u32,
    point_to_point: f64,
    transform: Affine2,
) -> Option<Aperture> {
    let scale = uniform_scale(transform)?;
    let determinant = transform.m00 * transform.m11 - transform.m01 * transform.m10;
    if determinant <= 0.0 {
        return None;
    }
    let rotation_degrees = transform.m10.atan2(transform.m00).to_degrees() - 90.0;
    Some(Aperture::solid(ApertureShape::Polygon {
        diameter: point_to_point * scale,
        vertices,
        rotation_degrees,
    }))
}

fn standard_flash_feature_is_eligible(feature: &Feature<ipc2581::Symbol>) -> bool {
    feature.polarity == Polarity::Dark
        && !feature.paths.is_empty()
        && (matches!(
            feature.intent.role,
            FeatureRole::Pad | FeatureRole::Via | FeatureRole::Hole
        ) || feature.is_fiducial())
}

fn standard_primitive_for_feature<'a>(
    imported: &'a ImportedDesign,
    feature: &Feature<ipc2581::Symbol>,
) -> Option<&'a StandardPrimitive> {
    let Some(PrimitiveRef::Standard(primitive_ref)) = feature.primitive_ref else {
        return None;
    };
    imported
        .content
        .dictionary_standard
        .entries
        .iter()
        .find(|entry| entry.id == primitive_ref)
        .map(|entry| &entry.primitive)
}

fn standard_primitive_is_solid_fill(primitive: &StandardPrimitive) -> bool {
    matches!(
        standard_primitive_fill_property(primitive),
        None | Some(FillProperty::Fill)
    )
}

fn standard_primitive_fill_property(primitive: &StandardPrimitive) -> Option<FillProperty> {
    match primitive {
        StandardPrimitive::Circle(styled) => styled.fill_property,
        StandardPrimitive::RectCenter(styled) => styled.fill_property,
        StandardPrimitive::RectRound(styled) => styled.fill_property,
        StandardPrimitive::RectCham(styled) => styled.fill_property,
        StandardPrimitive::RectCorner(styled) => styled.fill_property,
        StandardPrimitive::Oval(styled) => styled.fill_property,
        StandardPrimitive::Butterfly(styled) => styled.fill_property,
        StandardPrimitive::Diamond(styled) => styled.fill_property,
        StandardPrimitive::Donut(styled) => styled.fill_property,
        StandardPrimitive::Ellipse(styled) => styled.fill_property,
        StandardPrimitive::Hexagon(styled) => styled.fill_property,
        StandardPrimitive::Octagon(styled) => styled.fill_property,
        StandardPrimitive::Thermal(styled) => styled.fill_property,
        StandardPrimitive::Triangle(styled) => styled.fill_property,
        StandardPrimitive::Moire(_) | StandardPrimitive::Contour(_) => None,
    }
}

fn uniform_scale(transform: Affine2) -> Option<f64> {
    let sx = transform.m00.hypot(transform.m10);
    let sy = transform.m01.hypot(transform.m11);
    let dot = transform.m00 * transform.m01 + transform.m10 * transform.m11;
    if sx <= GEOMETRY_EPSILON
        || sy <= GEOMETRY_EPSILON
        || !nearly_equal(sx, sy)
        || dot.abs() > GEOMETRY_EPSILON * sx.max(sy).max(1.0)
    {
        return None;
    }
    Some((sx + sy) / 2.0)
}

fn axis_aligned_size(transform: Affine2, width: f64, height: f64) -> Option<(f64, f64)> {
    let sx = transform.m00.hypot(transform.m10);
    let sy = transform.m01.hypot(transform.m11);
    if sx <= GEOMETRY_EPSILON || sy <= GEOMETRY_EPSILON {
        return None;
    }

    if transform.m10.abs() <= GEOMETRY_EPSILON && transform.m01.abs() <= GEOMETRY_EPSILON {
        return Some((width * sx, height * sy));
    }
    if transform.m00.abs() <= GEOMETRY_EPSILON && transform.m11.abs() <= GEOMETRY_EPSILON {
        return Some((height * sy, width * sx));
    }
    None
}

fn flash_bbox(at: Point, aperture: &Aperture) -> BBox {
    let local = aperture.bbox();
    BBox::new(at + local.min, at + local.max)
}

const GEOMETRY_EPSILON: f64 = 1e-9;

fn nearly_equal(left: f64, right: f64) -> bool {
    (left - right).abs() <= GEOMETRY_EPSILON * left.abs().max(right.abs()).max(1.0)
}

fn object_attributes(
    imported: &ImportedDesign,
    doc: &IpcGeometryDocument,
    feature: &Feature<ipc2581::Symbol>,
    role: GerberLayerRole,
    side: IrSide,
    aperture_function: Option<Vec<String>>,
) -> ObjectAttributes {
    let pin_ref = feature.pin_refs.slice(&doc.pin_refs).first();
    let carries_netlist = role == GerberLayerRole::Copper;
    let carries_pins = carries_netlist && matches!(side, IrSide::Top | IrSide::Bottom);
    // Only pad-like copper and full balance voids may image as flashes.
    let keeps_flashes = match feature.flags.copper_balance {
        Some(kind) => kind == CopperBalanceKind::FullVoid,
        None => matches!(
            feature.bucket,
            FeatureBucket::Smd | FeatureBucket::Pth | FeatureBucket::Via | FeatureBucket::Fiducial
        ),
    };
    ObjectAttributes {
        aperture_function,
        lower_flashes_to_regions: role == GerberLayerRole::Copper && !keeps_flashes,
        net: if carries_netlist {
            feature
                .net
                .map(|symbol| imported.resolve(symbol).to_string())
        } else {
            None
        },
        component: pin_ref
            .and_then(|pin_ref| pin_ref.component_ref)
            .map(|symbol| imported.resolve(symbol).to_string()),
        pin: if carries_pins {
            pin_ref.map(|pin_ref| imported.resolve(pin_ref.pin).to_string())
        } else {
            None
        },
    }
}

fn aperture_function(
    feature: &Feature<ipc2581::Symbol>,
    role: GerberLayerRole,
    side: IrSide,
) -> Option<Vec<String>> {
    match role {
        GerberLayerRole::Soldermask | GerberLayerRole::Paste | GerberLayerRole::Legend => {
            return Some(vec!["Material".to_string()]);
        }
        GerberLayerRole::AssemblyDrawing | GerberLayerRole::FabricationDrawing => return None,
        GerberLayerRole::Profile => return Some(vec!["Profile".to_string()]),
        GerberLayerRole::Vcut => {
            return Some(vec!["Other".to_string(), "Vcut".to_string()]);
        }
        GerberLayerRole::Score => {
            return Some(vec!["Other".to_string(), "Score".to_string()]);
        }
        GerberLayerRole::Copper => {}
    }

    if feature.flags.copper_balance.is_some() {
        return Some(vec!["CopperBalancing".to_string()]);
    }

    match feature.intent.operation {
        FeatureOperation::Drill => {
            return Some(vec!["Other".to_string(), "Drill".to_string()]);
        }
        FeatureOperation::Score if feature.is_vcut() => {
            return Some(vec!["Other".to_string(), "Vcut".to_string()]);
        }
        FeatureOperation::Score if feature.is_score() => {
            return Some(vec!["Other".to_string(), "Score".to_string()]);
        }
        FeatureOperation::Route | FeatureOperation::Profile => {
            return Some(vec!["Profile".to_string()]);
        }
        _ => {}
    }

    match feature.intent.role {
        _ if feature.is_fiducial() => return Some(fiducial_aperture_function(feature)),
        FeatureRole::Pad => {
            return match feature.intent.plating {
                PlatingKind::Plated => Some(vec!["ComponentPad".to_string()]),
                PlatingKind::Via | PlatingKind::ViaCapped => Some(vec!["ViaPad".to_string()]),
                _ if matches!(side, IrSide::Top | IrSide::Bottom) => {
                    Some(vec!["SMDPad".to_string(), "CuDef".to_string()])
                }
                _ if !feature.pin_refs.is_empty() => Some(vec!["ComponentPad".to_string()]),
                _ => Some(vec!["OtherPad".to_string(), "InnerLayerPad".to_string()]),
            };
        }
        FeatureRole::Via => return Some(vec!["ViaPad".to_string()]),
        FeatureRole::Conductor => return Some(vec!["Conductor".to_string()]),
        FeatureRole::Hole => {
            return Some(vec!["Other".to_string(), "Hole".to_string()]);
        }
        FeatureRole::Slot => {
            return Some(vec!["Other".to_string(), "Slot".to_string()]);
        }
        FeatureRole::Cutout => {
            return Some(vec!["Other".to_string(), "Cutout".to_string()]);
        }
        FeatureRole::ArraySeparation if feature.is_vcut() => {
            return Some(vec!["Other".to_string(), "Vcut".to_string()]);
        }
        FeatureRole::ArraySeparation if feature.is_score() => {
            return Some(vec!["Other".to_string(), "Score".to_string()]);
        }
        FeatureRole::Route | FeatureRole::BoardOutline => {
            return Some(vec!["Profile".to_string()]);
        }
        _ => {}
    }

    Some(match feature.intent.domain {
        FeatureDomain::Copper => vec!["Conductor".to_string()],
        FeatureDomain::Drill => vec!["Other".to_string(), "Drill".to_string()],
        FeatureDomain::Rout | FeatureDomain::Profile => vec!["Profile".to_string()],
        FeatureDomain::VCut => vec!["Other".to_string(), "Vcut".to_string()],
        FeatureDomain::Score => vec!["Other".to_string(), "Score".to_string()],
        FeatureDomain::Soldermask
        | FeatureDomain::Paste
        | FeatureDomain::Legend
        | FeatureDomain::Mechanical
        | FeatureDomain::Other
        | FeatureDomain::Unknown => {
            vec!["OtherCopper".to_string(), "Unclassified".to_string()]
        }
    })
}

fn fiducial_aperture_function(feature: &Feature<ipc2581::Symbol>) -> Vec<String> {
    let kind = match feature.fiducial_kind {
        FiducialKind::Unknown => "Global",
        FiducialKind::Local => "Local",
        FiducialKind::Global => "Global",
        FiducialKind::Panel | FiducialKind::GoodPanel => "Panel",
        FiducialKind::BadBoard => {
            return vec!["OtherPad".to_string(), "BadBoardMark".to_string()];
        }
    };
    vec!["FiducialPad".to_string(), kind.to_string()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc2581 as ipc;
    #[cfg(feature = "cli")]
    use crate::manufacturing::{ManufacturingExportOptions, export_manufacturing_package};
    use crate::manufacturing::{ManufacturingFileKind, build_manufacturing_package};
    #[cfg(feature = "cli")]
    use std::io::{Cursor, Read};

    #[test]
    fn negative_set_before_a_later_fill_is_repainted_by_it() {
        let accuracy = GeometryAccuracy::default();

        // Sequential set semantics: a fill written after the clear repaints
        // the cleared area, so the fill survives intact.
        let ipc = ipc::Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <LayerRef name="TOP"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <Datum x="0" y="0"/>
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="10" y="0"/>
            <PolyStepSegment x="10" y="10"/>
            <PolyStepSegment x="0" y="10"/>
            <PolyStepSegment x="0" y="0"/>
          </Polygon>
        </Profile>
        <LayerFeature layerRef="TOP">
          <Set polarity="NEGATIVE">
            <Features>
              <UserSpecial>
                <Contour>
                  <Polygon>
                    <PolyBegin x="4" y="4"/>
                    <PolyStepSegment x="6" y="4"/>
                    <PolyStepSegment x="6" y="6"/>
                    <PolyStepSegment x="4" y="6"/>
                    <PolyStepSegment x="4" y="4"/>
                  </Polygon>
                </Contour>
              </UserSpecial>
            </Features>
          </Set>
          <Set>
            <Features>
              <UserSpecial>
                <Contour>
                  <Polygon>
                    <PolyBegin x="2" y="2"/>
                    <PolyStepSegment x="8" y="2"/>
                    <PolyStepSegment x="8" y="8"/>
                    <PolyStepSegment x="2" y="8"/>
                    <PolyStepSegment x="2" y="2"/>
                  </Polygon>
                </Contour>
              </UserSpecial>
            </Features>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();
        let files = build_gerber_x2_files(&ipc, ArtworkScope::Board, accuracy).unwrap();
        let copper = files
            .iter()
            .find(|file| file.filename == "F_Cu.gtl")
            .unwrap();
        let parsed = gerberx2::GerberX2::parse(&copper.contents).unwrap();

        let mask = pcb_ir::dialects::artwork::compose_to_mask(
            &gerberx2::geometry::extract_document(&parsed, accuracy).unwrap(),
            accuracy,
        )
        .unwrap();
        let mut rings = Vec::new();
        for layer in &mask.layers {
            for shape in mask.shapes(layer) {
                rings.extend(
                    pcb_ir::geom::ContourSet::from_contours(
                        &mask.arena.path_contours(shape),
                        pcb_ir::geom::FillRule::NonZero,
                        0.0,
                        accuracy,
                    )
                    .unwrap()
                    .rings,
                );
            }
        }
        let copper_area = pcb_ir::geom::ContourSet::new(
            rings,
            pcb_ir::geom::FillRule::NonZero,
            pcb_ir::geom::tol::REGION_MM,
        )
        .area();
        // The 6x6 fill paints after the clear and survives whole.
        let expected = 36.0;
        assert!(
            (copper_area - expected).abs() <= expected * 0.01,
            "expected intact fill area {expected:.2}, got {copper_area:.2}"
        );
    }

    #[test]
    fn catalogue_pads_flash_through_shared_apertures() {
        let accuracy = GeometryAccuracy::default();

        let ipc = ipc::Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <LayerRef name="TOP"/>
    <DictionaryStandard units="MILLIMETER">
      <EntryStandard id="pad"><RectRound width="2" height="1" radius="0.25" upperRight="true" upperLeft="true" lowerRight="true" lowerLeft="true"/></EntryStandard>
    </DictionaryStandard>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <Datum x="0" y="0"/>
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="20" y="0"/>
            <PolyStepSegment x="20" y="20"/>
            <PolyStepSegment x="0" y="20"/>
            <PolyStepSegment x="0" y="0"/>
          </Polygon>
        </Profile>
        <PadStackDef name="padstack">
          <PadstackPadDef layerRef="TOP" padUse="REGULAR">
            <StandardPrimitiveRef id="pad"/>
          </PadstackPadDef>
        </PadStackDef>
        <LayerFeature layerRef="TOP">
          <Set net="N1">
            <Pad padstackDefRef="padstack">
              <Location x="5" y="5"/>
              <StandardPrimitiveRef id="pad"/>
            </Pad>
            <Pad padstackDefRef="padstack">
              <Location x="5" y="15"/>
              <StandardPrimitiveRef id="pad"/>
            </Pad>
            <Pad padstackDefRef="padstack">
              <Xform rotation="45"/>
              <Location x="15" y="10"/>
              <StandardPrimitiveRef id="pad"/>
            </Pad>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();
        let files = build_gerber_x2_files(&ipc, ArtworkScope::Board, accuracy).unwrap();
        let copper = files
            .iter()
            .find(|file| file.filename == "F_Cu.gtl")
            .unwrap();
        assert_eq!(copper.contents.matches("D03*").count(), 3);
        assert!(
            !copper.contents.contains("G36*"),
            "catalogue pads must flash, not flatten to regions"
        );
        assert!(
            copper.contents.matches("%ADD").count() <= 2,
            "repeated orientations share aperture definitions"
        );
        assert!(!copper.contents.contains("%AMRoundedRect*"));
        assert!(!copper.contents.lines().any(|line| line.starts_with("21,")));
        assert!(!copper.contents.contains("%LR"));
        assert!(copper.contents.lines().any(|line| line.starts_with("4,")));

        let parsed = gerberx2::GerberX2::parse(&copper.contents).unwrap();
        let mask = pcb_ir::dialects::artwork::compose_to_mask(
            &gerberx2::geometry::extract_document(&parsed, accuracy).unwrap(),
            accuracy,
        )
        .unwrap();
        let mut rings = Vec::new();
        for layer in &mask.layers {
            for shape in mask.shapes(layer) {
                rings.extend(
                    pcb_ir::geom::ContourSet::from_contours(
                        &mask.arena.path_contours(shape),
                        pcb_ir::geom::FillRule::NonZero,
                        0.0,
                        accuracy,
                    )
                    .unwrap()
                    .rings,
                );
            }
        }
        let copper_area = pcb_ir::geom::ContourSet::new(
            rings,
            pcb_ir::geom::FillRule::NonZero,
            pcb_ir::geom::tol::REGION_MM,
        )
        .area();
        let corner_deficit = 0.25 * 0.25 * (4.0 - std::f64::consts::PI);
        let expected = 3.0 * (2.0 - corner_deficit);
        assert!(
            (copper_area - expected).abs() <= expected * 0.02,
            "expected three roundrect pads with area {expected:.4}, got {copper_area:.4}"
        );
    }

    #[test]
    fn oversized_corner_radius_clamps_to_the_obround_image() {
        let accuracy = GeometryAccuracy::default();

        let ipc = ipc::Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <LayerRef name="TOP"/>
    <DictionaryStandard units="MILLIMETER">
      <EntryStandard id="pad"><RectRound width="2" height="1" radius="0.75" upperRight="true" upperLeft="true" lowerRight="true" lowerLeft="true"/></EntryStandard>
    </DictionaryStandard>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <Datum x="0" y="0"/>
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="10" y="0"/>
            <PolyStepSegment x="10" y="10"/>
            <PolyStepSegment x="0" y="10"/>
            <PolyStepSegment x="0" y="0"/>
          </Polygon>
        </Profile>
        <PadStackDef name="padstack">
          <PadstackPadDef layerRef="TOP" padUse="REGULAR">
            <StandardPrimitiveRef id="pad"/>
          </PadstackPadDef>
        </PadStackDef>
        <LayerFeature layerRef="TOP">
          <Set net="N1">
            <Pad padstackDefRef="padstack">
              <Location x="5" y="5"/>
              <StandardPrimitiveRef id="pad"/>
            </Pad>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();
        let files = build_gerber_x2_files(&ipc, ArtworkScope::Board, accuracy).unwrap();
        let copper = files
            .iter()
            .find(|file| file.filename == "F_Cu.gtl")
            .unwrap();
        let parsed = gerberx2::GerberX2::parse(&copper.contents).unwrap();
        let mask = pcb_ir::dialects::artwork::compose_to_mask(
            &gerberx2::geometry::extract_document(&parsed, accuracy).unwrap(),
            accuracy,
        )
        .unwrap();
        let mut rings = Vec::new();
        for layer in &mask.layers {
            for shape in mask.shapes(layer) {
                rings.extend(
                    pcb_ir::geom::ContourSet::from_contours(
                        &mask.arena.path_contours(shape),
                        pcb_ir::geom::FillRule::NonZero,
                        0.0,
                        accuracy,
                    )
                    .unwrap()
                    .rings,
                );
            }
        }
        let copper_area = pcb_ir::geom::ContourSet::new(
            rings,
            pcb_ir::geom::FillRule::NonZero,
            pcb_ir::geom::tol::REGION_MM,
        )
        .area();
        // The radius clamps to height / 2, so the pad images as a 2x1 obround.
        let clamped = 0.5;
        let expected = 2.0 * 1.0 - clamped * clamped * (4.0 - std::f64::consts::PI);
        assert!(
            (copper_area - expected).abs() <= expected * 0.02,
            "expected clamped obround area {expected:.4}, got {copper_area:.4}"
        );
    }

    #[test]
    fn negative_set_after_an_overlay_pad_erases_it_natively() {
        let accuracy = GeometryAccuracy::default();

        // Sequential set semantics: the clear paints after the pad, erasing
        // the overlap, and exports natively as clear polarity.
        let ipc = ipc::Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <LayerRef name="TOP"/>
    <DictionaryStandard units="MILLIMETER">
      <EntryStandard id="pad"><Circle diameter="2"/></EntryStandard>
    </DictionaryStandard>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <Datum x="0" y="0"/>
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="10" y="0"/>
            <PolyStepSegment x="10" y="10"/>
            <PolyStepSegment x="0" y="10"/>
            <PolyStepSegment x="0" y="0"/>
          </Polygon>
        </Profile>
        <PadStackDef name="padstack">
          <PadstackPadDef layerRef="TOP" padUse="REGULAR">
            <StandardPrimitiveRef id="pad"/>
          </PadstackPadDef>
        </PadStackDef>
        <LayerFeature layerRef="TOP">
          <Set net="N1">
            <Pad padstackDefRef="padstack">
              <Location x="5" y="5"/>
              <StandardPrimitiveRef id="pad"/>
            </Pad>
          </Set>
          <Set polarity="NEGATIVE">
            <Features>
              <UserSpecial>
                <Contour>
                  <Polygon>
                    <PolyBegin x="4" y="4"/>
                    <PolyStepSegment x="5" y="4"/>
                    <PolyStepSegment x="5" y="5"/>
                    <PolyStepSegment x="4" y="5"/>
                    <PolyStepSegment x="4" y="4"/>
                  </Polygon>
                </Contour>
              </UserSpecial>
            </Features>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();
        let files = build_gerber_x2_files(&ipc, ArtworkScope::Board, accuracy).unwrap();
        let copper = files
            .iter()
            .find(|file| file.filename == "F_Cu.gtl")
            .unwrap();
        let parsed = gerberx2::GerberX2::parse(&copper.contents).unwrap();
        assert!(
            parsed
                .objects()
                .iter()
                .any(|object| object.polarity == Polarity::Clear),
            "the clear set should export natively as clear polarity"
        );

        let mask = pcb_ir::dialects::artwork::compose_to_mask(
            &gerberx2::geometry::extract_document(&parsed, accuracy).unwrap(),
            accuracy,
        )
        .unwrap();
        let mut rings = Vec::new();
        for layer in &mask.layers {
            for shape in mask.shapes(layer) {
                rings.extend(
                    pcb_ir::geom::ContourSet::from_contours(
                        &mask.arena.path_contours(shape),
                        pcb_ir::geom::FillRule::NonZero,
                        0.0,
                        accuracy,
                    )
                    .unwrap()
                    .rings,
                );
            }
        }
        let copper_area = pcb_ir::geom::ContourSet::new(
            rings,
            pcb_ir::geom::FillRule::NonZero,
            pcb_ir::geom::tol::REGION_MM,
        )
        .area();
        let expected = std::f64::consts::PI * (1.0 - 0.25);
        assert!(
            (copper_area - expected).abs() <= expected * 0.02,
            "expected quarter-cleared pad area {expected:.4}, got {copper_area:.4}"
        );
    }

    #[test]
    fn standard_dictionary_fiducials_keep_exact_circle_apertures() {
        let accuracy = GeometryAccuracy::default();

        // Repeated references to a standard catalogue entry are exact
        // primitives, not user-dictionary instances: they must flash as
        // circle apertures rather than flatten into outline macros.
        let ipc = ipc::Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <LayerRef name="TOP"/>
    <DictionaryStandard units="MILLIMETER">
      <EntryStandard id="fid"><Circle diameter="1"/></EntryStandard>
    </DictionaryStandard>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <Datum x="0" y="0"/>
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="10" y="0"/>
            <PolyStepSegment x="10" y="10"/>
            <PolyStepSegment x="0" y="10"/>
            <PolyStepSegment x="0" y="0"/>
          </Polygon>
        </Profile>
        <LayerFeature layerRef="TOP">
          <Set>
            <LocalFiducial>
              <Location x="3" y="3"/>
              <StandardPrimitiveRef id="fid"/>
            </LocalFiducial>
            <LocalFiducial>
              <Location x="7" y="7"/>
              <StandardPrimitiveRef id="fid"/>
            </LocalFiducial>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();
        let files = build_gerber_x2_files(&ipc, ArtworkScope::Board, accuracy).unwrap();
        let copper = files
            .iter()
            .find(|file| file.filename == "F_Cu.gtl")
            .unwrap();
        assert!(
            !copper.contents.contains("%AM"),
            "catalogue circles must not lower to outline macros"
        );
        assert!(
            copper.contents.contains("C,1"),
            "fiducials should flash through a shared circle aperture"
        );
    }

    #[test]
    fn drill_and_route_layers_are_not_exported_as_gerber_layers() {
        let accuracy = GeometryAccuracy::default();

        let ipc = ipc::Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner"><FunctionMode mode="FABRICATION"/></Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="Edge.Cuts" layerFunction="BOARD_OUTLINE" side="ALL"/>
      <Layer name="Drill" layerFunction="DRILL" side="ALL"/>
      <Layer name="F.Cu_B.Cu_1" layerFunction="ROUT" side="ALL"/>
      <Step name="board" type="BOARD"/>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();
        let imported = import_design(&ipc, accuracy).unwrap();

        let filenames = export_layer_plans(&imported, &imported.layer_definitions)
            .into_iter()
            .map(|plan| plan.filename)
            .collect::<Vec<_>>();

        assert_eq!(filenames, ["Edge_Cuts.gm1"]);
    }

    #[test]
    fn assembly_layers_use_source_names_and_valid_gerber_x2_functions() {
        let accuracy = GeometryAccuracy::default();

        let ipc = ipc::Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner"><FunctionMode mode="FABRICATION"/></Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="F.Fab" layerFunction="ASSEMBLY" side="TOP"/>
      <Layer name="B.Fab" layerFunction="ASSEMBLY" side="BOTTOM"/>
      <Layer name="Assembly Notes" layerFunction="ASSEMBLY" side="NONE"/>
      <Layer name="Board Fab" layerFunction="BOARD_FAB" side="ALL"/>
      <Step name="board" type="BOARD"/>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();

        let imported = import_design(&ipc, accuracy).unwrap();
        let plans = export_layer_plans(&imported, &imported.layer_definitions);
        let outputs = plans
            .iter()
            .map(|plan| (plan.filename.as_str(), plan.file_function.as_slice()))
            .collect::<Vec<_>>();

        assert_eq!(
            outputs,
            [
                (
                    "F_Fab.gbr",
                    ["AssemblyDrawing".to_string(), "Top".to_string()].as_slice()
                ),
                (
                    "B_Fab.gbr",
                    ["AssemblyDrawing".to_string(), "Bot".to_string()].as_slice()
                ),
                (
                    "Assembly_Notes.gbr",
                    ["OtherDrawing".to_string(), "Assembly".to_string()].as_slice()
                ),
                (
                    "Board_Fab.gbr",
                    ["FabricationDrawing".to_string()].as_slice()
                ),
            ]
        );
    }

    #[test]
    fn assembly_gerbers_preserve_phantom_patterns_for_boards_and_arrays() {
        let accuracy = GeometryAccuracy::default();

        let ipc = ipc::Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="panel"/>
    <LayerRef name="F.Fab"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="F.Fab" layerFunction="ASSEMBLY" side="TOP" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <LayerFeature layerRef="F.Fab">
          <Set>
            <Features>
              <Line startX="0" startY="0" endX="20" endY="0">
                <LineDesc lineWidth="1" lineEnd="ROUND" lineProperty="PHANTOM"/>
              </Line>
            </Features>
          </Set>
        </LayerFeature>
      </Step>
      <Step name="panel" type="PALLET">
        <StepRepeat stepRef="board" x="0" y="0" nx="2" ny="1" dx="30" dy="0"/>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();

        let board_files = build_gerber_x2_files(&ipc, ArtworkScope::Board, accuracy).unwrap();
        let board_fab = board_files
            .iter()
            .find(|file| file.filename == "F_Fab.gbr")
            .unwrap();
        assert!(
            board_fab
                .contents
                .contains("%TF.FileFunction,AssemblyDrawing,Top*%")
        );
        assert!(board_fab.contents.contains("%TF.Part,Single*%"));
        let parsed = gerberx2::GerberX2::parse(&board_fab.contents).unwrap();
        assert_eq!(
            parsed
                .objects()
                .iter()
                .filter(|object| matches!(object.kind, gerberx2::ObjectKind::Draw { .. }))
                .count(),
            2
        );
        assert_eq!(
            parsed
                .objects()
                .iter()
                .filter(|object| matches!(object.kind, gerberx2::ObjectKind::Flash { .. }))
                .count(),
            2
        );

        let array_files =
            build_gerber_x2_files(&ipc, ArtworkScope::ArrayFlattened, accuracy).unwrap();
        let array_fab = array_files
            .iter()
            .find(|file| file.filename == "F_Fab.gbr")
            .unwrap();
        assert!(array_fab.contents.contains("%TF.Part,Array*%"));
        assert!(!array_fab.contents.contains("%ABD"));
        assert!(array_fab.contents.contains("%SRX2Y1I30J0*%"));
        let parsed = gerberx2::GerberX2::parse(&array_fab.contents).unwrap();
        assert_eq!(parsed.objects().len(), 8);
        let artwork = gerberx2::geometry::extract_document(&parsed, accuracy).unwrap();
        assert!(artwork.blocks.is_empty());
        assert_eq!(artwork.objects.len(), 8);
    }

    #[test]
    fn repeated_fabrication_layer_roles_export_to_unique_filenames() {
        let accuracy = GeometryAccuracy::default();

        let ipc = ipc::Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner"><FunctionMode mode="FABRICATION"/></Content>
      <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="ROUT-A" layerFunction="ROUT" side="ALL"/>
      <Layer name="ROUT-B" layerFunction="ROUT" side="ALL"/>
      <Layer name="VCUT-A" layerFunction="V_CUT" side="NONE"/>
      <Layer name="VCUT-B" layerFunction="V_CUT" side="NONE"/>
      <Layer name="SCORE-A" layerFunction="SCORE" side="NONE"/>
      <Layer name="SCORE-B" layerFunction="SCORE" side="NONE"/>
      <Step name="board" type="BOARD"/>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();
        let imported = import_design(&ipc, accuracy).unwrap();

        let filenames = export_layer_plans(&imported, &imported.layer_definitions)
            .into_iter()
            .map(|plan| plan.filename)
            .collect::<Vec<_>>();
        let unique = filenames.iter().collect::<HashSet<_>>();

        assert_eq!(unique.len(), filenames.len());
        assert_eq!(
            filenames,
            ["V_Cut.gbr", "VCUT_B.gbr", "Score.gbr", "SCORE_B.gbr"]
        );
    }

    #[test]
    fn gerber_export_renders_step_profile_only_as_canonical_edge_cuts() {
        let accuracy = GeometryAccuracy::default();

        let ipc = ipc::Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <LayerRef name="Edge.Cuts"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="Edge.Cuts" layerFunction="BOARD_OUTLINE" side="ALL" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="10" y="0"/>
            <PolyStepSegment x="10" y="5"/>
            <PolyStepSegment x="0" y="5"/>
            <PolyStepSegment x="0" y="0"/>
          </Polygon>
        </Profile>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();

        let files = build_gerber_x2_files(&ipc, ArtworkScope::Board, accuracy).unwrap();
        let edge_cuts = files
            .iter()
            .find(|file| file.filename == "Edge_Cuts.gm1")
            .unwrap();

        assert!(edge_cuts.contents.contains("%TF.FileFunction,Profile,NP*%"));
        assert!(edge_cuts.contents.contains("%TA.AperFunction,Profile*%"));
        assert!(edge_cuts.contents.contains("%ADD10C,0.05*%"));
        gerberx2::GerberX2::parse(&edge_cuts.contents).unwrap();
    }

    #[test]
    fn standalone_profile_export_matches_both_layout_targets() {
        let accuracy = GeometryAccuracy::default();

        for outline_layer in [
            "",
            r#"<Layer name="Edge.Cuts" layerFunction="BOARD_OUTLINE" side="ALL" polarity="POSITIVE"/>"#,
        ] {
            let ipc = ipc::Ipc2581::parse(&format!(
                r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner"><FunctionMode mode="FABRICATION"/><StepRef name="board"/></Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      {outline_layer}
      <Step name="board" type="BOARD">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="10" y="0"/>
            <PolyStepSegment x="10" y="5"/>
            <PolyStepSegment x="0" y="5"/>
            <PolyStepSegment x="0" y="0"/>
          </Polygon>
          <Cutout>
            <PolyBegin x="2" y="2"/>
            <PolyStepSegment x="3" y="2"/>
            <PolyStepSegment x="3" y="3"/>
            <PolyStepSegment x="2" y="3"/>
            <PolyStepSegment x="2" y="2"/>
          </Cutout>
        </Profile>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
            ))
            .unwrap();
            let board = build_manufacturing_package(&ipc, ArtworkScope::Board, accuracy).unwrap();
            let array =
                build_manufacturing_package(&ipc, ArtworkScope::ArrayFlattened, accuracy).unwrap();
            assert_eq!(
                board
                    .files
                    .iter()
                    .map(|f| (&f.filename, &f.contents))
                    .collect::<Vec<_>>(),
                array
                    .files
                    .iter()
                    .map(|f| (&f.filename, &f.contents))
                    .collect::<Vec<_>>(),
            );
            let profile = &array
                .files
                .iter()
                .find(|f| f.filename == "Edge_Cuts.gm1")
                .unwrap()
                .contents;
            assert!(profile.contains("%TF.FileFunction,Profile,NP*%"));
            assert!(profile.contains("%TF.Part,Single*%"));
            let parsed = gerberx2::GerberX2::parse(profile).unwrap();
            let geometry = gerberx2::geometry::extract_document(&parsed, accuracy).unwrap();
            geometry.validate().unwrap();
            assert_eq!(geometry.layers[0].objects.count, 8);
        }
    }

    #[test]
    fn exports_ipc_layer_to_parseable_gerber_x2() {
        let accuracy = GeometryAccuracy::default();

        let ipc = ipc::Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <LayerRef name="TOP"/>
    <DictionaryStandard units="MILLIMETER">
      <EntryStandard id="pad"><Circle diameter="1"/></EntryStandard>
    </DictionaryStandard>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="10" y="0"/>
            <PolyStepSegment x="10" y="10"/>
            <PolyStepSegment x="0" y="0"/>
          </Polygon>
        </Profile>
        <PadStackDef name="padstack">
          <PadstackPadDef layerRef="TOP" padUse="REGULAR">
            <StandardPrimitiveRef id="pad"/>
          </PadstackPadDef>
        </PadStackDef>
        <LayerFeature layerRef="TOP">
          <Set net="N1">
            <Pad padstackDefRef="padstack">
              <Location x="2" y="3"/>
              <StandardPrimitiveRef id="pad"/>
              <PinRef componentRef="U1" pin="1"/>
            </Pad>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();
        let files = build_gerber_x2_files(&ipc, ArtworkScope::Board, accuracy).unwrap();

        assert!(files.iter().any(|file| file.filename == "F_Cu.gtl"));
        for file in &files {
            gerberx2::GerberX2::parse(&file.contents).unwrap();
        }
        let copper = files
            .iter()
            .find(|file| file.filename == "F_Cu.gtl")
            .unwrap();
        assert!(copper.contents.contains("%TF.FileFunction,Copper,L1,Top*%"));
        assert!(copper.contents.contains("%TF.Part,Single*%"));
        assert!(copper.contents.contains("%TF.FilePolarity,Positive*%"));
        assert!(copper.contents.contains("%TF.SameCoordinates*%"));
        assert!(copper.contents.contains("%TA.AperFunction,SMDPad,CuDef*%"));
        assert!(copper.contents.contains("%TO.C,U1*%"));
        assert!(copper.contents.contains("%TO.P,U1,1*%"));
        assert!(copper.contents.contains("%TO.N,N1*%"));

        let parsed = gerberx2::GerberX2::parse(&copper.contents).unwrap();
        assert!(
            parsed
                .objects()
                .iter()
                .any(|object| matches!(object.kind, gerberx2::ObjectKind::Flash { .. }))
        );

        let panel_target_files =
            build_gerber_x2_files(&ipc, ArtworkScope::ArrayFlattened, accuracy).unwrap();

        let panel_target_copper = panel_target_files
            .iter()
            .find(|file| file.filename == "F_Cu.gtl")
            .unwrap();
        assert!(panel_target_copper.contents.contains("%TF.Part,Single*%"));
        assert!(!panel_target_copper.contents.contains("%TF.Part,Array*%"));
    }

    #[test]
    fn mask_and_paste_use_specification_correct_attributes() {
        let accuracy = GeometryAccuracy::default();

        let ipc = ipc::Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <LayerRef name="F.Mask"/>
    <LayerRef name="F.Paste"/>
    <DictionaryStandard units="MILLIMETER">
      <EntryStandard id="pad"><Circle diameter="1"/></EntryStandard>
    </DictionaryStandard>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="F.Mask" layerFunction="SOLDERMASK" side="TOP" polarity="POSITIVE"/>
      <Layer name="F.Paste" layerFunction="SOLDERPASTE" side="TOP" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <PadStackDef name="padstack">
          <PadstackPadDef layerRef="F.Mask" padUse="REGULAR">
            <StandardPrimitiveRef id="pad"/>
          </PadstackPadDef>
          <PadstackPadDef layerRef="F.Paste" padUse="REGULAR">
            <StandardPrimitiveRef id="pad"/>
          </PadstackPadDef>
        </PadStackDef>
        <LayerFeature layerRef="F.Mask">
          <Set net="N1">
            <Pad padstackDefRef="padstack">
              <Location x="2" y="3"/>
              <StandardPrimitiveRef id="pad"/>
              <PinRef componentRef="U1" pin="1"/>
            </Pad>
          </Set>
        </LayerFeature>
        <LayerFeature layerRef="F.Paste">
          <Set net="N1">
            <Pad padstackDefRef="padstack">
              <Location x="2" y="3"/>
              <StandardPrimitiveRef id="pad"/>
              <PinRef componentRef="U1" pin="1"/>
            </Pad>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();
        let files = build_gerber_x2_files(&ipc, ArtworkScope::Board, accuracy).unwrap();

        let mask = files
            .iter()
            .find(|file| file.filename == "F_Mask.gts")
            .unwrap();
        assert!(mask.contents.contains("%TF.FilePolarity,Negative*%"));
        assert!(mask.contents.contains("%TF.SameCoordinates*%"));
        assert!(mask.contents.contains("%TA.AperFunction,Material*%"));
        assert!(mask.contents.contains("%TO.C,U1*%"));
        assert!(!mask.contents.contains("SMDPad"));
        assert!(!mask.contents.contains("%TO.P,"));
        assert!(!mask.contents.contains("%TO.N,"));
        gerberx2::GerberX2::parse(&mask.contents).unwrap();

        let paste = files
            .iter()
            .find(|file| file.filename == "F_Paste.gtp")
            .unwrap();
        assert!(paste.contents.contains("%TF.FilePolarity,Positive*%"));
        assert!(paste.contents.contains("%TF.SameCoordinates*%"));
        assert!(paste.contents.contains("%TA.AperFunction,Material*%"));
        assert!(paste.contents.contains("%TO.C,U1*%"));
        assert!(!paste.contents.contains("SMDPad"));
        assert!(!paste.contents.contains("%TO.P,"));
        assert!(!paste.contents.contains("%TO.N,"));
        gerberx2::GerberX2::parse(&paste.contents).unwrap();
    }

    #[test]
    fn gerber_export_places_pad_flashes_after_local_fill_cut_ins() {
        let accuracy = GeometryAccuracy::default();

        let ipc = ipc::Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <LayerRef name="TOP"/>
    <DictionaryStandard units="MILLIMETER">
      <EntryStandard id="pad"><Circle diameter="1"/></EntryStandard>
    </DictionaryStandard>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <PadStackDef name="padstack">
          <PadstackPadDef layerRef="TOP" padUse="REGULAR">
            <StandardPrimitiveRef id="pad"/>
          </PadstackPadDef>
        </PadStackDef>
        <LayerFeature layerRef="TOP">
          <Set net="N1">
            <Pad padstackDefRef="padstack">
              <Location x="5" y="5"/>
              <StandardPrimitiveRef id="pad"/>
            </Pad>
            <Features>
              <UserSpecial>
                <Contour>
                  <Polygon>
                    <PolyBegin x="0" y="0"/>
                    <PolyStepSegment x="10" y="0"/>
                    <PolyStepSegment x="10" y="10"/>
                    <PolyStepSegment x="0" y="10"/>
                    <PolyStepSegment x="0" y="0"/>
                  </Polygon>
                </Contour>
                <Contour>
                  <Polygon>
                    <PolyBegin x="4" y="4"/>
                    <PolyStepSegment x="6" y="4"/>
                    <PolyStepSegment x="6" y="6"/>
                    <PolyStepSegment x="4" y="6"/>
                    <PolyStepSegment x="4" y="4"/>
                  </Polygon>
                </Contour>
              </UserSpecial>
            </Features>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();
        let files = build_gerber_x2_files(&ipc, ArtworkScope::Board, accuracy).unwrap();

        let copper = files
            .iter()
            .find(|file| file.filename == "F_Cu.gtl")
            .unwrap();
        let parsed = gerberx2::GerberX2::parse(&copper.contents).unwrap();
        assert!(
            parsed
                .objects()
                .iter()
                .all(|object| object.polarity == Polarity::Dark),
            "positive compound region holes should not export as layer-global clear regions"
        );
        let region_index = parsed
            .objects()
            .iter()
            .position(|object| {
                object.polarity == Polarity::Dark
                    && matches!(object.kind, gerberx2::ObjectKind::Region { .. })
            })
            .expect("compound fill should export as a dark local cut-in region");
        let pad_flash_index = parsed
            .objects()
            .iter()
            .position(|object| matches!(object.kind, gerberx2::ObjectKind::Flash { .. }))
            .expect("standard circular pad should export as a flash");
        assert!(region_index < pad_flash_index);

        let geometry = gerberx2::geometry::extract_document(&parsed, accuracy).unwrap();
        let summary = pcb_ir::dialects::artwork::compare::summarize(&geometry, accuracy).unwrap();
        assert!(
            summary.area_mm2 > 96.7,
            "pad flash was not restored after local clear; area was {}",
            summary.area_mm2
        );
    }

    #[test]
    fn gerber_export_places_multi_contour_traces_after_local_fill_cut_ins() {
        let accuracy = GeometryAccuracy::default();

        let ipc = ipc::Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <LayerRef name="TOP"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <LayerFeature layerRef="TOP">
          <Set net="TRACE">
            <Features>
              <Line startX="4.2" startY="4.6" endX="5.8" endY="4.6">
                <LineDesc lineWidth="0.5" lineEnd="ROUND"/>
              </Line>
            </Features>
            <Features>
              <Line startX="4.2" startY="5.4" endX="5.8" endY="5.4">
                <LineDesc lineWidth="0.5" lineEnd="ROUND"/>
              </Line>
            </Features>
          </Set>
          <Set>
            <Features>
              <UserSpecial>
                <Contour>
                  <Polygon>
                    <PolyBegin x="0" y="0"/>
                    <PolyStepSegment x="10" y="0"/>
                    <PolyStepSegment x="10" y="10"/>
                    <PolyStepSegment x="0" y="10"/>
                    <PolyStepSegment x="0" y="0"/>
                  </Polygon>
                </Contour>
                <Contour>
                  <Polygon>
                    <PolyBegin x="4" y="4"/>
                    <PolyStepSegment x="6" y="4"/>
                    <PolyStepSegment x="6" y="6"/>
                    <PolyStepSegment x="4" y="6"/>
                    <PolyStepSegment x="4" y="4"/>
                  </Polygon>
                </Contour>
              </UserSpecial>
            </Features>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();
        let files = build_gerber_x2_files(&ipc, ArtworkScope::Board, accuracy).unwrap();

        let copper = files
            .iter()
            .find(|file| file.filename == "F_Cu.gtl")
            .unwrap();
        assert!(
            !copper.contents.contains("%LPC*%"),
            "positive compound region holes should not export as layer-global clear regions"
        );
        let fill_end_index = copper
            .contents
            .find("G37*")
            .expect("compound fill should export as a region");
        let trace_index = copper
            .contents
            .find("%TO.N,TRACE*%")
            .expect("multi-contour trace should keep its net attribute");
        assert!(fill_end_index < trace_index);

        let parsed = gerberx2::GerberX2::parse(&copper.contents).unwrap();
        let geometry = gerberx2::geometry::extract_document(&parsed, accuracy).unwrap();
        let summary = pcb_ir::dialects::artwork::compare::summarize(&geometry, accuracy).unwrap();
        assert!(
            summary.area_mm2 > 97.0,
            "multi-contour trace was not restored after local clear; area was {}",
            summary.area_mm2
        );
    }

    #[test]
    fn gerber_export_writes_separate_nc_drill_files_with_routes() {
        let accuracy = GeometryAccuracy::default();

        let ipc = ipc::Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <LayerRef name="TOP"/>
    <LayerRef name="BOTTOM"/>
    <LayerRef name="DRILL"/>
    <LayerRef name="ROUTE"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Layer name="BOTTOM" layerFunction="SIGNAL" side="BOTTOM" polarity="POSITIVE"/>
      <Layer name="DRILL" layerFunction="DRILL" side="ALL" polarity="POSITIVE">
        <Span fromLayer="TOP" toLayer="BOTTOM"/>
      </Layer>
      <Layer name="ROUTE" layerFunction="ROUT" side="ALL" polarity="POSITIVE">
        <Span fromLayer="TOP" toLayer="BOTTOM"/>
      </Layer>
      <Step name="board" type="BOARD">
        <LayerFeature layerRef="DRILL">
          <Set net="GND">
            <Hole name="V1" diameter="0.3" platingStatus="VIA" plusTol="0" minusTol="0" x="1" y="2"/>
          </Set>
          <Set>
            <Hole name="N1" diameter="0.65" platingStatus="NONPLATED" plusTol="0" minusTol="0" x="3" y="4"/>
          </Set>
        </LayerFeature>
        <LayerFeature layerRef="ROUTE">
          <Set net="GND">
            <SlotCavity name="S1" platingStatus="PLATED" plusTol="0" minusTol="0">
              <Location x="10" y="20"/>
              <Xform rotation="90"/>
              <Oval width="1.7" height="0.6"/>
            </SlotCavity>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();
        let package = build_manufacturing_package(&ipc, ArtworkScope::Board, accuracy).unwrap();

        assert!(
            !package
                .files
                .iter()
                .any(|file| file.filename == "Drill.gbr")
        );
        assert!(
            !package
                .files
                .iter()
                .any(|file| file.filename == "Route.gbr")
        );
        assert!(
            !package
                .files
                .iter()
                .any(|file| file.filename == "Edge_Cuts.gm1")
        );
        let pth = package
            .files
            .iter()
            .find(|file| file.filename == "PTH.drl")
            .unwrap();
        let npth = package
            .files
            .iter()
            .find(|file| file.filename == "NPTH.drl")
            .unwrap();
        assert!(
            !package
                .files
                .iter()
                .any(|file| file.filename == "PTH_Slots.drl")
        );

        assert!(matches!(pth.kind, ManufacturingFileKind::Xnc));
        assert!(
            pth.contents
                .contains("; #@! TF.FileFunction,Plated,1,2,PTH")
        );
        assert!(
            pth.contents
                .contains("; #@! TA.AperFunction,Plated,PTH,ViaDrill\nT01C0.3")
        );
        assert!(
            pth.contents
                .contains("; #@! TA.AperFunction,Plated,PTH,ComponentDrill\nT02C0.6")
        );
        assert!(pth.contents.contains("X10.0Y19.45G85X10.0Y20.55\nG05"));
        assert!(
            npth.contents
                .contains("; #@! TF.FileFunction,NonPlated,1,2,NPTH")
        );
        assert!(npth.contents.contains("T01C0.65"));
        assert!(npth.contents.contains("X3.0Y4.0"));
    }

    #[cfg(feature = "cli")]
    #[test]
    fn gerber_export_writes_zip_when_output_has_zip_extension() {
        let accuracy = GeometryAccuracy::default();

        let ipc = ipc::Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <LayerRef name="TOP"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="10" y="0"/>
            <PolyStepSegment x="10" y="10"/>
            <PolyStepSegment x="0" y="0"/>
          </Polygon>
        </Profile>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();
        let output_zip = std::env::temp_dir().join(format!(
            "pcb-ipc-gerber-zip-test-{}.zip",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&output_zip);

        let package = export_manufacturing_package(
            &ipc,
            &ManufacturingExportOptions {
                output: output_zip.clone(),
                view: ArtworkScope::Board,
                relief_debug_dir: None,
            },
            accuracy,
        )
        .unwrap();

        assert!(output_zip.is_file());
        let zip_file = std::fs::File::open(&output_zip).unwrap();
        let mut archive = zip::ZipArchive::new(zip_file).unwrap();
        let names = (0..archive.len())
            .map(|index| archive.by_index(index).unwrap().name().to_string())
            .collect::<Vec<_>>();
        assert_eq!(archive.len(), package.files.len());
        assert!(names.iter().any(|name| name == "F_Cu.gtl"));
        assert!(!names.iter().any(|name| name == "profile.gbr"));

        let mut top_copper = String::new();
        archive
            .by_name("F_Cu.gtl")
            .unwrap()
            .read_to_string(&mut top_copper)
            .unwrap();
        assert!(top_copper.contains("%TF.FileFunction,Copper,L1,Top*%"));
        let _ = std::fs::remove_file(output_zip);
    }

    #[test]
    fn gerber_export_preserves_user_special_counter_holes() {
        let accuracy = GeometryAccuracy::default();

        let ipc = ipc::Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <LayerRef name="F.SilkS"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="F.SilkS" layerFunction="LEGEND" side="TOP" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <LayerFeature layerRef="F.SilkS">
          <Set>
            <Features>
              <UserSpecial>
                <Contour>
                  <Polygon>
                    <PolyBegin x="0" y="0"/>
                    <PolyStepSegment x="4" y="0"/>
                    <PolyStepSegment x="4" y="4"/>
                    <PolyStepSegment x="0" y="4"/>
                    <PolyStepSegment x="0" y="0"/>
                  </Polygon>
                </Contour>
                <Contour>
                  <Polygon>
                    <PolyBegin x="1" y="1"/>
                    <PolyStepSegment x="3" y="1"/>
                    <PolyStepSegment x="3" y="3"/>
                    <PolyStepSegment x="1" y="3"/>
                    <PolyStepSegment x="1" y="1"/>
                  </Polygon>
                </Contour>
              </UserSpecial>
            </Features>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();
        let files = build_gerber_x2_files(&ipc, ArtworkScope::Board, accuracy).unwrap();

        let silk = files
            .iter()
            .find(|file| file.filename == "F_SilkS.gto")
            .unwrap();
        assert!(
            !silk.contents.contains("%LPC*%"),
            "positive compound region holes should not export as layer-global clear regions"
        );
        let parsed = gerberx2::GerberX2::parse(&silk.contents).unwrap();
        let geometry = gerberx2::geometry::extract_document(&parsed, accuracy).unwrap();
        let summary = pcb_ir::dialects::artwork::compare::summarize(&geometry, accuracy).unwrap();
        assert!(
            (summary.area_mm2 - 12.0).abs() < 1e-6,
            "compound region should preserve its counter hole; area was {}",
            summary.area_mm2
        );
    }

    #[test]
    fn gerber_preserves_leaf_board_repeats_without_nesting() {
        let accuracy = GeometryAccuracy::default();

        let ipc = ipc::Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="fab"/>
    <LayerRef name="TOP"/>
    <DictionaryStandard units="MILLIMETER">
      <EntryStandard id="pad"><RectCenter width="2" height="1"/></EntryStandard>
    </DictionaryStandard>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="10" y="0"/>
            <PolyStepSegment x="10" y="5"/>
            <PolyStepSegment x="0" y="5"/>
          </Polygon>
        </Profile>
        <PadStackDef name="padstack">
          <PadstackPadDef layerRef="TOP" padUse="REGULAR">
            <StandardPrimitiveRef id="pad"/>
          </PadstackPadDef>
        </PadStackDef>
        <LayerFeature layerRef="TOP">
          <Set net="N1">
            <Pad padstackDefRef="padstack"><Location x="2" y="3"/></Pad>
          </Set>
        </LayerFeature>
      </Step>
      <Step name="panel" type="PALLET">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="0" y="17"/>
            <PolyStepSegment x="28" y="17"/>
            <PolyStepSegment x="28" y="0"/>
          </Polygon>
        </Profile>
        <StepRepeat stepRef="board" x="4" y="6" nx="2" ny="1" dx="14" dy="0" angle="90"/>
      </Step>
      <Step name="fab" type="PALLET">
        <StepRepeat stepRef="panel" x="0" y="0" nx="3" ny="1" dx="30" dy="0"/>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();
        let files = build_gerber_x2_files(&ipc, ArtworkScope::ArrayFlattened, accuracy).unwrap();

        let top = files
            .iter()
            .find(|file| file.filename == "F_Cu.gtl")
            .unwrap();
        assert!(top.contents.contains("%TF.Part,Array*%"));
        assert_eq!(
            top.layer.objects.len(),
            3,
            "the three panel placements each retain one board grid"
        );
        assert!(!top.contents.contains("%ABD"));
        assert_eq!(top.contents.matches("%SRX2Y1I14J0*%").count(), 1);
        assert_eq!(top.contents.matches("%SR*%").count(), 1);
        assert!(!top.contents.contains("%SRX3Y1I30J0*%"));
        assert!(!top.contents.contains("%LM"));
        assert!(!top.contents.contains("%LR"));
        assert!(!top.contents.contains("%LS"));
        let parsed = gerberx2::GerberX2::parse(&top.contents).unwrap();
        assert_eq!(parsed.objects().len(), 6);
        let artwork = gerberx2::geometry::extract_document(&parsed, accuracy).unwrap();
        assert!(artwork.blocks.is_empty());
        assert_eq!(artwork.objects.len(), 6);
    }

    #[test]
    fn board_array_profile_does_not_infer_reliefs_without_vcut_lines() {
        let accuracy = GeometryAccuracy::default();

        let ipc = ipc::Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="panel"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Step name="board" type="BOARD">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="10" y="0"/>
            <PolyStepSegment x="10" y="10"/>
            <PolyStepSegment x="2" y="10"/>
            <PolyStepCurve x="0" y="8" centerX="2" centerY="8" clockwise="false"/>
            <PolyStepSegment x="0" y="0"/>
          </Polygon>
        </Profile>
      </Step>
      <Step name="panel" type="PALLET">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="20" y="0"/>
            <PolyStepSegment x="20" y="20"/>
            <PolyStepSegment x="0" y="20"/>
            <PolyStepSegment x="0" y="0"/>
          </Polygon>
        </Profile>
        <StepRepeat stepRef="board" x="5" y="5" nx="1" ny="1" dx="0" dy="0"/>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();
        let files = build_gerber_x2_files(&ipc, ArtworkScope::ArrayFlattened, accuracy).unwrap();

        assert!(files.iter().all(|file| file.filename != "V_Cut.gbr"));
        let profile = files
            .iter()
            .find(|file| file.filename == "Board_Array_Profile.gm1")
            .unwrap();
        assert!(profile.contents.contains("%TF.Part,Array*%"));
        assert!(!profile.contents.contains("G02*"));
        assert!(!profile.contents.contains("G03*"));
        gerberx2::GerberX2::parse(&profile.contents).unwrap();
    }

    #[test]
    fn gerber_export_carries_vcut_and_fiducial_x2_metadata() {
        let accuracy = GeometryAccuracy::default();

        let ipc = ipc::Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="Panel"/>
    <LayerRef name="TOP"/>
    <LayerRef name="VCUT"/>
    <LayerRef name="SCORE"/>
    <DictionaryLineDesc units="MILLIMETER">
      <EntryLineDesc id="fidline">
        <LineDesc lineWidth="0.1" lineEnd="ROUND"/>
      </EntryLineDesc>
    </DictionaryLineDesc>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER">
      <Spec name="VCut_1">
        <V_Cut type="ANGLE">
          <Property value="90" unit="DEGREES"/>
        </V_Cut>
      </Spec>
    </CadHeader>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Layer name="VCUT" layerFunction="V_CUT" side="ALL" polarity="POSITIVE">
        <SpecRef id="VCut_1"/>
      </Layer>
      <Layer name="SCORE" layerFunction="SCORE" side="ALL" polarity="POSITIVE"/>
      <Step name="Panel" type="PALLET">
        <LayerFeature layerRef="TOP">
          <Set>
            <GlobalFiducial>
              <Location x="1" y="2"/>
              <Circle diameter="1">
                <FillDesc fillProperty="HOLLOW"/>
                <LineDescRef id="fidline"/>
              </Circle>
              <PinRef componentRef="U1" pin="1"/>
            </GlobalFiducial>
          </Set>
        </LayerFeature>
        <LayerFeature layerRef="VCUT">
          <Set>
            <Features>
              <Line startX="0" startY="5" endX="10" endY="5">
                <LineDesc lineWidth="0.1" lineEnd="ROUND"/>
              </Line>
            </Features>
          </Set>
        </LayerFeature>
        <LayerFeature layerRef="SCORE">
          <Set>
            <Features>
              <Line startX="0" startY="7" endX="10" endY="7">
                <LineDesc lineWidth="0.1" lineEnd="ROUND"/>
              </Line>
            </Features>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();
        let files = build_gerber_x2_files(&ipc, ArtworkScope::ArrayFlattened, accuracy).unwrap();

        let top = files
            .iter()
            .find(|file| file.filename == "F_Cu.gtl")
            .unwrap();
        assert!(top.contents.contains("%TF.Part,Array*%"));
        assert!(
            top.contents
                .contains("%TA.AperFunction,FiducialPad,Global*%")
        );
        assert!(top.contents.contains("%TO.C,U1*%"));
        assert!(top.contents.contains("%TO.P,U1,1*%"));

        let vcut = files
            .iter()
            .find(|file| file.filename == "V_Cut.gbr")
            .unwrap();
        assert!(vcut.contents.contains("%TF.FileFunction,Vcut*%"));
        assert!(vcut.contents.contains("%TF.Part,Array*%"));
        assert!(vcut.contents.contains("%TA.AperFunction,Other,Vcut*%"));

        let score = files
            .iter()
            .find(|file| file.filename == "Score.gbr")
            .unwrap();
        assert!(score.contents.contains("%TF.FileFunction,Vcut*%"));
        assert!(score.contents.contains("%TF.Part,Array*%"));
        assert!(score.contents.contains("%TA.AperFunction,Other,Score*%"));
    }

    #[cfg(feature = "cli")]
    #[test]
    fn real_board_export_parseback_and_svg_paths_smoke() {
        let accuracy = GeometryAccuracy::default();

        let compressed = include_bytes!("../../../ipc2581/tests/data/DM0002-IPC-2518.xml.zst");
        let content = zstd::decode_all(Cursor::new(compressed)).unwrap();
        let ipc = ipc::Ipc2581::parse(std::str::from_utf8(&content).unwrap()).unwrap();
        let files = build_gerber_x2_files(&ipc, ArtworkScope::Board, accuracy).unwrap();

        assert!(files.len() >= 10);
        assert!(files.iter().any(|file| file.filename == "F_Cu.gtl"));
        assert!(files.iter().any(|file| file.filename == "Edge_Cuts.gm1"));

        for file in &files {
            let parsed = gerberx2::GerberX2::parse(&file.contents).unwrap();
            let geometry = gerberx2::geometry::extract_document(&parsed, accuracy).unwrap();
            geometry.validate().unwrap();

            let mask = pcb_ir::dialects::artwork::compose_to_mask(&geometry, accuracy).unwrap();
            mask.validate().unwrap();
            let svg = pcb_ir::render::svg(&mask, &pcb_ir::render::RenderOptions::layer(0));
            assert!(svg.contains("<svg"), "{} did not render SVG", file.filename);
        }

        let mut layer = geometry::extract_layer(&ipc, "F.Cu", accuracy).unwrap();
        pcb_ir::dialects::ipc::process::compose_for_rendering(&mut layer, accuracy).unwrap();
        let artwork = pcb_ir::dialects::ipc::lower_layer_to_artwork(
            &layer,
            0,
            LayerRole::Copper,
            pcb_ir::dialects::Side::Top,
            accuracy,
        )
        .unwrap();
        artwork.validate().unwrap();
        let mask = pcb_ir::dialects::artwork::compose_to_mask(&artwork, accuracy).unwrap();
        mask.validate().unwrap();
        assert!(
            pcb_ir::render::svg(&mask, &pcb_ir::render::RenderOptions::layer(0)).contains("<svg")
        );

        pcb_ir::dialects::ipc::process::flatten_layers_to_masks(&mut layer, accuracy).unwrap();
        let flat_artwork = pcb_ir::dialects::ipc::lower_layer_to_artwork(
            &layer,
            0,
            LayerRole::Copper,
            pcb_ir::dialects::Side::Top,
            accuracy,
        )
        .unwrap();
        flat_artwork.validate().unwrap();
        let flat_mask =
            pcb_ir::dialects::artwork::compose_to_mask(&flat_artwork, accuracy).unwrap();
        flat_mask.validate().unwrap();
        assert!(
            pcb_ir::render::svg(&flat_mask, &pcb_ir::render::RenderOptions::layer(0))
                .contains("<svg")
        );
    }
}
