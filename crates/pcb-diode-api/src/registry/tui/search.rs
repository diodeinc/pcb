//! Background search and detail worker threads

use super::super::download::{
    DownloadProgress, DownloadSource, RegistryIndexMetadata, download_registry_index_with_progress,
    fetch_registry_index_metadata, load_local_version, save_local_version,
};
use crate::bom::ComponentKey;
use crate::kicad_symbols::KicadSymbol;
use crate::kicad_symbols::download::{
    KicadSymbolsIndexMetadata, download_kicad_symbols_index_with_progress,
    fetch_kicad_symbols_index_metadata, load_local_version as load_local_kicad_symbols_version,
    save_local_version as save_local_kicad_symbols_version,
};
use crate::{
    KicadSymbolsClient, ModuleRelations, RegistryClient, RegistryModule, RegistryModuleHit,
    RegistrySymbol, RegistrySymbolHit, SearchHit,
};
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use super::app::SearchMode;

/// Query sent to the worker thread
#[derive(Debug, Clone)]
pub struct SearchQuery {
    pub id: u64,
    pub text: String,
    pub mode: SearchMode,
    /// If true, force a registry index update check
    pub force_update: bool,
}

/// Scoring details for one search result across index strategies.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct SearchScoring {
    pub trigram_position: Option<usize>,
    pub trigram_rank: Option<f64>,
    pub word_position: Option<usize>,
    pub word_rank: Option<f64>,
    pub docs_full_text_position: Option<usize>,
    pub docs_full_text_rank: Option<f64>,
    pub semantic_position: Option<usize>,
    pub semantic_rank: Option<f64>,
}

/// Results from the worker thread
#[derive(Debug, Clone, Default)]
pub enum SearchResults {
    #[default]
    Empty,
    RegistryModules(RegistryModuleSearchResults),
    RegistrySymbols(RegistrySymbolSearchResults),
    KicadSymbols(KicadSearchResults),
}

#[derive(Debug, Clone)]
pub struct RegistryModuleSearchResults {
    pub query_id: u64,
    pub trigram: Vec<RegistryModuleHit>,
    pub word: Vec<RegistryModuleHit>,
    pub docs_full_text: Vec<RegistryModuleHit>,
    pub semantic: Vec<RegistryModuleHit>,
    pub merged: Vec<RegistryModuleHit>,
    pub scoring: HashMap<String, SearchScoring>,
    pub duration: Duration,
}

#[derive(Debug, Clone)]
pub struct RegistrySymbolSearchResults {
    pub query_id: u64,
    pub trigram: Vec<RegistrySymbolHit>,
    pub word: Vec<RegistrySymbolHit>,
    pub docs_full_text: Vec<RegistrySymbolHit>,
    pub semantic: Vec<RegistrySymbolHit>,
    pub merged: Vec<RegistrySymbolHit>,
    pub scoring: HashMap<String, SearchScoring>,
    pub duration: Duration,
}

#[derive(Debug, Clone)]
pub struct KicadSearchResults {
    pub query_id: u64,
    pub trigram: Vec<SearchHit>,
    pub word: Vec<SearchHit>,
    pub docs_full_text: Vec<SearchHit>,
    pub semantic: Vec<SearchHit>,
    pub merged: Vec<SearchHit>,
    pub scoring: HashMap<String, SearchScoring>,
    pub duration: Duration,
}

impl SearchResults {
    pub fn query_id(&self) -> u64 {
        match self {
            SearchResults::Empty => 0,
            SearchResults::RegistryModules(results) => results.query_id,
            SearchResults::RegistrySymbols(results) => results.query_id,
            SearchResults::KicadSymbols(results) => results.query_id,
        }
    }

    pub fn len(&self) -> usize {
        match self {
            SearchResults::Empty => 0,
            SearchResults::RegistryModules(results) => results.merged.len(),
            SearchResults::RegistrySymbols(results) => results.merged.len(),
            SearchResults::KicadSymbols(results) => results.merged.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn duration(&self) -> Duration {
        match self {
            SearchResults::Empty => Duration::ZERO,
            SearchResults::RegistryModules(results) => results.duration,
            SearchResults::RegistrySymbols(results) => results.duration,
            SearchResults::KicadSymbols(results) => results.duration,
        }
    }

    pub fn scoring(&self) -> &HashMap<String, SearchScoring> {
        static EMPTY: OnceLock<HashMap<String, SearchScoring>> = OnceLock::new();
        match self {
            SearchResults::Empty => EMPTY.get_or_init(HashMap::new),
            SearchResults::RegistryModules(results) => &results.scoring,
            SearchResults::RegistrySymbols(results) => &results.scoring,
            SearchResults::KicadSymbols(results) => &results.scoring,
        }
    }

    pub fn availability_lookups_at(&self, idx: usize) -> Option<Vec<ComponentKey>> {
        match self {
            SearchResults::RegistrySymbols(results) => results
                .merged
                .get(idx)
                .map(|hit| hit.availability_lookups.clone()),
            SearchResults::KicadSymbols(results) => results
                .merged
                .get(idx)
                .map(|hit| hit.availability_lookups.clone()),
            SearchResults::Empty | SearchResults::RegistryModules(_) => None,
        }
    }

    pub fn selected_item_id(&self, idx: usize) -> Option<i64> {
        match self {
            SearchResults::Empty => None,
            SearchResults::RegistryModules(results) => results.merged.get(idx).map(|hit| hit.id),
            SearchResults::RegistrySymbols(results) => results.merged.get(idx).map(|hit| hit.id),
            SearchResults::KicadSymbols(results) => results.merged.get(idx).map(|hit| hit.id),
        }
    }

    pub fn selected_url(&self, idx: usize) -> Option<&str> {
        match self {
            SearchResults::Empty => None,
            SearchResults::RegistryModules(results) => {
                results.merged.get(idx).map(|hit| hit.url.as_str())
            }
            SearchResults::RegistrySymbols(results) => {
                results.merged.get(idx).map(|hit| hit.url.as_str())
            }
            SearchResults::KicadSymbols(results) => {
                results.merged.get(idx).map(|hit| hit.url.as_str())
            }
        }
    }
}

impl Default for KicadSearchResults {
    fn default() -> Self {
        Self {
            query_id: 0,
            trigram: Vec::new(),
            word: Vec::new(),
            docs_full_text: Vec::new(),
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

pub struct SearchWorkerOptions {
    pub registry_enabled: bool,
    pub kicad_enabled: bool,
    pub prefetched_registry_metadata: Option<RegistryIndexMetadata>,
    pub prefetched_kicad_metadata: Option<KicadSymbolsIndexMetadata>,
    pub registry_db_path_override: Option<std::path::PathBuf>,
}

/// Request to fetch details for a specific local-index item.
#[derive(Debug)]
pub struct DetailRequest {
    pub item_id: i64,
    pub mode: SearchMode,
}

/// Response with full local-index item details.
#[derive(Debug)]
pub struct DetailResponse {
    pub item_id: i64,
    pub mode: SearchMode,
    pub module: Option<RegistryModule>,
    pub symbol: Option<RegistrySymbol>,
    pub kicad_symbol: Option<KicadSymbol>,
    pub relations: ModuleRelations,
}

/// Spawn the detail worker thread (fetches full part details on demand)
pub fn spawn_detail_worker(
    req_rx: Receiver<DetailRequest>,
    resp_tx: Sender<DetailResponse>,
    registry_db_path_override: Option<std::path::PathBuf>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let registry_db_path = match registry_db_path_override {
            Some(path) => path,
            None => match RegistryClient::default_db_path() {
                Ok(path) => path,
                Err(_) => return,
            },
        };
        let kicad_db_path = match KicadSymbolsClient::default_db_path() {
            Ok(path) => path,
            Err(_) => return,
        };

        let mut registry_client: Option<RegistryClient> = None;
        let mut registry_mtime: Option<SystemTime> = None;
        let mut kicad_client: Option<KicadSymbolsClient> = None;
        let mut kicad_mtime: Option<SystemTime> = None;

        while let Ok(mut req) = req_rx.recv() {
            while let Ok(next) = req_rx.try_recv() {
                req = next;
            }

            match req.mode {
                SearchMode::RegistryModules | SearchMode::RegistryComponents => {
                    let current_mtime = get_file_mtime(&registry_db_path);
                    if (registry_client.is_none() || current_mtime != registry_mtime)
                        && let Ok(new_client) = RegistryClient::open_path(&registry_db_path)
                    {
                        registry_client = Some(new_client);
                        registry_mtime = current_mtime;
                    }

                    let Some(ref client) = registry_client else {
                        let _ = resp_tx.send(DetailResponse {
                            item_id: req.item_id,
                            mode: req.mode,
                            module: None,
                            symbol: None,
                            kicad_symbol: None,
                            relations: ModuleRelations::default(),
                        });
                        continue;
                    };

                    let (module, symbol, relations) = match req.mode {
                        SearchMode::RegistryModules => {
                            let module = client.get_module_by_id(req.item_id).ok().flatten();
                            let relations = if module.is_some() {
                                client.get_module_relations(req.item_id).unwrap_or_default()
                            } else {
                                ModuleRelations::default()
                            };
                            (module, None, relations)
                        }
                        SearchMode::RegistryComponents => {
                            let symbol = client.get_symbol_by_id(req.item_id).ok().flatten();
                            (None, symbol, ModuleRelations::default())
                        }
                        _ => unreachable!(),
                    };

                    let _ = resp_tx.send(DetailResponse {
                        item_id: req.item_id,
                        mode: req.mode,
                        module,
                        symbol,
                        kicad_symbol: None,
                        relations,
                    });
                }
                SearchMode::KicadSymbols => {
                    let current_mtime = get_file_mtime(&kicad_db_path);
                    if (kicad_client.is_none() || current_mtime != kicad_mtime)
                        && let Ok(new_client) = KicadSymbolsClient::open_path(&kicad_db_path)
                    {
                        kicad_client = Some(new_client);
                        kicad_mtime = current_mtime;
                    }

                    let symbol = kicad_client
                        .as_ref()
                        .and_then(|client| client.get_symbol_by_id(req.item_id).ok().flatten());

                    let _ = resp_tx.send(DetailResponse {
                        item_id: req.item_id,
                        mode: req.mode,
                        module: None,
                        symbol: None,
                        kicad_symbol: symbol,
                        relations: ModuleRelations::default(),
                    });
                }
                SearchMode::WebComponents => {}
            }
        }
    })
}

/// Get file modification time, returns None on error
fn get_file_mtime(path: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

fn index_update_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn download_kicad_symbols_index_with_app_progress(
    dest_path: &std::path::Path,
    download_tx: &Sender<DownloadProgress>,
    is_update: bool,
    prefetched_metadata: Option<&KicadSymbolsIndexMetadata>,
) -> anyhow::Result<()> {
    download_kicad_symbols_index_with_progress(
        dest_path,
        download_tx,
        is_update,
        prefetched_metadata,
    )
}

fn ensure_local_index_present<Meta>(
    db_path: &std::path::Path,
    download_tx: &Sender<DownloadProgress>,
    prefetched_metadata: Option<&Meta>,
    download_with_progress: impl FnOnce(
        &std::path::Path,
        &Sender<DownloadProgress>,
        Option<&Meta>,
    ) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    if !db_path.exists() {
        return download_with_progress(db_path, download_tx, prefetched_metadata);
    }

    Ok(())
}

fn spawn_registry_update_check(
    db_path: std::path::PathBuf,
    download_tx: Sender<DownloadProgress>,
    force: bool,
) {
    thread::spawn(move || {
        let _lock = index_update_lock().lock().unwrap();
        let meta = match fetch_registry_index_metadata() {
            Ok(metadata) => metadata,
            Err(err) => {
                if force {
                    let _ = download_tx.send(DownloadProgress {
                        source: DownloadSource::Registry,
                        pct: None,
                        done: true,
                        error: Some(err.to_string()),
                        is_update: true,
                    });
                }
                return;
            }
        };

        let remote_version = &meta.sha256;
        let local_version = load_local_version(&db_path);
        if !force && local_version.as_deref() == Some(remote_version.as_str()) {
            return;
        }

        if let Err(err) =
            download_registry_index_with_progress(&db_path, &download_tx, true, Some(&meta))
        {
            let _ = download_tx.send(DownloadProgress {
                source: DownloadSource::Registry,
                pct: None,
                done: true,
                error: Some(format!("Update failed: {}", err)),
                is_update: true,
            });
            return;
        }

        let _ = save_local_version(&db_path, remote_version);
    });
}

fn spawn_kicad_update_check(db_path: std::path::PathBuf, download_tx: Sender<DownloadProgress>) {
    thread::spawn(move || {
        let _lock = index_update_lock().lock().unwrap();
        let meta = match fetch_kicad_symbols_index_metadata() {
            Ok(metadata) => metadata,
            Err(_) => return,
        };

        let remote_version = match meta.version_token() {
            Ok(version) => version,
            Err(_) => return,
        };
        let local_version = load_local_kicad_symbols_version(&db_path);
        if local_version.as_deref() == Some(remote_version.as_str()) {
            return;
        }

        if let Err(err) = download_kicad_symbols_index_with_app_progress(
            &db_path,
            &download_tx,
            true,
            Some(&meta),
        ) {
            let _ = download_tx.send(DownloadProgress {
                source: DownloadSource::KicadSymbols,
                pct: None,
                done: true,
                error: Some(format!("Update failed: {}", err)),
                is_update: true,
            });
            return;
        }

        let _ = save_local_kicad_symbols_version(&db_path, &remote_version);
    });
}

pub(crate) trait ScoredHit {
    fn url(&self) -> &str;
    fn rank(&self) -> Option<f64>;
}

impl ScoredHit for SearchHit {
    fn url(&self) -> &str {
        &self.url
    }

    fn rank(&self) -> Option<f64> {
        self.rank
    }
}

impl ScoredHit for RegistryModuleHit {
    fn url(&self) -> &str {
        &self.url
    }

    fn rank(&self) -> Option<f64> {
        self.rank
    }
}

impl ScoredHit for RegistrySymbolHit {
    fn url(&self) -> &str {
        &self.url
    }

    fn rank(&self) -> Option<f64> {
        self.rank
    }
}

pub(crate) fn build_scoring<T: ScoredHit>(
    trigram: &[T],
    word: &[T],
    docs_full_text: &[T],
    semantic: &[T],
) -> HashMap<String, SearchScoring> {
    let mut scoring = HashMap::new();
    record_scores(&mut scoring, trigram, |entry, idx, rank| {
        entry.trigram_position = Some(idx);
        entry.trigram_rank = rank;
    });
    record_scores(&mut scoring, word, |entry, idx, rank| {
        entry.word_position = Some(idx);
        entry.word_rank = rank;
    });
    record_scores(&mut scoring, docs_full_text, |entry, idx, rank| {
        entry.docs_full_text_position = Some(idx);
        entry.docs_full_text_rank = rank;
    });
    record_scores(&mut scoring, semantic, |entry, idx, rank| {
        entry.semantic_position = Some(idx);
        entry.semantic_rank = rank;
    });
    scoring
}

fn record_scores<T: ScoredHit>(
    scoring: &mut HashMap<String, SearchScoring>,
    hits: &[T],
    mut update: impl FnMut(&mut SearchScoring, usize, Option<f64>),
) {
    for (idx, hit) in hits.iter().enumerate() {
        let entry = scoring.entry(hit.url().to_string()).or_default();
        update(entry, idx, hit.rank());
    }
}

/// Spawn the search worker thread
///
/// If `prefetched_metadata` is provided, it will be used for the initial download
/// to avoid a duplicate API request.
pub fn spawn_worker(
    query_rx: Receiver<SearchQuery>,
    result_tx: Sender<SearchResults>,
    download_tx: Sender<DownloadProgress>,
    options: SearchWorkerOptions,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let SearchWorkerOptions {
            registry_enabled,
            kicad_enabled,
            mut prefetched_registry_metadata,
            mut prefetched_kicad_metadata,
            registry_db_path_override,
        } = options;
        let registry_index_override = registry_db_path_override.is_some();
        let registry_db_path = match registry_db_path_override {
            Some(path) => path,
            None => match RegistryClient::default_db_path() {
                Ok(p) => p,
                Err(e) => {
                    let _ = download_tx.send(DownloadProgress {
                        source: DownloadSource::Registry,
                        pct: None,
                        done: true,
                        error: Some(format!("Failed to get registry db path: {}", e)),
                        is_update: false,
                    });
                    return;
                }
            },
        };
        let kicad_db_path = match KicadSymbolsClient::default_db_path() {
            Ok(p) => p,
            Err(e) => {
                let _ = download_tx.send(DownloadProgress {
                    source: DownloadSource::KicadSymbols,
                    pct: None,
                    done: true,
                    error: Some(format!("Failed to get KiCad symbols db path: {}", e)),
                    is_update: false,
                });
                return;
            }
        };

        let mut registry_client: Option<RegistryClient> = None;
        let mut registry_mtime = None;
        let mut kicad_client: Option<KicadSymbolsClient> = None;
        let mut kicad_mtime = None;
        let mut registry_ready = false;
        let mut kicad_ready = false;
        let mut registry_update_pending = false;
        let mut kicad_update_started = false;

        if registry_enabled {
            let registry_present = if registry_index_override {
                registry_db_path.exists()
            } else {
                ensure_local_index_present(
                    &registry_db_path,
                    &download_tx,
                    prefetched_registry_metadata.as_ref(),
                    |path, tx, metadata| {
                        download_registry_index_with_progress(path, tx, false, metadata)
                    },
                )
                .is_ok()
            };

            if registry_present && let Ok(client) = RegistryClient::open_path(&registry_db_path) {
                registry_client = Some(client);
                registry_mtime = get_file_mtime(&registry_db_path);
                registry_ready = true;
                prefetched_registry_metadata = None;
            }
        }

        if kicad_enabled
            && ensure_local_index_present(
                &kicad_db_path,
                &download_tx,
                prefetched_kicad_metadata.as_ref(),
                |path, tx, metadata| {
                    download_kicad_symbols_index_with_app_progress(path, tx, false, metadata)
                },
            )
            .is_ok()
            && let Ok(client) = KicadSymbolsClient::open_path(&kicad_db_path)
        {
            kicad_client = Some(client);
            kicad_mtime = get_file_mtime(&kicad_db_path);
            kicad_ready = true;
            prefetched_kicad_metadata = None;
        }

        if registry_ready && !registry_index_override {
            registry_update_pending = true;
            spawn_registry_update_check(registry_db_path.clone(), download_tx.clone(), false);
        }

        if kicad_ready {
            kicad_update_started = true;
            spawn_kicad_update_check(kicad_db_path.clone(), download_tx.clone());
        }

        while let Ok(mut query) = query_rx.recv() {
            while let Ok(next) = query_rx.try_recv() {
                query = next;
            }

            match query.mode {
                SearchMode::RegistryModules | SearchMode::RegistryComponents => {
                    if !registry_ready {
                        let registry_present = if registry_index_override {
                            registry_db_path.exists()
                        } else {
                            ensure_local_index_present(
                                &registry_db_path,
                                &download_tx,
                                prefetched_registry_metadata.as_ref(),
                                |path, tx, metadata| {
                                    download_registry_index_with_progress(path, tx, false, metadata)
                                },
                            )
                            .is_ok()
                        };
                        if !registry_present {
                            continue;
                        }
                        registry_client = match RegistryClient::open_path(&registry_db_path) {
                            Ok(client) => Some(client),
                            Err(err) => {
                                let _ = download_tx.send(DownloadProgress {
                                    source: DownloadSource::Registry,
                                    pct: None,
                                    done: true,
                                    error: Some(format!("Failed to open registry: {}", err)),
                                    is_update: false,
                                });
                                continue;
                            }
                        };
                        registry_mtime = get_file_mtime(&registry_db_path);
                        registry_ready = true;
                        prefetched_registry_metadata = None;

                        if !registry_index_override && !registry_update_pending {
                            registry_update_pending = true;
                            spawn_registry_update_check(
                                registry_db_path.clone(),
                                download_tx.clone(),
                                false,
                            );
                        }
                    }

                    let current_mtime = get_file_mtime(&registry_db_path);
                    if current_mtime != registry_mtime
                        && let Ok(new_client) = RegistryClient::open_path(&registry_db_path)
                    {
                        registry_client = Some(new_client);
                        registry_mtime = current_mtime;
                        registry_update_pending = false;
                    }

                    if query.force_update && registry_index_override {
                        let _ = download_tx.send(DownloadProgress {
                            source: DownloadSource::Registry,
                            pct: None,
                            done: true,
                            error: Some(
                                "Registry updates are disabled when --registry-index is set"
                                    .to_string(),
                            ),
                            is_update: true,
                        });
                    } else if query.force_update && !registry_update_pending {
                        registry_update_pending = true;
                        spawn_registry_update_check(
                            registry_db_path.clone(),
                            download_tx.clone(),
                            true,
                        );
                    }

                    let Some(client) = registry_client.as_ref() else {
                        continue;
                    };
                    let start = Instant::now();
                    match query.mode {
                        SearchMode::RegistryModules => {
                            let rrf = client.search_modules_rrf(&query.text);
                            let duration = start.elapsed();
                            let scoring = build_scoring(
                                &rrf.trigram,
                                &rrf.word,
                                &rrf.docs_full_text,
                                &rrf.semantic,
                            );
                            let _ = result_tx.send(SearchResults::RegistryModules(
                                RegistryModuleSearchResults {
                                    query_id: query.id,
                                    trigram: rrf.trigram,
                                    word: rrf.word,
                                    docs_full_text: rrf.docs_full_text,
                                    semantic: rrf.semantic,
                                    merged: rrf.merged,
                                    scoring,
                                    duration,
                                },
                            ));
                        }
                        SearchMode::RegistryComponents => {
                            let rrf = client.search_symbols_rrf(&query.text);
                            let duration = start.elapsed();
                            let scoring = build_scoring(
                                &rrf.trigram,
                                &rrf.word,
                                &rrf.docs_full_text,
                                &rrf.semantic,
                            );
                            let _ = result_tx.send(SearchResults::RegistrySymbols(
                                RegistrySymbolSearchResults {
                                    query_id: query.id,
                                    trigram: rrf.trigram,
                                    word: rrf.word,
                                    docs_full_text: rrf.docs_full_text,
                                    semantic: rrf.semantic,
                                    merged: rrf.merged,
                                    scoring,
                                    duration,
                                },
                            ));
                        }
                        _ => {}
                    }
                }
                SearchMode::KicadSymbols => {
                    if !kicad_ready {
                        if ensure_local_index_present(
                            &kicad_db_path,
                            &download_tx,
                            prefetched_kicad_metadata.as_ref(),
                            |path, tx, metadata| {
                                download_kicad_symbols_index_with_app_progress(
                                    path, tx, false, metadata,
                                )
                            },
                        )
                        .is_err()
                        {
                            continue;
                        }
                        kicad_client = match KicadSymbolsClient::open_path(&kicad_db_path) {
                            Ok(client) => Some(client),
                            Err(err) => {
                                let _ = download_tx.send(DownloadProgress {
                                    source: DownloadSource::KicadSymbols,
                                    pct: None,
                                    done: true,
                                    error: Some(format!(
                                        "Failed to open KiCad symbols index: {}",
                                        err
                                    )),
                                    is_update: false,
                                });
                                continue;
                            }
                        };
                        kicad_mtime = get_file_mtime(&kicad_db_path);
                        kicad_ready = true;
                        prefetched_kicad_metadata = None;

                        if !kicad_update_started {
                            kicad_update_started = true;
                            spawn_kicad_update_check(kicad_db_path.clone(), download_tx.clone());
                        }
                    }

                    let current_mtime = get_file_mtime(&kicad_db_path);
                    if current_mtime != kicad_mtime
                        && let Ok(new_client) = KicadSymbolsClient::open_path(&kicad_db_path)
                    {
                        kicad_client = Some(new_client);
                        kicad_mtime = current_mtime;
                    }

                    let Some(client) = kicad_client.as_ref() else {
                        continue;
                    };
                    let start = Instant::now();
                    let mut rrf = client.search_rrf(&query.text);
                    let duration = start.elapsed();
                    let _ = client.populate_availability_lookups(&mut rrf.merged);
                    let scoring =
                        build_scoring(&rrf.trigram, &rrf.word, &rrf.docs_full_text, &rrf.semantic);

                    let _ = result_tx.send(SearchResults::KicadSymbols(KicadSearchResults {
                        query_id: query.id,
                        trigram: rrf.trigram,
                        word: rrf.word,
                        docs_full_text: rrf.docs_full_text,
                        semantic: rrf.semantic,
                        merged: rrf.merged,
                        scoring,
                        duration,
                    }));
                }
                SearchMode::WebComponents => {}
            }
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

const AVAILABILITY_WORKER_CHUNK_SIZE: usize = 10;

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub enum AvailabilityKey {
    Component(ComponentKey),
    KicadSymbol(i64),
}

#[derive(Debug, Clone)]
pub struct AvailabilityRequest {
    pub key: AvailabilityKey,
    pub lookups: Vec<ComponentKey>,
}

/// Batch availability request for the current ordered set of missing lookup keys.
pub type PricingRequest = Vec<AvailabilityRequest>;

/// Outcome for a single pricing lookup key.
#[derive(Debug, Clone)]
pub enum PricingResult {
    Ready(Box<pcb_sch::bom::Availability>),
    Empty,
    Failed,
}

/// Chunk of resolved pricing lookup keys.
pub type PricingResponse = Vec<(AvailabilityKey, PricingResult)>;

/// Spawn a worker thread that fetches availability for components in batches
pub fn spawn_availability_worker(
    req_rx: Receiver<PricingRequest>,
    resp_tx: Sender<PricingResponse>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut auth_token: Option<String> = None;
        while let Ok(mut queue) = req_rx.recv() {
            while let Ok(next) = req_rx.try_recv() {
                queue = next;
            }

            while !queue.is_empty() {
                let chunk_len = queue.len().min(AVAILABILITY_WORKER_CHUNK_SIZE);
                let chunk: Vec<_> = queue.drain(..chunk_len).collect();

                let response = if auth_token.is_none() {
                    match crate::auth::get_valid_token() {
                        Ok(token) => {
                            auth_token = Some(token);
                            fetch_pricing_chunk(auth_token.as_deref().unwrap(), &chunk)
                        }
                        Err(e) => {
                            log::warn!("Pricing auth failed: {}", e);
                            chunk
                                .into_iter()
                                .map(|request| (request.key, PricingResult::Failed))
                                .collect()
                        }
                    }
                } else {
                    fetch_pricing_chunk(auth_token.as_deref().unwrap(), &chunk)
                };

                let _ = resp_tx.send(response);

                while let Ok(next) = req_rx.try_recv() {
                    queue = next;
                }
            }
        }
    })
}

fn fetch_pricing_chunk(auth_token: &str, chunk: &[AvailabilityRequest]) -> PricingResponse {
    let groups: Vec<_> = chunk
        .iter()
        .map(|request| request.lookups.clone())
        .collect();

    match crate::bom::fetch_pricing_grouped_batch(auth_token, &groups) {
        Ok(availability_results) => chunk
            .iter()
            .map(|request| request.key.clone())
            .zip(availability_results)
            .map(|(key, availability)| {
                let result = if crate::bom::has_search_availability(&availability) {
                    PricingResult::Ready(Box::new(availability))
                } else {
                    PricingResult::Empty
                };
                (key, result)
            })
            .collect(),
        Err(e) => {
            log::warn!("Pricing API failed: {}", e);
            chunk
                .iter()
                .map(|request| (request.key.clone(), PricingResult::Failed))
                .collect()
        }
    }
}
