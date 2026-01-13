use anyhow::{Context, Result};
use clap::Args;
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};

#[derive(Args)]
pub struct DocArgs {
    /// Documentation path for embedded docs (e.g. "spec", "tutorial")
    #[arg(default_value = "")]
    pub path: String,

    /// List available pages or sections instead of showing content
    #[arg(long, short = 'l')]
    pub list: bool,

    /// Generate documentation from a package (local path, @stdlib, or github.com/user/repo@version)
    #[arg(long, value_name = "PACKAGE")]
    pub package: Option<String>,
}

pub fn execute(args: DocArgs) -> Result<()> {
    // --package flag: generate docs for a Zener package
    if let Some(pkg) = &args.package {
        return run_docgen_for_package(pkg, args.list);
    }

    // Require a path or --list flag
    if args.path.is_empty() && !args.list {
        anyhow::bail!(
            "Usage: pcb doc <PAGE> or pcb doc --package <PACKAGE>\n\n\
             Examples:\n\
             \x20 pcb doc spec                  # Language specification\n\
             \x20 pcb doc --list                # List available pages\n\
             \x20 pcb doc --package @stdlib     # Generate stdlib docs"
        );
    }

    // Show embedded static docs
    render_embedded_docs(&args.path, args.list)
}

fn render_embedded_docs(path: &str, list: bool) -> Result<()> {
    let content = if list {
        pcb_docs::lookup_list(path)
    } else {
        pcb_docs::lookup(path)
    };

    match content {
        Ok(content) => {
            if !list && io::stdout().is_terminal() {
                termimad::print_text(&content);
            } else {
                println!("{}", content);
            }
            Ok(())
        }
        Err(e) => {
            // Add hint if it looks like a path or URL
            if looks_like_package_path(path) {
                anyhow::bail!("{}\n\nDid you mean: pcb doc --package {}", e, path)
            } else {
                anyhow::bail!("{}", e)
            }
        }
    }
}

/// Check if input looks like a filesystem path or package URL
fn looks_like_package_path(s: &str) -> bool {
    s.starts_with('.')
        || s.starts_with('/')
        || s.starts_with('@')
        || s.starts_with("github.com/")
        || s.starts_with("gitlab.com/")
        || s.contains('\\')
}

/// Generate docs for a package specified as local path, @stdlib, or remote URL
fn run_docgen_for_package(pkg: &str, list: bool) -> Result<()> {
    // Handle @stdlib alias (with optional subpath filter)
    if pkg == "@stdlib" || pkg.starts_with("@stdlib/") {
        let version = pcb_zen_core::STDLIB_VERSION;
        let module_path = pcb_zen_core::STDLIB_MODULE_PATH;

        // Extract filter if subpath provided
        let filter = if pkg.starts_with("@stdlib/") {
            Some(pkg.strip_prefix("@stdlib/").unwrap())
        } else {
            None
        };

        return run_docgen_for_remote_package("@stdlib", module_path, version, filter, list);
    }

    // Handle remote package URLs (github.com/user/repo@version)
    if pkg.starts_with("github.com/") || pkg.starts_with("gitlab.com/") {
        let (module_path, version) = parse_versioned_url(pkg)?;
        return run_docgen_for_remote_package(module_path, module_path, version, None, list);
    }

    // Local path - find package root and filter
    let path = PathBuf::from(pkg);
    let (package_dir, filter) = find_package_root_and_filter(&path)?;
    let url = get_local_package_url(&package_dir);
    let display_name = url
        .as_deref()
        .unwrap_or_else(|| package_dir.to_str().unwrap_or("."));
    if list {
        return list_package_files(display_name, &package_dir, filter.as_deref());
    }
    run_docgen(&package_dir, url.as_deref(), filter.as_deref())
}

/// Parse a versioned URL like "github.com/user/repo@1.0.0" into (module_path, version)
fn parse_versioned_url(url: &str) -> Result<(&str, &str)> {
    if let Some(at_pos) = url.rfind('@') {
        let module_path = &url[..at_pos];
        let version = &url[at_pos + 1..];
        if version.is_empty() {
            anyhow::bail!(
                "Version required for remote packages.\n\
                 Use format: pcb doc --package {}@<version>",
                module_path
            );
        }
        Ok((module_path, version))
    } else {
        anyhow::bail!(
            "Version required for remote packages.\n\
             Use format: pcb doc --package {}@<version>",
            url
        )
    }
}

/// Fetch and generate docs for a remote package
fn run_docgen_for_remote_package(
    display_name: &str,
    module_path: &str,
    version: &str,
    filter: Option<&str>,
    list: bool,
) -> Result<()> {
    let cache_dir = dirs::home_dir()
        .expect("Cannot determine home directory")
        .join(".pcb/cache")
        .join(module_path)
        .join(version);

    let package_root = pcb_zen::ensure_sparse_checkout(&cache_dir, module_path, version, true)
        .with_context(|| format!("Failed to fetch {}@{}", module_path, version))?;

    if list {
        return list_package_files(display_name, &package_root, filter);
    }
    run_docgen(&package_root, Some(module_path), filter)
}

/// Get the package URL for a local directory using workspace info
fn get_local_package_url(dir: &std::path::Path) -> Option<String> {
    let canonical = dir.canonicalize().ok()?;
    let file_provider = pcb_zen_core::DefaultFileProvider::new();
    let workspace_info = pcb_zen::get_workspace_info(&file_provider, &canonical).ok()?;
    let repo = workspace_info.repository()?;

    let relative = canonical.strip_prefix(&workspace_info.root).ok()?;
    let relative_str = relative.to_string_lossy().replace('\\', "/");

    if relative_str.is_empty() {
        Some(repo.to_string())
    } else {
        Some(format!("{}/{}", repo, relative_str))
    }
}

fn run_docgen(
    dir: &std::path::Path,
    package_url: Option<&str>,
    filter: Option<&str>,
) -> Result<()> {
    if !dir.exists() {
        anyhow::bail!("Package directory '{}' does not exist.", dir.display());
    }
    if !dir.is_dir() {
        anyhow::bail!("'{}' is not a directory.", dir.display());
    }

    let display_path = get_display_path(dir);
    let result = pcb_docgen::generate_docs(dir, package_url, display_path.as_deref(), filter)?;

    if result.library_count == 0 && result.module_count == 0 {
        let filter_msg = filter
            .map(|f| format!(" matching '{}'", f))
            .unwrap_or_default();
        anyhow::bail!(
            "No .zen files found{} under '{}'; nothing to document.",
            filter_msg,
            dir.display()
        );
    }

    if io::stdout().is_terminal() {
        termimad::print_text(&result.markdown);
    } else {
        println!("{}", result.markdown);
    }

    Ok(())
}

/// List .zen files in a package as a tree structure.
fn list_package_files(display_name: &str, dir: &Path, filter: Option<&str>) -> Result<()> {
    use ptree::TreeBuilder;
    use std::collections::BTreeMap;
    use walkdir::WalkDir;

    if !dir.exists() {
        anyhow::bail!("Package directory '{}' does not exist.", dir.display());
    }

    let canonical = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());

    // Collect all .zen files, excluding test/, layout/, and hidden directories
    let mut files: Vec<String> = WalkDir::new(&canonical)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "zen"))
        .filter(|e| {
            let rel_path = e.path().strip_prefix(&canonical).unwrap_or(e.path());
            !rel_path.components().any(|c| {
                let s = c.as_os_str().to_string_lossy();
                s == "test" || s == "layout" || s.starts_with('.')
            })
        })
        .filter_map(|e| {
            let rel_path = e.path().strip_prefix(&canonical).ok()?;
            let rel_str = rel_path.to_string_lossy().replace('\\', "/");

            // Apply filter if provided
            if let Some(f) = filter {
                if !rel_str.starts_with(f) && rel_str != f {
                    return None;
                }
            }
            Some(rel_str)
        })
        .collect();

    files.sort();

    if files.is_empty() {
        let filter_msg = filter
            .map(|f| format!(" matching '{}'", f))
            .unwrap_or_default();
        anyhow::bail!(
            "No .zen files found{} under '{}'.",
            filter_msg,
            dir.display()
        );
    }

    // Group files by directory
    let mut tree: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for file in &files {
        if let Some(slash_pos) = file.rfind('/') {
            let dir_part = &file[..slash_pos];
            let file_part = &file[slash_pos + 1..];
            tree.entry(dir_part.to_string())
                .or_default()
                .push(file_part.to_string());
        } else {
            tree.entry(String::new()).or_default().push(file.clone());
        }
    }

    // Build ptree
    let mut builder = TreeBuilder::new(display_name.to_string());

    for (dir_name, files_in_dir) in &tree {
        if dir_name.is_empty() {
            for file in files_in_dir {
                builder.add_empty_child(file.clone());
            }
        } else {
            builder.begin_child(format!("{}/", dir_name));
            for file in files_in_dir {
                builder.add_empty_child(file.clone());
            }
            builder.end_child();
        }
    }

    let tree = builder.build();
    ptree::print_tree(&tree)?;

    Ok(())
}

/// Find the package root directory and the filter path within it.
///
/// Walks up the directory tree to find a `pcb.toml` file. Returns the package
/// root directory and the relative path from the root to the original path.
fn find_package_root_and_filter(path: &Path) -> Result<(PathBuf, Option<String>)> {
    // Canonicalize the input path to resolve .. and symlinks
    let canonical = path
        .canonicalize()
        .with_context(|| format!("Path '{}' does not exist", path.display()))?;

    // Determine the starting directory for the search
    let start_dir = if canonical.is_file() {
        canonical
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| canonical.clone())
    } else {
        canonical.clone()
    };

    // Walk up to find pcb.toml
    let mut current = start_dir.as_path();
    loop {
        if current.join("pcb.toml").exists() {
            // Found package root
            let filter = canonical.strip_prefix(current).ok().and_then(|rel| {
                let s = rel.to_string_lossy().replace('\\', "/");
                if s.is_empty() {
                    None
                } else {
                    Some(s)
                }
            });
            return Ok((current.to_path_buf(), filter));
        }

        match current.parent() {
            Some(parent) => current = parent,
            None => {
                // No pcb.toml found, use the original path as package root with no filter
                // This maintains backward compatibility for directories without pcb.toml
                return Ok((canonical, None));
            }
        }
    }
}

/// Get the display path for the source comment.
///
/// If the workspace has a .pcb/cache symlink pointing to ~/.pcb/cache,
/// return a path relative to the workspace cache instead of the absolute path.
fn get_display_path(dir: &std::path::Path) -> Option<String> {
    let canonical = dir.canonicalize().ok()?;

    // Check if path is under ~/.pcb/cache
    let home_cache = dirs::home_dir()?.join(".pcb/cache");
    let home_cache_canonical = home_cache.canonicalize().ok()?;

    let relative_to_cache = canonical.strip_prefix(&home_cache_canonical).ok()?;

    // Check if current workspace has .pcb/cache symlink
    let cwd = std::env::current_dir().ok()?;
    let workspace_cache = cwd.join(".pcb/cache");

    if workspace_cache.is_symlink() {
        // Verify it points to ~/.pcb/cache
        if let Ok(target) = workspace_cache.read_link() {
            let target_canonical = if target.is_absolute() {
                target.canonicalize().ok()
            } else {
                cwd.join(&target).canonicalize().ok()
            };

            if target_canonical.as_ref() == Some(&home_cache_canonical) {
                // Use workspace-relative path
                let workspace_relative = PathBuf::from(".pcb/cache").join(relative_to_cache);
                return Some(workspace_relative.to_string_lossy().replace('\\', "/"));
            }
        }
    }

    // Fall back to absolute path
    Some(canonical.to_string_lossy().into_owned())
}
