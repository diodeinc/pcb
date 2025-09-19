use anyhow::{Context, Result};
use clap::{Args, ValueEnum};

use log::{debug, info, warn};
use pcb_kicad::{KiCadCliBuilder, PythonScriptBuilder};
use pcb_ui::{Colorize, Spinner, Style, StyledText};
use pcb_zen_core::config::get_workspace_info;
use pcb_zen_core::convert::ToSchematic;
use pcb_zen_core::DefaultFileProvider;
use pcb_zen_core::{EvalOutput, WithDiagnostics};

use crate::workspace::{gather_workspace_info, WorkspaceInfo};

use std::fs;
use std::io::Write;

use chrono::Utc;
use std::path::{Path, PathBuf};
use std::process::Command;

use zip::{write::FileOptions, ZipWriter};

use crate::vendor::sync_tracked_files;

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
}

/// All information gathered during the release preparation phase
pub struct ReleaseInfo {
    /// Common workspace information
    pub workspace: WorkspaceInfo,
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
}

type TaskFn = fn(&ReleaseInfo) -> Result<()>;

const BASE_TASKS: &[(&str, TaskFn)] = &[
    ("Copying source files and dependencies", copy_sources),
    ("Validating build from staged sources", validate_build),
    ("Copying layout files", copy_layout),
    ("Copying documentation", copy_docs),
    ("Substituting version variables", substitute_variables),
];

const MANUFACTURING_TASKS: &[(&str, TaskFn)] = &[
    ("Generating design BOM", generate_design_bom),
    ("Generating gerber files", generate_gerbers),
    ("Generating pick-and-place file", generate_cpl),
    ("Generating assembly drawings", generate_assembly_drawings),
    ("Generating ODB++ files", generate_odb),
    ("Generating 3D models", generate_3d_models),
];
const FINALIZATION_TASKS: &[(&str, TaskFn)] = &[
    ("Writing release metadata", write_metadata),
    ("Creating release archive", zip_release),
];

/// Execute a list of tasks with proper error handling and UI feedback
fn execute_tasks(info: &ReleaseInfo, tasks: &[(&str, TaskFn)]) -> Result<()> {
    for (name, task) in tasks {
        let spinner = Spinner::builder(*name).start();
        let res = task(info);

        spinner.finish();
        match res {
            Ok(()) => eprintln!("{} {name}", "✓".green()),
            Err(e) => {
                eprintln!("{} {name} failed", "✗".red());
                return Err(e.context(format!("{name} failed")));
            }
        }
    }
    Ok(())
}

pub fn execute(args: ReleaseArgs) -> Result<()> {
    // Gather all release information based on whether board or file was provided
    let release_info = {
        let info_spinner = Spinner::builder("Gathering release information").start();
        let info = if let Some(board_name) = args.board {
            gather_release_info(
                board_name,
                args.path,
                args.source_only,
                args.output_dir,
                args.output_name,
            )?
        } else if let Some(zen_file) = args.file {
            gather_release_info_from_file(
                zen_file,
                args.source_only,
                args.output_dir,
                args.output_name,
            )?
        } else {
            unreachable!("Either board or file must be provided due to clap validation")
        };
        info_spinner.finish();
        eprintln!("{} Release information gathered", "✓".green());
        info
    };

    // Execute base tasks
    execute_tasks(&release_info, BASE_TASKS)?;

    // Execute manufacturing tasks if full release
    if matches!(release_info.kind, ReleaseKind::Full) {
        execute_tasks(&release_info, MANUFACTURING_TASKS)?;
    }

    // Execute finalization tasks
    execute_tasks(&release_info, FINALIZATION_TASKS)?;

    // Calculate archive path
    let zip_path = archive_zip_path(&release_info);

    eprintln!(
        "{} {}",
        "✓".green().bold(),
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

/// Gather all information needed for the release using board name discovery
fn gather_release_info(
    board_name: String,
    path: Option<String>,
    source_only: bool,
    output_dir: Option<PathBuf>,
    output_name: Option<String>,
) -> Result<ReleaseInfo> {
    debug!("Starting release information gathering");

    // Get workspace info using discovery
    let start_path = path.as_deref().unwrap_or(".");
    let workspace_info = get_workspace_info(&DefaultFileProvider, Path::new(start_path))?;

    // Find the zen file for the given board
    let board_info = workspace_info.find_board_by_name(&board_name)?;

    let zen_path = board_info.absolute_zen_path(&workspace_info.root);

    // Use common workspace info gathering with the found zen file
    let workspace = gather_workspace_info(zen_path, true)?;

    // Use shared helper to build release info
    build_release_info(workspace, board_name, source_only, output_dir, output_name)
}

/// Build ReleaseInfo from workspace info and other parameters
fn build_release_info(
    workspace: WorkspaceInfo,
    board_name: String,
    source_only: bool,
    output_dir: Option<PathBuf>,
    output_name: Option<String>,
) -> Result<ReleaseInfo> {
    // Get version and git hash from git
    let (version, git_hash) = git_version_and_hash(&workspace.config.root, &board_name)?;

    // Create release staging directory in workspace root with flat structure:
    // Structure: {workspace_root}/.pcb/releases/{board_name}-{version}
    // Example: /workspace/.pcb/releases/test_board-f20ac95-dirty
    let staging_dir = workspace
        .config
        .root
        .join(".pcb/releases")
        .join(format!("{}-{}", board_name, version));

    // Determine output directory and name
    let default_output_dir = workspace.config.root.join(".pcb/releases");
    let output_dir = output_dir.unwrap_or(default_output_dir);
    let output_name = output_name.unwrap_or_else(|| {
        if source_only {
            format!("{}-{}.source.zip", board_name, version)
        } else {
            format!("{}-{}.zip", board_name, version)
        }
    });

    // Delete existing staging dir and recreate
    if staging_dir.exists() {
        fs::remove_dir_all(&staging_dir)?;
    }
    fs::create_dir_all(&staging_dir)?;

    // Extract layout path from evaluation
    let layout_path = extract_layout_path(&workspace.zen_path, &workspace.eval_result)?;

    let schematic = workspace
        .eval_result
        .output
        .as_ref()
        .map(|m| m.sch_module.to_schematic())
        .transpose()?
        .context("No schematic output from zen file")?;
    let kind = if source_only {
        ReleaseKind::SourceOnly
    } else {
        ReleaseKind::Full
    };

    Ok(ReleaseInfo {
        workspace,
        board_name,
        version,
        git_hash,
        staging_dir,
        layout_path,
        schematic,
        kind,
        output_dir,
        output_name,
    })
}

/// Gather all information needed for the release using direct zen file path
fn gather_release_info_from_file(
    zen_file_path: PathBuf,
    source_only: bool,
    output_dir: Option<PathBuf>,
    output_name: Option<String>,
) -> Result<ReleaseInfo> {
    debug!(
        "Starting release information gathering from file: {}",
        zen_file_path.display()
    );

    // Use common workspace info gathering with the zen file path
    let workspace = gather_workspace_info(zen_file_path, true)?;

    // Get board name from workspace or fallback to zen file stem
    let board_name = workspace.board_display_name();

    build_release_info(workspace, board_name, source_only, output_dir, output_name)
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
                .workspace
                .zen_path
                .strip_prefix(info.workspace.root())
                .unwrap_or(&info.workspace.zen_path)
                .display()
                .to_string();
            table.add_row(vec!["Zen File", &zen_file]);

            let staging_dir = info
                .staging_dir
                .strip_prefix(info.workspace.root())
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

/// Create the metadata JSON object (shared between display and file writing)
fn create_metadata_json(info: &ReleaseInfo) -> serde_json::Value {
    let source_only = matches!(info.kind, ReleaseKind::SourceOnly);
    let rfc3339_timestamp = Utc::now().to_rfc3339();

    serde_json::json!({
        "release": {
            "schema_version": RELEASE_SCHEMA_VERSION,
            "board_name": info.board_name,
            "git_version": info.version,
            "created_at": rfc3339_timestamp,
            "zen_file": info.workspace.zen_path.strip_prefix(info.workspace.root()).expect("zen_file must be within workspace_root"),
            "workspace_root": info.workspace.root(),
            "staging_directory": info.staging_dir,
            "layout_path": info.layout_path,
            "source_only": source_only
        },
        "system": {
            "user": std::env::var("USER").unwrap_or_else(|_| "unknown".to_string()),
            "platform": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "cli_version": env!("CARGO_PKG_VERSION"),
            "kicad_version": get_kicad_version()
        },
        "git": {
            "describe": info.version.clone(),
            "hash": info.git_hash.clone(),
            "workspace": info.workspace.root().display().to_string()
        }
    })
}

/// Determine release version using clean git-based logic:
/// - If working directory is dirty: {commit_hash}-dirty
/// - If current commit has a tag: {tag_name}
/// - If clean but no tag: {commit_hash}
fn git_version_and_hash(path: &Path, board_name: &str) -> Result<(String, String)> {
    debug!("Getting git version from: {}", path.display());

    // Check if working directory is dirty
    let status_out = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(path)
        .output()?;

    let is_dirty = status_out.status.success() && !status_out.stdout.is_empty();

    // Get current commit hash
    let commit_out = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(path)
        .output()?;

    if !commit_out.status.success() {
        warn!("Not a git repository, using 'unknown' as version and hash");
        return Ok(("unknown".into(), "unknown".into()));
    }

    let commit_hash = String::from_utf8(commit_out.stdout)?.trim().to_owned();

    // If dirty, return commit hash with dirty suffix
    if is_dirty {
        let version = format!("{commit_hash}-dirty");
        info!("Git version (dirty): {version}");
        return Ok((version, commit_hash.clone()));
    }

    // Check if current commit is tagged
    let tag_out = Command::new("git")
        .args(["tag", "--points-at", "HEAD"])
        .current_dir(path)
        .output()?;

    if tag_out.status.success() {
        let tags = String::from_utf8(tag_out.stdout)?;
        let tags: Vec<&str> = tags.lines().collect();

        // Look for board-specific tag in format "board_name/version" (case-insensitive board name)
        let tag_prefix = format!("{board_name}/");
        for tag in tags {
            if !tag.is_empty()
                && tag.len() > tag_prefix.len()
                && tag[..tag_prefix.len()].eq_ignore_ascii_case(&tag_prefix)
            {
                let version = tag[tag_prefix.len()..].to_string();
                info!("Git version (board tag): {version} for board {board_name}");
                return Ok((version, commit_hash.clone()));
            }
        }
    }

    // Not dirty and not tagged, use commit hash
    info!("Git version (commit): {commit_hash}");
    Ok((commit_hash.clone(), commit_hash))
}

/// Extract layout path from zen evaluation result
fn extract_layout_path(zen_path: &Path, eval: &WithDiagnostics<EvalOutput>) -> Result<PathBuf> {
    let output = eval
        .output
        .as_ref()
        .context("No output in evaluation result")?;
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
fn copy_sources(info: &ReleaseInfo) -> Result<()> {
    let output = info.workspace.eval_result.output.as_ref().unwrap();
    let tracked_files = output.core_resolver().unwrap().get_tracked_files();
    let workspace_root = info.workspace.root();
    let src_dir = info.staging_dir.join("src");
    let vendor_dir = src_dir.join("vendor");
    sync_tracked_files(&tracked_files, workspace_root, &vendor_dir, Some(&src_dir))?;
    Ok(())
}

/// Copy KiCad layout files
fn copy_layout(info: &ReleaseInfo) -> Result<()> {
    // If build directory doesn't exist, generate layout files first
    if !info.layout_path.exists() {
        pcb_layout::process_layout(&info.schematic, &info.workspace.zen_path)?;
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

/// Copy documentation files from docs directory adjacent to zen file
fn copy_docs(info: &ReleaseInfo) -> Result<()> {
    // Look for docs directory adjacent to zen file
    let docs_source_dir = info.workspace.zen_path.parent().unwrap().join("docs");

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
fn substitute_variables(info: &ReleaseInfo) -> Result<()> {
    debug!("Substituting version variables in KiCad files");

    // Determine display name of the board
    let board_name = info.workspace.board_display_name();

    // First, update the .kicad_pro file to ensure text variables are defined
    let kicad_pro_path = info.staging_dir.join("layout").join("layout.kicad_pro");
    update_kicad_pro_text_variables(&kicad_pro_path, &info.version, &info.git_hash, &board_name)?;

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
        git_hash = info.git_hash.replace('\'', "\\'"),
        board_name = board_name.replace('\'', "\\'")
    );

    PythonScriptBuilder::new(script)
        .arg(kicad_pcb_path.to_string_lossy())
        .run()?;
    debug!("Updated variables in: {}", kicad_pcb_path.display());
    Ok(())
}

/// Validate that the staged zen file can be built successfully
fn validate_build(info: &ReleaseInfo) -> Result<()> {
    // Calculate the zen file path in the staging directory
    let zen_file_rel = info
        .workspace
        .zen_path
        .strip_prefix(info.workspace.root())
        .context("Zen file must be within workspace root")?;
    let staged_zen_path = info.staging_dir.join("src").join(zen_file_rel);

    debug!("Validating build of: {}", staged_zen_path.display());

    // Use build function with strict settings - offline mode and deny warnings
    let mut has_errors = false;
    let _schematic = crate::build::build(
        &staged_zen_path,
        true, // offline mode since all dependencies should be vendored
        crate::build::create_diagnostics_passes(&["warnings".to_string()]), // deny all warnings
        &mut has_errors,
    );

    if has_errors {
        anyhow::bail!(
            "Build validation failed for staged zen file: {}",
            staged_zen_path.display()
        );
    }
    Ok(())
}

/// Generate design BOM JSON file
fn generate_design_bom(info: &ReleaseInfo) -> Result<()> {
    // Generate BOM entries from the schematic
    let bom = info.schematic.bom();

    // Create bom directory in staging
    let bom_dir = info.staging_dir.join("bom");
    fs::create_dir_all(&bom_dir)?;

    // Write design BOM as JSON
    let bom_file = bom_dir.join("design_bom.json");
    let mut file = fs::File::create(&bom_file)?;
    write!(file, "{}", bom.ungrouped_json())?;

    Ok(())
}

/// Write release metadata to JSON file
fn write_metadata(info: &ReleaseInfo) -> Result<()> {
    let metadata = create_metadata_json(info);
    let metadata_str = serde_json::to_string_pretty(&metadata)?;
    fs::write(info.staging_dir.join("metadata.json"), metadata_str)?;
    Ok(())
}

fn archive_zip_path(info: &ReleaseInfo) -> PathBuf {
    info.output_dir.join(&info.output_name)
}

/// Create zip archive of release staging directory
fn zip_release(info: &ReleaseInfo) -> Result<()> {
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
fn generate_gerbers(info: &ReleaseInfo) -> Result<()> {
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
        .arg("--layers")
        .arg("F.Cu,B.Cu,F.Paste,B.Paste,F.SilkS,B.SilkS,F.Mask,B.Mask,Edge.Cuts,In1.Cu,In2.Cu,In3.Cu,In4.Cu")
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
fn generate_cpl(info: &ReleaseInfo) -> Result<()> {
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
fn generate_assembly_drawings(info: &ReleaseInfo) -> Result<()> {
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
fn generate_odb(info: &ReleaseInfo) -> Result<()> {
    let manufacturing_dir = info.staging_dir.join("manufacturing");
    fs::create_dir_all(&manufacturing_dir)?;

    let kicad_pcb_path = info.layout_path.join("layout.kicad_pcb");
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

/// Generate 3D models (STEP, VRML, SVG)
fn generate_3d_models(info: &ReleaseInfo) -> Result<()> {
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
        .arg("--no-unspecified")
        .arg("--include-pads")
        .arg("--include-silkscreen")
        .arg("--include-soldermask")
        .arg(kicad_pcb_path.to_string_lossy())
        .log_file(devnull.try_clone()?)
        .suppress_error_output(true)
        .run();

    if let Err(e) = step_result {
        if step_path.exists() {
            warn!("KiCad CLI reported error but STEP file was created: {e}");
        } else {
            return Err(e).context("Failed to generate STEP model");
        }
    }

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
        .arg(kicad_pcb_path.to_string_lossy())
        .log_file(devnull.try_clone()?)
        .suppress_error_output(true)
        .run();

    if let Err(e) = wrl_result {
        if wrl_path.exists() {
            warn!("KiCad CLI reported error but VRML file was created: {e}");
        } else {
            return Err(e).context("Failed to generate VRML model");
        }
    }

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
        .log_file(devnull.try_clone()?)
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
