use anyhow::Context;
use log::{debug, info};
use pcb_sch::{AttributeValue, InstanceKind, Schematic, ATTR_LAYOUT_PATH};
use pcb_zen_core::diagnostics::Diagnostic;
use pcb_zen_core::lang::stackup::{
    ApproxEq, BoardConfig, BoardConfigError, NetClass, Stackup, StackupError, THICKNESS_EPS,
};
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};
use starlark::errors::EvalSeverity;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use thiserror::Error;

use pcb_kicad::PythonScriptBuilder;
use pcb_sch::kicad_netlist::{format_footprint, write_fp_lib_table};

mod moved;
pub use moved::compute_moved_paths_patches;

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
        let kind_short = self.kind.rsplit('.').next().unwrap_or(&self.kind);
        let body = match &self.reference {
            Some(ref_des) => format!("[{}] {}: {}", kind_short, ref_des, self.body),
            None => format!("[{}] {}", kind_short, self.body),
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

/// Apply moved() path renames to a PCB file
fn apply_moved_paths(
    pcb_path: &Path,
    moved_paths: &HashMap<String, String>,
    dry_run: bool,
    diagnostics: &mut pcb_zen_core::Diagnostics,
) -> anyhow::Result<()> {
    let pcb_path_str = pcb_path.to_string_lossy();
    let pcb_content = fs::read_to_string(pcb_path)
        .with_context(|| format!("Failed to read PCB file: {}", pcb_path.display()))?;
    let board = pcb_sexpr::parse(&pcb_content)
        .with_context(|| format!("Failed to parse PCB file: {}", pcb_path.display()))?;

    let (patches, renames) = compute_moved_paths_patches(&board, moved_paths);

    if renames.is_empty() {
        return Ok(());
    }

    if dry_run {
        for (old_path, new_path) in &renames {
            diagnostics.diagnostics.push(Diagnostic::categorized(
                &pcb_path_str,
                &format!(
                    "moved(\"{}\", \"{}\") would rename paths in layout (run without --check to apply)",
                    old_path, new_path
                ),
                "layout.moved",
                EvalSeverity::Warning,
            ));
        }
    } else {
        let tmp_path = pcb_path.with_extension("kicad_pcb.tmp");
        let file = fs::File::create(&tmp_path)
            .with_context(|| format!("Failed to create temp file: {}", tmp_path.display()))?;
        let mut writer = std::io::BufWriter::new(file);

        patches
            .write_to(&pcb_content, &mut writer)
            .with_context(|| format!("Failed to write patched PCB: {}", tmp_path.display()))?;

        writer
            .flush()
            .with_context(|| format!("Failed to flush patched PCB: {}", tmp_path.display()))?;

        fs::rename(&tmp_path, pcb_path)
            .with_context(|| format!("Failed to rename temp file to: {}", pcb_path.display()))?;

        for (old_path, new_path) in &renames {
            diagnostics.diagnostics.push(Diagnostic::categorized(
                &pcb_path_str,
                &format!("moved \"{}\" → \"{}\"", old_path, new_path),
                "layout.moved",
                EvalSeverity::Advice,
            ));
        }
    }
    Ok(())
}

/// Detect stale paths in the layout that should have been renamed by moved().
///
/// This runs after sync to catch cases where content was introduced during sync
/// (e.g., from child module layouts) and bypassed the pre-sync moved() patching.
fn detect_stale_moved_paths(
    pcb_path: &Path,
    moved_paths: &HashMap<String, String>,
    diagnostics: &mut pcb_zen_core::Diagnostics,
) -> anyhow::Result<()> {
    let pcb_path_str = pcb_path.to_string_lossy();
    let pcb_content = fs::read_to_string(pcb_path)
        .with_context(|| format!("Failed to read PCB file: {}", pcb_path.display()))?;
    let board = pcb_sexpr::parse(&pcb_content)
        .with_context(|| format!("Failed to parse PCB file: {}", pcb_path.display()))?;

    // Reuse compute_moved_paths_patches: any renames it would make are stale paths
    let (_, renames) = compute_moved_paths_patches(&board, moved_paths);

    if !renames.is_empty() {
        let examples: Vec<_> = renames
            .iter()
            .take(3)
            .map(|(old, _)| old.as_str())
            .collect();
        let example_str = examples
            .iter()
            .map(|s| format!("\"{}\"", s))
            .collect::<Vec<_>>()
            .join(", ");
        let more = if renames.len() > 3 {
            format!(" (and {} more)", renames.len() - 3)
        } else {
            String::new()
        };

        diagnostics.diagnostics.push(Diagnostic::categorized(
            &pcb_path_str,
            &format!(
                "moved() did not rename all paths: {}{} still exist in layout. \
                 This may indicate content was introduced during sync and bypassed moved() patching.",
                example_str, more
            ),
            "layout.moved.stale",
            EvalSeverity::Warning,
        ));
    }

    Ok(())
}

/// Run the Python layout sync script
fn run_sync_script(
    paths: &LayoutPaths,
    dry_run: bool,
    sync_board_config: bool,
    board_config_path: Option<&str>,
) -> anyhow::Result<()> {
    let script = include_str!("scripts/update_layout_file.py");
    let mut builder = PythonScriptBuilder::new(script)
        .arg("-j")
        .arg(paths.json_netlist.to_str().unwrap())
        .arg("-o")
        .arg(paths.pcb.to_str().unwrap())
        .arg("--diagnostics")
        .arg(paths.diagnostics.to_str().unwrap());

    if dry_run {
        builder = builder.arg("--dry-run");
    } else {
        builder = builder
            .arg("-s")
            .arg(paths.snapshot.to_str().unwrap())
            .arg("--sync-board-config")
            .arg(sync_board_config.to_string());

        if let Some(config_path) = board_config_path {
            builder = builder.arg("--board-config").arg(config_path);
        }
    }

    let log_file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&paths.log)?;

    builder.log_file(log_file).run()
}

/// Process a schematic and generate/update its layout files
///
/// When `dry_run` is false (normal mode):
/// 1. Extract the layout path from the schematic's root instance attributes
/// 2. Create the layout directory if it doesn't exist
/// 3. Generate/update the netlist file
/// 4. Write the footprint library table
/// 5. Create or update the KiCad PCB file
///
/// When `dry_run` is true (check mode):
/// - Requires PCB file to already exist
/// - Runs diagnostics without modifying the board
/// - Skips directory creation, netlist writing, and post-processing
pub fn process_layout(
    schematic: &Schematic,
    source_path: &Path,
    sync_board_config: bool,
    use_temp_dir: bool,
    dry_run: bool,
    diagnostics: &mut pcb_zen_core::Diagnostics,
) -> Result<Option<LayoutResult>, LayoutError> {
    // Resolve layout directory
    let layout_dir = if use_temp_dir {
        // Create a temporary directory and keep it (prevent cleanup on drop)
        tempfile::Builder::new()
            .prefix("pcb-layout-")
            .tempdir()
            .expect("Failed to create temporary directory")
            .keep()
    } else {
        match utils::resolve_layout_dir(schematic, source_path) {
            Some(path) => path,
            None => return Ok(None),
        }
    };

    let paths = utils::get_layout_paths(&layout_dir);

    // In dry-run mode, require PCB file to exist
    if dry_run && !paths.pcb.exists() {
        return Ok(None);
    }

    // Only create directories and write files in normal mode
    if !dry_run {
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
    }

    // Always write JSON netlist (needed by Python script)
    let json_content = schematic
        .to_json()
        .context("Failed to serialize schematic to JSON")?;
    fs::write(&paths.json_netlist, json_content).with_context(|| {
        format!(
            "Failed to write JSON netlist: {}",
            paths.json_netlist.display()
        )
    })?;

    // Extract board config (only used in normal mode)
    let board_config = if !dry_run {
        utils::extract_board_config(schematic)
    } else {
        None
    };

    // Write board config for Python script if it exists
    let board_config_path = board_config.as_ref().and_then(|config| {
        serde_json::to_string(config).ok().and_then(|json| {
            fs::write(&paths.board_config, json).ok()?;
            Some(paths.board_config.to_str().unwrap().to_string())
        })
    });

    let pcb_exists = paths.pcb.exists();
    debug!(
        "{} layout file: {}",
        if dry_run {
            "Checking"
        } else if pcb_exists {
            "Updating"
        } else {
            "Creating"
        },
        paths.pcb.display()
    );

    // Check for moved() paths that can't be applied to submodule layouts (always warn)
    let pcb_path_str = paths.pcb.to_string_lossy();
    for warning in check_submodule_moved_paths(schematic) {
        diagnostics.diagnostics.push(Diagnostic::categorized(
            &pcb_path_str,
            &warning,
            "layout.moved",
            EvalSeverity::Warning,
        ));
    }

    // Apply moved() path renames before sync (only if PCB exists and has renames)
    if pcb_exists && !schematic.moved_paths.is_empty() {
        apply_moved_paths(&paths.pcb, &schematic.moved_paths, dry_run, diagnostics)?;
    }

    // Run the Python sync script
    run_sync_script(
        &paths,
        dry_run,
        sync_board_config,
        board_config_path.as_deref(),
    )?;

    // Apply board config (stackup + netclass patterns) - only in normal mode
    if !dry_run && sync_board_config {
        if let Some(ref config) = board_config {
            if let Some(ref stackup) = config.stackup {
                patch_stackup_if_needed(&paths.pcb, stackup)?;
            }

            let assignments = build_netclass_assignments(schematic, config.netclasses());
            if !assignments.is_empty() {
                patch_netclass_patterns(&paths.pcb, &assignments)?;
            }
        }
    }

    // Add sync diagnostics from JSON file
    if paths.diagnostics.exists() {
        let sync_diagnostics = LayoutSyncDiagnostics::from_file(&paths.diagnostics)?;
        for sync_diag in sync_diagnostics.diagnostics {
            diagnostics
                .diagnostics
                .push(sync_diag.to_diagnostic(&pcb_path_str));
        }
    }

    // Post-sync: detect stale paths that should have been renamed by moved()
    if !dry_run && !schematic.moved_paths.is_empty() && paths.pcb.exists() {
        detect_stale_moved_paths(&paths.pcb, &schematic.moved_paths, diagnostics)?;
    }

    Ok(Some(LayoutResult {
        source_file: source_path.to_path_buf(),
        layout_dir,
        pcb_file: paths.pcb.clone(),
        netlist_file: paths.netlist,
        snapshot_file: paths.snapshot,
        log_file: paths.log,
        diagnostics_file: paths.diagnostics,
        created: !pcb_exists && !dry_run,
    }))
}

/// Utility functions
pub mod utils {
    use super::*;
    use pcb_sch::InstanceKind;
    use std::collections::HashMap;

    /// Extract layout path from schematic's root instance attributes
    pub fn extract_layout_path(schematic: &Schematic) -> Option<PathBuf> {
        let root_ref = schematic.root_ref.as_ref()?;
        let root = schematic.instances.get(root_ref)?;
        let layout_path_str = root
            .attributes
            .get(ATTR_LAYOUT_PATH)
            .and_then(|v| v.string())?;
        Some(PathBuf::from(layout_path_str))
    }

    /// Resolve layout directory from schematic, converting relative paths to absolute.
    /// Returns None if the schematic has no layout_path attribute.
    pub fn resolve_layout_dir(schematic: &Schematic, source_path: &Path) -> Option<PathBuf> {
        let layout_path = extract_layout_path(schematic)?;

        Some(if layout_path.is_relative() {
            source_path
                .parent()
                .unwrap_or(Path::new("."))
                .join(&layout_path)
        } else {
            layout_path
        })
    }

    /// Get all the file paths that would be generated for a layout
    pub fn get_layout_paths(layout_dir: &Path) -> LayoutPaths {
        let temp_dir = tempfile::tempdir().expect("Failed to create temp directory for netlist");
        let json_netlist = temp_dir.path().join("netlist.json");
        let board_config = temp_dir.path().join("board_config.json");
        let diagnostics = temp_dir.path().join("diagnostics.layout.json");
        LayoutPaths {
            netlist: layout_dir.join("default.net"),
            pcb: layout_dir.join("layout.kicad_pcb"),
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
                if let (_, Some((lib_name, dir))) = format_footprint(fp_attr) {
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
                    pv.value.to_f64()
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
                    pv.value.to_f64()
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
                    let error = ((imp - target) / target).abs();
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
                    let error = ((imp - target) / target).abs();
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

/// Apply stackup configuration if it differs from existing PCB file
fn patch_stackup_if_needed(pcb_path: &Path, zen_stackup: &Stackup) -> Result<(), LayoutError> {
    // Read current PCB file
    let pcb_content = fs::read_to_string(pcb_path).map_err(|e| {
        LayoutError::StackupPatchingError(format!("Failed to read PCB file: {}", e))
    })?;

    // Parse existing stackup from PCB file
    let existing_stackup = Stackup::from_kicad_pcb(&pcb_content)?;

    // Compare stackups - only patch if they're different
    let needs_update = match existing_stackup {
        Some(existing) => {
            let equivalent = zen_stackup.approx_eq(&existing, THICKNESS_EPS);
            if !equivalent {
                debug!("Zen stackup: {:?}", zen_stackup);
                debug!("Existing stackup: {:?}", existing);
            }
            !equivalent
        }
        None => {
            debug!("No existing stackup found in PCB file");
            true // No existing stackup, so we need to add it
        }
    };

    if !needs_update {
        debug!("Stackup configuration matches, skipping update");
        return Ok(());
    }

    info!("Updating stackup configuration in {}", pcb_path.display());

    // Generate new S-expressions (using default user layers)
    let layers_sexpr = zen_stackup.generate_layers_sexpr(4);
    let stackup_sexpr = zen_stackup.generate_stackup_sexpr();

    // Use surgical string replacement to avoid parsing issues with hex numbers
    let mut updated_content = pcb_content;
    updated_content = replace_section_in_pcb_content(&updated_content, "layers", &layers_sexpr)?;
    updated_content = replace_section_in_pcb_content(&updated_content, "stackup", &stackup_sexpr)?;

    // Write updated content back to file
    fs::write(pcb_path, updated_content).map_err(|e| {
        LayoutError::StackupPatchingError(format!("Failed to write updated PCB file: {}", e))
    })?;

    info!("Successfully updated stackup configuration");
    Ok(())
}

/// Replace a section in KiCad PCB content using careful string matching
fn replace_section_in_pcb_content(
    content: &str,
    section_name: &str,
    new_section: &str,
) -> Result<String, LayoutError> {
    // Find the section by parsing just enough to locate it
    let section_start = find_section_start(content, section_name)?;

    if let Some(start_pos) = section_start {
        let end_pos = find_matching_paren(content, start_pos)?;

        // Replace the section with the new content
        let mut result = String::with_capacity(content.len() + new_section.len());
        result.push_str(&content[..start_pos]);
        result.push_str(new_section);
        result.push_str(&content[end_pos + 1..]);
        Ok(result)
    } else {
        // Section doesn't exist, need to add it
        add_section_to_pcb_content(content, section_name, new_section)
    }
}

/// Find the start position of a section in PCB content
fn find_section_start(content: &str, section_name: &str) -> Result<Option<usize>, LayoutError> {
    let pattern = format!("({}", section_name);
    let mut pos = 0;

    while let Some(found) = content[pos..].find(&pattern) {
        let abs_pos = pos + found;

        // Check if this is a word boundary (not part of a larger identifier)
        let next_char_pos = abs_pos + pattern.len();
        if next_char_pos < content.len() {
            let next_char = content.chars().nth(next_char_pos).unwrap();
            if next_char.is_whitespace() || next_char == '\n' || next_char == '\t' {
                return Ok(Some(abs_pos));
            }
        } else {
            return Ok(Some(abs_pos));
        }

        pos = abs_pos + 1;
    }

    Ok(None)
}

/// Find the matching closing parenthesis for an opening parenthesis
fn find_matching_paren(content: &str, start_pos: usize) -> Result<usize, LayoutError> {
    let mut depth = 0;
    let mut in_string = false;
    let mut escaped = false;

    let chars: Vec<char> = content.chars().collect();

    for (i, &ch) in chars.iter().enumerate().skip(start_pos) {
        if escaped {
            escaped = false;
            continue;
        }

        match ch {
            '\\' if in_string => escaped = true,
            '"' => in_string = !in_string,
            '(' if !in_string => depth += 1,
            ')' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    return Ok(i);
                }
            }
            _ => {}
        }
    }

    Err(LayoutError::StackupPatchingError(
        "Could not find matching closing parenthesis".to_string(),
    ))
}

/// Add a new section to PCB content
fn add_section_to_pcb_content(
    content: &str,
    section_name: &str,
    new_section: &str,
) -> Result<String, LayoutError> {
    match section_name {
        "layers" => {
            // Add after general section
            if let Some(general_start) = find_section_start(content, "general")? {
                let general_end = find_matching_paren(content, general_start)?;
                let insert_pos = general_end + 1;

                let mut result = String::with_capacity(content.len() + new_section.len() + 10);
                result.push_str(&content[..insert_pos]);
                result.push('\n');
                result.push('\t');
                result.push_str(new_section);
                result.push_str(&content[insert_pos..]);
                Ok(result)
            } else {
                Err(LayoutError::StackupPatchingError(
                    "Could not find general section for layers insertion".to_string(),
                ))
            }
        }
        "stackup" => {
            // Add within setup section
            if let Some(setup_start) = find_section_start(content, "setup")? {
                let setup_end = find_matching_paren(content, setup_start)?;
                let insert_pos = setup_end; // Before closing paren

                let mut result = String::with_capacity(content.len() + new_section.len() + 20);
                result.push_str(&content[..insert_pos]);
                result.push('\n');
                result.push_str("\t\t");
                result.push_str(new_section);
                result.push('\n');
                result.push('\t');
                result.push_str(&content[insert_pos..]);
                Ok(result)
            } else {
                Err(LayoutError::StackupPatchingError(
                    "Could not find setup section for stackup insertion".to_string(),
                ))
            }
        }
        _ => Err(LayoutError::StackupPatchingError(format!(
            "Unknown section type: {}",
            section_name
        ))),
    }
}
