use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

use crate::bom::ComponentKey;
pub use crate::registry::download::RegistryInfo;

pub mod download;

const RRF_K: f64 = 10.0;
const PER_INDEX_LIMIT: usize = 50;
const MERGED_LIMIT: usize = 100;

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
    pub availability_key: Option<ComponentKey>,
}

/// Identifies a hit across registries.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SearchResultKey {
    registry_id: String,
    url: String,
}

impl SearchResultKey {
    fn new(registry_id: &str, url: &str) -> Self {
        Self {
            registry_id: registry_id.to_owned(),
            url: url.to_owned(),
        }
    }
}

trait RegistryHit: Clone {
    fn key(&self) -> SearchResultKey;
}

impl RegistryHit for RegistryModuleHit {
    fn key(&self) -> SearchResultKey {
        SearchResultKey::new(&self.registry.id, &self.url)
    }
}

impl RegistryHit for RegistrySymbolHit {
    fn key(&self) -> SearchResultKey {
        SearchResultKey::new(&self.registry.id, &self.url)
    }
}

/// Reciprocal-rank fusion of `lists`, best first. Ties keep the order hits were first seen.
fn merge_rrf<'a, T: RegistryHit + 'a>(
    lists: impl IntoIterator<Item = &'a Vec<T>>,
    limit: usize,
) -> Vec<T> {
    let mut slots: HashMap<SearchResultKey, usize> = HashMap::new();
    let mut scored: Vec<(f64, &T)> = Vec::new();
    for hits in lists {
        for (idx, hit) in hits.iter().enumerate() {
            let slot = *slots.entry(hit.key()).or_insert_with(|| {
                scored.push((0.0, hit));
                scored.len() - 1
            });
            scored[slot].0 += 1.0 / (RRF_K + (idx + 1) as f64);
        }
    }

    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    scored
        .into_iter()
        .take(limit)
        .map(|(_, hit)| hit.clone())
        .collect()
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

fn prefix_fts_clauses(query: &str) -> Vec<String> {
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
    clauses
}

/// Prefix FTS query matching every term of `query`, or any term when `any_term` is set.
fn prefix_fts_query(query: &str, any_term: bool) -> Option<String> {
    let clauses = prefix_fts_clauses(query);
    let operator = if any_term { " OR " } else { " AND " };
    (!clauses.is_empty()).then(|| clauses.join(operator))
}

fn escape_fts5(s: &str) -> String {
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
    pub fn open_path(path: &std::path::Path) -> Result<Self> {
        Self::open_path_with_registry(path, RegistryInfo::local(path))
    }

    pub fn open_path_with_registry(path: &std::path::Path, registry: RegistryInfo) -> Result<Self> {
        if !path.exists() {
            anyhow::bail!("Registry index not found at {}", path.display());
        }

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

    /// Runs one of a [`HitQueries`] statement, keeping the first `limit` distinct hits.
    fn hits<T: RegistryHit>(
        &self,
        sql: &str,
        fts_query: &str,
        map: fn(&rusqlite::Row, &RegistryInfo) -> rusqlite::Result<T>,
    ) -> Result<Vec<T>> {
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(
            rusqlite::params![fts_query, PER_INDEX_LIMIT as i64],
            |row| map(row, &self.registry),
        )?;
        let mut seen = HashSet::new();
        let mut hits = Vec::new();
        for hit in rows {
            let hit = hit?;
            if seen.insert(hit.key()) {
                hits.push(hit);
                if hits.len() == PER_INDEX_LIMIT {
                    break;
                }
            }
        }
        Ok(hits)
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
                Self::open_registries(registries, force)
            }
            download::RegistrySearchScope::IndexFiles(files) => Self::open_index_files(files),
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

    pub fn search_modules(&self, query: &str) -> Vec<RegistryModuleHit> {
        self.search(query, &MODULE_QUERIES)
    }

    pub fn search_symbols(&self, query: &str) -> Vec<RegistrySymbolHit> {
        self.search(query, &SYMBOL_QUERIES)
    }

    /// Hits from every registry, fused by reciprocal rank across the id, word and docs
    /// searches.
    fn search<T: RegistryHit>(&self, query: &str, queries: &HitQueries<T>) -> Vec<T> {
        let query = query.trim();
        if query.is_empty() {
            return Vec::new();
        }

        let search = |sql: &str, fts_query: &str| {
            self.clients
                .iter()
                .map(|client| client.hits(sql, fts_query, queries.map).unwrap_or_default())
                .collect::<Vec<_>>()
        };
        let keyword_search = |any_term| match prefix_fts_query(query, any_term) {
            Some(fts_query) => (
                search(queries.words, &fts_query),
                search(queries.docs, &fts_query),
            ),
            None => Default::default(),
        };

        let identifier = canonicalize_identifier(query);
        let trigram_lists = if identifier.is_empty() {
            Vec::new()
        } else {
            search(queries.ids, &escape_fts5(&identifier))
        };
        // A multi-term query that matches nothing in any registry is retried matching any
        // term, so descriptive queries such as "RP2354 controller" still find the parts they
        // name. Deciding across all registries keeps loose matches from mixing with complete
        // ones.
        let (mut word_lists, mut docs_lists) = keyword_search(false);
        if word_lists.iter().chain(&docs_lists).all(Vec::is_empty)
            && prefix_fts_clauses(query).len() > 1
        {
            (word_lists, docs_lists) = keyword_search(true);
        }

        merge_rrf(
            trigram_lists.iter().chain(&word_lists).chain(&docs_lists),
            MERGED_LIMIT,
        )
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

    fn client_for_registry(&self, registry_id: &str) -> Option<&RegistryClient> {
        self.clients
            .iter()
            .find(|client| client.registry().id == registry_id)
    }
}

/// The statements behind each search channel for one kind of hit. Each binds the FTS query and
/// the per-registry limit, and selects the columns `map` reads.
struct HitQueries<T> {
    ids: &'static str,
    words: &'static str,
    docs: &'static str,
    map: fn(&rusqlite::Row, &RegistryInfo) -> rusqlite::Result<T>,
}

const MODULE_QUERIES: HitQueries<RegistryModuleHit> = HitQueries {
    ids: r#"
        SELECT m.id, m.url, m.version, m.description
        FROM module_fts_ids fts
        JOIN modules m ON m.id = CAST(fts.module_id AS INTEGER)
        WHERE module_fts_ids MATCH ?1
        ORDER BY fts.rank
        LIMIT ?2
    "#,
    words: r#"
        SELECT m.id, m.url, m.version, m.description
        FROM module_fts_words fts
        JOIN modules m ON m.id = CAST(fts.module_id AS INTEGER)
        WHERE module_fts_words MATCH ?1
        ORDER BY fts.rank
        LIMIT ?2
    "#,
    // Several documents can belong to one module, so fetch extra rows to dedupe.
    docs: r#"
        SELECT m.id, m.url, m.version, m.description
        FROM documents_fts
        JOIN documents d ON d.id = documents_fts.rowid
        JOIN document_owners o ON o.document_id = d.id
        JOIN modules m ON m.url = o.owner_url
        WHERE documents_fts MATCH ?1
          AND o.owner_kind = 'module'
        ORDER BY bm25(documents_fts)
        LIMIT ?2 * 4
    "#,
    map: map_module_hit,
};

const SYMBOL_QUERIES: HitQueries<RegistrySymbolHit> = HitQueries {
    ids: r#"
        SELECT s.id, s.url, s.mpn, s.manufacturer, s.kicad_description,
               m.url AS module_url
        FROM symbol_fts_ids fts
        JOIN symbols s ON s.id = CAST(fts.symbol_id AS INTEGER)
        JOIN modules m ON m.id = s.module_id
        WHERE symbol_fts_ids MATCH ?1
        ORDER BY fts.rank
        LIMIT ?2
    "#,
    words: r#"
        SELECT s.id, s.url, s.mpn, s.manufacturer, s.kicad_description,
               m.url AS module_url
        FROM symbol_fts_words fts
        JOIN symbols s ON s.id = CAST(fts.symbol_id AS INTEGER)
        JOIN modules m ON m.id = s.module_id
        WHERE symbol_fts_words MATCH ?1
        ORDER BY fts.rank
        LIMIT ?2
    "#,
    // Several documents can belong to one symbol, so fetch extra rows to dedupe.
    docs: r#"
        SELECT s.id, s.url, s.mpn, s.manufacturer, s.kicad_description,
               m.url AS module_url
        FROM documents_fts
        JOIN documents d ON d.id = documents_fts.rowid
        JOIN document_owners o ON o.document_id = d.id
        JOIN symbols s ON s.url = o.owner_url
        JOIN modules m ON m.id = s.module_id
        WHERE documents_fts MATCH ?1
          AND o.owner_kind = 'symbol'
        ORDER BY bm25(documents_fts)
        LIMIT ?2 * 4
    "#,
    map: map_symbol_hit,
};

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
        availability_key: component_lookup_key(Some(&mpn), Some(&manufacturer)),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_fts_query_matches_all_or_any_term() {
        assert_eq!(
            prefix_fts_query("RP2354 controller", false).as_deref(),
            Some("rp2354* AND controller*")
        );
        assert_eq!(
            prefix_fts_query("RP2354 controller", true).as_deref(),
            Some("rp2354* OR controller*")
        );
        assert_eq!(
            prefix_fts_query("\"ideal diode\" controller", true).as_deref(),
            Some("\"ideal diode\" OR controller*")
        );
        assert_eq!(prefix_fts_query("  ", false), None);
    }
}
