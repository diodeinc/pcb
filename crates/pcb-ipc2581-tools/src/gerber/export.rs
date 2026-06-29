use std::collections::HashSet;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use gerberx2::{GerberLayer, write_layer};
use ipc2581::Ipc2581;
use ipc2581::types::{
    FillProperty, LayerFunction, Side as IpcSide, StandardPrimitive, ecad::Layer,
};

use super::artwork::{ArtworkLayer, LayerAttributes, ObjectAttributes};
use super::lower::lower_artwork_layer;
use crate::geometry;
use pcb_ir::common::{
    Affine2, BBox, LayerRole, LineCap, LineJoin, PaintPolarity, Point, Side as IrSide, Unit,
    arc_sweep_radians,
};
use pcb_ir::dialects::artwork::{
    ArtworkAperture, ArtworkGeometry, ArtworkObject, ArtworkPath, PaintOrder, PaintStage,
};
use pcb_ir::dialects::ipc::{
    BoardArrayFabricationProfile, FeatureBucket, FeatureDomain, FeatureOperation, FeatureRole,
    FiducialKind, GeometryFeature, GeometryPath, GeometryPathPaintClass, GeometryPolarity,
    GeometryView, PlatingKind, ProfileSet, profile_occurrences_for, relief,
    transformed_path_payloads,
};
use pcb_ir::dialects::path as common_path;

type IpcGeometryDocument = pcb_ir::dialects::ipc::GeometryDocument<ipc2581::Symbol, LayerFunction>;

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

pub fn build_gerber_x2_files(ipc: &Ipc2581, view: GeometryView) -> Result<Vec<GerberX2File>> {
    build_gerber_x2_files_with_options(ipc, view, &GerberExportOptions::default())
}

pub fn build_gerber_x2_files_with_options(
    ipc: &Ipc2581,
    view: GeometryView,
    options: &GerberExportOptions,
) -> Result<Vec<GerberX2File>> {
    if view == GeometryView::LayoutSymbolic {
        bail!("Gerber export does not support symbolic layout view; use board or board-array");
    }

    let ecad = ipc.ecad().context("IPC-2581 file has no ECAD section")?;
    let mut files = Vec::new();
    let plans = export_layer_plans(ipc, &ecad.cad_data.layers);
    let has_profile_plan = plans
        .iter()
        .any(|plan| plan.role == GerberLayerRole::Profile);

    for plan in &plans {
        let source_layer = plan.layer;
        let layer_name = ipc.resolve(source_layer.name);
        let mut doc = geometry::extract_layer_for_view(ipc, layer_name, view)
            .with_context(|| format!("failed to extract IPC-2581 layer '{layer_name}'"))?;
        pcb_ir::dialects::ipc::process::normalize_for_artwork(&mut doc);
        if let Err(error) = pcb_ir::dialects::ipc::validate_artwork_ready(&doc) {
            bail!("IPC-2581 layer '{layer_name}' is not artwork-ready: {error}");
        }
        if matches!(plan.role, GerberLayerRole::Vcut | GerberLayerRole::Score)
            && doc.layers[0].feature_count == 0
        {
            continue;
        }
        let part = gerber_part_for_doc(&doc);
        let artwork = artwork_from_ipc_layer(
            ipc,
            &doc,
            0,
            GerberArtworkSpec {
                role: plan.role,
                side: ir_side(source_layer.side),
                meta: layer_attributes(plan.file_function.clone(), part),
                view,
            },
        )?;
        let layer = lower_artwork_layer(&artwork)?;
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
    if view == GeometryView::ArrayFlattened {
        if let Some(file) =
            board_array_profile_gerber_file(ipc, options.relief_debug_dir.as_deref())?
        {
            files.push(file);
        }
    } else if !has_profile_plan && let Some(file) = synthetic_profile_gerber_file(ipc, view)? {
        files.push(file);
    }

    Ok(files)
}

struct ExportLayerPlan<'a> {
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
    Profile,
    Vcut,
    Score,
}

fn export_layer_plans<'a>(ipc: &Ipc2581, layers: &'a [Layer]) -> Vec<ExportLayerPlan<'a>> {
    let copper_count = copper_layer_count(layers);
    let mut copper_index = 0;
    let mut plans = Vec::new();
    let mut used_filenames = HashSet::new();

    for layer in layers {
        let Some(role) = gerber_layer_role(layer.layer_function) else {
            continue;
        };
        if role == GerberLayerRole::Copper {
            copper_index += 1;
        }
        let (filename, file_function) = layer_output(role, layer.side, copper_index, copper_count);
        let filename = allocate_filename(&mut used_filenames, &filename, ipc.resolve(layer.name));
        plans.push(ExportLayerPlan {
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
    match function {
        LayerFunction::Conductor
        | LayerFunction::CondFilm
        | LayerFunction::CondFoil
        | LayerFunction::Plane
        | LayerFunction::Signal
        | LayerFunction::Mixed => Some(GerberLayerRole::Copper),
        LayerFunction::Solderpaste | LayerFunction::Pastemask => Some(GerberLayerRole::Paste),
        LayerFunction::Soldermask => Some(GerberLayerRole::Soldermask),
        LayerFunction::Silkscreen | LayerFunction::Legend => Some(GerberLayerRole::Legend),
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
        GerberLayerRole::Profile => (
            "Edge_Cuts.gm1".to_string(),
            vec!["Profile".into(), "NP".into()],
        ),
        GerberLayerRole::Vcut => fabrication_line_layer_output("V_Cut.gbr", &["Vcut"], side),
        GerberLayerRole::Score => {
            fabrication_line_layer_output("Score.gbr", &["Other", "Score"], side)
        }
    }
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
        Some(IpcSide::Both) | Some(IpcSide::All) | Some(IpcSide::None) => {
            file_function.push("Top/Bot".to_string())
        }
        _ => {}
    }
    (filename.to_string(), file_function)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GerberPart {
    Single,
    Array,
}

impl GerberPart {
    fn as_str(self) -> &'static str {
        match self {
            Self::Single => "Single",
            Self::Array => "Array",
        }
    }
}

fn gerber_part_for_doc(doc: &IpcGeometryDocument) -> GerberPart {
    if pcb_ir::dialects::ipc::root_panel_step(doc).is_some() {
        GerberPart::Array
    } else {
        GerberPart::Single
    }
}

fn layer_attributes(file_function: Vec<String>, part: GerberPart) -> LayerAttributes {
    LayerAttributes {
        file_function,
        part: Some(vec![part.as_str().to_string()]),
        file_polarity: None,
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
        _ => format!("In{}_Cu.gbr", copper_index),
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
    ipc: &Ipc2581,
    doc: &IpcGeometryDocument,
    layer_index: usize,
    spec: GerberArtworkSpec,
) -> Result<ArtworkLayer> {
    let layer = &doc.layers[layer_index];
    let mut artwork = ArtworkLayer::new(Unit::Millimeter);
    let artwork_layer = artwork.push_layer(pcb_ir::dialects::artwork::ArtworkLayer {
        name: layer.name.clone(),
        role: spec.role.ir_role(),
        side: spec.side,
        object_start: 0,
        object_count: 0,
        bbox: layer.bbox,
        meta: spec.meta,
    });
    let features = &doc.features
        [layer.feature_start as usize..(layer.feature_start + layer.feature_count) as usize];

    for feature in features {
        push_artwork_feature(&mut artwork, artwork_layer, ipc, doc, feature, &layer.name)?;
    }
    if spec.role == GerberLayerRole::Profile
        && spec.view != GeometryView::ArrayFlattened
        && artwork.layers[artwork_layer as usize].object_count == 0
    {
        append_profile_occurrences(
            &mut artwork,
            artwork_layer,
            doc,
            spec.view.profile_set(),
            ProfileGerberStyle::default(),
        );
    }
    Ok(artwork)
}

struct GerberArtworkSpec {
    role: GerberLayerRole,
    side: IrSide,
    meta: LayerAttributes,
    view: GeometryView,
}

fn synthetic_profile_gerber_file(
    ipc: &Ipc2581,
    view: GeometryView,
) -> Result<Option<GerberX2File>> {
    let doc = geometry::extract_layout(ipc)?;
    let mut artwork = ArtworkLayer::new(Unit::Millimeter);
    let artwork_layer = artwork.push_layer(pcb_ir::dialects::artwork::ArtworkLayer {
        name: "Edge.Cuts".to_string(),
        role: LayerRole::Profile,
        side: IrSide::None,
        object_start: 0,
        object_count: 0,
        bbox: BBox::empty(),
        meta: layer_attributes(
            vec!["Profile".to_string(), "NP".to_string()],
            gerber_part_for_view(&doc, view),
        ),
    });
    append_profile_occurrences(
        &mut artwork,
        artwork_layer,
        &doc,
        view.profile_set(),
        ProfileGerberStyle::default(),
    );
    if artwork.layers[artwork_layer as usize].object_count == 0 {
        return Ok(None);
    }

    let layer = lower_artwork_layer(&artwork)?;
    let contents = write_layer(&layer)?;
    Ok(Some(GerberX2File {
        filename: "Edge_Cuts.gm1".to_string(),
        layer,
        contents,
    }))
}

fn board_array_profile_gerber_file(
    ipc: &Ipc2581,
    relief_debug_dir: Option<&Path>,
) -> Result<Option<GerberX2File>> {
    let doc = geometry::extract_layout(ipc)?;
    let score_lines = geometry::board_array_vscore_lines(ipc)?;
    let profile = if let Some(debug_dir) = relief_debug_dir {
        let (profile, relief_debug) =
            geometry::board_array_fabrication_profile_with_debug(ipc, &doc, &score_lines)?;
        write_vscore_relief_debug(debug_dir, &relief_debug)?;
        profile
    } else {
        geometry::board_array_fabrication_profile(ipc, &doc, &score_lines)?
    };
    if profile.array_outlines.is_empty() && profile.material_removal.is_empty() {
        return Ok(None);
    }

    let mut artwork = ArtworkLayer::new(Unit::Millimeter);
    let artwork_layer = artwork.push_layer(pcb_ir::dialects::artwork::ArtworkLayer {
        name: "Board Array Profile".to_string(),
        role: LayerRole::Profile,
        side: IrSide::None,
        object_start: 0,
        object_count: 0,
        bbox: BBox::empty(),
        meta: layer_attributes(
            vec!["Profile".to_string(), "NP".to_string()],
            GerberPart::Array,
        ),
    });
    let style = ProfileGerberStyle::default();
    append_board_array_profile(&mut artwork, artwork_layer, &profile, style);
    if artwork.layers[artwork_layer as usize].object_count == 0 {
        return Ok(None);
    }

    let layer = lower_artwork_layer(&artwork)?;
    let contents = write_layer(&layer)?;
    Ok(Some(GerberX2File {
        filename: "Board_Array_Profile.gm1".to_string(),
        layer,
        contents,
    }))
}

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
struct DebugSvgPathStyle {
    class_name: &'static str,
    fill: &'static str,
    stroke: &'static str,
    stroke_width: &'static str,
    extra_attrs: &'static str,
}

fn write_debug_path(
    svg: &mut String,
    entry_index: usize,
    payloads: &[common_path::PathPayload],
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

fn debug_path_data(payloads: &[common_path::PathPayload]) -> Option<String> {
    let mut data = String::new();
    for payload in payloads {
        append_debug_path_cmds(&mut data, &payload.cmds);
    }
    (!data.is_empty()).then_some(data)
}

fn append_debug_path_cmds(data: &mut String, cmds: &[common_path::PathCmd]) {
    let mut current = Point::default();
    for cmd in cmds {
        match cmd.op {
            common_path::PathOp::MoveTo => {
                current = cmd.p0;
                if !data.is_empty() {
                    data.push(' ');
                }
                write!(data, "M{} {}", debug_num(cmd.p0.x), debug_num(cmd.p0.y)).unwrap();
            }
            common_path::PathOp::LineTo => {
                current = cmd.p0;
                write!(data, " L{} {}", debug_num(cmd.p0.x), debug_num(cmd.p0.y)).unwrap();
            }
            common_path::PathOp::ArcTo => {
                write_debug_arc(data, current, cmd.p0, cmd.p1, cmd.clockwise);
                current = cmd.p0;
            }
            common_path::PathOp::CubicTo => {
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
            common_path::PathOp::Close => data.push_str(" Z"),
        }
    }
}

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
        u8::from(arc_sweep_radians(start, end, center, clockwise) > std::f64::consts::PI);
    write_debug_svg_arc(data, radius, large_arc, sweep_flag, end);
}

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

fn payloads_bbox(payloads: &[common_path::PathPayload]) -> BBox {
    payloads
        .iter()
        .fold(BBox::empty(), |bbox, payload| bbox.union(payload.bbox))
}

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

fn gerber_part_for_view(doc: &IpcGeometryDocument, view: GeometryView) -> GerberPart {
    if view == GeometryView::Board {
        GerberPart::Single
    } else {
        gerber_part_for_doc(doc)
    }
}

fn append_profile_occurrences(
    artwork: &mut ArtworkLayer,
    layer: u32,
    doc: &IpcGeometryDocument,
    profile_set: ProfileSet,
    style: ProfileGerberStyle,
) {
    for occurrence in profile_occurrences_for(doc, profile_set) {
        append_profile_path(
            artwork,
            layer,
            doc,
            occurrence.profile.outer_path,
            occurrence.transform,
            style,
        );
        append_profile_cutouts(
            artwork,
            layer,
            doc,
            occurrence.profile,
            occurrence.transform,
            style,
        );
    }
}

fn append_board_array_profile(
    artwork: &mut ArtworkLayer,
    layer: u32,
    profile: &BoardArrayFabricationProfile,
    style: ProfileGerberStyle,
) {
    for outline in &profile.array_outlines {
        append_profile_payloads(artwork, layer, outline.clone(), style);
    }
    if !profile.material_removal.is_empty() {
        append_profile_payloads(artwork, layer, profile.material_removal.clone(), style);
    }
}

fn append_profile_cutouts(
    artwork: &mut ArtworkLayer,
    layer: u32,
    doc: &IpcGeometryDocument,
    profile: &pcb_ir::dialects::ipc::StepProfile,
    transform: Affine2,
    style: ProfileGerberStyle,
) {
    for cutout in &doc.profile_cutouts
        [profile.cutout_start as usize..(profile.cutout_start + profile.cutout_count) as usize]
    {
        append_profile_path(artwork, layer, doc, cutout.path, transform, style);
    }
}

fn append_profile_path(
    artwork: &mut ArtworkLayer,
    layer: u32,
    doc: &IpcGeometryDocument,
    path: u32,
    transform: Affine2,
    style: ProfileGerberStyle,
) {
    append_profile_payloads(
        artwork,
        layer,
        transformed_path_payloads(doc, path, transform),
        style,
    );
}

fn append_profile_payloads(
    artwork: &mut ArtworkLayer,
    layer: u32,
    payloads: Vec<common_path::PathPayload>,
    style: ProfileGerberStyle,
) {
    let path = artwork.push_path(
        ArtworkPath::stroked(style.stroke_width_mm, style.line_cap, style.line_join),
        payloads,
    );
    let bbox = artwork.paths[path as usize].bbox;
    artwork.push_object(
        layer,
        ArtworkObject {
            paint: PaintPolarity::Dark,
            order: PaintOrder {
                stage: PaintStage::Overlay,
            },
            geometry: ArtworkGeometry::Stroke { path },
            net: None,
            bbox,
            meta: ObjectAttributes {
                aperture_function: Some(vec!["Profile".to_string()]),
                ..ObjectAttributes::default()
            },
        },
    );
    artwork.layers[layer as usize].bbox = artwork.layers[layer as usize].bbox.union(bbox);
}

fn ir_side(side: Option<IpcSide>) -> IrSide {
    match side {
        Some(IpcSide::Top) => IrSide::Top,
        Some(IpcSide::Bottom) => IrSide::Bottom,
        _ => IrSide::None,
    }
}

fn push_artwork_feature(
    artwork: &mut ArtworkLayer,
    artwork_layer: u32,
    ipc: &Ipc2581,
    doc: &IpcGeometryDocument,
    feature: &GeometryFeature<ipc2581::Symbol>,
    layer_name: &str,
) -> Result<()> {
    if let Some((aperture, at, bbox)) = standard_flash_aperture(ipc, feature) {
        let aperture = artwork.push_aperture(aperture);
        artwork.push_object(
            artwork_layer,
            ArtworkObject {
                paint: paint_polarity(feature.polarity),
                order: paint_order(feature),
                geometry: ArtworkGeometry::Flash {
                    aperture,
                    transform: Affine2::placement(at, 0.0, false, 1.0),
                },
                net: None,
                bbox,
                meta: object_attributes(ipc, doc, feature, Some(aperture_function(feature))),
            },
        );
        return Ok(());
    }

    if let Some((at, diameter)) = circle_flash(doc, feature) {
        artwork.push_object(
            artwork_layer,
            ArtworkObject {
                paint: paint_polarity(feature.polarity),
                order: paint_order(feature),
                geometry: ArtworkGeometry::CircleFlash { at, diameter },
                net: None,
                bbox: BBox::from_point(at).expand(diameter / 2.0),
                meta: object_attributes(ipc, doc, feature, Some(aperture_function(feature))),
            },
        );
        return Ok(());
    }

    for path in
        &doc.paths[feature.path_start as usize..(feature.path_start + feature.path_count) as usize]
    {
        let aperture_function = matches!(
            path.paint_class().ok().flatten(),
            Some(GeometryPathPaintClass::Stroked)
        )
        .then(|| aperture_function(feature));
        push_artwork_object(
            artwork,
            artwork_layer,
            doc,
            feature,
            path,
            object_attributes(ipc, doc, feature, aperture_function),
            layer_name,
        )?;
    }

    Ok(())
}

fn standard_flash_aperture(
    ipc: &Ipc2581,
    feature: &GeometryFeature<ipc2581::Symbol>,
) -> Option<(ArtworkAperture, Point, BBox)> {
    if !standard_flash_feature_is_eligible(feature) {
        return None;
    }

    let primitive = standard_primitive_for_feature(ipc, feature)?;
    if !standard_primitive_is_solid_fill(primitive) {
        return None;
    }

    let aperture = match primitive {
        StandardPrimitive::Circle(circle) => {
            let scale = uniform_scale(feature.transform)?;
            ArtworkAperture::Circle {
                diameter: circle.shape.diameter * scale,
            }
        }
        StandardPrimitive::RectCenter(rect) => {
            let (width, height) = axis_aligned_size(
                feature.transform,
                rect.shape.size.width,
                rect.shape.size.height,
            )?;
            ArtworkAperture::Rectangle { width, height }
        }
        StandardPrimitive::Oval(oval) => {
            let (width, height) = axis_aligned_size(
                feature.transform,
                oval.shape.size.width,
                oval.shape.size.height,
            )?;
            ArtworkAperture::Obround { width, height }
        }
        _ => return None,
    };

    let at = feature.center;
    let bbox = flash_bbox(at, aperture);
    Some((aperture, at, bbox))
}

fn standard_flash_feature_is_eligible(feature: &GeometryFeature<ipc2581::Symbol>) -> bool {
    feature.polarity == GeometryPolarity::Positive
        && feature.path_count != 0
        && (matches!(
            feature.intent.role,
            FeatureRole::Pad | FeatureRole::Via | FeatureRole::Hole
        ) || feature.is_fiducial())
}

fn standard_primitive_for_feature<'a>(
    ipc: &'a Ipc2581,
    feature: &GeometryFeature<ipc2581::Symbol>,
) -> Option<&'a StandardPrimitive> {
    let primitive_ref = feature.primitive_ref?;
    ipc.content()
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

fn flash_bbox(at: Point, aperture: ArtworkAperture) -> BBox {
    let (width, height) = match aperture {
        ArtworkAperture::Circle { diameter } => (diameter, diameter),
        ArtworkAperture::Rectangle { width, height }
        | ArtworkAperture::Obround { width, height } => (width, height),
    };
    BBox {
        min: Point::new(at.x - width / 2.0, at.y - height / 2.0),
        max: Point::new(at.x + width / 2.0, at.y + height / 2.0),
    }
}

const GEOMETRY_EPSILON: f64 = 1e-9;

fn nearly_equal(left: f64, right: f64) -> bool {
    (left - right).abs() <= GEOMETRY_EPSILON * left.abs().max(right.abs()).max(1.0)
}

fn paint_polarity(polarity: GeometryPolarity) -> PaintPolarity {
    match polarity {
        GeometryPolarity::Positive => PaintPolarity::Dark,
        GeometryPolarity::Negative => PaintPolarity::Clear,
    }
}

fn paint_order(feature: &GeometryFeature<ipc2581::Symbol>) -> PaintOrder {
    let stage = if feature.intent.role == FeatureRole::Cutout || feature.is_drill_like() {
        PaintStage::FinalCutout
    } else if feature.polarity == GeometryPolarity::Negative
        || feature.flags.clears_previous_in_set
        || matches!(
            feature.bucket,
            FeatureBucket::Fill | FeatureBucket::Thermal | FeatureBucket::Antipad
        )
    {
        PaintStage::Base
    } else {
        PaintStage::Overlay
    };
    PaintOrder { stage }
}

fn circle_flash(
    doc: &IpcGeometryDocument,
    feature: &GeometryFeature<ipc2581::Symbol>,
) -> Option<(Point, f64)> {
    if feature.outer_diameter <= 0.0 || feature.path_count != 1 {
        return None;
    }

    let path = &doc.paths[feature.path_start as usize];
    if path.paint_class().ok().flatten() != Some(GeometryPathPaintClass::Filled) {
        return None;
    }

    if feature.is_fiducial()
        || feature.intent.role == FeatureRole::Hole
        || feature.intent.operation == FeatureOperation::Drill
    {
        Some((feature.center, feature.outer_diameter))
    } else {
        None
    }
}

fn object_attributes(
    ipc: &Ipc2581,
    doc: &IpcGeometryDocument,
    feature: &GeometryFeature<ipc2581::Symbol>,
    aperture_function: Option<Vec<String>>,
) -> ObjectAttributes {
    let pin_ref = (feature.pin_ref_count > 0)
        .then(|| doc.pin_refs.get(feature.pin_ref_start as usize))
        .flatten();
    ObjectAttributes {
        aperture_function,
        net: feature.net.map(|symbol| ipc.resolve(symbol).to_string()),
        component: pin_ref
            .and_then(|pin_ref| pin_ref.component_ref)
            .map(|symbol| ipc.resolve(symbol).to_string()),
        pin: pin_ref.map(|pin_ref| ipc.resolve(pin_ref.pin).to_string()),
    }
}

fn aperture_function(feature: &GeometryFeature<ipc2581::Symbol>) -> Vec<String> {
    match feature.intent.operation {
        FeatureOperation::Drill => return vec!["Other".to_string(), "Drill".to_string()],
        FeatureOperation::Score if feature.is_vcut() => {
            return vec!["Other".to_string(), "Vcut".to_string()];
        }
        FeatureOperation::Score if feature.is_score() => {
            return vec!["Other".to_string(), "Score".to_string()];
        }
        FeatureOperation::Route | FeatureOperation::Profile => return vec!["Profile".to_string()],
        _ => {}
    }

    match feature.intent.role {
        _ if feature.is_fiducial() => return fiducial_aperture_function(feature),
        FeatureRole::Pad => {
            return match feature.intent.plating {
                PlatingKind::Plated => vec!["ComponentPad".to_string()],
                PlatingKind::Via => vec!["ViaPad".to_string()],
                _ => vec!["SMDPad".to_string()],
            };
        }
        FeatureRole::Via => return vec!["ViaPad".to_string()],
        FeatureRole::Conductor => return vec!["Conductor".to_string()],
        FeatureRole::Thermal => return vec!["ThermalRelief".to_string()],
        FeatureRole::Antipad => return vec!["AntiPad".to_string()],
        FeatureRole::Hole | FeatureRole::Slot | FeatureRole::Cutout => {
            return vec!["Other".to_string()];
        }
        FeatureRole::ArraySeparation if feature.is_vcut() => {
            return vec!["Other".to_string(), "Vcut".to_string()];
        }
        FeatureRole::ArraySeparation if feature.is_score() => {
            return vec!["Other".to_string(), "Score".to_string()];
        }
        FeatureRole::Route | FeatureRole::BoardOutline => return vec!["Profile".to_string()],
        _ => {}
    }

    match feature.intent.domain {
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
        | FeatureDomain::Unknown => vec!["Other".to_string()],
    }
}

fn fiducial_aperture_function(feature: &GeometryFeature<ipc2581::Symbol>) -> Vec<String> {
    let kind = match feature.fiducial_kind {
        FiducialKind::Unknown => "Global",
        FiducialKind::Local => "Local",
        FiducialKind::Global => "Global",
        FiducialKind::Panel | FiducialKind::GoodPanel => "Panel",
        FiducialKind::BadBoard => {
            return vec!["Other".to_string(), "BadBoardMark".to_string()];
        }
    };
    vec!["FiducialPad".to_string(), kind.to_string()]
}

fn push_artwork_path(
    artwork: &mut ArtworkLayer,
    artwork_path: ArtworkPath,
    doc: &IpcGeometryDocument,
    path: &GeometryPath,
) -> u32 {
    artwork.push_path(artwork_path, artwork_contours(doc, path))
}

fn push_artwork_object(
    artwork: &mut ArtworkLayer,
    artwork_layer: u32,
    doc: &IpcGeometryDocument,
    feature: &GeometryFeature<ipc2581::Symbol>,
    path: &GeometryPath,
    meta: ObjectAttributes,
    layer_name: &str,
) -> Result<()> {
    let paint_class = path.paint_class().map_err(|error| {
        anyhow::anyhow!("processed IPC geometry path is invalid on layer '{layer_name}': {error}")
    })?;
    let (geometry, path_id) = match paint_class {
        Some(GeometryPathPaintClass::Filled) => {
            let path = push_artwork_path(
                artwork,
                ArtworkPath::filled(path.style.fill.rule),
                doc,
                path,
            );
            (ArtworkGeometry::Region { path }, path)
        }
        Some(GeometryPathPaintClass::Stroked) => {
            let artwork_path = ArtworkPath::stroked_with_pattern(
                path.style.stroke.width,
                path.style.stroke.line_cap,
                LineJoin::Round,
                path.style.stroke.pattern,
            );
            let path = push_artwork_path(artwork, artwork_path, doc, path);
            (ArtworkGeometry::Stroke { path }, path)
        }
        None => {
            bail!(
                "processed IPC geometry path is neither filled nor stroked on layer '{layer_name}'"
            );
        }
    };
    artwork.push_object(
        artwork_layer,
        ArtworkObject {
            paint: paint_polarity(feature.polarity),
            order: paint_order(feature),
            geometry,
            net: None,
            bbox: artwork.paths[path_id as usize].bbox,
            meta,
        },
    );
    Ok(())
}

fn artwork_contours(
    doc: &IpcGeometryDocument,
    path: &GeometryPath,
) -> Vec<common_path::PathPayload> {
    doc.contours[path.contour_start as usize..(path.contour_start + path.contour_count) as usize]
        .iter()
        .map(|contour| common_path::PathPayload {
            bbox: contour.bbox,
            cmds: doc.path_cmds
                [contour.cmd_start as usize..(contour.cmd_start + contour.cmd_count) as usize]
                .to_vec(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc2581 as ipc;
    use crate::manufacturing::{
        ManufacturingExportOptions, ManufacturingFileKind, build_manufacturing_package,
        export_manufacturing_package,
    };
    use std::io::{Cursor, Read};

    #[test]
    fn drill_and_route_layers_are_not_exported_as_gerber_layers() {
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
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();
        let layers = &ipc.ecad().unwrap().cad_data.layers;

        let filenames = export_layer_plans(&ipc, layers)
            .into_iter()
            .map(|plan| plan.filename)
            .collect::<Vec<_>>();

        assert_eq!(filenames, ["Edge_Cuts.gm1"]);
    }

    #[test]
    fn repeated_fabrication_layer_roles_export_to_unique_filenames() {
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
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();
        let layers = &ipc.ecad().unwrap().cad_data.layers;

        let filenames = export_layer_plans(&ipc, layers)
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

        let files = build_gerber_x2_files(&ipc, GeometryView::Board).unwrap();
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
    fn exports_ipc_layer_to_parseable_gerber_x2() {
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
        let files = build_gerber_x2_files(&ipc, GeometryView::Board).unwrap();

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
        assert!(copper.contents.contains("%TA.AperFunction,SMDPad*%"));
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

        let panel_target_files = build_gerber_x2_files(&ipc, GeometryView::ArrayFlattened).unwrap();

        let panel_target_copper = panel_target_files
            .iter()
            .find(|file| file.filename == "F_Cu.gtl")
            .unwrap();
        assert!(panel_target_copper.contents.contains("%TF.Part,Single*%"));
        assert!(!panel_target_copper.contents.contains("%TF.Part,Array*%"));
    }

    #[test]
    fn gerber_export_places_pad_flashes_after_local_fill_cut_ins() {
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
        let files = build_gerber_x2_files(&ipc, GeometryView::Board).unwrap();

        let copper = files
            .iter()
            .find(|file| file.filename == "F_Cu.gtl")
            .unwrap();
        let parsed = gerberx2::GerberX2::parse(&copper.contents).unwrap();
        assert!(
            parsed
                .objects()
                .iter()
                .all(|object| object.polarity == pcb_ir::dialects::gerber::Polarity::Dark),
            "positive compound region holes should not export as layer-global clear regions"
        );
        let region_index = parsed
            .objects()
            .iter()
            .position(|object| {
                object.polarity == pcb_ir::dialects::gerber::Polarity::Dark
                    && matches!(object.kind, gerberx2::ObjectKind::Region { .. })
            })
            .expect("compound fill should export as a dark local cut-in region");
        let pad_flash_index = parsed
            .objects()
            .iter()
            .position(|object| matches!(object.kind, gerberx2::ObjectKind::Flash { .. }))
            .expect("standard circular pad should export as a flash");
        assert!(region_index < pad_flash_index);

        let geometry = gerberx2::geometry::extract_document(&parsed);
        let summary = pcb_ir::dialects::gerber::compare::summarize(&geometry);
        assert!(
            summary.area_mm2 > 96.7,
            "pad flash was not restored after local clear; area was {}",
            summary.area_mm2
        );
    }

    #[test]
    fn gerber_export_places_multi_contour_traces_after_local_fill_cut_ins() {
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
        let files = build_gerber_x2_files(&ipc, GeometryView::Board).unwrap();

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
        let geometry = gerberx2::geometry::extract_document(&parsed);
        let summary = pcb_ir::dialects::gerber::compare::summarize(&geometry);
        assert!(
            summary.area_mm2 > 97.0,
            "multi-contour trace was not restored after local clear; area was {}",
            summary.area_mm2
        );
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
        let package = build_manufacturing_package(&ipc, GeometryView::Board).unwrap();

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

    #[test]
    fn gerber_export_writes_zip_when_output_has_zip_extension() {
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
                view: GeometryView::Board,
                relief_debug_dir: None,
            },
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
    fn gerber_export_rejects_symbolic_layout_view() {
        let ipc = ipc::Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
  </Content>
</IPC-2581>"#,
        )
        .unwrap();
        let error = build_manufacturing_package(&ipc, GeometryView::LayoutSymbolic).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("manufacturing export does not support symbolic layout view")
        );
    }

    #[test]
    fn gerber_export_preserves_user_special_counter_holes() {
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
        let files = build_gerber_x2_files(&ipc, GeometryView::Board).unwrap();

        let silk = files
            .iter()
            .find(|file| file.filename == "F_SilkS.gto")
            .unwrap();
        assert!(
            !silk.contents.contains("%LPC*%"),
            "positive compound region holes should not export as layer-global clear regions"
        );
        let parsed = gerberx2::GerberX2::parse(&silk.contents).unwrap();
        let geometry = gerberx2::geometry::extract_document(&parsed);
        let summary = pcb_ir::dialects::gerber::compare::summarize(&geometry);
        assert!(
            (summary.area_mm2 - 12.0).abs() < 1e-6,
            "compound region should preserve its counter hole; area was {}",
            summary.area_mm2
        );
    }

    #[test]
    fn gerber_board_array_target_flattens_repeated_board_instances_as_array() {
        let ipc = ipc::Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="panel"/>
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
        <StepRepeat stepRef="board" x="4" y="6" nx="2" ny="1" dx="14" dy="0"/>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();
        let files = build_gerber_x2_files(&ipc, GeometryView::ArrayFlattened).unwrap();

        let top = files
            .iter()
            .find(|file| file.filename == "F_Cu.gtl")
            .unwrap();
        assert!(top.contents.contains("%TF.Part,Array*%"));

        let parsed = gerberx2::GerberX2::parse(&top.contents).unwrap();
        let geometry = gerberx2::geometry::extract_document(&parsed);
        assert!(geometry.objects.len() >= 2);
        assert!(geometry.layers[0].bbox.width() > 14.0);
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
        let files = build_gerber_x2_files(&ipc, GeometryView::ArrayFlattened).unwrap();

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
        let files = build_gerber_x2_files(&ipc, GeometryView::ArrayFlattened).unwrap();

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
        assert!(vcut.contents.contains("%TF.FileFunction,Vcut,Top/Bot*%"));
        assert!(vcut.contents.contains("%TF.Part,Array*%"));
        assert!(vcut.contents.contains("%TA.AperFunction,Other,Vcut*%"));

        let score = files
            .iter()
            .find(|file| file.filename == "Score.gbr")
            .unwrap();
        assert!(
            score
                .contents
                .contains("%TF.FileFunction,Other,Score,Top/Bot*%")
        );
        assert!(score.contents.contains("%TF.Part,Array*%"));
        assert!(score.contents.contains("%TA.AperFunction,Other,Score*%"));
        assert!(!score.contents.contains("Vcut"));
    }

    #[test]
    fn real_board_export_parseback_and_svg_paths_smoke() {
        let compressed = include_bytes!("../../../ipc2581/tests/data/DM0002-IPC-2518.xml.zst");
        let content = zstd::decode_all(Cursor::new(compressed)).unwrap();
        let ipc = ipc::Ipc2581::parse(std::str::from_utf8(&content).unwrap()).unwrap();
        let files = build_gerber_x2_files(&ipc, GeometryView::Board).unwrap();

        assert!(files.len() >= 10);
        assert!(files.iter().any(|file| file.filename == "F_Cu.gtl"));
        assert!(files.iter().any(|file| file.filename == "Edge_Cuts.gm1"));

        for file in &files {
            let parsed = gerberx2::GerberX2::parse(&file.contents).unwrap();
            let geometry = gerberx2::geometry::extract_document(&parsed);
            geometry.validate().unwrap();
            let svg = pcb_ir::dialects::gerber::svg::render_svg(&geometry);
            assert!(svg.contains("<svg"), "{} did not render SVG", file.filename);

            let mask = pcb_ir::dialects::artwork::compose_to_mask(&geometry);
            mask.validate().unwrap();
        }

        let mut layer = geometry::extract_layer(&ipc, "F.Cu").unwrap();
        pcb_ir::dialects::ipc::process::compose_for_rendering(&mut layer);
        let artwork = pcb_ir::dialects::ipc::lower_layer_to_artwork(
            &layer,
            0,
            LayerRole::Copper,
            pcb_ir::common::Side::Top,
        );
        artwork.validate().unwrap();
        let mask = pcb_ir::dialects::artwork::compose_to_mask(&artwork);
        mask.validate().unwrap();
        assert!(pcb_ir::dialects::mask::render_svg(&mask, 0).contains("<svg"));

        pcb_ir::dialects::ipc::process::flatten_layers_to_masks(&mut layer);
        let flat_artwork = pcb_ir::dialects::ipc::lower_layer_to_artwork(
            &layer,
            0,
            LayerRole::Copper,
            pcb_ir::common::Side::Top,
        );
        flat_artwork.validate().unwrap();
        let flat_mask = pcb_ir::dialects::artwork::compose_to_mask(&flat_artwork);
        flat_mask.validate().unwrap();
        assert!(pcb_ir::dialects::mask::render_svg(&flat_mask, 0).contains("<svg"));
    }
}
