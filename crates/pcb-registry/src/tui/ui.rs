//! UI rendering

use crate::RegistryPart;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols::border,
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame,
};
use std::time::Duration;

use super::app::App;

/// Render the entire UI
pub fn render(frame: &mut Frame, app: &mut App) {
    // Main horizontal split: left (results + search) and right (preview)
    let main_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(50), // Left: results panels + search
            Constraint::Percentage(50), // Right: preview panel
        ])
        .split(frame.area());

    // Left side: results panels stacked vertically, then search, toast, status
    let left_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(9),    // Results panels (stacked vertically)
            Constraint::Length(1), // Search input (single line, minimal)
            Constraint::Length(1), // Toast line
            Constraint::Length(1), // Status bar
        ])
        .split(main_chunks[0]);

    render_results_panels(frame, app, left_chunks[0]);
    render_search_input(frame, app, left_chunks[1]);
    render_toast(frame, app, left_chunks[2]);
    render_status_bar(frame, app, left_chunks[3]);

    // Right side: preview panel
    render_preview_panel(frame, app, main_chunks[1]);
}

/// Render the search input (minimal, no border)
fn render_search_input(frame: &mut Frame, app: &mut App, area: Rect) {
    app.textarea.set_block(Block::default());
    app.textarea.set_cursor_line_style(Style::default());
    app.textarea
        .set_cursor_style(Style::default().add_modifier(Modifier::REVERSED));

    // "Search" in yellow, thick bar separator, then input
    let prompt = Line::from(vec![
        Span::styled(
            " Search ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "┃",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::DIM),
        ),
    ]);
    let prompt_width = 10u16; // " Search ┃"

    let prompt_para = Paragraph::new(prompt);

    let prompt_area = Rect {
        x: area.x,
        y: area.y,
        width: prompt_width,
        height: 1,
    };

    let input_area = Rect {
        x: area.x + prompt_width,
        y: area.y,
        width: area.width.saturating_sub(prompt_width),
        height: 1,
    };

    frame.render_widget(prompt_para, prompt_area);
    frame.render_widget(&app.textarea, input_area);
}

/// Render the three results panels stacked vertically
fn render_results_panels(frame: &mut Frame, app: &App, area: Rect) {
    let panels = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Ratio(1, 3),
            Constraint::Ratio(1, 3),
            Constraint::Ratio(1, 3),
        ])
        .split(area);

    render_result_list(
        frame,
        "Trigram",
        &app.results.trigram,
        panels[0],
        Color::Yellow,
        None,
    );
    render_result_list(
        frame,
        "Word",
        &app.results.word,
        panels[1],
        Color::Green,
        None,
    );
    render_result_list(
        frame,
        "Merged",
        &app.results.merged,
        panels[2],
        Color::Magenta,
        Some(app.selected_index),
    );
}

/// Render a single results list panel with rounded borders
fn render_result_list(
    frame: &mut Frame,
    title: &str,
    parts: &[RegistryPart],
    area: Rect,
    color: Color,
    selected: Option<usize>,
) {
    let max_items = area.height.saturating_sub(2) as usize;

    let items: Vec<ListItem> = parts
        .iter()
        .enumerate()
        .take(max_items)
        .map(|(i, part)| {
            let is_selected = selected == Some(i);

            let mpn_style = if is_selected {
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .bg(Color::DarkGray)
                    .fg(Color::White)
            } else {
                Style::default().add_modifier(Modifier::BOLD)
            };

            let mfr_style = if is_selected {
                Style::default().bg(Color::DarkGray).fg(Color::Gray)
            } else {
                Style::default().fg(Color::DarkGray)
            };

            let mpn = Span::styled(&part.mpn, mpn_style);
            let mfr = part.manufacturer.as_deref().unwrap_or("");
            let mfr_span = Span::styled(format!(" ({})", mfr), mfr_style);

            if is_selected {
                let content_len = part.mpn.len() + mfr.len() + 3;
                let padding_len = (area.width as usize).saturating_sub(content_len + 2);
                let padding = Span::styled(
                    " ".repeat(padding_len),
                    Style::default().bg(Color::DarkGray),
                );
                ListItem::new(Line::from(vec![mpn, mfr_span, padding]))
            } else {
                ListItem::new(Line::from(vec![mpn, mfr_span]))
            }
        })
        .collect();

    let count = parts.len();
    let title_with_count = format!(" {} [{}] ", title, count);

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_set(border::ROUNDED)
            .border_style(Style::default().fg(color))
            .title(title_with_count),
    );

    frame.render_widget(list, area);
}

/// Render the preview panel showing selected part details
fn render_preview_panel(frame: &mut Frame, app: &App, area: Rect) {
    let selected_part = app.results.merged.get(app.selected_index);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Part Details ");

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if let Some(part) = selected_part {
        render_part_details(frame, app, part, inner);
    } else {
        let empty = Paragraph::new("No part selected")
            .style(Style::default().fg(Color::DarkGray))
            .alignment(ratatui::layout::Alignment::Center);
        frame.render_widget(empty, inner);
    }
}

/// Render detailed part information
fn render_part_details(frame: &mut Frame, app: &App, part: &RegistryPart, area: Rect) {
    let mut lines = Vec::new();
    let label_style = Style::default().fg(Color::DarkGray);
    let value_style = Style::default().fg(Color::White);
    let dim_style = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::ITALIC);

    // ═══════════════════════════════════════════
    // PART INFORMATION
    // ═══════════════════════════════════════════

    // MPN - prominent header
    lines.push(Line::from(vec![Span::styled(
        &part.mpn,
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )]));
    lines.push(Line::from("")); // Spacer

    // Manufacturer
    if let Some(ref mfr) = part.manufacturer {
        lines.push(Line::from(vec![
            Span::styled("Manufacturer  ", label_style),
            Span::styled(mfr, value_style),
        ]));
    }

    // Category & Type
    if let Some(ref cat) = part.category {
        lines.push(Line::from(vec![
            Span::styled("Category      ", label_style),
            Span::styled(cat, Style::default().fg(Color::Yellow)),
        ]));
    }

    if let Some(ref pt) = part.part_type {
        lines.push(Line::from(vec![
            Span::styled("Type          ", label_style),
            Span::styled(pt, Style::default().fg(Color::Green)),
        ]));
    }

    // Part ID
    lines.push(Line::from(vec![
        Span::styled("ID            ", label_style),
        Span::styled(part.id.to_string(), dim_style),
    ]));

    // Registry path
    lines.push(Line::from(vec![
        Span::styled("Path          ", label_style),
        Span::styled(&part.registry_path, dim_style),
    ]));

    // Description
    if let Some(ref desc) = part.short_description {
        lines.push(Line::from("")); // Spacer
        lines.push(Line::from(vec![Span::styled(
            "Description",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )]));
        let max_width = area.width.saturating_sub(2) as usize;
        for chunk in wrap_text(desc, max_width) {
            lines.push(Line::from(Span::styled(chunk, value_style)));
        }
    }

    // ═══════════════════════════════════════════
    // SEARCH SCORING
    // ═══════════════════════════════════════════

    lines.push(Line::from("")); // Spacer
    lines.push(Line::from(vec![Span::styled(
        "─── Search Scoring ───",
        Style::default().fg(Color::DarkGray),
    )]));

    // Get scoring for this part
    let scoring = app.results.scoring.get(&part.registry_path);

    // Helper to format position + rank compactly
    let format_index_result = |pos: Option<usize>, rank: Option<f64>, total: usize| -> Vec<Span> {
        match pos {
            Some(p) => {
                let mut spans = vec![
                    Span::styled(format!("#{}", p + 1), Style::default().fg(Color::White)),
                    Span::styled(format!("/{}", total), dim_style),
                ];
                if let Some(r) = rank {
                    spans.push(Span::styled(format!(" (rank {:.2})", r), dim_style));
                }
                spans
            }
            None => vec![Span::styled("—", dim_style)],
        }
    };

    // Trigram
    let (tri_pos, tri_rank) = scoring
        .map(|s| (s.trigram_position, s.trigram_rank))
        .unwrap_or((None, None));
    let mut tri_line = vec![
        Span::styled("Trigram ", Style::default().fg(Color::Yellow)),
    ];
    tri_line.extend(format_index_result(tri_pos, tri_rank, app.results.trigram.len()));
    lines.push(Line::from(tri_line));

    // Word
    let (word_pos, word_rank) = scoring
        .map(|s| (s.word_position, s.word_rank))
        .unwrap_or((None, None));
    let mut word_line = vec![
        Span::styled("Word    ", Style::default().fg(Color::Green)),
    ];
    word_line.extend(format_index_result(word_pos, word_rank, app.results.word.len()));
    lines.push(Line::from(word_line));

    // Merged
    let mut merged_line = vec![
        Span::styled("Merged  ", Style::default().fg(Color::Magenta)),
        Span::styled(
            format!("#{}", app.selected_index + 1),
            Style::default().fg(Color::White),
        ),
        Span::styled(format!("/{}", app.results.merged.len()), dim_style),
    ];
    // Source indicator
    let source = match (tri_pos, word_pos) {
        (Some(_), Some(_)) => " (both)",
        (Some(_), None) => " (trigram)",
        (None, Some(_)) => " (word)",
        (None, None) => "",
    };
    merged_line.push(Span::styled(source, dim_style));
    lines.push(Line::from(merged_line));

    let para = Paragraph::new(lines);
    frame.render_widget(para, area);
}

/// Simple text wrapping
fn wrap_text(text: &str, max_width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current_line = String::new();

    for word in text.split_whitespace() {
        if current_line.is_empty() {
            current_line = word.to_string();
        } else if current_line.len() + 1 + word.len() <= max_width {
            current_line.push(' ');
            current_line.push_str(word);
        } else {
            lines.push(current_line);
            current_line = word.to_string();
        }
    }

    if !current_line.is_empty() {
        lines.push(current_line);
    }

    lines
}

/// Render the toast notification line (dimmed blue text)
fn render_toast(frame: &mut Frame, app: &App, area: Rect) {
    if let Some(ref toast) = app.toast {
        let toast_text = Paragraph::new(format!(" {} ", toast.message))
            .style(Style::default().fg(Color::Blue));
        frame.render_widget(toast_text, area);
    }
}

/// Render the status bar
fn render_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let parts_count = app.parts_count;
    let query_time = format_duration(app.results.duration);
    let status_text = format!(
        " {} parts │ {} │ ↑↓ select │ Enter copy │ Esc quit",
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
