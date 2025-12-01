use anyhow::{Context, Result};
use ignore::WalkBuilder;
use pcb_zen_core::config::{PcbToml, WorkspaceConfig};
use pcb_zen_core::DefaultFileProvider;
use std::path::{Path, PathBuf};

use crate::file_walker::skip_vendor;

/// Get pcb-version from CARGO_PKG_VERSION (major.minor format)
fn pcb_version_from_cargo() -> String {
    let version = env!("CARGO_PKG_VERSION");
    let parts: Vec<&str> = version.split('.').collect();
    if parts.len() >= 2 {
        format!("{}.{}", parts[0], parts[1])
    } else {
        version.to_string()
    }
}

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

    // Detect and filter member patterns
    let members = detect_member_patterns(workspace_root)?;
    if !members.is_empty() {
        eprintln!("  Members: {:?}", members);
    }

    // Generate member package pcb.toml files
    generate_member_packages(workspace_root, &members)?;

    // Convert root pcb.toml
    let root_pcb_toml = workspace_root.join("pcb.toml");
    if root_pcb_toml.exists() {
        let repo_subpath_str = repo_subpath.map(|p| p.to_string_lossy().into_owned());
        convert_pcb_toml_to_v2(
            &root_pcb_toml,
            Some(repository),
            repo_subpath_str.as_deref(),
            &members,
        )?;
        eprintln!("  ✓ Converted {}", root_pcb_toml.display());
    }

    // Find and convert all member pcb.toml files (including newly created ones)
    let walker = WalkBuilder::new(workspace_root)
        .hidden(true)
        .git_ignore(true)
        .git_exclude(true)
        .filter_entry(skip_vendor)
        .build();

    for entry in walker.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.file_name() == Some(std::ffi::OsStr::new("pcb.toml")) && path != root_pcb_toml {
            convert_pcb_toml_to_v2(path, None, None, &[])?;
            eprintln!("  ✓ Converted {}", path.display());
        }
    }

    Ok(())
}

/// Detect member patterns based on existing directories
fn detect_member_patterns(workspace_root: &Path) -> Result<Vec<String>> {
    let base_patterns = pcb_zen_core::config::default_members();
    let mut filtered = Vec::new();

    for pattern in &base_patterns {
        let dir_name = pattern.trim_end_matches("/*");
        let dir_path = workspace_root.join(dir_name);

        if dir_path.exists() && dir_path.is_dir() {
            filtered.push(pattern.to_string());
        }
    }

    Ok(filtered)
}

/// Generate empty pcb.toml files for member packages
fn generate_member_packages(workspace_root: &Path, members: &[String]) -> Result<()> {
    use std::collections::BTreeSet;

    let package_extensions = ["zen", "kicad_mod", "kicad_sym"];

    // Collect all directories that contain package files or already have pcb.toml
    let mut candidate_dirs: BTreeSet<PathBuf> = BTreeSet::new();

    for pattern in members {
        let dir_name = pattern.trim_end_matches("/*");
        let base_dir = workspace_root.join(dir_name);
        if !base_dir.exists() {
            continue;
        }

        let walker = WalkBuilder::new(&base_dir)
            .max_depth(Some(3))
            .hidden(true)
            .git_ignore(true)
            .git_exclude(true)
            .filter_entry(skip_vendor)
            .build();

        for entry in walker.filter_map(|e| e.ok()) {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }

            let dominated_file = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|ext| package_extensions.contains(&ext));
            let is_manifest = path.file_name() == Some(std::ffi::OsStr::new("pcb.toml"));

            if dominated_file || is_manifest {
                if let Some(dir) = path.parent() {
                    candidate_dirs.insert(dir.to_path_buf());
                }
            }
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
    members: &[String],
) -> Result<()> {
    let file_provider = DefaultFileProvider::new();

    // Read existing config
    let mut config = PcbToml::from_file(&file_provider, path)?;

    // Check if already V2
    if config.is_v2() {
        eprintln!("  ⊙ Already V2: {}", path.display());
        return Ok(());
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
            name: None,
            repository: Some(repo.to_string()),
            path: repo_subpath.map(|s| s.to_string()),
            resolver: None,
            pcb_version: Some(pcb_version_from_cargo()),
            members: members.to_vec(),
            default_board,
            vendor: vec!["github.com/diodeinc/registry/**".to_string()],
        });
    } else if let Some(mut ws) = config.workspace.take() {
        // Member package - set pcb-version
        ws.resolver = None;
        ws.pcb_version = Some(pcb_version_from_cargo());
        config.workspace = Some(ws);
    }

    // Serialize and write back
    let content = toml::to_string_pretty(&config).context("Failed to serialize V2 config")?;

    std::fs::write(path, content).with_context(|| format!("Failed to write {}", path.display()))?;

    Ok(())
}
