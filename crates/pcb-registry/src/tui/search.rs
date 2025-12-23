//! Background search worker thread

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
    /// Position in trigram results (0-indexed), None if not found
    pub trigram_position: Option<usize>,
    /// FTS5 rank from trigram index
    pub trigram_rank: Option<f64>,
    /// Position in word results (0-indexed), None if not found
    pub word_position: Option<usize>,
    /// FTS5 rank from word index
    pub word_rank: Option<f64>,
}

/// Results from the worker thread
#[derive(Debug, Clone)]
pub struct SearchResults {
    pub query_id: u64,
    pub trigram: Vec<RegistryPart>,
    pub word: Vec<RegistryPart>,
    pub merged: Vec<RegistryPart>,
    /// Scoring details keyed by registry_path
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
                trigram: results.trigram,
                word: results.word,
                merged: results.merged,
                scoring: results.scoring,
                duration,
            });
        }
    })
}

/// Intermediate result struct
struct SearchOutput {
    trigram: Vec<RegistryPart>,
    word: Vec<RegistryPart>,
    merged: Vec<RegistryPart>,
    scoring: HashMap<String, PartScoring>,
}

/// Execute search against all indices and merge results
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

    // Build scoring map
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

    // Merge: interleave results, deduplicate by registry_path
    let merged = merge_results(&trigram, &word, limit);

    SearchOutput {
        trigram,
        word,
        merged,
        scoring,
    }
}

/// Merge results from multiple indices, deduplicating by registry_path
fn merge_results(
    trigram: &[RegistryPart],
    word: &[RegistryPart],
    limit: usize,
) -> Vec<RegistryPart> {
    use std::collections::HashSet;
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
