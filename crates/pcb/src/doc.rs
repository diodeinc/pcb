use anyhow::{Context, Result};
use clap::Args;
use pcb_zen::cache_index::ensure_stdlib_materialized;
use semver::Version;
use std::collections::BTreeMap;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;
use syntect::util::{LinesWithEndings, as_24_bit_terminal_escaped};
use termimad::MadSkin;

const LATEST_PACKAGE_VERSION: &str = "latest";
const CHANGELOG_PAGE: &str = "changelog";
const UNRELEASED_CHANGELOG_SELECTOR: &str = "unreleased";

#[derive(Debug, Args)]
pub struct DocArgs {
    /// Documentation path for embedded docs (e.g. "spec", "tutorial", "changelog@latest")
    #[arg(default_value = "")]
    pub path: String,

    /// List available pages or sections instead of showing content
    #[arg(long, short = 'l')]
    pub list: bool,

    /// Generate docs from a package (local path, @stdlib, or github.com/user/repo[@version])
    #[arg(long, value_name = "PACKAGE")]
    pub package: Option<String>,

    /// Install documentation files to ~/.pcb/docs
    #[arg(long)]
    pub install: bool,
}

// Include the generated changelog constants
include!(concat!(env!("OUT_DIR"), "/changelog.rs"));

pub fn execute(args: DocArgs) -> Result<()> {
    // --install flag: write embedded docs to ~/.pcb/docs
    if args.install {
        return install_docs();
    }

    if let Some(selector) = parse_changelog_path(&args.path)? {
        return render_changelog(selector, args.list);
    }

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
             \x20 pcb doc changelog             # Show latest release notes\n\
             \x20 pcb doc changelog@unreleased  # Show unreleased notes\n\
             \x20 pcb doc --package @stdlib     # Generate stdlib docs\n\
             \x20 pcb doc --package github.com/acme/lib@latest"
        );
    }

    // Show embedded static docs
    render_embedded_docs(&args.path, args.list)
}

/// Install embedded documentation files to ~/.pcb/docs
fn install_docs() -> Result<()> {
    let docs_dir = dirs::home_dir()
        .context("Cannot determine home directory")?
        .join(".pcb/docs");

    // Clear existing docs
    if docs_dir.exists() {
        std::fs::remove_dir_all(&docs_dir)
            .with_context(|| format!("Failed to remove {}", docs_dir.display()))?;
    }
    std::fs::create_dir_all(&docs_dir)
        .with_context(|| format!("Failed to create {}", docs_dir.display()))?;

    // Write each embedded page as a .md file
    for page in pcb_docs::list_pages() {
        let file_path = docs_dir.join(format!("{}.md", page.slug));
        std::fs::write(&file_path, page.markdown)
            .with_context(|| format!("Failed to write {}", file_path.display()))?;
    }

    // Write changelog
    std::fs::write(docs_dir.join("CHANGELOG.md"), CHANGELOG_MD)
        .context("Failed to write CHANGELOG.md")?;

    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ChangelogSelector {
    Latest,
    Unreleased,
    Version(String),
}

fn render_changelog(selector: ChangelogSelector, list: bool) -> Result<()> {
    if list {
        println!("{}", changelog_paths(CHANGELOG_MD).join("\n"));
        return Ok(());
    }

    match selector {
        ChangelogSelector::Latest => print_latest_release_notes(),
        ChangelogSelector::Unreleased => print_unreleased_release_notes(),
        ChangelogSelector::Version(version) => {
            let content = extract_versioned_release(CHANGELOG_MD, &version).ok_or_else(|| {
                anyhow::anyhow!(
                    "Release notes for version {version} not found.\n\
                     Use `pcb doc changelog --list` to see available versions."
                )
            })?;
            print_embedded_markdown(&content);
        }
    }

    Ok(())
}

fn parse_changelog_path(path: &str) -> Result<Option<ChangelogSelector>> {
    if path == CHANGELOG_PAGE {
        return Ok(Some(ChangelogSelector::Latest));
    }

    let Some((page, selector)) = path.split_once('@') else {
        return Ok(None);
    };
    if page != CHANGELOG_PAGE {
        return Ok(None);
    }

    if selector.is_empty() || selector.eq_ignore_ascii_case(LATEST_PACKAGE_VERSION) {
        return Ok(Some(ChangelogSelector::Latest));
    }
    if selector.eq_ignore_ascii_case(UNRELEASED_CHANGELOG_SELECTOR) {
        return Ok(Some(ChangelogSelector::Unreleased));
    }

    let version = pcb_zen::tags::parse_version(selector).ok_or_else(|| {
        anyhow::anyhow!(
            "Invalid changelog selector '{path}'.\n\
             Use `pcb doc changelog@latest`, `pcb doc changelog@unreleased`, or `pcb doc changelog@0.4.0`."
        )
    })?;

    Ok(Some(ChangelogSelector::Version(version.to_string())))
}

fn changelog_paths(content: &str) -> Vec<String> {
    let mut paths = vec![
        format!("{CHANGELOG_PAGE}@{LATEST_PACKAGE_VERSION}"),
        format!("{CHANGELOG_PAGE}@{UNRELEASED_CHANGELOG_SELECTOR}"),
    ];
    paths.extend(changelog_versions(content).map(|version| format!("{CHANGELOG_PAGE}@{version}")));
    paths
}

fn changelog_versions(content: &str) -> impl Iterator<Item = String> + '_ {
    content
        .lines()
        .filter_map(parse_release_heading)
        .filter(|heading| !heading.eq_ignore_ascii_case(UNRELEASED_CHANGELOG_SELECTOR))
        .filter_map(normalize_changelog_version)
}

fn extract_versioned_release(content: &str, version: &str) -> Option<String> {
    let mut result = Vec::new();
    let mut in_target_section = false;
    let mut has_content = false;

    for line in content.lines() {
        if let Some(heading) = parse_release_heading(line) {
            if in_target_section {
                break;
            }

            in_target_section = normalize_changelog_version(heading).as_deref() == Some(version);
            if in_target_section {
                result.push(line);
            }
            continue;
        }

        if in_target_section {
            if !line.trim().is_empty() {
                has_content = true;
            }
            if !line.trim().is_empty() || !result.is_empty() {
                result.push(line);
            }
        }
    }

    while result.last().is_some_and(|line| line.trim().is_empty()) {
        result.pop();
    }

    has_content.then(|| result.join("\n"))
}

fn parse_release_heading(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("## [")?;
    let end = rest.find(']')?;
    Some(&rest[..end])
}

fn normalize_changelog_version(version: &str) -> Option<String> {
    pcb_zen::tags::parse_version(version).map(|v| v.to_string())
}

/// Render just the latest release notes (used by self-update)
pub fn print_latest_release_notes() {
    print_embedded_markdown(LATEST_RELEASE_NOTES);
}

/// Render just the unreleased release notes.
pub fn print_unreleased_release_notes() {
    print_embedded_markdown(UNRELEASED_RELEASE_NOTES);
}

fn print_embedded_markdown(content: &str) {
    if io::stdout().is_terminal() {
        print_highlighted_markdown(content);
    } else {
        println!("{}", content);
    }
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
                print_highlighted_markdown(&content);
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
        // Extract filter if subpath provided
        let filter = if pkg.starts_with("@stdlib/") {
            Some(pkg.strip_prefix("@stdlib/").unwrap())
        } else {
            None
        };

        let cwd = std::env::current_dir()?;
        let file_provider = pcb_zen_core::DefaultFileProvider::new();
        let workspace_root = pcb_zen_core::config::find_workspace_root(&file_provider, &cwd)?;
        // Docgen intentionally does not support stdlib patch overrides.
        // Always render docs from the toolchain-managed embedded stdlib.
        let stdlib_root = ensure_stdlib_materialized(&workspace_root)?;
        if list {
            return list_package_files("@stdlib", &stdlib_root, filter);
        }
        return run_docgen(&stdlib_root, Some(pcb_zen_core::STDLIB_MODULE_PATH), filter);
    }

    // When a bare package URL matches the current workspace namespace, prefer the
    // local workspace member over the published remote package.
    if !pkg.contains('@')
        && let Some((package_dir, package_url, filter)) = resolve_local_workspace_package_url(pkg)
    {
        if list {
            return list_package_files(&package_url, &package_dir, filter.as_deref());
        }
        return run_docgen(&package_dir, Some(&package_url), filter.as_deref());
    }

    // Handle remote package URLs (github.com/user/repo@version)
    if pkg.starts_with("github.com/") || pkg.starts_with("gitlab.com/") {
        let (display_name, requested_version) = parse_remote_package_spec(pkg)?;
        let (module_path, version, filter) =
            resolve_remote_package(display_name, requested_version.as_ref())?;
        return run_docgen_for_remote_package(
            display_name,
            &module_path,
            &version,
            filter.as_deref(),
            list,
        );
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

fn resolve_local_workspace_package_url(pkg: &str) -> Option<(PathBuf, String, Option<String>)> {
    let cwd = std::env::current_dir().ok()?;
    let file_provider = pcb_zen_core::DefaultFileProvider::new();
    let workspace_info = pcb_zen::get_workspace_info(&file_provider, &cwd, true).ok()?;

    workspace_info
        .packages
        .iter()
        .filter(|(package_url, _)| pcb_zen_core::workspace::package_url_covers(package_url, pkg))
        .max_by_key(|(package_url, _)| package_url.len())
        .map(|(package_url, member)| {
            let filter = pkg
                .strip_prefix(package_url)
                .and_then(|rest| rest.strip_prefix('/'))
                .map(str::to_string);
            (
                member.dir(&workspace_info.root),
                package_url.clone(),
                filter,
            )
        })
}

/// Parse a remote package URL like "github.com/user/repo/pkg@1.0.0".
///
/// If no version is provided, the latest tagged version is used.
fn parse_remote_package_spec(url: &str) -> Result<(&str, Option<Version>)> {
    let url = url.trim();
    let Some((module_path, version)) = url.rsplit_once('@') else {
        return Ok((url, None));
    };
    if module_path.is_empty() {
        return Ok((url, None));
    }

    let version = version.trim();
    if version.eq_ignore_ascii_case(LATEST_PACKAGE_VERSION) {
        return Ok((module_path, None));
    }
    if version.is_empty() {
        anyhow::bail!(
            "Missing version after '@' in '{url}'.\n\
             Use format: pcb doc --package {module_path}@{LATEST_PACKAGE_VERSION} \
             or pcb doc --package {module_path}@0.4.0"
        );
    }

    let version = pcb_zen::tags::parse_version(version).ok_or_else(|| {
        anyhow::anyhow!(
            "Invalid version suffix in '{url}'.\n\
             Use format: pcb doc --package {module_path}@{LATEST_PACKAGE_VERSION} \
             or pcb doc --package {module_path}@0.4.0"
        )
    })?;

    Ok((module_path, Some(version)))
}

fn resolve_remote_package(
    requested_module_path: &str,
    requested_version: Option<&Version>,
) -> Result<(String, String, Option<String>)> {
    let (repo_url, _) = pcb_zen_core::config::split_repo_and_subpath(requested_module_path);
    let all_versions = pcb_zen::tags::get_all_versions_for_repo(repo_url)
        .with_context(|| format!("Failed to fetch versions from {repo_url}"))?;
    resolve_remote_package_from_versions(requested_module_path, requested_version, &all_versions)
}

fn resolve_remote_package_from_versions(
    requested_module_path: &str,
    requested_version: Option<&Version>,
    all_versions: &BTreeMap<String, Vec<Version>>,
) -> Result<(String, String, Option<String>)> {
    let (repo_url, requested_path) =
        pcb_zen_core::config::split_repo_and_subpath(requested_module_path);
    let (canonical_pkg_path, versions_for_pkg) =
        find_versioned_package(all_versions, requested_path, repo_url)?;

    let module_path = if canonical_pkg_path.is_empty() {
        repo_url.to_string()
    } else {
        format!("{repo_url}/{canonical_pkg_path}")
    };
    let resolved = match requested_version {
        Some(v) => {
            anyhow::ensure!(
                versions_for_pkg.contains(v),
                "Version {v} not found for {module_path}.\nAvailable versions: {}",
                versions_for_pkg
                    .iter()
                    .take(10)
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            v
        }
        None => versions_for_pkg
            .iter()
            .max()
            .ok_or_else(|| anyhow::anyhow!("No versions available for {module_path}"))?,
    };

    Ok((
        module_path,
        resolved.to_string(),
        remote_filter_from_requested_path(requested_path, canonical_pkg_path),
    ))
}

fn find_versioned_package<'a>(
    all_versions: &'a BTreeMap<String, Vec<Version>>,
    requested_path: &'a str,
    repo_url: &str,
) -> Result<(&'a str, &'a [Version])> {
    if let Some(versions) = all_versions.get(requested_path) {
        return Ok((requested_path, versions.as_slice()));
    }

    let mut path = requested_path;
    while let Some(parent_end) = path.rfind('/') {
        path = &requested_path[..parent_end];
        if let Some(versions) = all_versions.get(path) {
            return Ok((path, versions.as_slice()));
        }
    }

    // Fall back to root-level tags (key "") for repos that tag at the root.
    if let Some(versions) = all_versions.get("") {
        return Ok(("", versions.as_slice()));
    }

    if all_versions.is_empty() {
        anyhow::bail!("No tagged versions found in repository {repo_url}");
    }
    let names: Vec<_> = all_versions
        .keys()
        .take(10)
        .map(|s| {
            if s.is_empty() {
                "<repo root>"
            } else {
                s.as_str()
            }
        })
        .collect();
    let ellipsis = if all_versions.len() > 10 { ", ..." } else { "" };
    anyhow::bail!(
        "No tagged versions found for path '{requested_path}' in {repo_url}.\n\
         Available packages: {}{ellipsis}",
        names.join(", ")
    )
}

fn remote_filter_from_requested_path(
    requested_path: &str,
    canonical_pkg_path: &str,
) -> Option<String> {
    if requested_path.is_empty() || requested_path == canonical_pkg_path {
        return None;
    }

    if canonical_pkg_path.is_empty() {
        return Some(requested_path.to_string());
    }

    requested_path
        .strip_prefix(canonical_pkg_path)
        .and_then(|rest| rest.strip_prefix('/'))
        .map(str::to_string)
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

    let package_root =
        pcb_zen::ensure_sparse_checkout(&cache_dir, module_path, version, true, None)
            .with_context(|| format!("Failed to fetch {module_path}@{version}"))?;

    if list {
        return list_package_files(display_name, &package_root, filter);
    }
    run_docgen(&package_root, Some(module_path), filter)
}

/// Get the package URL for a local directory using workspace info
fn get_local_package_url(dir: &std::path::Path) -> Option<String> {
    let canonical = dir.canonicalize().ok()?;
    let file_provider = pcb_zen_core::DefaultFileProvider::new();
    let workspace_info = pcb_zen::get_workspace_info(&file_provider, &canonical, true).ok()?;
    let repo = workspace_info.repository()?;

    let relative = canonical.strip_prefix(&workspace_info.root).ok()?;
    let relative_str = relative.to_string_lossy().replace('\\', "/");

    if relative_str.is_empty() {
        Some(repo.to_string())
    } else {
        Some(format!("{}/{}", repo, relative_str))
    }
}

/// Normalize a path and filter: if path is a file, return parent dir and adjusted filter.
fn normalize_path_filter(path: &Path, filter: Option<&str>) -> Result<(PathBuf, Option<String>)> {
    if !path.exists() {
        anyhow::bail!("'{}' does not exist.", path.display());
    }
    if path.is_file() {
        let parent = path.parent().unwrap_or(path);
        let name = path.file_name().unwrap().to_string_lossy();
        let filter = filter.map_or_else(|| name.to_string(), |f| format!("{}/{}", f, name));
        Ok((parent.to_path_buf(), Some(filter)))
    } else {
        Ok((path.to_path_buf(), filter.map(String::from)))
    }
}

fn run_docgen(path: &Path, package_url: Option<&str>, filter: Option<&str>) -> Result<()> {
    let (dir, filter) = normalize_path_filter(path, filter)?;

    let display_path = get_display_path(&dir);
    let result = pcb_docgen::generate_docs(
        &dir,
        package_url,
        display_path.as_deref(),
        filter.as_deref(),
    )?;

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
        print_highlighted_markdown(&result.markdown);
    } else {
        println!("{}", result.markdown);
    }

    Ok(())
}

/// List .zen files in a package as a tree structure.
fn list_package_files(display_name: &str, path: &Path, filter: Option<&str>) -> Result<()> {
    use std::collections::BTreeMap;
    use walkdir::WalkDir;

    let (dir, filter) = normalize_path_filter(path, filter)?;
    let canonical = dir.canonicalize().unwrap_or(dir);

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
            if let Some(ref f) = filter
                && !rel_str.starts_with(f)
                && rel_str != *f
            {
                return None;
            }
            Some(rel_str)
        })
        .collect();

    files.sort();

    if files.is_empty() {
        let filter_msg = filter
            .as_ref()
            .map(|f| format!(" matching '{}'", f))
            .unwrap_or_default();
        anyhow::bail!(
            "No .zen files found{} under '{}'.",
            filter_msg,
            canonical.display()
        );
    }

    // Build a hierarchical directory tree from the file paths
    #[derive(Default)]
    struct DirTree {
        subdirs: BTreeMap<String, DirTree>,
        files: Vec<String>,
    }

    impl DirTree {
        fn insert(&mut self, path: &str) {
            let mut parts = path.split('/').peekable();
            let mut current = self;

            while let Some(part) = parts.next() {
                if parts.peek().is_some() {
                    current = current.subdirs.entry(part.to_string()).or_default();
                } else {
                    current.files.push(part.to_string());
                }
            }
        }
    }

    #[derive(Clone)]
    enum Node {
        Dir { name: String, children: Vec<Node> },
        File(String),
    }

    fn build_dir_node(name: String, tree: DirTree) -> Node {
        let mut children = Vec::new();
        for (subdir_name, subdir_tree) in tree.subdirs {
            children.push(build_dir_node(subdir_name, subdir_tree));
        }
        let mut file_names = tree.files;
        file_names.sort();
        for file in file_names {
            children.push(Node::File(file));
        }
        Node::Dir { name, children }
    }

    fn build_nodes(tree: DirTree) -> Vec<Node> {
        let mut nodes = Vec::new();
        for (dir_name, subdir_tree) in tree.subdirs {
            nodes.push(build_dir_node(dir_name, subdir_tree));
        }
        let mut root_files = tree.files;
        root_files.sort();
        for file in root_files {
            nodes.push(Node::File(file));
        }
        nodes
    }

    let mut tree = DirTree::default();
    for file in &files {
        tree.insert(file);
    }

    let roots = build_nodes(tree);

    pcb_zen::tree::print_tree(display_name.to_string(), roots, |node| match node {
        Node::Dir { name, children } => (format!("{}/", name), children.clone()),
        Node::File(name) => (name.clone(), vec![]),
    })?;

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
                if s.is_empty() { None } else { Some(s) }
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

/// Print markdown with syntax-highlighted code blocks
fn print_highlighted_markdown(content: &str) {
    let ps = SyntaxSet::load_defaults_newlines();
    let ts = ThemeSet::load_defaults();
    let theme = &ts.themes["base16-mocha.dark"];
    let skin = make_skin();

    let mut stdout = io::stdout().lock();
    let mut in_code_block = false;
    let mut code_lang = String::new();
    let mut code_buffer = String::new();
    let mut text_buffer = String::new();

    for line in content.lines() {
        if line.starts_with("```") {
            if in_code_block {
                // End of code block - highlight and print the accumulated code
                let syntax = ps
                    .find_syntax_by_token(&code_lang)
                    .unwrap_or_else(|| ps.find_syntax_plain_text());
                let mut h = HighlightLines::new(syntax, theme);

                for code_line in LinesWithEndings::from(&code_buffer) {
                    if let Ok(ranges) = h.highlight_line(code_line, &ps) {
                        let escaped = as_24_bit_terminal_escaped(&ranges[..], false);
                        let _ = write!(stdout, "{}", escaped);
                    }
                }
                let _ = write!(stdout, "\x1b[0m");

                code_buffer.clear();
                in_code_block = false;
            } else {
                // Start of code block - first flush any pending text
                if !text_buffer.is_empty() {
                    skin.write_text_on(&mut stdout, &text_buffer).ok();
                    text_buffer.clear();
                }

                // Extract language hint
                code_lang = line.trim_start_matches('`').trim().to_string();
                // Map common language names
                if code_lang == "python" || code_lang == "starlark" || code_lang == "zen" {
                    code_lang = "Python".to_string();
                } else if code_lang == "toml" {
                    code_lang = "TOML".to_string();
                } else if code_lang == "rust" {
                    code_lang = "Rust".to_string();
                }
                in_code_block = true;
            }
        } else if in_code_block {
            code_buffer.push_str(line);
            code_buffer.push('\n');
        } else {
            text_buffer.push_str(line);
            text_buffer.push('\n');
        }
    }

    // Flush remaining text
    if !text_buffer.is_empty() {
        skin.write_text_on(&mut stdout, &text_buffer).ok();
    }
    let _ = stdout.flush();
}

fn make_skin() -> MadSkin {
    use termimad::crossterm::style::{Attribute, Color::Rgb};

    let mut skin = MadSkin::default();

    // Gruvbox Dark palette
    let bright_orange = Rgb {
        r: 254,
        g: 128,
        b: 25,
    }; // #fe8019
    let bright_yellow = Rgb {
        r: 250,
        g: 189,
        b: 47,
    }; // #fabd2f
    let bright_green = Rgb {
        r: 184,
        g: 187,
        b: 38,
    }; // #b8bb26
    let bright_aqua = Rgb {
        r: 142,
        g: 192,
        b: 124,
    }; // #8ec07c
    let bright_blue = Rgb {
        r: 131,
        g: 165,
        b: 152,
    }; // #83a598
    let bright_purple = Rgb {
        r: 211,
        g: 134,
        b: 155,
    }; // #d3869b
    let fg3 = Rgb {
        r: 189,
        g: 174,
        b: 147,
    }; // #bdae93
    let bg1 = Rgb {
        r: 60,
        g: 56,
        b: 54,
    }; // #3c3836

    // Headers
    skin.headers[0].set_fg(bright_orange);
    skin.headers[0].add_attr(Attribute::Bold);
    skin.headers[1].set_fg(bright_yellow);
    skin.headers[1].add_attr(Attribute::Bold);
    skin.headers[2].set_fg(bright_aqua);
    skin.headers[3].set_fg(bright_blue);

    // Bold and italic
    skin.bold.set_fg(bright_orange);
    skin.italic.set_fg(fg3);
    skin.italic.add_attr(Attribute::Italic);

    // Code
    skin.code_block.set_bg(bg1);
    skin.code_block.set_fg(bright_green);
    skin.inline_code.set_fg(bright_yellow);

    // Bullet points
    skin.bullet.set_fg(bright_aqua);

    // Quote marks
    skin.quote_mark.set_fg(bright_purple);

    skin
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Debug, Parser)]
    struct TestDocCli {
        #[command(flatten)]
        args: DocArgs,
    }

    #[test]
    fn parse_remote_package_spec_treats_missing_and_latest_as_latest() {
        assert_eq!(
            parse_remote_package_spec("github.com/acme/components/SimpleResistor").unwrap(),
            ("github.com/acme/components/SimpleResistor", None)
        );
        assert_eq!(
            parse_remote_package_spec("github.com/acme/components/SimpleResistor@latest").unwrap(),
            ("github.com/acme/components/SimpleResistor", None)
        );
    }

    #[test]
    fn parse_remote_package_spec_normalizes_semver_suffix() {
        assert_eq!(
            parse_remote_package_spec("github.com/acme/components/SimpleResistor@v1.2.3").unwrap(),
            (
                "github.com/acme/components/SimpleResistor",
                Some(Version::new(1, 2, 3))
            )
        );
    }

    #[test]
    fn resolve_remote_package_with_versions_defaults_to_latest_and_preserves_filter() {
        let all_versions = BTreeMap::from([(
            "SimpleResistor".to_string(),
            vec![Version::new(1, 0, 0), Version::new(2, 0, 0)],
        )]);
        let (module_path, version, filter) = resolve_remote_package_from_versions(
            "github.com/acme/components/SimpleResistor/SimpleResistor.zen",
            None,
            &all_versions,
        )
        .unwrap();

        assert_eq!(module_path, "github.com/acme/components/SimpleResistor");
        assert_eq!(version, "2.0.0");
        assert_eq!(filter.as_deref(), Some("SimpleResistor.zen"));
    }

    #[test]
    fn resolve_remote_package_with_versions_rejects_unknown_version() {
        let all_versions = BTreeMap::from([(
            "SimpleResistor".to_string(),
            vec![Version::new(1, 0, 0), Version::new(2, 0, 0)],
        )]);
        let err = resolve_remote_package_from_versions(
            "github.com/acme/components/SimpleResistor",
            Some(&Version::new(3, 0, 0)),
            &all_versions,
        )
        .unwrap_err();

        assert!(err.to_string().contains("Version 3.0.0 not found"));
    }

    #[test]
    fn resolve_remote_package_falls_back_to_root_tagged_repo() {
        let all_versions = BTreeMap::from([(
            "".to_string(),
            vec![Version::new(1, 0, 0), Version::new(2, 0, 0)],
        )]);
        let (module_path, version, filter) = resolve_remote_package_from_versions(
            "github.com/acme/repo/file.zen",
            None,
            &all_versions,
        )
        .unwrap();

        assert_eq!(module_path, "github.com/acme/repo");
        assert_eq!(version, "2.0.0");
        assert_eq!(filter.as_deref(), Some("file.zen"));
    }
}
