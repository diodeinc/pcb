//! Background search worker thread

use crate::download::{
    download_registry_index_with_progress, fetch_registry_index_metadata, load_local_version,
    save_local_version, DownloadProgress,
};
use crate::{ParsedQuery, RegistryClient, RegistryPart};
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
}

/// Results from the worker thread
#[derive(Debug, Clone)]
pub struct SearchResults {
    pub query_id: u64,
    pub trigram: Vec<RegistryPart>,
    pub word: Vec<RegistryPart>,
    pub merged: Vec<RegistryPart>,
    pub scoring: HashMap<String, PartScoring>,
    pub duration: Duration,
}

impl Default for SearchResults {
    fn default() -> Self {
        Self {
            query_id: 0,
            trigram: Vec::new(),
            word: Vec::new(),
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
                // Fetch remote metadata (best-effort, don't fail if can't reach server)
                let meta = match fetch_registry_index_metadata() {
                    Ok(m) => m,
                    Err(_) => return, // Can't check, just use existing DB
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
                merged: results.merged,
                scoring: results.scoring,
                duration,
            });
        }
    })
}

struct SearchOutput {
    trigram: Vec<RegistryPart>,
    word: Vec<RegistryPart>,
    merged: Vec<RegistryPart>,
    scoring: HashMap<String, PartScoring>,
}

fn execute_search(client: &RegistryClient, query: &SearchQuery) -> SearchOutput {
    let query_text = query.text.trim();
    if query_text.is_empty() {
        return SearchOutput {
            trigram: Vec::new(),
            word: Vec::new(),
            merged: Vec::new(),
            scoring: HashMap::new(),
        };
    }

    let parsed = ParsedQuery::parse(&query.text);
    let limit = 50;

    let trigram = client
        .search_trigram_raw(&parsed, limit)
        .unwrap_or_default();
    let word = client.search_words_raw(&parsed, limit).unwrap_or_default();

    let mut scoring: HashMap<String, PartScoring> = HashMap::new();

    for (i, part) in trigram.iter().enumerate() {
        let entry = scoring.entry(part.registry_path.clone()).or_default();
        entry.trigram_position = Some(i);
        entry.trigram_rank = part.rank;
    }

    for (i, part) in word.iter().enumerate() {
        let entry = scoring.entry(part.registry_path.clone()).or_default();
        entry.word_position = Some(i);
        entry.word_rank = part.rank;
    }

    let merged = merge_results(&trigram, &word, limit);

    SearchOutput {
        trigram,
        word,
        merged,
        scoring,
    }
}

fn merge_results(
    trigram: &[RegistryPart],
    word: &[RegistryPart],
    limit: usize,
) -> Vec<RegistryPart> {
    use std::collections::HashSet;
    let mut seen = HashSet::new();
    let mut merged = Vec::new();

    let mut t_iter = trigram.iter();
    let mut w_iter = word.iter();

    loop {
        if merged.len() >= limit {
            break;
        }

        let mut added = false;

        if let Some(part) = t_iter.next() {
            if seen.insert(part.registry_path.clone()) {
                merged.push(part.clone());
                added = true;
            }
        }

        if merged.len() >= limit {
            break;
        }

        if let Some(part) = w_iter.next() {
            if seen.insert(part.registry_path.clone()) {
                merged.push(part.clone());
                added = true;
            }
        }

        if !added {
            break;
        }
    }

    merged
}
