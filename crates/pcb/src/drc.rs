use comfy_table::{presets, Attribute, Cell, ContentArrangement, Table};
use pcb_ui::prelude::*;
use pcb_zen_core::diagnostics::{
    compact_diagnostic, diagnostic_headline, diagnostic_location, DiagnosticsPass, Severity,
};
use pcb_zen_core::passes::{FilterHiddenPass, SuppressPass};
use starlark::errors::EvalSeverity;
use std::collections::{BTreeSet, HashMap};

type ColorFn = fn(String) -> colored::ColoredString;

/// Render diagnostics (filter, print, show summary table)
pub fn render_diagnostics(diagnostics: &mut pcb_zen_core::Diagnostics, suppress_kinds: &[String]) {
    // Apply filter passes
    for pass in [
        &FilterHiddenPass as &dyn DiagnosticsPass,
        &SuppressPass::new(suppress_kinds.to_vec()),
    ] {
        pass.apply(diagnostics);
    }

    // Print diagnostics
    let mut ordered: Vec<_> = diagnostics.diagnostics.iter().collect();
    ordered.sort_by_key(|d| match d.severity {
        // Put more severe items later so errors appear at the bottom.
        EvalSeverity::Disabled => 0u8,
        EvalSeverity::Advice => 1u8,
        EvalSeverity::Warning => 2u8,
        EvalSeverity::Error => 3u8,
    });

    for diagnostic in ordered {
        if !diagnostic.suppressed {
            if let Some((severity_str, severity_color)) = match diagnostic.severity {
                EvalSeverity::Error => Some(("Error", Style::Red)),
                EvalSeverity::Warning => Some(("Warning", Style::Yellow)),
                EvalSeverity::Advice => Some(("Advice", Style::Blue)),
                EvalSeverity::Disabled => None,
            } {
                let parts = compact_diagnostic(diagnostic);
                if !parts.first_line.is_empty() {
                    eprintln!(
                        "{}: {}",
                        severity_str.with_style(severity_color).bold(),
                        diagnostic_headline(diagnostic)
                    );
                    for line in parts.extra_lines {
                        eprintln!("{}", line.dimmed());
                    }
                    if let Some(loc) = diagnostic_location(diagnostic) {
                        eprintln!("{}", format!("  at {loc}").dimmed());
                    }
                }
            }
        }
    }

    // Print summary table
    if !diagnostics.diagnostics.is_empty() {
        eprintln!();
        let counts = diagnostics.counts();

        // Show DRC and parity in separate tables, since they represent very different
        // concepts during import. Keep any remaining kinds in an "Other" table.
        let mut sections: Vec<(&str, Vec<String>)> = Vec::new();

        let layout_drc = categories_from_counts(&counts, |k| {
            k.starts_with("layout.drc.") || k.starts_with("layout.unconnected.")
        });
        if !layout_drc.is_empty() {
            sections.push(("Layout DRC", layout_drc));
        }

        let schematic_erc = categories_from_counts(&counts, |k| k.starts_with("schematic.erc."));
        if !schematic_erc.is_empty() {
            sections.push(("Schematic ERC", schematic_erc));
        }

        let excluded_prefixes = [
            "layout.drc.",
            "layout.unconnected.",
            "schematic.erc.",
            "layout.parity.",
        ];
        let other = categories_from_counts(&counts, |k| {
            !excluded_prefixes.iter().any(|prefix| k.starts_with(prefix))
        });
        if !other.is_empty() {
            sections.push(("Other", other));
        }

        // Keep layout parity last.
        let layout_parity = categories_from_counts(&counts, |k| k.starts_with("layout.parity."));
        if !layout_parity.is_empty() {
            sections.push(("Layout Parity", layout_parity));
        }

        for (idx, (title, categories)) in sections.iter().enumerate() {
            render_summary_table(title, categories, &counts);
            if idx + 1 != sections.len() {
                eprintln!();
            }
        }

        // Keep output readable when only non-parity tables are present, but avoid an
        // extra blank line after the parity table (it is often followed immediately by
        // a blocking-issues recap).
        if sections
            .last()
            .is_some_and(|(title, _)| *title != "Layout Parity")
        {
            eprintln!();
        }
    }
}

fn categories_from_counts<F>(
    counts: &HashMap<(String, Severity, bool), usize>,
    mut keep: F,
) -> Vec<String>
where
    F: FnMut(&str) -> bool,
{
    let categories: BTreeSet<String> = counts
        .keys()
        .map(|(k, _, _)| k.as_str())
        .filter(|k| keep(k))
        .map(|k| k.to_string())
        .collect();
    categories.into_iter().collect()
}

fn render_summary_table(
    title: &str,
    categories: &[String],
    counts: &HashMap<(String, Severity, bool), usize>,
) {
    eprintln!("{}", title.bold());

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

    // Totals per severity
    let mut totals: [(usize, usize); 2] = [(0, 0), (0, 0)];

    // Category rows
    for category in categories {
        let mut row = vec![Cell::new(category)];
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
