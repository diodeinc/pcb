use anyhow::{Context, Result};
use clap::Args;
use crossterm::event::{self, Event, KeyCode, KeyEvent};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use pcb_ui::{Colorize, Spinner};
use pcb_zen::git;
use pcb_zen_core::config::get_workspace_info;
use pcb_zen_core::DefaultFileProvider;
use semver::Version;
use std::io::{self, Write};
use std::path::Path;

use crate::release;

#[derive(Args, Debug)]
#[command(about = "Create and manage PCB version tags")]
pub struct TagArgs {
    /// Board name (optional, uses default board if not specified)
    #[arg(short = 'b', long)]
    pub board: String,

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

    /// Exclude specific manufacturing artifacts from the release validation (can be specified multiple times)
    #[arg(long, value_enum)]
    pub exclude: Vec<release::ArtifactType>,

    /// Suppress diagnostics by kind or severity during validation
    #[arg(short = 'S', long = "suppress", value_name = "KIND")]
    pub suppress: Vec<String>,
}

/// Information gathered for tag operation
pub struct TagInfo {
    /// Workspace root path
    pub workspace_root: std::path::PathBuf,
    /// Board name being tagged
    pub board_name: String,
    /// Parsed semantic version
    pub version: Version,
    /// Generated tag name (board_name/v{version})
    pub tag_name: String,
    /// Whether to push the tag
    pub push: bool,
    /// Whether to skip confirmation prompts
    pub force: bool,
    /// Original path argument for workspace discovery
    pub discovery_path: Option<String>,
    /// Artifacts to exclude from release validation
    pub exclude: Vec<release::ArtifactType>,
    /// Diagnostics to suppress during validation
    pub suppress: Vec<String>,
}

type TaskFn = fn(&TagInfo) -> Result<()>;

const TAG_TASKS: &[(&str, TaskFn)] = &[
    ("Checking tag doesn't exist", check_tag_not_exists),
    ("Running full release build and validation", run_release),
    ("Creating git tag", create_tag),
];

const PUSH_TASKS: &[(&str, TaskFn)] = &[("Pushing tag to remote", push_tag)];

/// Execute a list of tasks with proper error handling and UI feedback
fn execute_tasks(info: &TagInfo, tasks: &[(&str, TaskFn)]) -> Result<()> {
    for (name, task) in tasks {
        // Special handling for release task - don't show spinner, just run it
        if *name == "Running full release build and validation" {
            task(info)?;
        } else {
            let spinner = Spinner::builder(*name).start();
            let res = task(info);
            spinner.finish();
            match res {
                Ok(()) => eprintln!("{} {name}", "✓".green()),
                Err(e) => {
                    eprintln!("{} {name} failed", "✗".red());
                    return Err(e.context(format!("{name} failed")));
                }
            }
        }
    }
    Ok(())
}

pub fn execute(args: TagArgs) -> Result<()> {
    // Gather tag information
    let tag_info = {
        let info_spinner = Spinner::builder("Gathering tag information").start();
        let start_path = args.path.as_deref().unwrap_or(".");
        let workspace_info =
            get_workspace_info(&DefaultFileProvider::new(), Path::new(start_path))?;
        let version = Version::parse(&args.version)
            .map_err(|_| anyhow::anyhow!("Invalid semantic version: '{}'", args.version))?;
        let tag_name = format!("{}/v{version}", args.board);

        let info = TagInfo {
            workspace_root: workspace_info.root,
            board_name: args.board,
            version,
            tag_name,
            push: args.push,
            force: args.force,
            discovery_path: args.path.clone(),
            exclude: args.exclude,
            suppress: args.suppress,
        };
        info_spinner.finish();
        info
    };

    // Execute tagging tasks
    execute_tasks(&tag_info, TAG_TASKS)?;

    // Execute push tasks if requested
    if tag_info.push {
        let should_push = tag_info.force || confirm_push(&tag_info)?;
        if should_push {
            execute_tasks(&tag_info, PUSH_TASKS)?;
        } else {
            eprintln!("Tag push cancelled");
        }
    }

    eprintln!(
        "{} {}",
        "✓".green().bold(),
        format!("Tag {} created successfully", tag_info.tag_name).bold()
    );

    Ok(())
}

/// Check that the tag doesn't already exist
fn check_tag_not_exists(info: &TagInfo) -> Result<()> {
    if git::tag_exists(&info.workspace_root, &info.tag_name) {
        anyhow::bail!("Tag '{}' already exists", info.tag_name);
    }
    Ok(())
}

/// Run full pcb release to validate board (builds, tests, generates artifacts)
fn run_release(info: &TagInfo) -> Result<()> {
    // Skip release validation if --force is set
    if info.force {
        return Ok(());
    }

    // Create ReleaseArgs with default settings for source-only release
    let release_args = release::ReleaseArgs {
        board: Some(info.board_name.clone()),
        file: None,
        path: info.discovery_path.clone(), // Use same path as tag command
        format: release::ReleaseOutputFormat::None, // No summary output
        source_only: false,                // Don't need manufacturing artifacts for tagging
        output_dir: None,                  // Use default
        output_name: None,                 // Use default
        exclude: info.exclude.clone(),     // Pass through exclude list from tag command
        yes: false,                        // Prompt for warnings
        suppress: info.suppress.clone(),   // Pass through suppress list from tag command
    };

    // Run the full release process - this validates everything
    release::execute(release_args).context("Release validation failed")?;

    Ok(())
}

/// Create the git tag
fn create_tag(info: &TagInfo) -> Result<()> {
    git::create_tag(
        &info.workspace_root,
        &info.tag_name,
        &format!("Release {} version {}", info.board_name, info.version),
    )
    .context("Failed to create git tag")
}

/// Push the tag to remote
fn push_tag(info: &TagInfo) -> Result<()> {
    git::push_tag(&info.workspace_root, &info.tag_name, "origin").context("Failed to push tag")
}

/// Confirm push with user interaction
fn confirm_push(info: &TagInfo) -> Result<bool> {
    let remote = git::get_remote_url(&info.workspace_root).unwrap_or_else(|_| "origin".to_string());

    print!(
        "Push tag {} to {}? (y/N): ",
        info.tag_name.bold().yellow(),
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

    Ok(input.eq_ignore_ascii_case(&'y'))
}
