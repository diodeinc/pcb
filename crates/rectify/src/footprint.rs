//! Parse `.kicad_mod` footprints via `pcb-sexpr`.
//!
//! Only the subset of the sexpr that the pose solver needs is extracted:
//! pads (with SMD/THT classification and F.Cu filter), holes, silk/fab/
//! courtyard line polygons, and the `(model ...)` block's path/rotate/offset
//! plus any embedded STEP bytes.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use pcb_sexpr::{Sexpr, SexprKind};

use crate::pose::EulerPose;

/// Everything the pose solver needs from a `.kicad_mod` file.
#[derive(Debug)]
pub struct FootprintData {
    pub path: PathBuf,
    pub name: String,
    /// Copper-layer pad shapes, in footprint-local coordinates.
    pub pads: Vec<PadShape>,
    /// Physical through-hole pad shapes (pads with type `thru_hole` /
    /// `np_thru_hole`). These preserve the legacy mixed-footprint behavior.
    pub holes: Vec<PadShape>,
    /// Actual drilled board holes from through-hole pads. These use `(drill ...)`
    /// geometry, not copper pad size, and include mechanical / non-plated holes.
    pub physical_drills: Vec<PadShape>,
    /// Connected through-hole drill shapes. Mechanical / non-plated holes are
    /// excluded because they are not pins the model needs to fit through.
    pub connected_holes: Vec<PadShape>,
    /// Mechanical / non-connected drill shapes. These are useful for THT
    /// alignment-pin tie breakers but should not drive conductive pin matching.
    pub mechanical_drills: Vec<PadShape>,
    pub silk: Option<Vec<Segment>>,
    pub fab: Option<Vec<Segment>>,
    pub courtyard: Option<Vec<Segment>>,
    /// The selected model block, if a usable one was found. `None` when all
    /// model blocks reference unusable formats (e.g. `.wrl` only) or the
    /// footprint has no `(model ...)` block at all.
    pub model: Option<ModelSpec>,
    #[allow(dead_code)] // Retained for future use (e.g. SMD-vs-THT heuristics).
    pub smd_pad_count: usize,
    #[allow(dead_code)] // Retained for future use (e.g. SMD-vs-THT heuristics).
    pub thru_hole_pad_count: usize,
}

impl FootprintData {
    #[allow(dead_code)] // Useful public API.
    pub fn is_smd_only(&self) -> bool {
        self.smd_pad_count > 0 && self.thru_hole_pad_count == 0
    }

    pub fn has_holes(&self) -> bool {
        !self.holes.is_empty()
    }

    pub fn footprint_kind(&self) -> FootprintKind {
        match (self.smd_pad_count > 0, self.thru_hole_pad_count > 0) {
            (true, true) => FootprintKind::Mixed,
            (true, false) => FootprintKind::SmdOnly,
            (false, true) => FootprintKind::ThtOnly,
            (false, false) => FootprintKind::Other,
        }
    }

    /// Returns a reference to the model spec, or an error describing why no
    /// usable model is available.
    pub fn require_model(&self) -> Result<&ModelSpec> {
        self.model.as_ref().ok_or_else(|| {
            anyhow!("footprint has no usable STEP model (only .wrl models found, or no (model ...) block)")
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FootprintKind {
    SmdOnly,
    ThtOnly,
    Mixed,
    Other,
}

impl FootprintKind {
    pub fn label(self) -> &'static str {
        match self {
            FootprintKind::SmdOnly => "smd_only",
            FootprintKind::ThtOnly => "tht_only",
            FootprintKind::Mixed => "mixed",
            FootprintKind::Other => "other",
        }
    }
}

/// A single pad shape in footprint-local coordinates. Rotation and translation
/// have already been baked in; consumers rasterize this directly.
#[derive(Debug, Clone)]
pub struct PadShape {
    pub kind: PadKind,
    pub at: [f64; 2],
    pub size: [f64; 2],
    pub angle_deg: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PadKind {
    Rect,
    RoundRect,
    Trapezoid,
    Circle,
    Oval,
}

#[derive(Debug, Clone)]
pub struct Segment {
    pub a: [f64; 2],
    pub b: [f64; 2],
}

#[derive(Debug, Clone)]
pub struct ModelSpec {
    pub path: String,
    pub filename: String,
    pub rotate: EulerPose,
    pub offset: [f64; 3],
    /// Decompressed raw STEP bytes, if this footprint embeds its own model.
    pub embedded_step: Option<Vec<u8>>,
}

pub fn parse(path: &Path) -> Result<FootprintData> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    parse_content(&content, path)
}

pub fn parse_content(content: &str, path: &Path) -> Result<FootprintData> {
    let root = pcb_sexpr::parse(content).map_err(|e| anyhow!("sexpr parse: {e}"))?;
    let items = root
        .as_list()
        .ok_or_else(|| anyhow!("expected list at footprint root"))?;
    let tag = items.first().and_then(Sexpr::as_sym).unwrap_or("");
    if tag != "footprint" && tag != "module" {
        bail!("expected (footprint ...) or (module ...)");
    }
    let name = items
        .get(1)
        .and_then(|s| s.as_str().or_else(|| s.as_sym()))
        .unwrap_or_else(|| path.file_stem().and_then(|s| s.to_str()).unwrap_or(""))
        .to_string();

    let mut pads: Vec<PadShape> = Vec::new();
    let mut holes: Vec<PadShape> = Vec::new();
    let mut physical_drills: Vec<PadShape> = Vec::new();
    let mut connected_holes: Vec<PadShape> = Vec::new();
    let mut mechanical_drills: Vec<PadShape> = Vec::new();
    // Wildcard-copper THT pads (`*.Cu` / `F&B.Cu`). Folded into `pads`
    // below only when the footprint has no literal-F.Cu pad — that covers
    // all-THT connectors (Molex CONN_SD) without polluting the pad grid on
    // mixed SMD+THT footprints (Samtec LSHM/ERF8, PMSA003I).
    let mut wildcard_tht_pads: Vec<PadShape> = Vec::new();
    let mut smd_pad_count = 0;
    let mut thru_hole_pad_count = 0;

    let mut silk: Vec<Segment> = Vec::new();
    let mut fab: Vec<Segment> = Vec::new();
    let mut courtyard: Vec<Segment> = Vec::new();

    let mut models: Vec<ModelSpec> = Vec::new();
    let mut embedded_files: Vec<(String, Vec<u8>)> = Vec::new();

    for item in items.iter().skip(2) {
        let Some(list) = item.as_list() else { continue };
        let head = list.first().and_then(Sexpr::as_sym).unwrap_or("");
        match head {
            "pad" => {
                if let Some((pad, drill, pad_name, pad_type, layers)) = parse_pad(list)? {
                    let is_connected_tht =
                        pad_type.as_str() == "thru_hole" && !pad_name.trim().is_empty();
                    match pad_type.as_str() {
                        "smd" => smd_pad_count += 1,
                        "thru_hole" | "np_thru_hole" => thru_hole_pad_count += 1,
                        _ => {}
                    }
                    let has_literal_f_cu = layers.iter().any(|l| l == "F.Cu");
                    let has_wildcard_cu = layers.iter().any(|l| l == "*.Cu" || l == "F&B.Cu");
                    let is_tht = matches!(pad_type.as_str(), "thru_hole" | "np_thru_hole");
                    if has_literal_f_cu {
                        pads.push(pad.clone());
                    } else if has_wildcard_cu && pad_type.as_str() == "thru_hole" {
                        wildcard_tht_pads.push(pad.clone());
                    }
                    if is_tht {
                        holes.push(pad.clone());
                        let drill_shape = drill.clone().unwrap_or_else(|| pad.clone());
                        physical_drills.push(drill_shape.clone());
                        if !is_connected_tht {
                            mechanical_drills.push(drill_shape);
                        }
                    }
                    if is_connected_tht {
                        connected_holes.push(drill.unwrap_or(pad));
                    }
                }
            }
            "fp_line" => {
                if let Some((seg, layer)) = parse_fp_line(list)? {
                    match layer.as_str() {
                        "F.SilkS" => silk.push(seg),
                        "F.Fab" => fab.push(seg),
                        "F.CrtYd" => courtyard.push(seg),
                        _ => {}
                    }
                }
            }
            "model" => {
                models.push(parse_model_block(list)?);
            }
            "embedded_files" => {
                for file in list.iter().skip(1).filter_map(Sexpr::as_list) {
                    if file.first().and_then(Sexpr::as_sym) != Some("file") {
                        continue;
                    }
                    if let Some((name, bytes)) = parse_embedded_file(file)? {
                        embedded_files.push((name, bytes));
                    }
                }
            }
            _ => {}
        }
    }

    let mut model = select_best_model(models);

    // Resolve `kicad-embed://` references against the embedded_files map.
    if let Some(m) = &mut model
        && let Some(stripped) = m.path.strip_prefix("kicad-embed://")
    {
        let target_stem = Path::new(stripped)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        m.embedded_step = embedded_files.iter().find_map(|(n, b)| {
            let stem = Path::new(n)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            (n == stripped || stem == target_stem).then(|| b.clone())
        });
    }

    if pads.is_empty() && !wildcard_tht_pads.is_empty() {
        pads = wildcard_tht_pads;
    }

    if pads.is_empty() {
        bail!("footprint has no F.Cu pads");
    }

    Ok(FootprintData {
        path: path.to_path_buf(),
        name,
        pads,
        holes,
        physical_drills,
        connected_holes,
        mechanical_drills,
        silk: (!silk.is_empty()).then_some(silk),
        fab: (!fab.is_empty()).then_some(fab),
        courtyard: (!courtyard.is_empty()).then_some(courtyard),
        model,
        smd_pad_count,
        thru_hole_pad_count,
    })
}

/// Select the best `(model ...)` block from a list of candidates.
///
/// Priority: `kicad-embed://` paths first, then `.step`/`.stp` extensions.
/// Returns `None` if `models` is empty or all models reference unusable
/// formats (e.g. `.wrl`).
fn select_best_model(models: Vec<ModelSpec>) -> Option<ModelSpec> {
    if let Some(m) = models
        .iter()
        .find(|m| m.path.starts_with("kicad-embed://") && is_step_path(&m.path))
    {
        return Some(m.clone());
    }
    if let Some(m) = models.iter().find(|m| is_step_path(&m.path)) {
        return Some(m.clone());
    }
    None
}

fn is_step_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".step") || lower.ends_with(".stp")
}

type ParsedPad = (PadShape, Option<PadShape>, String, String, Vec<String>);

fn parse_pad(list: &[Sexpr]) -> Result<Option<ParsedPad>> {
    // (pad "<num>" <type> <shape> (at x y [angle]) (size w h) (layers ...))
    let pad_name = list
        .get(1)
        .and_then(|s| s.as_str().or_else(|| s.as_sym()))
        .unwrap_or("")
        .to_string();
    let pad_type = list
        .get(2)
        .and_then(|s| s.as_sym().or_else(|| s.as_str()))
        .unwrap_or("")
        .to_string();
    let shape = list
        .get(3)
        .and_then(|s| s.as_sym().or_else(|| s.as_str()))
        .unwrap_or("")
        .to_ascii_lowercase();
    let Some(at) = find_list(list, "at") else {
        return Ok(None);
    };
    let Some(size) = find_list(list, "size") else {
        return Ok(None);
    };
    let layers = find_list(list, "layers")
        .map(|l| {
            l.iter()
                .skip(1)
                .filter_map(|s| s.as_str().or_else(|| s.as_sym()).map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let (x, y, angle) = read_xy_maybe_angle(at);
    let (w, h) = read_xy(size);
    if w <= f64::EPSILON || h <= f64::EPSILON {
        return Ok(None);
    }
    let kind = match shape.as_str() {
        "rect" => PadKind::Rect,
        "roundrect" => PadKind::RoundRect,
        "trapezoid" => PadKind::Trapezoid,
        "circle" => PadKind::Circle,
        "oval" => PadKind::Oval,
        _ => return Ok(None),
    };
    let pad = PadShape {
        kind,
        at: [x, y],
        size: [w, h],
        angle_deg: angle,
    };
    let drill = parse_drill_shape(list, [x, y], angle);
    Ok(Some((pad, drill, pad_name, pad_type, layers)))
}

fn parse_drill_shape(list: &[Sexpr], at: [f64; 2], angle_deg: f64) -> Option<PadShape> {
    let drill = find_list(list, "drill")?;
    let (kind, size) = match drill.get(1).and_then(|s| s.as_sym().or_else(|| s.as_str())) {
        Some("oval") => {
            let w = drill.get(2).and_then(read_number)?;
            let h = drill.get(3).and_then(read_number)?;
            (PadKind::Oval, [w, h])
        }
        _ => {
            let diameter = drill.get(1).and_then(read_number)?;
            (PadKind::Circle, [diameter, diameter])
        }
    };
    let (ox, oy) = find_list(drill, "offset")
        .map(read_xy)
        .unwrap_or((0.0, 0.0));
    Some(PadShape {
        kind,
        at: [at[0] + ox, at[1] + oy],
        size,
        angle_deg,
    })
}

fn parse_fp_line(list: &[Sexpr]) -> Result<Option<(Segment, String)>> {
    let (Some(start), Some(end), Some(layer)) = (
        find_list(list, "start"),
        find_list(list, "end"),
        find_list(list, "layer"),
    ) else {
        return Ok(None);
    };
    let (ax, ay) = read_xy(start);
    let (bx, by) = read_xy(end);
    let layer_name = layer
        .get(1)
        .and_then(|s| s.as_str().or_else(|| s.as_sym()))
        .unwrap_or("")
        .to_string();
    Ok(Some((
        Segment {
            a: [ax, ay],
            b: [bx, by],
        },
        layer_name,
    )))
}

fn parse_model_block(list: &[Sexpr]) -> Result<ModelSpec> {
    let path = list
        .get(1)
        .and_then(|s| s.as_str().or_else(|| s.as_sym()))
        .unwrap_or("")
        .to_string();
    let filename = path
        .trim_start_matches("kicad-embed://")
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_string();
    let rotate = read_xyz(list, "rotate").unwrap_or([0.0, 0.0, 0.0]);
    let offset = read_xyz(list, "offset").unwrap_or([0.0, 0.0, 0.0]);
    Ok(ModelSpec {
        path,
        filename,
        rotate: EulerPose::new(
            rotate[0].round() as i32,
            rotate[1].round() as i32,
            rotate[2].round() as i32,
        ),
        offset,
        embedded_step: None,
    })
}

fn parse_embedded_file(list: &[Sexpr]) -> Result<Option<(String, Vec<u8>)>> {
    let name = match find_list(list, "name").and_then(|l| {
        l.get(1)
            .and_then(|s| s.as_str().or_else(|| s.as_sym()).map(str::to_string))
    }) {
        Some(n) => n,
        None => return Ok(None),
    };
    let Some(data_list) = find_list(list, "data") else {
        return Ok(None);
    };
    // KiCad encodes embedded files as `(data |<base64>|)`, where the base64
    // payload is wrapped with `|` delimiters and can span many lines. The
    // bar-atom syntax is a KiCad extension that `pcb-sexpr` does not model
    // — its tokenizer breaks on whitespace, so everything between `|...|`
    // arrives as a sequence of Symbol children under the `data` list. We
    // stitch them back together and strip the delimiter / whitespace.
    let mut b64_clean = String::new();
    for item in data_list.iter().skip(1) {
        let atom = match &item.kind {
            SexprKind::Symbol(s) | SexprKind::String(s) => s.as_str(),
            _ => item.raw_atom.as_deref().unwrap_or(""),
        };
        for ch in atom.chars() {
            if !ch.is_whitespace() && ch != '|' {
                b64_clean.push(ch);
            }
        }
    }
    if b64_clean.is_empty() {
        return Ok(None);
    }
    let compressed = base64::engine::general_purpose::STANDARD
        .decode(b64_clean.as_bytes())
        .context("embedded data is not valid base64")?;
    let decompressed = zstd::decode_all(&*compressed).context("zstd decompress")?;
    Ok(Some((name, decompressed)))
}

fn find_list<'a>(list: &'a [Sexpr], name: &str) -> Option<&'a [Sexpr]> {
    list.iter()
        .filter_map(Sexpr::as_list)
        .find(|l| l.first().and_then(Sexpr::as_sym) == Some(name))
}

fn read_xy(list: &[Sexpr]) -> (f64, f64) {
    let x = list.get(1).and_then(read_number).unwrap_or(0.0);
    let y = list.get(2).and_then(read_number).unwrap_or(0.0);
    (x, y)
}

fn read_xy_maybe_angle(list: &[Sexpr]) -> (f64, f64, f64) {
    let x = list.get(1).and_then(read_number).unwrap_or(0.0);
    let y = list.get(2).and_then(read_number).unwrap_or(0.0);
    let a = list.get(3).and_then(read_number).unwrap_or(0.0);
    (x, y, a)
}

fn read_xyz(parent: &[Sexpr], key: &str) -> Option<[f64; 3]> {
    let block = find_list(parent, key)?;
    // Either `(key (xyz x y z))` or `(key x y z)`.
    let inner = find_list(block, "xyz").unwrap_or(block);
    let x = inner.get(1).and_then(read_number)?;
    let y = inner.get(2).and_then(read_number)?;
    let z = inner.get(3).and_then(read_number)?;
    Some([x, y, z])
}

fn read_number(s: &Sexpr) -> Option<f64> {
    match &s.kind {
        SexprKind::Int(n) => Some(*n as f64),
        SexprKind::F64(f) => Some(*f),
        SexprKind::Symbol(s) | SexprKind::String(s) => s.parse().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_footprint(model_blocks: &str) -> String {
        format!(
            r#"(footprint "test"
  (pad "1" smd rect (at 0 0) (size 1 1) (layers "F.Cu"))
  {model_blocks}
)"#
        )
    }

    #[test]
    fn selects_step_over_wrl() {
        let content = make_footprint(
            r#"(model "a.wrl" (offset (xyz 0 0 0)) (rotate (xyz 0 0 0)))
  (model "b.step" (offset (xyz 1 2 3)) (rotate (xyz 90 0 0)))"#,
        );
        let fp = parse_content(&content, Path::new("test.kicad_mod")).unwrap();
        let model = fp.model.expect("should have a model");
        assert_eq!(model.path, "b.step");
    }

    #[test]
    fn wrl_only_yields_none() {
        let content =
            make_footprint(r#"(model "only.wrl" (offset (xyz 0 0 0)) (rotate (xyz 0 0 0)))"#);
        let fp = parse_content(&content, Path::new("test.kicad_mod")).unwrap();
        assert!(
            fp.model.is_none(),
            "wrl-only footprint should have model=None"
        );
    }

    #[test]
    fn prefers_kicad_embed_listed_second() {
        let content = make_footprint(
            r#"(model "a.step" (offset (xyz 0 0 0)) (rotate (xyz 0 0 0)))
  (model "kicad-embed://steps/b.step" (offset (xyz 0 0 0)) (rotate (xyz 90 0 0)))"#,
        );
        let fp = parse_content(&content, Path::new("test.kicad_mod")).unwrap();
        let model = fp.model.expect("should have a model");
        assert!(model.path.starts_with("kicad-embed://"));
    }

    #[test]
    fn ignores_embedded_wrl_when_step_exists() {
        let content = make_footprint(
            r#"(model "kicad-embed://models/a.wrl" (offset (xyz 0 0 0)) (rotate (xyz 0 0 0)))
  (model "b.step" (offset (xyz 1 2 3)) (rotate (xyz 90 0 0)))"#,
        );
        let fp = parse_content(&content, Path::new("test.kicad_mod")).unwrap();
        let model = fp.model.expect("should have a usable STEP model");
        assert_eq!(model.path, "b.step");
    }

    #[test]
    fn no_model_block_yields_none() {
        let content = make_footprint("");
        let fp = parse_content(&content, Path::new("test.kicad_mod")).unwrap();
        assert!(fp.model.is_none());
    }

    #[test]
    fn classifies_smd_tht_and_mixed_footprints() {
        let smd = parse_content(&make_footprint(""), Path::new("test.kicad_mod")).unwrap();
        assert_eq!(smd.footprint_kind(), FootprintKind::SmdOnly);

        let tht = parse_content(
            r#"(footprint "test"
  (pad "1" thru_hole circle (at 0 0) (size 1 1) (drill 0.5) (layers "*.Cu"))
)"#,
            Path::new("test.kicad_mod"),
        )
        .unwrap();
        assert_eq!(tht.footprint_kind(), FootprintKind::ThtOnly);

        let mixed = parse_content(
            r#"(footprint "test"
  (pad "1" smd rect (at 0 0) (size 1 1) (layers "F.Cu"))
  (pad "2" thru_hole circle (at 2 0) (size 1 1) (layers "*.Cu"))
)"#,
            Path::new("test.kicad_mod"),
        )
        .unwrap();
        assert_eq!(mixed.footprint_kind(), FootprintKind::Mixed);
    }

    #[test]
    fn connected_holes_use_drill_geometry_and_ignore_mechanical_holes() {
        let fp = parse_content(
            r#"(footprint "test"
  (pad "" thru_hole circle (at -2 0) (size 3 3) (drill 1.4) (layers "*.Cu"))
  (pad "1" thru_hole circle (at 0 0) (size 2 2) (drill 0.6) (layers "*.Cu"))
  (pad "2" np_thru_hole circle (at 2 0) (size 2 2) (drill 1.2) (layers "*.Cu"))
)"#,
            Path::new("test.kicad_mod"),
        )
        .unwrap();

        assert_eq!(fp.holes.len(), 3);
        assert_eq!(fp.physical_drills.len(), 3);
        assert_eq!(fp.connected_holes.len(), 1);
        assert_eq!(fp.mechanical_drills.len(), 2);
        assert_eq!(fp.thru_hole_pad_count, 3);
        assert_eq!(fp.physical_drills[0].size, [1.4, 1.4]);
        assert_eq!(fp.physical_drills[2].size, [1.2, 1.2]);
        assert_eq!(fp.connected_holes[0].size, [0.6, 0.6]);
        assert_eq!(fp.mechanical_drills[0].size, [1.4, 1.4]);
        assert_eq!(fp.mechanical_drills[1].size, [1.2, 1.2]);
        assert_eq!(fp.footprint_kind(), FootprintKind::ThtOnly);
    }

    #[test]
    fn require_model_error_message_distinguishes_cases() {
        let wrl_content =
            make_footprint(r#"(model "only.wrl" (offset (xyz 0 0 0)) (rotate (xyz 0 0 0)))"#);
        let fp = parse_content(&wrl_content, Path::new("test.kicad_mod")).unwrap();
        let err = fp.require_model().unwrap_err();
        assert!(
            format!("{err}").contains("no usable STEP model"),
            "error should mention no usable STEP model, got: {err}"
        );
    }

    #[test]
    fn single_step_model_selected() {
        let content =
            make_footprint(r#"(model "part.step" (offset (xyz 1 2 3)) (rotate (xyz 0 0 90)))"#);
        let fp = parse_content(&content, Path::new("test.kicad_mod")).unwrap();
        let model = fp.model.expect("should have a model");
        assert_eq!(model.path, "part.step");
        assert_eq!(model.rotate.z, 90);
    }
}
