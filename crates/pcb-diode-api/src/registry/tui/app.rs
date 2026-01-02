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
use tui_textarea::{Input, Key, TextArea};

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

/// Application state
pub struct App<'a> {
    /// Search input textarea
    pub textarea: TextArea<'a>,
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
}

impl<'a> App<'a> {
    pub fn new() -> Self {
        let (query_tx, query_rx) = mpsc::channel::<SearchQuery>();
        let (result_tx, result_rx) = mpsc::channel::<SearchResults>();
        let (download_tx, download_rx) = mpsc::channel::<DownloadProgress>();

        spawn_worker(query_rx, result_tx, download_tx);

        let mut textarea = TextArea::default();
        textarea.set_cursor_line_style(ratatui::style::Style::default());

        let clipboard = Clipboard::new().ok();
        let image_protocol = ImageProtocol::detect();
        let picker = if image_protocol.is_supported() {
            Picker::from_query_stdio().ok()
        } else {
            None
        };

        Self {
            textarea,
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
        }
    }

    /// Get current query text
    fn current_query(&self) -> String {
        self.textarea.lines().join("")
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

    /// Clear the current line in the textarea
    fn clear_line(&mut self) {
        self.textarea.move_cursor(tui_textarea::CursorMove::Head);
        self.textarea.delete_line_by_end();
        self.last_input_time = Instant::now();
    }

    /// Handle input event (mouse scroll handled separately in run_loop)
    fn handle_event(&mut self, event: Event) {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => match (key.code, key.modifiers) {
                (KeyCode::Esc, _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                    self.should_quit = true
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
                (KeyCode::Char('u' | 'w'), KeyModifiers::CONTROL) => self.clear_line(),
                _ => {
                    let input = Input::from(key);
                    if input.key != Key::Enter {
                        self.textarea.input(input);
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
