//! UI rendering

use crate::{RegistryPart, SearchHit};
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

    // Command palette overlay (rendered last, on top)
    if app.show_command_palette {
        render_command_palette(frame, app);
    }
}

/// Render the search input (minimal, thick bar on left)
fn render_search_input(frame: &mut Frame, app: &App, area: Rect) {
    let cursor_style = Style::default().fg(Color::White).bg(Color::DarkGray);
    let text_style = Style::default().fg(Color::White);

    // Split the input at cursor position
    let (before, after) = app.search_input.text.split_at(app.search_input.cursor);
    let cursor_char = after.chars().next();
    let after_cursor = if cursor_char.is_some() {
        &after[cursor_char.unwrap().len_utf8()..]
    } else {
        ""
    };

    let mut spans = vec![Span::styled("▌ ", Style::default().fg(Color::Yellow))];

    if !before.is_empty() {
        spans.push(Span::styled(before, text_style));
    }

    // Cursor: show character at cursor position with block cursor, or a thick bar if at end
    if let Some(c) = cursor_char {
        spans.push(Span::styled(c.to_string(), cursor_style));
    } else {
        spans.push(Span::styled("█", Style::default().fg(Color::White)));
    }

    if !after_cursor.is_empty() {
        spans.push(Span::styled(after_cursor, text_style));
    }

    let line = Line::from(spans);
    let para = Paragraph::new(line);
    frame.render_widget(para, area);
}

/// Render the results panels: optionally Trigram/Word/Semantic on top, Merged below
fn render_results_panels(frame: &mut Frame, app: &mut App, area: Rect) {
    if app.show_debug_panels {
        // Split: debug panels on top, merged below
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage(40), // Trigram + Word + Semantic
                Constraint::Percentage(60), // Merged (larger)
            ])
            .split(area);

        // Semantic on left, Trigram + Word stacked on right
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(50), // Semantic
                Constraint::Percentage(50), // Trigram + Word stacked
            ])
            .split(rows[0]);

        // Right column: Trigram on top, Word on bottom
        let right_rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(cols[1]);

        render_result_list(
            frame,
            "Semantic",
            &app.results.semantic,
            cols[0],
            Color::Cyan,
            None,
            true,
        );
        render_result_list(
            frame,
            "Trigram",
            &app.results.trigram,
            right_rows[0],
            Color::Yellow,
            None,
            true,
        );
        render_result_list(
            frame,
            "Word",
            &app.results.word,
            right_rows[1],
            Color::Green,
            None,
            true,
        );

        render_merged_list(frame, app, rows[1]);
    } else {
        // Just the merged panel
        render_merged_list(frame, app, area);
    }
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

/// Render a simple results list panel (for Trigram/Word/Semantic - no selection, dimmed)
fn render_result_list(
    frame: &mut Frame,
    title: &str,
    hits: &[SearchHit],
    area: Rect,
    color: Color,
    _selected: Option<usize>,
    dimmed: bool,
) {
    let score_style = Style::default().fg(Color::DarkGray);

    let items: Vec<ListItem> = hits
        .iter()
        .map(|hit| {
            let mpn = Span::styled(&hit.mpn, Style::default().fg(Color::White));
            let prefix_span = if let Some(rank) = hit.rank {
                Span::styled(format!("{:>7.2} ", rank), score_style)
            } else {
                Span::styled("        ", score_style)
            };
            let mfr = hit.manufacturer.as_deref().unwrap_or("");
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
        .map(|(i, hit)| {
            let is_selected = selected_index == Some(i);

            let base_style = if is_selected {
                Style::default().bg(selection_bg)
            } else {
                Style::default()
            };

            let prefix = if is_selected { "▌ " } else { "  " };
            let prefix_style = if is_selected {
                Style::default().fg(Color::LightRed).bg(selection_bg)
            } else {
                Style::default()
            };

            // Line 1: registry_path
            let path_style = if is_selected {
                base_style.fg(Color::Yellow).add_modifier(Modifier::BOLD)
            } else {
                base_style.fg(Color::White).add_modifier(Modifier::BOLD)
            };
            let line1 = Line::from(vec![
                Span::styled(prefix, prefix_style),
                Span::styled(&hit.registry_path, path_style),
            ]);

            // Line 2: MPN + manufacturer (indented)
            let mpn_style = base_style.fg(Color::Gray);
            let mfr_style = base_style.fg(Color::DarkGray);
            let mfr = hit.manufacturer.as_deref().unwrap_or("");
            let line2 = Line::from(vec![
                Span::styled(prefix, prefix_style),
                Span::styled("  ", base_style),
                Span::styled(&hit.mpn, mpn_style),
                Span::styled(format!(" ({})", mfr), mfr_style),
            ]);

            // Line 3: short description (indented)
            let desc = hit.short_description.as_deref().unwrap_or("");
            let desc_style = base_style.fg(Color::DarkGray);
            let line3 = Line::from(vec![
                Span::styled(prefix, prefix_style),
                Span::styled("  ", base_style),
                Span::styled(desc, desc_style),
            ]);

            let item = ListItem::new(vec![line1, line2, line3]);
            if is_selected {
                item.style(Style::default().bg(selection_bg))
            } else {
                item
            }
        })
        .collect();

    let list = List::new(items).direction(ListDirection::BottomToTop);

    // Reserve 1 column on the right for scrollbar
    let list_area = Rect {
        width: area.width.saturating_sub(1),
        ..area
    };
    let scrollbar_area = Rect {
        x: area.x + area.width.saturating_sub(1),
        width: 1,
        ..area
    };

    StatefulWidget::render(list, list_area, frame.buffer_mut(), &mut app.list_state);

    // Scrollbar with stable thumb size (custom impl avoids ratatui's ±1 cell wobble)
    let total = app.results.merged.len();
    let visible = scrollbar_area.height as usize / 3;
    let max_offset = total.saturating_sub(visible);
    if max_offset > 0 {
        let offset = app.list_state.offset().min(max_offset);
        let track = scrollbar_area.height as usize;
        let thumb_len = (visible * track / total).clamp(1, track);
        let max_start = track - thumb_len;
        // Round to nearest instead of floor to distribute jumps more evenly
        let start = ((max_offset - offset) * max_start + max_offset / 2) / max_offset;

        let buf = frame.buffer_mut();
        for i in 0..track {
            let in_thumb = i >= start && i < start + thumb_len;
            buf[(scrollbar_area.x, scrollbar_area.y + i as u16)]
                .set_symbol(if in_thumb { "┃" } else { "│" })
                .set_style(Style::default().fg(if in_thumb {
                    Color::Magenta
                } else {
                    Color::DarkGray
                }));
        }
    }
}

/// Render the preview panel showing selected part details
fn render_preview_panel(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(Color::Gray).add_modifier(Modifier::DIM))
        .title(" Part Details ");

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Use the cached selected_part (fetched on-demand)
    if let Some(part) = app.selected_part.clone() {
        // Add 2 char left padding
        let padded = Rect {
            x: inner.x + 2,
            y: inner.y,
            width: inner.width.saturating_sub(2),
            height: inner.height,
        };
        render_part_details(frame, app, &part, padded);
    } else if app.results.merged.is_empty() {
        let empty = Paragraph::new("No part selected")
            .style(Style::default().fg(Color::DarkGray))
            .alignment(Alignment::Center);
        frame.render_widget(empty, inner);
    } else {
        // Part details are loading
        let loading = Paragraph::new("Loading...")
            .style(Style::default().fg(Color::DarkGray))
            .alignment(Alignment::Center);
        frame.render_widget(loading, inner);
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
        && part.image_data.as_ref().is_some_and(|d| !d.is_empty());

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
            let key_width = 32;
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
    let mut tri_line = vec![Span::styled(
        "Trigram  ",
        Style::default().fg(Color::Yellow),
    )];
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
    let mut word_line = vec![Span::styled("Word     ", Style::default().fg(Color::Green))];
    word_line.extend(format_index_result(
        word_pos,
        word_rank,
        app.results.word.len(),
    ));
    lines.push(Line::from(word_line));

    // Semantic
    let (sem_pos, sem_rank) = scoring
        .map(|s| (s.semantic_position, s.semantic_rank))
        .unwrap_or((None, None));
    let mut sem_line = vec![Span::styled("Semantic ", Style::default().fg(Color::Cyan))];
    sem_line.extend(format_index_result(
        sem_pos,
        sem_rank,
        app.results.semantic.len(),
    ));
    lines.push(Line::from(sem_line));

    // RRF Score calculation: score = Σ 1/(K + rank)
    const K: f64 = 10.0;
    let rrf_score = tri_pos.map(|p| 1.0 / (K + (p + 1) as f64)).unwrap_or(0.0)
        + word_pos.map(|p| 1.0 / (K + (p + 1) as f64)).unwrap_or(0.0)
        + sem_pos.map(|p| 1.0 / (K + (p + 1) as f64)).unwrap_or(0.0);

    // Show RRF formula breakdown
    let mut rrf_parts: Vec<String> = Vec::new();
    if let Some(p) = tri_pos {
        rrf_parts.push(format!(
            "1/(10+{})={:.3}",
            p + 1,
            1.0 / (K + (p + 1) as f64)
        ));
    }
    if let Some(p) = word_pos {
        rrf_parts.push(format!(
            "1/(10+{})={:.3}",
            p + 1,
            1.0 / (K + (p + 1) as f64)
        ));
    }
    if let Some(p) = sem_pos {
        rrf_parts.push(format!(
            "1/(10+{})={:.3}",
            p + 1,
            1.0 / (K + (p + 1) as f64)
        ));
    }

    // Merged position with RRF details
    let mut merged_line = vec![
        Span::styled("Merged   ", Style::default().fg(Color::Magenta)),
        Span::styled(
            format!("#{}", app.selected_index() + 1),
            Style::default().fg(Color::White),
        ),
        Span::styled(format!("/{}", app.results.merged.len()), dim_style),
    ];
    if !rrf_parts.is_empty() {
        merged_line.push(Span::styled(
            format!(" ({}={:.3})", rrf_parts.join("+"), rrf_score),
            dim_style,
        ));
    }
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

/// Render the command palette overlay
fn render_command_palette(frame: &mut Frame, app: &App) {
    let area = frame.area();

    // Size: 40% width, up to 20 rows height
    let width = (area.width * 40 / 100)
        .max(40)
        .min(area.width.saturating_sub(4));
    let height = 20.min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(width)) / 2;
    let y = area.height / 5;

    let palette_area = Rect::new(x, y, width, height);

    // Clear background
    frame.render_widget(ratatui::widgets::Clear, palette_area);

    // Draw outer block
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(Color::Green))
        .title(" Commands ");
    let inner = block.inner(palette_area);
    frame.render_widget(block, palette_area);

    // Layout: search input on top, empty line, commands below
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // Search input
            Constraint::Length(1), // Empty line
            Constraint::Min(1),    // Commands
        ])
        .split(inner);

    // Search input with cursor
    let input = &app.command_palette_input;
    let (before, after) = input.text.split_at(input.cursor);
    let cursor_char = after.chars().next();
    let after_cursor = cursor_char.map(|c| &after[c.len_utf8()..]).unwrap_or("");

    let cursor_style = Style::default().fg(Color::White).bg(Color::DarkGray);
    let text_style = Style::default().fg(Color::White);

    let mut spans = vec![Span::styled("▌ ", Style::default().fg(Color::Yellow))];
    if !before.is_empty() {
        spans.push(Span::styled(before, text_style));
    }
    if let Some(c) = cursor_char {
        spans.push(Span::styled(c.to_string(), cursor_style));
    } else {
        spans.push(Span::styled("█", Style::default().fg(Color::White)));
    }
    if !after_cursor.is_empty() {
        spans.push(Span::styled(after_cursor, text_style));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), chunks[0]);

    // Command list (chunks[2], after the empty line spacer)
    let selection_bg = Color::Rgb(38, 38, 38);
    let inner_width = chunks[2].width as usize;

    let items: Vec<ListItem> = app
        .command_palette_filtered
        .iter()
        .enumerate()
        .map(|(i, cmd)| {
            let is_selected = i == app.command_palette_index;

            if is_selected {
                let base_bg = Style::default().bg(selection_bg);
                let name_style = base_bg.fg(Color::White);
                let desc_style = base_bg.fg(Color::DarkGray);
                let prefix_style = Style::default().fg(Color::LightRed).bg(selection_bg);

                // Build line1: prefix + name + padding
                let name_text = cmd.name();
                let line1_used = 2 + name_text.len(); // "▌ " + name
                let line1_pad = inner_width.saturating_sub(line1_used);

                // Build line2: prefix + description + padding (red bar spans both lines)
                let desc_text = cmd.description();
                let line2_used = 2 + desc_text.len(); // "▌ " + desc
                let line2_pad = inner_width.saturating_sub(line2_used);

                ListItem::new(vec![
                    Line::from(vec![
                        Span::styled("▌ ", prefix_style),
                        Span::styled(name_text, name_style),
                        Span::styled(" ".repeat(line1_pad), base_bg),
                    ]),
                    Line::from(vec![
                        Span::styled("▌ ", prefix_style),
                        Span::styled(desc_text, desc_style),
                        Span::styled(" ".repeat(line2_pad), base_bg),
                    ]),
                ])
            } else {
                let name_style = Style::default().fg(Color::White);
                let desc_style = Style::default().fg(Color::DarkGray);

                ListItem::new(vec![
                    Line::from(vec![
                        Span::styled("  ", Style::default()),
                        Span::styled(cmd.name(), name_style),
                    ]),
                    Line::from(vec![
                        Span::styled("  ", desc_style),
                        Span::styled(cmd.description(), desc_style),
                    ]),
                ])
            }
        })
        .collect();

    let list = List::new(items);
    frame.render_widget(list, chunks[2]);
}
