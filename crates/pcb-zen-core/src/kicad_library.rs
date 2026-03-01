use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use semver::Version;

use crate::config::KicadLibraryConfig;

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
) -> (bool, Option<&'a KicadLibraryConfig>) {
    let mut saw_repo = false;
    for entry in entries {
        if !includes_repo(entry, module_path) {
            continue;
        }
        saw_repo = true;
        if entry.version.major == version.major {
            return (true, Some(entry));
        }
    }
    (saw_repo, None)
}

/// Resolve a symbol repo/version against workspace kicad_library entries.
pub fn match_kicad_library_for_symbol_repo<'a>(
    entries: &'a [KicadLibraryConfig],
    symbol_repo: &str,
    symbol_version: &Version,
) -> KicadSymbolLibraryMatch<'a> {
    let (has_symbol_repo, matched) =
        match_kicad_entry(entries, symbol_repo, symbol_version, |entry, repo| {
            entry.symbols == repo
        });
    if let Some(entry) = matched {
        KicadSymbolLibraryMatch::Matched(entry)
    } else if has_symbol_repo {
        KicadSymbolLibraryMatch::SelectorMismatch
    } else {
        KicadSymbolLibraryMatch::NotSymbolRepo
    }
}

fn is_any_kicad_repo(entry: &KicadLibraryConfig, repo: &str) -> bool {
    entry.symbols == repo
        || entry.footprints == repo
        || entry.models.values().any(|model_repo| model_repo == repo)
}

/// Resolve any configured asset dependency repo against workspace kicad_library entries.
pub fn match_kicad_managed_repo(
    entries: &[KicadLibraryConfig],
    module_path: &str,
    version: &Version,
) -> KicadRepoMatch {
    let (saw_repo, matched) = match_kicad_entry(entries, module_path, version, is_any_kicad_repo);
    if matched.is_some() {
        KicadRepoMatch::SelectorMatched
    } else if saw_repo {
        KicadRepoMatch::SelectorMismatch
    } else {
        KicadRepoMatch::NotManaged
    }
}

/// Get the configured HTTP mirror template for a managed repo/version, if any.
pub fn kicad_http_mirror_template_for_repo<'a>(
    entries: &'a [KicadLibraryConfig],
    module_path: &str,
    version: &Version,
) -> Result<Option<&'a str>> {
    let (saw_repo, matched) = match_kicad_entry(entries, module_path, version, is_any_kicad_repo);
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

/// Find `(repo, version, root_dir)` for a path by longest package root prefix.
pub fn package_coord_for_path(
    path: &Path,
    package_roots: &BTreeMap<String, PathBuf>,
) -> Option<(String, String, PathBuf)> {
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
                root.clone(),
            ))
        })
        .max_by_key(|(depth, _, _, _)| *depth)
        .map(|(_, repo, version, root)| (repo, version, root))
}

/// Validate required fields for a `[[workspace.kicad_library]]` entry.
pub fn validate_kicad_library_config(entry: &KicadLibraryConfig) -> Result<()> {
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
