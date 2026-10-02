//! Background search and detail worker threads

use super::super::download::{DownloadProgress, RegistrySearchScope};
use crate::bom::ComponentKey;
use crate::{
    ModuleRelations, RegistryModule, RegistryModuleHit, RegistrySearchClient, RegistrySymbol,
    RegistrySymbolHit,
};
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

/// Results from the worker thread
#[derive(Debug, Clone, Default)]
pub enum SearchResults {
    #[default]
    Empty,
    RegistryModules(HitResults<RegistryModuleHit>),
    RegistrySymbols(HitResults<RegistrySymbolHit>),
}

#[derive(Debug, Clone)]
pub struct HitResults<T> {
    pub query_id: u64,
    pub hits: Vec<T>,
    pub duration: Duration,
}

impl<T> HitResults<T> {
    fn new(query_id: u64, search: impl FnOnce() -> Vec<T>) -> Self {
        let start = Instant::now();
        let hits = search();
        Self {
            query_id,
            hits,
            duration: start.elapsed(),
        }
    }
}

impl SearchResults {
    pub fn query_id(&self) -> u64 {
        match self {
            SearchResults::Empty => 0,
            SearchResults::RegistryModules(results) => results.query_id,
            SearchResults::RegistrySymbols(results) => results.query_id,
        }
    }

    pub fn len(&self) -> usize {
        match self {
            SearchResults::Empty => 0,
            SearchResults::RegistryModules(results) => results.hits.len(),
            SearchResults::RegistrySymbols(results) => results.hits.len(),
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
        }
    }

    pub fn availability_key_at(&self, idx: usize) -> Option<ComponentKey> {
        match self {
            SearchResults::RegistrySymbols(results) => results
                .hits
                .get(idx)
                .and_then(|hit| hit.availability_key.clone()),
            SearchResults::Empty | SearchResults::RegistryModules(_) => None,
        }
    }

    /// Registry id and item id of the hit at `idx`.
    pub fn selected_item(&self, idx: usize) -> Option<(String, i64)> {
        match self {
            SearchResults::Empty => None,
            SearchResults::RegistryModules(results) => results
                .hits
                .get(idx)
                .map(|hit| (hit.registry.id.clone(), hit.id)),
            SearchResults::RegistrySymbols(results) => results
                .hits
                .get(idx)
                .map(|hit| (hit.registry.id.clone(), hit.id)),
        }
    }

    pub fn selected_url(&self, idx: usize) -> Option<&str> {
        match self {
            SearchResults::Empty => None,
            SearchResults::RegistryModules(results) => {
                results.hits.get(idx).map(|hit| hit.url.as_str())
            }
            SearchResults::RegistrySymbols(results) => {
                results.hits.get(idx).map(|hit| hit.url.as_str())
            }
        }
    }
}

/// Request to fetch details for a specific local-index item.
#[derive(Debug)]
pub struct DetailRequest {
    pub item_id: i64,
    pub registry_id: String,
    pub mode: SearchMode,
}

/// Response with full local-index item details.
#[derive(Debug)]
pub struct DetailResponse {
    pub item_id: i64,
    pub registry_id: String,
    pub mode: SearchMode,
    pub module: Option<RegistryModule>,
    pub symbol: Option<RegistrySymbol>,
    pub relations: ModuleRelations,
}

/// Spawn the detail worker thread (fetches full part details on demand)
pub fn spawn_detail_worker(
    req_rx: Receiver<DetailRequest>,
    resp_tx: Sender<DetailResponse>,
    registry_scope: RegistrySearchScope,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut registry_client: Option<RegistrySearchClient> = None;
        let mut registry_mtimes: Vec<Option<SystemTime>> = Vec::new();

        while let Ok(mut req) = req_rx.recv() {
            while let Ok(next) = req_rx.try_recv() {
                req = next;
            }

            let current_mtimes = registry_index_mtimes(&registry_scope);
            if registry_client.is_none() || current_mtimes != registry_mtimes {
                registry_client = RegistrySearchClient::open_cached_scope(&registry_scope).ok();
                registry_mtimes = current_mtimes;
            }

            let mut module = None;
            let mut symbol = None;
            let mut relations = ModuleRelations::default();
            if let Some(client) = registry_client.as_ref() {
                match req.mode {
                    SearchMode::RegistryModules => {
                        module = client
                            .get_module_by_key(&req.registry_id, req.item_id)
                            .ok()
                            .flatten();
                        if module.is_some() {
                            relations = client
                                .get_module_relations_by_key(&req.registry_id, req.item_id)
                                .unwrap_or_default();
                        }
                    }
                    SearchMode::RegistryComponents => {
                        symbol = client
                            .get_symbol_by_key(&req.registry_id, req.item_id)
                            .ok()
                            .flatten();
                    }
                }
            }

            let _ = resp_tx.send(DetailResponse {
                item_id: req.item_id,
                registry_id: req.registry_id,
                mode: req.mode,
                module,
                symbol,
                relations,
            });
        }
    })
}

/// Get file modification time, returns None on error
fn get_file_mtime(path: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

fn registry_index_mtimes(scope: &RegistrySearchScope) -> Vec<Option<SystemTime>> {
    scope
        .index_paths()
        .unwrap_or_default()
        .into_iter()
        .map(|path| get_file_mtime(&path))
        .collect()
}

fn index_update_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn spawn_registry_update_check(
    scope: RegistrySearchScope,
    download_tx: Sender<DownloadProgress>,
    force: bool,
) {
    thread::spawn(move || {
        let _lock = index_update_lock().lock().unwrap();
        if let Err(err) =
            RegistrySearchClient::open_scope_with_progress(scope, &download_tx, true, force)
        {
            let _ = download_tx.send(DownloadProgress {
                pct: None,
                done: true,
                error: Some(format!("Update failed: {}", err)),
                is_update: true,
            });
        }
    });
}

/// Spawn the search worker thread
pub fn spawn_worker(
    query_rx: Receiver<SearchQuery>,
    result_tx: Sender<SearchResults>,
    download_tx: Sender<DownloadProgress>,
    registry_scope: RegistrySearchScope,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let registry_updates_disabled = registry_scope.updates_disabled();
        let mut registry_client: Option<RegistrySearchClient> = None;
        let mut registry_mtimes = Vec::new();

        let client = if registry_scope.local_indexes_exist() {
            RegistrySearchClient::open_cached_scope(&registry_scope)
        } else {
            RegistrySearchClient::open_scope_with_progress(
                registry_scope.clone(),
                &download_tx,
                false,
                false,
            )
        };
        match client {
            Ok(client) => {
                registry_mtimes = registry_index_mtimes(&registry_scope);
                registry_client = Some(client);
                if !registry_updates_disabled {
                    spawn_registry_update_check(registry_scope.clone(), download_tx.clone(), false);
                }
            }
            Err(err) => {
                let _ = download_tx.send(DownloadProgress {
                    pct: None,
                    done: true,
                    error: Some(format!("Failed to open registry: {}", err)),
                    is_update: false,
                });
            }
        }

        while let Ok(mut query) = query_rx.recv() {
            while let Ok(next) = query_rx.try_recv() {
                query = next;
            }

            let current_mtimes = registry_index_mtimes(&registry_scope);
            if current_mtimes != registry_mtimes
                && let Ok(client) = RegistrySearchClient::open_cached_scope(&registry_scope)
            {
                registry_client = Some(client);
                registry_mtimes = current_mtimes;
            }

            if query.force_update && registry_updates_disabled {
                let _ = download_tx.send(DownloadProgress {
                    pct: None,
                    done: true,
                    error: Some(
                        "Registry updates are disabled when --registry-index is set".to_string(),
                    ),
                    is_update: true,
                });
            } else if query.force_update {
                spawn_registry_update_check(registry_scope.clone(), download_tx.clone(), true);
            }

            if registry_client.is_none() {
                registry_client = match RegistrySearchClient::open_scope_with_progress(
                    registry_scope.clone(),
                    &download_tx,
                    false,
                    false,
                ) {
                    Ok(client) => {
                        registry_mtimes = registry_index_mtimes(&registry_scope);
                        Some(client)
                    }
                    Err(err) => {
                        let _ = download_tx.send(DownloadProgress {
                            pct: None,
                            done: true,
                            error: Some(format!("Failed to open registry: {}", err)),
                            is_update: false,
                        });
                        continue;
                    }
                };
            }

            let Some(client) = registry_client.as_ref() else {
                continue;
            };
            let results = match query.mode {
                SearchMode::RegistryModules => {
                    SearchResults::RegistryModules(HitResults::new(query.id, || {
                        client.search_modules(&query.text)
                    }))
                }
                SearchMode::RegistryComponents => {
                    SearchResults::RegistrySymbols(HitResults::new(query.id, || {
                        client.search_symbols(&query.text)
                    }))
                }
            };
            let _ = result_tx.send(results);
        }
    })
}

const AVAILABILITY_WORKER_CHUNK_SIZE: usize = 10;

/// Batch availability request for the current ordered set of missing lookup keys.
pub type PricingRequest = Vec<ComponentKey>;

/// Outcome for a single pricing lookup key.
#[derive(Debug, Clone)]
pub enum PricingResult {
    Ready(Box<pcb_sch::bom::Availability>),
    Empty,
    Failed,
}

/// Chunk of resolved pricing lookup keys.
pub type PricingResponse = Vec<(ComponentKey, PricingResult)>;

/// Spawn a worker thread that fetches availability for components in batches
pub fn spawn_availability_worker(
    req_rx: Receiver<PricingRequest>,
    resp_tx: Sender<PricingResponse>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        while let Ok(mut queue) = req_rx.recv() {
            while let Ok(next) = req_rx.try_recv() {
                queue = next;
            }

            while !queue.is_empty() {
                let chunk_len = queue.len().min(AVAILABILITY_WORKER_CHUNK_SIZE);
                let chunk: Vec<_> = queue.drain(..chunk_len).collect();

                let response = match crate::auth::get_api_token() {
                    Ok(token) => fetch_pricing_chunk(token.as_deref(), &chunk),
                    Err(e) => {
                        log::warn!("Pricing auth failed: {}", e);
                        chunk
                            .into_iter()
                            .map(|key| (key, PricingResult::Failed))
                            .collect()
                    }
                };

                let _ = resp_tx.send(response);

                while let Ok(next) = req_rx.try_recv() {
                    queue = next;
                }
            }
        }
    })
}

fn fetch_pricing_chunk(auth_token: Option<&str>, chunk: &[ComponentKey]) -> PricingResponse {
    let groups: Vec<_> = chunk.iter().map(|key| vec![key.clone()]).collect();

    match crate::bom::fetch_pricing_grouped_batch(auth_token, &groups) {
        Ok(availability_results) => chunk
            .iter()
            .cloned()
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
                .map(|key| (key.clone(), PricingResult::Failed))
                .collect()
        }
    }
}
