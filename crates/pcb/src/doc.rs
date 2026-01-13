use anyhow::{Context, Result};
use clap::Args;
use std::io::{self, IsTerminal};
use std::path::PathBuf;

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
        return run_docgen_for_package(pkg);
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
fn run_docgen_for_package(pkg: &str) -> Result<()> {
    // Handle @stdlib alias -> fetch toolchain-pinned version
    if pkg == "@stdlib" || pkg.starts_with("@stdlib/") {
        let version = pcb_zen_core::STDLIB_VERSION;
        let module_path = pcb_zen_core::STDLIB_MODULE_PATH;
        eprintln!("Fetching stdlib v{}...", version);
        return run_docgen_for_remote_package(module_path, version);
    }

    // Handle remote package URLs (github.com/user/repo@version)
    if pkg.starts_with("github.com/") || pkg.starts_with("gitlab.com/") {
        let (module_path, version) = parse_versioned_url(pkg)?;
        eprintln!("Fetching {}@v{}...", module_path, version);
        return run_docgen_for_remote_package(module_path, version);
    }

    // Local directory
    let dir = PathBuf::from(pkg);
    let url = get_local_package_url(&dir);
    run_docgen(&dir, url.as_deref())
}

/// Parse a versioned URL like "github.com/user/repo@1.0.0" into (module_path, version)
fn parse_versioned_url(url: &str) -> Result<(&str, &str)> {
    if let Some(at_pos) = url.rfind('@') {
        Ok((&url[..at_pos], &url[at_pos + 1..]))
    } else {
        anyhow::bail!(
            "Version required for remote packages.\n\
             Use format: pcb doc --package {}@<version>",
            url
        )
    }
}

/// Fetch and generate docs for a remote package
fn run_docgen_for_remote_package(module_path: &str, version: &str) -> Result<()> {
    let cache_dir = dirs::home_dir()
        .expect("Cannot determine home directory")
        .join(".pcb/cache")
        .join(module_path)
        .join(version);

    let package_root = pcb_zen::ensure_sparse_checkout(&cache_dir, module_path, version, true)
        .with_context(|| format!("Failed to fetch {}@{}", module_path, version))?;

    run_docgen(&package_root, Some(module_path))
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

fn run_docgen(dir: &std::path::Path, package_url: Option<&str>) -> Result<()> {
    if !dir.exists() {
        anyhow::bail!("Package directory '{}' does not exist.", dir.display());
    }
    if !dir.is_dir() {
        anyhow::bail!("'{}' is not a directory.", dir.display());
    }

    let display_path = get_display_path(dir);
    let result = pcb_docgen::generate_docs(dir, package_url, display_path.as_deref())?;

    if result.library_count == 0 && result.module_count == 0 {
        anyhow::bail!(
            "No .zen files found under '{}'; nothing to document.",
            dir.display()
        );
    }

    if io::stdout().is_terminal() {
        termimad::print_text(&result.markdown);
    } else {
        println!("{}", result.markdown);
    }

    eprintln!(
        "Generated docs for {} libraries and {} modules.",
        result.library_count, result.module_count
    );

    Ok(())
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
