use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use crate::bom::ComponentKey;
use crate::ensure_sqlite_vec_registered;
pub use crate::registry::download::RegistryInfo;

pub mod download;
pub mod embeddings;
pub mod tui;

pub(crate) const RRF_K: f64 = 10.0;
const REGISTRY_SEMANTIC_DISTANCE_THRESHOLD: f64 = 1.3;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DigikeyData {
    pub mpn: Option<String>,
    pub manufacturer: Option<String>,
    pub description: Option<String>,
    pub category: Option<String>,
    #[serde(rename = "productUrl")]
    pub product_url: Option<String>,
    #[serde(rename = "datasheetUrl")]
    pub datasheet_url: Option<String>,
    #[serde(rename = "photoUrl")]
    pub photo_url: Option<String>,
    #[serde(rename = "unitPrice")]
    pub unit_price: Option<f64>,
    #[serde(rename = "quantityAvailable")]
    pub quantity_available: Option<i64>,
    pub status: Option<String>,
    #[serde(rename = "leadWeeks")]
    pub lead_weeks: Option<String>,
    #[serde(default)]
    pub parameters: BTreeMap<String, String>,
    #[serde(default)]
    pub pricing: Vec<DigikeyPriceBreak>,
    pub classifications: Option<DigikeyClassifications>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DigikeyPriceBreak {
    pub qty: i64,
    pub price: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DigikeyClassifications {
    pub rohs: Option<String>,
    pub reach: Option<String>,
    pub msl: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RegistryModuleEntrypoint {
    pub id: i64,
    pub url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RegistryModuleSymbol {
    pub id: i64,
    pub url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RegistryModule {
    pub registry: RegistryInfo,
    pub id: i64,
    pub url: String,
    pub name: String,
    pub version: String,
    pub published_at: Option<String>,
    pub description: String,
    pub entrypoints: Vec<RegistryModuleEntrypoint>,
    pub symbols: Vec<RegistryModuleSymbol>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rank: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RegistrySymbol {
    pub registry: RegistryInfo,
    pub id: i64,
    pub url: String,
    pub name: String,
    pub module_id: i64,
    pub module_url: String,
    pub module_version: String,
    pub module_published_at: Option<String>,
    pub footprint: String,
    pub datasheet: String,
    pub manufacturer: String,
    pub mpn: String,
    pub mpn_normalized: String,
    pub kicad_description: Option<String>,
    pub kicad_keywords: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub digikey: Option<DigikeyData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rank: Option<f64>,
}

impl RegistrySymbol {
    pub fn availability_lookup_key(&self) -> Option<ComponentKey> {
        component_lookup_key(Some(&self.mpn), Some(&self.manufacturer))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RegistryModuleDependency {
    pub id: i64,
    pub url: String,
    pub name: String,
    pub version: String,
    pub published_at: Option<String>,
    pub description: String,
}

impl RegistryModuleDependency {
    pub fn url_with_version(&self) -> String {
        if self.version.is_empty() {
            self.url.clone()
        } else {
            format!("{}@{}", self.url, self.version)
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ModuleRelations {
    pub dependencies: Vec<RegistryModuleDependency>,
    pub dependents: Vec<RegistryModuleDependency>,
}

#[derive(Debug, Clone)]
pub struct RegistryModuleHit {
    pub registry: RegistryInfo,
    pub id: i64,
    pub url: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub rank: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct RegistrySymbolHit {
    pub registry: RegistryInfo,
    pub id: i64,
    pub url: String,
    pub name: String,
    pub module_url: String,
    pub mpn: String,
    pub manufacturer: String,
    pub kicad_description: Option<String>,
    pub rank: Option<f64>,
    pub availability_lookups: Vec<ComponentKey>,
}

#[derive(Debug, Clone, Default)]
pub struct ModuleRrfSearchOutput {
    pub trigram: Vec<RegistryModuleHit>,
    pub word: Vec<RegistryModuleHit>,
    pub docs_full_text: Vec<RegistryModuleHit>,
    pub semantic: Vec<RegistryModuleHit>,
    pub merged: Vec<RegistryModuleHit>,
}

#[derive(Debug, Clone, Default)]
pub struct SymbolRrfSearchOutput {
    pub trigram: Vec<RegistrySymbolHit>,
    pub word: Vec<RegistrySymbolHit>,
    pub docs_full_text: Vec<RegistrySymbolHit>,
    pub semantic: Vec<RegistrySymbolHit>,
    pub merged: Vec<RegistrySymbolHit>,
}

/// Lightweight search hit retained for KiCad symbols search.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub id: i64,
    pub url: String,
    pub name: String,
    pub mpn: Option<String>,
    pub manufacturer: Option<String>,
    pub short_description: Option<String>,
    pub version: Option<String>,
    pub package_category: Option<String>,
    pub rank: Option<f64>,
    pub availability_lookups: Vec<ComponentKey>,
}

#[derive(Debug, Clone, Default)]
pub struct RrfSearchOutput {
    pub trigram: Vec<SearchHit>,
    pub word: Vec<SearchHit>,
    pub docs_full_text: Vec<SearchHit>,
    pub semantic: Vec<SearchHit>,
    pub merged: Vec<SearchHit>,
}

trait UrlKeyedHit: Clone {
    fn url(&self) -> &str;

    fn result_key(&self) -> String {
        self.url().to_string()
    }
}

impl UrlKeyedHit for SearchHit {
    fn url(&self) -> &str {
        &self.url
    }
}

impl UrlKeyedHit for RegistryModuleHit {
    fn url(&self) -> &str {
        &self.url
    }

    fn result_key(&self) -> String {
        format!("{}\0{}", self.registry.id, self.url)
    }
}

impl UrlKeyedHit for RegistrySymbolHit {
    fn url(&self) -> &str {
        &self.url
    }

    fn result_key(&self) -> String {
        format!("{}\0{}", self.registry.id, self.url)
    }
}

fn collect_deduped_by_url<I, T>(rows: I, limit: usize) -> Result<Vec<T>>
where
    I: IntoIterator<Item = rusqlite::Result<T>>,
    T: UrlKeyedHit,
{
    let mut seen_urls = HashSet::new();
    let mut deduped = Vec::with_capacity(limit);
    for row in rows {
        let hit = row?;
        if seen_urls.insert(hit.url().to_owned()) {
            deduped.push(hit);
            if deduped.len() >= limit {
                break;
            }
        }
    }

    Ok(deduped)
}

pub(crate) fn collect_deduped_hits_by_url<I>(rows: I, limit: usize) -> Result<Vec<SearchHit>>
where
    I: IntoIterator<Item = rusqlite::Result<SearchHit>>,
{
    collect_deduped_by_url(rows, limit)
}

fn merge_rrf_by_url<T>(lists: &[&[T]], limit: usize) -> Vec<T>
where
    T: UrlKeyedHit,
{
    let mut rrf_scores: HashMap<String, f64> = HashMap::new();
    for hits in lists {
        for (idx, hit) in hits.iter().enumerate() {
            *rrf_scores.entry(hit.result_key()).or_default() += 1.0 / (RRF_K + (idx + 1) as f64);
        }
    }

    let mut all_hits: HashMap<String, T> = HashMap::new();
    for hits in lists {
        for hit in hits.iter() {
            all_hits
                .entry(hit.result_key())
                .or_insert_with(|| hit.clone());
        }
    }

    let mut scored: Vec<_> = all_hits
        .into_iter()
        .map(|(url, hit)| (rrf_scores.get(&url).copied().unwrap_or(0.0), hit))
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    scored.into_iter().take(limit).map(|(_, hit)| hit).collect()
}

pub(crate) fn merge_rrf_hit_lists(lists: &[&[SearchHit]], limit: usize) -> Vec<SearchHit> {
    merge_rrf_by_url(lists, limit)
}

pub(crate) fn package_name_from_url(url: &str) -> String {
    url.split('/').next_back().unwrap_or(url).to_string()
}

fn symbol_name_from_url(url: &str) -> String {
    url.rsplit_once(':')
        .map(|(_, name)| name)
        .or_else(|| url.split('/').next_back())
        .unwrap_or(url)
        .to_string()
}

pub(crate) fn component_lookup_key(
    mpn: Option<&str>,
    manufacturer: Option<&str>,
) -> Option<ComponentKey> {
    let mpn = mpn?.trim();
    if mpn.is_empty() {
        return None;
    }
    let manufacturer = manufacturer
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    Some(ComponentKey {
        mpn: mpn.to_owned(),
        manufacturer,
    })
}

#[derive(Debug)]
pub struct ParsedQuery {
    pub original: String,
    pub identifier_canon: String,
    pub mpn_canon: String,
}

impl ParsedQuery {
    pub fn parse(query: &str) -> Self {
        let original = query.trim().to_string();
        let identifier_canon = canonicalize_identifier(&original);

        Self {
            original,
            mpn_canon: identifier_canon.clone(),
            identifier_canon,
        }
    }
}

fn canonicalize_identifier(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_uppercase()
}

fn tokenize_for_words(s: &str) -> Vec<String> {
    s.split(|c: char| c.is_whitespace() || c == ',' || c == ';')
        .map(|w| w.trim().to_lowercase())
        .filter(|w| w.len() >= 2)
        .collect()
}

fn push_prefix_fts_tokens(chunk: &str, clauses: &mut Vec<String>) {
    clauses.extend(
        tokenize_for_words(chunk)
            .into_iter()
            .map(|token| format!("{}*", escape_fts5(&token))),
    );
}

fn normalize_phrase_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn build_prefix_fts_query(query: &str) -> Option<String> {
    let mut clauses = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;

    for ch in query.chars() {
        if ch == '"' {
            if in_quotes {
                let phrase = normalize_phrase_whitespace(&current);
                if !phrase.is_empty() {
                    clauses.push(format!("\"{}\"", phrase.replace('"', "\"\"")));
                }
                current.clear();
                in_quotes = false;
            } else {
                push_prefix_fts_tokens(&current, &mut clauses);
                current.clear();
                in_quotes = true;
            }
        } else {
            current.push(ch);
        }
    }

    push_prefix_fts_tokens(&current, &mut clauses);

    (!clauses.is_empty()).then(|| clauses.join(" AND "))
}

pub(crate) fn normalize_semantic_query(query: &str) -> Option<String> {
    let normalized = query.replace('"', " ");
    let collapsed = normalized.split_whitespace().collect::<Vec<_>>().join(" ");
    (!collapsed.is_empty()).then_some(collapsed)
}

pub(crate) fn build_query_embedding(query: &str) -> Option<[f32; 1024]> {
    normalize_semantic_query(query).and_then(|q| embeddings::get_kicad_query_embedding(&q).ok())
}

pub(crate) fn build_registry_query_embedding(query: &str) -> Option<[f32; 512]> {
    normalize_semantic_query(query).and_then(|q| embeddings::get_registry_query_embedding(&q).ok())
}

pub(crate) fn escape_fts5(s: &str) -> String {
    if s.chars().any(|c| {
        matches!(
            c,
            '"' | '*' | '(' | ')' | ':' | '^' | '-' | '.' | '+' | '<' | '>' | '~' | '@'
        )
    }) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

pub struct RegistryClient {
    conn: Connection,
    registry: RegistryInfo,
}

impl RegistryClient {
    pub fn default_db_path() -> Result<PathBuf> {
        download::default_registry_db_path()
    }

    pub fn open_path(path: &std::path::Path) -> Result<Self> {
        Self::open_path_with_registry(path, RegistryInfo::local(path))
    }

    pub fn open_path_with_registry(path: &std::path::Path, registry: RegistryInfo) -> Result<Self> {
        if !path.exists() {
            anyhow::bail!(
                "Registry database not found at {}. Run `pcb registry update` to download it.",
                path.display()
            );
        }

        ensure_sqlite_vec_registered()?;

        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .context("Failed to open registry database")?;

        conn.execute_batch(
            "PRAGMA mmap_size = 268435456;
             PRAGMA cache_size = -65536;
             PRAGMA query_only = ON;",
        )
        .context("Failed to set read-only pragmas")?;

        Ok(Self { conn, registry })
    }

    pub fn registry(&self) -> &RegistryInfo {
        &self.registry
    }

    pub fn count_modules(&self) -> Result<i64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM modules", [], |row| row.get(0))
            .map_err(Into::into)
    }

    pub fn count_symbols(&self) -> Result<i64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))
            .map_err(Into::into)
    }

    pub fn search_modules_rrf(&self, query: &str) -> ModuleRrfSearchOutput {
        const PER_INDEX_LIMIT: usize = 50;
        const MERGED_LIMIT: usize = 100;
        const SEMANTIC_FETCH_LIMIT: usize = 100;

        let query_text = query.trim();
        if query_text.is_empty() {
            return ModuleRrfSearchOutput::default();
        }

        let parsed = ParsedQuery::parse(query_text);
        let trigram = self
            .search_module_trigram_hits(&parsed, PER_INDEX_LIMIT)
            .unwrap_or_default();
        let word = self
            .search_module_word_hits(&parsed, PER_INDEX_LIMIT)
            .unwrap_or_default();
        let docs_full_text = self
            .search_module_docs_full_text_hits(&parsed, PER_INDEX_LIMIT)
            .unwrap_or_default();
        let semantic = build_registry_query_embedding(query_text)
            .and_then(|embedding| {
                self.search_module_semantic_hits(&embedding, SEMANTIC_FETCH_LIMIT)
                    .ok()
            })
            .unwrap_or_default()
            .into_iter()
            .filter(|hit| {
                hit.rank
                    .map(|d| d < REGISTRY_SEMANTIC_DISTANCE_THRESHOLD)
                    .unwrap_or(false)
            })
            .take(PER_INDEX_LIMIT)
            .collect::<Vec<_>>();

        let merged = merge_rrf_by_url(&[&trigram, &word, &docs_full_text, &semantic], MERGED_LIMIT);

        ModuleRrfSearchOutput {
            trigram,
            word,
            docs_full_text,
            semantic,
            merged,
        }
    }

    pub fn search_symbols_rrf(&self, query: &str) -> SymbolRrfSearchOutput {
        const PER_INDEX_LIMIT: usize = 50;
        const MERGED_LIMIT: usize = 100;
        const SEMANTIC_FETCH_LIMIT: usize = 100;

        let query_text = query.trim();
        if query_text.is_empty() {
            return SymbolRrfSearchOutput::default();
        }

        let parsed = ParsedQuery::parse(query_text);
        let trigram = self
            .search_symbol_trigram_hits(&parsed, PER_INDEX_LIMIT)
            .unwrap_or_default();
        let word = self
            .search_symbol_word_hits(&parsed, PER_INDEX_LIMIT)
            .unwrap_or_default();
        let docs_full_text = self
            .search_symbol_docs_full_text_hits(&parsed, PER_INDEX_LIMIT)
            .unwrap_or_default();
        let semantic = build_registry_query_embedding(query_text)
            .and_then(|embedding| {
                self.search_symbol_semantic_hits(&embedding, SEMANTIC_FETCH_LIMIT)
                    .ok()
            })
            .unwrap_or_default()
            .into_iter()
            .filter(|hit| {
                hit.rank
                    .map(|d| d < REGISTRY_SEMANTIC_DISTANCE_THRESHOLD)
                    .unwrap_or(false)
            })
            .take(PER_INDEX_LIMIT)
            .collect::<Vec<_>>();

        let merged = merge_rrf_by_url(&[&trigram, &word, &docs_full_text, &semantic], MERGED_LIMIT);

        SymbolRrfSearchOutput {
            trigram,
            word,
            docs_full_text,
            semantic,
            merged,
        }
    }

    fn search_module_trigram_hits(
        &self,
        parsed: &ParsedQuery,
        limit: usize,
    ) -> Result<Vec<RegistryModuleHit>> {
        if parsed.identifier_canon.is_empty() {
            return Ok(Vec::new());
        }

        let fts_query = escape_fts5(&parsed.identifier_canon);
        let mut stmt = self.conn.prepare(
            r#"
            SELECT m.id, m.url, m.version, m.description, fts.rank
            FROM module_fts_ids fts
            JOIN modules m ON m.id = CAST(fts.module_id AS INTEGER)
            WHERE module_fts_ids MATCH ?1
            ORDER BY fts.rank
            LIMIT ?2
            "#,
        )?;
        let registry = self.registry.clone();
        let rows = stmt.query_map(rusqlite::params![fts_query, limit as i64], |row| {
            map_module_hit(row, &registry)
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    fn search_module_word_hits(
        &self,
        parsed: &ParsedQuery,
        limit: usize,
    ) -> Result<Vec<RegistryModuleHit>> {
        let Some(fts_query) = build_prefix_fts_query(&parsed.original) else {
            return Ok(Vec::new());
        };
        let mut stmt = self.conn.prepare(
            r#"
            SELECT m.id, m.url, m.version, m.description, fts.rank
            FROM module_fts_words fts
            JOIN modules m ON m.id = CAST(fts.module_id AS INTEGER)
            WHERE module_fts_words MATCH ?1
            ORDER BY fts.rank
            LIMIT ?2
            "#,
        )?;
        let registry = self.registry.clone();
        let rows = stmt.query_map(rusqlite::params![fts_query, limit as i64], |row| {
            map_module_hit(row, &registry)
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    fn search_module_docs_full_text_hits(
        &self,
        parsed: &ParsedQuery,
        limit: usize,
    ) -> Result<Vec<RegistryModuleHit>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let Some(fts_query) = build_prefix_fts_query(&parsed.original) else {
            return Ok(Vec::new());
        };
        let fetch_limit = limit.saturating_mul(4);
        let mut stmt = self.conn.prepare(
            r#"
            SELECT m.id, m.url, m.version, m.description, bm25(documents_fts) AS score
            FROM documents_fts
            JOIN documents d ON d.id = documents_fts.rowid
            JOIN document_owners o ON o.document_id = d.id
            JOIN modules m ON m.url = o.owner_url
            WHERE documents_fts MATCH ?1
              AND o.owner_kind = 'module'
            ORDER BY score
            LIMIT ?2
            "#,
        )?;
        let registry = self.registry.clone();
        let rows = stmt.query_map(rusqlite::params![fts_query, fetch_limit as i64], |row| {
            map_module_hit(row, &registry)
        })?;
        collect_deduped_by_url(rows, limit)
    }

    fn search_module_semantic_hits(
        &self,
        embedding: &[f32; 512],
        limit: usize,
    ) -> Result<Vec<RegistryModuleHit>> {
        let embedding_bytes: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();
        let mut stmt = self.conn.prepare(
            r#"
            SELECT m.id, m.url, m.version, m.description, v.distance
            FROM module_vec v
            JOIN modules m ON m.id = v.rowid
            WHERE v.embedding MATCH ?1 AND v.k = ?2
            ORDER BY v.distance
            "#,
        )?;
        let registry = self.registry.clone();
        let rows = stmt.query_map(rusqlite::params![embedding_bytes, limit as i64], |row| {
            map_module_hit(row, &registry)
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    fn search_symbol_trigram_hits(
        &self,
        parsed: &ParsedQuery,
        limit: usize,
    ) -> Result<Vec<RegistrySymbolHit>> {
        if parsed.identifier_canon.is_empty() {
            return Ok(Vec::new());
        }

        let fts_query = escape_fts5(&parsed.identifier_canon);
        let mut stmt = self.conn.prepare(
            r#"
            SELECT s.id, s.url, s.mpn, s.manufacturer, s.kicad_description,
                   m.url AS module_url, fts.rank
            FROM symbol_fts_ids fts
            JOIN symbols s ON s.id = CAST(fts.symbol_id AS INTEGER)
            JOIN modules m ON m.id = s.module_id
            WHERE symbol_fts_ids MATCH ?1
            ORDER BY fts.rank
            LIMIT ?2
            "#,
        )?;
        let registry = self.registry.clone();
        let rows = stmt.query_map(rusqlite::params![fts_query, limit as i64], |row| {
            map_symbol_hit(row, &registry)
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    fn search_symbol_word_hits(
        &self,
        parsed: &ParsedQuery,
        limit: usize,
    ) -> Result<Vec<RegistrySymbolHit>> {
        let Some(fts_query) = build_prefix_fts_query(&parsed.original) else {
            return Ok(Vec::new());
        };
        let mut stmt = self.conn.prepare(
            r#"
            SELECT s.id, s.url, s.mpn, s.manufacturer, s.kicad_description,
                   m.url AS module_url, fts.rank
            FROM symbol_fts_words fts
            JOIN symbols s ON s.id = CAST(fts.symbol_id AS INTEGER)
            JOIN modules m ON m.id = s.module_id
            WHERE symbol_fts_words MATCH ?1
            ORDER BY fts.rank
            LIMIT ?2
            "#,
        )?;
        let registry = self.registry.clone();
        let rows = stmt.query_map(rusqlite::params![fts_query, limit as i64], |row| {
            map_symbol_hit(row, &registry)
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    fn search_symbol_docs_full_text_hits(
        &self,
        parsed: &ParsedQuery,
        limit: usize,
    ) -> Result<Vec<RegistrySymbolHit>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let Some(fts_query) = build_prefix_fts_query(&parsed.original) else {
            return Ok(Vec::new());
        };
        let fetch_limit = limit.saturating_mul(4);
        let mut stmt = self.conn.prepare(
            r#"
            SELECT s.id, s.url, s.mpn, s.manufacturer, s.kicad_description,
                   m.url AS module_url, bm25(documents_fts) AS score
            FROM documents_fts
            JOIN documents d ON d.id = documents_fts.rowid
            JOIN document_owners o ON o.document_id = d.id
            JOIN symbols s ON s.url = o.owner_url
            JOIN modules m ON m.id = s.module_id
            WHERE documents_fts MATCH ?1
              AND o.owner_kind = 'symbol'
            ORDER BY score
            LIMIT ?2
            "#,
        )?;
        let registry = self.registry.clone();
        let rows = stmt.query_map(rusqlite::params![fts_query, fetch_limit as i64], |row| {
            map_symbol_hit(row, &registry)
        })?;
        collect_deduped_by_url(rows, limit)
    }

    fn search_symbol_semantic_hits(
        &self,
        embedding: &[f32; 512],
        limit: usize,
    ) -> Result<Vec<RegistrySymbolHit>> {
        let embedding_bytes: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();
        let mut stmt = self.conn.prepare(
            r#"
            SELECT s.id, s.url, s.mpn, s.manufacturer, s.kicad_description,
                   m.url AS module_url, v.distance
            FROM symbol_vec v
            JOIN symbols s ON s.id = v.rowid
            JOIN modules m ON m.id = s.module_id
            WHERE v.embedding MATCH ?1 AND v.k = ?2
            ORDER BY v.distance
            "#,
        )?;
        let registry = self.registry.clone();
        let rows = stmt.query_map(rusqlite::params![embedding_bytes, limit as i64], |row| {
            map_symbol_hit(row, &registry)
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn get_module_by_id(&self, id: i64) -> Result<Option<RegistryModule>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT id, url, version, published_at, description
            FROM modules
            WHERE id = ?1
            "#,
        )?;
        let module = stmt
            .query_row([id], |row| {
                let url: String = row.get(1)?;
                Ok(RegistryModule {
                    registry: self.registry.clone(),
                    id: row.get(0)?,
                    name: package_name_from_url(&url),
                    url,
                    version: row.get(2)?,
                    published_at: row.get(3)?,
                    description: row.get(4)?,
                    entrypoints: Vec::new(),
                    symbols: Vec::new(),
                    rank: None,
                })
            })
            .optional()?;

        let Some(mut module) = module else {
            return Ok(None);
        };
        module.entrypoints = self.get_module_entrypoints(id)?;
        module.symbols = self.get_module_symbols(id)?;
        Ok(Some(module))
    }

    pub fn get_symbol_by_id(&self, id: i64) -> Result<Option<RegistrySymbol>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT s.id, s.url, s.module_id, m.url AS module_url, m.version AS module_version,
                   m.published_at AS module_published_at, s.footprint, s.datasheet,
                   s.manufacturer, s.mpn, s.mpn_normalized, s.kicad_description,
                   s.kicad_keywords, json(s.digikey), s.image_sha256
            FROM symbols s
            JOIN modules m ON m.id = s.module_id
            WHERE s.id = ?1
            "#,
        )?;

        stmt.query_row([id], |row| {
            let url: String = row.get(1)?;
            let digikey_json: Option<String> = row.get(13)?;
            Ok(RegistrySymbol {
                registry: self.registry.clone(),
                id: row.get(0)?,
                name: symbol_name_from_url(&url),
                url,
                module_id: row.get(2)?,
                module_url: row.get(3)?,
                module_version: row.get(4)?,
                module_published_at: row.get(5)?,
                footprint: row.get(6)?,
                datasheet: row.get(7)?,
                manufacturer: row.get(8)?,
                mpn: row.get(9)?,
                mpn_normalized: row.get(10)?,
                kicad_description: row.get(11)?,
                kicad_keywords: row.get(12)?,
                digikey: digikey_json.and_then(|s| serde_json::from_str(&s).ok()),
                image_sha256: row.get(14)?,
                rank: None,
            })
        })
        .optional()
        .map_err(Into::into)
    }

    pub fn get_module_entrypoints(&self, module_id: i64) -> Result<Vec<RegistryModuleEntrypoint>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT id, url
            FROM module_zen_entrypoints
            WHERE module_id = ?1
            ORDER BY url
            "#,
        )?;
        let rows = stmt.query_map([module_id], |row| {
            Ok(RegistryModuleEntrypoint {
                id: row.get(0)?,
                url: row.get(1)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn get_module_symbols(&self, module_id: i64) -> Result<Vec<RegistryModuleSymbol>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT id, url
            FROM symbols
            WHERE module_id = ?1
            ORDER BY url
            "#,
        )?;
        let rows = stmt.query_map([module_id], |row| {
            Ok(RegistryModuleSymbol {
                id: row.get(0)?,
                url: row.get(1)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn get_module_dependencies(&self, module_id: i64) -> Result<Vec<RegistryModuleDependency>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT dep.id, dep.url, dep.version, dep.published_at, dep.description
            FROM module_deps d
            JOIN modules dep ON dep.id = d.dependency_module_id
            WHERE d.module_id = ?1
            ORDER BY dep.url
            "#,
        )?;
        let rows = stmt.query_map([module_id], map_module_dependency)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn get_module_dependents(&self, module_id: i64) -> Result<Vec<RegistryModuleDependency>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT parent.id, parent.url, parent.version, parent.published_at, parent.description
            FROM module_deps d
            JOIN modules parent ON parent.id = d.module_id
            WHERE d.dependency_module_id = ?1
            ORDER BY parent.url
            "#,
        )?;
        let rows = stmt.query_map([module_id], map_module_dependency)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn get_module_relations(&self, module_id: i64) -> Result<ModuleRelations> {
        Ok(ModuleRelations {
            dependencies: self.get_module_dependencies(module_id)?,
            dependents: self.get_module_dependents(module_id)?,
        })
    }
}

pub struct RegistrySearchClient {
    clients: Vec<RegistryClient>,
}

impl RegistrySearchClient {
    pub fn open_registries(registries: Vec<RegistryInfo>, force: bool) -> Result<Self> {
        let files = download::ensure_registry_indexes(registries, force)?;
        Self::open_index_files(files)
    }

    pub fn open_scope(scope: download::RegistrySearchScope, force: bool) -> Result<Self> {
        match scope {
            download::RegistrySearchScope::Registries(registries) => {
                if !force {
                    let scope = download::RegistrySearchScope::Registries(registries.clone());
                    if scope.local_indexes_exist() {
                        return Self::open_cached(&registries);
                    }
                }
                Self::open_registries(registries, force)
            }
            download::RegistrySearchScope::Index(file) => Self::open_index_files(vec![*file]),
        }
    }

    pub fn open_registries_with_progress(
        registries: Vec<RegistryInfo>,
        progress_tx: &std::sync::mpsc::Sender<download::DownloadProgress>,
        is_update: bool,
        force: bool,
    ) -> Result<Self> {
        let files = download::ensure_registry_indexes_with_progress(
            registries,
            progress_tx,
            is_update,
            force,
        )?;
        Self::open_index_files(files)
    }

    pub fn open_scope_with_progress(
        scope: download::RegistrySearchScope,
        progress_tx: &std::sync::mpsc::Sender<download::DownloadProgress>,
        is_update: bool,
        force: bool,
    ) -> Result<Self> {
        match scope {
            download::RegistrySearchScope::Registries(registries) => {
                if !force {
                    let scope = download::RegistrySearchScope::Registries(registries.clone());
                    if scope.local_indexes_exist() {
                        return Self::open_cached(&registries);
                    }
                }
                Self::open_registries_with_progress(registries, progress_tx, is_update, force)
            }
            download::RegistrySearchScope::Index(file) => Self::open_index_files(vec![*file]),
        }
    }

    pub fn open_cached(registries: &[RegistryInfo]) -> Result<Self> {
        let files = registries
            .iter()
            .map(|registry| {
                Ok(download::RegistryIndexFile {
                    registry: registry.clone(),
                    path: download::registry_db_path(registry)?,
                    downloaded: false,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Self::open_index_files(files)
    }

    pub fn open_cached_scope(scope: &download::RegistrySearchScope) -> Result<Self> {
        match scope {
            download::RegistrySearchScope::Registries(registries) => Self::open_cached(registries),
            download::RegistrySearchScope::Index(file) => {
                Self::open_index_files(vec![(**file).clone()])
            }
        }
    }

    pub fn single(client: RegistryClient) -> Self {
        Self {
            clients: vec![client],
        }
    }

    fn open_index_files(files: Vec<download::RegistryIndexFile>) -> Result<Self> {
        let clients = files
            .into_iter()
            .map(|file| RegistryClient::open_path_with_registry(&file.path, file.registry))
            .collect::<Result<Vec<_>>>()?;
        if clients.is_empty() {
            anyhow::bail!("No registry indexes available");
        }
        Ok(Self { clients })
    }

    pub fn count_modules(&self) -> Result<i64> {
        self.clients
            .iter()
            .map(RegistryClient::count_modules)
            .try_fold(0, |acc, count| count.map(|count| acc + count))
    }

    pub fn count_symbols(&self) -> Result<i64> {
        self.clients
            .iter()
            .map(RegistryClient::count_symbols)
            .try_fold(0, |acc, count| count.map(|count| acc + count))
    }

    pub fn search_modules_rrf(&self, query: &str) -> ModuleRrfSearchOutput {
        const MERGED_LIMIT: usize = 100;
        let outputs = self
            .clients
            .iter()
            .map(|client| client.search_modules_rrf(query))
            .collect::<Vec<_>>();

        let trigram = interleave_ranked(outputs.iter().map(|out| out.trigram.clone()).collect());
        let word = interleave_ranked(outputs.iter().map(|out| out.word.clone()).collect());
        let docs_full_text = interleave_ranked(
            outputs
                .iter()
                .map(|out| out.docs_full_text.clone())
                .collect(),
        );
        let semantic = interleave_ranked(outputs.iter().map(|out| out.semantic.clone()).collect());
        let merged = merge_rrf_by_url(&[&trigram, &word, &docs_full_text, &semantic], MERGED_LIMIT);

        ModuleRrfSearchOutput {
            trigram,
            word,
            docs_full_text,
            semantic,
            merged,
        }
    }

    pub fn search_symbols_rrf(&self, query: &str) -> SymbolRrfSearchOutput {
        const MERGED_LIMIT: usize = 100;
        let outputs = self
            .clients
            .iter()
            .map(|client| client.search_symbols_rrf(query))
            .collect::<Vec<_>>();

        let trigram = interleave_ranked(outputs.iter().map(|out| out.trigram.clone()).collect());
        let word = interleave_ranked(outputs.iter().map(|out| out.word.clone()).collect());
        let docs_full_text = interleave_ranked(
            outputs
                .iter()
                .map(|out| out.docs_full_text.clone())
                .collect(),
        );
        let semantic = interleave_ranked(outputs.iter().map(|out| out.semantic.clone()).collect());
        let merged = merge_rrf_by_url(&[&trigram, &word, &docs_full_text, &semantic], MERGED_LIMIT);

        SymbolRrfSearchOutput {
            trigram,
            word,
            docs_full_text,
            semantic,
            merged,
        }
    }

    pub fn get_module_by_hit(&self, hit: &RegistryModuleHit) -> Result<Option<RegistryModule>> {
        let Some(client) = self.client_for_registry(&hit.registry.id) else {
            return Ok(None);
        };
        client.get_module_by_id(hit.id)
    }

    pub fn get_symbol_by_hit(&self, hit: &RegistrySymbolHit) -> Result<Option<RegistrySymbol>> {
        let Some(client) = self.client_for_registry(&hit.registry.id) else {
            return Ok(None);
        };
        client.get_symbol_by_id(hit.id)
    }

    pub fn get_module_relations_by_hit(&self, hit: &RegistryModuleHit) -> Result<ModuleRelations> {
        let Some(client) = self.client_for_registry(&hit.registry.id) else {
            return Ok(ModuleRelations::default());
        };
        client.get_module_relations(hit.id)
    }

    pub fn get_module_by_key(
        &self,
        registry_id: &str,
        module_id: i64,
    ) -> Result<Option<RegistryModule>> {
        let Some(client) = self.client_for_registry(registry_id) else {
            return Ok(None);
        };
        client.get_module_by_id(module_id)
    }

    pub fn get_symbol_by_key(
        &self,
        registry_id: &str,
        symbol_id: i64,
    ) -> Result<Option<RegistrySymbol>> {
        let Some(client) = self.client_for_registry(registry_id) else {
            return Ok(None);
        };
        client.get_symbol_by_id(symbol_id)
    }

    pub fn get_module_relations_by_key(
        &self,
        registry_id: &str,
        module_id: i64,
    ) -> Result<ModuleRelations> {
        let Some(client) = self.client_for_registry(registry_id) else {
            return Ok(ModuleRelations::default());
        };
        client.get_module_relations(module_id)
    }

    fn client_for_registry(&self, registry_id: &str) -> Option<&RegistryClient> {
        self.clients
            .iter()
            .find(|client| client.registry().id == registry_id)
    }
}

fn interleave_ranked<T: Clone>(lists: Vec<Vec<T>>) -> Vec<T> {
    let max_len = lists.iter().map(Vec::len).max().unwrap_or(0);
    let mut merged = Vec::new();
    for idx in 0..max_len {
        for list in &lists {
            if let Some(hit) = list.get(idx) {
                merged.push(hit.clone());
            }
        }
    }
    merged
}

fn map_module_hit(
    row: &rusqlite::Row,
    registry: &RegistryInfo,
) -> rusqlite::Result<RegistryModuleHit> {
    let url: String = row.get(1)?;
    Ok(RegistryModuleHit {
        registry: registry.clone(),
        id: row.get(0)?,
        name: package_name_from_url(&url),
        url,
        version: row.get(2)?,
        description: row.get(3)?,
        rank: row.get(4)?,
    })
}

fn map_symbol_hit(
    row: &rusqlite::Row,
    registry: &RegistryInfo,
) -> rusqlite::Result<RegistrySymbolHit> {
    let url: String = row.get(1)?;
    let mpn: String = row.get(2)?;
    let manufacturer: String = row.get(3)?;
    Ok(RegistrySymbolHit {
        registry: registry.clone(),
        id: row.get(0)?,
        name: symbol_name_from_url(&url),
        url,
        mpn: mpn.clone(),
        manufacturer: manufacturer.clone(),
        kicad_description: row.get(4)?,
        module_url: row.get(5)?,
        rank: row.get(6)?,
        availability_lookups: component_lookup_key(Some(&mpn), Some(&manufacturer))
            .into_iter()
            .collect(),
    })
}

fn map_module_dependency(row: &rusqlite::Row) -> rusqlite::Result<RegistryModuleDependency> {
    let url: String = row.get(1)?;
    Ok(RegistryModuleDependency {
        id: row.get(0)?,
        name: package_name_from_url(&url),
        url,
        version: row.get(2)?,
        published_at: row.get(3)?,
        description: row.get(4)?,
    })
}
