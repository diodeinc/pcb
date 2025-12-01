use crate::build::create_diagnostics_passes;
use anyhow::Result;
use clap::Args;
use log::debug;
use pcb_ui::{Colorize, Spinner, Style, StyledText};
use pcb_zen::{get_workspace_info, resolve_dependencies, vendor_deps, EvalConfig};
use pcb_zen_core::DefaultFileProvider;
use pcb_zen_core::LoadSpec;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Args)]
pub struct VendorArgs {
    /// Path to .zen file or directory to analyze for dependencies.
    /// If a directory, will search recursively for .zen files.
    pub zen_path: PathBuf,

    /// Continue vendoring even if some designs have build errors
    #[arg(long = "ignore-errors")]
    pub ignore_errors: bool,
}

pub fn execute(args: VendorArgs) -> Result<()> {
    let zen_path = args.zen_path.canonicalize()?;
    let mut workspace_info = get_workspace_info(&DefaultFileProvider::new(), &zen_path)?;

    // Check if this is a V2 workspace - use simplified closure-based vendoring
    if workspace_info.is_v2() {
        return execute_v2(&mut workspace_info);
    }

    // V1 path: discover zen files and gather dependencies via evaluation
    execute_v1(args, &workspace_info.root)
}

/// V2 vendoring: uses dependency closure from resolution
fn execute_v2(workspace_info: &mut pcb_zen::WorkspaceInfo) -> Result<()> {
    println!("V2 workspace detected - using closure-based vendoring\n");

    // Vendoring always needs network access (offline=false)
    let resolution = resolve_dependencies(workspace_info, false)?;

    // Vendor everything - pass ["**"] pattern to match all packages and assets
    let result = vendor_deps(workspace_info, &resolution, &["**".to_string()], None)?;

    println!();
    println!(
        "{} {}",
        "✓".green().bold(),
        format!(
            "Vendored {} packages and {} assets",
            result.package_count, result.asset_count
        )
        .bold()
    );
    println!(
        "Vendor directory: {}",
        result
            .vendor_dir
            .display()
            .to_string()
            .with_style(Style::Cyan)
    );

    Ok(())
}

/// V1 vendoring: discovers files via evaluation and tracks dependencies
fn execute_v1(args: VendorArgs, workspace_root: &Path) -> Result<()> {
    let zen_path = &args.zen_path;

    // Discover zen files to process
    let discovery_spinner = Spinner::builder("Discovering zen files").start();
    let zen_files = discover_zen_files(zen_path)?;
    discovery_spinner.finish();
    println!(
        "{} Found {} zen files to analyze",
        "✓".green(),
        zen_files.len()
    );

    // Gather vendor information from all zen files
    let info_spinner = Spinner::builder("Analyzing dependencies").start();
    let zen_files_count = zen_files.len();
    let tracked_files = gather_vendor_info(zen_files, args.ignore_errors)?;
    let vendor_dir = workspace_root.join("vendor");
    info_spinner.finish();
    println!("{} Dependencies analyzed", "✓".green());

    // Create vendor directory
    let _ = fs::remove_dir_all(&vendor_dir);
    fs::create_dir_all(&vendor_dir)?;

    // Copy vendor dependencies
    let vendor_spinner = Spinner::builder("Copying vendor dependencies").start();
    let vendor_count = sync_tracked_files(&tracked_files, workspace_root, &vendor_dir, None)?;
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
fn gather_vendor_info(
    zen_files: Vec<PathBuf>,
    ignore_errors: bool,
) -> Result<HashMap<PathBuf, LoadSpec>> {
    if zen_files.is_empty() {
        anyhow::bail!("No zen files to process");
    }
    // Evaluate each zen file and collect tracked files
    let mut tracked_files: HashMap<PathBuf, LoadSpec> = HashMap::default();
    let mut has_errors = false;
    // Prepare passes once if we are going to render diagnostics
    let passes = if ignore_errors {
        None
    } else {
        Some(create_diagnostics_passes(&[]))
    };
    for zen_file in &zen_files {
        // Don't use the vendor path for eval, we're just gathering dependencies
        let eval_cfg = EvalConfig {
            use_vendor: false,
            ..Default::default()
        };
        let eval_result = pcb_zen::eval(zen_file, eval_cfg);

        // Decide if this file has errors (render diagnostics only when not ignoring errors)
        let mut diagnostics = eval_result.diagnostics.clone();
        if let Some(passes) = &passes {
            diagnostics.apply_passes(passes);
        }
        let file_has_errors = diagnostics.has_errors();

        if file_has_errors && ignore_errors {
            println!(
                "{} {}: Build failed; skipping dependencies",
                "⚠".yellow(),
                zen_file.display().to_string().with_style(Style::Yellow)
            );
            continue;
        }

        if file_has_errors {
            has_errors = true;
            continue;
        }

        // Collect dependencies from successful evaluations
        if let Some(output) = eval_result.output.as_ref() {
            let resolver = output.core_resolver().unwrap();
            tracked_files.extend(resolver.get_tracked_files());
        }
    }

    if has_errors {
        anyhow::bail!("Build failed with errors");
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
        // Skip paths that don't exist to avoid panics
        if !path.exists() {
            log::debug!("Skipping non-existent path: {}", path.display());
            continue;
        }
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
            fs::copy(path, &dest_path)?;
            make_readonly(&dest_path)?;
            synced_files += 1;
        } else {
            synced_files += copy_dir_all(path, dest_path)?;
        }
    }
    Ok(synced_files)
}

fn copy_dir_all(src: impl AsRef<Path>, dst: impl AsRef<Path>) -> Result<usize> {
    fs::create_dir_all(&dst)?;
    let mut synced_files = 0;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let dest_path = dst.as_ref().join(entry.file_name());
        if entry.file_type()?.is_dir() {
            synced_files += copy_dir_all(entry.path(), &dest_path)?;
            make_readonly(&dest_path)?;
        } else {
            fs::copy(entry.path(), &dest_path)?;
            make_readonly(&dest_path)?;
            synced_files += 1;
        }
    }
    Ok(synced_files)
}

/// Make a single file or directory read-only
fn make_readonly(path: &Path) -> Result<()> {
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_readonly(true);
    fs::set_permissions(path, perms)?;
    Ok(())
}
