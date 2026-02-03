use anyhow::{Context, Result};
use clap::Args;
use pcb_zen_core::Diagnostics;
use pcb_zen_core::{config::find_workspace_root, config::PcbToml, DefaultFileProvider};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;
use walkdir::WalkDir;

use crate::codegen;
use crate::drc;
use crate::new;
use crate::tty;
use log::debug;
use pcb_sexpr::Sexpr;
use pcb_sexpr::{board as sexpr_board, kicad as sexpr_kicad};
use pcb_zen_core::lang::stackup as zen_stackup;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
struct KiCadRefDes(String);

impl KiCadRefDes {
    fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for KiCadRefDes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<String> for KiCadRefDes {
    fn from(value: String) -> Self {
        Self(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
struct KiCadNetName(String);

impl KiCadNetName {
    fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for KiCadNetName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<String> for KiCadNetName {
    fn from(value: String) -> Self {
        Self(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
struct KiCadPinNumber(String);

impl KiCadPinNumber {
    fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for KiCadPinNumber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<String> for KiCadPinNumber {
    fn from(value: String) -> Self {
        Self(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
struct KiCadLibId(String);

impl KiCadLibId {
    fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for KiCadLibId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<String> for KiCadLibId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

#[derive(Args, Debug, Clone)]
#[command(about = "Import KiCad projects into a Zener workspace")]
pub struct ImportArgs {
    /// Path to a Zener workspace (defaults to current directory)
    #[arg(value_name = "WORKSPACE_PATH", value_hint = clap::ValueHint::AnyPath)]
    pub workspace: Option<PathBuf>,

    /// Path to a KiCad project directory (or a .kicad_pro file)
    #[arg(long = "kicad-project", value_name = "PATH", value_hint = clap::ValueHint::AnyPath)]
    pub kicad_project: PathBuf,

    /// Skip interactive confirmations (continue even if ERC/DRC errors are present)
    #[arg(long = "force")]
    pub force: bool,
}

#[derive(Debug, Serialize)]
struct ImportDiscovery {
    workspace_root: PathBuf,
    kicad_project_root: PathBuf,
    board_name: Option<String>,
    board_name_source: Option<BoardNameSource>,
    files: KicadDiscoveredFiles,
    extraction: Option<ImportExtraction>,
    validation: Option<ImportValidation>,
    generated: Option<GeneratedArtifacts>,
}

#[derive(Debug, Serialize)]
struct ImportExtraction {
    /// Netlist is the primary source-of-truth for component identities during import.
    ///
    /// Keys serialize to the derived KiCad PCB footprint `(path "...")` strings.
    netlist_components: BTreeMap<KiCadUuidPathKey, ImportComponentData>,
    /// Netlist-derived connectivity for each KiCad net.
    ///
    /// Keys are KiCad net names.
    netlist_nets: BTreeMap<KiCadNetName, ImportNetData>,

    /// Embedded library symbol definitions found in `.kicad_sch` files.
    ///
    /// Keys are KiCad `lib_id` strings (e.g. `myLib:MySymbol`).
    schematic_lib_symbols: BTreeMap<KiCadLibId, String>,
}

/// Key that can join KiCad schematic/netlist/PCB data for a single component instance.
///
/// This corresponds to:
/// - netlist: `(sheetpath (tstamps "..."))` + `(tstamps "...")`
/// - pcb: footprint `(path "/<sheet_uuid_chain>/<symbol_uuid>")`
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
struct KiCadUuidPathKey {
    /// Normalized to start and end with `/`. Root sheet is `/`.
    sheetpath_tstamps: String,
    /// UUID string for the component/symbol instance.
    symbol_uuid: String,
}

impl KiCadUuidPathKey {
    fn pcb_path(&self) -> String {
        let sheetpath = normalize_sheetpath_tstamps(&self.sheetpath_tstamps);
        if sheetpath == "/" {
            format!("/{}", self.symbol_uuid)
        } else {
            format!("{sheetpath}{}", self.symbol_uuid)
        }
    }

    fn from_pcb_path(pcb_path: &str) -> Result<Self> {
        let trimmed = pcb_path.trim();
        if !trimmed.starts_with('/') {
            anyhow::bail!("Expected KiCad PCB footprint path to start with '/': {pcb_path:?}");
        }
        let trimmed = trimmed.trim_end_matches('/');
        let mut parts: Vec<&str> = trimmed.split('/').filter(|s| !s.is_empty()).collect();
        let Some(symbol_uuid) = parts.pop() else {
            anyhow::bail!("KiCad PCB footprint path has no UUID segment: {pcb_path:?}");
        };
        let sheetpath_tstamps = if parts.is_empty() {
            "/".to_string()
        } else {
            format!("/{}/", parts.join("/"))
        };
        Ok(Self {
            sheetpath_tstamps,
            symbol_uuid: symbol_uuid.to_string(),
        })
    }
}

impl std::fmt::Display for KiCadUuidPathKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.pcb_path())
    }
}

impl From<KiCadUuidPathKey> for String {
    fn from(value: KiCadUuidPathKey) -> Self {
        value.pcb_path()
    }
}

impl TryFrom<String> for KiCadUuidPathKey {
    type Error = anyhow::Error;

    fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
        KiCadUuidPathKey::from_pcb_path(&value)
    }
}

#[derive(Debug, Clone, Serialize)]
struct ImportComponentData {
    netlist: ImportNetlistComponent,
    schematic: Option<ImportSchematicComponent>,
    layout: Option<ImportLayoutComponent>,
}

#[derive(Debug, Clone, Serialize)]
struct ImportNetlistComponent {
    /// Refdes from the netlist export (human-facing; not used as primary identity).
    refdes: KiCadRefDes,
    value: Option<String>,
    footprint: Option<String>,
    sheetpath_names: Option<String>,
    /// KiCad PCB footprint `(path "...")` strings for every unit in a multi-unit symbol.
    ///
    /// For single-unit symbols, this has length 1.
    unit_pcb_paths: Vec<KiCadUuidPathKey>,
}

#[derive(Debug, Clone, Serialize)]
struct ImportSchematicComponent {
    /// Schematic symbol instances keyed by derived KiCad PCB footprint `(path "...")` strings.
    ///
    /// For single-unit symbols this has a single entry. Multi-unit symbols have one entry per unit.
    units: BTreeMap<KiCadUuidPathKey, ImportSchematicUnit>,
}

#[derive(Debug, Clone, Serialize)]
struct ImportSchematicUnit {
    lib_id: Option<KiCadLibId>,
    unit: Option<i64>,
    at: Option<ImportSchematicAt>,
    mirror: Option<String>,
    in_bom: Option<bool>,
    on_board: Option<bool>,
    dnp: Option<bool>,
    exclude_from_sim: Option<bool>,
    /// Raw `(instances ... (project ... (path "...")))` path string for debugging.
    instance_path: Option<String>,
    /// All `(property "...")` name/value pairs on the symbol instance.
    properties: BTreeMap<String, String>,
    /// Optional pin UUIDs keyed by pin number.
    pins: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Clone, Serialize)]
struct ImportSchematicAt {
    x: f64,
    y: f64,
    rot: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
struct ImportLayoutComponent {}

#[derive(Debug, Clone, Serialize)]
struct ImportNetData {
    /// The set of ports (component pin) connected to this net.
    ///
    /// The `component` field is the derived KiCad PCB footprint `(path "...")` string for the
    /// instance, allowing future joins against the PCB layout.
    ports: BTreeSet<ImportNetPort>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord)]
struct ImportNetPort {
    component: KiCadUuidPathKey,
    pin: KiCadPinNumber,
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
    schematic_parity_tolerated: usize,
    schematic_parity_blocking: usize,
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

    let pcb_refdes_to_anchor_key = extract_kicad_pcb_refdes_to_anchor_key(
        &kicad_project_root,
        &validation_run.summary.selected,
    )?;

    let mut netlist = extract_kicad_netlist(
        &kicad_project_root,
        &validation_run.summary.selected,
        &pcb_refdes_to_anchor_key,
    )?;

    let schematic_lib_symbols = extract_kicad_schematic_data(
        &kicad_project_root,
        &files.kicad_sch,
        &netlist.unit_to_anchor,
        &mut netlist.components,
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
        if args.force {
            eprintln!(
                "Warning: KiCad ERC/DRC reported {error_count} errors; continuing due to --force."
            );
        } else if !tty::is_interactive() || std::env::var("CI").is_ok() {
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

    let board_dir = workspace_root.join("boards").join(board_name_str);
    if board_dir.exists() {
        debug!(
            "Removing existing board directory for clean import: {}",
            board_dir.display()
        );
        fs::remove_dir_all(&board_dir)
            .with_context(|| format!("Failed to remove {}", board_dir.display()))?;
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
        extraction: Some(ImportExtraction {
            netlist_components: netlist.components,
            netlist_nets: netlist.nets,
            schematic_lib_symbols,
        }),
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
    let (schematic_parity_tolerated, schematic_parity_blocking) =
        classify_schematic_parity(&drc_report.schematic_parity);
    let schematic_parity_ok = schematic_parity_blocking == 0;

    Ok(ImportValidationRun {
        summary: ImportValidation {
            selected,
            schematic_parity_ok,
            schematic_parity_violations,
            schematic_parity_tolerated,
            schematic_parity_blocking,
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

fn classify_schematic_parity(parity: &[pcb_kicad::drc::DrcViolation]) -> (usize, usize) {
    // Import uses KiCad's parity check as a guardrail against having a split
    // schematic/layout source of truth. Some parity issues are tolerable for
    // import, notably extra footprints that are not represented in the schematic.
    //
    // We can exclude these from the Zener world later by only generating
    // components that exist in the schematic/netlist.
    let tolerated = parity
        .iter()
        .filter(|v| v.violation_type == "extra_footprint")
        .count();
    let blocking = parity.len().saturating_sub(tolerated);
    (tolerated, blocking)
}

#[derive(Debug)]
struct KiCadNetlistExtraction {
    components: BTreeMap<KiCadUuidPathKey, ImportComponentData>,
    nets: BTreeMap<KiCadNetName, ImportNetData>,
    unit_to_anchor: BTreeMap<KiCadUuidPathKey, KiCadUuidPathKey>,
}

#[derive(Debug)]
struct KiCadNetlistComponentsExtraction {
    components: BTreeMap<KiCadUuidPathKey, ImportComponentData>,
    refdes_to_anchor: BTreeMap<KiCadRefDes, KiCadUuidPathKey>,
    unit_to_anchor: BTreeMap<KiCadUuidPathKey, KiCadUuidPathKey>,
}

fn extract_kicad_pcb_refdes_to_anchor_key(
    kicad_project_root: &Path,
    selected: &SelectedKicadFiles,
) -> Result<BTreeMap<KiCadRefDes, KiCadUuidPathKey>> {
    let pcb_abs = kicad_project_root.join(&selected.kicad_pcb);
    if !pcb_abs.exists() {
        anyhow::bail!("PCB file not found: {}", pcb_abs.display());
    }

    let text = fs::read_to_string(&pcb_abs)
        .with_context(|| format!("Failed to read {}", pcb_abs.display()))?;
    parse_kicad_pcb_refdes_to_anchor_key(&text).with_context(|| {
        format!(
            "Failed to parse KiCad PCB file for refdes/path anchors: {}",
            pcb_abs.display()
        )
    })
}

fn parse_kicad_pcb_refdes_to_anchor_key(
    pcb_text: &str,
) -> Result<BTreeMap<KiCadRefDes, KiCadUuidPathKey>> {
    let root = pcb_sexpr::parse(pcb_text).context("Failed to parse KiCad PCB as S-expression")?;

    let raw = sexpr_board::extract_footprint_refdes_to_kiid_path(&root)
        .map_err(|e| anyhow::anyhow!(e))?;

    let mut out: BTreeMap<KiCadRefDes, KiCadUuidPathKey> = BTreeMap::new();
    for (refdes, path) in raw {
        let refdes = KiCadRefDes::from(refdes);
        let key = KiCadUuidPathKey::from_pcb_path(&path)?;
        if out.insert(refdes.clone(), key).is_some() {
            anyhow::bail!(
                "KiCad PCB contains multiple footprints with refdes {}",
                refdes.as_str()
            );
        }
    }
    Ok(out)
}

fn extract_kicad_schematic_data(
    kicad_project_root: &Path,
    kicad_sch_files: &[PathBuf],
    unit_to_anchor: &BTreeMap<KiCadUuidPathKey, KiCadUuidPathKey>,
    netlist_components: &mut BTreeMap<KiCadUuidPathKey, ImportComponentData>,
) -> Result<BTreeMap<KiCadLibId, String>> {
    let mut lib_symbols: BTreeMap<KiCadLibId, String> = BTreeMap::new();

    for rel in kicad_sch_files {
        let abs = kicad_project_root.join(rel);
        let text = fs::read_to_string(&abs)
            .with_context(|| format!("Failed to read {}", abs.display()))?;

        let root = pcb_sexpr::parse(&text).with_context(|| {
            format!(
                "Failed to parse KiCad schematic as S-expression: {}",
                abs.display()
            )
        })?;

        // Extract embedded library symbol definitions.
        if let Some(lib) = root.find_list("lib_symbols") {
            for node in lib.iter().skip(1) {
                let Some(items) = node.as_list() else {
                    continue;
                };
                if items.first().and_then(Sexpr::as_sym) != Some("symbol") {
                    continue;
                }
                let Some(lib_id) = items.get(1).and_then(Sexpr::as_str) else {
                    continue;
                };
                let lib_id = KiCadLibId::from(lib_id.to_string());

                let rendered = node.to_string();
                match lib_symbols.get(&lib_id) {
                    None => {
                        lib_symbols.insert(lib_id, rendered);
                    }
                    Some(existing) if existing == &rendered => {}
                    Some(_) => {
                        debug!(
                            "Conflicting embedded lib_symbols entry for {}; keeping first",
                            lib_id.as_str()
                        );
                    }
                }
            }
        }

        // Extract placed symbol instances (direct children of the schematic root).
        for sym in root.find_all_lists("symbol") {
            let Some(symbol_uuid) = sexpr_kicad::string_prop(sym, "uuid") else {
                continue;
            };

            let instance_path = sexpr_kicad::schematic_instance_path(sym);
            let Some(instance_path) = instance_path else {
                continue;
            };

            let unit_key = key_from_schematic_instance_path(&instance_path, &symbol_uuid)?;

            let Some(anchor_key) = unit_to_anchor.get(&unit_key) else {
                debug!(
                    "Schematic symbol {} has no matching netlist key (unit {}); skipping",
                    symbol_uuid,
                    unit_key.pcb_path()
                );
                continue;
            };

            let Some(component) = netlist_components.get_mut(anchor_key) else {
                debug!(
                    "Schematic symbol {} maps to missing component anchor {}; skipping",
                    symbol_uuid,
                    anchor_key.pcb_path()
                );
                continue;
            };

            let schematic = component
                .schematic
                .get_or_insert_with(|| ImportSchematicComponent {
                    units: BTreeMap::new(),
                });

            let unit_data = ImportSchematicUnit {
                lib_id: sexpr_kicad::string_prop(sym, "lib_id").map(KiCadLibId::from),
                unit: sexpr_kicad::int_prop(sym, "unit"),
                at: sexpr_kicad::schematic_at(sym).map(|(x, y, rot)| ImportSchematicAt {
                    x,
                    y,
                    rot,
                }),
                mirror: sexpr_kicad::sym_prop(sym, "mirror"),
                in_bom: sexpr_kicad::yes_no_prop(sym, "in_bom"),
                on_board: sexpr_kicad::yes_no_prop(sym, "on_board"),
                dnp: sexpr_kicad::yes_no_prop(sym, "dnp"),
                exclude_from_sim: sexpr_kicad::yes_no_prop(sym, "exclude_from_sim"),
                instance_path: Some(instance_path),
                properties: sexpr_kicad::schematic_properties(sym),
                pins: sexpr_kicad::schematic_pins(sym),
            };

            if schematic.units.insert(unit_key, unit_data).is_some() {
                debug!(
                    "Duplicate schematic unit entry for {}; overwriting",
                    symbol_uuid
                );
            }
        }
    }

    Ok(lib_symbols)
}

fn extract_kicad_netlist(
    kicad_project_root: &Path,
    selected: &SelectedKicadFiles,
    pcb_refdes_to_anchor_key: &BTreeMap<KiCadRefDes, KiCadUuidPathKey>,
) -> Result<KiCadNetlistExtraction> {
    let kicad_sch_abs = kicad_project_root.join(&selected.kicad_sch);
    let netlist_text = export_kicad_sexpr_netlist(&kicad_sch_abs, kicad_project_root)
        .context("Failed to export KiCad netlist")?;
    parse_kicad_sexpr_netlist(&netlist_text, pcb_refdes_to_anchor_key)
        .context("Failed to parse KiCad netlist")
}

fn export_kicad_sexpr_netlist(kicad_sch_abs: &Path, working_dir: &Path) -> Result<String> {
    if !kicad_sch_abs.exists() {
        anyhow::bail!("Schematic file not found: {}", kicad_sch_abs.display());
    }

    let tmp = NamedTempFile::new().context("Failed to create temporary netlist file")?;

    pcb_kicad::KiCadCliBuilder::new()
        .command("sch")
        .subcommand("export")
        .subcommand("netlist")
        .arg("--format")
        .arg("kicadsexpr")
        .arg("--output")
        .arg(tmp.path().to_string_lossy())
        .arg(kicad_sch_abs.to_string_lossy())
        .current_dir(working_dir.to_string_lossy().to_string())
        .run()
        .context("kicad-cli sch export netlist failed")?;

    fs::read_to_string(tmp.path())
        .with_context(|| format!("Failed to read generated netlist {}", tmp.path().display()))
}

fn parse_kicad_sexpr_netlist(
    netlist_text: &str,
    pcb_refdes_to_anchor_key: &BTreeMap<KiCadRefDes, KiCadUuidPathKey>,
) -> Result<KiCadNetlistExtraction> {
    let root =
        pcb_sexpr::parse(netlist_text).context("Failed to parse KiCad netlist as S-expression")?;

    let comps = parse_kicad_sexpr_netlist_components(&root, pcb_refdes_to_anchor_key)?;
    let nets = parse_kicad_sexpr_netlist_nets(&root, &comps.refdes_to_anchor)?;

    Ok(KiCadNetlistExtraction {
        components: comps.components,
        nets,
        unit_to_anchor: comps.unit_to_anchor,
    })
}

fn parse_kicad_sexpr_netlist_components(
    root: &Sexpr,
    pcb_refdes_to_anchor_key: &BTreeMap<KiCadRefDes, KiCadUuidPathKey>,
) -> Result<KiCadNetlistComponentsExtraction> {
    let components = root
        .find_list("components")
        .ok_or_else(|| anyhow::anyhow!("Netlist missing (components ...) section"))?;

    let mut by_key: BTreeMap<KiCadUuidPathKey, ImportComponentData> = BTreeMap::new();
    let mut refdes_to_key: BTreeMap<KiCadRefDes, KiCadUuidPathKey> = BTreeMap::new();
    let mut unit_to_anchor: BTreeMap<KiCadUuidPathKey, KiCadUuidPathKey> = BTreeMap::new();

    for node in components.iter().skip(1) {
        let Some(comp) = node.as_list() else {
            continue;
        };
        if comp.first().and_then(Sexpr::as_sym) != Some("comp") {
            continue;
        }

        let refdes = sexpr_kicad::string_prop(comp, "ref")
            .ok_or_else(|| anyhow::anyhow!("Netlist component missing ref"))?;
        let refdes = KiCadRefDes::from(refdes);

        let symbol_uuids = sexpr_kicad::string_list_prop(comp, "tstamps").ok_or_else(|| {
            anyhow::anyhow!("Netlist component {refdes} missing tstamps (symbol UUID)")
        })?;

        let (sheetpath_names, sheetpath_tstamps) = sexpr_kicad::sheetpath(comp)
            .with_context(|| format!("Netlist component {refdes} missing sheetpath (tstamps)"))?;

        let footprint = sexpr_kicad::string_prop(comp, "footprint");
        let value = sexpr_kicad::string_prop(comp, "value");

        let normalized_sheetpath_tstamps = normalize_sheetpath_tstamps(&sheetpath_tstamps);

        let anchor_key = if let Some(anchor_key) = pcb_refdes_to_anchor_key.get(&refdes) {
            anchor_key.clone()
        } else {
            // Fallback: choose the first tstamps entry deterministically.
            let Some(symbol_uuid) = symbol_uuids.first() else {
                anyhow::bail!("Netlist component {refdes} has empty tstamps list");
            };
            KiCadUuidPathKey {
                sheetpath_tstamps: normalized_sheetpath_tstamps.clone(),
                symbol_uuid: symbol_uuid.clone(),
            }
        };

        let mut unit_keys: Vec<KiCadUuidPathKey> = symbol_uuids
            .iter()
            .map(|symbol_uuid| KiCadUuidPathKey {
                sheetpath_tstamps: normalized_sheetpath_tstamps.clone(),
                symbol_uuid: symbol_uuid.clone(),
            })
            .collect();

        // Ensure the anchor is represented in the unit list (especially when the PCB anchor differs
        // from the first netlist tstamps entry).
        if !unit_keys.contains(&anchor_key) {
            unit_keys.push(anchor_key.clone());
            unit_keys.sort();
            unit_keys.dedup();
        }

        let data = ImportComponentData {
            netlist: ImportNetlistComponent {
                refdes: refdes.clone(),
                value,
                footprint,
                sheetpath_names,
                unit_pcb_paths: unit_keys.clone(),
            },
            schematic: None,
            layout: None,
        };

        if by_key.insert(anchor_key.clone(), data).is_some() {
            anyhow::bail!(
                "Netlist produced a duplicate component key (anchor path): {}",
                anchor_key.pcb_path()
            );
        }
        if refdes_to_key
            .insert(refdes.clone(), anchor_key.clone())
            .is_some()
        {
            anyhow::bail!("Netlist produced a duplicate refdes");
        }

        for unit_key in unit_keys {
            if unit_to_anchor
                .insert(unit_key.clone(), anchor_key.clone())
                .is_some()
            {
                anyhow::bail!(
                    "Netlist produced a duplicate unit key mapping for {}",
                    unit_key.pcb_path()
                );
            }
        }
    }

    Ok(KiCadNetlistComponentsExtraction {
        components: by_key,
        refdes_to_anchor: refdes_to_key,
        unit_to_anchor,
    })
}

fn parse_kicad_sexpr_netlist_nets(
    root: &Sexpr,
    refdes_to_key: &BTreeMap<KiCadRefDes, KiCadUuidPathKey>,
) -> Result<BTreeMap<KiCadNetName, ImportNetData>> {
    let nets = root
        .find_list("nets")
        .ok_or_else(|| anyhow::anyhow!("Netlist missing (nets ...) section"))?;

    let mut out: BTreeMap<KiCadNetName, ImportNetData> = BTreeMap::new();

    for node in nets.iter().skip(1) {
        let Some(net) = node.as_list() else {
            continue;
        };
        if net.first().and_then(Sexpr::as_sym) != Some("net") {
            continue;
        }

        let name = sexpr_kicad::string_prop(net, "name")
            .ok_or_else(|| anyhow::anyhow!("Netlist net missing name"))?;
        let name = KiCadNetName::from(name);

        let mut ports: BTreeSet<ImportNetPort> = BTreeSet::new();

        for child in net.iter().skip(1) {
            let Some(items) = child.as_list() else {
                continue;
            };
            if items.first().and_then(Sexpr::as_sym) != Some("node") {
                continue;
            }

            let node_ref = sexpr_kicad::string_prop(items, "ref")
                .ok_or_else(|| anyhow::anyhow!("Netlist net {name} contains node without ref"))?;
            let node_ref = KiCadRefDes::from(node_ref);

            let pin = sexpr_kicad::string_prop(items, "pin").ok_or_else(|| {
                anyhow::anyhow!("Netlist net {name} contains node without pin (ref {node_ref})")
            })?;
            let pin = KiCadPinNumber::from(pin);

            let Some(key) = refdes_to_key.get(&node_ref) else {
                debug!("Netlist net {name} references unknown refdes {node_ref}; skipping");
                continue;
            };

            ports.insert(ImportNetPort {
                component: key.clone(),
                pin,
            });
        }

        if out.insert(name.clone(), ImportNetData { ports }).is_some() {
            anyhow::bail!("Netlist produced a duplicate net name: {}", name.as_str());
        }
    }

    Ok(out)
}

fn normalize_sheetpath_tstamps(sheetpath: &str) -> String {
    let trimmed = sheetpath.trim();
    if trimmed.is_empty() || trimmed == "/" {
        return "/".to_string();
    }
    let mut out = trimmed.to_string();
    if !out.starts_with('/') {
        out.insert(0, '/');
    }
    if !out.ends_with('/') {
        out.push('/');
    }
    out
}

fn key_from_schematic_instance_path(
    instance_path: &str,
    symbol_uuid: &str,
) -> Result<KiCadUuidPathKey> {
    let trimmed = instance_path.trim();
    if !trimmed.starts_with('/') {
        anyhow::bail!("Expected schematic instance path to start with '/': {instance_path:?}");
    }
    let parts: Vec<&str> = trimmed
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();

    // Instance paths include the root schematic UUID as the first segment; PCB paths do not.
    let sheet_parts = if parts.len() <= 1 {
        &[][..]
    } else {
        &parts[1..]
    };
    let sheetpath_tstamps = if sheet_parts.is_empty() {
        "/".to_string()
    } else {
        format!("/{}/", sheet_parts.join("/"))
    };

    Ok(KiCadUuidPathKey {
        sheetpath_tstamps,
        symbol_uuid: symbol_uuid.to_string(),
    })
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

    #[test]
    fn parses_kicad_sexpr_netlist_and_builds_uuid_path_keys() -> Result<()> {
        let netlist = r#"
(export (version "E")
  (design (source "x") (date "x") (tool "Eeschema"))
  (components
    (comp (ref "R1")
      (value "10k")
      (footprint "Resistor_SMD:R_0402_1005Metric")
      (sheetpath (names "/") (tstamps "/"))
      (tstamps "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"))
    (comp (ref "U1")
      (value "MCU")
      (footprint "Package_QFP:LQFP-48_7x7mm_P0.5mm")
      (sheetpath (names "/SoM/") (tstamps "/11111111-2222-3333-4444-555555555555/"))
      (tstamps "99999999-8888-7777-6666-555555555555"))
  )
  (nets
    (net (code "1") (name "VCC") (class "Default")
      (node (ref "R1") (pin "1") (pintype "passive"))
      (node (ref "U1") (pin "3") (pintype "power_in")))
  )
)
"#;

        let parsed = parse_kicad_sexpr_netlist(netlist)?;
        assert_eq!(parsed.components.len(), 2);
        assert_eq!(parsed.nets.len(), 1);

        assert!(parsed
            .components
            .contains_key("/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"));
        assert!(parsed.components.contains_key(
            "/11111111-2222-3333-4444-555555555555/99999999-8888-7777-6666-555555555555"
        ));

        let net = parsed.nets.get("VCC").expect("missing net");
        assert!(net.ports.contains(&ImportNetPort {
            component: "/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_string(),
            pin: "1".to_string()
        }));
        assert!(net.ports.contains(&ImportNetPort {
            component: "/11111111-2222-3333-4444-555555555555/99999999-8888-7777-6666-555555555555"
                .to_string(),
            pin: "3".to_string()
        }));

        Ok(())
    }
}
