use anyhow::{Context, Result};
use globset::{Glob, GlobSetBuilder};
use pcb_eda::kicad::symbol_library::KicadSymbolLibrary;
use pcb_ui::Spinner;
use pcb_zen_core::config::{ManifestPart, PcbToml, split_repo_and_subpath};
use pcb_zen_core::kicad_library::{
    KICAD_PARTS_INDEX_FILE, kicad_http_mirror_template_for_repo, kicad_parts_url_for_symbol_repo,
    render_repo_url_template,
};
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
        return Ok(VendorResult {
            package_count: 0,
            pruned_count: 0,
            vendor_dir,
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

    fs::create_dir_all(&vendor_dir)?;

    // Track all desired {url}/{version-or-ref} roots for pruning stale entries
    let mut desired_roots: HashSet<PathBuf> = HashSet::new();

    // Copy matching packages from workspace vendor or cache (vendor takes precedence)
    let mut package_count = 0;
    for (path, version) in package_roots {
        if !glob_set.is_match(path) {
            continue;
        }

        // Track this package root for pruning
        let rel_root = PathBuf::from(path).join(version);
        desired_roots.insert(rel_root);

        let dst = vendor_dir.join(path).join(version);
        if matches!(
            copy_remote_package_to_vendor(&workspace_vendor, cache, path, version, &dst)?,
            RemotePackageVendorStatus::Copied
        ) {
            package_count += 1;
        }
    }

    // Prune stale {url}/{version-or-ref} directories not in the resolution
    let pruned_count = if prune {
        prune_stale_vendor_roots(&vendor_dir, &desired_roots)?
    } else {
        0
    };

    Ok(VendorResult {
        package_count,
        pruned_count,
        vendor_dir,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemotePackageVendorStatus {
    AlreadyPresent,
    Copied,
    MissingSource,
}

pub fn copy_remote_package_to_vendor(
    workspace_vendor: &Path,
    cache_dir: &Path,
    module_path: &str,
    version: &str,
    dst: &Path,
) -> Result<RemotePackageVendorStatus> {
    if dst.exists() {
        return Ok(RemotePackageVendorStatus::AlreadyPresent);
    }

    let vendor_src = workspace_vendor.join(module_path).join(version);
    let cache_src = cache_dir.join(module_path).join(version);
    let src = if vendor_src.exists() {
        vendor_src
    } else {
        cache_src
    };
    if !src.exists() {
        return Ok(RemotePackageVendorStatus::MissingSource);
    }

    copy_canonical_files(
        &src,
        dst,
        Some(CanonicalTarOptions {
            exclude_nested_packages: true,
            ..Default::default()
        }),
    )?;
    Ok(RemotePackageVendorStatus::Copied)
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

/// Prune stale {path}/{version} directories from vendor/
///
/// Walks vendor/ recursively and removes directories not in desired_roots
/// or on the path to a desired root. Returns the number of roots pruned.
fn prune_stale_vendor_roots(vendor_dir: &Path, desired_roots: &HashSet<PathBuf>) -> Result<usize> {
    if !vendor_dir.exists() {
        return Ok(0);
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

    let mut pruned = 0;
    prune_dir(
        vendor_dir,
        &PathBuf::new(),
        desired_roots,
        &ancestors,
        &mut pruned,
    )?;
    Ok(pruned)
}

fn prune_dir(
    base: &Path,
    rel: &Path,
    desired_roots: &HashSet<PathBuf>,
    ancestors: &HashSet<PathBuf>,
    pruned: &mut usize,
) -> Result<()> {
    for entry in fs::read_dir(base.join(rel))? {
        let entry = entry?;
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
                // On path to a desired root - recurse to find what to prune
                prune_dir(base, &child_rel, desired_roots, ancestors, pruned)?;
                // Clean up if now empty
                if entry.path().read_dir()?.next().is_none() {
                    fs::remove_dir(entry.path())?;
                }
            } else {
                // Not needed - prune entire subtree
                log::debug!("Pruning stale vendor path: {}", child_rel.display());
                fs::remove_dir_all(entry.path())?;
                *pruned += 1;
            }
        }
        // Files at the root level of vendor/ shouldn't exist, ignore them
    }
    Ok(())
}

/// Materialize asset dependencies selected by dependency resolution.
pub fn materialize_asset_deps<'a>(
    workspace_info: &WorkspaceInfo,
    selected_kicad_assets: impl IntoIterator<Item = (&'a str, &'a Version)>,
    offline: bool,
) -> Result<()> {
    let selected_kicad_assets: BTreeSet<(String, Version)> = selected_kicad_assets
        .into_iter()
        .map(|(repo, version)| (repo.to_string(), version.clone()))
        .collect();

    if selected_kicad_assets.is_empty() {
        return Ok(());
    }

    let workspace_cache = workspace_info.root.join(".pcb/cache");
    let missing: Vec<(String, Version)> = selected_kicad_assets
        .iter()
        .filter(|(repo, version)| {
            !workspace_cache
                .join(repo)
                .join(version.to_string())
                .join(".pcb-cached")
                .exists()
        })
        .map(|(repo, version)| (repo.clone(), version.clone()))
        .collect();

    if offline && !missing.is_empty() {
        let first = &missing[0];
        anyhow::bail!(
            "{}@{} is not cached. Run `pcb build` once online to fetch it.",
            first.0,
            first.1
        );
    }

    let kicad_entries = workspace_info.kicad_library_entries();
    if let Some((first_repo, _)) = missing.first() {
        let total = missing.len();
        let spinner = Spinner::builder(format!(
            "Fetching {}",
            first_repo.rsplit('/').next().unwrap_or(first_repo)
        ))
        .start();

        for (idx, (repo, version)) in missing.into_iter().enumerate() {
            let version_str = version.to_string();
            let repo_name = repo.rsplit('/').next().unwrap_or(&repo);
            spinner.set_message(format!(
                "Fetching [{}/{}] {}@{}",
                idx + 1,
                total,
                repo_name,
                version_str
            ));

            let http_mirror = kicad_http_mirror_template_for_repo(&kicad_entries, &repo, &version)?
                .map(|template| render_repo_url_template(&template, &repo, &version))
                .transpose()
                .with_context(|| {
                    format!(
                        "Failed to render http_mirror URL for {}@{}",
                        repo, version_str
                    )
                })?;

            let cache_dir = cache_base().join(&repo).join(&version_str);
            let fetch_result = ensure_sparse_checkout(
                &cache_dir,
                &repo,
                &version_str,
                false,
                http_mirror.as_deref(),
            )
            .map(|_| ())
            .with_context(|| format!("Failed to fetch {}@{}", repo, version_str));

            if let Err(err) = fetch_result {
                spinner.error(format!("Failed to fetch {}", repo_name));
                return Err(err);
            }
        }

        spinner.finish();
    }

    materialize_kicad_symbol_manifests(&kicad_entries, &selected_kicad_assets, offline)?;
    Ok(())
}

fn materialize_kicad_symbol_manifests(
    kicad_entries: &[pcb_zen_core::config::KicadLibraryConfig],
    selected_kicad_assets: &BTreeSet<(String, Version)>,
    offline: bool,
) -> Result<()> {
    for (repo, version) in selected_kicad_assets {
        ensure_kicad_parts_index(&cache_base(), kicad_entries, repo, version, offline)?;
    }

    Ok(())
}

pub fn ensure_kicad_parts_index(
    cache_root: &Path,
    kicad_entries: &[pcb_zen_core::config::KicadLibraryConfig],
    repo: &str,
    version: &Version,
    offline: bool,
) -> Result<()> {
    let Some(url) = kicad_parts_url_for_symbol_repo(kicad_entries, repo, version)? else {
        return Ok(());
    };
    let cache_dir = cache_root.join(repo).join(version.to_string());
    let index_path = cache_dir.join(KICAD_PARTS_INDEX_FILE);
    if index_path.exists() {
        return Ok(());
    }
    if offline {
        anyhow::bail!(
            "{} is not cached for {}@{}. Run `pcb build` once online to fetch it.",
            KICAD_PARTS_INDEX_FILE,
            repo,
            version
        );
    }

    let _lock = git::lock_dir(&cache_dir)?;
    if index_path.exists() {
        return Ok(());
    }

    let raw_index_path = cache_dir.join(".parts.json.tmp");
    let fetch_result = (|| -> Result<()> {
        let manifest = PcbToml::parse(&crate::archive::fetch_http_text(&url)?)?;
        let normalized = normalize_kicad_parts_index(repo, version, &cache_dir, &manifest.parts)?;
        fs::write(&raw_index_path, serde_json::to_vec(&normalized)?)?;
        fs::rename(&raw_index_path, &index_path)?;
        Ok(())
    })();

    let _ = fs::remove_file(&raw_index_path);
    fetch_result
}

fn normalize_kicad_parts_index(
    repo: &str,
    version: &Version,
    cache_dir: &Path,
    parts: &[ManifestPart],
) -> Result<BTreeMap<String, Vec<ManifestPart>>> {
    let mut normalized_parts = Vec::with_capacity(parts.len());
    let mut symbol_name_cache: HashMap<PathBuf, SymbolNameResolution> = HashMap::new();
    for part in parts {
        let abs_symbol = cache_dir.join(&part.symbol);
        normalized_parts.push(normalize_manifest_part_symbol_name(
            part,
            &abs_symbol,
            &mut symbol_name_cache,
        )?);
    }

    let mut result = HashMap::new();
    let package_roots = BTreeMap::from([(format!("{repo}@{version}"), cache_dir.to_path_buf())]);
    add_parts_to_symbol_map(&mut result, &package_roots, &normalized_parts, cache_dir)?;
    Ok(result.into_iter().collect())
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

    ensure_sparse_checkout(&checkout_dir, module_path, &version_str, true, None)?;

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

fn normalize_manifest_part_symbol_name(
    part: &ManifestPart,
    abs_symbol: &Path,
    symbol_name_cache: &mut HashMap<PathBuf, SymbolNameResolution>,
) -> Result<ManifestPart> {
    if part.symbol_name.is_some() {
        return Ok(part.clone());
    }

    let resolution = symbol_name_cache
        .entry(abs_symbol.to_path_buf())
        .or_insert_with(|| SymbolNameResolution::from_symbol_library(abs_symbol))
        .clone();

    match resolution {
        SymbolNameResolution::Single(only_name) => {
            let mut normalized = part.clone();
            normalized.symbol_name = Some(only_name);
            Ok(normalized)
        }
        SymbolNameResolution::Empty => anyhow::bail!(
            "Manifest part for '{}' cannot infer `symbol_name` because {} contains no symbols",
            part.symbol,
            abs_symbol.display()
        ),
        SymbolNameResolution::Multiple(symbol_names) => anyhow::bail!(
            "Manifest part for '{}' must set `symbol_name` because {} contains multiple symbols: {}",
            part.symbol,
            abs_symbol.display(),
            symbol_names.join(", ")
        ),
        SymbolNameResolution::Invalid(err) => Err(anyhow::anyhow!(err)).with_context(|| {
            format!(
                "Failed to read KiCad symbol library at {}",
                abs_symbol.display()
            )
        }),
    }
}

#[derive(Clone)]
enum SymbolNameResolution {
    Single(String),
    Empty,
    Multiple(Vec<String>),
    Invalid(String),
}

impl SymbolNameResolution {
    fn from_symbol_library(path: &Path) -> Self {
        match KicadSymbolLibrary::from_file(path) {
            Ok(lib) => {
                let mut names: Vec<String> =
                    lib.symbol_names().into_iter().map(str::to_string).collect();
                names.sort();
                match names.as_slice() {
                    [] => Self::Empty,
                    [only_name] => Self::Single(only_name.clone()),
                    _ => Self::Multiple(names),
                }
            }
            Err(err) => Self::Invalid(err.to_string()),
        }
    }
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
    let kicad_entries = workspace_info.kicad_library_entries();
    let mut seen_roots = HashSet::new();

    for (pkg_root, package) in &resolution.packages {
        seen_roots.insert(pkg_root.clone());
        if !package.parts.is_empty() {
            add_parts_to_symbol_map(&mut result, &package_roots, &package.parts, pkg_root)
                .with_context(|| {
                    format!("Failed to build symbol parts from {}", pkg_root.display())
                })?;
        }
    }

    add_kicad_parts_indexes(&mut result, &package_roots, &kicad_entries, &mut seen_roots)?;
    Ok(result)
}

fn add_kicad_parts_indexes(
    result: &mut HashMap<String, Vec<ManifestPart>>,
    package_roots: &BTreeMap<String, PathBuf>,
    kicad_entries: &[pcb_zen_core::config::KicadLibraryConfig],
    seen_roots: &mut HashSet<PathBuf>,
) -> Result<()> {
    for (package_coord, pkg_root) in package_roots {
        if !seen_roots.insert(pkg_root.clone()) {
            continue;
        }

        let Some((repo, version)) = package_coord.rsplit_once('@') else {
            continue;
        };
        let Ok(version) = Version::parse(version) else {
            continue;
        };
        if kicad_parts_url_for_symbol_repo(kicad_entries, repo, &version)?.is_none() {
            continue;
        }

        let index_path = pkg_root.join(KICAD_PARTS_INDEX_FILE);
        if !index_path.exists() {
            continue;
        }

        let index: BTreeMap<String, Vec<ManifestPart>> = serde_json::from_slice(
            &fs::read(&index_path)
                .with_context(|| format!("Failed to read parts index {}", index_path.display()))?,
        )
        .with_context(|| format!("Failed to parse parts index {}", index_path.display()))?;
        for (symbol_uri, mut manifest_parts) in index {
            result
                .entry(symbol_uri)
                .or_default()
                .append(&mut manifest_parts);
        }
    }

    Ok(())
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

fn is_kicad_asset_repo(module_path: &str) -> bool {
    matches!(
        module_path,
        "gitlab.com/kicad/libraries/kicad-footprints"
            | "gitlab.com/kicad/libraries/kicad-symbols"
            | "gitlab.com/kicad/libraries/kicad-packages3D"
    )
}

const MACOS_SIDECAR_SCRUB_MARKER: &str = ".pcb-macos-sidecars-scrubbed";
const SIDECAR_REMOVE_ATTEMPTS: usize = 4;
const SIDECAR_REMOVE_RETRY_DELAY_MS: u64 = 50;

fn make_sidecar_deletable(path: &Path) -> Result<()> {
    let context = || {
        format!(
            "Failed to update macOS sidecar permissions {}",
            path.display()
        )
    };
    let mut permissions = match fs::metadata(path) {
        Ok(metadata) => metadata.permissions(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err).with_context(context),
    };
    if !permissions.readonly() {
        return Ok(());
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        permissions.set_mode(permissions.mode() | 0o200);
    }

    #[cfg(not(unix))]
    {
        permissions.set_readonly(false);
    }

    match fs::set_permissions(path, permissions) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err).with_context(context),
    }

    Ok(())
}

fn remove_file_if_present(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

fn remove_sidecar_file(path: &Path) -> Result<()> {
    let context = || format!("Failed to remove macOS sidecar {}", path.display());
    for attempt in 0..SIDECAR_REMOVE_ATTEMPTS {
        match remove_file_if_present(path) {
            Ok(()) => return Ok(()),
            Err(err)
                if err.kind() == std::io::ErrorKind::PermissionDenied
                    && attempt + 1 < SIDECAR_REMOVE_ATTEMPTS =>
            {
                let _ = make_sidecar_deletable(path);
                std::thread::sleep(std::time::Duration::from_millis(
                    SIDECAR_REMOVE_RETRY_DELAY_MS,
                ));
            }
            Err(err) => return Err(err).with_context(context),
        }
    }

    Ok(())
}

// Remove this once the KiCad mirrors stop shipping macOS sidecars and
// existing user caches have been refreshed or cleaned.
fn ensure_macos_sidecars_scrubbed(root: &Path) -> Result<()> {
    let marker = root.join(MACOS_SIDECAR_SCRUB_MARKER);
    if marker.exists() {
        return Ok(());
    }

    scrub_macos_sidecars(root)?;
    fs::write(marker, "")?;
    Ok(())
}

fn scrub_macos_sidecars(root: &Path) -> Result<()> {
    if !root.exists() {
        return Ok(());
    }

    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            let file_name = entry.file_name();
            let file_name = file_name.to_string_lossy();

            if path.is_dir() {
                pending.push(path);
                continue;
            }

            if file_name == ".DS_Store" || file_name.starts_with("._") {
                remove_sidecar_file(&path)?;
            }
        }
    }

    Ok(())
}

/// Populate a cache directory with exclusive locking.
///
/// Only one process fetches; others wait for the lock and then see the completed result.
/// If the fetching process crashes, the OS releases the lock and waiters retry.
fn populate_cache<F>(
    cache_dir: &Path,
    marker: &str,
    scrub_macos_sidecars_on_hit: bool,
    fetch: F,
) -> Result<PathBuf>
where
    F: FnOnce(&Path) -> Result<()>,
{
    let return_cached_dir = || -> Result<PathBuf> {
        if scrub_macos_sidecars_on_hit {
            ensure_macos_sidecars_scrubbed(cache_dir)?;
        }
        Ok(cache_dir.to_path_buf())
    };

    // Fast path: already complete
    if cache_dir.join(marker).exists() {
        return return_cached_dir();
    }

    // Acquire exclusive lock (blocks until available, auto-releases on crash)
    let _lock = git::lock_dir(cache_dir)?;

    // Double-check after acquiring lock
    if cache_dir.join(marker).exists() {
        return return_cached_dir();
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
    add_v_prefix: bool,
    http_mirror_url: Option<&str>,
) -> Result<PathBuf> {
    let marker = if add_v_prefix {
        "pcb.toml"
    } else {
        ".pcb-cached"
    };
    let scrub_kicad_cache = !add_v_prefix && is_kicad_asset_repo(module_path);
    let (repo_url, subpath) = split_repo_and_subpath(module_path);

    populate_cache(checkout_dir, marker, scrub_kicad_cache, |dest| {
        let is_pseudo_version = version_str.contains("-0.");

        // Construct ref_spec (tag name or commit hash)
        // For pseudo-versions, use commit hash directly (no subpath prefix)
        // For regular versions, include subpath prefix in tag name
        let ref_spec = if is_pseudo_version {
            version_str.rsplit('-').next().unwrap().to_string()
        } else {
            let version_part = if add_v_prefix {
                format!("v{}", version_str)
            } else {
                version_str.to_string()
            };
            if subpath.is_empty() {
                version_part
            } else {
                format!("{}/{}", subpath, version_part)
            }
        };

        if !add_v_prefix {
            anyhow::ensure!(
                !is_pseudo_version,
                "KiCad library versions must be semver tags, got {} for {}",
                version_str,
                module_path
            );
            anyhow::ensure!(
                subpath.is_empty(),
                "KiCad library must resolve to repo root, got {}",
                module_path
            );

            if let Some(url) = http_mirror_url {
                if let Err(mirror_err) = crate::archive::fetch_http_archive(url, dest) {
                    log::warn!(
                        "HTTP mirror fetch failed for {}@{} ({}); falling back to git sparse checkout",
                        module_path,
                        version_str,
                        mirror_err
                    );
                    let _ = std::fs::remove_dir_all(dest);
                    std::fs::create_dir_all(dest)?;
                    fetch_via_git(dest, repo_url, &ref_spec, subpath, false).with_context(|| {
                        format!(
                            "Failed to fetch {} via git sparse checkout after mirror failure ({})",
                            module_path, url
                        )
                    })?;
                }
            } else {
                fetch_via_git(dest, repo_url, &ref_spec, subpath, false).with_context(|| {
                    format!("Failed to fetch {} via git sparse checkout", module_path)
                })?;
            }
            if scrub_kicad_cache {
                ensure_macos_sidecars_scrubbed(dest)?;
            }
            std::fs::write(dest.join(".pcb-cached"), "")?;
            return Ok(());
        }

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
    fn test_kicad_repos_offline_requires_cache() {
        let temp = TempDir::new().unwrap();
        let mut config = PcbToml::default();
        config.workspace = Some(pcb_zen_core::config::WorkspaceConfig {
            kicad_library: vec![pcb_zen_core::config::KicadLibraryConfig {
                version: Version::new(9, 0, 0),
                symbols: "gitlab.com/kicad/libraries/kicad-symbols".to_string(),
                footprints: "gitlab.com/kicad/libraries/kicad-footprints".to_string(),
                models: BTreeMap::new(),
                parts: None,
                http_mirror: None,
            }],
            ..Default::default()
        });

        let mut workspace = workspace_with_root_config(config);
        workspace.root = temp.path().to_path_buf();
        let version = Version::new(9, 0, 0);
        let selected_kicad_assets = [("gitlab.com/kicad/libraries/kicad-symbols", &version)];
        let err = materialize_asset_deps(&workspace, selected_kicad_assets, true)
            .expect_err("expected offline mode to require cached asset deps");

        assert!(err.to_string().contains("not cached"));
    }

    #[test]
    fn test_kicad_repos_offline_keeps_same_family_versions() {
        let temp = TempDir::new().unwrap();
        let mut workspace = workspace_with_root_config(PcbToml::default());
        workspace.root = temp.path().to_path_buf();

        let repo = "gitlab.com/kicad/libraries/kicad-symbols";
        let cached = Version::new(9, 0, 0);
        let missing = Version::new(9, 0, 1);
        let cached_dir = workspace
            .root
            .join(".pcb/cache")
            .join(repo)
            .join(cached.to_string());
        std::fs::create_dir_all(&cached_dir).unwrap();
        std::fs::write(cached_dir.join(".pcb-cached"), "").unwrap();

        let err = materialize_asset_deps(&workspace, [(repo, &cached), (repo, &missing)], true)
            .expect_err("expected uncached same-family asset version to be required");

        assert!(err.to_string().contains("@9.0.1"));
    }

    #[test]
    fn test_build_frozen_symbol_parts_reads_kicad_parts_index() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().to_path_buf();
        let dep_root = root.join(".pcb/cache/gitlab.com/kicad/libraries/kicad-symbols/9.0.3");
        std::fs::create_dir_all(&dep_root).unwrap();
        std::fs::write(
            dep_root.join(KICAD_PARTS_INDEX_FILE),
            serde_json::to_vec(&HashMap::from([(
                "package://gitlab.com/kicad/libraries/kicad-symbols@9.0.3/Diode.kicad_sym"
                    .to_string(),
                vec![ManifestPart {
                    mpn: "1N4004-E3/54".to_string(),
                    symbol: "Diode.kicad_sym".to_string(),
                    symbol_name: Some("1N4004".to_string()),
                    manufacturer: "Vishay".to_string(),
                    qualifications: vec![],
                    datasheet: None,
                }],
            )]))
            .unwrap(),
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
                    deps: BTreeMap::from([(
                        "gitlab.com/kicad/libraries/kicad-symbols".to_string(),
                        dep_root.clone(),
                    )]),
                    parts: Vec::new(),
                },
            )]),
        };

        let symbol_parts = build_frozen_symbol_parts(&workspace, &resolution).unwrap();
        let key = "package://gitlab.com/kicad/libraries/kicad-symbols@9.0.3/Diode.kicad_sym";
        let parts = symbol_parts.get(key).expect("expected symbol parts entry");
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].mpn, "1N4004-E3/54");
        assert_eq!(parts[0].symbol_name.as_deref(), Some("1N4004"));
    }

    #[test]
    fn test_normalize_kicad_parts_index_requires_symbol_name_for_multi_symbol_library() {
        let temp = TempDir::new().unwrap();
        let dep_root = temp
            .path()
            .join(".pcb/cache/gitlab.com/kicad/libraries/kicad-symbols/9.0.3");
        std::fs::create_dir_all(&dep_root).unwrap();
        std::fs::write(
            dep_root.join("Device.kicad_sym"),
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

        let err = normalize_kicad_parts_index(
            "gitlab.com/kicad/libraries/kicad-symbols",
            &Version::new(9, 0, 3),
            &dep_root,
            &[ManifestPart {
                mpn: "GENERIC".to_string(),
                symbol: "Device.kicad_sym".to_string(),
                symbol_name: None,
                manufacturer: "Acme".to_string(),
                qualifications: vec![],
                datasheet: None,
            }],
        )
        .expect_err("expected ambiguous multi-symbol manifest part to fail");
        assert!(format!("{err:#}").contains("must set `symbol_name`"));
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
