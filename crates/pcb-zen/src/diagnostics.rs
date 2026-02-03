use ariadne::{sources, ColorGenerator, Label, Report, ReportKind};
use pcb_ui::prelude::*;
use pcb_zen_core::diagnostics::{compact_diagnostic, diagnostic_headline, diagnostic_location};
use starlark::errors::EvalSeverity;
use std::collections::HashMap;
use std::io::Write;
use std::ops::Range;

use crate::{Diagnostic, Diagnostics};

/// Render all diagnostics to a string for snapshot testing.
pub fn render_diagnostics_to_string(diagnostics: &Diagnostics) -> String {
    let mut output = Vec::new();
    for diag in &diagnostics.diagnostics {
        render_diagnostic_to_writer(diag, &mut output, false);
        writeln!(output).ok();
    }
    String::from_utf8(output).unwrap_or_default()
}

/// A pass that renders diagnostics to the console using Ariadne
pub struct RenderPass;

impl pcb_zen_core::DiagnosticsPass for RenderPass {
    fn apply(&self, diagnostics: &mut Diagnostics) {
        // Count suppressed diagnostics by severity
        let mut suppressed_errors = 0;
        let mut suppressed_warnings = 0;

        // Render non-suppressed diagnostics
        for diag in &diagnostics.diagnostics {
            // Skip advice severity diagnostics to reduce noise
            if matches!(diag.severity, EvalSeverity::Advice) {
                continue;
            }

            // Skip suppressed diagnostics - we'll summarize them at the end
            if diag.suppressed {
                match diag.severity {
                    EvalSeverity::Error => suppressed_errors += 1,
                    EvalSeverity::Warning => suppressed_warnings += 1,
                    _ => {}
                }
                continue;
            }

            render_diagnostic_to_writer(diag, &mut std::io::stderr(), true);
            eprintln!();
        }

        // Print summary of suppressed diagnostics
        use pcb_ui::prelude::*;

        if suppressed_errors > 0 || suppressed_warnings > 0 {
            let mut parts = Vec::new();

            if suppressed_errors > 0 {
                let error_text = format!(
                    "{} error{}",
                    suppressed_errors,
                    if suppressed_errors == 1 { "" } else { "s" }
                );
                parts.push(error_text.red().to_string());
            }

            if suppressed_warnings > 0 {
                let warning_text = format!(
                    "{} warning{}",
                    suppressed_warnings,
                    if suppressed_warnings == 1 { "" } else { "s" }
                );
                parts.push(warning_text.yellow().to_string());
            }

            eprintln!("{} {}", "Suppressed".dimmed(), parts.join(", "));
        }
    }
}

/// Extract the first line from a message for use in span labels.
/// Multi-line messages are displayed in full at the top of the report,
/// but only the first line is shown in span annotations to reduce clutter.
fn first_line(message: &str) -> &str {
    message.lines().next().unwrap_or(message)
}

/// Render a [`Diagnostic`] using the `ariadne` crate.
///
/// All related diagnostics that refer to the same file are rendered together in a
/// single coloured report so that the context is easy to follow.
/// Diagnostics that originate from a different file fall back to a separate
/// Ariadne report (or a plain `eprintln!` when source code cannot be read).
fn render_diagnostic_to_writer<W: Write>(diagnostic: &Diagnostic, writer: &mut W, color: bool) {
    // Collect all EvalMessages in the diagnostic chain (primary + children) for convenience.
    fn collect_messages<'a>(d: &'a Diagnostic, out: &mut Vec<&'a Diagnostic>) {
        out.push(d);
        if let Some(child) = &d.child {
            collect_messages(child, out);
        }
    }

    let mut messages: Vec<&Diagnostic> = Vec::new();
    collect_messages(diagnostic, &mut messages);

    // 0. Attempt to read source for every file referenced by any message that has a span.
    let mut sources_map: HashMap<String, String> = HashMap::new();
    for msg in &messages {
        if msg.span.is_some() {
            let path = msg.path.clone();
            sources_map
                .entry(path.clone())
                .or_insert_with(|| std::fs::read_to_string(&path).unwrap_or_default());
        }
    }

    // Identify deepest message in the chain.
    let deepest_error_msg: &Diagnostic = messages.last().copied().unwrap_or(diagnostic);

    // Helper to render fallback (innermost first, then parents as context)
    let render_fallback = |writer: &mut W| {
        let (severity_str, severity_style) = match diagnostic.severity {
            EvalSeverity::Error => ("Error", Style::Red),
            EvalSeverity::Warning => ("Warning", Style::Yellow),
            EvalSeverity::Advice => ("Advice", Style::Blue),
            EvalSeverity::Disabled => ("Advice", Style::Blue),
        };

        let sev = if color {
            severity_str.with_style(severity_style).bold().to_string()
        } else {
            severity_str.to_string()
        };

        let parts = compact_diagnostic(deepest_error_msg);
        writeln!(writer, "{sev}: {}", diagnostic_headline(deepest_error_msg)).ok();

        for line in parts.extra_lines {
            if color {
                writeln!(writer, "{}", line.dimmed()).ok();
            } else {
                writeln!(writer, "{line}").ok();
            }
        }

        // Print location on a separate dimmed line (span if available, otherwise path only).
        if let Some(loc) = diagnostic_location(deepest_error_msg) {
            if color {
                writeln!(writer, "  {}", format!("at {loc}").dimmed()).ok();
            } else {
                writeln!(writer, "  at {loc}").ok();
            }
        }

        // Print parent context (from innermost to outermost, skipping deepest).
        // Keep it compact and consistent with other spanless renderers.
        for msg in messages.iter().rev().skip(1) {
            let ctx = compact_diagnostic(msg);
            let loc = diagnostic_location(msg).unwrap_or_default();
            let line = if loc.is_empty() {
                format!("in {}", ctx.first_line)
            } else {
                format!("in {loc}: {}", ctx.first_line)
            };
            if color {
                writeln!(writer, "  {}", line.dimmed()).ok();
            } else {
                writeln!(writer, "  {line}").ok();
            }
        }
    };

    // Determine ReportKind from parent (outermost) diagnostic severity
    // This allows wrapper diagnostics (like electrical checks) to control the severity
    let kind = match diagnostic.severity {
        EvalSeverity::Error => ReportKind::Error,
        EvalSeverity::Warning => ReportKind::Warning,
        EvalSeverity::Advice => ReportKind::Advice,
        EvalSeverity::Disabled => ReportKind::Advice,
    };

    // Compute span for deepest message. Fall back to plain rendering if we can't.
    let primary_src_str = match sources_map.get(&deepest_error_msg.path) {
        Some(src) => src,
        None => {
            render_fallback(writer);
            return;
        }
    };
    let primary_span = match compute_span(primary_src_str, deepest_error_msg) {
        Some(span) => span,
        None => {
            render_fallback(writer);
            return;
        }
    };

    // Build report with colours.
    let mut colors = ColorGenerator::new();
    let red = colors.next(); // red-ish for the deepest (primary) error
    let yellow = colors.next(); // yellow for all other messages in the chain

    let primary_path_id = deepest_error_msg.path.clone();

    let compact = !matches!(deepest_error_msg.severity, EvalSeverity::Error);

    // Create message with suppressed count if any
    let message = if let Some(count) = diagnostic.suppressed_count() {
        if color {
            format!(
                "{}\n{}",
                deepest_error_msg.body,
                format!(
                    "{} similar warning(s) were suppressed",
                    format!("{count}").bold()
                )
                .blue()
            )
        } else {
            format!(
                "{}\n{} similar warning(s) were suppressed",
                deepest_error_msg.body, count
            )
        }
    } else {
        deepest_error_msg.body.clone()
    };

    let mut report = Report::build(kind, (primary_path_id.clone(), primary_span.clone()))
        .with_config(
            ariadne::Config::default()
                .with_compact(compact)
                .with_color(color),
        )
        .with_message(&message)
        .with_label(
            Label::new((primary_path_id.clone(), primary_span))
                .with_message(first_line(&deepest_error_msg.body))
                .with_color(red),
        );

    // Add all other messages in the chain (except the deepest) in yellow.
    for (idx, msg) in messages.iter().enumerate().rev() {
        // Skip the deepest message (already added in red)
        if idx == messages.len() - 1 {
            continue;
        }

        if let Some(src) = sources_map.get(&msg.path) {
            if let Some(span) = compute_span(src, msg) {
                report = report.with_label(
                    Label::new((msg.path.clone(), span))
                        .with_message(first_line(&msg.body))
                        .with_color(yellow)
                        .with_order((idx + 2) as i32), // Order 1 is the primary, so start from 2
                );
            }
        }
    }

    // Prepare sources for printing (plain strings are fine – Ariadne wraps them).
    let src_vec: Vec<(String, String)> = sources_map.into_iter().collect();

    // Print the report.
    let _ = report.finish().write(sources(src_vec), &mut *writer);

    // Render stack trace for errors (CLI only)
    // Reuse `messages` which is already outer-to-inner order
    if color && !messages.is_empty() && !compact {
        // Build helper for rendering locations.
        let render_loc = |msg: &Diagnostic| -> String {
            if let Some(sp) = &msg.span {
                format!("{}:{}:{}", msg.path, sp.begin.line + 1, sp.begin.column + 1)
            } else {
                msg.path.clone()
            }
        };

        writeln!(writer, "\nStack trace (most recent call last):").ok();

        for (idx, d) in messages.iter().enumerate() {
            let is_last_diag = idx + 1 == messages.len();

            // Instantiation location + message (plain, no tree chars).
            writeln!(writer, "    {} ({})", render_loc(d), d.body).ok();

            // Render frames with tree characters underneath this instantiation.
            if let Some(fe) = &d.call_stack {
                for (f_idx, frame) in fe.frames.iter().enumerate() {
                    let is_last_frame = f_idx + 1 == fe.frames.len();

                    // Base indent aligns under the instantiation line.
                    let base_indent = "      ";

                    // If not the last diagnostic, skip the last stack frame
                    if !is_last_diag && is_last_frame {
                        continue;
                    }

                    let branch = if is_last_frame { "╰─ " } else { "├─ " };

                    writeln!(writer, "{base_indent}{branch}{frame}").ok();
                }
            }
        }
    }
}

/// Convert a ResolvedSpan (line/column) to a character range for Ariadne.
///
/// IMPORTANT: Ariadne expects CHARACTER indices, not BYTE indices.
/// This is crucial for files containing multi-byte UTF-8 characters.
fn compute_span(source: &str, msg: &Diagnostic) -> Option<Range<usize>> {
    let span = msg.span.as_ref()?;

    let mut char_offset = 0;
    let mut lines = source.lines().enumerate();

    // Find the start character offset
    for (line_idx, line) in &mut lines {
        if line_idx == span.begin.line {
            let start = char_offset + span.begin.column;
            let end = if line_idx == span.end.line {
                char_offset + span.end.column
            } else {
                // Multi-line span: continue counting to find end
                let mut end_offset = char_offset + line.chars().count() + 1;
                for (idx, l) in lines {
                    if idx == span.end.line {
                        return Some(start..end_offset + span.end.column);
                    }
                    end_offset += l.chars().count() + 1;
                }
                return None;
            };
            return if start < end && end <= source.chars().count() {
                Some(start..end)
            } else {
                None
            };
        }
        char_offset += line.chars().count() + 1; // +1 for newline
    }

    None
}
