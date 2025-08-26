use crate::workspace::gather_workspace_info;
use anyhow::{Context, Result};
use clap::Args;
use log::debug;
use pcb_ui::{Colorize, Spinner, Style, StyledText};
use pcb_zen_core::LoadSpec;
use pcb_zen_core::{config::find_workspace_root, DefaultFileProvider};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

#[derive(Args)]
pub struct VendorArgs {
    /// Path to .zen file or directory to analyze for dependencies.
    /// If a directory, will search recursively for .zen files.
    pub zen_path: PathBuf,

    /// Check if vendor directory is up-to-date (useful for CI)
    #[arg(long)]
    pub check: bool,
}

pub fn execute(args: VendorArgs) -> Result<()> {
    let zen_path = args.zen_path.canonicalize()?;
    let workspace_root = find_workspace_root(&DefaultFileProvider, &zen_path).canonicalize()?;
    if !workspace_root.join("pcb.toml").exists() {
        anyhow::bail!(
            "No pcb.toml found in workspace. \
            Specify a path within a workspace that contains pcb.toml.",
        );
    }

    // Discover zen files to process
    let discovery_spinner = Spinner::builder("Discovering zen files").start();
    let zen_files = discover_zen_files(&zen_path)?;
    discovery_spinner.finish();
    println!(
        "{} Found {} zen files to analyze",
        "✓".green(),
        zen_files.len()
    );

    // Gather vendor information from all zen files
    let info_spinner = Spinner::builder("Analyzing dependencies").start();
    let zen_files_count = zen_files.len();
    let tracked_files = gather_vendor_info(zen_files)?;
    let vendor_dir = workspace_root.join("vendor");
    info_spinner.finish();
    println!("{} Dependencies analyzed", "✓".green());

    // Handle check mode for CI
    if args.check {
        let check_spinner = Spinner::builder("Checking vendor directory").start();
        debug!("Checking vendor directory: {}", vendor_dir.display());
        let is_up_to_date = check_vendor_directory(&tracked_files, &workspace_root, &vendor_dir)?;
        check_spinner.finish();

        if is_up_to_date {
            println!("{} Vendor directory is up-to-date", "✓".green());
            return Ok(());
        } else {
            println!("{} Vendor directory is out-of-date", "✗".red());
            anyhow::bail!("Vendor directory is not up-to-date. Run 'pcb vendor' to update it.");
        }
    }

    // Create vendor directory
    let _ = fs::remove_dir_all(&vendor_dir);
    fs::create_dir_all(&vendor_dir)?;

    // Copy vendor dependencies
    let vendor_spinner = Spinner::builder("Copying vendor dependencies").start();
    let vendor_count = sync_tracked_files(&tracked_files, &workspace_root, &vendor_dir, None)?;
    vendor_spinner.finish();

    println!();
    println!(
        "{} {}",
        "✓".green().bold(),
        format!("Vendored {vendor_count} dependencies from {zen_files_count} designs").bold()
    );
    println!(
        "Vendor directory: {}",
        vendor_dir.display().to_string().with_style(Style::Cyan)
    );

    Ok(())
}

/// Discover zen files to process
fn discover_zen_files(path: &Path) -> Result<Vec<PathBuf>> {
    let mut zen_files = Vec::new();

    if path.is_file() {
        // Verify it's a zen file
        if path.extension().and_then(|ext| ext.to_str()) == Some("zen") {
            zen_files.push(path.to_path_buf());
        } else {
            anyhow::bail!("Not a zen file: {}", path.display());
        }
    } else if path.is_dir() {
        // Search directory for zen files
        zen_files.extend(find_zen_files_in_directory(path)?);
    } else {
        anyhow::bail!("Path does not exist: {}", path.display());
    }

    if zen_files.is_empty() {
        anyhow::bail!("No zen files found in search paths");
    }

    Ok(zen_files)
}

/// Find zen files in a directory, applying smart filtering including .gitignore
fn find_zen_files_in_directory(dir: &std::path::Path) -> Result<Vec<PathBuf>> {
    let mut zen_files = Vec::new();

    // Configure ignore walker to skip vendor and other common directories
    let mut builder = ignore::WalkBuilder::new(dir);
    builder
        .follow_links(false)
        .add_custom_ignore_filename(".pcbignore") // Custom ignore file for PCB-specific exclusions
        .filter_entry(|entry| {
            // Additional filtering for directories that shouldn't contain source zen files
            if let Some(file_name) = entry.file_name().to_str() {
                // Always skip vendor directory to avoid recursive dependencies
                if file_name == "vendor" {
                    debug!("Skipping vendor directory: {}", entry.path().display());
                    return false;
                }
                // Skip other common build/cache directories not typically in .gitignore
                if matches!(file_name, ".pcb" | "target" | "build" | "dist" | "out") {
                    debug!("Skipping build directory: {}", entry.path().display());
                    return false;
                }
            }
            true
        });

    // Use the configured walker with simplified filtering
    for entry in builder
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_some_and(|ft| ft.is_file()))
    {
        let path = entry.into_path();

        // Check if it's a zen file
        if path.extension().and_then(|ext| ext.to_str()) != Some("zen") {
            continue;
        }

        // Skip hidden files
        if path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with('.'))
        {
            continue;
        }

        zen_files.push(path);
    }

    if zen_files.is_empty() {
        anyhow::bail!("No zen files found in directory: {}", dir.display());
    }

    debug!(
        "Found {} zen files in {}: {:?}",
        zen_files.len(),
        dir.display(),
        zen_files
    );
    Ok(zen_files)
}

/// Gather and aggregate vendor information from multiple zen files
fn gather_vendor_info(zen_files: Vec<PathBuf>) -> Result<HashMap<PathBuf, LoadSpec>> {
    if zen_files.is_empty() {
        anyhow::bail!("No zen files to process");
    }
    // Evaluate each zen file and collect tracked files
    let mut tracked_files: HashMap<PathBuf, LoadSpec> = HashMap::default();
    for zen_file in &zen_files {
        // Don't use the vendor path for the workspace info, we're just gathering dependencies
        let workspace_info = gather_workspace_info(zen_file.clone(), false)?;
        tracked_files.extend(workspace_info.resolver.get_tracked_files());
    }
    Ok(tracked_files)
}

pub fn sync_tracked_files(
    tracked_files: &HashMap<PathBuf, LoadSpec>,
    workspace_root: &Path,
    vendor_dir: &Path,
    src_dir: Option<&Path>,
) -> Result<usize> {
    let mut synced_files = 0;
    for (path, load_spec) in tracked_files {
        let dest_path = if load_spec.is_remote() {
            // remote file
            vendor_dir.join(load_spec.vendor_path()?)
        } else {
            // local file
            let Some(src_dir) = src_dir else {
                // no src dir was provided, so skip local files
                continue;
            };
            let Ok(rel_path) = path.strip_prefix(workspace_root) else {
                anyhow::bail!("Failed to strip prefix from path: {}", path.display())
            };
            src_dir.join(rel_path)
        };
        log::info!(
            "Syncing file: {} to {}",
            path.display(),
            dest_path.display()
        );
        if path.is_file() {
            let parent = dest_path.parent().unwrap();
            fs::create_dir_all(parent)?;
            fs::copy(path, dest_path)?;
            synced_files += 1;
        } else {
            synced_files += copy_dir_all(path, dest_path)?;
        }
    }
    Ok(synced_files)
}

fn copy_dir_all(src: impl AsRef<Path>, dst: impl AsRef<Path>) -> std::io::Result<usize> {
    fs::create_dir_all(&dst)?;
    let mut synced_files = 0;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            synced_files += copy_dir_all(entry.path(), dst.as_ref().join(entry.file_name()))?;
        } else {
            fs::copy(entry.path(), dst.as_ref().join(entry.file_name()))?;
            synced_files += 1;
        }
    }
    Ok(synced_files)
}

/// Check if vendor directory is up-to-date by vendoring to a temp directory and comparing
fn check_vendor_directory(
    tracked_files: &HashMap<PathBuf, LoadSpec>,
    workspace_root: &Path,
    vendor_dir: &Path,
) -> Result<bool> {
    // If vendor directory doesn't exist, it's not up-to-date
    if !vendor_dir.exists() {
        debug!("Vendor directory does not exist: {}", vendor_dir.display());
        return Ok(false);
    }

    // Create temporary directory for comparison
    let temp_dir = TempDir::new().context("Failed to create temporary directory")?;
    let temp_vendor_dir = temp_dir.path().join("vendor");
    fs::create_dir_all(&temp_vendor_dir)?;

    sync_tracked_files(tracked_files, workspace_root, &temp_vendor_dir, None)?;

    // Compare temp directory with actual vendor directory using dir-diff
    let are_different = dir_diff::is_different(&temp_vendor_dir, vendor_dir)
        .context("Failed to compare vendor directories")?;

    if are_different {
        debug!(
            "Vendor directory differs from expected (temp: {}, actual: {})",
            temp_vendor_dir.display(),
            vendor_dir.display()
        );
        Ok(false)
    } else {
        debug!("Vendor directory matches expected content");
        Ok(true)
    }
}
