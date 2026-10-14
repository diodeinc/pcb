//! KiCad PCB to STEP assembly export, matching what `kicad-cli pcb export
//! step` produces by default: one board solid with its drills, and every
//! footprint's STEP model placed as an assembly occurrence.
//!
//! The library is in-memory only. [`Board::parse`] borrows the board text
//! and [`export`] streams STEP to any writer. Models come from the board's
//! embedded files.

mod board;
#[cfg(feature = "cli")]
pub mod cli;
mod copper;
mod coverage;
mod donor;
mod faces;
mod font;
mod geom;
mod holes;
mod image;
pub mod manifest;
mod models;
mod outline;
mod outline_font;
mod rings;
pub mod scene;
mod sexpr;
mod step;

use std::fmt;
use std::io::Write;

use rayon::prelude::*;

use crate::donor::Donor;
use crate::geom::{Transform, Vec2};
use crate::scene::{LayerKind, Scene, Shape};
use crate::step::{Part, Root, Writer};

pub use board::Board;
pub use coverage::{Coverage, ModelCoverage, Status};
pub use image::{
    BoardImage, ImageKind, ImageOptions, LayerImage, LayerRole, NetId, PhysicalLayer, Side,
    board_image,
};
pub use models::{LoadedModel, MAX_EXTERNAL_BYTES, ModelError, ModelFormat, ModelResolver};
pub use outline::OutlinePolicy;
pub use scene::ProxyMode;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Origin {
    Board,
    Drill,
    Grid,
    User { x: f64, y: f64 },
}

#[derive(Debug, Clone)]
pub struct Options {
    pub board_body: bool,
    pub components: bool,
    /// Cut via drills into the body as well as pad drills.
    pub cut_vias: bool,
    /// Draw the board as KiCad's VRML export does: surface vias bored
    /// through body and mask, copper trimmed to the outline, and paste.
    pub visual: bool,
    /// Copper: pad prisms and plating; tracks with via rings and barrels;
    /// zone fills. Outer layers only unless `inner_copper`.
    pub pads: bool,
    pub tracks: bool,
    pub zones: bool,
    pub inner_copper: bool,
    /// Silkscreen and solder mask as flat faces above the copper.
    pub silkscreen: bool,
    pub soldermask: bool,
    pub include_dnp: bool,
    pub include_unspecified: bool,
    /// Reference designator globs; empty means every footprint.
    pub component_filter: Vec<String>,
    pub origin: Origin,
    /// Product name of the assembly, normally the board file stem.
    pub name: String,
    /// Project text variables, substituted into `${NAME}` in silkscreen
    /// text as KiCad substitutes them.
    pub text_variables: Vec<(String, String)>,
    /// Where models that are not embedded may be read from.
    pub models: ModelResolver,
    pub outline: OutlinePolicy,
    pub proxies: ProxyMode,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            board_body: true,
            components: true,
            cut_vias: false,
            visual: false,
            pads: false,
            tracks: false,
            zones: false,
            inner_copper: false,
            silkscreen: true,
            soldermask: true,
            include_dnp: true,
            include_unspecified: true,
            component_filter: Vec::new(),
            origin: Origin::Board,
            name: "board".to_owned(),
            text_variables: Vec::new(),
            models: ModelResolver::default(),
            outline: OutlinePolicy::Strict,
            proxies: ProxyMode::Off,
        }
    }
}

#[derive(Debug, Default)]
pub struct Report {
    pub warnings: Vec<String>,
    /// Models that were found but could not be used.
    pub failed_models: usize,
    pub coverage: Coverage,
}

impl Report {
    pub fn model_error(&mut self, key: &str, err: &ModelError) {
        self.warnings.push(match err {
            ModelError::Absent(_) => format!("could not find 3D model: {key}"),
            ModelError::Unsupported(_) => format!("skipped 3D model {key}: {err}"),
            ModelError::Empty => format!("model has no solid geometry: {key}"),
            ModelError::Failed(_) => {
                self.failed_models += 1;
                format!("could not load model {key}: {err}")
            }
        });
    }
}

#[derive(Debug)]
pub enum Error {
    Utf8,
    Syntax(&'static str),
    Number(&'static str),
    Outline(&'static str),
    OpenOutline(Vec2),
    Step(&'static str),
    Io(std::io::Error),
    Base64,
    Zstd(String),
    NothingToExport,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Utf8 => write!(f, "input is not valid UTF-8"),
            Error::Syntax(what) => write!(f, "malformed board file: {what}"),
            Error::Number(what) => write!(f, "malformed number in {what}"),
            Error::Outline(what) => write!(f, "board outline: {what}"),
            Error::OpenOutline(p) => write!(
                f,
                "board outline has an unclosed path near ({:.3}, {:.3}) mm",
                p.x, -p.y
            ),
            Error::Step(what) => write!(f, "malformed STEP model: {what}"),
            Error::Io(err) => write!(f, "{err}"),
            Error::Base64 => write!(f, "embedded file is not valid base64"),
            Error::Zstd(err) => write!(f, "embedded file failed to decompress: {err}"),
            Error::NothingToExport => write!(f, "nothing selected for export"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Error::Io(err)
    }
}

impl Board<'_> {
    /// The embedded STEP payload for a model reference. An exact name wins;
    /// otherwise case is ignored, as it is for files on disk.
    fn embedded_step(&self, key: &str) -> Option<&[u8]> {
        let mut fallback = None;
        for file in &self.embedded {
            if file.name == key {
                return Some(file.data);
            }
            if file.name.eq_ignore_ascii_case(key) {
                fallback.get_or_insert(file.data);
            }
        }
        fallback
    }

    pub fn load_model(
        &self,
        resolver: &ModelResolver,
        reference: &str,
    ) -> Result<LoadedModel, ModelError> {
        let embedded = reference.strip_prefix("kicad-embed://");
        let name = embedded.unwrap_or(reference);
        let Some(data) = self.embedded_step(name) else {
            return match embedded {
                Some(_) => Err(ModelError::Absent("not embedded in the board".to_owned())),
                None => resolver.load(reference),
            };
        };
        let format = ModelFormat::of_path(name).ok_or_else(models::unsupported_format)?;
        let bytes = decode_embedded(data).map_err(|e| ModelError::Failed(e.to_string()))?;
        Ok(LoadedModel { bytes, format })
    }
}

/// The names every font embedded in a board answers to, lowercased: the
/// family, full and typographic family names of each face. A text's
/// `(face ...)` is drawn from an embedded font when its lowercased name
/// is among them.
pub fn embedded_font_names(source: &[u8]) -> Result<Vec<String>, Error> {
    let board = Board::parse_with(source, false, false)?;
    let mut warnings = Vec::new();
    Ok(outline_font::Fonts::load(&board, &mut warnings).names())
}

/// Write the STEP assembly for `board` to `sink`, adding warnings and
/// models that could not be read to `report` as they are found.
pub fn export(
    board: &Board,
    options: &Options,
    sink: &mut dyn Write,
    report: &mut Report,
) -> Result<(), Error> {
    let mut scene = Scene::build(board, options, &mut report.warnings)?;

    let mut w = Writer::new(1);
    let root = Root::reserve(&mut w);
    w.text(
        "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION(('KiCad electronic assembly'),'2;1');\n\
FILE_NAME('",
    );
    w.text(&options.name.replace('\'', "''"));
    w.text(
        ".step','',('pcb-step'),(''),'pcb-step','pcb-step','');\n\
FILE_SCHEMA(('AUTOMOTIVE_DESIGN { 1 0 10303 214 1 1 1 1 }'));\n\
ENDSEC;\nDATA;\n",
    );
    let mut placements: Vec<u32> = Vec::new();
    let outcomes = export_components(
        board,
        options,
        &scene,
        &root,
        &mut w,
        sink,
        report,
        &mut placements,
    )?;
    scene.resolve(board, options, &outcomes);
    report.coverage = scene.coverage.clone();

    for layer in &scene.layers {
        let (suffix, occurrence) = match layer.kind {
            LayerKind::Body => ("PCB", "PCB"),
            LayerKind::Copper => ("copper", "copper"),
            LayerKind::Pads => ("pad", "pads"),
            LayerKind::Vias => ("via", "vias"),
            LayerKind::Silkscreen { front: true } => ("silkscreen", "Top Silkscreen"),
            LayerKind::Silkscreen { front: false } => ("silkscreen", "Bottom Silkscreen"),
            LayerKind::Soldermask { front: true } => ("soldermask", "Top Soldermask"),
            LayerKind::Soldermask { front: false } => ("soldermask", "Bottom Soldermask"),
            LayerKind::Paste { front: true } => ("paste", "Top Paste"),
            LayerKind::Paste { front: false } => ("paste", "Bottom Paste"),
        };
        // OCCT encodes KiCad's colours from linear to sRGB.
        let srgb = layer.color.map(board::linear_to_srgb);
        let placement = w.axis_placement(&Transform::IDENTITY);
        let mut items = vec![placement];
        let representation = match (&layer.shape, layer.kind) {
            (Shape::Solids(solids), LayerKind::Body) => {
                let mut styled = Vec::with_capacity(solids.len());
                for (index, prism) in solids.iter().enumerate() {
                    let name = if index == 0 {
                        "PCB".to_owned()
                    } else {
                        format!("PCB {}", index + 1)
                    };
                    let id = w.solid(&name, &prism.solid, prism.z0, prism.z1);
                    styled.push(w.styled_solid(id, srgb));
                    items.push(id);
                }
                let representation = w.id();
                w.brep_representation(representation, &items, root.geom_context);
                w.presentation_representation(&styled, root.geom_context);
                representation
            }
            (Shape::Solids(solids), _) => {
                let style = w.style_assignment(srgb);
                let weights: Vec<usize> = solids
                    .iter()
                    .map(|s| loop_edges(&s.solid.outer, &s.solid.holes))
                    .collect();
                let ids = write_batched(&mut w, sink, &weights, |local, i| {
                    let s = &solids[i];
                    local.solid(&format!("{suffix} {}", i + 1), &s.solid, s.z0, s.z1)
                })?;
                items.extend(ids);
                let styled: Vec<u32> = items[1..]
                    .iter()
                    .map(|&id| w.styled_item(id, style))
                    .collect();
                let representation = w.id();
                w.brep_representation(representation, &items, root.geom_context);
                w.presentation_representation(&styled, root.geom_context);
                representation
            }
            (Shape::Faces { z, up, faces }, _) => {
                let style = w.style_assignment_with(srgb, layer.transparency);
                let weights: Vec<usize> = faces
                    .iter()
                    .map(|f| loop_edges(&f.outer, &f.holes))
                    .collect();
                let ids = write_batched(&mut w, sink, &weights, |local, i| {
                    let face = &faces[i];
                    local.flat_face(&face.outer, &face.holes, *z, *up)
                })?;
                let styled: Vec<u32> = ids.iter().map(|&id| w.styled_item(id, style)).collect();
                items.extend(ids);
                let representation = w.id();
                w.shape_representation(representation, &items, root.geom_context);
                w.presentation_representation(&styled, root.geom_context);
                representation
            }
        };
        let part = w.part(
            &root,
            &format!("{}_{suffix}", options.name),
            representation,
            placement,
        );
        let axis = w.occurrence(
            &root,
            part,
            placements.len() + 1,
            occurrence,
            &Transform::IDENTITY,
        );
        placements.push(axis);
    }

    if placements.is_empty() {
        return Err(Error::NothingToExport);
    }
    root.emit(&mut w, &options.name, &placements);
    w.text("ENDSEC;\nEND-ISO-10303-21;\n");
    sink.write_all(&w.buf)?;
    Ok(())
}

/// Write items in parallel batches with ids from 1, a wave of batches at
/// a time, each batch then moved up to its place in the file chunk by
/// chunk, so memory stays at one wave of text rather than the whole
/// product. `weights` are the items' edge counts, a proxy for their text
/// size; `emit` writes one item and returns its id. Returns the ids as
/// they stand in the file.
fn write_batched<W: Write + ?Sized>(
    w: &mut Writer,
    sink: &mut W,
    weights: &[usize],
    emit: impl Fn(&mut Writer, usize) -> u32 + Sync,
) -> Result<Vec<u32>, Error> {
    sink.write_all(&w.buf)?;
    w.buf.clear();
    let mut ids = Vec::with_capacity(weights.len());
    let threads = rayon::current_num_threads();
    for wave in batches_of(weights, threads * 4).chunks(threads) {
        let written: Vec<_> = wave
            .par_iter()
            .map(|range| {
                let mut local = Writer::new(1);
                local
                    .buf
                    .reserve(weights[range.clone()].iter().sum::<usize>() * 640);
                let ids: Vec<u32> = range.clone().map(|i| emit(&mut local, i)).collect();
                (local.buf, local.next_id - 1, ids)
            })
            .collect();
        for (buf, used, batch) in written {
            let offset = w.next_id - 1;
            w.next_id += used;
            let chunks = line_chunks(&buf, threads);
            let moved: Vec<Vec<u8>> = chunks
                .par_iter()
                .map(|chunk| {
                    let mut out = Vec::with_capacity(chunk.len() + chunk.len() / 16);
                    step::relocate_ids(chunk, offset, &mut out);
                    out
                })
                .collect();
            for chunk in &moved {
                sink.write_all(chunk)?;
            }
            ids.extend(batch.iter().map(|id| id + offset));
        }
    }
    Ok(ids)
}

/// Split items into at most `count` contiguous batches of similar
/// weight, so emission threads finish together.
fn batches_of(weights: &[usize], count: usize) -> Vec<std::ops::Range<usize>> {
    let total: usize = weights.iter().sum();
    let target = total.div_ceil(count.max(1)).max(1);
    let mut batches = Vec::new();
    let mut start = 0;
    let mut sum = 0;
    for (i, weight) in weights.iter().enumerate() {
        sum += weight;
        if sum >= target {
            batches.push(start..i + 1);
            start = i + 1;
            sum = 0;
        }
    }
    if start < weights.len() {
        batches.push(start..weights.len());
    }
    batches
}

/// `text` in up to `count` runs of whole lines, for relocating in parallel.
fn line_chunks(text: &[u8], count: usize) -> Vec<&[u8]> {
    let mut chunks = Vec::with_capacity(count);
    let target = text.len().div_ceil(count.max(1)).max(1);
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + target).min(text.len());
        if end < text.len() {
            end += memchr::memchr(b'\n', &text[end..]).map_or(text.len() - end, |i| i + 1);
        }
        chunks.push(&text[start..end]);
        start = end;
    }
    chunks
}

/// Edges of an outer loop and its holes, a proxy for their STEP text size.
fn loop_edges(outer: &outline::Loop, holes: &[outline::Loop]) -> usize {
    outer.edges.len() + holes.iter().map(|h| h.edges.len()).sum::<usize>()
}

struct Ready {
    hash: u64,
    donor: Donor,
    analysis: donor::Analysis,
}

/// Decode and analyze one model. Runs on a worker thread.
fn load_model(
    board: &Board,
    resolver: &ModelResolver,
    model: &scene::Model,
) -> Result<Box<Ready>, ModelError> {
    if model.uniform_scale().is_none() {
        return Err(ModelError::Unsupported(format!(
            "non-uniform scale {} cannot be placed in a STEP assembly",
            model.scale
        )));
    }
    let loaded = board.load_model(resolver, &model.key)?;
    if loaded.format != ModelFormat::Step {
        return Err(ModelError::Unsupported(
            "VRML models cannot be written to STEP".to_owned(),
        ));
    }
    let hash = content_hash(&loaded.bytes);
    let donor = Donor::parse(loaded.bytes).map_err(|e| ModelError::Failed(e.to_string()))?;
    let analysis = donor
        .analyze()
        .map_err(|e| ModelError::Failed(e.to_string()))?;
    if analysis.is_empty() {
        return Err(ModelError::Empty);
    }
    Ok(Box::new(Ready {
        hash,
        donor,
        analysis,
    }))
}

/// Copy each distinct model into the file and place its occurrences.
#[allow(clippy::too_many_arguments)]
fn export_components(
    board: &Board,
    options: &Options,
    scene: &Scene,
    root: &Root,
    w: &mut Writer,
    sink: &mut dyn Write,
    report: &mut Report,
    placements: &mut Vec<u32>,
) -> Result<Vec<Result<(), ModelError>>, Error> {
    let mut parts: Vec<Option<Part>> = vec![None; scene.models.len()];
    let mut outcomes = Vec::with_capacity(scene.models.len());
    // Models are copied in batches: each batch is decoded and analyzed on
    // worker threads, given id ranges in order, emitted on worker threads
    // into private buffers, and written out in order. Ids depend only on
    // model order, so the output is the same whatever the thread count.
    let batch = rayon::current_num_threads() * 4;
    // Distinct payloads by content, so one file embedded under two names
    // is still copied once.
    let mut parts_by_content: Vec<(u64, f64, Part)> = Vec::new();
    struct Job {
        index: usize,
        base: u32,
        hash: u64,
        scale: f64,
        donor: Donor,
        analysis: donor::Analysis,
    }
    for first in (0..scene.models.len()).step_by(batch) {
        let models = &scene.models[first..(first + batch).min(scene.models.len())];
        let loaded: Vec<_> = models
            .par_iter()
            .with_max_len(1)
            .map(|model| load_model(board, &options.models, model))
            .collect();

        let mut jobs: Vec<Job> = Vec::new();
        for (offset, loaded) in loaded.into_iter().enumerate() {
            let index = first + offset;
            let model = &scene.models[index];
            outcomes.push(loaded.as_ref().map(|_| ()).map_err(Clone::clone));
            match loaded {
                Err(err) => report.model_error(&model.key, &err),
                Ok(ready) => {
                    let Ready {
                        hash,
                        donor,
                        analysis,
                    } = *ready;
                    let scale = model.uniform_scale().unwrap_or(1.0);
                    if let Some((_, _, part)) = parts_by_content
                        .iter()
                        .find(|(h, s, _)| *h == hash && *s == scale)
                    {
                        parts[index] = Some(*part);
                        continue;
                    }
                    // The same payload twice in one batch is resolved below.
                    let base = if jobs.iter().any(|j| j.hash == hash && j.scale == scale) {
                        0
                    } else {
                        let base = w.next_id;
                        w.next_id += analysis.id_budget();
                        base
                    };
                    jobs.push(Job {
                        index,
                        base,
                        hash,
                        scale,
                        donor,
                        analysis,
                    });
                }
            }
        }
        let emitted: Vec<_> = jobs
            .par_iter()
            .with_max_len(1)
            .map(|job| -> Result<_, Error> {
                if job.base == 0 {
                    return Ok(None);
                }
                let mut local = Writer::new(job.base);
                let name = scene.models[job.index].name();
                let part = job
                    .analysis
                    .emit(&job.donor, &mut local, root, name, job.scale)?;
                debug_assert_eq!(local.next_id, job.base + job.analysis.id_budget());
                Ok(Some((local.buf, part)))
            })
            .collect();

        sink.write_all(&w.buf)?;
        w.buf.clear();
        for (job, result) in jobs.iter().zip(emitted) {
            let Some((buf, part)) = result? else {
                parts[job.index] = parts_by_content
                    .iter()
                    .find(|(h, s, _)| *h == job.hash && *s == job.scale)
                    .map(|(_, _, part)| *part);
                continue;
            };
            sink.write_all(&buf)?;
            parts_by_content.push((job.hash, job.scale, part));
            parts[job.index] = Some(part);
        }
    }

    for component in &scene.components {
        let Some(part) = parts[component.model] else {
            continue;
        };
        let axis = w.occurrence(
            root,
            part,
            placements.len() + 1,
            &component.reference,
            &Transform(component.transform),
        );
        placements.push(axis);
    }
    Ok(outcomes)
}

/// Cheap content fingerprint for deduplicating identical model payloads.
pub fn content_hash(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325 ^ data.len() as u64;
    let (chunks, rest) = data.as_chunks::<8>();
    for chunk in chunks {
        h = (h ^ u64::from_le_bytes(*chunk))
            .wrapping_mul(0x9e3779b97f4a7c15)
            .rotate_left(29);
    }
    for b in rest {
        h = (h ^ *b as u64).wrapping_mul(0x100000001b3);
    }
    h
}

fn glob_match(pattern: &str, text: &str) -> bool {
    let p = pattern.as_bytes();
    let t = text.as_bytes();
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == b'?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

/// Decode KiCad's embedded payload: base64 text (whitespace allowed) of a
/// zstd stream. Like the C decoder, this reads every frame, skips skippable
/// frames, and checks each frame's content size and checksum when present.
pub(crate) fn decode_embedded(text: &[u8]) -> Result<Vec<u8>, Error> {
    use ruzstd::decoding::StreamingDecoder;
    use ruzstd::decoding::errors::{FrameDecoderError, ReadFrameHeaderError};
    use std::io::Read;
    let compressed = base64_decode(text)?;
    let zstd_error = |err: &dyn std::fmt::Display| Error::Zstd(err.to_string());
    let mut input = compressed.as_slice();
    let mut out = Vec::new();
    while !input.is_empty() {
        let header = input;
        let mut decoder = match StreamingDecoder::new(&mut input) {
            Ok(decoder) => decoder,
            Err(FrameDecoderError::ReadFrameHeaderError(ReadFrameHeaderError::SkipFrame {
                length,
                ..
            })) => {
                input = input
                    .get(length as usize..)
                    .ok_or_else(|| Error::Zstd("truncated skippable frame".to_owned()))?;
                continue;
            }
            Err(err) => return Err(zstd_error(&err)),
        };
        // content_size() is zero both for an absent and an empty size; the
        // descriptor's size flag or single-segment bit says one is present.
        let expected = (header[4] & 0xe0 != 0).then(|| decoder.decoder.content_size());
        let start = out.len();
        // The size is untrusted, so one that cannot be reserved is ignored.
        let _ = out.try_reserve_exact(expected.unwrap_or(0) as usize);
        decoder.read_to_end(&mut out).map_err(|e| zstd_error(&e))?;
        if expected.is_some_and(|size| size != (out.len() - start) as u64) {
            return Err(Error::Zstd("content size mismatch".to_owned()));
        }
        let frame = &decoder.decoder;
        if frame
            .get_checksum_from_data()
            .is_some_and(|sum| frame.get_calculated_checksum() != Some(sum))
        {
            return Err(Error::Zstd("checksum mismatch".to_owned()));
        }
    }
    Ok(out)
}

fn base64_decode(text: &[u8]) -> Result<Vec<u8>, Error> {
    use base64::Engine;
    // Copy the runs between line breaks whole, not byte by byte.
    let mut compact = Vec::with_capacity(text.len());
    for run in text.split(u8::is_ascii_whitespace) {
        compact.extend_from_slice(run);
    }
    base64::engine::general_purpose::STANDARD
        .decode(compact)
        .map_err(|_| Error::Base64)
}

#[cfg(test)]
mod tests;
