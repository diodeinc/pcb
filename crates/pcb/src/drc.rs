use comfy_table::{presets, Attribute, Cell, ContentArrangement, Table};
use pcb_ui::prelude::*;
use pcb_zen_core::diagnostics::{DiagnosticsPass, Severity};
use pcb_zen_core::passes::{FilterHiddenPass, SortPass, SuppressPass};
use starlark::errors::EvalSeverity;

type ColorFn = fn(String) -> colored::ColoredString;

/// Render diagnostics (filter, print, show summary table)
pub fn render_diagnostics(diagnostics: &mut pcb_zen_core::Diagnostics, suppress_kinds: &[String]) {
    // Apply filter passes
    for pass in [
        &FilterHiddenPass as &dyn DiagnosticsPass,
        &SuppressPass::new(suppress_kinds.to_vec()),
        &SortPass,
    ] {
        pass.apply(diagnostics);
    }

    // Print diagnostics
    for diagnostic in &diagnostics.diagnostics {
        if !diagnostic.suppressed {
            if let Some((severity_str, severity_color)) = match diagnostic.severity {
                EvalSeverity::Error => Some(("Error", Style::Red)),
                EvalSeverity::Warning => Some(("Warning", Style::Yellow)),
                EvalSeverity::Advice => Some(("Advice", Style::Blue)),
                EvalSeverity::Disabled => None,
            } {
                let lines: Vec<&str> = diagnostic.body.lines().collect();
                if let Some(first_line) = lines.first() {
                    eprintln!(
                        "{}: {}",
                        severity_str.with_style(severity_color).bold(),
                        first_line
                    );
                    for line in lines.iter().skip(1) {
                        eprintln!("{}", line.dimmed());
                    }
                }
            }
        }
    }

    // Print summary table
    if !diagnostics.diagnostics.is_empty() {
        eprintln!();
        let mut table = Table::new();
        table
            .load_preset(presets::UTF8_BORDERS_ONLY)
            .set_content_arrangement(ContentArrangement::Dynamic);

        // Severity columns: (severity, header_name, color_fn)
        let columns: [(Severity, &str, ColorFn); 2] = [
            (Severity::Error, "Errors", |s| s.red()),
            (Severity::Warning, "Warnings", |s| s.yellow()),
        ];

        // Header row
        let mut header = vec![Cell::new("Category").add_attribute(Attribute::Bold)];
        for (_, name, color_fn) in &columns {
            header.push(Cell::new(format!(
                "{} {}",
                color_fn(name.to_string()).bold(),
                "(excluded)".dimmed()
            )));
        }
        table.set_header(header);

        // Get counts and categories
        let counts = diagnostics.counts();
        let mut categories: Vec<_> = counts.keys().map(|(k, _, _)| k.as_str()).collect();
        categories.sort();
        categories.dedup();

        // Totals per severity
        let mut totals: [(usize, usize); 2] = [(0, 0), (0, 0)];

        // Category rows
        for category in &categories {
            let mut row = vec![Cell::new(*category)];
            for (i, (sev, _, color_fn)) in columns.iter().enumerate() {
                let active = counts
                    .get(&(category.to_string(), *sev, false))
                    .copied()
                    .unwrap_or(0);
                let suppressed = counts
                    .get(&(category.to_string(), *sev, true))
                    .copied()
                    .unwrap_or(0);
                totals[i].0 += active;
                totals[i].1 += suppressed;
                row.push(Cell::new(format_count(active, suppressed, color_fn)));
            }
            table.add_row(row);
        }

        // Total row
        let mut total_row = vec![Cell::new("Total").add_attribute(Attribute::Bold)];
        for (i, (_, _, color_fn)) in columns.iter().enumerate() {
            total_row.push(Cell::new(format_count(totals[i].0, totals[i].1, |s| {
                color_fn(s).bold()
            })));
        }
        table.add_row(total_row);

        eprintln!("{}", table);
    }
}

/// Format count with optional excluded count in parentheses
fn format_count<F>(count: usize, excluded: usize, color_fn: F) -> String
where
    F: Fn(String) -> colored::ColoredString,
{
    match (count, excluded) {
        (0, 0) => "-".dimmed().to_string(),
        (0, e) => format!("({})", e).dimmed().to_string(),
        (c, 0) => color_fn(c.to_string()).to_string(),
        (c, e) => format!(
            "{} {}",
            color_fn(c.to_string()),
            format!("({})", e).dimmed()
        ),
    }
}
