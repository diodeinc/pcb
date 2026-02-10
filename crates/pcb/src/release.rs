use anyhow::{Context, Result};
use clap::ValueEnum;
use log::{debug, warn};
use pcb_kicad::{KiCadCliBuilder, PythonScriptBuilder};
use pcb_layout::utils as layout_utils;
use pcb_ui::{Colorize, Spinner, Style, StyledText};

use crate::bom::generate_bom_with_fallback;
use pcb_zen::workspace::{get_workspace_info, WorkspaceInfoExt};
use pcb_zen::{PackageClosure, ResolutionResult};
use pcb_zen_core::DefaultFileProvider;
use pcb_zen_core::{EvalOutput, WithDiagnostics};

use pcb_zen::WorkspaceInfo;

use inquire::Confirm;
use std::collections::HashSet;
use std::fs;
use std::io::{BufWriter, Write};
use std::time::Instant;

use chrono::Utc;
use std::path::{Path, PathBuf};

use zip::{write::FileOptions, ZipWriter};

use pcb_zen::{copy_dir_all, git};

const RELEASE_SCHEMA_VERSION: &str = "1";

/// Serialize a value to RFC 8785 canonical JSON (sorted keys, consistent formatting).
fn to_canonical_json<T: serde::Serialize>(value: &T) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    let mut ser =
        serde_json::Serializer::with_formatter(&mut buf, canon_json::CanonicalFormatter::new());
    serde::Serialize::serialize(value, &mut ser)?;
    Ok(buf)
}

#[derive(ValueEnum, Debug, Clone, PartialEq)]
#[value(rename_all = "lowercase")]
pub enum ArtifactType {
    Drc,
    Bom,
    Gerbers,
    Cpl,
    Assembly,
    Odb,
    Ipc2581,
    Step,
    Vrml,
    Glb,
    Svg,
}

impl ArtifactType {
    /// Get the human-readable task name for this artifact type
    fn task_name(&self) -> &'static str {
        match self {
            ArtifactType::Drc => "Running KiCad DRC checks",
            ArtifactType::Bom => "Generating design BOM",
            ArtifactType::Gerbers => "Generating gerber files",
            ArtifactType::Cpl => "Generating pick-and-place file",
            ArtifactType::Assembly => "Generating assembly drawings",
            ArtifactType::Odb => "Generating ODB++ files",
            ArtifactType::Ipc2581 => "Generating IPC-2581 file",
            ArtifactType::Step => "Generating STEP model",
            ArtifactType::Vrml => "Generating VRML model",
            ArtifactType::Glb => "Generating GLB model",
            ArtifactType::Svg => "Generating SVG rendering",
        }
    }

    /// Get the task function for this artifact type
    fn task_fn(&self) -> TaskFn {
        match self {
            ArtifactType::Drc => run_kicad_drc,
            ArtifactType::Bom => generate_design_bom,
            ArtifactType::Gerbers => generate_gerbers,
            ArtifactType::Cpl => generate_cpl,
            ArtifactType::Assembly => generate_assembly_drawings,
            ArtifactType::Odb => generate_odb,
            ArtifactType::Ipc2581 => generate_ipc2581,
            ArtifactType::Step => generate_step_model,
            ArtifactType::Vrml => generate_vrml_model,
            ArtifactType::Glb => generate_glb_model,
            ArtifactType::Svg => generate_svg_rendering,
        }
    }

    /// Whether this artifact type requires a layout directory to generate
    fn requires_layout(&self) -> bool {
        match self {
            ArtifactType::Bom => false, // BOM is generated from schematic
            _ => true,                  // All other artifacts require KiCad layout files
        }
    }
}

/// All information gathered during the release preparation phase
#[derive(Debug, Clone)]
struct ReleaseLayout {
    /// Path to the KiCad project file, relative to the workspace root.
    kicad_pro_rel: PathBuf,
}

impl ReleaseLayout {
    fn layout_dir_rel(&self) -> &Path {
        self.kicad_pro_rel.parent().unwrap_or(Path::new(""))
    }
}

struct ReleaseInfo {
    config: WorkspaceInfo,
    zen_path: PathBuf,
    board_name: String,
    version: String,
    git_hash: String,
    staging_dir: PathBuf,
    layout: Option<ReleaseLayout>,
    schematic: pcb_sch::Schematic,
    output_dir: PathBuf,
    output_name: String,
    suppress: Vec<String>,
    resolution: ResolutionResult,
    closure: Option<PackageClosure>,
    allow_errors: bool,
}

impl ReleaseInfo {
    fn workspace_root(&self) -> &Path {
        &self.config.root
    }

    fn board_display_name(&self) -> String {
        self.config
            .board_name_for_zen(&self.zen_path)
            .unwrap_or_else(|| {
                self.zen_path
                    .file_stem()
                    .unwrap()
                    .to_string_lossy()
                    .to_string()
            })
    }

    fn has_layout(&self) -> bool {
        self.layout.is_some()
    }

    fn staged_layout_dir(&self) -> Option<PathBuf> {
        self.layout
            .as_ref()
            .map(|l| self.staging_dir.join("src").join(l.layout_dir_rel()))
    }

    fn staged_kicad_files(&self) -> Option<layout_utils::KiCadLayoutFiles> {
        let layout = self.layout.as_ref()?;
        Some(layout_utils::KiCadLayoutFiles {
            kicad_pro: self.staging_dir.join("src").join(&layout.kicad_pro_rel),
        })
    }

    fn staged_pcb_path(&self) -> Option<PathBuf> {
        self.staged_kicad_files().map(|f| f.kicad_pcb())
    }
}

type TaskFn = fn(&ReleaseInfo, &Spinner) -> Result<()>;

const BASE_TASKS: &[(&str, TaskFn)] = &[
    ("Copying source files and dependencies", copy_sources),
    ("Generating netlist from staged sources", validate_build),
    ("Substituting version variables", substitute_variables),
];

/// All manufacturing artifacts in the order they should be generated
const MANUFACTURING_ARTIFACTS: &[ArtifactType] = &[
    ArtifactType::Drc, // Run DRC checks first, before generating any manufacturing files
    ArtifactType::Bom,
    ArtifactType::Gerbers,
    ArtifactType::Cpl,
    ArtifactType::Assembly,
    ArtifactType::Odb,
    ArtifactType::Ipc2581,
    ArtifactType::Step,
    ArtifactType::Vrml,
    ArtifactType::Glb,
    ArtifactType::Svg,
];

const FINALIZATION_TASKS: &[(&str, TaskFn)] = &[
    ("Writing release metadata", write_metadata),
    ("Creating release archive", zip_release),
];

/// Get manufacturing tasks as (name, function) pairs, filtered by exclusions and layout availability
fn get_manufacturing_tasks(
    excluded: &[ArtifactType],
    has_layout: bool,
) -> Vec<(&'static str, TaskFn)> {
    MANUFACTURING_ARTIFACTS
        .iter()
        .filter(|artifact| !excluded.contains(artifact))
        .filter(|artifact| has_layout || !artifact.requires_layout())
        .map(|artifact| (artifact.task_name(), artifact.task_fn()))
        .collect()
}

/// Format cumulative time as MM:SS
fn format_cumulative_time(seconds: f64) -> String {
    let total_secs = seconds as u64;
    let mins = total_secs / 60;
    let secs = total_secs % 60;
    format!("{:02}:{:02}", mins, secs)
}

/// Format task duration as seconds or minutes depending on the value
fn format_task_duration_value(seconds: f64) -> String {
    if seconds >= 60.0 {
        format!("{:4.1}m", seconds / 60.0)
    } else {
        format!("{:4.1}s", seconds)
    }
}

/// Format a task duration (dimmed if < 60s, red if >= 60s)
fn format_task_duration(seconds: f64) -> colored::ColoredString {
    let formatted = format_task_duration_value(seconds);
    if seconds >= 60.0 {
        formatted.red()
    } else {
        formatted.dimmed()
    }
}

fn confirm_continue_on_error(spinner: Option<&Spinner>, allow_errors: bool, message: &str) -> bool {
    if !allow_errors {
        return false;
    }

    let confirm = || {
        if crate::tty::is_interactive() {
            Confirm::new(message)
                .with_default(true)
                .prompt()
                .unwrap_or(false)
        } else {
            eprintln!("{message} Continuing despite errors.");
            true
        }
    };

    if let Some(spinner) = spinner {
        spinner.suspend(confirm)
    } else {
        confirm()
    }
}

/// Execute a list of tasks with proper error handling and UI feedback
fn execute_tasks(info: &ReleaseInfo, tasks: &[(&str, TaskFn)], start_time: Instant) -> Result<()> {
    for (name, task) in tasks {
        let spinner = Spinner::builder(*name).start();

        let task_start = Instant::now();
        task(info, &spinner)?;
        let task_duration = task_start.elapsed().as_secs_f64();
        let cumulative_duration = start_time.elapsed().as_secs_f64();

        spinner.finish();
        eprintln!(
            "{}: {} ({}) {name}",
            format_cumulative_time(cumulative_duration),
            "✓".green(),
            format_task_duration(task_duration)
        );
    }
    Ok(())
}

/// Build a release for a board file. Used by `pcb publish --board`.
/// If version is provided (e.g. "v1.2.3"), uses that. Otherwise uses git commit hash.
/// Takes pre-resolved workspace info to avoid duplicate resolution.
/// Returns the path to the created release zip file.
pub fn build_board_release(
    mut workspace: WorkspaceInfo,
    zen_path: PathBuf,
    board_name: String,
    suppress: Vec<String>,
    version: Option<String>,
    exclude: Vec<ArtifactType>,
    allow_errors: bool,
) -> Result<PathBuf> {
    let start_time = Instant::now();

    let release_info = {
        let info_spinner = Spinner::builder("Gathering release information").start();

        // Require a lockfile for release - running in locked mode without one
        // would fail to resolve @stdlib and other implicit dependencies
        if workspace.lockfile.is_none() {
            anyhow::bail!(
                "No lockfile found. Run 'pcb build' or 'pcb layout' first to generate one.\n\
                 Release requires a lockfile to ensure reproducible builds."
            );
        }

        info_spinner.set_message("Resolving dependencies");
        let resolution = pcb_zen::resolve_dependencies(&mut workspace, false, true)?;

        // Find the package URL for this board
        let closure = workspace
            .package_url_for_zen(&zen_path)
            .map(|url| workspace.package_closure(&url, &resolution));

        info_spinner.set_message("Evaluating zen file");

        // Evaluate the zen file (still needed for schematic)
        // Pass resolution so Module() paths resolve correctly
        let eval_result = pcb_zen::eval(&zen_path, resolution.clone());

        let has_eval_errors = eval_result.diagnostics.has_errors();
        if has_eval_errors || eval_result.output.is_none() {
            info_spinner.suspend(|| {
                let mut diagnostics = eval_result.diagnostics.clone();
                let passes = crate::build::create_diagnostics_passes(&[], &[]);
                diagnostics.apply_passes(&passes);
            });
            if eval_result.output.is_none() {
                info_spinner.finish();
                anyhow::bail!("Evaluation failed");
            }
            if has_eval_errors {
                if !allow_errors {
                    info_spinner.finish();
                    anyhow::bail!("Evaluation failed");
                }
                if !confirm_continue_on_error(
                    Some(&info_spinner),
                    allow_errors,
                    "Evaluation completed with errors. Do you want to proceed anyway?",
                ) {
                    std::process::exit(1);
                }
            }
        }

        info_spinner.finish();

        let eval_output = eval_result.output.unwrap();

        // Get git hash for metadata
        let git_hash =
            git::rev_parse_head(&workspace.root).unwrap_or_else(|| "unknown".to_string());

        // Use provided version, or fall back to short git hash
        let version = version.unwrap_or_else(|| {
            git::rev_parse_short_head(&workspace.root).unwrap_or_else(|| "unknown".to_string())
        });

        // Create release staging directory in workspace root with flat structure
        let staging_dir = workspace
            .root
            .join(".pcb/releases")
            .join(format!("{}-{}", board_name, version));

        // Output directory and name use defaults
        let output_dir = workspace.root.join(".pcb/releases");
        let output_name = format!("{}-{}.zip", board_name, version);

        // Delete existing staging dir and recreate
        if staging_dir.exists() {
            debug!(
                "Removing existing staging directory: {}",
                staging_dir.display()
            );
            remove_dir_all_with_permissions(&staging_dir)?;
        }
        fs::create_dir_all(&staging_dir)?;

        let layout = match discover_layout_from_output(&zen_path, &eval_output)? {
            Some(discovered) => match discovered
                .kicad_files
                .kicad_pro
                .strip_prefix(&workspace.root)
            {
                Ok(kicad_pro_rel) => Some(ReleaseLayout {
                    kicad_pro_rel: kicad_pro_rel.to_path_buf(),
                }),
                Err(_) => {
                    warn!(
                        "Layout path {} is outside workspace root, ignoring",
                        discovered.layout_dir.display()
                    );
                    None
                }
            },
            None => None,
        };

        let schematic = eval_output.to_schematic()?;

        let info = ReleaseInfo {
            config: workspace,
            zen_path,
            board_name,
            version,
            git_hash,
            staging_dir,
            layout,
            schematic,
            output_dir,
            output_name,
            suppress,
            resolution,
            closure,
            allow_errors,
        };

        let elapsed = start_time.elapsed().as_secs_f64();
        eprintln!(
            "{}: {} ({}) Release information gathered",
            format_cumulative_time(elapsed),
            "✓".green(),
            format_task_duration(elapsed),
        );

        info
    };

    // Execute base tasks
    execute_tasks(&release_info, BASE_TASKS, start_time)?;

    // Execute manufacturing tasks
    let manufacturing_tasks = get_manufacturing_tasks(&exclude, release_info.has_layout());
    execute_tasks(&release_info, &manufacturing_tasks, start_time)?;

    // Execute finalization tasks
    execute_tasks(&release_info, FINALIZATION_TASKS, start_time)?;

    // Calculate archive path
    let zip_path = archive_zip_path(&release_info);

    eprintln!(
        "{} {}",
        "✓".green(),
        format!("Release {} staged successfully", release_info.version).bold()
    );
    display_release_info(&release_info);

    eprintln!(
        "Archive: {}",
        zip_path.display().to_string().with_style(Style::Cyan)
    );

    Ok(zip_path)
}

/// Display release information summary
fn display_release_info(info: &ReleaseInfo) {
    eprintln!(
        "{}",
        "Release Summary".to_string().with_style(Style::Blue).bold()
    );
    let mut table = comfy_table::Table::new();
    table
        .load_preset(comfy_table::presets::UTF8_BORDERS_ONLY)
        .set_content_arrangement(comfy_table::ContentArrangement::Dynamic);

    table.add_row(vec!["Release Type", "Full Release"]);
    table.add_row(vec!["Version", &info.version]);
    table.add_row(vec![
        "Git Hash",
        &info.git_hash[..8.min(info.git_hash.len())],
    ]);

    let zen_file = info
        .zen_path
        .strip_prefix(info.workspace_root())
        .unwrap_or(&info.zen_path)
        .display()
        .to_string();
    table.add_row(vec!["Zen File", &zen_file]);

    let staging_dir = info
        .staging_dir
        .strip_prefix(info.workspace_root())
        .unwrap_or(&info.staging_dir)
        .display()
        .to_string();
    table.add_row(vec!["Staging Dir", &staging_dir]);

    table.add_row(vec!["Platform", std::env::consts::OS]);
    table.add_row(vec!["Architecture", std::env::consts::ARCH]);
    table.add_row(vec!["CLI Version", env!("CARGO_PKG_VERSION")]);
    table.add_row(vec!["KiCad Version", &get_kicad_version()]);

    let user = std::env::var("USER").unwrap_or_else(|_| "unknown".to_string());
    table.add_row(vec!["Created By", &user]);

    let timestamp = Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string();
    table.add_row(vec!["Created At", &timestamp]);

    println!("{table}");
}

/// Get KiCad CLI version
fn get_kicad_version() -> String {
    KiCadCliBuilder::new()
        .command("version")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|version| version.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Get git remotes as a map of name -> url
fn get_git_remotes(path: &Path) -> serde_json::Value {
    let mut remotes = serde_json::Map::new();
    let Some(remote_list) = git::run_output_opt(path, &["remote"]) else {
        return serde_json::Value::Object(remotes);
    };

    for name in remote_list.lines() {
        if let Ok(url) = git::get_remote_url_for(path, name) {
            remotes.insert(name.to_string(), serde_json::Value::String(url));
        }
    }

    serde_json::Value::Object(remotes)
}

/// Create the metadata JSON object (shared between display and file writing)
fn create_metadata_json(info: &ReleaseInfo) -> serde_json::Value {
    let rfc3339_timestamp = Utc::now().to_rfc3339();

    // Get board description if available
    let board_description = info
        .config
        .board_info_for_zen(&info.zen_path)
        .map(|b| b.description)
        .filter(|d| !d.is_empty());

    let mut release_obj = serde_json::json!({
        "schema_version": RELEASE_SCHEMA_VERSION,
        "board_name": info.board_name,
        "git_version": info.version,
        "created_at": rfc3339_timestamp,
        "zen_file": info.zen_path.strip_prefix(info.workspace_root()).expect("zen_file must be within workspace_root"),
        "workspace_root": info.workspace_root(),
        "staging_directory": info.staging_dir
    });

    // Add layout_path if present
    if let Some(ref layout) = info.layout {
        release_obj["layout_path"] = serde_json::json!(layout.layout_dir_rel());
    }

    // Add description if present
    if let Some(desc) = board_description {
        release_obj["description"] = serde_json::json!(desc);
    }

    // Get git info
    let workspace_root = info.workspace_root();
    let branch = git::rev_parse_abbrev_ref_head(workspace_root);
    let remotes = get_git_remotes(workspace_root);

    let mut git_obj = serde_json::json!({
        "describe": info.version.clone(),
        "hash": info.git_hash.clone(),
        "workspace": workspace_root.display().to_string(),
        "remotes": remotes
    });

    if let Some(branch) = branch {
        git_obj["branch"] = serde_json::Value::String(branch);
    }

    serde_json::json!({
        "release": release_obj,
        "system": {
            "user": std::env::var("USER").unwrap_or_else(|_| "unknown".to_string()),
            "platform": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "cli_version": env!("CARGO_PKG_VERSION"),
            "kicad_version": get_kicad_version()
        },
        "git": git_obj
    })
}

/// Extract layout path from zen evaluation result (public for bom.rs)
/// Returns None if no layout_path property exists or the layout directory doesn't exist
pub fn extract_layout_path(
    zen_path: &Path,
    eval: &WithDiagnostics<EvalOutput>,
) -> Result<Option<PathBuf>> {
    let Some(output) = eval.output.as_ref() else {
        return Ok(None);
    };
    Ok(discover_layout_from_output(zen_path, output)?.map(|d| d.layout_dir))
}

struct DiscoveredLayout {
    layout_dir: PathBuf,
    kicad_files: layout_utils::KiCadLayoutFiles,
}

/// Discover layout info from zen evaluation output.
/// Returns None if no layout_path property exists or the layout directory doesn't contain KiCad files.
fn discover_layout_from_output(
    zen_path: &Path,
    output: &EvalOutput,
) -> Result<Option<DiscoveredLayout>> {
    let properties = output.sch_module.properties();

    let Some(layout_path_value) = properties.get("layout_path") else {
        return Ok(None);
    };

    let layout_path_str = layout_path_value.to_string();
    let clean_path_str = layout_path_str.trim_matches('"');

    // Layout path is relative to the zen file's parent directory
    let Some(zen_parent_dir) = zen_path.parent() else {
        return Ok(None);
    };
    let layout_path = zen_parent_dir.join(clean_path_str);

    // Discover KiCad files (accept a single top-level .kicad_pro or .kicad_pcb).
    // If there are multiple candidates, treat it as an error (ambiguous).
    let discovered = layout_utils::discover_kicad_files(&layout_path)?;
    if discovered.is_none() {
        if layout_path.exists() {
            warn!(
                "Layout directory {} exists but has no discoverable KiCad project/layout files, skipping layout tasks",
                layout_path.display()
            );
        } else {
            debug!(
                "Layout path {} does not exist, skipping layout tasks",
                layout_path.display()
            );
        }
        return Ok(None);
    }

    debug!(
        "Extracted layout path: {} -> {}",
        clean_path_str,
        layout_path.display()
    );
    Ok(Some(DiscoveredLayout {
        layout_dir: layout_path,
        kicad_files: discovered.unwrap(),
    }))
}

/// Copy source files and vendor dependencies
fn copy_sources(info: &ReleaseInfo, _spinner: &Spinner) -> Result<()> {
    let workspace_root = info.workspace_root();
    let src_dir = info.staging_dir.join("src");
    let vendor_dir = src_dir.join("vendor");

    fs::create_dir_all(&src_dir)?;

    // 1. Copy workspace root pcb.toml
    let root_pcb_toml = workspace_root.join("pcb.toml");
    if root_pcb_toml.exists() {
        fs::copy(&root_pcb_toml, src_dir.join("pcb.toml"))?;
    }

    // 2. Copy local workspace packages that the board depends on
    if let Some(closure) = &info.closure {
        // Precompute all package roots for nested package exclusion
        let all_pkg_roots: HashSet<PathBuf> = info
            .config
            .packages
            .values()
            .map(|p| workspace_root.join(&p.rel_path))
            .collect();

        for pkg_url in &closure.local_packages {
            if let Some(pkg) = info.config.packages.get(pkg_url) {
                let dest = src_dir.join(&pkg.rel_path);
                copy_dir_all(&pkg.dir(workspace_root), &dest, &all_pkg_roots)?;
                debug!("Copied package {} to {}", pkg_url, dest.display());
            }
        }
    }

    // 3. Vendor remote dependencies using vendor_deps with "**" pattern
    let result = pcb_zen::vendor_deps(
        &info.config,
        &info.resolution,
        &["**".to_string()],
        Some(&vendor_dir),
        true, // Always prune for release
    )?;
    debug!(
        "Vendored {} packages and {} assets",
        result.package_count, result.asset_count
    );

    // Copy pcb.sum lockfile if present
    let lockfile_src = workspace_root.join("pcb.sum");
    if lockfile_src.exists() {
        fs::copy(&lockfile_src, src_dir.join("pcb.sum"))?;
    }

    Ok(())
}

/// Ensure text variables are defined in .kicad_pro file
fn update_kicad_pro_text_variables(
    kicad_pro_path: &Path,
    version: &str,
    git_hash: &str,
    board_name: &str,
) -> Result<()> {
    // Read the existing .kicad_pro file
    let content = fs::read_to_string(kicad_pro_path).with_context(|| {
        format!(
            "Failed to read .kicad_pro file: {}",
            kicad_pro_path.display()
        )
    })?;

    // Parse as JSON
    let mut project: serde_json::Value = serde_json::from_str(&content).with_context(|| {
        format!(
            "Failed to parse .kicad_pro file as JSON: {}",
            kicad_pro_path.display()
        )
    })?;

    // Check if text variables already exist
    let text_vars = project.get("text_variables").and_then(|v| v.as_object());
    let needs_pcb_version = text_vars.is_none_or(|vars| !vars.contains_key("PCB_VERSION"));
    let needs_pcb_git_hash = text_vars.is_none_or(|vars| !vars.contains_key("PCB_GIT_HASH"));
    let needs_pcb_name = text_vars.is_none_or(|vars| !vars.contains_key("PCB_NAME"));

    // Only modify if we need to add missing variables
    if needs_pcb_version || needs_pcb_git_hash || needs_pcb_name {
        // Ensure text_variables object exists
        if project.get("text_variables").is_none() || !project["text_variables"].is_object() {
            project["text_variables"] = serde_json::json!({});
        }

        let text_vars = project["text_variables"].as_object_mut().unwrap();

        // Add missing variables with correct values
        text_vars.insert(
            "PCB_VERSION".to_string(),
            serde_json::Value::String(version.to_string()),
        );
        text_vars.insert(
            "PCB_GIT_HASH".to_string(),
            serde_json::Value::String(git_hash.to_string()),
        );
        text_vars.insert(
            "PCB_NAME".to_string(),
            serde_json::Value::String(board_name.to_string()),
        );

        // Write back to file with pretty formatting
        let updated_content = serde_json::to_string_pretty(&project)?;
        fs::write(kicad_pro_path, updated_content).with_context(|| {
            format!(
                "Failed to write updated .kicad_pro file: {}",
                kicad_pro_path.display()
            )
        })?;

        debug!(
            "Added missing text variables to: {}",
            kicad_pro_path.display()
        );
    } else {
        debug!(
            "Text variables already exist in: {}",
            kicad_pro_path.display()
        );
    }

    Ok(())
}

/// Substitute version, git hash and name variables in KiCad PCB files
fn substitute_variables(info: &ReleaseInfo, _spinner: &Spinner) -> Result<()> {
    let Some(kicad_files) = info.staged_kicad_files() else {
        debug!("No layout directory, skipping variable substitution");
        return Ok(());
    };

    // Determine display name of the board
    let board_name = info.board_display_name();

    // Use short hash (7 chars) for variable substitution
    let short_hash = &info.git_hash[..7.min(info.git_hash.len())];

    // First, update the .kicad_pro file to ensure text variables are defined
    let kicad_pro_path = kicad_files.kicad_pro.clone();
    update_kicad_pro_text_variables(&kicad_pro_path, &info.version, short_hash, &board_name)?;

    // Then update the .kicad_pcb file with the actual values
    let kicad_pcb_path = kicad_files.kicad_pcb();
    let script = format!(
        r#"
import sys
import pcbnew

# Load the board
board = pcbnew.LoadBoard(sys.argv[1])

# Get text variables
text_vars = board.GetProperties()

# Update variables
text_vars['PCB_VERSION'] = '{version}'
text_vars['PCB_GIT_HASH'] = '{git_hash}'
text_vars['PCB_NAME'] = '{board_name}'

# Save the board
board.Save(sys.argv[1])
print("Text variables updated successfully")
"#,
        version = info.version.replace('\'', "\\'"),
        git_hash = short_hash.replace('\'', "\\'"),
        board_name = board_name.replace('\'', "\\'")
    );

    PythonScriptBuilder::new(script)
        .arg(kicad_pcb_path.to_string_lossy())
        .run()?;
    debug!("Updated variables in: {}", kicad_pcb_path.display());
    Ok(())
}

/// Validate that the staged zen file can be built successfully
fn validate_build(info: &ReleaseInfo, spinner: &Spinner) -> Result<()> {
    // Calculate the zen file path in the staging directory
    let zen_file_rel = info
        .zen_path
        .strip_prefix(info.workspace_root())
        .context("Zen file must be within workspace root")?;
    let staged_src = info.staging_dir.join("src");
    let staged_zen_path = staged_src.join(zen_file_rel);

    debug!("Validating build of: {}", staged_zen_path.display());

    // Re-resolve dependencies on the staged sources
    // This is cleaner than remapping paths from the original resolution
    let mut staged_workspace = get_workspace_info(&DefaultFileProvider::new(), &staged_zen_path)?;
    // Staged sources have vendored deps, so run resolution in offline+locked mode
    let staged_resolution = pcb_zen::resolve_dependencies(&mut staged_workspace, true, true)?;

    // Use build function with offline mode but allow warnings
    // Suspend spinner during build to allow diagnostics to render properly
    let (has_errors, has_warnings, schematic) = spinner.suspend(|| {
        let mut has_errors = false;
        let mut has_warnings = false;

        // Export diagnostics to JSON for release artifacts
        let mut passes = crate::build::create_diagnostics_passes(&[], &[]);
        passes.push(Box::new(pcb_zen_core::JsonExportPass::new(
            info.staging_dir.join("diagnostics.json"),
            zen_file_rel.display().to_string(),
        )));

        let schematic = crate::build::build(
            &staged_zen_path,
            passes,
            false, // don't deny warnings - we'll prompt user instead
            &mut has_errors,
            &mut has_warnings,
            staged_resolution,
        );
        (has_errors, has_warnings, schematic)
    });

    if has_errors
        && !confirm_continue_on_error(
            Some(spinner),
            info.allow_errors,
            "Build completed with errors. Do you want to proceed anyway?",
        )
    {
        std::process::exit(1);
    }

    // Handle warnings: prompt interactively, proceed silently in CI
    if has_warnings && crate::tty::is_interactive() {
        spinner.suspend(|| {
            let confirmed = Confirm::new(
                "Build completed with warnings. Do you want to proceed with the release?",
            )
            .with_default(true)
            .prompt()
            .unwrap_or(false);
            if !confirmed {
                std::process::exit(1);
            }
        });
    }
    // In non-interactive mode (CI), warnings have been rendered - proceed with release

    // Write fp-lib-table with correct vendor/ paths to staged layout directory
    // The staged schematic has footprint paths pointing to src/vendor/ instead of .pcb/cache
    if let Some(ref sch) = schematic {
        if let Some(staged_layout_dir) = info.staged_layout_dir() {
            if staged_layout_dir.exists() {
                pcb_layout::utils::write_footprint_library_table(&staged_layout_dir, sch)
                    .context("Failed to write fp-lib-table for staged layout")?;
            }
        }

        // Write netlist JSON to staging directory (RFC 8785 canonical for deterministic output)
        let netlist_json = to_canonical_json(sch).context("Failed to serialize netlist")?;
        fs::write(info.staging_dir.join("netlist.json"), &netlist_json)
            .context("Failed to write netlist.json")?;
    }

    Ok(())
}

/// Generate design BOM JSON file (with optional KiCad fallback if layout exists)
fn generate_design_bom(info: &ReleaseInfo, _spinner: &Spinner) -> Result<()> {
    // Generate BOM entries from the schematic
    let bom = info.schematic.bom();

    // Create bom directory in staging
    let bom_dir = info.staging_dir.join("bom");
    fs::create_dir_all(&bom_dir)?;

    // Apply fallback logic only if layout exists
    let layout_path = info
        .layout
        .as_ref()
        .map(|l| info.workspace_root().join(l.layout_dir_rel()));
    let final_bom = generate_bom_with_fallback(bom, layout_path.as_deref())?;

    // Write design BOM as JSON
    let bom_file = bom_dir.join("design_bom.json");
    let mut file = fs::File::create(&bom_file)?;
    write!(file, "{}", final_bom.ungrouped_json())?;

    Ok(())
}

/// Write release metadata to JSON file
fn write_metadata(info: &ReleaseInfo, _spinner: &Spinner) -> Result<()> {
    let metadata = create_metadata_json(info);
    let metadata_str = serde_json::to_string_pretty(&metadata)?;
    fs::write(info.staging_dir.join("metadata.json"), metadata_str)?;
    Ok(())
}

/// Remove a directory tree, making files and directories writable first to avoid permission issues
/// This is needed because vendor sync makes files readonly, which prevents normal removal
fn remove_dir_all_with_permissions(dir: &Path) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }

    // Make the directory itself writable
    if let Ok(mut perms) = fs::metadata(dir).map(|m| m.permissions()) {
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        let _ = fs::set_permissions(dir, perms);
    }

    // Recursively process directory contents
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();

        // Check symlink first - is_dir() follows symlinks, so we need to check this explicitly
        if path.is_symlink() {
            fs::remove_file(&path)?;
        } else if path.is_dir() {
            remove_dir_all_with_permissions(&path)?;
        } else {
            // Make file writable before removal
            if let Ok(mut perms) = fs::metadata(&path).map(|m| m.permissions()) {
                #[allow(clippy::permissions_set_readonly_false)]
                perms.set_readonly(false);
                let _ = fs::set_permissions(&path, perms);
            }
            fs::remove_file(&path)?;
        }
    }

    // Remove the now-empty directory
    fs::remove_dir(dir)?;
    Ok(())
}

fn archive_zip_path(info: &ReleaseInfo) -> PathBuf {
    info.output_dir.join(&info.output_name)
}

/// Create zip archive of release staging directory
fn zip_release(info: &ReleaseInfo, _spinner: &Spinner) -> Result<()> {
    let zip_path = archive_zip_path(info);

    // Ensure output directory exists
    if let Some(parent) = zip_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let zip_file = fs::File::create(&zip_path)?;
    // Use buffered writer for better I/O performance
    let buffered = BufWriter::with_capacity(256 * 1024, zip_file);
    let mut zip = ZipWriter::new(buffered);
    add_directory_to_zip(&mut zip, &info.staging_dir, &info.staging_dir)?;
    zip.finish()?;
    Ok(())
}

/// Recursively add directory contents to zip
fn add_directory_to_zip<W: std::io::Write + std::io::Seek>(
    zip: &mut ZipWriter<W>,
    dir: &Path,
    base_path: &Path,
) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        // Skip symlinks to avoid including external directories (e.g., .pcb/cache -> ~/.pcb/cache)
        if path.is_symlink() {
            continue;
        }
        if path.is_dir() {
            add_directory_to_zip(zip, &path, base_path)?;
        } else {
            let file_name = path
                .strip_prefix(base_path)?
                .to_string_lossy()
                .replace('\\', "/");
            zip.start_file(file_name, FileOptions::<()>::default())?;
            std::io::copy(&mut fs::File::open(&path)?, zip)?;
        }
    }
    Ok(())
}

/// Generate gerber files
fn generate_gerbers(info: &ReleaseInfo, _spinner: &Spinner) -> Result<()> {
    let manufacturing_dir = info.staging_dir.join("manufacturing");
    fs::create_dir_all(&manufacturing_dir)?;

    let kicad_pcb_path = info
        .staged_pcb_path()
        .context("No layout directory for gerber generation")?;

    // Generate gerber files to a temporary directory
    let gerbers_dir = manufacturing_dir.join("gerbers_temp");
    fs::create_dir_all(&gerbers_dir)?;

    KiCadCliBuilder::new()
        .command("pcb")
        .subcommand("export")
        .subcommand("gerbers")
        .arg("--output")
        .arg(gerbers_dir.to_string_lossy())
        .arg("--use-drill-file-origin")
        .arg(kicad_pcb_path.to_string_lossy())
        .run()
        .context("Failed to generate gerber files")?;

    // Generate drill files (separate PTH/NPTH) with PDF map(s)
    KiCadCliBuilder::new()
        .command("pcb")
        .subcommand("export")
        .subcommand("drill")
        .arg("--output")
        .arg(gerbers_dir.to_string_lossy())
        .arg("--format")
        .arg("excellon")
        .arg("--drill-origin")
        .arg("plot")
        .arg("--excellon-zeros-format")
        .arg("decimal")
        .arg("--excellon-units")
        .arg("mm")
        .arg("--excellon-separate-th")
        .arg("--generate-map")
        .arg("--map-format")
        .arg("pdf")
        .arg(kicad_pcb_path.to_string_lossy())
        .run()
        .context("Failed to generate drill files")?;

    // Generate drill map(s) as Gerber X2 as well (for CAM tooling that prefers Gerber over PDF)
    KiCadCliBuilder::new()
        .command("pcb")
        .subcommand("export")
        .subcommand("drill")
        .arg("--output")
        .arg(gerbers_dir.to_string_lossy())
        .arg("--format")
        .arg("excellon")
        .arg("--drill-origin")
        .arg("plot")
        .arg("--excellon-zeros-format")
        .arg("decimal")
        .arg("--excellon-units")
        .arg("mm")
        .arg("--excellon-separate-th")
        .arg("--generate-map")
        .arg("--map-format")
        .arg("gerberx2")
        .arg(kicad_pcb_path.to_string_lossy())
        .run()
        .context("Failed to generate gerber drill map(s)")?;

    // Create gerbers.zip from the temp directory
    create_gerbers_zip(&gerbers_dir, &manufacturing_dir.join("gerbers.zip"))?;

    // Clean up temp directory
    fs::remove_dir_all(&gerbers_dir)?;

    Ok(())
}

/// Generate pick-and-place file
fn generate_cpl(info: &ReleaseInfo, _spinner: &Spinner) -> Result<()> {
    let manufacturing_dir = info.staging_dir.join("manufacturing");
    fs::create_dir_all(&manufacturing_dir)?;

    let kicad_pcb_path = info
        .staged_pcb_path()
        .context("No layout directory for CPL generation")?;

    KiCadCliBuilder::new()
        .command("pcb")
        .subcommand("export")
        .subcommand("pos")
        .arg("--format")
        .arg("csv")
        .arg("--units")
        .arg("mm")
        .arg("--use-drill-file-origin")
        .arg("--output")
        .arg(manufacturing_dir.join("cpl.csv").to_string_lossy())
        .arg(kicad_pcb_path.to_string_lossy())
        .run()
        .context("Failed to generate pick-and-place file")?;

    // Fix CPL CSV header to match expected format
    fix_cpl_header(&manufacturing_dir.join("cpl.csv"))?;

    Ok(())
}

/// Generate assembly drawings (front and back PDFs)
fn generate_assembly_drawings(info: &ReleaseInfo, _spinner: &Spinner) -> Result<()> {
    let manufacturing_dir = info.staging_dir.join("manufacturing");
    fs::create_dir_all(&manufacturing_dir)?;

    let kicad_pcb_path = info
        .staged_pcb_path()
        .context("No layout directory for assembly drawings")?;

    // Generate front assembly drawing
    KiCadCliBuilder::new()
        .command("pcb")
        .subcommand("export")
        .subcommand("pdf")
        .arg("--output")
        .arg(
            manufacturing_dir
                .join("assembly_front.pdf")
                .to_string_lossy(),
        )
        .arg("--layers")
        .arg("F.Fab,Edge.Cuts")
        .arg("--include-border-title")
        .arg(kicad_pcb_path.to_string_lossy())
        .run()
        .context("Failed to generate front assembly drawing")?;

    // Generate back assembly drawing
    KiCadCliBuilder::new()
        .command("pcb")
        .subcommand("export")
        .subcommand("pdf")
        .arg("--output")
        .arg(
            manufacturing_dir
                .join("assembly_back.pdf")
                .to_string_lossy(),
        )
        .arg("--layers")
        .arg("B.Fab,Edge.Cuts")
        .arg("--mirror")
        .arg("--include-border-title")
        .arg(kicad_pcb_path.to_string_lossy())
        .run()
        .context("Failed to generate back assembly drawing")?;

    Ok(())
}

/// Create a ZIP archive from gerber files directory
fn create_gerbers_zip(gerbers_dir: &Path, zip_path: &Path) -> Result<()> {
    let zip_file = fs::File::create(zip_path)?;
    let buffered = BufWriter::with_capacity(256 * 1024, zip_file);
    let mut zip = zip::ZipWriter::new(buffered);

    for entry in fs::read_dir(gerbers_dir)? {
        let entry = entry?;
        let path = entry.path();
        // Skip symlinks for safety
        if path.is_symlink() {
            continue;
        }
        if path.is_file() {
            let name = path.file_name().unwrap().to_string_lossy();
            zip.start_file(name, zip::write::FileOptions::<()>::default())?;
            std::io::copy(&mut fs::File::open(&path)?, &mut zip)?;
        }
    }
    zip.finish()?;
    Ok(())
}

/// Fix the CPL CSV header to match expected format
fn fix_cpl_header(cpl_path: &Path) -> Result<()> {
    let content = fs::read_to_string(cpl_path)?;
    let lines: Vec<&str> = content.lines().collect();
    if lines.len() > 1 {
        let fixed_content = format!(
            "Designator,Val,Package,Mid X,Mid Y,Rotation,Layer\n{}",
            lines[1..].join("\n")
        );
        fs::write(cpl_path, fixed_content)?;
    }
    Ok(())
}

/// Generate ODB++ files
fn generate_odb(info: &ReleaseInfo, _spinner: &Spinner) -> Result<()> {
    let manufacturing_dir = info.staging_dir.join("manufacturing");
    fs::create_dir_all(&manufacturing_dir)?;

    let kicad_pcb_path = info
        .staged_pcb_path()
        .context("No layout directory for ODB++ generation")?;
    let odb_path = manufacturing_dir.join("odb.zip");

    KiCadCliBuilder::new()
        .command("pcb")
        .subcommand("export")
        .subcommand("odb")
        .arg("--output")
        .arg(odb_path.to_string_lossy())
        .arg("--units")
        .arg("mm")
        .arg("--precision")
        .arg("2")
        .arg("--compression")
        .arg("zip")
        .arg(kicad_pcb_path.to_string_lossy())
        .run()
        .context("Failed to generate ODB++ files")?;

    Ok(())
}

/// Generate IPC-2581 file
fn generate_ipc2581(info: &ReleaseInfo, _spinner: &Spinner) -> Result<()> {
    let manufacturing_dir = info.staging_dir.join("manufacturing");
    fs::create_dir_all(&manufacturing_dir)?;

    let kicad_pcb_path = info
        .staged_pcb_path()
        .context("No layout directory for IPC-2581 generation")?;
    let ipc2581_path = manufacturing_dir.join("ipc2581.xml");

    KiCadCliBuilder::new()
        .command("pcb")
        .subcommand("export")
        .subcommand("ipc2581")
        .arg("--output")
        .arg(ipc2581_path.to_string_lossy())
        .arg("--bom-col-int-id")
        .arg("Path")
        .arg("--bom-col-mfg-pn")
        .arg("Mpn")
        .arg("--bom-col-mfg")
        .arg("Manufacturer")
        .arg(kicad_pcb_path.to_string_lossy())
        .run()
        .context("Failed to generate IPC-2581 file")?;

    // Generate HTML export from the IPC-2581 XML file (silently, without printing)
    let ipc2581_html_path = manufacturing_dir.join("ipc2581.html");
    let ipc_content = pcb_ipc2581_tools::utils::file::load_ipc_file(&ipc2581_path)
        .context("Failed to load IPC-2581 file for HTML export")?;
    let ipc = pcb_ipc2581_tools::ipc2581::Ipc2581::parse(&ipc_content)
        .context("Failed to parse IPC-2581 file for HTML export")?;
    let accessor = pcb_ipc2581_tools::accessors::IpcAccessor::new(&ipc);
    let html = pcb_ipc2581_tools::commands::html_export::generate_html(
        &accessor,
        pcb_ipc2581_tools::UnitFormat::Mm,
    )
    .context("Failed to generate HTML from IPC-2581")?;
    fs::write(&ipc2581_html_path, html).context("Failed to write IPC-2581 HTML export")?;

    Ok(())
}

/// Generate STEP model
fn generate_step_model(info: &ReleaseInfo, _spinner: &Spinner) -> Result<()> {
    let models_dir = info.staging_dir.join("3d");
    fs::create_dir_all(&models_dir)?;

    let kicad_pcb_path = info
        .staged_pcb_path()
        .context("No layout directory for STEP model generation")?;

    // Create a temp file to capture and discard verbose KiCad output
    let devnull = tempfile::tempfile()?;

    // Generate STEP model - KiCad CLI has platform-specific exit code issues
    let step_path = models_dir.join("model.step");
    let step_result = KiCadCliBuilder::new()
        .command("pcb")
        .subcommand("export")
        .subcommand("step")
        .arg("--subst-models")
        .arg("--force")
        .arg("--output")
        .arg(step_path.to_string_lossy())
        .arg("--no-dnp")
        // FIXME: kicad-imported projects have unspecified footprints, so allow these temporarily
        // .arg("--no-unspecified")
        .arg("--include-silkscreen")
        .arg(kicad_pcb_path.to_string_lossy())
        .log_file(devnull)
        .suppress_error_output(true)
        .run();

    if let Err(e) = step_result {
        if step_path.exists() {
            warn!("KiCad CLI reported error but STEP file was created: {e}");
        } else {
            return Err(e).context("Failed to generate STEP model");
        }
    }

    Ok(())
}

/// Generate VRML model
fn generate_vrml_model(info: &ReleaseInfo, _spinner: &Spinner) -> Result<()> {
    let models_dir = info.staging_dir.join("3d");
    fs::create_dir_all(&models_dir)?;

    let kicad_pcb_path = info
        .staged_pcb_path()
        .context("No layout directory for VRML model generation")?;

    // Create a temp file to capture and discard verbose KiCad output
    let devnull = tempfile::tempfile()?;

    // Generate VRML model - KiCad CLI has platform-specific exit code issues
    let wrl_path = models_dir.join("model.wrl");
    let wrl_result = KiCadCliBuilder::new()
        .command("pcb")
        .subcommand("export")
        .subcommand("vrml")
        .arg("--output")
        .arg(wrl_path.to_string_lossy())
        .arg("--units")
        .arg("mm")
        .arg("--no-dnp")
        // FIXME: kicad-imported projects have unspecified footprints, so allow these temporarily
        // .arg("--no-unspecified")
        .arg(kicad_pcb_path.to_string_lossy())
        .log_file(devnull)
        .suppress_error_output(true)
        .run();

    if let Err(e) = wrl_result {
        if wrl_path.exists() {
            warn!("KiCad CLI reported error but VRML file was created: {e}");
        } else {
            return Err(e).context("Failed to generate VRML model");
        }
    }

    Ok(())
}

/// Generate GLB model
fn generate_glb_model(info: &ReleaseInfo, _spinner: &Spinner) -> Result<()> {
    let models_dir = info.staging_dir.join("3d");
    fs::create_dir_all(&models_dir)?;

    let kicad_pcb_path = info
        .staged_pcb_path()
        .context("No layout directory for GLB model generation")?;

    // Create a temp file to capture and discard verbose KiCad output
    let devnull = tempfile::tempfile()?;

    // Generate GLB model - KiCad CLI has platform-specific exit code issues
    let glb_path = models_dir.join("model.glb");
    let glb_result = KiCadCliBuilder::new()
        .command("pcb")
        .subcommand("export")
        .subcommand("glb")
        .arg("--output")
        .arg(glb_path.to_string_lossy())
        .arg("--subst-models")
        .arg("--force")
        .arg("--no-dnp")
        // FIXME: kicad-imported projects have unspecified footprints, so allow these temporarily
        // .arg("--no-unspecified")
        .arg("--include-pads")
        .arg("--include-silkscreen")
        .arg(kicad_pcb_path.to_string_lossy())
        .log_file(devnull)
        .suppress_error_output(true)
        .run();

    if let Err(e) = glb_result {
        if glb_path.exists() {
            warn!("KiCad CLI reported error but GLB file was created: {e}");
        } else {
            return Err(e).context("Failed to generate GLB model");
        }
    }

    // Optimize GLB file with gltfpack
    match gltfpack_sys::compress(&glb_path, &glb_path) {
        Ok(()) => {
            debug!("GLB file optimized successfully with gltfpack");
        }
        Err(code) => {
            warn!("gltfpack failed with error code: {code}, skipping optimization");
        }
    }

    Ok(())
}

/// Generate SVG rendering
fn generate_svg_rendering(info: &ReleaseInfo, _spinner: &Spinner) -> Result<()> {
    let models_dir = info.staging_dir.join("3d");
    fs::create_dir_all(&models_dir)?;

    let kicad_pcb_path = info
        .staged_pcb_path()
        .context("No layout directory for SVG rendering")?;

    // Create a temp file to capture and discard verbose KiCad output
    let devnull = tempfile::tempfile()?;

    // Generate SVG rendering - KiCad CLI has platform-specific exit code issues
    let svg_path = models_dir.join("model.svg");
    let svg_result = KiCadCliBuilder::new()
        .command("pcb")
        .subcommand("export")
        .subcommand("svg")
        .arg("--output")
        .arg(svg_path.to_string_lossy())
        .arg("--layers")
        .arg("F.Cu,B.Cu,F.SilkS,B.SilkS,F.Mask,B.Mask,Edge.Cuts")
        .arg("--page-size-mode")
        .arg("2") // Board area only
        .arg(kicad_pcb_path.to_string_lossy())
        .log_file(devnull)
        .suppress_error_output(true)
        .run();

    if let Err(e) = svg_result {
        if svg_path.exists() {
            warn!("KiCad CLI reported error but SVG file was created: {e}");
        } else {
            return Err(e).context("Failed to generate SVG rendering");
        }
    }

    Ok(())
}

/// Run KiCad DRC checks on the layout file
fn run_kicad_drc(info: &ReleaseInfo, spinner: &Spinner) -> Result<()> {
    // Use staged files so fp-lib-table vendor paths are correct.
    let kicad_pcb_path = info
        .staged_kicad_files()
        .context("No layout directory for DRC checks")?
        .kicad_pcb();

    // Collect diagnostics from layout sync check
    let mut diagnostics = pcb_zen_core::Diagnostics::default();
    pcb_layout::process_layout(
        &info.schematic,
        &info.zen_path,
        false,
        false,
        true,
        &mut diagnostics,
    )?;

    // Run DRC, writing raw KiCad JSON report to staging directory
    let drc_json_path = info.staging_dir.join("drc.json");
    let report = pcb_kicad::run_drc(&kicad_pcb_path, false, None, &drc_json_path)?;
    report.add_to_diagnostics(&mut diagnostics, &kicad_pcb_path.to_string_lossy());
    spinner.suspend(|| crate::drc::render_diagnostics(&mut diagnostics, &info.suppress));

    // Fail if there are errors
    if diagnostics.error_count() > 0
        && !confirm_continue_on_error(
            Some(spinner),
            info.allow_errors,
            &format!(
                "DRC completed with {} error(s). Do you want to proceed anyway?",
                diagnostics.error_count()
            ),
        )
    {
        std::process::exit(1);
    }

    // Prompt user if there are warnings (interactive mode only)
    if diagnostics.warning_count() > 0 && crate::tty::is_interactive() {
        spinner.suspend(|| {
            let confirmed = Confirm::new(&format!(
                "DRC completed with {} warning(s). Do you want to proceed with the release?",
                diagnostics.warning_count()
            ))
            .with_default(true)
            .prompt()
            .unwrap_or(false);
            if !confirmed {
                std::process::exit(1);
            }
        });
    }

    Ok(())
}
