use anyhow::{Context, Result};
use clap::{Args, ValueEnum};

use log::{debug, info, warn};
use pcb_kicad::{KiCadCliBuilder, PythonScriptBuilder};
use pcb_sch::generate_bom_entries;
use pcb_ui::{Colorize, Spinner, Style, StyledText};
use pcb_zen_core::convert::ToSchematic;
use pcb_zen_core::{EvalOutput, WithDiagnostics};

use std::collections::HashSet;
use std::fs;
use std::io::Write;

use chrono::Utc;
use std::path::{Path, PathBuf};
use std::process::Command;

use zip::{write::FileOptions, ZipWriter};

use crate::workspace::{
    classify_file, gather_workspace_info, loadspec_to_vendor_path, FileClassification,
    WorkspaceInfo,
};

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
}

impl std::fmt::Display for ReleaseOutputFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReleaseOutputFormat::Human => write!(f, "human"),
            ReleaseOutputFormat::Json => write!(f, "json"),
        }
    }
}

#[derive(Args)]
pub struct ReleaseArgs {
    /// Path to .zen file to release
    pub zen_path: PathBuf,

    /// Output format
    #[arg(short, long, value_enum, default_value_t = ReleaseOutputFormat::Human)]
    pub format: ReleaseOutputFormat,

    /// Create source-only release without manufacturing artifacts
    #[arg(long)]
    pub source_only: bool,
}

/// All information gathered during the release preparation phase
pub struct ReleaseInfo {
    /// Common workspace information
    pub workspace: WorkspaceInfo,
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
}

type TaskFn = fn(&ReleaseInfo) -> Result<()>;

const BASE_TASKS: &[(&str, TaskFn)] = &[
    ("Copying source files and dependencies", copy_sources),
    ("Copying layout files", copy_layout),
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
fn execute_tasks(info: &ReleaseInfo, tasks: &[(&str, TaskFn)], human: bool) -> Result<()> {
    for (name, task) in tasks {
        let maybe_spinner = human.then(|| Spinner::builder(*name).start());
        let res = task(info);

        if let Some(spinner) = maybe_spinner {
            spinner.finish();
        }

        match res {
            Ok(()) if human => println!("{} {name}", "✓".green()),
            Err(e) => {
                if human {
                    println!("{} {name} failed", "✗".red());
                }
                return Err(e.context(format!("{name} failed")));
            }
            _ => {}
        }
    }
    Ok(())
}

pub fn execute(args: ReleaseArgs) -> Result<()> {
    let using_human = matches!(args.format, ReleaseOutputFormat::Human);

    // Gather all release information
    let release_info = if using_human {
        let info_spinner = Spinner::builder("Gathering release information").start();
        let info = gather_release_info(args.zen_path, args.source_only)?;
        info_spinner.finish();
        println!("{} Release information gathered", "✓".green());
        display_release_info(&info, args.source_only);
        info
    } else {
        gather_release_info(args.zen_path, args.source_only)?
    };

    // Execute base tasks
    execute_tasks(&release_info, BASE_TASKS, using_human)?;

    // Execute manufacturing tasks if full release
    if matches!(release_info.kind, ReleaseKind::Full) {
        execute_tasks(&release_info, MANUFACTURING_TASKS, using_human)?;
    }

    // Execute finalization tasks
    execute_tasks(&release_info, FINALIZATION_TASKS, using_human)?;

    // Calculate archive path
    let zip_path = archive_zip_path(&release_info);

    info!("Release {} staged successfully", release_info.version);

    if using_human {
        println!();
        println!(
            "{} {}",
            "✓".green().bold(),
            format!("Release {} staged successfully", release_info.version).bold()
        );
        println!("Archive: {}", zip_path.with_style(Style::Cyan));
    } else {
        let output = serde_json::json!({
            "archive": zip_path,
            "staging_directory": release_info.staging_dir,
            "version": release_info.version,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    }

    Ok(())
}

/// Gather all information needed for the release
fn gather_release_info(zen_path: PathBuf, source_only: bool) -> Result<ReleaseInfo> {
    debug!("Starting release information gathering");

    // Use common workspace info gathering
    let workspace = gather_workspace_info(zen_path, true)?;

    // Get board name from workspace info with fallback to zen filename
    let board_name = workspace.board_name().unwrap_or_else(|| {
        workspace
            .zen_path
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .to_string()
    });
    // Get version and git hash from git
    let (version, git_hash) = git_version_and_hash(&workspace.config.root, &board_name)?;

    // Create release staging directory in workspace root:
    // Structure: {workspace_root}/.pcb/releases/{board_name}/{version}
    // Example: /workspace/.pcb/releases/test_board/f20ac95-dirty
    let staging_dir = workspace
        .config
        .root
        .join(".pcb/releases")
        .join(&board_name)
        .join(&version);

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
        version,
        git_hash,
        staging_dir,
        layout_path,
        schematic,
        kind,
    })
}

/// Display all the gathered release information
fn display_release_info(info: &ReleaseInfo, _source_only: bool) {
    println!();
    let release_type = match info.kind {
        ReleaseKind::SourceOnly => "Source-Only Release",
        ReleaseKind::Full => "Full Release",
    };
    println!(
        "{}",
        format!("{release_type} Metadata")
            .with_style(Style::Blue)
            .bold()
    );

    // Create and display the metadata that will be saved
    let metadata = create_metadata_json(info);
    println!(
        "{}",
        serde_json::to_string_pretty(&metadata).unwrap_or_default()
    );

    info!(
        "Release info gathered - zen: {}, workspace: {}, version: {}",
        info.workspace.zen_path.display(),
        info.workspace.root().display(),
        info.version
    );
}

/// Create the metadata JSON object (shared between display and file writing)
fn create_metadata_json(info: &ReleaseInfo) -> serde_json::Value {
    let source_only = matches!(info.kind, ReleaseKind::SourceOnly);
    let rfc3339_timestamp = Utc::now().to_rfc3339();

    serde_json::json!({
        "release": {
            "schema_version": RELEASE_SCHEMA_VERSION,
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
            "cli_version": env!("CARGO_PKG_VERSION")
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
    let mut vendor_files = HashSet::new();

    // Copy pcb.toml from workspace root if it exists
    let pcb_toml_path = info.workspace.root().join("pcb.toml");
    if pcb_toml_path.exists() {
        let dest_path = info.staging_dir.join("src").join("pcb.toml");
        if let Some(parent) = dest_path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create parent directory: {}", parent.display())
            })?;
        }
        fs::copy(&pcb_toml_path, &dest_path).with_context(|| {
            format!(
                "Failed to copy pcb.toml: {} -> {}",
                pcb_toml_path.display(),
                dest_path.display()
            )
        })?;
    }

    for path in info.workspace.resolver.get_tracked_files() {
        match classify_file(info.workspace.root(), &path, &info.workspace.resolver) {
            FileClassification::Local(rel) => {
                let dest_path = info.staging_dir.join("src").join(rel);
                if let Some(parent) = dest_path.parent() {
                    fs::create_dir_all(parent).with_context(|| {
                        format!("Failed to create parent directory: {}", parent.display())
                    })?;
                }
                fs::copy(&path, &dest_path).with_context(|| {
                    format!(
                        "Failed to copy {} -> {}",
                        path.display(),
                        dest_path.display()
                    )
                })?;
            }
            FileClassification::Vendor(load_spec) => {
                let vendor_path = loadspec_to_vendor_path(&load_spec)?;
                if vendor_files.insert(vendor_path.clone()) {
                    let dest_path = info
                        .staging_dir
                        .join("src")
                        .join("vendor")
                        .join(&vendor_path);
                    if let Some(parent) = dest_path.parent() {
                        fs::create_dir_all(parent).with_context(|| {
                            format!("Failed to create parent directory: {}", parent.display())
                        })?;
                    }
                    fs::copy(&path, &dest_path).with_context(|| {
                        format!(
                            "Failed to copy {} -> {}",
                            path.display(),
                            dest_path.display()
                        )
                    })?;
                }
            }
            FileClassification::Irrelevant => {}
        }
    }
    Ok(())
}

/// Copy KiCad layout files
fn copy_layout(info: &ReleaseInfo) -> Result<()> {
    let build_dir = info.layout_path.parent().unwrap_or(&info.layout_path);

    // If build directory doesn't exist, generate layout files first
    if !build_dir.exists() {
        pcb_layout::process_layout(&info.schematic, &info.workspace.zen_path)?;
    }

    let layout_staging_dir = info.staging_dir.join("layout");
    fs::create_dir_all(&layout_staging_dir)?;

    for entry in walkdir::WalkDir::new(build_dir)
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

/// Ensure text variables are defined in .kicad_pro file
fn update_kicad_pro_text_variables(
    kicad_pro_path: &Path,
    version: &str,
    git_hash: &str,
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

    // Only modify if we need to add missing variables
    if needs_pcb_version || needs_pcb_git_hash {
        // Ensure text_variables object exists
        if project.get("text_variables").is_none() || !project["text_variables"].is_object() {
            project["text_variables"] = serde_json::json!({});
        }

        let text_vars = project["text_variables"].as_object_mut().unwrap();

        // Add missing variables with correct values
        if needs_pcb_version {
            text_vars.insert(
                "PCB_VERSION".to_string(),
                serde_json::Value::String(version.to_string()),
            );
        }
        if needs_pcb_git_hash {
            text_vars.insert(
                "PCB_GIT_HASH".to_string(),
                serde_json::Value::String(git_hash.to_string()),
            );
        }

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

/// Substitute version and git hash variables in KiCad PCB files
fn substitute_variables(info: &ReleaseInfo) -> Result<()> {
    debug!("Substituting version variables in KiCad files");

    // First, update the .kicad_pro file to ensure text variables are defined
    let kicad_pro_path = info.staging_dir.join("layout").join("layout.kicad_pro");
    update_kicad_pro_text_variables(&kicad_pro_path, &info.version, &info.git_hash)?;

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

# Save the board
board.Save(sys.argv[1])
print("Text variables updated successfully")
"#,
        version = info.version.replace('\'', "\\'"), // Escape single quotes
        git_hash = info.git_hash.replace('\'', "\\'")  // Escape single quotes
    );

    PythonScriptBuilder::new(script)
        .arg(kicad_pcb_path.to_string_lossy())
        .run()?;
    debug!("Updated variables in: {}", kicad_pcb_path.display());
    Ok(())
}

/// Generate design BOM JSON file
fn generate_design_bom(info: &ReleaseInfo) -> Result<()> {
    // Generate BOM entries from the schematic
    let bom_entries = generate_bom_entries(&mut info.schematic.clone());

    // Create bom directory in staging
    let bom_dir = info.staging_dir.join("bom");
    fs::create_dir_all(&bom_dir)?;

    // Write design BOM as JSON
    let bom_file = bom_dir.join("design_bom.json");
    let file = fs::File::create(&bom_file)?;
    serde_json::to_writer_pretty(file, &bom_entries)?;

    Ok(())
}

/// Write release metadata to JSON file
fn write_metadata(info: &ReleaseInfo) -> Result<()> {
    let metadata = create_metadata_json(info);
    let metadata_str = serde_json::to_string_pretty(&metadata)?;
    fs::write(info.staging_dir.join("metadata.json"), metadata_str)?;
    Ok(())
}

fn archive_zip_path(info: &ReleaseInfo) -> String {
    if matches!(info.kind, ReleaseKind::SourceOnly) {
        format!("{}.source.zip", info.staging_dir.display())
    } else {
        format!("{}.zip", info.staging_dir.display())
    }
}

/// Create zip archive of release staging directory
fn zip_release(info: &ReleaseInfo) -> Result<()> {
    let zip_path = archive_zip_path(info);
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
