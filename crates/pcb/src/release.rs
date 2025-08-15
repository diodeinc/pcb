use anyhow::{Context, Result};
use clap::Args;

use log::{debug, info, warn};
use pcb_ui::{Colorize, Spinner, Style, StyledText};
use pcb_zen::load::{cache_dir, DefaultRemoteFetcher};

use pcb_sch::generate_bom as generate_bom_entries;
use pcb_zen_core::convert::ToSchematic;
use pcb_zen_core::{
    CoreLoadResolver, DefaultFileProvider, EvalContext, EvalOutput, InputMap, LoadSpec,
    WithDiagnostics,
};

use std::collections::HashSet;
use std::fs;

use chrono::Utc;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use zip::{write::FileOptions, ZipWriter};

use crate::tracking_resolver::TrackingLoadResolver;

#[derive(Args)]
pub struct ReleaseArgs {
    /// Path to .zen file to release
    pub zen_path: PathBuf,
}

/// All information gathered during the release preparation phase
pub struct ReleaseInfo {
    /// Canonical path to the .zen file being released
    pub zen_path: PathBuf,
    /// Root directory of the workspace
    pub workspace_root: PathBuf,
    /// Release version (from git or fallback)
    pub version: String,
    /// Path to the staging directory where release will be assembled
    pub staging_dir: PathBuf,
    /// Path to the layout directory containing KiCad files
    pub layout_path: PathBuf,
    /// Dependency tracker for finding all referenced files
    pub tracker: Arc<TrackingLoadResolver>,
    /// Evaluated schematic from the zen file
    pub schematic: pcb_sch::Schematic,
}

type TaskFn = fn(&ReleaseInfo) -> Result<()>;

const TASKS: &[(&str, TaskFn)] = &[
    ("Copying source files and dependencies", copy_sources),
    ("Copying layout files", copy_layout),
    ("Generating unmatched BOM", generate_unmatched_bom),
    ("Writing release metadata", write_metadata),
    ("Creating release archive", zip_release),
];

pub fn execute(args: ReleaseArgs) -> Result<()> {
    // Gather all release information
    let info_spinner = Spinner::builder("Gathering release information").start();
    let release_info = gather_release_info(args.zen_path)?;
    info_spinner.finish();
    println!("{} Release information gathered", "✓".green());

    display_release_info(&release_info);

    // Execute release tasks
    for (name, task) in TASKS {
        let spinner = Spinner::builder(*name).start();
        match task(&release_info) {
            Ok(()) => {
                spinner.finish();
                println!("{} {name}", "✓".green());
            }
            Err(e) => {
                spinner.finish();
                println!("{} {name} failed", "✗".red());
                return Err(e.context(format!("{name} failed")));
            }
        }
    }

    // Calculate zip path
    let zip_path = format!("{}.zip", release_info.staging_dir.display());

    info!("Release {} staged successfully", release_info.version);
    println!();
    println!(
        "{} {}",
        "✓".green().bold(),
        format!("Release {} staged successfully", release_info.version).bold()
    );
    println!("Archive: {}", zip_path.with_style(Style::Cyan));

    Ok(())
}

/// Gather all information needed for the release
fn gather_release_info(zen_path: PathBuf) -> Result<ReleaseInfo> {
    debug!("Starting release information gathering");

    // Canonicalize the zen path
    let zen_path = zen_path
        .canonicalize()
        .with_context(|| format!("Failed to canonicalize zen path: {}", zen_path.display()))?;

    // Try to find workspace root by walking up for pcb.toml
    let initial_workspace_root = find_workspace_root(&zen_path)?;

    // Evaluate the zen file and track dependencies
    let (tracker, eval) = eval_zen_entrypoint(&zen_path, &initial_workspace_root)?;

    // Refine workspace root based on tracked files if no pcb.toml was found
    let workspace_root = if initial_workspace_root.join("pcb.toml").exists() {
        initial_workspace_root
    } else {
        // No pcb.toml found, use common ancestor of tracked files
        detect_workspace_root_from_files(&zen_path, &tracker.files())?
    };

    // Log workspace root info for debugging
    info!("Using workspace root: {}", workspace_root.display());

    // Get version from git
    let version = git_describe(&workspace_root)?;

    // Create release staging directory in workspace root:
    // Structure: {workspace_root}/.pcb/releases/{relative_path_to_zen}/{board_name}/{version}
    // Example: /workspace/.pcb/releases/boards/TestBoard/TestBoard/f20ac95-dirty
    let zen_relative_path = zen_path.strip_prefix(&workspace_root)?;
    let zen_dir = zen_relative_path
        .parent()
        .context("Zen file must have a parent directory")?;
    let board_name = zen_path.file_stem().context("Zen file must have a name")?;
    let staging_dir = workspace_root
        .join(".pcb/releases")
        .join(zen_dir)
        .join(board_name)
        .join(&version);

    // Delete existing staging dir and recreate
    if staging_dir.exists() {
        fs::remove_dir_all(&staging_dir)?;
    }
    fs::create_dir_all(&staging_dir)?;

    // Extract layout path from evaluation
    let layout_path = extract_layout_path(&zen_path, &eval)?;

    let schematic = eval
        .output
        .map(|m| m.sch_module.to_schematic())
        .transpose()?
        .context("No schematic output from zen file")?;

    Ok(ReleaseInfo {
        zen_path,
        workspace_root,
        version,
        staging_dir,
        layout_path,
        tracker,
        schematic,
    })
}

/// Display all the gathered release information
fn display_release_info(info: &ReleaseInfo) {
    println!();
    println!("{}", "Release Metadata".with_style(Style::Blue).bold());

    // Create and display the metadata that will be saved
    let metadata = create_metadata_json(info);
    println!(
        "{}",
        serde_json::to_string_pretty(&metadata).unwrap_or_default()
    );

    info!(
        "Release info gathered - zen: {}, workspace: {}, version: {}",
        info.zen_path.display(),
        info.workspace_root.display(),
        info.version
    );
}

/// Create the metadata JSON object (shared between display and file writing)
fn create_metadata_json(info: &ReleaseInfo) -> serde_json::Value {
    let rfc3339_timestamp = Utc::now().to_rfc3339();

    serde_json::json!({
        "release": {
            "version": info.version,
            "created_at": rfc3339_timestamp,
            "zen_file": info.zen_path,
            "workspace_root": info.workspace_root,
            "staging_directory": info.staging_dir,
            "layout_path": info.layout_path
        },
        "system": {
            "user": std::env::var("USER").unwrap_or_else(|_| "unknown".to_string()),
            "platform": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "cli_version": env!("CARGO_PKG_VERSION")
        },
        "git": {
            "describe": info.version.clone(),
            "workspace": info.workspace_root.display().to_string()
        }
    })
}

/// Find workspace root by walking up from entry file to find pcb.toml
fn find_workspace_root(entry: &Path) -> Result<PathBuf> {
    let mut current_dir = entry.parent().unwrap_or(entry);
    loop {
        if current_dir.join("pcb.toml").exists() {
            return Ok(current_dir.to_path_buf());
        }
        if let Some(parent) = current_dir.parent() {
            current_dir = parent;
        } else {
            // Reached filesystem root without finding pcb.toml
            break;
        }
    }

    // Fallback: Use entry's parent as initial workspace root
    let parent = entry
        .parent()
        .context("Entry file has no parent directory")?;
    Ok(parent.to_path_buf())
}

/// Detect workspace root from tracked files when no pcb.toml is found
fn detect_workspace_root_from_files(entry: &Path, tracked: &HashSet<PathBuf>) -> Result<PathBuf> {
    let cache_root = cache_dir()?.canonicalize()?;

    let mut paths: Vec<PathBuf> = tracked
        .iter()
        .filter_map(|p| p.canonicalize().ok())
        .filter(|p| !p.starts_with(&cache_root))
        .collect();

    paths.push(entry.canonicalize()?);

    let root = paths
        .into_iter()
        .reduce(|a, b| {
            a.components()
                .zip(b.components())
                .take_while(|(x, y)| x == y)
                .map(|(c, _)| c.as_os_str())
                .collect()
        })
        .context("No paths found for workspace root calculation")?;

    Ok(root)
}

/// Run the Starlark interpreter once and return both the tracker (for copying)
/// and the evaluation output (for KiCad extraction).
fn eval_zen_entrypoint(
    entry: &Path,
    workspace_root: &Path,
) -> Result<(Arc<TrackingLoadResolver>, WithDiagnostics<EvalOutput>)> {
    debug!("Starting zen file evaluation: {}", entry.display());

    let file_provider = Arc::new(DefaultFileProvider);

    let remote_fetcher = Arc::new(DefaultRemoteFetcher);
    let base_resolver = Arc::new(CoreLoadResolver::new(
        file_provider.clone(),
        remote_fetcher,
        Some(workspace_root.to_path_buf()),
    ));

    let tracking_resolver = Arc::new(TrackingLoadResolver::new(
        base_resolver,
        file_provider.clone(),
    ));

    // Pre-seed with the entrypoint itself
    tracking_resolver.track(entry.to_path_buf());

    let eval_context = EvalContext::new()
        .set_file_provider(file_provider.clone())
        .set_load_resolver(tracking_resolver.clone())
        .set_source_path(entry.to_path_buf())
        .set_inputs(InputMap::new());

    let eval_result = eval_context.eval();

    // Check for errors and bail if evaluation failed
    if !eval_result.is_success() {
        let errors: Vec<String> = eval_result
            .diagnostics
            .iter()
            .filter(|d| d.is_error())
            .map(|d| d.to_string())
            .collect();
        if !errors.is_empty() {
            anyhow::bail!("Zen file evaluation failed:\n{}", errors.join("\n"));
        }
    }

    info!("Zen file evaluation completed successfully");
    Ok((tracking_resolver, eval_result))
}

/// Determine release version using clean git-based logic:
/// - If working directory is dirty: {commit_hash}-dirty
/// - If current commit has a tag: {tag_name}
/// - If clean but no tag: {commit_hash}
fn git_describe(path: &Path) -> Result<String> {
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
        warn!("Not a git repository, using 'unknown' as version");
        return Ok("unknown".into());
    }

    let commit_hash = String::from_utf8(commit_out.stdout)?.trim().to_owned();

    // If dirty, return commit hash with dirty suffix
    if is_dirty {
        let version = format!("{commit_hash}-dirty");
        info!("Git version (dirty): {version}");
        return Ok(version);
    }

    // Check if current commit is tagged
    let tag_out = Command::new("git")
        .args(["tag", "--points-at", "HEAD"])
        .current_dir(path)
        .output()?;

    if tag_out.status.success() {
        let tags = String::from_utf8(tag_out.stdout)?;
        let tags: Vec<&str> = tags.lines().collect();

        // Use the first tag if any exist
        if let Some(tag) = tags.first() {
            if !tag.is_empty() {
                let version = tag.to_string();
                info!("Git version (tag): {version}");
                return Ok(version);
            }
        }
    }

    // Not dirty and not tagged, use commit hash
    info!("Git version (commit): {commit_hash}");
    Ok(commit_hash)
}

#[derive(Debug)]
enum SrcKind<'a> {
    Local(&'a Path),
    Vendor,
    Irrelevant,
}

fn classify_file<'a>(
    workspace_root: &Path,
    path: &'a Path,
    tracker: &TrackingLoadResolver,
) -> SrcKind<'a> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default();

    let is_zen = ext == "zen";
    let is_kicad = matches!(ext, "kicad_mod" | "kicad_sym") || ext.starts_with("kicad_");

    if !(is_zen || is_kicad) {
        return SrcKind::Irrelevant;
    }

    // Use proper path comparison instead of string matching
    if path.starts_with(workspace_root) {
        if let Ok(rel) = path.strip_prefix(workspace_root) {
            debug!(
                "Classified as local: {} (relative: {})",
                path.display(),
                rel.display()
            );
            SrcKind::Local(rel)
        } else {
            SrcKind::Irrelevant
        }
    } else if tracker.get_load_spec(path).is_some() {
        debug!("Classified as vendor: {}", path.display());
        SrcKind::Vendor
    } else {
        debug!(
            "Classified as irrelevant: {} (outside workspace, no LoadSpec)",
            path.display()
        );
        SrcKind::Irrelevant
    }
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

    for path in info.tracker.files() {
        match classify_file(&info.workspace_root, &path, &info.tracker) {
            SrcKind::Local(rel) => {
                let dest_path = info.staging_dir.join("src").join(rel);
                if let Some(parent) = dest_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::copy(&path, &dest_path)?;
            }
            SrcKind::Vendor => {
                let load_spec = info
                    .tracker
                    .get_load_spec(&path)
                    .context("Vendor file must have LoadSpec")?;
                let vendor_path = loadspec_to_vendor_path(&load_spec)?;
                if vendor_files.insert(vendor_path.clone()) {
                    let dest_path = info
                        .staging_dir
                        .join("src")
                        .join("vendor")
                        .join(&vendor_path);
                    if let Some(parent) = dest_path.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    fs::copy(&path, &dest_path)?;
                }
            }
            SrcKind::Irrelevant => {}
        }
    }
    Ok(())
}

fn loadspec_to_vendor_path(spec: &LoadSpec) -> Result<PathBuf> {
    // Resolve package aliases to canonical git form
    let canonical_spec = match spec {
        LoadSpec::Package { .. } => spec
            .resolve(None, None)
            .context("Failed to resolve package alias to canonical form")?,
        _ => spec.clone(),
    };

    // Convert canonical spec to vendor path
    match canonical_spec {
        LoadSpec::Github {
            user,
            repo,
            rev,
            path,
        } => Ok(PathBuf::from("github.com")
            .join(user)
            .join(repo)
            .join(rev)
            .join(path)),
        LoadSpec::Gitlab {
            project_path,
            rev,
            path,
        } => Ok(PathBuf::from("gitlab.com")
            .join(project_path)
            .join(rev)
            .join(path)),
        LoadSpec::Package { package, tag, path } => {
            warn!("Package spec not resolved to canonical form: {package}");
            Ok(PathBuf::from("packages").join(package).join(tag).join(path))
        }
        LoadSpec::Path { .. } | LoadSpec::WorkspacePath { .. } => {
            anyhow::bail!("Local path specs should not reach vendor handling - indicates a classification bug")
        }
    }
}

/// Copy KiCad layout files
fn copy_layout(info: &ReleaseInfo) -> Result<()> {
    let build_dir = info.layout_path.parent().unwrap_or(&info.layout_path);

    // If build directory doesn't exist, generate layout files first
    if !build_dir.exists() {
        pcb_layout::process_layout(&info.schematic, &info.zen_path)?;
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

/// Generate unmatched BOM JSON file
fn generate_unmatched_bom(info: &ReleaseInfo) -> Result<()> {
    // Generate BOM entries from the schematic
    let bom_entries = generate_bom_entries(&info.schematic);

    // Create bom directory in staging
    let bom_dir = info.staging_dir.join("bom");
    fs::create_dir_all(&bom_dir)?;

    // Write unmatched BOM as JSON
    let bom_file = bom_dir.join("unmatched.json");
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

/// Create zip archive of release staging directory
fn zip_release(info: &ReleaseInfo) -> Result<()> {
    let zip_path = format!("{}.zip", info.staging_dir.display());
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
