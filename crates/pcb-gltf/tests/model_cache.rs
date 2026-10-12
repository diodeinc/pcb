//! The model cache changes how long an export takes and nothing it writes
//! or reports.

use std::fs;
use std::path::Path;

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

fn assert_same(text: &str, cache: &ModelCache) {
    let uncached = export(text, None);
    let cached = export(text, Some(cache));
    assert_eq!(uncached, cached);
}

fn entries(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    for shard in fs::read_dir(dir).unwrap() {
        for entry in fs::read_dir(shard.unwrap().path()).unwrap() {
            found.push(entry.unwrap().path());
        }
    }
    found
}

#[test]
fn warm_runs_match_uncached_runs_and_reuse_across_boards() {
    let (a, b) = (step(4.0, 3.0), step(7.0, 2.0));
    let dir = tempfile::tempdir().unwrap();
    let cache = ModelCache::open(dir.path()).unwrap();

    let one = board(
        &[
            ("a.step", 1.0, 10.0),
            ("a.step", 1.0, 30.0),
            ("b.step", 1.0, 50.0),
        ],
        &[("a.step", a.clone()), ("b.step", b)],
    );
    assert_same(&one, &cache);
    let s = cache.stats();
    assert_eq!((s.hits, s.misses, s.stored), (0, 2, 2));
    // The uncached run in assert_same did not touch it; run warm.
    assert_same(&one, &cache);
    let s = cache.stats();
    assert_eq!((s.hits, s.misses, s.stored, s.corrupt), (2, 2, 2, 0));

    // Another board, or a later revision: the same file under another
    // name, moved, rotated and used by other footprints.
    let two = board(&[("renamed.stp", 1.0, 5.0)], &[("renamed.stp", a.clone())]);
    assert_same(&two, &cache);
    let s = cache.stats();
    assert_eq!((s.hits, s.misses, s.stored), (3, 2, 2));
    // Used at another scale, the tolerance differs, so it is another entry.
    let scaled = board(&[("a.step", 2.0, 5.0)], &[("a.step", a.clone())]);
    assert_same(&scaled, &cache);
    assert_eq!(cache.stats().stored, 3);
    // Changed content is another entry.
    let changed = board(&[("a.step", 1.0, 5.0)], &[("a.step", step(4.0, 3.5))]);
    assert_same(&changed, &cache);
    assert_eq!(cache.stats().stored, 4);
    assert_eq!(entries(dir.path()).len(), 4);
}

#[test]
fn failures_and_warnings_are_preserved() {
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
    for _ in 0..2 {
        assert_eq!(export(&text, Some(&cache)), uncached);
    }
    assert_eq!(cache.stats().hits, 2);
}

#[test]
fn damaged_entries_are_recomputed_and_repaired() {
    let dir = tempfile::tempdir().unwrap();
    let cache = ModelCache::open(dir.path()).unwrap();
    let text = board(&[("a.step", 1.0, 10.0)], &[("a.step", step(4.0, 3.0))]);
    let uncached = export(&text, None);
    assert_eq!(export(&text, Some(&cache)), uncached);
    let path = entries(dir.path()).pop().unwrap();
    let good = fs::read(&path).unwrap();

    for damaged in [
        good[..good.len() / 2].to_vec(),
        Vec::new(),
        [&good[..good.len() - 1], &[good[good.len() - 1] ^ 1]].concat(),
        b"garbage".to_vec(),
    ] {
        fs::write(&path, damaged).unwrap();
        let before = cache.stats();
        assert_eq!(export(&text, Some(&cache)), uncached);
        let after = cache.stats();
        assert_eq!(after.corrupt, before.corrupt + 1);
        assert_eq!(after.stored, before.stored + 1);
        assert_eq!(fs::read(&path).unwrap(), good, "entry is rewritten");
    }
}

#[test]
fn unusable_storage_does_not_change_the_export() {
    let dir = tempfile::tempdir().unwrap();
    let text = board(&[("a.step", 1.0, 10.0)], &[("a.step", step(4.0, 3.0))]);
    let uncached = export(&text, None);

    // The directory vanishes and is replaced by a file after it was opened.
    let gone = dir.path().join("gone");
    let cache = ModelCache::open(&gone).unwrap();
    fs::remove_dir_all(&gone).unwrap();
    fs::write(&gone, b"in the way").unwrap();
    assert_eq!(export(&text, Some(&cache)), uncached);
    assert_eq!(cache.stats().stored, 0);
    assert!(cache.stats().write_failures > 0);

    // A directory that cannot be created at all.
    assert!(ModelCache::open(gone.join("sub")).is_err());
}

#[cfg(unix)]
#[test]
fn a_read_only_cache_still_serves_hits() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let cache = ModelCache::open(dir.path()).unwrap();
    let (a, b) = (step(4.0, 3.0), step(5.0, 3.0));
    let one = board(&[("a.step", 1.0, 10.0)], &[("a.step", a.clone())]);
    let both = board(
        &[("a.step", 1.0, 10.0), ("b.step", 1.0, 30.0)],
        &[("a.step", a), ("b.step", b)],
    );
    let uncached = export(&both, None);
    assert_eq!(export(&one, Some(&cache)), export(&one, None));

    let lock = |mode| {
        for shard in fs::read_dir(dir.path()).unwrap() {
            fs::set_permissions(shard.unwrap().path(), fs::Permissions::from_mode(mode)).unwrap();
        }
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(mode)).unwrap();
    };
    lock(0o555);
    let writable = fs::File::create(dir.path().join("probe")).is_ok();
    let before = cache.stats();
    let out = export(&both, Some(&cache));
    lock(0o755);
    assert_eq!(out, uncached);
    let after = cache.stats();
    assert_eq!(after.hits, before.hits + 1);
    if !writable {
        assert_eq!(after.write_failures, 1);
    }
}

#[test]
fn concurrent_exports_share_one_cache() {
    let dir = tempfile::tempdir().unwrap();
    let cache = ModelCache::open(dir.path()).unwrap();
    let files = [
        ("a.step", step(4.0, 3.0)),
        ("b.step", step(6.0, 2.0)),
        ("c.step", step(3.0, 3.0)),
    ];
    let text = board(
        &[
            ("a.step", 1.0, 10.0),
            ("b.step", 1.0, 30.0),
            ("c.step", 1.0, 50.0),
        ],
        &files,
    );
    let uncached = export(&text, None);
    std::thread::scope(|scope| {
        for _ in 0..6 {
            scope.spawn(|| assert_eq!(export(&text, Some(&cache)), uncached));
        }
    });
    // A separate handle on the same directory, as another process has.
    let other = ModelCache::open(dir.path()).unwrap();
    assert_eq!(export(&text, Some(&other)), uncached);
    assert_eq!(other.stats().hits, 3);
    assert_eq!(cache.stats().corrupt, 0);
    assert_eq!(entries(dir.path()).len(), 3);
}
