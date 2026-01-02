//! Background search worker thread

use super::super::download::{
    download_registry_index_with_progress, fetch_registry_index_metadata, load_local_version,
    save_local_version, DownloadProgress,
};
use super::super::embeddings;
use crate::{ParsedQuery, RegistryClient, SearchHit};
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Query sent to the worker thread
#[derive(Debug, Clone)]
pub struct SearchQuery {
    pub id: u64,
    pub text: String,
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
    pub registry_path: String,
    pub mpn: String,
    pub manufacturer: Option<String>,
    pub short_description: Option<String>,
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

/// Spawn the search worker thread
pub fn spawn_worker(
    query_rx: Receiver<SearchQuery>,
    result_tx: Sender<SearchResults>,
    download_tx: Sender<DownloadProgress>,
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
            if let Err(e) = download_registry_index_with_progress(&db_path, &download_tx, false) {
                let _ = download_tx.send(DownloadProgress {
                    pct: None,
                    done: true,
                    error: Some(e.to_string()),
                    is_update: false,
                });
                return;
            }
        } else {
            // DB exists, signal ready immediately
            let _ = download_tx.send(DownloadProgress {
                pct: Some(100),
                done: true,
                error: None,
                is_update: false,
            });
        }

        // Open initial client
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

        // Create reload channel for background updater to signal when new DB is ready
        let (reload_tx, reload_rx) = std::sync::mpsc::channel::<()>();

        // Spawn background updater thread to check for staleness
        {
            let db_path = db_path.clone();
            let download_tx = download_tx.clone();
            thread::spawn(move || {
                // Fetch remote metadata
                let meta = match fetch_registry_index_metadata() {
                    Ok(m) => m,
                    Err(e) => {
                        // Surface the error so user knows why update check failed
                        let _ = download_tx.send(DownloadProgress {
                            pct: None,
                            done: true,
                            error: Some(e.to_string()),
                            is_update: true,
                        });
                        return;
                    }
                };

                let remote_version = &meta.sha256;
                let local_version = load_local_version(&db_path);

                if local_version.as_deref() == Some(remote_version.as_str()) {
                    return; // Up-to-date, nothing to do
                }

                // Stale: download new index in background
                if let Err(e) = download_registry_index_with_progress(&db_path, &download_tx, true)
                {
                    let _ = download_tx.send(DownloadProgress {
                        pct: None,
                        done: true,
                        error: Some(format!("Update failed: {}", e)),
                        is_update: true,
                    });
                    return;
                }

                // Save version is already done inside download_registry_index_with_progress
                // Just need to save it for first-time downloads that didn't have version file
                let _ = save_local_version(&db_path, remote_version);

                // Notify worker to reload DB
                let _ = reload_tx.send(());
            });
        }

        // Main search loop
        while let Ok(query) = query_rx.recv() {
            // Check for pending reload (non-blocking)
            if reload_rx.try_recv().is_ok() {
                if let Ok(new_client) = RegistryClient::open_path(&db_path) {
                    client = new_client;
                }
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

fn execute_search(client: &RegistryClient, query: &SearchQuery) -> SearchOutput {
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

    let parsed = ParsedQuery::parse(&query.text);
    const PER_INDEX_LIMIT: usize = 20;
    const MERGED_LIMIT: usize = 50;

    // Run all three rankers - each returns up to PER_INDEX_LIMIT results
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
        let entry = scoring.entry(hit.registry_path.clone()).or_default();
        entry.trigram_position = Some(i);
        entry.trigram_rank = hit.rank;
    }

    for (i, hit) in word.iter().enumerate() {
        let entry = scoring.entry(hit.registry_path.clone()).or_default();
        entry.word_position = Some(i);
        entry.word_rank = hit.rank;
    }

    for (i, hit) in semantic.iter().enumerate() {
        let entry = scoring.entry(hit.registry_path.clone()).or_default();
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
        *rrf_scores.entry(hit.registry_path.clone()).or_default() += score;
    }

    for (i, hit) in word.iter().enumerate() {
        let score = W_WORD / (K + (i + 1) as f64);
        *rrf_scores.entry(hit.registry_path.clone()).or_default() += score;
    }

    for (i, hit) in semantic.iter().enumerate() {
        let score = W_SEMANTIC / (K + (i + 1) as f64);
        *rrf_scores.entry(hit.registry_path.clone()).or_default() += score;
    }

    // Collect unique hits
    let mut all_hits: HashMap<String, MergedHit> = HashMap::new();
    for hit in trigram.iter().chain(word.iter()).chain(semantic.iter()) {
        all_hits
            .entry(hit.registry_path.clone())
            .or_insert_with(|| MergedHit {
                id: hit.id,
                registry_path: hit.registry_path.clone(),
                mpn: hit.mpn.clone(),
                manufacturer: hit.manufacturer.clone(),
                short_description: hit.short_description.clone(),
            });
    }

    // Sort by RRF score descending
    let mut scored_hits: Vec<_> = all_hits
        .into_iter()
        .map(|(path, hit)| {
            let rrf = rrf_scores.get(&path).copied().unwrap_or(0.0);
            (rrf, hit)
        })
        .collect();

    scored_hits.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    scored_hits
        .into_iter()
        .take(limit)
        .map(|(_, hit)| hit)
        .collect()
}
