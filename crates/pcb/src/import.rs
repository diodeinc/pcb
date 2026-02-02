use anyhow::{Context, Result};
use clap::Args;
use pcb_zen_core::Diagnostics;
use pcb_zen_core::{config::find_workspace_root, config::PcbToml, DefaultFileProvider};
use serde::Serialize;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::codegen;
use crate::drc;
use crate::new;
use crate::tty;
use log::debug;
use pcb_zen_core::lang::stackup as zen_stackup;

#[derive(Args, Debug, Clone)]
#[command(about = "Import KiCad projects into a Zener workspace")]
pub struct ImportArgs {
    /// Path to a Zener workspace (defaults to current directory)
    #[arg(value_name = "WORKSPACE_PATH", value_hint = clap::ValueHint::AnyPath)]
    pub workspace: Option<PathBuf>,

    /// Path to a KiCad project directory (or a .kicad_pro file)
    #[arg(long = "kicad-project", value_name = "PATH", value_hint = clap::ValueHint::AnyPath)]
    pub kicad_project: PathBuf,
}

#[derive(Debug, Serialize)]
struct ImportDiscovery {
    workspace_root: PathBuf,
    kicad_project_root: PathBuf,
    board_name: Option<String>,
    board_name_source: Option<BoardNameSource>,
    files: KicadDiscoveredFiles,
    validation: Option<ImportValidation>,
    generated: Option<GeneratedArtifacts>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum BoardNameSource {
    KicadProArgument,
    SingleKicadProFound,
}

#[derive(Debug, Default, Serialize)]
struct KicadDiscoveredFiles {
    /// Paths are relative to `kicad_project_root`
    kicad_pro: Vec<PathBuf>,
    kicad_sch: Vec<PathBuf>,
    kicad_pcb: Vec<PathBuf>,
    kicad_sym: Vec<PathBuf>,
    kicad_mod: Vec<PathBuf>,
    kicad_prl: Vec<PathBuf>,
    kicad_dru: Vec<PathBuf>,
    fp_lib_table: Vec<PathBuf>,
    sym_lib_table: Vec<PathBuf>,
}

#[derive(Debug, Serialize)]
struct ImportValidation {
    selected: SelectedKicadFiles,
    schematic_parity_ok: bool,
    schematic_parity_violations: usize,
    erc_errors: usize,
    erc_warnings: usize,
    drc_errors: usize,
    drc_warnings: usize,
}

#[derive(Debug, Serialize)]
struct GeneratedArtifacts {
    board_dir: PathBuf,
    board_zen: PathBuf,
    validation_diagnostics_json: PathBuf,
    layout_dir: PathBuf,
    layout_kicad_pro: PathBuf,
    layout_kicad_pcb: PathBuf,
}

struct ImportValidationRun {
    summary: ImportValidation,
    diagnostics: Diagnostics,
}

#[derive(Debug, Serialize)]
struct SelectedKicadFiles {
    /// Relative to `kicad_project_root`
    kicad_pro: PathBuf,
    /// Relative to `kicad_project_root`
    kicad_sch: PathBuf,
    /// Relative to `kicad_project_root`
    kicad_pcb: PathBuf,
}

pub fn execute(args: ImportArgs) -> Result<()> {
    let workspace_start = match args.workspace {
        Some(p) => p,
        None => env::current_dir()?,
    };
    if !workspace_start.exists() {
        anyhow::bail!(
            "Workspace path does not exist: {}",
            workspace_start.display()
        );
    }
    let workspace_start = fs::canonicalize(&workspace_start).unwrap_or(workspace_start);
    let workspace_root = require_existing_workspace(&workspace_start)?;

    let (kicad_project_root, passed_kicad_pro) = normalize_kicad_project_path(&args.kicad_project)?;
    let mut files = discover_kicad_files(&kicad_project_root)?;

    // If the user provided a .kicad_pro file, ensure it is included in discovery even if the
    // project root contains other nested projects.
    if let Some(kicad_pro_path) = passed_kicad_pro {
        let canonical_root =
            fs::canonicalize(&kicad_project_root).unwrap_or(kicad_project_root.clone());
        let canonical_pro = fs::canonicalize(&kicad_pro_path).unwrap_or(kicad_pro_path);
        if let Ok(rel) = canonical_pro.strip_prefix(&canonical_root) {
            let rel = rel.to_path_buf();
            if !files.kicad_pro.contains(&rel) {
                files.kicad_pro.push(rel);
            }
        }
    }

    sort_discovered_files(&mut files);

    // For directory projects, require a single top-level .kicad_pro to avoid ambiguous source-of-truth.
    if args.kicad_project.is_dir() && files.kicad_pro.len() != 1 {
        anyhow::bail!(
            "Expected exactly one .kicad_pro under {}, found {}.\nFound:\n  - {}",
            kicad_project_root.display(),
            files.kicad_pro.len(),
            files
                .kicad_pro
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join("\n  - ")
        );
    }

    let (board_name, board_name_source) = infer_board_name(&args.kicad_project, &files);

    let Some(board_name_str) = board_name.as_deref() else {
        anyhow::bail!("Failed to determine KiCad project name (expected a .kicad_pro file)");
    };

    let validation_run = validate_kicad_project(
        &kicad_project_root,
        &args.kicad_project,
        &files,
        Some(board_name_str),
    )?;

    // Persist a copy of the raw diagnostics (before render filters mutate suppression state).
    let diagnostics_for_file = Diagnostics {
        diagnostics: validation_run.diagnostics.diagnostics.clone(),
    };

    // Render diagnostics for the user (this is intentionally noisy and useful).
    let mut diagnostics_for_render = validation_run.diagnostics;
    drc::render_diagnostics(&mut diagnostics_for_render, &[]);

    if !validation_run.summary.schematic_parity_ok {
        anyhow::bail!(
            "KiCad schematic/layout parity check failed: schematic and PCB appear out of sync"
        );
    }

    let error_count = diagnostics_for_render.error_count();
    if error_count > 0 {
        if !tty::is_interactive() || std::env::var("CI").is_ok() {
            anyhow::bail!(
                "KiCad ERC/DRC reported {error_count} errors. Fix them, or re-run in an interactive terminal to confirm continuing."
            );
        }

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

    let board_scaffold = new::scaffold_board(&workspace_root, board_name_str)?;
    let diagnostics_path = write_validation_diagnostics(
        &board_scaffold.board_dir,
        &kicad_project_root,
        &validation_run.summary,
        &diagnostics_for_file,
    )?;

    let (layout_dir, layout_kicad_pro, layout_kicad_pcb) = copy_layout_sources(
        &kicad_project_root,
        &validation_run.summary.selected,
        &board_scaffold.board_dir,
        board_name_str,
    )?;

    apply_stackup_to_board_zen(&board_scaffold.zen_file, board_name_str, &layout_kicad_pcb)?;

    let generated = GeneratedArtifacts {
        board_dir: board_scaffold
            .board_dir
            .strip_prefix(&workspace_root)
            .unwrap_or(&board_scaffold.board_dir)
            .to_path_buf(),
        board_zen: board_scaffold
            .zen_file
            .strip_prefix(&workspace_root)
            .unwrap_or(&board_scaffold.zen_file)
            .to_path_buf(),
        validation_diagnostics_json: diagnostics_path
            .strip_prefix(&workspace_root)
            .unwrap_or(&diagnostics_path)
            .to_path_buf(),
        layout_dir: layout_dir
            .strip_prefix(&workspace_root)
            .unwrap_or(&layout_dir)
            .to_path_buf(),
        layout_kicad_pro: layout_kicad_pro
            .strip_prefix(&workspace_root)
            .unwrap_or(&layout_kicad_pro)
            .to_path_buf(),
        layout_kicad_pcb: layout_kicad_pcb
            .strip_prefix(&workspace_root)
            .unwrap_or(&layout_kicad_pcb)
            .to_path_buf(),
    };

    let output = ImportDiscovery {
        workspace_root,
        kicad_project_root,
        board_name,
        board_name_source,
        files,
        validation: Some(validation_run.summary),
        generated: Some(generated),
    };

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn normalize_kicad_project_path(path: &Path) -> Result<(PathBuf, Option<PathBuf>)> {
    let meta = fs::metadata(path)
        .with_context(|| format!("Failed to stat KiCad project path: {}", path.display()))?;

    if meta.is_dir() {
        return Ok((path.to_path_buf(), None));
    }

    if meta.is_file() && path.extension() == Some(OsStr::new("kicad_pro")) {
        let parent = path
            .parent()
            .context("A .kicad_pro path must have a parent directory")?;
        return Ok((parent.to_path_buf(), Some(path.to_path_buf())));
    }

    anyhow::bail!(
        "Expected --kicad-project to be a directory or a .kicad_pro file, got: {}",
        path.display()
    );
}

fn discover_kicad_files(root: &Path) -> Result<KicadDiscoveredFiles> {
    let canonical_root = fs::canonicalize(root).with_context(|| {
        format!(
            "Failed to canonicalize KiCad project root: {}",
            root.display()
        )
    })?;

    let mut out = KicadDiscoveredFiles::default();

    for entry in WalkDir::new(&canonical_root)
        .follow_links(false)
        .into_iter()
    {
        let entry = entry.with_context(|| {
            format!(
                "Failed while traversing KiCad project directory: {}",
                canonical_root.display()
            )
        })?;

        if !entry.file_type().is_file() {
            continue;
        }

        let abs_path = entry.path();
        let rel = abs_path
            .strip_prefix(&canonical_root)
            .unwrap_or(abs_path)
            .to_path_buf();

        let Some(kind) = classify_kicad_file(&rel) else {
            continue;
        };

        match kind {
            KicadFileKind::KicadPro => out.kicad_pro.push(rel),
            KicadFileKind::KicadSch => out.kicad_sch.push(rel),
            KicadFileKind::KicadPcb => out.kicad_pcb.push(rel),
            KicadFileKind::KicadSym => out.kicad_sym.push(rel),
            KicadFileKind::KicadMod => out.kicad_mod.push(rel),
            KicadFileKind::KicadPrl => out.kicad_prl.push(rel),
            KicadFileKind::KicadDru => out.kicad_dru.push(rel),
            KicadFileKind::FpLibTable => out.fp_lib_table.push(rel),
            KicadFileKind::SymLibTable => out.sym_lib_table.push(rel),
        }
    }

    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KicadFileKind {
    KicadPro,
    KicadSch,
    KicadPcb,
    KicadSym,
    KicadMod,
    KicadPrl,
    KicadDru,
    FpLibTable,
    SymLibTable,
}

fn classify_kicad_file(rel_path: &Path) -> Option<KicadFileKind> {
    let file_name = rel_path.file_name().and_then(|s| s.to_str()).unwrap_or("");

    if file_name == "fp-lib-table" {
        return Some(KicadFileKind::FpLibTable);
    }
    if file_name == "sym-lib-table" {
        return Some(KicadFileKind::SymLibTable);
    }

    match rel_path.extension().and_then(|s| s.to_str()) {
        Some("kicad_pro") => Some(KicadFileKind::KicadPro),
        Some("kicad_sch") => Some(KicadFileKind::KicadSch),
        Some("kicad_pcb") => Some(KicadFileKind::KicadPcb),
        Some("kicad_sym") => Some(KicadFileKind::KicadSym),
        Some("kicad_mod") => Some(KicadFileKind::KicadMod),
        Some("kicad_prl") => Some(KicadFileKind::KicadPrl),
        Some("kicad_dru") => Some(KicadFileKind::KicadDru),
        _ => None,
    }
}

fn sort_discovered_files(files: &mut KicadDiscoveredFiles) {
    files.kicad_pro.sort();
    files.kicad_sch.sort();
    files.kicad_pcb.sort();
    files.kicad_sym.sort();
    files.kicad_mod.sort();
    files.kicad_prl.sort();
    files.kicad_dru.sort();
    files.fp_lib_table.sort();
    files.sym_lib_table.sort();
}

fn infer_board_name(
    kicad_project_arg: &Path,
    files: &KicadDiscoveredFiles,
) -> (Option<String>, Option<BoardNameSource>) {
    // If the user gave a specific project file, use it directly.
    if kicad_project_arg.is_file() && kicad_project_arg.extension() == Some(OsStr::new("kicad_pro"))
    {
        if let Some(stem) = kicad_project_arg.file_stem().and_then(|s| s.to_str()) {
            return (
                Some(stem.to_string()),
                Some(BoardNameSource::KicadProArgument),
            );
        }
    }

    // For directory projects, we require exactly one .kicad_pro, so this is definitive.
    if files.kicad_pro.len() == 1 {
        if let Some(stem) = files.kicad_pro[0].file_stem().and_then(|s| s.to_str()) {
            return (
                Some(stem.to_string()),
                Some(BoardNameSource::SingleKicadProFound),
            );
        }
    }

    (None, None)
}

fn validate_kicad_project(
    kicad_project_root: &Path,
    kicad_project_arg: &Path,
    files: &KicadDiscoveredFiles,
    preferred_stem: Option<&str>,
) -> Result<ImportValidationRun> {
    let selected =
        select_kicad_files(kicad_project_root, kicad_project_arg, files, preferred_stem)?;

    let kicad_pro_abs = kicad_project_root.join(&selected.kicad_pro);
    let kicad_sch_abs = kicad_project_root.join(&selected.kicad_sch);
    let kicad_pcb_abs = kicad_project_root.join(&selected.kicad_pcb);

    if !kicad_pro_abs.exists() {
        anyhow::bail!(
            "Selected KiCad project file does not exist: {}",
            kicad_pro_abs.display()
        );
    }

    let mut diagnostics = Diagnostics::default();

    // ERC (schematic)
    let erc_report = pcb_kicad::run_erc_report(&kicad_sch_abs, Some(kicad_project_root))
        .context("KiCad ERC failed")?;
    erc_report.add_to_diagnostics(&mut diagnostics, &kicad_sch_abs.to_string_lossy());

    let (erc_errors, erc_warnings) = count_erc(&erc_report);

    // DRC + schematic parity (layout)
    let drc_report = pcb_kicad::run_drc_report(&kicad_pcb_abs, true, Some(kicad_project_root))
        .context("KiCad DRC failed")?;
    drc_report.add_to_diagnostics(&mut diagnostics, &kicad_pcb_abs.to_string_lossy());
    drc_report
        .add_unconnected_items_to_diagnostics(&mut diagnostics, &kicad_pcb_abs.to_string_lossy());
    drc_report
        .add_schematic_parity_to_diagnostics(&mut diagnostics, &kicad_pcb_abs.to_string_lossy());

    let (drc_errors, drc_warnings) = drc_report.violation_counts();
    let schematic_parity_violations = drc_report.schematic_parity.len();
    let schematic_parity_ok = schematic_parity_violations == 0;

    Ok(ImportValidationRun {
        summary: ImportValidation {
            selected,
            schematic_parity_ok,
            schematic_parity_violations,
            erc_errors,
            erc_warnings,
            drc_errors,
            drc_warnings,
        },
        diagnostics,
    })
}

fn select_kicad_files(
    kicad_project_root: &Path,
    kicad_project_arg: &Path,
    files: &KicadDiscoveredFiles,
    preferred_stem: Option<&str>,
) -> Result<SelectedKicadFiles> {
    if files.kicad_pro.is_empty() {
        anyhow::bail!(
            "No .kicad_pro files found under {}",
            kicad_project_root.display()
        );
    }

    let kicad_pro = select_kicad_pro(kicad_project_root, kicad_project_arg, &files.kicad_pro)?;
    let kicad_sch = select_by_stem("kicad_sch", &files.kicad_sch, preferred_stem)?;
    let kicad_pcb = select_by_stem("kicad_pcb", &files.kicad_pcb, preferred_stem)?;

    Ok(SelectedKicadFiles {
        kicad_pro,
        kicad_sch,
        kicad_pcb,
    })
}

fn select_kicad_pro(
    kicad_project_root: &Path,
    kicad_project_arg: &Path,
    discovered: &[PathBuf],
) -> Result<PathBuf> {
    if kicad_project_arg.is_file() && kicad_project_arg.extension() == Some(OsStr::new("kicad_pro"))
    {
        let canonical_root =
            fs::canonicalize(kicad_project_root).unwrap_or(kicad_project_root.to_path_buf());
        let canonical_pro =
            fs::canonicalize(kicad_project_arg).unwrap_or(kicad_project_arg.to_path_buf());
        let rel = canonical_pro
            .strip_prefix(&canonical_root)
            .with_context(|| {
                format!(
                    "Provided .kicad_pro is not under the project root {}: {}",
                    canonical_root.display(),
                    canonical_pro.display()
                )
            })?;
        return Ok(rel.to_path_buf());
    }

    if discovered.len() == 1 {
        return Ok(discovered[0].clone());
    }

    anyhow::bail!(
        "Multiple .kicad_pro files found; pass a specific .kicad_pro via --kicad-project.\nFound:\n  - {}",
        discovered
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("\n  - ")
    );
}

fn select_by_stem(
    kind: &str,
    discovered: &[PathBuf],
    preferred_stem: Option<&str>,
) -> Result<PathBuf> {
    if discovered.is_empty() {
        anyhow::bail!("No .{kind} files found in KiCad project");
    }

    if let Some(stem) = preferred_stem {
        let matches: Vec<_> = discovered
            .iter()
            .filter(|p| p.file_stem().and_then(|s| s.to_str()) == Some(stem))
            .cloned()
            .collect();
        if matches.len() == 1 {
            return Ok(matches[0].clone());
        }
    }

    if discovered.len() == 1 {
        return Ok(discovered[0].clone());
    }

    anyhow::bail!(
        "Multiple .{kind} files found; unable to select one unambiguously.\nFound:\n  - {}",
        discovered
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("\n  - ")
    );
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

fn require_existing_workspace(start_path: &Path) -> Result<PathBuf> {
    let file_provider = DefaultFileProvider::new();
    let workspace_root = find_workspace_root(&file_provider, start_path)
        .with_context(|| format!("Not inside a pcb workspace: {}", start_path.display()))?;

    let pcb_toml = workspace_root.join("pcb.toml");
    if !pcb_toml.exists() {
        anyhow::bail!(
            "Workspace is missing pcb.toml: {}\nCreate one with `pcb new --workspace <NAME> --repo <URL>`.",
            pcb_toml.display()
        );
    }

    let config = PcbToml::from_file(&file_provider, &pcb_toml)
        .with_context(|| format!("Failed to parse {}", pcb_toml.display()))?;
    if !config.is_workspace() {
        anyhow::bail!("pcb.toml is not a workspace config: {}", pcb_toml.display());
    }

    Ok(workspace_root)
}

fn write_validation_diagnostics(
    board_dir: &Path,
    kicad_project_root: &Path,
    validation: &ImportValidation,
    diagnostics: &Diagnostics,
) -> Result<PathBuf> {
    #[derive(Serialize)]
    struct ImportValidationDiagnosticsFile<'a> {
        kicad_project_root: &'a Path,
        selected: &'a SelectedKicadFiles,
        diagnostics: &'a Diagnostics,
    }

    let out_path = board_dir.join(".kicad.validation.diagnostics.json");
    let payload = ImportValidationDiagnosticsFile {
        kicad_project_root,
        selected: &validation.selected,
        diagnostics,
    };

    fs::write(&out_path, serde_json::to_string_pretty(&payload)?)
        .with_context(|| format!("Failed to write {}", out_path.display()))?;
    Ok(out_path)
}

fn copy_layout_sources(
    kicad_project_root: &Path,
    selected: &SelectedKicadFiles,
    board_dir: &Path,
    board_name: &str,
) -> Result<(PathBuf, PathBuf, PathBuf)> {
    // This matches the default `pcb new --board` template: `layout_path = "layout/<board_name>"`.
    let layout_dir = board_dir.join("layout").join(board_name);
    fs::create_dir_all(&layout_dir)
        .with_context(|| format!("Failed to create layout directory {}", layout_dir.display()))?;

    let src_pro = kicad_project_root.join(&selected.kicad_pro);
    let src_pcb = kicad_project_root.join(&selected.kicad_pcb);

    let dst_pro = layout_dir.join("layout.kicad_pro");
    let dst_pcb = layout_dir.join("layout.kicad_pcb");

    if dst_pro.exists() || dst_pcb.exists() {
        anyhow::bail!(
            "Layout directory already contains KiCad files (refusing to overwrite): {}",
            layout_dir.display()
        );
    }

    fs::copy(&src_pro, &dst_pro).with_context(|| {
        format!(
            "Failed to copy {} -> {}",
            src_pro.display(),
            dst_pro.display()
        )
    })?;
    fs::copy(&src_pcb, &dst_pcb).with_context(|| {
        format!(
            "Failed to copy {} -> {}",
            src_pcb.display(),
            dst_pcb.display()
        )
    })?;

    Ok((layout_dir, dst_pro, dst_pcb))
}

fn apply_stackup_to_board_zen(
    board_zen: &Path,
    board_name: &str,
    layout_kicad_pcb: &Path,
) -> Result<()> {
    let pcb_text = match fs::read_to_string(layout_kicad_pcb) {
        Ok(s) => s,
        Err(e) => {
            debug!(
                "Skipping stackup extraction (failed to read {}): {}",
                layout_kicad_pcb.display(),
                e
            );
            return Ok(());
        }
    };

    let stackup = match zen_stackup::Stackup::from_kicad_pcb(&pcb_text) {
        Ok(Some(s)) => s,
        Ok(None) => {
            debug!(
                "No KiCad stackup section found in {}; leaving default board config",
                layout_kicad_pcb.display()
            );
            return Ok(());
        }
        Err(e) => {
            debug!(
                "Skipping stackup extraction (failed to parse stackup from {}): {}",
                layout_kicad_pcb.display(),
                e
            );
            return Ok(());
        }
    };

    let Some(layers) = stackup.layers.as_deref() else {
        debug!(
            "Skipping stackup extraction (stackup had no layers) for {}",
            layout_kicad_pcb.display()
        );
        return Ok(());
    };
    if layers.is_empty() {
        debug!(
            "Skipping stackup extraction (stackup had 0 layers) for {}",
            layout_kicad_pcb.display()
        );
        return Ok(());
    }

    let copper_layers = stackup.copper_layer_count();
    if !matches!(copper_layers, 2 | 4 | 6 | 8 | 10) {
        debug!(
            "Skipping stackup extraction (unsupported copper layer count: {}) for {}",
            copper_layers,
            layout_kicad_pcb.display()
        );
        return Ok(());
    }

    let board_zen_content =
        codegen::board::render_board_with_stackup(board_name, copper_layers, &stackup);
    codegen::zen::write_zen_formatted(board_zen, &board_zen_content)
        .with_context(|| format!("Failed to write {}", board_zen.display()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_kicad_files_and_infers_name() -> Result<()> {
        // Use an existing KiCad project fixture already in the repo.
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../pcb-sch/test/kicad-bom");

        let files = discover_kicad_files(&root)?;
        assert!(!files.kicad_pro.is_empty());
        assert!(!files.kicad_pcb.is_empty());
        assert!(!files.kicad_sch.is_empty());

        let (name, src) = infer_board_name(&root, &files);
        assert_eq!(name.as_deref(), Some("layout"));
        assert!(matches!(src, Some(BoardNameSource::SingleKicadProFound)));

        Ok(())
    }
}
