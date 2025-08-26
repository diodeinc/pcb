use anyhow::{Context, Result};
use clap::Args;
use crossterm::event::{self, Event, KeyCode, KeyEvent};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use pcb_ui::{Colorize, Style, StyledText};
use pcb_zen::git;
use pcb_zen_core::config::get_workspace_info;
use pcb_zen_core::DefaultFileProvider;
use semver::Version;
use std::io::{self, Write};
use std::path::Path;

#[derive(Args, Debug)]
#[command(about = "Create and manage PCB version tags")]
pub struct TagArgs {
    /// Board name (optional, uses default board if not specified)
    #[arg(short = 'b', long)]
    pub board: Option<String>,

    /// Version to tag (must be valid semantic version)
    #[arg(short = 'v', long, required = true)]
    pub version: String,

    /// Push tag to remote repository
    #[arg(long)]
    pub push: bool,

    /// Skip confirmation prompts
    #[arg(short = 'f', long)]
    pub force: bool,

    /// Optional path to start discovery from (defaults to current directory)
    pub path: Option<String>,
}

pub fn execute(args: TagArgs) -> Result<()> {
    let start_path = args.path.as_deref().unwrap_or(".");
    let workspace_info = get_workspace_info(&DefaultFileProvider, Path::new(start_path))?;

    let board_name = match args.board {
        Some(name) => {
            if !workspace_info.boards.iter().any(|b| b.name == name) {
                let available: Vec<_> = workspace_info
                    .boards
                    .iter()
                    .map(|b| b.name.as_str())
                    .collect();
                anyhow::bail!(
                    "Board '{name}' not found. Available: [{}]",
                    available.join(", ")
                );
            }
            name
        }
        None => workspace_info
            .config
            .as_ref()
            .and_then(|c| c.default_board.clone())
            .context(format!(
                "No default board found in workspace {}",
                workspace_info.root.display()
            ))?,
    };

    let version = Version::parse(&args.version)
        .map_err(|_| anyhow::anyhow!("Invalid semantic version: '{}'", args.version))?;

    let tag_name = format!("{board_name}/v{version}");
    if git::tag_exists(&workspace_info.root, &tag_name) {
        anyhow::bail!("Tag '{tag_name}' already exists");
    }

    git::create_tag(
        &workspace_info.root,
        &tag_name,
        &format!("Release {board_name} version {version}"),
    )
    .context("Failed to create git tag")?;

    println!(
        "{} Created tag: {}",
        "✓".with_style(Style::Green),
        tag_name.bold().green()
    );

    if args.push {
        let should_push = args.force || {
            let remote =
                git::get_remote_url(&workspace_info.root).unwrap_or_else(|_| "origin".to_string());
            print!(
                "Push tag {} to {}? (y/N): ",
                tag_name.bold().yellow(),
                remote
            );
            io::stdout().flush()?;

            enable_raw_mode()?;
            let input = loop {
                if let Event::Key(KeyEvent { code, .. }) = event::read()? {
                    match code {
                        KeyCode::Char(c) => break c,
                        KeyCode::Esc => break 'n',
                        _ => continue,
                    }
                }
            };
            disable_raw_mode()?;
            println!("{input}");
            input.eq_ignore_ascii_case(&'y')
        };

        if should_push {
            git::push_tag(&workspace_info.root, &tag_name).context("Failed to push tag")?;
            println!(
                "{} Pushed tag {}",
                "✓".with_style(Style::Green),
                tag_name.bold().green()
            );
        } else {
            println!("Tag push cancelled");
        }
    }

    Ok(())
}
