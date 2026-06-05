use anyhow::{Context, Result};
use ignore::{Walk, WalkBuilder};
use pcb_zen_core::DefaultFileProvider;
use pcb_zen_core::config::{PcbToml, WorkspaceConfig, pcb_version_from_cargo};
use pcb_zen_core::workspace::{WORKSPACE_DISCOVERY_EXCLUDE_DIRS, WORKSPACE_DISCOVERY_MAX_DEPTH};
use std::path::{Path, PathBuf};

/// Convert all pcb.toml files in workspace to V2
pub fn convert_workspace_to_v2(
    workspace_root: &Path,
    repository: &str,
    repo_subpath: Option<&Path>,
) -> Result<()> {
    eprintln!("  Repository: {}", repository);
    if let Some(p) = repo_subpath {
        eprintln!("  Repo subpath: {}", p.display());
    }

    generate_package_manifests(workspace_root)?;

    // Convert root pcb.toml
    let root_pcb_toml = workspace_root.join("pcb.toml");
    if root_pcb_toml.exists() {
        let repo_subpath_str = repo_subpath.map(|p| p.to_string_lossy().into_owned());
        if convert_pcb_toml_to_v2(
            &root_pcb_toml,
            Some(repository),
            repo_subpath_str.as_deref(),
        )? {
            eprintln!("  ✓ Converted {}", root_pcb_toml.display());
        }
    }

    // Convert all package manifests, including newly created ones.
    for entry in workspace_walker(workspace_root).filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.file_name() != Some(std::ffi::OsStr::new("pcb.toml")) || path == root_pcb_toml {
            continue;
        }

        if convert_pcb_toml_to_v2(path, None, None)? {
            eprintln!("  ✓ Converted {}", path.display());
        }
    }

    Ok(())
}

/// Generate empty pcb.toml files for discovered package roots.
fn generate_package_manifests(workspace_root: &Path) -> Result<()> {
    use std::collections::BTreeSet;

    let package_extensions = ["zen", "kicad_mod", "kicad_sym"];

    // Collect all directories that contain package files or already have pcb.toml
    let mut candidate_dirs: BTreeSet<PathBuf> = BTreeSet::new();

    for entry in workspace_walker(workspace_root).filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let dominated_file = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| package_extensions.contains(&ext));
        let is_manifest = path.file_name() == Some(std::ffi::OsStr::new("pcb.toml"));

        if (dominated_file || is_manifest)
            && let Some(dir) = path.parent()
            && dir != workspace_root
        {
            candidate_dirs.insert(dir.to_path_buf());
        }
    }

    // Sort by depth (shallowest first) so parent packages are processed before children
    let mut sorted_dirs: Vec<_> = candidate_dirs.into_iter().collect();
    sorted_dirs.sort_by_key(|p| p.components().count());

    // Process directories, tracking which subtrees are already covered
    let mut covered: Vec<PathBuf> = Vec::new();

    for dir in sorted_dirs {
        if covered.iter().any(|pkg| dir.starts_with(pkg)) {
            continue;
        }

        let pcb_toml = dir.join("pcb.toml");
        if !pcb_toml.exists() {
            std::fs::write(&pcb_toml, "")?;
            eprintln!("  ✓ Created {}", pcb_toml.display());
        }
        covered.push(dir);
    }

    Ok(())
}

/// Convert a single pcb.toml file to V2 format
fn convert_pcb_toml_to_v2(
    path: &Path,
    repository: Option<&str>,
    repo_subpath: Option<&str>,
) -> Result<bool> {
    let file_provider = DefaultFileProvider::new();

    // Read existing config
    let mut config = PcbToml::from_file(&file_provider, path)?;

    // Check if already V2
    if config.is_v2() {
        return Ok(false);
    }

    // Clone default_board before conversion
    let default_board = config
        .workspace
        .as_ref()
        .and_then(|w| w.default_board.clone());

    // Clear V1 fields
    config.packages.clear();
    config.module = None;

    // Update workspace section if this is the root
    if let Some(repo) = repository {
        config.workspace = Some(WorkspaceConfig {
            repository: Some(repo.to_string()),
            path: repo_subpath.map(|s| s.to_string()),
            pcb_version: Some(pcb_version_from_cargo()),
            default_board,
            vendor: vec!["github.com/diodeinc/registry/**".to_string()],
            ..WorkspaceConfig::default()
        });
    } else {
        // In V2, only the workspace root has a [workspace] section. Packages and boards
        // must not have workspace metadata.
        config.workspace = None;
    }

    // Serialize and write back
    let content = toml::to_string_pretty(&config).context("Failed to serialize V2 config")?;

    std::fs::write(path, content).with_context(|| format!("Failed to write {}", path.display()))?;

    Ok(true)
}

fn workspace_walker(root: &Path) -> Walk {
    WalkBuilder::new(root)
        .max_depth(Some(WORKSPACE_DISCOVERY_MAX_DEPTH + 1))
        .hidden(true)
        .git_ignore(true)
        .git_exclude(true)
        .filter_entry(skip_generated_dirs)
        .build()
}

fn skip_generated_dirs(entry: &ignore::DirEntry) -> bool {
    if entry.file_type().is_some_and(|ft| ft.is_dir())
        && let Some(name) = entry.file_name().to_str()
    {
        return !WORKSPACE_DISCOVERY_EXCLUDE_DIRS.contains(&name);
    }
    true
}
