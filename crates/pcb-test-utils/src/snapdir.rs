//! Snapshot a whole directory with `insta` (simple API).
//! - Respects `.gitignore` and `.ignore` files
//! - Only includes UTF-8 text files (CRLF→LF), ignores binary files  
//! - Deterministic path order
//!
//!   Review changes: `cargo insta review`

use ignore::WalkBuilder;
use insta::Settings;
use std::{fs, io::Read, path::Path};

/// Snapshot directory with defaults.
pub fn assert_dir_snapshot(root: impl AsRef<Path>) {
    assert_dir_snapshot_with(root, &[], None)
}

/// Snapshot directory with insta filters and optional custom name.
/// - `filters`: regex replacements applied before assertion, e.g. [ (r"\b[0-9a-f]{40}\b", "<SHA>") ]
/// - `name`: optional custom name for the snapshot file
pub fn assert_dir_snapshot_with(
    root: impl AsRef<Path>,
    filters: &[(&str, &str)],
    name: Option<&str>,
) {
    let manifest = build_manifest(root.as_ref());
    let mut settings = Settings::clone_current();
    for &(re, rep) in filters {
        settings.add_filter(re, rep);
    }
    if let Some(name) = name {
        settings.set_snapshot_suffix(name);
    }
    settings.bind(|| {
        insta::assert_snapshot!(manifest);
    });
}

fn build_manifest(root: &Path) -> String {
    let base = fs::canonicalize(root).expect("failed to canonicalize root path");

    // Gitignore-aware file walker, but deterministic and confined to `base`
    let mut wb = WalkBuilder::new(&base);
    wb.hidden(true)
        .git_ignore(true) // Respect .gitignore files
        .ignore(true) // Respect .ignore files
        .git_exclude(true) // Keep host-independent
        .git_global(false) // No global git config
        .parents(false); // Don't traverse up directory tree

    let mut entries: Vec<(String, String)> = Vec::new();

    for dent in wb.build().filter_map(Result::ok) {
        let p = dent.path();
        if p == base {
            continue;
        }

        let rel = p
            .strip_prefix(&base)
            .expect("path should be within base")
            .to_string_lossy()
            .replace('\\', "/");

        let Some(ft) = dent.file_type() else { continue };

        if ft.is_dir() {
            // Skip directory entries - only show files
        } else if ft.is_file() {
            let mut buf = Vec::new();
            fs::File::open(p)
                .expect("failed to open file")
                .read_to_end(&mut buf)
                .expect("failed to read file");

            // Only include UTF-8 files, ignore non-UTF-8 files
            if let Ok(s) = std::str::from_utf8(&buf) {
                let mut body = s.replace("\r\n", "\n");
                if !body.ends_with('\n') {
                    body.push('\n');
                }
                entries.push((rel, body));
            }
            // Non-UTF-8 files are ignored
        }
    }

    // Stable order
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    // Manifest
    let mut out = String::new();
    for (rel, body) in entries {
        out.push_str(&format!("=== {rel}\n"));
        out.push_str(&body);
    }
    out
}
