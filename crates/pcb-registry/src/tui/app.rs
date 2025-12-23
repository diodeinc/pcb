//! Main application state and event loop

use super::search::{spawn_worker, SearchQuery, SearchResults};
use super::ui;
use crate::RegistryClient;
use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io::{self, Stdout};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};
use tui_textarea::{Input, TextArea};

/// Application state
pub struct App<'a> {
    /// Search input textarea
    pub textarea: TextArea<'a>,
    /// Current search results
    pub results: SearchResults,
    /// Total parts count in registry
    pub parts_count: i64,
    /// Should quit?
    pub should_quit: bool,
    /// Query counter for deduplication
    query_counter: u64,
    /// Last query text (for change detection)
    last_query: String,
    /// Channel to send queries to worker
    query_tx: Sender<SearchQuery>,
    /// Channel to receive results from worker
    result_rx: Receiver<SearchResults>,
    /// Debounce timer
    last_input_time: Instant,
}

impl<'a> App<'a> {
    pub fn new(parts_count: i64) -> Self {
        let (query_tx, query_rx) = mpsc::channel::<SearchQuery>();
        let (result_tx, result_rx) = mpsc::channel::<SearchResults>();

        // Spawn worker thread
        spawn_worker(query_rx, result_tx);

        let mut textarea = TextArea::default();
        textarea.set_cursor_line_style(ratatui::style::Style::default());

        Self {
            textarea,
            results: SearchResults::default(),
            parts_count,
            should_quit: false,
            query_counter: 0,
            last_query: String::new(),
            query_tx,
            result_rx,
            last_input_time: Instant::now(),
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
            }
        }
    }

    /// Handle input event
    fn handle_event(&mut self, event: Event) {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Esc => self.should_quit = true,
                _ => {
                    self.textarea.input(Input::from(key));
                    self.last_input_time = Instant::now();
                }
            },
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
