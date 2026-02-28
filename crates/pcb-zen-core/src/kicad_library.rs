use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use semver::Version;

use crate::config::KicadLibraryConfig;

fn parse_relaxed_semver(raw: &str) -> Option<Version> {
    Version::parse(raw).ok().or_else(|| {
        raw.strip_prefix('v')
            .and_then(|trimmed| Version::parse(trimmed).ok())
    })
}

fn kicad_entry_version(entry: &KicadLibraryConfig) -> Result<Version> {
    parse_relaxed_semver(&entry.version).ok_or_else(|| {
        anyhow::anyhow!(
            "Invalid [[workspace.kicad_library]].version '{}': expected semver like \"9.0.3\"",
            entry.version
        )
    })
}

fn kicad_entry_major(entry: &KicadLibraryConfig) -> Result<u64> {
    Ok(kicad_entry_version(entry)?.major)
}

fn matches_entry_major(entry: &KicadLibraryConfig, version: &Version) -> Result<bool> {
    Ok(kicad_entry_major(entry)? == version.major)
}

/// Validate `[[workspace.kicad_library]].version`.
pub fn validate_kicad_library_version(version: &str) -> Result<()> {
    parse_relaxed_semver(version).ok_or_else(|| {
        anyhow::anyhow!(
            "Invalid [[workspace.kicad_library]].version '{}': expected semver like \"9.0.3\"",
            version
        )
    })?;
    Ok(())
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
        if matches_entry_major(entry, version)? {
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

fn is_any_kicad_repo(entry: &KicadLibraryConfig, repo: &str) -> bool {
    entry.symbols == repo
        || entry.footprints == repo
        || entry.models.values().any(|model_repo| model_repo == repo)
}

/// Resolve any configured kicad-style repo against workspace kicad_library entries.
pub fn match_kicad_managed_repo(
    entries: &[KicadLibraryConfig],
    module_path: &str,
    version: &Version,
) -> Result<KicadRepoMatch> {
    let (saw_repo, matched) = match_kicad_entry(entries, module_path, version, is_any_kicad_repo)?;
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
    let (saw_repo, matched) = match_kicad_entry(entries, module_path, version, is_any_kicad_repo)?;
    if let Some(entry) = matched {
        Ok(entry.http_mirror.as_deref())
    } else if saw_repo {
        anyhow::bail!(
            "Dependency {}@{} does not match any [[workspace.kicad_library]] major version",
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

/// Build deterministic concrete versions for all configured kicad-style repos.
///
/// When a repo is referenced by multiple entries, we keep the highest configured version.
pub fn configured_kicad_repo_versions(
    entries: &[KicadLibraryConfig],
) -> Result<BTreeMap<String, Version>> {
    let mut selected = BTreeMap::<String, Version>::new();

    for entry in entries {
        let version = kicad_entry_version(entry)?;
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

    Ok(selected)
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

/// Compute KiCad model variable directories from configured kicad_library entries.
pub fn kicad_model_dirs(
    cache_dir: &Path,
    entries: &[KicadLibraryConfig],
) -> BTreeMap<String, PathBuf> {
    let mut model_dirs = BTreeMap::new();
    for entry in entries {
        let Ok(version) = kicad_entry_version(entry).map(|v| v.to_string()) else {
            continue;
        };
        for (var, repo) in &entry.models {
            model_dirs.insert(var.clone(), cache_dir.join(repo).join(&version));
        }
    }
    model_dirs
}

/// Validate required fields for a `[[workspace.kicad_library]]` entry.
pub fn validate_kicad_library_config(entry: &KicadLibraryConfig) -> Result<()> {
    validate_kicad_library_version(&entry.version)?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WorkspaceConfig;

    fn default_entry() -> crate::config::KicadLibraryConfig {
        WorkspaceConfig::default()
            .kicad_library
            .into_iter()
            .next()
            .expect("default kicad library")
    }

    #[test]
    fn test_configured_kicad_repo_versions_default_entry() {
        let selected = configured_kicad_repo_versions(&[default_entry()]).expect("selection");
        assert_eq!(
            selected.get("gitlab.com/kicad/libraries/kicad-symbols"),
            Some(&Version::parse("9.0.3").unwrap())
        );
        assert_eq!(
            selected.get("gitlab.com/kicad/libraries/kicad-footprints"),
            Some(&Version::parse("9.0.3").unwrap())
        );
        assert_eq!(
            selected.get("gitlab.com/kicad/libraries/kicad-packages3D"),
            Some(&Version::parse("9.0.3").unwrap())
        );
    }

    #[test]
    fn test_kicad_dependency_aliases_includes_symbols_and_footprints() {
        let aliases = kicad_dependency_aliases(&[default_entry()]);
        assert_eq!(
            aliases.get("kicad-symbols"),
            Some(&"gitlab.com/kicad/libraries/kicad-symbols".to_string())
        );
        assert_eq!(
            aliases.get("kicad-footprints"),
            Some(&"gitlab.com/kicad/libraries/kicad-footprints".to_string())
        );
    }
}
