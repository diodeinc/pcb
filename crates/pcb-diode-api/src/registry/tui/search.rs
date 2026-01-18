//! Background search and detail worker threads

use super::super::download::{
    download_registry_index_with_progress, fetch_registry_index_metadata, load_local_version,
    save_local_version, DownloadProgress, RegistryIndexMetadata,
};
use crate::{PackageRelations, RegistryClient, RegistryPart, SearchHit};
use colored::Colorize;
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

/// URL prefix for Diode registry components (excludes modules/generics/etc)
pub const DIODE_REGISTRY_COMPONENTS_PREFIX: &str = "github.com/diodeinc/registry/components";

/// Filter for search results based on URL prefix
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchFilter {
    /// Only packages with github.com/diodeinc/registry/components prefix (registry:components)
    ComponentsOnly,
    /// Exclude packages with github.com/diodeinc/registry/components prefix (registry:modules)
    ExcludeComponents,
}

impl SearchFilter {
    /// Returns (sql_clause, pattern) for use in WHERE conditions
    /// The clause uses ?N placeholder for the pattern parameter
    pub fn sql_clause(&self, param_num: u8) -> (&'static str, String) {
        let pattern = format!("{}%", DIODE_REGISTRY_COMPONENTS_PREFIX);
        let clause = match self {
            SearchFilter::ComponentsOnly => {
                if param_num == 3 {
                    "AND p.url LIKE ?3"
                } else {
                    "AND p.url LIKE ?1"
                }
            }
            SearchFilter::ExcludeComponents => {
                if param_num == 3 {
                    "AND p.url NOT LIKE ?3"
                } else {
                    "AND p.url NOT LIKE ?1"
                }
            }
        };
        (clause, pattern)
    }

    /// Check if a URL matches this filter
    pub fn matches(&self, url: &str) -> bool {
        match self {
            SearchFilter::ComponentsOnly => url.starts_with(DIODE_REGISTRY_COMPONENTS_PREFIX),
            SearchFilter::ExcludeComponents => !url.starts_with(DIODE_REGISTRY_COMPONENTS_PREFIX),
        }
    }
}

/// Color category for display
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathColor {
    Component, // green
    Module,    // blue
    Reference, // magenta
    Default,   // white
}

impl PathColor {
    pub fn from_category(category: Option<&str>) -> Self {
        match category {
            Some("component") => PathColor::Component,
            Some("module") => PathColor::Module,
            Some("reference") => PathColor::Reference,
            _ => PathColor::Default,
        }
    }

    /// Convert to ratatui Color
    pub fn to_ratatui(&self) -> ratatui::style::Color {
        use ratatui::style::Color;
        match self {
            PathColor::Component => Color::Green,
            PathColor::Module => Color::Blue,
            PathColor::Reference => Color::Magenta,
            PathColor::Default => Color::White,
        }
    }
}

/// Formatted display of a registry search result (shared between TUI and CLI)
pub struct RegistryResultDisplay {
    pub path: String,
    pub path_color: PathColor,
    pub version: Option<String>,
    pub line2_parts: Vec<(String, bool)>, // (text, is_dimmed)
    pub line3: Option<String>,            // description for components mode
}

impl RegistryResultDisplay {
    /// Create display from registry package data
    pub fn from_registry(
        url: &str,
        version: Option<&str>,
        package_category: Option<&str>,
        mpn: Option<&str>,
        manufacturer: Option<&str>,
        short_description: Option<&str>,
        is_modules_mode: bool,
    ) -> Self {
        let path = url
            .split('/')
            .skip(3) // Skip "github.com/diodeinc/registry"
            .collect::<Vec<_>>()
            .join("/");

        let path_color = PathColor::from_category(package_category);

        let mut line2_parts = Vec::new();
        if let Some(mpn_val) = mpn {
            line2_parts.push((mpn_val.to_string(), false)); // MPN: light grey
            if let Some(mfr) = manufacturer.filter(|m| !m.is_empty()) {
                line2_parts.push((" · ".to_string(), true));
                line2_parts.push((mfr.to_string(), true)); // Manufacturer: dark grey
            }
        } else {
            let desc = short_description.unwrap_or("");
            line2_parts.push((desc.to_string(), true));
        }

        let line3 = if !is_modules_mode && mpn.is_some() {
            Some(short_description.unwrap_or("").to_string())
        } else {
            None
        };

        Self {
            path,
            path_color,
            version: version.map(|v| v.to_string()),
            line2_parts,
            line3,
        }
    }

    /// Render to CLI output using colored crate
    pub fn to_cli_lines(&self) -> Vec<String> {
        let colored_path = match self.path_color {
            PathColor::Component => self.path.green().to_string(),
            PathColor::Module => self.path.blue().to_string(),
            PathColor::Reference => self.path.magenta().to_string(),
            PathColor::Default => self.path.white().to_string(),
        };

        let version_text = self
            .version
            .as_ref()
            .map(|v| format!(" ({})", v).yellow().dimmed().to_string())
            .unwrap_or_default();

        let line1 = format!("{}{}", colored_path, version_text);

        let line2_text: String = self
            .line2_parts
            .iter()
            .map(|(text, dimmed)| {
                if *dimmed {
                    text.dimmed().to_string()
                } else {
                    text.clone()
                }
            })
            .collect();
        let line2 = format!("  {}", line2_text);

        let mut lines = vec![line1, line2];
        if let Some(ref desc) = self.line3 {
            lines.push(format!("  {}", desc.dimmed()));
        }
        lines
    }

    /// Render to ratatui Lines for TUI
    pub fn to_tui_lines(
        &self,
        is_selected: bool,
        base_style: ratatui::style::Style,
        prefix_style: ratatui::style::Style,
    ) -> Vec<ratatui::text::Line<'static>> {
        use ratatui::style::{Color, Modifier};
        use ratatui::text::{Line, Span};

        let prefix = if is_selected { "▌" } else { " " };

        // Line 1: path + version
        let path_style = if is_selected {
            base_style.fg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else {
            base_style.fg(self.path_color.to_ratatui())
        };
        let version_style = base_style.fg(Color::Yellow).add_modifier(Modifier::DIM);
        let version_text = self
            .version
            .as_ref()
            .map(|v| format!(" ({})", v))
            .unwrap_or_default();

        let line1 = Line::from(vec![
            Span::styled(prefix.to_string(), prefix_style),
            Span::styled(" ".to_string(), base_style),
            Span::styled(self.path.clone(), path_style),
            Span::styled(version_text, version_style),
        ]);

        // Line 2: MPN · manufacturer or description
        let mut line2_spans = vec![
            Span::styled(prefix.to_string(), prefix_style),
            Span::styled("   ".to_string(), base_style),
        ];
        for (text, dimmed) in &self.line2_parts {
            let style = if *dimmed {
                base_style.fg(Color::DarkGray)
            } else {
                base_style.fg(Color::Gray) // MPN: lighter grey
            };
            line2_spans.push(Span::styled(text.clone(), style));
        }
        let line2 = Line::from(line2_spans);

        let mut lines = vec![line1, line2];

        // Line 3: description (only for components mode)
        if let Some(ref desc) = self.line3 {
            let line3 = Line::from(vec![
                Span::styled(prefix.to_string(), prefix_style),
                Span::styled("   ".to_string(), base_style),
                Span::styled(desc.clone(), base_style.fg(Color::DarkGray)),
            ]);
            lines.push(line3);
        }

        lines
    }
}

/// Formatted display of a web component search result (shared between TUI and CLI)
pub struct WebComponentDisplay {
    pub path: String,
    pub source: Option<String>,
    pub has_ecad: bool,
    pub has_step: bool,
    pub has_datasheet: bool,
    pub mpn: String,
    pub manufacturer: Option<String>,
    pub package: Option<String>,
    pub description: Option<String>,
}

impl WebComponentDisplay {
    pub fn from_component(result: &crate::component::ComponentSearchResult) -> Self {
        use crate::component::sanitize_mpn_for_path;

        let mfr = result
            .manufacturer
            .as_deref()
            .map(sanitize_mpn_for_path)
            .unwrap_or_else(|| "unknown".to_string());
        let mpn_sanitized = sanitize_mpn_for_path(&result.part_number);
        let path = format!("components/{}/{}", mfr, mpn_sanitized);

        Self {
            path,
            source: result.source.clone(),
            has_ecad: result.model_availability.ecad_model,
            has_step: result.model_availability.step_model,
            has_datasheet: !result.datasheets.is_empty(),
            mpn: result.part_number.clone(),
            manufacturer: result.manufacturer.clone(),
            package: result.package_category.clone(),
            description: result.description.clone(),
        }
    }

    fn source_abbrev(&self) -> &'static str {
        self.source
            .as_deref()
            .and_then(|s| {
                let lower = s.to_lowercase();
                if lower.contains("cse") {
                    Some("C")
                } else if lower.contains("lcsc") {
                    Some("L")
                } else if lower.contains("ncti") {
                    Some("N")
                } else {
                    None
                }
            })
            .unwrap_or("?")
    }

    /// Render to CLI output using colored crate
    pub fn to_cli_lines(&self) -> Vec<String> {
        let line1 = self.path.green().to_string();

        // Line 2: [source] EDA:✓ STEP:✗ Datasheet:✓ · MPN · Manufacturer · Package
        let check = "✓".green().to_string();
        let cross = "✗".red().to_string();
        let src = self.source_abbrev();

        let mut line2_parts = vec![
            format!("[{}]", src).dimmed().to_string(),
            " EDA:".to_string(),
            if self.has_ecad {
                check.clone()
            } else {
                cross.clone()
            },
            " STEP:".to_string(),
            if self.has_step {
                check.clone()
            } else {
                cross.clone()
            },
            " Datasheet:".to_string(),
            if self.has_datasheet { check } else { cross },
            " · ".dimmed().to_string(),
            self.mpn.yellow().to_string(),
        ];

        if let Some(ref mfr) = self.manufacturer {
            line2_parts.push(" · ".dimmed().to_string());
            line2_parts.push(mfr.dimmed().to_string());
        }
        if let Some(ref pkg) = self.package {
            line2_parts.push(" · ".dimmed().to_string());
            line2_parts.push(pkg.dimmed().to_string());
        }

        let line2 = format!("  {}", line2_parts.join(""));

        let line3 = format!("  {}", self.description.as_deref().unwrap_or("").dimmed());

        vec![line1, line2, line3]
    }

    fn source_color(&self) -> ratatui::style::Color {
        use ratatui::style::Color;
        self.source
            .as_deref()
            .map(|s| {
                let lower = s.to_lowercase();
                if lower.contains("cse") {
                    Color::Green
                } else if lower.contains("lcsc") {
                    Color::Yellow
                } else if lower.contains("ncti") {
                    Color::Cyan
                } else {
                    Color::DarkGray
                }
            })
            .unwrap_or(Color::DarkGray)
    }

    /// Render to ratatui Lines for TUI
    pub fn to_tui_lines(
        &self,
        is_selected: bool,
        base_style: ratatui::style::Style,
        prefix_style: ratatui::style::Style,
    ) -> Vec<ratatui::text::Line<'static>> {
        use ratatui::style::{Color, Modifier, Style};
        use ratatui::text::{Line, Span};

        let prefix = if is_selected { "▌" } else { " " };

        // Line 1: path
        let path_style = if is_selected {
            base_style.fg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else {
            base_style.fg(Color::Green)
        };
        let line1 = Line::from(vec![
            Span::styled(prefix.to_string(), prefix_style),
            Span::styled(" ".to_string(), base_style),
            Span::styled(self.path.clone(), path_style),
        ]);

        // Line 2: [source] EDA:✓ STEP:✗ Datasheet:✓ · MPN · Manufacturer · Package
        let dim_bracket = Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM);
        let dim_src = Style::default()
            .fg(self.source_color())
            .add_modifier(Modifier::DIM);
        let label_style = Style::default().fg(Color::Gray);
        let check = Span::styled("✓".to_string(), Style::default().fg(Color::Green));
        let cross = Span::styled("✗".to_string(), Style::default().fg(Color::Red));

        let mut line2_spans = vec![
            Span::styled(prefix.to_string(), prefix_style),
            Span::styled("   [".to_string(), dim_bracket),
            Span::styled(self.source_abbrev().to_string(), dim_src),
            Span::styled("] ".to_string(), dim_bracket),
            Span::styled("EDA:".to_string(), label_style),
            if self.has_ecad {
                check.clone()
            } else {
                cross.clone()
            },
            Span::styled(" STEP:".to_string(), label_style),
            if self.has_step {
                check.clone()
            } else {
                cross.clone()
            },
            Span::styled(" Datasheet:".to_string(), label_style),
            if self.has_datasheet { check } else { cross },
            Span::styled(" · ".to_string(), Style::default().fg(Color::DarkGray)),
            Span::styled(self.mpn.clone(), base_style.fg(Color::Yellow)),
        ];

        if let Some(ref mfr) = self.manufacturer {
            line2_spans.push(Span::styled(
                " · ".to_string(),
                Style::default().fg(Color::DarkGray),
            ));
            line2_spans.push(Span::styled(mfr.clone(), base_style.fg(Color::DarkGray)));
        }
        if let Some(ref pkg) = self.package {
            line2_spans.push(Span::styled(
                " · ".to_string(),
                Style::default().fg(Color::DarkGray),
            ));
            line2_spans.push(Span::styled(pkg.clone(), base_style.fg(Color::DarkGray)));
        }

        let line2 = Line::from(line2_spans);

        // Line 3: Description
        let desc = self.description.as_deref().unwrap_or("");
        let line3 = Line::from(vec![
            Span::styled(prefix.to_string(), prefix_style),
            Span::styled("   ".to_string(), base_style),
            Span::styled(desc.to_string(), base_style.fg(Color::DarkGray)),
        ]);

        vec![line1, line2, line3]
    }
}

/// Query sent to the worker thread
#[derive(Debug, Clone)]
pub struct SearchQuery {
    pub id: u64,
    pub text: String,
    /// If true, force a registry index update check
    pub force_update: bool,
    /// Optional filter for URL prefix
    pub filter: Option<SearchFilter>,
}

/// Scoring details for a part across indices (for debug panels)
#[derive(Debug, Clone, Default)]
pub struct PartScoring {
    pub trigram_position: Option<usize>,
    pub trigram_rank: Option<f64>,
    pub word_position: Option<usize>,
    pub word_rank: Option<f64>,
    pub semantic_position: Option<usize>,
    pub semantic_rank: Option<f64>,
}

/// Results from the worker thread
#[derive(Debug, Clone)]
pub struct SearchResults {
    pub query_id: u64,
    pub trigram: Vec<SearchHit>,
    pub word: Vec<SearchHit>,
    pub semantic: Vec<SearchHit>,
    pub merged: Vec<SearchHit>,
    pub scoring: HashMap<String, PartScoring>,
    pub duration: Duration,
}

impl Default for SearchResults {
    fn default() -> Self {
        Self {
            query_id: 0,
            trigram: Vec::new(),
            word: Vec::new(),
            semantic: Vec::new(),
            merged: Vec::new(),
            scoring: HashMap::new(),
            duration: Duration::ZERO,
        }
    }
}

/// Query sent to the component search worker (online API)
#[derive(Debug, Clone)]
pub struct ComponentSearchQuery {
    pub id: u64,
    pub text: String,
}

/// Results from the component search worker
#[derive(Debug, Clone)]
pub struct ComponentSearchResults {
    pub query_id: u64,
    pub results: Vec<crate::component::ComponentSearchResult>,
    pub duration: Duration,
    pub error: Option<String>,
}

impl Default for ComponentSearchResults {
    fn default() -> Self {
        Self {
            query_id: 0,
            results: Vec::new(),
            duration: Duration::ZERO,
            error: None,
        }
    }
}

/// Request to fetch details for a specific part
#[derive(Debug)]
pub struct DetailRequest {
    pub part_id: i64,
}

/// Response with full part details
#[derive(Debug)]
pub struct DetailResponse {
    pub part_id: i64,
    pub part: Option<RegistryPart>,
    pub relations: PackageRelations,
}

/// Spawn the detail worker thread (fetches full part details on demand)
pub fn spawn_detail_worker(
    req_rx: Receiver<DetailRequest>,
    resp_tx: Sender<DetailResponse>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let db_path = match RegistryClient::default_db_path() {
            Ok(p) => p,
            Err(_) => return,
        };

        // Wait for database to become available (search worker downloads it)
        let mut client: Option<RegistryClient> = None;
        let mut last_mtime: Option<SystemTime> = None;

        while let Ok(mut req) = req_rx.recv() {
            // Coalesce rapid selection changes - keep only the latest request
            while let Ok(next) = req_rx.try_recv() {
                req = next;
            }

            // Try to open/reload DB if file changed or client not yet available
            let current_mtime = get_file_mtime(&db_path);
            if client.is_none() || current_mtime != last_mtime {
                if let Ok(new_client) = RegistryClient::open_path(&db_path) {
                    client = Some(new_client);
                    last_mtime = current_mtime;
                }
            }

            // If client still not available (DB not downloaded yet), send empty response
            let Some(ref c) = client else {
                let _ = resp_tx.send(DetailResponse {
                    part_id: req.part_id,
                    part: None,
                    relations: PackageRelations::default(),
                });
                continue;
            };

            let part = c.get_part_by_id(req.part_id).ok().flatten();

            let relations = if part.is_some() {
                PackageRelations {
                    dependencies: c.get_dependencies(req.part_id).unwrap_or_default(),
                    dependents: c.get_dependents(req.part_id).unwrap_or_default(),
                }
            } else {
                PackageRelations::default()
            };

            let _ = resp_tx.send(DetailResponse {
                part_id: req.part_id,
                part,
                relations,
            });
        }
    })
}

/// Get file modification time, returns None on error
fn get_file_mtime(path: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

/// Spawn the search worker thread
///
/// If `prefetched_metadata` is provided, it will be used for the initial download
/// to avoid a duplicate API request.
pub fn spawn_worker(
    query_rx: Receiver<SearchQuery>,
    result_tx: Sender<SearchResults>,
    download_tx: Sender<DownloadProgress>,
    prefetched_metadata: Option<RegistryIndexMetadata>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let db_path = match RegistryClient::default_db_path() {
            Ok(p) => p,
            Err(e) => {
                let _ = download_tx.send(DownloadProgress {
                    pct: None,
                    done: true,
                    error: Some(format!("Failed to get db path: {}", e)),
                    is_update: false,
                });
                return;
            }
        };

        // If DB doesn't exist, must download first (blocking)
        if !db_path.exists() {
            if download_registry_index_with_progress(
                &db_path,
                &download_tx,
                false,
                prefetched_metadata.as_ref(),
            )
            .is_err()
            {
                return;
            }
            // Download succeeded - send done signal
            let _ = download_tx.send(DownloadProgress {
                pct: Some(100),
                done: true,
                error: None,
                is_update: false,
            });
        } else {
            // DB exists, signal ready immediately
            let _ = download_tx.send(DownloadProgress {
                pct: Some(100),
                done: true,
                error: None,
                is_update: false,
            });
        }

        // Open initial client and track file mtime
        let mut client = match RegistryClient::open_path(&db_path) {
            Ok(c) => c,
            Err(e) => {
                let _ = download_tx.send(DownloadProgress {
                    pct: None,
                    done: true,
                    error: Some(format!("Failed to open registry: {}", e)),
                    is_update: false,
                });
                return;
            }
        };
        let mut last_mtime = get_file_mtime(&db_path);

        // Helper function to perform update check in background
        fn spawn_update_check(
            db_path: std::path::PathBuf,
            download_tx: Sender<DownloadProgress>,
            force: bool,
        ) {
            thread::spawn(move || {
                // Fetch remote metadata (any error just becomes a failed update)
                let meta = match fetch_registry_index_metadata() {
                    Ok(m) => m,
                    Err(e) => {
                        // Failed to check for updates - silently ignore (we have a working index)
                        // Only show error if this was a forced update
                        if force {
                            let _ = download_tx.send(DownloadProgress {
                                pct: None,
                                done: true,
                                error: Some(e.to_string()),
                                is_update: true,
                            });
                        }
                        return;
                    }
                };

                let remote_version = &meta.sha256;
                let local_version = load_local_version(&db_path);

                if !force && local_version.as_deref() == Some(remote_version.as_str()) {
                    return; // Up-to-date, nothing to do
                }

                // Stale or forced: download new index (use fetched metadata to avoid duplicate request)
                if let Err(e) =
                    download_registry_index_with_progress(&db_path, &download_tx, true, Some(&meta))
                {
                    let _ = download_tx.send(DownloadProgress {
                        pct: None,
                        done: true,
                        error: Some(format!("Update failed: {}", e)),
                        is_update: true,
                    });
                    return;
                }

                let _ = save_local_version(&db_path, remote_version);

                // Send done - worker will detect file change via mtime
                let _ = download_tx.send(DownloadProgress {
                    pct: Some(100),
                    done: true,
                    error: None,
                    is_update: true,
                });
            });
        }

        // Spawn initial background update check
        spawn_update_check(db_path.clone(), download_tx.clone(), false);

        // Main search loop
        let mut update_pending = false;
        while let Ok(mut query) = query_rx.recv() {
            // Drain pending queries, keep only the latest (coalesce rapid typing)
            while let Ok(next) = query_rx.try_recv() {
                query = next;
            }

            // Check if DB file was modified (simple, robust reload detection)
            let current_mtime = get_file_mtime(&db_path);
            if current_mtime != last_mtime {
                if let Ok(new_client) = RegistryClient::open_path(&db_path) {
                    client = new_client;
                    last_mtime = current_mtime;
                    update_pending = false;
                }
            }

            // Handle force update request (only one at a time)
            if query.force_update && !update_pending {
                update_pending = true;
                spawn_update_check(db_path.clone(), download_tx.clone(), true);
            }

            let start = Instant::now();
            let rrf = client.search_rrf(&query.text, query.filter);
            let duration = start.elapsed();

            // Compute scoring for debug panels
            let mut scoring: HashMap<String, PartScoring> = HashMap::new();
            for (i, hit) in rrf.trigram.iter().enumerate() {
                let entry = scoring.entry(hit.url.clone()).or_default();
                entry.trigram_position = Some(i);
                entry.trigram_rank = hit.rank;
            }
            for (i, hit) in rrf.word.iter().enumerate() {
                let entry = scoring.entry(hit.url.clone()).or_default();
                entry.word_position = Some(i);
                entry.word_rank = hit.rank;
            }
            for (i, hit) in rrf.semantic.iter().enumerate() {
                let entry = scoring.entry(hit.url.clone()).or_default();
                entry.semantic_position = Some(i);
                entry.semantic_rank = hit.rank;
            }

            let _ = result_tx.send(SearchResults {
                query_id: query.id,
                trigram: rrf.trigram,
                word: rrf.word,
                semantic: rrf.semantic,
                merged: rrf.merged,
                scoring,
                duration,
            });
        }
    })
}

/// Spawn the component search worker thread (online API search)
///
/// This worker handles searches for new components from online sources.
/// It caches the auth token lazily on first request.
pub fn spawn_component_worker(
    query_rx: Receiver<ComponentSearchQuery>,
    result_tx: Sender<ComponentSearchResults>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        // Cache auth token (fetched lazily on first request)
        let mut auth_token: Option<String> = None;

        while let Ok(mut query) = query_rx.recv() {
            // Coalesce rapid queries - keep only the latest
            while let Ok(next) = query_rx.try_recv() {
                query = next;
            }

            let query_text = query.text.trim();

            // Empty query - return empty results
            if query_text.is_empty() {
                let _ = result_tx.send(ComponentSearchResults {
                    query_id: query.id,
                    results: Vec::new(),
                    duration: Duration::ZERO,
                    error: None,
                });
                continue;
            }

            // Get auth token (lazy)
            if auth_token.is_none() {
                match crate::auth::get_valid_token() {
                    Ok(token) => auth_token = Some(token),
                    Err(e) => {
                        let _ = result_tx.send(ComponentSearchResults {
                            query_id: query.id,
                            results: Vec::new(),
                            duration: Duration::ZERO,
                            error: Some(format!("Auth failed: {}", e)),
                        });
                        continue;
                    }
                }
            }

            let token = auth_token.as_ref().unwrap();

            // Execute search
            let start = Instant::now();
            let search_result = crate::component::search_components(token, query_text);
            let duration = start.elapsed();

            let (results, error) = match search_result {
                Ok(all_results) => {
                    // Filter to only components with ECAD models
                    let filtered: Vec<_> = all_results
                        .into_iter()
                        .filter(|r| r.model_availability.ecad_model)
                        .collect();
                    (filtered, None)
                }
                Err(e) => {
                    // Clear cached token on auth errors so we retry
                    if e.to_string().contains("401") || e.to_string().contains("403") {
                        auth_token = None;
                    }
                    (Vec::new(), Some(e.to_string()))
                }
            };

            let _ = result_tx.send(ComponentSearchResults {
                query_id: query.id,
                results,
                duration,
                error,
            });
        }
    })
}

/// Batch availability request: Vec of (id, mpn, manufacturer)
pub type PricingRequest = Vec<(String, String, Option<String>)>;

/// Batch availability response: Map of id -> availability
pub type PricingResponse = HashMap<String, pcb_sch::bom::Availability>;

/// Spawn a worker thread that fetches availability for components in batches
pub fn spawn_availability_worker(
    req_rx: Receiver<PricingRequest>,
    resp_tx: Sender<PricingResponse>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        use crate::bom::ComponentKey;

        let mut auth_token: Option<String> = None;
        let mut cache: HashMap<ComponentKey, pcb_sch::bom::Availability> = HashMap::new();

        while let Ok(mut req) = req_rx.recv() {
            // Coalesce rapid requests - keep only the latest
            while let Ok(next) = req_rx.try_recv() {
                req = next;
            }

            if req.is_empty() {
                let _ = resp_tx.send(HashMap::new());
                continue;
            }

            // Check cache, collect uncached
            let mut result: HashMap<String, pcb_sch::bom::Availability> = HashMap::new();
            let mut uncached: Vec<&(String, String, Option<String>)> = Vec::new();

            for item in &req {
                let key = ComponentKey {
                    mpn: item.1.clone(),
                    manufacturer: item.2.clone(),
                };
                if let Some(cached) = cache.get(&key) {
                    result.insert(item.0.clone(), cached.clone());
                } else {
                    uncached.push(item);
                }
            }

            if uncached.is_empty() {
                let _ = resp_tx.send(result);
                continue;
            }

            // Get auth token
            if auth_token.is_none() {
                match crate::auth::get_valid_token() {
                    Ok(token) => auth_token = Some(token),
                    Err(e) => {
                        log::warn!("Pricing auth failed: {}", e);
                        let _ = resp_tx.send(result);
                        continue;
                    }
                }
            }

            let token = auth_token.as_ref().unwrap();
            let batch_keys: Vec<_> = uncached
                .iter()
                .map(|item| ComponentKey {
                    mpn: item.1.clone(),
                    manufacturer: item.2.clone(),
                })
                .collect();

            match crate::bom::fetch_pricing_batch(token, &batch_keys) {
                Ok(availability_results) => {
                    for (item, availability) in uncached.iter().zip(availability_results) {
                        let key = ComponentKey {
                            mpn: item.1.clone(),
                            manufacturer: item.2.clone(),
                        };
                        cache.insert(key, availability.clone());
                        result.insert(item.0.clone(), availability);
                    }
                }
                Err(e) => {
                    log::warn!("Pricing API failed: {}", e);
                }
            }

            let _ = resp_tx.send(result);
        }
    })
}
