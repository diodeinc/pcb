use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::Serialize;
use std::path::{Path, PathBuf};

use crate::SearchHit;
use crate::bom::ComponentKey;
use crate::registry::{
    ParsedQuery, RrfSearchOutput, build_prefix_fts_query, build_query_embedding,
    collect_deduped_hits_by_url, merge_rrf_hit_lists,
};

pub mod download;

const SEMANTIC_DISTANCE_THRESHOLD: f64 = 1.3;
const SEMANTIC_FETCH_LIMIT: usize = 100;
const PER_INDEX_LIMIT: usize = 50;
const MERGED_LIMIT: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnsureIndexResult {
    AlreadyPresent,
    Downloaded,
}

#[derive(Debug, Clone, Serialize)]
pub struct KicadSymbol {
    pub id: i64,
    pub symbol_library: String,
    pub symbol_name: String,
    pub footprint_library: String,
    pub footprint_name: String,
    pub manufacturer: String,
    pub datasheet_url: Option<String>,
    pub datasheet_sha256: Option<String>,
    pub datasheet_source: Option<String>,
    pub kicad_description: Option<String>,
    pub kicad_keywords: Option<String>,
    pub kicad_fp_filters: Option<String>,
    pub phase3_description: String,
    pub phase3_keywords: String,
    #[serde(skip_serializing)]
    pub image_data: Option<Vec<u8>>,
    pub matched_mpns: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rank: Option<f64>,
}

impl KicadSymbol {
    pub fn path(&self) -> String {
        format!("{}.kicad_sym:{}", self.symbol_library, self.symbol_name)
    }

    pub fn clipboard_url(&self) -> String {
        format!("@kicad-symbols/{}", self.path())
    }

    pub fn primary_mpn(&self) -> Option<&str> {
        self.matched_mpns.first().map(String::as_str)
    }

    pub fn availability_lookup_keys(&self) -> Vec<ComponentKey> {
        self.matched_mpns
            .iter()
            .map(|mpn| ComponentKey {
                mpn: mpn.clone(),
                manufacturer: Some(self.manufacturer.clone()),
            })
            .collect()
    }

    pub fn description(&self) -> Option<&str> {
        non_empty_string(self.phase3_description.as_str())
            .or_else(|| self.kicad_description.as_deref().and_then(non_empty_string))
    }
}

pub struct KicadSymbolsClient {
    conn: Connection,
}

impl KicadSymbolsClient {
    /// Get the default KiCad symbols database path (~/.pcb/kicad-symbols/symbols.db)
    pub fn default_db_path() -> Result<PathBuf> {
        let home = dirs::home_dir().context("Could not determine home directory")?;
        Ok(home.join(".pcb").join("kicad-symbols").join("symbols.db"))
    }

    /// Get the default KiCad symbols version sidecar path (~/.pcb/kicad-symbols/symbols.db.version)
    pub fn default_version_path() -> Result<PathBuf> {
        Ok(Self::default_db_path()?.with_extension("db.version"))
    }

    /// Returns true when the default KiCad symbols cache exists locally.
    pub fn is_cached() -> Result<bool> {
        Ok(Self::default_db_path()?.exists())
    }

    /// Returns the locally cached KiCad symbols version token, if present.
    pub fn local_version() -> Result<Option<String>> {
        let path = Self::default_db_path()?;
        Ok(download::load_local_version(&path))
    }

    /// Ensure the default KiCad symbols index exists locally.
    ///
    /// A prefetched metadata object can be provided to avoid a duplicate API request.
    pub fn ensure_cached(
        prefetched_metadata: Option<&download::KicadSymbolsIndexMetadata>,
    ) -> Result<EnsureIndexResult> {
        let path = Self::default_db_path()?;
        if path.exists() {
            return Ok(EnsureIndexResult::AlreadyPresent);
        }

        if let Some(metadata) = prefetched_metadata {
            let (progress_tx, progress_rx) = std::sync::mpsc::channel();
            let _ = progress_rx;
            download::download_kicad_symbols_index_with_progress(
                &path,
                &progress_tx,
                false,
                Some(metadata),
            )?;
        } else {
            download::download_kicad_symbols_index(&path)?;
        }

        Ok(EnsureIndexResult::Downloaded)
    }

    /// Refresh the default KiCad symbols index when the server-side version changes.
    pub fn refresh_if_stale() -> Result<download::RefreshResult> {
        let path = Self::default_db_path()?;
        download::refresh_kicad_symbols_index_if_stale(&path)
    }

    /// Open the KiCad symbols database from the default location.
    /// Downloads the index from the API server if not present locally.
    pub fn open() -> Result<Self> {
        let path = Self::default_db_path()?;
        Self::ensure_cached(None)?;
        Self::open_path(&path)
    }

    /// Open the KiCad symbols database from a specific path.
    pub fn open_path(path: &Path) -> Result<Self> {
        if !path.exists() {
            anyhow::bail!("KiCad symbols database not found at {}.", path.display());
        }

        unsafe {
            rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
                *const (),
                unsafe extern "C" fn(
                    *mut rusqlite::ffi::sqlite3,
                    *mut *mut i8,
                    *const rusqlite::ffi::sqlite3_api_routines,
                ) -> i32,
            >(
                sqlite_vec::sqlite3_vec_init as *const (),
            )));
        }

        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .context("Failed to open KiCad symbols database")?;

        conn.execute_batch(
            "PRAGMA mmap_size = 268435456;
             PRAGMA cache_size = -65536;
             PRAGMA query_only = ON;",
        )
        .context("Failed to set read-only pragmas")?;

        Ok(Self { conn })
    }

    pub fn count_symbols(&self) -> Result<i64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))
            .map_err(Into::into)
    }

    pub fn search_rrf(&self, query: &str) -> RrfSearchOutput {
        let query_text = query.trim();
        if query_text.is_empty() {
            return RrfSearchOutput::default();
        }

        let parsed = ParsedQuery::parse(query_text);
        let trigram = self
            .search_trigram_hits(&parsed, PER_INDEX_LIMIT)
            .unwrap_or_default();
        let word = self
            .search_word_hits(&parsed, PER_INDEX_LIMIT)
            .unwrap_or_default();
        let docs_full_text = self
            .search_docs_full_text_hits(&parsed, PER_INDEX_LIMIT)
            .unwrap_or_default();
        let semantic = build_query_embedding(query_text)
            .and_then(|embedding| {
                self.search_semantic_hits(&embedding, SEMANTIC_FETCH_LIMIT)
                    .ok()
            })
            .unwrap_or_default()
            .into_iter()
            .filter(|hit| {
                hit.rank
                    .map(|distance| distance < SEMANTIC_DISTANCE_THRESHOLD)
                    .unwrap_or(false)
            })
            .take(PER_INDEX_LIMIT)
            .collect::<Vec<_>>();

        let merged =
            merge_rrf_hit_lists(&[&trigram, &word, &docs_full_text, &semantic], MERGED_LIMIT);

        RrfSearchOutput {
            trigram,
            word,
            docs_full_text,
            semantic,
            merged,
        }
    }

    pub fn get_symbol_by_id(&self, id: i64) -> Result<Option<KicadSymbol>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT id, symbol_library, symbol_name, footprint_library, footprint_name, manufacturer,
                   datasheet_url, datasheet_sha256, datasheet_source, kicad_description,
                   kicad_keywords, kicad_fp_filters, phase3_description, phase3_keywords, image
            FROM symbols
            WHERE id = ?1
            "#,
        )?;

        let symbol = stmt
            .query_row([id], |row| {
                Ok(KicadSymbol {
                    id: row.get(0)?,
                    symbol_library: row.get(1)?,
                    symbol_name: row.get(2)?,
                    footprint_library: row.get(3)?,
                    footprint_name: row.get(4)?,
                    manufacturer: row.get(5)?,
                    datasheet_url: row.get(6)?,
                    datasheet_sha256: row.get(7)?,
                    datasheet_source: row.get(8)?,
                    kicad_description: row.get(9)?,
                    kicad_keywords: row.get(10)?,
                    kicad_fp_filters: row.get(11)?,
                    phase3_description: row.get(12)?,
                    phase3_keywords: row.get(13)?,
                    image_data: row.get(14)?,
                    matched_mpns: Vec::new(),
                    rank: None,
                })
            })
            .optional()?;

        let Some(mut symbol) = symbol else {
            return Ok(None);
        };

        symbol.matched_mpns = self.get_matched_mpns(id)?;
        Ok(Some(symbol))
    }

    pub fn get_matched_mpns(&self, symbol_id: i64) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT mpn
            FROM symbol_mpns
            WHERE symbol_id = ?1
            ORDER BY mpn
            "#,
        )?;
        let rows = stmt.query_map([symbol_id], |row| row.get(0))?;
        rows.collect::<std::result::Result<Vec<String>, _>>()
            .map_err(Into::into)
    }

    pub fn populate_availability_lookups(&self, hits: &mut [SearchHit]) -> Result<()> {
        for hit in hits {
            hit.availability_lookups = self
                .get_matched_mpns(hit.id)?
                .into_iter()
                .map(|mpn| ComponentKey {
                    mpn,
                    manufacturer: hit.manufacturer.clone(),
                })
                .collect();
        }

        Ok(())
    }

    fn search_trigram_hits(&self, parsed: &ParsedQuery, limit: usize) -> Result<Vec<SearchHit>> {
        if parsed.mpn_canon.is_empty() {
            return Ok(Vec::new());
        }

        let fts_query = escape_fts5(&parsed.mpn_canon);
        let mut stmt = self.conn.prepare(
            r#"
            SELECT s.id,
                   '@kicad-symbols/' || s.symbol_library || '.kicad_sym:' || s.symbol_name,
                   s.symbol_name,
                   s.manufacturer,
                   COALESCE(
                     (SELECT mpn FROM symbol_mpns sm WHERE sm.symbol_id = s.id ORDER BY mpn LIMIT 1),
                     s.symbol_name
                   ),
                   COALESCE(NULLIF(s.phase3_description, ''), s.kicad_description),
                   s.symbol_library,
                   fts.rank
            FROM symbol_fts_ids fts
            JOIN symbols s ON s.id = CAST(fts.symbol_id AS INTEGER)
            WHERE symbol_fts_ids MATCH ?1
            ORDER BY rank
            LIMIT ?2
            "#,
        )?;

        let rows = stmt.query_map([&fts_query, &limit.to_string()], map_search_hit)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    fn search_word_hits(&self, parsed: &ParsedQuery, limit: usize) -> Result<Vec<SearchHit>> {
        let Some(fts_query) = build_prefix_fts_query(&parsed.original) else {
            return Ok(Vec::new());
        };

        let mut stmt = self.conn.prepare(
            r#"
            SELECT s.id,
                   '@kicad-symbols/' || s.symbol_library || '.kicad_sym:' || s.symbol_name,
                   s.symbol_name,
                   s.manufacturer,
                   COALESCE(
                     (SELECT mpn FROM symbol_mpns sm WHERE sm.symbol_id = s.id ORDER BY mpn LIMIT 1),
                     s.symbol_name
                   ),
                   COALESCE(NULLIF(s.phase3_description, ''), s.kicad_description),
                   s.symbol_library,
                   fts.rank
            FROM symbol_fts_words fts
            JOIN symbols s ON s.id = CAST(fts.symbol_id AS INTEGER)
            WHERE symbol_fts_words MATCH ?1
            ORDER BY rank
            LIMIT ?2
            "#,
        )?;

        let rows = stmt.query_map([&fts_query, &limit.to_string()], map_search_hit)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    fn search_docs_full_text_hits(
        &self,
        parsed: &ParsedQuery,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let Some(fts_query) = build_prefix_fts_query(&parsed.original) else {
            return Ok(Vec::new());
        };
        let fetch_limit = limit.saturating_mul(4);

        let mut stmt = self.conn.prepare(
            r#"
            SELECT s.id,
                   '@kicad-symbols/' || s.symbol_library || '.kicad_sym:' || s.symbol_name,
                   s.symbol_name,
                   s.manufacturer,
                   COALESCE(
                     (SELECT mpn FROM symbol_mpns sm WHERE sm.symbol_id = s.id ORDER BY mpn LIMIT 1),
                     s.symbol_name
                   ),
                   COALESCE(NULLIF(s.phase3_description, ''), s.kicad_description),
                   s.symbol_library,
                   bm25(symbol_ocr_docs_fts) AS score
            FROM symbol_ocr_docs_fts
            JOIN symbol_ocr_docs d ON d.id = symbol_ocr_docs_fts.rowid
            JOIN symbols s ON s.id = d.symbol_id
            WHERE symbol_ocr_docs_fts MATCH ?1
            ORDER BY score
            LIMIT ?2
            "#,
        )?;

        let rows = stmt.query_map([&fts_query, &fetch_limit.to_string()], map_search_hit)?;

        collect_deduped_hits_by_url(rows, limit)
    }

    fn search_semantic_hits(
        &self,
        embedding: &[f32; 1024],
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        let embedding_bytes: Vec<u8> = embedding
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect();
        let mut stmt = self.conn.prepare(
            r#"
            SELECT s.id,
                   '@kicad-symbols/' || s.symbol_library || '.kicad_sym:' || s.symbol_name,
                   s.symbol_name,
                   s.manufacturer,
                   COALESCE(
                     (SELECT mpn FROM symbol_mpns sm WHERE sm.symbol_id = s.id ORDER BY mpn LIMIT 1),
                     s.symbol_name
                   ),
                   COALESCE(NULLIF(s.phase3_description, ''), s.kicad_description),
                   s.symbol_library,
                   v.distance
            FROM symbol_vec v
            JOIN symbols s ON s.id = v.rowid
            WHERE v.embedding MATCH ?1 AND v.k = ?2
            ORDER BY v.distance
            "#,
        )?;

        let rows = stmt.query_map(
            rusqlite::params![embedding_bytes, limit as i64],
            map_search_hit,
        )?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
}

fn non_empty_string(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn escape_fts5(token: &str) -> String {
    if token.chars().any(|c| {
        matches!(
            c,
            '"' | '*' | '(' | ')' | ':' | '^' | '-' | '.' | '+' | '<' | '>' | '~' | '@'
        )
    }) {
        format!("\"{}\"", token.replace('"', "\"\""))
    } else {
        token.to_string()
    }
}

fn map_search_hit(row: &rusqlite::Row) -> rusqlite::Result<SearchHit> {
    Ok(SearchHit {
        id: row.get(0)?,
        url: row.get(1)?,
        name: row.get(2)?,
        mpn: row.get(4)?,
        manufacturer: row.get(3)?,
        short_description: row.get(5)?,
        version: None,
        package_category: row.get(6)?,
        rank: row.get(7)?,
        availability_lookups: Vec::new(),
    })
}
