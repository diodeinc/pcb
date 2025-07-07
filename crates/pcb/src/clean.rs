use anyhow::Result;
use clap::Args;
use pcb_zen::load::{cache_dir, find_workspace_root};

#[derive(Args, Debug)]
#[command(about = "Clean generated files")]
pub struct CleanArgs {
    #[arg(short, long, help = "Remove all generated files")]
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
    let project_root = find_workspace_root(&current_dir).unwrap_or(current_dir);

    // Define the temp directories to clean
    let temp_dirs = vec![project_root.join(".pcb")];

    // Clean up temp directories
    for path in temp_dirs {
        if path.exists() {
            println!("Removing {}", path.display());
            std::fs::remove_dir_all(&path)?;
        }
    }

    // Remove remote cache directory
    if !args.keep_cache {
        if let Ok(cache_dir) = cache_dir() {
            if cache_dir.exists() {
                println!("Removing cache directory {}", cache_dir.display());
                std::fs::remove_dir_all(&cache_dir)?;
            }
        }
    }

    println!("Clean complete");
    Ok(())
}
