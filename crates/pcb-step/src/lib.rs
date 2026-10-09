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
mod donor;
mod faces;
mod font;
mod geom;
mod holes;
mod newstroke;
mod outline;
mod outline_font;
mod rings;
pub mod scene;
mod sexpr;
mod step;

use std::fmt;
use std::io::Write;

use crate::donor::Donor;
use crate::geom::{Transform, Vec2};
use crate::scene::{LayerKind, Scene, Shape};
use crate::step::{Part, Root, Writer};

pub use board::Board;

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
}

impl Default for Options {
    fn default() -> Self {
        Self {
            board_body: true,
            components: true,
            cut_vias: false,
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
        }
    }
}

#[derive(Debug, Default)]
pub struct Report {
    pub warnings: Vec<String>,
    /// Models that were found but could not be used.
    pub failed_models: usize,
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

    /// The decoded STEP payload of an embedded model, or `None` when the
    /// board embeds no file of that name.
    pub fn model(&self, key: &str) -> Option<Result<Vec<u8>, Error>> {
        self.embedded_step(key).map(decode_embedded)
    }
}

fn model_key(path: &str) -> &str {
    path.strip_prefix("kicad-embed://").unwrap_or(path)
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

/// Write the STEP assembly for `board` to `sink`.
pub fn export(board: &Board, options: &Options, sink: &mut dyn Write) -> Result<Report, Error> {
    let mut report = Report::default();
    let scene = Scene::build(board, options, &mut report.warnings)?;

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
    export_components(
        board,
        &scene,
        &root,
        &mut w,
        sink,
        &mut report,
        &mut placements,
    )?;

    let threads = worker_threads();
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
        };
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
                    styled.push(w.styled_solid(id, layer.color));
                    items.push(id);
                }
                let representation = w.id();
                w.brep_representation(representation, &items, root.geom_context);
                w.presentation_representation(&styled, root.geom_context);
                representation
            }
            (Shape::Solids(solids), _) => {
                let style = w.style_assignment(layer.color);
                let weights: Vec<usize> = solids.iter().map(|s| solid_edges(&s.solid)).collect();
                let ids = write_batched(&mut w, sink, threads, &weights, |local, i| {
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
                let style = w.style_assignment_with(layer.color, layer.transparency);
                let weights: Vec<usize> = faces
                    .iter()
                    .map(|f| {
                        f.outer.edges.len() + f.holes.iter().map(|h| h.edges.len()).sum::<usize>()
                    })
                    .collect();
                let ids = write_batched(&mut w, sink, threads, &weights, |local, i| {
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
    Ok(report)
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
    threads: usize,
    weights: &[usize],
    emit: impl Fn(&mut Writer, usize) -> u32 + Sync,
) -> Result<Vec<u32>, Error> {
    sink.write_all(&w.buf)?;
    w.buf.clear();
    let mut ids = Vec::with_capacity(weights.len());
    for wave in batches_of(weights, threads * 4).chunks(threads) {
        let written = parallel_map(threads, wave, |range| {
            let mut local = Writer::new(1);
            local
                .buf
                .reserve(weights[range.clone()].iter().sum::<usize>() * 640);
            let ids: Vec<u32> = range.clone().map(|i| emit(&mut local, i)).collect();
            (local.buf, local.next_id - 1, ids)
        });
        for (buf, used, batch) in written {
            let offset = w.next_id - 1;
            w.next_id += used;
            let chunks = line_chunks(&buf, threads);
            let moved = parallel_map(threads, &chunks, |chunk| {
                let mut out = Vec::with_capacity(chunk.len() + chunk.len() / 16);
                step::relocate_ids(chunk, offset, &mut out);
                out
            });
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

/// Loop edges of a solid, a proxy for its STEP text size.
fn solid_edges(solid: &outline::Solid) -> usize {
    solid.outer.edges.len() + solid.holes.iter().map(|h| h.edges.len()).sum::<usize>()
}

enum Loaded {
    Missing,
    Failed(String),
    Empty,
    Ready(Box<Ready>),
}

struct Ready {
    hash: u64,
    donor: Donor,
    analysis: donor::Analysis,
}

/// Decode and analyze one model. Runs on a worker thread.
fn load_model(board: &Board, key: &str) -> Loaded {
    let payload = match board.model(key) {
        Some(Ok(bytes)) => bytes,
        Some(Err(err)) => return Loaded::Failed(err.to_string()),
        None => return Loaded::Missing,
    };
    let hash = content_hash(&payload);
    match Donor::parse(payload).and_then(|donor| donor.analyze().map(|a| (donor, a))) {
        Ok((_, analysis)) if analysis.is_empty() => Loaded::Empty,
        Ok((donor, analysis)) => Loaded::Ready(Box::new(Ready {
            hash,
            donor,
            analysis,
        })),
        Err(err) => Loaded::Failed(err.to_string()),
    }
}

/// Map `items` on up to `threads` worker threads, preserving order.
pub(crate) fn parallel_map<T: Sync, R: Send>(
    threads: usize,
    items: &[T],
    f: impl Fn(&T) -> R + Sync,
) -> Vec<R> {
    if threads <= 1 || items.len() <= 1 {
        return items.iter().map(&f).collect();
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results: std::sync::Mutex<Vec<Option<R>>> =
        std::sync::Mutex::new((0..items.len()).map(|_| None).collect());
    std::thread::scope(|scope| {
        for _ in 0..threads.min(items.len()) {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if i >= items.len() {
                        break;
                    }
                    let r = f(&items[i]);
                    results.lock().unwrap()[i] = Some(r);
                }
            });
        }
    });
    results
        .into_inner()
        .unwrap()
        .into_iter()
        .map(|r| r.unwrap())
        .collect()
}

/// Copy each distinct model into the file and place its occurrences.
fn export_components(
    board: &Board,
    scene: &Scene,
    root: &Root,
    w: &mut Writer,
    sink: &mut dyn Write,
    report: &mut Report,
    placements: &mut Vec<u32>,
) -> Result<(), Error> {
    let mut parts: Vec<Option<Part>> = vec![None; scene.models.len()];
    // Models are copied in batches: each batch is decoded and analyzed on
    // worker threads, given id ranges in order, emitted on worker threads
    // into private buffers, and written out in order. Ids depend only on
    // model order, so the output is the same whatever the thread count.
    let threads = worker_threads();
    let batch = threads * 4;
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
        let loaded = parallel_map(threads, models, |model| load_model(board, &model.key));

        let mut jobs: Vec<Job> = Vec::new();
        for (offset, loaded) in loaded.into_iter().enumerate() {
            let index = first + offset;
            let model = &scene.models[index];
            match loaded {
                Loaded::Missing => report
                    .warnings
                    .push(format!("could not find 3D model: {}", model.key)),
                Loaded::Empty => report
                    .warnings
                    .push(format!("model has no solid geometry: {}", model.key)),
                Loaded::Failed(err) => {
                    report
                        .warnings
                        .push(format!("could not load model {}: {err}", model.key));
                    report.failed_models += 1;
                }
                Loaded::Ready(ready) => {
                    let Ready {
                        hash,
                        donor,
                        analysis,
                    } = *ready;
                    let scale = model.scale;
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
        let emitted = parallel_map(threads, &jobs, |job| -> Result<_, Error> {
            if job.base == 0 {
                return Ok(None);
            }
            let mut local = Writer::new(job.base);
            let key = &scene.models[job.index].key;
            let stem = std::path::Path::new(key)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(key);
            let part = job
                .analysis
                .emit(&job.donor, &mut local, root, stem, job.scale)?;
            debug_assert_eq!(local.next_id, job.base + job.analysis.id_budget());
            Ok(Some((local.buf, part)))
        });

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
    Ok(())
}

/// Cheap content fingerprint for deduplicating identical model payloads.
fn content_hash(data: &[u8]) -> u64 {
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
/// zstd frame.
pub(crate) fn decode_embedded(text: &[u8]) -> Result<Vec<u8>, Error> {
    let compressed = base64_decode(text)?;
    match zstd::zstd_safe::get_frame_content_size(&compressed) {
        Ok(Some(size)) if size > 0 => zstd::bulk::decompress(&compressed, size as usize)
            .map_err(|e| Error::Zstd(e.to_string())),
        _ => {
            zstd::stream::decode_all(compressed.as_slice()).map_err(|e| Error::Zstd(e.to_string()))
        }
    }
}

fn base64_decode(text: &[u8]) -> Result<Vec<u8>, Error> {
    use base64::Engine;
    let compact: Vec<u8> = text
        .iter()
        .copied()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    base64::engine::general_purpose::STANDARD
        .decode(compact)
        .map_err(|_| Error::Base64)
}

#[cfg(test)]
mod tests;

/// Worker threads for the parallel stages: the machine's parallelism,
/// never more than eight.
pub(crate) fn worker_threads() -> usize {
    std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(8)
}
