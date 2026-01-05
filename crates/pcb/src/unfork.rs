use anyhow::{Context, Result};
use clap::Args;
use colored::Colorize;
use pcb_zen::get_workspace_info;
use pcb_zen::git::has_uncommitted_changes_in_path;
use pcb_zen_core::config::PcbToml;
use pcb_zen_core::DefaultFileProvider;
use std::fs;
use std::path::Path;

#[derive(Args)]
pub struct UnforkArgs {
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

pub fn execute(args: UnforkArgs) -> Result<()> {
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
            "pcb unfork requires a V2 workspace with pcb.toml at {}",
            pcb_toml_path.display()
        );
    }

    let mut config = PcbToml::from_file(&file_provider, &pcb_toml_path)?;
    if !config.is_v2() {
        anyhow::bail!("pcb unfork only supports V2 workspaces (pcb-version >= 0.3)");
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
        "Unforking".cyan().bold(),
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
    println!("{} Unforked successfully!", "✓".green().bold());
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
