//! KiCad PCB to STEP assembly export, matching what `kicad-cli pcb export
//! step` produces by default: one board solid with its drills, and every
//! footprint's STEP model placed as an assembly occurrence.
//!
//! The library is in-memory only. [`Board::parse`] borrows the board text
//! and [`export`] streams STEP to any writer. Models come from the board's
//! embedded files.

mod board;
mod copper;
mod donor;
mod faces;
mod font;
mod geom;
mod holes;
mod newstroke;
mod outline;
mod rings;
mod sexpr;
mod step;

use std::fmt;
use std::io::Write;

use crate::donor::Donor;
use crate::geom::{Transform, Vec2};
use crate::outline::{Frame, Loop, board_solids, cut_holes, cut_round};
use crate::step::{Part, Root, Writer};

pub use board::Board;

/// Standoff KiCad leaves between the copper surface and a model.
const MODEL_STANDOFF: f64 = 0.05;

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
}

fn model_key(path: &str) -> &str {
    path.strip_prefix("kicad-embed://").unwrap_or(path)
}

/// Write the STEP assembly for `board` to `sink`.
pub fn export(board: &Board, options: &Options, sink: &mut dyn Write) -> Result<Report, Error> {
    if !options.board_body
        && !options.components
        && !(options.pads || options.tracks || options.zones)
        && !(options.silkscreen || options.soldermask)
    {
        return Err(Error::NothingToExport);
    }
    let mut report = Report::default();
    let origin = match options.origin {
        Origin::Board => Vec2::ZERO,
        Origin::Drill => board.aux_origin,
        Origin::Grid => board.grid_origin,
        Origin::User { x, y } => Vec2::new(x, y),
    };
    let frame = Frame { origin };
    let physical = board.physical();

    let mut w = Writer::new(1);
    let root = Root::reserve(&mut w);
    w.text(
        "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION(('KiCad electronic assembly'),'2;1');\n\
FILE_NAME('",
    );
    w.text(&options.name.replace('\'', "''"));
    w.text(
        ".step','',('pcb-step'),(''),'pcb-step','pcb-step','');\n\
FILE_SCHEMA(('AP242_MANAGED_MODEL_BASED_3D_ENGINEERING_MIM_LF { 1 0 10303 442 1 1 4 }'));\n\
ENDSEC;\nDATA;\n",
    );
    let mut placements: Vec<u32> = Vec::new();

    if options.components {
        export_components(
            board,
            options,
            frame,
            &physical,
            &root,
            &mut w,
            sink,
            &mut report,
            &mut placements,
        )?;
    }

    if options.board_body {
        let solids = board_solids(board, frame)?;
        // Plain through drills first, in one boolean; machined holes after,
        // each falling back to a plain drill where its shape cannot stand
        // clear of everything else.
        let mut drills = Vec::new();
        let mut machined = Vec::new();
        for hole in &board.holes {
            let plain = hole.machining == board::Machining::default();
            if plain || hole.a.distance(hole.b) > 1e-6 {
                if !plain {
                    report.warnings.push(format!(
                        "slot at ({:.3}, {:.3}) mm: machining is only cut on round drills",
                        hole.a.x, hole.a.y
                    ));
                }
                drills.push(Loop::stadium(
                    frame.point(hole.a),
                    frame.point(hole.b),
                    hole.r,
                ));
            } else if let Some(round) =
                holes::pad_hole(frame.point(hole.a), hole.r, hole.machining, &physical)
            {
                machined.push(round);
            }
        }
        if options.cut_vias {
            for via in &board.vias {
                let Some(round) =
                    holes::via_hole(frame.point(via.at), via, &physical, &mut report.warnings)
                else {
                    continue;
                };
                if round.is_plain() {
                    let r = round.profile[0].1;
                    drills.push(Loop::stadium(round.center, round.center, r));
                } else {
                    machined.push(round);
                }
            }
        }
        let mut solids = cut_holes(solids, drills, &mut report.warnings);
        let fallbacks: Vec<Loop> = machined
            .into_iter()
            .filter_map(|round| cut_round(&mut solids, round, &mut report.warnings))
            .collect();
        if !fallbacks.is_empty() {
            solids = cut_holes(solids, fallbacks, &mut report.warnings);
        }

        // KiCad paints the body in the mask colour unless the mask is
        // exported as a layer of its own.
        let color = if options.soldermask {
            board.core_color()
        } else {
            board.body_color()
        };
        let mut items = Vec::with_capacity(1 + solids.len());
        let mut styled = Vec::with_capacity(solids.len());
        let placement = w.axis_placement(&Transform::IDENTITY);
        items.push(placement);
        for (index, solid) in solids.iter().enumerate() {
            let name = if index == 0 {
                "PCB".to_owned()
            } else {
                format!("PCB {}", index + 1)
            };
            let id = w.solid(&name, solid, 0.0, physical.body_top);
            styled.push(w.styled_solid(id, color));
            items.push(id);
        }
        let representation = w.id();
        w.brep_representation(representation, &items, root.geom_context);
        w.presentation_representation(&styled, root.geom_context);
        let part = w.part(
            &root,
            &format!("{}_PCB", options.name),
            representation,
            placement,
        );
        let axis = w.occurrence(
            &root,
            part,
            placements.len() + 1,
            "PCB",
            &Transform::IDENTITY,
        );
        placements.push(axis);
    }

    let copper_options = copper::CopperOptions {
        pads: options.pads,
        tracks: options.tracks,
        zones: options.zones,
        inner: options.inner_copper,
    };
    if copper_options.any() {
        let threads = worker_threads();
        let copper = copper::build(
            board,
            frame,
            &physical,
            copper_options,
            threads,
            &mut report.warnings,
        );
        let copper_rgb = [0.7, 0.61, 0.0].map(board::linear_to_srgb);
        let pad_rgb = if options.components {
            [0.5, 0.5, 0.5].map(board::linear_to_srgb)
        } else {
            copper_rgb
        };
        for (solids, suffix, occurrence, rgb) in [
            (&copper.islands, "copper", "copper", copper_rgb),
            (&copper.pads, "pad", "pads", pad_rgb),
            (&copper.vias, "via", "vias", copper_rgb),
        ] {
            if solids.is_empty() {
                continue;
            }
            let placement = w.axis_placement(&Transform::IDENTITY);
            let style = w.style_assignment(rgb);
            let mut items = Vec::with_capacity(1 + solids.len());
            let mut styled = Vec::with_capacity(solids.len());
            items.push(placement);
            let weights: Vec<usize> = solids.iter().map(|s| solid_edges(&s.solid)).collect();
            let ids = write_batched(&mut w, sink, threads, &weights, |local, i| {
                let s = &solids[i];
                local.solid(&format!("{suffix} {}", i + 1), &s.solid, s.z0, s.z1)
            })?;
            items.extend(ids);
            for &id in &items[1..] {
                styled.push(w.styled_item(id, style));
            }
            let representation = w.id();
            w.brep_representation(representation, &items, root.geom_context);
            w.presentation_representation(&styled, root.geom_context);
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
    }

    if options.silkscreen || options.soldermask {
        let threads = worker_threads();
        let variables: Vec<(String, String)> = board
            .title_block
            .iter()
            .chain(&options.text_variables)
            .cloned()
            .collect();
        let layers = faces::build(
            board,
            frame,
            &physical,
            options.silkscreen,
            options.soldermask,
            &variables,
            threads,
            &mut report.warnings,
        )?;
        for layer in &layers {
            if layer.faces.is_empty() {
                continue;
            }
            let (suffix, occurrence, rgb, transparency) = match layer.tech {
                board::Tech::FrontSilk => {
                    ("silkscreen", "Top Silkscreen", board.silk_color(true), 0.1)
                }
                board::Tech::BackSilk => (
                    "silkscreen",
                    "Bottom Silkscreen",
                    board.silk_color(false),
                    0.1,
                ),
                board::Tech::FrontMask => {
                    ("soldermask", "Top Soldermask", board.mask_color(true), 0.17)
                }
                board::Tech::BackMask => (
                    "soldermask",
                    "Bottom Soldermask",
                    board.mask_color(false),
                    0.17,
                ),
            };
            let placement = w.axis_placement(&Transform::IDENTITY);
            let style = w.style_assignment_with(rgb, Some(transparency));
            let mut items = Vec::with_capacity(1 + layer.faces.len());
            items.push(placement);
            let weights: Vec<usize> = layer
                .faces
                .iter()
                .map(|f| f.outer.edges.len() + f.holes.iter().map(|h| h.edges.len()).sum::<usize>())
                .collect();
            let ids = write_batched(&mut w, sink, threads, &weights, |local, i| {
                let face = &layer.faces[i];
                local.flat_face(&face.outer, &face.holes, layer.z, layer.tech.front())
            })?;
            let styled: Vec<u32> = ids.iter().map(|&id| w.styled_item(id, style)).collect();
            items.extend(ids);
            let representation = w.id();
            w.shape_representation(representation, &items, root.geom_context);
            w.presentation_representation(&styled, root.geom_context);
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

struct Occurrence {
    model: u32,
    reference: String,
    transform: Transform,
}

/// One distinct model file at one scale.
struct ModelUse {
    key: String,
    scale: f64,
    part: Option<Part>,
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
    let payload = if let Some(data) = board.embedded_step(key) {
        match decode_embedded(data) {
            Ok(bytes) => bytes,
            Err(err) => return Loaded::Failed(err.to_string()),
        }
    } else {
        return Loaded::Missing;
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

#[allow(clippy::too_many_arguments)]
fn export_components(
    board: &Board,
    options: &Options,
    frame: Frame,
    physical: &board::Physical,
    root: &Root,
    w: &mut Writer,
    sink: &mut dyn Write,
    report: &mut Report,
    placements: &mut Vec<u32>,
) -> Result<(), Error> {
    let mut uses: Vec<ModelUse> = Vec::new();
    let mut occurrences: Vec<Occurrence> = Vec::new();
    let top = physical.body_top + physical.front_copper;
    let bottom = -physical.back_copper;

    for fp in &board.footprints {
        if (fp.dnp && !options.include_dnp)
            || (fp.unspecified && !options.include_unspecified)
            || !(options.component_filter.is_empty()
                || options
                    .component_filter
                    .iter()
                    .any(|g| glob_match(g, fp.reference)))
        {
            continue;
        }
        for model in &board.models[fp.models.start as usize..fp.models.end as usize] {
            let scale = model.scale;
            if (scale.x - scale.y).abs() > 1e-9
                || (scale.x - scale.z).abs() > 1e-9
                || scale.x <= 0.0
            {
                report.warnings.push(format!(
                    "{}: skipped model with non-uniform scale: {}",
                    fp.reference, model.name
                ));
                continue;
            }
            let path = model.path();
            let key = model_key(&path);
            let index = match uses.iter().position(|u| u.key == key && u.scale == scale.x) {
                Some(i) => i,
                None => {
                    uses.push(ModelUse {
                        key: key.to_owned(),
                        scale: scale.x,
                        part: None,
                    });
                    uses.len() - 1
                }
            };
            let position = frame.point(fp.at);
            let mut offset = model.offset;
            offset.z += MODEL_STANDOFF;
            let mut transform = Transform::translation(position.extend(0.0))
                .then(&Transform::rotation_z(fp.rotation.to_radians()));
            if fp.back {
                offset.z -= bottom;
                transform = transform.then(&Transform::rotation_x(std::f64::consts::PI));
            } else {
                offset.z += top;
            }
            let rotate = model.rotate;
            let transform = transform
                .then(&Transform::translation(offset))
                .then(&Transform::rotation_z(-rotate.z.to_radians()))
                .then(&Transform::rotation_y(-rotate.y.to_radians()))
                .then(&Transform::rotation_x(-rotate.x.to_radians()));
            occurrences.push(Occurrence {
                model: index as u32,
                reference: fp.reference.to_owned(),
                transform,
            });
        }
    }

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
    for first in (0..uses.len()).step_by(batch) {
        let keys: Vec<String> = uses[first..(first + batch).min(uses.len())]
            .iter()
            .map(|u| u.key.clone())
            .collect();
        let loaded = parallel_map(threads, &keys, |key| load_model(board, key));

        let mut jobs: Vec<Job> = Vec::new();
        for (offset, loaded) in loaded.into_iter().enumerate() {
            let index = first + offset;
            match loaded {
                Loaded::Missing => report
                    .warnings
                    .push(format!("could not find 3D model: {}", uses[index].key)),
                Loaded::Empty => report
                    .warnings
                    .push(format!("model has no solid geometry: {}", uses[index].key)),
                Loaded::Failed(err) => {
                    report
                        .warnings
                        .push(format!("could not load model {}: {err}", uses[index].key));
                    report.failed_models += 1;
                }
                Loaded::Ready(ready) => {
                    let Ready {
                        hash,
                        donor,
                        analysis,
                    } = *ready;
                    let scale = uses[index].scale;
                    if let Some((_, _, part)) = parts_by_content
                        .iter()
                        .find(|(h, s, _)| *h == hash && *s == scale)
                    {
                        uses[index].part = Some(*part);
                        continue;
                    }
                    if jobs
                        .iter()
                        .any(|j| j.hash == hash && uses[j.index].scale == scale)
                    {
                        // Same payload twice in one batch: resolved below.
                        jobs.push(Job {
                            index,
                            base: 0,
                            hash,
                            scale,
                            donor,
                            analysis,
                        });
                        continue;
                    }
                    let base = w.next_id;
                    w.next_id += analysis.id_budget();
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
        let context = root.context;
        let emitted = parallel_map(threads, &jobs, |job| -> Result<_, Error> {
            if job.base == 0 {
                return Ok(None);
            }
            let mut local = Writer::new(job.base);
            let shape = job
                .analysis
                .emit(&job.donor, &mut local, &context, job.scale)?;
            Ok(Some((local.buf, shape)))
        });

        sink.write_all(&w.buf)?;
        w.buf.clear();
        for (job, result) in jobs.iter().zip(emitted) {
            let use_ = &mut uses[job.index];
            let Some((buf, shape)) = result? else {
                use_.part = parts_by_content
                    .iter()
                    .find(|(h, s, _)| *h == job.hash && *s == use_.scale)
                    .map(|(_, _, part)| *part);
                continue;
            };
            sink.write_all(&buf)?;
            let stem = std::path::Path::new(&use_.key)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(&use_.key);
            let part = w.part(root, stem, shape.representation, shape.origin);
            parts_by_content.push((job.hash, use_.scale, part));
            use_.part = Some(part);
        }
    }

    for occurrence in &occurrences {
        let Some(part) = uses[occurrence.model as usize].part else {
            continue;
        };
        let axis = w.occurrence(
            root,
            part,
            placements.len() + 1,
            &occurrence.reference,
            &occurrence.transform,
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
fn decode_embedded(text: &[u8]) -> Result<Vec<u8>, Error> {
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
