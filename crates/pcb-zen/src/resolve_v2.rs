use anyhow::Result;
use globset::{Glob, GlobSetBuilder};
use pcb_zen_core::config::{find_workspace_root, DependencySpec, PcbToml};
use pcb_zen_core::{DefaultFileProvider, FileProvider};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use walkdir::WalkDir;

/// Check if the input paths are in a V2 workspace and run dependency resolution if needed
///
/// This is called once per `pcb build` invocation (workspace-first architecture).
/// For V2 workspaces, it runs dependency resolution before any .zen file discovery.
pub fn maybe_resolve_v2_workspace(paths: &[PathBuf]) -> Result<()> {
    let input_path = if paths.is_empty() {
        std::env::current_dir()?
    } else {
        paths[0].clone()
    };

    let file_provider = Arc::new(DefaultFileProvider::new());
    let workspace_root = find_workspace_root(&*file_provider, &input_path);

    let pcb_toml_path = workspace_root.join("pcb.toml");
    if !file_provider.exists(&pcb_toml_path) {
        return Ok(());
    }

    let config = PcbToml::from_file(&*file_provider, &pcb_toml_path)?;
    if let PcbToml::V2(_) = config {
        resolve_dependencies(&*file_provider, &workspace_root, &config)?;
    }

    Ok(())
}

/// V2 dependency resolution
///
/// Discovers member packages and builds dependency graph.
/// TODO: Implement MVS algorithm and lockfile verification.
fn resolve_dependencies(
    file_provider: &dyn FileProvider,
    workspace_root: &Path,
    config: &PcbToml,
) -> Result<()> {
    let PcbToml::V2(v2) = config else {
        unreachable!("resolve_dependencies called on non-V2 config");
    };

    println!("🔍 V2 Dependency Resolution");
    println!("  Workspace root: {}", workspace_root.display());

    // Discover member packages
    let member_patterns = v2
        .workspace
        .as_ref()
        .map(|w| w.members.as_slice())
        .unwrap_or(&[]);
    let packages = discover_packages(file_provider, workspace_root, member_patterns)?;

    // Display workspace type
    if v2.workspace.is_some() {
        println!("  Type: Explicit workspace");
        if !member_patterns.is_empty() {
            println!("  Member patterns: {:?}", member_patterns);
        }
    } else {
        println!("  Type: Standalone package (implicit workspace)");
    }

    // Display discovered packages
    println!("\n📦 Discovered {} package(s):", packages.len());
    for (pcb_toml_path, config) in &packages {
        let PcbToml::V2(v2) = config else { continue };

        let package_name = pcb_toml_path
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy())
            .unwrap_or_else(|| "unknown".into());

        if let Some(board) = &v2.board {
            println!("  - {} (board: {}) → {}", package_name, board.name, pcb_toml_path.display());
        } else {
            println!("  - {} → {}", package_name, pcb_toml_path.display());
        }
    }

    println!("\n✅ V2 dependency resolution complete (stub - exiting for now)");
    std::process::exit(0);
}

/// Discover V2 packages matching glob patterns
fn discover_packages(
    file_provider: &dyn FileProvider,
    workspace_root: &Path,
    patterns: &[String],
) -> Result<Vec<(PathBuf, PcbToml)>> {
    if patterns.is_empty() {
        return Ok(vec![]);
    }

    // Build glob matchers
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(pattern)?);
        // Match pattern without /* suffix for exact directory matches
        if let Some(exact) = pattern.strip_suffix("/*") {
            builder.add(Glob::new(exact)?);
        }
    }
    let glob_set = builder.build()?;

    // Walk workspace and collect matching V2 packages
    let mut packages = Vec::new();
    for entry in WalkDir::new(workspace_root)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if !entry.path().is_dir() {
            continue;
        }

        if let Ok(rel_path) = entry.path().strip_prefix(workspace_root) {
            if glob_set.is_match(rel_path) {
                let pcb_toml = entry.path().join("pcb.toml");
                if file_provider.exists(&pcb_toml) {
                    if let Ok(config) = PcbToml::from_file(file_provider, &pcb_toml) {
                        if matches!(config, PcbToml::V2(_)) {
                            packages.push((pcb_toml, config));
                        }
                    }
                }
            }
        }
    }

    Ok(packages)
}
