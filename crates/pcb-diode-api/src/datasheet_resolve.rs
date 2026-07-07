//! Deterministic datasheet resolution for the `pcb datasheet` command.
//!
//! A query is one of three forms, tried in this order:
//!   1. An **encoded component id** — base64url JSON `{source, mpn, manufacturer?, backendId}`
//!      as returned in the `component_id` field of `pcb search --mode web:components` results.
//!      Resolved via `POST /api/component/download`.
//!   2. A **reference designator** (e.g. `U3`) — valid only inside a workspace. Resolved by
//!      evaluating the board's BOM (done by the caller), preferring the design's own resolved
//!      symbol, then falling back to the MPN tiers.
//!   3. An **MPN** — resolved through deterministic tiers: workspace component packages, the
//!      local registry SQLite index, the KiCad symbol index, then `POST /api/component/search`.
//!
//! This module owns all resolution logic that does *not* require evaluating a board (board
//! evaluation lives in the `pcb` crate, which reuses the same machinery as `pcb bom`).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

pub use pcb_component_gen::sanitize_mpn_for_path;

/// How the query string was interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Interpretation {
    ComponentId,
    Refdes,
    Mpn,
}

/// Which tier produced the resolved datasheet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DatasheetSource {
    /// `POST /api/component/download` signed URL (encoded component id).
    DownloadCache,
    /// A component package in the current workspace (`components/…` or `vendor/…`).
    Workspace,
    /// The local registry SQLite index (`registry:components`).
    RegistryIndex,
    /// The KiCad symbol index (`kicad:components`).
    KicadIndex,
    /// `POST /api/component/search` best-scored result.
    WebSearch,
}

/// A fully resolved datasheet reference (a URL or a local filesystem path).
#[derive(Debug, Clone)]
pub struct ResolvedDatasheet {
    pub interpretation: Interpretation,
    pub mpn: Option<String>,
    pub manufacturer: Option<String>,
    /// A datasheet URL or a local file path.
    pub url: String,
    pub source: DatasheetSource,
}

/// The decoded contents of an encoded component id.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct DecodedComponentId {
    pub source: String,
    pub mpn: String,
    #[serde(default)]
    pub manufacturer: Option<String>,
    /// Opaque backend identifier; only its presence matters for detection.
    #[serde(rename = "backendId")]
    pub backend_id: serde_json::Value,
}

/// Attempt to decode `encoded` as a base64url-encoded component id JSON object.
///
/// Returns `Some` only when the decoded value is a JSON object carrying the mandatory
/// `source`, `mpn`, and `backendId` fields. This makes detection unambiguous: ordinary MPNs and
/// reference designators do not decode into such an object.
pub fn decode_component_id(encoded: &str) -> Option<DecodedComponentId> {
    let trimmed = encoded.trim();
    if trimmed.is_empty() {
        return None;
    }

    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(trimmed)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(trimmed))
        .ok()?;

    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let obj = value.as_object()?;

    // Require the mandatory string fields and a present backendId before treating the input as a
    // component id.
    obj.get("source").and_then(|v| v.as_str())?;
    obj.get("mpn").and_then(|v| v.as_str())?;
    if !obj.contains_key("backendId") {
        return None;
    }

    serde_json::from_value(value).ok()
}

/// Whether `query` has the shape of a reference designator (e.g. `U3`, `R5`, `J12`, `IC10`).
///
/// A refdes-shaped string is only *treated* as a refdes if it also matches an actual BOM
/// designator (checked by the caller after evaluating the board); otherwise it falls through to
/// MPN resolution.
pub fn looks_like_refdes(query: &str) -> bool {
    let bytes = query.as_bytes();
    if bytes.is_empty() {
        return false;
    }

    let letters = query
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .count();
    if !(1..=3).contains(&letters) {
        return false;
    }

    let rest = &query[letters..];
    if rest.is_empty() || !rest.chars().next().unwrap().is_ascii_digit() {
        return false;
    }

    // Digits, optionally followed by a single trailing letter suffix (e.g. `R5A`).
    let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
    let tail = &rest[digits..];
    tail.is_empty() || (tail.len() == 1 && tail.chars().next().unwrap().is_ascii_alphabetic())
}

// ---------------------------------------------------------------------------
// Component id (download) tier
// ---------------------------------------------------------------------------

/// Resolve an encoded component id to its signed datasheet URL via the download API.
///
/// Distinguishes "component not found" (download request failed) from "component found but no
/// datasheet on record" (download succeeded without a datasheet URL).
pub fn resolve_component_id(auth_token: &str, encoded_id: &str) -> Result<ResolvedDatasheet> {
    let decoded = decode_component_id(encoded_id);

    let download = crate::component::download_component(auth_token, encoded_id)
        .context("component not found for the provided component id")?;

    let mpn = Some(download.metadata.mpn.clone())
        .filter(|m| !m.is_empty())
        .or_else(|| decoded.as_ref().map(|d| d.mpn.clone()));
    let manufacturer = download
        .metadata
        .manufacturer
        .clone()
        .or_else(|| decoded.as_ref().and_then(|d| d.manufacturer.clone()));

    match download.datasheet_url {
        Some(url) => Ok(ResolvedDatasheet {
            interpretation: Interpretation::ComponentId,
            mpn,
            manufacturer,
            url,
            source: DatasheetSource::DownloadCache,
        }),
        None => anyhow::bail!(
            "component '{}' found but no datasheet on record",
            mpn.as_deref().unwrap_or(encoded_id)
        ),
    }
}

// ---------------------------------------------------------------------------
// Workspace tier
// ---------------------------------------------------------------------------

/// Resolve a datasheet from a component package's resolved symbol.
///
/// Prefers a local sibling `<MPN>.pdf` (or `<symbol-stem>.pdf`) in the component package dir, then
/// falls back to the `Datasheet` property of the `.kicad_sym`. Returns the datasheet URL or local
/// file path, or `None` when neither is available.
pub fn datasheet_from_symbol(
    symbol_path: &Path,
    mpn: Option<&str>,
    symbol_name: Option<&str>,
) -> Option<String> {
    let dir = symbol_path.parent().unwrap_or_else(|| Path::new("."));

    // Prefer a locally vendored PDF, which pins the exact design intent.
    let mut pdf_candidates: Vec<PathBuf> = Vec::new();
    if let Some(mpn) = mpn {
        pdf_candidates.push(dir.join(format!("{}.pdf", sanitize_mpn_for_path(mpn))));
    }
    if let Some(stem) = symbol_path.file_stem().and_then(|s| s.to_str()) {
        pdf_candidates.push(dir.join(format!("{stem}.pdf")));
    }
    for pdf in pdf_candidates {
        if pdf.is_file() {
            return Some(pdf.to_string_lossy().into_owned());
        }
    }

    // Otherwise, the Datasheet property recorded on the symbol.
    crate::datasheet::extract_datasheet_url_from_kicad_sym(symbol_path, symbol_name).ok()
}

/// Find a component package in the workspace matching `mpn` and return its datasheet, if any.
///
/// Searches `components/…` and `vendor/…` for `<MPN>.kicad_sym`, then applies
/// [`datasheet_from_symbol`]. When `manufacturer` is provided, only packages whose path contains
/// the sanitized manufacturer as a component are considered (the canonical download layout is
/// `components/<manufacturer>/<mpn>/`), disambiguating parts that share an MPN.
pub fn workspace_datasheet_for_mpn(
    workspace_root: &Path,
    mpn: &str,
    manufacturer: Option<&str>,
) -> Option<String> {
    let sanitized = sanitize_mpn_for_path(mpn);
    let target = format!("{sanitized}.kicad_sym");
    let want_mfr = manufacturer.map(sanitize_mpn_for_path);

    for base in ["components", "vendor"] {
        let dir = workspace_root.join(base);
        if !dir.is_dir() {
            continue;
        }
        for entry in WalkDir::new(&dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
        {
            let matches_name = entry
                .path()
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n == target)
                .unwrap_or(false);
            if !matches_name {
                continue;
            }
            if let Some(want) = &want_mfr
                && !entry.path().components().any(|c| {
                    c.as_os_str()
                        .to_str()
                        .is_some_and(|s| s.eq_ignore_ascii_case(want))
                })
            {
                continue;
            }
            if let Some(ds) = datasheet_from_symbol(entry.path(), Some(mpn), None) {
                return Some(ds);
            }
        }
    }

    None
}

// ---------------------------------------------------------------------------
// KiCad symbol index tier
// ---------------------------------------------------------------------------

/// Directories to scan for KiCad symbol libraries.
///
/// Overridable via `PCB_KICAD_SYMBOL_PATH` (a platform path list). Otherwise probes the standard
/// KiCad install locations.
fn kicad_symbol_dirs() -> Vec<PathBuf> {
    if let Ok(paths) = std::env::var("PCB_KICAD_SYMBOL_PATH") {
        return std::env::split_paths(&paths).collect();
    }

    let mut dirs = vec![
        PathBuf::from("/usr/share/kicad/symbols"),
        PathBuf::from("/usr/local/share/kicad/symbols"),
        PathBuf::from("/Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols"),
    ];
    if let Ok(program_files) = std::env::var("ProgramFiles") {
        dirs.push(PathBuf::from(program_files).join("KiCad/share/kicad/symbols"));
    }
    dirs
}

/// Resolve a datasheet from the KiCad symbol index by matching a symbol named `mpn`.
pub fn kicad_index_datasheet(mpn: &str) -> Option<String> {
    for dir in kicad_symbol_dirs() {
        if !dir.is_dir() {
            continue;
        }
        for entry in WalkDir::new(&dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
        {
            let is_symbol = entry
                .path()
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e == "kicad_sym")
                .unwrap_or(false);
            if !is_symbol {
                continue;
            }

            let Ok(lib) = pcb_eda::SymbolLibrary::from_file(entry.path()) else {
                continue;
            };

            let symbol = lib.get_symbol(mpn).or_else(|| {
                lib.symbols()
                    .iter()
                    .find(|s| s.name.eq_ignore_ascii_case(mpn))
            });

            if let Some(symbol) = symbol
                && let Some(ds) = symbol.datasheet.as_deref()
            {
                let ds = ds.trim();
                if crate::datasheet::is_usable_datasheet_value(ds) {
                    return Some(ds.to_string());
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Web search tier
// ---------------------------------------------------------------------------

/// Outcome of the web-search tier, distinguishing "not found" from "found but no datasheet".
enum WebOutcome {
    Found {
        url: String,
        mpn: String,
        manufacturer: Option<String>,
    },
    FoundButNoDatasheet {
        mpn: String,
    },
    NotFound,
}

fn web_search_datasheet(
    auth_token: &str,
    mpn: &str,
    manufacturer: Option<&str>,
) -> Result<WebOutcome> {
    let mut results = crate::component::search_components(auth_token, mpn)?;

    if let Some(want) = manufacturer {
        results.retain(|r| {
            r.manufacturer
                .as_deref()
                .map(|m| m.eq_ignore_ascii_case(want))
                .unwrap_or(false)
        });
    }

    if results.is_empty() {
        return Ok(WebOutcome::NotFound);
    }

    // Pick the best-scored result (highest score; missing scores rank lowest, ties keep the
    // API's original relevance order).
    let best = results
        .into_iter()
        .enumerate()
        .max_by(|(ai, a), (bi, b)| {
            let sa = a.score.unwrap_or(f64::NEG_INFINITY);
            let sb = b.score.unwrap_or(f64::NEG_INFINITY);
            sa.partial_cmp(&sb)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(bi.cmp(ai)) // earlier index wins on ties
        })
        .map(|(_, r)| r)
        .expect("results is non-empty");

    match best.datasheets.first() {
        Some(url) => Ok(WebOutcome::Found {
            url: url.clone(),
            mpn: best.part_number,
            manufacturer: best.manufacturer,
        }),
        None => Ok(WebOutcome::FoundButNoDatasheet {
            mpn: best.part_number,
        }),
    }
}

// ---------------------------------------------------------------------------
// MPN tier chain
// ---------------------------------------------------------------------------

/// Configuration for MPN resolution.
pub struct MpnResolveConfig<'a> {
    /// Workspace root, if the command is running inside a workspace.
    pub workspace_root: Option<&'a Path>,
    /// Optional manufacturer to disambiguate parts sharing an MPN.
    pub manufacturer: Option<&'a str>,
    /// Offline mode: never download the registry index (only use an existing local copy) and
    /// skip the web-search tier.
    pub offline: bool,
}

/// Open the registry index, honoring offline mode (which forbids downloading a missing index).
fn open_registry(offline: bool) -> Option<crate::RegistryClient> {
    if offline {
        let path = crate::RegistryClient::default_db_path().ok()?;
        if !path.exists() {
            return None;
        }
        crate::RegistryClient::open_path(&path).ok()
    } else {
        crate::RegistryClient::open().ok()
    }
}

/// Resolve an MPN to a datasheet through the deterministic tier chain.
pub fn resolve_mpn(mpn: &str, cfg: &MpnResolveConfig) -> Result<ResolvedDatasheet> {
    let make = |url: String, source: DatasheetSource| ResolvedDatasheet {
        interpretation: Interpretation::Mpn,
        mpn: Some(mpn.to_string()),
        manufacturer: cfg.manufacturer.map(str::to_string),
        url,
        source,
    };

    // Tier 1: workspace component packages.
    if let Some(root) = cfg.workspace_root
        && let Some(url) = workspace_datasheet_for_mpn(root, mpn, cfg.manufacturer)
    {
        return Ok(make(url, DatasheetSource::Workspace));
    }

    // Tier 2: local registry SQLite index (registry:components). Query errors (e.g. a stale or
    // corrupt local index) fall through to the next tier rather than aborting resolution.
    if let Some(client) = open_registry(cfg.offline)
        && let Some(url) = client
            .find_component_datasheet(mpn, cfg.manufacturer)
            .ok()
            .flatten()
    {
        return Ok(make(url, DatasheetSource::RegistryIndex));
    }

    // Tier 3: KiCad symbol index (kicad:components).
    if let Some(url) = kicad_index_datasheet(mpn) {
        return Ok(make(url, DatasheetSource::KicadIndex));
    }

    // Tier 4: web search via POST /api/component/search.
    if !cfg.offline {
        let token = crate::auth::get_valid_token()?;
        match web_search_datasheet(&token, mpn, cfg.manufacturer)? {
            WebOutcome::Found {
                url,
                mpn: found_mpn,
                manufacturer,
            } => {
                return Ok(ResolvedDatasheet {
                    interpretation: Interpretation::Mpn,
                    mpn: Some(found_mpn),
                    manufacturer: manufacturer.or_else(|| cfg.manufacturer.map(str::to_string)),
                    url,
                    source: DatasheetSource::WebSearch,
                });
            }
            WebOutcome::FoundButNoDatasheet { mpn: found_mpn } => {
                anyhow::bail!("component '{found_mpn}' found but no datasheet on record");
            }
            WebOutcome::NotFound => {}
        }
    }

    anyhow::bail!(
        "component '{mpn}' not found (searched workspace packages, registry index, and KiCad index{})",
        if cfg.offline { "" } else { ", and web search" }
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(json: &serde_json::Value) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(json).unwrap())
    }

    #[test]
    fn decode_component_id_accepts_valid_object() {
        let id = encode(&serde_json::json!({
            "source": "web",
            "mpn": "LM358",
            "manufacturer": "Texas Instruments",
            "backendId": 42,
        }));
        let decoded = decode_component_id(&id).expect("should decode");
        assert_eq!(decoded.source, "web");
        assert_eq!(decoded.mpn, "LM358");
        assert_eq!(decoded.manufacturer.as_deref(), Some("Texas Instruments"));
        assert_eq!(decoded.backend_id, serde_json::json!(42));
    }

    #[test]
    fn decode_component_id_allows_missing_manufacturer() {
        let id = encode(&serde_json::json!({
            "source": "web",
            "mpn": "LM358",
            "backendId": "abc-123",
        }));
        let decoded = decode_component_id(&id).expect("should decode");
        assert!(decoded.manufacturer.is_none());
        assert_eq!(decoded.backend_id, serde_json::json!("abc-123"));
    }

    #[test]
    fn decode_component_id_rejects_non_component_ids() {
        // Plain MPNs and reference designators.
        assert!(decode_component_id("LM358").is_none());
        assert!(decode_component_id("STM32F407VGT6").is_none());
        assert!(decode_component_id("U3").is_none());
        // Not base64 / empty.
        assert!(decode_component_id("").is_none());
        assert!(decode_component_id("not base64!!!").is_none());
        // Missing backendId.
        let id = encode(&serde_json::json!({"source": "web", "mpn": "LM358"}));
        assert!(decode_component_id(&id).is_none());
        // Missing mpn.
        let id = encode(&serde_json::json!({"source": "web", "backendId": 1}));
        assert!(decode_component_id(&id).is_none());
        // Not an object.
        let id = encode(&serde_json::json!(["source", "mpn", "backendId"]));
        assert!(decode_component_id(&id).is_none());
    }

    #[test]
    fn looks_like_refdes_matches_common_designators() {
        assert!(looks_like_refdes("U3"));
        assert!(looks_like_refdes("R5"));
        assert!(looks_like_refdes("J12"));
        assert!(looks_like_refdes("C100"));
        assert!(looks_like_refdes("IC10"));
        assert!(looks_like_refdes("R5A")); // trailing suffix
    }

    #[test]
    fn looks_like_refdes_rejects_non_designator_shapes() {
        assert!(!looks_like_refdes("STM32F407")); // 4+ leading letters, then letters mixed in
        assert!(!looks_like_refdes("U")); // no digits
        assert!(!looks_like_refdes("3V3")); // starts with digit
        assert!(!looks_like_refdes("")); // empty
        assert!(!looks_like_refdes("MAX232EN")); // multi-letter suffix -> MPN, not a designator
    }

    #[test]
    fn looks_like_refdes_is_permissive_for_short_mpns() {
        // Short MPNs such as `LM358` / `TPS82140` are intentionally refdes-shaped. They are only
        // treated as a refdes when they match a real BOM designator (checked at the board level);
        // otherwise they fall through to MPN resolution.
        assert!(looks_like_refdes("LM358"));
        assert!(looks_like_refdes("TPS82140"));
    }
}
