use crate::workspace::{
    detect_workspace_root_from_files, gather_workspace_info, is_vendor_dependency,
    loadspec_to_vendor_path, WorkspaceInfo,
};
use anyhow::{Context, Result};
use clap::Args;
use log::{debug, info};
use pcb_ui::{Colorize, Spinner, Style, StyledText};
use pcb_zen_core::LoadSpec;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

#[derive(Args)]
pub struct VendorArgs {
    /// Path(s) to .zen file(s) or directories to analyze for dependencies.
    /// Directories will be searched recursively for .zen files.
    /// If none specified, will auto-discover all .zen files in the workspace.
    pub zen_paths: Vec<PathBuf>,

    /// Check if vendor directory is up-to-date (useful for CI)
    #[arg(long)]
    pub check: bool,
}

/// Aggregated dependency information from multiple designs
#[derive(Debug, Clone)]
pub struct DependencyInfo {
    /// LoadSpec for this dependency
    pub load_spec: LoadSpec,
    /// Canonical file path for this dependency
    pub file_path: PathBuf,
    /// Set of zen files that use this dependency
    pub used_by: HashSet<PathBuf>,
}

/// Information needed for vendoring dependencies from multiple designs
pub struct VendorInfo {
    /// Path to the vendor directory in workspace  
    pub vendor_dir: PathBuf,
    /// Aggregated dependencies from all designs (keyed by normalized vendor path)
    pub dependencies: HashMap<PathBuf, DependencyInfo>,
}

pub fn execute(args: VendorArgs) -> Result<()> {
    // Discover zen files to process
    let discovery_spinner = Spinner::builder("Discovering zen files").start();
    let zen_files = discover_zen_files(&args)?;
    discovery_spinner.finish();
    println!(
        "{} Found {} zen files to analyze",
        "✓".green(),
        zen_files.len()
    );

    // Gather vendor information from all zen files
    let info_spinner = Spinner::builder("Analyzing dependencies").start();
    let zen_files_count = zen_files.len();
    let vendor_info = gather_vendor_info(zen_files)?;
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

/// Discover zen files to process based on arguments
fn discover_zen_files(args: &VendorArgs) -> Result<Vec<PathBuf>> {
    let search_paths = if args.zen_paths.is_empty() {
        vec![std::env::current_dir().context("Failed to get current directory")?]
    } else {
        args.zen_paths.clone()
    };

    let mut zen_files = Vec::new();
    let mut errors = Vec::new();
    let search_paths_len = search_paths.len();

    for path in search_paths {
        if path.is_file() {
            // Verify it's a zen file
            if path.extension().and_then(|ext| ext.to_str()) == Some("zen") {
                zen_files.push(path);
            } else {
                errors.push(format!("Not a zen file: {}", path.display()));
            }
        } else if path.is_dir() {
            // Search directory for zen files
            match find_zen_files_in_directory(&path) {
                Ok(dir_zen_files) => zen_files.extend(dir_zen_files),
                Err(e) => errors.push(format!("Directory {}: {}", path.display(), e)),
            }
        } else {
            errors.push(format!("Path does not exist: {}", path.display()));
        }
    }

    // Report all errors at once
    if !errors.is_empty() {
        anyhow::bail!("Invalid paths provided:\n{}", errors.join("\n"));
    }

    if zen_files.is_empty() {
        anyhow::bail!("No zen files found in search paths");
    }

    debug!(
        "Found {} zen files from {} search paths",
        zen_files.len(),
        search_paths_len
    );
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
fn gather_vendor_info(zen_files: Vec<PathBuf>) -> Result<VendorInfo> {
    debug!(
        "Starting vendor information gathering for {} files",
        zen_files.len()
    );

    if zen_files.is_empty() {
        anyhow::bail!("No zen files to process");
    }

    // 1. Evaluate each zen file only once
    let mut workspaces = Vec::with_capacity(zen_files.len());
    for zen_file in zen_files {
        workspaces.push(gather_workspace_info(zen_file)?);
    }

    // 2. Determine unified workspace root from evaluated workspaces
    let workspace_root = unify_workspace_root(&workspaces)?;
    let vendor_dir = workspace_root.join("vendor");

    // 3. Aggregate dependencies from the same workspaces (no re-evaluation)
    let dependencies = aggregate_dependencies(&workspaces, &workspace_root)?;

    info!(
        "Aggregated {} unique dependencies from {} zen files",
        dependencies.len(),
        workspaces.len()
    );

    Ok(VendorInfo {
        vendor_dir,
        dependencies,
    })
}

/// Determine unified workspace root from already-evaluated workspace infos
fn unify_workspace_root(workspaces: &[WorkspaceInfo]) -> Result<PathBuf> {
    // Group workspaces by their detected workspace root
    let mut workspace_groups: HashMap<PathBuf, Vec<&WorkspaceInfo>> = HashMap::new();
    for workspace in workspaces {
        workspace_groups
            .entry(workspace.workspace_root.clone())
            .or_default()
            .push(workspace);
    }

    // Check for pcb.toml conflicts
    let pcb_toml_roots: Vec<_> = workspace_groups
        .keys()
        .filter(|root| root.join("pcb.toml").exists())
        .collect();

    match (workspace_groups.len(), pcb_toml_roots.len()) {
        (1, _) => {
            // All zen files share the same workspace root
            Ok(workspace_groups.into_keys().next().unwrap())
        }
        (_, 1..) => {
            // Multiple workspaces with pcb.toml files - this is a clear conflict
            let conflicting_files = workspace_groups
                .iter()
                .flat_map(|(root, workspaces)| workspaces.iter().map(move |w| (root, &w.zen_path)))
                .map(|(root, zen_path)| {
                    format!("  {} (workspace: {})", zen_path.display(), root.display())
                })
                .collect::<Vec<_>>()
                .join("\n");

            anyhow::bail!(
                "Zen files from different workspaces cannot be vendored together:\n\
                {}\n\
                \n\
                Solution: Run 'pcb vendor' separately for each workspace directory.",
                conflicting_files
            );
        }
        (_, 0) => {
            // Multiple workspace roots but no pcb.toml - find common ancestor
            let all_tracked_files: HashSet<PathBuf> = workspaces
                .iter()
                .flat_map(|w| w.tracker.files())
                .chain(workspaces.iter().map(|w| w.zen_path.clone()))
                .collect();

            detect_workspace_root_from_files(&workspaces[0].zen_path, &all_tracked_files)
        }
    }
}

/// Aggregate dependencies from workspace infos
fn aggregate_dependencies(
    workspaces: &[WorkspaceInfo],
    workspace_root: &Path,
) -> Result<HashMap<PathBuf, DependencyInfo>> {
    let mut aggregated_deps = HashMap::new();

    for workspace in workspaces {
        for path in workspace.tracker.files() {
            if is_vendor_dependency(workspace_root, &path, &workspace.tracker) {
                let load_spec = workspace
                    .tracker
                    .get_load_spec(&path)
                    .context("Vendor file must have LoadSpec")?
                    .clone();

                let vendor_path = loadspec_to_vendor_path(&load_spec)?;

                aggregated_deps
                    .entry(vendor_path)
                    .or_insert_with(|| DependencyInfo {
                        load_spec,
                        file_path: path.to_path_buf(),
                        used_by: HashSet::new(),
                    })
                    .used_by
                    .insert(workspace.zen_path.clone());
            }
        }
    }

    Ok(aggregated_deps)
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
    for dep_info in info.dependencies.values() {
        let vendor_path = loadspec_to_vendor_path(&dep_info.load_spec)?;
        let dest_path = info.vendor_dir.join(&vendor_path);

        copy_with_context(&dep_info.file_path, &dest_path)?;

        debug!(
            "Vendored: {} -> {} (used by {} designs)",
            dep_info.file_path.display(),
            dest_path.display(),
            dep_info.used_by.len()
        );
    }

    Ok(info.dependencies.len())
}

/// Copy a file with rich error context
fn copy_with_context(src: &std::path::Path, dst: &std::path::Path) -> Result<()> {
    // Create parent directory
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create parent directory: {}", parent.display()))?;
    }

    // Copy the file
    fs::copy(src, dst)
        .with_context(|| format!("Failed to copy {} -> {}", src.display(), dst.display()))
        .map(|_| ())
}
