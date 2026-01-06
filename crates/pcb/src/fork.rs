use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use colored::Colorize;
use pcb_ui::{Style, StyledText};
use pcb_zen::fork::{fork_package, upstream_forks, ForkOptions};
use pcb_zen::get_workspace_info;
use pcb_zen::git::has_uncommitted_changes_in_path;
use pcb_zen_core::config::PcbToml;
use pcb_zen_core::DefaultFileProvider;
use std::fs;
use std::path::Path;

#[derive(Args)]
pub struct ForkArgs {
    #[command(subcommand)]
    pub command: ForkCommands,
}

#[derive(Subcommand)]
pub enum ForkCommands {
    /// Fork a package for local development
    Add(AddArgs),

    /// Remove a fork and revert to remote dependency
    Remove(RemoveArgs),

    /// Push all forked packages to a branch on the upstream registry
    Upstream(UpstreamArgs),
}

#[derive(Args)]
pub struct AddArgs {
    /// Fully-qualified module URL (e.g., github.com/diodeinc/registry/modules/UsbPdController)
    #[arg(value_name = "URL")]
    pub url: String,

    /// Specific version to fork (default: latest tagged version)
    #[arg(long)]
    pub version: Option<String>,

    /// Force overwrite if fork directory already exists
    #[arg(long)]
    pub force: bool,
}

#[derive(Args)]
pub struct RemoveArgs {
    /// Fully-qualified module URL to unfork (e.g., github.com/diodeinc/registry/modules/UsbPdController)
    #[arg(value_name = "URL")]
    pub url: Option<String>,

    /// Unfork all packages with path patches
    #[arg(long)]
    pub all: bool,

    /// Force deletion of fork directories even if they have uncommitted changes
    #[arg(long)]
    pub force: bool,
}

#[derive(Args)]
pub struct UpstreamArgs {
    /// Dry run - show what would be done without making changes
    #[arg(long)]
    pub dry_run: bool,
}

pub fn execute(args: ForkArgs) -> Result<()> {
    match args.command {
        ForkCommands::Add(add_args) => execute_add(add_args),
        ForkCommands::Remove(remove_args) => execute_remove(remove_args),
        ForkCommands::Upstream(upstream_args) => execute_upstream(upstream_args),
    }
}

fn execute_add(args: AddArgs) -> Result<()> {
    println!("{} {}", "Forking".cyan().bold(), args.url.bold());
    println!("  {} Discovering versions...", "→".dimmed());

    let result = fork_package(ForkOptions {
        url: args.url,
        version: args.version,
        force: args.force,
    })?;

    // Success message
    println!();
    println!("{} Forked successfully!", "✓".green().bold());
    println!();
    println!(
        "  {} {}",
        "Fork location:".dimmed(),
        result
            .fork_dir
            .display()
            .to_string()
            .with_style(Style::Cyan)
    );
    println!(
        "  {} [patch].\"{}\" = {{ path = \"{}\" }}",
        "Patch entry:".dimmed(),
        result.module_url,
        result.patch_path
    );
    println!();
    println!(
        "{}",
        "You can now edit files in the fork directory. Changes will be used by 'pcb build'."
            .dimmed()
    );

    Ok(())
}

fn execute_remove(args: RemoveArgs) -> Result<()> {
    // Validate args
    if args.url.is_none() && !args.all {
        anyhow::bail!("Either specify a URL or use --all to unfork all packages");
    }

    let cwd = std::env::current_dir()?;
    let file_provider = DefaultFileProvider::new();
    let workspace_info = get_workspace_info(&file_provider, &cwd)
        .context("Failed to detect PCB workspace (no pcb.toml found up the tree?)")?;
    let workspace_root = &workspace_info.root;

    // Validate V2 workspace
    let pcb_toml_path = workspace_root.join("pcb.toml");
    if !pcb_toml_path.exists() {
        anyhow::bail!(
            "pcb fork remove requires a V2 workspace with pcb.toml at {}",
            pcb_toml_path.display()
        );
    }

    let mut config = PcbToml::from_file(&file_provider, &pcb_toml_path)?;
    if !config.is_v2() {
        anyhow::bail!("pcb fork remove only supports V2 workspaces (pcb-version >= 0.3)");
    }

    // Identify patches to remove
    let patches_to_remove: Vec<(String, Option<String>)> = if args.all {
        // Collect all path patches
        config
            .patch
            .iter()
            .filter_map(|(url, spec)| {
                if spec.path.is_some() {
                    Some((url.clone(), spec.path.clone()))
                } else {
                    None
                }
            })
            .collect()
    } else {
        let url = args.url.as_ref().unwrap().trim().to_string();
        if let Some(spec) = config.patch.get(&url) {
            if spec.path.is_none() {
                // It's a branch/rev patch, not a path patch
                anyhow::bail!(
                    "Patch for '{}' is not a local path fork (branch={:?}, rev={:?}).\n\
                     Remove manually from pcb.toml if desired.",
                    url,
                    spec.branch,
                    spec.rev
                );
            }
            vec![(url, spec.path.clone())]
        } else {
            anyhow::bail!("No patch found for '{}' in pcb.toml", url);
        }
    };

    if patches_to_remove.is_empty() {
        println!(
            "{}",
            "No forked packages (path patches) found in pcb.toml".dimmed()
        );
        return Ok(());
    }

    println!(
        "{} {} package(s)...",
        "Removing fork(s)".cyan().bold(),
        patches_to_remove.len()
    );

    let mut dirs_deleted = 0;
    let mut dirs_skipped = 0;

    for (url, path_opt) in &patches_to_remove {
        println!("  {} {}", "→".dimmed(), url);

        // Remove patch entry
        config.patch.remove(url);

        // Delete fork directory if it's under fork/
        if let Some(path) = path_opt {
            if path.starts_with("fork/") {
                let fork_dir = workspace_root.join(path);
                if fork_dir.exists() {
                    // Check for uncommitted changes before deleting
                    let rel_path = fork_dir.strip_prefix(workspace_root).unwrap_or(&fork_dir);
                    let has_changes = has_uncommitted_changes_in_path(workspace_root, rel_path);

                    if has_changes && !args.force {
                        println!(
                            "    {} Fork has uncommitted changes, use --force to delete",
                            "⚠".yellow()
                        );
                        dirs_skipped += 1;
                        continue;
                    }

                    fs::remove_dir_all(&fork_dir)
                        .with_context(|| format!("Failed to delete {}", fork_dir.display()))?;
                    dirs_deleted += 1;

                    // Clean up empty parent directories under fork/
                    cleanup_empty_parents(&fork_dir, &workspace_root.join("fork"))?;
                }
            } else {
                println!(
                    "    {} Path '{}' is outside fork/, not deleting",
                    "⚠".yellow(),
                    path
                );
            }
        }
    }

    // Write updated config
    let new_toml = toml::to_string_pretty(&config)?;
    fs::write(&pcb_toml_path, new_toml)
        .with_context(|| format!("Failed to write {}", pcb_toml_path.display()))?;

    // Success message
    println!();
    println!("{} Fork(s) removed successfully!", "✓".green().bold());
    println!();
    println!(
        "  {} {} patch(es) removed from pcb.toml",
        "Patches:".dimmed(),
        patches_to_remove.len()
    );
    if dirs_deleted > 0 {
        println!(
            "  {} {} directory(ies) deleted",
            "Directories:".dimmed(),
            dirs_deleted
        );
    }
    if dirs_skipped > 0 {
        println!(
            "  {} {} directory(ies) kept (uncommitted changes)",
            "Skipped:".dimmed(),
            dirs_skipped
        );
    }
    println!();
    println!(
        "{}",
        "Run 'pcb build' to resolve remote versions and update pcb.sum.".dimmed()
    );

    Ok(())
}

/// Remove empty parent directories up to (but not including) stop_at
fn cleanup_empty_parents(path: &Path, stop_at: &Path) -> Result<()> {
    let mut current = path.parent();
    while let Some(dir) = current {
        // Stop if we've reached or passed the stop_at directory
        if dir == stop_at || !dir.starts_with(stop_at) {
            break;
        }

        // Check if directory is empty
        if dir.exists() {
            let is_empty = dir.read_dir()?.next().is_none();
            if is_empty {
                fs::remove_dir(dir)?;
            } else {
                break; // Not empty, stop cleaning
            }
        }

        current = dir.parent();
    }

    // Also try to remove stop_at (fork/) if it's empty
    if stop_at.exists() {
        if let Ok(mut entries) = stop_at.read_dir() {
            if entries.next().is_none() {
                let _ = fs::remove_dir(stop_at);
            }
        }
    }

    Ok(())
}

fn execute_upstream(args: UpstreamArgs) -> Result<()> {
    println!("{}", "Upstreaming forks to registry...".cyan().bold());

    let result = upstream_forks(args.dry_run)?;

    if result.packages.is_empty() {
        println!(
            "{} No forks to upstream (no path patches in pcb.toml)",
            "!".yellow().bold()
        );
        return Ok(());
    }

    for (fork_path, target_path) in &result.packages {
        println!(
            "  {} {} → {}",
            "→".dimmed(),
            fork_path.dimmed(),
            target_path.bold()
        );
    }

    println!();
    if args.dry_run {
        println!("{} Dry run complete", "✓".green().bold());
    } else {
        println!("{} Branch pushed successfully!", "✓".green().bold());
    }
    println!();
    println!(
        "  {} {}",
        "Branch:".dimmed(),
        result.branch_name.with_style(Style::Cyan)
    );
    println!(
        "  {} {} package(s)",
        "Packages:".dimmed(),
        result.packages.len()
    );
    println!();
    println!(
        "  {} {}",
        "Create PR at:".dimmed(),
        result.pr_url.with_style(Style::Cyan)
    );

    Ok(())
}
