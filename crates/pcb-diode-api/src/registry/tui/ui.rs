//! UI rendering

use crate::{PackageDependency, RegistryPart, SearchHit};
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

    // Left side: results panels stacked vertically, then status, toast, search
    let left_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(9),    // Results panels (stacked vertically)
            Constraint::Length(1), // Results count + query time
            Constraint::Length(1), // Status bar (mode + shortcuts)
            Constraint::Length(1), // Toast line (notifications)
            Constraint::Length(1), // Search input (single line, minimal)
        ])
        .split(main_chunks[0]);

    render_results_panels(frame, app, left_chunks[0]);
    render_results_count(frame, app, left_chunks[1]);
    render_status_bar(frame, app, left_chunks[2]);
    render_toast_line(frame, app, left_chunks[3]);
    render_search_input(frame, app, left_chunks[4]);

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
    let after_cursor = if let Some(c) = cursor_char {
        &after[c.len_utf8()..]
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
    if app.mode.requires_registry() {
        // Registry modes (modules or components)
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
    } else {
        // WebComponents mode
        render_component_list(frame, app, area);
    }
}

/// Render results count + query time line (subtle)
fn render_results_count(frame: &mut Frame, app: &App, area: Rect) {
    let line = if app.mode.requires_registry() {
        let count = app.results.merged.len();
        let query_time = format_duration(app.results.duration);

        if count == 0 {
            Line::from(vec![Span::styled(
                format!("  0/{}", app.packages_count),
                Style::default().fg(Color::DarkGray),
            )])
        } else {
            Line::from(vec![
                Span::styled(
                    format!("  {}/{} ", count, app.packages_count),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    format!("({})", query_time),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::DIM),
                ),
            ])
        }
    } else {
        // WebComponents mode
        if app.component_searching {
            // Show spinner while searching
            let spinner = spinner_frame(app.component_search_started);
            Line::from(vec![Span::styled(
                format!("  {} Searching...", spinner),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::DIM),
            )])
        } else {
            let count = app.component_results.results.len();
            let query_time = format_duration(app.component_results.duration);

            if count == 0 {
                Line::from(vec![Span::styled(
                    "  0 results",
                    Style::default().fg(Color::DarkGray),
                )])
            } else {
                Line::from(vec![
                    Span::styled(
                        format!("  {} results ", count),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        format!("({})", query_time),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::DIM),
                    ),
                ])
            }
        }
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
            let display_name = hit.mpn.as_deref().unwrap_or(&hit.name);
            let name_span = Span::styled(display_name, Style::default().fg(Color::White));
            let prefix_span = if let Some(rank) = hit.rank {
                Span::styled(format!("{:>7.2} ", rank), score_style)
            } else {
                Span::styled("        ", score_style)
            };
            let mfr = hit.manufacturer.as_deref().unwrap_or("");
            let mfr_span = if !mfr.is_empty() {
                Span::styled(format!(" ({})", mfr), Style::default().fg(Color::DarkGray))
            } else {
                Span::styled("", Style::default())
            };
            ListItem::new(Line::from(vec![prefix_span, name_span, mfr_span]))
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
    use super::search::RegistryResultDisplay;

    let selection_bg = Color::Rgb(38, 38, 38);
    let selected_index = app.list_state.selected();
    let is_modules_mode = app.mode == super::app::SearchMode::RegistryModules;

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

            let prefix_style = if is_selected {
                Style::default().fg(Color::LightRed).bg(selection_bg)
            } else {
                Style::default()
            };

            let display = RegistryResultDisplay::from_registry(
                &hit.url,
                hit.version.as_deref(),
                hit.package_category.as_deref(),
                hit.mpn.as_deref(),
                hit.manufacturer.as_deref(),
                hit.short_description.as_deref(),
                is_modules_mode,
            );

            let lines = display.to_tui_lines(is_selected, base_style, prefix_style);

            let item = ListItem::new(lines);
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
    let item_height = if app.mode == super::app::SearchMode::RegistryModules {
        2
    } else {
        3
    };
    let visible = scrollbar_area.height as usize / item_height;
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

/// Render component search results list (New mode)
fn render_component_list(frame: &mut Frame, app: &mut App, area: Rect) {
    use super::search::WebComponentDisplay;

    let selection_bg = Color::Rgb(38, 38, 38);
    let selected_index = app.component_list_state.selected();

    let items: Vec<ListItem> = app
        .component_results
        .results
        .iter()
        .enumerate()
        .map(|(i, result)| {
            let is_selected = selected_index == Some(i);

            let base_style = if is_selected {
                Style::default().bg(selection_bg)
            } else {
                Style::default()
            };

            let prefix_style = if is_selected {
                Style::default().fg(Color::LightRed).bg(selection_bg)
            } else {
                Style::default()
            };

            let display = WebComponentDisplay::from_component(result);
            let lines = display.to_tui_lines(is_selected, base_style, prefix_style);

            let item = ListItem::new(lines);
            if is_selected {
                item.style(Style::default().bg(selection_bg))
            } else {
                item
            }
        })
        .collect();

    let list = List::new(items).direction(ListDirection::BottomToTop);

    StatefulWidget::render(
        list,
        area,
        frame.buffer_mut(),
        &mut app.component_list_state,
    );
}

/// Render the preview panel showing selected package details
fn render_preview_panel(frame: &mut Frame, app: &mut App, area: Rect) {
    let title = if app.mode.requires_registry() {
        " Package Details "
    } else {
        " Component Details "
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(Color::Gray).add_modifier(Modifier::DIM))
        .title(title);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.mode.requires_registry() {
        // Use the cached selected_part (fetched on-demand)
        // We keep showing old details while new ones load to avoid flicker.
        // Only show "Loading..." if we've been waiting > 100ms (via is_loading_details).
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
        } else if app.is_loading_details() {
            // Only show loading after a delay to avoid flicker
            let loading = Paragraph::new("Loading...")
                .style(Style::default().fg(Color::DarkGray))
                .alignment(Alignment::Center);
            frame.render_widget(loading, inner);
        }
        // else: waiting for details but delay not elapsed - show nothing
    } else {
        // WebComponents mode - show selected component details
        let selected_index = app.component_list_state.selected();
        if let Some(idx) = selected_index {
            if let Some(result) = app.component_results.results.get(idx) {
                // Add 2 char left padding
                let padded = Rect {
                    x: inner.x + 2,
                    y: inner.y,
                    width: inner.width.saturating_sub(2),
                    height: inner.height,
                };
                render_component_details(frame, result, padded);
            }
        } else if app.component_results.results.is_empty() {
            let empty = Paragraph::new("No component selected")
                .style(Style::default().fg(Color::DarkGray))
                .alignment(Alignment::Center);
            frame.render_widget(empty, inner);
        }
    }
}

/// Render component details in the preview panel (for New mode)
fn render_component_details(
    frame: &mut Frame,
    result: &crate::component::ComponentSearchResult,
    area: Rect,
) {
    use crate::component::sanitize_mpn_for_path;

    let label_style = Style::default().fg(Color::DarkGray);
    let value_style = Style::default().fg(Color::White);
    let artifact_label = Style::default().fg(Color::Gray);

    let mut lines: Vec<Line> = Vec::new();

    // Path (bold, prominent - matches registry mode URL)
    let sanitized_mfr = result
        .manufacturer
        .as_deref()
        .map(sanitize_mpn_for_path)
        .unwrap_or_else(|| "unknown".to_string());
    let sanitized_mpn = sanitize_mpn_for_path(&result.part_number);
    let path = format!("components/{}/{}", sanitized_mfr, sanitized_mpn);

    lines.push(Line::from(vec![
        Span::styled("Path          ", label_style),
        Span::styled(
            path,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
    ]));

    // Part Number
    lines.push(Line::from(vec![
        Span::styled("Part Number   ", label_style),
        Span::styled(&result.part_number, value_style),
    ]));

    // Manufacturer
    if let Some(ref mfr) = result.manufacturer {
        lines.push(Line::from(vec![
            Span::styled("Manufacturer  ", label_style),
            Span::styled(mfr, value_style),
        ]));
    }

    // Package Category
    if let Some(ref pkg) = result.package_category {
        lines.push(Line::from(vec![
            Span::styled("Package       ", label_style),
            Span::styled(pkg, Style::default().fg(Color::Yellow)),
        ]));
    }

    lines.push(Line::from(""));

    // ═══════════════════════════════════════════
    // ARTIFACT AVAILABILITY (one per line)
    // ═══════════════════════════════════════════
    lines.push(Line::from(vec![Span::styled(
        "─── Artifacts ───",
        Style::default().fg(Color::DarkGray),
    )]));

    let has_eda = result.model_availability.ecad_model;
    let has_step = result.model_availability.step_model;
    let has_datasheet = !result.datasheets.is_empty();

    let check = Span::styled("✓", Style::default().fg(Color::Green));
    let cross = Span::styled("✗", Style::default().fg(Color::Red));

    lines.push(Line::from(vec![
        Span::styled("EDA Symbol    ", artifact_label),
        if has_eda {
            check.clone()
        } else {
            cross.clone()
        },
    ]));
    lines.push(Line::from(vec![
        Span::styled("STEP Model    ", artifact_label),
        if has_step {
            check.clone()
        } else {
            cross.clone()
        },
    ]));
    lines.push(Line::from(vec![
        Span::styled("Datasheet     ", artifact_label),
        if has_datasheet { check } else { cross },
    ]));

    // Score (if available from API)
    if let Some(score) = result.score {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![Span::styled(
            "─── Search Score ───",
            Style::default().fg(Color::DarkGray),
        )]));
        lines.push(Line::from(vec![
            Span::styled("Score         ", label_style),
            Span::styled(format!("{:.2}", score), Style::default().fg(Color::Yellow)),
        ]));
    }

    // Description
    if let Some(ref desc) = result.description {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![Span::styled(
            "─── Description ───",
            Style::default().fg(Color::DarkGray),
        )]));
        // Wrap description
        for chunk in wrap_text(desc, area.width.saturating_sub(4) as usize) {
            lines.push(Line::from(vec![Span::styled(chunk, value_style)]));
        }
    }

    let para = Paragraph::new(lines);
    frame.render_widget(para, area);
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

/// Render detailed package information with adaptive layout
fn render_part_details(frame: &mut Frame, app: &mut App, part: &RegistryPart, area: Rect) {
    let label_style = Style::default().fg(Color::DarkGray);
    let value_style = Style::default().fg(Color::White);
    let dim_style = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::ITALIC);

    let mut lines: Vec<Line> = Vec::new();

    // ═══════════════════════════════════════════
    // PACKAGE INFO (always shown)
    // ═══════════════════════════════════════════
    lines.push(Line::from(vec![
        Span::styled("URL           ", label_style),
        Span::styled(
            &part.url,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
    ]));

    if let Some(ref version) = part.version {
        lines.push(Line::from(vec![
            Span::styled("Version       ", label_style),
            Span::styled(version, value_style),
        ]));
    }

    // Keywords (after URL/version, wrap with indentation)
    if !part.keywords.is_empty() {
        let label = "Keywords      ";
        let indent = " ".repeat(label.len());
        let max_width = area.width.saturating_sub(label.len() as u16 + 4) as usize;

        let keywords_str = part.keywords.join(", ");
        let wrapped = wrap_text(&keywords_str, max_width);

        for (i, line_text) in wrapped.into_iter().enumerate() {
            if i == 0 {
                lines.push(Line::from(vec![
                    Span::styled(label, label_style),
                    Span::styled(line_text, Style::default().fg(Color::DarkGray)),
                ]));
            } else {
                lines.push(Line::from(vec![
                    Span::styled(indent.clone(), label_style),
                    Span::styled(line_text, Style::default().fg(Color::DarkGray)),
                ]));
            }
        }
    }

    lines.push(Line::from(""));

    // ═══════════════════════════════════════════
    // COMPONENT DETAILS (only if has MPN or manufacturer)
    // ═══════════════════════════════════════════
    let has_component_details = part.mpn.is_some() || part.manufacturer.is_some();
    if has_component_details {
        lines.push(Line::from(vec![Span::styled(
            "─── Component Details ───",
            Style::default().fg(Color::DarkGray),
        )]));

        if let Some(ref mpn) = part.mpn {
            lines.push(Line::from(vec![
                Span::styled("MPN           ", label_style),
                Span::styled(mpn, value_style),
            ]));
        }

        if let Some(ref mfr) = part.manufacturer {
            lines.push(Line::from(vec![
                Span::styled("Manufacturer  ", label_style),
                Span::styled(mfr, value_style),
            ]));
        }

        if let Some(ref pt) = part.part_type {
            lines.push(Line::from(vec![
                Span::styled("Type          ", label_style),
                Span::styled(pt, Style::default().fg(Color::Green)),
            ]));
        }

        lines.push(Line::from(""));
    }

    // ═══════════════════════════════════════════
    // IMAGE (in component details, before description)
    // ═══════════════════════════════════════════
    let has_image = app.image_protocol.is_supported()
        && app.picker.is_some()
        && part.image_data.as_ref().is_some_and(|d| !d.is_empty());

    if has_image {
        // Render text so far, then image, then continue with description
        let header_height = lines.len() as u16;
        let image_height: u16 = 8;

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(header_height),
                Constraint::Length(image_height),
                Constraint::Length(1), // Spacer
                Constraint::Min(0),    // Rest of text
            ])
            .split(area);

        let header_para = Paragraph::new(lines);
        frame.render_widget(header_para, chunks[0]);
        render_part_image(frame, app, part, chunks[1]);

        // Continue rendering the rest in chunks[3]
        render_part_details_rest(
            frame,
            app,
            part,
            chunks[3],
            label_style,
            value_style,
            dim_style,
        );
        return;
    }

    append_detail_body(
        &mut lines,
        app,
        part,
        area.width,
        label_style,
        value_style,
        dim_style,
    );

    let para = Paragraph::new(lines);
    frame.render_widget(para, area);
}

/// Render the rest of part details (description onwards) - used when image splits the layout
fn render_part_details_rest(
    frame: &mut Frame,
    app: &mut App,
    part: &RegistryPart,
    area: Rect,
    label_style: Style,
    value_style: Style,
    dim_style: Style,
) {
    let mut lines: Vec<Line> = Vec::new();
    append_detail_body(
        &mut lines,
        app,
        part,
        area.width,
        label_style,
        value_style,
        dim_style,
    );
    frame.render_widget(Paragraph::new(lines), area);
}

/// Append description, dependencies, parameters, and scoring to lines
fn append_detail_body(
    lines: &mut Vec<Line>,
    app: &mut App,
    part: &RegistryPart,
    width: u16,
    label_style: Style,
    value_style: Style,
    dim_style: Style,
) {
    // Description
    let description = part
        .detailed_description
        .as_ref()
        .or(part.short_description.as_ref());

    if let Some(desc) = description {
        lines.push(Line::from(Span::styled(
            "Description",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )));
        for chunk in wrap_text(desc, width.saturating_sub(4) as usize) {
            lines.push(Line::from(Span::styled(chunk, value_style)));
        }
        lines.push(Line::from(""));
    }

    // Dependencies
    if !app.package_relations.dependencies.is_empty() {
        lines.push(Line::from(Span::styled(
            "─── Dependencies ───",
            Style::default().fg(Color::DarkGray),
        )));
        render_dependency_tree(lines, &app.package_relations.dependencies);
        lines.push(Line::from(""));
    }

    // Dependents
    if !app.package_relations.dependents.is_empty() {
        lines.push(Line::from(Span::styled(
            "─── Used By ───",
            Style::default().fg(Color::DarkGray),
        )));
        render_dependency_tree(lines, &app.package_relations.dependents);
        lines.push(Line::from(""));
    }

    // Parameters (Digikey)
    if let Some(ref dk) = part.digikey {
        if !dk.parameters.is_empty() {
            lines.push(Line::from(Span::styled(
                "─── Parameters ───",
                Style::default().fg(Color::DarkGray),
            )));
            append_parameters(lines, &dk.parameters, label_style, value_style);
            lines.push(Line::from(""));
        }
    }

    // Search Scoring
    lines.push(Line::from(Span::styled(
        "─── Search Scoring ───",
        Style::default().fg(Color::DarkGray),
    )));
    append_search_scoring(lines, app, part, dim_style);
}

/// Append DigiKey parameters with priority ordering
fn append_parameters(
    lines: &mut Vec<Line>,
    parameters: &std::collections::BTreeMap<String, String>,
    label_style: Style,
    value_style: Style,
) {
    const PRIORITY_KEYS: &[&str] = &[
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
    const MAX_PARAMS: usize = 8;
    const KEY_WIDTH: usize = 32;

    let format_key = |key: &str| -> String {
        if key.len() >= KEY_WIDTH {
            format!("{}… ", &key[..KEY_WIDTH - 1])
        } else {
            format!("{:<width$} ", key, width = KEY_WIDTH)
        }
    };

    let mut shown = std::collections::HashSet::new();
    let mut count = 0;

    // Priority keys first
    for key in PRIORITY_KEYS {
        if count >= MAX_PARAMS {
            break;
        }
        if let Some(value) = parameters.get(*key) {
            lines.push(Line::from(vec![
                Span::styled(format_key(key), label_style),
                Span::styled(value.clone(), value_style),
            ]));
            shown.insert(*key);
            count += 1;
        }
    }

    // Remaining keys
    for (key, value) in parameters {
        if count >= MAX_PARAMS {
            break;
        }
        if !shown.contains(key.as_str()) {
            lines.push(Line::from(vec![
                Span::styled(format_key(key), label_style),
                Span::styled(value.clone(), value_style),
            ]));
            count += 1;
        }
    }
}

/// Append search scoring section
fn append_search_scoring(lines: &mut Vec<Line>, app: &App, part: &RegistryPart, dim_style: Style) {
    let scoring = app.results.scoring.get(&part.url);

    let format_result = |pos: Option<usize>, rank: Option<f64>, total: usize| -> Vec<Span> {
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

    let (tri_pos, tri_rank) = scoring
        .map(|s| (s.trigram_position, s.trigram_rank))
        .unwrap_or((None, None));
    let (word_pos, word_rank) = scoring
        .map(|s| (s.word_position, s.word_rank))
        .unwrap_or((None, None));
    let (sem_pos, sem_rank) = scoring
        .map(|s| (s.semantic_position, s.semantic_rank))
        .unwrap_or((None, None));

    let mut tri_line = vec![Span::styled(
        "Trigram  ",
        Style::default().fg(Color::Yellow),
    )];
    tri_line.extend(format_result(tri_pos, tri_rank, app.results.trigram.len()));
    lines.push(Line::from(tri_line));

    let mut word_line = vec![Span::styled("Word     ", Style::default().fg(Color::Green))];
    word_line.extend(format_result(word_pos, word_rank, app.results.word.len()));
    lines.push(Line::from(word_line));

    let mut sem_line = vec![Span::styled("Semantic ", Style::default().fg(Color::Cyan))];
    sem_line.extend(format_result(sem_pos, sem_rank, app.results.semantic.len()));
    lines.push(Line::from(sem_line));

    // RRF score calculation
    const K: f64 = 10.0;
    let rrf = |pos: Option<usize>| pos.map(|p| 1.0 / (K + (p + 1) as f64)).unwrap_or(0.0);
    let rrf_score = rrf(tri_pos) + rrf(word_pos) + rrf(sem_pos);

    let rrf_parts: Vec<String> = [tri_pos, word_pos, sem_pos]
        .iter()
        .filter_map(|&pos| {
            pos.map(|p| format!("1/(10+{})={:.3}", p + 1, 1.0 / (K + (p + 1) as f64)))
        })
        .collect();

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
}

/// Render dependency list with full URLs and colored paths
fn render_dependency_tree(lines: &mut Vec<Line>, deps: &[PackageDependency]) {
    let max_shown = 10;
    let count = deps.len();

    for dep in deps.iter().take(max_shown) {
        // Split URL into parts: "github.com/diodeinc/registry" / rest
        let parts: Vec<_> = dep.url.split('/').collect();
        let registry_prefix = if parts.len() >= 3 {
            format!("{}/{}/{}/", parts[0], parts[1], parts[2])
        } else {
            String::new()
        };
        let rest_path = if parts.len() > 3 {
            parts[3..].join("/")
        } else {
            String::new()
        };

        // Color the full path after registry based on category
        let path_color = match dep.package_category.as_deref() {
            Some("component") => Color::Green,
            Some("module") => Color::Blue,
            Some("reference") => Color::Magenta,
            _ => Color::White,
        };

        lines.push(Line::from(vec![
            Span::styled(registry_prefix, Style::default().fg(Color::Gray)),
            Span::styled(rest_path, Style::default().fg(path_color)),
        ]));
    }

    if count > max_shown {
        lines.push(Line::from(Span::styled(
            format!("... and {} more", count - max_shown),
            Style::default().fg(Color::DarkGray),
        )));
    }
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

/// Render the status bar (mode indicator + keyboard shortcuts + download status)
fn render_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let dim = Style::default().fg(Color::DarkGray);
    let dim_yellow = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::DIM);
    let bracket = Style::default().fg(Color::DarkGray);

    // Mode indicator using display_name()
    // Pad to longest mode name ("registry:components" = 20 chars) so UI doesn't shift
    let mode_text = format!("{:<20}", app.mode.display_name());
    let mode_color = match app.mode {
        super::app::SearchMode::RegistryModules => Color::Magenta,
        super::app::SearchMode::RegistryComponents => Color::Green,
        super::app::SearchMode::WebComponents => Color::Cyan,
    };

    // Mode-specific Enter action
    let enter_action = if app.mode.requires_registry() {
        "Enter copy"
    } else {
        "Enter add"
    };

    let mut spans = vec![
        Span::styled("  mode:", dim),
        Span::styled(
            mode_text,
            Style::default().fg(mode_color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" [", bracket),
        Span::styled("↑↓ select", dim),
        Span::styled("]", bracket),
    ];

    // Only show mode cycle hint if multiple modes available
    if app.available_modes.len() > 1 {
        spans.extend([
            Span::styled(" [", bracket),
            Span::styled("^s cycle", dim),
            Span::styled("]", bracket),
        ]);
    }

    spans.extend([
        Span::styled(" [", bracket),
        Span::styled("^o cmds", dim),
        Span::styled("] [", bracket),
        Span::styled("Esc quit", dim),
        Span::styled("] [", bracket),
        Span::styled(enter_action, dim),
        Span::styled("]", bracket),
    ]);

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
            // Only show error if registry modes are available
            if app.available_modes.iter().any(|m| m.requires_registry()) {
                spans.push(Span::styled(" [", bracket));
                spans.push(Span::styled(
                    format!("✗ {}", msg),
                    Style::default().fg(Color::Red).add_modifier(Modifier::DIM),
                ));
                spans.push(Span::styled("]", bracket));
            }
        }
        DownloadState::Done => {}
    }

    let status = Paragraph::new(Line::from(spans));
    frame.render_widget(status, area);
}

/// Render the toast notification line (below status bar)
fn render_toast_line(frame: &mut Frame, app: &App, area: Rect) {
    if let Some(ref toast) = app.toast {
        let bracket = Style::default().fg(Color::DarkGray);
        let toast_style = if toast.is_error {
            Style::default().fg(Color::Red).add_modifier(Modifier::DIM)
        } else {
            Style::default().fg(Color::Blue).add_modifier(Modifier::DIM)
        };

        let spans = vec![
            Span::styled("  [", bracket),
            Span::styled(&toast.message, toast_style),
            Span::styled("]", bracket),
        ];

        let line = Paragraph::new(Line::from(spans));
        frame.render_widget(line, area);
    }
    // If no toast, leave the line empty
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
            let is_enabled = cmd.is_enabled(app.selected_part.as_ref(), &app.available_modes);

            if is_selected {
                let base_bg = Style::default().bg(selection_bg);
                let (name_style, desc_style, prefix_style) = if is_enabled {
                    (
                        base_bg.fg(Color::Yellow).add_modifier(Modifier::BOLD),
                        base_bg.fg(Color::DarkGray),
                        Style::default().fg(Color::LightRed).bg(selection_bg),
                    )
                } else {
                    (
                        base_bg.fg(Color::Gray).add_modifier(Modifier::BOLD),
                        base_bg.fg(Color::DarkGray),
                        Style::default().fg(Color::DarkGray).bg(selection_bg),
                    )
                };

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
                let (name_style, desc_style) = if is_enabled {
                    (
                        Style::default().fg(Color::White),
                        Style::default().fg(Color::DarkGray),
                    )
                } else {
                    (
                        Style::default().fg(Color::Gray).add_modifier(Modifier::DIM),
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::DIM),
                    )
                };

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
