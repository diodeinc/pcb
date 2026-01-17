use comfy_table::{presets, Attribute, Cell, ContentArrangement, Table};
use pcb_ui::prelude::*;
use pcb_zen_core::diagnostics::{DiagnosticsPass, Severity};
use pcb_zen_core::passes::{FilterHiddenPass, SortPass, SuppressPass};
use starlark::errors::EvalSeverity;

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
                _ => None,
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

        table.set_header(vec![
            Cell::new("Category").add_attribute(Attribute::Bold),
            Cell::new(format!(
                "{} {}",
                "Errors".red().bold(),
                "(excluded)".dimmed()
            )),
            Cell::new(format!(
                "{} {}",
                "Warnings".yellow().bold(),
                "(excluded)".dimmed()
            )),
        ]);

        let counts = diagnostics.counts();
        let mut categories: Vec<_> = counts.keys().map(|(k, _, _)| k.as_str()).collect();
        categories.sort();
        categories.dedup();

        for category in &categories {
            let get = |sev, sup| {
                counts
                    .get(&(category.to_string(), sev, sup))
                    .copied()
                    .unwrap_or(0)
            };
            table.add_row(vec![
                Cell::new(category),
                Cell::new(format_count(
                    get(Severity::Error, false),
                    get(Severity::Error, true),
                    |s| s.red(),
                )),
                Cell::new(format_count(
                    get(Severity::Warning, false),
                    get(Severity::Warning, true),
                    |s| s.yellow(),
                )),
            ]);
        }

        table.add_row(vec![
            Cell::new("Total").add_attribute(Attribute::Bold),
            Cell::new(format_count(
                diagnostics.error_count(),
                diagnostics.suppressed_error_count(),
                |s| s.red().bold(),
            )),
            Cell::new(format_count(
                diagnostics.warning_count(),
                diagnostics.suppressed_warning_count(),
                |s| s.yellow().bold(),
            )),
        ]);

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
