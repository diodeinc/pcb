use ariadne::{sources, ColorGenerator, Label, Report, ReportKind};
use pcb_ui::Colorize;
use starlark::errors::EvalSeverity;
use std::collections::HashMap;
use std::ops::Range;

use crate::{Diagnostic, Diagnostics};

/// A pass that renders diagnostics to the console using Ariadne
pub struct RenderPass;

impl pcb_zen_core::DiagnosticsPass for RenderPass {
    fn apply(&self, diagnostics: &mut Diagnostics) {
        for diag in &diagnostics.diagnostics {
            render_diagnostic(diag);
            eprintln!();
        }
    }
}

/// Render a [`Diagnostic`] using the `ariadne` crate.
///
/// All related diagnostics that refer to the same file are rendered together in a
/// single coloured report so that the context is easy to follow.
/// Diagnostics that originate from a different file fall back to a separate
/// Ariadne report (or a plain `eprintln!` when source code cannot be read).
fn render_diagnostic(diagnostic: &Diagnostic) {
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

    // If we failed to read the primary file source, fall back to plain printing.
    let primary_src = sources_map.get(&diagnostic.path);
    if primary_src.is_none() {
        for m in &messages {
            eprintln!("{m}");
        }
        return;
    }

    // Identify deepest message in the chain.
    let deepest_error_msg: &Diagnostic = messages.last().copied().unwrap_or(diagnostic);

    // Determine ReportKind from deepest error severity (more relevant).
    let kind = match deepest_error_msg.severity {
        EvalSeverity::Error => ReportKind::Error,
        EvalSeverity::Warning => ReportKind::Warning,
        EvalSeverity::Advice => ReportKind::Advice,
        EvalSeverity::Disabled => ReportKind::Advice,
    };

    // Compute span for deepest message.
    let primary_src_str = sources_map.get(&deepest_error_msg.path).unwrap();
    let primary_span = compute_span(primary_src_str, deepest_error_msg);
    if primary_span.is_none() {
        for m in &messages {
            eprintln!("{m}");
        }
        return;
    }

    // Build report with colours.
    let mut colors = ColorGenerator::new();
    let red = colors.next(); // red-ish for the deepest (primary) error
    let yellow = colors.next(); // yellow for all other messages in the chain

    let primary_path_id = deepest_error_msg.path.clone();

    let compact = !matches!(deepest_error_msg.severity, EvalSeverity::Error);

    // Create message with suppressed count if any
    let message = if let Some(count) = diagnostic.suppressed_count() {
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
        deepest_error_msg.body.clone()
    };

    let mut report = Report::build(
        kind,
        (primary_path_id.clone(), primary_span.clone().unwrap()),
    )
    .with_config(ariadne::Config::default().with_compact(compact))
    .with_message(&message)
    .with_label(
        Label::new((primary_path_id.clone(), primary_span.unwrap()))
            .with_message(&deepest_error_msg.body)
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
                        .with_message(&msg.body)
                        .with_color(yellow)
                        .with_order((idx + 2) as i32), // Order 1 is the primary, so start from 2
                );
            }
        }
    }

    // Prepare sources for printing (plain strings are fine – Ariadne wraps them).
    let src_vec: Vec<(String, String)> = sources_map.into_iter().collect();

    // Print the report.
    let _ = report.finish().print(sources(src_vec));

    // Build helper for rendering locations.
    let render_loc = |msg: &Diagnostic| -> String {
        if let Some(sp) = &msg.span {
            format!("{}:{}:{}", msg.path, sp.begin.line + 1, sp.begin.column + 1)
        } else {
            msg.path.clone()
        }
    };

    // Gather diagnostics from outer-most to inner-most.
    let mut chain: Vec<&Diagnostic> = Vec::new();
    let mut current: Option<&Diagnostic> = Some(diagnostic);
    while let Some(d) = current {
        chain.push(d);
        current = d.child.as_deref();
    }

    if !chain.is_empty() && !compact {
        eprintln!("\nStack trace (most recent call last):");

        for (idx, d) in chain.iter().enumerate() {
            let is_last_diag = idx + 1 == chain.len();

            // Instantiation location + message (plain, no tree chars).
            eprintln!("    {} ({})", render_loc(d), d.body);

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

                    eprintln!("{base_indent}{branch}{frame}");
                }
            }
        }
    }
}

/// Compute the byte-range `Span` inside `source` (UTF-8 string) that corresponds to `msg.span`.
fn compute_span(source: &str, msg: &Diagnostic) -> Option<Range<usize>> {
    let Some(span) = &msg.span else { return None };

    // Compute byte offsets for the span based on line + column info.
    let mut offset = 0usize;
    let mut begin_byte = None;
    let mut end_byte = None;

    for (idx, line) in source.lines().enumerate() {
        let line_len = line.len() + 1; // +1 for newline character
        if idx == span.begin.line {
            begin_byte = Some(offset + span.begin.column);
        }
        if idx == span.end.line {
            end_byte = Some(offset + span.end.column);
            break;
        }
        offset += line_len;
    }

    match (begin_byte, end_byte) {
        (Some(b), Some(e)) if b < e && e <= source.len() => Some(b..e),
        _ => None,
    }
}
