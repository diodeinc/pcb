//! Main application state and event loop

use super::search::{spawn_worker, SearchQuery, SearchResults};
use super::ui;
use crate::RegistryClient;
use anyhow::Result;
use arboard::Clipboard;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io::{self, Stdout};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};
use tui_textarea::{Input, Key, TextArea};

/// Toast notification state
pub struct Toast {
    pub message: String,
    pub expires_at: Instant,
}

impl Toast {
    pub fn new(message: String, duration: Duration) -> Self {
        Self {
            message,
            expires_at: Instant::now() + duration,
        }
    }

    pub fn is_expired(&self) -> bool {
        Instant::now() >= self.expires_at
    }
}

/// Application state
pub struct App<'a> {
    /// Search input textarea
    pub textarea: TextArea<'a>,
    /// Current search results
    pub results: SearchResults,
    /// Selected index in merged results (0-indexed)
    pub selected_index: usize,
    /// Total parts count in registry
    pub parts_count: i64,
    /// Should quit?
    pub should_quit: bool,
    /// Toast notification
    pub toast: Option<Toast>,
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
    /// Debounce timer
    last_input_time: Instant,
    /// Clipboard handle
    clipboard: Option<Clipboard>,
}

impl<'a> App<'a> {
    pub fn new(parts_count: i64) -> Self {
        let (query_tx, query_rx) = mpsc::channel::<SearchQuery>();
        let (result_tx, result_rx) = mpsc::channel::<SearchResults>();

        // Spawn worker thread
        spawn_worker(query_rx, result_tx);

        let mut textarea = TextArea::default();
        textarea.set_cursor_line_style(ratatui::style::Style::default());

        // Try to initialize clipboard (may fail on headless systems)
        let clipboard = Clipboard::new().ok();

        Self {
            textarea,
            results: SearchResults::default(),
            selected_index: 0,
            parts_count,
            should_quit: false,
            toast: None,
            query_counter: 0,
            last_query: String::new(),
            last_results_id: 0,
            query_tx,
            result_rx,
            last_input_time: Instant::now(),
            clipboard,
        }
    }

    /// Get current query text
    fn current_query(&self) -> String {
        self.textarea.lines().join("")
    }

    /// Check if query changed and send to worker if so
    fn maybe_send_query(&mut self) {
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

    /// Poll for results from worker (non-blocking)
    fn poll_results(&mut self) {
        while let Ok(results) = self.result_rx.try_recv() {
            // Only accept results for the latest query
            if results.query_id == self.query_counter {
                self.results = results;

                // Reset selection when results change
                if self.results.query_id != self.last_results_id {
                    self.selected_index = 0;
                    self.last_results_id = self.results.query_id;
                }
            }
        }
    }

    /// Move selection up
    fn select_prev(&mut self) {
        if !self.results.merged.is_empty() && self.selected_index > 0 {
            self.selected_index -= 1;
        }
    }

    /// Move selection down
    fn select_next(&mut self) {
        if !self.results.merged.is_empty() && self.selected_index < self.results.merged.len() - 1 {
            self.selected_index += 1;
        }
    }

    /// Copy selected item's MPN to clipboard
    fn copy_selected(&mut self) {
        if let Some(part) = self.results.merged.get(self.selected_index) {
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

    /// Handle input event
    fn handle_event(&mut self, event: Event) {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                match (key.code, key.modifiers) {
                    (KeyCode::Esc, _) => self.should_quit = true,
                    (KeyCode::Char('c'), KeyModifiers::CONTROL) => self.should_quit = true,
                    (KeyCode::Up, _) | (KeyCode::Char('k'), KeyModifiers::CONTROL) => {
                        self.select_prev()
                    }
                    (KeyCode::Down, _) | (KeyCode::Char('j'), KeyModifiers::CONTROL) => {
                        self.select_next()
                    }
                    (KeyCode::Enter, _) => self.copy_selected(),
                    // Ctrl+U (unix kill-line) clears the line
                    (KeyCode::Char('u'), KeyModifiers::CONTROL) => self.clear_line(),
                    // Cmd+Backspace / Ctrl+Backspace / Alt+Backspace clears the line
                    (KeyCode::Backspace, m)
                        if m.contains(KeyModifiers::SUPER)
                            || m.contains(KeyModifiers::CONTROL)
                            || m.contains(KeyModifiers::ALT) =>
                    {
                        self.clear_line()
                    }
                    _ => {
                        // Convert to tui-textarea input, but filter out Enter
                        let input = Input::from(key);
                        if input.key != Key::Enter {
                            self.textarea.input(input);
                            self.last_input_time = Instant::now();
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Run the TUI application
pub fn run() -> Result<()> {
    // Get parts count before entering TUI
    let parts_count = RegistryClient::open()?.count().unwrap_or(0);

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app
    let mut app = App::new(parts_count);

    // Main loop
    let result = run_loop(&mut terminal, &mut app);

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn run_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> Result<()> {
    const DEBOUNCE_MS: u64 = 50;

    loop {
        // Update toast expiry
        app.update_toast();

        // Render
        terminal.draw(|f| ui::render(f, app))?;

        // Poll for results from worker
        app.poll_results();

        // Check if we should send query (debounced)
        if app.last_input_time.elapsed() > Duration::from_millis(DEBOUNCE_MS) {
            app.maybe_send_query();
        }

        // Handle input events (with timeout so we can poll results)
        if event::poll(Duration::from_millis(16))? {
            let event = event::read()?;
            app.handle_event(event);
        }

        if app.should_quit {
            break;
        }
    }

    Ok(())
}
