//! The model cache changes how long an export takes and nothing it writes
//! or reports.

use std::fs;

use pcb_gltf::{ModelCache, export_with_cache};
use pcb_step::{Board, Options, Report};

const HEAD: &str = r#"(kicad_pcb (version 20241229) (generator "pcbnew")
  (general (thickness 1.6) (legacy_teardrops no))
  (layers (0 "F.Cu" signal) (2 "B.Cu" signal) (25 "Edge.Cuts" user))
"#;

fn outline(w: f64, h: f64) -> String {
    format!(
        r#"(gr_line (start 0 0) (end {w} 0) (layer "Edge.Cuts"))
(gr_line (start {w} 0) (end {w} {h}) (layer "Edge.Cuts"))
(gr_line (start {w} {h}) (end 0 {h}) (layer "Edge.Cuts"))
(gr_line (start 0 {h}) (end 0 0) (layer "Edge.Cuts"))
"#
    )
}

/// A STEP file with a solid of the given size: pcb-step's own export of a
/// bare board.
fn step(w: f64, h: f64) -> Vec<u8> {
    let text = format!("{HEAD}{})", outline(w, h));
    let board = Board::parse(text.as_bytes()).unwrap();
    let options = Options {
        components: false,
        silkscreen: false,
        soldermask: false,
        ..Options::default()
    };
    let mut out = Vec::new();
    pcb_step::export(&board, &options, &mut out, &mut Report::default()).unwrap();
    out
}

/// Encode a file the way KiCad embeds it: zstd, then base64 in bar text.
fn embed(name: &str, data: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut text = String::new();
    for chunk in zstd::bulk::compress(data, 3).unwrap().chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..4 {
            text.push(if i <= chunk.len() {
                ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char
            } else {
                '='
            });
        }
    }
    format!(r#"(embedded_files (file (name "{name}") (type model) (data |{text}|)))"#)
}

/// A board with `parts` as (embedded name, scale, x) footprints and the
/// files it embeds.
fn board(parts: &[(&str, f64, f64)], files: &[(&str, Vec<u8>)]) -> String {
    let mut text = format!("{HEAD}{}", outline(60.0, 30.0));
    for (i, (model, scale, x)) in parts.iter().enumerate() {
        text += &format!(
            r#"(footprint "Lib:P" (layer "F.Cu") (at {x} 10 {angle}) (property "Reference" "U{i}")
  (model "kicad-embed://{model}" (offset (xyz 0 0 0)) (scale (xyz {scale} {scale} {scale})) (rotate (xyz 0 0 0))))
"#,
            angle = i * 30
        );
    }
    for (name, data) in files {
        text += &embed(name, data);
    }
    text + ")"
}

fn export(text: &str, cache: Option<&ModelCache>) -> (Vec<u8>, Vec<String>, usize) {
    let board = Board::parse(text.as_bytes()).unwrap();
    let mut out = Vec::new();
    let mut report = Report::default();
    let options = Options {
        name: "t".to_owned(),
        ..Options::default()
    };
    let result = export_with_cache(&board, &options, cache, &mut out, &mut report);
    if let Err(err) = &result {
        out = format!("error: {err}").into_bytes();
    }
    (out, report.warnings, report.failed_models)
}

#[test]
fn cached_exports_match_uncached_and_reuse_across_boards() {
    let (a, b) = (step(4.0, 3.0), step(7.0, 2.0));
    let dir = tempfile::tempdir().unwrap();
    let cache = ModelCache::open(dir.path()).unwrap();
    // Exports `text` cached and checks it against the uncached export.
    let check = |text: &str| {
        assert_eq!(export(text, Some(&cache)), export(text, None));
        let s = cache.stats();
        (s.hits, s.misses)
    };

    let one = board(
        &[
            ("a.step", 1.0, 10.0),
            ("a.step", 1.0, 30.0),
            ("b.step", 1.0, 50.0),
        ],
        &[("a.step", a.clone()), ("b.step", b)],
    );
    assert_eq!(check(&one), (0, 2));
    assert_eq!(check(&one), (2, 2));
    // Another board, or revision: the file under another name and placement.
    let two = board(&[("renamed.stp", 1.0, 5.0)], &[("renamed.stp", a.clone())]);
    assert_eq!(check(&two), (3, 2));
    // Another scale or other content is another entry.
    let scaled = board(&[("a.step", 2.0, 5.0)], &[("a.step", a)]);
    assert_eq!(check(&scaled), (3, 3));
    let changed = board(&[("a.step", 1.0, 5.0)], &[("a.step", step(4.0, 3.5))]);
    assert_eq!(check(&changed), (3, 4));
}

#[test]
fn failures_are_preserved_and_damaged_entries_repaired() {
    let dir = tempfile::tempdir().unwrap();
    let cache = ModelCache::open(dir.path()).unwrap();
    let text = board(
        &[
            ("good.step", 1.0, 10.0),
            ("bad.step", 1.0, 20.0),
            ("gone.step", 1.0, 30.0),
        ],
        &[
            ("good.step", step(4.0, 3.0)),
            ("bad.step", b"not a step file".to_vec()),
        ],
    );
    let uncached = export(&text, None);
    assert!(uncached.2 > 0 || !uncached.1.is_empty(), "{uncached:?}");
    assert_eq!(export(&text, Some(&cache)), uncached);

    // Truncate every entry: recomputed, same output, then hits again.
    for entry in fs::read_dir(dir.path()).unwrap() {
        let path = entry.unwrap().path();
        let data = fs::read(&path).unwrap();
        fs::write(path, &data[..data.len() / 2]).unwrap();
    }
    assert_eq!(export(&text, Some(&cache)), uncached);
    assert_eq!(export(&text, Some(&cache)), uncached);
    assert_eq!(cache.stats().hits, 2);
}
