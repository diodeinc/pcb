use pcb_ir::geom::{GeometryAccuracy, Resolution};
use std::collections::{HashMap, HashSet};
#[cfg(feature = "cli")]
use std::fmt::Write as _;
#[cfg(feature = "cli")]
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
#[cfg(feature = "cli")]
use gerberx2::trim_decimal;
use gerberx2::write_layer;
use ipc2581::types::{LayerFunction, Side as IpcSide, StandardPrimitive, ecad::Layer};

use crate::geometry;
use gerberx2::from_artwork::lower_artwork_layer;
use gerberx2::from_artwork::{ArtworkDocument as GerberArtwork, LayerAttributes, ObjectAttributes};
use pcb_ir::dialects::artwork::{
    Aperture, ApertureShape, Geometry as ArtworkGeometry, Object as ArtworkObject, PaintOrder,
    PaintStage,
};
#[cfg(feature = "cli")]
use pcb_ir::dialects::ipc::relief;
use pcb_ir::dialects::ipc::{
    ArtworkScope, ArtworkTarget, Feature, FeatureBucket, FeatureDomain, FeatureOperation,
    FeatureRole, FiducialKind, LayoutPurpose, PlatingKind, PrimitiveRef, ProfileSet,
    lower_layer_to_artwork_objects_with, lower_layer_to_artwork_with, profile_occurrences_for,
};
use pcb_ir::dialects::{LayerRole, Side as IrSide};
use pcb_ir::geom::path::ContourBuf;
use pcb_ir::geom::{BBox, Paint, Polarity, Span, StrokeStyle};
use pcb_ir::import::ipc2581::{GeometryDocument, ImportedDesign, LayerId};
#[cfg(not(target_family = "wasm"))]
use rayon::prelude::*;

/// The standard-primitive dictionary by entry id, built once per export:
/// every pad-like feature of every layer looks its primitive up.
type StandardPrimitives<'a> = HashMap<ipc2581::Symbol, &'a StandardPrimitive>;

#[derive(Debug, Clone)]
pub struct GerberX2File {
    pub filename: String,
    pub contents: String,
}

#[derive(Debug, Clone, Default)]
pub struct GerberExportOptions {
    pub relief_debug_dir: Option<PathBuf>,
}

/// Profiles image as round strokes of this width.
const PROFILE_STROKE_WIDTH_MM: f64 = 0.05;

pub fn build_gerber_x2_files(
    imported: &ImportedDesign,
    view: ArtworkScope,
    options: &GerberExportOptions,
    resolution: Resolution,
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
    let plans = export_layer_plans(imported, &imported.layer_definitions);
    let has_profile_plan = plans
        .iter()
        .any(|plan| plan.role == GerberLayerRole::Profile);
    let part = gerber_part_for_ipc_view(imported, view)?;
    // The first entry of an id wins, as a scan of the dictionary finds it.
    let standard_primitives: StandardPrimitives = imported
        .content
        .dictionary_standard
        .entries
        .iter()
        .rev()
        .map(|entry| (entry.id, &entry.primitive))
        .collect();

    // Layers are independent of one another.
    let export = |plan: &ExportLayerPlan<'_>| -> Result<Option<GerberX2File>> {
        let source_layer = plan.layer;
        let layer_name = imported.resolve(source_layer.name);
        let spec = GerberArtworkSpec {
            role: plan.role,
            side: crate::layers::ir_side(source_layer.side),
            meta: layer_attributes(plan.file_function.clone(), part, plan.role),
            view,
        };
        let artwork = if view == ArtworkScope::ArrayFlattened {
            hierarchical_artwork_from_ipc_layer(
                imported,
                &standard_primitives,
                plan.layer_id,
                layer_name,
                spec,
                resolution,
            )?
        } else {
            let mut doc = imported
                .materialize_layer(plan.layer_id, view)
                .with_context(|| format!("failed to extract IPC-2581 layer '{layer_name}'"))?;
            pcb_ir::dialects::ipc::process::normalize_for_positive_artwork(&mut doc, resolution)
                .with_context(|| format!("failed to normalize IPC-2581 layer '{layer_name}'"))?;
            if let Err(error) = pcb_ir::dialects::ipc::validate_artwork_ready(&doc) {
                bail!("IPC-2581 layer '{layer_name}' is not artwork-ready: {error}");
            }
            artwork_from_ipc_layer(imported, &standard_primitives, &doc, spec)
        };
        if matches!(plan.role, GerberLayerRole::Vcut | GerberLayerRole::Score)
            && artwork.layers[0].objects.is_empty()
        {
            return Ok(None);
        }
        let layer = lower_artwork_layer(&artwork, resolution.accuracy)?;
        if plan.role == GerberLayerRole::Profile && layer.objects.is_empty() {
            return Ok(None);
        }
        Ok(Some(GerberX2File {
            filename: plan.filename.clone(),
            contents: write_layer(&layer)?,
        }))
    };
    #[cfg(not(target_family = "wasm"))]
    let exported = plans.par_iter().map(export);
    #[cfg(target_family = "wasm")]
    let exported = plans.iter().map(export);
    let mut files = exported
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    if view == ArtworkScope::ArrayFlattened {
        files.extend(board_array_profile_gerber_files(
            imported,
            options.relief_debug_dir.as_deref(),
            resolution,
        )?);
    } else if !has_profile_plan
        && let Some(file) = synthetic_profile_gerber_file(imported, view, resolution.accuracy)?
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
    let copper_count = layers
        .iter()
        .filter(|layer| gerber_layer_role(layer.layer_function) == Some(GerberLayerRole::Copper))
        .count();
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

fn allocate_filename(
    used: &mut HashSet<String>,
    preferred: &str,
    source_layer_name: &str,
) -> String {
    if used.insert(preferred.to_string()) {
        return preferred.to_string();
    }

    let (stem, extension) = preferred
        .rsplit_once('.')
        .map_or((preferred, String::new()), |(stem, extension)| {
            (stem, format!(".{extension}"))
        });
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
    let fields = |fields: &[&str]| fields.iter().map(|field| field.to_string()).collect();
    // A layer that exists on the two outer sides only.
    let outer = |top: &str, bottom: &str, function: &str| match side {
        Some(IpcSide::Bottom) => (bottom.to_string(), fields(&[function, "Bot"])),
        _ => (top.to_string(), fields(&[function, "Top"])),
    };
    let vcut = |filename: &str| match side {
        Some(IpcSide::Top) => (filename.to_string(), fields(&["Vcut", "Top"])),
        Some(IpcSide::Bottom) => (filename.to_string(), fields(&["Vcut", "Bot"])),
        _ => (filename.to_string(), fields(&["Vcut"])),
    };
    match role {
        GerberLayerRole::Copper => {
            let (filename, index, side) = match side {
                Some(IpcSide::Top) => ("F_Cu.gtl".to_string(), 1, "Top"),
                Some(IpcSide::Bottom) => ("B_Cu.gbl".to_string(), copper_count, "Bot"),
                // KiCad numbers inner layers from 1, excluding the top layer.
                _ => (
                    format!("In{}_Cu.gbr", copper_index - 1),
                    copper_index,
                    "Inr",
                ),
            };
            (filename, fields(&["Copper", &format!("L{index}"), side]))
        }
        GerberLayerRole::Paste => outer("F_Paste.gtp", "B_Paste.gbp", "Paste"),
        GerberLayerRole::Soldermask => outer("F_Mask.gts", "B_Mask.gbs", "Soldermask"),
        GerberLayerRole::Legend => outer("F_SilkS.gto", "B_SilkS.gbo", "Legend"),
        GerberLayerRole::AssemblyDrawing => {
            let (fallback_stem, file_function) = match side {
                Some(IpcSide::Top) => ("F_Fab", ["AssemblyDrawing", "Top"]),
                Some(IpcSide::Bottom) => ("B_Fab", ["AssemblyDrawing", "Bot"]),
                _ => ("Assembly", ["OtherDrawing", "Assembly"]),
            };
            (
                drawing_filename(source_layer_name, fallback_stem),
                fields(&file_function),
            )
        }
        GerberLayerRole::FabricationDrawing => (
            drawing_filename(source_layer_name, "Fabrication_Drawing"),
            fields(&["FabricationDrawing"]),
        ),
        GerberLayerRole::Profile => ("Edge_Cuts.gm1".to_string(), fields(&["Profile", "NP"])),
        GerberLayerRole::Vcut => vcut("V_Cut.gbr"),
        // Gerber calls the scored-line data function `Vcut`; the specification
        // explicitly treats scoring as the same fabrication operation.
        GerberLayerRole::Score => vcut("Score.gbr"),
    }
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

fn gerber_part_for_ipc_view(imported: &ImportedDesign, view: ArtworkScope) -> Result<GerberPart> {
    let step = geometry::step_artwork::root_step(imported, false)?;
    Ok(if view == ArtworkScope::Board || !step.is_panel() {
        GerberPart::Single
    } else if imported.resolve(step.name) == crate::steps::FAB_PANEL_STEP_NAME {
        GerberPart::FabricationPanel
    } else {
        GerberPart::Array
    })
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

fn artwork_from_ipc_layer(
    imported: &ImportedDesign,
    standard_primitives: &StandardPrimitives,
    doc: &GeometryDocument,
    spec: GerberArtworkSpec,
) -> GerberArtwork {
    let layer = &doc.layers[0];
    let header = pcb_ir::dialects::artwork::Layer {
        name: layer.name.clone(),
        role: spec.role.ir_role(),
        side: spec.side,
        objects: Span::EMPTY,
        bbox: layer.bbox,
        meta: spec.meta,
    };
    let mut artwork = lower_layer_to_artwork_with(
        doc,
        0,
        header,
        &gerber_target(spec.role, &|primitive| {
            catalogue_aperture(standard_primitives, primitive)
        }),
        &|doc, feature| object_attributes(imported, doc, feature, spec.role, spec.side),
    );

    if spec.role == GerberLayerRole::Profile
        && spec.view != ArtworkScope::ArrayFlattened
        && artwork.layers[0].objects.is_empty()
    {
        append_profile_occurrences(&mut artwork, 0, doc, spec.view.profile_set());
    }
    artwork
}

/// One layer of the Step graph under the primary Step, each Step normalized
/// to positive artwork and lowered once in its own coordinates.
fn hierarchical_artwork_from_ipc_layer(
    imported: &ImportedDesign,
    standard_primitives: &StandardPrimitives,
    layer: LayerId,
    layer_name: &str,
    spec: GerberArtworkSpec,
    resolution: Resolution,
) -> Result<GerberArtwork> {
    let header = pcb_ir::dialects::artwork::Layer {
        name: layer_name.to_string(),
        role: spec.role.ir_role(),
        side: spec.side,
        objects: Span::EMPTY,
        bbox: BBox::empty(),
        meta: spec.meta,
    };
    geometry::step_artwork::step_graph_artwork(
        imported,
        layer,
        geometry::step_artwork::root_step(imported, false)?,
        header,
        |step, mut local, artwork| {
            pcb_ir::dialects::ipc::process::normalize_for_positive_artwork(&mut local, resolution)?;
            if let Err(error) = pcb_ir::dialects::ipc::validate_artwork_ready(&local) {
                bail!(
                    "IPC-2581 Step '{}' layer '{layer_name}' is not artwork-ready: {error}",
                    imported.resolve(step.name),
                );
            }
            Ok(lower_layer_to_artwork_objects_with(
                &local,
                0,
                artwork,
                &gerber_target(spec.role, &|primitive| {
                    catalogue_aperture(standard_primitives, primitive)
                }),
                &|doc, feature| object_attributes(imported, doc, feature, spec.role, spec.side),
            ))
        },
    )
    .with_context(|| format!("failed to lower IPC-2581 layer '{layer_name}'"))
}

/// Gerber's half of IPC artwork lowering: standard-dictionary primitives
/// flash through the standard apertures `catalogue` holds, and on copper
/// only pad-like features and tiled balance cells may image as flashes.
fn gerber_target<'a>(
    role: GerberLayerRole,
    catalogue: &'a dyn Fn(PrimitiveRef) -> Option<Aperture>,
) -> ArtworkTarget<'a> {
    ArtworkTarget {
        catalogue,
        flashes: if role == GerberLayerRole::Copper {
            &|doc, feature| {
                doc.feature_set(feature)
                    .is_some_and(|set| set.copper_balance_void.is_some())
                    || matches!(
                        feature.bucket,
                        FeatureBucket::Smd
                            | FeatureBucket::Pth
                            | FeatureBucket::Via
                            | FeatureBucket::Fiducial
                    )
            }
        } else {
            &|_, _| true
        },
        paint_order: &gerber_paint_order,
    }
}

/// Gerber orders removals rather than imaging them as clears, so every
/// drilled or routed feature stages last regardless of its bucket.
fn gerber_paint_order(feature: &Feature) -> PaintOrder {
    let stage = if feature.is_drill_like() {
        PaintStage::FinalCutout
    } else if feature.bucket == FeatureBucket::Fill {
        PaintStage::Base
    } else {
        PaintStage::Overlay
    };
    PaintOrder { stage }
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
    let mut artwork = profile_artwork("Edge.Cuts", gerber_part_for_ipc_view(imported, view)?);
    append_profile_occurrences(&mut artwork, 0, &imported.geometry, view.profile_set());
    profile_file(&artwork, "Edge_Cuts.gm1", accuracy)
}

fn board_array_profile_gerber_files(
    imported: &ImportedDesign,
    relief_debug_dir: Option<&Path>,
    resolution: Resolution,
) -> Result<Vec<GerberX2File>> {
    let doc = &imported.geometry;
    let score_lines = geometry::board_array_vscore_lines(imported)?;
    #[cfg(not(feature = "cli"))]
    if relief_debug_dir.is_some() {
        bail!("filesystem debug output requires the cli feature");
    }
    #[cfg(feature = "cli")]
    let profile = if let Some(debug_dir) = relief_debug_dir {
        let (profile, relief_debug) = geometry::board_array_fabrication_profile_with_debug(
            imported,
            doc,
            &score_lines,
            resolution,
        )?;
        write_vscore_relief_debug(debug_dir, &relief_debug)?;
        profile
    } else {
        geometry::board_array_fabrication_profile(imported, doc, &score_lines, resolution)?
    };
    #[cfg(not(feature = "cli"))]
    let profile =
        geometry::board_array_fabrication_profile(imported, doc, &score_lines, resolution)?;
    if profile.purpose == LayoutPurpose::Product {
        let mut contour_groups = profile.array_outlines;
        contour_groups.push(profile.material_removal);
        return Ok(profile_gerber_file(
            "Board Array Profile",
            "Board_Array_Profile.gm1",
            contour_groups,
            GerberPart::Array,
            resolution.accuracy,
        )?
        .into_iter()
        .collect());
    }

    [
        (
            "Fab Panel Outline",
            "Fab_Panel_Outline.gm1",
            profile.array_outlines,
        ),
        (
            "Assembly Panel Outlines",
            "Assembly_Panel_Outlines.gm1",
            profile.assembly_panel_outlines,
        ),
        (
            "Board Cutouts",
            "Board_Cutouts.gm1",
            vec![profile.material_removal],
        ),
    ]
    .into_iter()
    .filter_map(|(layer_name, filename, contour_groups)| {
        profile_gerber_file(
            layer_name,
            filename,
            contour_groups,
            GerberPart::FabricationPanel,
            resolution.accuracy,
        )
        .transpose()
    })
    .collect()
}

fn profile_gerber_file(
    layer_name: &str,
    filename: &str,
    contour_groups: Vec<Vec<ContourBuf>>,
    part: GerberPart,
    accuracy: GeometryAccuracy,
) -> Result<Option<GerberX2File>> {
    let mut artwork = profile_artwork(layer_name, part);
    for contours in contour_groups
        .into_iter()
        .filter(|contours| !contours.is_empty())
    {
        append_profile_payloads(&mut artwork, 0, contours);
    }
    profile_file(&artwork, filename, accuracy)
}

/// An empty document with one profile layer.
fn profile_artwork(layer_name: &str, part: GerberPart) -> GerberArtwork {
    let mut artwork = GerberArtwork::new();
    artwork.push_layer(pcb_ir::dialects::artwork::Layer {
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
    artwork
}

/// The file of a profile layer, unless it draws nothing.
fn profile_file(
    artwork: &GerberArtwork,
    filename: &str,
    accuracy: GeometryAccuracy,
) -> Result<Option<GerberX2File>> {
    if artwork.layers[0].objects.is_empty() {
        return Ok(None);
    }
    Ok(Some(GerberX2File {
        filename: filename.to_string(),
        contents: write_layer(&lower_artwork_layer(artwork, accuracy)?)?,
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
    let [x, y, width, height] = [
        bbox.min.x - padding,
        -(bbox.max.y + padding),
        bbox.width() + 2.0 * padding,
        bbox.height() + 2.0 * padding,
    ]
    .map(|value| trim_decimal(value, 6));
    let mut svg = String::new();
    writeln!(
        svg,
        "<svg xmlns='http://www.w3.org/2000/svg' viewBox='{x} {y} {width} {height}' data-vscore-relief-debug='true'>"
    )
    .unwrap();
    writeln!(
        svg,
        "  <rect x='{x}' y='{y}' width='{width}' height='{height}' fill='#ffffff'/>"
    )
    .unwrap();
    writeln!(svg, "  <g transform='scale(1 -1)'>").unwrap();

    // Style: class, fill, stroke, stroke width, extra attributes.
    let mut path = |entry: usize, payloads: &[ContourBuf], style: [&str; 5]| {
        let data = pcb_ir::render::svg_path_data(payloads);
        let [class, fill, stroke, width, extra] = style;
        if !data.is_empty() {
            writeln!(
                svg,
                "    <path class='{class}' data-entry='{entry}' d='{data}' fill='{fill}' stroke='{stroke}' stroke-width='{width}' {extra} fill-rule='evenodd'/>"
            )
            .unwrap();
        }
    };
    for (index, entry) in debug.entries.iter().enumerate() {
        let dashed = "stroke-dasharray='0.6 0.6'";
        path(
            index,
            std::slice::from_ref(&entry.score_cell),
            ["score-cell", "none", "#64748b", "0.08", dashed],
        );
        path(
            index,
            &entry.board_boundary,
            ["board-boundary", "none", "#064e3b", "0.08", ""],
        );
        let translucent = "fill-opacity='0.18'";
        path(
            index,
            &entry.dead_space_pockets,
            [
                "dead-space-pocket",
                "#f59e0b",
                "#f59e0b",
                "0.05",
                translucent,
            ],
        );
        let translucent = "fill-opacity='0.16'";
        path(
            index,
            &entry.legal_tool_centers,
            [
                "legal-tool-center",
                "#2563eb",
                "#1d4ed8",
                "0.05",
                translucent,
            ],
        );
        path(
            index,
            &entry.relief_contours,
            ["relief-contour", "none", "#dc2626", "0.1", ""],
        );
    }
    path(
        debug.entries.len(),
        &debug.merged_relief_contours,
        ["merged-relief-contour", "none", "#7c3aed", "0.14", ""],
    );

    writeln!(svg, "  </g>").unwrap();
    writeln!(svg, "</svg>").unwrap();
    Some(svg)
}

#[cfg(feature = "cli")]
fn payloads_bbox(payloads: &[ContourBuf]) -> BBox {
    payloads
        .iter()
        .fold(BBox::empty(), |bbox, payload| bbox.union(payload.bbox))
}

fn append_profile_occurrences(
    artwork: &mut GerberArtwork,
    layer: u32,
    doc: &GeometryDocument,
    profile_set: ProfileSet,
) {
    for occurrence in profile_occurrences_for(doc, profile_set) {
        let profile = occurrence.profile;
        let cutouts = profile.cutouts.slice(&doc.profile_cutouts);
        for path in std::iter::once(profile.outer_path).chain(cutouts.iter().map(|c| c.path)) {
            append_profile_payloads(
                artwork,
                layer,
                doc.transformed_path_contours(path, occurrence.transform),
            );
        }
    }
}

fn append_profile_payloads(artwork: &mut GerberArtwork, layer: u32, payloads: Vec<ContourBuf>) {
    let path = artwork.push_path(
        Paint::Stroke(StrokeStyle::round(PROFILE_STROKE_WIDTH_MM)),
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

/// The standard-dictionary primitives the artwork dialect carries as exact
/// apertures.
fn catalogue_aperture(
    standard_primitives: &StandardPrimitives,
    primitive: PrimitiveRef,
) -> Option<Aperture> {
    let PrimitiveRef::Standard(id) = primitive else {
        return None;
    };
    // IPC hexagons and octagons place their first vertex pointing down.
    let polygon = |vertices, point_to_point| ApertureShape::Polygon {
        diameter: point_to_point,
        vertices,
        rotation_degrees: -90.0,
    };
    Some(Aperture::solid(match standard_primitives.get(&id)? {
        StandardPrimitive::Circle(circle) => ApertureShape::Circle {
            diameter: circle.shape.diameter,
        },
        StandardPrimitive::RectCenter(rect) => ApertureShape::Rectangle {
            width: rect.shape.size.width,
            height: rect.shape.size.height,
        },
        StandardPrimitive::Oval(oval) => ApertureShape::Obround {
            width: oval.shape.size.width,
            height: oval.shape.size.height,
        },
        StandardPrimitive::RectRound(rect)
            if rect.shape.upper_right
                && rect.shape.upper_left
                && rect.shape.lower_right
                && rect.shape.lower_left
                && rect.shape.radius > 0.0 =>
        {
            ApertureShape::RoundRect {
                width: rect.shape.size.width,
                height: rect.shape.size.height,
                radius: rect.shape.radius,
            }
        }
        StandardPrimitive::Hexagon(hexagon) => polygon(6, hexagon.shape.point_to_point),
        StandardPrimitive::Octagon(octagon) => polygon(8, octagon.shape.point_to_point),
        _ => return None,
    }))
}

fn object_attributes(
    imported: &ImportedDesign,
    doc: &GeometryDocument,
    feature: &Feature,
    role: GerberLayerRole,
    side: IrSide,
) -> ObjectAttributes {
    let pin_ref = feature.pin_refs.slice(&doc.pin_refs).first();
    let carries_netlist = role == GerberLayerRole::Copper;
    let carries_pins = carries_netlist && matches!(side, IrSide::Top | IrSide::Bottom);
    ObjectAttributes {
        aperture_function: aperture_function(doc, feature, role, side),
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
    doc: &GeometryDocument,
    feature: &Feature,
    role: GerberLayerRole,
    side: IrSide,
) -> Option<Vec<String>> {
    let function: &[&str] = match role {
        GerberLayerRole::Soldermask | GerberLayerRole::Paste | GerberLayerRole::Legend => {
            &["Material"]
        }
        GerberLayerRole::AssemblyDrawing | GerberLayerRole::FabricationDrawing => return None,
        GerberLayerRole::Profile => &["Profile"],
        GerberLayerRole::Vcut => &["Other", "Vcut"],
        GerberLayerRole::Score => &["Other", "Score"],
        GerberLayerRole::Copper => copper_aperture_function(doc, feature, side),
    };
    Some(function.iter().map(|field| field.to_string()).collect())
}

/// What a copper feature is, from the most to the least specific thing the
/// source says about it.
fn copper_aperture_function(
    doc: &GeometryDocument,
    feature: &Feature,
    side: IrSide,
) -> &'static [&'static str] {
    if doc
        .feature_set(feature)
        .is_some_and(|set| set.copper_balance)
    {
        return &["CopperBalancing"];
    }

    match feature.intent.operation {
        FeatureOperation::Drill => return &["Other", "Drill"],
        FeatureOperation::Score if feature.is_vcut() => return &["Other", "Vcut"],
        FeatureOperation::Score if feature.is_score() => return &["Other", "Score"],
        FeatureOperation::Route | FeatureOperation::Profile => return &["Profile"],
        _ => {}
    }

    match feature.intent.role {
        _ if feature.is_fiducial() => {
            return match feature.fiducial_kind {
                FiducialKind::Unknown | FiducialKind::Global => &["FiducialPad", "Global"],
                FiducialKind::Local => &["FiducialPad", "Local"],
                FiducialKind::Panel | FiducialKind::GoodPanel => &["FiducialPad", "Panel"],
                FiducialKind::BadBoard => &["OtherPad", "BadBoardMark"],
            };
        }
        FeatureRole::Pad => {
            return match feature.intent.plating {
                PlatingKind::Plated => &["ComponentPad"],
                PlatingKind::Via | PlatingKind::ViaCapped => &["ViaPad"],
                _ if matches!(side, IrSide::Top | IrSide::Bottom) => &["SMDPad", "CuDef"],
                _ if !feature.pin_refs.is_empty() => &["ComponentPad"],
                _ => &["OtherPad", "InnerLayerPad"],
            };
        }
        FeatureRole::Via => return &["ViaPad"],
        FeatureRole::Conductor => return &["Conductor"],
        FeatureRole::Hole => return &["Other", "Hole"],
        FeatureRole::Slot => return &["Other", "Slot"],
        FeatureRole::ArraySeparation if feature.is_vcut() => return &["Other", "Vcut"],
        FeatureRole::ArraySeparation if feature.is_score() => return &["Other", "Score"],
        FeatureRole::Route | FeatureRole::BoardOutline => return &["Profile"],
        _ => {}
    }

    match feature.intent.domain {
        FeatureDomain::Copper => &["Conductor"],
        FeatureDomain::Drill => &["Other", "Drill"],
        FeatureDomain::Rout | FeatureDomain::Profile => &["Profile"],
        FeatureDomain::VCut => &["Other", "Vcut"],
        FeatureDomain::Score => &["Other", "Score"],
        FeatureDomain::Soldermask
        | FeatureDomain::Paste
        | FeatureDomain::Legend
        | FeatureDomain::Mechanical
        | FeatureDomain::Other
        | FeatureDomain::Unknown => &["OtherCopper", "Unclassified"],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc2581 as ipc;
    use crate::manufacturing::{
        ManufacturingExportOptions, ManufacturingPackage, build_manufacturing_package,
    };
    use gerberx2::ObjectKind;
    use gerberx2::geometry::GerberArtworkDocument;
    use ipc2581::Ipc2581;
    use pcb_ir::import::ipc2581::import_design;
    use std::collections::BTreeMap;
    #[cfg(feature = "cli")]
    use std::io::{Cursor, Read};

    /// Exported file contents by filename.
    type Files = BTreeMap<String, String>;

    fn gerber_files(ipc: &Ipc2581, view: ArtworkScope) -> Files {
        build_gerber_x2_files(
            &import_design(ipc, Resolution::default()).unwrap(),
            view,
            &GerberExportOptions::default(),
            Resolution::default(),
        )
        .unwrap()
        .into_iter()
        .map(|file| (file.filename, file.contents))
        .collect()
    }

    fn manufacturing_package(ipc: &Ipc2581, view: ArtworkScope) -> ManufacturingPackage {
        build_manufacturing_package(
            &import_design(ipc, Resolution::default()).unwrap(),
            &ManufacturingExportOptions {
                view,
                relief_debug_dir: None,
            },
            Resolution::default(),
        )
        .unwrap()
    }

    fn manufacturing_files(ipc: &Ipc2581, view: ArtworkScope) -> Files {
        manufacturing_package(ipc, view)
            .files
            .into_iter()
            .map(|file| (file.filename, file.contents))
            .collect()
    }

    fn extracted(contents: &str) -> GerberArtworkDocument {
        let parsed = gerberx2::GerberX2::parse(contents).unwrap();
        gerberx2::geometry::extract_document(&parsed, GeometryAccuracy::default()).unwrap()
    }

    /// The area a Gerber file images.
    fn area(contents: &str) -> f64 {
        pcb_ir::dialects::artwork::compare::summarize(&extracted(contents), Resolution::default())
            .unwrap()
            .area_mm2
    }

    fn count(contents: &str, kind: impl Fn(&ObjectKind) -> bool) -> usize {
        let parsed = gerberx2::GerberX2::parse(contents).unwrap();
        parsed
            .objects()
            .iter()
            .filter(|object| kind(&object.kind))
            .count()
    }

    /// A filled axis-aligned rectangle as a `UserSpecial` contour.
    fn rect_contour(x0: f64, y0: f64, x1: f64, y1: f64) -> String {
        format!(
            r#"<Contour><Polygon>
              <PolyBegin x="{x0}" y="{y0}"/><PolyStepSegment x="{x1}" y="{y0}"/>
              <PolyStepSegment x="{x1}" y="{y1}"/><PolyStepSegment x="{x0}" y="{y1}"/>
              <PolyStepSegment x="{x0}" y="{y0}"/>
            </Polygon></Contour>"#
        )
    }

    /// A one-Step board with a 20 x 20 profile and one `TOP` signal layer
    /// carrying `features`. `pad` is the standard primitive of dictionary
    /// entry `pad`, which padstack `padstack` places on `TOP`.
    fn top_copper_board(pad: &str, features: &str) -> Ipc2581 {
        ipc::Ipc2581::parse(&format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <LayerRef name="TOP"/>
    <DictionaryStandard units="MILLIMETER">
      <EntryStandard id="pad">{pad}</EntryStandard>
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
        <LayerFeature layerRef="TOP">{features}</LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#
        ))
        .unwrap()
    }

    /// `pad` placed through `padstack` at each location, with optional
    /// extra child elements.
    fn pads<const N: usize>(locations: [(f64, f64, &str); N]) -> String {
        locations
            .iter()
            .map(|(x, y, extra)| {
                format!(
                    r#"<Pad padstackDefRef="padstack">{extra}<Location x="{x}" y="{y}"/>
                      <StandardPrimitiveRef id="pad"/></Pad>"#
                )
            })
            .collect()
    }

    const CIRCLE_1: &str = r#"<Circle diameter="1"/>"#;

    #[test]
    fn negative_sets_clear_only_what_was_painted_before_them() {
        let user_special =
            |contours: &str| format!("<Features><UserSpecial>{contours}</UserSpecial></Features>");
        // Sequential set semantics: a fill written after the clear repaints
        // the cleared area, so the 6 x 6 fill survives whole.
        let repainted = format!(
            r#"<Set polarity="NEGATIVE">{}</Set><Set>{}</Set>"#,
            user_special(&rect_contour(4.0, 4.0, 6.0, 6.0)),
            user_special(&rect_contour(2.0, 2.0, 8.0, 8.0)),
        );
        // A clear painted after a pad erases their overlap, a quarter of it.
        let erased = format!(
            r#"<Set net="N1">{}</Set><Set polarity="NEGATIVE">{}</Set>"#,
            pads([(5.0, 5.0, "")]),
            user_special(&rect_contour(4.0, 4.0, 5.0, 5.0)),
        );
        for (features, expected) in [
            (repainted, 36.0),
            (erased, std::f64::consts::PI * (1.0 - 0.25)),
        ] {
            let ipc = top_copper_board(r#"<Circle diameter="2"/>"#, &features);
            let copper = &gerber_files(&ipc, ArtworkScope::Board)["F_Cu.gtl"];
            assert!(
                !copper.contains("%LPC*%"),
                "Gerber carries a clear set as the dark copper it leaves"
            );
            let actual = area(copper);
            assert!(
                (actual - expected).abs() <= expected * 0.02,
                "expected area {expected:.4}, got {actual:.4}"
            );
        }
    }

    #[test]
    fn catalogue_pads_flash_through_shared_apertures() {
        let ipc = top_copper_board(
            r#"<RectRound width="2" height="1" radius="0.25" upperRight="true" upperLeft="true" lowerRight="true" lowerLeft="true"/>"#,
            &format!(
                r#"<Set net="N1">{}</Set>"#,
                pads([
                    (5.0, 5.0, ""),
                    (5.0, 15.0, ""),
                    (15.0, 10.0, r#"<Xform rotation="45"/>"#)
                ])
            ),
        );
        let copper = &gerber_files(&ipc, ArtworkScope::Board)["F_Cu.gtl"];
        assert_eq!(copper.matches("D03*").count(), 3);
        assert!(
            !copper.contains("G36*"),
            "catalogue pads must flash, not flatten to regions"
        );
        assert!(
            copper.matches("%ADD").count() <= 2,
            "repeated orientations share aperture definitions"
        );
        assert!(!copper.contains("%AMRoundedRect*"));
        assert!(!copper.lines().any(|line| line.starts_with("21,")));
        assert!(!copper.contains("%LR"));
        assert!(copper.lines().any(|line| line.starts_with("4,")));

        let corner_deficit = 0.25 * 0.25 * (4.0 - std::f64::consts::PI);
        let expected = 3.0 * (2.0 - corner_deficit);
        let actual = area(copper);
        assert!(
            (actual - expected).abs() <= expected * 0.02,
            "expected three roundrect pads with area {expected:.4}, got {actual:.4}"
        );
    }

    #[test]
    fn oversized_corner_radius_clamps_to_the_obround_image() {
        let ipc = top_copper_board(
            r#"<RectRound width="2" height="1" radius="0.75" upperRight="true" upperLeft="true" lowerRight="true" lowerLeft="true"/>"#,
            &format!(r#"<Set net="N1">{}</Set>"#, pads([(5.0, 5.0, "")])),
        );
        let copper = &gerber_files(&ipc, ArtworkScope::Board)["F_Cu.gtl"];
        // The radius clamps to height / 2, so the pad images as a 2x1 obround.
        let clamped = 0.5;
        let expected = 2.0 * 1.0 - clamped * clamped * (4.0 - std::f64::consts::PI);
        let actual = area(copper);
        assert!(
            (actual - expected).abs() <= expected * 0.02,
            "expected clamped obround area {expected:.4}, got {actual:.4}"
        );
    }

    #[test]
    fn standard_dictionary_fiducials_keep_exact_circle_apertures() {
        // Repeated references to a standard catalogue entry are exact
        // primitives, not user-dictionary instances: they must flash as
        // circle apertures rather than flatten into outline macros.
        let ipc = top_copper_board(
            CIRCLE_1,
            r#"<Set>
              <LocalFiducial><Location x="3" y="3"/><StandardPrimitiveRef id="pad"/></LocalFiducial>
              <LocalFiducial><Location x="7" y="7"/><StandardPrimitiveRef id="pad"/></LocalFiducial>
            </Set>"#,
        );
        let copper = &gerber_files(&ipc, ArtworkScope::Board)["F_Cu.gtl"];
        assert!(
            !copper.contains("%AM"),
            "catalogue circles must not lower to outline macros"
        );
        assert!(
            copper.contains("C,1"),
            "fiducials should flash through a shared circle aperture"
        );
    }

    #[test]
    fn layer_plans_name_every_exported_layer_once() {
        for (layers, expected) in [
            // Drill and rout layers are NC files, not Gerber layers.
            (
                r#"<Layer name="Edge.Cuts" layerFunction="BOARD_OUTLINE" side="ALL"/>
                   <Layer name="Drill" layerFunction="DRILL" side="ALL"/>
                   <Layer name="F.Cu_B.Cu_1" layerFunction="ROUT" side="ALL"/>"#,
                vec!["Edge_Cuts.gm1: Profile,NP"],
            ),
            // Drawings keep their source names under valid X2 functions.
            (
                r#"<Layer name="F.Fab" layerFunction="ASSEMBLY" side="TOP"/>
                   <Layer name="B.Fab" layerFunction="ASSEMBLY" side="BOTTOM"/>
                   <Layer name="Assembly Notes" layerFunction="ASSEMBLY" side="NONE"/>
                   <Layer name="Board Fab" layerFunction="BOARD_FAB" side="ALL"/>"#,
                vec![
                    "F_Fab.gbr: AssemblyDrawing,Top",
                    "B_Fab.gbr: AssemblyDrawing,Bot",
                    "Assembly_Notes.gbr: OtherDrawing,Assembly",
                    "Board_Fab.gbr: FabricationDrawing",
                ],
            ),
            // A repeated role falls back to its source layer name.
            (
                r#"<Layer name="ROUT-A" layerFunction="ROUT" side="ALL"/>
                   <Layer name="ROUT-B" layerFunction="ROUT" side="ALL"/>
                   <Layer name="VCUT-A" layerFunction="V_CUT" side="NONE"/>
                   <Layer name="VCUT-B" layerFunction="V_CUT" side="NONE"/>
                   <Layer name="SCORE-A" layerFunction="SCORE" side="NONE"/>
                   <Layer name="SCORE-B" layerFunction="SCORE" side="NONE"/>"#,
                vec![
                    "V_Cut.gbr: Vcut",
                    "VCUT_B.gbr: Vcut",
                    "Score.gbr: Vcut",
                    "SCORE_B.gbr: Vcut",
                ],
            ),
        ] {
            let ipc = ipc::Ipc2581::parse(&format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner"><FunctionMode mode="FABRICATION"/></Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      {layers}
      <Step name="board" type="BOARD"/>
    </CadData>
  </Ecad>
</IPC-2581>"#
            ))
            .unwrap();
            let imported = import_design(&ipc, Resolution::default()).unwrap();
            let outputs = export_layer_plans(&imported, &imported.layer_definitions)
                .iter()
                .map(|plan| format!("{}: {}", plan.filename, plan.file_function.join(",")))
                .collect::<Vec<_>>();
            assert_eq!(outputs, expected);
        }
    }

    #[test]
    fn assembly_gerbers_preserve_phantom_patterns_for_boards_and_arrays() {
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

        let board_fab = &gerber_files(&ipc, ArtworkScope::Board)["F_Fab.gbr"];
        assert!(board_fab.contains("%TF.FileFunction,AssemblyDrawing,Top*%"));
        assert!(board_fab.contains("%TF.Part,Single*%"));
        assert_eq!(
            count(board_fab, |kind| matches!(kind, ObjectKind::Draw { .. })),
            2
        );
        assert_eq!(
            count(board_fab, |kind| matches!(kind, ObjectKind::Flash { .. })),
            2
        );

        let array_fab = &gerber_files(&ipc, ArtworkScope::ArrayFlattened)["F_Fab.gbr"];
        assert!(array_fab.contains("%TF.Part,Array*%"));
        assert!(!array_fab.contains("%ABD"));
        assert!(array_fab.contains("%SRX2Y1I30J0*%"));
        assert_eq!(count(array_fab, |_| true), 4);
        let artwork = extracted(array_fab);
        assert_eq!(artwork.blocks.len(), 1);
        assert_eq!(
            pcb_ir::dialects::artwork::expand_instances(&artwork)
                .objects
                .len(),
            8
        );
    }

    #[test]
    fn standalone_profile_export_matches_both_layout_targets() {
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
            let array = manufacturing_files(&ipc, ArtworkScope::ArrayFlattened);
            assert_eq!(manufacturing_files(&ipc, ArtworkScope::Board), array);
            let profile = &array["Edge_Cuts.gm1"];
            assert!(profile.contains("%TF.FileFunction,Profile,NP*%"));
            assert!(profile.contains("%TF.Part,Single*%"));
            assert!(profile.contains("%TA.AperFunction,Profile*%"));
            assert!(profile.contains("%ADD10C,0.05*%"));
            let geometry = extracted(profile);
            geometry.validate().unwrap();
            assert_eq!(geometry.layers[0].objects.count, 8);
        }
    }

    #[test]
    fn exports_ipc_layer_to_parseable_gerber_x2() {
        let ipc = top_copper_board(
            CIRCLE_1,
            &format!(
                r#"<Set net="N1">{}</Set>"#,
                pads([(2.0, 3.0, r#"<PinRef componentRef="U1" pin="1"/>"#)])
            ),
        );
        let files = gerber_files(&ipc, ArtworkScope::Board);
        for contents in files.values() {
            gerberx2::GerberX2::parse(contents).unwrap();
        }
        let copper = &files["F_Cu.gtl"];
        for attribute in [
            "%TF.FileFunction,Copper,L1,Top*%",
            "%TF.Part,Single*%",
            "%TF.FilePolarity,Positive*%",
            "%TF.SameCoordinates*%",
            "%TA.AperFunction,SMDPad,CuDef*%",
            "%TO.C,U1*%",
            "%TO.P,U1,1*%",
            "%TO.N,N1*%",
        ] {
            assert!(copper.contains(attribute), "{attribute} is missing");
        }
        assert_eq!(
            count(copper, |kind| matches!(kind, ObjectKind::Flash { .. })),
            1
        );

        // An array of this one board is the board itself.
        let array_copper = &gerber_files(&ipc, ArtworkScope::ArrayFlattened)["F_Cu.gtl"];
        assert!(array_copper.contains("%TF.Part,Single*%"));
    }

    #[test]
    fn mask_and_paste_use_specification_correct_attributes() {
        let pad = r#"<Set net="N1">
            <Pad padstackDefRef="padstack">
              <Location x="2" y="3"/>
              <StandardPrimitiveRef id="pad"/>
              <PinRef componentRef="U1" pin="1"/>
            </Pad>
          </Set>"#;
        let ipc = ipc::Ipc2581::parse(&format!(
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
        <LayerFeature layerRef="F.Mask">{pad}</LayerFeature>
        <LayerFeature layerRef="F.Paste">{pad}</LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#
        ))
        .unwrap();
        let files = gerber_files(&ipc, ArtworkScope::Board);
        for (filename, polarity) in [("F_Mask.gts", "Negative"), ("F_Paste.gtp", "Positive")] {
            let layer = &files[filename];
            assert!(layer.contains(&format!("%TF.FilePolarity,{polarity}*%")));
            assert!(layer.contains("%TF.SameCoordinates*%"));
            assert!(layer.contains("%TA.AperFunction,Material*%"));
            assert!(layer.contains("%TO.C,U1*%"));
            assert!(!layer.contains("SMDPad"));
            assert!(!layer.contains("%TO.P,"));
            assert!(!layer.contains("%TO.N,"));
            gerberx2::GerberX2::parse(layer).unwrap();
        }
    }

    #[test]
    fn overlay_copper_follows_the_local_cut_ins_of_a_fill() {
        let donut = format!(
            "<Features><UserSpecial>{}{}</UserSpecial></Features>",
            rect_contour(0.0, 0.0, 10.0, 10.0),
            rect_contour(4.0, 4.0, 6.0, 6.0)
        );
        let line = |y: f64| {
            format!(
                r#"<Features><Line startX="4.2" startY="{y}" endX="5.8" endY="{y}">
                  <LineDesc lineWidth="0.5" lineEnd="ROUND"/>
                </Line></Features>"#
            )
        };
        for (features, overlay, restored_area) in [
            (
                format!(r#"<Set net="N1">{}{donut}</Set>"#, pads([(5.0, 5.0, "")])),
                "D03*",
                96.7,
            ),
            (
                format!(
                    r#"<Set net="TRACE">{}{}</Set><Set>{donut}</Set>"#,
                    line(4.6),
                    line(5.4)
                ),
                "%TO.N,TRACE*%",
                97.0,
            ),
        ] {
            let ipc = top_copper_board(CIRCLE_1, &features);
            let copper = &gerber_files(&ipc, ArtworkScope::Board)["F_Cu.gtl"];
            assert!(
                !copper.contains("%LPC*%"),
                "positive compound region holes should not export as layer-global clear regions"
            );
            let fill_end = copper.rfind("G37*").expect("the fill exports as regions");
            let overlay_start = copper.find(overlay).expect("the overlay is exported");
            assert!(fill_end < overlay_start);
            let actual = area(copper);
            assert!(
                actual > restored_area,
                "{overlay} was not restored after the local cut-in; area was {actual}"
            );
        }
    }

    #[test]
    fn gerber_export_writes_separate_nc_drill_files_with_routes() {
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
        let files = manufacturing_files(&ipc, ArtworkScope::Board);
        for absent in ["Drill.gbr", "Route.gbr", "Edge_Cuts.gm1", "PTH_Slots.drl"] {
            assert!(!files.contains_key(absent), "{absent} was exported");
        }

        let pth = &files["PTH.drl"];
        assert!(pth.contains("; #@! TF.FileFunction,Plated,1,2,PTH"));
        assert!(pth.contains("; #@! TA.AperFunction,Plated,PTH,ViaDrill\nT01C0.3"));
        assert!(pth.contains("; #@! TA.AperFunction,Plated,PTH,ComponentDrill\nT02C0.6"));
        assert!(pth.contains("X10.0Y19.45G85X10.0Y20.55\nG05"));
        let npth = &files["NPTH.drl"];
        assert!(npth.contains("; #@! TF.FileFunction,NonPlated,1,2,NPTH"));
        assert!(npth.contains("T01C0.65"));
        assert!(npth.contains("X3.0Y4.0"));
    }

    #[cfg(feature = "cli")]
    #[test]
    fn gerber_export_writes_zip_when_output_has_zip_extension() {
        let ipc = top_copper_board(CIRCLE_1, "");
        let output_zip = std::env::temp_dir().join(format!(
            "pcb-ipc-gerber-zip-test-{}.zip",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&output_zip);

        let package = manufacturing_package(&ipc, ArtworkScope::Board);
        crate::manufacturing::write_manufacturing_package(&package, &output_zip).unwrap();

        let zip_file = std::fs::File::open(&output_zip).unwrap();
        let mut archive = zip::ZipArchive::new(zip_file).unwrap();
        assert_eq!(archive.len(), package.files.len());
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
    fn gerber_export_preserves_overlapping_contours_and_local_cutouts() {
        let resolution = Resolution::default();
        let source = r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner"><FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/><LayerRef name="TOP"/>
  </Content>
  <Ecad><CadHeader units="MILLIMETER"/><CadData>
    <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
    <Step name="board" type="BOARD"><LayerFeature layerRef="TOP">
      <Set net="VCC"><Features><UserSpecial>
        <Contour>
          <Polygon>
            <PolyBegin x="0" y="0"/><PolyStepSegment x="6" y="0"/>
            <PolyStepSegment x="6" y="4"/><PolyStepSegment x="0" y="4"/>
            <PolyStepSegment x="0" y="0"/>
          </Polygon>
          <Cutout>
            <PolyBegin x="1" y="1"/><PolyStepSegment x="3" y="1"/>
            <PolyStepSegment x="3" y="3"/><PolyStepSegment x="1" y="3"/>
            <PolyStepSegment x="1" y="1"/>
          </Cutout>
        </Contour>
        <!-- Opposite winding; overlaps both the first polygon and its cutout. -->
        <Contour><Polygon>
          <PolyBegin x="8" y="2"/><PolyStepSegment x="2" y="2"/>
          <PolyStepSegment x="2" y="5"/><PolyStepSegment x="8" y="5"/>
          <PolyStepSegment x="8" y="2"/>
        </Polygon></Contour>
        <!-- A nested positive sibling is additional material, not a counter. -->
        <Contour><Polygon>
          <PolyBegin x="4" y="0.5"/><PolyStepSegment x="5" y="0.5"/>
          <PolyStepSegment x="5" y="1.5"/><PolyStepSegment x="4" y="1.5"/>
          <PolyStepSegment x="4" y="0.5"/>
        </Polygon></Contour>
      </UserSpecial></Features></Set>
    </LayerFeature></Step>
  </CadData></Ecad>
</IPC-2581>"#;
        for (function, filename) in [("SIGNAL", "F_Cu.gtl"), ("LEGEND", "F_SilkS.gto")] {
            let ipc = ipc::Ipc2581::parse(&source.replace("SIGNAL", function)).unwrap();

            // Check source import and normalization before exporting.
            let mut doc = pcb_ir::import::ipc2581::extract_layer(&ipc, "TOP", resolution).unwrap();
            let image = |doc: &GeometryDocument| {
                pcb_ir::geom::ContourSet::from_painted_paths(
                    &doc.arena,
                    doc.features
                        .iter()
                        .flat_map(|f| f.paths.slice(&doc.arena.paths)),
                    resolution,
                )
                .unwrap()
            };
            assert!((image(&doc).area() - 31.0).abs() < 1e-6);
            pcb_ir::dialects::ipc::process::normalize_for_artwork(&mut doc, resolution).unwrap();
            let region = image(&doc);
            assert!((region.area() - 31.0).abs() < 1e-6);
            assert!(!region.contains_point(pcb_ir::geom::Point::new(1.5, 2.5)));
            assert!(region.contains_point(pcb_ir::geom::Point::new(2.5, 2.5)));
            assert!(region.contains_point(pcb_ir::geom::Point::new(4.5, 1.0)));

            let layer = &gerber_files(&ipc, ArtworkScope::Board)[filename];
            assert!(!layer.contains("%LPC*%"), "cutouts must stay local");
            // 24 - 4 + 18 - (8 - 1) = 31; the nested sibling adds no new area.
            let actual = area(layer);
            assert!((actual - 31.0).abs() < 1e-6, "area: {actual}");
        }
    }

    #[test]
    fn gerber_export_preserves_user_special_counter_holes() {
        let silk_board = |contours: &str| {
            ipc::Ipc2581::parse(&format!(
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
          <Set><Features><UserSpecial>{contours}</UserSpecial></Features></Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#
            ))
            .unwrap()
        };
        let cutout = r#"<Contour>
          <Polygon>
            <PolyBegin x="0" y="0"/><PolyStepSegment x="4" y="0"/>
            <PolyStepSegment x="4" y="4"/><PolyStepSegment x="0" y="4"/>
            <PolyStepSegment x="0" y="0"/>
          </Polygon>
          <Cutout>
            <PolyBegin x="1" y="1"/><PolyStepSegment x="3" y="1"/>
            <PolyStepSegment x="3" y="3"/><PolyStepSegment x="1" y="3"/>
            <PolyStepSegment x="1" y="1"/>
          </Cutout>
        </Contour>"#;
        // KiCad's Fracture() joins a hole to its outer ring with a retraced
        // bridge. Knockout text can also leave a separate positive counter island.
        let fractured = format!(
            r#"<Contour><Polygon>
            <PolyBegin x="0" y="0"/><PolyStepSegment x="4" y="0"/>
            <PolyStepSegment x="4" y="4"/><PolyStepSegment x="0" y="4"/>
            <PolyStepSegment x="0" y="0"/><PolyStepSegment x="1" y="1"/>
            <PolyStepSegment x="1" y="3"/><PolyStepSegment x="3" y="3"/>
            <PolyStepSegment x="3" y="1"/><PolyStepSegment x="1" y="1"/>
            <PolyStepSegment x="0" y="0"/>
          </Polygon></Contour>{}"#,
            rect_contour(1.5, 1.5, 2.0, 2.0)
        );
        for (contours, expected_area) in [(cutout, 12.0), (fractured.as_str(), 12.25)] {
            let silk = &gerber_files(&silk_board(contours), ArtworkScope::Board)["F_SilkS.gto"];
            assert!(
                !silk.contains("%LPC*%"),
                "positive compound region holes should not export as layer-global clear regions"
            );
            let actual = area(silk);
            assert!(
                (actual - expected_area).abs() < 1e-6,
                "compound region should preserve its counter hole; area was {actual}"
            );
        }
    }

    #[test]
    fn gerber_preserves_leaf_board_repeats_without_nesting() {
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
        let top = &gerber_files(&ipc, ArtworkScope::ArrayFlattened)["F_Cu.gtl"];
        assert!(top.contains("%TF.Part,Array*%"));
        assert!(!top.contains("%ABD"));
        assert_eq!(top.matches("%SRX2Y1I14J0*%").count(), 1);
        assert_eq!(top.matches("%SR*%").count(), 1);
        assert!(!top.contains("%SRX3Y1I30J0*%"));
        assert!(!top.contains("%LM"));
        assert!(!top.contains("%LR"));
        assert!(!top.contains("%LS"));
        assert_eq!(
            count(top, |_| true),
            3,
            "the three panel placements each retain one board grid"
        );
        let artwork = extracted(top);
        assert_eq!(artwork.blocks.len(), 1);
        assert_eq!(
            pcb_ir::dialects::artwork::expand_instances(&artwork)
                .objects
                .len(),
            6
        );
    }

    #[test]
    fn board_array_profile_does_not_infer_reliefs_without_vcut_lines() {
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
        let files = gerber_files(&ipc, ArtworkScope::ArrayFlattened);
        assert!(!files.contains_key("V_Cut.gbr"));
        let profile = &files["Board_Array_Profile.gm1"];
        assert!(profile.contains("%TF.Part,Array*%"));
        assert!(!profile.contains("G02*"));
        assert!(!profile.contains("G03*"));
        gerberx2::GerberX2::parse(profile).unwrap();
    }

    #[test]
    fn gerber_export_carries_vcut_and_fiducial_x2_metadata() {
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
        let files = gerber_files(&ipc, ArtworkScope::ArrayFlattened);
        for (filename, attributes) in [
            (
                "F_Cu.gtl",
                [
                    "%TA.AperFunction,FiducialPad,Global*%",
                    "%TO.C,U1*%",
                    "%TO.P,U1,1*%",
                ]
                .as_slice(),
            ),
            (
                "V_Cut.gbr",
                &["%TF.FileFunction,Vcut*%", "%TA.AperFunction,Other,Vcut*%"],
            ),
            (
                "Score.gbr",
                &["%TF.FileFunction,Vcut*%", "%TA.AperFunction,Other,Score*%"],
            ),
        ] {
            let contents = &files[filename];
            assert!(contents.contains("%TF.Part,Array*%"));
            for attribute in attributes {
                assert!(contents.contains(attribute), "{filename}: {attribute}");
            }
        }
    }

    #[cfg(feature = "cli")]
    #[test]
    fn real_board_export_parseback_and_svg_paths_smoke() {
        let resolution = Resolution::default();

        let compressed = include_bytes!("../../../ipc2581/tests/data/DM0002-IPC-2518.xml.zst");
        let content = zstd::decode_all(Cursor::new(compressed)).unwrap();
        let ipc = ipc::Ipc2581::parse(std::str::from_utf8(&content).unwrap()).unwrap();
        let files = gerber_files(&ipc, ArtworkScope::Board);

        assert!(files.len() >= 10);
        assert!(files.contains_key("F_Cu.gtl"));
        assert!(files.contains_key("Edge_Cuts.gm1"));

        for (filename, contents) in &files {
            let geometry = extracted(contents);
            geometry.validate().unwrap();

            let mask = pcb_ir::dialects::artwork::compose_to_mask(&geometry, resolution).unwrap();
            mask.validate().unwrap();
            let svg = pcb_ir::render::svg(&mask, &pcb_ir::render::RenderOptions::layer(0));
            assert!(svg.contains("<svg"), "{filename} did not render SVG");
        }

        let mut layer = geometry::extract_layer(&ipc, "F.Cu", resolution).unwrap();
        pcb_ir::dialects::ipc::process::normalize_for_artwork(&mut layer, resolution).unwrap();
        let artwork = pcb_ir::dialects::ipc::lower_layer_to_artwork(
            &layer,
            0,
            LayerRole::Copper,
            pcb_ir::dialects::Side::Top,
        );
        artwork.validate().unwrap();
        let mask = pcb_ir::dialects::artwork::compose_to_mask(&artwork, resolution).unwrap();
        mask.validate().unwrap();
        assert!(
            pcb_ir::render::svg(&mask, &pcb_ir::render::RenderOptions::layer(0)).contains("<svg")
        );
    }
}
