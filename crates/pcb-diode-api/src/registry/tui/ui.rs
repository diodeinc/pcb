//! UI rendering

use crate::kicad_symbols::KicadSymbol;
use crate::{RegistryModule, RegistryModuleDependency, RegistrySymbol, SearchHit};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols::border,
    text::{Line, Span},
    widgets::{Block, Borders, List, ListDirection, ListItem, Paragraph, StatefulWidget},
};
use ratatui_image::StatefulImage;
use std::collections::HashSet;
use std::time::{Duration, Instant};

use super::app::{App, DownloadState, registry_symbol_has_image};
use super::image::{decode_image, image_dimensions};

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
    if app.mode.requires_local_index() {
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
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(rows[0]);

            let left_rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(cols[0]);
            let right_rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(cols[1]);

            match &app.results {
                super::search::SearchResults::RegistryModules(results) => {
                    render_module_result_list(
                        frame,
                        "Semantic",
                        &results.semantic,
                        left_rows[0],
                        Color::Cyan,
                    );
                    render_module_result_list(
                        frame,
                        "Trigram",
                        &results.trigram,
                        left_rows[1],
                        Color::Yellow,
                    );
                    render_module_result_list(
                        frame,
                        "Word",
                        &results.word,
                        right_rows[0],
                        Color::Green,
                    );
                    render_module_result_list(
                        frame,
                        "Docs",
                        &results.docs_full_text,
                        right_rows[1],
                        Color::LightMagenta,
                    );
                }
                super::search::SearchResults::RegistrySymbols(results) => {
                    render_symbol_result_list(
                        frame,
                        "Semantic",
                        &results.semantic,
                        left_rows[0],
                        Color::Cyan,
                    );
                    render_symbol_result_list(
                        frame,
                        "Trigram",
                        &results.trigram,
                        left_rows[1],
                        Color::Yellow,
                    );
                    render_symbol_result_list(
                        frame,
                        "Word",
                        &results.word,
                        right_rows[0],
                        Color::Green,
                    );
                    render_symbol_result_list(
                        frame,
                        "Docs",
                        &results.docs_full_text,
                        right_rows[1],
                        Color::LightMagenta,
                    );
                }
                super::search::SearchResults::KicadSymbols(results) => {
                    render_result_list(
                        frame,
                        "Semantic",
                        &results.semantic,
                        left_rows[0],
                        Color::Cyan,
                        None,
                        true,
                    );
                    render_result_list(
                        frame,
                        "Trigram",
                        &results.trigram,
                        left_rows[1],
                        Color::Yellow,
                        None,
                        true,
                    );
                    render_result_list(
                        frame,
                        "Word",
                        &results.word,
                        right_rows[0],
                        Color::Green,
                        None,
                        true,
                    );
                    render_result_list(
                        frame,
                        "Docs",
                        &results.docs_full_text,
                        right_rows[1],
                        Color::LightMagenta,
                        None,
                        true,
                    );
                }
                super::search::SearchResults::Empty => {}
            }

            render_local_merged_list(frame, app, rows[1]);
        } else {
            render_local_merged_list(frame, app, area);
        }
    } else {
        // WebComponents mode
        render_component_list(frame, app, area);
    }
}

/// Render results count + query time line (subtle)
fn render_results_count(frame: &mut Frame, app: &App, area: Rect) {
    let line = if app.mode.requires_local_index() {
        let count = app.results.len();
        let query_time = format_duration(app.results.duration());

        if count == 0 {
            Line::from(vec![Span::styled(
                format!("  0/{}", app.index_count),
                Style::default().fg(Color::DarkGray),
            )])
        } else {
            Line::from(vec![
                Span::styled(
                    format!("  {}/{} ", count, app.index_count),
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

fn render_module_result_list(
    frame: &mut Frame,
    title: &str,
    hits: &[crate::RegistryModuleHit],
    area: Rect,
    color: Color,
) {
    let score_style = Style::default().fg(Color::DarkGray);
    let items: Vec<ListItem> = hits
        .iter()
        .map(|hit| {
            let prefix_span = if let Some(rank) = hit.rank {
                Span::styled(format!("{:>7.2} ", rank), score_style)
            } else {
                Span::styled("        ", score_style)
            };
            ListItem::new(Line::from(vec![
                prefix_span,
                Span::styled(hit.name.clone(), Style::default().fg(Color::White)),
                Span::styled(
                    format!(" ({})", hit.version),
                    Style::default().fg(Color::DarkGray),
                ),
            ]))
        })
        .collect();
    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_set(border::ROUNDED)
            .border_style(Style::default().fg(color).add_modifier(Modifier::DIM))
            .title(format!(" {} ", title)),
    );
    frame.render_widget(list, area);
}

fn render_symbol_result_list(
    frame: &mut Frame,
    title: &str,
    hits: &[crate::RegistrySymbolHit],
    area: Rect,
    color: Color,
) {
    let score_style = Style::default().fg(Color::DarkGray);
    let items: Vec<ListItem> = hits
        .iter()
        .map(|hit| {
            let prefix_span = if let Some(rank) = hit.rank {
                Span::styled(format!("{:>7.2} ", rank), score_style)
            } else {
                Span::styled("        ", score_style)
            };
            ListItem::new(Line::from(vec![
                prefix_span,
                Span::styled(hit.mpn.clone(), Style::default().fg(Color::White)),
                Span::styled(
                    format!(" ({})", hit.manufacturer),
                    Style::default().fg(Color::DarkGray),
                ),
            ]))
        })
        .collect();
    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_set(border::ROUNDED)
            .border_style(Style::default().fg(color).add_modifier(Modifier::DIM))
            .title(format!(" {} ", title)),
    );
    frame.render_widget(list, area);
}

/// Render the merged local-index results list with selection and auto-scrolling
fn render_local_merged_list(frame: &mut Frame, app: &mut App, area: Rect) {
    if app.mode == super::app::SearchMode::KicadSymbols {
        render_kicad_merged_list(frame, app, area);
        return;
    }

    use super::display::{RegistryModuleDisplay, RegistrySymbolDisplay};

    let selection_bg = Color::Rgb(38, 38, 38);
    let selected_index = app.list_state.selected();

    let items: Vec<ListItem> = match &app.results {
        super::search::SearchResults::RegistryModules(results) => results
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
                let display = RegistryModuleDisplay::from_hit(hit);
                let item =
                    ListItem::new(display.to_tui_lines(is_selected, base_style, prefix_style));
                if is_selected {
                    item.style(Style::default().bg(selection_bg))
                } else {
                    item
                }
            })
            .collect(),
        super::search::SearchResults::RegistrySymbols(results) => results
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
                let display = RegistrySymbolDisplay::from_hit(hit);
                let item =
                    ListItem::new(display.to_tui_lines(is_selected, base_style, prefix_style));
                if is_selected {
                    item.style(Style::default().bg(selection_bg))
                } else {
                    item
                }
            })
            .collect(),
        _ => Vec::new(),
    };

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

    let item_height = if app.mode == super::app::SearchMode::RegistryModules {
        2
    } else {
        3
    };
    render_scrollbar(
        frame,
        scrollbar_area,
        app.results.len(),
        app.list_state.offset(),
        item_height,
    );
}

fn render_kicad_merged_list(frame: &mut Frame, app: &mut App, area: Rect) {
    use super::display::KicadSymbolDisplay;

    let selection_bg = Color::Rgb(38, 38, 38);
    let selected_index = app.list_state.selected();

    let hits = match &app.results {
        super::search::SearchResults::KicadSymbols(results) => &results.merged,
        _ => return,
    };
    let items: Vec<ListItem> = hits
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

            let display = KicadSymbolDisplay::from_hit(hit);
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
    render_scrollbar(
        frame,
        scrollbar_area,
        hits.len(),
        app.list_state.offset(),
        3,
    );
}

/// Render component search results list (New mode)
fn render_component_list(frame: &mut Frame, app: &mut App, area: Rect) {
    use super::display::WebComponentDisplay;

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

    StatefulWidget::render(
        list,
        list_area,
        frame.buffer_mut(),
        &mut app.component_list_state,
    );

    render_scrollbar(
        frame,
        scrollbar_area,
        app.component_results.results.len(),
        app.component_list_state.offset(),
        3, // WebComponentDisplay uses 3 lines per item
    );
}

/// Render the preview panel showing selected package details
fn render_preview_panel(frame: &mut Frame, app: &mut App, area: Rect) {
    let title = if app.mode == super::app::SearchMode::RegistryModules {
        " Module Details "
    } else if app.mode == super::app::SearchMode::RegistryComponents {
        " Component Details "
    } else if app.mode == super::app::SearchMode::KicadSymbols {
        " Symbol Details "
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
        let padded = Rect {
            x: inner.x + 2,
            y: inner.y,
            width: inner.width.saturating_sub(2),
            height: inner.height,
        };
        if app.mode == super::app::SearchMode::RegistryModules {
            if let Some(module) = app.selected_module.clone() {
                render_registry_module_details(frame, app, &module, padded);
            } else if app.results.is_empty() {
                let empty = Paragraph::new("No module selected")
                    .style(Style::default().fg(Color::DarkGray))
                    .alignment(Alignment::Center);
                frame.render_widget(empty, inner);
            } else if app.is_loading_details() {
                let loading = Paragraph::new("Loading...")
                    .style(Style::default().fg(Color::DarkGray))
                    .alignment(Alignment::Center);
                frame.render_widget(loading, inner);
            }
        } else if let Some(symbol) = app.selected_symbol.clone() {
            render_registry_symbol_details(frame, app, &symbol, padded);
        } else if app.results.is_empty() {
            let empty = Paragraph::new("No component selected")
                .style(Style::default().fg(Color::DarkGray))
                .alignment(Alignment::Center);
            frame.render_widget(empty, inner);
        } else if app.is_loading_details() {
            let loading = Paragraph::new("Loading...")
                .style(Style::default().fg(Color::DarkGray))
                .alignment(Alignment::Center);
            frame.render_widget(loading, inner);
        }
    } else if app.mode == super::app::SearchMode::KicadSymbols {
        if let Some(symbol) = app.selected_kicad_symbol.clone() {
            let padded = Rect {
                x: inner.x + 2,
                y: inner.y,
                width: inner.width.saturating_sub(2),
                height: inner.height,
            };
            render_kicad_symbol_details(frame, app, &symbol, padded);
        } else if app.results.is_empty() {
            let empty = Paragraph::new("No symbol selected")
                .style(Style::default().fg(Color::DarkGray))
                .alignment(Alignment::Center);
            frame.render_widget(empty, inner);
        } else if app.is_loading_details() {
            let loading = Paragraph::new("Loading...")
                .style(Style::default().fg(Color::DarkGray))
                .alignment(Alignment::Center);
            frame.render_widget(loading, inner);
        }
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
                let (availability, is_loading) = app.availability_for_lookup(
                    Some(result.part_number.as_str()),
                    result.manufacturer.as_deref(),
                );
                render_component_details(frame, result, padded, availability, is_loading);
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
    availability: Option<&pcb_sch::bom::Availability>,
    is_loading_availability: bool,
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

    // ═══════════════════════════════════════════
    // AVAILABILITY (2 fixed lines: US and Global)
    // ═══════════════════════════════════════════
    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::styled(
        "─── Availability ───",
        Style::default().fg(Color::DarkGray),
    )]));

    // US line
    lines.push(format_avail_line(
        "US",
        availability.and_then(|p| p.us.as_ref()),
        is_loading_availability,
    ));
    // Global line
    lines.push(format_avail_line(
        "Global",
        availability.and_then(|p| p.global.as_ref()),
        is_loading_availability,
    ));

    // ═══════════════════════════════════════════
    // OFFERS (raw offer data)
    // ═══════════════════════════════════════════
    if let Some(p) = availability {
        let in_stock: Vec<_> = p.offers.iter().filter(|o| o.stock > 0).take(6).collect();
        if !in_stock.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![Span::styled(
                "─── Offers ───",
                Style::default().fg(Color::DarkGray),
            )]));
            lines.extend(format_offer_lines(&in_stock));
        }
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

/// Format a price value for display (re-export from bom)
fn format_price(price: f64) -> String {
    crate::bom::format_price(price)
}

/// Format availability line: "US:     $3.22 (1,234)" or "US:     ..." while loading
fn format_avail_line<'a>(
    region: &str,
    avail: Option<&pcb_sch::bom::AvailabilitySummary>,
    is_loading: bool,
) -> Line<'a> {
    let label = format!("{:<8}", format!("{}:", region));
    let label_style = Style::default().fg(Color::Gray);

    if is_loading {
        return Line::from(vec![
            Span::styled(label, label_style),
            Span::styled(
                "...",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            ),
        ]);
    }

    match avail {
        Some(a) => {
            let price_str = a.price.map(format_price).unwrap_or_else(|| "—".to_string());
            let stock_str = format!("({})", a.stock);
            let stock_color = if a.stock > 0 {
                Color::Green
            } else {
                Color::Red
            };

            let mut spans = vec![
                Span::styled(label, label_style),
                Span::styled(price_str, Style::default().fg(Color::Yellow)),
                Span::styled(" ", Style::default()),
                Span::styled(stock_str, Style::default().fg(stock_color)),
            ];

            // Show alt stock if present
            if a.alt_stock > 0 {
                spans.push(Span::styled(
                    format!(" +{} alt", a.alt_stock),
                    Style::default().fg(Color::DarkGray),
                ));
            }

            Line::from(spans)
        }
        None => Line::from(vec![
            Span::styled(label, label_style),
            Span::styled("—", Style::default().fg(Color::DarkGray)),
        ]),
    }
}

/// Format offer lines with dynamic column widths based on content
fn format_offer_lines(offers: &[&pcb_sch::bom::Offer]) -> Vec<Line<'static>> {
    if offers.is_empty() {
        return Vec::new();
    }

    // Calculate max widths for each column
    let max_dist = offers
        .iter()
        .map(|o| o.distributor.len())
        .max()
        .unwrap_or(0);
    let max_price = offers
        .iter()
        .filter_map(|o| o.price.map(|p| format_price(p).len()))
        .max()
        .unwrap_or(1);
    let max_stock = offers
        .iter()
        .map(|o| format!("({})", o.stock).len())
        .max()
        .unwrap_or(3);

    offers
        .iter()
        .map(|offer| {
            let dist = format!(
                "{:<width$}",
                offer.distributor.to_uppercase(),
                width = max_dist + 2
            );
            let price = offer
                .price
                .map(|p| format!("{:<width$}", format_price(p), width = max_price + 2))
                .unwrap_or_else(|| format!("{:<width$}", "—", width = max_price + 2));
            let stock = format!(
                "{:<width$}",
                format!("({})", offer.stock),
                width = max_stock + 2
            );
            let part_id = offer.part_id.clone().unwrap_or_default();

            Line::from(vec![
                Span::styled(dist, Style::default().fg(Color::Cyan)),
                Span::styled(price, Style::default().fg(Color::Yellow)),
                Span::styled(
                    stock,
                    Style::default().fg(if offer.stock > 0 {
                        Color::Green
                    } else {
                        Color::DarkGray
                    }),
                ),
                Span::styled(part_id, Style::default().fg(Color::DarkGray)),
            ])
        })
        .collect()
}

fn render_image_bytes(frame: &mut Frame, app: &App, image_data: &[u8], area: Rect) {
    let picker = match &app.picker {
        Some(p) => p,
        None => return, // No picker available
    };

    let target_area = image_dimensions(image_data)
        .map(|(width, height)| fitted_image_rect(width, height, area))
        .unwrap_or(area);

    if let Some(mut protocol) = decode_image(image_data, picker) {
        let image_widget = StatefulImage::default();
        frame.render_stateful_widget(image_widget, target_area, &mut protocol);
    }
}

fn fitted_image_rect(image_width: u32, image_height: u32, area: Rect) -> Rect {
    if area.width == 0 || area.height == 0 || image_width == 0 || image_height == 0 {
        return area;
    }

    const CELL_ASPECT: f64 = 0.5;

    let area_width = area.width as f64;
    let area_height = area.height as f64;
    let image_aspect = image_width as f64 / image_height as f64;
    let area_aspect = (area_width * CELL_ASPECT) / area_height;

    let (target_width, target_height) = if image_aspect > area_aspect {
        let height = ((area_width * CELL_ASPECT) / image_aspect).floor().max(1.0);
        (area.width, height as u16)
    } else {
        let width = ((area_height * image_aspect) / CELL_ASPECT)
            .floor()
            .max(1.0);
        (width as u16, area.height)
    };

    let width = target_width.min(area.width).max(1);
    let height = target_height.min(area.height).max(1);

    Rect {
        x: area.x,
        y: area.y,
        width,
        height,
    }
}

fn render_kicad_symbol_details(frame: &mut Frame, app: &mut App, symbol: &KicadSymbol, area: Rect) {
    let label_style = Style::default().fg(Color::DarkGray);
    let value_style = Style::default().fg(Color::White);
    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(vec![
        Span::styled("Path          ", label_style),
        Span::styled(
            symbol.clipboard_url(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Symbol        ", label_style),
        Span::styled(&symbol.symbol_name, value_style),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Library       ", label_style),
        Span::styled(&symbol.symbol_library, value_style),
    ]));

    if let Some(mpn) = symbol.primary_mpn() {
        lines.push(Line::from(vec![
            Span::styled("MPN           ", label_style),
            Span::styled(mpn, value_style),
        ]));
    }

    lines.push(Line::from(vec![
        Span::styled("Manufacturer  ", label_style),
        Span::styled(&symbol.manufacturer, value_style),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Footprint     ", label_style),
        Span::styled(
            format!("{}/{}", symbol.footprint_library, symbol.footprint_name),
            Style::default().fg(Color::Yellow),
        ),
    ]));

    let keywords = kicad_symbol_keywords(symbol);
    if !keywords.is_empty() {
        let label = "Keywords      ";
        let indent = " ".repeat(label.len());
        let max_width = area.width.saturating_sub(label.len() as u16 + 4) as usize;

        for (idx, line_text) in wrap_text(&keywords.join(", "), max_width)
            .into_iter()
            .enumerate()
        {
            if idx == 0 {
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

    let has_image = app.image_protocol.is_supported()
        && app.picker.is_some()
        && symbol
            .image_data
            .as_ref()
            .is_some_and(|data| !data.is_empty());
    if has_image {
        let header_height = lines.len() as u16;
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(header_height),
                Constraint::Length(8),
                Constraint::Length(1),
                Constraint::Min(0),
            ])
            .split(area);

        frame.render_widget(Paragraph::new(lines), chunks[0]);
        if let Some(data) = symbol.image_data.as_deref() {
            render_image_bytes(frame, app, data, chunks[1]);
        }
        let lines = render_kicad_symbol_detail_lines(
            app,
            symbol,
            chunks[3].width,
            label_style,
            value_style,
        );
        frame.render_widget(Paragraph::new(lines), chunks[3]);
        return;
    }

    let body = render_kicad_symbol_detail_lines(app, symbol, area.width, label_style, value_style);
    lines.extend(body);
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_kicad_symbol_detail_lines(
    app: &mut App,
    symbol: &KicadSymbol,
    width: u16,
    label_style: Style,
    value_style: Style,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from("")];

    if let Some(description) = symbol.description() {
        lines.push(Line::from(vec![Span::styled(
            "─── Description ───",
            Style::default().fg(Color::DarkGray),
        )]));
        for chunk in wrap_text(description, width.saturating_sub(4) as usize) {
            lines.push(Line::from(vec![Span::styled(chunk, value_style)]));
        }
        lines.push(Line::from(""));
    }

    if !symbol.matched_mpns.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "─── Matched MPNs ───",
            Style::default().fg(Color::DarkGray),
        )]));
        for chunk in wrap_text(
            &symbol.matched_mpns.join(", "),
            width.saturating_sub(4) as usize,
        ) {
            lines.push(Line::from(vec![Span::styled(chunk, value_style)]));
        }
        lines.push(Line::from(""));
    }

    lines.push(Line::from(vec![Span::styled(
        "─── Availability ───",
        Style::default().fg(Color::DarkGray),
    )]));
    let (availability, is_loading_availability) = app.selected_kicad_symbol_availability();
    lines.push(format_avail_line(
        "US",
        availability.and_then(|pricing| pricing.us.as_ref()),
        is_loading_availability,
    ));
    lines.push(format_avail_line(
        "Global",
        availability.and_then(|pricing| pricing.global.as_ref()),
        is_loading_availability,
    ));

    if let Some(pricing) = availability {
        let in_stock: Vec<_> = pricing
            .offers
            .iter()
            .filter(|offer| offer.stock > 0)
            .take(6)
            .collect();
        if !in_stock.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "─── Offers ───",
                Style::default().fg(Color::DarkGray),
            )));
            lines.extend(format_offer_lines(&in_stock));
        }
    }

    if let Some(datasheet_url) = symbol.datasheet_url.as_deref() {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("Datasheet     ", label_style),
            Span::styled(datasheet_url.to_string(), value_style),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "─── Search Scoring ───",
        Style::default().fg(Color::DarkGray),
    )));
    append_search_scoring_for_url(&mut lines, app, &symbol.clipboard_url(), label_style);

    lines
}

fn kicad_symbol_keywords(symbol: &KicadSymbol) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut keywords = Vec::new();

    for source in [
        Some(symbol.phase3_keywords.as_str()),
        symbol.kicad_keywords.as_deref(),
    ] {
        let Some(source) = source else {
            continue;
        };
        for keyword in source
            .split([',', ';'])
            .map(str::trim)
            .filter(|keyword| !keyword.is_empty())
        {
            if seen.insert(keyword.to_ascii_lowercase()) {
                keywords.push(keyword.to_string());
            }
        }
    }

    keywords
}

fn render_registry_module_details(
    frame: &mut Frame,
    app: &mut App,
    module: &RegistryModule,
    area: Rect,
) {
    let label_style = Style::default().fg(Color::DarkGray);
    let value_style = Style::default().fg(Color::White);
    let dim_style = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::ITALIC);
    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(vec![
        Span::styled("URL           ", label_style),
        Span::styled(
            module.url.clone(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Version       ", label_style),
        Span::styled(module.version.clone(), value_style),
    ]));
    if let Some(published_at) = module.published_at.as_deref() {
        lines.push(Line::from(vec![
            Span::styled("Published     ", label_style),
            Span::styled(published_at.to_string(), value_style),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "─── Description ───",
        Style::default().fg(Color::DarkGray),
    )));
    for chunk in wrap_text(&module.description, area.width.saturating_sub(4) as usize) {
        lines.push(Line::from(Span::styled(chunk, value_style)));
    }

    if !module.entrypoints.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "─── Entrypoints ───",
            Style::default().fg(Color::DarkGray),
        )));
        for entrypoint in &module.entrypoints {
            lines.push(Line::from(vec![Span::styled(
                module_relative_url(&entrypoint.url, &module.url),
                Style::default().fg(Color::Blue),
            )]));
        }
    }

    if !module.symbols.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "─── Symbols ───",
            Style::default().fg(Color::DarkGray),
        )));
        for symbol in &module.symbols {
            lines.push(Line::from(vec![Span::styled(
                module_relative_url(&symbol.url, &module.url),
                Style::default().fg(Color::Green),
            )]));
        }
    }

    if !app.module_relations.dependencies.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "─── Dependencies ───",
            Style::default().fg(Color::DarkGray),
        )));
        render_dependency_tree(&mut lines, &app.module_relations.dependencies);
    }

    if !app.module_relations.dependents.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "─── Used By ───",
            Style::default().fg(Color::DarkGray),
        )));
        render_dependency_tree(&mut lines, &app.module_relations.dependents);
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "─── Search Scoring ───",
        Style::default().fg(Color::DarkGray),
    )));
    append_search_scoring_for_url(&mut lines, app, &module.url, dim_style);

    frame.render_widget(Paragraph::new(lines), area);
}

fn module_relative_url(url: &str, module_url: &str) -> String {
    url.strip_prefix(module_url)
        .and_then(|suffix| suffix.strip_prefix('/'))
        .unwrap_or(url)
        .to_string()
}

fn render_registry_symbol_details(
    frame: &mut Frame,
    app: &mut App,
    symbol: &RegistrySymbol,
    area: Rect,
) {
    let label_style = Style::default().fg(Color::DarkGray);
    let value_style = Style::default().fg(Color::White);
    let dim_style = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::ITALIC);
    let mut header_lines: Vec<Line> = vec![
        Line::from(vec![
            Span::styled("URL           ", label_style),
            Span::styled(
                symbol.url.clone(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("Version       ", label_style),
            Span::styled(symbol.module_version.clone(), value_style),
        ]),
        Line::from(vec![
            Span::styled("MPN           ", label_style),
            Span::styled(symbol.mpn.clone(), value_style),
        ]),
        Line::from(vec![
            Span::styled("Manufacturer  ", label_style),
            Span::styled(symbol.manufacturer.clone(), value_style),
        ]),
        Line::from(vec![
            Span::styled("Footprint     ", label_style),
            Span::styled(symbol.footprint.clone(), Style::default().fg(Color::Yellow)),
        ]),
    ];
    if let Some(published_at) = symbol.module_published_at.as_deref() {
        header_lines.insert(
            2,
            Line::from(vec![
                Span::styled("Published     ", label_style),
                Span::styled(published_at.to_string(), value_style),
            ]),
        );
    }
    if !symbol.datasheet.is_empty() {
        header_lines.push(Line::from(vec![
            Span::styled("Datasheet     ", label_style),
            Span::styled(
                module_relative_url(&symbol.datasheet, &symbol.module_url),
                value_style,
            ),
        ]));
    }

    if let Some(keywords) = symbol
        .kicad_keywords
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        header_lines.push(Line::from(vec![
            Span::styled("Keywords      ", label_style),
            Span::styled(keywords.to_string(), Style::default().fg(Color::DarkGray)),
        ]));
    }

    let (image, is_loading_image) = app.registry_symbol_image(symbol);
    let should_reserve_image_panel = app.image_protocol.is_supported()
        && app.picker.is_some()
        && registry_symbol_has_image(symbol);
    if should_reserve_image_panel {
        let header_height = header_lines.len() as u16;
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(header_height),
                Constraint::Length(1),
                Constraint::Length(8),
                Constraint::Min(0),
            ])
            .split(area);

        frame.render_widget(Paragraph::new(header_lines), chunks[0]);
        if let Some(image) = image {
            render_image_bytes(frame, app, image.as_slice(), chunks[2]);
        } else if is_loading_image {
            frame.render_widget(
                Paragraph::new("Loading image...")
                    .style(Style::default().fg(Color::DarkGray))
                    .alignment(Alignment::Center),
                chunks[2],
            );
        }

        let body_lines = render_registry_symbol_body_lines(
            app,
            symbol,
            chunks[3].width,
            label_style,
            value_style,
            dim_style,
        );
        frame.render_widget(Paragraph::new(body_lines), chunks[3]);
        return;
    }

    let body_lines = render_registry_symbol_body_lines(
        app,
        symbol,
        area.width,
        label_style,
        value_style,
        dim_style,
    );
    header_lines.extend(body_lines);
    frame.render_widget(Paragraph::new(header_lines), area);
}

fn render_registry_symbol_body_lines(
    app: &mut App,
    symbol: &RegistrySymbol,
    width: u16,
    label_style: Style,
    value_style: Style,
    dim_style: Style,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    if let Some(kicad_description) = symbol
        .kicad_description
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "─── Description ───",
            Style::default().fg(Color::DarkGray),
        )));
        for chunk in wrap_text(kicad_description, width.saturating_sub(4) as usize) {
            lines.push(Line::from(Span::styled(chunk, value_style)));
        }
    }

    if let Some(ref dk) = symbol.digikey
        && !dk.parameters.is_empty()
    {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "─── Parameters ───",
            Style::default().fg(Color::DarkGray),
        )));
        append_parameters(&mut lines, &dk.parameters, label_style, value_style);
        lines.push(Line::from(""));
    }

    let (availability, is_loading) =
        app.availability_for_lookup(Some(&symbol.mpn), Some(&symbol.manufacturer));

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "─── Availability ───",
        Style::default().fg(Color::DarkGray),
    )));

    lines.push(format_avail_line(
        "US",
        availability.and_then(|p| p.us.as_ref()),
        is_loading,
    ));
    lines.push(format_avail_line(
        "Global",
        availability.and_then(|p| p.global.as_ref()),
        is_loading,
    ));

    if let Some(p) = availability {
        let in_stock: Vec<_> = p.offers.iter().filter(|o| o.stock > 0).take(6).collect();
        if !in_stock.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "─── Offers ───",
                Style::default().fg(Color::DarkGray),
            )));
            lines.extend(format_offer_lines(&in_stock));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "─── Search Scoring ───",
        Style::default().fg(Color::DarkGray),
    )));
    append_search_scoring_for_url(&mut lines, app, &symbol.url, dim_style);

    lines
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
fn append_search_scoring_for_url(lines: &mut Vec<Line>, app: &App, url: &str, dim_style: Style) {
    let scoring = app.results.scoring().get(url);
    let (trigram_len, word_len, docs_len, semantic_len, merged_len) = match &app.results {
        super::search::SearchResults::RegistryModules(results) => (
            results.trigram.len(),
            results.word.len(),
            results.docs_full_text.len(),
            results.semantic.len(),
            results.merged.len(),
        ),
        super::search::SearchResults::RegistrySymbols(results) => (
            results.trigram.len(),
            results.word.len(),
            results.docs_full_text.len(),
            results.semantic.len(),
            results.merged.len(),
        ),
        super::search::SearchResults::KicadSymbols(results) => (
            results.trigram.len(),
            results.word.len(),
            results.docs_full_text.len(),
            results.semantic.len(),
            results.merged.len(),
        ),
        super::search::SearchResults::Empty => (0, 0, 0, 0, 0),
    };

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
    let (docs_pos, docs_rank) = scoring
        .map(|s| (s.docs_full_text_position, s.docs_full_text_rank))
        .unwrap_or((None, None));
    let (sem_pos, sem_rank) = scoring
        .map(|s| (s.semantic_position, s.semantic_rank))
        .unwrap_or((None, None));

    let mut tri_line = vec![Span::styled(
        "Trigram  ",
        Style::default().fg(Color::Yellow),
    )];
    tri_line.extend(format_result(tri_pos, tri_rank, trigram_len));
    lines.push(Line::from(tri_line));

    let mut word_line = vec![Span::styled("Word     ", Style::default().fg(Color::Green))];
    word_line.extend(format_result(word_pos, word_rank, word_len));
    lines.push(Line::from(word_line));

    let mut docs_line = vec![Span::styled(
        "Docs     ",
        Style::default().fg(Color::LightMagenta),
    )];
    docs_line.extend(format_result(docs_pos, docs_rank, docs_len));
    lines.push(Line::from(docs_line));

    let mut sem_line = vec![Span::styled("Semantic ", Style::default().fg(Color::Cyan))];
    sem_line.extend(format_result(sem_pos, sem_rank, semantic_len));
    lines.push(Line::from(sem_line));

    // RRF score calculation
    let rrf = |pos: Option<usize>| {
        pos.map(|p| 1.0 / (crate::registry::RRF_K + (p + 1) as f64))
            .unwrap_or(0.0)
    };
    let rrf_score = rrf(tri_pos) + rrf(word_pos) + rrf(docs_pos) + rrf(sem_pos);

    let rrf_parts: Vec<String> = [tri_pos, word_pos, docs_pos, sem_pos]
        .iter()
        .filter_map(|&pos| {
            pos.map(|p| {
                format!(
                    "1/(10+{})={:.3}",
                    p + 1,
                    1.0 / (crate::registry::RRF_K + (p + 1) as f64)
                )
            })
        })
        .collect();

    let mut merged_line = vec![
        Span::styled("Merged   ", Style::default().fg(Color::Magenta)),
        Span::styled(
            format!("#{}", app.selected_index() + 1),
            Style::default().fg(Color::White),
        ),
        Span::styled(format!("/{}", merged_len), dim_style),
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
fn render_dependency_tree(lines: &mut Vec<Line>, deps: &[RegistryModuleDependency]) {
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

        let version_suffix = if dep.version.is_empty() {
            String::new()
        } else {
            format!("@{}", dep.version)
        };

        lines.push(Line::from(vec![
            Span::styled(registry_prefix, Style::default().fg(Color::Gray)),
            Span::styled(rest_path, Style::default().fg(Color::Blue)),
            Span::styled(version_suffix, Style::default().fg(Color::DarkGray)),
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
        super::app::SearchMode::KicadSymbols => Color::Cyan,
        super::app::SearchMode::WebComponents => Color::Cyan,
    };

    // Mode-specific Enter action
    let enter_action = if app.mode.requires_local_index() {
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
            if app.available_modes.iter().any(|m| m.requires_local_index()) {
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
            let is_enabled = cmd.is_enabled(app.selected_symbol.as_ref(), &app.available_modes);

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

/// Render a scrollbar with stable thumb size (custom impl avoids ratatui's ±1 cell wobble)
fn render_scrollbar(
    frame: &mut Frame,
    area: Rect,
    total: usize,
    offset: usize,
    item_height: usize,
) {
    let visible = area.height as usize / item_height;
    let max_offset = total.saturating_sub(visible);
    if max_offset == 0 {
        return;
    }

    let offset = offset.min(max_offset);
    let track = area.height as usize;
    let thumb_len = (visible * track / total).clamp(1, track);
    let max_start = track - thumb_len;
    // Round to nearest instead of floor to distribute jumps more evenly
    let start = ((max_offset - offset) * max_start + max_offset / 2) / max_offset;

    let buf = frame.buffer_mut();
    for i in 0..track {
        let in_thumb = i >= start && i < start + thumb_len;
        buf[(area.x, area.y + i as u16)]
            .set_symbol(if in_thumb { "┃" } else { "│" })
            .set_style(Style::default().fg(if in_thumb {
                Color::Magenta
            } else {
                Color::DarkGray
            }));
    }
}
