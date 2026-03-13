use anyhow::{Context, Result, bail};
use clap::Args;
use pcb_layout::utils as layout_utils;
use pcb_zen::workspace::WorkspaceInfoExt;
use pcb_zen::{get_workspace_info, git, resolve_dependencies};
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

    /// Print the legacy package content and manifest hashes instead of building a bundle
    #[arg(long = "hash-only")]
    hash_only: bool,

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
    primary_zen: Option<PathBuf>,
    description: Option<String>,
}

type WorkspaceTargetParts = (
    String,
    PathBuf,
    String,
    String,
    Option<PathBuf>,
    Option<String>,
);

#[derive(Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
enum PackageOutput {
    Bundle(BundleOutput),
    HashOnly(HashOnlyOutput),
}

#[derive(Serialize)]
struct BundleOutput {
    package_url: String,
    package_dir: PathBuf,
    bundle_stem: String,
    target_name: String,
    staging_dir: PathBuf,
    output_path: PathBuf,
    output_size_bytes: u64,
}

#[derive(Serialize)]
struct HashOnlyOutput {
    package_url: String,
    package_dir: PathBuf,
    output_path: Option<PathBuf>,
    output_size_bytes: Option<u64>,
    content_hash: String,
    manifest_hash: Option<String>,
}

pub fn execute(args: PackageArgs) -> Result<()> {
    if !args.path.exists() {
        bail!("Path does not exist: {}", args.path.display());
    }
    let path = args.path.canonicalize()?;
    if !path.is_dir() {
        bail!(
            "`pcb package` requires a package directory, not a file: {}",
            path.display()
        );
    }
    if args.verbose && matches!(args.format, OutputFormat::Json) {
        bail!("--verbose is not supported with --format json");
    }

    let target = resolve_target(&path, !args.hash_only)?;
    let output = if args.hash_only {
        package_hash_only(target, &args)?
    } else {
        package_workspace_target(target, &args)?
    };

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
    match output {
        PackageOutput::Bundle(output) => {
            println!("Packaging bundle: {}", output.package_dir.display());
            println!("Staging dir: {}", output.staging_dir.display());
            println!("Wrote bundle to: {}", output.output_path.display());
            println!("Bundle size: {} bytes", output.output_size_bytes);
        }
        PackageOutput::HashOnly(output) => {
            if let Some(output_path) = &output.output_path {
                println!("Wrote tar to: {}", output_path.display());
            }
            if let Some(output_size_bytes) = output.output_size_bytes {
                println!("Tar size: {} bytes", output_size_bytes);
            }
            println!("Content hash: {}", output.content_hash);
            if let Some(manifest_hash) = &output.manifest_hash {
                println!("Manifest hash: {}", manifest_hash);
            }
        }
    }
}

#[instrument(name = "package_workspace_target", skip_all)]
fn package_workspace_target(target: WorkspaceTarget, args: &PackageArgs) -> Result<PackageOutput> {
    let primary_zen = target.primary_zen.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "No .zen files found in package {}",
            target.package_dir.display()
        )
    })?;
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
    let layout_path = target
        .primary_zen
        .as_ref()
        .map(|primary_zen| resolve_package_layout_path(primary_zen, &resolution))
        .transpose()?
        .flatten();
    let closure = resolution.package_closure(&target.package_url);
    let resolved_paths = collect_bundle_resolved_paths(&resolution, &closure)?;

    bundle::stage_source_bundle(&SourceBundlePlan {
        resolution: &resolution,
        closure: Some(&closure),
        remote_vendoring: bundle::RemoteVendoring::ClosureOnly,
        staged_src: &staging_dir.join("src"),
        resolved_paths: &resolved_paths,
    })?;

    bundle::write_metadata_json(&MetadataInput {
        name: &target.target_name,
        version: &version,
        git_hash: &git_hash,
        workspace_root: &resolution.workspace_info.root,
        staging_dir: &staging_dir,
        zen_path: primary_zen,
        layout_path: layout_path.as_deref(),
        description: target.description.as_deref(),
        include_kicad_version: false,
    })?;

    if args.verbose {
        println!("\nFiles included:");
        let entries = pcb_canonical::list_canonical_tar_entries(
            &staging_dir,
            Some(pcb_canonical::CanonicalTarOptions {
                exclude_nested_packages: false,
            }),
        )?;
        for entry in &entries {
            println!("  {}", entry);
        }
        println!("\nTotal: {} entries\n", entries.len());
    }

    bundle::write_canonical_bundle(&staging_dir, &output_path)?;

    Ok(PackageOutput::Bundle(BundleOutput {
        package_url: target.package_url,
        package_dir: target.package_dir,
        bundle_stem: target.bundle_stem,
        target_name: target.target_name,
        staging_dir,
        output_path: output_path.clone(),
        output_size_bytes: fs::metadata(&output_path)?.len(),
    }))
}

fn package_hash_only(target: WorkspaceTarget, args: &PackageArgs) -> Result<PackageOutput> {
    if args.verbose {
        println!("\nFiles included:");
        let entries = pcb_canonical::list_canonical_tar_entries(&target.package_dir, None)?;
        for entry in &entries {
            println!("  {}", entry);
        }
        println!("\nTotal: {} entries\n", entries.len());
    }

    if let Some(output_path) = &args.output {
        let mut tar_data = Vec::new();
        pcb_canonical::create_canonical_tar(&target.package_dir, &mut tar_data, None)?;
        fs::write(output_path, tar_data)?;
    }

    Ok(PackageOutput::HashOnly(HashOnlyOutput {
        package_url: target.package_url,
        package_dir: target.package_dir.clone(),
        output_path: args.output.clone(),
        output_size_bytes: args
            .output
            .as_ref()
            .map(|output_path| fs::metadata(output_path).map(|metadata| metadata.len()))
            .transpose()?,
        content_hash: pcb_canonical::compute_content_hash_from_dir(&target.package_dir)?,
        manifest_hash: manifest_hash_for_package_dir(&target.package_dir)?,
    }))
}

#[instrument(name = "resolve_package_target", skip_all)]
fn resolve_target(path: &Path, require_primary_zen: bool) -> Result<WorkspaceTarget> {
    let file_provider = DefaultFileProvider::new();
    let workspace = get_workspace_info(&file_provider, path, false)?;

    if !workspace.errors.is_empty() {
        for err in &workspace.errors {
            eprintln!("{}", err.error);
        }
        bail!("Found {} invalid pcb.toml file(s)", workspace.errors.len());
    }

    let Some((package_url, package_dir, bundle_stem, target_name, primary_zen, description)) =
        resolve_workspace_target(&workspace, path, require_primary_zen)?
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
    require_primary_zen: bool,
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
            Some(package_dir.join(zen_rel)),
            (!board.description.is_empty()).then_some(board.description),
        )));
    }

    let target_name = package_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("package")
        .to_string();
    let primary_zen = find_primary_package_zen(workspace, &package_url, &package_dir)?;
    if require_primary_zen && primary_zen.is_none() {
        bail!("No .zen files found in package {}", package_dir.display());
    }

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

fn resolve_package_layout_path(
    primary_zen: &Path,
    resolution: &ResolutionResult,
) -> Result<Option<PathBuf>> {
    let eval_result = pcb_zen::eval(primary_zen, resolution.clone());
    let output = eval_result.output_result().map_err(|diagnostics| {
        let rendered_diagnostics = diagnostics
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        anyhow::anyhow!(
            "Failed to evaluate {} while resolving package layout path:\n{}",
            primary_zen.display(),
            rendered_diagnostics
        )
    })?;
    let schematic = output
        .to_schematic()
        .with_context(|| format!("Failed to build schematic for {}", primary_zen.display()))?;
    let Some(layout_path) = layout_utils::resolve_layout_dir(&schematic)? else {
        return Ok(None);
    };

    Ok(Some(
        layout_path
            .strip_prefix(&resolution.workspace_info.root)
            .with_context(|| {
                format!(
                    "Layout path {} is not within workspace root {}",
                    layout_path.display(),
                    resolution.workspace_info.root.display()
                )
            })?
            .to_path_buf(),
    ))
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

fn find_primary_package_zen(
    workspace: &pcb_zen::WorkspaceInfo,
    package_url: &str,
    package_dir: &Path,
) -> Result<Option<PathBuf>> {
    if let Some(name) = package_dir.file_name().and_then(|name| name.to_str()) {
        let preferred = package_dir.join(format!("{name}.zen"));
        if preferred.exists() {
            return Ok(Some(preferred));
        }
    }

    Ok(
        collect_owned_zen_files(workspace, package_url, package_dir)?
            .into_iter()
            .next(),
    )
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
        let pcb_zen::WithDiagnostics {
            diagnostics,
            output,
        } = eval_result;
        let Some(output) = output else {
            if diagnostics.is_empty() {
                bail!(
                    "Failed to evaluate {} while collecting bundle resolved paths",
                    zen_file.display()
                );
            }

            let rendered_diagnostics = diagnostics
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            bail!(
                "Failed to evaluate {} while collecting bundle resolved paths:\n{}",
                zen_file.display(),
                rendered_diagnostics
            );
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

fn manifest_hash_for_package_dir(path: &Path) -> Result<Option<String>> {
    let manifest_path = path.join("pcb.toml");
    if !manifest_path.exists() {
        return Ok(None);
    }

    let manifest_content = fs::read_to_string(&manifest_path)?;
    Ok(Some(pcb_canonical::compute_manifest_hash(
        &manifest_content,
    )))
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
