//! Fingerprints what the model cache's entries depend on: the locked
//! versions of the crates that tessellate and simplify a model. A change to
//! any of them gives every entry a new key. When the lock file cannot be
//! found the fingerprint is empty and the cache stays off.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use sha2::{Digest, Sha256};

/// The crates whose code decides a model's mesh, with everything they pull in.
const ROOTS: &[&str] = &[
    "triangulate",
    "meshopt-rs",
    "i_triangle",
    "glam",
    "rustc-hash",
];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let fingerprint = manifest
        .ancestors()
        .map(|dir| dir.join("Cargo.lock"))
        .find(|lock| lock.is_file())
        .and_then(|lock| {
            println!("cargo:rerun-if-changed={}", lock.display());
            fingerprint(&std::fs::read_to_string(lock).ok()?)
        })
        .unwrap_or_default();
    println!("cargo:rustc-env=PCB_GLTF_MODEL_CACHE_DEPS={fingerprint}");
}

fn fingerprint(lock: &str) -> Option<String> {
    let lock: toml::Table = lock.parse().ok()?;
    let packages = lock.get("package")?.as_array()?;
    let field = |p: &toml::Value, key: &str| {
        p.get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_owned()
    };
    // Dependencies are "name", "name version" or "name version (source)".
    let mut by_name: BTreeMap<String, Vec<&toml::Value>> = BTreeMap::new();
    for p in packages {
        by_name.entry(field(p, "name")).or_default().push(p);
    }
    let mut seen = BTreeSet::new();
    let mut todo: Vec<String> = ROOTS.iter().map(|s| (*s).to_owned()).collect();
    let mut lines = Vec::new();
    while let Some(spec) = todo.pop() {
        let mut parts = spec.split(' ');
        let (name, want) = (parts.next()?, parts.next());
        for p in by_name.get(name)? {
            let (version, source, sum) = (
                field(p, "version"),
                field(p, "source"),
                field(p, "checksum"),
            );
            if want.is_some_and(|w| w != version)
                || !seen.insert(format!("{name} {version} {source}"))
            {
                continue;
            }
            lines.push(format!("{name} {version} {source} {sum}"));
            let deps = p.get("dependencies").and_then(|d| d.as_array());
            todo.extend(
                deps.into_iter()
                    .flatten()
                    .filter_map(|d| d.as_str().map(str::to_owned)),
            );
        }
    }
    if lines.len() < ROOTS.len() {
        return None;
    }
    lines.sort();
    lines.dedup();
    let digest = Sha256::digest(lines.join("\n"));
    Some(digest.iter().map(|b| format!("{b:02x}")).collect())
}
