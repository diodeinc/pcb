use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use semver::Version;

use crate::config::{AssetDependencySpec, DependencySpec, KicadLibraryConfig, PcbToml};

/// Validate major-version selector for `[[workspace.kicad_library]]`.
pub fn validate_kicad_library_version_selector(version: &str) -> Result<()> {
    if version.is_empty() || !version.chars().all(|c| c.is_ascii_digit()) {
        anyhow::bail!(
            "Invalid [[workspace.kicad_library]].version '{}': expected major selector like \"9\"",
            version
        );
    }
    Ok(())
}

/// Parse `[[workspace.kicad_library]].version` major selector.
pub fn parse_kicad_library_selector_major(selector: &str) -> Result<u64> {
    validate_kicad_library_version_selector(selector)?;
    selector.parse::<u64>().map_err(|_| {
        anyhow::anyhow!(
            "Invalid [[workspace.kicad_library]].version '{}': expected major selector like \"9\"",
            selector
        )
    })
}

/// Check whether a semver version matches a kicad_library major selector.
pub fn selector_matches_version(selector: &str, version: &Version) -> Result<bool> {
    Ok(parse_kicad_library_selector_major(selector)? == version.major)
}

/// Select the highest semver version string that matches a kicad_library major selector.
pub fn select_highest_matching_kicad_version(
    selector: &str,
    versions: impl IntoIterator<Item = String>,
) -> Option<String> {
    let selector_major = parse_kicad_library_selector_major(selector).ok()?;
    versions
        .into_iter()
        .filter_map(|s| Version::parse(&s).ok())
        .filter(|v| v.major == selector_major)
        .max()
        .map(|v| v.to_string())
}

/// Match result for resolving a symbol repo/version to a kicad_library entry.
pub enum KicadSymbolLibraryMatch<'a> {
    Matched(&'a KicadLibraryConfig),
    SelectorMismatch,
    NotSymbolRepo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KicadRepoMatch {
    NotManaged,
    SelectorMatched,
    SelectorMismatch,
}

fn parse_relaxed_semver(raw: &str) -> Option<Version> {
    Version::parse(raw).ok().or_else(|| {
        raw.strip_prefix('v')
            .and_then(|trimmed| Version::parse(trimmed).ok())
    })
}

fn match_kicad_entry<'a>(
    entries: &'a [KicadLibraryConfig],
    module_path: &str,
    version: &Version,
    includes_repo: impl Fn(&KicadLibraryConfig, &str) -> bool,
) -> Result<(bool, Option<&'a KicadLibraryConfig>)> {
    let mut saw_repo = false;
    for entry in entries {
        if !includes_repo(entry, module_path) {
            continue;
        }
        saw_repo = true;
        if selector_matches_version(&entry.version, version)? {
            return Ok((true, Some(entry)));
        }
    }
    Ok((saw_repo, None))
}

/// Resolve a symbol repo/version against workspace kicad_library entries.
pub fn match_kicad_library_for_symbol_repo<'a>(
    entries: &'a [KicadLibraryConfig],
    symbol_repo: &str,
    symbol_version: &Version,
) -> Result<KicadSymbolLibraryMatch<'a>> {
    let (has_symbol_repo, matched) =
        match_kicad_entry(entries, symbol_repo, symbol_version, |entry, repo| {
            entry.symbols == repo
        })?;
    if let Some(entry) = matched {
        Ok(KicadSymbolLibraryMatch::Matched(entry))
    } else if has_symbol_repo {
        Ok(KicadSymbolLibraryMatch::SelectorMismatch)
    } else {
        Ok(KicadSymbolLibraryMatch::NotSymbolRepo)
    }
}

/// Resolve any configured kicad-style repo against workspace kicad_library entries.
pub fn match_kicad_managed_repo(
    entries: &[KicadLibraryConfig],
    module_path: &str,
    version: &Version,
) -> Result<KicadRepoMatch> {
    let (saw_repo, matched) = match_kicad_entry(entries, module_path, version, |entry, repo| {
        entry.symbols == repo
            || entry.footprints == repo
            || entry.models.values().any(|model_repo| model_repo == repo)
    })?;
    if matched.is_some() {
        Ok(KicadRepoMatch::SelectorMatched)
    } else if saw_repo {
        Ok(KicadRepoMatch::SelectorMismatch)
    } else {
        Ok(KicadRepoMatch::NotManaged)
    }
}

/// Get the configured HTTP mirror template for a managed repo/version, if any.
pub fn kicad_http_mirror_template_for_repo<'a>(
    entries: &'a [KicadLibraryConfig],
    module_path: &str,
    version: &Version,
) -> Result<Option<&'a str>> {
    let (saw_repo, matched) = match_kicad_entry(entries, module_path, version, |entry, repo| {
        entry.symbols == repo
            || entry.footprints == repo
            || entry.models.values().any(|model_repo| model_repo == repo)
    })?;
    if let Some(entry) = matched {
        Ok(entry.http_mirror.as_deref())
    } else if saw_repo {
        anyhow::bail!(
            "Dependency {}@{} does not match any [[workspace.kicad_library]] version selector",
            module_path,
            version
        );
    } else {
        Ok(None)
    }
}

/// Build unique dependency aliases for kicad symbol/footprint repos by last path segment.
pub fn kicad_dependency_aliases(entries: &[KicadLibraryConfig]) -> HashMap<String, String> {
    let mut aliases = HashMap::<String, String>::new();
    let mut conflicts = HashSet::<String>::new();

    let mut add = |repo: &str| {
        let Some(alias) = repo.rsplit('/').next() else {
            return;
        };
        if alias.is_empty() {
            return;
        }
        match aliases.get(alias) {
            Some(existing) if existing != repo => {
                conflicts.insert(alias.to_string());
            }
            Some(_) => {}
            None => {
                aliases.insert(alias.to_string(), repo.to_string());
            }
        }
    };

    for entry in entries {
        add(&entry.symbols);
        add(&entry.footprints);
    }

    for alias in conflicts {
        aliases.remove(&alias);
    }

    aliases
}

/// Collect all configured kicad repository roots.
pub fn kicad_repo_roots(entries: &[KicadLibraryConfig]) -> HashSet<String> {
    let mut repos = HashSet::new();
    for entry in entries {
        repos.insert(entry.symbols.clone());
        repos.insert(entry.footprints.clone());
        for repo in entry.models.values() {
            repos.insert(repo.clone());
        }
    }
    repos
}

/// Find `<repo>@<version>` coordinate for a path by longest package root prefix.
pub fn package_coord_for_path(
    path: &Path,
    package_roots: &BTreeMap<String, PathBuf>,
) -> Option<(String, String)> {
    package_roots
        .iter()
        .filter_map(|(coord, root)| {
            if !path.starts_with(root) {
                return None;
            }
            let (repo, version) = coord.rsplit_once('@')?;
            Some((
                root.components().count(),
                repo.to_string(),
                version.to_string(),
            ))
        })
        .max_by_key(|(depth, _, _)| *depth)
        .map(|(_, repo, version)| (repo, version))
}

/// Compute KiCad model variable directories from selected package roots.
pub fn kicad_model_dirs_from_package_roots(
    workspace_root: &Path,
    package_roots: &BTreeMap<String, PathBuf>,
    entries: &[KicadLibraryConfig],
) -> BTreeMap<String, PathBuf> {
    fn versions_for_repo(package_roots: &BTreeMap<String, PathBuf>, repo: &str) -> Vec<String> {
        package_roots
            .keys()
            .filter_map(|coord| {
                let (path, version) = coord.rsplit_once('@')?;
                (path == repo).then_some(version.to_string())
            })
            .collect()
    }

    let mut model_dirs = BTreeMap::new();
    for entry in entries {
        let mut candidates = versions_for_repo(package_roots, &entry.footprints);
        candidates.extend(versions_for_repo(package_roots, &entry.symbols));
        let Some(version) = select_highest_matching_kicad_version(&entry.version, candidates)
        else {
            continue;
        };
        for (var, repo) in &entry.models {
            model_dirs.insert(
                var.clone(),
                workspace_root.join(".pcb/cache").join(repo).join(&version),
            );
        }
    }
    model_dirs
}

/// Validate required fields for a `[[workspace.kicad_library]]` entry.
pub fn validate_kicad_library_config(entry: &KicadLibraryConfig) -> Result<()> {
    validate_kicad_library_version_selector(&entry.version)?;

    if entry.symbols.trim().is_empty() {
        anyhow::bail!("Invalid [[workspace.kicad_library]]: `symbols` must not be empty");
    }
    if entry.footprints.trim().is_empty() {
        anyhow::bail!("Invalid [[workspace.kicad_library]]: `footprints` must not be empty");
    }
    for (var, repo) in &entry.models {
        if var.trim().is_empty() {
            anyhow::bail!(
                "Invalid [[workspace.kicad_library]]: model variable names must not be empty"
            );
        }
        if repo.trim().is_empty() {
            anyhow::bail!(
                "Invalid [[workspace.kicad_library]]: model repo for `{}` must not be empty",
                var
            );
        }
    }
    if let Some(mirror) = &entry.http_mirror
        && mirror.trim().is_empty()
    {
        anyhow::bail!("Invalid [[workspace.kicad_library]]: `http_mirror` must not be empty");
    }

    Ok(())
}

fn dependency_version(spec: &DependencySpec) -> Option<Version> {
    let raw = match spec {
        DependencySpec::Version(v) => v.as_str(),
        DependencySpec::Detailed(d) => d.version.as_deref()?,
    };
    parse_relaxed_semver(raw)
}

fn asset_version(spec: &AssetDependencySpec) -> Option<Version> {
    let raw = match spec {
        AssetDependencySpec::Ref(v) => v.as_str(),
        AssetDependencySpec::Detailed(d) => d.version.as_deref()?,
    };
    parse_relaxed_semver(raw)
}

fn infer_repo_root_from_asset_url<'a>(
    asset_url: &str,
    repo_roots: &'a HashSet<String>,
) -> Option<&'a str> {
    repo_roots
        .iter()
        .map(String::as_str)
        .filter(|repo| {
            asset_url == *repo
                || asset_url
                    .strip_prefix(repo)
                    .is_some_and(|rest| rest.starts_with('/'))
        })
        .max_by_key(|repo| repo.len())
}

/// Deterministically select concrete versions for configured kicad repos from manifests.
pub fn selected_kicad_repo_versions<'a>(
    entries: &[KicadLibraryConfig],
    manifests: impl IntoIterator<Item = &'a PcbToml>,
) -> Result<BTreeMap<String, String>> {
    let repo_roots = kicad_repo_roots(entries);
    let mut candidates: BTreeMap<String, BTreeSet<Version>> = BTreeMap::new();
    for config in manifests {
        for (url, spec) in &config.dependencies {
            if !repo_roots.contains(url) {
                continue;
            }
            let Some(version) = dependency_version(spec) else {
                continue;
            };
            candidates.entry(url.clone()).or_default().insert(version);
        }
        for (asset_url, spec) in &config.assets {
            let Some(version) = asset_version(spec) else {
                continue;
            };
            let Some(repo) = infer_repo_root_from_asset_url(asset_url, &repo_roots) else {
                continue;
            };
            candidates
                .entry(repo.to_string())
                .or_default()
                .insert(version);
        }
    }

    let mut selected: BTreeMap<String, Version> = BTreeMap::new();
    for entry in entries {
        let selector_major = parse_kicad_library_selector_major(&entry.version)?;
        let chosen = [&entry.symbols, &entry.footprints]
            .into_iter()
            .flat_map(|repo| candidates.get(repo).into_iter().flat_map(|s| s.iter()))
            .filter(|v| v.major == selector_major)
            .max()
            .cloned();
        let Some(version) = chosen else {
            continue;
        };

        for repo in std::iter::once(&entry.symbols)
            .chain(std::iter::once(&entry.footprints))
            .chain(entry.models.values())
        {
            selected
                .entry(repo.clone())
                .and_modify(|cur| {
                    if version > *cur {
                        *cur = version.clone();
                    }
                })
                .or_insert_with(|| version.clone());
        }
    }

    Ok(selected
        .into_iter()
        .map(|(repo, version)| (repo, version.to_string()))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AssetDependencySpec, PcbToml, WorkspaceConfig};

    fn default_entry() -> crate::config::KicadLibraryConfig {
        WorkspaceConfig::default()
            .kicad_library
            .into_iter()
            .next()
            .expect("default kicad library")
    }

    #[test]
    fn test_selected_kicad_versions_from_assets() {
        let mut manifest = PcbToml::default();
        manifest.assets.insert(
            "gitlab.com/kicad/libraries/kicad-footprints/Resistor_SMD.pretty/R_0603_1608Metric.kicad_mod"
                .to_string(),
            AssetDependencySpec::Ref("9.0.3".to_string()),
        );
        manifest.assets.insert(
            "gitlab.com/kicad/libraries/kicad-symbols/Device.kicad_sym".to_string(),
            AssetDependencySpec::Ref("v9.0.3".to_string()),
        );

        let selected =
            selected_kicad_repo_versions(&[default_entry()], [&manifest]).expect("selection");
        assert_eq!(
            selected.get("gitlab.com/kicad/libraries/kicad-symbols"),
            Some(&"9.0.3".to_string())
        );
        assert_eq!(
            selected.get("gitlab.com/kicad/libraries/kicad-footprints"),
            Some(&"9.0.3".to_string())
        );
        assert_eq!(
            selected.get("gitlab.com/kicad/libraries/kicad-packages3D"),
            Some(&"9.0.3".to_string())
        );
    }

    #[test]
    fn test_selected_kicad_versions_dependencies_override_assets() {
        let mut manifest = PcbToml::default();
        manifest.assets.insert(
            "gitlab.com/kicad/libraries/kicad-symbols/Device.kicad_sym".to_string(),
            AssetDependencySpec::Ref("9.0.3".to_string()),
        );
        manifest.dependencies.insert(
            "gitlab.com/kicad/libraries/kicad-symbols".to_string(),
            DependencySpec::Version("9.0.4".to_string()),
        );
        manifest.dependencies.insert(
            "gitlab.com/kicad/libraries/kicad-footprints".to_string(),
            DependencySpec::Version("9.0.4".to_string()),
        );

        let selected =
            selected_kicad_repo_versions(&[default_entry()], [&manifest]).expect("selection");
        assert_eq!(
            selected.get("gitlab.com/kicad/libraries/kicad-symbols"),
            Some(&"9.0.4".to_string())
        );
        assert_eq!(
            selected.get("gitlab.com/kicad/libraries/kicad-footprints"),
            Some(&"9.0.4".to_string())
        );
        assert_eq!(
            selected.get("gitlab.com/kicad/libraries/kicad-packages3D"),
            Some(&"9.0.4".to_string())
        );
    }
}

/// Build concrete `<repo, version>` targets to materialize for configured kicad-style repos.
pub fn collect_kicad_materialization_targets<'a>(
    entries: &[KicadLibraryConfig],
    selected: impl IntoIterator<Item = (&'a str, &'a Version)>,
) -> Result<Vec<(String, String)>> {
    let selected: Vec<(String, Version)> = selected
        .into_iter()
        .map(|(path, version)| (path.to_string(), version.clone()))
        .collect();
    let mut required: BTreeSet<(String, String)> = BTreeSet::new();

    for (path, version) in &selected {
        match match_kicad_managed_repo(entries, path, version)? {
            KicadRepoMatch::NotManaged => {}
            KicadRepoMatch::SelectorMatched => {
                required.insert((path.clone(), version.to_string()));
            }
            KicadRepoMatch::SelectorMismatch => {
                anyhow::bail!(
                    "Dependency {}@{} does not match any [[workspace.kicad_library]] version selector",
                    path,
                    version
                );
            }
        }
    }

    for entry in entries {
        let selector_major = parse_kicad_library_selector_major(&entry.version)?;
        let chosen = selected
            .iter()
            .filter_map(|(path, version)| {
                ((path == &entry.symbols || path == &entry.footprints)
                    && version.major == selector_major)
                    .then_some(version)
            })
            .max()
            .cloned();
        let Some(version) = chosen else {
            continue;
        };
        let version = version.to_string();
        required.insert((entry.symbols.clone(), version.clone()));
        required.insert((entry.footprints.clone(), version.clone()));
        for repo in entry.models.values() {
            required.insert((repo.clone(), version.clone()));
        }
    }

    Ok(required.into_iter().collect())
}
