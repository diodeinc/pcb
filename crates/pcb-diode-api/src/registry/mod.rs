use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;

pub mod download;
pub mod embeddings;
pub mod tui;

/// Digikey distribution data parsed from JSON
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DigikeyData {
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
    pub parameters: std::collections::BTreeMap<String, String>,
}

/// eDatasheet structured component data parsed from JSON
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EDatasheetData {
    #[serde(rename = "componentID")]
    pub component_id: Option<EDatasheetComponentId>,
    #[serde(rename = "coreProperties")]
    pub core_properties: Option<serde_json::Value>,
    pub package: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EDatasheetComponentId {
    #[serde(rename = "partType")]
    pub part_type: Option<String>,
    pub manufacturer: Option<String>,
    #[serde(rename = "componentName")]
    pub component_name: Option<String>,
    pub status: Option<String>,
}

/// Lightweight search hit - just enough for ranking
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub id: i64,
    pub registry_path: String,
    pub mpn: String,
    pub manufacturer: Option<String>,
    pub rank: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryPart {
    pub id: i64,
    pub mpn: String,
    pub manufacturer: Option<String>,
    pub part_type: Option<String>,
    pub category: Option<String>,
    pub short_description: Option<String>,
    pub detailed_description: Option<String>,
    pub registry_path: String,
    /// FTS5 rank score (lower is better match, typically negative)
    #[serde(default)]
    pub rank: Option<f64>,
    /// Digikey distribution data (parsed from JSONB blob)
    #[serde(default)]
    pub digikey: Option<DigikeyData>,
    /// eDatasheet structured component data (parsed from JSONB blob)
    #[serde(default)]
    pub edatasheet: Option<EDatasheetData>,
    /// AVIF-encoded image data
    #[serde(default, skip)]
    pub image_data: Option<Vec<u8>>,
}

/// Preprocessed query ready for FTS search
#[derive(Debug)]
pub struct ParsedQuery {
    /// Original query string
    pub original: String,
    /// Canonicalized form for trigram MPN search (alphanumeric only, uppercase)
    pub mpn_canon: String,
    /// Tokens for word-based FTS search
    pub word_tokens: Vec<String>,
    /// Whether the query looks like an MPN (vs natural language description)
    pub looks_like_mpn: bool,
}

impl ParsedQuery {
    pub fn parse(query: &str) -> Self {
        let original = query.trim().to_string();
        let mpn_canon = canonicalize_mpn(&original);
        let word_tokens = tokenize_for_words(&original);
        let looks_like_mpn = detect_mpn_query(&original, &mpn_canon);

        Self {
            original,
            mpn_canon,
            word_tokens,
            looks_like_mpn,
        }
    }
}

/// Canonicalize an MPN query: uppercase, remove all non-alphanumeric chars
/// This matches how mpn_canon is stored in the FTS index
fn canonicalize_mpn(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_uppercase()
}

/// Tokenize for word-based FTS search
/// Splits on whitespace, lowercases, removes very short tokens
fn tokenize_for_words(s: &str) -> Vec<String> {
    s.split(|c: char| c.is_whitespace() || c == ',' || c == ';')
        .map(|w| w.trim().to_lowercase())
        .filter(|w| w.len() >= 2)
        .collect()
}

/// Detect if query looks like an MPN vs natural language
/// MPNs typically: have digits, are single "word", have specific patterns
fn detect_mpn_query(original: &str, canon: &str) -> bool {
    let word_count = original.split_whitespace().count();
    let has_digits = canon.chars().any(|c| c.is_ascii_digit());
    let alpha_count = canon.chars().filter(|c| c.is_ascii_alphabetic()).count();
    let digit_count = canon.chars().filter(|c| c.is_ascii_digit()).count();

    // Single "word" with digits is likely an MPN
    if word_count == 1 && has_digits {
        return true;
    }

    // Two words where one looks like MPN prefix (e.g., "STM32 microcontroller")
    if word_count == 2 {
        let first = original.split_whitespace().next().unwrap_or("");
        let first_canon = canonicalize_mpn(first);
        if first_canon.len() >= 4
            && first_canon.chars().any(|c| c.is_ascii_digit())
            && first_canon.chars().any(|c| c.is_ascii_alphabetic())
        {
            return true;
        }
    }

    // High ratio of digits suggests MPN
    if canon.len() >= 4 && digit_count as f32 / canon.len() as f32 > 0.3 {
        return true;
    }

    // Known MPN prefixes
    let prefixes = [
        "STM32", "STM8", "ESP32", "ESP8266", "ATM", "ATMEGA", "ATTINY", "PIC", "LM", "TPS", "TLV",
        "MAX", "LTC", "AD", "ADP", "TCA", "INA", "OPA", "LDO", "REG", "MCP", "24LC", "93LC", "W25",
        "SST", "IRFZ", "IRF", "BSS", "SI", "FDS", "AO", "DMG", "CSD",
    ];
    let upper = original.to_uppercase();
    for prefix in prefixes {
        if upper.starts_with(prefix) {
            return true;
        }
    }

    // More than 2 words is likely a description
    if word_count > 2 {
        return false;
    }

    // Default: if short and has mixed alpha+digit, assume MPN
    alpha_count > 0 && digit_count > 0 && canon.len() <= 20
}

/// Escape special FTS5 characters in a token
fn escape_fts5(s: &str) -> String {
    // FTS5 special chars that need quoting: " * ( ) : ^ - . + < > ~ @
    // We wrap in quotes to make it literal
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
}

impl RegistryClient {
    /// Get the default registry database path (~/.pcb/registry/parts.db)
    pub fn default_db_path() -> Result<PathBuf> {
        let home = dirs::home_dir().context("Could not determine home directory")?;
        Ok(home.join(".pcb").join("registry").join("parts.db"))
    }

    /// Open the registry database from the default location.
    /// Downloads the index from the API server if not present locally.
    pub fn open() -> Result<Self> {
        let path = Self::default_db_path()?;
        if !path.exists() {
            download::download_registry_index(&path)?;
        }
        Self::open_path(&path)
    }

    /// Open the registry database from a specific path
    pub fn open_path(path: &PathBuf) -> Result<Self> {
        if !path.exists() {
            anyhow::bail!(
                "Registry database not found at {}. Run `pcb registry update` to download it.",
                path.display()
            );
        }

        // Register sqlite-vec extension BEFORE opening connection
        unsafe {
            rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute(
                sqlite_vec::sqlite3_vec_init as *const (),
            )));
        }

        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .context("Failed to open registry database")?;

        // Optimize for read-only access
        conn.execute_batch(
            "PRAGMA mmap_size = 268435456;  -- 256MB memory-mapped I/O
             PRAGMA cache_size = -65536;    -- 64MB page cache
             PRAGMA query_only = ON;",
        )
        .context("Failed to set read-only pragmas")?;

        Ok(Self { conn })
    }

    /// Search the registry with automatic query preprocessing
    /// Searches both trigram (MPN) and word indices, deduplicates results
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<RegistryPart>> {
        let parsed = ParsedQuery::parse(query);

        // Search both indices
        let trigram_results = self.search_trigram_internal(&parsed, limit)?;
        let word_results = self.search_words_internal(&parsed, limit)?;

        // Merge and deduplicate, preserving order (trigram first if MPN-like)
        let mut seen = HashSet::new();
        let mut results = Vec::new();

        let (primary, secondary) = if parsed.looks_like_mpn {
            (trigram_results, word_results)
        } else {
            (word_results, trigram_results)
        };

        for part in primary {
            if seen.insert(part.id) {
                results.push(part);
            }
        }
        for part in secondary {
            if seen.insert(part.id) {
                results.push(part);
            }
        }

        results.truncate(limit);
        Ok(results)
    }

    /// Lightweight trigram search - returns only IDs, MPNs, and ranks
    pub fn search_trigram_hits(
        &self,
        parsed: &ParsedQuery,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        if parsed.mpn_canon.is_empty() {
            return Ok(Vec::new());
        }

        let fts_query = escape_fts5(&parsed.mpn_canon);

        let mut stmt = self.conn.prepare(
            r#"
            SELECT p.id, p.registry_path, p.mpn, p.manufacturer, fts.rank
            FROM part_fts_ids fts
            JOIN parts p ON p.id = CAST(fts.part_id AS INTEGER)
            WHERE part_fts_ids MATCH ?1
            ORDER BY rank
            LIMIT ?2
            "#,
        )?;

        let rows = stmt.query_map([&fts_query, &limit.to_string()], |row| {
            Ok(SearchHit {
                id: row.get(0)?,
                registry_path: row.get(1)?,
                mpn: row.get(2)?,
                manufacturer: row.get(3)?,
                rank: row.get(4)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Lightweight word search - returns only IDs, MPNs, and ranks
    pub fn search_words_hits(&self, parsed: &ParsedQuery, limit: usize) -> Result<Vec<SearchHit>> {
        if parsed.word_tokens.is_empty() {
            return Ok(Vec::new());
        }

        let fts_query = parsed
            .word_tokens
            .iter()
            .map(|t| format!("{}*", escape_fts5(t)))
            .collect::<Vec<_>>()
            .join(" ");

        let mut stmt = self.conn.prepare(
            r#"
            SELECT p.id, p.registry_path, p.mpn, p.manufacturer, fts.rank
            FROM part_fts_words fts
            JOIN parts p ON p.id = CAST(fts.part_id AS INTEGER)
            WHERE part_fts_words MATCH ?1
            ORDER BY rank
            LIMIT ?2
            "#,
        )?;

        let rows = stmt.query_map([&fts_query, &limit.to_string()], |row| {
            Ok(SearchHit {
                id: row.get(0)?,
                registry_path: row.get(1)?,
                mpn: row.get(2)?,
                manufacturer: row.get(3)?,
                rank: row.get(4)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Lightweight semantic search - returns only IDs, MPNs, and distances
    pub fn search_semantic_hits(
        &self,
        embedding: &[f32; 1024],
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        let embedding_bytes: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();

        let mut stmt = self.conn.prepare(
            r#"
            SELECT p.id, p.registry_path, p.mpn, p.manufacturer, v.distance
            FROM part_vec v
            JOIN parts p ON p.id = v.rowid
            WHERE v.embedding MATCH ?1 AND v.k = ?2
            ORDER BY v.distance
            "#,
        )?;

        let rows = stmt.query_map(rusqlite::params![embedding_bytes, limit as i64], |row| {
            Ok(SearchHit {
                id: row.get(0)?,
                registry_path: row.get(1)?,
                mpn: row.get(2)?,
                manufacturer: row.get(3)?,
                rank: row.get(4)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Fetch full details for a single part by ID
    pub fn get_part_by_id(&self, id: i64) -> Result<Option<RegistryPart>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT id, mpn, manufacturer, part_type, category,
                   short_description, detailed_description, registry_path,
                   json(edatasheet), json(digikey), image
            FROM parts
            WHERE id = ?1
            "#,
        )?;

        let result = stmt
            .query_row([id], |row| {
                let edatasheet_json: Option<String> = row.get(8)?;
                let digikey_json: Option<String> = row.get(9)?;
                Ok(RegistryPart {
                    id: row.get(0)?,
                    mpn: row.get(1)?,
                    manufacturer: row.get(2)?,
                    part_type: row.get(3)?,
                    category: row.get(4)?,
                    short_description: row.get(5)?,
                    detailed_description: row.get(6)?,
                    registry_path: row.get(7)?,
                    rank: None,
                    edatasheet: edatasheet_json.and_then(|s| serde_json::from_str(&s).ok()),
                    digikey: digikey_json.and_then(|s| serde_json::from_str(&s).ok()),
                    image_data: row.get(10)?,
                })
            })
            .ok();

        Ok(result)
    }

    /// Search using trigram matching (for MPN/part number matching)
    /// Takes a pre-parsed query - useful for TUI where we control parsing
    pub fn search_trigram_raw(
        &self,
        parsed: &ParsedQuery,
        limit: usize,
    ) -> Result<Vec<RegistryPart>> {
        self.search_trigram_internal(parsed, limit)
    }

    /// Search using word tokenization (for description/keyword matching)
    /// Takes a pre-parsed query - useful for TUI where we control parsing
    pub fn search_words_raw(
        &self,
        parsed: &ParsedQuery,
        limit: usize,
    ) -> Result<Vec<RegistryPart>> {
        self.search_words_internal(parsed, limit)
    }

    /// Search using semantic vector similarity
    /// Takes a pre-computed embedding vector
    pub fn search_semantic(
        &self,
        embedding: &[f32; 1024],
        limit: usize,
    ) -> Result<Vec<RegistryPart>> {
        // Convert embedding to bytes for sqlite-vec
        let embedding_bytes: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();

        let mut stmt = self.conn.prepare(
            r#"
            SELECT p.id, p.mpn, p.manufacturer, p.part_type, p.category,
                   p.short_description, p.detailed_description, p.registry_path,
                   v.distance,
                   json(p.edatasheet), json(p.digikey), p.image
            FROM part_vec v
            JOIN parts p ON p.id = v.rowid
            WHERE v.embedding MATCH ?1 AND v.k = ?2
            ORDER BY v.distance
            "#,
        )?;

        let rows = stmt.query_map(rusqlite::params![embedding_bytes, limit as i64], |row| {
            let edatasheet_json: Option<String> = row.get(9)?;
            let digikey_json: Option<String> = row.get(10)?;
            Ok(RegistryPart {
                id: row.get(0)?,
                mpn: row.get(1)?,
                manufacturer: row.get(2)?,
                part_type: row.get(3)?,
                category: row.get(4)?,
                short_description: row.get(5)?,
                detailed_description: row.get(6)?,
                registry_path: row.get(7)?,
                rank: row.get(8)?,
                edatasheet: edatasheet_json.and_then(|s| serde_json::from_str(&s).ok()),
                digikey: digikey_json.and_then(|s| serde_json::from_str(&s).ok()),
                image_data: row.get(11)?,
            })
        })?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }

        Ok(results)
    }

    fn search_trigram_internal(
        &self,
        parsed: &ParsedQuery,
        limit: usize,
    ) -> Result<Vec<RegistryPart>> {
        if parsed.mpn_canon.is_empty() {
            return Ok(Vec::new());
        }

        // For trigram search, we search the canonicalized MPN directly
        // The trigram tokenizer will match substrings
        let fts_query = escape_fts5(&parsed.mpn_canon);

        let mut stmt = self.conn.prepare(
            r#"
            SELECT p.id, p.mpn, p.manufacturer, p.part_type, p.category, 
                   p.short_description, p.detailed_description, p.registry_path, fts.rank,
                   json(p.edatasheet), json(p.digikey), p.image
            FROM part_fts_ids fts
            JOIN parts p ON p.id = CAST(fts.part_id AS INTEGER)
            WHERE part_fts_ids MATCH ?1
            ORDER BY rank
            LIMIT ?2
            "#,
        )?;

        let rows = stmt.query_map([&fts_query, &limit.to_string()], |row| {
            let edatasheet_json: Option<String> = row.get(9)?;
            let digikey_json: Option<String> = row.get(10)?;
            Ok(RegistryPart {
                id: row.get(0)?,
                mpn: row.get(1)?,
                manufacturer: row.get(2)?,
                part_type: row.get(3)?,
                category: row.get(4)?,
                short_description: row.get(5)?,
                detailed_description: row.get(6)?,
                registry_path: row.get(7)?,
                rank: row.get(8)?,
                edatasheet: edatasheet_json.and_then(|s| serde_json::from_str(&s).ok()),
                digikey: digikey_json.and_then(|s| serde_json::from_str(&s).ok()),
                image_data: row.get(11)?,
            })
        })?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }

        Ok(results)
    }

    /// Search using word tokenization (for description/keyword matching)
    fn search_words_internal(
        &self,
        parsed: &ParsedQuery,
        limit: usize,
    ) -> Result<Vec<RegistryPart>> {
        if parsed.word_tokens.is_empty() {
            return Ok(Vec::new());
        }

        // Build FTS5 query with prefix matching for each token
        let fts_query = parsed
            .word_tokens
            .iter()
            .map(|t| format!("{}*", escape_fts5(t)))
            .collect::<Vec<_>>()
            .join(" ");

        let mut stmt = self.conn.prepare(
            r#"
            SELECT p.id, p.mpn, p.manufacturer, p.part_type, p.category, 
                   p.short_description, p.detailed_description, p.registry_path, fts.rank,
                   json(p.edatasheet), json(p.digikey), p.image
            FROM part_fts_words fts
            JOIN parts p ON p.id = CAST(fts.part_id AS INTEGER)
            WHERE part_fts_words MATCH ?1
            ORDER BY rank
            LIMIT ?2
            "#,
        )?;

        let rows = stmt.query_map([&fts_query, &limit.to_string()], |row| {
            let edatasheet_json: Option<String> = row.get(9)?;
            let digikey_json: Option<String> = row.get(10)?;
            Ok(RegistryPart {
                id: row.get(0)?,
                mpn: row.get(1)?,
                manufacturer: row.get(2)?,
                part_type: row.get(3)?,
                category: row.get(4)?,
                short_description: row.get(5)?,
                detailed_description: row.get(6)?,
                registry_path: row.get(7)?,
                rank: row.get(8)?,
                edatasheet: edatasheet_json.and_then(|s| serde_json::from_str(&s).ok()),
                digikey: digikey_json.and_then(|s| serde_json::from_str(&s).ok()),
                image_data: row.get(11)?,
            })
        })?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }

        Ok(results)
    }

    /// Get total count of parts in registry
    pub fn count(&self) -> Result<i64> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM parts", [], |row| row.get(0))?;
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_canonicalize_mpn() {
        assert_eq!(canonicalize_mpn("STM32G431"), "STM32G431");
        assert_eq!(canonicalize_mpn("stm32g431"), "STM32G431");
        assert_eq!(canonicalize_mpn("STM32-G431"), "STM32G431");
        assert_eq!(canonicalize_mpn("1-406541-1"), "14065411");
        assert_eq!(canonicalize_mpn("LM358 "), "LM358");
        assert_eq!(canonicalize_mpn("TPS82140SILR"), "TPS82140SILR");
    }

    #[test]
    fn test_detect_mpn_query() {
        // Clear MPNs
        assert!(detect_mpn_query("STM32G431", "STM32G431"));
        assert!(detect_mpn_query("TPS82140", "TPS82140"));
        assert!(detect_mpn_query("1-406541-1", "14065411"));
        assert!(detect_mpn_query("2N7002", "2N7002"));
        assert!(detect_mpn_query("LM358", "LM358"));

        // Clear descriptions
        assert!(!detect_mpn_query(
            "n-channel mosfet 60v",
            "NCHANNELMOSFET60V"
        ));
        assert!(!detect_mpn_query(
            "voltage regulator 3.3v",
            "VOLTAGEREGULATOR33V"
        ));
        assert!(!detect_mpn_query(
            "usb type c connector",
            "USBTYPECCONNECTOR"
        ));

        // Edge cases
        assert!(detect_mpn_query(
            "STM32 microcontroller",
            "STM32MICROCONTROLLER"
        ));
        assert!(!detect_mpn_query("mosfet", "MOSFET"));
        assert!(!detect_mpn_query("capacitor", "CAPACITOR"));
    }

    #[test]
    fn test_tokenize_for_words() {
        assert_eq!(
            tokenize_for_words("n-channel mosfet"),
            vec!["n-channel", "mosfet"]
        );
        assert_eq!(
            tokenize_for_words("voltage regulator 3.3V"),
            vec!["voltage", "regulator", "3.3v"]
        );
        assert_eq!(tokenize_for_words("a b cd"), vec!["cd"]); // filters short tokens
    }

    #[test]
    fn test_escape_fts5() {
        assert_eq!(escape_fts5("simple"), "simple");
        assert_eq!(escape_fts5("has-dash"), "\"has-dash\"");
        assert_eq!(escape_fts5("has*star"), "\"has*star\"");
        assert_eq!(escape_fts5("3.3v"), "\"3.3v\"");
        assert_eq!(escape_fts5("test@email"), "\"test@email\"");
    }

    #[test]
    fn test_parsed_query() {
        let q = ParsedQuery::parse("STM32G431");
        assert_eq!(q.mpn_canon, "STM32G431");
        assert!(q.looks_like_mpn);

        let q = ParsedQuery::parse("n-channel mosfet 60v");
        assert!(!q.looks_like_mpn);
        assert_eq!(q.word_tokens, vec!["n-channel", "mosfet", "60v"]);
    }
}
