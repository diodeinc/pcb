//! Background search worker thread

use crate::{ParsedQuery, RegistryClient, RegistryPart};
use std::collections::HashSet;
use std::sync::mpsc::{Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Query sent to the worker thread
#[derive(Debug, Clone)]
pub struct SearchQuery {
    pub id: u64,
    pub text: String,
}

/// Results from the worker thread
#[derive(Debug, Clone)]
pub struct SearchResults {
    pub query_id: u64,
    #[allow(dead_code)]
    pub query_text: String,
    pub trigram: Vec<RegistryPart>,
    pub word: Vec<RegistryPart>,
    pub merged: Vec<RegistryPart>,
    pub duration: Duration,
}

impl Default for SearchResults {
    fn default() -> Self {
        Self {
            query_id: 0,
            query_text: String::new(),
            trigram: Vec::new(),
            word: Vec::new(),
            merged: Vec::new(),
            duration: Duration::ZERO,
        }
    }
}

/// Spawn the search worker thread
pub fn spawn_worker(
    query_rx: Receiver<SearchQuery>,
    result_tx: Sender<SearchResults>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let client = match RegistryClient::open() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("Failed to open registry: {}", e);
                return;
            }
        };

        while let Ok(query) = query_rx.recv() {
            let start = Instant::now();
            let results = execute_search(&client, &query);
            let duration = start.elapsed();

            let _ = result_tx.send(SearchResults {
                query_id: query.id,
                query_text: query.text,
                trigram: results.0,
                word: results.1,
                merged: results.2,
                duration,
            });
        }
    })
}

/// Execute search against all indices and merge results
fn execute_search(
    client: &RegistryClient,
    query: &SearchQuery,
) -> (Vec<RegistryPart>, Vec<RegistryPart>, Vec<RegistryPart>) {
    if query.text.trim().is_empty() {
        return (Vec::new(), Vec::new(), Vec::new());
    }

    let parsed = ParsedQuery::parse(&query.text);
    let limit = 50;

    let trigram = client
        .search_trigram_raw(&parsed, limit)
        .unwrap_or_default();
    let word = client.search_words_raw(&parsed, limit).unwrap_or_default();

    // Merge: interleave results, deduplicate by registry_path
    let merged = merge_results(&trigram, &word, limit);

    (trigram, word, merged)
}

/// Merge results from multiple indices, deduplicating by registry_path
fn merge_results(
    trigram: &[RegistryPart],
    word: &[RegistryPart],
    limit: usize,
) -> Vec<RegistryPart> {
    let mut seen = HashSet::new();
    let mut merged = Vec::new();

    // Interleave: take from trigram then word alternately
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
