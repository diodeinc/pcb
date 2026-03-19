//! Package forking functionality
//!
//! This module provides the core logic for forking packages into a workspace.
//! It is used by both the CLI (`pcb fork`) and the TUI.

use crate::cache_index::{cache_base, ensure_stdlib_materialized};
use crate::git;
use crate::tags::{get_all_versions_for_repo, parse_version};
use crate::{copy_dir_all, ensure_sparse_checkout, get_workspace_info};
use anyhow::{Context, Result};
use path_slash::PathExt;
use pcb_zen_core::DefaultFileProvider;
use pcb_zen_core::config::{PatchSpec, PcbToml, split_repo_and_subpath};
use semver::Version;
use std::collections::BTreeMap;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Options for forking a package (no clap dependency)
pub struct ForkOptions {
    /// Fully-qualified package URL (e.g., github.com/diodeinc/registry/modules/UsbPdController)
    pub url: String,
    /// Specific version to fork (default: latest tagged version)
    pub version: Option<String>,
    /// Force overwrite if fork directory already exists
    pub force: bool,
}

/// Result of a successful fork operation
pub struct ForkSuccess {
    /// Path to the fork directory
    pub fork_dir: PathBuf,
    /// Canonical module URL (may differ from input if normalized)
    pub module_url: String,
    /// Version that was forked
    pub version: String,
    /// Relative path used in [patch] section
    pub patch_path: String,
}

/// Fork a package into the current workspace.
///
/// This function:
/// 1. Validates the workspace has pcb.toml with pcb-version >= 0.3
/// 2. Discovers available versions for the package
/// 3. Fetches the package into the cache
/// 4. Copies it to `fork/<url>/<version>/`
/// 5. Updates pcb.toml with a [patch] entry
///
/// Returns `ForkSuccess` with details about the fork, or an error.
pub fn fork_package(options: ForkOptions) -> Result<ForkSuccess> {
    let cwd = std::env::current_dir()?;
    let file_provider = DefaultFileProvider::new();
    let workspace_info = get_workspace_info(&file_provider, &cwd, true)
        .context("Failed to detect PCB workspace (no pcb.toml found up the tree?)")?;
    let workspace_root = &workspace_info.root;

    // Validate workspace
    let pcb_toml_path = workspace_root.join("pcb.toml");
    if !pcb_toml_path.exists() {
        anyhow::bail!(
            "pcb fork requires a workspace with pcb.toml at {}",
            pcb_toml_path.display()
        );
    }

    let mut config = PcbToml::from_file(&file_provider, &pcb_toml_path)?;

    let input_url = options.url.trim().to_string();
    if pcb_zen_core::is_stdlib_module_path(&input_url) {
        return fork_stdlib(workspace_root, &pcb_toml_path, &mut config, options.force);
    }

    let (input_url, version_from_url) = parse_fork_url_and_version(&input_url)?;
    if let (Some(flag_version), Some(url_version)) = (options.version.as_deref(), version_from_url)
    {
        anyhow::bail!(
            "Version specified twice for '{}': @{} and --version {}.\n\
             Use only one.",
            input_url,
            url_version,
            flag_version.trim()
        );
    }
    let requested_version = options
        .version
        .as_deref()
        .map(str::trim)
        .or(version_from_url);

    // Parse module URL
    let (repo_url, pkg_path) = split_repo_and_subpath(input_url);

    // Discover versions
    let all_versions = get_all_versions_for_repo(repo_url)
        .with_context(|| format!("Failed to fetch versions from {}", repo_url))?;

    // Find versioned package by walking up the path (supports .zen file paths)
    let (canonical_pkg_path, versions_for_pkg) =
        find_versioned_package(&all_versions, pkg_path, repo_url)?;

    // Compute the canonical module URL
    let module_url = if canonical_pkg_path.is_empty() {
        repo_url.to_string()
    } else {
        format!("{}/{}", repo_url, canonical_pkg_path)
    };

    // Select version - use iter().max() to explicitly pick highest semver
    let version = if let Some(v_str) = requested_version {
        let v = parse_version(v_str).with_context(|| {
            format!(
                "Invalid version '{}'. Expected semver format like 0.4.0 or v0.4.0.",
                v_str
            )
        })?;
        if !versions_for_pkg.contains(&v) {
            anyhow::bail!(
                "Version {} not found for {}.\nAvailable versions: {}",
                v,
                module_url,
                versions_for_pkg
                    .iter()
                    .take(10)
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        v
    } else {
        versions_for_pkg
            .iter()
            .max()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("No versions available for {}", module_url))?
    };
    let version_str = version.to_string();

    // Compute fork directory and relative path
    let fork_dir = workspace_root
        .join("fork")
        .join(&module_url)
        .join(&version_str);

    let relative_fork_path = fork_dir
        .strip_prefix(workspace_root)
        .expect("fork_dir should be under workspace_root")
        .to_slash_lossy()
        .into_owned();

    // Check for conflicting patch BEFORE modifying filesystem
    let patch_updated =
        update_patch_section(&mut config, &module_url, &relative_fork_path, options.force)?;

    // Ensure package is in cache using shared sparse checkout logic
    let cache_dir = cache_base().join(&module_url).join(&version_str);
    ensure_sparse_checkout(&cache_dir, &module_url, &version_str, true, None)
        .with_context(|| format!("Failed to fetch {}@{} into cache", module_url, version_str))?;

    // Handle fork directory
    let fork_existed = fork_dir.exists();
    if fork_existed && options.force {
        fs::remove_dir_all(&fork_dir)
            .with_context(|| format!("Failed to remove {}", fork_dir.display()))?;
    }
    // If not force and exists, skip copy (handled below)

    // Copy to fork directory (if needed)
    if !fork_dir.exists() {
        copy_dir_all(&cache_dir, &fork_dir, &HashSet::new()).with_context(|| {
            format!(
                "Failed to copy from {} to {}",
                cache_dir.display(),
                fork_dir.display()
            )
        })?;
    }

    // Validate package has pcb.toml
    let fork_pcb_toml = fork_dir.join("pcb.toml");
    if !fork_pcb_toml.exists() {
        // Clean up if we just created it
        if !fork_existed {
            let _ = fs::remove_dir_all(&fork_dir);
        }
        anyhow::bail!(
            "pcb fork only supports packages with pcb.toml.\n\
             {} has no pcb.toml (not a package repository).\n\
             Use 'pcb vendor' or clone manually.",
            module_url
        );
    }

    // Write updated config
    if patch_updated {
        let new_toml = toml::to_string_pretty(&config)?;
        fs::write(&pcb_toml_path, new_toml)
            .with_context(|| format!("Failed to write {}", pcb_toml_path.display()))?;
    }

    Ok(ForkSuccess {
        fork_dir,
        module_url,
        version: version_str,
        patch_path: relative_fork_path,
    })
}

fn fork_stdlib(
    workspace_root: &Path,
    pcb_toml_path: &Path,
    config: &mut PcbToml,
    force: bool,
) -> Result<ForkSuccess> {
    let source_dir = ensure_stdlib_materialized(workspace_root)
        .context("Failed to materialize embedded stdlib")?;
    let module_url = pcb_zen_core::STDLIB_MODULE_PATH.to_string();
    let fork_dir = workspace_root.join("fork").join(&module_url);
    let relative_fork_path = fork_dir
        .strip_prefix(workspace_root)
        .expect("fork_dir should be under workspace_root")
        .to_slash_lossy()
        .into_owned();

    let patch_updated = update_patch_section(config, &module_url, &relative_fork_path, force)?;

    let fork_existed = fork_dir.exists();
    if fork_existed && force {
        fs::remove_dir_all(&fork_dir)
            .with_context(|| format!("Failed to remove {}", fork_dir.display()))?;
    }

    if !fork_dir.exists() {
        copy_dir_all(&source_dir, &fork_dir, &HashSet::new()).with_context(|| {
            format!(
                "Failed to copy embedded stdlib from {} to {}",
                source_dir.display(),
                fork_dir.display()
            )
        })?;
    }

    if patch_updated {
        let new_toml = toml::to_string_pretty(config)?;
        fs::write(pcb_toml_path, new_toml)
            .with_context(|| format!("Failed to write {}", pcb_toml_path.display()))?;
    }

    Ok(ForkSuccess {
        fork_dir,
        module_url,
        version: pcb_zen_core::TOOLCHAIN_VERSION.to_string(),
        patch_path: relative_fork_path,
    })
}

/// Update the [patch] section in the config. Returns true if config was modified.
fn update_patch_section(
    config: &mut PcbToml,
    module_url: &str,
    relative_fork_path: &str,
    force: bool,
) -> Result<bool> {
    if let Some(existing_patch) = config.patch.get(module_url) {
        // Check if it's already pointing to the same path
        if existing_patch.path.as_deref() == Some(relative_fork_path)
            && existing_patch.branch.is_none()
            && existing_patch.rev.is_none()
        {
            // Already correctly configured
            return Ok(false);
        }

        // There's a different patch
        if !force {
            anyhow::bail!(
                "A patch for '{}' already exists in pcb.toml (path={:?}, branch={:?}, rev={:?}).\n\
                 Remove it manually or use --force to override.",
                module_url,
                existing_patch.path,
                existing_patch.branch,
                existing_patch.rev
            );
        }
    }

    // Add or update the patch entry
    config.patch.insert(
        module_url.to_string(),
        PatchSpec {
            path: Some(relative_fork_path.to_string()),
            branch: None,
            rev: None,
        },
    );

    Ok(true)
}

/// Find a versioned package by walking up the path segments.
///
/// Supports forking by .zen file path (e.g., `reference/TCA9406x/TCA9406x.zen`)
/// by finding the parent package (`reference/TCA9406x`).
fn find_versioned_package<'a>(
    all_versions: &'a BTreeMap<String, Vec<Version>>,
    requested_path: &'a str,
    repo_url: &str,
) -> Result<(&'a str, &'a Vec<Version>)> {
    // Try exact match first
    if let Some(versions) = all_versions.get(requested_path) {
        return Ok((requested_path, versions));
    }

    // Walk up parent paths
    let mut path = requested_path;
    while let Some(parent_end) = path.rfind('/') {
        path = &requested_path[..parent_end];
        if let Some(versions) = all_versions.get(path) {
            return Ok((path, versions));
        }
    }

    // Root package is only valid when the caller explicitly requested the repo root.
    if requested_path.is_empty()
        && let Some(versions) = all_versions.get("")
    {
        return Ok(("", versions));
    }

    // Error with available packages
    let available_packages: Vec<_> = all_versions.keys().take(10).collect();
    if available_packages.is_empty() {
        anyhow::bail!("No tagged versions found in repository {}", repo_url)
    } else {
        anyhow::bail!(
            "No tagged versions found for path '{}' in {}.\nAvailable packages: {}{}",
            requested_path,
            repo_url,
            available_packages
                .iter()
                .map(|s| {
                    if s.is_empty() {
                        "<repo root>"
                    } else {
                        s.as_str()
                    }
                })
                .collect::<Vec<_>>()
                .join(", "),
            if all_versions.len() > 10 { ", ..." } else { "" }
        )
    }
}

fn parse_fork_url_and_version(url: &str) -> Result<(&str, Option<&str>)> {
    let Some(at_pos) = url.rfind('@') else {
        return Ok((url, None));
    };

    if at_pos == 0 {
        return Ok((url, None));
    }

    let module_url = &url[..at_pos];
    let version = url[at_pos + 1..].trim();
    if version.is_empty() {
        anyhow::bail!(
            "Missing version after '@' in '{}'.\nUse format: pcb fork add {}@<version>",
            url,
            module_url
        );
    }

    if parse_version(version).is_none() {
        anyhow::bail!(
            "Invalid version suffix in '{}'.\nUse format: pcb fork add {}@0.4.0 or pcb fork add {} --version 0.4.0",
            url,
            module_url,
            module_url
        );
    }

    Ok((module_url, Some(version)))
}

/// Result of a successful upstream operation
pub struct UpstreamResult {
    /// Name of the branch that was pushed
    pub branch_name: String,
    /// Packages that were copied: (fork_path, target_path)
    pub packages: Vec<(String, String)>,
    /// URL to create a PR
    pub pr_url: String,
}

/// Upstream all forked packages to a branch on the registry.
///
/// This function:
/// 1. Finds all path patches in pcb.toml (these are local forks)
/// 2. Clones/fetches the registry to a staging directory
/// 3. Creates a branch named `fork/<owner>/<repo>` based on workspace git remote
/// 4. Copies fork contents to the appropriate registry locations
/// 5. Commits and pushes the branch
pub fn upstream_forks(dry_run: bool) -> Result<UpstreamResult> {
    let cwd = std::env::current_dir()?;
    let file_provider = DefaultFileProvider::new();
    let workspace_info = get_workspace_info(&file_provider, &cwd, true)
        .context("Failed to detect PCB workspace (no pcb.toml found up the tree?)")?;
    let workspace_root = &workspace_info.root;

    // Load and validate workspace
    let pcb_toml_path = workspace_root.join("pcb.toml");
    let config = PcbToml::from_file(&file_provider, &pcb_toml_path)?;

    // Collect all path patches (these are forks)
    let forks: Vec<_> = config
        .patch
        .iter()
        .filter_map(|(url, spec)| spec.path.as_ref().map(|path| (url.clone(), path.clone())))
        .collect();

    if forks.is_empty() {
        return Ok(UpstreamResult {
            branch_name: String::new(),
            packages: vec![],
            pr_url: String::new(),
        });
    }

    // Determine branch name from workspace git remote
    let branch_name = compute_branch_name(workspace_root)?;

    // All forks should be in the same registry - validate and extract registry URL
    let registry_url = extract_registry_url(&forks)?;

    // Staging directory for the registry clone
    let staging_dir = upstream_staging_dir(&registry_url);

    // Clone or fetch the registry
    if staging_dir.exists() {
        git::fetch(&staging_dir, "origin")
            .with_context(|| format!("Failed to fetch registry at {}", staging_dir.display()))?;
    } else {
        git::clone_with_fallback(&registry_url, &staging_dir)
            .with_context(|| format!("Failed to clone registry {}", registry_url))?;
    }

    // Create/reset branch from origin/main
    git::checkout_branch_reset(&staging_dir, &branch_name, "origin/main")
        .with_context(|| format!("Failed to create branch {}", branch_name))?;

    // Copy each fork to the registry
    let mut packages = Vec::new();
    for (module_url, fork_path) in &forks {
        let target_path = compute_registry_target_path(module_url)?;
        let fork_full_path = workspace_root.join(fork_path);

        if !fork_full_path.exists() {
            continue;
        }

        if !dry_run {
            let target_full_path = staging_dir.join(&target_path);
            if target_full_path.exists() {
                fs::remove_dir_all(&target_full_path)?;
            }
            copy_dir_filtered(&fork_full_path, &target_full_path)?;
        }

        packages.push((fork_path.clone(), target_path));
    }

    let pr_url = format!("https://{}/compare/{}", registry_url, branch_name);

    if dry_run || packages.is_empty() {
        return Ok(UpstreamResult {
            branch_name,
            packages,
            pr_url,
        });
    }

    // Commit and push
    let commit_message = format!(
        "Update {} forked package(s)\n\nUpstreamed from workspace via `pcb fork upstream`",
        packages.len()
    );
    git::commit(&staging_dir, &commit_message).context("Failed to commit changes")?;
    git::push_branch_force(&staging_dir, &branch_name, "origin")
        .with_context(|| format!("Failed to push branch {}", branch_name))?;

    Ok(UpstreamResult {
        branch_name,
        packages,
        pr_url,
    })
}

/// Compute a deterministic branch name from the workspace's git remote
fn compute_branch_name(workspace_root: &Path) -> Result<String> {
    let (owner, repo) = git::remote_owner_repo(workspace_root)
        .context("Failed to get git remote URL. Is this a git repository with a remote?")?;
    let owner_repo = format!("{owner}/{repo}");

    Ok(format!("fork/{}", owner_repo))
}

/// Extract the registry URL from fork module URLs, validating they're all from the same registry
fn extract_registry_url(forks: &[(String, String)]) -> Result<String> {
    let mut registry_url: Option<String> = None;

    for (module_url, _) in forks {
        let (repo_url, _) = split_repo_and_subpath(module_url);

        if let Some(ref existing) = registry_url {
            if existing != repo_url {
                anyhow::bail!(
                    "All forked packages must be from the same registry.\n\
                     Found packages from both '{}' and '{}'",
                    existing,
                    repo_url
                );
            }
        } else {
            registry_url = Some(repo_url.to_string());
        }
    }

    registry_url.ok_or_else(|| anyhow::anyhow!("No forks found"))
}

/// Compute the target path in the registry for a module URL
///
/// Input: module_url = "github.com/diodeinc/registry/modules/Foo"
/// Output: "modules/Foo" (strip host and registry name)
fn compute_registry_target_path(module_url: &str) -> Result<String> {
    let (_, subpath) = split_repo_and_subpath(module_url);
    if subpath.is_empty() {
        anyhow::bail!(
            "Cannot upstream root package. Module URL must include a subpath: {}",
            module_url
        );
    }
    Ok(subpath.to_string())
}

/// Get the staging directory for a registry
fn upstream_staging_dir(registry_url: &str) -> PathBuf {
    cache_base()
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("upstream-staging")
        .join(registry_url)
}

/// Copy directory recursively, excluding root-level git files that shouldn't be in packages.
/// These files get included by sparse-checkout cone mode but don't belong in the package.
fn copy_dir_filtered(src: &Path, dst: &Path) -> Result<()> {
    const EXCLUDED: &[&str] = &[".gitattributes", ".gitignore", ".git"];

    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Skip excluded files at root level
        if EXCLUDED.iter().any(|&e| name_str == e) {
            continue;
        }

        let src_path = entry.path();
        let dst_path = dst.join(&name);

        if src_path.is_dir() {
            // Recurse into subdirectories (no filtering needed for nested dirs)
            copy_dir_all(&src_path, &dst_path, &HashSet::new())?;
        } else {
            fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    fn write_min_workspace_manifest(path: &Path) {
        fs::write(
            path,
            r#"[workspace]
name = "tmp"
pcb-version = "0.3"
"#,
        )
        .expect("write pcb.toml");
    }

    #[test]
    fn fork_stdlib_materializes_and_writes_patch() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let workspace_root = temp.path();
        let pcb_toml_path = workspace_root.join("pcb.toml");
        write_min_workspace_manifest(&pcb_toml_path);

        let file_provider = DefaultFileProvider::new();
        let mut config =
            PcbToml::from_file(&file_provider, &pcb_toml_path).expect("parse pcb.toml");

        let result =
            fork_stdlib(workspace_root, &pcb_toml_path, &mut config, false).expect("fork stdlib");

        assert_eq!(result.module_url, pcb_zen_core::STDLIB_MODULE_PATH);
        assert_eq!(result.patch_path, "fork/stdlib");
        assert!(workspace_root.join(".pcb/stdlib/units.zen").exists());
        assert!(workspace_root.join("fork/stdlib/units.zen").exists());
        assert_eq!(
            config
                .patch
                .get(pcb_zen_core::STDLIB_MODULE_PATH)
                .and_then(|p| p.path.as_deref()),
            Some("fork/stdlib")
        );
    }

    #[test]
    fn parse_fork_url_and_version_supports_at_version_syntax() {
        assert_eq!(
            parse_fork_url_and_version("github.com/diodeinc/registry/reference/IRQ-W80x@0.4.0")
                .unwrap(),
            (
                "github.com/diodeinc/registry/reference/IRQ-W80x",
                Some("0.4.0")
            )
        );
        assert_eq!(
            parse_fork_url_and_version("github.com/diodeinc/registry/reference/IRQ-W80x@v0.4.0")
                .unwrap(),
            (
                "github.com/diodeinc/registry/reference/IRQ-W80x",
                Some("v0.4.0")
            )
        );
        assert_eq!(
            parse_fork_url_and_version("github.com/diodeinc/registry/reference/IRQ-W80x").unwrap(),
            ("github.com/diodeinc/registry/reference/IRQ-W80x", None)
        );
    }

    #[test]
    fn parse_fork_url_and_version_rejects_invalid_suffix() {
        let err =
            parse_fork_url_and_version("github.com/diodeinc/registry/reference/IRQ-W80x@latest")
                .unwrap_err();
        assert!(
            err.to_string().contains("Invalid version suffix"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    #[serial]
    fn specifying_version_twice_is_rejected() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let workspace_root = temp.path();
        let pcb_toml_path = workspace_root.join("pcb.toml");
        write_min_workspace_manifest(&pcb_toml_path);

        let original_cwd = std::env::current_dir().expect("get cwd");
        std::env::set_current_dir(workspace_root).expect("set cwd");

        let result = fork_package(ForkOptions {
            url: "github.com/diodeinc/registry/reference/IRQ-W80x@0.4.0".to_string(),
            version: Some("0.5.0".to_string()),
            force: false,
        });

        std::env::set_current_dir(original_cwd).expect("restore cwd");

        let err = match result {
            Ok(_) => panic!("expected fork_package to fail"),
            Err(err) => err,
        };

        assert!(
            err.to_string().contains("Version specified twice"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn find_versioned_package_does_not_fall_back_to_repo_root_for_missing_package() {
        let all_versions = BTreeMap::from([
            ("".to_string(), vec![Version::new(0, 3, 17)]),
            (
                "reference/IRQ-W80x".to_string(),
                vec![Version::new(0, 4, 0), Version::new(0, 3, 3)],
            ),
        ]);

        let err = find_versioned_package(
            &all_versions,
            "reference/DOES-NOT-EXIST",
            "github.com/foo/bar",
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("No tagged versions found for path 'reference/DOES-NOT-EXIST'"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn find_versioned_package_still_walks_up_from_zen_file_path() {
        let all_versions = BTreeMap::from([(
            "reference/IRQ-W80x".to_string(),
            vec![Version::new(0, 4, 0), Version::new(0, 3, 3)],
        )]);

        let (path, versions) = find_versioned_package(
            &all_versions,
            "reference/IRQ-W80x/IRQ-W80x.zen",
            "github.com/foo/bar",
        )
        .unwrap();

        assert_eq!(path, "reference/IRQ-W80x");
        assert_eq!(versions.first(), Some(&Version::new(0, 4, 0)));
    }
}
