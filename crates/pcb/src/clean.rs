use anyhow::Result;
use clap::Args;
use pcb_zen::load::cache_dir;
use pcb_zen_core::config::find_workspace_root;
use pcb_zen_core::DefaultFileProvider;
use std::fs;
use walkdir::WalkDir;

#[derive(Args, Debug)]
#[command(about = "Clean generated files")]
pub struct CleanArgs {
    #[arg(
        short,
        long,
        help = "Remove all cache files including shared repositories"
    )]
    pub force: bool,

    #[arg(
        long,
        help = "Avoid removing the remote cache (downloaded packages & GitHub repos)"
    )]
    pub keep_cache: bool,
}

pub fn execute(args: CleanArgs) -> Result<()> {
    // Find the workspace root starting from current directory
    let current_dir = std::env::current_dir()?;
    let file_provider = DefaultFileProvider::new();
    let project_root = find_workspace_root(&file_provider, &current_dir)?;

    // Define the temp directories to clean
    let temp_dirs = vec![project_root.join(".pcb")];

    // Clean up temp directories
    for path in temp_dirs {
        if path.exists() {
            println!("Removing {}", path.display());
            std::fs::remove_dir_all(&path)?;
        }
    }

    // Handle remote cache directory
    if !args.keep_cache {
        if let Ok(cache_dir) = cache_dir() {
            if cache_dir.exists() {
                if args.force {
                    // Force mode: delete everything
                    println!("Removing cache directory {}", cache_dir.display());
                    std::fs::remove_dir_all(&cache_dir)?;
                } else {
                    // Default mode: only delete worktree directories, keep .repo bare repos
                    clean_worktrees(&cache_dir)?;
                }
            }
        }
    }

    println!("Clean complete");
    Ok(())
}

/// Clean only worktree directories, preserving .repo bare repositories and .repo.lock files
fn clean_worktrees(cache_dir: &std::path::Path) -> Result<()> {
    let mut deleted_count = 0;

    // Walk through github/ and gitlab/ subdirectories
    for provider in ["github", "gitlab"] {
        let provider_dir = cache_dir.join(provider);
        if !provider_dir.exists() {
            continue;
        }

        // Walk through repos and find worktree directories
        for entry in WalkDir::new(&provider_dir)
            .min_depth(1)
            .max_depth(10)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();

            // Skip .repo directories, .repo.lock files, and anything inside .repo
            if path.file_name().and_then(|n| n.to_str()) == Some(".repo")
                || path.file_name().and_then(|n| n.to_str()) == Some(".repo.lock")
                || path
                    .ancestors()
                    .any(|p| p.file_name().and_then(|n| n.to_str()) == Some(".repo"))
            {
                continue;
            }

            // If it's a directory that's not .repo, it's a worktree - delete it
            if path.is_dir() && path != provider_dir {
                // Check if this is a repo root level (contains .repo as sibling)
                if let Some(parent) = path.parent() {
                    if parent.join(".repo").exists() {
                        println!("Removing {}", path.display());
                        match fs::remove_dir_all(path) {
                            Ok(()) => deleted_count += 1,
                            Err(e) => {
                                eprintln!("Warning: Failed to remove {}: {}", path.display(), e)
                            }
                        }
                    }
                }
            }
        }
    }

    if deleted_count == 0 {
        println!("No worktree directories to clean");
    }

    Ok(())
}
