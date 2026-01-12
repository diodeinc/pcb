//! Main application state and event loop

use super::super::download::{
    check_registry_access, DownloadProgress, RegistryAccessResult, RegistryIndexMetadata,
};
use super::image::ImageProtocol;
use super::search::{
    spawn_component_worker, spawn_detail_worker, spawn_worker, ComponentSearchQuery,
    ComponentSearchResults, DetailRequest, DetailResponse, SearchQuery, SearchResults,
};
use super::ui;
use crate::{PackageRelations, RegistryClient, RegistryPart};
use anyhow::Result;
use arboard::Clipboard;
use crossterm::{
    cursor::SetCursorStyle,
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
        MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use pcb_zen::fork::{fork_package, ForkOptions, ForkSuccess};
use ratatui::{backend::CrosstermBackend, widgets::ListState, Terminal};
use ratatui_image::picker::Picker;
use std::io::{self, Stdout};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

/// Simple single-line text input with cursor
#[derive(Default, Clone)]
pub struct TextInput {
    pub text: String,
    pub cursor: usize,
}

impl TextInput {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a character at the cursor position
    pub fn insert_char(&mut self, c: char) {
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    /// Delete the character before the cursor
    pub fn delete_char_before(&mut self) {
        if self.cursor > 0 {
            let prev = self.text[..self.cursor]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.text.drain(prev..self.cursor);
            self.cursor = prev;
        }
    }

    /// Delete the character at the cursor
    pub fn delete_char_at(&mut self) {
        if self.cursor < self.text.len() {
            let next = self.text[self.cursor..]
                .chars()
                .next()
                .map(|c| self.cursor + c.len_utf8())
                .unwrap_or(self.cursor);
            self.text.drain(self.cursor..next);
        }
    }

    /// Move cursor left by one character
    pub fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor = self.text[..self.cursor]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
        }
    }

    /// Move cursor right by one character
    pub fn move_right(&mut self) {
        if self.cursor < self.text.len() {
            self.cursor = self.text[self.cursor..]
                .chars()
                .next()
                .map(|c| self.cursor + c.len_utf8())
                .unwrap_or(self.text.len());
        }
    }

    /// Move cursor to start
    pub fn move_start(&mut self) {
        self.cursor = 0;
    }

    /// Move cursor to end
    pub fn move_end(&mut self) {
        self.cursor = self.text.len();
    }

    /// Check if character is a word boundary (whitespace or path separator)
    fn is_word_boundary(c: char) -> bool {
        c.is_whitespace() || c == '/'
    }

    /// Move cursor left by one word
    pub fn move_word_left(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let s = &self.text[..self.cursor];
        // Skip trailing boundary characters
        let trimmed_len = s
            .char_indices()
            .rev()
            .find(|(_, c)| !Self::is_word_boundary(*c))
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(0);
        if trimmed_len == 0 {
            self.cursor = 0;
            return;
        }
        let trimmed = &s[..trimmed_len];
        self.cursor = trimmed
            .char_indices()
            .rev()
            .find(|(_, c)| Self::is_word_boundary(*c))
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(0);
    }

    /// Move cursor right by one word
    pub fn move_word_right(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        let s = &self.text[self.cursor..];
        // Skip leading boundary characters
        let boundary_count = s.chars().take_while(|c| Self::is_word_boundary(*c)).count();
        let boundary_bytes: usize = s.chars().take(boundary_count).map(|c| c.len_utf8()).sum();
        let after = &s[boundary_bytes..];
        let word_bytes: usize = after
            .chars()
            .take_while(|c| !Self::is_word_boundary(*c))
            .map(|c| c.len_utf8())
            .sum();
        self.cursor += boundary_bytes + word_bytes;
    }

    /// Delete word before cursor
    pub fn delete_word_before(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let s = &self.text[..self.cursor];
        // Skip trailing boundary characters
        let trimmed_len = s
            .char_indices()
            .rev()
            .find(|(_, c)| !Self::is_word_boundary(*c))
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(0);
        if trimmed_len == 0 {
            self.text.drain(..self.cursor);
            self.cursor = 0;
            return;
        }
        let trimmed = &s[..trimmed_len];
        let start = trimmed
            .char_indices()
            .rev()
            .find(|(_, c)| Self::is_word_boundary(*c))
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(0);
        self.text.drain(start..self.cursor);
        self.cursor = start;
    }

    /// Clear all text
    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    /// Handle a key event, returns true if the event was consumed
    pub fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        // Helper to check if a modifier is present (handles combined modifiers)
        let has_ctrl = modifiers.contains(KeyModifiers::CONTROL);
        let has_alt = modifiers.contains(KeyModifiers::ALT);
        let has_super = modifiers.contains(KeyModifiers::SUPER);
        let has_word_mod = has_alt || has_super; // macOS Option may report as SUPER

        match code {
            // Ctrl+U: clear all
            KeyCode::Char('u') if has_ctrl => self.clear(),
            // Ctrl+W: delete word
            KeyCode::Char('w') if has_ctrl => self.delete_word_before(),
            // Alt+Backspace: delete word (macOS Option+Delete)
            KeyCode::Backspace if has_alt || has_super => self.delete_word_before(),
            // Ctrl+A or Home: move to start
            KeyCode::Char('a') if has_ctrl => self.move_start(),
            KeyCode::Home => self.move_start(),
            // Ctrl+E or End: move to end
            KeyCode::Char('e') if has_ctrl => self.move_end(),
            KeyCode::End => self.move_end(),
            // Alt+B or Esc-B: word left (emacs style, common on macOS)
            KeyCode::Char('b') if has_alt || has_super => self.move_word_left(),
            // Alt+F or Esc-F: word right (emacs style, common on macOS)
            KeyCode::Char('f') if has_alt || has_super => self.move_word_right(),
            // Alt+Left or Ctrl+Left: word left
            KeyCode::Left if has_word_mod || has_ctrl => self.move_word_left(),
            // Alt+Right or Ctrl+Right: word right
            KeyCode::Right if has_word_mod || has_ctrl => self.move_word_right(),
            // Left: move left (no modifiers)
            KeyCode::Left => self.move_left(),
            // Right: move right (no modifiers)
            KeyCode::Right => self.move_right(),
            // Backspace: delete before (no modifier - checked after Alt+Backspace)
            KeyCode::Backspace => self.delete_char_before(),
            // Delete: delete at cursor
            KeyCode::Delete => self.delete_char_at(),
            // Regular char (no Ctrl/Alt modifiers)
            KeyCode::Char(c) if !has_ctrl && !has_alt => self.insert_char(c),
            _ => return false,
        }
        true
    }
}

/// Toast notification state
pub struct Toast {
    pub message: String,
    pub expires_at: Instant,
    pub is_error: bool,
}

impl Toast {
    pub fn new(message: String, duration: Duration) -> Self {
        Self {
            message,
            expires_at: Instant::now() + duration,
            is_error: false,
        }
    }

    pub fn error(message: String, duration: Duration) -> Self {
        Self {
            message,
            expires_at: Instant::now() + duration,
            is_error: true,
        }
    }

    pub fn is_expired(&self) -> bool {
        Instant::now() >= self.expires_at
    }
}

/// Search mode - Registry (local) or New (online API)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchMode {
    /// Search local registry database (fast)
    #[default]
    Registry,
    /// Search online APIs to create new components (slow)
    New,
}

impl SearchMode {
    /// Cycle to next mode
    pub fn cycle(self) -> Self {
        match self {
            SearchMode::Registry => SearchMode::New,
            SearchMode::New => SearchMode::Registry,
        }
    }
}

/// Download state for the registry index
#[derive(Debug, Clone)]
pub enum DownloadState {
    NotStarted,
    Downloading {
        pct: Option<u8>,
        started_at: Instant,
    },
    Updating {
        pct: Option<u8>,
        started_at: Instant,
    },
    Done,
    Failed(String),
}

/// Command palette commands
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    SwitchMode,
    ToggleDebugPanels,
    UpdateRegistryIndex,
    OpenInDigikey,
    ForkPackage,
}

impl Command {
    pub const ALL: &'static [Command] = &[
        Command::SwitchMode,
        Command::ToggleDebugPanels,
        Command::UpdateRegistryIndex,
        Command::OpenInDigikey,
        Command::ForkPackage,
    ];

    /// Short machine-readable name
    pub fn name(&self) -> &'static str {
        match self {
            Command::SwitchMode => "switch-mode",
            Command::ToggleDebugPanels => "toggle-debug-panels",
            Command::UpdateRegistryIndex => "update-registry-index",
            Command::OpenInDigikey => "open-in-digikey",
            Command::ForkPackage => "fork-package",
        }
    }

    /// Human-readable description
    pub fn description(&self) -> &'static str {
        match self {
            Command::SwitchMode => "Switch between registry and new component search modes",
            Command::ToggleDebugPanels => {
                "Show or hide the Trigram, Word, and Semantic search result panels"
            }
            Command::UpdateRegistryIndex => "Force re-download the registry index",
            Command::OpenInDigikey => "Open the selected part on Digikey",
            Command::ForkPackage => "Fork the selected package into your workspace",
        }
    }

    /// Score command against a fuzzy query (higher = better match, None = no match)
    pub fn match_score(&self, query: &str) -> Option<i64> {
        use fuzzy_matcher::skim::SkimMatcherV2;
        use fuzzy_matcher::FuzzyMatcher;

        if query.is_empty() {
            return Some(0);
        }

        let matcher = SkimMatcherV2::default();

        // Try matching against name first (higher priority)
        if let Some(score) = matcher.fuzzy_match(self.name(), query) {
            return Some(score + 1000);
        }

        // Try matching against description
        matcher.fuzzy_match(self.description(), query)
    }

    /// Check if command is enabled given current app state
    pub fn is_enabled(
        &self,
        selected_part: Option<&RegistryPart>,
        registry_mode_available: bool,
    ) -> bool {
        match self {
            Command::SwitchMode | Command::UpdateRegistryIndex => registry_mode_available,
            Command::ToggleDebugPanels => true,
            Command::OpenInDigikey => {
                // Only enabled if we have a component with DigiKey product URL
                selected_part
                    .and_then(|p| p.digikey.as_ref())
                    .and_then(|dk| dk.product_url.as_ref())
                    .is_some()
            }
            Command::ForkPackage => {
                // Enabled if we have a selected package
                selected_part.is_some()
            }
        }
    }
}

/// Application state
pub struct App {
    /// Current search mode
    pub mode: SearchMode,
    /// Search input
    pub search_input: TextInput,
    /// Current search results
    pub results: SearchResults,
    /// List state for merged results (handles selection + scroll)
    pub list_state: ListState,
    /// Total packages count in registry (0 until index is ready)
    pub packages_count: i64,
    /// Should quit?
    pub should_quit: bool,
    /// Toast notification
    pub toast: Option<Toast>,
    /// Download state
    pub download_state: DownloadState,
    /// Query counter for deduplication
    query_counter: u64,
    /// Last query text (for change detection)
    last_query: String,
    /// Last results query_id (for detecting result changes)
    last_results_id: u64,
    /// Channel to send queries to worker
    query_tx: Sender<SearchQuery>,
    /// Channel to receive results from worker
    result_rx: Receiver<SearchResults>,
    /// Channel to receive download progress
    download_rx: Receiver<DownloadProgress>,
    /// Debounce timer for search queries
    last_input_time: Instant,
    /// Clipboard handle
    clipboard: Option<Clipboard>,
    /// Detected image protocol support
    pub image_protocol: ImageProtocol,
    /// Image picker for decoding (None if not supported)
    pub picker: Option<Picker>,
    /// Cached selected part details (fetched asynchronously)
    pub selected_part: Option<RegistryPart>,
    /// Cached dependencies/dependents for selected package
    pub package_relations: PackageRelations,
    /// Channel to send detail requests to worker
    detail_tx: Sender<DetailRequest>,
    /// Channel to receive detail responses from worker
    detail_rx: Receiver<DetailResponse>,
    /// Part ID of pending detail request (None = not waiting)
    pending_detail_for: Option<i64>,
    /// When we started waiting for current detail request (for delayed "Loading..." display)
    detail_request_started: Option<Instant>,
    /// Command palette visible
    pub show_command_palette: bool,
    /// Command palette selection index
    pub command_palette_index: usize,
    /// Command palette search input
    pub command_palette_input: TextInput,
    /// Filtered commands based on query
    pub command_palette_filtered: Vec<Command>,
    /// Show debug panels (Trigram/Word/Semantic)
    pub show_debug_panels: bool,
    /// Channel to receive fork results
    fork_rx: Receiver<Result<ForkSuccess, String>>,
    /// Sender for fork results (cloned into spawned threads)
    fork_tx: Sender<Result<ForkSuccess, String>>,
    /// Channel to send queries to component search worker
    component_query_tx: Sender<ComponentSearchQuery>,
    /// Channel to receive results from component search worker
    component_result_rx: Receiver<ComponentSearchResults>,
    /// Current component search results (for New mode)
    pub component_results: ComponentSearchResults,
    /// Query counter for component search (separate from registry search)
    component_query_counter: u64,
    /// Last component query text (for change detection)
    last_component_query: String,
    /// Whether component search is in progress
    pub component_searching: bool,
    /// When component search started (for spinner animation)
    pub component_search_started: Instant,
    /// List state for component results (handles selection + scroll)
    pub component_list_state: ListState,
    /// Selected component to download after TUI exits (New mode)
    pub selected_component_for_download: Option<crate::component::ComponentSearchResult>,
    /// Whether registry mode is available (false if index fetch failed)
    pub registry_mode_available: bool,
}

/// Preflight configuration for TUI startup
pub struct Preflight {
    /// Starting search mode
    pub start_mode: SearchMode,
    /// Whether to spawn the registry worker (false = component-only mode)
    pub spawn_registry_worker: bool,
    /// Pre-fetched registry index metadata (avoids duplicate request during download)
    pub registry_metadata: Option<RegistryIndexMetadata>,
}

impl App {
    pub fn new(preflight: Preflight) -> Self {
        let (query_tx, query_rx) = mpsc::channel::<SearchQuery>();
        let (result_tx, result_rx) = mpsc::channel::<SearchResults>();
        let (download_tx, download_rx) = mpsc::channel::<DownloadProgress>();
        let (detail_tx, detail_req_rx) = mpsc::channel::<DetailRequest>();
        let (detail_resp_tx, detail_rx) = mpsc::channel::<DetailResponse>();

        // Only spawn registry workers if enabled
        if preflight.spawn_registry_worker {
            spawn_worker(query_rx, result_tx, download_tx, preflight.registry_metadata);
            spawn_detail_worker(detail_req_rx, detail_resp_tx);
        }

        let (fork_tx, fork_rx) = mpsc::channel::<Result<ForkSuccess, String>>();

        // Component search (online API) channels
        let (component_query_tx, component_query_rx) = mpsc::channel::<ComponentSearchQuery>();
        let (component_result_tx, component_result_rx) = mpsc::channel::<ComponentSearchResults>();
        spawn_component_worker(component_query_rx, component_result_tx);

        let clipboard = Clipboard::new().ok();
        let image_protocol = ImageProtocol::detect();
        let picker = if image_protocol.is_supported() {
            Picker::from_query_stdio().ok()
        } else {
            None
        };

        // If registry worker not spawned, mark as already done (no download needed)
        let download_state = if preflight.spawn_registry_worker {
            DownloadState::NotStarted
        } else {
            DownloadState::Done
        };

        Self {
            mode: preflight.start_mode,
            search_input: TextInput::new(),
            results: SearchResults::default(),
            list_state: ListState::default(),
            packages_count: 0,
            should_quit: false,
            toast: None,
            download_state,
            query_counter: 0,
            last_query: String::new(),
            last_results_id: 0,
            query_tx,
            result_rx,
            download_rx,
            last_input_time: Instant::now(),
            clipboard,
            image_protocol,
            picker,
            selected_part: None,
            package_relations: PackageRelations::default(),
            detail_tx,
            detail_rx,
            pending_detail_for: None,
            detail_request_started: None,
            show_command_palette: false,
            command_palette_index: 0,
            command_palette_input: TextInput::new(),
            command_palette_filtered: Command::ALL.to_vec(),
            show_debug_panels: false,
            fork_rx,
            fork_tx,
            component_query_tx,
            component_result_rx,
            component_results: ComponentSearchResults::default(),
            component_query_counter: 0,
            last_component_query: String::new(),
            component_searching: false,
            component_search_started: Instant::now(),
            component_list_state: ListState::default(),
            selected_component_for_download: None,
            registry_mode_available: preflight.spawn_registry_worker,
        }
    }

    /// Get current query text
    fn current_query(&self) -> String {
        self.search_input.text.clone()
    }

    /// Check if query changed and send to worker if so
    /// Send registry search query if changed (for Registry mode)
    fn maybe_send_registry_query(&mut self) {
        if self.mode != SearchMode::Registry {
            return;
        }

        if !matches!(
            self.download_state,
            DownloadState::Done | DownloadState::Updating { .. }
        ) {
            return;
        }

        let query = self.current_query();
        if query != self.last_query {
            self.last_query = query.clone();
            self.query_counter += 1;

            let _ = self.query_tx.send(SearchQuery {
                id: self.query_counter,
                text: query,
                force_update: false,
            });
        }
    }

    /// Send component search query if changed (for New mode)
    fn maybe_send_component_query(&mut self) {
        if self.mode != SearchMode::New {
            return;
        }

        let query = self.current_query();
        if query != self.last_component_query {
            self.last_component_query = query.clone();
            self.component_query_counter += 1;
            self.component_searching = true;

            let _ = self.component_query_tx.send(ComponentSearchQuery {
                id: self.component_query_counter,
                text: query,
            });
        }
    }

    /// Handle input change in New mode - immediately clear results and show spinner
    fn on_input_change_new_mode(&mut self) {
        if self.mode == SearchMode::New {
            // Clear results immediately for responsive feedback
            self.component_results = ComponentSearchResults::default();
            self.component_searching = true;
            self.component_search_started = Instant::now();
        }
    }

    /// Get the selected index (0-indexed into merged results)
    pub fn selected_index(&self) -> usize {
        self.list_state.selected().unwrap_or(0)
    }

    /// Poll for download progress from worker (non-blocking)
    fn poll_download(&mut self) {
        while let Ok(progress) = self.download_rx.try_recv() {
            match &progress {
                // Initial download completed successfully
                DownloadProgress {
                    done: true,
                    error: None,
                    is_update: false,
                    ..
                } => {
                    self.download_state = DownloadState::Done;
                    if let Ok(client) = RegistryClient::open() {
                        self.packages_count = client.count().unwrap_or(0);
                    }
                    self.last_query.clear();
                }
                // Update completed successfully
                DownloadProgress {
                    done: true,
                    error: None,
                    is_update: true,
                    ..
                } => {
                    self.download_state = DownloadState::Done;
                    self.toast = Some(Toast::new(
                        "Index updated".to_string(),
                        Duration::from_secs(2),
                    ));
                    if let Ok(client) = RegistryClient::open() {
                        self.packages_count = client.count().unwrap_or(0);
                    }
                    // Trigger re-search with updated DB
                    self.query_counter += 1;
                    self.last_query = self.current_query();
                    let _ = self.query_tx.send(SearchQuery {
                        id: self.query_counter,
                        text: self.last_query.clone(),
                        force_update: false,
                    });
                }
                // Initial download failed - show error
                DownloadProgress {
                    done: true,
                    error: Some(e),
                    is_update: false,
                    ..
                } => {
                    self.download_state = DownloadState::Failed(e.clone());
                    self.toast = Some(Toast::error(
                        format!("Download failed: {}", e),
                        Duration::from_secs(5),
                    ));
                }
                // Update failed - keep using old DB
                DownloadProgress {
                    done: true,
                    error: Some(e),
                    is_update: true,
                    ..
                } => {
                    self.download_state = DownloadState::Done;
                    self.toast = Some(Toast::error(e.clone(), Duration::from_secs(5)));
                }
                // In progress - initial download
                DownloadProgress {
                    pct,
                    done: false,
                    is_update: false,
                    ..
                } => match &self.download_state {
                    DownloadState::Downloading { started_at, .. } => {
                        self.download_state = DownloadState::Downloading {
                            pct: *pct,
                            started_at: *started_at,
                        };
                    }
                    _ => {
                        self.download_state = DownloadState::Downloading {
                            pct: *pct,
                            started_at: Instant::now(),
                        };
                    }
                },
                // In progress - updating
                DownloadProgress {
                    pct,
                    done: false,
                    is_update: true,
                    ..
                } => match &self.download_state {
                    DownloadState::Updating { started_at, .. } => {
                        self.download_state = DownloadState::Updating {
                            pct: *pct,
                            started_at: *started_at,
                        };
                    }
                    _ => {
                        self.download_state = DownloadState::Updating {
                            pct: *pct,
                            started_at: Instant::now(),
                        };
                    }
                },
            }
        }
    }

    /// Poll for results from worker (non-blocking)
    fn poll_results(&mut self) {
        while let Ok(results) = self.result_rx.try_recv() {
            if results.query_id == self.query_counter {
                let is_new_query = self.results.query_id != results.query_id;
                self.results = results;

                if is_new_query {
                    // Reset selection for new query
                    self.list_state = ListState::default();
                    if !self.results.merged.is_empty() {
                        self.list_state.select(Some(0));
                    }
                    self.last_results_id = self.results.query_id;
                    // Trigger detail fetch for the new selection
                    self.enqueue_detail_request();
                } else {
                    // Clamp selection if results shrunk
                    let len = self.results.merged.len();
                    if let Some(sel) = self.list_state.selected() {
                        if len == 0 {
                            self.list_state.select(None);
                        } else if sel >= len {
                            self.list_state.select(Some(len - 1));
                        }
                    }
                }
            }
        }
    }

    /// Enqueue a detail request for the currently selected part.
    /// Called when selection changes - the worker will coalesce rapid requests.
    /// Note: We keep showing old `selected_part` until new data arrives to avoid flicker.
    fn enqueue_detail_request(&mut self) {
        let idx = self.selected_index();
        let Some(hit) = self.results.merged.get(idx) else {
            self.pending_detail_for = None;
            self.detail_request_started = None;
            self.selected_part = None;
            self.package_relations = PackageRelations::default();
            return;
        };

        let part_id = hit.id;

        // Already requested this exact part and it's still pending
        if self.pending_detail_for == Some(part_id) {
            return;
        }

        // Mark as pending but keep showing old details until new ones arrive
        self.pending_detail_for = Some(part_id);
        self.detail_request_started = Some(Instant::now());

        let _ = self.detail_tx.send(DetailRequest { part_id });
    }

    /// Poll for detail responses from worker (non-blocking)
    fn poll_detail_responses(&mut self) {
        while let Ok(resp) = self.detail_rx.try_recv() {
            // Ignore responses for parts we no longer care about
            if self.pending_detail_for != Some(resp.part_id) {
                continue;
            }

            self.selected_part = resp.part;
            self.package_relations = resp.relations;
            self.pending_detail_for = None;
            self.detail_request_started = None;
        }
    }

    /// Poll for fork results from background thread (non-blocking)
    fn poll_fork_results(&mut self) {
        while let Ok(result) = self.fork_rx.try_recv() {
            match result {
                Ok(success) => {
                    self.toast = Some(Toast::new(
                        format!("Forked to {}", success.fork_dir.display()),
                        Duration::from_secs(3),
                    ));
                }
                Err(e) => {
                    // Extract just the first line of the error for display
                    let first_line = e.lines().next().unwrap_or("Fork failed");
                    self.toast = Some(Toast::error(first_line.to_string(), Duration::from_secs(5)));
                }
            }
        }
    }

    /// Poll for component search results from worker (non-blocking)
    fn poll_component_results(&mut self) {
        while let Ok(results) = self.component_result_rx.try_recv() {
            if results.query_id == self.component_query_counter {
                self.component_searching = false;

                // Handle errors
                if let Some(ref error) = results.error {
                    self.toast = Some(Toast::error(error.clone(), Duration::from_secs(5)));
                }

                self.component_results = results;

                // Reset selection to first item
                self.component_list_state = ListState::default();
                if !self.component_results.results.is_empty() {
                    self.component_list_state.select(Some(0));
                }
            }
        }
    }

    /// Returns true if we're waiting for details and should show a loading indicator.
    /// Only returns true after a short delay to avoid flicker on fast responses.
    pub fn is_loading_details(&self) -> bool {
        const LOADING_DELAY_MS: u64 = 100;
        self.detail_request_started
            .map(|t| t.elapsed() > Duration::from_millis(LOADING_DELAY_MS))
            .unwrap_or(false)
    }

    /// Move selection up by n items (toward index 0 = best matches at bottom of display)
    fn scroll_up(&mut self, n: u16) {
        match self.mode {
            SearchMode::Registry => {
                if self.results.merged.is_empty() {
                    return;
                }
                let current = self.list_state.selected().unwrap_or(0);
                let new_index = current.saturating_sub(n as usize);
                self.list_state.select(Some(new_index));
                self.enqueue_detail_request();
            }
            SearchMode::New => {
                if self.component_results.results.is_empty() {
                    return;
                }
                let current = self.component_list_state.selected().unwrap_or(0);
                let new_index = current.saturating_sub(n as usize);
                self.component_list_state.select(Some(new_index));
            }
        }
    }

    /// Move selection down by n items (toward higher indices = worse matches at top of display)
    fn scroll_down(&mut self, n: u16) {
        match self.mode {
            SearchMode::Registry => {
                if self.results.merged.is_empty() {
                    return;
                }
                let current = self.list_state.selected().unwrap_or(0);
                let max_index = self.results.merged.len().saturating_sub(1);
                let new_index = current.saturating_add(n as usize).min(max_index);
                self.list_state.select(Some(new_index));
                self.enqueue_detail_request();
            }
            SearchMode::New => {
                if self.component_results.results.is_empty() {
                    return;
                }
                let current = self.component_list_state.selected().unwrap_or(0);
                let max_index = self.component_results.results.len().saturating_sub(1);
                let new_index = current.saturating_add(n as usize).min(max_index);
                self.component_list_state.select(Some(new_index));
            }
        }
    }

    /// Jump to first result (index 0 = best match, displayed at bottom)
    fn select_first(&mut self) {
        match self.mode {
            SearchMode::Registry => {
                if !self.results.merged.is_empty() {
                    self.list_state.select(Some(0));
                    self.enqueue_detail_request();
                }
            }
            SearchMode::New => {
                if !self.component_results.results.is_empty() {
                    self.component_list_state.select(Some(0));
                }
            }
        }
    }

    /// Jump to last result (highest index = worst match, displayed at top)
    fn select_last(&mut self) {
        match self.mode {
            SearchMode::Registry => {
                if !self.results.merged.is_empty() {
                    self.list_state.select(Some(self.results.merged.len() - 1));
                    self.enqueue_detail_request();
                }
            }
            SearchMode::New => {
                if !self.component_results.results.is_empty() {
                    self.component_list_state
                        .select(Some(self.component_results.results.len() - 1));
                }
            }
        }
    }

    /// Handle Enter key - mode-specific behavior
    fn handle_enter(&mut self) {
        match self.mode {
            SearchMode::Registry => self.copy_selected(),
            SearchMode::New => self.select_component_for_download(),
        }
    }

    /// Copy selected item's URL to clipboard (Registry mode)
    fn copy_selected(&mut self) {
        if let Some(part) = self.results.merged.get(self.selected_index()) {
            let url = part.url.clone();

            if let Some(ref mut clipboard) = self.clipboard {
                if clipboard.set_text(&url).is_ok() {
                    self.toast = Some(Toast::new(
                        format!("Copied: {}", url),
                        Duration::from_secs(2),
                    ));
                } else {
                    self.toast = Some(Toast::new(
                        "Failed to copy to clipboard".to_string(),
                        Duration::from_secs(2),
                    ));
                }
            } else {
                self.toast = Some(Toast::new(
                    "Clipboard not available".to_string(),
                    Duration::from_secs(2),
                ));
            }
        }
    }

    /// Select component for download and exit TUI (New mode)
    fn select_component_for_download(&mut self) {
        let selected_index = self.component_list_state.selected();
        if let Some(idx) = selected_index {
            if let Some(result) = self.component_results.results.get(idx) {
                self.selected_component_for_download = Some(result.clone());
                self.should_quit = true;
            }
        }
    }

    /// Clear expired toast
    fn update_toast(&mut self) {
        if let Some(ref toast) = self.toast {
            if toast.is_expired() {
                self.toast = None;
            }
        }
    }

    /// Switch search mode (Registry <-> New)
    fn switch_mode(&mut self) {
        // Don't allow switching if registry mode is not available
        if !self.registry_mode_available {
            return;
        }
        self.mode = self.mode.cycle();

        // Clear results when switching modes
        self.results = SearchResults::default();
        self.list_state = ListState::default();
        self.selected_part = None;
        self.package_relations = PackageRelations::default();
        self.pending_detail_for = None;
        self.detail_request_started = None;
        self.last_query.clear();

        let mode_name = match self.mode {
            SearchMode::Registry => "registry",
            SearchMode::New => "new",
        };
        self.toast = Some(Toast::new(
            format!("Switched to {} mode", mode_name),
            Duration::from_secs(2),
        ));
    }

    /// Execute a command from the palette
    fn execute_command(&mut self, cmd: Command) {
        match cmd {
            Command::SwitchMode => {
                self.switch_mode();
            }
            Command::ToggleDebugPanels => {
                self.show_debug_panels = !self.show_debug_panels;
                let state = if self.show_debug_panels {
                    "shown"
                } else {
                    "hidden"
                };
                self.toast = Some(Toast::new(
                    format!("Debug panels {}", state),
                    Duration::from_secs(2),
                ));
            }
            Command::UpdateRegistryIndex => {
                // Send a query with force_update flag to trigger re-download
                self.query_counter += 1;
                let _ = self.query_tx.send(SearchQuery {
                    id: self.query_counter,
                    text: self.search_input.text.clone(),
                    force_update: true,
                });
                self.toast = Some(Toast::new(
                    "Updating registry index...".to_string(),
                    Duration::from_secs(2),
                ));
            }
            Command::OpenInDigikey => {
                if let Some(ref part) = self.selected_part {
                    if let Some(ref dk) = part.digikey {
                        if let Some(ref url) = dk.product_url {
                            if open::that(url).is_ok() {
                                self.toast = Some(Toast::new(
                                    "Opened in browser".to_string(),
                                    Duration::from_secs(2),
                                ));
                            } else {
                                self.toast = Some(Toast::error(
                                    "Failed to open browser".to_string(),
                                    Duration::from_secs(2),
                                ));
                            }
                        } else {
                            self.toast = Some(Toast::error(
                                "No Digikey URL available".to_string(),
                                Duration::from_secs(2),
                            ));
                        }
                    } else {
                        self.toast = Some(Toast::error(
                            "No Digikey data for this part".to_string(),
                            Duration::from_secs(2),
                        ));
                    }
                } else {
                    self.toast = Some(Toast::error(
                        "No part selected".to_string(),
                        Duration::from_secs(2),
                    ));
                }
            }
            Command::ForkPackage => {
                if let Some(ref part) = self.selected_part {
                    let url = part.url.clone();
                    let tx = self.fork_tx.clone();

                    // Show immediate feedback
                    self.toast = Some(Toast::new(
                        "Forking package...".to_string(),
                        Duration::from_secs(30), // Long duration, will be replaced
                    ));

                    // Spawn thread to do the fork
                    thread::spawn(move || {
                        let result = fork_package(ForkOptions {
                            url,
                            version: None, // Use latest
                            force: false,
                        });
                        let _ = tx.send(result.map_err(|e| e.to_string()));
                    });
                } else {
                    self.toast = Some(Toast::error(
                        "No package selected".to_string(),
                        Duration::from_secs(2),
                    ));
                }
            }
        }
    }

    /// Update filtered commands based on query
    fn update_command_filter(&mut self) {
        let query = &self.command_palette_input.text;
        let mut scored: Vec<_> = Command::ALL
            .iter()
            .copied()
            .filter_map(|cmd| cmd.match_score(query).map(|score| (cmd, score)))
            .collect();
        // Sort by score descending
        scored.sort_by(|a, b| b.1.cmp(&a.1));
        self.command_palette_filtered = scored.into_iter().map(|(cmd, _)| cmd).collect();
        // Reset selection
        self.command_palette_index = 0;
    }

    /// Open command palette
    fn open_command_palette(&mut self) {
        self.show_command_palette = true;
        self.command_palette_index = 0;
        self.command_palette_input.clear();
        self.command_palette_filtered = Command::ALL.to_vec();
    }

    /// Close command palette
    fn close_command_palette(&mut self) {
        self.show_command_palette = false;
        self.command_palette_input.clear();
    }

    /// Handle input event (mouse scroll handled separately in run_loop)
    fn handle_event(&mut self, event: Event) {
        // Handle command palette events separately
        if self.show_command_palette {
            if let Event::Key(key) = event {
                if key.kind == KeyEventKind::Press {
                    match (key.code, key.modifiers) {
                        (KeyCode::Esc, _)
                        | (KeyCode::Char('c'), KeyModifiers::CONTROL)
                        | (KeyCode::Char('o'), KeyModifiers::CONTROL) => {
                            self.close_command_palette();
                        }
                        (KeyCode::Up, _) | (KeyCode::Char('k'), KeyModifiers::CONTROL) => {
                            if self.command_palette_index > 0 {
                                self.command_palette_index -= 1;
                            }
                        }
                        (KeyCode::Down, _) | (KeyCode::Char('j'), KeyModifiers::CONTROL) => {
                            let max = self.command_palette_filtered.len().saturating_sub(1);
                            if self.command_palette_index < max {
                                self.command_palette_index += 1;
                            }
                        }
                        (KeyCode::Enter, _) => {
                            if let Some(&cmd) = self
                                .command_palette_filtered
                                .get(self.command_palette_index)
                            {
                                if cmd.is_enabled(self.selected_part.as_ref(), self.registry_mode_available) {
                                    self.close_command_palette();
                                    self.execute_command(cmd);
                                }
                            }
                        }
                        _ => {
                            if self
                                .command_palette_input
                                .handle_key(key.code, key.modifiers)
                            {
                                self.update_command_filter();
                            }
                        }
                    }
                }
            }
            return;
        }

        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => match (key.code, key.modifiers) {
                (KeyCode::Esc, _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                    self.should_quit = true
                }
                (KeyCode::Char('o'), KeyModifiers::CONTROL) => {
                    self.open_command_palette();
                }
                (KeyCode::Char('s'), KeyModifiers::CONTROL)
                    if self.registry_mode_available =>
                {
                    self.switch_mode();
                }
                (KeyCode::Up, _) | (KeyCode::Char('k'), KeyModifiers::CONTROL) => {
                    self.scroll_down(1)
                }
                (KeyCode::Down, _) | (KeyCode::Char('j'), KeyModifiers::CONTROL) => {
                    self.scroll_up(1)
                }
                (KeyCode::Enter, _) => self.handle_enter(),
                (KeyCode::Char('b'), KeyModifiers::CONTROL) | (KeyCode::PageUp, _) => {
                    self.scroll_down(20)
                }
                (KeyCode::Char('f'), KeyModifiers::CONTROL) | (KeyCode::PageDown, _) => {
                    self.scroll_up(20)
                }
                (KeyCode::Home, _) => self.select_last(),
                (KeyCode::End, _) => self.select_first(),
                // "/" opens command palette when search is empty
                (KeyCode::Char('/'), _) if self.search_input.text.is_empty() => {
                    self.open_command_palette();
                }
                // Text input handling
                _ => {
                    let text_before = self.search_input.text.clone();
                    if self.search_input.handle_key(key.code, key.modifiers) {
                        // Only trigger search if text actually changed (not just cursor movement)
                        if self.search_input.text != text_before {
                            self.last_input_time = Instant::now();
                            // In New mode, immediately clear results for responsive feedback
                            self.on_input_change_new_mode();
                        }
                    }
                }
            },
            _ => {}
        }
    }
}

/// Result from running the TUI
pub struct TuiResult {
    /// Component selected for download (New mode only)
    pub selected_component: Option<crate::component::ComponentSearchResult>,
}

/// Determine the preflight configuration based on auth and registry access
fn compute_preflight() -> Result<Preflight> {
    // Step 1: Check authentication (bail early if not authenticated)
    crate::auth::get_valid_token()?;

    // Step 2: Check if we have a cached registry index
    let db_path = RegistryClient::default_db_path()?;
    let has_cached_index = db_path.exists();

    if has_cached_index {
        // Cached index always works, even for non-admins (they just can't update)
        return Ok(Preflight {
            start_mode: SearchMode::Registry,
            spawn_registry_worker: true,
            registry_metadata: None, // Worker will fetch metadata for updates
        });
    }

    // Step 3: No cached index - check if we can download one
    match check_registry_access()? {
        RegistryAccessResult::Allowed(metadata) => {
            // Admin - can download index, pass pre-fetched metadata to avoid duplicate request
            Ok(Preflight {
                start_mode: SearchMode::Registry,
                spawn_registry_worker: true,
                registry_metadata: Some(metadata),
            })
        }
        RegistryAccessResult::Forbidden => {
            // Non-admin, no cached index - silently use component mode
            Ok(Preflight {
                start_mode: SearchMode::New,
                spawn_registry_worker: false,
                registry_metadata: None,
            })
        }
    }
}

/// Run the TUI application
pub fn run() -> Result<TuiResult> {
    let preflight = compute_preflight()?;

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        SetCursorStyle::BlinkingBar
    )?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(preflight);

    let result = run_loop(&mut terminal, &mut app);

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        SetCursorStyle::DefaultUserShape
    )?;
    terminal.show_cursor()?;

    result?;

    Ok(TuiResult {
        selected_component: app.selected_component_for_download,
    })
}

fn run_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> Result<()> {
    // Mode-specific debounce times
    // Registry: very short (worker coalesces), just batch keystrokes
    const REGISTRY_DEBOUNCE_MS: u64 = 5;
    // Component: longer since API calls are expensive
    const COMPONENT_DEBOUNCE_MS: u64 = 200;
    // Target ~120Hz refresh rate
    const FRAME_TIME: Duration = Duration::from_micros(8333);

    loop {
        let frame_start = Instant::now();

        // Drain all pending events first (lowest latency for input)
        let mut scroll_delta: isize = 0;
        let mut events_processed = 0usize;
        while event::poll(Duration::from_millis(0))? && events_processed < 100 {
            let ev = event::read()?;
            match &ev {
                Event::Mouse(mouse) => match mouse.kind {
                    MouseEventKind::ScrollDown => scroll_delta += 1,
                    MouseEventKind::ScrollUp => scroll_delta -= 1,
                    _ => app.handle_event(ev),
                },
                _ => app.handle_event(ev),
            }
            events_processed += 1;
            if app.should_quit {
                break;
            }
        }

        // Apply coalesced scroll (divide by 3 since each item is 3 lines tall)
        if !app.show_command_palette && scroll_delta != 0 {
            let scroll_amount = (scroll_delta.abs() / 3).clamp(1, 10) as u16;
            if scroll_delta > 0 {
                app.scroll_down(scroll_amount);
            } else {
                app.scroll_up(scroll_amount);
            }
        }

        if app.should_quit {
            break;
        }

        // Send search query after mode-specific debounce
        let debounce_ms = match app.mode {
            SearchMode::Registry => REGISTRY_DEBOUNCE_MS,
            SearchMode::New => COMPONENT_DEBOUNCE_MS,
        };
        if app.last_input_time.elapsed() > Duration::from_millis(debounce_ms) {
            app.maybe_send_registry_query();
            app.maybe_send_component_query();
        }

        // Poll for async results
        app.update_toast();
        app.poll_download();
        app.poll_results();
        app.poll_detail_responses();
        app.poll_fork_results();
        app.poll_component_results();

        // Render
        terminal.draw(|f| ui::render(f, app))?;

        // Wait for next event or frame timeout - poll() wakes immediately on input
        // unlike sleep() which ignores incoming events
        let remaining = FRAME_TIME.saturating_sub(frame_start.elapsed());
        if !remaining.is_zero() {
            let _ = event::poll(remaining);
        }
    }

    Ok(())
}
