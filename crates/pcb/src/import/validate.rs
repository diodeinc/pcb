use super::*;
use anyhow::{Context, Result};
use colored::Colorize;
use pcb_sexpr::{find_child_list, SexprKind};
use pcb_zen_core::diagnostics::{diagnostic_headline, diagnostic_location};
use pcb_zen_core::lang::error::CategorizedDiagnostic;
use pcb_zen_core::Diagnostics;

pub(super) fn validate(
    paths: &ImportPaths,
    selection: &ImportSelection,
    args: &ImportArgs,
) -> Result<ImportValidationRun> {
    let kicad_pro_abs = paths.kicad_project_root.join(&selection.selected.kicad_pro);
    let kicad_sch_abs = paths.kicad_project_root.join(&selection.selected.kicad_sch);
    let kicad_pcb_abs = paths.kicad_project_root.join(&selection.selected.kicad_pcb);

    if !kicad_pro_abs.exists() {
        anyhow::bail!(
            "Selected KiCad project file does not exist: {}",
            kicad_pro_abs.display()
        );
    }

    let mut diagnostics = Diagnostics::default();

    // ERC (schematic)
    let erc_report = pcb_kicad::run_erc_report(&kicad_sch_abs, Some(&paths.kicad_project_root))
        .context("KiCad ERC failed")?;
    erc_report.add_to_diagnostics(&mut diagnostics, &kicad_sch_abs.to_string_lossy());
    let (erc_errors, erc_warnings) = count_erc(&erc_report);

    // DRC + schematic parity (layout)
    let drc_report =
        pcb_kicad::run_drc_report(&kicad_pcb_abs, true, Some(&paths.kicad_project_root))
            .context("KiCad DRC failed")?;
    drc_report.add_to_diagnostics(&mut diagnostics, &kicad_pcb_abs.to_string_lossy());
    drc_report
        .add_unconnected_items_to_diagnostics(&mut diagnostics, &kicad_pcb_abs.to_string_lossy());
    drc_report
        .add_schematic_parity_to_diagnostics(&mut diagnostics, &kicad_pcb_abs.to_string_lossy());

    let (drc_errors, drc_warnings) = drc_report.violation_counts();
    let schematic_parity_violations = drc_report.schematic_parity.len();

    let pcb_text_for_parity =
        std::fs::read_to_string(&kicad_pcb_abs).context("Failed to read KiCad PCB for parity")?;
    let footprint_index = build_kicad_pcb_footprint_index(&pcb_text_for_parity).ok();

    let (schematic_parity_tolerated, schematic_parity_blocking) =
        classify_schematic_parity(&drc_report.schematic_parity, footprint_index.as_ref());
    let schematic_parity_ok = schematic_parity_blocking == 0;

    let summary = ImportValidation {
        selected: selection.selected.clone(),
        schematic_parity_ok,
        schematic_parity_violations,
        schematic_parity_tolerated,
        schematic_parity_blocking,
        erc_errors,
        erc_warnings,
        drc_errors,
        drc_warnings,
    };

    // Persist a copy of the raw diagnostics (before render filters mutate suppression state).
    let diagnostics_for_file = Diagnostics {
        diagnostics: diagnostics.diagnostics.clone(),
    };

    // Render diagnostics for the user (this is intentionally noisy and useful).
    let mut diagnostics_for_render = diagnostics;
    crate::drc::render_diagnostics(&mut diagnostics_for_render, &[]);

    if !summary.schematic_parity_ok {
        print_parity_blocking_recap(&diagnostics_for_render, 50);
        anyhow::bail!(
            "KiCad schematic/layout parity check failed: schematic and PCB appear out of sync"
        );
    }

    let error_count = diagnostics_for_render.error_count();
    if error_count > 0 {
        if args.force {
            eprintln!(
                "Warning: KiCad ERC/DRC reported {error_count} errors; continuing due to --force."
            );
        } else if !crate::tty::is_interactive() || std::env::var("CI").is_ok() {
            anyhow::bail!(
                "KiCad ERC/DRC reported {error_count} errors. Fix them, or re-run in an interactive terminal to confirm continuing."
            );
        } else {
            let continue_anyway = inquire::Confirm::new(&format!(
                "KiCad ERC/DRC reported {error_count} errors. Continue anyway?"
            ))
            .with_default(false)
            .prompt()
            .context("Failed to read confirmation")?;

            if !continue_anyway {
                anyhow::bail!("Aborted due to KiCad ERC/DRC errors");
            }
        }
    }

    Ok(ImportValidationRun {
        summary,
        diagnostics: diagnostics_for_file,
    })
}

fn print_parity_blocking_recap(diagnostics: &Diagnostics, limit: usize) {
    let mut parity_issues: Vec<_> = diagnostics
        .diagnostics
        .iter()
        .filter(|d| !d.suppressed)
        .filter(|d| is_layout_parity_diagnostic(d))
        .collect();

    if parity_issues.is_empty() {
        return;
    }

    eprintln!();
    eprintln!("{}", "Blocking issues (layout parity):".red().bold());

    let total = parity_issues.len();
    parity_issues.truncate(limit);
    for d in parity_issues {
        let headline = diagnostic_headline(d);
        if let Some(loc) = diagnostic_location(d) {
            eprintln!("  - {headline} ({loc})");
        } else {
            eprintln!("  - {headline}");
        }
    }
    if total > limit {
        eprintln!("  ... and {} more", total - limit);
    }
}

fn is_layout_parity_diagnostic(d: &pcb_zen_core::diagnostics::Diagnostic) -> bool {
    d.source_error
        .as_ref()
        .and_then(|e| e.downcast_ref::<CategorizedDiagnostic>())
        .is_some_and(|c| c.kind.starts_with("layout.parity."))
}

fn count_erc(report: &pcb_kicad::erc::ErcReport) -> (usize, usize) {
    let mut errors = 0;
    let mut warnings = 0;
    for sheet in &report.sheets {
        for v in &sheet.violations {
            match v.severity.as_str() {
                "error" => errors += 1,
                "warning" => warnings += 1,
                _ => {}
            }
        }
    }
    (errors, warnings)
}

fn classify_schematic_parity(
    parity: &[pcb_kicad::drc::DrcViolation],
    footprint_index: Option<&std::collections::BTreeMap<String, KicadPcbFootprintMeta>>,
) -> (usize, usize) {
    // Import uses KiCad's parity check as a guardrail against having a split
    // schematic/layout source of truth.
    //
    // Some parity issues are tolerable for import because the importer only
    // generates Zener components from netlist-keyed symbols/footprints, and
    // intentionally ignores any unkeyed/unlinked footprints (mechanicals,
    // tooling, experiments, etc.).
    let tolerated = parity
        .iter()
        .filter(|v| is_tolerated_parity(v, footprint_index))
        .count();
    let blocking = parity.len().saturating_sub(tolerated);
    (tolerated, blocking)
}

fn is_tolerated_parity(
    v: &pcb_kicad::drc::DrcViolation,
    footprint_index: Option<&std::collections::BTreeMap<String, KicadPcbFootprintMeta>>,
) -> bool {
    match v.violation_type.as_str() {
        // Common and acceptable during adoption: footprints placed in the PCB that are not
        // represented in the schematic/netlist (mechanical items, tooling, experimentation, etc.).
        //
        // Even if a footprint still has a stale KiCad `(path ...)`, import ignores any footprints
        // that are not present in the netlist, so extra footprints do not create a split source of
        // truth for the imported Zener design.
        "extra_footprint" => true,

        // Duplicate footprints can occur in a PCB that contains unannotated/unmanaged helper
        // footprints (often with a `**` suffix). These should not block import because the
        // importer ignores unkeyed footprints and only generates components from the netlist.
        "duplicate_footprints" => v
            .items
            .iter()
            .all(|item| is_unmanaged_footprint_item(item, footprint_index)),

        _ => false,
    }
}

#[derive(Debug, Clone)]
struct KicadPcbFootprintMeta {
    refdes: Option<String>,
    has_kicad_path: bool,
}

fn is_unmanaged_footprint_item(
    item: &pcb_kicad::drc::DrcItem,
    footprint_index: Option<&std::collections::BTreeMap<String, KicadPcbFootprintMeta>>,
) -> bool {
    if let Some(index) = footprint_index {
        if let Some(meta) = index.get(&item.uuid) {
            let is_unannotated = meta
                .refdes
                .as_deref()
                .is_some_and(|r| r.trim_end().ends_with("**"));
            return !meta.has_kicad_path || is_unannotated;
        }
    }

    // Fallback: use KiCad's rendered description. For parity diagnostics this is stable and
    // includes the footprint reference (e.g. "Footprint REF** ...").
    extract_footprint_ref_from_item_description(&item.description)
        .is_some_and(|r| r.ends_with("**"))
}

fn extract_footprint_ref_from_item_description(desc: &str) -> Option<&str> {
    // Typical KiCad DRC item: "Footprint REF** at (x, y)".
    let desc = desc.trim();
    let rest = desc.strip_prefix("Footprint ")?;
    let refdes = rest.split_whitespace().next()?;
    Some(refdes)
}

fn build_kicad_pcb_footprint_index(
    pcb_text: &str,
) -> Result<std::collections::BTreeMap<String, KicadPcbFootprintMeta>> {
    let root = pcb_sexpr::parse(pcb_text).map_err(|e| anyhow::anyhow!(e))?;
    let root_list = root
        .as_list()
        .ok_or_else(|| anyhow::anyhow!("KiCad PCB root is not a list"))?;

    let mut out: std::collections::BTreeMap<String, KicadPcbFootprintMeta> =
        std::collections::BTreeMap::new();

    for node in root_list.iter().skip(1) {
        let Some(items) = node.as_list() else {
            continue;
        };
        if items.first().and_then(|n| n.as_sym()) != Some("footprint") {
            continue;
        }

        let uuid = find_child_list(items, "uuid")
            .and_then(|l| l.get(1))
            .and_then(as_atom_str);
        let Some(uuid) = uuid else {
            continue;
        };

        let has_kicad_path = find_child_list(items, "path").is_some();
        let refdes = find_all_child_properties(items)
            .into_iter()
            .find(|(k, _)| k == "Reference")
            .map(|(_, v)| v);

        out.insert(
            uuid.to_string(),
            KicadPcbFootprintMeta {
                refdes,
                has_kicad_path,
            },
        );
    }

    Ok(out)
}

fn as_atom_str(sexpr: &pcb_sexpr::Sexpr) -> Option<&str> {
    match &sexpr.kind {
        SexprKind::Symbol(s) | SexprKind::String(s) => Some(s.as_str()),
        _ => None,
    }
}

fn find_all_child_properties(items: &[pcb_sexpr::Sexpr]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for item in items {
        let Some(list) = item.as_list() else {
            continue;
        };
        if list.first().and_then(|n| n.as_sym()) != Some("property") {
            continue;
        }
        let Some(key) = list.get(1).and_then(as_atom_str) else {
            continue;
        };
        let Some(value) = list.get(2).and_then(as_atom_str) else {
            continue;
        };
        out.push((key.to_string(), value.to_string()));
    }
    out
}
