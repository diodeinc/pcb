use anyhow::{Context, Result};
use pcb_layout::LayoutSyncDiagnostic;
use pcb_ui::prelude::*;
use pcb_zen_core::diagnostics::{Diagnostic, DiagnosticsPass};
use pcb_zen_core::lang::error::CategorizedDiagnostic;
use pcb_zen_core::passes::{FilterHiddenPass, SortPass, SuppressPass};
use starlark::errors::EvalSeverity;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

/// Run KiCad DRC checks and print results
///
/// Returns (had_errors, warning_count)
pub fn run_and_print_drc(
    kicad_pcb_path: &Path,
    suppress_kinds: &[String],
    sync_diagnostics: &[LayoutSyncDiagnostic],
) -> Result<(bool, usize)> {
    let drc_report = pcb_kicad::run_drc(kicad_pcb_path).context("Failed to run KiCad DRC")?;
    let mut diagnostics = drc_report.to_diagnostics(&kicad_pcb_path.to_string_lossy());
    add_sync_diagnostics(&mut diagnostics, sync_diagnostics, kicad_pcb_path);
    filter_and_print(&mut diagnostics, suppress_kinds)
}

/// Print sync diagnostics only (without running DRC)
///
/// Returns (had_errors, warning_count)
pub fn print_sync_diagnostics(
    kicad_pcb_path: &Path,
    suppress_kinds: &[String],
    sync_diagnostics: &[LayoutSyncDiagnostic],
) -> Result<(bool, usize)> {
    if sync_diagnostics.is_empty() {
        return Ok((false, 0));
    }
    let mut diagnostics = pcb_zen_core::Diagnostics::default();
    add_sync_diagnostics(&mut diagnostics, sync_diagnostics, kicad_pcb_path);
    filter_and_print(&mut diagnostics, suppress_kinds)
}

fn add_sync_diagnostics(
    diagnostics: &mut pcb_zen_core::Diagnostics,
    sync_diagnostics: &[LayoutSyncDiagnostic],
    pcb_path: &Path,
) {
    let pcb_path_str = pcb_path.to_string_lossy();
    for sync_diag in sync_diagnostics {
        if let Ok(diag) = convert_sync_diagnostic(sync_diag, &pcb_path_str) {
            diagnostics.diagnostics.push(diag);
        }
    }
}

fn filter_and_print(
    diagnostics: &mut pcb_zen_core::Diagnostics,
    suppress_kinds: &[String],
) -> Result<(bool, usize)> {
    for pass in [
        &FilterHiddenPass as &dyn DiagnosticsPass,
        &SuppressPass::new(suppress_kinds.to_vec()),
        &SortPass,
    ] {
        pass.apply(diagnostics);
    }
    print_diagnostics(diagnostics)
}

/// Convert a LayoutSyncDiagnostic to a standard Diagnostic
fn convert_sync_diagnostic(sync_diag: &LayoutSyncDiagnostic, pcb_path: &str) -> Result<Diagnostic> {
    let severity = match sync_diag.severity.as_str() {
        "error" => EvalSeverity::Error,
        "warning" => EvalSeverity::Warning,
        _ => EvalSeverity::Warning,
    };

    // Include reference designator if available
    let kind_short = sync_diag.kind.rsplit('.').next().unwrap_or(&sync_diag.kind);
    let body = match &sync_diag.reference {
        Some(ref_des) => format!("[{}] {}: {}", kind_short, ref_des, sync_diag.body),
        None => format!("[{}] {}", kind_short, sync_diag.body),
    };

    let categorized = CategorizedDiagnostic::new(body.clone(), sync_diag.kind.clone())?;

    Ok(Diagnostic {
        path: pcb_path.to_string(),
        span: None,
        severity,
        body,
        call_stack: None,
        child: None,
        source_error: Some(Arc::new(anyhow::Error::new(categorized))),
        suppressed: false,
    })
}

/// Print diagnostics and return (had_errors, warning_count)
fn print_diagnostics(diagnostics: &pcb_zen_core::Diagnostics) -> Result<(bool, usize)> {
    use comfy_table::{presets, Attribute, Cell, ContentArrangement, Table};

    let mut category_counts: HashMap<String, (usize, usize, usize, usize)> = HashMap::new();
    let mut errors = 0;
    let mut warnings = 0;
    let mut suppressed_errors = 0;
    let mut suppressed_warnings = 0;

    for diagnostic in &diagnostics.diagnostics {
        let category = diagnostic
            .source_error
            .as_ref()
            .and_then(|e| e.downcast_ref::<CategorizedDiagnostic>())
            .map(|c| c.kind.as_str())
            .unwrap_or("other");

        let entry = category_counts
            .entry(category.to_string())
            .or_insert((0, 0, 0, 0));

        match (diagnostic.severity, diagnostic.suppressed) {
            (EvalSeverity::Error, false) => {
                entry.0 += 1;
                errors += 1;
            }
            (EvalSeverity::Error, true) => {
                entry.1 += 1;
                suppressed_errors += 1;
            }
            (EvalSeverity::Warning, false) => {
                entry.2 += 1;
                warnings += 1;
            }
            (EvalSeverity::Warning, true) => {
                entry.3 += 1;
                suppressed_warnings += 1;
            }
            _ => {}
        }

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

        let mut sorted_categories: Vec<_> = category_counts.iter().collect();
        sorted_categories.sort_by_key(|(k, _)| *k);

        for (category, (e, se, w, sw)) in sorted_categories {
            table.add_row(vec![
                Cell::new(category),
                Cell::new(format_count(*e, *se, |s| s.red())),
                Cell::new(format_count(*w, *sw, |s| s.yellow())),
            ]);
        }

        table.add_row(vec![
            Cell::new("Total").add_attribute(Attribute::Bold),
            Cell::new(format_count(errors, suppressed_errors, |s| s.red().bold())),
            Cell::new(format_count(warnings, suppressed_warnings, |s| {
                s.yellow().bold()
            })),
        ]);

        eprintln!("{}", table);
    }

    if errors > 0 {
        eprintln!(
            "\n{} DRC check failed with {} error(s)",
            pcb_ui::icons::error(),
            errors
        );
    }

    Ok((errors > 0, warnings))
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
