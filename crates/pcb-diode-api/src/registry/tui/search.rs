//! Background search and detail worker threads

use super::super::download::{
    download_registry_index_with_progress, fetch_registry_index_metadata, load_local_version,
    save_local_version, DownloadProgress, RegistryIndexMetadata,
};
use super::super::embeddings;
use crate::{PackageRelations, ParsedQuery, RegistryClient, RegistryPart, SearchHit};
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

/// Query sent to the worker thread
#[derive(Debug, Clone)]
pub struct SearchQuery {
    pub id: u64,
    pub text: String,
    /// If true, force a registry index update check
    pub force_update: bool,
}

/// Scoring details for a part across indices
#[derive(Debug, Clone, Default)]
pub struct PartScoring {
    pub trigram_position: Option<usize>,
    pub trigram_rank: Option<f64>,
    pub word_position: Option<usize>,
    pub word_rank: Option<f64>,
    pub semantic_position: Option<usize>,
    pub semantic_rank: Option<f64>,
}

/// Merged result item - lightweight, just enough for display in the list
#[derive(Debug, Clone)]
pub struct MergedHit {
    pub id: i64,
    pub url: String,
    pub mpn: Option<String>,
    pub manufacturer: Option<String>,
    pub short_description: Option<String>,
    pub version: Option<String>,
    pub package_category: Option<String>,
}

/// Results from the worker thread
#[derive(Debug, Clone)]
pub struct SearchResults {
    pub query_id: u64,
    pub trigram: Vec<SearchHit>,
    pub word: Vec<SearchHit>,
    pub semantic: Vec<SearchHit>,
    pub merged: Vec<MergedHit>,
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
            let results = execute_search(&client, &query);
            let duration = start.elapsed();

            let _ = result_tx.send(SearchResults {
                query_id: query.id,
                trigram: results.trigram,
                word: results.word,
                semantic: results.semantic,
                merged: results.merged,
                scoring: results.scoring,
                duration,
            });
        }
    })
}

struct SearchOutput {
    trigram: Vec<SearchHit>,
    word: Vec<SearchHit>,
    semantic: Vec<SearchHit>,
    merged: Vec<MergedHit>,
    scoring: HashMap<String, PartScoring>,
}

/// Execute a search query and return results from all indices
fn execute_search(client: &RegistryClient, query: &SearchQuery) -> SearchOutput {
    const PER_INDEX_LIMIT: usize = 50;
    const MERGED_LIMIT: usize = 100;

    let query_text = query.text.trim();

    if query_text.is_empty() {
        return SearchOutput {
            trigram: Vec::new(),
            word: Vec::new(),
            semantic: Vec::new(),
            merged: Vec::new(),
            scoring: HashMap::new(),
        };
    }

    let parsed = ParsedQuery::parse(query_text);

    // Run all three searches
    let trigram = client
        .search_trigram_hits(&parsed, PER_INDEX_LIMIT)
        .unwrap_or_default();
    let word = client
        .search_words_hits(&parsed, PER_INDEX_LIMIT)
        .unwrap_or_default();
    let semantic = embeddings::get_query_embedding(query_text)
        .and_then(|emb| client.search_semantic_hits(&emb, PER_INDEX_LIMIT))
        .unwrap_or_default();

    let mut scoring: HashMap<String, PartScoring> = HashMap::new();

    for (i, hit) in trigram.iter().enumerate() {
        let entry = scoring.entry(hit.url.clone()).or_default();
        entry.trigram_position = Some(i);
        entry.trigram_rank = hit.rank;
    }

    for (i, hit) in word.iter().enumerate() {
        let entry = scoring.entry(hit.url.clone()).or_default();
        entry.word_position = Some(i);
        entry.word_rank = hit.rank;
    }

    for (i, hit) in semantic.iter().enumerate() {
        let entry = scoring.entry(hit.url.clone()).or_default();
        entry.semantic_position = Some(i);
        entry.semantic_rank = hit.rank;
    }

    let merged = merge_results_rrf(&trigram, &word, &semantic, MERGED_LIMIT);

    SearchOutput {
        trigram,
        word,
        semantic,
        merged,
        scoring,
    }
}

/// Merge results using Reciprocal Rank Fusion (RRF)
///
/// Standard RRF with equal weights - robust to both MPN and descriptive queries
/// without needing query-type classification. Documents appearing in multiple
/// rankers naturally bubble up (implicit consensus effect).
fn merge_results_rrf(
    trigram: &[SearchHit],
    word: &[SearchHit],
    semantic: &[SearchHit],
    limit: usize,
) -> Vec<MergedHit> {
    // Equal weights - no query-type heuristics needed
    // RRF naturally handles both exact matches (strong in trigram) and
    // descriptive queries (strong in word/semantic)
    const W_TRIGRAM: f64 = 1.0;
    const W_WORD: f64 = 1.0;
    const W_SEMANTIC: f64 = 1.0;

    // K=10: gives meaningful score differences across ranks 1-20
    const K: f64 = 10.0;

    // Calculate RRF scores: score(d) = Σ w_i / (k + rank_i)
    let mut rrf_scores: HashMap<String, f64> = HashMap::new();

    for (i, hit) in trigram.iter().enumerate() {
        let score = W_TRIGRAM / (K + (i + 1) as f64);
        *rrf_scores.entry(hit.url.clone()).or_default() += score;
    }

    for (i, hit) in word.iter().enumerate() {
        let score = W_WORD / (K + (i + 1) as f64);
        *rrf_scores.entry(hit.url.clone()).or_default() += score;
    }

    for (i, hit) in semantic.iter().enumerate() {
        let score = W_SEMANTIC / (K + (i + 1) as f64);
        *rrf_scores.entry(hit.url.clone()).or_default() += score;
    }

    // Collect unique hits
    let mut all_hits: HashMap<String, MergedHit> = HashMap::new();
    for hit in trigram.iter().chain(word.iter()).chain(semantic.iter()) {
        all_hits
            .entry(hit.url.clone())
            .or_insert_with(|| MergedHit {
                id: hit.id,
                url: hit.url.clone(),
                mpn: hit.mpn.clone(),
                manufacturer: hit.manufacturer.clone(),
                short_description: hit.short_description.clone(),
                version: hit.version.clone(),
                package_category: hit.package_category.clone(),
            });
    }

    // Sort by RRF score descending
    let mut scored_hits: Vec<_> = all_hits
        .into_iter()
        .map(|(path, hit)| (rrf_scores.get(&path).copied().unwrap_or(0.0), hit))
        .collect();
    scored_hits.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    scored_hits
        .into_iter()
        .take(limit)
        .map(|(_, h)| h)
        .collect()
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
