//! Main application state and event loop

use super::super::download::DownloadProgress;
use super::image::ImageProtocol;
use super::search::{spawn_worker, SearchQuery, SearchResults};
use super::ui;
use crate::{RegistryClient, RegistryPart};
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
use ratatui::{backend::CrosstermBackend, widgets::ListState, Terminal};
use ratatui_image::picker::Picker;
use std::io::{self, Stdout};
use std::sync::mpsc::{self, Receiver, Sender};
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

    /// Move cursor left by one word
    pub fn move_word_left(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let s = &self.text[..self.cursor];
        let trimmed = s.trim_end();
        if trimmed.is_empty() {
            self.cursor = 0;
            return;
        }
        self.cursor = trimmed
            .char_indices()
            .rev()
            .find(|(_, c)| c.is_whitespace())
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(0);
    }

    /// Move cursor right by one word
    pub fn move_word_right(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        let s = &self.text[self.cursor..];
        let ws_count = s.chars().take_while(|c| c.is_whitespace()).count();
        let ws_bytes: usize = s.chars().take(ws_count).map(|c| c.len_utf8()).sum();
        let after = &s[ws_bytes..];
        let word_bytes: usize = after
            .chars()
            .take_while(|c| !c.is_whitespace())
            .map(|c| c.len_utf8())
            .sum();
        self.cursor += ws_bytes + word_bytes;
    }

    /// Delete word before cursor
    pub fn delete_word_before(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let s = &self.text[..self.cursor];
        let trimmed = s.trim_end();
        if trimmed.is_empty() {
            self.text.drain(..self.cursor);
            self.cursor = 0;
            return;
        }
        let start = trimmed
            .char_indices()
            .rev()
            .find(|(_, c)| c.is_whitespace())
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
    ToggleDebugPanels,
    UpdateRegistryIndex,
    OpenInDigikey,
    CopyRegistryPath,
}

impl Command {
    pub const ALL: &'static [Command] = &[
        Command::ToggleDebugPanels,
        Command::UpdateRegistryIndex,
        Command::OpenInDigikey,
        Command::CopyRegistryPath,
    ];

    /// Short machine-readable name
    pub fn name(&self) -> &'static str {
        match self {
            Command::ToggleDebugPanels => "toggle-debug-panels",
            Command::UpdateRegistryIndex => "update-registry-index",
            Command::OpenInDigikey => "open-in-digikey",
            Command::CopyRegistryPath => "copy-registry-path",
        }
    }

    /// Human-readable description
    pub fn description(&self) -> &'static str {
        match self {
            Command::ToggleDebugPanels => {
                "Show or hide the Trigram, Word, and Semantic search result panels"
            }
            Command::UpdateRegistryIndex => "Force re-download the registry index",
            Command::OpenInDigikey => "Open the selected part on Digikey",
            Command::CopyRegistryPath => "Copy the selected part's registry path to clipboard",
        }
    }

    /// Check if command matches a fuzzy query
    pub fn matches(&self, query: &str) -> bool {
        if query.is_empty() {
            return true;
        }
        let query = query.to_lowercase();
        let name = self.name().to_lowercase();
        let desc = self.description().to_lowercase();

        // Simple fuzzy: all query chars appear in order
        let mut chars = query.chars().peekable();
        for c in name.chars().chain(std::iter::once(' ')).chain(desc.chars()) {
            if chars.peek() == Some(&c) {
                chars.next();
            }
            if chars.peek().is_none() {
                return true;
            }
        }
        false
    }
}

/// Application state
pub struct App {
    /// Search input
    pub search_input: TextInput,
    /// Current search results
    pub results: SearchResults,
    /// List state for merged results (handles selection + scroll)
    pub list_state: ListState,
    /// Total parts count in registry (0 until index is ready)
    pub parts_count: i64,
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
    /// Debounce timer
    last_input_time: Instant,
    /// Clipboard handle
    clipboard: Option<Clipboard>,
    /// Detected image protocol support
    pub image_protocol: ImageProtocol,
    /// Image picker for decoding (None if not supported)
    pub picker: Option<Picker>,
    /// Cached selected part details (fetched on-demand)
    pub selected_part: Option<RegistryPart>,
    /// ID of the cached selected part
    selected_part_id: Option<i64>,
    /// Registry client for fetching part details
    registry_client: Option<RegistryClient>,
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
}

impl App {
    pub fn new() -> Self {
        let (query_tx, query_rx) = mpsc::channel::<SearchQuery>();
        let (result_tx, result_rx) = mpsc::channel::<SearchResults>();
        let (download_tx, download_rx) = mpsc::channel::<DownloadProgress>();

        spawn_worker(query_rx, result_tx, download_tx);

        let clipboard = Clipboard::new().ok();
        let image_protocol = ImageProtocol::detect();
        let picker = if image_protocol.is_supported() {
            Picker::from_query_stdio().ok()
        } else {
            None
        };

        Self {
            search_input: TextInput::new(),
            results: SearchResults::default(),
            list_state: ListState::default(),
            parts_count: 0,
            should_quit: false,
            toast: None,
            download_state: DownloadState::NotStarted,
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
            selected_part_id: None,
            registry_client: None,
            show_command_palette: false,
            command_palette_index: 0,
            command_palette_input: TextInput::new(),
            command_palette_filtered: Command::ALL.to_vec(),
            show_debug_panels: false,
        }
    }

    /// Get current query text
    fn current_query(&self) -> String {
        self.search_input.text.clone()
    }

    /// Check if query changed and send to worker if so
    fn maybe_send_query(&mut self) {
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

    /// Get the selected index (0-indexed into merged results)
    pub fn selected_index(&self) -> usize {
        self.list_state.selected().unwrap_or(0)
    }

    /// Poll for download progress from worker (non-blocking)
    fn poll_download(&mut self) {
        while let Ok(progress) = self.download_rx.try_recv() {
            match &progress {
                // Completed successfully
                DownloadProgress {
                    done: true,
                    error: None,
                    is_update: false,
                    ..
                } => {
                    self.download_state = DownloadState::Done;
                    if let Ok(client) = RegistryClient::open() {
                        self.parts_count = client.count().unwrap_or(0);
                    }
                    // Only show toast if we were actually downloading
                    if matches!(self.download_state, DownloadState::Downloading { .. }) {
                        self.toast = Some(Toast::new(
                            "Index ready".to_string(),
                            Duration::from_secs(2),
                        ));
                    }
                    self.last_query.clear();
                }
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
                        self.parts_count = client.count().unwrap_or(0);
                    }
                    self.last_query.clear();
                }
                // Failed
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
                DownloadProgress {
                    done: true,
                    error: Some(e),
                    is_update: true,
                    ..
                } => {
                    // Update failed, but we still have the old DB working
                    self.download_state = DownloadState::Done;
                    self.toast = Some(Toast::error(e.clone(), Duration::from_secs(5)));
                }
                // In progress - downloading
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
                self.results = results;

                if self.results.query_id != self.last_results_id {
                    // Reset selection and scroll offset for new results
                    if self.results.merged.is_empty() {
                        self.list_state = ListState::default(); // No selection for empty results
                    } else {
                        self.list_state = ListState::default().with_selected(Some(0));
                    }
                    self.last_results_id = self.results.query_id;
                    // Clear cached part on new results
                    self.selected_part = None;
                    self.selected_part_id = None;
                }
            }
        }
    }

    /// Fetch details for the currently selected part if not already cached
    fn maybe_fetch_details(&mut self) {
        if let Some(hit) = self.results.merged.get(self.selected_index()) {
            let current_id = hit.id;
            if self.selected_part_id != Some(current_id) {
                self.selected_part_id = Some(current_id);
                // Lazily open client if needed
                if self.registry_client.is_none() {
                    self.registry_client = RegistryClient::open().ok();
                }
                // Fetch synchronously (fast SQLite lookup)
                self.selected_part = self
                    .registry_client
                    .as_ref()
                    .and_then(|c| c.get_part_by_id(current_id).ok().flatten());
            }
        }
    }

    /// Move selection up by n items (toward index 0 = best matches at bottom of display)
    fn scroll_up(&mut self, n: u16) {
        if self.results.merged.is_empty() {
            return;
        }
        let current = self.list_state.selected().unwrap_or(0);
        let new_index = current.saturating_sub(n as usize);
        self.list_state.select(Some(new_index));
    }

    /// Move selection down by n items (toward higher indices = worse matches at top of display)
    fn scroll_down(&mut self, n: u16) {
        if self.results.merged.is_empty() {
            return;
        }
        let current = self.list_state.selected().unwrap_or(0);
        let max_index = self.results.merged.len().saturating_sub(1);
        let new_index = current.saturating_add(n as usize).min(max_index);
        self.list_state.select(Some(new_index));
    }

    /// Jump to first result (index 0 = best match, displayed at bottom)
    fn select_first(&mut self) {
        if !self.results.merged.is_empty() {
            self.list_state.select(Some(0));
        }
    }

    /// Jump to last result (highest index = worst match, displayed at top)
    fn select_last(&mut self) {
        if !self.results.merged.is_empty() {
            self.list_state.select(Some(self.results.merged.len() - 1));
        }
    }

    /// Copy selected item's MPN to clipboard
    fn copy_selected(&mut self) {
        if let Some(part) = self.results.merged.get(self.selected_index()) {
            let mpn = part.mpn.clone();

            if let Some(ref mut clipboard) = self.clipboard {
                if clipboard.set_text(&mpn).is_ok() {
                    self.toast = Some(Toast::new(
                        format!("Copied: {}", mpn),
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

    /// Clear expired toast
    fn update_toast(&mut self) {
        if let Some(ref toast) = self.toast {
            if toast.is_expired() {
                self.toast = None;
            }
        }
    }

    /// Execute a command from the palette
    fn execute_command(&mut self, cmd: Command) {
        match cmd {
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
            Command::CopyRegistryPath => {
                if let Some(ref part) = self.selected_part {
                    let path = part.registry_path.clone();
                    if let Some(ref mut clipboard) = self.clipboard {
                        if clipboard.set_text(&path).is_ok() {
                            self.toast = Some(Toast::new(
                                format!("Copied: {}", path),
                                Duration::from_secs(2),
                            ));
                        } else {
                            self.toast = Some(Toast::error(
                                "Failed to copy to clipboard".to_string(),
                                Duration::from_secs(2),
                            ));
                        }
                    } else {
                        self.toast = Some(Toast::error(
                            "Clipboard not available".to_string(),
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
        }
    }

    /// Update filtered commands based on query
    fn update_command_filter(&mut self) {
        self.command_palette_filtered = Command::ALL
            .iter()
            .copied()
            .filter(|cmd| cmd.matches(&self.command_palette_input.text))
            .collect();
        // Reset selection if out of bounds
        if self.command_palette_index >= self.command_palette_filtered.len() {
            self.command_palette_index = 0;
        }
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
                        (KeyCode::Up, _) => {
                            if self.command_palette_index > 0 {
                                self.command_palette_index -= 1;
                            }
                        }
                        (KeyCode::Down, _) => {
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
                                self.close_command_palette();
                                self.execute_command(cmd);
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
                (KeyCode::Up, _) | (KeyCode::Char('k'), KeyModifiers::CONTROL) => {
                    self.scroll_down(1)
                }
                (KeyCode::Down, _) | (KeyCode::Char('j'), KeyModifiers::CONTROL) => {
                    self.scroll_up(1)
                }
                (KeyCode::Enter, _) => self.copy_selected(),
                (KeyCode::Char('b'), KeyModifiers::CONTROL) | (KeyCode::PageUp, _) => {
                    self.scroll_down(20)
                }
                (KeyCode::Char('f'), KeyModifiers::CONTROL) | (KeyCode::PageDown, _) => {
                    self.scroll_up(20)
                }
                (KeyCode::Home, _) => self.select_last(),
                (KeyCode::End, _) => self.select_first(),
                // Text input handling
                _ => {
                    if self.search_input.handle_key(key.code, key.modifiers) {
                        self.last_input_time = Instant::now();
                    }
                }
            },
            _ => {}
        }
    }
}

/// Run the TUI application
pub fn run() -> Result<()> {
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

    let mut app = App::new();

    let result = run_loop(&mut terminal, &mut app);

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        SetCursorStyle::DefaultUserShape
    )?;
    terminal.show_cursor()?;

    result
}

fn run_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> Result<()> {
    const DEBOUNCE_MS: u64 = 50;

    loop {
        app.update_toast();
        app.poll_download();
        app.poll_results();

        // Drain all pending events, coalescing scroll events into net delta
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
        let scroll_amount = (scroll_delta.abs() / 3).max(1).min(10) as u16;
        if scroll_delta > 0 {
            app.scroll_down(scroll_amount);
        } else if scroll_delta < 0 {
            app.scroll_up(scroll_amount);
        }

        if app.should_quit {
            break;
        }

        app.maybe_fetch_details();
        terminal.draw(|f| ui::render(f, app))?;

        if app.last_input_time.elapsed() > Duration::from_millis(DEBOUNCE_MS) {
            app.maybe_send_query();
        }

        // Sleep when idle to avoid busy-spin
        if events_processed == 0 {
            std::thread::sleep(Duration::from_millis(16));
        }
    }

    Ok(())
}
