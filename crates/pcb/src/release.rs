use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use inquire::Confirm;
use log::{debug, info, warn};
use pcb_kicad::{KiCadCliBuilder, PythonScriptBuilder};
use pcb_ui::{Colorize, Spinner, Style, StyledText};

use crate::bom::generate_bom_with_fallback;
use pcb_zen::workspace::{get_workspace_info, WorkspaceInfoExt};
use pcb_zen::{EvalConfig, PackageClosure, ResolutionResult};
use pcb_zen_core::DefaultFileProvider;
use pcb_zen_core::{EvalOutput, WithDiagnostics};

use pcb_zen::WorkspaceInfo;

use std::fs;
use std::io::{IsTerminal, Write};
use std::time::Instant;

use chrono::Utc;
use std::path::{Path, PathBuf};

use zip::{write::FileOptions, ZipWriter};

use crate::vendor::sync_tracked_files;
use pcb_zen::git;

const RELEASE_SCHEMA_VERSION: &str = "1";

#[derive(Debug, Clone, PartialEq)]
pub enum ReleaseKind {
    SourceOnly,
    Full,
}

#[derive(ValueEnum, Debug, Clone, Default)]
pub enum ReleaseOutputFormat {
    #[default]
    #[value(name = "human")]
    Human,
    Json,
    None,
}

impl std::fmt::Display for ReleaseOutputFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReleaseOutputFormat::Human => write!(f, "human"),
            ReleaseOutputFormat::Json => write!(f, "json"),
            ReleaseOutputFormat::None => write!(f, "none"),
        }
    }
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
    FabHtml,
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
            ArtifactType::FabHtml => "Generating fabrication drawing",
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
            ArtifactType::FabHtml => generate_fab_drawing,
        }
    }
}

#[derive(Args)]
pub struct ReleaseArgs {
    /// Board name to release
    #[arg(
        short = 'b',
        long,
        conflicts_with = "file",
        required_unless_present = "file"
    )]
    pub board: Option<String>,

    /// Path to .zen file to release (alternative to --board)
    #[arg(long, conflicts_with = "board", required_unless_present = "board")]
    pub file: Option<PathBuf>,

    /// Optional path to start discovery from (defaults to current directory)
    pub path: Option<String>,

    /// Output format
    #[arg(short, long, value_enum, default_value_t = ReleaseOutputFormat::Human)]
    pub format: ReleaseOutputFormat,

    /// Create source-only release without manufacturing artifacts
    #[arg(long)]
    pub source_only: bool,

    /// Directory where the release .zip file will be placed (defaults to <workspace_root>/.pcb/releases)
    #[arg(long)]
    pub output_dir: Option<PathBuf>,

    /// Name of the output .zip file (defaults to <board>-<version>.zip)
    #[arg(long)]
    pub output_name: Option<String>,

    /// Exclude specific manufacturing artifacts from the release (can be specified multiple times)
    #[arg(long, value_enum)]
    pub exclude: Vec<ArtifactType>,

    /// Skip confirmation prompt when warnings are present during validation
    #[arg(long)]
    pub yes: bool,

    /// Suppress diagnostics by kind or severity. Use 'warnings' or 'errors' for all
    /// warnings/errors, or specific kinds like 'layout.drc.clearance'.
    /// Supports hierarchical matching (e.g., 'layout.drc' matches 'layout.drc.clearance')
    #[arg(short = 'S', long = "suppress", value_name = "KIND")]
    pub suppress: Vec<String>,
}

/// All information gathered during the release preparation phase
pub struct ReleaseInfo {
    /// Workspace configuration from pcb.toml discovery
    pub config: WorkspaceInfo,
    /// Canonical path to the .zen file being released
    pub zen_path: PathBuf,
    /// Eval output (needed for V1 file tracking in copy_sources)
    pub eval_output: EvalOutput,
    /// Board name being released
    pub board_name: String,
    /// Release version (from git or fallback)
    pub version: String,
    /// Git commit hash (for variable substitution)
    pub git_hash: String,
    /// Path to the staging directory where release will be assembled
    pub staging_dir: PathBuf,
    /// Path to the layout directory containing KiCad files
    pub layout_path: PathBuf,
    /// Evaluated schematic from the zen file
    pub schematic: pcb_sch::Schematic,
    /// Type of release being created
    pub kind: ReleaseKind,
    /// Directory where the final .zip file will be placed
    pub output_dir: PathBuf,
    /// Name of the output .zip file
    pub output_name: String,
    /// Skip confirmation prompt for warnings
    pub yes: bool,
    /// Diagnostic kinds to suppress
    pub suppress: Vec<String>,
    /// V2 resolution result (None for V1 workspaces)
    pub v2_resolution: Option<ResolutionResult>,
    /// V2 package closure for the board (None for V1 workspaces)
    pub v2_closure: Option<PackageClosure>,
}

impl ReleaseInfo {
    /// Get the workspace root directory
    pub fn workspace_root(&self) -> &Path {
        &self.config.root
    }

    /// Get a human-friendly board display name
    pub fn board_display_name(&self) -> String {
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
}

type TaskFn = fn(&ReleaseInfo, &Spinner) -> Result<()>;

const BASE_TASKS: &[(&str, TaskFn)] = &[
    ("Copying source files and dependencies", copy_sources),
    ("Validating build from staged sources", validate_build),
    ("Copying layout files", copy_layout),
    ("Generating board config", generate_board_config),
    ("Copying documentation", copy_docs),
    ("Substituting version variables", substitute_variables),
];

/// All manufacturing artifacts in the order they should be generated
const MANUFACTURING_ARTIFACTS: &[ArtifactType] = &[
    ArtifactType::Drc, // Run DRC checks first, before generating any manufacturing files
    ArtifactType::FabHtml,
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

/// Get manufacturing tasks as (name, function) pairs, filtered by exclusions
fn get_manufacturing_tasks(excluded: &[ArtifactType]) -> Vec<(&'static str, TaskFn)> {
    MANUFACTURING_ARTIFACTS
        .iter()
        .filter(|artifact| !excluded.contains(artifact))
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

pub fn execute(args: ReleaseArgs) -> Result<()> {
    let start_time = Instant::now();

    let release_info = {
        let info_spinner = Spinner::builder("Gathering release information").start();

        // Get workspace info first to detect V2
        let start_path = args.path.as_deref().unwrap_or(".");
        let mut config_workspace_info =
            get_workspace_info(&DefaultFileProvider::new(), Path::new(start_path))?;
        let is_v2 = config_workspace_info.is_v2();

        // Determine board name and zen path
        let (zen_path, board_name) = if let Some(board_name) = &args.board {
            let board_info = config_workspace_info.find_board_by_name(board_name)?;
            let zen_path = board_info.absolute_zen_path(&config_workspace_info.root);
            (zen_path, board_name.clone())
        } else if let Some(zen_file) = &args.file {
            let zen_path = zen_file.canonicalize()?;
            let board_name = config_workspace_info
                .board_name_for_zen(&zen_path)
                .unwrap_or_else(|| zen_path.file_stem().unwrap().to_string_lossy().to_string());
            (zen_path, board_name)
        } else {
            unreachable!("Either board or file must be provided due to clap validation")
        };

        // For V2: run resolution before eval to get package closure
        let (v2_resolution, v2_closure) = if is_v2 {
            info_spinner.set_message("Resolving V2 dependencies");
            let resolution = pcb_zen::resolve_dependencies(&mut config_workspace_info, false)?;

            // Find the package URL for this board
            let closure = config_workspace_info
                .package_url_for_zen(&zen_path)
                .map(|url| config_workspace_info.package_closure(&url, &resolution));

            (Some(resolution), closure)
        } else {
            (None, None)
        };

        info_spinner.set_message("Evaluating zen file");

        // Evaluate the zen file (still needed for schematic)
        // For V2: pass resolution so Module() paths resolve correctly
        let eval_cfg = EvalConfig {
            use_vendor: true,
            resolution_result: v2_resolution.clone(),
            ..Default::default()
        };
        let eval_result = pcb_zen::eval(&zen_path, eval_cfg);

        info_spinner.finish();

        // Render diagnostics and fail early if there are errors
        if eval_result.diagnostics.has_errors() || eval_result.output.is_none() {
            let mut diagnostics = eval_result.diagnostics.clone();
            let passes = crate::build::create_diagnostics_passes(&[]);
            diagnostics.apply_passes(&passes);
            anyhow::bail!("Evaluation failed");
        }

        let mut info = build_release_info(
            config_workspace_info,
            zen_path,
            eval_result.output.unwrap(),
            board_name,
            &args,
        )?;

        // Attach V2 data
        info.v2_resolution = v2_resolution;
        info.v2_closure = v2_closure;

        let elapsed = start_time.elapsed().as_secs_f64();
        eprintln!(
            "{}: {} ({}) Release information gathered{}",
            format_cumulative_time(elapsed),
            "✓".green(),
            format_task_duration(elapsed),
            if is_v2 { " (V2)" } else { "" }
        );

        info
    };

    // Execute base tasks
    execute_tasks(&release_info, BASE_TASKS, start_time)?;

    // Execute manufacturing tasks if full release
    if matches!(release_info.kind, ReleaseKind::Full) {
        let manufacturing_tasks = get_manufacturing_tasks(&args.exclude);
        execute_tasks(&release_info, &manufacturing_tasks, start_time)?;
    }

    // Execute finalization tasks
    execute_tasks(&release_info, FINALIZATION_TASKS, start_time)?;

    // Calculate archive path
    let zip_path = archive_zip_path(&release_info);

    eprintln!(
        "{} {}",
        "✓".green(),
        format!("Release {} staged successfully", release_info.version).bold()
    );
    display_release_info(&release_info, args.format.clone());

    // Only show archive path for Human format
    if !matches!(args.format, ReleaseOutputFormat::None) {
        eprintln!(
            "Archive: {}",
            zip_path.display().to_string().with_style(Style::Cyan)
        );
    }

    Ok(())
}

/// Build ReleaseInfo from workspace info and other parameters
fn build_release_info(
    config: WorkspaceInfo,
    zen_path: PathBuf,
    eval_output: EvalOutput,
    board_name: String,
    args: &ReleaseArgs,
) -> Result<ReleaseInfo> {
    // Get version and git hash from git
    let (version, git_hash) = git_version_and_hash(&config.root, &board_name)?;

    // Create release staging directory in workspace root with flat structure:
    // Structure: {workspace_root}/.pcb/releases/{board_name}-{version}
    // Example: /workspace/.pcb/releases/test_board-f20ac95-dirty
    let staging_dir = config
        .root
        .join(".pcb/releases")
        .join(format!("{}-{}", board_name, version));

    // Determine output directory and name
    let default_output_dir = config.root.join(".pcb/releases");
    let output_dir = args.output_dir.clone().unwrap_or(default_output_dir);
    let output_name = args.output_name.clone().unwrap_or_else(|| {
        if args.source_only {
            format!("{}-{}.source.zip", board_name, version)
        } else {
            format!("{}-{}.zip", board_name, version)
        }
    });

    // Delete existing staging dir and recreate
    if staging_dir.exists() {
        debug!(
            "Removing existing staging directory: {}",
            staging_dir.display()
        );
        remove_dir_all_with_permissions(&staging_dir)?;
    }
    fs::create_dir_all(&staging_dir)?;

    // Extract layout path from evaluation
    let layout_path = extract_layout_path_from_output(&zen_path, &eval_output)?;

    let schematic = eval_output.to_schematic()?;
    let kind = if args.source_only {
        ReleaseKind::SourceOnly
    } else {
        ReleaseKind::Full
    };

    Ok(ReleaseInfo {
        config,
        zen_path,
        eval_output,
        board_name,
        version,
        git_hash,
        staging_dir,
        layout_path,
        schematic,
        kind,
        output_dir,
        output_name,
        yes: args.yes,
        suppress: args.suppress.clone(),
        v2_resolution: None,
        v2_closure: None,
    })
}

/// Display all the gathered release information
fn display_release_info(info: &ReleaseInfo, format: ReleaseOutputFormat) {
    let release_type = match info.kind {
        ReleaseKind::SourceOnly => "Source-Only Release",
        ReleaseKind::Full => "Full Release",
    };
    match format {
        ReleaseOutputFormat::Human => {
            eprintln!(
                "{}",
                "Release Summary".to_string().with_style(Style::Blue).bold()
            );
            let mut table = comfy_table::Table::new();
            table
                .load_preset(comfy_table::presets::UTF8_BORDERS_ONLY)
                .set_content_arrangement(comfy_table::ContentArrangement::Dynamic);

            // Add release information
            table.add_row(vec!["Release Type", release_type]);
            table.add_row(vec!["Version", &info.version]);
            table.add_row(vec![
                "Git Hash",
                &info.git_hash[..8.min(info.git_hash.len())],
            ]); // Show short hash

            // Add file paths (relative to make them shorter)
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

            // Add system info
            table.add_row(vec!["Platform", std::env::consts::OS]);
            table.add_row(vec!["Architecture", std::env::consts::ARCH]);
            table.add_row(vec!["CLI Version", env!("CARGO_PKG_VERSION")]);
            table.add_row(vec!["KiCad Version", &get_kicad_version()]);

            // Add user and timestamp
            let user = std::env::var("USER").unwrap_or_else(|_| "unknown".to_string());
            table.add_row(vec!["Created By", &user]);

            let timestamp = Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string();
            table.add_row(vec!["Created At", &timestamp]);

            println!("{table}");
        }
        ReleaseOutputFormat::Json => {
            eprintln!(
                "{}",
                "Release Summary".to_string().with_style(Style::Blue).bold()
            );
            // Create and display the metadata that will be saved
            let metadata = create_metadata_json(info);
            println!(
                "{}",
                serde_json::to_string_pretty(&metadata).unwrap_or_default()
            );
        }
        ReleaseOutputFormat::None => {}
    }
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
    let source_only = matches!(info.kind, ReleaseKind::SourceOnly);
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
        "staging_directory": info.staging_dir,
        "layout_path": info.layout_path,
        "source_only": source_only
    });

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

/// Determine release version using clean git-based logic:
/// - If working directory is dirty: {commit_hash}-dirty
/// - If current commit has a tag: {tag_name}
/// - If clean but no tag: {commit_hash}
fn git_version_and_hash(path: &Path, board_name: &str) -> Result<(String, String)> {
    debug!("Getting git version from: {}", path.display());

    // Check if working directory is dirty
    let is_dirty = pcb_zen::git::has_uncommitted_changes_in_path(path, Path::new("."));

    // Get current commit hash
    let Some(full_hash) = pcb_zen::git::rev_parse_head(path) else {
        warn!("Not a git repository, using 'unknown' as version and hash");
        return Ok(("unknown".into(), "unknown".into()));
    };

    let short_hash = full_hash[..7.min(full_hash.len())].to_owned();

    // If dirty, return short hash with dirty suffix for version, full hash for metadata
    if is_dirty {
        let version = format!("{short_hash}-dirty");
        info!("Git version (dirty): {version}");
        return Ok((version, full_hash));
    }

    // Check if current commit is tagged
    let tags = pcb_zen::git::tags_pointing_at_head(path);

    // Look for board-specific tag in format "board_name/version" (case-insensitive board name)
    let tag_prefix = format!("{board_name}/");
    for tag in &tags {
        if !tag.is_empty()
            && tag.len() > tag_prefix.len()
            && tag[..tag_prefix.len()].eq_ignore_ascii_case(&tag_prefix)
        {
            let version = tag[tag_prefix.len()..].to_string();
            info!("Git version (board tag): {version} for board {board_name}");
            return Ok((version, full_hash));
        }
    }

    // Not dirty and not tagged, use short hash for version, full hash for metadata
    info!("Git version (commit): {short_hash}");
    Ok((short_hash, full_hash))
}

/// Extract layout path from zen evaluation result (public for bom.rs)
pub fn extract_layout_path(zen_path: &Path, eval: &WithDiagnostics<EvalOutput>) -> Result<PathBuf> {
    let output = eval
        .output
        .as_ref()
        .context("Evaluation failed - see diagnostics above")?;
    extract_layout_path_from_output(zen_path, output)
}

/// Extract layout path from zen evaluation output
fn extract_layout_path_from_output(zen_path: &Path, output: &EvalOutput) -> Result<PathBuf> {
    let properties = output.sch_module.properties();

    let layout_path_value = properties.get("layout_path")
        .context("No layout_path property found in zen file - add_property(\"layout_path\", \"path\") is required")?;

    let layout_path_str = layout_path_value.to_string();
    let clean_path_str = layout_path_str.trim_matches('"');

    // Layout path is relative to the zen file's parent directory
    let zen_parent_dir = zen_path
        .parent()
        .context("Zen file has no parent directory")?;
    let layout_path = zen_parent_dir.join(clean_path_str);

    debug!(
        "Extracted layout path: {} -> {}",
        clean_path_str,
        layout_path.display()
    );
    Ok(layout_path)
}

/// Copy source files and vendor dependencies
fn copy_sources(info: &ReleaseInfo, _spinner: &Spinner) -> Result<()> {
    // Use V2 package-based copying if we have resolution data
    if let Some(closure) = &info.v2_closure {
        return copy_sources_v2(info, closure);
    }

    // V1 fallback: use file tracking from eval
    let output = &info.eval_output;
    let tracked_files = output.core_resolver().unwrap().get_tracked_files();
    let workspace_root = info.workspace_root();
    let src_dir = info.staging_dir.join("src");
    let vendor_dir = src_dir.join("vendor");
    sync_tracked_files(&tracked_files, workspace_root, &vendor_dir, Some(&src_dir))?;
    Ok(())
}

/// V2 source copying: copy workspace packages and vendor remote dependencies
fn copy_sources_v2(info: &ReleaseInfo, closure: &PackageClosure) -> Result<()> {
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
    for pkg_url in &closure.local_packages {
        if let Some(pkg) = info.config.packages.get(pkg_url) {
            let dest = src_dir.join(&pkg.rel_path);
            copy_dir_excluding_git(&pkg.dir, &dest)?;
            debug!("Copied package {} to {}", pkg_url, dest.display());
        }
    }

    // 3. Vendor remote dependencies using vendor_deps with "**" pattern
    if let Some(resolution) = &info.v2_resolution {
        let result = pcb_zen::vendor_deps(
            &info.config,
            resolution,
            &["**".to_string()],
            Some(&vendor_dir),
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
    }

    Ok(())
}

/// Copy a directory recursively, excluding .git
fn copy_dir_excluding_git(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        let src_path = entry.path();
        let dst_path = dst.join(name);
        if entry.file_type()?.is_dir() {
            copy_dir_excluding_git(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

/// Copy KiCad layout files
fn copy_layout(info: &ReleaseInfo, _spinner: &Spinner) -> Result<()> {
    // If build directory doesn't exist, generate layout files first
    if !info.layout_path.exists() {
        pcb_layout::process_layout(&info.schematic, &info.zen_path, false, false)?;
    }

    let layout_staging_dir = info.staging_dir.join("layout");
    fs::create_dir_all(&layout_staging_dir)?;

    for entry in walkdir::WalkDir::new(&info.layout_path)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
    {
        if let Some(filename) = entry.path().file_name() {
            fs::copy(entry.path(), layout_staging_dir.join(filename))?;
        }
    }
    Ok(())
}

/// Generate board config JSON file
fn generate_board_config(info: &ReleaseInfo, _spinner: &Spinner) -> Result<()> {
    // Extract board config from the schematic
    let Some(board_config) = pcb_layout::utils::extract_board_config(&info.schematic) else {
        debug!("No board config found in schematic, skipping");
        return Ok(());
    };

    // Write board config to layout directory
    let layout_staging_dir = info.staging_dir.join("layout");
    let board_config_path = layout_staging_dir.join("board_config.json");

    let board_config_json = serde_json::to_string_pretty(&board_config)
        .context("Failed to serialize board config to JSON")?;

    fs::write(&board_config_path, board_config_json)
        .context("Failed to write board config file")?;

    debug!("Generated board config at: {}", board_config_path.display());
    Ok(())
}

/// Generate fabrication drawing HTML file
fn generate_fab_drawing(info: &ReleaseInfo, _spinner: &Spinner) -> Result<()> {
    // Extract board config from the schematic
    let Some(board_config) = pcb_layout::utils::extract_board_config(&info.schematic) else {
        debug!("No board config found in schematic, skipping fab drawing");
        return Ok(());
    };

    let manufacturing_dir = info.staging_dir.join("manufacturing");
    fs::create_dir_all(&manufacturing_dir)?;

    // Generate HTML fab drawing
    let html = pcb_layout::fab_drawing::generate_html(&board_config);
    let html_path = manufacturing_dir.join("fab_drawing.html");
    fs::write(&html_path, html).context("Failed to write HTML fab drawing")?;
    debug!("Generated HTML fab drawing at: {}", html_path.display());
    Ok(())
}

/// Copy documentation files from docs directory adjacent to zen file
fn copy_docs(info: &ReleaseInfo, _spinner: &Spinner) -> Result<()> {
    // Look for docs directory adjacent to zen file
    let docs_source_dir = info.zen_path.parent().unwrap().join("docs");

    // Only proceed if docs directory exists
    if !docs_source_dir.exists() {
        debug!("No docs directory found at: {}", docs_source_dir.display());
        return Ok(());
    }

    // Copy all files from docs source to staging docs directory
    let docs_staging_dir = info.staging_dir.join("docs");
    for entry in walkdir::WalkDir::new(&docs_source_dir)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
    {
        let relative_path = entry.path().strip_prefix(&docs_source_dir)?;
        let dest_path = docs_staging_dir.join(relative_path);

        // Ensure parent directory exists
        if let Some(parent) = dest_path.parent() {
            fs::create_dir_all(parent)?;
        }

        fs::copy(entry.path(), dest_path)?;
    }

    debug!(
        "Copied docs from: {} to: {}",
        docs_source_dir.display(),
        docs_staging_dir.display()
    );
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
    debug!("Substituting version variables in KiCad files");

    // Determine display name of the board
    let board_name = info.board_display_name();

    // Use short hash (7 chars) for variable substitution
    let short_hash = &info.git_hash[..7.min(info.git_hash.len())];

    // First, update the .kicad_pro file to ensure text variables are defined
    let kicad_pro_path = info.staging_dir.join("layout").join("layout.kicad_pro");
    update_kicad_pro_text_variables(&kicad_pro_path, &info.version, short_hash, &board_name)?;

    // Then update the .kicad_pcb file with the actual values
    let kicad_pcb_path = info.staging_dir.join("layout").join("layout.kicad_pcb");
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

    let file_name = staged_zen_path.file_name().unwrap().to_string_lossy();

    // For V2: re-resolve dependencies on the staged sources
    // This is cleaner than remapping paths from the original resolution
    let staged_resolution = if info.v2_resolution.is_some() {
        let mut staged_workspace =
            get_workspace_info(&DefaultFileProvider::new(), &staged_zen_path)?;
        // Staged sources have vendored deps, so run resolution in offline mode
        let resolution = pcb_zen::resolve_dependencies(&mut staged_workspace, true)?;
        Some(resolution)
    } else {
        None
    };

    // Use build function with offline mode but allow warnings
    // Suspend spinner during build to allow diagnostics to render properly
    let (has_errors, has_warnings) = spinner.suspend(|| {
        let mut has_errors = false;
        let mut has_warnings = false;

        // Export diagnostics to JSON for release artifacts
        let mut passes = crate::build::create_diagnostics_passes(&[]);
        passes.push(Box::new(pcb_zen_core::JsonExportPass::new(
            info.staging_dir.join("diagnostics.json"),
            zen_file_rel.display().to_string(),
        )));

        let _schematic = crate::build::build(
            &staged_zen_path,
            true, // offline mode since all dependencies should be vendored
            passes,
            false, // don't deny warnings - we'll prompt user instead
            &mut has_errors,
            &mut has_warnings,
            staged_resolution,
        );
        (has_errors, has_warnings)
    });

    if has_errors {
        std::process::exit(1);
    }

    // Handle warnings if present and --yes flag wasn't passed
    if has_warnings && !info.yes {
        // Suspend spinner during user prompt
        spinner.suspend(|| {
            // Non-interactive if stdin OR stdout is not a terminal
            if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
                eprintln!(
                    "{} {}: Build failed",
                    pcb_ui::icons::error(),
                    file_name.with_style(Style::Red).bold()
                );
                std::process::exit(1);
            }
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

    Ok(())
}

/// Generate design BOM JSON file with KiCad fallback
fn generate_design_bom(info: &ReleaseInfo, _spinner: &Spinner) -> Result<()> {
    // Generate BOM entries from the schematic
    let bom = info.schematic.bom();

    // Create bom directory in staging
    let bom_dir = info.staging_dir.join("bom");
    fs::create_dir_all(&bom_dir)?;

    // Apply fallback logic
    let final_bom = generate_bom_with_fallback(bom, Some(&info.layout_path))
        .with_context(|| "Failed to generate BOM with KiCad fallback")?;

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

        if path.is_dir() {
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
    let mut zip = ZipWriter::new(zip_file);
    add_directory_to_zip(&mut zip, &info.staging_dir, &info.staging_dir)?;
    zip.finish()?;
    Ok(())
}

/// Recursively add directory contents to zip
fn add_directory_to_zip(zip: &mut ZipWriter<fs::File>, dir: &Path, base_path: &Path) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
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

    let kicad_pcb_path = info.staging_dir.join("layout").join("layout.kicad_pcb");

    // Generate gerber files to a temporary directory
    let gerbers_dir = manufacturing_dir.join("gerbers_temp");
    fs::create_dir_all(&gerbers_dir)?;

    KiCadCliBuilder::new()
        .command("pcb")
        .subcommand("export")
        .subcommand("gerbers")
        .arg("--output")
        .arg(gerbers_dir.to_string_lossy())
        .arg("--no-x2")
        .arg("--use-drill-file-origin")
        .arg(kicad_pcb_path.to_string_lossy())
        .run()
        .context("Failed to generate gerber files")?;

    // Generate drill files with PDF map
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
        .arg("--generate-map")
        .arg("--map-format")
        .arg("pdf")
        .arg(kicad_pcb_path.to_string_lossy())
        .run()
        .context("Failed to generate drill files")?;

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

    let kicad_pcb_path = info.staging_dir.join("layout").join("layout.kicad_pcb");

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

    let kicad_pcb_path = info.staging_dir.join("layout").join("layout.kicad_pcb");

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
    let mut zip = zip::ZipWriter::new(zip_file);

    for entry in fs::read_dir(gerbers_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() {
            let name = path.file_name().unwrap().to_string_lossy();
            zip.start_file(name, zip::write::FileOptions::<()>::default())?;
            let content = fs::read(&path)?;
            zip.write_all(&content)?;
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

    let kicad_pcb_path = info.staging_dir.join("layout").join("layout.kicad_pcb");
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

    let kicad_pcb_path = info.staging_dir.join("layout").join("layout.kicad_pcb");
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

    Ok(())
}

/// Generate STEP model
fn generate_step_model(info: &ReleaseInfo, _spinner: &Spinner) -> Result<()> {
    let models_dir = info.staging_dir.join("3d");
    fs::create_dir_all(&models_dir)?;

    let kicad_pcb_path = info.staging_dir.join("layout").join("layout.kicad_pcb");

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

    let kicad_pcb_path = info.staging_dir.join("layout").join("layout.kicad_pcb");

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

    let kicad_pcb_path = info.staging_dir.join("layout").join("layout.kicad_pcb");

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

    let kicad_pcb_path = info.staging_dir.join("layout").join("layout.kicad_pcb");

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
    // Find the .kicad_pcb file in the layout directory
    let layout_dir = info.staging_dir.join("layout");
    let kicad_pcb_path = std::fs::read_dir(&layout_dir)
        .context("Failed to read layout directory")?
        .filter_map(Result::ok)
        .find(|e| e.path().extension().and_then(|s| s.to_str()) == Some("kicad_pcb"))
        .map(|e| e.path())
        .ok_or_else(|| anyhow::anyhow!("No .kicad_pcb file found in {}", layout_dir.display()))?;

    // Run DRC checks and print results
    let (had_errors, warnings) =
        spinner.suspend(|| crate::drc::run_and_print_drc(&kicad_pcb_path, &info.suppress))?;

    // Handle errors - always fail if there are errors
    if had_errors {
        std::process::exit(1);
    }

    // Handle warnings - prompt user if there are warnings (unless --yes flag)
    if warnings > 0 && !info.yes {
        spinner.suspend(|| {
            // Non-interactive if stdin OR stdout is not a terminal
            if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
                eprintln!("{} DRC check failed with warnings", pcb_ui::icons::error());
                std::process::exit(1);
            }
            let confirmed = Confirm::new(&format!(
                "DRC completed with {} warning(s). Do you want to proceed with the release?",
                warnings
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
