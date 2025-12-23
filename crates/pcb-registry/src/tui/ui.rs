//! UI rendering

use super::app::App;
use crate::RegistryPart;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame,
};
use std::time::Duration;

/// Render the entire UI
pub fn render(frame: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Search input
            Constraint::Min(10),   // Results panels
            Constraint::Length(1), // Status bar
        ])
        .split(frame.area());

    render_search_input(frame, app, chunks[0]);
    render_results_panels(frame, app, chunks[1]);
    render_status_bar(frame, app, chunks[2]);
}

/// Render the search input box
fn render_search_input(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Search ");

    app.textarea.set_block(block);
    app.textarea.set_cursor_line_style(Style::default());
    app.textarea
        .set_cursor_style(Style::default().add_modifier(Modifier::REVERSED));

    frame.render_widget(&app.textarea, area);
}

/// Render the three results panels side by side
fn render_results_panels(frame: &mut Frame, app: &App, area: Rect) {
    let panels = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(33),
            Constraint::Percentage(33),
            Constraint::Percentage(34),
        ])
        .split(area);

    render_result_list(
        frame,
        " Trigram ",
        &app.results.trigram,
        panels[0],
        Color::Yellow,
    );
    render_result_list(frame, " Word ", &app.results.word, panels[1], Color::Green);
    render_result_list(
        frame,
        " Merged ",
        &app.results.merged,
        panels[2],
        Color::Magenta,
    );
}

/// Render a single results list panel
fn render_result_list(
    frame: &mut Frame,
    title: &str,
    parts: &[RegistryPart],
    area: Rect,
    color: Color,
) {
    let items: Vec<ListItem> = parts
        .iter()
        .take(area.height.saturating_sub(2) as usize) // Account for borders
        .map(|part| {
            let mpn = Span::styled(&part.mpn, Style::default().add_modifier(Modifier::BOLD));
            let mfr = part.manufacturer.as_deref().unwrap_or("");
            let mfr_span =
                Span::styled(format!(" ({})", mfr), Style::default().fg(Color::DarkGray));
            ListItem::new(Line::from(vec![mpn, mfr_span]))
        })
        .collect();

    let count = parts.len();
    let title_with_count = format!("{} [{}]", title.trim(), count);

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(color))
            .title(title_with_count),
    );

    frame.render_widget(list, area);
}

/// Render the status bar
fn render_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let parts_count = app.parts_count;
    let query_time = format_duration(app.results.duration);
    let status_text = format!(
        " {} parts │ Query: {} │ Esc: quit │ Enter: select",
        parts_count, query_time
    );

    let status = Paragraph::new(status_text).style(Style::default().fg(Color::DarkGray));

    frame.render_widget(status, area);
}

fn format_duration(d: Duration) -> String {
    let micros = d.as_micros();
    if micros < 1000 {
        format!("{}µs", micros)
    } else {
        format!("{:.1}ms", micros as f64 / 1000.0)
    }
}
