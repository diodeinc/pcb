use anyhow::{Context, Result};
use globset::{Glob, GlobSetBuilder};
use pcb_zen_core::config::{ManifestPart, split_repo_and_subpath};
use pcb_zen_core::resolution::{FrozenResolutionMap, ResolutionResult, build_package_roots};
use semver::Version;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use tracing::instrument;

use crate::cache_index::{CacheIndex, cache_base, ensure_source_repo, source_repo_dir};
use crate::git;
use crate::workspace::WorkspaceInfo;
use pcb_canonical::{
    CanonicalTarOptions, compute_content_hash_from_dir, compute_manifest_hash, copy_canonical_files,
};

/// Result of vendoring operation
pub struct VendorResult {
    /// Number of packages vendored
    pub package_count: usize,
    /// Number of stale entries pruned from vendor/
    pub pruned_count: usize,
    /// Path to vendor directory
    pub vendor_dir: PathBuf,
}

pub struct VendorCopy {
    pub src: PathBuf,
    pub dst: PathBuf,
}

pub struct VendorPlan {
    pub vendor_dir: PathBuf,
    pub copies: Vec<VendorCopy>,
    pub prunes: Vec<PathBuf>,
}

impl VendorPlan {
    pub fn is_empty(&self) -> bool {
        self.copies.is_empty() && self.prunes.is_empty()
    }

    pub fn apply(&self) -> Result<VendorResult> {
        for copy in &self.copies {
            copy_canonical_files(
                &copy.src,
                &copy.dst,
                Some(CanonicalTarOptions {
                    exclude_nested_packages: true,
                }),
            )?;
        }

        for root in &self.prunes {
            log::debug!("Pruning stale vendor path: {}", root.display());
            fs::remove_dir_all(root)?;
            remove_empty_ancestors_until(&self.vendor_dir, root)?;
        }

        Ok(VendorResult {
            package_count: self.copies.len(),
            pruned_count: self.prunes.len(),
            vendor_dir: self.vendor_dir.clone(),
        })
    }
}

/// Vendor dependencies from cache to vendor directory
///
/// Vendors package entries matching workspace.vendor patterns plus any additional_patterns.
/// No-op if combined patterns is empty. Incremental - skips existing entries.
///
/// If `target_vendor_dir` is provided, vendors to that directory instead of
/// `workspace_info.root/vendor`. This is used by `pcb publish` to vendor into
/// the staging directory.
///
/// This function performs an incremental sync:
/// - Adds matching packages from the resolution that are missing in vendor/
/// - When `prune=true`, removes any {url}/{version-or-ref} directories not in the resolution
///
/// Pruning should be disabled when offline (can't re-fetch deleted deps).
#[instrument(name = "vendor_deps", skip_all)]
pub fn vendor_deps(
    resolution: &ResolutionResult,
    additional_patterns: &[String],
    target_vendor_dir: Option<&Path>,
    prune: bool,
) -> Result<VendorResult> {
    let package_roots: BTreeSet<_> = resolution
        .remote_package_versions()
        .into_iter()
        .flat_map(|(path, versions)| {
            versions
                .into_iter()
                .map(move |version| (path.clone(), version))
        })
        .collect();
    vendor_package_roots(
        &resolution.workspace_info,
        &package_roots,
        additional_patterns,
        target_vendor_dir,
        prune,
    )
}

#[instrument(name = "vendor_package_roots", skip_all)]
pub fn vendor_package_roots(
    workspace_info: &WorkspaceInfo,
    package_roots: &BTreeSet<(String, String)>,
    additional_patterns: &[String],
    target_vendor_dir: Option<&Path>,
    prune: bool,
) -> Result<VendorResult> {
    plan_vendor_package_roots(
        workspace_info,
        package_roots,
        additional_patterns,
        target_vendor_dir,
        prune,
    )?
    .apply()
}

#[instrument(name = "plan_vendor_package_roots", skip_all)]
pub fn plan_vendor_package_roots(
    workspace_info: &WorkspaceInfo,
    package_roots: &BTreeSet<(String, String)>,
    additional_patterns: &[String],
    target_vendor_dir: Option<&Path>,
    prune: bool,
) -> Result<VendorPlan> {
    let vendor_dir = target_vendor_dir
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_info.root.join("vendor"));

    // Combine workspace.vendor patterns with additional patterns
    let mut patterns: Vec<&str> = workspace_info
        .config
        .as_ref()
        .and_then(|c| c.workspace.as_ref())
        .map(|w| w.vendor.iter().map(|s| s.as_str()).collect())
        .unwrap_or_default();
    patterns.extend(additional_patterns.iter().map(|s| s.as_str()));

    // No patterns = no-op
    if patterns.is_empty() {
        log::debug!("No vendor patterns configured, skipping vendoring");
        return Ok(VendorPlan {
            vendor_dir,
            copies: Vec::new(),
            prunes: Vec::new(),
        });
    }
    log::debug!("Vendor patterns: {:?}", patterns);

    let cache = &workspace_info.cache_dir;
    let workspace_vendor = workspace_info.root.join("vendor");

    // Build glob matcher
    let mut builder = GlobSetBuilder::new();
    for pattern in &patterns {
        builder.add(Glob::new(pattern)?);
    }
    let glob_set = builder.build()?;

    // Track all desired {url}/{version-or-ref} roots for pruning stale entries
    let mut desired_roots: HashSet<PathBuf> = HashSet::new();

    // Copy matching packages from workspace vendor or cache (vendor takes precedence)
    let mut copies = Vec::new();
    for (path, version) in package_roots {
        if !glob_set.is_match(path) {
            continue;
        }

        // Track this package root for pruning
        let rel_root = PathBuf::from(path).join(version);
        desired_roots.insert(rel_root);

        let dst = vendor_dir.join(path).join(version);
        if dst.exists() {
            continue;
        }

        let Some(src) = remote_package_vendor_source(&workspace_vendor, cache, path, version)
        else {
            continue;
        };

        copies.push(VendorCopy { src, dst });
    }

    let prunes = if prune {
        collect_stale_vendor_roots(&vendor_dir, &desired_roots)?
    } else {
        Vec::new()
    };

    Ok(VendorPlan {
        vendor_dir,
        copies,
        prunes,
    })
}

fn remote_package_vendor_source(
    workspace_vendor: &Path,
    cache_dir: &Path,
    module_path: &str,
    version: &str,
) -> Option<PathBuf> {
    let vendor_src = workspace_vendor.join(module_path).join(version);
    if vendor_src.exists() {
        return Some(vendor_src);
    }

    let cache_src = cache_dir.join(module_path).join(version);
    if cache_src.exists() {
        Some(cache_src)
    } else {
        None
    }
}

fn remove_empty_ancestors_until(base: &Path, removed_root: &Path) -> Result<()> {
    let Some(mut current) = removed_root.parent().map(Path::to_path_buf) else {
        return Ok(());
    };

    while current.starts_with(base) && current != base {
        if current.read_dir()?.next().is_some() {
            break;
        }
        fs::remove_dir(&current)?;
        let Some(parent) = current.parent().map(Path::to_path_buf) else {
            break;
        };
        current = parent;
    }
    Ok(())
}

/// Recursively copy a directory, excluding hidden directories/files and symlinks.
///
/// Optionally excludes specified directory roots (used when copying workspace
/// packages to exclude nested packages that are separate workspace packages).
pub fn copy_dir_all(src: &Path, dst: &Path, excluded_roots: &HashSet<PathBuf>) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        // Skip hidden files/directories (starting with .)
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let src_path = entry.path();
        let dst_path = dst.join(name);
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            // Skip if this directory is the root of another workspace package
            if excluded_roots.contains(&src_path) {
                log::debug!(
                    "Skipping nested package dir during staging: {}",
                    src_path.display()
                );
                continue;
            }
            copy_dir_all(&src_path, &dst_path, excluded_roots)?;
        } else {
            fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

/// Collect stale directories from vendor/
///
/// Walks vendor/ recursively and returns directories not in desired_roots
/// or on the path to a desired root.
fn collect_stale_vendor_roots(
    vendor_dir: &Path,
    desired_roots: &HashSet<PathBuf>,
) -> Result<Vec<PathBuf>> {
    if !vendor_dir.exists() {
        return Ok(Vec::new());
    }

    // Build set of ancestor paths (paths we must traverse to reach desired roots)
    let mut ancestors: HashSet<PathBuf> = HashSet::new();
    for root in desired_roots {
        let mut ancestor = PathBuf::new();
        for component in root.components() {
            ancestors.insert(ancestor.clone());
            ancestor.push(component);
        }
    }

    let mut stale_roots = Vec::new();
    collect_stale_dir(
        vendor_dir,
        &PathBuf::new(),
        desired_roots,
        &ancestors,
        &mut stale_roots,
    )?;
    Ok(stale_roots)
}

fn collect_stale_dir(
    base: &Path,
    rel: &Path,
    desired_roots: &HashSet<PathBuf>,
    ancestors: &HashSet<PathBuf>,
    stale_roots: &mut Vec<PathBuf>,
) -> Result<()> {
    let mut entries = fs::read_dir(base.join(rel))?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let name = entry.file_name();
        let child_rel = if rel.as_os_str().is_empty() {
            PathBuf::from(&name)
        } else {
            rel.join(&name)
        };

        if entry.file_type()?.is_dir() {
            if desired_roots.contains(&child_rel) {
                // This is a desired root - keep everything inside it
                continue;
            } else if ancestors.contains(&child_rel) {
                collect_stale_dir(base, &child_rel, desired_roots, ancestors, stale_roots)?;
            } else {
                // Not needed - stale root, prune entire subtree on apply
                stale_roots.push(entry.path());
            }
        }
        // Files at the root level of vendor/ shouldn't exist, ignore them
    }
    Ok(())
}

/// Returns a dependency manifest using the shared cache-backed materialization path.
pub fn ensure_package_manifest_in_cache(
    module_path: &str,
    version: &Version,
    index: &CacheIndex,
) -> Result<PathBuf> {
    let checkout_dir = cache_base().join(module_path).join(version.to_string());
    let version_str = version.to_string();
    let pcb_toml_path = checkout_dir.join("pcb.toml");

    if index.get_package(module_path, &version_str).is_some() && pcb_toml_path.exists() {
        return Ok(pcb_toml_path);
    }

    ensure_sparse_checkout(&checkout_dir, module_path, &version_str)?;

    let content_hash = compute_content_hash_from_dir(&checkout_dir)?;
    let manifest_content = std::fs::read_to_string(&pcb_toml_path)?;
    let manifest_hash = compute_manifest_hash(&manifest_content);

    verify_tag_hashes(module_path, version, &content_hash, &manifest_hash)?;
    index.set_package(module_path, &version_str, &content_hash, &manifest_hash)?;

    Ok(pcb_toml_path)
}

fn add_parts_to_symbol_map(
    result: &mut HashMap<String, Vec<ManifestPart>>,
    package_roots: &BTreeMap<String, PathBuf>,
    parts: &[ManifestPart],
    pkg_dir: &Path,
) -> Result<()> {
    for part in parts {
        let abs_symbol = pkg_dir.join(&part.symbol);
        if let Some(uri) = pcb_sch::format_package_uri(&abs_symbol, package_roots) {
            result.entry(uri).or_default().push(part.clone());
        } else {
            log::warn!(
                "Could not resolve symbol path '{}' in {} to a package URI",
                part.symbol,
                pkg_dir.display()
            );
        }
    }

    Ok(())
}

/// Build the symbol → parts mapping from all manifests in scope.
///
/// Iterates workspace packages plus any resolved dependency roots that have a
/// parts-bearing manifest, resolving each `ManifestPart.symbol` into a
/// `package://` URI.
pub fn build_frozen_symbol_parts(
    workspace_info: &pcb_zen_core::workspace::WorkspaceInfo,
    resolution: &FrozenResolutionMap,
) -> Result<HashMap<String, Vec<ManifestPart>>> {
    let mut result: HashMap<String, Vec<ManifestPart>> = HashMap::new();
    let package_roots = build_package_roots(
        workspace_info,
        resolution.packages.values().map(|package| &package.deps),
    );

    for (pkg_root, package) in &resolution.packages {
        if !package.parts.is_empty() {
            add_parts_to_symbol_map(&mut result, &package_roots, &package.parts, pkg_root)
                .with_context(|| {
                    format!("Failed to build symbol parts from {}", pkg_root.display())
                })?;
        }
    }

    Ok(result)
}

/// Verify computed hashes match the expected hashes from the git tag annotation
fn verify_tag_hashes(
    module_path: &str,
    version: &Version,
    content_hash: &str,
    manifest_hash: &str,
) -> Result<()> {
    let (repo_url, subpath) = split_repo_and_subpath(module_path);
    let source_dir = source_repo_dir(repo_url)?;
    let tag_name = if subpath.is_empty() {
        format!("v{}", version)
    } else {
        format!("{}/v{}", subpath, version)
    };

    // Read the annotated tag directly from the shared source repo. Materialized
    // cache directories are plain extracted files now, not git repos.
    let Some(tag_body) = git::cat_file(&source_dir, &tag_name) else {
        return Ok(());
    };

    let Some((expected_content, expected_manifest)) = parse_hashes_from_tag_body(&tag_body) else {
        return Ok(());
    };

    fn check_hash(
        kind: &str,
        computed: &str,
        expected: &str,
        module_path: &str,
        version: &Version,
    ) -> Result<()> {
        if computed != expected {
            anyhow::bail!(
                "{} hash mismatch for {}@v{}\n  \
                Expected (from tag): {}\n  \
                Computed:            {}\n\n\
                This may indicate a bug in the packaging toolchain.",
                kind,
                module_path,
                version,
                expected,
                computed
            );
        }
        Ok(())
    }

    check_hash(
        "Content",
        content_hash,
        &expected_content,
        module_path,
        version,
    )?;
    check_hash(
        "Manifest",
        manifest_hash,
        &expected_manifest,
        module_path,
        version,
    )?;

    Ok(())
}

/// Parse content and manifest hashes from tag annotation body
fn parse_hashes_from_tag_body(body: &str) -> Option<(String, String)> {
    let mut content_hash = None;
    let mut manifest_hash = None;

    for line in body.lines() {
        let line = line.trim();
        if let Some(hash_start) = line.find(" h1:") {
            let hash = line[hash_start + 1..].to_string();
            if line[..hash_start].ends_with("/pcb.toml") {
                manifest_hash = Some(hash);
            } else {
                content_hash = Some(hash);
            }
        }
    }

    content_hash.zip(manifest_hash)
}

/// Populate a cache directory with exclusive locking.
///
/// Only one process fetches; others wait for the lock and then see the completed result.
/// If the fetching process crashes, the OS releases the lock and waiters retry.
fn populate_cache<F>(cache_dir: &Path, marker: &str, fetch: F) -> Result<PathBuf>
where
    F: FnOnce(&Path) -> Result<()>,
{
    // Fast path: already complete
    if cache_dir.join(marker).exists() {
        return Ok(cache_dir.to_path_buf());
    }

    // Acquire exclusive lock (blocks until available, auto-releases on crash)
    let _lock = git::lock_dir(cache_dir)?;

    // Double-check after acquiring lock
    if cache_dir.join(marker).exists() {
        return Ok(cache_dir.to_path_buf());
    }

    // Clean up any incomplete cache before fetching
    let _ = std::fs::remove_dir_all(cache_dir);
    std::fs::create_dir_all(cache_dir)?;

    fetch(cache_dir)?;

    Ok(cache_dir.to_path_buf())
}

/// Ensure a cached package checkout for a specific version.
///
/// On a warm cache hit, this returns the existing immutable cache entry without
/// touching Git. On a cold miss, it materializes the package once from the
/// shared source repo into `~/.pcb/cache/...`, then later builds reuse that cache.
/// Tagged versions archive from the version tag; pseudo-versions archive from
/// the pinned commit.
///
/// Returns the package root path (where pcb.toml lives)
pub fn ensure_sparse_checkout(
    checkout_dir: &Path,
    module_path: &str,
    version_str: &str,
) -> Result<PathBuf> {
    let marker = "pcb.toml";
    let (repo_url, subpath) = split_repo_and_subpath(module_path);

    populate_cache(checkout_dir, marker, |dest| {
        let is_pseudo_version = version_str.contains("-0.");

        // Construct ref_spec (tag name or commit hash)
        // For pseudo-versions, use commit hash directly (no subpath prefix)
        // For regular versions, include subpath prefix in tag name
        let ref_spec = if is_pseudo_version {
            version_str.rsplit('-').next().unwrap().to_string()
        } else {
            let version_part = format!("v{}", version_str);
            if subpath.is_empty() {
                version_part
            } else {
                format!("{}/{}", subpath, version_part)
            }
        };

        fetch_via_git(dest, repo_url, &ref_spec, subpath, is_pseudo_version)
            .with_context(|| format!("Failed to fetch {} via git sparse checkout", module_path))?;
        Ok(())
    })
}

/// Materialize a repo ref into a package directory.
fn fetch_via_git(
    dest: &Path,
    repo_url: &str,
    ref_spec: &str,
    subpath: &str,
    is_pseudo: bool,
) -> Result<()> {
    // Materialize packages directly from the shared source checkout instead of
    // creating a temporary repo just to fetch, sparse-checkout, and flatten a
    // subdirectory.
    std::fs::create_dir_all(dest)?;
    let source_dir = ensure_source_repo(repo_url)?;

    if is_pseudo {
        git::ensure_rev_in_source_repo(&source_dir, ref_spec)?;
    }
    let ref_name = ref_spec.to_string();
    let treeish = if subpath.is_empty() {
        ref_name
    } else {
        format!("{ref_name}:{subpath}")
    };
    git::archive_to_dir(&source_dir, &treeish, dest)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::WorkspacePackage;
    use pcb_zen_core::config::PcbToml;
    use tempfile::TempDir;

    fn workspace_with_root_config(config: PcbToml) -> WorkspaceInfo {
        let mut packages = BTreeMap::new();
        packages.insert(
            "workspace".to_string(),
            WorkspacePackage {
                rel_path: PathBuf::new(),
                config: config.clone(),
                version: None,
                published_at: None,
                preferred: false,
                dirty: false,
                entrypoints: Vec::new(),
                symbol_files: Vec::new(),
            },
        );

        WorkspaceInfo {
            root: PathBuf::from("/workspace"),
            cache_dir: PathBuf::new(),
            config: Some(config),
            packages,
            errors: vec![],
        }
    }

    #[test]
    fn test_build_frozen_symbol_parts_allows_manifest_parts_without_symbol_name() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().to_path_buf();
        std::fs::write(
            root.join("Device.kicad_sym"),
            r#"(kicad_symbol_lib
  (symbol "Symbol1"
    (property "Reference" "U" (at 0 0 0))
    (property "Value" "Symbol1" (at 0 0 0))
  )
  (symbol "Symbol2"
    (property "Reference" "U" (at 0 0 0))
    (property "Value" "Symbol2" (at 0 0 0))
  )
)
"#,
        )
        .unwrap();

        let mut workspace = workspace_with_root_config(PcbToml::default());
        workspace.root = root.clone();
        let resolution = FrozenResolutionMap {
            selected_remote: BTreeMap::new(),
            packages: BTreeMap::from([(
                root.clone(),
                pcb_zen_core::resolution::FrozenPackage {
                    identity: pcb_zen_core::resolution::FrozenPackageIdentity::Workspace(
                        "workspace".to_string(),
                    ),
                    deps: BTreeMap::new(),
                    parts: vec![ManifestPart {
                        mpn: "GENERIC".to_string(),
                        symbol: "Device.kicad_sym".to_string(),
                        symbol_name: None,
                        manufacturer: "Acme".to_string(),
                        qualifications: Vec::new(),
                        datasheet: None,
                    }],
                },
            )]),
        };
        let symbol_parts = build_frozen_symbol_parts(&workspace, &resolution)
            .expect("workspace manifest parts should not require symbol_name");
        let parts = symbol_parts
            .get("package://workspace/Device.kicad_sym")
            .expect("expected symbol parts entry");
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].symbol_name, None);
    }
}
