use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use pcb_zen::{MemberPackage, WorkspaceInfo};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AddTarget {
    pub(crate) package_url: String,
    pub(crate) pcb_toml_path: PathBuf,
}

pub(crate) fn discover_add_targets(
    workspace: &WorkspaceInfo,
    start_path: &Path,
) -> Result<Vec<AddTarget>> {
    let candidate_dir = if start_path.is_file() {
        start_path.parent().unwrap_or(start_path)
    } else {
        start_path
    };
    let candidate_dir = candidate_dir
        .canonicalize()
        .unwrap_or_else(|_| candidate_dir.to_path_buf());
    let workspace_root = workspace
        .root
        .canonicalize()
        .unwrap_or_else(|_| workspace.root.clone());

    if candidate_dir == workspace_root {
        let mut targets: Vec<_> = workspace
            .packages
            .iter()
            .map(|(package_url, pkg)| add_target_for_member(&workspace.root, package_url, pkg))
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

    let mut best_match: Option<(usize, AddTarget)> = None;
    for (package_url, pkg) in &workspace.packages {
        let pkg_dir = pkg
            .dir(&workspace.root)
            .canonicalize()
            .unwrap_or_else(|_| pkg.dir(&workspace.root));
        if candidate_dir == pkg_dir || candidate_dir.starts_with(&pkg_dir) {
            let score = pkg_dir.as_os_str().len();
            let target = add_target_for_member(&workspace.root, package_url, pkg);
            if best_match
                .as_ref()
                .map(|(best_score, _)| score > *best_score)
                .unwrap_or(true)
            {
                best_match = Some((score, target));
            }
        }
    }

    if let Some((_, target)) = best_match {
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

fn add_target_for_member(
    workspace_root: &Path,
    package_url: &str,
    pkg: &MemberPackage,
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

    fn member(rel_path: &str) -> MemberPackage {
        MemberPackage {
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

    fn workspace_with_members(root: &str) -> WorkspaceInfo {
        WorkspaceInfo {
            root: PathBuf::from(root),
            cache_dir: PathBuf::new(),
            config: None,
            packages: BTreeMap::from([
                (
                    "github.com/example/repo/boards/Main".to_string(),
                    member("boards/Main"),
                ),
                (
                    "github.com/example/repo/modules/Lib".to_string(),
                    member("modules/Lib"),
                ),
            ]),
            lockfile: None,
            errors: vec![],
        }
    }

    #[test]
    fn discover_add_targets_from_workspace_root_selects_all_packages() {
        let workspace = workspace_with_members("/repo");
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
        let workspace = workspace_with_members("/repo");
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
