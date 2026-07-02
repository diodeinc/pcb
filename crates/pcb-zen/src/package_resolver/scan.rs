use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::ast_utils::skip_vendor;
use crate::cache_index::CacheIndex;
use crate::import_scanner::extract_imports;
use anyhow::{Context, Result};
use ignore::WalkBuilder;
use pcb_zen_core::DefaultFileProvider;
use pcb_zen_core::FileProvider;
use pcb_zen_core::config::{DependencySpec, PcbToml};
use pcb_zen_core::workspace::package_url_covers;

#[derive(Debug, Default, Clone)]
pub(crate) struct ScannedDirectDeps {
    pub(crate) remote: BTreeMap<String, DependencySpec>,
    pub(crate) workspace: BTreeSet<String>,
}

pub(crate) fn scan_package_direct_deps(
    workspace_info: &crate::WorkspaceInfo,
    package_index: &WorkspacePackageIndex,
    package_url: &str,
    package_dir: &Path,
    current_config: &PcbToml,
    index: &CacheIndex,
) -> Result<ScannedDirectDeps> {
    let file_provider = DefaultFileProvider::new();
    let mut scanned = ScannedDirectDeps::default();

    for zen_path in package_index.zen_files(package_dir, package_url) {
        let content = file_provider
            .read_file(&zen_path)
            .with_context(|| format!("Failed to read {}", zen_path.display()))?;
        let extracted = extract_imports(&content)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse {}", zen_path.display()))?;

        for url in extracted.urls {
            if let Some(workspace_package_url) = workspace_info.package_url_for_url(&url) {
                if workspace_package_url == package_url {
                    anyhow::bail!(
                        "{} uses package URL '{}' that points into its own package '{}'; use a relative path instead",
                        zen_path.display(),
                        url,
                        package_url
                    );
                }
                scanned.workspace.insert(workspace_package_url.to_string());
                continue;
            }

            add_remote_dep(&mut scanned.remote, &url, current_config, index)?;
        }

        for rel_path in extracted.relative_paths {
            let file_dir = zen_path.parent().unwrap_or(package_dir);
            let Ok(resolved) = file_dir.join(&rel_path).canonicalize() else {
                continue;
            };
            if let Some(workspace_package_url) = package_index.owner_for_path(&resolved)
                && workspace_package_url != package_url
            {
                scanned.workspace.insert(workspace_package_url.to_string());
            }
        }
    }

    Ok(scanned)
}

pub(crate) struct WorkspacePackageIndex {
    package_dirs: BTreeMap<PathBuf, String>,
}

impl WorkspacePackageIndex {
    pub(crate) fn new(workspace_info: &crate::WorkspaceInfo) -> Self {
        let package_dirs = workspace_info
            .packages
            .iter()
            .map(|(url, pkg)| {
                let dir = pkg.dir(&workspace_info.root);
                let dir = dir.canonicalize().unwrap_or(dir);
                (dir, url.clone())
            })
            .collect();
        Self { package_dirs }
    }

    fn owner_for_path(&self, path: &Path) -> Option<&str> {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.package_dirs
            .range(..=path.clone())
            .rev()
            .find(|(package_dir, _)| path.starts_with(package_dir))
            .map(|(_, url)| url.as_str())
    }

    fn zen_files(&self, package_dir: &Path, package_url: &str) -> Vec<PathBuf> {
        let mut builder = WalkBuilder::new(package_dir);
        builder
            .hidden(true)
            .git_ignore(true)
            .git_exclude(true)
            .filter_entry(skip_vendor);

        let mut files: Vec<_> = builder
            .build()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.into_path())
            .filter(|path| {
                path.is_file()
                    && path.extension() == Some(std::ffi::OsStr::new("zen"))
                    && self.owner_for_path(path) == Some(package_url)
            })
            .collect();

        files.sort();
        files
    }
}

fn add_remote_dep(
    remote: &mut BTreeMap<String, DependencySpec>,
    url: &str,
    current_config: &PcbToml,
    index: &CacheIndex,
) -> Result<()> {
    if let Some((module_path, spec)) = existing_manifest_dep(url, current_config) {
        remote.entry(module_path).or_insert(spec);
        return Ok(());
    }

    let Some(candidate) = index.find_remote_package(url)? else {
        anyhow::bail!("No remote package found covering '{}'", url);
    };

    remote
        .entry(candidate.module_path)
        .or_insert(DependencySpec::Version(candidate.version));
    Ok(())
}

fn existing_manifest_dep(url: &str, config: &PcbToml) -> Option<(String, DependencySpec)> {
    config
        .dependencies
        .direct
        .iter()
        .filter(|(module_path, _)| package_url_covers(module_path, url))
        .max_by_key(|(module_path, _)| module_path.len())
        .map(|(module_path, spec)| (module_path.clone(), spec.clone()))
}
