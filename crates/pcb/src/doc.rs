use anyhow::{Context, Result};
use clap::Args;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;
use syntect::util::{LinesWithEndings, as_24_bit_terminal_escaped};
use termimad::MadSkin;

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

    /// Show the changelog (release notes)
    #[arg(long)]
    pub changelog: bool,

    /// Show only the latest release notes (requires --changelog)
    #[arg(long, requires = "changelog")]
    pub latest: bool,
}

// Include the generated changelog constants
include!(concat!(env!("OUT_DIR"), "/changelog.rs"));

pub fn execute(args: DocArgs) -> Result<()> {
    // --changelog flag: show embedded changelog
    if args.changelog {
        if args.latest {
            print_latest_release_notes();
            return Ok(());
        }
        return render_changelog();
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
             \x20 pcb doc --package @stdlib     # Generate stdlib docs\n\
             \x20 pcb doc --changelog           # Show changelog"
        );
    }

    // Show embedded static docs
    render_embedded_docs(&args.path, args.list)
}

fn render_changelog() -> Result<()> {
    if io::stdout().is_terminal() {
        print_highlighted_markdown(CHANGELOG_MD);
    } else {
        println!("{}", CHANGELOG_MD);
    }
    Ok(())
}

/// Render just the latest release notes (used by self-update)
pub fn print_latest_release_notes() {
    if io::stdout().is_terminal() {
        print_highlighted_markdown(LATEST_RELEASE_NOTES);
    } else {
        println!("{}", LATEST_RELEASE_NOTES);
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

    let package_root =
        pcb_zen::ensure_sparse_checkout(&cache_dir, module_path, version, true, None)
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
