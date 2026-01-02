//! UI rendering

use crate::RegistryPart;
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols::border,
    text::{Line, Span},
    widgets::{Block, Borders, List, ListDirection, ListItem, Paragraph, StatefulWidget},
    Frame,
};
use ratatui_image::StatefulImage;
use std::time::{Duration, Instant};

use super::app::{App, DownloadState};
use super::image::decode_image;

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

    // Left side: results panels stacked vertically, then status, search
    let left_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(9),    // Results panels (stacked vertically)
            Constraint::Length(1), // Results count + query time
            Constraint::Length(1), // Status bar (with toast at end)
            Constraint::Length(1), // Search input (single line, minimal)
        ])
        .split(main_chunks[0]);

    render_results_panels(frame, app, left_chunks[0]);
    render_results_count(frame, app, left_chunks[1]);
    render_status_bar(frame, app, left_chunks[2]);
    render_search_input(frame, app, left_chunks[3]);

    // Right side: preview panel
    render_preview_panel(frame, app, main_chunks[1]);
}

/// Render the search input (minimal, thick bar on left)
fn render_search_input(frame: &mut Frame, app: &mut App, area: Rect) {
    app.textarea.set_block(Block::default());
    app.textarea.set_cursor_line_style(Style::default());
    app.textarea
        .set_cursor_style(Style::default().add_modifier(Modifier::REVERSED));

    // Thick bar on left, then space, then input
    let prompt = Span::styled("▌ ", Style::default().fg(Color::Yellow));
    let prompt_width = 2u16;

    let prompt_para = Paragraph::new(Line::from(prompt));

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

/// Render the results panels: Trigram/Word on top, Merged below
fn render_results_panels(frame: &mut Frame, app: &mut App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(40), // Trigram + Word
            Constraint::Percentage(60), // Merged (larger)
        ])
        .split(area);

    // Trigram and Word side by side, dimmed
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[0]);

    render_result_list(
        frame,
        "Trigram",
        &app.results.trigram,
        cols[0],
        Color::Yellow,
        None,
        true, // dimmed
    );
    render_result_list(
        frame,
        "Word",
        &app.results.word,
        cols[1],
        Color::Green,
        None,
        true, // dimmed
    );

    // Merged panel with magenta border, bottom-up display (best match at bottom)
    render_merged_list(frame, app, rows[1]);
}

/// Render results count + query time line (subtle)
fn render_results_count(frame: &mut Frame, app: &App, area: Rect) {
    let count = app.results.merged.len();
    let query_time = format_duration(app.results.duration);

    let line = if count == 0 {
        Line::from(vec![Span::styled(
            format!("  0/{}", app.parts_count),
            Style::default().fg(Color::DarkGray),
        )])
    } else {
        Line::from(vec![
            Span::styled(
                format!("  {}/{} ", count, app.parts_count),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                format!("({})", query_time),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::DIM),
            ),
        ])
    };

    let para = Paragraph::new(line);
    frame.render_widget(para, area);
}

/// Render a simple results list panel (for Trigram/Word - no selection, dimmed)
fn render_result_list(
    frame: &mut Frame,
    title: &str,
    parts: &[RegistryPart],
    area: Rect,
    color: Color,
    _selected: Option<usize>,
    dimmed: bool,
) {
    let score_style = Style::default().fg(Color::DarkGray);

    let items: Vec<ListItem> = parts
        .iter()
        .map(|part| {
            let mpn = Span::styled(&part.mpn, Style::default().fg(Color::White));
            let prefix_span = if let Some(rank) = part.rank {
                Span::styled(format!("{:>7.2} ", rank), score_style)
            } else {
                Span::styled("        ", score_style)
            };
            let mfr = part.manufacturer.as_deref().unwrap_or("");
            let mfr_span =
                Span::styled(format!(" ({})", mfr), Style::default().fg(Color::DarkGray));
            ListItem::new(Line::from(vec![prefix_span, mpn, mfr_span]))
        })
        .collect();

    let border_style = if dimmed {
        Style::default().fg(color).add_modifier(Modifier::DIM)
    } else {
        Style::default().fg(color)
    };

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_set(border::ROUNDED)
            .border_style(border_style)
            .title(format!(" {} ", title)),
    );

    frame.render_widget(list, area);
}

/// Render the merged results list with selection and auto-scrolling
fn render_merged_list(frame: &mut Frame, app: &mut App, area: Rect) {
    let selection_bg = Color::Rgb(38, 38, 38);
    let selected_index = app.list_state.selected();

    let items: Vec<ListItem> = app
        .results
        .merged
        .iter()
        .enumerate()
        .map(|(i, part)| {
            let is_selected = selected_index == Some(i);

            let mpn_style = if is_selected {
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .bg(selection_bg)
                    .fg(Color::White)
            } else {
                Style::default().fg(Color::White)
            };

            let mfr_style = if is_selected {
                Style::default().bg(selection_bg).fg(Color::DarkGray)
            } else {
                Style::default().fg(Color::DarkGray)
            };

            let prefix_span = if is_selected {
                Span::styled("▌ ", Style::default().fg(Color::LightRed).bg(selection_bg))
            } else {
                Span::styled("  ", Style::default())
            };

            let mpn = Span::styled(&part.mpn, mpn_style);
            let mfr = part.manufacturer.as_deref().unwrap_or("");
            let mfr_span = Span::styled(format!(" ({})", mfr), mfr_style);

            ListItem::new(Line::from(vec![prefix_span, mpn, mfr_span]))
        })
        .collect();

    let list = List::new(items).direction(ListDirection::BottomToTop);

    StatefulWidget::render(list, area, frame.buffer_mut(), &mut app.list_state);
}

/// Render the preview panel showing selected part details
fn render_preview_panel(frame: &mut Frame, app: &mut App, area: Rect) {
    let selected_part = app.results.merged.get(app.selected_index());

    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(Color::Gray).add_modifier(Modifier::DIM))
        .title(" Part Details ");

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if let Some(part) = selected_part.cloned() {
        // Add 2 char left padding
        let padded = Rect {
            x: inner.x + 2,
            y: inner.y,
            width: inner.width.saturating_sub(2),
            height: inner.height,
        };
        render_part_details(frame, app, &part, padded);
    } else {
        let empty = Paragraph::new("No part selected")
            .style(Style::default().fg(Color::DarkGray))
            .alignment(Alignment::Center);
        frame.render_widget(empty, inner);
    }
}

/// Render part image in the preview panel (decodes from embedded image_data)
fn render_part_image(frame: &mut Frame, app: &App, part: &RegistryPart, area: Rect) {
    let image_data = match &part.image_data {
        Some(data) if !data.is_empty() => data,
        _ => return, // No image data, render nothing
    };

    let picker = match &app.picker {
        Some(p) => p,
        None => return, // No picker available
    };

    if let Some(mut protocol) = decode_image(image_data, picker) {
        let image_widget = StatefulImage::default();
        frame.render_stateful_widget(image_widget, area, &mut protocol);
    }
}

/// Render detailed part information with image inline before description
fn render_part_details(frame: &mut Frame, app: &mut App, part: &RegistryPart, area: Rect) {
    let label_style = Style::default().fg(Color::DarkGray);
    let value_style = Style::default().fg(Color::White);
    let dim_style = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::ITALIC);

    // ═══════════════════════════════════════════
    // HEADER SECTION (above image)
    // ═══════════════════════════════════════════
    let mut header_lines = Vec::new();

    // MPN - prominent header
    header_lines.push(Line::from(vec![Span::styled(
        &part.mpn,
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )]));
    header_lines.push(Line::from("")); // Spacer

    // Manufacturer
    if let Some(ref mfr) = part.manufacturer {
        header_lines.push(Line::from(vec![
            Span::styled("Manufacturer  ", label_style),
            Span::styled(mfr, value_style),
        ]));
    }

    // Category & Type (nested tree style)
    if let Some(ref cat) = part.category {
        let cat_parts: Vec<&str> = cat.split(" > ").collect();
        for (i, cat_part) in cat_parts.iter().enumerate() {
            if i == 0 {
                header_lines.push(Line::from(vec![
                    Span::styled("Category      ", label_style),
                    Span::styled(*cat_part, Style::default().fg(Color::Yellow)),
                ]));
            } else {
                let indent = "   ".repeat(i - 1);
                header_lines.push(Line::from(vec![
                    Span::styled(format!("              {}└─ ", indent), label_style),
                    Span::styled(*cat_part, Style::default().fg(Color::Yellow)),
                ]));
            }
        }
    }

    if let Some(ref pt) = part.part_type {
        header_lines.push(Line::from(vec![
            Span::styled("Type          ", label_style),
            Span::styled(pt, Style::default().fg(Color::Green)),
        ]));
    }

    // Registry path
    header_lines.push(Line::from(vec![
        Span::styled("Path          ", label_style),
        Span::styled(&part.registry_path, dim_style),
    ]));
    header_lines.push(Line::from("")); // Spacer after header section

    let header_height = header_lines.len() as u16;

    // Check if we have an image to show
    let has_image = app.image_protocol.is_supported()
        && app.picker.is_some()
        && part.image_data.as_ref().map_or(false, |d| !d.is_empty());

    // Calculate layout: header, optional image+spacing, rest
    let image_height: u16 = if has_image { 10 } else { 0 };
    let spacing_height: u16 = if has_image { 1 } else { 0 };

    // Split area into sections
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header_height),
            Constraint::Length(image_height),
            Constraint::Length(spacing_height),
            Constraint::Min(1),
        ])
        .split(area);

    let header_area = chunks[0];
    let image_area = chunks[1];
    let rest_area = chunks[3];

    // Render header
    let header_para = Paragraph::new(header_lines);
    frame.render_widget(header_para, header_area);

    // Render image if applicable
    if has_image {
        render_part_image(frame, app, part, image_area);
    }

    // ═══════════════════════════════════════════
    // REST SECTION (below image)
    // ═══════════════════════════════════════════
    let mut lines = Vec::new();

    // Description - prefer detailed_description, fallback to short_description
    let description = part
        .detailed_description
        .as_ref()
        .or(part.short_description.as_ref());

    if let Some(desc) = description {
        lines.push(Line::from(vec![Span::styled(
            "Description",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )]));
        let max_width = rest_area.width.saturating_sub(2) as usize;
        for chunk in wrap_text(desc, max_width) {
            lines.push(Line::from(Span::styled(chunk, value_style)));
        }
    }

    // ═══════════════════════════════════════════
    // PARAMETERS (from Digikey)
    // ═══════════════════════════════════════════

    if let Some(ref dk) = part.digikey {
        if !dk.parameters.is_empty() {
            lines.push(Line::from("")); // Spacer
            lines.push(Line::from(vec![Span::styled(
                "─── Parameters ───",
                Style::default().fg(Color::DarkGray),
            )]));

            // Show important parameters first, limit total shown
            let priority_keys = [
                "Capacitance",
                "Resistance",
                "Inductance",
                "Voltage - Rated",
                "Current - Output",
                "Current Rating",
                "Power (Watts)",
                "Tolerance",
                "Package / Case",
                "Mounting Type",
                "Operating Temperature",
            ];

            let mut shown = std::collections::HashSet::new();
            let max_params = 8;
            let key_width = 28;
            let mut count = 0;

            // Truncate key to fit column width
            let format_key = |key: &str| -> String {
                if key.len() > key_width {
                    format!("{:<width$}", &key[..key_width], width = key_width)
                } else {
                    format!("{:<width$}", key, width = key_width)
                }
            };

            // Priority keys first
            for key in priority_keys.iter() {
                if count >= max_params {
                    break;
                }
                if let Some(value) = dk.parameters.get(*key) {
                    lines.push(Line::from(vec![
                        Span::styled(format_key(key), label_style),
                        Span::styled(value, value_style),
                    ]));
                    shown.insert(*key);
                    count += 1;
                }
            }

            // Then other parameters
            for (key, value) in &dk.parameters {
                if count >= max_params {
                    break;
                }
                if !shown.contains(key.as_str()) {
                    lines.push(Line::from(vec![
                        Span::styled(format_key(key), label_style),
                        Span::styled(value, value_style),
                    ]));
                    count += 1;
                }
            }
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
    let mut tri_line = vec![Span::styled("Trigram ", Style::default().fg(Color::Yellow))];
    tri_line.extend(format_index_result(
        tri_pos,
        tri_rank,
        app.results.trigram.len(),
    ));
    lines.push(Line::from(tri_line));

    // Word
    let (word_pos, word_rank) = scoring
        .map(|s| (s.word_position, s.word_rank))
        .unwrap_or((None, None));
    let mut word_line = vec![Span::styled("Word    ", Style::default().fg(Color::Green))];
    word_line.extend(format_index_result(
        word_pos,
        word_rank,
        app.results.word.len(),
    ));
    lines.push(Line::from(word_line));

    // Merged
    let mut merged_line = vec![
        Span::styled("Merged  ", Style::default().fg(Color::Magenta)),
        Span::styled(
            format!("#{}", app.selected_index() + 1),
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
    frame.render_widget(para, rest_area);
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

/// Render the status bar (with download status and toast)
fn render_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let dim = Style::default().fg(Color::DarkGray);
    let dim_blue = Style::default().fg(Color::Blue).add_modifier(Modifier::DIM);
    let dim_yellow = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::DIM);
    let bracket = Style::default().fg(Color::DarkGray);

    let mut spans = vec![
        Span::styled("  [", bracket),
        Span::styled("↑↓/^jk select", dim),
        Span::styled("] [", bracket),
        Span::styled("Enter copy", dim),
        Span::styled("] [", bracket),
        Span::styled("Esc quit", dim),
        Span::styled("]", bracket),
    ];

    match &app.download_state {
        DownloadState::NotStarted => {
            spans.push(Span::styled(" [", bracket));
            spans.push(Span::styled("⠋ Initializing...", dim_yellow));
            spans.push(Span::styled("]", bracket));
        }
        DownloadState::Downloading { pct, started_at } => {
            let spinner = spinner_frame(*started_at);
            let pct_text = pct.map(|p| format!(" {}%", p)).unwrap_or_default();
            spans.push(Span::styled(" [", bracket));
            spans.push(Span::styled(
                format!("{} Downloading{}", spinner, pct_text),
                dim_yellow,
            ));
            spans.push(Span::styled("]", bracket));
        }
        DownloadState::Updating { pct, started_at } => {
            let spinner = spinner_frame(*started_at);
            let pct_text = pct.map(|p| format!(" {}%", p)).unwrap_or_default();
            spans.push(Span::styled(" [", bracket));
            spans.push(Span::styled(
                format!("{} Updating{}", spinner, pct_text),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM),
            ));
            spans.push(Span::styled("]", bracket));
        }
        DownloadState::Failed(msg) => {
            spans.push(Span::styled(" [", bracket));
            spans.push(Span::styled(
                format!("✗ {}", msg),
                Style::default().fg(Color::Red).add_modifier(Modifier::DIM),
            ));
            spans.push(Span::styled("]", bracket));
        }
        DownloadState::Done => {}
    }

    if let Some(ref toast) = app.toast {
        let toast_style = if toast.is_error {
            Style::default().fg(Color::Red).add_modifier(Modifier::DIM)
        } else {
            dim_blue
        };
        spans.push(Span::styled(" [", bracket));
        spans.push(Span::styled(&toast.message, toast_style));
        spans.push(Span::styled("]", bracket));
    }

    let status = Paragraph::new(Line::from(spans));
    frame.render_widget(status, area);
}

fn spinner_frame(started_at: Instant) -> &'static str {
    const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let elapsed = started_at.elapsed().as_millis() / 80;
    let idx = (elapsed as usize) % FRAMES.len();
    FRAMES[idx]
}

fn format_duration(d: Duration) -> String {
    let micros = d.as_micros();
    if micros < 1000 {
        format!("{}µs", micros)
    } else {
        format!("{:.1}ms", micros as f64 / 1000.0)
    }
}
