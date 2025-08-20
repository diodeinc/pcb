use anyhow::Result;
use clap::Args;
use pcb_ui::{Colorize, Style, StyledText};
use pcb_zen_core::config::get_workspace_info;
use pcb_zen_core::DefaultFileProvider;
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

pub fn execute(args: InfoArgs) -> Result<()> {
    let start_path = match &args.path {
        Some(path) => Path::new(path).to_path_buf(),
        None => env::current_dir()?,
    };

    let file_provider = DefaultFileProvider;
    let workspace_info = get_workspace_info(&file_provider, &start_path)?;

    match args.format {
        OutputFormat::Human => print_human_readable(&workspace_info),
        OutputFormat::Json => print_json(&workspace_info)?,
    }

    Ok(())
}

fn print_human_readable(info: &pcb_zen_core::config::WorkspaceInfo) {
    println!("{}", "Workspace Information".with_style(Style::Blue));
    println!("Root: {}", info.root.display());

    if let Some(config) = &info.config {
        if let Some(name) = &config.name {
            println!("Name: {name}");
        }
        // Only show members if not default value
        if config.members != vec!["boards/*".to_string()] {
            println!("Members: {}", config.members.join(", "));
        }
    } else {
        println!("No workspace configuration found");
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
        if let Some(config) = &info.config {
            // Only show members if not default value
            if config.members != vec!["boards/*".to_string()] {
                println!("Members: {}", config.members.join(", "));
            }
        }
    } else {
        // Get default board for marking
        let default_board = info.config.as_ref().and_then(|c| c.default_board.as_ref());

        println!(
            "{} ({})",
            "Boards".with_style(Style::Blue),
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

            if board.description.is_empty() {
                println!(
                    "  {} - {} in {}",
                    name_display,
                    board.zen_path,
                    board.directory.display()
                );
            } else {
                println!(
                    "  {} - {} in {}",
                    name_display,
                    board.zen_path,
                    board.directory.display()
                );
                println!("    {}", board.description);
            }
        }
    }
}

fn print_json(info: &pcb_zen_core::config::WorkspaceInfo) -> Result<()> {
    let json = serde_json::to_string_pretty(info)?;
    println!("{json}");
    Ok(())
}
