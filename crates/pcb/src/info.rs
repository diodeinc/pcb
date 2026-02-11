use anyhow::Result;
use clap::Args;
use colored::Colorize as ColoredExt;
use pcb_ui::{Style, StyledText};
use pcb_zen::workspace::{get_workspace_info, MemberPackage, WorkspaceInfo, WorkspaceInfoExt};
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

    /// Show dependency tree
    #[arg(long)]
    pub tree: bool,

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

pub fn execute(args: InfoArgs) -> Result<()> {
    let start_path = match &args.path {
        Some(path) => Path::new(path).to_path_buf(),
        None => env::current_dir()?,
    };

    let file_provider = DefaultFileProvider::new();
    let mut workspace_info = get_workspace_info(&file_provider, &start_path)?;

    // Populate dirty status for all packages (used by both human and JSON output)
    workspace_info.populate_dirty();

    match args.format {
        OutputFormat::Human => {
            print_human_readable(&workspace_info);
        }
        OutputFormat::Json => print_json(&workspace_info)?,
    }

    // Print dependency tree if requested
    if args.tree {
        println!();
        println!("{}", "Dependencies".with_style(Style::Blue).bold());
        let result = pcb_zen::resolve_dependencies(&mut workspace_info, false, false)?;
        pcb_zen::print_dep_tree(&result);
    }

    Ok(())
}

fn print_human_readable(ws: &WorkspaceInfo) {
    // Header
    println!("{}", "Workspace".with_style(Style::Blue).bold());
    println!("Root: {}", ws.root.display());

    if let Some(repo) = ws.repository() {
        println!("Repository: {}", repo.with_style(Style::Cyan));
    }
    if let Some(pcb_version) = ws.pcb_version() {
        println!("Toolchain: pcb >= {}", pcb_version);
    }

    // Member patterns (if not default)
    let member_patterns = ws.member_patterns();
    if !member_patterns.is_empty() && member_patterns != vec!["boards/*".to_string()] {
        println!("Members: {}", member_patterns.join(", "));
    }

    println!();

    // Separate boards from other packages
    let all_packages = ws.all_packages();
    let (mut boards, mut other_packages): (Vec<_>, Vec<_>) = all_packages
        .into_iter()
        .partition(|p| p.config.board.is_some());

    // Sort by relative path
    boards.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    other_packages.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));

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
            if let Some(board) = &pkg.config.board {
                // board.path is already populated by populate_board_zen_paths()
                let zen_path = board
                    .path
                    .as_ref()
                    .map(|p| {
                        // Make path relative to workspace root
                        let pkg_rel = pkg.rel_path.to_string_lossy();
                        if pkg_rel.is_empty() {
                            p.clone()
                        } else {
                            format!("{}/{}", pkg_rel, p)
                        }
                    })
                    .unwrap_or_else(|| "(no .zen file found)".to_string());

                // Use package version (which is board version for board packages)
                let version_str = format_version(&pkg.version, false);

                println!("  {} {} - {}", board.name.bold(), version_str, zen_path);

                if !board.description.is_empty() {
                    println!("    {}", board.description);
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
            print_package_line(pkg);
        }
    }
}

fn print_package_line(pkg: &MemberPackage) {
    let is_root = pkg.rel_path.as_os_str().is_empty();

    // Package name (last segment of relative path, or "root")
    let name = if is_root {
        "root".to_string()
    } else {
        pkg.rel_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| pkg.rel_path.to_string_lossy().to_string())
    };

    let is_dirty = pkg.dirty;
    let version_str = format_version(&pkg.version, is_dirty);

    // Relative path from workspace root
    let rel_path = pkg.rel_path.to_string_lossy().to_string();

    // Deps/assets suffix
    let dep_count = pkg.dependencies().count();
    let mut extras = Vec::new();
    if dep_count > 0 {
        extras.push(format!("{} deps", dep_count));
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

fn print_json<T: Serialize>(info: &T) -> Result<()> {
    let json = serde_json::to_string_pretty(info)?;
    println!("{json}");
    Ok(())
}

/// Format version string with dirty indicator
fn format_version(version: &Option<String>, dirty: bool) -> String {
    match (version, dirty) {
        (Some(v), true) => format!("{}{}", format!("(v{})", v).green(), "*".red()),
        (Some(v), false) => format!("(v{})", v).green().to_string(),
        (None, _) => "(unpublished)".yellow().to_string(),
    }
}
