use anyhow::{Context, Result as AnyhowResult};
use log::{debug, info};
use pcb_sch::{AttributeValue, InstanceKind, Schematic, ATTR_LAYOUT_PATH};
use pcb_zen_core::diagnostics::Diagnostic;
use pcb_zen_core::lang::stackup::{BoardConfig, BoardConfigError, NetClass, Stackup, StackupError};
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};
use starlark::errors::EvalSeverity;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use thiserror::Error;

use include_dir::{include_dir, Dir};
use pcb_kicad::PythonScriptBuilder;
use pcb_sch::kicad_netlist::{format_footprint, write_fp_lib_table};

mod moved;
mod repair_nets;
pub use moved::compute_moved_paths_patches;
pub use moved::compute_net_renames_patches;

/// Embedded lens module directory (for Python imports)
static LENS_MODULE: Dir = include_dir!("$CARGO_MANIFEST_DIR/src/scripts/lens");

/// Result of layout generation/update
#[derive(Debug)]
pub struct LayoutResult {
    pub source_file: PathBuf,
    pub layout_dir: PathBuf,
    pub pcb_file: PathBuf,
    pub netlist_file: PathBuf,
    pub snapshot_file: PathBuf,
    pub log_file: PathBuf,
    pub diagnostics_file: PathBuf,
    pub created: bool, // true if new, false if updated
    pub shadow: Option<ShadowLayoutContext>,
}

#[derive(Debug)]
pub struct ShadowLayoutContext {
    _dir: TempDir,
    original_pcb_file: PathBuf,
}

impl LayoutResult {
    /// Path to show in diagnostics/output. In `--check` mode this is the original PCB path,
    /// not the shadow-copy PCB.
    pub fn display_pcb_file(&self) -> &Path {
        self.shadow
            .as_ref()
            .map(|s| s.original_pcb_file.as_path())
            .unwrap_or(self.pcb_file.as_path())
    }
}

/// Error types for layout operations
#[derive(Debug, Error)]
pub enum LayoutError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    PcbGeneration(#[from] anyhow::Error),

    #[error("Stackup patching error: {0}")]
    StackupPatchingError(String),

    #[error("Stackup error: {0}")]
    StackupError(#[from] StackupError),

    #[error("Board config error: {0}")]
    BoardConfigError(#[from] BoardConfigError),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Helper struct for layout file paths
#[derive(Debug)]
pub struct LayoutPaths {
    pub netlist: PathBuf,
    pub pcb: PathBuf,
    pub snapshot: PathBuf,
    pub log: PathBuf,
    pub json_netlist: PathBuf,
    pub board_config: PathBuf,
    pub diagnostics: PathBuf,
    pub temp_dir: TempDir,
}

/// A single diagnostic from layout sync (e.g., FPID mismatch)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutSyncDiagnostic {
    pub kind: String,
    pub severity: String,
    pub body: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub reference: Option<String>,
}

impl LayoutSyncDiagnostic {
    /// Convert to a pcb_zen_core Diagnostic
    pub fn to_diagnostic(&self, pcb_path: &str) -> Diagnostic {
        let severity = match self.severity.as_str() {
            "error" => EvalSeverity::Error,
            _ => EvalSeverity::Warning,
        };
        let body = match &self.reference {
            Some(ref_des) => format!("{}: {}", ref_des, self.body),
            None => self.body.clone(),
        };
        Diagnostic::categorized(pcb_path, &body, &self.kind, severity)
    }
}

/// Container for layout sync diagnostics from Python script
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutSyncDiagnostics {
    pub diagnostics: Vec<LayoutSyncDiagnostic>,
}

impl LayoutSyncDiagnostics {
    /// Parse diagnostics from a JSON file
    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        let contents = fs::read_to_string(path).context("Failed to read diagnostics file")?;
        serde_json::from_str(&contents).context("Failed to parse diagnostics JSON")
    }
}

/// Check for moved() paths that target content inside submodules with their own layouts.
/// Returns warnings for paths that can't be fully applied because submodule layouts are read-only.
/// Only warns about instance paths (components/modules), not net names.
fn check_submodule_moved_paths(schematic: &Schematic) -> Vec<String> {
    let mut warnings = Vec::new();

    if schematic.moved_paths.is_empty() {
        return warnings;
    }

    // Build a set of all instance paths for quick lookup
    let instance_paths: HashSet<String> = schematic
        .instances
        .keys()
        .map(|iref| iref.instance_path.join("."))
        .filter(|p| !p.is_empty())
        .collect();

    // Collect paths of modules that have their own layout_path attribute
    let mut module_layout_paths: Vec<String> = Vec::new();
    for (instance_ref, instance) in &schematic.instances {
        if instance.kind == InstanceKind::Module
            && instance.attributes.contains_key(ATTR_LAYOUT_PATH)
        {
            // Build the path string from instance_path (e.g., ["board", "module"] -> "board.module")
            let path = instance_ref
                .instance_path
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(".");
            if !path.is_empty() {
                module_layout_paths.push(path);
            }
        }
    }

    if module_layout_paths.is_empty() {
        return warnings;
    }

    // Check each moved_path to see if it renames something INSIDE a submodule
    for (old_path, new_path) in &schematic.moved_paths {
        // Only warn if the old_path corresponds to an actual instance path
        // (not just a net name that happens to look hierarchical)
        let is_instance_path = instance_paths.contains(old_path)
            || instance_paths
                .iter()
                .any(|ip| ip.starts_with(&format!("{}.", old_path)));

        if !is_instance_path {
            continue; // This is likely a net rename, skip
        }

        for module_path in &module_layout_paths {
            // Check if old_path starts with module_path and extends beyond it
            // e.g., old_path="board.module.R1" with module_path="board.module"
            if old_path.starts_with(module_path) {
                let suffix = &old_path[module_path.len()..];
                if suffix.starts_with('.') {
                    warnings.push(format!(
                        "moved(\"{}\", \"{}\") renames content inside submodule '{}' which has its own layout; \
                         submodule layouts are read-only and won't be patched",
                        old_path, new_path, module_path
                    ));
                }
            }
        }
    }

    warnings
}

fn apply_patches_to_file(
    pcb_path: &Path,
    pcb_content: &str,
    patches: &pcb_sexpr::PatchSet,
) -> anyhow::Result<()> {
    let patched = render_patches(pcb_content, patches)?;
    write_text_atomic(pcb_path, &patched)
}

fn write_text_atomic(path: &Path, text: &str) -> anyhow::Result<()> {
    let tmp_path = path.with_extension("kicad_pcb.tmp");
    let file = fs::File::create(&tmp_path)
        .with_context(|| format!("Failed to create temp file: {}", tmp_path.display()))?;
    let mut writer = std::io::BufWriter::new(file);

    writer
        .write_all(text.as_bytes())
        .with_context(|| format!("Failed to write file: {}", path.display()))?;

    writer
        .flush()
        .with_context(|| format!("Failed to flush temp file: {}", tmp_path.display()))?;
    fs::rename(&tmp_path, path)
        .with_context(|| format!("Failed to rename temp file to: {}", path.display()))?;
    Ok(())
}

fn render_patches(source: &str, patches: &pcb_sexpr::PatchSet) -> anyhow::Result<String> {
    let mut out = Vec::new();
    patches
        .write_to(source, &mut out)
        .context("Failed to apply patches")?;
    String::from_utf8(out).context("Patched PCB is not valid UTF-8")
}

/// Apply moved() path renames to a PCB file
fn apply_moved_paths(
    pcb_path: &Path,
    moved_paths: &HashMap<String, String>,
    diagnostics_pcb_path: &str,
    diagnostics: &mut pcb_zen_core::Diagnostics,
) -> anyhow::Result<()> {
    if moved_paths.is_empty() {
        return Ok(());
    }

    let pcb_content = fs::read_to_string(pcb_path)
        .with_context(|| format!("Failed to read PCB file: {}", pcb_path.display()))?;
    let board = pcb_sexpr::parse(&pcb_content)
        .with_context(|| format!("Failed to parse PCB file: {}", pcb_path.display()))?;

    let (patches, renames) = compute_moved_paths_patches(&board, moved_paths);

    if renames.is_empty() {
        return Ok(());
    }

    apply_patches_to_file(pcb_path, &pcb_content, &patches)?;

    for (old_path, new_path) in &renames {
        diagnostics.diagnostics.push(Diagnostic::categorized(
            diagnostics_pcb_path,
            &format!("moved \"{}\" → \"{}\"", old_path, new_path),
            "layout.moved",
            EvalSeverity::Advice,
        ));
    }
    Ok(())
}

/// Detect and apply implicit net renames.
///
/// This is Phase 1.5: after explicit moved() renames, before Python sync.
/// Detects nets that were renamed without explicit moved() directives and
/// patches the layout file to update the net names.
fn repair_net_names(
    pcb_path: &Path,
    schematic: &Schematic,
    diagnostics_pcb_path: &str,
    diagnostics: &mut pcb_zen_core::Diagnostics,
) -> anyhow::Result<()> {
    let pcb_content = fs::read_to_string(pcb_path)
        .with_context(|| format!("Failed to read PCB file: {}", pcb_path.display()))?;
    let board = pcb_sexpr::parse(&pcb_content)
        .with_context(|| format!("Failed to parse PCB file: {}", pcb_path.display()))?;

    let result = repair_nets::detect_implicit_renames(schematic, &board);

    if result.renames.is_empty() && result.orphaned_layout_nets.is_empty() {
        return Ok(());
    }

    // Report orphaned layout-only nets as warnings
    for orphaned_net in &result.orphaned_layout_nets {
        let msg = format!(
            "\"{}\" not in netlist and could not be auto-resolved",
            orphaned_net
        );
        diagnostics.diagnostics.push(Diagnostic::categorized(
            diagnostics_pcb_path,
            &msg,
            "layout.orphaned_net",
            EvalSeverity::Warning,
        ));
    }

    if !result.renames.is_empty() {
        let (patches, _) = moved::compute_net_renames_patches(&board, &result.renames);
        apply_patches_to_file(pcb_path, &pcb_content, &patches)?;

        // Only report implicit renames after the patch successfully applies.
        for (old_net, new_net) in &result.renames {
            let msg = format!("implicit rename \"{}\" -> \"{}\"", old_net, new_net);

            diagnostics.diagnostics.push(Diagnostic::categorized(
                diagnostics_pcb_path,
                &msg,
                "layout.implicit_rename",
                EvalSeverity::Advice,
            ));
        }
    }

    Ok(())
}

/// Extract the embedded lens module to a directory.
///
/// Writes all .py files from the embedded lens module to a "lens" subdirectory
/// under the given path, making it importable via PYTHONPATH.
fn extract_lens_module(base_path: &Path) -> AnyhowResult<PathBuf> {
    let lens_dir = base_path.join("lens");
    fs::create_dir_all(&lens_dir)
        .with_context(|| format!("Failed to create lens directory: {}", lens_dir.display()))?;

    // Extract all files from the embedded directory
    extract_dir_recursive(&LENS_MODULE, &lens_dir)?;

    Ok(base_path.to_path_buf())
}

/// Recursively extract files from an include_dir Dir to a filesystem path
fn extract_dir_recursive(dir: &Dir, target: &Path) -> AnyhowResult<()> {
    // Extract files
    for file in dir.files() {
        let file_path = target.join(file.path().file_name().unwrap());
        fs::write(&file_path, file.contents())
            .with_context(|| format!("Failed to write file: {}", file_path.display()))?;
    }

    // Recursively extract subdirectories (but skip 'tests' and 'tla')
    for subdir in dir.dirs() {
        let subdir_name = subdir.path().file_name().unwrap().to_str().unwrap();
        if subdir_name == "tests" || subdir_name == "tla" || subdir_name == "__pycache__" {
            continue;
        }

        let subdir_path = target.join(subdir_name);
        fs::create_dir_all(&subdir_path)?;
        extract_dir_recursive(subdir, &subdir_path)?;
    }

    Ok(())
}

/// Run the Python layout sync script
fn run_sync_script(
    paths: &LayoutPaths,
    lens_python_path: &Path,
    board_config_path: Option<&str>,
) -> anyhow::Result<()> {
    let script = include_str!("scripts/update_layout_file.py");
    let mut builder = PythonScriptBuilder::new(script)
        .python_path(lens_python_path.to_str().unwrap())
        .arg("-j")
        .arg(paths.json_netlist.to_str().unwrap())
        .arg("-o")
        .arg(paths.pcb.to_str().unwrap())
        .arg("-s")
        .arg(paths.snapshot.to_str().unwrap())
        .arg("--diagnostics")
        .arg(paths.diagnostics.to_str().unwrap());

    if let Some(config_path) = board_config_path {
        builder = builder.arg("--board-config").arg(config_path);
    }

    let log_file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&paths.log)?;

    builder.log_file(log_file).run()
}

fn write_board_config_for_script(
    board_config: Option<&BoardConfig>,
    board_config_path: &Path,
) -> anyhow::Result<Option<String>> {
    let Some(config) = board_config else {
        return Ok(None);
    };

    let json = serde_json::to_string(config).context("Failed to serialize board config")?;
    fs::write(board_config_path, json).with_context(|| {
        format!(
            "Failed to write board config file: {}",
            board_config_path.display()
        )
    })?;
    Ok(Some(board_config_path.to_string_lossy().into_owned()))
}

fn create_shadow_layout_dir(layout_dir: &Path) -> anyhow::Result<TempDir> {
    let parent = layout_dir.parent().ok_or_else(|| {
        anyhow::anyhow!("Layout directory has no parent: {}", layout_dir.display())
    })?;
    let base_name = layout_dir
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow::anyhow!("Layout directory has invalid UTF-8 name"))?;
    let temp_dir = tempfile::Builder::new()
        .prefix(&format!(".{base_name}.pcb-check."))
        .tempdir_in(parent)
        .with_context(|| {
            format!(
                "Failed to create shadow layout directory near {}",
                layout_dir.display()
            )
        })?;
    copy_dir_recursive(layout_dir, temp_dir.path())?;
    Ok(temp_dir)
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> anyhow::Result<()> {
    for entry in fs::read_dir(src).with_context(|| format!("Failed to read {}", src.display()))? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let file_type = entry.file_type()?;

        if file_type.is_dir() {
            fs::create_dir(&dst_path)
                .with_context(|| format!("Failed to create {}", dst_path.display()))?;
            copy_dir_recursive(&src_path, &dst_path)?;
            continue;
        }

        fs::copy(&src_path, &dst_path).with_context(|| {
            format!(
                "Failed to copy {} to {}",
                src_path.display(),
                dst_path.display()
            )
        })?;
    }
    Ok(())
}

/// Process a schematic and generate/update its layout files
///
/// When `check_mode` is false (normal mode):
/// 1. Extract the layout path from the schematic's root instance attributes
/// 2. Create the layout directory if it doesn't exist
/// 3. Generate/update the netlist file
/// 4. Write the footprint library table
/// 5. Create or update the KiCad PCB file
///
/// When `check_mode` is true:
/// - Requires the real layout to already exist
/// - Creates a hidden shadow sibling layout directory
/// - Runs the exact same mutating sync pipeline against the shadow copy
/// - Returns paths pointing to the shadow copy for downstream DRC
pub fn process_layout(
    schematic: &Schematic,
    use_temp_dir: bool,
    check_mode: bool,
    diagnostics: &mut pcb_zen_core::Diagnostics,
) -> Result<Option<LayoutResult>, LayoutError> {
    // Resolve layout directory
    let resolved_layout_dir = if use_temp_dir {
        // Create a temporary directory and keep it (prevent cleanup on drop)
        tempfile::Builder::new()
            .prefix("pcb-layout-")
            .tempdir()
            .expect("Failed to create temporary directory")
            .keep()
    } else {
        match utils::resolve_layout_dir(schematic)? {
            Some(path) => path,
            None => return Ok(None),
        }
    };

    let source_path = schematic
        .root_ref
        .as_ref()
        .map(|r| r.module.source_path.clone())
        .unwrap_or_default();

    let shadow = if check_mode {
        let Some(existing_files) = utils::discover_kicad_files(&resolved_layout_dir)? else {
            return Ok(None);
        };
        let original_pcb_file = existing_files.kicad_pcb();
        if !original_pcb_file.exists() {
            return Ok(None);
        }
        let shadow_dir = create_shadow_layout_dir(&resolved_layout_dir)?;
        Some(ShadowLayoutContext {
            _dir: shadow_dir,
            original_pcb_file,
        })
    } else {
        None
    };

    let layout_dir = shadow
        .as_ref()
        .map(|s| s._dir.path().to_path_buf())
        .unwrap_or_else(|| resolved_layout_dir.clone());

    let kicad_files = utils::resolve_kicad_files(&layout_dir)?;
    let paths = utils::get_layout_paths_for_pcb(&layout_dir, kicad_files.kicad_pcb());
    let diagnostics_pcb_path = shadow
        .as_ref()
        .map(|s| s.original_pcb_file.to_string_lossy().to_string())
        .unwrap_or_else(|| paths.pcb.to_string_lossy().to_string());

    debug!(
        "Generating layout for {} in {}",
        source_path.display(),
        layout_dir.display()
    );

    fs::create_dir_all(&layout_dir).with_context(|| {
        format!(
            "Failed to create layout directory: {}",
            layout_dir.display()
        )
    })?;

    // Write netlist files
    let netlist_content = pcb_sch::kicad_netlist::to_kicad_netlist(schematic);
    fs::write(&paths.netlist, netlist_content)
        .with_context(|| format!("Failed to write netlist: {}", paths.netlist.display()))?;

    // Write footprint library table
    utils::write_footprint_library_table(&layout_dir, schematic)?;

    // Write JSON netlist for Python script
    let json_content = schematic
        .to_json()
        .context("Failed to serialize schematic to JSON")?;
    fs::write(&paths.json_netlist, json_content).with_context(|| {
        format!(
            "Failed to write JSON netlist: {}",
            paths.json_netlist.display()
        )
    })?;

    let board_config = utils::extract_board_config(schematic);

    // Write board config for Python script if it exists
    let board_config_path =
        write_board_config_for_script(board_config.as_ref(), &paths.board_config)?;

    let pcb_exists = paths.pcb.exists();
    debug!(
        "{} layout file: {}",
        if pcb_exists { "Updating" } else { "Creating" },
        paths.pcb.display()
    );

    // Check for moved() paths that can't be applied to submodule layouts (always warn)
    for warning in check_submodule_moved_paths(schematic) {
        diagnostics.diagnostics.push(Diagnostic::categorized(
            &diagnostics_pcb_path,
            &warning,
            "layout.moved",
            EvalSeverity::Warning,
        ));
    }

    // Apply moved() path renames and detect implicit net renames before sync
    if pcb_exists {
        apply_moved_paths(
            &paths.pcb,
            &schematic.moved_paths,
            &diagnostics_pcb_path,
            diagnostics,
        )?;
        repair_net_names(&paths.pcb, schematic, &diagnostics_pcb_path, diagnostics)?;
    }

    // Extract lens module to temp directory for Python imports
    let lens_python_path =
        extract_lens_module(paths.temp_dir.path()).context("Failed to extract lens module")?;

    // Run the Python sync script
    run_sync_script(&paths, &lens_python_path, board_config_path.as_deref())?;

    // Apply board config (stackup + netclass patterns)
    if let Some(ref config) = board_config {
        if let Some(ref stackup) = config.stackup {
            patch_stackup(&paths.pcb, stackup)?;
        }

        let assignments = build_netclass_assignments(schematic, config.netclasses());
        if !assignments.is_empty() {
            patch_netclass_patterns(&paths.pcb, &assignments)?;
        }
    }

    // Add sync diagnostics from JSON file
    if paths.diagnostics.exists() {
        let sync_diagnostics = LayoutSyncDiagnostics::from_file(&paths.diagnostics)?;
        for sync_diag in sync_diagnostics.diagnostics {
            diagnostics
                .diagnostics
                .push(sync_diag.to_diagnostic(&diagnostics_pcb_path));
        }
    }

    // In check mode, fail if the layout would be modified by a sync.
    //
    // Since check mode runs the full sync pipeline against a shadow copy, we can compare
    // the shadow PCB file after sync with the original PCB file in the real layout dir.
    if check_mode {
        if let Some(ref shadow) = shadow {
            let original_bytes = fs::read(&shadow.original_pcb_file).with_context(|| {
                format!(
                    "Failed to read PCB file: {}",
                    shadow.original_pcb_file.display()
                )
            })?;
            let shadow_bytes = fs::read(&paths.pcb)
                .with_context(|| format!("Failed to read PCB file: {}", paths.pcb.display()))?;
            if original_bytes != shadow_bytes {
                diagnostics.diagnostics.push(Diagnostic::categorized(
                    &diagnostics_pcb_path,
                    "Layout is out of sync (run without --check to apply changes)",
                    "layout.sync",
                    EvalSeverity::Error,
                ));
            }
        }
    }

    Ok(Some(LayoutResult {
        source_file: source_path,
        layout_dir,
        pcb_file: paths.pcb.clone(),
        netlist_file: paths.netlist,
        snapshot_file: paths.snapshot,
        log_file: paths.log,
        diagnostics_file: paths.diagnostics,
        created: !pcb_exists,
        shadow,
    }))
}

/// Utility functions
pub mod utils {
    use super::*;
    use pcb_sch::InstanceKind;
    use std::collections::HashMap;

    /// Resolve layout directory from schematic.
    /// Returns `Ok(None)` if no `layout_path` attribute is set.
    pub fn resolve_layout_dir(schematic: &Schematic) -> anyhow::Result<Option<PathBuf>> {
        let uri = schematic
            .root_ref
            .as_ref()
            .and_then(|r| schematic.instances.get(r))
            .and_then(|inst| inst.attributes.get(ATTR_LAYOUT_PATH))
            .and_then(|v| v.string());
        match uri {
            None => Ok(None),
            Some(s) => schematic
                .resolve_package_uri(s)
                .map(Some)
                .with_context(|| format!("Failed to resolve layout_path '{s}'")),
        }
    }

    pub const DEFAULT_KICAD_BASENAME: &str = "layout";

    #[derive(Debug, Clone)]
    pub struct KiCadLayoutFiles {
        /// KiCad project file path (`.kicad_pro`).
        pub kicad_pro: PathBuf,
    }

    impl KiCadLayoutFiles {
        pub fn kicad_pcb(&self) -> PathBuf {
            self.kicad_pro.with_extension("kicad_pcb")
        }

        pub fn kicad_sch(&self) -> PathBuf {
            self.kicad_pro.with_extension("kicad_sch")
        }
    }

    /// Discover KiCad files in a layout directory by finding a single `.kicad_pro` file.
    ///
    /// The `.kicad_pcb` path is derived from the project file name. This avoids
    /// false ambiguity from KiCad autosave files like `_autosave-layout.kicad_pcb`.
    pub fn discover_kicad_files(layout_dir: &Path) -> anyhow::Result<Option<KiCadLayoutFiles>> {
        if !layout_dir.exists() {
            return Ok(None);
        }
        if !layout_dir.is_dir() {
            anyhow::bail!("Path is not a directory: {}", layout_dir.display());
        }

        let mut pro_path: Option<PathBuf> = None;
        for entry in fs::read_dir(layout_dir)
            .with_context(|| format!("Failed to read {}", layout_dir.display()))?
        {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("kicad_pro")
                && pro_path.replace(path).is_some()
            {
                anyhow::bail!(
                    "Multiple .kicad_pro files found in {}",
                    layout_dir.display()
                );
            }
        }

        Ok(pro_path.map(|p| KiCadLayoutFiles { kicad_pro: p }))
    }

    /// Require a discoverable KiCad layout in `layout_dir`.
    pub fn require_kicad_files(layout_dir: &Path) -> anyhow::Result<KiCadLayoutFiles> {
        discover_kicad_files(layout_dir)?
            .ok_or_else(|| anyhow::anyhow!("No .kicad_pro file found in {}", layout_dir.display()))
    }

    /// Resolve target file names for layout generation (defaults to `layout.*`).
    pub fn resolve_kicad_files(layout_dir: &Path) -> anyhow::Result<KiCadLayoutFiles> {
        if let Some(existing) = discover_kicad_files(layout_dir)? {
            return Ok(existing);
        }
        Ok(KiCadLayoutFiles {
            kicad_pro: layout_dir.join(format!("{DEFAULT_KICAD_BASENAME}.kicad_pro")),
        })
    }

    /// Get all the file paths that would be generated for a layout, with explicit PCB path.
    pub fn get_layout_paths_for_pcb(layout_dir: &Path, pcb_path: PathBuf) -> LayoutPaths {
        let temp_dir = tempfile::tempdir().expect("Failed to create temp directory for netlist");
        let json_netlist = temp_dir.path().join("netlist.json");
        let board_config = temp_dir.path().join("board_config.json");
        let diagnostics = temp_dir.path().join("diagnostics.layout.json");
        LayoutPaths {
            netlist: layout_dir.join("default.net"),
            pcb: pcb_path,
            snapshot: layout_dir.join("snapshot.layout.json"),
            log: layout_dir.join("layout.log"),
            json_netlist,
            board_config,
            diagnostics,
            temp_dir,
        }
    }

    /// Extract and parse board config from schematic's root instance attributes
    pub fn extract_board_config(schematic: &Schematic) -> Option<BoardConfig> {
        let root = schematic.instances.get(schematic.root_ref.as_ref()?)?;

        // Find board_config.* property (prefer "default")
        let config_json = root
            .attributes
            .iter()
            .filter(|(k, _)| k.starts_with("board_config."))
            .find(|(k, _)| k == &"board_config.default")
            .or_else(|| {
                root.attributes
                    .iter()
                    .find(|(k, _)| k.starts_with("board_config."))
            })
            .and_then(|(_, v)| v.string())?;

        BoardConfig::from_json_str(config_json).ok()
    }

    /// Write footprint library table for a layout
    pub fn write_footprint_library_table(
        layout_dir: &Path,
        schematic: &Schematic,
    ) -> anyhow::Result<()> {
        let mut fp_libs: HashMap<String, PathBuf> = HashMap::new();

        for inst in schematic.instances.values() {
            if inst.kind != InstanceKind::Component {
                continue;
            }

            if let Some(AttributeValue::String(fp_attr)) = inst.attributes.get("footprint") {
                let resolved_fp = schematic
                    .resolve_package_uri(fp_attr)
                    .with_context(|| format!("Failed to resolve footprint path '{fp_attr}'"))?
                    .to_string_lossy()
                    .into_owned();
                if let (_, Some((lib_name, dir))) = format_footprint(&resolved_fp) {
                    fp_libs.entry(lib_name).or_insert(dir);
                }
            }
        }

        // Canonicalize the layout directory to avoid symlink issues on macOS
        let canonical_layout_dir = layout_dir
            .canonicalize()
            .unwrap_or_else(|_| layout_dir.to_path_buf());

        // Write or update the fp-lib-table for this layout directory
        write_fp_lib_table(&canonical_layout_dir, &fp_libs).with_context(|| {
            format!("Failed to write fp-lib-table for {}", layout_dir.display())
        })?;

        Ok(())
    }
}

/// Build netclass assignments from net impedance properties
fn build_netclass_assignments(
    schematic: &Schematic,
    netclasses: &[NetClass],
) -> HashMap<String, String> {
    const TOLERANCE: f64 = 0.05; // ±5%

    let mut assignments = HashMap::new();

    for (net_name, net) in &schematic.nets {
        // Check for differential impedance (from DiffPair propagation)
        let diff_impedance = net
            .properties
            .get("differential_impedance")
            .and_then(AttributeValue::physical)
            .and_then(|pv| {
                if pv.unit == pcb_sch::PhysicalUnit::Ohms.into() {
                    pv.nominal.to_f64()
                } else {
                    None
                }
            });

        // Check for single-ended impedance (from individual nets)
        let se_impedance = net
            .properties
            .get("impedance")
            .and_then(AttributeValue::physical)
            .and_then(|pv| {
                if pv.unit == pcb_sch::PhysicalUnit::Ohms.into() {
                    pv.nominal.to_f64()
                } else {
                    None
                }
            });

        // Match differential impedance to differential netclasses
        if let Some(imp) = diff_impedance {
            let matched = netclasses
                .iter()
                .filter_map(|nc| {
                    let target = nc.differential_pair_impedance_ohms()?;
                    let error: f64 = ((imp - target) / target).abs();
                    (error <= TOLERANCE).then_some((nc, error))
                })
                .min_by(|(_, e1), (_, e2)| e1.partial_cmp(e2).unwrap());

            if let Some((nc, _)) = matched {
                assignments.insert(net_name.clone(), nc.name.clone());
            }
        }
        // Match single-ended impedance to single-ended netclasses
        else if let Some(imp) = se_impedance {
            let matched = netclasses
                .iter()
                .filter_map(|nc| {
                    let target = nc.single_ended_impedance_ohms()?;
                    let error: f64 = ((imp - target) / target).abs();
                    (error <= TOLERANCE).then_some((nc, error))
                })
                .min_by(|(_, e1), (_, e2)| e1.partial_cmp(e2).unwrap());

            if let Some((nc, _)) = matched {
                assignments.insert(net_name.clone(), nc.name.clone());
            }
        }
    }

    assignments
}

/// Apply netclass pattern assignments to .kicad_pro file
fn patch_netclass_patterns(
    pcb_path: &Path,
    assignments: &std::collections::HashMap<String, String>,
) -> Result<(), LayoutError> {
    if assignments.is_empty() {
        debug!("No netclass assignments to write");
        return Ok(());
    }

    let pro_path = pcb_path.with_extension("kicad_pro");
    info!(
        "Writing {} netclass patterns to {}",
        assignments.len(),
        pro_path.display()
    );

    // Read, modify, and write .kicad_pro JSON
    let pro_content = fs::read_to_string(&pro_path)
        .with_context(|| format!("Failed to read {}", pro_path.display()))?;

    let mut pro_json: serde_json::Value = serde_json::from_str(&pro_content)
        .with_context(|| format!("Failed to parse {}", pro_path.display()))?;

    // Sort netclass patterns by net name for stable output (prevent spurious diffs)
    let mut sorted_assignments: Vec<_> = assignments.iter().collect();
    sorted_assignments.sort_by_key(|(net_name, _)| *net_name);

    let patterns: Vec<_> = sorted_assignments
        .into_iter()
        .map(|(net_name, netclass_name)| {
            serde_json::json!({"pattern": net_name, "netclass": netclass_name})
        })
        .collect();

    pro_json["net_settings"]["netclass_patterns"] = serde_json::json!(patterns);

    fs::write(&pro_path, serde_json::to_string_pretty(&pro_json)?)
        .with_context(|| format!("Failed to write {}", pro_path.display()))?;

    Ok(())
}

/// Apply and normalize stackup-related sections in a PCB file.
fn patch_stackup(pcb_path: &Path, zen_stackup: &Stackup) -> Result<(), LayoutError> {
    let pcb_content = fs::read_to_string(pcb_path).map_err(|e| {
        LayoutError::StackupPatchingError(format!("Failed to read PCB file: {}", e))
    })?;

    let board = pcb_sexpr::parse(&pcb_content).map_err(|e| {
        LayoutError::StackupPatchingError(format!("Failed to parse PCB file: {}", e))
    })?;

    let board_thickness_iu = stackup_thickness_iu(zen_stackup);
    let layers = zen_stackup.generate_layers_expr(4);
    let stackup = zen_stackup.generate_stackup_expr();
    let patches = build_stackup_patchset(&board, &layers, &stackup, board_thickness_iu)?;
    let patched = render_patches(&pcb_content, &patches).map_err(|e| {
        LayoutError::StackupPatchingError(format!("Failed to apply stackup patches: {}", e))
    })?;
    let updated_content =
        pcb_sexpr::formatter::prettify(&patched, pcb_sexpr::formatter::FormatMode::Normal);

    info!("Updating stackup configuration in {}", pcb_path.display());
    write_text_atomic(pcb_path, &updated_content).map_err(|e| {
        LayoutError::StackupPatchingError(format!(
            "Failed to write updated PCB file {}: {}",
            pcb_path.display(),
            e
        ))
    })?;
    info!("Successfully updated stackup configuration");

    Ok(())
}

const PCB_IU_PER_MM: f64 = 1_000_000.0;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct PcbIu(i64);

impl PcbIu {
    fn from_mm(mm: f64) -> Option<Self> {
        let scaled = mm * PCB_IU_PER_MM;
        if !scaled.is_finite() || scaled < i64::MIN as f64 || scaled > i64::MAX as f64 {
            return None;
        }

        Some(Self(scaled.round() as i64))
    }

    fn to_kicad_mm_text(self) -> String {
        let sign = if self.0 < 0 { "-" } else { "" };
        let abs = self.0.unsigned_abs();
        let whole = abs / 1_000_000;
        let frac = abs % 1_000_000;

        if frac == 0 {
            return format!("{sign}{whole}");
        }

        let mut frac_text = format!("{frac:06}");
        while frac_text.ends_with('0') {
            frac_text.pop();
        }

        format!("{sign}{whole}.{frac_text}")
    }
}

fn build_stackup_patchset(
    board: &pcb_sexpr::Sexpr,
    layers: &pcb_sexpr::Sexpr,
    stackup: &pcb_sexpr::Sexpr,
    board_thickness_iu: Option<PcbIu>,
) -> Result<pcb_sexpr::PatchSet, LayoutError> {
    let root_items = board.as_list().ok_or_else(|| {
        LayoutError::StackupPatchingError("PCB root is not an S-expression list".to_string())
    })?;
    if root_items.first().and_then(pcb_sexpr::Sexpr::as_sym) != Some("kicad_pcb") {
        return Err(LayoutError::StackupPatchingError(
            "PCB root must start with (kicad_pcb ...)".to_string(),
        ));
    }

    let mut patches = pcb_sexpr::PatchSet::new();

    let layers_idx = pcb_sexpr::find_named_list_index(root_items, "layers").ok_or_else(|| {
        LayoutError::StackupPatchingError("PCB file is missing (layers ...) section".to_string())
    })?;
    let layers_span = root_items
        .get(layers_idx)
        .ok_or_else(|| {
            LayoutError::StackupPatchingError("Invalid layers span in PCB file".to_string())
        })?
        .span;
    patches.replace_raw(layers_span, layers.to_string());

    let setup_idx = pcb_sexpr::find_named_list_index(root_items, "setup").ok_or_else(|| {
        LayoutError::StackupPatchingError("PCB file is missing (setup ...) section".to_string())
    })?;
    let setup_node = root_items.get(setup_idx).ok_or_else(|| {
        LayoutError::StackupPatchingError("Invalid setup span in PCB file".to_string())
    })?;
    let setup_items = setup_node.as_list().ok_or_else(|| {
        LayoutError::StackupPatchingError("setup section is not a list".to_string())
    })?;
    if let Some(stackup_idx) = pcb_sexpr::find_named_list_index(setup_items, "stackup") {
        let stackup_span = setup_items
            .get(stackup_idx)
            .ok_or_else(|| {
                LayoutError::StackupPatchingError("Invalid stackup span in PCB file".to_string())
            })?
            .span;
        patches.replace_raw(stackup_span, stackup.to_string());
    } else {
        let mut new_setup = setup_node.clone();
        let setup_items = new_setup.as_list_mut().ok_or_else(|| {
            LayoutError::StackupPatchingError("setup section is not a list".to_string())
        })?;
        pcb_sexpr::set_or_insert_named_list(setup_items, "stackup", stackup.clone(), None);
        patches.replace_raw(setup_node.span, new_setup.to_string());
    }

    if let Some(board_thickness_iu) = board_thickness_iu {
        let general_idx =
            pcb_sexpr::find_named_list_index(root_items, "general").ok_or_else(|| {
                LayoutError::StackupPatchingError(
                    "PCB file is missing (general ...) section".to_string(),
                )
            })?;
        let general_node = root_items.get(general_idx).ok_or_else(|| {
            LayoutError::StackupPatchingError("Invalid general span in PCB file".to_string())
        })?;
        let general_items = general_node.as_list().ok_or_else(|| {
            LayoutError::StackupPatchingError("general section is not a list".to_string())
        })?;

        let thickness = pcb_sexpr::Sexpr::list(vec![
            pcb_sexpr::Sexpr::symbol("thickness"),
            pcb_sexpr::Sexpr::symbol(board_thickness_iu.to_kicad_mm_text()),
        ]);

        if let Some(thickness_idx) = pcb_sexpr::find_named_list_index(general_items, "thickness") {
            let thickness_span = general_items
                .get(thickness_idx)
                .ok_or_else(|| {
                    LayoutError::StackupPatchingError(
                        "Invalid thickness span in PCB file".to_string(),
                    )
                })?
                .span;
            patches.replace_raw(thickness_span, thickness.to_string());
        } else {
            let mut new_general = general_node.clone();
            let general_items = new_general.as_list_mut().ok_or_else(|| {
                LayoutError::StackupPatchingError("general section is not a list".to_string())
            })?;
            general_items.insert(1, thickness);
            patches.replace_raw(general_node.span, new_general.to_string());
        }
    }

    Ok(patches)
}

fn stackup_thickness_iu(stackup: &Stackup) -> Option<PcbIu> {
    stackup.kicad_board_thickness().and_then(PcbIu::from_mm)
}

#[cfg(test)]
mod tests {
    use super::{build_stackup_patchset, stackup_thickness_iu, PcbIu};
    use pcb_zen_core::lang::stackup::{CopperRole, DielectricForm, Layer, Stackup};

    #[test]
    fn stackup_thickness_iu_rounds_like_kicad() {
        let stackup = Stackup {
            materials: None,
            silk_screen_color: None,
            solder_mask_color: None,
            layers: Some(vec![
                Layer::Copper {
                    thickness: 0.0150004,
                    role: CopperRole::Signal,
                },
                Layer::Dielectric {
                    thickness: 1.5750005,
                    material: "FR4".to_string(),
                    form: DielectricForm::Core,
                },
                Layer::Copper {
                    thickness: 0.0150004,
                    role: CopperRole::Signal,
                },
            ]),
            copper_finish: None,
        };

        // Core is 15000.4 + 1575000.5 + 15000.4 -> 1_605_001 IU after per-layer rounding,
        // plus fixed 2 * 0.01 mm solder mask = 20_000 IU.
        assert_eq!(stackup_thickness_iu(&stackup), Some(PcbIu(1_625_001)));
    }

    #[test]
    fn format_pcb_internal_units_matches_kicad_style() {
        assert_eq!(PcbIu(1_606_200).to_kicad_mm_text(), "1.6062");
        assert_eq!(PcbIu(1_600_000).to_kicad_mm_text(), "1.6");
        assert_eq!(PcbIu(2_000_000).to_kicad_mm_text(), "2");
        assert_eq!(PcbIu(-1_234_568).to_kicad_mm_text(), "-1.234568");
    }

    #[test]
    fn build_stackup_patchset_preserves_unrelated_numeric_lexemes() {
        let input = r#"(kicad_pcb
	(version 20240101)
	(generator "pcbnew")
	(general
		(thickness 1.7062)
		(legacy_teardrops no)
	)
	(layers
		(0 "F.Cu" signal)
		(2 "B.Cu" signal)
	)
	(setup
		(stackup (old yes))
		(pcbplotparams
			(dashed_line_dash_ratio 12.000000)
			(dashed_line_gap_ratio 3.000000)
			(hpglpendiameter 15.000000)
		)
	)
)"#;

        let board = pcb_sexpr::parse(input).unwrap();
        let layers = pcb_sexpr::parse(r#"(layers (0 "F.Cu" signal) (2 "B.Cu" signal))"#).unwrap();
        let stackup = pcb_sexpr::parse(
            r#"(stackup
                (layer "F.Mask" (type "Top Solder Mask") (thickness 0.01))
                (layer "F.Cu" (type "copper") (thickness 0.035))
                (layer "dielectric 1" (type "core") (thickness 1.5312) (material "FR4"))
                (layer "B.Cu" (type "copper") (thickness 0.02))
                (layer "B.Mask" (type "Bottom Solder Mask") (thickness 0.01))
            )"#,
        )
        .unwrap();

        let patches =
            build_stackup_patchset(&board, &layers, &stackup, Some(PcbIu(1_606_200))).unwrap();
        let mut out = Vec::new();
        patches.write_to(input, &mut out).unwrap();
        let out = String::from_utf8(out).unwrap();

        assert!(out.contains("(dashed_line_dash_ratio 12.000000)"));
        assert!(out.contains("(dashed_line_gap_ratio 3.000000)"));
        assert!(out.contains("(hpglpendiameter 15.000000)"));
        assert!(out.contains("(thickness 1.6062)"));
    }
}
