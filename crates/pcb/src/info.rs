use anyhow::Result;
use clap::Args;
use colored::Colorize as ColoredExt;
use pcb_ui::{Style, StyledText};
use pcb_zen::workspace::{detect_v2_workspace, PackageInfo, V2Workspace};
use pcb_zen_core::config::get_workspace_info;
use pcb_zen_core::DefaultFileProvider;
use serde::Serialize;
use std::env;
use std::path::Path;

#[derive(Args, Debug)]
#[command(about = "Display workspace and board information")]
pub struct InfoArgs {
    /// Output format
    #[arg(short = 'f', long, value_enum, default_value = "human")]
    pub format: OutputFormat,

    /// Optional path to start discovery from (defaults to current directory)
    pub path: Option<String>,
}

#[derive(Debug, Clone, clap::ValueEnum)]
pub enum OutputFormat {
    /// Human-readable output
    Human,
    /// JSON output
    Json,
}

/// Combined workspace info for JSON output (supports both V1 and V2)
#[derive(Debug, Serialize)]
#[serde(untagged)]
enum WorkspaceInfoOutput {
    V1(pcb_zen_core::config::WorkspaceInfo),
    V2(V2Workspace),
}

pub fn execute(args: InfoArgs) -> Result<()> {
    let start_path = match &args.path {
        Some(path) => Path::new(path).to_path_buf(),
        None => env::current_dir()?,
    };

    // Try V2 first
    if let Some(v2_workspace) = detect_v2_workspace(&start_path)? {
        match args.format {
            OutputFormat::Human => print_v2_human_readable(&v2_workspace),
            OutputFormat::Json => print_json(&WorkspaceInfoOutput::V2(v2_workspace))?,
        }
        return Ok(());
    }

    // Fall back to V1
    let file_provider = DefaultFileProvider::new();
    let workspace_info = get_workspace_info(&file_provider, &start_path)?;

    match args.format {
        OutputFormat::Human => print_v1_human_readable(&workspace_info),
        OutputFormat::Json => print_json(&WorkspaceInfoOutput::V1(workspace_info))?,
    }

    Ok(())
}

fn print_v2_human_readable(ws: &V2Workspace) {
    // Header
    println!("{}", "Workspace".with_style(Style::Blue).bold());
    println!("Root: {}", ws.root.display());

    if let Some(repo) = &ws.repository {
        println!("Repository: {}", repo.with_style(Style::Cyan));
    }
    if let Some(pcb_version) = &ws.pcb_version {
        println!("Toolchain: pcb >= {}", pcb_version);
    }

    // Member patterns (if not default)
    if !ws.member_patterns.is_empty() && ws.member_patterns != vec!["boards/*".to_string()] {
        println!("Members: {}", ws.member_patterns.join(", "));
    }

    println!();

    // Separate boards from other packages
    let all_packages = ws.all_packages();
    let (mut boards, mut other_packages): (Vec<_>, Vec<_>) =
        all_packages.into_iter().partition(|p| p.board.is_some());

    // Sort by relative path
    let sort_by_path = |a: &&PackageInfo, b: &&PackageInfo| {
        let a_rel = a.path.strip_prefix(&ws.root).unwrap_or(&a.path);
        let b_rel = b.path.strip_prefix(&ws.root).unwrap_or(&b.path);
        a_rel.cmp(b_rel)
    };
    boards.sort_by(sort_by_path);
    other_packages.sort_by(sort_by_path);

    // Boards section (like V1)
    if boards.is_empty() {
        println!("No boards discovered");
    } else {
        println!(
            "{} ({})",
            "Boards".with_style(Style::Blue).bold(),
            boards.len()
        );

        for pkg in &boards {
            if let Some(board) = &pkg.board {
                let zen_path = board
                    .zen_path
                    .as_ref()
                    .map(|p| {
                        // Make path relative to workspace root
                        let pkg_rel = pkg
                            .path
                            .strip_prefix(&ws.root)
                            .map(|p| p.to_string_lossy().to_string())
                            .unwrap_or_default();
                        if pkg_rel.is_empty() {
                            p.clone()
                        } else {
                            format!("{}/{}", pkg_rel, p)
                        }
                    })
                    .unwrap_or_else(|| "?.zen".to_string());

                let version_str = format_version(&board.version, board.dirty, pkg.transitive_dirty);

                println!("  {} {} - {}", board.name.bold(), version_str, zen_path);

                if let Some(desc) = &board.description {
                    println!("    {}", desc);
                }
            }
        }
    }

    // Packages section (non-boards)
    if !other_packages.is_empty() {
        println!();
        println!(
            "{} ({})",
            "Packages".with_style(Style::Blue).bold(),
            other_packages.len()
        );

        for pkg in &other_packages {
            print_package_line(pkg, ws);
        }
    }
}

fn print_package_line(pkg: &PackageInfo, ws: &V2Workspace) {
    let is_root = ws
        .root_package
        .as_ref()
        .map(|r| r.url == pkg.url)
        .unwrap_or(false);

    // Package name (last segment of relative path, or "root")
    let name = if is_root {
        "root".to_string()
    } else {
        pkg.path
            .strip_prefix(&ws.root)
            .ok()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| {
                pkg.url
                    .split('/')
                    .next_back()
                    .unwrap_or(&pkg.url)
                    .to_string()
            })
    };

    let version_str = format_version(&pkg.latest_version, pkg.dirty, pkg.transitive_dirty);

    // Relative path from workspace root
    let rel_path = pkg
        .path
        .strip_prefix(&ws.root)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    // Deps/assets suffix
    let mut extras = Vec::new();
    if !pkg.dependencies.is_empty() {
        extras.push(format!("{} deps", pkg.dependencies.len()));
    }
    if pkg.asset_count > 0 {
        extras.push(format!("{} assets", pkg.asset_count));
    }
    let extras_str = if extras.is_empty() {
        String::new()
    } else {
        format!(" ({})", extras.join(", ")).dimmed().to_string()
    };

    // Root indicator
    let root_str = if is_root {
        " (workspace root)".cyan().to_string()
    } else {
        String::new()
    };

    // Path display
    let path_str = if rel_path.is_empty() || is_root {
        String::new()
    } else {
        format!(" {}", rel_path.dimmed())
    };

    println!(
        "  {} {}{}{}{}",
        name.bold(),
        version_str,
        root_str,
        path_str,
        extras_str
    );
}

fn print_v1_human_readable(info: &pcb_zen_core::config::WorkspaceInfo) {
    println!("{}", "Workspace".with_style(Style::Blue).bold());
    println!("Root: {}", info.root.display());

    if let Some(name) = &info.config.name {
        println!("Name: {name}");
    }
    // Only show members if not default value
    if info.config.members != vec!["boards/*".to_string()] {
        println!("Members: {}", info.config.members.join(", "));
    }

    // Display errors if any
    if !info.errors.is_empty() {
        println!();
        println!("{}", "Discovery Errors:".with_style(Style::Red));
        for error in &info.errors {
            println!("  {}: {}", error.path.display(), error.error);
        }
    }

    println!();

    if info.boards.is_empty() {
        println!("No boards discovered");
        println!("Searched for pcb.toml files with [board] sections");
        // Only show members if not default value
        if info.config.members != vec!["boards/*".to_string()] {
            println!("Members: {}", info.config.members.join(", "));
        }
    } else {
        // Get default board for marking
        let default_board = info.config.default_board.as_ref();

        println!(
            "{} ({})",
            "Boards".with_style(Style::Blue).bold(),
            info.boards.len()
        );

        for board in &info.boards {
            let name_display = if default_board.map(|s| s.as_str()) == Some(board.name.as_str()) {
                format!(
                    "{} {}",
                    board.name.as_str().bold().green(),
                    "(default)".with_style(Style::Yellow)
                )
            } else {
                board.name.as_str().bold().green().to_string()
            };

            println!("  {} - {}", name_display, board.zen_path);
            if !board.description.is_empty() {
                println!("    {}", board.description);
            }
        }
    }
}

fn print_json<T: Serialize>(info: &T) -> Result<()> {
    let json = serde_json::to_string_pretty(info)?;
    println!("{json}");
    Ok(())
}

/// Format version string with dirty indicators
/// Red * = directly dirty, Yellow * = transitive dirty
fn format_version(version: &Option<String>, dirty: bool, transitive_dirty: bool) -> String {
    match (version, dirty, transitive_dirty) {
        (Some(v), true, _) => format!("{}{}", format!("(v{})", v).green(), "*".red()),
        (Some(v), false, true) => format!("{}{}", format!("(v{})", v).green(), "*".yellow()),
        (Some(v), false, false) => format!("(v{})", v).green().to_string(),
        (None, _, _) => "(unpublished)".yellow().to_string(),
    }
}
