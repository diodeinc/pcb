use anyhow::{Context, Result, bail};
use clap::Args;
use pcb_zen::workspace::{WorkspaceInfoExt, get_workspace_info_without_versions};
use pcb_zen::{git, resolve_dependencies};
use pcb_zen_core::DefaultFileProvider;
use pcb_zen_core::resolution::{PackageClosure, ResolutionResult};
use serde::Serialize;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{info_span, instrument};

use crate::bundle::{self, MetadataInput, SourceBundlePlan};
use crate::file_walker::collect_zen_files;
use crate::info::OutputFormat;

#[derive(Args)]
pub struct PackageArgs {
    /// Package directory to bundle
    path: PathBuf,

    /// Output archive path (workspace bundles default to .tar.zst)
    #[arg(short = 'o', long = "output")]
    output: Option<PathBuf>,

    /// Output format
    #[arg(short = 'f', long, value_enum, default_value = "human")]
    format: OutputFormat,

    /// Enable verbose output (shows staged file list)
    #[arg(short = 'v', long = "verbose")]
    verbose: bool,
}

struct WorkspaceTarget {
    workspace: pcb_zen::WorkspaceInfo,
    package_url: String,
    package_dir: PathBuf,
    bundle_stem: String,
    target_name: String,
    primary_zen: PathBuf,
    description: Option<String>,
}

type WorkspaceTargetParts = (String, PathBuf, String, String, PathBuf, Option<String>);

#[derive(Serialize)]
struct PackageOutput {
    package_url: String,
    package_dir: PathBuf,
    bundle_stem: String,
    target_name: String,
    staging_dir: PathBuf,
    output_path: PathBuf,
    output_size_bytes: u64,
}

pub fn execute(args: PackageArgs) -> Result<()> {
    let path = args.path.canonicalize()?;

    if !path.exists() {
        bail!("Path does not exist: {}", path.display());
    }
    if !path.is_dir() {
        bail!(
            "`pcb package` requires a package directory, not a file: {}",
            path.display()
        );
    }
    if args.verbose && matches!(args.format, OutputFormat::Json) {
        bail!("--verbose is not supported with --format json");
    }

    let target = resolve_target(&path)?;
    let output = package_workspace_target(target, &args)?;

    print_output(&output, &args)
}

fn print_output(output: &PackageOutput, args: &PackageArgs) -> Result<()> {
    match args.format {
        OutputFormat::Human => print_human_output(output),
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(output)?),
    }

    Ok(())
}

fn print_human_output(output: &PackageOutput) {
    println!("Packaging bundle: {}", output.package_dir.display());
    println!("Staging dir: {}", output.staging_dir.display());
    println!("Wrote bundle to: {}", output.output_path.display());
    println!("Bundle size: {} bytes", output.output_size_bytes);
}

#[instrument(name = "package_workspace_target", skip_all)]
fn package_workspace_target(target: WorkspaceTarget, args: &PackageArgs) -> Result<PackageOutput> {
    let git_hash = git::rev_parse_head(&target.workspace.root).unwrap_or_else(|| "unknown".into());
    let version =
        git::rev_parse_short_head(&target.workspace.root).unwrap_or_else(|| "unknown".into());
    let bundle_root = target.workspace.root.join(".pcb/packages");
    let staging_dir = bundle_root.join(format!("{}-{}", target.bundle_stem, version));
    let output_path = args
        .output
        .clone()
        .unwrap_or_else(|| bundle_root.join(format!("{}-{}.tar.zst", target.bundle_stem, version)));

    if staging_dir.exists() {
        bundle::remove_dir_all_with_permissions(&staging_dir)?;
    }
    fs::create_dir_all(&staging_dir)?;

    let mut workspace = target.workspace;
    let locked = workspace.lockfile.is_some();
    let resolution = {
        let _span = info_span!("resolve_package_bundle_dependencies").entered();
        resolve_dependencies(&mut workspace, false, locked)?
    };
    let closure = resolution.package_closure(&target.package_url);
    let resolved_paths = collect_bundle_resolved_paths(&resolution, &closure)?;

    bundle::stage_source_bundle(&SourceBundlePlan {
        resolution: &resolution,
        closure: &closure,
        staged_src: &staging_dir.join("src"),
        resolved_paths: &resolved_paths,
    })?;

    bundle::write_metadata_json(&MetadataInput {
        name: &target.target_name,
        version: &version,
        git_hash: &git_hash,
        workspace_root: &resolution.workspace_info.root,
        staging_dir: &staging_dir,
        zen_path: &target.primary_zen,
        layout_path: None,
        description: target.description.as_deref(),
        include_kicad_version: false,
    })?;

    if args.verbose {
        println!("\nFiles included:");
        let entries = pcb_canonical::list_canonical_tar_entries_with_options(
            &staging_dir,
            pcb_canonical::CanonicalTarOptions {
                exclude_nested_packages: false,
            },
        )?;
        for entry in &entries {
            println!("  {}", entry);
        }
        println!("\nTotal: {} entries\n", entries.len());
    }

    bundle::write_canonical_bundle(&staging_dir, &output_path)?;

    Ok(PackageOutput {
        package_url: target.package_url,
        package_dir: target.package_dir,
        bundle_stem: target.bundle_stem,
        target_name: target.target_name,
        staging_dir,
        output_path: output_path.clone(),
        output_size_bytes: fs::metadata(&output_path)?.len(),
    })
}

#[instrument(name = "resolve_package_target", skip_all)]
fn resolve_target(path: &Path) -> Result<WorkspaceTarget> {
    let file_provider = DefaultFileProvider::new();
    let workspace = get_workspace_info_without_versions(&file_provider, path)?;

    if !workspace.errors.is_empty() {
        for err in &workspace.errors {
            eprintln!("{}", err.error);
        }
        bail!("Found {} invalid pcb.toml file(s)", workspace.errors.len());
    }

    let Some((package_url, package_dir, bundle_stem, target_name, primary_zen, description)) =
        resolve_workspace_target(&workspace, path)?
    else {
        bail!("`{}` is not a workspace package directory", path.display());
    };

    Ok(WorkspaceTarget {
        workspace,
        package_url,
        package_dir,
        bundle_stem,
        target_name,
        primary_zen,
        description,
    })
}

fn resolve_workspace_target(
    workspace: &pcb_zen::WorkspaceInfo,
    path: &Path,
) -> Result<Option<WorkspaceTargetParts>> {
    let Some((package_url, package_dir, board_config)) =
        workspace.packages.iter().find_map(|(url, pkg)| {
            let dir = pkg.dir(&workspace.root);
            (path == dir).then(|| (url.clone(), dir, pkg.config.board.clone()))
        })
    else {
        return Ok(None);
    };

    if let Some(board) = board_config {
        let zen_rel = board
            .path
            .as_ref()
            .context("Board package missing [board].path")?;
        return Ok(Some((
            package_url,
            package_dir.clone(),
            bundle_stem_from_package_dir(workspace, &package_dir)?,
            board.name,
            package_dir.join(zen_rel),
            (!board.description.is_empty()).then_some(board.description),
        )));
    }

    let target_name = package_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("package")
        .to_string();
    let primary_zen = choose_primary_package_zen(workspace, &package_url, &package_dir)?;

    let bundle_stem = bundle_stem_from_package_dir(workspace, &package_dir)?;

    Ok(Some((
        package_url,
        package_dir,
        bundle_stem,
        target_name,
        primary_zen,
        None,
    )))
}

fn bundle_stem_from_package_dir(
    workspace: &pcb_zen::WorkspaceInfo,
    package_dir: &Path,
) -> Result<String> {
    let rel_path = package_dir.strip_prefix(&workspace.root).with_context(|| {
        format!(
            "Package dir {} is not within workspace root {}",
            package_dir.display(),
            workspace.root.display()
        )
    })?;

    let stem = rel_path
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("--");

    if stem.is_empty() {
        bail!(
            "Package path {} produced an empty bundle stem",
            rel_path.display()
        );
    }

    Ok(stem)
}

fn choose_primary_package_zen(
    workspace: &pcb_zen::WorkspaceInfo,
    package_url: &str,
    package_dir: &Path,
) -> Result<PathBuf> {
    if let Some(name) = package_dir.file_name().and_then(|name| name.to_str()) {
        let preferred = package_dir.join(format!("{name}.zen"));
        if preferred.exists() {
            return Ok(preferred);
        }
    }

    collect_owned_zen_files(workspace, package_url, package_dir)?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("No .zen files found in package {}", package_dir.display()))
}

#[instrument(name = "collect_bundle_resolved_paths", skip_all)]
fn collect_bundle_resolved_paths(
    resolution: &ResolutionResult,
    closure: &PackageClosure,
) -> Result<Vec<PathBuf>> {
    let workspace = &resolution.workspace_info;
    let mut zen_files = BTreeSet::new();

    for package_url in &closure.local_packages {
        let Some(pkg) = workspace.packages.get(package_url) else {
            continue;
        };
        let package_dir = pkg.dir(&workspace.root);
        for zen_file in collect_owned_zen_files(workspace, package_url, &package_dir)? {
            zen_files.insert(zen_file);
        }
    }

    let mut resolved_paths = BTreeSet::new();
    for zen_file in zen_files {
        let eval_result = {
            let _span = info_span!("eval_bundle_zen_file", path = %zen_file.display()).entered();
            pcb_zen::eval(&zen_file, resolution.clone())
        };
        let Some(output) = eval_result.output else {
            continue;
        };
        for path in output.config.tracked_resolved_paths() {
            resolved_paths.insert(path);
        }
    }

    Ok(resolved_paths.into_iter().collect())
}

fn collect_owned_zen_files(
    workspace: &pcb_zen::WorkspaceInfo,
    package_url: &str,
    package_dir: &Path,
) -> Result<Vec<PathBuf>> {
    let mut zen_files = collect_zen_files(&[package_dir.to_path_buf()])?;
    zen_files.retain(|path| workspace.package_url_for_zen(path).as_deref() == Some(package_url));
    zen_files.sort();
    zen_files.dedup();
    Ok(zen_files)
}

#[cfg(test)]
mod tests {
    use super::bundle_stem_from_package_dir;
    use pcb_zen::WorkspaceInfo;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    #[test]
    fn bundle_stem_uses_workspace_relative_path() {
        let workspace = WorkspaceInfo {
            root: PathBuf::from("/tmp/workspace"),
            cache_dir: PathBuf::new(),
            config: None,
            packages: BTreeMap::new(),
            lockfile: None,
            errors: Vec::new(),
        };

        let stem = bundle_stem_from_package_dir(
            &workspace,
            &PathBuf::from("/tmp/workspace/reference/TPS543620RPYR"),
        )
        .unwrap();

        assert_eq!(stem, "reference--TPS543620RPYR");
    }
}
