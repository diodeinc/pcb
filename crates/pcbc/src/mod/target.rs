use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use pcb_zen::{WorkspaceInfo, WorkspacePackage};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AddTarget {
    pub(crate) package_url: String,
    pub(crate) pcb_toml_path: PathBuf,
}

pub(crate) fn discover_add_targets(
    workspace: &WorkspaceInfo,
    start_path: &Path,
) -> Result<Vec<AddTarget>> {
    let candidate_dir = candidate_dir(start_path);
    let workspace_root = canonicalize(&workspace.root);

    if candidate_dir == workspace_root {
        let mut targets: Vec<_> = workspace
            .packages
            .iter()
            .map(|(package_url, pkg)| add_target_for_package(&workspace.root, package_url, pkg))
            .collect();
        targets.sort_by(|a, b| a.pcb_toml_path.cmp(&b.pcb_toml_path));

        if !targets.is_empty() {
            return Ok(targets);
        }

        let root_pcb_toml = workspace_root.join("pcb.toml");
        if root_pcb_toml.exists() {
            return Ok(vec![AddTarget {
                package_url: root_package_url(workspace),
                pcb_toml_path: root_pcb_toml,
            }]);
        }
    }

    if let Some(target) = package_target_for_dir(workspace, &candidate_dir) {
        return Ok(vec![target]);
    }

    bail!(
        "`pcb mod sync` must be run from a package directory or the workspace root.\n\
         Current path: {}\n\
         Workspace root: {}",
        candidate_dir.display(),
        workspace_root.display()
    );
}

pub(crate) fn discover_package_target(
    workspace: &WorkspaceInfo,
    start_path: &Path,
) -> Option<AddTarget> {
    package_target_for_dir(workspace, &candidate_dir(start_path))
}

fn candidate_dir(start_path: &Path) -> PathBuf {
    let dir = if start_path.is_file() {
        start_path.parent().unwrap_or(start_path)
    } else {
        start_path
    };
    canonicalize(dir)
}

fn canonicalize(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn package_target_for_dir(workspace: &WorkspaceInfo, candidate_dir: &Path) -> Option<AddTarget> {
    workspace
        .packages
        .iter()
        .filter_map(|(package_url, pkg)| {
            let pkg_dir = canonicalize(&pkg.dir(&workspace.root));
            (candidate_dir == pkg_dir || candidate_dir.starts_with(&pkg_dir)).then(|| {
                (
                    pkg_dir.as_os_str().len(),
                    add_target_for_package(&workspace.root, package_url, pkg),
                )
            })
        })
        .max_by_key(|(score, _)| *score)
        .map(|(_, target)| target)
}

pub(crate) fn add_target_for_package(
    workspace_root: &Path,
    package_url: &str,
    pkg: &WorkspacePackage,
) -> AddTarget {
    AddTarget {
        package_url: package_url.to_string(),
        pcb_toml_path: pkg.dir(workspace_root).join("pcb.toml"),
    }
}

fn root_package_url(workspace: &WorkspaceInfo) -> String {
    workspace
        .workspace_base_url()
        .unwrap_or_else(|| "workspace".to_string())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn package(rel_path: &str) -> WorkspacePackage {
        WorkspacePackage {
            rel_path: PathBuf::from(rel_path),
            config: pcb_zen_core::config::PcbToml::default(),
            version: None,
            published_at: None,
            preferred: false,
            dirty: false,
            entrypoints: Vec::new(),
            symbol_files: Vec::new(),
        }
    }

    fn workspace_with_packages(root: &str) -> WorkspaceInfo {
        WorkspaceInfo {
            root: PathBuf::from(root),
            cache_dir: PathBuf::new(),
            config: None,
            packages: BTreeMap::from([
                (
                    "github.com/example/repo/boards/Main".to_string(),
                    package("boards/Main"),
                ),
                (
                    "github.com/example/repo/modules/Lib".to_string(),
                    package("modules/Lib"),
                ),
            ]),
            errors: vec![],
        }
    }

    #[test]
    fn discover_add_targets_from_workspace_root_selects_all_packages() {
        let workspace = workspace_with_packages("/repo");
        let targets = discover_add_targets(&workspace, Path::new("/repo")).unwrap();
        assert_eq!(targets.len(), 2);
        assert_eq!(
            targets[0].pcb_toml_path,
            PathBuf::from("/repo/boards/Main/pcb.toml")
        );
        assert_eq!(
            targets[1].pcb_toml_path,
            PathBuf::from("/repo/modules/Lib/pcb.toml")
        );
    }

    #[test]
    fn discover_add_targets_from_package_dir_selects_single_package() {
        let workspace = workspace_with_packages("/repo");
        let targets = discover_add_targets(&workspace, Path::new("/repo/modules/Lib/src")).unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(
            targets[0].package_url,
            "github.com/example/repo/modules/Lib".to_string()
        );
        assert_eq!(
            targets[0].pcb_toml_path,
            PathBuf::from("/repo/modules/Lib/pcb.toml")
        );
    }
}
