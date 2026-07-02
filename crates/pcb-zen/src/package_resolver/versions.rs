use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::git;
use crate::tags;
use anyhow::{Result, bail};
use pcb_zen_core::config::{DependencyDetail, DependencySpec, split_repo_and_subpath};
use pcb_zen_core::initial_package_version;
use semver::Version;

#[derive(Default)]
pub struct SpecVersionResolver {
    source_repos: BTreeMap<String, PathBuf>,
    base_versions: BTreeMap<String, BTreeMap<String, Version>>,
}

impl SpecVersionResolver {
    pub(crate) fn resolve_spec(
        &mut self,
        module_path: &str,
        spec: &DependencySpec,
    ) -> Result<Version> {
        match spec {
            DependencySpec::Version(version) => parse_version_string(version),
            DependencySpec::Detailed(detail) => self.resolve_detail(module_path, detail),
        }
    }

    pub fn resolve_ref_or_branch(&mut self, module_path: &str, selector: &str) -> Result<Version> {
        match self.generate_pseudo_version(module_path, selector) {
            Ok(version) => Ok(version),
            Err(rev_err) => {
                let commit =
                    git::resolve_branch_head(module_path, selector).map_err(|branch_err| {
                        anyhow::anyhow!(
                            "Failed to resolve '{}' as a rev or branch for {}: {}; {}",
                            selector,
                            module_path,
                            rev_err,
                            branch_err
                        )
                    })?;
                self.generate_pseudo_version(module_path, &commit)
            }
        }
    }

    fn resolve_detail(&mut self, module_path: &str, detail: &DependencyDetail) -> Result<Version> {
        if let Some(version) = &detail.version {
            return parse_version_string(version);
        }
        if let Some(rev) = &detail.rev {
            return self.resolve_ref_or_branch(module_path, rev);
        }
        if let Some(branch) = &detail.branch {
            return self.resolve_ref_or_branch(module_path, branch);
        }
        if detail.path.is_some() {
            bail!(
                "Path dependency in remote MVS state is not supported yet for {}",
                module_path
            );
        }
        bail!(
            "Dependency has no version, rev, or branch for {}",
            module_path
        )
    }

    fn generate_pseudo_version(&mut self, module_path: &str, commit: &str) -> Result<Version> {
        let (repo_url, subpath) = split_repo_and_subpath(module_path);
        let source_dir = self.ensure_source_repo(repo_url)?;
        let commit_full = git::rev_parse(&source_dir, commit).ok_or_else(|| {
            anyhow::anyhow!(
                "Failed to resolve rev '{}' in {}",
                &commit[..commit.len().min(12)],
                repo_url
            )
        })?;
        let timestamp = git::show_commit_timestamp(&source_dir, &commit_full)
            .ok_or_else(|| anyhow::anyhow!("Failed to read timestamp for {}", commit_full))?;
        let base_version = self
            .latest_tagged_version(repo_url, subpath, &source_dir)
            .unwrap_or_else(initial_package_version);
        let dt = jiff::Timestamp::from_second(timestamp)?;
        let pseudo = format!(
            "{}.{}.{}-0.{}-{}",
            base_version.major,
            base_version.minor,
            base_version.patch + 1,
            dt.strftime("%Y%m%d%H%M%S"),
            commit_full
        );
        Version::parse(&pseudo)
            .map_err(|e| anyhow::anyhow!("Failed to parse pseudo-version {}: {}", pseudo, e))
    }

    fn ensure_source_repo(&mut self, repo_url: &str) -> Result<PathBuf> {
        if let Some(path) = self.source_repos.get(repo_url) {
            return Ok(path.clone());
        }
        let path = crate::cache_index::ensure_source_repo(repo_url)?;
        self.source_repos.insert(repo_url.to_string(), path.clone());
        Ok(path)
    }

    fn latest_tagged_version(
        &mut self,
        repo_url: &str,
        subpath: &str,
        source_dir: &std::path::Path,
    ) -> Option<Version> {
        if !self.base_versions.contains_key(repo_url) {
            let mut versions = BTreeMap::new();
            if let Ok(tags) = git::list_all_tags(source_dir) {
                for tag in tags {
                    if let Some((pkg_path, version)) = tags::parse_tag(&tag) {
                        versions
                            .entry(pkg_path)
                            .and_modify(|current| {
                                if version > *current {
                                    *current = version.clone();
                                }
                            })
                            .or_insert(version);
                    }
                }
            }
            self.base_versions.insert(repo_url.to_string(), versions);
        }

        self.base_versions
            .get(repo_url)
            .and_then(|versions| versions.get(subpath))
            .cloned()
    }
}

pub(crate) fn parse_version_string(raw: &str) -> Result<Version> {
    pcb_zen_core::parse_relaxed_version(raw)
        .ok_or_else(|| anyhow::anyhow!("Invalid version string '{}'", raw))
}
