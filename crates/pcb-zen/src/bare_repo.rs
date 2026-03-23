use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use semver::Version;

use crate::git;

pub(crate) fn bare_repo_dir(repo_url: &str) -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("Cannot determine home directory"))?;
    Ok(home.join(".pcb/bare").join(repo_url))
}

#[derive(Clone, Debug)]
pub(crate) struct BareRepo {
    path: PathBuf,
}

#[derive(Default)]
pub(crate) struct BareRepoCache {
    repos: HashMap<String, BareRepo>,
}

impl BareRepo {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub(crate) fn sync(repo_url: &str) -> Result<Self> {
        let bare_dir = bare_repo_dir(repo_url)?;
        let _lock = git::lock_dir(&bare_dir)?;

        if repo_exists(&bare_dir) {
            git::fetch_in_bare_repo(&bare_dir)?;
        } else {
            git::clone_bare_with_fallback(repo_url, &bare_dir)?;
        }

        Ok(Self::new(bare_dir))
    }

    pub(crate) fn list_tags(&self) -> Result<Vec<String>> {
        git::list_all_tags(&self.path)
    }

    pub(crate) fn rev_parse(&self, rev: &str) -> Option<String> {
        git::rev_parse(&self.path, rev)
    }

    pub(crate) fn show_commit_timestamp(&self, commit: &str) -> Option<i64> {
        git::show_commit_timestamp(&self.path, commit)
    }

    pub(crate) fn read_package_tag(&self, subpath: &str, version: &Version) -> Option<String> {
        git::cat_file(&self.path, &Self::package_tag_name(subpath, version))
    }

    pub(crate) fn archive_package(
        &self,
        dest: &Path,
        ref_spec: &str,
        subpath: &str,
        is_pseudo: bool,
    ) -> Result<()> {
        std::fs::create_dir_all(dest)?;

        let ref_name = if is_pseudo {
            git::ensure_rev_in_bare_repo(&self.path, ref_spec)?;
            ref_spec.to_string()
        } else {
            ref_spec.to_string()
        };
        let treeish = if subpath.is_empty() {
            ref_name
        } else {
            format!("{ref_name}:{subpath}")
        };
        git::archive_to_dir(&self.path, &treeish, dest)
    }

    pub(crate) fn version_ref(subpath: &str, version: &str, add_v_prefix: bool) -> String {
        let version_part = if add_v_prefix {
            format!("v{version}")
        } else {
            version.to_string()
        };
        if subpath.is_empty() {
            version_part
        } else {
            format!("{subpath}/{version_part}")
        }
    }

    fn package_tag_name(subpath: &str, version: &Version) -> String {
        Self::version_ref(subpath, &version.to_string(), true)
    }
}

impl BareRepoCache {
    pub(crate) fn ensure_synced(&mut self, repo_url: &str) -> Result<&BareRepo> {
        match self.repos.entry(repo_url.to_string()) {
            Entry::Occupied(entry) => Ok(entry.into_mut()),
            Entry::Vacant(entry) => {
                let repo = BareRepo::sync(repo_url)?;
                Ok(entry.insert(repo))
            }
        }
    }

    pub(crate) fn get(&self, repo_url: &str) -> Option<&BareRepo> {
        self.repos.get(repo_url)
    }
}

fn repo_exists(bare_dir: &Path) -> bool {
    bare_dir.join("HEAD").exists()
}
