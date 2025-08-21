use crate::workspace::{gather_workspace_info, is_vendor_dependency, loadspec_to_vendor_path};
use anyhow::{Context, Result};
use clap::Args;
use log::{debug, info};
use pcb_ui::{Colorize, Spinner, Style, StyledText};
use pcb_zen_core::{workspace::find_workspace_root, DefaultFileProvider};
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

/// Information needed for vendoring dependencies from multiple designs
pub struct VendorInfo {
    /// Path to the vendor directory in workspace  
    pub vendor_dir: PathBuf,
    /// Dependencies: vendor path -> source file path
    pub dependencies: HashMap<PathBuf, PathBuf>,
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
    let vendor_info = gather_vendor_info(zen_files, workspace_root)?;
    info_spinner.finish();
    println!("{} Dependencies analyzed", "✓".green());

    // Handle check mode for CI
    if args.check {
        let check_spinner = Spinner::builder("Checking vendor directory").start();
        debug!(
            "Checking vendor directory: {}",
            vendor_info.vendor_dir.display()
        );
        let is_up_to_date = check_vendor_directory(&vendor_info)?;
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
    let _ = fs::remove_dir_all(&vendor_info.vendor_dir);
    fs::create_dir_all(&vendor_info.vendor_dir)?;

    // Copy vendor dependencies
    let vendor_spinner = Spinner::builder("Copying vendor dependencies").start();
    let vendor_count = copy_vendor_dependencies(&vendor_info)?;
    vendor_spinner.finish();

    info!(
        "Vendored {} dependencies to {}",
        vendor_count,
        vendor_info.vendor_dir.display()
    );
    println!();
    println!(
        "{} {}",
        "✓".green().bold(),
        format!("Vendored {vendor_count} dependencies from {zen_files_count} designs").bold()
    );
    println!(
        "Vendor directory: {}",
        vendor_info
            .vendor_dir
            .display()
            .to_string()
            .with_style(Style::Cyan)
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
fn gather_vendor_info(zen_files: Vec<PathBuf>, workspace_root: PathBuf) -> Result<VendorInfo> {
    if zen_files.is_empty() {
        anyhow::bail!("No zen files to process");
    }

    let mut dependencies = HashMap::new();
    let vendor_dir = workspace_root.join("vendor");

    // Evaluate each zen file and collect dependencies
    for zen_file in &zen_files {
        // Don't use the vendor path for the workspace info, we're just gathering dependencies
        let workspace_info = gather_workspace_info(zen_file.clone(), false)?;

        for path in workspace_info.resolver.get_tracked_files() {
            if is_vendor_dependency(&workspace_root, &path, &workspace_info.resolver) {
                if let Some(load_spec) = workspace_info.resolver.get_load_spec_for_path(&path) {
                    let vendor_path = loadspec_to_vendor_path(&load_spec)?;

                    dependencies
                        .entry(vendor_path)
                        .or_insert(path.to_path_buf());
                }
            }
        }
    }

    Ok(VendorInfo {
        vendor_dir,
        dependencies,
    })
}

/// Check if vendor directory is up-to-date by vendoring to a temp directory and comparing
fn check_vendor_directory(info: &VendorInfo) -> Result<bool> {
    // If vendor directory doesn't exist, it's not up-to-date
    if !info.vendor_dir.exists() {
        debug!(
            "Vendor directory does not exist: {}",
            info.vendor_dir.display()
        );
        return Ok(false);
    }

    // Create temporary directory for comparison
    let temp_dir = TempDir::new().context("Failed to create temporary directory")?;
    let temp_vendor_dir = temp_dir.path().join("vendor");
    fs::create_dir_all(&temp_vendor_dir)?;

    // Create a temporary VendorInfo with the temp directory
    let temp_info = VendorInfo {
        vendor_dir: temp_vendor_dir.clone(),
        dependencies: info.dependencies.clone(),
    };

    // Vendor dependencies to temp directory
    copy_vendor_dependencies(&temp_info).context("Failed to vendor to temporary directory")?;

    // Compare temp directory with actual vendor directory using dir-diff
    let are_different = dir_diff::is_different(&temp_vendor_dir, &info.vendor_dir)
        .context("Failed to compare vendor directories")?;

    if are_different {
        debug!(
            "Vendor directory differs from expected (temp: {}, actual: {})",
            temp_vendor_dir.display(),
            info.vendor_dir.display()
        );
        Ok(false)
    } else {
        debug!("Vendor directory matches expected content");
        Ok(true)
    }
}

/// Copy vendor dependencies to vendor directory
fn copy_vendor_dependencies(info: &VendorInfo) -> Result<usize> {
    for (vendor_path, src_path) in &info.dependencies {
        let dest_path = info.vendor_dir.join(vendor_path);

        // Create parent directory
        if let Some(parent) = dest_path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Copy the file
        fs::copy(src_path, &dest_path)
            .with_context(|| format!("copy {} -> {}", src_path.display(), dest_path.display()))?;
    }

    Ok(info.dependencies.len())
}
